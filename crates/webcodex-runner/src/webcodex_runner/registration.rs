//! Runner registration and capability projection.

use crate::webcodex_runner;
use crate::webcodex_runner::jobs::JobManager;
use crate::webcodex_runner::transport::http::{
    RUNNER_REGISTER_PATH, RunnerHttpError, RunnerHttpErrorKind, bounded_single_line,
    is_active_instance_lease_conflict, looks_like_auth_failure_message, post_json,
};
use crate::webcodex_runner::{
    HotRunnerConfig, ReloadableRunnerConfig, RunnerConfig, RunnerProjectCache, ShellConfig,
    SshConnectionPool, hostname, max_concurrent_jobs,
};
use reqwest::blocking::Client;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use webcodex_core::runner_protocol::{
    RUNNER_PROTOCOL_GENERATION_V2, RunnerCapabilities, RunnerPolicySummary, RunnerProjectSummary,
    RunnerRegisterRequest, RunnerRegisterResponse, ShellJobInventory, ShellProfileSummaryEntry,
    ShellProfilesSummary, ShellProjectInventoryStatus,
};
use webcodex_core::{build_info, runner_protocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegisterRecoveryAction {
    Retry,
    WaitForLease,
    Fatal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegisterErrorKind {
    Transient,
    LeaseConflict,
    Auth,
    EndpointMissing,
    Rejected,
    Config,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegisterError {
    pub(crate) kind: RegisterErrorKind,
    pub(crate) message: String,
}

impl RegisterError {
    pub(crate) fn from_http(error: RunnerHttpError, client_id: &str) -> Self {
        let kind = match error.kind {
            RunnerHttpErrorKind::ServerUnavailable
            | RunnerHttpErrorKind::Status
            | RunnerHttpErrorKind::RequestTimeout
            | RunnerHttpErrorKind::Request
            | RunnerHttpErrorKind::DecodeTransient => RegisterErrorKind::Transient,
            RunnerHttpErrorKind::Auth => RegisterErrorKind::Auth,
            RunnerHttpErrorKind::NotFound => RegisterErrorKind::EndpointMissing,
            RunnerHttpErrorKind::Config => RegisterErrorKind::Config,
            RunnerHttpErrorKind::ProtocolDecode => RegisterErrorKind::Protocol,
            RunnerHttpErrorKind::ClientRejected
                if is_active_instance_lease_conflict(client_id, error.server_error.as_deref()) =>
            {
                RegisterErrorKind::LeaseConflict
            }
            RunnerHttpErrorKind::ClientRejected => RegisterErrorKind::Rejected,
        };
        let message = if error.kind == RunnerHttpErrorKind::ProtocolDecode {
            format!(
                "register response incompatible with server protocol: endpoint={} {}",
                error.path, error.summary
            )
        } else {
            error.to_string()
        };
        Self { kind, message }
    }

    pub(crate) fn from_response_error(client_id: &str, error: Option<String>) -> Self {
        let summary =
            bounded_single_line(error.as_deref().unwrap_or("register failed without error"));
        let kind = if is_active_instance_lease_conflict(client_id, Some(&summary)) {
            RegisterErrorKind::LeaseConflict
        } else if looks_like_auth_failure_message(&summary) {
            RegisterErrorKind::Auth
        } else {
            RegisterErrorKind::Rejected
        };
        Self {
            kind,
            message: format!("register rejected by server: {summary}"),
        }
    }

    pub(crate) fn recovery_action(&self) -> RegisterRecoveryAction {
        match self.kind {
            RegisterErrorKind::Transient => RegisterRecoveryAction::Retry,
            RegisterErrorKind::LeaseConflict => RegisterRecoveryAction::WaitForLease,
            RegisterErrorKind::Auth
            | RegisterErrorKind::EndpointMissing
            | RegisterErrorKind::Rejected
            | RegisterErrorKind::Config
            | RegisterErrorKind::Protocol => RegisterRecoveryAction::Fatal,
        }
    }

    pub(crate) fn into_message(self) -> String {
        self.message
    }
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Hidden, test/ops-only knob: parse `WEBCODEX_RUNNER_DISABLE_JOB_STATE_RECONCILIATION`
/// as a boolean. Default false (reconciliation stays on). Inline rather than
/// shared because the runner crate does not depend on the server config helpers.
pub(crate) fn disable_job_state_reconciliation_for_test() -> bool {
    matches!(
        std::env::var("WEBCODEX_RUNNER_DISABLE_JOB_STATE_RECONCILIATION")
            .ok()
            .map(|raw| raw.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

pub(crate) fn runner_register_capabilities(cfg: &RunnerConfig) -> RunnerCapabilities {
    let mut capabilities = cfg.capabilities.clone().unwrap_or_default();
    capabilities.jobs = true;
    capabilities.file_read = true;
    capabilities.file_write = true;
    // This binary implements the narrow internal seek/read export-chunk path.
    // Older binaries omit the field so Control uses the existing slow fallback.
    capabilities.artifact_export_chunk_read = true;
    // Large export metadata (size/SHA/MIME) is verified with bounded streaming
    // I/O. Keep this separate from chunk-read support for rolling upgrades.
    capabilities.artifact_export_streaming_metadata = true;
    // This binary implements the complete bounded structured delete contract.
    // Older binaries omit the field and therefore keep using the Server's legacy path.
    capabilities.structured_file_delete = true;
    // This binary enforces ApplyTextEditInput.occurrence exactly. Older binaries
    // omit this additive effect-semantics capability and must not receive selectors.
    capabilities.apply_text_edit_occurrence = true;
    // Line scopes are an additive rolling-upgrade fence: advertise only because
    // this binary resolves full-match containment before any mutation.
    capabilities.apply_text_edit_line_scope = true;
    // Codex Patch is an additive request kind with Runner-authoritative parsing and
    // transaction semantics. Older Runners omit it and must fail closed.
    capabilities.apply_patch = true;
    // WebCodex 0.4 requires every successful patch to expose the complete bounded
    // patch-plan/match metadata consumed by Server validation. Older apply_patch
    // implementations omit this capability and are rejected before dispatch.
    capabilities.apply_patch_match_metadata = true;
    // Enum-based matching is the 0.4 model-facing authority. Older Runners omit
    // it, so current Servers fail closed instead of falling back to old defaults.
    capabilities.apply_patch_matching_mode = true;
    // Retain the legacy bit only so an older Server can roll against this Runner.
    capabilities.apply_patch_strict_matching = true;
    capabilities.async_jobs = true;
    capabilities.async_shell_jobs = true;
    // SSH support intentionally depends on the local OpenSSH executable.
    // Authentication and Host aliases remain entirely Runner-local.
    capabilities.ssh_shell = SshConnectionPool::is_available();
    // This binary installs the bounded, process-local persistent-shell
    // manager. Older binaries omit this field and therefore fail closed.
    capabilities.persistent_shell = webcodex_persistent_shell::local_shell_supported();
    // SSH persistent shells reuse the same OpenSSH executable as `ssh_shell`.
    // Older binaries omit this field and therefore fail closed; it is never
    // inferred from `ssh_shell` + `persistent_shell`.
    capabilities.ssh_persistent_shell = SshConnectionPool::persistent_shell_available();
    capabilities.structured_validation_argv = true;
    // This binary durably round-trips Cargo test-count assertions with
    // validation Job context and reconciliation snapshots.
    capabilities.structured_cargo_test_count_assertion = true;
    // Explicit require_tests/no_run policy changes validation proof semantics,
    // so advertise durable preservation independently from the older count
    // assertion capability for rolling upgrades.
    capabilities.structured_cargo_test_execution_policy = true;
    // This binary accepts both legacy Go validation argv from old Servers and
    // the current machine-readable JSON argv. Do not trust static config or
    // infer this from generic structured validation support.
    capabilities.structured_go_test_json = true;
    // This binary also understands the first-class go_test durable metadata
    // identity. Keep this independent from JSON parsing so an old Runner that
    // supported Connector Go evidence cannot be mistaken for a first-class
    // go_test executor by a newer Server.
    capabilities.structured_go_test_tool = true;
    // Focused first-class go_test packages extend the older fixed `./...`
    // wire shape, so advertise them independently for rolling upgrades.
    capabilities.structured_go_test_packages = true;
    capabilities.structured_process_argv = true;
    capabilities.structured_script_payload = true;
    capabilities.internal_posix_script = true;
    capabilities.structured_execution_jobs = true;
    // Configuration alone grants no isolated execution support. Only the
    // register builder replaces this after actual JobManager initialization.
    capabilities.agent_isolated_execution_v1 = false;
    // Detached process ownership is an independent additive authority. Until
    // each native backend is implemented and dogfooded it must fail closed
    // rather than being inferred from structured process + durable Jobs.
    capabilities.detached_process_jobs =
        cfg!(any(target_os = "linux", target_os = "macos", windows));
    capabilities.project_lifecycle = true;
    // This binary implements resolve_or_register_project; do not trust config to
    // advertise a capability that the binary does not implement.
    capabilities.project_path_registration = true;
    capabilities.managed_worktree = true;
    // Runner-global operator-installed Skill store read and management are
    // explicit rolling-upgrade capabilities implemented by this binary.
    capabilities.skill_store_read = true;
    capabilities.skill_store_manage = true;
    // Native Tool Plugins are a separate Runner-local gateway capability. Keep
    // this explicit even when zero Plugins are configured so cross-platform
    // `plugin_tool reload` can target the exact Runner.
    capabilities.native_tool_plugins = true;
    capabilities.managed_ssh_resources = true;
    // Formal config check/reload is implemented directly against this process's
    // startup-bound runner.toml path on every supported platform. Unix SIGHUP is
    // only an additional trigger and is not part of this capability contract.
    capabilities.runner_config_control = true;
    // MCP gateway support is fenced by the validated provider inventory in
    // registration rather than a separate capability bit. Older binaries omit
    // that inventory, so a newer Server will never target them.
    // `job_state_reconciliation` is on by default. A hidden, test/ops-only env
    // knob lets an E2E exercise the valid generation-2 no-reconciliation mode
    // (it then has no job inventory and a disconnect falls straight to `lost`).
    // Default production behavior is unchanged: only the explicit opt-out
    // disables it, and the server already rejects inventory without the
    // capability and vice-versa.
    // Native read-only desktop observation is implemented only on macOS and
    // Windows. Unsupported platforms advertise false and fail closed.
    capabilities.computer_observe = cfg!(any(target_os = "macos", windows));
    // Installed-application discovery and exact launch are native macOS/Windows
    // additive capabilities. Neither is inferred from observation/control or
    // from each other.
    capabilities.computer_application_discovery = cfg!(any(target_os = "macos", windows));
    capabilities.computer_application_launch = cfg!(any(target_os = "macos", windows));
    // Exact full-display discovery/snapshot is independently implemented by
    // the native macOS and Windows backends; unsupported platforms fail closed.
    capabilities.computer_display_observe = cfg!(any(target_os = "macos", windows));
    // Snapshot-fenced exact coordinate pointer input is independently implemented by
    // the native macOS and Windows backends; unsupported platforms fail closed.
    capabilities.computer_pointer_control = cfg!(any(target_os = "macos", windows));
    // Bounded Unicode-text clipboard observation/replacement are separate
    // native capabilities on macOS and Windows.
    capabilities.computer_clipboard_read = cfg!(any(target_os = "macos", windows));
    capabilities.computer_clipboard_write = cfg!(any(target_os = "macos", windows));
    // Region/downscale snapshot requests use a distinct additive wire fence so
    // old Runners that support only whole-window snapshots fail closed.
    capabilities.computer_snapshot_region = cfg!(any(target_os = "macos", windows));
    // Accessibility inspection is a separate read-only semantic capability.
    // macOS AX and Windows UI Automation share the same model-facing tree;
    // observation authority never implies computer-control authority.
    capabilities.computer_accessibility_observe = cfg!(any(target_os = "macos", windows));
    // Normalized element-state observation is a separate rolling-upgrade wire
    // capability implemented by the same native read-only backends.
    capabilities.computer_element_state = cfg!(any(target_os = "macos", windows));
    // Accessibility control is independently fenced and implemented by the
    // native macOS AX and Windows UI Automation backends.
    capabilities.computer_control = cfg!(any(target_os = "macos", windows));
    // Semantic native scroll-to-visible is independently fenced for rolling upgrades;
    // existing computer_control support never implies it.
    capabilities.computer_scroll_to_element = cfg!(any(target_os = "macos", windows));
    // Closed key input is a separate effect/wire capability implemented by the
    // native macOS and Windows paths and is never implied by control.
    capabilities.computer_key_input = cfg!(any(target_os = "macos", windows));
    // Exact window activation is a separate effect/wire capability. It is
    // independently advertised by native macOS and Windows implementations.
    capabilities.computer_window_activate = cfg!(any(target_os = "macos", windows));
    // Bounded Accessibility text input is a separate rolling-upgrade fence;
    // older native Runners with computer_control must not be treated as capable.
    capabilities.computer_text_input = cfg!(any(target_os = "macos", windows));
    capabilities.job_state_reconciliation = !disable_job_state_reconciliation_for_test();

    // New agents always advertise read-only LSP navigation. Older agents omit
    // the field and deserialize as false on the server.
    capabilities.lsp_read_only_navigation = true;
    // Advertise the distinct capability only because this binary installs the
    // bounded typed prepare/incoming/outgoing traversal implementation.
    capabilities.lsp_call_hierarchy = true;
    capabilities
}

#[cfg(test)]
mod capability_tests {
    use super::super::jobs::agent_resources::AgentExecutionResourcesConfig;
    use super::agent_execution_resource_registration;

    #[test]
    fn agent_execution_registration_requires_ready_backend_and_verified_resources() {
        let empty = AgentExecutionResourcesConfig::default();
        assert!(agent_execution_resource_registration(false, &empty).is_empty());
        assert!(agent_execution_resource_registration(true, &empty).is_empty());
        let invalid: AgentExecutionResourcesConfig = serde_json::from_value(serde_json::json!({
            "profiles":[{"id":"fixture", "executable":"missing.exe", "executable_sha256":"invalid", "files":[]}]
        })).unwrap();
        assert!(agent_execution_resource_registration(false, &invalid).is_empty());
        assert!(agent_execution_resource_registration(true, &invalid).is_empty());
    }
}

fn agent_execution_resource_registration(
    backend_ready: bool,
    config: &super::jobs::agent_resources::AgentExecutionResourcesConfig,
) -> Vec<runner_protocol::AgentExecutionResourceDescriptor> {
    if !cfg!(windows) || !backend_ready {
        return Vec::new();
    }
    super::jobs::agent_resources::validate_profiles(config)
        .and_then(|profiles| {
            profiles.verify_sources()?;
            Ok(profiles.descriptors())
        })
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn build_register_request(
    cfg: &RunnerConfig,
    runner_instance_id: &str,
    prepared_cache_count: usize,
) -> RunnerRegisterRequest {
    let runtime = ReloadableRunnerConfig::new(cfg.clone(), PathBuf::new());
    build_register_request_with_provider_status(
        cfg,
        &runtime,
        runner_instance_id,
        prepared_cache_count,
        ShellJobInventory {
            active_complete: true,
            jobs: Vec::new(),
        },
    )
    .0
}

pub(crate) fn build_register_request_with_provider_status(
    cfg: &RunnerConfig,
    runtime: &ReloadableRunnerConfig,
    runner_instance_id: &str,
    prepared_cache_count: usize,
    job_inventory: ShellJobInventory,
) -> (
    RunnerRegisterRequest,
    Arc<webcodex_runner::external_tools::ExternalToolRouter>,
    u64,
) {
    let hot = runtime.snapshot();
    let mut capabilities = runner_register_capabilities(cfg);
    let agent_execution_resources = agent_execution_resource_registration(
        runtime.agent_execution_ready(),
        &cfg.agent_execution_resources,
    );
    capabilities.agent_isolated_execution_v1 = !agent_execution_resources.is_empty();
    let coding_agent_providers = runtime
        .coding_agents()
        .map(|manager| manager.providers())
        .unwrap_or_default();
    let coding_agent_inventory = runtime.coding_agents().map(|manager| manager.inventory());
    capabilities.coding_agent_runs = !coding_agent_providers.is_empty();
    let (mut tool_providers, revision) = hot.external_tools.registration_status();
    tool_providers.config_reload = hot.reload_status();
    (
        RunnerRegisterRequest {
            client_id: cfg.client_id.clone(),
            runner_instance_id: runner_instance_id.to_string(),
            runner_protocol_generation: RUNNER_PROTOCOL_GENERATION_V2,
            display_name: cfg.display_name.clone(),
            owner: cfg.owner.clone(),
            hostname: cfg.hostname.clone().or_else(hostname),
            host_context: cfg.host_context.clone(),
            capabilities,
            agent_execution_resources,
            policy: Some(register_policy_summary(
                &hot,
                prepared_cache_count,
                tool_providers,
                runtime.mcp_gateway().provider_inventory(),
            )),
            process_started_at: Some(process_started_at()),
            build: Some(runner_build_info()),
            job_concurrency_limit: Some(max_concurrent_jobs(cfg)),
            // A Runner with reconciliation disabled for E2E must not send a job
            // inventory: the server rejects inventory without the capability and
            // vice-versa.
            job_inventory: if disable_job_state_reconciliation_for_test() {
                None
            } else {
                Some(job_inventory)
            },
            coding_agent_providers: (!coding_agent_providers.is_empty())
                .then_some(coding_agent_providers),
            coding_agent_inventory,
        },
        Arc::clone(&hot.external_tools),
        revision,
    )
}

/// Unix timestamp when this runner process started. Captured on first call;
/// `run_runner` initializes it at startup so registration payloads report the
/// real process start, not the first register time after a reconnect.
pub(crate) fn process_started_at() -> i64 {
    static STARTED_AT: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *STARTED_AT.get_or_init(|| chrono::Utc::now().timestamp())
}

/// Non-secret runner build identity for mixed-version diagnostics.
pub(crate) fn runner_build_info() -> runner_protocol::RunnerBuildInfo {
    let info = build_info::current();
    runner_protocol::RunnerBuildInfo {
        version: Some(info.version.to_string()),
        git_commit: info.git_commit.map(str::to_string),
        git_dirty: info.git_dirty,
    }
}

/// Shell dialect derived from a program path basename. Only `sh` and `bash`
/// map to portable POSIX dialects; `powershell`/`powershell.exe`/`pwsh` map to
/// the Windows PowerShell dialect; anything else is `custom` and callers that
/// need deterministic syntax must select an explicit `shell=sh|bash` (or a
/// configured dialect on the Runner side).
pub(crate) fn shell_dialect_for_program(program: &str) -> &'static str {
    match std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
    {
        "sh" => "sh",
        "bash" => "bash",
        "powershell" | "powershell.exe" | "pwsh" => "powershell",
        _ => "custom",
    }
}

/// Build the sanitized shell-profiles summary from the active shell config.
/// Exposes only safe metadata: profile names, whether each has an init_script
/// (boolean, never the body), env key counts (never values), the resolved
/// program, and arg counts. `prepared_cache_count` is the number of snapshots
/// prepared at call time (typically 0 right after start). Never includes env
/// values, init_script bodies, tokens, or the full env snapshot.
pub(crate) fn build_shell_profiles_summary(
    shell: &ShellConfig,
    prepared_cache_count: usize,
) -> ShellProfilesSummary {
    let profiles: Vec<ShellProfileSummaryEntry> = shell
        .profiles
        .iter()
        .map(|(name, profile)| {
            let program = profile
                .program
                .clone()
                .unwrap_or_else(|| shell.program.clone());
            let args = profile.args.clone().unwrap_or_else(|| shell.args.clone());
            let dialect = shell_dialect_for_program(&program);
            ShellProfileSummaryEntry {
                name: name.clone(),
                has_init_script: profile.init_script.is_some(),
                env_keys_count: profile.env.len(),
                program,
                args_count: args.len(),
                dialect: Some(dialect.to_string()),
            }
        })
        .collect();
    // Default execution path when the caller selects no explicit shell:
    // shell.default_profile if set, otherwise the plain shell program.
    // (A project-level shell_profile override is reported per project.)
    let default_program = shell
        .default_profile
        .as_deref()
        .and_then(|name| shell.profiles.get(name))
        .and_then(|profile| profile.program.clone())
        .unwrap_or_else(|| shell.program.clone());
    let default_dialect = shell_dialect_for_program(&default_program).to_string();
    // Explicit shell=sh|bash always resolves on the runner; configured custom
    // profiles add the custom dialect.
    let mut available: Vec<String> = vec!["sh".to_string(), "bash".to_string()];
    for entry in &profiles {
        if let Some(dialect) = entry.dialect.as_deref() {
            if !available.iter().any(|existing| existing == dialect) {
                available.push(dialect.to_string());
            }
        }
    }
    ShellProfilesSummary {
        default_profile: shell.default_profile.clone(),
        configured_count: shell.profiles.len(),
        prepared_cache_count,
        profiles,
        default_dialect: Some(default_dialect),
        available_dialects: Some(available),
    }
}

/// Build the sanitized Runner policy summary sent at registration. The wire
/// projection remains unchanged; it mirrors local `RunnerPolicy` but carries
/// only non-secret fields. The shell env
/// values and init_script path are intentionally NOT included. The sanitized
/// shell-profiles summary is attached so observability can show which profile
/// a project resolves to without exposing env values or init_script bodies.
pub(crate) fn register_policy_summary(
    cfg: &HotRunnerConfig,
    prepared_cache_count: usize,
    tool_providers: runner_protocol::ToolProvidersStatus,
    mcp_gateway_providers: Vec<crate::mcp_gateway::McpGatewayProvider>,
) -> RunnerPolicySummary {
    RunnerPolicySummary {
        allow_raw_shell: cfg.policy.allow_raw_shell,
        allow_cwd_anywhere: cfg.policy.allow_cwd_anywhere,
        allowed_roots: cfg.policy.allowed_roots.clone(),
        max_timeout_secs: cfg.policy.max_timeout_secs,
        max_output_bytes: cfg.policy.max_output_bytes,
        shell_profiles: Some(build_shell_profiles_summary(
            &cfg.shell,
            prepared_cache_count,
        )),
        tool_providers: Some(tool_providers),
        mcp_gateway_providers: Some(mcp_gateway_providers),
    }
}

pub(crate) fn register(
    client: &Client,
    cfg: &RunnerConfig,
    runtime: &ReloadableRunnerConfig,
    project_cache: &mut RunnerProjectCache,
    shutdown: Option<&AtomicBool>,
    runner_instance_id: &str,
    prepared_cache_count: usize,
    jobs: &JobManager,
) -> Result<
    (
        usize,
        ShellJobInventory,
        Vec<RunnerProjectSummary>,
        ShellProjectInventoryStatus,
    ),
    RegisterError,
> {
    let projects = project_cache.get_with_shutdown(cfg, shutdown);
    let projects_count = projects.iter().filter(|project| !project.disabled).count();
    let job_inventory = jobs.inventory();
    let (body, provider, provider_revision) = build_register_request_with_provider_status(
        cfg,
        runtime,
        runner_instance_id,
        prepared_cache_count,
        job_inventory.clone(),
    );
    let response: RunnerRegisterResponse = post_json(client, cfg, RUNNER_REGISTER_PATH, &body)
        .map_err(|error| RegisterError::from_http(error, &cfg.client_id))?;
    if response.success {
        provider.mark_status_reported(provider_revision);
        let inventory_status = response
            .client
            .as_ref()
            .and_then(|client| client.project_inventory.clone())
            .ok_or_else(|| RegisterError {
                kind: RegisterErrorKind::Protocol,
                message: "register response missing canonical project_inventory acknowledgement; Server is incompatible with this 0.4 Runner".to_string(),
            })?;
        Ok((projects_count, job_inventory, projects, inventory_status))
    } else {
        Err(RegisterError::from_response_error(
            &cfg.client_id,
            response.error,
        ))
    }
}
