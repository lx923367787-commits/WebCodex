//! Goal tool family. Admission is owned by the canonical pipeline.
use crate::auth::AuthContext;
use crate::tool_runtime::{ToolCall, ToolResult, ToolRuntime};
impl ToolRuntime {
    pub(super) async fn dispatch_goal_family(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        match call {
            call @ (ToolCall::GoalModeStart { .. }
            | ToolCall::GoalPlanSave { .. }
            | ToolCall::GoalConfirmPlan { .. }
            | ToolCall::GoalBootstrap { .. }
            | ToolCall::GoalHandoff { .. }
            | ToolCall::GoalClaimNode { .. }
            | ToolCall::GoalCheckpoint { .. }
            | ToolCall::GoalCheckNode { .. }) => {
                Box::pin(self.dispatch_host_goal_tool(call, auth)).await
            }
            ToolCall::GoalStart {
                project,
                provider_id,
                objective,
                criteria,
                artifact_paths,
                reviewer_provider_id,
                limits,
                config,
                idempotency_key,
            } => {
                Box::pin(self.goal_start(
                    webcodex_core::goal::GoalSpec {
                        project,
                        provider_id,
                        objective,
                        criteria,
                        artifact_paths,
                        reviewer_provider_id,
                        limits: limits.unwrap_or_default(),
                        config: config.unwrap_or_default(),
                    },
                    idempotency_key,
                    auth,
                ))
                .await
            }
            ToolCall::GoalRead {
                goal_id,
                round_offset,
                round_limit,
                history_after,
                history_limit,
                history_kind,
                view,
                node_id,
                section,
                cursor,
                source_context_epoch,
            } => {
                if goal_id.starts_with("hgoal_") {
                    return Box::pin(self.host_goal_read(
                        &goal_id,
                        history_after,
                        history_limit,
                        history_kind.as_deref(),
                        view.as_deref(),
                        node_id.as_deref(),
                        section.as_deref(),
                        cursor.as_deref(),
                        source_context_epoch.as_deref(),
                        auth,
                    ))
                    .await;
                }
                if source_context_epoch.is_some() || view.as_deref() == Some("handoff") {
                    return crate::tool_runtime::host_goal::failure(
                        "invalid_goal_view",
                        "Handoff evidence is available only for a host-driven Goal",
                    );
                }
                Box::pin(self.goal_read(goal_id, round_offset, round_limit, auth)).await
            }
            ToolCall::GoalList { project, limit } => {
                Box::pin(self.goal_list(project, limit, auth)).await
            }
            ToolCall::GoalCancel { goal_id } => Box::pin(self.goal_cancel(goal_id, auth)).await,
            ToolCall::GoalResume { goal_id } => Box::pin(self.goal_resume(goal_id, auth)).await,

            _ => unreachable!("typed call routed to the wrong goal handler"),
        }
    }
}
