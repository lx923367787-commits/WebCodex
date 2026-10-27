//! Immutable task scope for a successor run. Kept on the run, never on a
//! removable conversation binding or replaceable handoff projection.
use super::*;

fn denied() -> AgentIsolationError {
    AgentIsolationError("agent_task_scope_denied")
}

fn components(path: &str) -> Result<Vec<&str>, AgentIsolationError> {
    if path.is_empty() || path.contains(['\\', ':', '\0']) {
        return Err(denied());
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == ".." || p.ends_with([' ', '.']))
    {
        return Err(denied());
    }
    Ok(parts)
}

fn task_directory(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    goal_id: &str,
) -> Result<String, AgentIsolationError> {
    task_directory_with_legacy_root(conn, binding, goal_id, false)
}

fn task_directory_with_legacy_root(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    goal_id: &str,
    allow_root: bool,
) -> Result<String, AgentIsolationError> {
    let path: Option<String> = conn.query_row("SELECT json_extract(snapshot_json,'$.task_directory') FROM wc_host_goals WHERE goal_id=?1 AND owner_key=?2 AND project=?3",params![goal_id,binding.host_goal_owner_key(),binding.project()], |r|r.get(0)).optional().map_err(unavailable)?.flatten();
    let path = path.ok_or_else(denied)?;
    let parts = components(&path)?;
    if (!allow_root && parts.len() < 2) || parts[0] != "workspace" {
        return Err(denied());
    }
    Ok(path)
}

fn prefix(a: &[&str], b: &[&str]) -> bool {
    a.len() <= b.len()
        && a.iter()
            .zip(b)
            .all(|(left, right)| left.to_lowercase() == right.to_lowercase())
}

fn exclusive_task_directory(
    conn: &Connection,
    source: &VerifiedAgentRun,
    goal_id: &str,
) -> Result<(), AgentIsolationError> {
    let path = task_directory(conn, source, goal_id)?;
    let parts = components(&path)?;
    let mut statement = conn.prepare("SELECT json_extract(snapshot_json,'$.task_directory') FROM wc_host_goals WHERE owner_key=?1 AND goal_id<>?2").map_err(unavailable)?;
    let others = statement
        .query_map(params![source.host_goal_owner_key(), goal_id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    for other in others {
        // A legacy whole-workspace Goal is not an exclusive authorization boundary.
        let other = other.unwrap_or_else(|| "workspace".into());
        let other_parts = components(&other)?;
        if prefix(&parts, &other_parts) || prefix(&other_parts, &parts) {
            return Err(denied());
        }
    }
    Ok(())
}

pub(crate) fn agent_run_task_goal_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
) -> Result<Option<String>, AgentIsolationError> {
    validate_agent_run_on_connection(conn, binding)?;
    conn.query_row(
        "SELECT task_goal_id FROM wc_agent_runs WHERE run_id=?1",
        [binding.run_id()],
        |r| r.get(0),
    )
    .map_err(unavailable)
}

pub(crate) fn agent_run_goal_scope_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    goal_id: Option<&str>,
) -> Result<(), AgentIsolationError> {
    super::enrollment::authorize_enrollment_goal_on_connection(conn, binding, goal_id)?;
    if agent_run_task_goal_on_connection(conn, binding)?
        .as_deref()
        .is_some_and(|scope| Some(scope) != goal_id)
    {
        return Err(denied());
    }
    Ok(())
}

/// Called only by the composed Goal/run handoff transaction before activation.
pub(crate) fn bind_pending_task_goal_on_connection(
    conn: &Connection,
    source: &VerifiedAgentRun,
    pending_run_id: &str,
    goal_id: &str,
) -> Result<(), AgentIsolationError> {
    pending_agent_run_on_connection(conn, source, pending_run_id)?;
    agent_run_goal_scope_on_connection(conn, source, Some(goal_id))?;
    exclusive_task_directory(conn, source, goal_id)?;
    let owned: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goals WHERE goal_id=?1 AND owner_key=?2 AND project=?3)", params![goal_id,source.host_goal_owner_key(),source.project()], |r|r.get(0)).map_err(unavailable)?;
    if !owned {
        return Err(denied());
    }
    let changed = conn.execute("UPDATE wc_agent_runs SET task_goal_id=COALESCE(task_goal_id,?2) WHERE run_id=?1 AND state='pending' AND (task_goal_id IS NULL OR task_goal_id=?2)", params![pending_run_id,goal_id]).map_err(unavailable)?;
    if changed != 1 {
        return Err(denied());
    }
    Ok(())
}

pub(super) enum Target<'a> {
    Goal(&'a str),
    Path { path: &'a str, write: bool },
}

pub(super) fn workspace_write_gate(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    goal_id: &str,
) -> Result<(), AgentIsolationError> {
    agent_run_business_gate_on_connection(conn, binding)?;
    let context = webcodex_core::host_goal::HostGoalInvocationContext {
        context_epoch: binding.run_id().into(),
        identity_source: "agent_run".into(),
    };
    crate::host_goal_context::context_goal_gate(
        conn,
        &binding.host_goal_owner_key(),
        Some(&context),
        goal_id,
        false,
    )
    .map_err(|e| AgentIsolationError::new(e.code()))?;
    // Execution ownership, Goal phase, attempt identity and fence are inspected
    // in the same IMMEDIATE transaction retained across the filesystem effect.
    let current: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goals g JOIN wc_host_goal_attempts a ON a.goal_id=g.goal_id AND a.attempt_id=json_extract(g.snapshot_json,'$.current_attempt_id') WHERE g.goal_id=?1 AND g.owner_key=?2 AND g.project=?3 AND g.phase='executing' AND json_extract(g.snapshot_json,'$.plan_materialized')=1 AND json_extract(g.snapshot_json,'$.confirmation.plan_version')=json_extract(g.snapshot_json,'$.plan_version') AND json_extract(g.snapshot_json,'$.confirmation.plan_hash')=json_extract(g.snapshot_json,'$.plan_hash') AND json_extract(a.snapshot_json,'$.context_epoch')=?4 AND json_extract(a.snapshot_json,'$.identity_source')='agent_run' AND json_extract(a.snapshot_json,'$.status')='running' AND json_extract(a.snapshot_json,'$.binding.fence')=json_extract(g.snapshot_json,'$.fence') AND json_extract(a.snapshot_json,'$.binding.plan_version')=json_extract(g.snapshot_json,'$.plan_version') AND json_extract(a.snapshot_json,'$.binding.node_id')=json_extract(g.snapshot_json,'$.current_node_id'))",params![goal_id,binding.host_goal_owner_key(),binding.project(),binding.run_id()], |r|r.get(0)).map_err(unavailable)?;
    if !current {
        return Err(denied());
    }
    Ok(())
}

pub(super) fn authorize_target(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    action: &str,
    target: Option<Target<'_>>,
) -> Result<(), AgentIsolationError> {
    let Some(goal_id) = agent_run_task_goal_on_connection(conn, binding)? else {
        if let Some(Target::Path { path, write: true }) = target {
            if path.split('/').next() == Some("workspace") {
                let bound:Option<(String,Option<String>,Option<String>)>=conn.query_row("SELECT json_extract(binding_json,'$.goal_id'),json_extract(binding_json,'$.context_epoch'),json_extract(binding_json,'$.project') FROM wc_host_goal_windows WHERE owner_key=?1 AND window_key=?2",params![binding.host_goal_owner_key(),binding.run_id()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(unavailable)?;
                if let Some((goal_id, epoch, project)) = bound {
                    if epoch.as_deref() != Some(binding.run_id())
                        || project.as_deref() != Some(binding.project())
                    {
                        return Err(denied());
                    }
                    let parts = components(path)?;
                    let directory = task_directory_with_legacy_root(conn, binding, &goal_id, true)?;
                    let directory_parts = components(&directory)?;
                    if !parts.starts_with(&directory_parts) || parts.len() == directory_parts.len()
                    {
                        return Err(denied());
                    }
                    workspace_write_gate(conn, binding, &goal_id)?;
                }
            }
        }
        return Ok(());
    };
    match target {
        None => Err(denied()),
        Some(Target::Goal(target_goal)) => {
            if target_goal != goal_id || action == "private_write" {
                return Err(denied());
            }
            if matches!(action, "goal_plan_publish" | "goal_checkpoint_publish") {
                let context = webcodex_core::host_goal::HostGoalInvocationContext {
                    context_epoch: binding.run_id().into(),
                    identity_source: "agent_run".into(),
                };
                crate::host_goal_context::context_goal_gate(
                    conn,
                    &binding.host_goal_owner_key(),
                    Some(&context),
                    target_goal,
                    true,
                )
                .map_err(|e| AgentIsolationError::new(e.code()))?;
            }
            Ok(())
        }
        Some(Target::Path { path, write }) => {
            let parts = components(path)?;
            // The user explicitly keeps this logical Agent's own memory across
            // runs. The broker still confines it to that Agent's namespace.
            if parts[0] == "memory" {
                if write {
                    if parts.len() == 1 {
                        return Err(denied());
                    }
                    agent_run_business_gate_on_connection(conn, binding)?;
                }
                return Ok(());
            }
            if parts.len() < 2 {
                return Err(denied());
            }
            match parts[0] {
                "workspace" => {
                    let directory = task_directory(conn, binding, &goal_id)?;
                    let directory_parts = components(&directory)?;
                    if !parts.starts_with(&directory_parts)
                        || (write && parts.len() == directory_parts.len())
                    {
                        return Err(denied());
                    }
                    if write {
                        workspace_write_gate(conn, binding, &goal_id)?;
                    }
                    Ok(())
                }
                "goals" | "checkpoints" | "handoff" if !write && parts[1] == goal_id => Ok(()),
                _ => Err(denied()),
            }
        }
    }
}

impl Database {
    pub fn agent_run_task_goal(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<Option<String>, AgentIsolationError> {
        agent_run_task_goal_on_connection(&self.conn.lock().unwrap(), binding)
    }

    pub fn with_agent_run_for_goal<T>(
        &self,
        binding: &VerifiedAgentRun,
        action: &str,
        goal_id: &str,
        operation: impl FnOnce() -> Result<T, AgentIsolationError>,
    ) -> Result<T, AgentIsolationError> {
        self.with_agent_run_target(binding, action, Some(Target::Goal(goal_id)), operation)
    }

    pub fn with_agent_run_for_path<T>(
        &self,
        binding: &VerifiedAgentRun,
        action: &str,
        path: &str,
        write: bool,
        operation: impl FnOnce() -> Result<T, AgentIsolationError>,
    ) -> Result<T, AgentIsolationError> {
        self.with_agent_run_target(
            binding,
            action,
            Some(Target::Path { path, write }),
            operation,
        )
    }
}
