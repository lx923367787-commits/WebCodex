//! Native execution references are receipts of known runtime tools, never IDs
//! inferred from arbitrary plugin output. Run completion is not Goal acceptance.
use super::{
    failure, now, store_error, HostGoalOperation, HostGoalOperationStatus, HostGoalRecord,
    ToolCall, ToolResult, ToolRuntime,
};
use crate::auth::AuthContext;
use serde::{Deserialize, Serialize};
use webcodex_core::coding_agent::{
    validate_run_id, CodingAgentExecutionState, CodingAgentRunState,
};

const SOURCE: &str = "runtime_coding_agent";

pub(crate) use webcodex_core::host_goal::host_goal_tool_has_native_job_receipt as has_native_job_receipt;

/// Request success is distinct from the original process outcome. Reuse the
/// canonical lifecycle, preserving uncertainty even for a terminal lost Job.
pub(crate) fn job_receipt_status(output: &serde_json::Value) -> HostGoalOperationStatus {
    use webcodex_core::runner_job_lifecycle::RunnerJobLifecycle as Job;
    use HostGoalOperationStatus as Operation;
    if output["command_execution_state"] == "outcome_unknown"
        || output["execution_state"] == "outcome_unknown"
        || output["dispatch_certainty"] == "unknown"
    {
        return Operation::Unknown;
    }
    let status = output["job_status"]
        .as_str()
        .or_else(|| output["status"].as_str())
        .unwrap_or("");
    match Job::from_wire(status) {
        Ok(Job::Completed) => match output["exit_code"].as_i64() {
            Some(0) if output["error"].is_null() && output["validation"]["passed"] != false => {
                Operation::Succeeded
            }
            Some(_) => Operation::Failed,
            None => Operation::Unknown,
        },
        Ok(Job::Failed | Job::Timeout | Job::TimedOut) => Operation::Failed,
        Ok(Job::Stopped | Job::Cancelled) => Operation::Cancelled,
        Ok(state) if state.is_active() => Operation::Running,
        _ => Operation::Unknown,
    }
}

pub(super) fn operation_receipt<'a>(
    tool: &str,
    result: &'a ToolResult,
) -> (HostGoalOperationStatus, Option<&'a str>) {
    use HostGoalOperationStatus as Operation;
    let output = &result.output;
    let supplied_job = output["job_id"].as_str();
    let job = supplied_job.filter(|_| has_native_job_receipt(tool));
    let status = output["status"]
        .as_str()
        .or_else(|| output["state"].as_str())
        .unwrap_or("");
    if (supplied_job.is_some() && job.is_none())
        || output["run_id"].is_string()
        || tool == "coding_agent_start"
        || output["execution_state"] == "outcome_unknown"
        || output["dispatch_certainty"] == "unknown"
        || output["failure_kind"] == "outcome_unknown"
        || matches!(status, "outcome_unknown" | "lost" | "recovering")
    {
        return (Operation::Unknown, job);
    }
    // Document tools return a validated worker manifest, not a process result.
    if matches!(
        tool,
        "present_document" | "document_edit" | "document_convert" | "document_restore"
    ) {
        if status == "completed" {
            return (
                if result.success {
                    Operation::Succeeded
                } else {
                    Operation::Failed
                },
                job,
            );
        }
        if status == "pending" && job.is_some() && result.success {
            return (Operation::Running, job);
        }
    }
    if job.is_some() {
        let state = job_receipt_status(output);
        // An unsuccessful observation cannot prove successful business execution.
        return (
            if !result.success && state == Operation::Succeeded {
                Operation::Unknown
            } else {
                state
            },
            job,
        );
    }
    let state = if output["execution_state"] == "not_started"
        || output["dispatch_certainty"] == "not_started"
        || super::super::permissions::is_hard_denied_output(output, result.error.as_deref())
    {
        Operation::Failed
    } else if matches!(status, "cancelled" | "stopped") {
        Operation::Cancelled
    } else if matches!(status, "failed" | "timeout" | "timed_out")
        || output["execution_state"] == "timed_out"
    {
        Operation::Failed
    } else if output["execution_state"] == "completed" {
        match output["exit_code"].as_i64() {
            Some(0)
                if result.success
                    && output["error"].is_null()
                    && output["command_ok"] != false
                    && output["passed"] != false =>
            {
                Operation::Succeeded
            }
            Some(_) => Operation::Failed,
            None => Operation::Unknown,
        }
    } else if output["execution_state"].is_string()
        || output["command_completed"] == false
        || matches!(
            tool,
            "run_shell" | "run_process" | "run_script" | "run_job" | "run_detached_process"
        )
    {
        Operation::Unknown
    } else if result.success && output["exit_code"].as_i64().is_none_or(|code| code == 0) {
        Operation::Succeeded
    } else {
        Operation::Unknown
    };
    (state, None)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRunReceipt {
    source: String,
    run_id: String,
    state: Option<CodingAgentRunState>,
    execution_state: CodingAgentExecutionState,
}

impl NativeRunReceipt {
    fn from_output(output: &serde_json::Value) -> Option<Self> {
        let run_id = output["run_id"].as_str()?;
        validate_run_id(run_id).ok()?;
        let execution_state =
            CodingAgentExecutionState::from_str(output["execution_state"].as_str()?)?;
        let state = output["state"]
            .as_str()
            .and_then(CodingAgentRunState::from_str);
        Some(Self {
            source: SOURCE.into(),
            run_id: run_id.into(),
            state,
            execution_state,
        })
    }

    fn status(&self) -> HostGoalOperationStatus {
        use CodingAgentExecutionState as Execution;
        use CodingAgentRunState as Run;
        match (&self.state, self.execution_state) {
            (Some(Run::Completed), Execution::Completed) => HostGoalOperationStatus::Succeeded,
            (Some(Run::Failed), Execution::NotStarted | Execution::Completed) => {
                HostGoalOperationStatus::Failed
            }
            (Some(Run::Cancelled), Execution::NotStarted | Execution::Completed) => {
                HostGoalOperationStatus::Cancelled
            }
            (Some(Run::Starting), Execution::NotStarted)
            | (Some(Run::Running | Run::WaitingPermission), Execution::Started) => {
                HostGoalOperationStatus::Running
            }
            (None, Execution::NotStarted) => HostGoalOperationStatus::Failed,
            _ => HostGoalOperationStatus::Unknown,
        }
    }

    fn summary(&self) -> String {
        serde_json::to_string(self).expect("native run receipt is serializable")
    }
}

fn saved_native_run(operation: &HostGoalOperation) -> Option<NativeRunReceipt> {
    if operation.tool_name != "coding_agent_start" {
        return None;
    }
    let summary = operation.result_summary.as_deref()?;
    if summary.len() > 1024 {
        return None;
    }
    let saved: NativeRunReceipt = serde_json::from_str(summary).ok()?;
    if saved.source != SOURCE || validate_run_id(&saved.run_id).is_err() {
        return None;
    }
    Some(saved)
}

/// Only the native start tool can create this trusted marker. A plugin with an
/// identically shaped `run_id` does not enter the native observation path.
pub(super) fn native_run_receipt(
    tool: &str,
    result: &ToolResult,
) -> Option<(HostGoalOperationStatus, String)> {
    if tool != "coding_agent_start" {
        return None;
    }
    let receipt = NativeRunReceipt::from_output(&result.output)?;
    let status =
        if !result.success && receipt.execution_state != CodingAgentExecutionState::NotStarted {
            HostGoalOperationStatus::Unknown
        } else {
            receipt.status()
        };
    Some((status, receipt.summary()))
}

impl ToolRuntime {
    pub(crate) async fn reconcile_host_goal_native_run(
        &self,
        goal: &HostGoalRecord,
        operation: &HostGoalOperation,
        auth: Option<&AuthContext>,
    ) -> Result<bool, ToolResult> {
        let Some(original) = saved_native_run(operation) else {
            return Ok(false);
        };
        if operation.status.terminal() {
            return Ok(true);
        }
        let result = self
            .host_goal_internal_tool(
                ToolCall::CodingAgentObserve {
                    run_id: original.run_id.clone(),
                    after_observation_token: None,
                    wait_secs: None,
                },
                auth,
            )
            .await;
        if !result.success {
            return Err(result);
        }
        let observed = NativeRunReceipt::from_output(&result.output)
            .filter(|r| r.state.is_some())
            .ok_or_else(|| {
                failure(
                    "goal_run_observation_invalid",
                    "Native Run observation did not contain a valid state and execution certainty",
                )
            })?;
        if observed.run_id != original.run_id || result.output["project"] != goal.project {
            return Err(failure(
                "goal_run_identity_conflict",
                "Native Run observation belongs to another execution or project",
            ));
        }
        let state = observed.status();
        self.host_goal_db()?
            .finish_host_goal_operation(
                &goal.owner_key,
                &goal.goal_id,
                &operation.operation_id,
                state,
                None,
                Some(&observed.summary()),
                now(),
            )
            .map_err(store_error)?;
        if state.terminal() {
            return Ok(true);
        }
        Err(ToolResult::err_with_output(
            if state == HostGoalOperationStatus::Unknown {
                "The original CodingAgentRun outcome remains unknown; reconcile the same Run without starting a replacement."
            } else {
                "The original CodingAgentRun is still active or waiting for permission; observe or cancel that exact Run before continuing."
            },
            serde_json::json!({"error_kind":if state==HostGoalOperationStatus::Unknown {"goal_run_outcome_unknown"}else{"goal_run_pending"},"run_id":observed.run_id,"state":observed.state,"execution_state":observed.execution_state,
                "continuation":{"tool":"coding_agent_observe","arguments":{"run_id":observed.run_id}}}),
        ))
    }

    pub(super) async fn host_goal_admit_native_cancel(
        &self,
        goal_id: &str,
        run_id: &str,
        project: Option<&str>,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        validate_run_id(run_id).map_err(|e| failure("invalid_goal_run", e))?;
        let goal = self.host_goal_access(goal_id, auth, false).await?;
        if project.is_some_and(|project| project != goal.project) {
            return Err(failure(
                "goal_project_mismatch",
                "Cancel the native Run through its exact goal project",
            ));
        }
        let operation = self
            .host_goal_db()?
            .find_host_goal_operation_by_native_run(&goal.owner_key, goal_id, run_id)
            .map_err(store_error)?;
        if !operation
            .as_ref()
            .and_then(saved_native_run)
            .is_some_and(|r| r.run_id == run_id)
        {
            return Err(failure(
                "goal_run_mismatch",
                "This native Run is not recorded in the selected goal",
            ));
        }
        // The native cancellation handler still checks its original authority,
        // provider/Runner identity, and lifecycle policy before any effect.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_goal_operation_receipts_require_actual_execution_completion() {
        use HostGoalOperationStatus as Operation;
        for (output, expected) in [
            (
                json!({"execution_state":"completed","exit_code":0}),
                Operation::Succeeded,
            ),
            (
                json!({"execution_state":"completed","exit_code":2}),
                Operation::Failed,
            ),
            (json!({"execution_state":"completed"}), Operation::Unknown),
            (json!({"execution_state":"started"}), Operation::Unknown),
            (
                json!({"execution_state":"timed_out","exit_code":null}),
                Operation::Failed,
            ),
            (json!({"execution_state":"not_started"}), Operation::Failed),
            (
                json!({"execution_state":"outcome_unknown","exit_code":0}),
                Operation::Unknown,
            ),
            (json!({}), Operation::Unknown),
            (
                json!({"job_id":"job","status":"completed","exit_code":0}),
                Operation::Succeeded,
            ),
            (
                json!({"job_id":"job","job_status":"completed","exit_code":0}),
                Operation::Succeeded,
            ),
            (
                json!({"job_id":"job","status":"completed"}),
                Operation::Unknown,
            ),
            (
                json!({"job_id":"job","status":"completed","exit_code":0,"error":"cleanup failed"}),
                Operation::Failed,
            ),
            (
                json!({"job_id":"job","status":"failed","exit_code":1}),
                Operation::Failed,
            ),
            (
                json!({"job_id":"job","status":"failed","command_execution_state":"outcome_unknown"}),
                Operation::Unknown,
            ),
            (
                json!({"job_id":"job","status":"timeout"}),
                Operation::Failed,
            ),
            (
                json!({"job_id":"job","status":"timed_out"}),
                Operation::Failed,
            ),
            (
                json!({"job_id":"job","status":"cancelled","exit_code":0}),
                Operation::Cancelled,
            ),
            (
                json!({"job_id":"job","status":"stopped"}),
                Operation::Cancelled,
            ),
            (
                json!({"job_id":"job","status":"lost","exit_code":0}),
                Operation::Unknown,
            ),
            (
                json!({"job_id":"job","status":"recovering"}),
                Operation::Unknown,
            ),
            (
                json!({"job_id":"job","status":"running"}),
                Operation::Running,
            ),
            (
                json!({"job_id":"job","status":"stop_requested"}),
                Operation::Running,
            ),
            (
                json!({"job_id":"job","execution_state":"not_started","failure_kind":"idempotency_recovery_required"}),
                Operation::Unknown,
            ),
        ] {
            let result = ToolResult::ok(output.clone());
            assert_eq!(
                operation_receipt("run_process", &result).0,
                expected,
                "{output}"
            );
        }
        let failed = ToolResult::err_with_output(
            "response failed",
            json!({"job_id":"job","status":"completed","exit_code":0}),
        );
        assert_eq!(
            operation_receipt("run_process", &failed).0,
            Operation::Unknown
        );
    }

    #[test]
    fn host_goal_operation_receipts_keep_native_job_authority_and_document_manifest_semantics() {
        use HostGoalOperationStatus as Operation;
        let forged =
            ToolResult::ok(json!({"job_id":"existing-job","status":"completed","exit_code":0}));
        assert_eq!(
            operation_receipt("plugin_call", &forged),
            (Operation::Unknown, None)
        );
        let completed = ToolResult::ok(json!({"job_id":"document-job","status":"completed"}));
        assert_eq!(
            operation_receipt("document_edit", &completed),
            (Operation::Succeeded, Some("document-job"))
        );
        let pending = ToolResult::ok(json!({"job_id":"document-job","status":"pending"}));
        assert_eq!(
            operation_receipt("document_edit", &pending).0,
            Operation::Running
        );
        let unknown = ToolResult::err_with_output(
            "manifest missing",
            json!({"job_id":"document-job","failure_kind":"outcome_unknown"}),
        );
        assert_eq!(
            operation_receipt("document_edit", &unknown).0,
            Operation::Unknown
        );
        assert_eq!(
            operation_receipt("write_file", &ToolResult::ok(json!({"written":true}))).0,
            Operation::Succeeded
        );
        for tool in ["cargo_check", "cargo_test", "cargo_fmt", "go_test"] {
            let handoff = ToolResult::ok(
                json!({"job_id":"validation-job","job_status":"running","execution_state":"started","command_completed":false}),
            );
            assert_eq!(
                operation_receipt(tool, &handoff),
                (Operation::Running, Some("validation-job")),
                "{tool}"
            );
        }
        for output in [
            json!({"execution_state":"completed","exit_code":0,"error":"cleanup failed"}),
            json!({"execution_state":"completed","exit_code":0,"passed":false}),
            json!({"job_id":"validation-job","status":"completed","exit_code":0,"validation":{"passed":false}}),
        ] {
            assert_eq!(
                operation_receipt("cargo_test", &ToolResult::ok(output)).0,
                Operation::Failed
            );
        }
    }

    #[test]
    fn host_goal_native_receipts_do_not_trust_plugin_ids_or_unknown_terminal_claims() {
        let output = json!({"run_id":"wc_agent_run_example","state":"completed","execution_state":"completed","project":"p","large_provider_text":"x".repeat(16000)});
        assert!(native_run_receipt("plugin_call", &ToolResult::ok(output.clone())).is_none());
        let (status, summary) =
            native_run_receipt("coding_agent_start", &ToolResult::ok(output)).unwrap();
        assert_eq!(status, HostGoalOperationStatus::Succeeded);
        assert!(summary.len() < 1024);
        assert!(!summary.contains("large_provider_text"));
        for (state, execution) in [
            ("lost", "outcome_unknown"),
            ("completed", "outcome_unknown"),
            ("running", "outcome_unknown"),
        ] {
            let (status,_)=native_run_receipt("coding_agent_start",&ToolResult::ok(json!({"run_id":"wc_agent_run_example","state":state,"execution_state":execution}))).unwrap();
            assert_eq!(status, HostGoalOperationStatus::Unknown);
        }
        let (status,_)=native_run_receipt("coding_agent_start",&ToolResult::ok(json!({"run_id":"wc_agent_run_example","state":"waiting_permission","execution_state":"started"}))).unwrap();
        assert_eq!(status, HostGoalOperationStatus::Running);
    }
}
