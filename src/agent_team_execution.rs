//! Unified CLI entry and the retained explicit isolated team protocol.
//! Local calls use task-owned native jobs. Isolated calls retain their resource
//! and attempt fences; neither path manufactures a user AuthContext.
use crate::tool_runtime::{ToolResult, ToolRuntime};
use crate::Database;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use webcodex_core::agent_cli::AgentCliProvider;
use webcodex_core::agent_cli_controls::{compose_reviewed_cli_argv, ReviewedAgentCliControls};
use webcodex_core::runner_protocol::{
    compute_workspace_launch_intent_hash, AgentExecutionBinding, AgentExecutionCarrier,
    AgentExecutionCloseReceipt, AgentExecutionProcess, AgentExecutionStart, AgentExecutionTarget,
    AgentExecutionWorkspaceSource, ShellCommandExecutionState, ShellJobInfo,
};
use webcodex_runner_registry::{
    AgentCliDeliveryStoreEvidence, AgentCliDeliveryStoreProof, AgentExecutionStoreAdmission,
    AgentExecutionStorePreparationEvidence, RunnerAccess, RunnerRegistry,
    VerifiedAgentCliDeliverySnapshot, VerifiedAgentExecutionCloseReceipt,
};
use webcodex_store::{
    AgentAssignmentOperation, AgentCliDeliveryReference, AgentCliObservationPermit,
    AgentCliPreparationPhase, AgentCliPreparationRequest, AgentExecutionCloseProgress,
    AgentTaskState, AuthenticatedAgentCliDeliverySnapshot, AuthenticatedAgentExecutionCloseReceipt,
    TeamCliDispatchState, TeamCliObservation, VerifiedAgentRun,
};

pub(crate) type AssetFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Root-controlled boundary for the real D resource directory and Host catalogs.
/// No production constructor from model Value and no fabricated default list.
/// Provider controls must have been exercised on this exact installed binary.
pub(crate) trait AgentTeamAssets: Send + Sync {
    fn catalog<'a>(&'a self, caller: &'a VerifiedAgentRun) -> AssetFuture<'a, AgentTeamCatalog>;
    fn selected_catalog<'a>(
        &'a self,
        caller: &'a VerifiedAgentRun,
        _provider: AgentCliProvider,
        _require_host: bool,
        _configured_default: bool,
    ) -> AssetFuture<'a, AgentTeamCatalog> {
        self.catalog(caller)
    }
    fn inventory<'a>(
        &'a self,
        _caller: &'a VerifiedAgentRun,
        _provider: Option<AgentCliProvider>,
    ) -> AssetFuture<'a, webcodex_core::agent_cli::AgentCliInventory> {
        Box::pin(async { Err("agent_cli_inventory_unavailable".into()) })
    }
    fn native_inventory<'a>(
        &'a self,
        caller: &'a VerifiedAgentRun,
        provider: Option<AgentCliProvider>,
    ) -> AssetFuture<'a, webcodex_core::agent_cli::AgentCliInventory> {
        self.inventory(caller, provider)
    }
    fn host_catalog<'a>(
        &'a self,
        caller: &'a VerifiedAgentRun,
    ) -> AssetFuture<'a, AgentTeamCatalog> {
        self.catalog(caller)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ObservedCliProvider {
    pub host_cli: Option<webcodex_core::host_cli::HostCliRegistration>,
    pub provider_id: String,
    pub version: String,
    pub executable_sha256: String,
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    pub model_efforts: std::collections::BTreeMap<String, Vec<String>>,
    pub source: String,
    pub resource_set_id: String,
    /// Resource version covered by the provider-controls review. Registry's
    /// authenticated descriptor, not this expectation, supplies the actual hash.
    pub resource_manifest_hash: String,
    /// Exact typed record read from the authenticated Runner's trusted profile.
    /// A consistent record can be displayed without its loader being supported.
    pub controls: Option<ReviewedAgentCliControls>,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentTeamCatalog {
    pub project: String,
    pub target: AgentExecutionTarget,
    pub observed_at_ms: i64,
    pub expires_at_ms: i64,
    pub providers: Vec<ObservedCliProvider>,
    pub unavailable_providers: Vec<UnavailableCliProvider>,
    pub host_profile_ids: Vec<String>,
    pub host_catalog_ref: String,
    pub host_profiles_expires_at_ms: i64,
    pub host_profiles_error: Option<String>,
    pub host_default_supported: bool,
}

/// Fixed diagnostics only; no provider stderr, host paths or credentials.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct UnavailableCliProvider {
    pub provider_id: String,
    pub reason_code: &'static str,
}

impl ToolRuntime {
    /// Trusted composition root only. B/D/E must supply observed facts; absence
    /// is reported before factory mutation, never disguised as a ready provider.
    pub(crate) fn with_agent_team_assets(mut self, assets: Arc<dyn AgentTeamAssets>) -> Self {
        self.agent_team_assets = Some(assets);
        self
    }
}

fn denied(code: &str) -> ToolResult {
    let mut result = ToolResult::err_with_output(
        code,
        json!({"error_kind":code,"state_changed":false,"dispatched":false,"dispatch_certainty":"not_started"}),
    );
    if code == "team_plan_required" {
        result.output["next_action"] = json!("For trusted local CLI execution use cli start in its default local mode. Explicit isolated execution requires its intended frozen plan. The assigned task still exists; do not create a replacement manager or retry an uncertain execution through a different mode.");
    }
    result
}
fn catalog_unavailable(reason: &str) -> ToolResult {
    // Only fixed local failure codes cross this boundary. Provider diagnostics,
    // paths, credentials and arbitrary error text remain private.
    let mut result = denied("agent_team_catalog_unavailable");
    if let Some(details) = reason.strip_prefix("agent_team_host_catalog_unavailable:") {
        if let Some((stage, code)) = details.split_once(':') {
            if matches!(stage, "providers_list" | "tools_list" | "profiles_read") {
                result.output["stage"] = json!(stage);
            }
            let code = match code {
                "plugin_provider_unavailable"
                | "plugin_runtime_not_active"
                | "plugin_provider_busy"
                | "stale_plugin_provider"
                | "plugin_schema_changed"
                | "plugin_tool_unavailable"
                | "plugin_arguments_schema_invalid"
                | "plugin_timeout"
                | "plugin_manager_stopping" => code,
                _ => "agent_team_host_catalog_unavailable",
            };
            result.output["reason_code"] = json!(code);
            return result;
        }
    }
    let reason = match reason {
        "agent_team_catalog_expired"
        | "agent_team_catalog_owner_changed"
        | "agent_team_catalog_runner_changed"
        | "agent_team_catalog_runner_unavailable"
        | "agent_team_catalog_timeout"
        | "agent_team_catalog_projection_invalid"
        | "agent_team_cli_catalog_invalid"
        | "agent_team_cli_resource_binding_mismatch"
        | "agent_team_exact_project_required"
        | "agent_team_host_catalog_invalid"
        | "agent_team_host_catalog_unavailable"
        | "agent_team_host_profile_read_unavailable"
        | "agent_team_host_profiles_expired"
        | "agent_team_host_profiles_not_ready"
        | "agent_team_host_default_unavailable"
        | "agent_team_profile_not_discovered"
        | "agent_team_host_provider_unavailable"
        | "agent_team_owner_unavailable"
        | "workspace_team_new_chat_required"
        | "workspace_team_target_changed" => reason,
        _ => "agent_team_catalog_unavailable",
    };
    result.output["reason_code"] = json!(reason);
    result
}

fn inventory_unavailable(reason: &str) -> ToolResult {
    let mut result = denied("agent_cli_inventory_unavailable");
    let code = match reason {
        "agent_cli_not_installed"
        | "local_cli_runner_upgrade_required"
        | "agent_cli_adapter_unavailable"
        | "agent_cli_launcher_requires_adapter"
        | "agent_cli_metadata_context_unavailable"
        | "agent_cli_metadata_source_unavailable"
        | "cli_adapter_config_invalid"
        | "cli_adapter_config_unreadable"
        | "cli_adapter_duplicate_id"
        | "agent_cli_query_timeout"
        | "agent_team_catalog_runner_changed"
        | "agent_team_catalog_runner_unavailable" => reason,
        _ => "agent_cli_inventory_unavailable",
    };
    result.output["reason_code"] = json!(code);
    result
}
fn unresolved(code: &str, attempt: Option<&str>) -> ToolResult {
    ToolResult::err_with_output(
        code,
        json!({"error_kind":code,"source_attempt_id":attempt,"state_changed":null,"dispatched":false,"dispatch_certainty":"outcome_unknown","recovery":"Observe the original attempt; do not replace its key, worker identity or Job."}),
    )
}
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

// Versioned, deterministic instructions are part of the frozen launch hash.
// The Runner must never append to or rewrite an already committed stdin.
const CLI_PRIVATE_DIRECTORIES: &str = "Only memory, execution objectives and acceptance criteria belong in your own three instance directories. Their absolute paths are in WEBCODEX_MEMORY_DIR, WEBCODEX_OBJECTIVES_DIR and WEBCODEX_ACCEPTANCE_DIR. You may read and update your own directories; do not read or modify those three directories belonging to other instances. This is a cooperation convention, not a technical security boundary. All other authorized project files, tools, logs, results and deliverables are shared. Keep the normal project working directory and use installed capabilities normally. Do not publish credentials. Report the actual results and checks through normal stdout/stderr; no publication manifest or review is required to expose ordinary results.";

fn cli_worker_prompt(brief: &str) -> String {
    format!("{brief}\n\n{CLI_PRIVATE_DIRECTORIES}")
}

fn cli_worker_input_prompt(
    brief: &str,
    inputs: &[webcodex_store::AgentEnrollmentSnapshot],
    _workspace: Option<&AgentExecutionWorkspaceSource>,
) -> Result<String, String> {
    cli_native_input_prompt(brief, inputs)
}

fn cli_native_input_prompt(
    brief: &str,
    inputs: &[webcodex_store::AgentEnrollmentSnapshot],
) -> Result<String, String> {
    let material = serde_json::to_string(inputs).map_err(|_| "agent_cli_inputs_invalid")?;
    Ok(format!("{}\n\nExplicit delegated input (JSON data):\n{material}\n\nOnly the brief and inputs above define your delegated task; no parent memory, objectives or acceptance criteria are supplied automatically.", cli_worker_prompt(brief)))
}

pub(crate) async fn cli_workspace_source(
    runtime: &ToolRuntime,
    db: &Database,
    caller: &VerifiedAgentRun,
    access: &RunnerAccess,
    target: &AgentExecutionTarget,
) -> Result<Option<AgentExecutionWorkspaceSource>, String> {
    use webcodex_store::AgentProjectFilesGrant;
    let grant = match db.agent_project_files_grant(caller, false) {
        Ok(grant) => grant,
        // Explicit private-only enrollments retain their input-snapshot workflow.
        Err(error) if error.code() == "workspace_host_unavailable" => return Ok(None),
        Err(error) => return Err(error.code().into()),
    };
    let project = db
        .agent_team_execution_project(caller)
        .map_err(|e| e.code().to_owned())?;
    let expected_root = match &grant {
        AgentProjectFilesGrant::Named(workspace) => Some(
            workspace
                .folders
                .iter()
                .find(|folder| folder.project == project)
                .ok_or("workspace_folder_not_authorized")?
                .path
                .as_str(),
        ),
        AgentProjectFilesGrant::Registered { project: allowed } if allowed == &project => None,
        _ => return Err("workspace_folder_not_authorized".into()),
    };
    let resolved = runtime
        .execution()
        .resolve_project_input(&project)
        .await
        .map_err(|_| "workspace_folder_unavailable")?;
    if resolved.resolved_id != project
        || resolved.config.client_id != target.client_id
        || expected_root.is_some_and(|root| root != resolved.config.path)
    {
        return Err("workspace_folder_changed".into());
    }
    runtime
        .runner_registry
        .assert_runner_access(Some(access), &target.client_id)
        .await
        .map_err(|_| "workspace_folder_not_authorized")?;
    if !db
        .agent_project_files_grant(caller, false)
        .is_ok_and(|current| current == grant)
    {
        return Err("workspace_folder_not_authorized".into());
    }
    let source = AgentExecutionWorkspaceSource {
        project,
        root: resolved.config.path,
    };
    source.validate()?;
    Ok(Some(source))
}
fn project_client(project: &str) -> Result<&str, String> {
    let rest = project
        .strip_prefix("agent:")
        .ok_or("agent_team_exact_project_required")?;
    let (client, project) = rest
        .split_once(':')
        .ok_or("agent_team_exact_project_required")?;
    if !bounded(client, 128) || !bounded(project, 256) {
        return Err("agent_team_exact_project_required".into());
    }
    Ok(client)
}

/// This access projection carries no credential and no bypass. Store's exact
/// attempt permit remains required before any Registry operation.
fn owner_access(db: &Database, binding: &VerifiedAgentRun) -> Result<RunnerAccess, String> {
    db.validate_agent_run(binding)
        .map_err(|e| e.code().to_string())?;
    let user = db
        .get_user_by_id(binding.owner_user())
        .map_err(|_| "agent_team_owner_unavailable")?
        .filter(|user| !user.is_disabled())
        .ok_or("agent_team_owner_unavailable")?;
    Ok(RunnerAccess {
        global_visibility: false,
        owner_bypass: false,
        username: Some(user.username),
        group: None,
    })
}

struct StoreAdmission<'a>(&'a Database);
impl AgentExecutionStoreAdmission for StoreAdmission<'_> {
    fn validate_persisted(&self, binding: &AgentExecutionBinding) -> Result<(), String> {
        self.0
            .validate_agent_execution_admission(binding)
            .map_err(|e| e.code().into())
    }
}

struct StorePreparationEvidence<'a> {
    db: &'a Database,
    caller: &'a VerifiedAgentRun,
    identity: &'a AgentExecutionBinding,
}
impl AgentExecutionStorePreparationEvidence for StorePreparationEvidence<'_> {
    fn recoverable_preparation(&self) -> Result<AgentExecutionBinding, String> {
        self.db
            .recoverable_agent_cli_preparation(self.caller, self.identity)
            .map_err(|error| error.code().into())
    }
}

// Only Registry's private-field verified object can construct this root bridge.
// Do not implement this trait for the raw wire receipt, Value or Agent run.
struct StoreCloseReceipt(VerifiedAgentExecutionCloseReceipt);
impl AuthenticatedAgentExecutionCloseReceipt for StoreCloseReceipt {
    fn authenticated_receipt(&self) -> &AgentExecutionCloseReceipt {
        self.0.receipt()
    }
}

// The publication bridge can only wrap D's private-field verified snapshot.
// Model JSON, an observation permit and arbitrary bytes cannot construct it.
struct StoreDeliverySnapshot(VerifiedAgentCliDeliverySnapshot);
impl AuthenticatedAgentCliDeliverySnapshot for StoreDeliverySnapshot {
    fn binding(&self) -> &AgentExecutionBinding {
        self.0.binding()
    }
    fn artifact_bytes(&self) -> &[u8] {
        self.0.artifact_bytes()
    }
    fn artifact_sha256(&self) -> &str {
        self.0.artifact_sha256()
    }
}

struct StoreDeliveryEvidence<'a> {
    db: &'a Database,
    reviewer: &'a VerifiedAgentRun,
    expected: &'a AgentExecutionBinding,
}
impl AgentCliDeliveryStoreEvidence for StoreDeliveryEvidence<'_> {
    fn validate_closed_delivery(
        &self,
        binding: &AgentExecutionBinding,
    ) -> Result<AgentCliDeliveryStoreProof, String> {
        if binding != self.expected {
            return Err("agent_cli_delivery_identity_mismatch".into());
        }
        // Every invocation re-reads current reviewer authority and the original
        // accepted full TreeExited receipt. No cached DTO or stop boolean.
        let proof = self
            .db
            .read_agent_cli_delivery_stop_evidence(self.reviewer, binding)
            .map_err(|error| error.code().to_string())?;
        Ok(AgentCliDeliveryStoreProof {
            close: proof.close().clone(),
            receipt: proof.receipt().clone(),
        })
    }
}

async fn collect_delivery(
    db: &Database,
    registry: &RunnerRegistry,
    access: &RunnerAccess,
    reviewer: &VerifiedAgentRun,
    permit: &AgentCliObservationPermit,
) -> Result<AgentCliDeliveryReference, String> {
    let expected = permit.binding();
    let evidence = StoreDeliveryEvidence {
        db,
        reviewer,
        expected,
    };
    let snapshot = registry
        .read_agent_cli_delivery_snapshot_recovered(
            access,
            expected,
            &evidence,
            "agent-team-delivery-collector".into(),
        )
        .await?;
    // The read can last up to forty seconds. Fence a handoff/revocation or
    // ownership change before persisting its explicit publication.
    if owner_access(db, reviewer)? != *access {
        return Err("agent_cli_delivery_reviewer_changed".into());
    }
    evidence.validate_closed_delivery(expected)?;
    let context = db
        .ensure_agent_host_context(reviewer, None)
        .map_err(|error| error.code().to_string())?;
    db.gate_host_context(&reviewer.host_goal_owner_key(), Some(&context), false)
        .map_err(|error| error.code().to_string())?;
    db.publish_agent_cli_delivery(&StoreDeliverySnapshot(snapshot))
        .map_err(|error| error.code().into())
}

async fn publish_delivery(
    db: &Database,
    registry: &RunnerRegistry,
    access: &RunnerAccess,
    permit: &AgentCliObservationPermit,
) -> Result<AgentCliDeliveryReference, String> {
    let snapshot = registry
        .read_agent_cli_delivery_snapshot(
            access,
            permit.binding(),
            "agent-team-delivery-controller".into(),
        )
        .await?;
    db.publish_agent_cli_delivery(&StoreDeliverySnapshot(snapshot))
        .map_err(|error| error.code().into())
}

/// Invoked exclusively by the existing authenticated Runner result handlers.
/// Their client/instance/connection are transport facts, never model arguments.
/// The caller may ACK stored=true only after this Store transaction commits.
pub(crate) async fn accept_authenticated_close_receipt(
    db: &Database,
    registry: &RunnerRegistry,
    client_id: &str,
    runner_instance_id: &str,
    connection_id: Option<&str>,
    request_id: &str,
    receipt: AgentExecutionCloseReceipt,
) -> Result<AgentExecutionCloseProgress, String> {
    let verified = registry
        .accept_agent_execution_close_receipt(
            client_id,
            runner_instance_id,
            connection_id,
            request_id,
            receipt,
        )
        .await?;
    db.accept_agent_execution_close_receipt(&StoreCloseReceipt(verified))
        .map_err(|e| e.code().into())
}

pub(crate) fn provider_supports_cli_default(provider: &ObservedCliProvider) -> bool {
    provider
        .controls
        .as_ref()
        .and_then(|control| control.adapter.clone())
        .or_else(|| provider.host_cli.as_ref().map(|host| host.adapter.clone()))
        .or_else(|| {
            webcodex_core::cli_adapter::cli_adapters(&[])
                .ok()?
                .into_iter()
                .find(|a| a.id == provider.provider_id)
        })
        .and_then(|a| a.invocation)
        .is_some_and(|i| i.supports_configured_default)
}

fn cli_selection_discovered(
    provider: &ObservedCliProvider,
    model: impl Into<webcodex_core::cli_adapter::CliModelSelection>,
    effort: Option<&str>,
) -> bool {
    let model = model.into();
    let Some(model) = model.named() else {
        // The configured/default model is selected by the CLI, so a model-list
        // lookup cannot establish its effort combination. The registered adapter
        // validates and renders any effort override before Worker reservation;
        // unsupported options retain that adapter's concrete failure code.
        return provider_supports_cli_default(provider);
    };
    let levels = if provider.model_efforts.is_empty() {
        &provider.efforts
    } else {
        let Some(levels) = provider.model_efforts.get(model) else {
            return false;
        };
        levels
    };
    provider.models.iter().any(|known| known == model)
        && match effort {
            Some(effort) => levels.iter().any(|known| known == effort),
            // Omission asks the selected CLI/model to use its own default.
            // Available override levels do not make an override mandatory.
            None => true,
        }
}

fn validate_catalog(
    db: &Database,
    catalog: &AgentTeamCatalog,
    binding: &VerifiedAgentRun,
    now: i64,
) -> Result<(), String> {
    let execution_project = db
        .agent_team_execution_project(binding)
        .map_err(|error| error.code().to_string())?;
    if catalog.project != binding.project()
        || catalog.target.client_id != project_client(&execution_project)?
        || !bounded(&catalog.target.runner_instance_id, 128)
        || catalog.observed_at_ms > now
        || catalog.expires_at_ms <= now
        || catalog.expires_at_ms.saturating_sub(catalog.observed_at_ms) > 3_600_000
        || catalog.providers.len() > webcodex_core::cli_adapter::MAX_CLI_ADAPTERS
        || catalog.unavailable_providers.len() > webcodex_core::cli_adapter::MAX_CLI_ADAPTERS
        || catalog.host_profile_ids.len() > 33
        || (catalog.host_profiles_error.is_none() && !bounded(&catalog.host_catalog_ref, 256))
    {
        return Err("agent_team_catalog_unavailable".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for provider in &catalog.providers {
        if !webcodex_core::cli_adapter::valid_cli_id(&provider.provider_id)
            || !ids.insert(&provider.provider_id)
            || !bounded(&provider.version, 128)
            || !digest(&provider.executable_sha256)
            || !digest(&provider.resource_manifest_hash)
            || !bounded(&provider.resource_set_id, 256)
            || !matches!(
                provider.source.as_str(),
                "installed_help" | "live_catalog" | "configured_default"
            )
            || (provider.models.is_empty() && !provider_supports_cli_default(provider))
            || provider.models.len() > 256
            || provider.efforts.len() > 32
            || provider.models.iter().any(|value| !bounded(value, 256))
            || provider.efforts.iter().any(|value| !bounded(value, 64))
            || webcodex_core::agent_cli::validate_model_efforts(
                &provider.models,
                &provider.efforts,
                &provider.model_efforts,
            )
            .is_err()
        {
            return Err("agent_team_catalog_unavailable".into());
        }
    }
    let profiles: std::collections::BTreeSet<_> = catalog.host_profile_ids.iter().collect();
    if profiles.len() != catalog.host_profile_ids.len()
        || profiles.iter().any(|id| !bounded(id, 128))
    {
        return Err("agent_team_catalog_unavailable".into());
    }
    Ok(())
}

fn validate_provider_controls_binding(
    provider: &ObservedCliProvider,
) -> Result<&ReviewedAgentCliControls, String> {
    let controls = provider
        .controls
        .as_ref()
        .ok_or("agent_cli_delegation_controls_unverified")?;
    let expected_provider = AgentCliProvider::parse(&provider.provider_id)
        .map_err(|_| "agent_cli_provider_unavailable")?;
    if controls.cli_version != provider.version {
        return Err("agent_cli_controls_version_mismatch".into());
    }
    controls.validate_resource(
        &provider.resource_set_id,
        Some(expected_provider),
        &format!("sha256:{}", provider.executable_sha256),
        &provider.resource_manifest_hash,
    )?;
    Ok(controls)
}

fn require_provider_controls(
    provider: &ObservedCliProvider,
) -> Result<&ReviewedAgentCliControls, String> {
    let controls = validate_provider_controls_binding(provider)?;
    controls.require_supported_loading()?;
    Ok(controls)
}

pub(crate) fn inventory_projection(
    inventory: webcodex_core::agent_cli::AgentCliInventory,
) -> Value {
    let capabilities = inventory.capabilities.map(|capabilities| {
        let mut public = serde_json::to_value(capabilities).expect("typed CLI capability");
        public
            .as_object_mut()
            .expect("typed object")
            .remove("executableIdentity");
        public
            .as_object_mut()
            .expect("typed object")
            .remove("adapter");
        public
    });
    json!({"installations":inventory.installations,"capabilities":capabilities,"observed_at_ms":inventory.observed_at_ms,
        "expires_at_ms":inventory.expires_at_ms,"execution_granted":false,"state_changed":false})
}

fn catalog_projection(catalog: &AgentTeamCatalog) -> Value {
    json!({"project":catalog.project,"observed_at_ms":catalog.observed_at_ms,"expires_at_ms":catalog.expires_at_ms,
        "providers":catalog.providers.iter().map(|p|json!({"provider_id":p.provider_id,"version":p.version,"models":p.models,"efforts":p.efforts,"model_efforts":p.model_efforts,"source":p.source,"configured_default_supported":provider_supports_cli_default(p),"model_selection_required":!provider_supports_cli_default(p),"execution_controls_verified":p.host_cli.is_some() || require_provider_controls(p).is_ok(),"execution_mode":if p.host_cli.is_some(){"native_host"}else{"host_program"}})).collect::<Vec<_>>(),
        "unavailable_providers":catalog.unavailable_providers,
        "ordinary_local_cli_tool":"cli","ordinary_local_cli_requires_isolation_resources":false,
        "thinking_profile_ids":catalog.host_profile_ids,"host_profiles_expires_at_ms":catalog.host_profiles_expires_at_ms,
        "host_profiles_error":catalog.host_profiles_error.as_ref().map(|reason| catalog_unavailable(reason).output["reason_code"].clone()),"host_default_supported":catalog.host_default_supported,
        "default_selection":"default","host_selection_required":false,"selection_required":false,"account_availability_verified":false})
}

fn validate_host_selection(
    catalog: &AgentTeamCatalog,
    selection: &str,
    require_host: bool,
) -> Result<(), String> {
    if selection == "default" {
        return if !require_host || catalog.host_default_supported {
            Ok(())
        } else {
            Err("agent_team_host_default_unavailable".into())
        };
    }
    if let Some(reason) = &catalog.host_profiles_error {
        return Err(reason.clone());
    }
    if catalog.host_profiles_expires_at_ms <= now_ms() {
        return Err("agent_team_host_profiles_expired".into());
    }
    if !catalog.host_profile_ids.iter().any(|id| id == selection) {
        return Err("agent_team_profile_not_discovered".into());
    }
    Ok(())
}

fn worker_task_projection(snapshot: &webcodex_store::AgentCliWorkerTaskSnapshot) -> Value {
    // Public lifecycle/CAS only. The retained observer's access to the Worker
    // does not publish private completion text or grant reviewer write rights.
    json!({
        "task_id":snapshot.execution.task_id,
        "state":snapshot.execution.state,
        "revision":snapshot.execution.revision,
        "completion_expected_revision":snapshot.completion_expected_revision,
        "result":snapshot.execution.terminal_result,"reason":snapshot.execution.terminal_reason
    })
}

pub(crate) fn read_worker_task_projection(
    db: &Database,
    current: &VerifiedAgentRun,
    attempt_id: &str,
) -> Result<Option<Value>, String> {
    // Obtain a fresh current-caller permit rather than exposing a retained
    // observer capability. Store admits the original caller and exact root
    // manager; the permit itself never crosses the model boundary.
    let permit = db
        .agent_cli_observation_permit(current, attempt_id)
        .map_err(|error| error.code().to_string())?;
    db.read_agent_cli_worker_task(&permit)
        .map(|snapshot| snapshot.as_ref().map(worker_task_projection))
        .map_err(|error| error.code().into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactAttempt {
    source_attempt_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogRequest {
    task_id: String,
}

pub(crate) fn tool_specs() -> Vec<Value> {
    let attempt = json!({"type":"object","properties":{"source_attempt_id":{"type":"string","minLength":1,"maxLength":128}},"required":["source_attempt_id"],"additionalProperties":false});
    vec![
        json!({"description":"Discover and invoke CLIs installed on the assigned Runner. Default execution_mode=local uses its user login/environment and durable native jobs; it requires the authorized project, not a CLI count quota, fixed product/version/hash approval or a frozen plan. This trusted local mode is not an OS filesystem sandbox. discover request:{task_id} lists installed CLIs and stable commands; models request:{task_id,cli_id} reads native model/effort options. An arbitrary command uses discover request:{project,commands:[name_or_absolute_path]}. start request:{project,cli_id,model?,effort?,instruction,idempotency_key,timeout_secs?} uses the discovered adapter, or {project,executable,args,stdin?,model?,effort?,idempotency_key,timeout_secs?} uses explicit native argv after inspecting actual CLI help. Omit model/effort for adapter-supported native defaults; explicit selections must be supported. Metadata is not account entitlement. status/cancel request:{call_id} observes/stops your original Job; status request:{} lists this Agent task receipts after a lost response or update. Preserve keys on unknown outcomes; validate output, not only exit code. Explicit execution_mode=isolated retains frozen team slot start and source_attempt_id status/cancel through the existing team attempt contract; null/omitted planned model uses an adapter-supported default.","inputSchema":{"properties":{"request":{"type":"object"},"action":{"type":"string","enum":["discover","models","start","status","cancel"]},"execution_mode":{"enum":["local","isolated"]}},"required":["action","request"],"type":"object","additionalProperties":false},"name":"cli"}),
        json!({"name":"team_cli_observe","description":"Purely read an authorized original CLI attempt, its worker task state/revision and exact executor Job. Use the current worker revision for an explicit delivery decision; terminal workers need no new decision, and completion_expected_revision is only for identical-key replay. This never starts, closes, reconciles or resends execution. Missing Runner state stays unknown even when a durable worker task is readable; task state is not physical-stop proof. Ordinary stdout/stderr, exit status and launch arguments are returned directly; no publication or review is required to read them.","inputSchema":attempt}),
        json!({"name":"team_cli_close","description":"Request a durable close of the original worker execution authorized by this attempt. A request or transport acknowledgment is not stopped proof. It does not refund the original caller slot or recreate a worker.","inputSchema":attempt}),
        json!({"name":"team_cli_delivery_collect","description":"Collect and persist the original stopped worker's explicitly published UTF-8 deliverables for review. Only the original caller or authorized root manager can request this effect. Existing published artifacts are reused; no private path, raw log or new CLI execution is accepted. This does not approve the work or complete any task. Use the original attempt ID to recover a missing delivery, then read and review its exact version; do not restart the CLI or change its key.","inputSchema":attempt}),
        json!({"name":"team_cli_catalog","description":"Read current Runner CLI models/efforts and separate Host profile status for this exact task. Expired Host profiles do not suppress CLI entries. Use default for Host settings or select a fresh explicit profile. Omitted providers have fixed reason codes in unavailable_providers; missing Runner capabilities/resources need local setup, not a replacement task or guessed model. Select explicit discovered values, or omit the frozen call model for an adapter-supported CLI default; configured_default source means model discovery was not used, not account verification. Catalog visibility is not account entitlement or provider execution readiness.","inputSchema":{"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"],"additionalProperties":false}}),
    ]
}

pub(crate) async fn call(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
    name: &str,
    arguments: Value,
) -> ToolResult {
    let Some(db) = runtime.isolation_database() else {
        return denied("agent_store_unavailable");
    };
    let access = match owner_access(db, binding) {
        Ok(value) => value,
        Err(code) => return denied(&code),
    };
    if crate::agent_team_delivery::supports(name) {
        return crate::agent_team_delivery::call(db, binding, name, arguments);
    }
    match name {
        "cli" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Request {
                action: String,
                request: Value,
                execution_mode: Option<String>,
            }
            let request: Request = match serde_json::from_value(arguments) {
                Ok(request) => request,
                Err(_) => return denied("agent_team_invalid_input"),
            };
            let isolated = match request.execution_mode.as_deref() {
                None | Some("local") => false,
                Some("isolated") => true,
                _ => return denied("agent_cli_execution_mode_invalid"),
            };
            if !isolated
                && request.action == "discover"
                && request.request.get("commands").is_some()
            {
                let mut native = request.request;
                let Some(object) = native.as_object_mut() else {
                    return denied("agent_team_invalid_input");
                };
                object.insert("action".into(), json!("discover"));
                return crate::tool_runtime::local_cli::call(runtime, binding, native).await;
            }
            if request.action == "discover" || request.action == "models" {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Query {
                    task_id: String,
                    cli_id: Option<String>,
                }
                let query: Query = match serde_json::from_value(request.request) {
                    Ok(query) => query,
                    Err(_) => return denied("agent_team_invalid_input"),
                };
                if request.action == "discover" && query.cli_id.is_some() {
                    return denied("agent_team_invalid_input");
                }
                if request.action == "models" && query.cli_id.is_none() {
                    if !isolated {
                        return denied("agent_cli_id_required");
                    }
                    return Box::pin(call(
                        runtime,
                        binding,
                        "team_cli_catalog",
                        json!({"task_id":query.task_id}),
                    ))
                    .await;
                }
                let provider = match query
                    .cli_id
                    .as_deref()
                    .map(AgentCliProvider::parse)
                    .transpose()
                {
                    Ok(provider) => provider,
                    Err(_) => return denied("agent_cli_invalid_provider_id"),
                };
                if let Err(error) = db.authorize_agent_assignment(
                    binding,
                    &query.task_id,
                    AgentAssignmentOperation::Read,
                ) {
                    return denied(error.code());
                }
                let Some(assets) = &runtime.agent_team_assets else {
                    return denied("agent_team_catalog_source_missing");
                };
                let inventory = if isolated {
                    assets.inventory(binding, provider).await
                } else {
                    assets.native_inventory(binding, provider).await
                };
                return match inventory {
                    Ok(inventory) => ToolResult::ok(inventory_projection(inventory)),
                    Err(reason) => inventory_unavailable(&reason),
                };
            }
            if !isolated {
                if request.action == "start" && request.request.get("cli_id").is_some() {
                    return crate::tool_runtime::local_cli::start_selected(
                        runtime,
                        binding,
                        request.request,
                    )
                    .await;
                }
                let action = match request.action.as_str() {
                    "start" => "start",
                    "status" => "observe",
                    "cancel" => "stop",
                    _ => return denied("agent_cli_action_invalid"),
                };
                let mut native = request.request;
                let Some(object) = native.as_object_mut() else {
                    return denied("agent_team_invalid_input");
                };
                if object.contains_key("action") {
                    return denied("agent_team_invalid_input");
                }
                object.insert("action".into(), json!(action));
                return crate::tool_runtime::local_cli::call(runtime, binding, native).await;
            }
            if request.action == "start" {
                let request: crate::agent_team::CliStart =
                    match serde_json::from_value(request.request) {
                        Ok(request) => request,
                        Err(_) => return denied("agent_team_invalid_input"),
                    };
                return start(runtime, db, binding, access, request, true).await;
            }
            let target = match request.action.as_str() {
                "models" => "team_cli_catalog",
                "start" => "team_cli_start",
                "status" => "team_cli_observe",
                "cancel" => "team_cli_close",
                _ => return denied("agent_cli_action_invalid"),
            };
            Box::pin(call(runtime, binding, target, request.request)).await
        }
        "team_cli_catalog" => {
            let request: CatalogRequest = match serde_json::from_value(arguments) {
                Ok(v) => v,
                Err(_) => return denied("agent_team_invalid_input"),
            };
            if let Err(error) = db.authorize_agent_assignment(
                binding,
                &request.task_id,
                AgentAssignmentOperation::Read,
            ) {
                return denied(error.code());
            }
            let Some(assets) = &runtime.agent_team_assets else {
                return denied("agent_team_catalog_source_missing");
            };
            match assets.catalog(binding).await {
                Ok(catalog) => match validate_catalog(db, &catalog, binding, now_ms()) {
                    Ok(()) => ToolResult::ok(catalog_projection(&catalog)),
                    Err(_) => catalog_unavailable("agent_team_catalog_projection_invalid"),
                },
                Err(reason) => catalog_unavailable(&reason),
            }
        }
        "team_cli_start" => {
            let request: crate::agent_team::CliStart = match serde_json::from_value(arguments) {
                Ok(v) => v,
                Err(_) => return denied("agent_team_invalid_input"),
            };
            start(runtime, db, binding, access, request, false).await
        }
        "team_agent_start" => {
            let request: crate::agent_team::AgentStart = match serde_json::from_value(arguments) {
                Ok(value) => value,
                Err(_) => return denied("agent_team_invalid_input"),
            };
            if let Err(code) = request.validate() {
                return denied(code);
            }
            // Exact replay must remain readable when external catalogs expire.
            // This Store lookup creates no slot, enrollment or dispatch grant.
            match db.lookup_agent_host_member(
                binding,
                &request.root_task_id,
                &request.source_task_id,
                &request.enrollment_id,
                &request.idempotency_key,
                &request.thinking_profile_id,
            ) {
                Ok(Some(queue)) => return member_queue_result(db, binding, queue),
                Ok(None) => {}
                Err(error) => return denied(error.code()),
            }
            if let Err(error) = db.authorize_agent_assignment(
                binding,
                &request.root_task_id,
                AgentAssignmentOperation::Delegate,
            ) {
                return denied(error.code());
            }
            // Host thinking options are optional UI settings. Assignment, plan,
            // identity and budget checks below remain the dispatch authority.
            // This real B2 transaction revalidates the current manager, exact
            // independently created Member enrollment and original frozen plan.
            // It reserves the existing queue; no grant is returned to the parent.
            match db.reserve_agent_host_member(
                binding,
                &request.root_task_id,
                &request.source_task_id,
                &request.enrollment_id,
                &request.idempotency_key,
                &request.thinking_profile_id,
            ) {
                Ok(queue) => member_queue_result(db, binding, queue),
                Err(error) => denied(error.code()),
            }
        }
        "team_cli_delivery_collect" => {
            let request: ExactAttempt = match serde_json::from_value(arguments) {
                Ok(value) => value,
                Err(_) => return denied("agent_team_invalid_input"),
            };
            if !bounded(&request.source_attempt_id, 128) {
                return denied("agent_team_invalid_input");
            }
            let permit = match db.agent_cli_observation_permit(binding, &request.source_attempt_id)
            {
                Ok(value) => value,
                Err(error) => return denied(error.code()),
            };
            if permit.binding().executor.run.agent_id == binding.agent_id() {
                return denied("agent_cli_delivery_reviewer_required");
            }
            // A persisted publication survives Registry restart and needs no
            // fresh Runner query. Approval still uses B's separate review gate.
            if let Ok(view) = db.read_agent_cli_delivery(binding, &request.source_attempt_id) {
                return ToolResult::ok(
                    json!({"delivery":view.delivery,"published":true,"replayed":true,"state_changed":false,"execution_started":false}),
                );
            }
            match collect_delivery(db, &runtime.runner_registry, &access, binding, &permit).await {
                Ok(reference) => ToolResult::ok(
                    json!({"delivery":reference,"published":true,"execution_started":false,"business_verified":false}),
                ),
                Err(code) => unresolved(&code, Some(&request.source_attempt_id)),
            }
        }
        "team_cli_observe" | "team_cli_close" => {
            let request: ExactAttempt = match serde_json::from_value(arguments) {
                Ok(v) => v,
                Err(_) => return denied("agent_team_invalid_input"),
            };
            let permit = match db.agent_cli_observation_permit(binding, &request.source_attempt_id)
            {
                Ok(v) => v,
                Err(error) if name == "team_cli_observe" => {
                    let receipt =
                        match db.read_agent_cli_attempt(binding, &request.source_attempt_id) {
                            Ok(receipt) => receipt,
                            Err(error) => return denied(error.code()),
                        };
                    if receipt.dispatch_state == TeamCliDispatchState::Reserved
                        && receipt.job_id.is_none()
                        && receipt.execution_state.is_none()
                    {
                        return ToolResult::ok(json!({"attempt":receipt,"job":null,
                            "preparation_state":"reserved_not_dispatched","execution_state":"not_started",
                            "dispatched":false,"execution_started":false,"state_changed":false,
                            "recovery":"Retry the original cli start request with the same task, call_index and idempotency_key. The original worker and attempt are retained."}));
                    }
                    return denied(error.code());
                }
                Err(error) => return denied(error.code()),
            };
            if name == "team_cli_close" {
                match close_executor(
                    db,
                    &runtime.runner_registry,
                    &access,
                    binding.owner_user(),
                    &permit,
                )
                .await
                {
                    Ok(progress) => ToolResult::ok(
                        json!({"close":progress,"stop_requested":true,"stopped_confirmed":progress.complete,"state_changed":true}),
                    ),
                    Err(code) => unresolved(&code, Some(&request.source_attempt_id)),
                }
            } else {
                let job = runtime
                    .runner_registry
                    .observe_agent_execution_output(&access, permit.binding())
                    .await;
                // Re-read through the current caller after the asynchronous
                // Registry lookup; an earlier retained permit is not authority
                // to reveal state to a caller revoked during the lookup.
                let receipt = match db.read_agent_cli_attempt(binding, &request.source_attempt_id) {
                    Ok(value) => value,
                    Err(error) => return denied(error.code()),
                };
                let worker_task =
                    match read_worker_task_projection(db, binding, &request.source_attempt_id) {
                        Ok(snapshot) => snapshot,
                        Err(code) => return unresolved(&code, Some(&request.source_attempt_id)),
                    };
                match job {
                    Ok(job) => ToolResult::ok(
                        json!({"attempt":receipt,"job":job,"worker_task":worker_task,"worker_task_missing":worker_task.is_none(),"runner_observation":"available","state_changed":false,"dispatched":false}),
                    ),
                    Err(code) => ToolResult::err_with_output(
                        &code,
                        json!({"error_kind":code,"attempt":receipt,"worker_task":worker_task,"worker_task_missing":worker_task.is_none(),"runner_observation":"unavailable","state_changed":false,"dispatched":false,"dispatch_certainty":"outcome_unknown","recovery":"Preserve the original attempt, key and worker. Persisted task state is not process-stop proof; delivery decisions independently require durable stopped-execution evidence."}),
                    ),
                }
            }
        }
        _ => denied("agent_team_operation_not_admitted"),
    }
}

fn member_queue_result(
    db: &Database,
    caller: &VerifiedAgentRun,
    queue: webcodex_store::AgentHostQueueBinding,
) -> ToolResult {
    match db.read_agent_host_dispatch(
        caller,
        webcodex_store::AgentHostQueueKind::Member,
        &queue.queue_id,
    ) {
        Ok(status) => ToolResult::ok(
            json!({"binding":queue,"status":status,"direct_start_performed":false,"business_verified":false}),
        ),
        Err(error) => unresolved(error.code(), None),
    }
}

async fn start(
    runtime: &ToolRuntime,
    db: &Database,
    caller: &VerifiedAgentRun,
    access: RunnerAccess,
    request: crate::agent_team::CliStart,
    check_installed_identity: bool,
) -> ToolResult {
    if !bounded(&request.idempotency_key, 128)
        || !bounded(&request.fallback_thinking_profile_id, 128)
    {
        return denied("agent_team_invalid_input");
    }
    if let Err(e) = db.authorize_agent_assignment(
        caller,
        &request.source_task_id,
        AgentAssignmentOperation::UseCli,
    ) {
        return denied(e.code());
    }
    // Existing slots are observed before consulting expiring external catalogs.
    let prior = match db.read_agent_cli_preparation(
        caller,
        &request.root_task_id,
        &request.source_task_id,
        request.call_index,
        &request.idempotency_key,
        &request.fallback_thinking_profile_id,
        Some(900),
    ) {
        Ok(value) => value,
        Err(error) => return denied(error.code()),
    };
    if let Some(prior) = &prior {
        if prior.phase == AgentCliPreparationPhase::Finalized {
            let existing = &prior.receipt;
            let permit = match db.agent_cli_observation_permit(caller, &existing.attempt_id) {
                Ok(value) => value,
                Err(_) => {
                    return ToolResult::err_with_output(
                        "agent_cli_preparation_reconciliation_required",
                        json!({"attempt":existing,"dispatched":false,"state_changed":false,"recovery":"The original unfinalized/cancelled preparation needs B's same-identity controller recovery. Do not allocate replacement target IDs or a worker."}),
                    )
                }
            };
            let b = permit.binding();
            // B's exact preparation read already checked the original key and
            // frozen request intent without refreshing an external catalog.
            // Only D2's opaque token for the exact existing in-epoch Job can
            // finish a known never-enqueued dispatch. Its original stored claim
            // is revalidated; an enqueued replay cannot send a second request.
            if let Ok(prepared) = runtime
                .runner_registry
                .read_prepared_agent_execution(&access, b)
                .await
            {
                if let Err(code) = runtime
                    .runner_registry
                    .dispatch_prepared_agent_execution(&access, &prepared, &StoreAdmission(db))
                    .await
                {
                    return unresolved(&code, Some(&existing.attempt_id));
                }
            }
            let result = observe(runtime, db, &access, caller.owner_user(), &permit).await;
            if result.output["reconciliation_complete"] != true {
                spawn_observer(
                    runtime.clone(),
                    caller.owner_user().into(),
                    access,
                    permit,
                    existing.created_at_unix_ms,
                    existing.timeout_secs.unwrap_or(900),
                );
            }
            return result;
        }
        if prior.phase == AgentCliPreparationPhase::Unprepared {
            return unresolved(
                "agent_cli_legacy_preparation_unavailable",
                Some(&prior.receipt.attempt_id),
            );
        }
    }
    let slot = match db.read_agent_cli_planned_slot(
        caller,
        &request.root_task_id,
        &request.source_task_id,
        request.call_index,
    ) {
        Ok(value) => value,
        Err(error) => return denied(error.code()),
    };
    let Some(assets) = &runtime.agent_team_assets else {
        return denied("agent_team_catalog_source_missing");
    };
    let selected_provider = match AgentCliProvider::parse(&slot.provider_id) {
        Ok(provider) => provider,
        Err(_) => return denied("agent_cli_invalid_provider_id"),
    };
    let catalog = match assets
        .selected_catalog(
            caller,
            selected_provider,
            request.fallback_thinking_profile_id != "default",
            slot.model.is_cli_default(),
        )
        .await
    {
        Ok(value) => value,
        Err(reason) => return catalog_unavailable(&reason),
    };
    if let Err(code) = validate_catalog(db, &catalog, caller, now_ms()) {
        return denied(&code);
    }
    if let Err(reason) =
        validate_host_selection(&catalog, &request.fallback_thinking_profile_id, false)
    {
        return catalog_unavailable(&reason);
    }
    let Some(provider) = catalog
        .providers
        .iter()
        .find(|p| p.provider_id == slot.provider_id)
    else {
        let mut result = denied("agent_cli_provider_not_discovered");
        if let Some(missing) = catalog
            .unavailable_providers
            .iter()
            .find(|entry| entry.provider_id == slot.provider_id)
        {
            result.output["reason_code"] = json!(missing.reason_code);
        }
        result.output["next_action"] = json!("Use cli discover/models to inspect this installation and its adapter; execution still requires this Runner's registered resources. Do not create a replacement task.");
        return result;
    };
    if !cli_selection_discovered(provider, &slot.model, slot.effort.as_deref()) {
        return denied("agent_cli_selection_not_discovered");
    }
    let controls = if let Some(host) = &provider.host_cli {
        if host.validate().is_err()
            || host.adapter.id != provider.provider_id
            || host.executable_sha256 != provider.executable_sha256
            || host.manifest(&provider.resource_set_id).ok().as_deref()
                != Some(&provider.resource_manifest_hash)
            || provider.controls.is_some()
        {
            return denied("native_cli_resource_binding_mismatch");
        }
        None
    } else {
        match require_provider_controls(provider) {
            Ok(controls) => Some(controls),
            Err(code) => return denied(&code),
        }
    };
    let selected_adapter = provider
        .host_cli
        .as_ref()
        .map(|host| &host.adapter)
        .or_else(|| controls.and_then(|control| control.adapter.as_ref()));
    // A default selection needs no model-list round trip. The separately
    // authenticated exact resource/controls below and Runner image pins still
    // verify what will execute before any Worker/slot mutation.
    if check_installed_identity && !slot.model.is_cli_default() {
        let selection = match AgentCliProvider::parse(&provider.provider_id) {
            Ok(selection) => selection,
            Err(_) => return denied("agent_cli_invalid_provider_id"),
        };
        let inventory = match assets.inventory(caller, Some(selection)).await {
            Ok(inventory) => inventory,
            Err(_) => return denied("agent_cli_installation_unavailable"),
        };
        let Some(capabilities) = inventory.capabilities else {
            return denied("agent_cli_capabilities_missing");
        };
        if !capabilities.installed || capabilities.status != "ready" {
            return denied("agent_cli_installation_unavailable");
        }
        if inventory.target != catalog.target
            || capabilities.executable_identity != format!("sha256:{}", provider.executable_sha256)
            || capabilities.version != provider.version
        {
            return denied("agent_cli_installation_changed");
        }
        let current_selection = ObservedCliProvider {
            models: capabilities.models,
            efforts: capabilities.efforts,
            model_efforts: capabilities.model_efforts,
            ..provider.clone()
        };
        if !cli_selection_discovered(&current_selection, &slot.model, slot.effort.as_deref()) {
            return denied("agent_cli_selection_not_discovered");
        }
    }
    // Actual resource facts come from authenticated Runner registration, not
    // from a model argument or a hand-written hash/path in this adapter.
    let resources = match runtime
        .runner_registry
        .read_agent_execution_resource(&access, &catalog.target, &provider.resource_set_id)
        .await
    {
        Ok(value) => value,
        Err(code) => return denied(&code),
    };
    if resources.validate().is_err()
        || resources.resource_set_id != provider.resource_set_id
        || resources.manifest_hash != provider.resource_manifest_hash
        || resources.executable_identity != format!("sha256:{}", provider.executable_sha256)
        || resources.provider.as_ref().map(|p| p.id()) != Some(provider.provider_id.as_str())
        || resources.host_cli != provider.host_cli
        || resources.reviewed_controls.as_ref() != controls
    {
        return denied("agent_cli_resource_binding_mismatch");
    }
    // Validate the exact wire-sized input before reserving any Worker/slot.
    let workspace_source =
        match cli_workspace_source(runtime, db, caller, &access, &catalog.target).await {
            Ok(source) => source,
            Err(code) => return denied(&code),
        };
    if workspace_source.is_some()
        && !resources.project_workspace_copy
        && resources.host_cli.is_none()
    {
        return denied("agent_workspace_copy_unavailable");
    }
    // Enrollment canonicalizes input labels; preview that same ordering.
    let mut input_preview: Vec<_> = slot
        .inputs
        .iter()
        .map(|input| webcodex_store::AgentEnrollmentSnapshot {
            name: input.name.clone(),
            content: input.content.clone(),
            sha256: webcodex_core::agent_cli::sha256(input.content.as_bytes()),
        })
        .collect();
    input_preview.sort_by(|a, b| a.name.cmp(&b.name));
    let preview = match if provider.host_cli.is_some() {
        cli_native_input_prompt(&slot.instruction, &input_preview)
    } else {
        cli_worker_input_prompt(&slot.instruction, &input_preview, workspace_source.as_ref())
    } {
        Ok(value) => value,
        Err(code) => return denied(&code),
    };
    let (_, _, wire_input) = match crate::tool_runtime::cli_process_with_adapter(
        selected_adapter,
        &slot.provider_id,
        &slot.model,
        slot.effort.as_deref(),
        &preview,
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if wire_input.len() > webcodex_core::runner_protocol::PROCESS_STDIN_MAX_BYTES
        || wire_input.contains('\0')
    {
        return denied("agent_cli_input_too_large");
    }
    let make_request = |target, carrier| AgentCliPreparationRequest {
        root_task_id: request.root_task_id.clone(),
        source_task_id: request.source_task_id.clone(),
        call_index: request.call_index,
        idempotency_key: request.idempotency_key.clone(),
        fallback_thinking_profile_id: request.fallback_thinking_profile_id.clone(),
        timeout_secs: Some(900),
        target,
        carrier,
    };
    let (prepared, reservation) = if let Some(prior) = prior {
        let Some(identity) = prior.binding else {
            return unresolved(
                "agent_cli_preparation_identity_missing",
                Some(&prior.receipt.attempt_id),
            );
        };
        if identity.target != catalog.target {
            return unresolved(
                "agent_team_catalog_target_changed",
                Some(&prior.receipt.attempt_id),
            );
        }
        let result = if prior.phase == AgentCliPreparationPhase::Cancelled {
            let Some(fence) = prior.resume_fence else {
                return unresolved(
                    "agent_cli_preparation_resume_fence_missing",
                    Some(&prior.receipt.attempt_id),
                );
            };
            // B alone proves never-dispatched cancellation and binds the
            // successor runs to this original slot, task and logical Worker.
            db.resume_cancelled_agent_cli_preparation(caller, &prior.receipt.attempt_id, &fence)
        } else {
            db.prepare_agent_cli_worker(caller, make_request(identity.target, identity.carrier))
        };
        match result {
            Ok(value) => (value, None),
            Err(error) => return unresolved(error.code(), Some(&prior.receipt.attempt_id)),
        }
    } else {
        let reservation = match runtime
            .runner_registry
            .reserve_agent_execution_target(&access, &catalog.target.client_id)
            .await
        {
            Ok(value) => value,
            Err(code) => return denied(&code),
        };
        if reservation.target() != &catalog.target {
            return denied("agent_team_catalog_target_changed");
        }
        let carrier = AgentExecutionCarrier::Job {
            job_id: reservation.job_id().into(),
            start_request_id: reservation.start_request_id().into(),
        };
        match db
            .prepare_agent_cli_worker(caller, make_request(reservation.target().clone(), carrier))
        {
            Ok(value) => (value, Some(reservation)),
            Err(error) => return denied(error.code()),
        }
    };
    let original_attempt = prepared.receipt.attempt_id.clone();
    let Some(preparation) = prepared.preparation else {
        return unresolved(
            "agent_cli_preparation_not_dispatchable",
            Some(&original_attempt),
        );
    };
    let worker = match db.read_agent_enrollment(preparation.worker()) {
        Ok(v) => v,
        Err(e) => return unresolved(e.code(), Some(&original_attempt)),
    };
    let (_, args, stdin) = match crate::tool_runtime::cli_process_with_adapter(
        selected_adapter,
        &prepared.receipt.provider_id,
        &prepared.receipt.model,
        prepared.receipt.effort.as_deref(),
        &match if provider.host_cli.is_some() {
            cli_native_input_prompt(&worker.brief, &worker.inputs)
        } else {
            cli_worker_input_prompt(&worker.brief, &worker.inputs, workspace_source.as_ref())
        } {
            Ok(prompt) => prompt,
            Err(code) => return unresolved(&code, Some(&original_attempt)),
        },
    ) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let args = if let Some(controls) = controls {
        match compose_reviewed_cli_argv(controls, &args) {
            Ok(args) => args,
            Err(code) => return unresolved(&code, Some(&original_attempt)),
        }
    } else {
        if let Err(code) = provider.host_cli.as_ref().unwrap().validate_args(&args) {
            return unresolved(&code, Some(&original_attempt));
        }
        args
    };
    let process = AgentExecutionProcess {
        executable: resources.executable_identity,
        args,
    };
    let mut identity = preparation.binding_identity().clone();
    identity.launch_intent_hash = match compute_workspace_launch_intent_hash(
        &process,
        Some(&stdin),
        900,
        workspace_source.as_ref(),
    ) {
        Ok(v) => v,
        Err(_) => return unresolved("agent_cli_intent_invalid", Some(&original_attempt)),
    };
    identity.resource_manifest_hash = resources.manifest_hash;
    let start = AgentExecutionStart {
        workspace_source,
        binding: identity,
        resource_set_id: resources.resource_set_id,
        process,
        stdin: Some(stdin),
        timeout_secs: 900,
    };
    // D2 preparation is inert. Only the following B transaction can commit
    // the exact claim that D2 revalidates before its first queue insertion.
    let registered = match runtime
        .runner_registry
        .read_prepared_agent_execution(&access, &start.binding)
        .await
    {
        Ok(value) => value,
        Err(_) => {
            let reservation = match reservation {
                Some(value) => value,
                None => match runtime
                    .runner_registry
                    .recover_agent_execution_target_reservation(
                        &access,
                        &StorePreparationEvidence {
                            db,
                            caller,
                            identity: preparation.binding_identity(),
                        },
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(code) => return unresolved(&code, Some(&original_attempt)),
                },
            };
            match runtime
                .runner_registry
                .prepare_agent_execution(
                    &access,
                    &reservation,
                    start.clone(),
                    caller.agent_id().into(),
                )
                .await
            {
                Ok(value) => value,
                Err(code) => {
                    return ToolResult::err_with_output(
                        &code,
                        json!({
                            "error_kind":code,"source_attempt_id":original_attempt,"dispatched":false,
                            "execution_started":false,"execution_state":"not_started",
                            "dispatch_certainty":"not_started","preparation_persisted":true,
                            "recovery":"Retry the original request and idempotency_key after fixing this preparation error; do not create another worker or attempt."
                        }),
                    )
                }
            }
        }
    };
    if let Err(error) = db.start_agent_public_task(
        preparation.worker(),
        &worker.task_id,
        &format!("cli-task-{original_attempt}"),
    ) {
        return unresolved(error.code(), Some(&original_attempt));
    }
    let permit = match db.finalize_agent_cli_execution(caller, preparation, start) {
        Ok(v) => v,
        Err(e) => return unresolved(e.code(), Some(&original_attempt)),
    };
    let observer = permit.observation_permit(); // retain BEFORE consuming dispatch permission
    let committed = permit.into_start();
    if registered.binding() != &committed.binding {
        return unresolved(
            "agent_cli_committed_binding_mismatch",
            Some(&original_attempt),
        );
    }
    let dispatched = match runtime
        .runner_registry
        .dispatch_prepared_agent_execution(&access, &registered, &StoreAdmission(db))
        .await
    {
        Ok(v) => v,
        Err(code) => return unresolved(&code, Some(&original_attempt)),
    };
    let attempt = match db.bind_agent_cli_execution_job(&observer, &observer.binding().target) {
        Ok(v) => v,
        Err(e) => return unresolved(e.code(), Some(&original_attempt)),
    };
    spawn_observer(
        runtime.clone(),
        caller.owner_user().into(),
        access,
        observer,
        attempt.created_at_unix_ms,
        attempt.timeout_secs.unwrap_or(900),
    );
    ToolResult::ok(
        json!({"source_attempt_id":original_attempt,"dispatched":dispatched.dispatched,"replayed":dispatched.replayed,
        "job":job_projection(&dispatched.job),"worker_task_id":worker.task_id,"worker_agent_id":worker.agent_id,"business_verified":false}),
    )
}

fn job_projection(job: &ShellJobInfo) -> Value {
    json!({"job_id":job.job_id,"status":job.status,"execution_state":job.command_execution_state,
        "exit_code":job.exit_code,"error":job.error,"cwd":job.cwd,"duration_ms":job.duration_ms,
        "stdout_log_truncated":job.stdout_log_truncated,"stderr_log_truncated":job.stderr_log_truncated,
        "business_verified":false})
}

async fn close_executor(
    db: &Database,
    registry: &RunnerRegistry,
    access: &RunnerAccess,
    owner: &str,
    permit: &AgentCliObservationPermit,
) -> Result<AgentExecutionCloseProgress, String> {
    let b = permit.binding();
    let plan = db
        .begin_agent_execution_close(
            owner,
            &b.project,
            &b.executor.run,
            now_ms().saturating_add(30_000),
        )
        .map_err(|e| e.code().to_string())?;
    if !plan.blocked_attempt_ids().is_empty() {
        return Err("agent_execution_close_blocked".into());
    }
    let progress = db
        .agent_execution_close_status(owner, &b.project, &b.executor.run)
        .map_err(|e| e.code().to_string())?;
    if progress.complete {
        return Ok(progress);
    }
    for request in plan.requests() {
        registry
            .enqueue_agent_execution_close(
                access,
                request.clone(),
                close_request_id(request),
                "agent-team-controller".into(),
            )
            .await?;
    }
    db.agent_execution_close_status(owner, &b.project, &b.executor.run)
        .map_err(|e| e.code().into())
}

fn close_request_id(close: &webcodex_core::runner_protocol::AgentExecutionClose) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"webcodex.agent-team.close-request.v1\0");
    hash.update(close.close_ref.close_id.as_bytes());
    hash.update(b"\0");
    hash.update(close.target.client_id.as_bytes());
    hash.update(b"\0");
    hash.update(close.target.runner_instance_id.as_bytes());
    format!("wc_close_{:x}", hash.finalize())
}

async fn observe(
    runtime: &ToolRuntime,
    db: &Database,
    access: &RunnerAccess,
    owner: &str,
    permit: &AgentCliObservationPermit,
) -> ToolResult {
    let b = permit.binding();
    let job = match runtime
        .runner_registry
        .observe_agent_execution(access, b)
        .await
    {
        Ok(v) => v,
        Err(code) => return unresolved(&code, Some(&b.requester.cli_attempt_id)),
    };
    let attempt = match db.bind_agent_cli_execution_job(permit, &b.target) {
        Ok(v) => v,
        Err(e) => return unresolved(e.code(), Some(&b.requester.cli_attempt_id)),
    };
    let terminal = crate::tool_runtime::agent_cli_job_terminal(&job.status);
    if !terminal {
        return ToolResult::ok(
            json!({"attempt_id":attempt.attempt_id,"job":job_projection(&job),"dispatched":false,"terminal":false}),
        );
    }
    let close = match close_executor(db, &runtime.runner_registry, access, owner, permit).await {
        Ok(v) => v,
        Err(code) => return unresolved(&code, Some(&attempt.attempt_id)),
    };
    if !close.complete {
        return ToolResult::ok(
            json!({"attempt_id":attempt.attempt_id,"job":job_projection(&job),"close":close,"terminal":false,"execution_state":"outcome_unknown","dispatched":false}),
        );
    }
    let known = !matches!(job.status.as_str(), "lost" | "recovering")
        && matches!(
            job.command_execution_state,
            Some(
                ShellCommandExecutionState::Completed
                    | ShellCommandExecutionState::TimedOut
                    | ShellCommandExecutionState::NotStarted
            )
        );
    let stdout = match runtime
        .runner_registry
        .hidden_job_log_for_auth(Some(access), &job.job_id, Some(64))
        .await
    {
        Ok((_, stdout, _, _, _)) => stdout.unwrap_or_default(),
        Err(_) => String::new(),
    };
    let code = if attempt.dispatch_state == TeamCliDispatchState::Terminal {
        // A terminal receipt owns the frozen classification. Logs can expire;
        // replay must not turn a recorded quota failure into provider_failed.
        attempt.error_code.clone()
    } else if matches!(job.status.as_str(), "stopped" | "cancelled") {
        Some("user_cancelled".into())
    } else if !known {
        Some("observation_unknown".into())
    } else if job.command_execution_state == Some(ShellCommandExecutionState::NotStarted) {
        job.error.as_deref().map(|error| {
            let lower = error.to_ascii_lowercase();
            if [
                "permission",
                "denied",
                "sandbox",
                "isolation",
                "not allowed",
            ]
            .iter()
            .any(|word| lower.contains(word))
            {
                "permission_denied".into()
            } else {
                crate::tool_runtime::agent_cli_process_failure(error).into()
            }
        })
    } else {
        crate::tool_runtime::structured_failure_code(&attempt.provider_id, &stdout).or_else(|| {
            (job.exit_code != Some(0) || job.status != "completed" || job.error.is_some())
                .then(|| "provider_failed".into())
        })
    };
    let reconciled = if attempt.dispatch_state == TeamCliDispatchState::Terminal {
        attempt
    } else {
        match db.observe_agent_cli_execution(
            permit,
            &b.target,
            attempt.observation_revision,
            TeamCliObservation {
                job_id: Some(job.job_id.clone()),
                dispatch_state: if known {
                    TeamCliDispatchState::Terminal
                } else {
                    TeamCliDispatchState::Unknown
                },
                execution_state: if known {
                    job.command_execution_state
                } else {
                    Some(ShellCommandExecutionState::OutcomeUnknown)
                },
                error_code: code.clone(),
                result_summary: Some(
                    "Exact CLI process observation; stdout/stderr are directly available through cli status.".into(),
                ),
            },
        ) {
            Ok(v) => v,
            Err(e) => return unresolved(e.code(), Some(&b.requester.cli_attempt_id)),
        }
    };
    let successful_process = known
        && job.status == "completed"
        && job.error.is_none()
        && job.command_execution_state == Some(ShellCommandExecutionState::Completed)
        && job.exit_code == Some(0)
        && code.is_none();
    let business_review_required = false;
    let worker_completion = if known {
        Some(match db.read_agent_cli_worker_task(permit) {
            Ok(Some(snapshot))
                if matches!(
                    snapshot.execution.state,
                    AgentTaskState::Succeeded | AgentTaskState::Failed
                ) =>
            {
                // Replay reads the committed public decision rather than
                // completing it again with a new revision or changed reason.
                json!({"task_id":b.executor.task_id,"state":snapshot.execution.state,"replayed":true,"state_changed":false})
            }
            Ok(Some(snapshot)) => match db.complete_agent_cli_worker_task(
                permit,
                snapshot.completion_expected_revision,
                if successful_process { AgentTaskState::Succeeded } else { AgentTaskState::Failed },
                successful_process.then_some("CLI process and native result protocol completed successfully; task acceptance remains separate."),
                code.as_deref(),
                &format!("cli-worker-finish-{}", b.requester.cli_attempt_id),
            ) {
                Ok(value) => {
                    json!({"task_id":value.task.task_id,"state":value.task.state,"replayed":value.replayed})
                }
                Err(error) => json!({"error_kind":error.code(),"verified":false}),
            },
            Ok(None) => json!({"error_kind":"public_task_execution_missing","verified":false}),
            Err(error) => json!({"error_kind":error.code(),"verified":false}),
        })
    } else {
        None
    };
    // B derives the original owner/slot from this retained opaque permit and
    // checks exact Job plus durable stopped proof in the same transaction.
    // E consumes the original recovery queue; this observer cannot open chats.
    let recovery_enqueue = (code.as_deref() != Some("user_cancelled"))
        .then(|| crate::tool_runtime::agent_cli_failure_reason(&job, code.as_deref()))
        .flatten()
        .map(|reason| {
            match db.reserve_agent_cli_fallback(
                permit,
                &b.target,
                &job,
                reason,
                webcodex_core::cli_fallback::CliCancellationOrigin::None,
            ) {
                Ok(value) => json!({"success":true,"reservation":value,"dispatched":false}),
                Err(error) => json!({"success":false,"error_kind":error.code(),"dispatched":false}),
            }
        });
    // Optional archival delivery is independent of ordinary process results.
    let delivery_publication: Option<Value> = None;
    let reconciliation_complete = known
        && worker_completion
            .as_ref()
            .is_some_and(|value| value.get("error_kind").is_none())
        && recovery_enqueue
            .as_ref()
            .map_or(true, |value| value["success"] == true)
        && delivery_publication
            .as_ref()
            .map_or(true, |value| value["success"] == true);
    ToolResult::ok(
        json!({"attempt_id":reconciled.attempt_id,"job":job_projection(&job),"terminal":known,"reconciliation_complete":reconciliation_complete,"business_review_required":business_review_required,"dispatched":false,"close":close,"worker_completion":worker_completion,"recovery_enqueue":recovery_enqueue,"delivery_publication":delivery_publication,"business_verified":false}),
    )
}

struct ObserverGuard {
    active: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    key: String,
}
impl Drop for ObserverGuard {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

fn spawn_observer(
    runtime: ToolRuntime,
    owner: String,
    access: RunnerAccess,
    permit: AgentCliObservationPermit,
    created_at: i64,
    timeout: u64,
) {
    let active = runtime.agent_team_watchers();
    let key = format!("isolated:{}", permit.binding().requester.cli_attempt_id);
    if !active
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key.clone())
    {
        return;
    }
    let guard = ObserverGuard { active, key };
    tokio::spawn(async move {
        let _guard = guard;
        let remaining = timeout
            .saturating_add(70)
            .saturating_mul(1000)
            .saturating_sub(now_ms().saturating_sub(created_at).max(0) as u64);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(remaining);
        loop {
            let Some(db) = runtime.isolation_database() else {
                return;
            };
            let result = observe(&runtime, db, &access, &owner, &permit).await;
            if result.output["reconciliation_complete"] == true {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                let b = permit.binding();
                // Only the retained immutable observer permit may update this
                // original attempt after revocation; it never permits a send.
                if let Ok(attempt) = db.bind_agent_cli_execution_job(&permit, &b.target) {
                    if attempt.dispatch_state != TeamCliDispatchState::Terminal {
                        let _ = db.observe_agent_cli_execution(
                            &permit,
                            &b.target,
                            attempt.observation_revision,
                            TeamCliObservation {
                                job_id: attempt.job_id,
                                dispatch_state: TeamCliDispatchState::Unknown,
                                execution_state: Some(ShellCommandExecutionState::OutcomeUnknown),
                                error_code: Some("observation_unknown".into()),
                                result_summary: None,
                            },
                        );
                    }
                }
                return;
            }
            tokio::time::sleep_until(std::cmp::min(
                deadline,
                tokio::time::Instant::now() + Duration::from_secs(1),
            ))
            .await;
        }
    });
}

#[cfg(test)]
#[path = "agent_team_execution_tests.rs"]
mod tests;
