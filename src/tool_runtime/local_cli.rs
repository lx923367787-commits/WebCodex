//! Authenticated task adapter for ordinary locally installed CLIs. Execution
//! belongs to the Runner user; Agent receipt ownership is not an OS sandbox.
use super::{ToolResult, ToolRuntime};
use crate::agent_isolation::denial;
use crate::projects::ProjectConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webcodex_core::local_cli::{LocalCliInventory, LocalCliQuery, LOCAL_CLI_MAX_BYTES};
use webcodex_core::runner_protocol::{ShellJobInfo, ShellJobOpRequest, ShellProcessArgv};
use webcodex_runner_registry::{
    DetachedInitiatorIdentity, RunnerAccess, RunnerFeature, ShellJobStartMetadata,
    ShellJobVisibility, StructuredJobExecution,
};
use webcodex_store::{AgentProjectFilesGrant, LocalCliCall, VerifiedAgentRun};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Discover {
        project: String,
        commands: Vec<String>,
    },
    Start {
        project: String,
        executable: String,
        #[serde(default)]
        args: Vec<String>,
        stdin: Option<String>,
        timeout_secs: Option<u64>,
        idempotency_key: String,
        model: Option<String>,
        effort: Option<String>,
    },
    Observe {
        call_id: Option<String>,
        tail_lines: Option<usize>,
        after_observation_token: Option<String>,
        wait_secs: Option<u64>,
    },
    Stop {
        call_id: String,
    },
}

fn owner_access(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
) -> Result<RunnerAccess, ToolResult> {
    let db = runtime
        .isolation_database()
        .ok_or_else(|| denial("agent_isolation_unavailable"))?;
    let owner = db
        .get_user_by_id(binding.owner_user())
        .ok()
        .flatten()
        .filter(|owner| !owner.is_disabled())
        .ok_or_else(|| denial("workspace_owner_unavailable"))?;
    Ok(RunnerAccess {
        global_visibility: false,
        owner_bypass: false,
        username: Some(owner.username),
        group: None,
    })
}

async fn project_scope(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
    id: &str,
    write: bool,
) -> Result<(ProjectConfig, RunnerAccess), ToolResult> {
    let db = runtime
        .isolation_database()
        .ok_or_else(|| denial("agent_isolation_unavailable"))?;
    let grant = db
        .agent_project_files_grant(binding, write)
        .map_err(|e| denial(e.code()))?;
    let root = match &grant {
        AgentProjectFilesGrant::Named(workspace) => Some(
            workspace
                .folders
                .iter()
                .find(|f| f.project == id)
                .ok_or_else(|| denial("workspace_folder_not_authorized"))?
                .path
                .as_str(),
        ),
        AgentProjectFilesGrant::Registered { project } if project == id => None,
        _ => return Err(denial("workspace_folder_not_authorized")),
    };
    let resolved = runtime
        .execution()
        .resolve_project_input(id)
        .await
        .map_err(|_| denial("workspace_folder_unavailable"))?;
    if resolved.resolved_id != id
        || root.is_some_and(|root| resolved.config.path != root)
        || webcodex_core::sensitive_paths::is_secret_path(&resolved.config.path)
    {
        return Err(denial("workspace_folder_changed"));
    }
    if write && !resolved.config.allow_patch {
        return Err(denial("workspace_folder_read_only"));
    }
    let access = owner_access(runtime, binding)?;
    runtime
        .runner_registry
        .assert_runner_access(Some(&access), &resolved.config.client_id)
        .await
        .map_err(|_| denial("workspace_folder_not_authorized"))?;
    if db.agent_project_files_grant(binding, write).ok().as_ref() != Some(&grant) {
        return Err(denial("workspace_folder_not_authorized"));
    }
    Ok((resolved.config, access))
}

fn job_state(job: &ShellJobInfo) -> &'static str {
    match job.status.as_str() {
        "completed" => {
            if job.exit_code == Some(0) {
                "completed"
            } else {
                "failed"
            }
        }
        "failed" => "failed",
        "stopped" => "stopped",
        "queued" | "running" | "stopping" => "running",
        _ => "unknown",
    }
}

fn execution_confirmed(job: &ShellJobInfo) -> bool {
    matches!(
        job.command_execution_state,
        Some(
            webcodex_core::runner_protocol::ShellCommandExecutionState::Completed
                | webcodex_core::runner_protocol::ShellCommandExecutionState::TimedOut
        )
    )
}

fn receipt(call: &LocalCliCall, job: &ShellJobInfo) -> ToolResult {
    ToolResult::ok(json!({"call":call,"job":job,"execution_mode":"runner_user",
        "terminal":matches!(call.state.as_str(),"completed"|"failed"|"stopped"),
        "process_success":job.status=="completed" && job.exit_code==Some(0),
        "cli_success":call.result_contract.as_ref().and_then(|_| match call.state.as_str() {"completed"=>Some(true),"failed"|"stopped"=>Some(false),_=>None}),
        "task_acceptance_verified":false,
        "next_action":"Observe this call_id until a trustworthy terminal result, then validate the requested deliverables."}))
}

pub(crate) async fn call(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
    args: Value,
) -> ToolResult {
    call_with_result_contract(runtime, binding, args, None).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedStart {
    project: String,
    cli_id: String,
    model: String,
    effort: Option<String>,
    instruction: String,
    idempotency_key: String,
    timeout_secs: Option<u64>,
}

pub(crate) async fn start_selected(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
    args: Value,
) -> ToolResult {
    let request: SelectedStart = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(_) => return denial("local_cli_invalid_input"),
    };
    let provider = match webcodex_core::agent_cli::AgentCliProvider::parse(&request.cli_id) {
        Ok(v) => v,
        Err(_) => return denial("agent_cli_invalid_provider_id"),
    };
    let (target, _) = match project_scope(runtime, binding, &request.project, true).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(assets) = &runtime.agent_team_assets else {
        return denial("agent_team_catalog_source_missing");
    };
    let inventory = match assets.inventory(binding, Some(provider)).await {
        Ok(v) => v,
        Err(_) => return denial("agent_cli_inventory_unavailable"),
    };
    if inventory.target.client_id != target.client_id {
        return denial("local_cli_project_runner_mismatch");
    }
    let Some(installation) = inventory
        .installations
        .iter()
        .find(|i| i.provider_id == request.cli_id && i.installed)
    else {
        return denial("local_cli_not_installed");
    };
    let Some(capabilities) = inventory.capabilities.as_ref() else {
        return denial("local_cli_capabilities_unavailable");
    };
    let Some(adapter) = capabilities
        .adapter
        .as_ref()
        .filter(|adapter| adapter.id == request.cli_id)
    else {
        return denial("local_cli_native_arguments_required");
    };
    let Some(invocation) = adapter.invocation.as_ref() else {
        return denial("local_cli_native_arguments_required");
    };
    if capabilities.status != "ready" || !capabilities.models.contains(&request.model) {
        return denial("local_cli_model_not_discovered");
    }
    if request
        .effort
        .as_ref()
        .is_some_and(|effort| !capabilities.efforts.contains(effort))
    {
        return denial("local_cli_effort_not_discovered");
    }
    let (argv, stdin) = match adapter.invocation(
        &request.model,
        request.effort.as_deref(),
        &request.instruction,
    ) {
        Ok(v) => v,
        Err(e) => return denial(&e),
    };
    call_with_result_contract(
        runtime,
        binding,
        json!({"action":"start","project":request.project,"executable":installation.command,
        "args":argv,"stdin":stdin,"model":request.model,"effort":request.effort,
        "idempotency_key":request.idempotency_key,"timeout_secs":request.timeout_secs}),
        invocation.result.clone(),
    )
    .await
}

async fn terminal_state(
    runtime: &ToolRuntime,
    access: &RunnerAccess,
    call: &LocalCliCall,
    job: &ShellJobInfo,
) -> (&'static str, Option<&'static str>) {
    let state = job_state(job);
    if state != "completed" {
        return (state, None);
    }
    let Some(contract) = call.result_contract.as_ref() else {
        return (state, None);
    };
    let Ok((full, stdout, _, _, _, _)) = runtime
        .runner_registry
        .job_log_for_auth(
            Some(access),
            &job.job_id,
            Some(0),
            Some(0),
            None,
            None,
            None,
        )
        .await
    else {
        return ("unknown", Some("local_cli_result_unavailable"));
    };
    if full.stdout_log_truncated {
        return ("failed", Some("local_cli_result_truncated"));
    }
    let mut verifier = webcodex_core::cli_result::CliResultVerifier::new(Some(contract.clone()));
    verifier.observe_stdout(stdout.as_deref().unwrap_or("").as_bytes());
    match verifier.finish() {
        Ok(()) => ("completed", None),
        Err(code) => ("failed", Some(code)),
    }
}

fn result_error(mut result: ToolResult, error: Option<&str>) -> ToolResult {
    if let Some(error) = error {
        result.success = false;
        result.error = Some(error.into());
        result.output["error_kind"] = json!(error);
    }
    result
}

async fn call_with_result_contract(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
    args: Value,
    result_contract: Option<webcodex_core::cli_adapter::CliResultContract>,
) -> ToolResult {
    let request: Request = match serde_json::from_value(args.clone()) {
        Ok(request) => request,
        Err(_) => return denial("local_cli_invalid_input"),
    };
    let Some(db) = runtime.isolation_database() else {
        return denial("agent_isolation_unavailable");
    };
    match request {
        Request::Discover { project, commands } => {
            let (target, access) = match project_scope(runtime, binding, &project, false).await {
                Ok(v) => v,
                Err(e) => return e,
            };
            let query = LocalCliQuery { commands };
            let (request_id, receiver) = match runtime
                .runner_registry
                .enqueue_local_cli_discovery(&target.client_id, query, &access)
                .await
            {
                Ok(v) => v,
                Err(e) => return denial(&e),
            };
            let response =
                match tokio::time::timeout(std::time::Duration::from_secs(30), receiver).await {
                    Ok(Ok(v)) => v,
                    _ => {
                        runtime.runner_registry.cancel_request(&request_id).await;
                        return denial("local_cli_discovery_timeout");
                    }
                };
            if db.validate_agent_run(binding).is_err() {
                return denial("agent_run_invalid");
            }
            let Some(output) = response
                .stdout
                .filter(|text| text.len() <= LOCAL_CLI_MAX_BYTES)
            else {
                return denial("local_cli_discovery_failed");
            };
            let inventory: LocalCliInventory = match serde_json::from_str(&output) {
                Ok(v) => v,
                Err(_) => return denial("local_cli_discovery_failed"),
            };
            ToolResult::ok(
                json!({"project":project,"inventory":inventory,"account_availability_verified":false,
                "next_action":"Use each CLI's actual --help/--version/model listing and supported native arguments. Discovery does not prove a login or model entitlement."}),
            )
        }
        Request::Start {
            project,
            executable,
            args: argv,
            stdin,
            timeout_secs,
            idempotency_key,
            model,
            effort,
        } => {
            let timeout = timeout_secs.unwrap_or(900);
            let process = ShellProcessArgv {
                executable,
                args: argv,
            };
            if !(1..=3600).contains(&timeout)
                || webcodex_core::runner_protocol::validate_process_argv(&process).is_err()
                || stdin.as_ref().is_some_and(|s| {
                    s.len() > webcodex_core::runner_protocol::PROCESS_STDIN_MAX_BYTES
                        || s.contains('\0')
                })
            {
                return denial("local_cli_invalid_input");
            }
            if let Some(decision) = runtime
                .config
                .permission_evaluator
                .evaluate("run_process", Some(&project))
            {
                if !decision.allows_execution() {
                    return super::permissions::permission_execution_denied_result(&decision);
                }
            }
            let (target, access) = match project_scope(runtime, binding, &project, true).await {
                Ok(v) => v,
                Err(e) => return e,
            };
            let features = match runtime
                .runner_registry
                .get_runner_feature_set(&target.client_id)
                .await
            {
                Ok(v) => v,
                Err(_) => return denial("local_cli_runner_unavailable"),
            };
            if !features.supports(RunnerFeature::LocalCli)
                || !features.supports(RunnerFeature::DetachedProcessJobs)
            {
                return denial("local_cli_runner_upgrade_required");
            }
            let fingerprint = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&(
                        "webcodex.local-cli-request.v1",
                        &project,
                        &target.path,
                        &process,
                        &stdin,
                        timeout,
                        &model,
                        &effort,
                        &result_contract,
                    ))
                    .unwrap()
                )
            );
            let (record, _) = match db.reserve_local_cli_call(
                binding,
                &idempotency_key,
                &fingerprint,
                &project,
                &target.client_id,
                &target.path,
                result_contract.clone(),
            ) {
                Ok(v) => v,
                Err(e) => return denial(e.code()),
            };
            if matches!(record.state.as_str(), "completed" | "failed" | "stopped") {
                return ToolResult::ok(json!({"call":record,"replayed":true,"dispatched":false}));
            }
            let initiator = DetachedInitiatorIdentity::from_stable_principal(format!(
                "local-cli:{}:{}:{}",
                binding.owner_user(),
                binding.agent_id(),
                record.task_id
            ));
            let job_id = match webcodex_runner_registry::detached_job_id_for_key(
                &initiator,
                &record.call_id,
            ) {
                Ok(v) => v,
                Err(_) => return denial("local_cli_invalid_input"),
            };
            if let Err(e) = db.record_local_cli_job(binding, &record.call_id, &job_id, "unknown") {
                return denial(e.code());
            }
            let result = runtime
                .runner_registry
                .start_job_with_metadata_for_access(
                    ShellJobOpRequest {
                        op: "start".into(),
                        client_id: Some(target.client_id.clone()),
                        cwd: Some(target.path.clone()),
                        command: Some(String::new()),
                        timeout_secs: Some(timeout),
                        job_id: None,
                        since_stdout_line: None,
                        since_stderr_line: None,
                        tail_lines: None,
                        limit: None,
                        codex: None,
                    },
                    "local_cli".into(),
                    ShellJobStartMetadata {
                        project_id: Some(project),
                        project_cwd: Some(".".into()),
                        purpose: Some("operation".into()),
                        shell: Some("direct_argv".into()),
                        visibility: ShellJobVisibility::Public,
                        structured_execution: Some(StructuredJobExecution::DetachedProcess(
                            process,
                        )),
                        stdin,
                        detached_idempotency_key: Some(record.call_id.clone()),
                        ..Default::default()
                    },
                    Some(&access),
                    Some(&initiator),
                )
                .await;
            match result {
                Ok(job) => {
                    let (state, cli_error) = terminal_state(runtime, &access, &record, &job).await;
                    match db.record_local_cli_observation(
                        binding,
                        &record.call_id,
                        &job.job_id,
                        state,
                        execution_confirmed(&job),
                    ) {
                        Ok(call) => result_error(receipt(&call, &job), cli_error),
                        Err(e) => ToolResult::err_with_output(
                            e.code(),
                            json!({"call_id":record.call_id,"job_id":job_id,"dispatch_certainty":"outcome_unknown"}),
                        ),
                    }
                }
                Err(error) => ToolResult::err_with_output(
                    error,
                    json!({"call_id":record.call_id,"job_id":job_id,"dispatch_certainty":"outcome_unknown",
                    "next_action":"Observe this original call. Never change the key to repeat an uncertain launch."}),
                ),
            }
        }
        Request::Observe {
            call_id,
            tail_lines,
            after_observation_token,
            wait_secs,
        } => {
            let Some(call_id) = call_id else {
                return match db.list_local_cli_calls(binding) {
                    Ok(calls) => ToolResult::ok(
                        json!({"calls":calls,"dispatched":false,"next_action":"Use an existing call_id for status or cancellation; do not start a replacement for an unknown result."}),
                    ),
                    Err(error) => denial(error.code()),
                };
            };
            if tail_lines.is_some_and(|n| n == 0 || n > 500) || wait_secs.is_some_and(|n| n > 30) {
                return denial("local_cli_invalid_input");
            }
            let record = match db.read_local_cli_call(binding, &call_id) {
                Ok(v) => v,
                Err(e) => return denial(e.code()),
            };
            let Some(job_id) = &record.job_id else {
                return ToolResult::ok(json!({"call":record,"dispatch_certainty":"not_started"}));
            };
            let access = match owner_access(runtime, binding) {
                Ok(v) => v,
                Err(e) => return e,
            };
            match runtime
                .runner_registry
                .job_log_for_auth(
                    Some(&access),
                    job_id,
                    None,
                    None,
                    Some(tail_lines.unwrap_or(100)),
                    after_observation_token.as_deref(),
                    wait_secs,
                )
                .await
            {
                Ok((job, stdout, stderr, _, _, observation)) => {
                    if job.client_id != record.client_id
                        || job.project_id.as_deref() != Some(record.project.as_str())
                    {
                        return denial("local_cli_job_identity_conflict");
                    }
                    let (state, cli_error) = terminal_state(runtime, &access, &record, &job).await;
                    let call = match db.record_local_cli_observation(
                        binding,
                        &call_id,
                        job_id,
                        state,
                        execution_confirmed(&job),
                    ) {
                        Ok(v) => v,
                        Err(e) => return denial(e.code()),
                    };
                    let mut result = receipt(&call, &job);
                    result.output["stdout_tail"] = json!(stdout);
                    result.output["stderr_tail"] = json!(stderr);
                    result.output["stdout_truncated"] = json!(observation.stdout_truncated);
                    result.output["stderr_truncated"] = json!(observation.stderr_truncated);
                    result.output["stdout_delta_reset"] = json!(observation.stdout_delta_reset);
                    result.output["stderr_delta_reset"] = json!(observation.stderr_delta_reset);
                    result_error(result, cli_error)
                }
                Err(error) => ToolResult::err_with_output(
                    error,
                    json!({"call":record,"dispatch_certainty":"outcome_unknown","dispatched":false}),
                ),
            }
        }
        Request::Stop { call_id } => {
            let record = match db.read_local_cli_call(binding, &call_id) {
                Ok(v) => v,
                Err(e) => return denial(e.code()),
            };
            let Some(job_id) = &record.job_id else {
                return denial("local_cli_job_not_admitted");
            };
            let access = match owner_access(runtime, binding) {
                Ok(v) => v,
                Err(e) => return e,
            };
            match runtime
                .runner_registry
                .stop_job_for_auth(Some(&access), job_id, "local_cli".into())
                .await
            {
                Ok(job) => {
                    let call = match db.record_local_cli_observation(
                        binding,
                        &call_id,
                        job_id,
                        job_state(&job),
                        execution_confirmed(&job),
                    ) {
                        Ok(v) => v,
                        Err(e) => return denial(e.code()),
                    };
                    receipt(&call, &job)
                }
                Err(error) => {
                    ToolResult::err_with_output(error, json!({"call":record,"stop_proven":false}))
                }
            }
        }
    }
}
