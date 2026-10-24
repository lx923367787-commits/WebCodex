//! Project-owned ordinary team plans. The Server reserves each fixed CLI slot
//! before the existing typed process path dispatches it; reads never dispatch.
use super::{ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use webcodex_core::runner_protocol::ShellCommandExecutionState;
use webcodex_store::{
    CommunicationStoreError, TeamCliAttemptRecord, TeamCliDispatchState, TeamCliObservation,
    TeamPlanInput, TeamPolicyInput,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRequest {
    project: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicySave {
    project: String,
    expected_revision: i64,
    policy: TeamPolicyInput,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    project: String,
    root_task_id: String,
    expected_policy_revision: i64,
    plan: TeamPlanInput,
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootRequest {
    project: String,
    root_task_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanReadRequest {
    project: String,
    root_task_id: String,
    #[serde(default)]
    attempts_offset: usize,
    #[serde(default = "page_size")]
    attempts_limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequest {
    project: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_size")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CliStart {
    project: String,
    root_task_id: String,
    source_task_id: String,
    call_index: u32,
    idempotency_key: String,
    fallback_thinking_profile_id: String,
    #[serde(default = "task_timeout")]
    timeout_secs: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observe {
    project: String,
    source_attempt_id: String,
}
fn page_size() -> usize {
    20
}
fn task_timeout() -> u64 {
    900
}
fn error(code: &str, message: &str) -> ToolResult {
    ToolResult::err_with_output(message, json!({"error_kind":code,"state_changed":false}))
}
fn store_error(value: CommunicationStoreError) -> ToolResult {
    error(value.code(), value.message())
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ToolResult> {
    serde_json::from_value(value)
        .map_err(|_| error("team_invalid_request", "Invalid team request fields"))
}
fn serialized<T: serde::Serialize>(value: T) -> ToolResult {
    match serde_json::to_value(value) {
        Ok(value) => ToolResult::ok(value),
        Err(_) => error("team_result_invalid", "Cannot serialize team result"),
    }
}

struct TeamWatchGuard {
    watchers: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    attempt_id: String,
}
impl Drop for TeamWatchGuard {
    fn drop(&mut self) {
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.attempt_id);
    }
}

fn dispatch_result(attempt: &TeamCliAttemptRecord, result: ToolResult) -> ToolResult {
    let mut output =
        json!({"attempt":attempt,"dispatched":true,"replayed":false,"process_result":result});
    for key in [
        "job_id",
        "job_status",
        "execution_state",
        "terminal",
        "command_started",
        "command_completed",
        "command_ok",
        "exit_code",
        "failure_kind",
        "effective_timeout_secs",
    ] {
        if let Some(value) = result.output.get(key) {
            output[key] = value.clone();
        }
    }
    if output.get("execution_state").is_none() {
        output["execution_state"] = json!("outcome_unknown");
    }
    output["error"] = json!(result.error);
    ToolResult {
        success: result.success,
        error: result.error,
        output,
    }
}

fn recovery_rows(
    db: &crate::Database,
    principal: &webcodex_store::CommunicationPrincipal,
    root: &str,
    policy_revision: i64,
) -> Result<Vec<Value>, ToolResult> {
    db.list_cli_fallbacks(principal, root)
        .map(|rows| {
            rows.iter()
                .map(|record| {
                    let mut value = super::cli_fallback::fallback_projection(record);
                    value["policy_revision"] = json!(policy_revision);
                    value["blocked_reason"] = Value::Null;
                    value
                })
                .collect()
        })
        .map_err(|_| {
            error(
                "team_recovery_unavailable",
                "Cannot read recovery observations",
            )
        })
}

/// Only protocol error-code fields are interpreted. Model prose and arbitrary
/// stderr never become provider quota/network evidence.
pub(crate) fn structured_failure_code(provider: &str, text: &str) -> Option<String> {
    let accepted = [
        "quota_exhausted",
        "insufficient_quota",
        "rate_limited",
        "rate_limit_exceeded",
        "network_unavailable",
        "connection_failed",
        "provider_unavailable",
        "authentication_failed",
    ];
    let candidate = |value: &Value| -> Option<String> {
        let envelope = match provider {
            "workbuddy" if value["type"] == "result" && value["is_error"] == true => value,
            "antigravity" if value["event"] == "result" && value["result"]["status"] == "ERROR" => {
                &value["result"]
            }
            _ => return None,
        };
        let code = envelope
            .get("error_code")
            .and_then(Value::as_str)
            .or_else(|| envelope.pointer("/error/code").and_then(Value::as_str))?;
        accepted.contains(&code).then(|| code.to_string())
    };
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(code) = candidate(&value) {
            return Some(code);
        }
    }
    text.lines()
        .rev()
        .take(64)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| candidate(&value))
}

fn cli_process(
    attempt: &TeamCliAttemptRecord,
) -> Result<(String, Vec<String>, String), ToolResult> {
    fixed_cli_process(
        &attempt.provider_id,
        &attempt.model,
        attempt.effort.as_deref(),
        &attempt.instruction,
    )
}

pub(crate) fn fixed_cli_process(
    provider: &str,
    model: &str,
    effort: Option<&str>,
    instruction: &str,
) -> Result<(String, Vec<String>, String), ToolResult> {
    let effort = effort.ok_or_else(|| {
        error(
            "team_effort_required",
            "Select the CLI's discovered reasoning effort before dispatch",
        )
    })?;
    let mut args = vec![
        "--model".into(),
        model.into(),
        "--effort".into(),
        effort.into(),
    ];
    match provider {
        "workbuddy" => {
            args.extend([
                "--print".into(),
                "--input-format".into(),
                "text".into(),
                "--output-format".into(),
                "json".into(),
            ]);
            Ok(("codebuddy".into(), args, instruction.into()))
        }
        "antigravity" => {
            args.extend([
                "--input-format".into(),
                "stream-json".into(),
                "--output-format".into(),
                "stream-json".into(),
            ]);
            let stdin = format!(
                "{}\n",
                json!({"event":"user","message":{"content":instruction}})
            );
            Ok(("agy".into(), args, stdin))
        }
        _ => Err(error(
            "team_provider_unavailable",
            "The selected CLI provider is not supported",
        )),
    }
}

impl ToolRuntime {
    fn start_team_watch(&self, attempt: TeamCliAttemptRecord, auth: Option<AuthContext>) {
        let watchers = self.execution_state.team_cli_watchers.clone();
        if !watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(attempt.attempt_id.clone())
        {
            return;
        }
        let guard = TeamWatchGuard {
            watchers,
            attempt_id: attempt.attempt_id.clone(),
        };
        let runtime = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            runtime.watch_team_cli(attempt, auth).await;
        });
    }

    async fn team_replay_receipt(
        &self,
        attempt: TeamCliAttemptRecord,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        // A crash or failed registry observation can occur after Terminal is
        // durable but before recovery is reserved. Repair only that reservation;
        // never dispatch the source CLI again or require a fresh profile choice.
        let recovery_enqueue = if attempt.dispatch_state == TeamCliDispatchState::Terminal
            && attempt.job_id.is_some()
        {
            Some(
                self.enqueue_cli_fallback_from_attempt(auth, &attempt.attempt_id)
                    .await,
            )
        } else {
            None
        };
        if let Some(job_id) = &attempt.job_id {
            let status = self.job_status_for_auth(job_id.clone(), false, auth).await;
            if status.success {
                let state = status
                    .output
                    .get("command_execution_state")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(json!("outcome_unknown"));
                let job_status = status.output["status"].as_str().unwrap_or("unknown");
                let terminal = super::jobs::is_terminal_job_status(job_status)
                    && !matches!(job_status, "lost" | "recovering");
                let success =
                    !terminal || (job_status == "completed" && status.output["exit_code"] == 0);
                return ToolResult {
                    success,
                    error: (!success).then(|| "The original CLI Job did not succeed".into()),
                    output: json!({"attempt":attempt,"replayed":true,"dispatched":false,"job_id":job_id,"job_status":job_status,"execution_state":state,"terminal":terminal,"exit_code":status.output["exit_code"],"error":status.output["error"],"recovery_enqueue":recovery_enqueue}),
                };
            }
        }
        ToolResult {
            success: false,
            error: Some(
                "The original execution requires reconciliation; it was not redispatched".into(),
            ),
            output: json!({"attempt":attempt,"replayed":true,"dispatched":false,"job_id":attempt.job_id,"execution_state":"outcome_unknown","terminal":false,"error_kind":"team_execution_unresolved","recovery_enqueue":recovery_enqueue}),
        }
    }
    pub(crate) async fn dispatch_team_tool(
        &self,
        auth: Option<&AuthContext>,
        tool: &str,
        params: Value,
    ) -> ToolResult {
        let Some(project) = params
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return error(
                "team_project_required",
                "An exact registered project is required",
            );
        };
        let resolved = match self
            .execution()
            .resolve_project_input_for_auth(&project, auth)
            .await
        {
            Ok(value) => value,
            Err(_) => {
                return error(
                    "team_project_unavailable",
                    "The exact project is unavailable to this account",
                )
            }
        };
        if resolved.resolved_id != project {
            return error(
                "team_exact_project_required",
                "Use the full registered project id",
            );
        }
        let principal = match super::communication::communication_principal(auth) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let Some(db) = self.communication.communication_db.as_ref() else {
            return error("team_store_unavailable", "Team storage is unavailable");
        };
        macro_rules! input {
            ($ty:ty) => {
                match parse::<$ty>(params) {
                    Ok(value) => value,
                    Err(error) => return error,
                }
            };
        }
        match tool {
            "team_policy_get" => {
                let request = input!(ProjectRequest);
                match db.get_team_policy(&principal, &request.project) {
                    Ok(policy) => ToolResult::ok(
                        json!({"project":project,"policy":policy,"enforcement":"managed_tasks"}),
                    ),
                    Err(value) => store_error(value),
                }
            }
            "team_policy_save" => {
                let request = input!(PolicySave);
                match db.save_team_policy(
                    &principal,
                    &request.project,
                    request.expected_revision,
                    request.policy,
                ) {
                    Ok(policy) => ToolResult::ok(
                        json!({"project":project,"policy":policy,"enforcement":"managed_tasks"}),
                    ),
                    Err(value) => store_error(value),
                }
            }
            "team_plan_register" => {
                let request = input!(Register);
                match db.register_team_plan(
                    &principal,
                    &request.project,
                    &request.root_task_id,
                    request.expected_policy_revision,
                    request.plan,
                    &request.idempotency_key,
                ) {
                    Ok(plan) => serialized(plan),
                    Err(value) => store_error(value),
                }
            }
            "team_plan_list" => {
                let request = input!(ListRequest);
                match db.list_team_plans(
                    &principal,
                    &request.project,
                    request.offset,
                    request.limit,
                ) {
                    Ok(page) => serialized(page),
                    Err(value) => store_error(value),
                }
            }
            "team_plan_read" => {
                let request = input!(PlanReadRequest);
                let plan = match db.read_team_plan(&principal, &request.root_task_id) {
                    Ok(value) if value.project_id == project => value,
                    Ok(_) => return error("team_not_found", "Team plan not found"),
                    Err(value) => return store_error(value),
                };
                let completion = match db.read_team_completion(
                    &principal,
                    &request.project,
                    &request.root_task_id,
                ) {
                    Ok(value) => value,
                    Err(value) => return store_error(value),
                };
                match db.list_team_cli_attempts(
                    &principal,
                    &project,
                    Some(&request.root_task_id),
                    request.attempts_offset,
                    request.attempts_limit,
                ) {
                    Ok(attempts) => {
                        let fallbacks = match recovery_rows(
                            db,
                            &principal,
                            &plan.root_task_id,
                            plan.policy.revision,
                        ) {
                            Ok(rows) => rows,
                            Err(error) => return error,
                        };
                        let agents = match db.list_team_agent_attempts(
                            &principal,
                            &project,
                            Some(&plan.root_task_id),
                            false,
                            100,
                        ) {
                            Ok(rows) => rows
                                .iter()
                                .map(super::team_agent::team_agent_projection)
                                .collect::<Vec<_>>(),
                            Err(value) => return store_error(value),
                        };
                        let mut task_summaries = Vec::new();
                        for member in
                            std::iter::once(&plan.plan.manager).chain(plan.plan.subagents.iter())
                        {
                            match db.read_agent_task(&principal, &member.task_id) {
                                Ok(task) if task.summary.referenced_project_id.as_deref()==Some(project.as_str()) => task_summaries.push(json!({"task_id":task.summary.task_id,"title":task.summary.title,"state":task.summary.state})),
                                _ => return error("team_source_unavailable", "A team task is no longer available"),
                            }
                        }
                        ToolResult::ok(
                            json!({"plan":plan,"completion":completion,"attempts":attempts,"agents":agents,"task_summaries":task_summaries,"fallbacks":fallbacks,"fallbacks_may_be_truncated":fallbacks.len()==100,"enforcement":"managed_tasks"}),
                        )
                    }
                    Err(value) => store_error(value),
                }
            }
            "team_plan_cancel" => {
                let request = input!(RootRequest);
                let plan = match db.read_team_plan(&principal, &request.root_task_id) {
                    Ok(value) if value.project_id == project => value,
                    Ok(_) => return error("team_not_found", "Team plan not found"),
                    Err(value) => return store_error(value),
                };
                let cancelled = match db.mark_team_plan_cancelled(&principal, &plan.root_task_id) {
                    Ok(value) => value,
                    Err(value) => return store_error(value),
                };
                let recovery_cancel_pending = db
                    .cancel_root_cli_fallbacks(&principal, &plan.root_task_id)
                    .is_err();
                let agent_cancel_pending = db
                    .cancel_root_team_agents(&principal, &project, &plan.root_task_id)
                    .is_err();
                let mut offset = 0;
                let mut requested_stops = 0;
                let mut pending_stops = Vec::new();
                loop {
                    let page = match db.list_team_cli_attempts(
                        &principal,
                        &project,
                        Some(&plan.root_task_id),
                        offset,
                        20,
                    ) {
                        Ok(value) => value,
                        Err(value) => return store_error(value),
                    };
                    for attempt in page.attempts {
                        if matches!(
                            attempt.dispatch_state,
                            TeamCliDispatchState::Bound
                                | TeamCliDispatchState::Unknown
                                | TeamCliDispatchState::Dispatching
                        ) {
                            if let Some(job_id) = attempt.job_id {
                                let result = self.stop_job(job_id.clone(), auth).await;
                                requested_stops += 1;
                                if !result.success {
                                    pending_stops.push(job_id);
                                }
                            }
                        }
                    }
                    let Some(next) = page.next_offset else { break };
                    offset = next;
                }

                ToolResult::ok(
                    json!({"plan":cancelled.plan,"stop_requests":requested_stops,"unconfirmed_stops":pending_stops,"recovery_cancel_pending":recovery_cancel_pending,"agent_cancel_pending":agent_cancel_pending,"stopped_confirmed":false}),
                )
            }
            "team_cli_start" => {
                let request = input!(CliStart);
                if !(1..=3600).contains(&request.timeout_secs) {
                    return error(
                        "team_timeout_invalid",
                        "Task timeout must be 1–3600 seconds",
                    );
                }
                match db.lookup_team_cli_attempt_replay(
                    &principal,
                    &project,
                    &request.root_task_id,
                    &request.source_task_id,
                    request.call_index,
                    &request.idempotency_key,
                    &request.fallback_thinking_profile_id,
                    Some(request.timeout_secs),
                ) {
                    Ok(Some(attempt))
                        if attempt.dispatch_state != TeamCliDispatchState::Reserved =>
                    {
                        if attempt.job_id.is_some()
                            && attempt.dispatch_state != TeamCliDispatchState::Terminal
                        {
                            self.start_team_watch(attempt.clone(), auth.cloned());
                        }
                        return self.team_replay_receipt(attempt, auth).await;
                    }
                    Ok(_) => {}
                    Err(value) => return store_error(value),
                }
                if let Err(result) = self
                    .validate_team_fallback_profile(
                        auth,
                        &request.project,
                        &request.fallback_thinking_profile_id,
                    )
                    .await
                {
                    return result;
                }
                let reservation = match db.reserve_team_cli_attempt(
                    &principal,
                    &project,
                    &request.root_task_id,
                    &request.source_task_id,
                    request.call_index,
                    &request.idempotency_key,
                    &request.fallback_thinking_profile_id,
                    Some(request.timeout_secs),
                ) {
                    Ok(value) => value,
                    Err(value) => return store_error(value),
                };
                let (executable, args, stdin) = match cli_process(&reservation.attempt) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
                let claim =
                    match db.claim_team_cli_dispatch(&principal, &reservation.attempt.attempt_id) {
                        Ok(value) => value,
                        Err(value) => return store_error(value),
                    };
                if !claim.claimed {
                    return self.team_replay_receipt(claim.attempt, auth).await;
                }
                let id = claim.attempt.attempt_id.clone();
                let result = self
                    .run_team_cli_process(
                        project.clone(),
                        executable,
                        args,
                        Some(stdin),
                        request.timeout_secs,
                        auth,
                    )
                    .await;
                let job_id = result
                    .output
                    .get("job_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let attempt = if let Some(job_id) = job_id {
                    match db.bind_team_cli_job(&principal, &id, &job_id) { Ok(value) => value, Err(_) => return error("team_job_binding_unknown", "A CLI Job may be running; preserve the attempt and reconcile, do not resubmit") }
                } else {
                    let execution_state = result.output.get("execution_state").and_then(|v| {
                        serde_json::from_value::<ShellCommandExecutionState>(v.clone()).ok()
                    });
                    let state = if execution_state == Some(ShellCommandExecutionState::NotStarted) {
                        TeamCliDispatchState::Terminal
                    } else {
                        TeamCliDispatchState::Unknown
                    };
                    match db.reconcile_team_cli_attempt(&principal, &id, claim.attempt.observation_revision, TeamCliObservation { job_id:None, dispatch_state:state, execution_state, error_code:result.output.get("failure_kind").and_then(Value::as_str).map(str::to_owned), result_summary:Some("The typed process did not return a retained Job; inspect the original admission result.".into()) }) { Ok(value) => value, Err(value) => return store_error(value) }
                };
                if attempt.job_id.is_some() {
                    self.start_team_watch(attempt.clone(), auth.cloned());
                }
                dispatch_result(&attempt, result)
            }
            "team_cli_observe" => {
                let request = input!(Observe);
                match db.read_team_cli_attempt(&principal, &request.source_attempt_id) {
                    Ok(attempt) if attempt.project_id == project => {
                        let job = if let Some(id) = &attempt.job_id {
                            Some(self.job_status_for_auth(id.clone(), false, auth).await)
                        } else {
                            None
                        };
                        let plan = match db.read_team_plan(&principal, &attempt.root_task_id) {
                            Ok(value) => value,
                            Err(value) => return store_error(value),
                        };
                        let fallbacks = match recovery_rows(
                            db,
                            &principal,
                            &attempt.root_task_id,
                            plan.policy.revision,
                        ) {
                            Ok(rows) => rows
                                .into_iter()
                                .filter(|row| row["source_attempt_id"] == attempt.attempt_id)
                                .collect::<Vec<_>>(),
                            Err(error) => return error,
                        };
                        ToolResult::ok(json!({"attempt":attempt,"job":job,"fallbacks":fallbacks}))
                    }
                    Ok(_) => error("team_not_found", "Team CLI attempt not found"),
                    Err(value) => store_error(value),
                }
            }
            _ => error("team_operation_unknown", "Unknown team operation"),
        }
    }

    async fn watch_team_cli(&self, initial: TeamCliAttemptRecord, auth: Option<AuthContext>) {
        let Ok(principal) = super::communication::communication_principal(auth.as_ref()) else {
            return;
        };
        let Some(db) = self.communication.communication_db.as_ref() else {
            return;
        };
        let Some(job_id) = &initial.job_id else {
            return;
        };
        let access = crate::runner_http::runner_access_from_auth(auth.as_ref());
        let elapsed = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(initial.created_at_unix_ms)
            .max(0) as u64;
        let remaining = initial
            .timeout_secs
            .unwrap_or(900)
            .saturating_add(60)
            .saturating_mul(1000)
            .saturating_sub(elapsed);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(remaining);
        let cleanup_deadline = deadline + Duration::from_secs(10);
        let mut stop_sent = false;
        loop {
            let Ok(current) = db.read_team_cli_attempt(&principal, &initial.attempt_id) else {
                return;
            };
            if current.dispatch_state == TeamCliDispatchState::Terminal {
                let _ = self
                    .enqueue_cli_fallback_from_attempt(auth.as_ref(), &current.attempt_id)
                    .await;
                return;
            }
            let cancelled = db
                .read_team_plan(&principal, &initial.root_task_id)
                .map(|plan| plan.cancel_requested)
                .unwrap_or(true);
            if cancelled && !stop_sent {
                let _ = self.stop_job(job_id.clone(), auth.as_ref()).await;
                stop_sent = true;
            }
            if let Ok(job) = self
                .runner_registry
                .get_job_for_auth(access.as_ref(), job_id)
                .await
            {
                if super::jobs::is_terminal_job_status(&job.status) {
                    let known = !matches!(job.status.as_str(), "lost" | "recovering")
                        && matches!(
                            job.command_execution_state,
                            Some(
                                ShellCommandExecutionState::Completed
                                    | ShellCommandExecutionState::TimedOut
                                    | ShellCommandExecutionState::NotStarted
                            )
                        );
                    let mut code = None;
                    if known
                        && job.command_execution_state
                            == Some(ShellCommandExecutionState::NotStarted)
                    {
                        code = job.error.as_deref().map(|message| {
                            let lower = message.to_ascii_lowercase();
                            if [
                                "permission",
                                "denied",
                                "sandbox",
                                "isolation",
                                "not allowed",
                            ]
                            .iter()
                            .any(|needle| lower.contains(needle))
                            {
                                "permission_denied".to_string()
                            } else {
                                super::process::classify_process_failure(message).to_string()
                            }
                        });
                    }
                    if known && !cancelled && job.exit_code != Some(0) {
                        let log = self.job_log_for_team(job_id, auth.as_ref()).await;
                        code = structured_failure_code(&initial.provider_id, &log).or(code);
                    }
                    if job.status == "stopped" {
                        // A stopped Job does not prove who requested it. Only
                        // the native TimedOut result enables automatic timeout recovery.
                        code = cancelled.then(|| "user_cancelled".into());
                    }
                    let observation = TeamCliObservation {
                        job_id: Some(job_id.clone()),
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
                        error_code: code,
                        result_summary: Some(format!(
                            "CLI Job {}: state={}, exit_code={:?}",
                            job_id, job.status, job.exit_code
                        )),
                    };
                    if db
                        .reconcile_team_cli_attempt(
                            &principal,
                            &initial.attempt_id,
                            current.observation_revision,
                            observation,
                        )
                        .is_ok()
                        && known
                        && !cancelled
                    {
                        let _ = self
                            .enqueue_cli_fallback_from_attempt(auth.as_ref(), &initial.attempt_id)
                            .await;
                    }
                    return;
                }
            }
            if tokio::time::Instant::now() >= cleanup_deadline {
                let _ = db.reconcile_team_cli_attempt(&principal, &initial.attempt_id, current.observation_revision, TeamCliObservation { job_id:Some(job_id.clone()), dispatch_state:TeamCliDispatchState::Unknown, execution_state:Some(ShellCommandExecutionState::OutcomeUnknown), error_code:Some("observation_unknown".into()), result_summary:Some("Job termination could not be confirmed; no replacement was dispatched.".into()) });
                return;
            }
            tokio::time::sleep_until(std::cmp::min(
                cleanup_deadline,
                tokio::time::Instant::now() + Duration::from_secs(1),
            ))
            .await;
        }
    }

    async fn job_log_for_team(&self, job_id: &str, auth: Option<&AuthContext>) -> String {
        let access = crate::runner_http::runner_access_from_auth(auth);
        match self
            .runner_registry
            .hidden_job_log_for_auth(access.as_ref(), job_id, Some(64))
            .await
        {
            Ok((_, stdout, _, _, _)) => stdout.unwrap_or_default(),
            Err(_) => String::new(),
        }
    }
}

#[cfg(test)]
#[path = "tests/team_task.rs"]
mod tests;
