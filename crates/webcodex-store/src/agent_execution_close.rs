//! Durable execution admission/closure on the existing run and CLI-attempt rows.
//! The authenticated transport bridge supplies observations, never model JSON.
use crate::{AgentIsolationError, Database};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use webcodex_core::runner_protocol::{
    compute_members_sha256, AgentExecutionBinding, AgentExecutionClose, AgentExecutionCloseReceipt,
    AgentExecutionCloseRef, AgentRunRef,
};

type Result<T> = std::result::Result<T, AgentIsolationError>;
const MAX_RUNS: usize = 1024;
const MAX_ATTEMPTS: usize = 4096;
fn error(code: &'static str) -> AgentIsolationError {
    AgentIsolationError::new(code)
}
fn sql(_: rusqlite::Error) -> AgentIsolationError {
    error("agent_execution_store_unavailable")
}
fn invalid(_: impl std::fmt::Display) -> AgentIsolationError {
    error("agent_execution_binding_invalid")
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(invalid)
}

/// A trusted adapter must wrap Registry's private-field verified receipt type.
/// This trait is an integration trust boundary, NOT a cryptographic proof.
/// Production must not implement it for wire DTO/JSON, an Agent run, or arbitrary
/// caller data. Store still verifies exact frozen membership and revisions.
pub trait AuthenticatedAgentExecutionCloseReceipt {
    fn authenticated_receipt(&self) -> &AgentExecutionCloseReceipt;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExecutionClosePlan {
    close_ref: AgentExecutionCloseRef,
    owner_user: String,
    project: String,
    runs: Vec<AgentRunRef>,
    requests: Vec<AgentExecutionClose>,
    blocked_attempt_ids: Vec<String>,
    requires_explicit_close: bool,
}
impl AgentExecutionClosePlan {
    pub fn close_ref(&self) -> &AgentExecutionCloseRef {
        &self.close_ref
    }
    pub fn requests(&self) -> &[AgentExecutionClose] {
        &self.requests
    }
    pub fn blocked_attempt_ids(&self) -> &[String] {
        &self.blocked_attempt_ids
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentExecutionCloseProgress {
    pub close_id: String,
    pub complete: bool,
    pub accepted_targets: usize,
    pub required_targets: usize,
    pub unresolved_attempts: usize,
}

pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    let columns = conn
        .prepare("PRAGMA table_info(wc_agent_runs)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (name, definition) in [
        ("execution_close_id", "TEXT"),
        ("execution_close_plan_json", "TEXT"),
        ("execution_close_receipts_json", "TEXT"),
        ("execution_closed_at_unix_ms", "INTEGER"),
    ] {
        if !columns.iter().any(|c| c == name) {
            conn.execute(
                &format!("ALTER TABLE wc_agent_runs ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS wc_agent_execution_close ON wc_agent_runs(execution_close_id);
        CREATE TRIGGER IF NOT EXISTS wc_agent_execution_close_monotonic BEFORE UPDATE OF execution_close_id,execution_closed_at_unix_ms ON wc_agent_runs
        WHEN (OLD.execution_close_id IS NOT NULL AND NEW.execution_close_id IS NOT OLD.execution_close_id)
          OR (OLD.execution_closed_at_unix_ms IS NOT NULL AND NEW.execution_closed_at_unix_ms IS NOT OLD.execution_closed_at_unix_ms)
        BEGIN SELECT RAISE(ABORT,'execution admission cannot reopen'); END;")
}

/// Pending is permitted only for factory preparation. The final binding gate
/// separately requires both runs active. No caller can clear a close tombstone.
pub(crate) fn ensure_agent_run_execution_open_on_connection(
    conn: &Connection,
    run: &AgentRunRef,
) -> Result<()> {
    let open: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2 AND generation=?3 AND state IN ('pending','active') AND expires_at_unix_ms>?4 AND execution_close_id IS NULL)",
        params![run.agent_run_id,run.agent_id,run.generation,now()],|r|r.get(0)).map_err(sql)?;
    if open {
        Ok(())
    } else {
        Err(error("agent_execution_closed"))
    }
}

fn same_run(left: &AgentRunRef, right: &AgentRunRef) -> bool {
    left == right
}
fn run_owned(conn: &Connection, owner: &str, project: &str, run: &AgentRunRef) -> Result<()> {
    let yes:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id WHERE r.run_id=?1 AND r.agent_id=?2 AND r.generation=?3 AND a.owner_user=?4 AND a.project=?5)",params![run.agent_run_id,run.agent_id,run.generation,owner,project],|r|r.get(0)).map_err(sql)?;
    if yes {
        Ok(())
    } else {
        Err(error("agent_execution_binding_mismatch"))
    }
}

pub(crate) fn validate_agent_execution_admission_on_connection(
    conn: &Connection,
    binding: &AgentExecutionBinding,
) -> Result<()> {
    binding.validate().map_err(invalid)?;
    ensure_agent_run_execution_open_on_connection(conn, &binding.requester.run)?;
    ensure_agent_run_execution_open_on_connection(conn, &binding.executor.run)?;
    let owner: String = conn
        .query_row(
            "SELECT owner_user FROM wc_isolated_agents WHERE agent_id=?1",
            [&binding.requester.run.agent_id],
            |r| r.get(0),
        )
        .map_err(sql)?;
    run_owned(conn, &owner, &binding.project, &binding.requester.run)?;
    run_owned(conn, &owner, &binding.project, &binding.executor.run)?;
    for (run, task, operation) in [
        (
            &binding.requester.run,
            binding.requester.source_task_id.as_str(),
            crate::AgentAssignmentOperation::UseCli,
        ),
        (
            &binding.executor.run,
            binding.executor.task_id.as_str(),
            crate::AgentAssignmentOperation::TaskStart,
        ),
    ] {
        let verified = crate::agent_isolation::verify_agent_run_context_on_connection(
            conn,
            &crate::agent_isolation::host_goal_owner_key(&binding.project, &run.agent_id),
            &run.agent_run_id,
        )?;
        crate::agent_isolation::authorize_agent_assignment_on_connection(
            conn, &verified, task, operation,
        )?;
    }
    let requester:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.agent_id=e.agent_id JOIN wc_agent_enrollment_assignments a ON a.agent_id=e.agent_id JOIN wc_agent_tasks t ON t.task_id=e.task_id WHERE e.enrollment_id=?1 AND e.agent_id=?2 AND e.owner_user=?3 AND e.project=?4 AND e.task_id=?5 AND e.state='claimed' AND e.purpose IN ('manager','member') AND json_extract(e.policy_json,'$.max_cli_calls')>0 AND a.state='active' AND t.state IN ('ready','active') AND r.run_id=?6 AND r.generation=?7 AND r.state='active')",params![binding.requester.enrollment_id,binding.requester.run.agent_id,owner,binding.project,binding.requester.source_task_id,binding.requester.run.agent_run_id,binding.requester.run.generation],|r|r.get(0)).map_err(sql)?;
    let worker:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.agent_id=e.agent_id JOIN wc_agent_enrollment_assignments a ON a.agent_id=e.agent_id JOIN wc_agent_tasks t ON t.task_id=e.task_id WHERE e.enrollment_id=?1 AND e.agent_id=?2 AND e.owner_user=?3 AND e.project=?4 AND e.task_id=?5 AND e.state='claimed' AND e.purpose='cli_worker' AND json_extract(e.policy_json,'$.max_subagents')=0 AND json_extract(e.policy_json,'$.max_cli_calls')=0 AND a.state='active' AND t.state IN ('ready','active') AND e.source_agent_id=?6 AND COALESCE(e.delivery_source_run_id,e.source_run_id)=?7 AND COALESCE(e.delivery_source_generation,e.source_generation)=?8 AND r.run_id=?9 AND r.generation=?10 AND r.state='active')",params![binding.executor.enrollment_id,binding.executor.run.agent_id,owner,binding.project,binding.executor.task_id,binding.requester.run.agent_id,binding.requester.run.agent_run_id,binding.requester.run.generation,binding.executor.run.agent_run_id,binding.executor.run.generation],|r|r.get(0)).map_err(sql)?;
    if !requester || !worker {
        return Err(error("agent_execution_assignment_denied"));
    }
    let stored:Option<String>=conn.query_row("SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1 AND project_id=?2 AND root_task_id=?3 AND source_task_id=?4 AND call_index=?5",params![binding.requester.cli_attempt_id,binding.project,binding.requester.root_task_id,binding.requester.source_task_id,binding.requester.call_index],|r|r.get(0)).optional().map_err(sql)?;
    let value: serde_json::Value =
        serde_json::from_str(&stored.ok_or_else(|| error("agent_execution_binding_missing"))?)
            .map_err(invalid)?;
    let frozen: AgentExecutionBinding =
        serde_json::from_value(value["execution_binding"].clone()).map_err(invalid)?;
    if frozen != *binding
        || !matches!(
            value["dispatch_state"].as_str(),
            Some("dispatching" | "bound")
        )
    {
        return Err(error("agent_execution_binding_mismatch"));
    }
    Ok(())
}

fn descendants(
    conn: &Connection,
    owner: &str,
    project: &str,
    source: &AgentRunRef,
) -> Result<Vec<AgentRunRef>> {
    run_owned(conn, owner, project, source)?;
    let delivery = conn
        .prepare("PRAGMA table_info(wc_agent_enrollments)")
        .map_err(sql)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql)?
        .iter()
        .any(|c| c == "delivery_source_run_id");
    let lineage = if delivery {
        "COALESCE(e.delivery_source_run_id,e.source_run_id)=?4 AND COALESCE(e.delivery_source_generation,e.source_generation)=?5 AND NOT (e.delivery_state='reserved' AND e.purpose IN ('member','fallback') AND e.state='issued' AND r.state='pending')"
    } else {
        "e.source_run_id=?4 AND e.source_generation=?5"
    };
    let mut runs = vec![source.clone()];
    let mut index = 0;
    while index < runs.len() {
        let source = &runs[index];
        let query=format!("SELECT r.agent_id,r.run_id,r.generation FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.agent_id=e.agent_id WHERE e.owner_user=?1 AND e.project=?2 AND e.source_agent_id=?3 AND {lineage} ORDER BY r.run_id LIMIT 1025");
        let mut stmt = conn.prepare(&query).map_err(sql)?;
        let children = stmt
            .query_map(
                params![
                    owner,
                    project,
                    source.agent_id,
                    source.agent_run_id,
                    source.generation
                ],
                |r| {
                    Ok(AgentRunRef {
                        agent_id: r.get(0)?,
                        agent_run_id: r.get(1)?,
                        generation: r.get(2)?,
                    })
                },
            )
            .map_err(sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql)?;
        for child in children {
            if !runs.iter().any(|r| same_run(r, &child)) {
                runs.push(child);
            }
        }
        if runs.len() > MAX_RUNS {
            return Err(error("agent_execution_close_limit"));
        }
        index += 1;
    }
    Ok(runs)
}

fn stored_plan(conn: &Connection, source: &AgentRunRef) -> Result<Option<AgentExecutionClosePlan>> {
    let saved:Option<Option<String>>=conn.query_row("SELECT p.execution_close_plan_json FROM wc_agent_runs r JOIN wc_agent_runs p ON p.execution_close_id=r.execution_close_id WHERE r.run_id=?1 AND r.agent_id=?2 AND r.generation=?3 AND p.execution_close_plan_json IS NOT NULL",params![source.agent_run_id,source.agent_id,source.generation],|r|r.get(0)).optional().map_err(sql)?;
    saved
        .flatten()
        .map(|v| serde_json::from_str(&v).map_err(invalid))
        .transpose()
}

fn plan_on_connection(
    conn: &Connection,
    owner: &str,
    project: &str,
    source: &AgentRunRef,
    deadline: i64,
) -> Result<AgentExecutionClosePlan> {
    run_owned(conn, owner, project, source)?;
    if let Some(plan) = stored_plan(conn, source)? {
        return Ok(plan);
    }
    if deadline <= now() {
        return Err(error("agent_execution_close_deadline"));
    }
    let runs = descendants(conn, owner, project, source)?;
    let principal = crate::agent_isolation::enrollment_principal_for_execution(owner);
    let tasks = {
        let mut tasks = BTreeSet::new();
        for run in &runs {
            if let Some(task) = conn
                .query_row(
                    "SELECT task_id FROM wc_agent_enrollments WHERE agent_id=?1",
                    [&run.agent_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(sql)?
            {
                tasks.insert(task);
            }
        }
        tasks
    };
    let mut bindings = Vec::new();
    let mut blocked = Vec::new();
    let mut reserved = Vec::new();
    let preparations = conn
        .prepare("PRAGMA table_info(wc_team_cli_attempts)")
        .map_err(sql)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql)?
        .iter()
        .any(|c| c == "execution_preparation_json");
    for task in &tasks {
        for (label,query) in [
            ("legacy_coding", "SELECT EXISTS(SELECT 1 FROM wc_agent_task_coding_runs WHERE task_id=?1)"),
            ("delegated_agent", "SELECT EXISTS(SELECT 1 FROM wc_team_agent_attempts WHERE source_task_id=?1 AND state<>'confirmed_not_started')"),
            ("fallback", "SELECT EXISTS(SELECT 1 FROM wc_cli_fallbacks WHERE source_task_id=?1 AND state<>'confirmed_not_started')"),
        ] {
            if conn.query_row(query,[&task],|r|r.get::<_,bool>(0)).map_err(sql)? { blocked.push(format!("{label}:{task}")); }
        }
    }
    let run_ids = runs
        .iter()
        .map(|r| r.agent_run_id.as_str())
        .collect::<Vec<_>>();
    let (projection, extra) = if preparations {
        ("execution_preparation_json,execution_preparation_state"," OR json_extract(execution_preparation_json,'$.executor.run.agent_run_id') IN (SELECT value FROM json_each(?5))")
    } else {
        ("NULL,NULL", "")
    };
    let query=format!("SELECT attempt_id,record_json,{projection} FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 AND (source_task_id IN (SELECT value FROM json_each(?4)) OR json_extract(record_json,'$.execution_binding.requester.run.agent_run_id') IN (SELECT value FROM json_each(?5)) OR json_extract(record_json,'$.execution_binding.executor.run.agent_run_id') IN (SELECT value FROM json_each(?5)){extra}) ORDER BY attempt_id LIMIT 4097");
    let rows = conn
        .prepare(&query)
        .map_err(sql)?
        .query_map(
            params![
                principal.kind,
                principal.digest,
                project,
                encode(&tasks)?,
                encode(&run_ids)?
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .map_err(sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql)?;
    if rows.len() > MAX_ATTEMPTS {
        return Err(error("agent_execution_close_limit"));
    }
    for (id, json, preparation, preparation_state) in rows {
        let mut value: serde_json::Value = serde_json::from_str(&json).map_err(invalid)?;
        if value["execution_binding"].is_object() {
            let binding: AgentExecutionBinding =
                serde_json::from_value(value["execution_binding"].clone()).map_err(invalid)?;
            binding.validate().map_err(invalid)?;
            if runs
                .iter()
                .any(|r| r == &binding.requester.run || r == &binding.executor.run)
            {
                if binding.requester.cli_attempt_id != id || binding.project != project {
                    return Err(error("agent_execution_binding_mismatch"));
                }
                run_owned(conn, owner, project, &binding.executor.run)?;
                let exited:bool=conn.query_row("SELECT execution_closed_at_unix_ms IS NOT NULL FROM wc_agent_runs WHERE run_id=?1",[&binding.executor.run.agent_run_id],|r|r.get(0)).map_err(sql)?;
                if !exited {
                    if !runs.iter().any(|r| r == &binding.executor.run) {
                        return Err(error("agent_execution_close_lineage_mismatch"));
                    }
                    bindings.push(binding);
                }
            }
        } else if value["dispatch_state"] == "reserved"
            && value["job_id"].is_null()
            && matches!(preparation_state.as_deref(), None | Some("prepared"))
        {
            let prepared = preparation
                .map(|p| serde_json::from_str::<AgentExecutionBinding>(&p).map_err(invalid))
                .transpose()?;
            if let Some(prepared) = &prepared {
                prepared.identity_hash().map_err(invalid)?;
                if prepared.requester.cli_attempt_id != id
                    || prepared.project != project
                    || !runs.contains(&prepared.executor.run)
                {
                    return Err(error("agent_execution_close_lineage_mismatch"));
                }
                run_owned(conn, owner, project, &prepared.executor.run)?;
            } else if preparation_state.is_some() {
                return Err(error("agent_execution_binding_missing"));
            }
            value["dispatch_state"] = "terminal".into();
            value["execution_state"] = "not_started".into();
            value["error_code"] = "agent_execution_closed".into();
            let revision = value["observation_revision"]
                .as_i64()
                .filter(|r| *r >= 0)
                .and_then(|r| r.checked_add(1))
                .ok_or_else(|| error("agent_execution_binding_invalid"))?;
            value["observation_revision"] = revision.into();
            value["updated_at_unix_ms"] = now().into();
            reserved.push((id, value, prepared));
        } else if !(value["dispatch_state"] == "terminal"
            && value["execution_state"] == "not_started"
            && value["job_id"].is_null())
        {
            blocked.push(id);
        }
    }
    if bindings.len() + blocked.len() + reserved.len() > MAX_ATTEMPTS {
        return Err(error("agent_execution_close_limit"));
    }
    // Any legacy unresolved carrier lacking the new lineage is an explicit
    // blocker, never silently discarded because it lacks a new binding.
    for run in &runs {
        let owner_key = crate::agent_isolation::host_goal_owner_key(project, &run.agent_id);
        let old:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_operations o JOIN wc_host_goals g ON g.goal_id=o.goal_id WHERE g.owner_key=?1 AND o.terminal=0)",[owner_key],|r|r.get(0)).map_err(sql)?;
        if old {
            blocked.push(format!("unresolved_goal:{}", run.agent_run_id));
        }
    }
    let close_ref = AgentExecutionCloseRef {
        close_id: format!("wc_execution_close_{}", uuid::Uuid::new_v4().simple()),
        source: source.clone(),
    };
    let mut groups: BTreeMap<(String, String), Vec<AgentExecutionBinding>> = BTreeMap::new();
    for binding in bindings {
        groups
            .entry((
                binding.target.client_id.clone(),
                binding.target.runner_instance_id.clone(),
            ))
            .or_default()
            .push(binding);
    }
    let mut requests = Vec::new();
    for (_, mut members) in groups {
        members.sort_by(|a, b| a.requester.cli_attempt_id.cmp(&b.requester.cli_attempt_id));
        let request = AgentExecutionClose {
            version: 1,
            close_ref: close_ref.clone(),
            target: members[0].target.clone(),
            deadline_at_ms: deadline,
            members_sha256: compute_members_sha256(&members).map_err(invalid)?,
            members,
        };
        request.validate().map_err(invalid)?;
        requests.push(request);
    }
    // Refuse overlapping closures; their prior source must be reconciled first.
    for run in &runs {
        let (prior,done):(Option<String>,Option<i64>)=conn.query_row("SELECT execution_close_id,execution_closed_at_unix_ms FROM wc_agent_runs WHERE run_id=?1",[&run.agent_run_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sql)?;
        if prior.is_some() && done.is_none() {
            return Err(error("agent_execution_close_conflict"));
        }
    }
    let requires_explicit_close = !requests.is_empty()
        || !blocked.is_empty()
        || reserved.iter().any(|(_, _, prepared)| prepared.is_some());
    let plan = AgentExecutionClosePlan {
        close_ref,
        owner_user: owner.into(),
        project: project.into(),
        runs,
        requests,
        blocked_attempt_ids: blocked,
        requires_explicit_close,
    };
    let complete = plan.requests.is_empty() && plan.blocked_attempt_ids.is_empty();
    for run in &plan.runs {
        conn.execute("UPDATE wc_agent_runs SET execution_close_id=?2,execution_closed_at_unix_ms=?3 WHERE run_id=?1 AND execution_close_id IS NULL",params![run.agent_run_id,plan.close_ref.close_id,complete.then(now)]).map_err(sql)?;
    }
    conn.execute("UPDATE wc_agent_runs SET execution_close_plan_json=?2,execution_close_receipts_json='[]' WHERE run_id=?1",params![source.agent_run_id,encode(&plan)?]).map_err(sql)?;
    for (id, value, prepared) in reserved {
        conn.execute(
            "UPDATE wc_team_cli_attempts SET record_json=?2 WHERE attempt_id=?1",
            params![id, encode(&value)?],
        )
        .map_err(sql)?;
        if let Some(prepared) = prepared {
            let changed=conn.execute("UPDATE wc_team_cli_attempts SET execution_preparation_state='cancelled' WHERE attempt_id=?1 AND execution_preparation_state='prepared'",[&id]).map_err(sql)?;
            if changed != 1 {
                return Err(error("agent_execution_binding_mismatch"));
            }
            conn.execute("UPDATE wc_agent_runs SET state='revoked' WHERE run_id=?1 AND agent_id=?2 AND generation=?3",params![prepared.executor.run.agent_run_id,prepared.executor.run.agent_id,prepared.executor.run.generation]).map_err(sql)?;
        }
    }
    Ok(plan)
}

fn progress(
    conn: &Connection,
    plan: &AgentExecutionClosePlan,
) -> Result<AgentExecutionCloseProgress> {
    let (done,saved):(Option<i64>,String)=conn.query_row("SELECT execution_closed_at_unix_ms,execution_close_receipts_json FROM wc_agent_runs WHERE run_id=?1 AND execution_close_id=?2",params![plan.close_ref.source.agent_run_id,plan.close_ref.close_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sql)?;
    let receipts: Vec<AgentExecutionCloseReceipt> =
        serde_json::from_str(&saved).map_err(invalid)?;
    Ok(AgentExecutionCloseProgress {
        close_id: plan.close_ref.close_id.clone(),
        complete: done.is_some(),
        accepted_targets: receipts.len(),
        required_targets: plan.requests.len(),
        unresolved_attempts: plan.blocked_attempt_ids.len(),
    })
}

/// Physical stop evidence for one original frozen CLI execution. This never
/// interprets a normal Job/Task terminal enum as a process-tree receipt.
pub(crate) fn agent_execution_binding_stop_proven_on_connection(
    conn: &Connection,
    binding: &AgentExecutionBinding,
) -> Result<bool> {
    binding.validate().map_err(invalid)?;
    let record:Option<String>=conn.query_row("SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1 AND project_id=?2 AND root_task_id=?3 AND source_task_id=?4 AND call_index=?5",params![binding.requester.cli_attempt_id,binding.project,binding.requester.root_task_id,binding.requester.source_task_id,binding.requester.call_index],|r|r.get(0)).optional().map_err(sql)?;
    let value: serde_json::Value =
        serde_json::from_str(&record.ok_or_else(|| error("agent_execution_binding_missing"))?)
            .map_err(invalid)?;
    let frozen: AgentExecutionBinding =
        serde_json::from_value(value["execution_binding"].clone()).map_err(invalid)?;
    if frozen != *binding {
        return Err(error("agent_execution_binding_mismatch"));
    }
    let Some(plan) = stored_plan(conn, &binding.executor.run)? else {
        return Ok(false);
    };
    let Some(request) = plan.requests.iter().find(|c| c.members.contains(binding)) else {
        return Ok(false);
    };
    let saved:String=conn.query_row("SELECT execution_close_receipts_json FROM wc_agent_runs WHERE run_id=?1 AND execution_close_id=?2",params![plan.close_ref.source.agent_run_id,plan.close_ref.close_id],|r|r.get(0)).map_err(sql)?;
    let receipts: Vec<AgentExecutionCloseReceipt> =
        serde_json::from_str(&saved).map_err(invalid)?;
    let Some(receipt) = receipts.iter().find(|r| r.target == request.target) else {
        return Ok(false);
    };
    receipt
        .validate_for(request)
        .map_err(|_| error("agent_execution_close_receipt_mismatch"))?;
    Ok(true)
}

/// Read the exact previously accepted physical-exit proof, without creating a
/// close plan, accepting a new receipt, or reopening admission. A generic stop
/// bool and NeverCreated cannot authorize reading a stopped worker's output.
pub(crate) fn read_agent_execution_tree_exited_proof_on_connection(
    conn: &Connection,
    binding: &AgentExecutionBinding,
) -> Result<(AgentExecutionClose, AgentExecutionCloseReceipt)> {
    binding.validate().map_err(invalid)?;
    let frozen: Option<String> = conn
        .query_row(
            "SELECT json_extract(record_json,'$.execution_binding') FROM wc_team_cli_attempts
         WHERE attempt_id=?1 AND project_id=?2 AND root_task_id=?3 AND source_task_id=?4
         AND call_index=?5 AND json_extract(record_json,'$.dispatch_state')='terminal'",
            params![
                binding.requester.cli_attempt_id,
                binding.project,
                binding.requester.root_task_id,
                binding.requester.source_task_id,
                binding.requester.call_index
            ],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    let frozen: AgentExecutionBinding = serde_json::from_str(
        &frozen.ok_or_else(|| error("agent_cli_delivery_stop_evidence_unavailable"))?,
    )
    .map_err(invalid)?;
    if frozen != *binding {
        return Err(error("agent_execution_binding_mismatch"));
    }
    let plan = stored_plan(conn, &binding.executor.run)?
        .ok_or_else(|| error("agent_cli_delivery_stop_evidence_unavailable"))?;
    if plan.project != binding.project
        || !plan.blocked_attempt_ids.is_empty()
        || plan.runs.is_empty()
        || plan.runs.len() > MAX_RUNS
        || plan.requests.is_empty()
        || plan.requests.len() > MAX_ATTEMPTS
        || !plan.runs.contains(&binding.executor.run)
        || !plan.runs.contains(&plan.close_ref.source)
    {
        return Err(error("agent_cli_delivery_stop_evidence_unavailable"));
    }
    run_owned(
        conn,
        &plan.owner_user,
        &plan.project,
        &plan.close_ref.source,
    )?;
    run_owned(conn, &plan.owner_user, &plan.project, &binding.executor.run)?;
    let mut run_ids = BTreeSet::new();
    for run in &plan.runs {
        run_owned(conn, &plan.owner_user, &plan.project, run)?;
        // A logical worker may have older, already closed generations in the
        // saved descendant set. Their tombstones must retain the original ID.
        // Only this source and actual members must belong to this close cohort.
        let current_cohort = *run == plan.close_ref.source
            || plan
                .requests
                .iter()
                .flat_map(|request| &request.members)
                .any(|member| member.requester.run == *run || member.executor.run == *run);
        let closed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2
             AND generation=?3 AND execution_close_id IS NOT NULL AND execution_closed_at_unix_ms IS NOT NULL
             AND (?5=0 OR execution_close_id=?4))",
            params![run.agent_run_id,run.agent_id,run.generation,plan.close_ref.close_id,current_cohort],
            |r| r.get(0),
        ).map_err(sql)?;
        if !closed || !run_ids.insert(&run.agent_run_id) {
            return Err(error("agent_cli_delivery_stop_evidence_unavailable"));
        }
    }
    let saved: String = conn.query_row(
        "SELECT execution_close_receipts_json FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2
         AND generation=?3 AND execution_close_id=?4 AND execution_close_plan_json IS NOT NULL
         AND execution_closed_at_unix_ms IS NOT NULL",
        params![plan.close_ref.source.agent_run_id,plan.close_ref.source.agent_id,
            plan.close_ref.source.generation,plan.close_ref.close_id], |r| r.get(0),
    ).map_err(sql)?;
    let receipts: Vec<AgentExecutionCloseReceipt> =
        serde_json::from_str(&saved).map_err(invalid)?;
    if receipts.len() != plan.requests.len() {
        return Err(error("agent_cli_delivery_stop_evidence_unavailable"));
    }
    let mut targets = BTreeSet::new();
    let mut selected = None;
    for request in &plan.requests {
        if request.close_ref != plan.close_ref
            || !targets.insert((
                &request.target.client_id,
                &request.target.runner_instance_id,
            ))
        {
            return Err(error("agent_cli_delivery_stop_evidence_unavailable"));
        }
        let mut matching = receipts.iter().filter(|r| r.target == request.target);
        let receipt = matching
            .next()
            .ok_or_else(|| error("agent_cli_delivery_stop_evidence_unavailable"))?;
        if matching.next().is_some() {
            return Err(error("agent_cli_delivery_stop_evidence_unavailable"));
        }
        // Revalidate every accepted target, not just the requested member. A
        // partially closed parent group never becomes a complete recovery proof.
        receipt
            .validate_for(request)
            .map_err(|_| error("agent_execution_close_receipt_mismatch"))?;
        if let Some(index) = request.members.iter().position(|member| member == binding) {
            if selected.is_some()
                || request.target != binding.target
                || !matches!(
                    &receipt.receipts[index].outcome,
                    webcodex_core::runner_protocol::AgentExecutionStopOutcome::TreeExited { .. }
                )
            {
                return Err(error("agent_cli_delivery_tree_exited_required"));
            }
            selected = Some((request.clone(), receipt.clone()));
        }
    }
    selected.ok_or_else(|| error("agent_cli_delivery_stop_evidence_unavailable"))
}

/// Caller/task and independent executor/task intersections share this one
/// physical-proof gate. Existing non-CLI/delegated-task policy gates remain.
pub(crate) fn require_agent_task_execution_quiescent_on_connection(
    conn: &Connection,
    owner_user: &str,
    project: &str,
    task_id: &str,
) -> Result<()> {
    let principal = crate::agent_isolation::enrollment_principal_for_execution(owner_user);
    let preparations = conn
        .prepare("PRAGMA table_info(wc_team_cli_attempts)")
        .map_err(sql)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql)?
        .iter()
        .any(|c| c == "execution_preparation_json");
    let (projection, extra) = if preparations {
        (
            "execution_preparation_state",
            " OR json_extract(execution_preparation_json,'$.executor.task_id')=?4",
        )
    } else {
        ("NULL", "")
    };
    let query=format!("SELECT record_json,{projection} FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 AND (root_task_id=?4 OR source_task_id=?4 OR json_extract(record_json,'$.execution_binding.executor.task_id')=?4{extra}) LIMIT 4097");
    let rows = conn
        .prepare(&query)
        .map_err(sql)?
        .query_map(
            params![principal.kind, principal.digest, project, task_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .map_err(sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql)?;
    if rows.len() > MAX_ATTEMPTS {
        return Err(error("agent_execution_close_limit"));
    }
    for (record, preparation) in rows {
        let value: serde_json::Value = serde_json::from_str(&record).map_err(invalid)?;
        if value["execution_binding"].is_object() {
            let binding: AgentExecutionBinding =
                serde_json::from_value(value["execution_binding"].clone()).map_err(invalid)?;
            if !agent_execution_binding_stop_proven_on_connection(conn, &binding)? {
                return Err(error("agent_execution_stop_proof_required"));
            }
        } else if !(value["dispatch_state"] == "terminal"
            && value["execution_state"] == "not_started"
            && value["job_id"].is_null()
            && matches!(preparation.as_deref(), None | Some("cancelled")))
        {
            return Err(error("agent_execution_stop_proof_required"));
        }
    }
    Ok(())
}

/// Used inside the final activation/recovery/release SQL transaction. A truly
/// empty closure is recorded durably here; an execution-bearing closure must
/// already have been begun and completed through authenticated observations.
pub(crate) fn require_agent_run_execution_closed_on_connection(
    conn: &Connection,
    owner: &str,
    project: &str,
    run: &AgentRunRef,
) -> Result<()> {
    run_owned(conn, owner, project, run)?;
    let (id,done):(Option<String>,Option<i64>)=conn.query_row("SELECT execution_close_id,execution_closed_at_unix_ms FROM wc_agent_runs WHERE run_id=?1",[&run.agent_run_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sql)?;
    if done.is_some() {
        return Ok(());
    }
    if id.is_some() {
        return Err(error("agent_execution_close_pending"));
    }
    let plan = plan_on_connection(conn, owner, project, run, now().saturating_add(30_000))?;
    if !plan.requires_explicit_close && progress(conn, &plan)?.complete {
        Ok(())
    } else {
        Err(error("agent_execution_close_required"))
    }
}

pub(crate) fn require_agent_assignment_execution_closed_on_connection(
    conn: &Connection,
    owner: &str,
    project: &str,
    agent: &str,
) -> Result<()> {
    let runs=conn.prepare("SELECT agent_id,run_id,generation FROM wc_agent_runs WHERE agent_id=?1 ORDER BY generation LIMIT 1025").map_err(sql)?.query_map([agent],|r|Ok(AgentRunRef{agent_id:r.get(0)?,agent_run_id:r.get(1)?,generation:r.get(2)?})).map_err(sql)?.collect::<std::result::Result<Vec<_>,_>>().map_err(sql)?;
    if runs.len() > MAX_RUNS {
        return Err(error("agent_execution_close_limit"));
    }
    for run in runs {
        require_agent_run_execution_closed_on_connection(conn, owner, project, &run)?;
    }
    Ok(())
}

impl Database {
    pub fn validate_agent_execution_admission(
        &self,
        binding: &AgentExecutionBinding,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sql)?;
        validate_agent_execution_admission_on_connection(&tx, binding)
    }
    pub fn begin_agent_execution_close(
        &self,
        owner_user: &str,
        project: &str,
        source: &AgentRunRef,
        deadline_at_ms: i64,
    ) -> Result<AgentExecutionClosePlan> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let plan = plan_on_connection(&tx, owner_user, project, source, deadline_at_ms)?;
        tx.commit().map_err(sql)?;
        Ok(plan)
    }
    pub fn agent_execution_close_status(
        &self,
        owner_user: &str,
        project: &str,
        source: &AgentRunRef,
    ) -> Result<AgentExecutionCloseProgress> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sql)?;
        run_owned(&tx, owner_user, project, source)?;
        let plan =
            stored_plan(&tx, source)?.ok_or_else(|| error("agent_execution_close_missing"))?;
        progress(&tx, &plan)
    }
    pub fn accept_agent_execution_close_receipt(
        &self,
        trusted: &impl AuthenticatedAgentExecutionCloseReceipt,
    ) -> Result<AgentExecutionCloseProgress> {
        let incoming = trusted.authenticated_receipt();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let plan = stored_plan(&tx, &incoming.close_ref.source)?
            .ok_or_else(|| error("agent_execution_close_missing"))?;
        let request = plan
            .requests
            .iter()
            .find(|c| c.target == incoming.target)
            .ok_or_else(|| error("agent_execution_close_target_mismatch"))?;
        incoming
            .validate_for(request)
            .map_err(|_| error("agent_execution_close_receipt_mismatch"))?;
        let saved:String=tx.query_row("SELECT execution_close_receipts_json FROM wc_agent_runs WHERE run_id=?1 AND execution_close_id=?2",params![plan.close_ref.source.agent_run_id,plan.close_ref.close_id],|r|r.get(0)).map_err(sql)?;
        let mut receipts: Vec<AgentExecutionCloseReceipt> =
            serde_json::from_str(&saved).map_err(invalid)?;
        if let Some(old) = receipts.iter().find(|r| r.target == incoming.target) {
            if old == incoming {
                return progress(&tx, &plan);
            }
            if incoming.observation_revision <= old.observation_revision
                || incoming
                    .receipts
                    .iter()
                    .zip(&old.receipts)
                    .any(|(new, old)| {
                        new.observation_revision < old.observation_revision
                            || (new.observation_revision == old.observation_revision && new != old)
                    })
            {
                return Err(error("agent_execution_close_receipt_stale"));
            }
            // A completed proof is immutable, even if a peer invents a newer revision.
            return Err(error("agent_execution_close_receipt_conflict"));
        }
        // Re-read every original frozen binding before accepting any result.
        for request in &plan.requests {
            for binding in &request.members {
                let json: String = tx
                    .query_row(
                        "SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1",
                        [&binding.requester.cli_attempt_id],
                        |r| r.get(0),
                    )
                    .map_err(sql)?;
                let value: serde_json::Value = serde_json::from_str(&json).map_err(invalid)?;
                let frozen: AgentExecutionBinding =
                    serde_json::from_value(value["execution_binding"].clone()).map_err(invalid)?;
                if frozen != *binding {
                    return Err(error("agent_execution_binding_mismatch"));
                }
                run_owned(&tx, &plan.owner_user, &plan.project, &binding.executor.run)?;
                for run in [&binding.requester.run, &binding.executor.run]
                    .into_iter()
                    .filter(|run| plan.runs.contains(run))
                {
                    let closed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2 AND generation=?3 AND execution_close_id=?4)",params![run.agent_run_id,run.agent_id,run.generation,plan.close_ref.close_id],|r|r.get(0)).map_err(sql)?;
                    if !closed {
                        return Err(error("agent_execution_close_lineage_mismatch"));
                    }
                }
            }
        }
        receipts.push(incoming.clone());
        tx.execute(
            "UPDATE wc_agent_runs SET execution_close_receipts_json=?2 WHERE run_id=?1",
            params![plan.close_ref.source.agent_run_id, encode(&receipts)?],
        )
        .map_err(sql)?;
        if receipts.len() == plan.requests.len() && plan.blocked_attempt_ids.is_empty() {
            tx.execute("UPDATE wc_agent_runs SET execution_closed_at_unix_ms=?2 WHERE execution_close_id=?1 AND execution_closed_at_unix_ms IS NULL",params![plan.close_ref.close_id,now()]).map_err(sql)?;
        }
        let result = progress(&tx, &plan)?;
        tx.commit().map_err(sql)?;
        Ok(result)
    }
}
