//! Agent task tool family. Admission is owned by the canonical pipeline.
use crate::auth::AuthContext;
use crate::tool_runtime::{ToolCall, ToolResult, ToolRuntime};
impl ToolRuntime {
    pub(super) async fn dispatch_agent_task_family(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        match call {
            ToolCall::CreateAgentTask {
                title,
                instruction,
                assignee_agent_id,
                source_conversation_id,
                source_message_id,
                referenced_project_id,
                idempotency_key,
            } => self.create_agent_task(
                auth,
                title,
                instruction,
                assignee_agent_id,
                source_conversation_id,
                source_message_id,
                referenced_project_id,
                idempotency_key,
            ),

            ToolCall::ListAgentTasks {
                assignee_agent_id,
                offset,
                limit,
            } => self.list_agent_tasks(auth, assignee_agent_id, offset, limit),

            ToolCall::ReadAgentTask { task_id } => self.read_agent_task(auth, task_id),

            ToolCall::AssignAgentTask {
                task_id,
                assignee_agent_id,
            } => self.assign_agent_task(auth, task_id, assignee_agent_id),

            ToolCall::StartAgentTaskAttempt {
                task_id,
                assignee_agent_id,
                idempotency_key,
            } => self.start_agent_task_attempt(auth, task_id, assignee_agent_id, idempotency_key),

            ToolCall::StartAgentTaskCodingRun {
                project,
                task_id,
                attempt_id,
                assignee_agent_id,
                attempt_fence,
                attempt_controller_generation,
                provider_id,
                config,
                timeout_secs,
            } => {
                // Keep this relatively large orchestration future off the shared dispatch
                // future's inline state so unrelated tool calls do not inherit its stack cost.
                Box::pin(self.start_agent_task_coding_run(
                    auth,
                    project,
                    task_id,
                    attempt_id,
                    assignee_agent_id,
                    attempt_fence,
                    attempt_controller_generation,
                    provider_id,
                    config,
                    timeout_secs,
                ))
                .await
            }

            ToolCall::ReconcileAgentTaskCodingRun {
                task_id,
                attempt_id,
            } => Box::pin(self.reconcile_agent_task_coding_run(auth, task_id, attempt_id)).await,

            ToolCall::HeartbeatAgentTaskAttempt {
                task_id,
                attempt_id,
                assignee_agent_id,
                attempt_fence,
                attempt_controller_generation,
            } => self.heartbeat_agent_task_attempt(
                auth,
                task_id,
                attempt_id,
                assignee_agent_id,
                attempt_fence,
                attempt_controller_generation,
            ),

            ToolCall::CompleteAgentTaskAttempt {
                task_id,
                attempt_id,
                assignee_agent_id,
                attempt_fence,
                attempt_controller_generation,
                outcome,
                terminal_result,
                terminal_reason,
                completion_key,
            } => self.complete_agent_task_attempt(
                auth,
                task_id,
                attempt_id,
                assignee_agent_id,
                attempt_fence,
                attempt_controller_generation,
                outcome,
                terminal_result,
                terminal_reason,
                completion_key,
            ),

            _ => unreachable!("typed call routed to the wrong agent_task handler"),
        }
    }
}
