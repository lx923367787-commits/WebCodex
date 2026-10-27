//! Trusted catalog reads for the exact Runner owned by a current logical Agent.
//! The Host read uses only the fixed, observed `read_chat_profiles` tool. It
//! cannot discover/refresh profiles, launch a worker, or choose a default model.
//! CLI metadata comes only from D's fixed exact-Runner/resource query. Metadata
//! cannot prove account entitlement or authorize nested execution.

use crate::agent_team_execution::{
    AgentTeamAssets, AgentTeamCatalog, AssetFuture, ObservedCliProvider, UnavailableCliProvider,
};
use crate::Database;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::time::{timeout_at, Instant};
use webcodex_core::agent_cli::{
    AgentCliCatalog, AgentCliProvider, AGENT_CLI_CATALOG_TTL_MS, AGENT_CLI_CONTROL_TIMEOUT_SECS,
};
use webcodex_core::plugin::{
    validate_plugin_input_arguments, validate_plugin_structured_output,
    validate_response_for_request, PluginCatalog, PluginDispatchState, PluginGatewayRequest,
    PluginGatewayResponsePayload,
};
use webcodex_core::runner_protocol::{AgentExecutionResourceDescriptor, AgentExecutionTarget};
use webcodex_runner_registry::{RunnerAccess, RunnerRegistry};
use webcodex_store::VerifiedAgentRun;

const HOST_PROVIDER: &str = "chat-agents";
const HOST_PROFILE_TOOL: &str = "read_chat_profiles";
const HOST_PROFILE_MAX_AGE_MS: i64 = 3_600_000;
const CATALOG_DEADLINE: Duration = Duration::from_secs(30);
const CLI_SCOPE_VALIDATION_BUDGET: Duration = Duration::from_secs(5);

pub(crate) struct RunnerAgentTeamAssets {
    registry: Arc<RunnerRegistry>,
    db: Arc<Database>,
}

// Private and never deserialized: neither model JSON nor User AuthContext can
// select a Runner, provider instance, schema, or a broader access projection.
struct CatalogScope {
    access: RunnerAccess,
    target: AgentExecutionTarget,
}

struct HostProfiles {
    ids: Vec<String>,
    observed_at_ms: i64,
    expires_at_ms: i64,
}

struct ReadyCliCatalog {
    provider: ObservedCliProvider,
    observed_at_ms: i64,
    expires_at_ms: i64,
}

impl RunnerAgentTeamAssets {
    pub(crate) fn new(registry: Arc<RunnerRegistry>, db: Arc<Database>) -> Self {
        Self { registry, db }
    }

    fn owner_access(&self, caller: &VerifiedAgentRun) -> Result<RunnerAccess, String> {
        self.db
            .validate_agent_run(caller)
            .map_err(|error| error.code().to_string())?;
        let owner = self
            .db
            .get_user_by_id(caller.owner_user())
            .map_err(|_| "agent_team_owner_unavailable")?
            .filter(|user| !user.is_disabled())
            .ok_or("agent_team_owner_unavailable")?;
        Ok(RunnerAccess {
            global_visibility: false,
            owner_bypass: false,
            username: Some(owner.username),
            group: None,
        })
    }

    async fn checked_target(
        &self,
        caller: &VerifiedAgentRun,
        access: &RunnerAccess,
    ) -> Result<AgentExecutionTarget, String> {
        if self.owner_access(caller)? != *access {
            return Err("agent_team_catalog_owner_changed".into());
        }
        let folder = self
            .db
            .agent_team_execution_folder(caller)
            .map_err(|error| error.code().to_string())?;
        let execution_project = folder
            .as_ref()
            .map_or_else(|| caller.project().to_owned(), |f| f.project.clone());
        let client_id = project_client(&execution_project)?;
        let runner = self
            .registry
            .get_runner_semantic_view_checked_for_auth(client_id, Some(access))
            .await
            .map_err(|_| "agent_team_catalog_runner_unavailable")?;
        if !runner.view.connected {
            return Err("agent_team_catalog_runner_unavailable".into());
        }
        if let Some(folder) = &folder {
            let local_id = execution_project
                .strip_prefix(&format!("agent:{client_id}:"))
                .ok_or("agent_team_exact_project_required")?;
            if !runner
                .view
                .projects
                .iter()
                .any(|p| p.id == local_id && !p.disabled && p.path == folder.path)
            {
                return Err("workspace_team_target_changed".into());
            }
        }
        let target = AgentExecutionTarget {
            client_id: runner.view.client_id,
            runner_instance_id: runner.view.runner_instance_id,
        };
        if !bounded(&target.client_id, 128) || !bounded(&target.runner_instance_id, 128) {
            return Err("agent_team_catalog_runner_unavailable".into());
        }
        // Recheck the run and owner after the asynchronous Registry read.
        if target.client_id != client_id
            || self.owner_access(caller)? != *access
            || self
                .db
                .agent_team_execution_project(caller)
                .map_err(|error| error.code().to_string())?
                != execution_project
        {
            return Err("agent_team_catalog_owner_changed".into());
        }
        Ok(target)
    }

    async fn validate_scope(
        &self,
        caller: &VerifiedAgentRun,
        scope: &CatalogScope,
    ) -> Result<(), String> {
        if self.checked_target(caller, &scope.access).await? != scope.target {
            return Err("agent_team_catalog_runner_changed".into());
        }
        Ok(())
    }

    async fn read_plugin(
        &self,
        caller: &VerifiedAgentRun,
        scope: &CatalogScope,
        request: PluginGatewayRequest,
        deadline: Instant,
    ) -> Result<PluginGatewayResponsePayload, String> {
        timeout_at(deadline, self.validate_scope(caller, scope))
            .await
            .map_err(|_| "agent_team_catalog_timeout")??;
        if Instant::now() >= deadline {
            return Err("agent_team_catalog_timeout".into());
        }
        let (request_id, response) = timeout_at(
            deadline,
            self.registry.enqueue_plugin_gateway(
                &scope.target.client_id,
                &scope.target.runner_instance_id,
                request.clone(),
                Some(&scope.access),
                format!("agent:{}:catalog", caller.agent_id()),
            ),
        )
        .await
        .map_err(|_| "agent_team_catalog_timeout")?
        .map_err(|_| "agent_team_host_catalog_unavailable")?;
        let response = match timeout_at(deadline, response).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err("agent_team_host_catalog_unavailable".into()),
            Err(_) => {
                // Best effort within the same deadline; cleanup cannot extend
                // the read or cause any redispatch after a timeout.
                let _ = timeout_at(
                    deadline,
                    self.registry.cancel_request_dispatch_state(&request_id),
                )
                .await;
                return Err("agent_team_catalog_timeout".into());
            }
        };
        timeout_at(deadline, self.validate_scope(caller, scope))
            .await
            .map_err(|_| "agent_team_catalog_timeout")??;
        validate_response_for_request(&request, &response)
            .map_err(|_| "agent_team_host_catalog_invalid")?;
        if response.dispatch_state != PluginDispatchState::Completed || response.error.is_some() {
            let stage = match request {
                PluginGatewayRequest::ProvidersList => "providers_list",
                PluginGatewayRequest::ToolsList { .. } => "tools_list",
                PluginGatewayRequest::ToolsCall { .. } => "profiles_read",
                _ => "unknown",
            };
            let reason = response
                .error
                .as_ref()
                .map(|error| error.code.as_str())
                .unwrap_or("");
            return Err(format!(
                "agent_team_host_catalog_unavailable:{stage}:{reason}"
            ));
        }
        response
            .payload
            .ok_or_else(|| "agent_team_host_catalog_invalid".into())
    }

    async fn read_cli_catalogs(
        &self,
        caller: &VerifiedAgentRun,
        scope: &CatalogScope,
    ) -> Result<(Vec<ReadyCliCatalog>, Vec<UnavailableCliProvider>), String> {
        let started = Instant::now();
        let deadline = started
            + Duration::from_secs(AGENT_CLI_CONTROL_TIMEOUT_SECS)
            + CLI_SCOPE_VALIDATION_BUDGET;
        // Scope and resource reads share a fixed extra five seconds. Bound the
        // first check to that allowance so D's full deadline still fits;
        // progress cannot renew the batch deadline used by all later reads.
        timeout_at(
            started + CLI_SCOPE_VALIDATION_BUDGET,
            self.validate_scope(caller, scope),
        )
        .await
        .map_err(|_| "agent_team_catalog_timeout")??;
        // D owns each query's absolute deadline and cleanup. Do not wrap these
        // futures in a shorter timeout or drop one because the other failed.
        let resources = timeout_at(
            deadline,
            self.registry
                .list_agent_execution_resources(&scope.access, &scope.target),
        )
        .await
        .map_err(|_| "agent_team_catalog_timeout")??;
        let providers: BTreeSet<_> = resources
            .iter()
            .filter_map(|resource| resource.provider)
            .collect();
        let results = futures_util::future::join_all(providers.iter().map(|provider| async {
            (
                *provider,
                self.registry
                    .query_agent_cli_catalog(
                        &scope.access,
                        &scope.target,
                        *provider,
                        format!("agent:{}:catalog", caller.agent_id()),
                    )
                    .await,
            )
        }))
        .await;
        timeout_at(deadline, self.validate_scope(caller, scope))
            .await
            .map_err(|_| "agent_team_catalog_timeout")??;
        let mut catalogs = Vec::with_capacity(providers.len());
        let mut unavailable = Vec::new();
        if providers.is_empty() {
            let inventory = self
                .registry
                .query_agent_cli_inventory(
                    &scope.access,
                    &scope.target,
                    None,
                    format!("agent:{}:cli-discover", caller.agent_id()),
                )
                .await?;
            timeout_at(deadline, self.validate_scope(caller, scope))
                .await
                .map_err(|_| "agent_team_catalog_timeout")??;
            for installation in inventory.installations {
                unavailable.push(cli_unavailable(
                    AgentCliProvider::parse(&installation.provider_id)?,
                    "agent_cli_provider_resource_unavailable",
                ));
            }
        }
        for (expected_provider, result) in results {
            // Missing/unsupported resources need not block ordinary GPT work.
            // Absence is not proof of installation status; raw errors stay private.
            let catalog = match result {
                Ok(catalog) => catalog,
                Err(reason) => {
                    unavailable.push(cli_unavailable(expected_provider, &reason));
                    continue;
                }
            };
            let unavailable_status = if catalog.status == "unavailable" {
                "agent_cli_metadata_unavailable"
            } else {
                "agent_cli_metadata_probe_failed"
            };
            if let Some(mut ready) = map_cli_catalog(
                catalog,
                &scope.target,
                expected_provider,
                chrono::Utc::now().timestamp_millis(),
            )? {
                timeout_at(deadline, self.validate_scope(caller, scope))
                    .await
                    .map_err(|_| "agent_team_catalog_timeout")??;
                let resource = timeout_at(
                    deadline,
                    self.registry.read_agent_execution_resource(
                        &scope.access,
                        &scope.target,
                        &ready.provider.resource_set_id,
                    ),
                )
                .await;
                // Consume neither a resource nor its error after a run, owner
                // or exact Runner change. The original deadline is unchanged.
                timeout_at(deadline, self.validate_scope(caller, scope))
                    .await
                    .map_err(|_| "agent_team_catalog_timeout")??;
                let Ok(resource) = resource.map_err(|_| "agent_team_catalog_timeout")? else {
                    // This provider may have been withdrawn. A current scope
                    // does not make that absence invalidate another provider.
                    unavailable.push(cli_unavailable(
                        expected_provider,
                        "agent_execution_resource_unavailable",
                    ));
                    continue;
                };
                bind_reviewed_controls(&mut ready.provider, resource)?;
                catalogs.push(ready);
            } else {
                unavailable.push(cli_unavailable(expected_provider, unavailable_status));
            }
        }
        Ok((catalogs, unavailable))
    }

    async fn read_catalog(
        &self,
        caller: &VerifiedAgentRun,
        include_cli: bool,
    ) -> Result<AgentTeamCatalog, String> {
        // One deadline covers all three fixed reads; progress cannot extend it.
        let deadline = Instant::now() + CATALOG_DEADLINE;
        let access = self.owner_access(caller)?;
        let target = timeout_at(deadline, self.checked_target(caller, &access))
            .await
            .map_err(|_| "agent_team_catalog_timeout")??;
        let scope = CatalogScope { access, target };
        let PluginGatewayResponsePayload::Providers { providers } = self
            .read_plugin(
                caller,
                &scope,
                PluginGatewayRequest::ProvidersList,
                deadline,
            )
            .await?
        else {
            return Err("agent_team_host_catalog_invalid".into());
        };
        let provider = providers
            .iter()
            .find(|provider| provider.provider_id == HOST_PROVIDER)
            .filter(|provider| provider.status == "ready" && provider.error_code.is_none())
            .ok_or("agent_team_host_provider_unavailable")?;
        let provider_instance_id = provider.provider_instance_id.clone();
        let PluginGatewayResponsePayload::Tools { tools } = self
            .read_plugin(
                caller,
                &scope,
                PluginGatewayRequest::ToolsList {
                    provider_id: HOST_PROVIDER.into(),
                    provider_instance_id: provider_instance_id.clone(),
                },
                deadline,
            )
            .await?
        else {
            return Err("agent_team_host_catalog_invalid".into());
        };
        let catalog = PluginCatalog::admit(tools).map_err(|_| "agent_team_host_catalog_invalid")?;
        let tool = catalog
            .tool(HOST_PROFILE_TOOL)
            .ok_or("agent_team_host_profile_read_unavailable")?;
        let annotations = tool
            .annotations
            .as_ref()
            .ok_or("agent_team_host_profile_read_unavailable")?;
        if annotations.get("readOnlyHint").and_then(Value::as_bool) != Some(true)
            || annotations.get("destructiveHint").and_then(Value::as_bool) != Some(false)
            || annotations.get("openWorldHint").and_then(Value::as_bool) != Some(false)
        {
            return Err("agent_team_host_profile_read_unavailable".into());
        }
        let arguments = json!({});
        validate_plugin_input_arguments(&tool.input_schema, &arguments)
            .map_err(|_| "agent_team_host_profile_read_unavailable")?;
        let PluginGatewayResponsePayload::ToolResult { result } = self
            .read_plugin(
                caller,
                &scope,
                PluginGatewayRequest::ToolsCall {
                    provider_id: HOST_PROVIDER.into(),
                    provider_instance_id: provider_instance_id.clone(),
                    name: HOST_PROFILE_TOOL.into(),
                    arguments,
                    expected_schema: tool.schema_observation(),
                },
                deadline,
            )
            .await?
        else {
            return Err("agent_team_host_catalog_invalid".into());
        };
        if result.is_error {
            return Err("agent_team_host_profiles_not_ready".into());
        }
        let value = result
            .structured_content
            .ok_or("agent_team_host_catalog_invalid")?;
        if let Some(schema) = &tool.output_schema {
            validate_plugin_structured_output(schema, &value)
                .map_err(|_| "agent_team_host_catalog_invalid")?;
        }
        let profiles = parse_host_profiles(&value, chrono::Utc::now().timestamp_millis())?;
        let identity = serde_json::to_vec(&json!({
            "kind": "webcodex_agent_host_profiles_v1",
            "project": caller.project(),
            "target": &scope.target,
            "provider_instance_id": provider_instance_id,
            "tool_catalog_digest": catalog.digest(),
            "observed_catalog": value,
        }))
        .map_err(|_| "agent_team_host_catalog_invalid")?;
        let host_catalog_ref = format!("sha256:{:x}", Sha256::digest(identity));
        // Ordinary GPT starts need only the Host's cached profile directory.
        // Do not construct or dispatch a CLI metadata query on that path.
        let (cli_catalogs, unavailable_providers) = if include_cli {
            self.read_cli_catalogs(caller, &scope).await?
        } else {
            (Vec::new(), Vec::new())
        };
        let mut observed_at_ms = profiles.observed_at_ms;
        let mut expires_at_ms = profiles.expires_at_ms;
        let mut providers = Vec::with_capacity(cli_catalogs.len());
        for cli in cli_catalogs {
            observed_at_ms = observed_at_ms.max(cli.observed_at_ms);
            expires_at_ms = expires_at_ms.min(cli.expires_at_ms);
            providers.push(cli.provider);
        }
        if expires_at_ms <= chrono::Utc::now().timestamp_millis() {
            return Err("agent_team_catalog_expired".into());
        }
        // Project only IDs and a digest. Raw Host labels, reasons, descriptions,
        // response text, and arbitrary metadata never cross the model surface.
        Ok(AgentTeamCatalog {
            project: caller.project().into(),
            target: scope.target,
            observed_at_ms,
            expires_at_ms,
            providers,
            unavailable_providers,
            host_profile_ids: profiles.ids,
            host_catalog_ref,
            host_profiles_expires_at_ms: profiles.expires_at_ms,
        })
    }
}

fn cli_unavailable(provider: AgentCliProvider, reason: &str) -> UnavailableCliProvider {
    let reason_code = match reason {
        "agent_isolated_execution_unavailable" => "agent_isolated_execution_unavailable",
        "agent_cli_provider_resource_unavailable" | "agent_execution_resource_unavailable" => {
            "agent_cli_provider_resource_unavailable"
        }
        "agent_cli_provider_resource_ambiguous" => "agent_cli_provider_resource_ambiguous",
        "agent_cli_metadata_unavailable" => "agent_cli_metadata_unavailable",
        "agent_cli_metadata_probe_failed" => "agent_cli_metadata_probe_failed",
        _ => "agent_cli_catalog_query_failed",
    };
    UnavailableCliProvider {
        provider_id: provider.id().into(),
        reason_code,
    }
}

impl AgentTeamAssets for RunnerAgentTeamAssets {
    fn inventory<'a>(
        &'a self,
        caller: &'a VerifiedAgentRun,
        provider: Option<AgentCliProvider>,
    ) -> AssetFuture<'a, webcodex_core::agent_cli::AgentCliInventory> {
        Box::pin(async move {
            let access = self.owner_access(caller)?;
            let target = self.checked_target(caller, &access).await?;
            let inventory = self
                .registry
                .query_agent_cli_inventory(
                    &access,
                    &target,
                    provider,
                    format!("agent:{}:cli-discover", caller.agent_id()),
                )
                .await?;
            if self.checked_target(caller, &access).await? != target {
                return Err("agent_team_catalog_runner_changed".into());
            }
            Ok(inventory)
        })
    }
    fn catalog<'a>(
        &'a self,
        caller: &'a VerifiedAgentRun,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamCatalog, String>> + Send + 'a>> {
        Box::pin(self.read_catalog(caller, true))
    }

    fn host_catalog<'a>(
        &'a self,
        caller: &'a VerifiedAgentRun,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamCatalog, String>> + Send + 'a>> {
        Box::pin(self.read_catalog(caller, false))
    }
}

fn project_client(project: &str) -> Result<&str, String> {
    let (client, project) = project
        .strip_prefix("agent:")
        .and_then(|rest| rest.split_once(':'))
        .ok_or("agent_team_exact_project_required")?;
    if !bounded(client, 128) || !bounded(project, 256) {
        return Err("agent_team_exact_project_required".into());
    }
    Ok(client)
}

fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn map_cli_catalog(
    catalog: AgentCliCatalog,
    target: &AgentExecutionTarget,
    provider: AgentCliProvider,
    now_ms: i64,
) -> Result<Option<ReadyCliCatalog>, String> {
    let invalid = "agent_team_cli_catalog_invalid";
    // Registry already validated the response against its retained resource
    // fence. Recheck the projection's exact identity, age and non-authority.
    if catalog.version != 1
        || catalog.target != *target
        || catalog.provider != provider
        || catalog.observed_at_ms <= 0
        || catalog.observed_at_ms > now_ms
        || catalog.expires_at_ms <= now_ms
        || catalog.expires_at_ms.checked_sub(catalog.observed_at_ms)
            != Some(AGENT_CLI_CATALOG_TTL_MS)
        || catalog.account_availability_verified
        || catalog.nested_delegation_verified_disabled
    {
        return Err(invalid.into());
    }
    match catalog.status.as_str() {
        "error" | "unavailable" => return Ok(None),
        "ready" => {}
        _ => return Err(invalid.into()),
    }
    let executable_sha256 = catalog
        .executable_identity
        .strip_prefix("sha256:")
        .filter(|value| is_sha256(value))
        .ok_or(invalid)?;
    if !bounded(&catalog.cli_version, 128)
        || !bounded(&catalog.resource_set_id, 256)
        || !is_sha256(&catalog.resource_manifest_hash)
        || !matches!(catalog.source.as_str(), "installed_help" | "live_catalog")
        || catalog.models.is_empty()
        || !bounded_options(&catalog.models, 256, 128)
        || !bounded_options(&catalog.efforts, 32, 64)
    {
        return Err(invalid.into());
    }
    Ok(Some(ReadyCliCatalog {
        observed_at_ms: catalog.observed_at_ms,
        expires_at_ms: catalog.expires_at_ms,
        provider: ObservedCliProvider {
            provider_id: provider.id().into(),
            version: catalog.cli_version,
            executable_sha256: executable_sha256.into(),
            models: catalog.models,
            efforts: catalog.efforts,
            source: catalog.source,
            resource_set_id: catalog.resource_set_id,
            resource_manifest_hash: catalog.resource_manifest_hash,
            // Metadata alone supplies no review. The exact-instance resource
            // read below supplies the separately trusted typed record, if any.
            controls: None,
        },
    }))
}

fn bind_reviewed_controls(
    provider: &mut ObservedCliProvider,
    resource: AgentExecutionResourceDescriptor,
) -> Result<(), String> {
    let mismatch = "agent_team_cli_resource_binding_mismatch";
    resource.validate().map_err(|_| mismatch)?;
    if resource.provider.as_ref().map(AgentCliProvider::id) != Some(provider.provider_id.as_str())
        || resource.resource_set_id != provider.resource_set_id
        || resource.manifest_hash != provider.resource_manifest_hash
        || resource.executable_identity != format!("sha256:{}", provider.executable_sha256)
        || resource
            .reviewed_controls
            .as_ref()
            .is_some_and(|record| record.cli_version != provider.version)
    {
        return Err(mismatch.into());
    }
    // Descriptor validation checks the record's shape and exact P/R/F resource
    // association. Trusted provenance is the existing Runner resource read,
    // not a model field or a new issuer. Valid facts remain readable even when
    // require_supported_loading would reject execution, as it currently does.
    provider.controls = resource.reviewed_controls;
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded_options(values: &[String], count: usize, bytes: usize) -> bool {
    let mut unique = BTreeSet::new();
    values.len() <= count
        && values
            .iter()
            .all(|value| bounded(value, bytes) && unique.insert(value))
}

fn parse_host_profiles(value: &Value, now_ms: i64) -> Result<HostProfiles, String> {
    let invalid = "agent_team_host_catalog_invalid";
    if serde_json::to_vec(value).map_err(|_| invalid)?.len() > 64 * 1024 {
        return Err(invalid.into());
    }
    if value.get("state").and_then(Value::as_str) != Some("ready") {
        return Err("agent_team_host_profiles_not_ready".into());
    }
    if value.get("refresh_required").and_then(Value::as_bool) != Some(false) {
        return Err("agent_team_host_profiles_expired".into());
    }
    let observed_seconds = value
        .get("observed_at")
        .and_then(Value::as_f64)
        .ok_or(invalid)?;
    let observed_millis = observed_seconds * 1000.0;
    if !observed_millis.is_finite() || observed_millis < 0.0 || observed_millis > now_ms as f64 {
        return Err(invalid.into());
    }
    let observed_at_ms = observed_millis.floor() as i64;
    // Host's cache observation is the lease origin; reading it never renews it.
    let expires_at_ms = observed_at_ms
        .checked_add(HOST_PROFILE_MAX_AGE_MS)
        .filter(|expiry| *expiry > now_ms)
        .ok_or("agent_team_host_profiles_expired")?;
    let profiles = value
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or(invalid)?;
    if profiles.is_empty()
        || profiles.len() > 33
        || serde_json::to_vec(profiles).map_err(|_| invalid)?.len() > 32768
    {
        return Err(invalid.into());
    }
    let mut ids = Vec::with_capacity(profiles.len());
    let mut unique = BTreeSet::new();
    for profile in profiles {
        if serde_json::to_vec(profile).map_err(|_| invalid)?.len() > 8192 {
            return Err(invalid.into());
        }
        let id = profile
            .get("profile_id")
            .and_then(Value::as_str)
            .ok_or(invalid)?;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
            || !unique.insert(id)
        {
            return Err(invalid.into());
        }
        ids.push(id.to_string());
    }
    Ok(HostProfiles {
        ids,
        observed_at_ms,
        expires_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use webcodex_core::agent_cli_controls::{
        ReviewedAgentCliControls, ReviewedControlArg, ReviewedControlFile,
    };

    #[test]
    fn cli_diagnostics_preserve_only_known_codes_without_private_error_text() {
        let unavailable = cli_unavailable(
            AgentCliProvider::WorkBuddy,
            "agent_isolated_execution_unavailable",
        );
        assert_eq!(
            unavailable.reason_code,
            "agent_isolated_execution_unavailable"
        );
        assert_eq!(unavailable.provider_id, "workbuddy");
        let hidden = cli_unavailable(
            AgentCliProvider::Antigravity,
            "private path and credential-bearing provider stderr",
        );
        assert_eq!(hidden.reason_code, "agent_cli_catalog_query_failed");
        assert!(!serde_json::to_string(&hidden).unwrap().contains("private"));
    }

    fn cli_observed() -> AgentCliCatalog {
        AgentCliCatalog {
            version: 1,
            target: AgentExecutionTarget {
                client_id: "observed-client".into(),
                runner_instance_id: "observed-instance".into(),
            },
            resource_set_id: "observed-resource".into(),
            resource_manifest_hash: "a".repeat(64),
            executable_identity: format!("sha256:{}", "b".repeat(64)),
            provider: AgentCliProvider::WorkBuddy,
            cli_version: "observed-version".into(),
            models: vec!["observed-model".into()],
            efforts: Vec::new(),
            source: "installed_help".into(),
            status: "ready".into(),
            message: "Raw probe diagnostics are not projected".into(),
            observed_at_ms: 1_000_000,
            expires_at_ms: 1_060_000,
            account_availability_verified: false,
            nested_delegation_verified_disabled: false,
        }
    }

    fn resource_for(catalog: &AgentCliCatalog) -> AgentExecutionResourceDescriptor {
        AgentExecutionResourceDescriptor {
            project_workspace_copy: false,
            resource_set_id: catalog.resource_set_id.clone(),
            manifest_hash: catalog.resource_manifest_hash.clone(),
            executable_identity: catalog.executable_identity.clone(),
            provider: Some(catalog.provider),
            reviewed_controls: None,
        }
    }

    // Shape/hash fixtures only: no installed provider or mechanism is approved.
    fn synthetic_record_fixture() -> (AgentCliCatalog, AgentExecutionResourceDescriptor) {
        let mut catalog = cli_observed();
        let record = ReviewedAgentCliControls {
            version: 1,
            record_id: "synthetic-review".into(),
            provider: catalog.provider,
            cli_version: catalog.cli_version.clone(),
            executable_sha256: "b".repeat(64),
            resource_set_id: catalog.resource_set_id.clone(),
            payload_sha256: "c".repeat(64),
            control_files: vec![ReviewedControlFile {
                path: "input/controls.json".into(),
                sha256: "d".repeat(64),
            }],
            loading_method: "synthetic-unverified-loader".into(),
            fixed_args: vec![ReviewedControlArg {
                flag: "--synthetic-controls".into(),
                value: Some("input/controls.json".into()),
            }],
            verification_evidence_sha256: "e".repeat(64),
            independent_review_sha256: "f".repeat(64),
            adapter: None,
        };
        catalog.resource_manifest_hash = record.execution_manifest_sha256().unwrap();
        let mut resource = resource_for(&catalog);
        resource.reviewed_controls = Some(record);
        (catalog, resource)
    }

    fn mapped_provider(catalog: AgentCliCatalog) -> ObservedCliProvider {
        let target = catalog.target.clone();
        let provider = catalog.provider;
        map_cli_catalog(catalog, &target, provider, 1_001_000)
            .unwrap()
            .unwrap()
            .provider
    }

    #[test]
    fn ready_cli_keeps_observed_values_without_inventing_effort_or_controls() {
        let catalog = cli_observed();
        let resource = resource_for(&catalog);
        let target = catalog.target.clone();
        let mut ready = map_cli_catalog(catalog, &target, AgentCliProvider::WorkBuddy, 1_001_000)
            .unwrap()
            .unwrap();
        bind_reviewed_controls(&mut ready.provider, resource).unwrap();
        assert_eq!(ready.observed_at_ms, 1_000_000);
        assert_eq!(ready.expires_at_ms, 1_060_000);
        assert_eq!(ready.provider.models, vec!["observed-model"]);
        assert!(ready.provider.efforts.is_empty());
        assert!(ready.provider.controls.is_none());
        assert_eq!(ready.provider.executable_sha256, "b".repeat(64));
    }

    #[test]
    fn valid_typed_record_is_readable_without_a_supported_loading_mechanism() {
        let (catalog, resource) = synthetic_record_fixture();
        let record = resource.reviewed_controls.as_ref().unwrap().clone();
        let mut provider = mapped_provider(catalog);
        bind_reviewed_controls(&mut provider, resource).unwrap();
        assert_eq!(provider.controls.as_ref(), Some(&record));
        assert_eq!(provider.models, vec!["observed-model"]);
        assert!(provider.efforts.is_empty());
        assert_eq!(
            record.require_supported_loading().unwrap_err(),
            "agent_cli_controls_mechanism_unverified"
        );
    }

    #[test]
    fn resource_binding_rejects_record_version_manifest_and_record_substitution() {
        let (catalog, resource) = synthetic_record_fixture();

        // Keep the alternate record internally consistent, including its F,
        // to prove the observed CLI version is checked independently.
        let mut wrong_version = resource.clone();
        wrong_version
            .reviewed_controls
            .as_mut()
            .unwrap()
            .cli_version = "another-version".into();
        wrong_version.manifest_hash = wrong_version
            .reviewed_controls
            .as_ref()
            .unwrap()
            .execution_manifest_sha256()
            .unwrap();
        let mut same_manifest = catalog.clone();
        same_manifest.resource_manifest_hash = wrong_version.manifest_hash.clone();
        assert!(
            bind_reviewed_controls(&mut mapped_provider(same_manifest), wrong_version).is_err()
        );

        let mut wrong_manifest = resource.clone();
        wrong_manifest.manifest_hash = "d".repeat(64);
        assert!(
            bind_reviewed_controls(&mut mapped_provider(catalog.clone()), wrong_manifest).is_err()
        );

        let mut changed_record = resource;
        changed_record.reviewed_controls.as_mut().unwrap().record_id = "another-record".into();
        assert!(bind_reviewed_controls(&mut mapped_provider(catalog), changed_record).is_err());
    }

    #[test]
    fn unreviewed_resource_still_must_match_exact_metadata_identity() {
        let catalog = cli_observed();
        let resource = resource_for(&catalog);
        let mut wrong_provider = resource.clone();
        wrong_provider.provider = Some(AgentCliProvider::Antigravity);
        let mut wrong_resource = resource.clone();
        wrong_resource.resource_set_id = "another-resource".into();
        let mut wrong_executable = resource.clone();
        wrong_executable.executable_identity = format!("sha256:{}", "d".repeat(64));
        let mut wrong_manifest = resource;
        wrong_manifest.manifest_hash = "e".repeat(64);
        for changed in [
            wrong_provider,
            wrong_resource,
            wrong_executable,
            wrong_manifest,
        ] {
            let mut provider = mapped_provider(catalog.clone());
            assert!(bind_reviewed_controls(&mut provider, changed).is_err());
            assert!(provider.controls.is_none());
        }
    }

    #[test]
    fn cli_projection_rejects_stale_retargeted_and_execution_authority_claims() {
        let catalog = cli_observed();
        let target = catalog.target.clone();
        let provider = AgentCliProvider::WorkBuddy;
        assert!(map_cli_catalog(catalog.clone(), &target, provider, 1_060_000).is_err());
        let mut changed = catalog.clone();
        changed.target.runner_instance_id = "another-instance".into();
        assert!(map_cli_catalog(changed, &target, provider, 1_001_000).is_err());
        let mut claimed_controls = catalog.clone();
        claimed_controls.nested_delegation_verified_disabled = true;
        assert!(map_cli_catalog(claimed_controls, &target, provider, 1_001_000).is_err());
        let mut claimed_account = catalog;
        claimed_account.account_availability_verified = true;
        assert!(map_cli_catalog(claimed_account, &target, provider, 1_001_000).is_err());
    }

    #[test]
    fn unsuccessful_cli_observation_does_not_supply_a_provider() {
        for status in ["unavailable", "error"] {
            let mut catalog = cli_observed();
            let target = catalog.target.clone();
            catalog.status = status.into();
            catalog.models.clear();
            assert!(
                map_cli_catalog(catalog, &target, AgentCliProvider::WorkBuddy, 1_001_000)
                    .unwrap()
                    .is_none()
            );
        }
    }

    fn observed() -> Value {
        json!({
            "state": "ready",
            "refresh_required": false,
            "observed_at": 1000.25,
            "profiles": [{"profile_id": "observed-profile-1", "label": "Host metadata"}],
        })
    }

    #[test]
    fn repeated_reads_preserve_host_observation_and_expiry() {
        let value = observed();
        let first = parse_host_profiles(&value, 1_001_000).unwrap();
        let later = parse_host_profiles(&value, 2_000_000).unwrap();
        assert_eq!(first.observed_at_ms, 1_000_250);
        assert_eq!(first.expires_at_ms, 4_600_250);
        assert_eq!(first.expires_at_ms, later.expires_at_ms);
        assert_eq!(later.ids, vec!["observed-profile-1"]);
    }

    #[test]
    fn stale_future_and_unready_catalogs_do_not_become_usable() {
        let value = observed();
        assert!(parse_host_profiles(&value, 4_600_250).is_err());
        assert!(parse_host_profiles(&value, 1_000_249).is_err());
        let mut stale = value.clone();
        stale["refresh_required"] = json!(true);
        assert!(parse_host_profiles(&stale, 1_001_000).is_err());
        let mut not_ready = value;
        not_ready["state"] = json!("discovering");
        assert!(parse_host_profiles(&not_ready, 1_001_000).is_err());
    }

    #[test]
    fn malformed_or_duplicate_profile_ids_are_rejected() {
        let mut value = observed();
        let duplicate = value["profiles"][0].clone();
        value["profiles"].as_array_mut().unwrap().push(duplicate);
        assert!(parse_host_profiles(&value, 1_001_000).is_err());
        value["profiles"] = json!([{"profile_id": "untrusted\ntext"}]);
        assert!(parse_host_profiles(&value, 1_001_000).is_err());
    }
}
