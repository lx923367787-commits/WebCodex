//! Included as a child of enrollment; uses its existing trusted run issuance.
use super::*;
use webcodex_core::runner_protocol::{AgentExecutionBinding, AgentRunRef};

pub(crate) fn recover_cancelled_cli_worker_on_connection(
    conn: &Connection,
    source: &VerifiedAgentRun,
    old: &AgentExecutionBinding,
) -> Result<VerifiedAgentRun, AgentIsolationError> {
    if old.project != source.project()
        || old.requester.run.agent_id != source.agent_id()
        || old.requester.run.agent_run_id == source.run_id()
    {
        return Err(denied());
    }
    let stored:Option<String>=conn.query_row("SELECT execution_preparation_json FROM wc_team_cli_attempts WHERE attempt_id=?1 AND execution_preparation_state='cancelled' AND execution_start_json IS NULL AND json_extract(record_json,'$.execution_binding') IS NULL AND json_extract(record_json,'$.job_id') IS NULL AND json_extract(record_json,'$.dispatch_state')='terminal' AND json_extract(record_json,'$.execution_state')='not_started' AND json_extract(record_json,'$.error_code')='agent_execution_closed'",[&old.requester.cli_attempt_id],|r|r.get(0)).optional().map_err(unavailable_enrollment)?;
    let exact: AgentExecutionBinding =
        serde_json::from_str(&stored.ok_or_else(denied)?).map_err(unavailable_enrollment)?;
    if exact != *old {
        return Err(denied());
    }
    let enrolled = row(conn, &old.executor.enrollment_id)?.ok_or_else(denied)?;
    if enrolled.agent_id != old.executor.run.agent_id
        || enrolled.request.task_id != old.executor.task_id
        || enrolled.request.purpose != AgentEnrollmentPurpose::CliWorker
        || enrolled.state != "claimed"
        || enrolled.source_agent_id.as_deref() != Some(source.agent_id())
        || enrolled.request.policy.max_subagents != 0
        || enrolled.request.policy.max_cli_calls != 0
        || !assignment_active(conn, &enrolled.agent_id)?
    {
        return Err(denied());
    }
    source_valid(conn, &enrolled.request, Some(source))?;
    crate::agent_isolation::ensure_agent_run_execution_open_on_connection(
        conn,
        &AgentRunRef {
            agent_id: source.agent_id().into(),
            agent_run_id: source.run_id().into(),
            generation: source.generation(),
        },
    )?;
    require_enrollment_public_task_on_connection(
        conn,
        source.owner_user(),
        source.project(),
        &old.executor.task_id,
    )?;
    crate::agent_execution_close::require_agent_run_execution_closed_on_connection(
        conn,
        source.owner_user(),
        source.project(),
        &old.requester.run,
    )?;
    crate::agent_execution_close::require_agent_assignment_execution_closed_on_connection(
        conn,
        source.owner_user(),
        source.project(),
        &old.executor.run.agent_id,
    )?;
    let (maximum,state,goal):(i64,String,Option<String>)=conn.query_row("SELECT (SELECT MAX(generation) FROM wc_agent_runs WHERE agent_id=?1),state,task_goal_id FROM wc_agent_runs WHERE run_id=?2 AND agent_id=?1 AND generation=?3",params![old.executor.run.agent_id,old.executor.run.agent_run_id,old.executor.run.generation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(unavailable_enrollment)?;
    if maximum != old.executor.run.generation || state != "revoked" {
        return Err(AgentIsolationError::new("agent_run_generation_changed"));
    }
    let generation = maximum.checked_add(1).ok_or_else(denied)?;
    let agent = owned_agent(
        conn,
        source.owner_user(),
        source.project(),
        &old.executor.run.agent_id,
    )?;
    let grant = issue_run(
        conn,
        agent,
        generation,
        "active",
        Some(&old.executor.run.agent_run_id),
    )?;
    if let Some(goal) = &goal {
        conn.execute(
            "UPDATE wc_agent_runs SET task_goal_id=?2 WHERE run_id=?1",
            params![grant.run_id, goal],
        )
        .map_err(unavailable_enrollment)?;
    }
    conn.execute("UPDATE wc_agent_enrollments SET delivery_source_run_id=?2,delivery_source_generation=?3 WHERE enrollment_id=?1",params![enrolled.id,source.run_id(),source.generation()]).map_err(unavailable_enrollment)?;
    let binding = verify_agent_run_context_on_connection(
        conn,
        &host_goal_owner_key(source.project(), &old.executor.run.agent_id),
        &grant.run_id,
    )?;
    if let Some(goal) = goal {
        crate::host_goal::recover_handoff_run_on_connection(conn, &binding, &goal, now_ms())
            .map_err(|e| AgentIsolationError::new(e.code()))?;
    }
    audit(
        conn,
        "cli_worker_recover",
        binding.agent_id(),
        binding.run_id(),
    )?;
    // The newly minted bearer token is intentionally dropped, as at prepare.
    Ok(binding)
}
