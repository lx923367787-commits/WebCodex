//! Handoff preflight observes existing executions; it never starts replacements.
use super::{
    context, failure, now, store_error, HostGoalOperation, HostGoalOperationStatus, HostGoalRecord,
    ToolCall, ToolResult, ToolRuntime,
};
use crate::auth::AuthContext;
use serde_json::json;

const DEFAULT_TAKEOVER_TIMEOUT_SECS: u64 = 300;

fn takeover_timeout() -> Result<u64, ToolResult> {
    match std::env::var("WEBCODEX_HANDOFF_TAKEOVER_TIMEOUT_SECS") {
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_TAKEOVER_TIMEOUT_SECS),
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=3600).contains(n))
            .ok_or_else(|| {
                failure(
                    "goal_handoff_configuration",
                    "WEBCODEX_HANDOFF_TAKEOVER_TIMEOUT_SECS must be 1..3600 seconds",
                )
            }),
        Err(_) => Err(failure(
            "goal_handoff_configuration",
            "Invalid takeover timeout configuration",
        )),
    }
}

fn required<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str, ToolResult> {
    value
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .ok_or_else(|| failure("invalid_goal_handoff", format!("{name} is required")))
}

fn handoff_response(h: &webcodex_store::HostGoalHandoff) -> ToolResult {
    let granted = h.status == "committed"
        && context::current_window().is_some()
        && context::current_epoch().as_deref() == Some(h.execution_owner_epoch.as_str());
    ToolResult::ok(
        json!({"driver":"host","goal_id":h.goal_id,"handoff":h,"execution_granted":granted,
        "notice":if h.status == "failed" {"交接失败，已停止执行，已保留可人工恢复的状态。仅用户明确要求后才能恢复。"}
            else if granted {"Execution ownership is confirmed for this conversation. Read and claim the current node before business work."}
            else {"No execution permission. Only server-confirmed ownership allows business work; query status after an uncertain response."}}),
    )
}

impl ToolRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn host_goal_handoff_action(
        &self,
        id: &str,
        action: &str,
        handoff_id: Option<&str>,
        attempt_id: Option<&str>,
        checkpoint_sequence: Option<u64>,
        package_sha256: Option<&str>,
        reason: Option<&str>,
        auth: Option<&AuthContext>,
    ) -> Result<ToolResult, ToolResult> {
        let goal = self.host_goal_access(id, auth, false).await?;
        let db = self.host_goal_db()?;
        let h = db
            .read_host_goal_handoff(&goal.owner_key, id)
            .map_err(store_error)?
            .ok_or_else(|| {
                failure(
                    "goal_handoff_missing",
                    "Save a handoff checkpoint for this exact Goal first",
                )
            })?;
        if action == "status" {
            return Ok(handoff_response(&h));
        }
        let handoff_id = required(handoff_id, "handoff_id")?;
        if handoff_id != h.handoff_id {
            return Err(failure("goal_handoff_stale", "Handoff has been superseded"));
        }
        if matches!(action, "begin_attempt" | "fail_attempt" | "abort") {
            // Browser models never control retries. This is the existing local
            // scheduler's operator credential, not an asserted agent identity.
            if !auth.is_some_and(|a| {
                !a.is_oauth_token() && !a.is_agent_token() && (a.is_admin() || a.is_shared_key())
            }) || context::current_window().is_some()
            {
                return Err(failure(
                    "goal_handoff_scheduler_required",
                    "Only the trusted local scheduler manages handoff attempts",
                ));
            }
            if h.status == "committed" || h.status == "failed" {
                return Ok(handoff_response(&h));
            }
            let h = match action {
                "begin_attempt" => {
                    if h.deadline_at.is_some_and(|deadline| now() < deadline) {
                        return Ok(handoff_response(&h));
                    }
                    if h.attempt_id.as_deref() != attempt_id {
                        return Err(failure(
                            "goal_handoff_stale",
                            "Observe the current attempt before reserving another",
                        ));
                    }
                    // Safe failure is preferable to granting a second executor
                    // while an original CLI, Job, or request could still write.
                    if let Err(error) = self.quiesce_host_goal_handoff(&goal, &h, auth).await {
                        let stopped = db
                            .abort_host_goal_handoff_attempt(
                                &goal.owner_key,
                                id,
                                handoff_id,
                                h.attempt_id.as_deref(),
                                error.output["error_kind"]
                                    .as_str()
                                    .unwrap_or("old_execution_not_quiescent"),
                                now(),
                            )
                            .map_err(store_error)?;
                        return Ok(handoff_response(&stopped));
                    }
                    let timeout = takeover_timeout()?;
                    context::guarded_goal_handoff(id, || {
                        db.begin_host_goal_handoff_attempt(
                            &goal.owner_key,
                            id,
                            handoff_id,
                            attempt_id,
                            now(),
                            timeout,
                        )
                    })
                    .map_err(|e| failure("goal_operation_still_active", e))?
                    .map_err(store_error)?
                }
                "fail_attempt" => db
                    .fail_host_goal_handoff_attempt(
                        &goal.owner_key,
                        id,
                        handoff_id,
                        required(attempt_id, "attempt_id")?,
                        reason.unwrap_or("candidate_failed"),
                        now(),
                    )
                    .map_err(store_error)?,
                _ => db
                    .abort_host_goal_handoff_attempt(
                        &goal.owner_key,
                        id,
                        handoff_id,
                        attempt_id,
                        reason.unwrap_or("handoff_aborted"),
                        now(),
                    )
                    .map_err(store_error)?,
            };
            return Ok(handoff_response(&h));
        }
        let invocation = context::invocation_context().filter(|c| c.identity_source == "window_bound")
            .ok_or_else(||failure("goal_handoff_identity_required","A fresh trusted host conversation is required; caller-supplied epochs cannot take over"))?;
        let attempt_id = required(attempt_id, "attempt_id")?;
        let sequence = checkpoint_sequence
            .ok_or_else(|| failure("invalid_goal_handoff", "checkpoint_sequence is required"))?;
        if action == "prepare" {
            if sequence != h.checkpoint_sequence {
                return Err(failure("goal_handoff_stale", "Checkpoint version mismatch"));
            }
            db.bind_host_goal_handoff_candidate(
                &goal.owner_key,
                id,
                handoff_id,
                attempt_id,
                &invocation,
                now(),
            )
            .map_err(store_error)?;
            let snapshot = db
                .read_host_goal_handoff_snapshot(&goal.owner_key, id, &h.source_context_epoch)
                .map_err(store_error)?;
            let checkpoint = snapshot
                .checkpoint
                .filter(|c| c.sequence == sequence)
                .ok_or_else(|| {
                    failure(
                        "goal_handoff_checkpoint_missing",
                        "The exact handoff checkpoint is missing",
                    )
                })?;
            let plan = self.verify_host_goal_plan(&goal, auth).await?;
            self.verify_project_goal_handoff(&goal, &checkpoint, auth)
                .await?;
            // No private memory, dialogue history, other Goal or agent directory
            // is read to construct this task-only package.
            let package = json!({"schema_version":1,"project":goal.project,"goal_id":id,
                "handoff_id":handoff_id,"attempt_id":attempt_id,"checkpoint_sequence":sequence,
                "checkpoint_revision":checkpoint.revision,
                "plan_version":plan.version,"plan_sha256":plan.hash,"plan":plan.plan,
                "progress":checkpoint.state});
            let digest = super::files::sha256(
                serde_json::to_string(&package)
                    .map_err(|e| failure("goal_handoff_package", e.to_string()))?
                    .as_bytes(),
            );
            let prepared = db
                .record_host_goal_handoff_prepared(
                    &goal.owner_key,
                    id,
                    handoff_id,
                    attempt_id,
                    sequence,
                    &digest,
                    &invocation,
                    now(),
                )
                .map_err(store_error)?;
            let mut result = handoff_response(&prepared);
            result.output["package"] = package;
            result.output["package_sha256"] = json!(digest);
            return Ok(result);
        }
        if action == "commit" {
            let digest = required(package_sha256, "package_sha256")?;
            if h.attempt_id.as_deref() != Some(attempt_id)
                || h.candidate_context_epoch.as_deref() != Some(invocation.context_epoch.as_str())
                || h.checkpoint_sequence != sequence
                || h.prepared_package_sha256.as_deref() != Some(digest)
                || (h.status != "committed"
                    && (h.status != "prepared" || h.deadline_at.is_none_or(|d| now() >= d)))
            {
                return Err(failure(
                    "goal_handoff_stale",
                    "Only the prepared, unexpired candidate may commit this exact package",
                ));
            }
            if h.status != "committed" {
                if let Err(error) = self.quiesce_host_goal_handoff(&goal, &h, auth).await {
                    let stopped = db
                        .abort_host_goal_handoff_attempt(
                            &goal.owner_key,
                            id,
                            handoff_id,
                            h.attempt_id.as_deref(),
                            error.output["error_kind"]
                                .as_str()
                                .unwrap_or("old_execution_not_quiescent"),
                            now(),
                        )
                        .map_err(store_error)?;
                    return Ok(handoff_response(&stopped));
                }
            }
            let committed = context::guarded_goal_handoff(id, || {
                db.commit_host_goal_handoff_ready(
                    &goal.owner_key,
                    id,
                    handoff_id,
                    attempt_id,
                    sequence,
                    digest,
                    &invocation,
                    now(),
                )
            })
            .map_err(|e| failure("goal_operation_still_active", e))?
            .map_err(store_error)?;
            self.bind_host_goal_context(&goal, None)?;
            return Ok(handoff_response(&committed));
        }
        Err(failure(
            "invalid_goal_handoff",
            "Unsupported handoff action",
        ))
    }

    async fn quiesce_host_goal_handoff(
        &self,
        goal: &HostGoalRecord,
        handoff: &webcodex_store::HostGoalHandoff,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        context::guarded_goal_handoff(&goal.goal_id, || ())
            .map_err(|e| failure("goal_operation_still_active", e))?;
        let source_fence = self
            .host_goal_db()?
            .read_host_goal_handoff_snapshot(
                &goal.owner_key,
                &goal.goal_id,
                &handoff.source_context_epoch,
            )
            .map_err(store_error)?
            .checkpoint
            .filter(|checkpoint| checkpoint.sequence == handoff.checkpoint_sequence)
            .and_then(|checkpoint| checkpoint.source_fence)
            .ok_or_else(|| {
                failure(
                    "goal_handoff_checkpoint_missing",
                    "The immutable source execution fence is unavailable",
                )
            })?;
        let operations = self
            .host_goal_db()?
            .list_host_goal_operations(&goal.owner_key, &goal.goal_id, 0, 100, true)
            .map_err(store_error)?;
        for operation in operations
            .into_iter()
            .filter(|operation| operation.binding.fence <= source_fence)
        {
            if let Some(job_id) = &operation.job_id {
                if !super::execution_refs::has_native_job_receipt(&operation.tool_name) {
                    return Err(failure(
                        "goal_operation_outcome_unknown",
                        "Untrusted historical Job receipt",
                    ));
                }
                let stop = self
                    .host_goal_internal_tool(
                        ToolCall::StopJob {
                            project: goal.project.clone(),
                            job_id: job_id.clone(),
                            session_id: None,
                            confirm: true,
                        },
                        auth,
                    )
                    .await;
                if !stop.success {
                    return Err(stop);
                }
            } else {
                // No generic kill guesses. A native Run must first have an
                // authoritative terminal receipt from its existing mechanism.
                if !self
                    .stop_host_goal_native_run(goal, &operation, auth)
                    .await?
                {
                    return Err(failure(
                        "goal_operation_outcome_unknown",
                        "Cannot identify and stop the original execution chain",
                    ));
                }
            }
        }
        let remaining = self
            .observe_host_goal_handoff_operations(goal, auth, Some(source_fence))
            .await?;
        if !remaining.is_empty() {
            return Err(failure(
                "goal_handoff_execution_active",
                "Old execution chain has not authoritatively stopped",
            ));
        }
        Ok(())
    }
    pub(crate) fn host_goal_context_gate(
        &self,
        principal: &str,
        allow_draining: bool,
    ) -> Result<(), ToolResult> {
        let call_context = context::invocation_context();
        if context::current_owner().is_some() && call_context.is_none() {
            return Err(failure("goal_context_missing", "Managed Goal work requires the host conversation identity or a fixed explicit context_epoch"));
        }
        self.host_goal_db()?
            .gate_host_context(principal, call_context.as_ref(), allow_draining)
            .map_err(store_error)
    }

    /// Saving a checkpoint may retain known old operations while business is
    /// frozen. Ownership transfer separately requires their confirmed stop.
    pub(super) async fn prepare_host_goal_handoff(
        &self,
        goal: &HostGoalRecord,
        auth: Option<&AuthContext>,
    ) -> Result<Vec<HostGoalOperation>, ToolResult> {
        self.observe_host_goal_handoff_operations(goal, auth, None)
            .await
    }

    async fn observe_host_goal_handoff_operations(
        &self,
        goal: &HostGoalRecord,
        auth: Option<&AuthContext>,
        source_fence: Option<u64>,
    ) -> Result<Vec<HostGoalOperation>, ToolResult> {
        context::guarded_goal_handoff(&goal.goal_id, || ())
            .map_err(|e| failure("goal_operation_still_active", e))?;
        let db = self.host_goal_db()?;
        let operations = db
            .list_host_goal_operations(&goal.owner_key, &goal.goal_id, 0, 100, true)
            .map_err(store_error)?;
        let mut verified = Vec::new();
        for operation in operations
            .into_iter()
            .filter(|operation| source_fence.is_none_or(|fence| operation.binding.fence <= fence))
        {
            if operation.job_id.is_some()
                && !super::execution_refs::has_native_job_receipt(&operation.tool_name)
            {
                self.quarantine_host_goal_job_binding(goal, &operation)?;
                return Err(failure("goal_operation_outcome_unknown", "An untrusted historical Job binding was removed. Resolve the original provider operation explicitly before handoff"));
            }
            if operation.status == HostGoalOperationStatus::Unknown
                || operation.status == HostGoalOperationStatus::Intent
            {
                return Err(failure("goal_operation_outcome_unknown", "Reconcile the original operation and save ordinary progress before handing off; its effect is unknown"));
            }
            if let Some(job_id) = operation.job_id.as_ref() {
                let result = self
                    .host_goal_internal_tool(
                        ToolCall::JobStatus {
                            job_id: job_id.clone(),
                            include_command_preview: false,
                        },
                        auth,
                    )
                    .await;
                if !result.success {
                    return Err(result);
                }
                if result.output["job_id"].as_str() != Some(job_id.as_str())
                    || result.output["project"] != goal.project
                {
                    return Err(failure(
                        "goal_job_identity_conflict",
                        "The original Job does not match the saved goal project and execution",
                    ));
                }
                let status = result.output["status"].as_str().unwrap_or("");
                let state = super::execution_refs::job_receipt_status(&result.output);
                if state == HostGoalOperationStatus::Unknown {
                    return Err(failure("goal_operation_outcome_unknown", "Original Job status is not authoritative enough for handoff; observe the same Job, never restart it"));
                }
                let summary = json!({"source":"job_status","job_id":job_id,"status":status,"exit_code":result.output["exit_code"]}).to_string();
                let current = db
                    .finish_host_goal_operation(
                        &goal.owner_key,
                        &goal.goal_id,
                        &operation.operation_id,
                        state,
                        Some(job_id),
                        Some(&summary),
                        now(),
                    )
                    .map_err(store_error)?;
                if !state.terminal() {
                    verified.push(current);
                }
            } else {
                match self
                    .reconcile_host_goal_native_run(goal, &operation, auth)
                    .await
                {
                    Ok(true) => {}
                    Err(result) if result.output["error_kind"] == "goal_run_pending" => {
                        let current = db
                            .get_host_goal_operation(
                                &goal.owner_key,
                                &goal.goal_id,
                                &operation.operation_id,
                            )
                            .map_err(store_error)?
                            .ok_or_else(|| {
                                failure(
                                    "goal_operation_outcome_unknown",
                                    "Original Run receipt disappeared",
                                )
                            })?;
                        if current.status != HostGoalOperationStatus::Running {
                            return Err(failure(
                                "goal_operation_outcome_unknown",
                                "The original Run outcome remains unknown",
                            ));
                        }
                        verified.push(current);
                    }
                    Err(result) => return Err(result),
                    Ok(false) => {
                        return Err(failure(
                            "goal_operation_outcome_unknown",
                            "Pending operation has no authoritative Job or Run receipt",
                        ))
                    }
                }
            }
        }
        Ok(verified)
    }
}
