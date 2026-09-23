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
