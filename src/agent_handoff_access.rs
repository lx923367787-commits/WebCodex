//! A successor keeps its Agent's memory and authorized meetings; its business
//! authority stays on this task and cannot be erased by bootstrap/Exit.
use crate::agent_isolation::denial;
use crate::tool_runtime::ToolResult;
use crate::Database;
use serde_json::Value;
use webcodex_store::VerifiedAgentRun;

pub(crate) fn ensure(
    db: &Database,
    binding: &VerifiedAgentRun,
    tool: &str,
    arguments: &Value,
) -> Result<(), ToolResult> {
    let scope = db
        .agent_run_task_goal(binding)
        .map_err(|e| denial(e.code()))?;
    let Some(goal_id) = scope else {
        return Ok(());
    };
    if matches!(
        tool,
        "agent_identity" | "agent_assignment" | "agent_private" | "agent_meeting"
    ) || crate::agent_team::supports(tool)
    {
        // File targets/write fences and meeting membership are checked again
        // in the transaction surrounding the actual operation.
        return Ok(());
    }
    if matches!(
        tool,
        "goal_read"
            | "goal_bootstrap"
            | "goal_plan_save"
            | "goal_confirm_plan"
            | "goal_claim_node"
            | "goal_checkpoint"
            | "goal_handoff"
    ) && arguments["goal_id"].as_str() == Some(goal_id.as_str())
    {
        return Ok(());
    }
    // Retained memory and meeting access never grants another Goal's authority.
    Err(denial("agent_task_scope_denied"))
}
