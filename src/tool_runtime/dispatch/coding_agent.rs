//! Coding agent tool family. Admission is owned by the canonical pipeline.
use crate::auth::AuthContext;
use crate::tool_runtime::{ToolCall, ToolResult, ToolRuntime};
impl ToolRuntime {
    pub(super) async fn dispatch_coding_agent_family(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        match call {
            ToolCall::CodingAgentStart {
                project,
                provider_id,
                idempotency_key,
                instruction,
                config,
                timeout_secs,
                recording_session_id,
            } => {
                Box::pin(self.coding_agent_start(
                    project,
                    provider_id,
                    idempotency_key,
                    instruction,
                    config,
                    timeout_secs,
                    recording_session_id,
                    auth,
                ))
                .await
            }
            ToolCall::CodingAgentObserve {
                run_id,
                after_observation_token,
                wait_secs,
            } => {
                self.coding_agent_observe(run_id, after_observation_token, wait_secs, auth)
                    .await
            }
            ToolCall::CodingAgentCancel { run_id } => self.coding_agent_cancel(run_id, auth).await,

            _ => unreachable!("typed call routed to the wrong coding_agent handler"),
        }
    }
}
