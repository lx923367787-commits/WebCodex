//! Durable admission for ordinary project programs; independent of model-call
//! quotas and delegated CLI workers. A retained dispatch cannot be reissued
//! after response loss or Server restart without original Job evidence.
use crate::agent_isolation::{
    authorize_agent_assignment_on_connection, validate_agent_run_on_connection,
};
use crate::{AgentAssignmentOperation, Database, VerifiedAgentRun};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use webcodex_core::agent_program::{AgentProgramBinding, AgentProgramStart};

type Result<T> = std::result::Result<T, String>;
#[path = "agent_browser.rs"]
mod browser;
#[cfg(test)]
#[path = "agent_program_tests.rs"]
mod tests;
pub use browser::{AgentBrowserRecord, AuthenticatedAgentBrowserReceipt};
fn storage(_: impl std::fmt::Display) -> String {
    "agent_program_store_unavailable".into()
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

pub(crate) fn init(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_agent_programs(
        program_id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,run_id TEXT NOT NULL,
        task_id TEXT NOT NULL REFERENCES wc_agent_tasks(task_id),project TEXT NOT NULL,
        key_hash TEXT NOT NULL,intent_hash TEXT NOT NULL,start_json TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('prepared','dispatch_fenced','terminal','cancelled')),
        created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,terminal_json TEXT,
        UNIQUE(agent_id,key_hash));
        CREATE TRIGGER IF NOT EXISTS wc_agent_program_identity_immutable BEFORE UPDATE OF program_id,agent_id,run_id,task_id,project,key_hash,intent_hash,start_json ON wc_agent_programs BEGIN SELECT RAISE(ABORT,'program identity is immutable'); END;
        CREATE TRIGGER IF NOT EXISTS wc_agent_program_history_preserved BEFORE DELETE ON wc_agent_programs BEGIN SELECT RAISE(ABORT,'program history is preserved'); END;")?;
    browser::init(conn)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProgramReceipt {
    pub program_id: String,
    pub job_id: String,
    pub state: String,
    pub replayed: bool,
    pub run_id: String,
    pub generation: i64,
    pub created_at_unix_ms: i64,
    pub terminal: Option<serde_json::Value>,
}
fn record(conn: &Connection, id: &str) -> Result<(AgentProgramStart, AgentProgramReceipt)> {
    let raw:Option<(String,String,i64,Option<String>)>=conn.query_row("SELECT start_json,state,created_at,terminal_json FROM wc_agent_programs WHERE program_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(storage)?;
    let (start, state, created, terminal) = raw.ok_or("agent_program_not_found")?;
    let start: AgentProgramStart = serde_json::from_str(&start).map_err(storage)?;
    let receipt = AgentProgramReceipt {
        program_id: id.into(),
        job_id: start.binding.job_id.clone(),
        state,
        replayed: false,
        run_id: start.binding.run.agent_run_id.clone(),
        generation: start.binding.run.generation,
        created_at_unix_ms: created,
        terminal: terminal
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(storage)?,
    };
    Ok((start, receipt))
}

fn authorize(
    conn: &Connection,
    run: &VerifiedAgentRun,
    b: &AgentProgramBinding,
    write: bool,
) -> Result<()> {
    validate_agent_run_on_connection(conn, run).map_err(|e| e.code().to_string())?;
    let auth = authorize_agent_assignment_on_connection(
        conn,
        run,
        &b.task_id,
        if write {
            AgentAssignmentOperation::ProjectFiles
        } else {
            AgentAssignmentOperation::Read
        },
    )
    .map_err(|e| e.code().to_string())?;
    if auth.agent_id != b.run.agent_id || run.project() != b.project {
        return Err("agent_program_assignment_denied".into());
    }
    if write
        && (run.run_id() != b.run.agent_run_id
            || run.generation() != b.run.generation
            || b.expires_at_unix_ms > run.expires_at_unix_ms()
            || b.expires_at_unix_ms <= now())
    {
        return Err("agent_program_stale_executor".into());
    }
    if write {
        crate::agent_execution_close::ensure_agent_run_execution_open_on_connection(conn, &b.run)
            .map_err(|e| e.code().to_string())?;
    }
    if let Some(folder) = crate::native_workspaces::team_target_on_connection(conn, run)
        .map_err(|e| e.code().to_string())?
    {
        if folder.project != b.workspace_project || folder.path != b.workspace_root {
            return Err("agent_program_workspace_changed".into());
        }
    } else if b.workspace_project != run.project() {
        return Err("agent_program_workspace_denied".into());
    }
    let enrollment:String=conn.query_row("SELECT enrollment_id FROM wc_agent_enrollments WHERE agent_id=?1 AND task_id=?2 AND state='claimed'",params![run.agent_id(),b.task_id],|r|r.get(0)).map_err(storage)?;
    if enrollment != b.enrollment_id {
        return Err("agent_program_assignment_denied".into());
    }
    Ok(())
}

pub trait AuthenticatedAgentProgramObservation {
    fn binding(&self) -> &AgentProgramBinding;
    fn job(&self) -> &webcodex_core::runner_protocol::ShellJobInfo;
}

pub(crate) fn pending_on(conn: &Connection, run: &str) -> Result<usize> {
    conn.query_row("SELECT (SELECT COUNT(*) FROM wc_agent_programs WHERE run_id=?1 AND state NOT IN ('terminal','cancelled')) + (SELECT COUNT(*) FROM wc_agent_browser_actions WHERE run_id=?1 AND state NOT IN ('terminal','cancelled'))",[run],|r|r.get::<_,i64>(0)).map(|n|n as usize).map_err(storage)
}

impl Database {
    pub fn agent_program_by_key(
        &self,
        run: &VerifiedAgentRun,
        key: &str,
    ) -> Result<Option<(AgentProgramStart, AgentProgramReceipt)>> {
        if key.trim().is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err("agent_program_invalid_key".into());
        }
        let conn = self.conn.lock().unwrap();
        validate_agent_run_on_connection(&conn, run).map_err(|e| e.code().to_string())?;
        let id: Option<String> = conn
            .query_row(
                "SELECT program_id FROM wc_agent_programs WHERE agent_id=?1 AND key_hash=?2",
                params![run.agent_id(), hash(key)],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        let Some(id) = id else { return Ok(None) };
        let (start, mut receipt) = record(&conn, &id)?;
        authorize(&conn, run, &start.binding, false)?;
        receipt.replayed = true;
        Ok(Some((start, receipt)))
    }
    pub fn record_agent_program_observation(
        &self,
        run: &VerifiedAgentRun,
        evidence: &dyn AuthenticatedAgentProgramObservation,
    ) -> Result<AgentProgramReceipt> {
        use webcodex_core::runner_protocol::ShellCommandExecutionState;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let (start, receipt) = record(&tx, &evidence.binding().program_id)?;
        authorize(&tx, run, &start.binding, false)?;
        let job = evidence.job();
        if &start.binding != evidence.binding()
            || job.job_id != start.binding.job_id
            || job.client_id != start.binding.target.client_id
        {
            return Err("agent_program_observation_mismatch".into());
        }
        let stopped = matches!(
            job.status.as_str(),
            "completed" | "failed" | "stopped" | "timeout"
        ) && match job.command_execution_state {
            Some(ShellCommandExecutionState::NotStarted) => true,
            Some(ShellCommandExecutionState::Completed | ShellCommandExecutionState::TimedOut) => {
                job.exit_code.is_some()
            }
            _ => false,
        };
        if stopped {
            let terminal = serde_json::json!({"job_id":job.job_id,"status":job.status,"execution_state":job.command_execution_state,"exit_code":job.exit_code});
            if receipt
                .terminal
                .as_ref()
                .is_some_and(|old| old != &terminal)
            {
                return Err("agent_program_terminal_conflict".into());
            }
            tx.execute("UPDATE wc_agent_programs SET state='terminal',terminal_json=?2,updated_at=?3 WHERE program_id=?1 AND state IN ('dispatch_fenced','terminal')",params![start.binding.program_id,terminal.to_string(),now()]).map_err(storage)?;
        }
        let (_, receipt) = record(&tx, &start.binding.program_id)?;
        tx.commit().map_err(storage)?;
        Ok(receipt)
    }
    pub fn prepare_agent_program(
        &self,
        run: &VerifiedAgentRun,
        start: AgentProgramStart,
        key: &str,
    ) -> Result<AgentProgramReceipt> {
        start.validate()?;
        if key.trim().is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err("agent_program_invalid_key".into());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        authorize(&tx, run, &start.binding, true)?;
        let key = hash(key);
        // Generated Job/program ids are not part of the caller's semantic
        // intent. Exact replay returns the original identities, never the new candidates.
        let fingerprint = hash(
            &serde_json::to_string(&(
                &start.binding.task_id,
                &start.binding.project,
                &start.binding.workspace_project,
                &start.binding.workspace_root,
                &start.binding.relative_cwd,
                &start.binding.resource_set_id,
                &start.binding.resource_manifest_hash,
                &start.args,
                &start.stdin,
                start.timeout_secs,
                start.binding.local_service_port,
                &start.binding.resume_from_program,
            ))
            .map_err(storage)?,
        );
        let prior:Option<(String,String)>=tx.query_row("SELECT program_id,intent_hash FROM wc_agent_programs WHERE agent_id=?1 AND key_hash=?2",params![run.agent_id(),key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage)?;
        if let Some((id, previous)) = prior {
            if previous != fingerprint {
                return Err("agent_program_idempotency_conflict".into());
            }
            let (_, mut receipt) = record(&tx, &id)?;
            receipt.replayed = true;
            return Ok(receipt);
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_programs WHERE task_id=?1",
                [&start.binding.task_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if count >= 4096 {
            return Err("agent_program_capacity".into());
        }
        if let Some(previous) = &start.binding.resume_from_program {
            let (old, receipt) = record(&tx, previous)?;
            authorize(&tx, run, &old.binding, false)?;
            if receipt.state != "terminal"
                || receipt.terminal.as_ref().is_none_or(|v| {
                    !matches!(
                        v["execution_state"].as_str(),
                        Some("completed" | "timed_out")
                    )
                })
                || old.binding.task_id != start.binding.task_id
                || old.binding.run.agent_id != start.binding.run.agent_id
                || old.binding.workspace_project != start.binding.workspace_project
                || old.binding.workspace_root != start.binding.workspace_root
                || old.binding.relative_cwd != start.binding.relative_cwd
                || old.binding.target.client_id != start.binding.target.client_id
            {
                return Err("agent_program_resume_source_not_stopped_or_owned".into());
            }
        }
        tx.execute("INSERT INTO wc_agent_programs(program_id,agent_id,run_id,task_id,project,key_hash,intent_hash,start_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'prepared',?9,?9)",params![start.binding.program_id,run.agent_id(),run.run_id(),start.binding.task_id,run.project(),key,fingerprint,serde_json::to_string(&start).map_err(storage)?,now()]).map_err(storage)?;
        let (_, receipt) = record(&tx, &start.binding.program_id)?;
        tx.commit().map_err(storage)?;
        Ok(receipt)
    }

    /// Framework-only read of the retained command for an exact authenticated
    /// owner. Tool responses expose the safe receipt, not this command bundle.
    pub fn agent_program_start(
        &self,
        run: &VerifiedAgentRun,
        id: &str,
    ) -> Result<(AgentProgramStart, AgentProgramReceipt)> {
        let conn = self.conn.lock().unwrap();
        let (start, receipt) = record(&conn, id)?;
        authorize(&conn, run, &start.binding, false)?;
        Ok((start, receipt))
    }

    /// Called under Registry admission immediately before enqueue. Side effects
    /// remain external: false means already fenced, not that a process finished.
    pub fn claim_agent_program_dispatch(
        &self,
        run: &VerifiedAgentRun,
        expected: &AgentProgramStart,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let (saved, receipt) = record(&tx, &expected.binding.program_id)?;
        authorize(&tx, run, &saved.binding, true)?;
        if saved != *expected {
            return Err("agent_program_intent_changed".into());
        }
        if receipt.state != "prepared" {
            return Ok(false);
        }
        tx.execute("UPDATE wc_agent_programs SET state='dispatch_fenced',updated_at=?2 WHERE program_id=?1 AND state='prepared'",params![expected.binding.program_id,now()]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(true)
    }
}
