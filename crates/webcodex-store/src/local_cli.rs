//! Task-owned receipts for generic local CLI jobs. CLI names and versions do not
//! grant authority; the live assignment and its existing call budget do.
use crate::agent_isolation::{
    authorize_agent_assignment_on_connection, enrollment_projection_on_connection,
};
use crate::{AgentAssignmentOperation, AgentIsolationError, Database, VerifiedAgentRun};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
#[path = "local_cli_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCliCall {
    pub call_id: String,
    pub task_id: String,
    pub project: String,
    pub client_id: String,
    pub root: String,
    pub request_sha256: String,
    pub state: String,
    pub job_id: Option<String>,
}

fn error(code: &'static str) -> AgentIsolationError {
    AgentIsolationError::new(code)
}
fn unavailable(_: impl std::fmt::Display) -> AgentIsolationError {
    error("local_cli_store_unavailable")
}

pub(crate) fn init_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS wc_local_cli_calls (
        call_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, task_id TEXT NOT NULL,
        key_sha256 TEXT NOT NULL, request_sha256 TEXT NOT NULL, record_json TEXT NOT NULL,
        UNIQUE(agent_id,task_id,key_sha256));
        CREATE INDEX IF NOT EXISTS wc_local_cli_task ON wc_local_cli_calls(task_id);",
    )
}

pub(crate) fn call_count(conn: &Connection, task: &str) -> rusqlite::Result<u32> {
    conn.query_row(
        "SELECT COUNT(*) FROM wc_local_cli_calls WHERE task_id=?1",
        [task],
        |r| r.get(0),
    )
}

pub(crate) fn require_quiescent(conn: &Connection, task: &str) -> Result<(), AgentIsolationError> {
    let active: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_local_cli_calls WHERE task_id=?1
        AND json_extract(record_json,'$.state') NOT IN ('completed','failed','stopped'))",
            [task],
            |r| r.get(0),
        )
        .map_err(unavailable)?;
    if active {
        Err(error("local_cli_jobs_active_or_unknown"))
    } else {
        Ok(())
    }
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_local_cli_call(
        &self,
        binding: &VerifiedAgentRun,
        key: &str,
        request_sha256: &str,
        project: &str,
        client_id: &str,
        root: &str,
    ) -> Result<(LocalCliCall, bool), AgentIsolationError> {
        if key.trim().is_empty()
            || key.len() > 128
            || key.chars().any(char::is_control)
            || request_sha256.len() != 64
            || !request_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(error("local_cli_invalid_request"));
        }
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let assignment = enrollment_projection_on_connection(&tx, binding)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            &assignment.task_id,
            AgentAssignmentOperation::UseCli,
        )?;
        let key_sha256 = format!("{:x}", Sha256::digest(key.as_bytes()));
        let prior: Option<String> = tx
            .query_row(
                "SELECT record_json FROM wc_local_cli_calls
            WHERE agent_id=?1 AND task_id=?2 AND key_sha256=?3",
                params![binding.agent_id(), assignment.task_id, key_sha256],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        if let Some(prior) = prior {
            let record: LocalCliCall = serde_json::from_str(&prior).map_err(unavailable)?;
            if record.request_sha256 != request_sha256
                || record.project != project
                || record.client_id != client_id
                || record.root != root
            {
                return Err(error("local_cli_idempotency_conflict"));
            }
            return Ok((record, false));
        }
        let legacy: u32 = tx
            .query_row(
                "SELECT COUNT(*) FROM wc_team_cli_attempts WHERE source_task_id=?1",
                [&assignment.task_id],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
        if legacy.saturating_add(call_count(&tx, &assignment.task_id).map_err(unavailable)?)
            >= authority.policy.max_cli_calls
        {
            return Err(error("local_cli_budget_exhausted"));
        }
        let record = LocalCliCall {
            call_id: format!("wc_local_cli_{}", uuid::Uuid::new_v4().simple()),
            task_id: assignment.task_id,
            project: project.into(),
            client_id: client_id.into(),
            root: root.into(),
            request_sha256: request_sha256.into(),
            state: "reserved".into(),
            job_id: None,
        };
        tx.execute("INSERT INTO wc_local_cli_calls(call_id,agent_id,task_id,key_sha256,request_sha256,record_json)
            VALUES(?1,?2,?3,?4,?5,?6)", params![record.call_id,binding.agent_id(),record.task_id,key_sha256,
            record.request_sha256,serde_json::to_string(&record).map_err(unavailable)?]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok((record, true))
    }

    pub fn read_local_cli_call(
        &self,
        binding: &VerifiedAgentRun,
        id: &str,
    ) -> Result<LocalCliCall, AgentIsolationError> {
        let conn = self.conn.lock().map_err(unavailable)?;
        let assignment = enrollment_projection_on_connection(&conn, binding)?;
        authorize_agent_assignment_on_connection(
            &conn,
            binding,
            &assignment.task_id,
            AgentAssignmentOperation::Read,
        )?;
        let raw: String = conn.query_row("SELECT record_json FROM wc_local_cli_calls WHERE call_id=?1 AND agent_id=?2 AND task_id=?3",
            params![id,binding.agent_id(),assignment.task_id], |r|r.get(0)).optional().map_err(unavailable)?
            .ok_or_else(|| error("local_cli_call_not_found"))?;
        serde_json::from_str(&raw).map_err(unavailable)
    }

    /// Only trusted registry observations may settle a receipt. Model inputs never
    /// supply a status, exit code or replacement Job id to this method.
    pub fn record_local_cli_job(
        &self,
        binding: &VerifiedAgentRun,
        id: &str,
        job_id: &str,
        state: &str,
    ) -> Result<LocalCliCall, AgentIsolationError> {
        if !matches!(
            state,
            "reserved" | "running" | "unknown" | "completed" | "failed" | "stopped"
        ) {
            return Err(error("local_cli_invalid_state"));
        }
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let assignment = enrollment_projection_on_connection(&tx, binding)?;
        authorize_agent_assignment_on_connection(
            &tx,
            binding,
            &assignment.task_id,
            AgentAssignmentOperation::Read,
        )?;
        let raw: String = tx.query_row("SELECT record_json FROM wc_local_cli_calls WHERE call_id=?1 AND agent_id=?2 AND task_id=?3",
            params![id,binding.agent_id(),assignment.task_id], |r|r.get(0)).optional().map_err(unavailable)?
            .ok_or_else(|| error("local_cli_call_not_found"))?;
        let mut record: LocalCliCall = serde_json::from_str(&raw).map_err(unavailable)?;
        if record.job_id.as_deref().is_some_and(|old| old != job_id) {
            return Err(error("local_cli_job_identity_conflict"));
        }
        if !matches!(record.state.as_str(), "completed" | "failed" | "stopped") {
            record.job_id = Some(job_id.into());
            record.state = state.into();
            tx.execute(
                "UPDATE wc_local_cli_calls SET record_json=?2 WHERE call_id=?1",
                params![id, serde_json::to_string(&record).map_err(unavailable)?],
            )
            .map_err(unavailable)?;
        }
        tx.commit().map_err(unavailable)?;
        Ok(record)
    }
}
