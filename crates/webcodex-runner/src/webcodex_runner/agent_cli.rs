//! Narrow, typed Agent CLI metadata and explicit artifact projection.
//! This does not grant execution, inspect accounts, or return raw CLI output.
use super::{CommandResult, RunnerSink};
use std::path::Path;
use std::time::{Duration, Instant};
#[cfg(test)]
use webcodex_core::agent_cli::AgentCliProvider;
use webcodex_core::agent_cli::{AgentCliCatalog, AgentCliCatalogQuery, AGENT_CLI_CATALOG_TTL_MS};
use webcodex_core::runner_protocol::AgentExecutionResourceDescriptor;
use webcodex_process::cli_catalog::{CliCatalog, Provider, TrustedMetadataContext};

pub(crate) fn discover_inventory(
    sink: &RunnerSink,
    query: &webcodex_core::agent_cli::AgentCliInventoryQuery,
    adapters: &[webcodex_core::cli_adapter::CliAdapter],
) -> Result<webcodex_core::agent_cli::AgentCliInventory, String> {
    use webcodex_core::agent_cli::{AgentCliInstallation, AgentCliInventory};
    query.validate()?;
    validate_target(
        sink,
        &query.target.client_id,
        &query.target.runner_instance_id,
    )?;
    let deadline = ControlDeadline::new(query.deadline_at_ms)?;
    let discovered = webcodex_process::cli_registry::discover_cli_installations(adapters)?;
    let capabilities = if let Some(provider_id) = query.provider {
        let mut definitions = webcodex_process::cli_registry::configured_cli_adapters()?;
        for adapter in adapters {
            if let Some(existing) = definitions.iter_mut().find(|entry| entry.id == adapter.id) {
                *existing = adapter.clone();
            } else {
                definitions.push(adapter.clone());
            }
        }
        let adapter = definitions
            .iter()
            .find(|entry| entry.id == provider_id.id())
            .ok_or("agent_cli_adapter_unavailable")?;
        let installation = discovered
            .iter()
            .find(|entry| entry.provider_id == provider_id.id())
            .ok_or("agent_cli_not_installed")?;
        if !installation.native {
            return Err("agent_cli_launcher_requires_adapter".into());
        }
        let pinned = super::jobs::agent_resources::pin_discovered_catalog_executable(
            &installation.executable,
            provider_id,
        )?;
        let proxy = super::shell::native_metadata_system_proxy();
        let context = TrustedMetadataContext::for_current_user(
            Provider::for_adapter(adapter),
            &pinned.program,
            proxy.as_deref(),
        )
        .map_err(|_| "agent_cli_metadata_context_unavailable")?;
        let raw = probe_metadata(adapter, &pinned.program, &context, &deadline)?;
        Some(webcodex_core::agent_cli::AgentCliCapabilities {
            adapter: Some(adapter.clone()),
            executable_identity: pinned.descriptor.executable_identity.clone(),
            provider_id: raw.provider_id,
            label: raw.label,
            installed: raw.installed,
            version: raw.version,
            models: raw.models,
            efforts: raw.efforts,
            source: raw.source.into(),
            status: raw.status.into(),
            message: raw.message,
            account_availability_verified: false,
        })
    } else {
        None
    };
    deadline.require(Duration::ZERO)?;
    let now = chrono::Utc::now().timestamp_millis();
    let inventory = AgentCliInventory {
        capabilities,
        version: 1,
        target: query.target.clone(),
        observed_at_ms: now,
        expires_at_ms: now + AGENT_CLI_CATALOG_TTL_MS,
        installations: discovered
            .into_iter()
            .map(|entry| AgentCliInstallation {
                command: entry.command,
                provider_id: entry.provider_id,
                label: entry.label,
                installed: entry.installed,
                native: entry.native,
                adapter_available: entry.adapter_available,
                status: entry.status.into(),
                message: entry.message.into(),
            })
            .collect(),
    };
    inventory.validate_for(query, now)?;
    Ok(inventory)
}

#[cfg(test)]
#[path = "agent_cli_tests.rs"]
mod tests;

pub(crate) struct ControlDeadline {
    start: Instant,
    wall_start_ms: i64,
    deadline_at_ms: i64,
}

impl ControlDeadline {
    pub(crate) fn new(deadline_at_ms: i64) -> Result<Self, String> {
        let value = Self {
            start: Instant::now(),
            wall_start_ms: chrono::Utc::now().timestamp_millis(),
            deadline_at_ms,
        };
        value.require(Duration::ZERO)?;
        Ok(value)
    }

    fn now_ms(&self) -> i64 {
        let elapsed = self.start.elapsed().as_millis().min(i64::MAX as u128) as i64;
        chrono::Utc::now()
            .timestamp_millis()
            .max(self.wall_start_ms.saturating_add(elapsed))
    }

    pub(crate) fn require(&self, remaining: Duration) -> Result<(), String> {
        let required = remaining.as_millis().min(i64::MAX as u128) as i64;
        let left = self.deadline_at_ms.saturating_sub(self.now_ms());
        if left <= 0 || left < required {
            Err("agent_cli_control_deadline_expired".into())
        } else {
            Ok(())
        }
    }
}

pub(crate) fn validate_target(
    sink: &RunnerSink,
    client_id: &str,
    instance_id: &str,
) -> Result<(), String> {
    if client_id != sink.client_id() || instance_id != sink.runner_instance_id() {
        Err("agent_cli_target_mismatch".into())
    } else {
        Ok(())
    }
}

fn adapter_for(
    resource: &AgentExecutionResourceDescriptor,
) -> Result<webcodex_core::cli_adapter::CliAdapter, String> {
    if let Some(adapter) = resource
        .reviewed_controls
        .as_ref()
        .and_then(|record| record.adapter.as_ref())
    {
        return Ok(adapter.clone());
    }
    let provider = resource
        .provider
        .ok_or("agent_cli_provider_mapping_mismatch")?;
    webcodex_core::cli_adapter::cli_adapters(&[])?
        .into_iter()
        .find(|adapter| adapter.id == provider.id())
        .ok_or("agent_cli_adapter_unavailable".into())
}

fn project_catalog(
    query: &AgentCliCatalogQuery,
    resource: &AgentExecutionResourceDescriptor,
    raw: CliCatalog,
    deadline: &ControlDeadline,
) -> Result<AgentCliCatalog, String> {
    deadline.require(Duration::ZERO)?;
    let expected_source = match adapter_for(resource)?.metadata {
        webcodex_core::cli_adapter::CliMetadataRecipe::AntigravityModels => "live_catalog",
        _ => "installed_help",
    };
    if raw.provider_id != query.provider.id()
        || raw.account_availability_verified
        || !matches!(raw.status, "ready" | "error" | "unavailable")
        || !(raw.source == "unavailable" || raw.source == expected_source)
        || (raw.status == "ready" && (!raw.installed || raw.source != expected_source))
        || (!raw.installed && raw.status != "unavailable")
    {
        return Err("agent_cli_helper_identity_or_state_mismatch".into());
    }
    let observed_at_ms = raw
        .checked_at
        .parse::<i64>()
        .map_err(|_| "agent_cli_helper_timestamp_invalid")?;
    let now = deadline.now_ms();
    if observed_at_ms <= 0 || observed_at_ms > now || observed_at_ms < deadline.wall_start_ms {
        return Err("agent_cli_helper_timestamp_invalid".into());
    }
    let catalog = AgentCliCatalog {
        version: 1,
        target: query.target.clone(),
        resource_set_id: resource.resource_set_id.clone(),
        resource_manifest_hash: resource.manifest_hash.clone(),
        executable_identity: resource.executable_identity.clone(),
        provider: query.provider,
        cli_version: raw.version,
        models: raw.models,
        efforts: raw.efforts,
        source: raw.source.into(),
        status: raw.status.into(),
        message: raw.message,
        observed_at_ms,
        expires_at_ms: observed_at_ms
            .checked_add(AGENT_CLI_CATALOG_TTL_MS)
            .ok_or("agent_cli_helper_timestamp_invalid")?,
        account_availability_verified: false,
        nested_delegation_verified_disabled: false,
    };
    catalog.validate_for(query, resource)?;
    deadline.require(Duration::ZERO)?;
    Ok(catalog)
}

/// The caller retains the verified executable/ancestor handles for this entire
/// fixed metadata probe, including its own bounded process cleanup.
pub(crate) fn query_catalog(
    query: &AgentCliCatalogQuery,
    resource: &AgentExecutionResourceDescriptor,
    program: &Path,
    entrypoint: Option<&Path>,
    codebuddy_config_directory: Option<&Path>,
    deadline: &ControlDeadline,
) -> Result<AgentCliCatalog, String> {
    query.validate()?;
    resource.validate()?;
    if resource.provider != Some(query.provider) {
        return Err("agent_cli_provider_mapping_mismatch".into());
    }
    // Do not cancel the shared helper half-way through its cleanup or borrow a
    // business CLI timeout. The Registry control budget must fit both phases.
    deadline.require(Duration::from_secs(33))?;
    let adapter = adapter_for(resource)?;
    let provider = Provider::for_adapter(&adapter);
    let proxy = super::shell::native_metadata_system_proxy();
    let node_mapping = resource.reviewed_controls.as_ref().is_some_and(|control| {
        control.loading_method == webcodex_core::agent_cli_controls::CODEBUDDY_NODE_FILES_LOADING
    });
    if node_mapping != entrypoint.is_some() || node_mapping != codebuddy_config_directory.is_some()
    {
        return Err("agent_cli_metadata_entrypoint_mismatch".into());
    }
    let context = if let Some(entrypoint) = entrypoint {
        if provider != Provider::WorkBuddy
            || !resource.reviewed_controls.as_ref().is_some_and(|control| {
                control.loading_method
                    == webcodex_core::agent_cli_controls::CODEBUDDY_NODE_FILES_LOADING
            })
        {
            return Err("agent_cli_metadata_entrypoint_mismatch".into());
        }
        let config = codebuddy_config_directory.ok_or("agent_cli_metadata_entrypoint_mismatch")?;
        TrustedMetadataContext::for_codebuddy_node(program, entrypoint, config)
    } else {
        TrustedMetadataContext::for_current_user(provider, program, proxy.as_deref())
    }
    .map_err(|_| "agent_cli_metadata_context_unavailable")?;
    deadline.require(Duration::from_secs(33))?;
    let raw = probe_metadata(&adapter, program, &context, deadline)?;
    project_catalog(query, resource, raw, deadline)
}

fn probe_metadata(
    adapter: &webcodex_core::cli_adapter::CliAdapter,
    program: &Path,
    context: &TrustedMetadataContext,
    deadline: &ControlDeadline,
) -> Result<CliCatalog, String> {
    let raw = std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("agent-cli-metadata".into())
            .spawn_scoped(scope, || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "agent_cli_metadata_runtime_unavailable".to_owned())?;
                // Deadline is checked on the thread immediately before starting
                // the fixed helper, so scheduler delay cannot consume its budget.
                deadline.require(Duration::from_secs(33))?;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    runtime.block_on(webcodex_process::cli_catalog::probe_cli_adapter(
                        &adapter, program, &context,
                    ))
                }))
                .map_err(|_| "agent_cli_metadata_worker_failed".to_owned())
                .and_then(|result| {
                    result.map_err(|_| "agent_cli_metadata_probe_failed".to_owned())
                });
                // abort() cannot interrupt a running spawn_blocking pipe read. A
                // failed helper must not turn Runtime::drop into an unbounded wait.
                // This bounds the adapter; it does not certify an unfinished reader
                // as stopped. Helper cleanup failures remain error, never ready.
                runtime.shutdown_timeout(Duration::from_millis(100));
                result
            })
            .map_err(|_| "agent_cli_metadata_worker_unavailable".to_owned())?;
        worker
            .join()
            .map_err(|_| "agent_cli_metadata_worker_failed".to_owned())?
    })?;
    Ok(raw)
}

pub(crate) fn bounded_json_result<T: serde::Serialize>(
    result: Result<T, String>,
    max_bytes: usize,
) -> CommandResult {
    let result = result.and_then(|value| {
        let text = serde_json::to_string(&value)
            .map_err(|_| "agent_cli_response_encoding_failed".to_owned())?;
        if text.len() > max_bytes {
            return Err("agent_cli_response_too_large".into());
        }
        Ok(text)
    });
    match result {
        Ok(stdout) => CommandResult {
            exit_code: Some(0),
            stdout: Some(stdout),
            stderr: None,
            duration_ms: Some(0),
            error: None,
        },
        Err(error) => CommandResult {
            exit_code: None,
            stdout: None,
            stderr: None,
            duration_ms: Some(0),
            error: Some(error),
        },
    }
}
