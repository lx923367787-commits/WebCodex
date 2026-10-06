//! Handoff preflight observes existing executions; it never starts replacements.
use super::{
    context, failure, now, store_error, HostGoalOperation, HostGoalOperationStatus, HostGoalRecord,
    ToolCall, ToolResult, ToolRuntime,
};
use crate::auth::AuthContext;
use serde_json::json;

impl ToolRuntime {
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

    /// Allow only authoritative, already detached background work across a
    /// handoff. The store compares these exact snapshots inside its commit.
    pub(super) async fn prepare_host_goal_handoff(
        &self,
        goal: &HostGoalRecord,
        auth: Option<&AuthContext>,
    ) -> Result<Vec<HostGoalOperation>, ToolResult> {
        if context::invocation_context().is_none() {
            return Err(failure("goal_context_identity_required", "A handoff requires a fixed conversation identity; no old chat will be declared retired without it"));
        }
        context::guarded_goal_handoff(&goal.goal_id, || ())
            .map_err(|e| failure("goal_operation_still_active", e))?;
        let db = self.host_goal_db()?;
        let operations = db
            .list_host_goal_operations(&goal.owner_key, &goal.goal_id, 0, 100, true)
            .map_err(store_error)?;
        let mut verified = Vec::new();
        for operation in operations {
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
                let state = match status {
                    "completed" if result.output["exit_code"] == 0 => HostGoalOperationStatus::Succeeded,
                    "completed" | "failed" => HostGoalOperationStatus::Failed,
                    "cancelled" | "stopped" => HostGoalOperationStatus::Cancelled,
                    "queued" | "running" => HostGoalOperationStatus::Running,
                    _ => return Err(failure("goal_operation_outcome_unknown", "Original Job status is not authoritative enough for handoff; observe the same Job, never restart it")),
                };
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
