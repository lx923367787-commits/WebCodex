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
    // Handoff scopes private Goal data, not general runtime capabilities.
    if !tool.starts_with("goal_") {
        return Ok(());
    }
    let scope = db
        .agent_run_task_goal(binding)
        .map_err(|e| denial(e.code()))?;
    let Some(goal_id) = scope else {
        return Ok(());
    };
    if tool == "goal_bootstrap"
        && arguments.get("goal_id").is_none_or(Value::is_null)
        && db
            .agent_enrollment_goal(binding)
            .map_err(|error| denial(error.code()))?
            .as_deref()
            == Some(goal_id.as_str())
    {
        return Ok(());
    }
    if matches!(
        tool,
        "agent_identity"
            | "agent_assignment"
            | "bind_chat_task_goal"
            | "agent_private"
            | "agent_meeting"
            | "agent_project_files"
            | "agent_process"
    ) || crate::agent_team::supports(tool)
        || crate::tool_runtime::named_project_files::supports(tool)
    {
        // Each adapter rechecks its own authority: private Goal write fences,
        // meeting membership, or the enrolled named-project grant. This list
        // alone grants neither a filesystem target nor execution.
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
