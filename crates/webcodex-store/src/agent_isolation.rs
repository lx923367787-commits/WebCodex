//! Credential-bound Agent identity. Account ownership is management authority;
//! parent links are metadata and never participate in authorization.
use crate::Database;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;

#[path = "agent_enrollment.rs"]
mod enrollment;
pub(crate) use crate::agent_execution_close::{
    ensure_agent_run_execution_open_on_connection, validate_agent_execution_admission_on_connection,
};
pub(crate) use enrollment::managed_user_principal as enrollment_principal_for_execution;
#[cfg(test)]
pub(crate) use enrollment::managed_user_principal;
pub(crate) use enrollment::{
    activate_cli_worker_on_connection, agent_enrollment_source_principal_on_connection,
    authorize_agent_assignment_on_connection, authorize_agent_delegated_task_on_connection,
    authorize_agent_goal_creation_on_connection, bind_agent_enrollment_goal_on_connection,
    claim_agent_enrollment_on_connection, delegated_cli_budget_on_connection,
    enrollment_projection_on_connection, issue_delivery_grant_on_connection,
    issue_agent_enrollment_on_connection,
    recover_cancelled_cli_worker_on_connection, require_enrollment_public_task_on_connection,
    reserve_agent_enrollment_on_connection,
    reset_unstarted_agent_enrollment_delivery_on_connection,
    revoke_agent_enrollments_for_principal_on_connection,
    validate_agent_enrollment_dispatch_on_connection, AgentEnrollmentDispatchProjection,
};
pub use enrollment::{
    AgentAssignmentOperation, AgentEnrollmentAuthority, AgentEnrollmentClaim, AgentEnrollmentGrant,
    AgentEnrollmentInput, AgentEnrollmentIssued, AgentEnrollmentPolicy, AgentEnrollmentProjection,
    AgentEnrollmentPurpose, AgentEnrollmentSnapshot, AgentEnrollmentStatus,
    AgentHostManagerBinding, NewAgentEnrollment,
};

#[path = "agent_task_scope.rs"]
mod task_scope;
pub(crate) use task_scope::{
    agent_run_goal_scope_on_connection, agent_run_task_goal_on_connection,
    bind_pending_task_goal_on_connection,
};

pub const AGENT_RUN_TOKEN_PREFIX: &str = "wc_ar_";
pub const AGENT_RUN_LIFETIME_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentIsolationError(&'static str);

impl AgentIsolationError {
    /// Callers map implementation failures to fixed, non-sensitive codes.
    pub const fn new(code: &'static str) -> Self {
        Self(code)
    }

    pub fn code(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for AgentIsolationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for AgentIsolationError {}

fn unavailable(_: rusqlite::Error) -> AgentIsolationError {
    AgentIsolationError("agent_isolation_unavailable")
}

fn invalid_run() -> AgentIsolationError {
    AgentIsolationError("agent_run_invalid")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IsolatedAgentIdentity {
    pub agent_id: String,
    pub owner_user: String,
    pub project: String,
    pub parent_agent_id: Option<String>,
    pub created_at_unix_ms: i64,
}

/// Constructed only after authentication or trusted management activation.
/// It is not deserializable and carries no reusable credential. Every operation
/// must revalidate it against the authoritative store in its own transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAgentRun {
    agent_id: String,
    owner_user: String,
    project: String,
    run_id: String,
    generation: i64,
    expires_at_unix_ms: i64,
}

impl VerifiedAgentRun {
    /// Stable Goal owner for this authenticated logical Agent across runs.
    pub fn host_goal_owner_key(&self) -> String {
        host_goal_owner_key(&self.project, &self.agent_id)
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
    pub fn owner_user(&self) -> &str {
        &self.owner_user
    }
    pub fn project(&self) -> &str {
        &self.project
    }
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn expires_at_unix_ms(&self) -> i64 {
        self.expires_at_unix_ms
    }
}

pub(crate) fn host_goal_owner_key(project: &str, agent_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"webcodex-coding-agent-authority-v1\0");
    digest.update(format!("isolated-agent:{project}:{agent_id}").as_bytes());
    format!("auth_{:x}", digest.finalize())
}

/// A one-time credential issuance result. Deliberately not serializable or
/// cloneable; adapters must explicitly choose whether to deliver its token.
pub struct AgentRunGrant {
    pub agent: IsolatedAgentIdentity,
    pub run_id: String,
    pub generation: i64,
    pub expires_at_unix_ms: i64,
    pub token: String,
}

impl fmt::Debug for AgentRunGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentRunGrant")
            .field("agent_id", &self.agent.agent_id)
            .field("run_id", &self.run_id)
            .field("generation", &self.generation)
            .field("expires_at_unix_ms", &self.expires_at_unix_ms)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

fn bounded_identity(value: &str) -> Result<(), AgentIsolationError> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(AgentIsolationError("agent_isolation_invalid_input"));
    }
    Ok(())
}

fn token_hash(token: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"webcodex.isolated-agent-run-token.v1\0");
    hash.update(token.as_bytes());
    format!("{:x}", hash.finalize())
}

fn new_token() -> String {
    random_secret(
        AGENT_RUN_TOKEN_PREFIX,
        b"webcodex.isolated-agent-run-secret.v1\0",
    )
}

pub(crate) fn new_handoff_claim_token() -> String {
    random_secret("wc_hc_", b"webcodex.host-handoff-claim-secret.v1\0")
}

fn random_secret(prefix: &str, domain: &[u8]) -> String {
    // UUID v4 uses the OS random source but fixes six bits. Three independent
    // UUIDs supply 366 random bits, hashed to a 256-bit secret (two supply 244).
    let mut random = Sha256::new();
    random.update(domain);
    for _ in 0..3 {
        random.update(uuid::Uuid::new_v4().as_bytes());
    }
    format!("{prefix}{:x}", random.finalize())
}

fn valid_token_shape(token: &str) -> bool {
    token
        .strip_prefix(AGENT_RUN_TOKEN_PREFIX)
        .is_some_and(|secret| secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn owned_agent(
    conn: &Connection,
    owner: &str,
    project: &str,
    agent_id: &str,
) -> Result<IsolatedAgentIdentity, AgentIsolationError> {
    conn.query_row(
        "SELECT agent_id, owner_user, project, parent_agent_id, created_at_unix_ms
         FROM wc_isolated_agents WHERE agent_id=?1 AND owner_user=?2 AND project=?3",
        params![agent_id, owner, project],
        |row| {
            Ok(IsolatedAgentIdentity {
                agent_id: row.get(0)?,
                owner_user: row.get(1)?,
                project: row.get(2)?,
                parent_agent_id: row.get(3)?,
                created_at_unix_ms: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(unavailable)?
    .ok_or(AgentIsolationError("isolated_agent_not_found"))
}

fn verified_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VerifiedAgentRun> {
    Ok(VerifiedAgentRun {
        agent_id: row.get(0)?,
        owner_user: row.get(1)?,
        project: row.get(2)?,
        run_id: row.get(3)?,
        generation: row.get(4)?,
        expires_at_unix_ms: row.get(5)?,
    })
}

/// Use within the same transaction as a protected read or mutation. Validating
/// in a separate lock scope leaves a run-replacement race at the effect boundary.
pub(crate) fn validate_agent_run_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
) -> Result<(), AgentIsolationError> {
    let current: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_runs r
         JOIN wc_isolated_agents a ON a.agent_id=r.agent_id
         WHERE r.run_id=?1 AND r.agent_id=?2 AND r.generation=?3
           AND a.owner_user=?4 AND a.project=?5 AND r.expires_at_unix_ms=?6
           AND r.state='active' AND r.expires_at_unix_ms>?7)",
            params![
                binding.run_id,
                binding.agent_id,
                binding.generation,
                binding.owner_user,
                binding.project,
                binding.expires_at_unix_ms,
                now_ms()
            ],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if current {
        enrollment::enrollment_run_gate(conn, binding)
    } else {
        Err(invalid_run())
    }
}

/// Host Goal context epochs are server-selected run IDs, never credentials.
/// Resolve their current binding inside the Goal transaction and require the
/// exact stable Agent owner before any Goal state or replay can be returned.
pub(crate) fn verify_agent_run_context_on_connection(
    conn: &Connection,
    owner: &str,
    run_id: &str,
) -> Result<VerifiedAgentRun, AgentIsolationError> {
    let binding = conn
        .query_row(
            "SELECT a.agent_id,a.owner_user,a.project,r.run_id,r.generation,r.expires_at_unix_ms
         FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id
         WHERE r.run_id=?1 AND r.state='active' AND r.expires_at_unix_ms>?2",
            params![run_id, now_ms()],
            verified_row,
        )
        .optional()
        .map_err(unavailable)?
        .ok_or_else(invalid_run)?;
    if binding.host_goal_owner_key() != owner {
        return Err(invalid_run());
    }
    enrollment::enrollment_run_gate(conn, &binding)?;
    Ok(binding)
}

/// An authenticated Agent with no Goal context may do ordinary private work.
/// Once any context bookkeeping exists, its lifecycle must gate business
/// effects in this same transaction; orphaned state must not bypass the meter.
pub(crate) fn agent_run_business_gate_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
) -> Result<(), AgentIsolationError> {
    validate_agent_run_on_connection(conn, binding)?;
    let owner = binding.host_goal_owner_key();
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2 UNION ALL SELECT 1 FROM wc_host_context_states WHERE owner_key=?1 AND context_epoch=?2)",
        params![owner,binding.run_id()],|row|row.get(0)).map_err(unavailable)?;
    if exists {
        let context = webcodex_core::host_goal::HostGoalInvocationContext {
            context_epoch: binding.run_id().into(),
            identity_source: "agent_run".into(),
        };
        super::host_goal_context::context_gate(conn, &owner, Some(&context), false)
            .map_err(|error| AgentIsolationError::new(error.code()))?;
    }
    Ok(())
}

fn audit_code(
    conn: &Connection,
    action: &str,
    agent_id: &str,
    run_id: &str,
    code: &str,
) -> Result<(), AgentIsolationError> {
    conn.execute(
        "INSERT INTO wc_agent_isolation_audit(event_id,agent_id,run_id,action,code,created_at_unix_ms)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![format!("wc_aia_{}",uuid::Uuid::new_v4().simple()),agent_id,run_id,action,code,now_ms()],
    ).map_err(unavailable)?;
    Ok(())
}

fn audit(
    conn: &Connection,
    action: &str,
    agent_id: &str,
    run_id: &str,
) -> Result<(), AgentIsolationError> {
    audit_code(conn, action, agent_id, run_id, "ok")
}

/// Composes activation with another store transition in one caller-owned
/// immediate transaction. Does not acquire a lock or commit that transaction.
pub(crate) fn pending_agent_run_on_connection(
    conn: &Connection,
    current: &VerifiedAgentRun,
    pending_run_id: &str,
) -> Result<VerifiedAgentRun, AgentIsolationError> {
    validate_agent_run_on_connection(conn, current)?;
    conn.query_row(
        "SELECT a.agent_id,a.owner_user,a.project,r.run_id,r.generation,r.expires_at_unix_ms
         FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id
         WHERE r.run_id=?1 AND r.agent_id=?2 AND a.owner_user=?3 AND a.project=?4
           AND r.state='pending' AND r.expires_at_unix_ms>?5
           AND r.previous_run_id=?6 AND r.generation>?7",
        params![
            pending_run_id,
            current.agent_id(),
            current.owner_user(),
            current.project(),
            now_ms(),
            current.run_id(),
            current.generation()
        ],
        verified_row,
    )
    .optional()
    .map_err(unavailable)?
    .ok_or_else(invalid_run)
}

/// Caller-owned transaction; successful return is not a commit.
pub(crate) fn activate_agent_run_on_connection(
    conn: &Connection,
    owner_user: &str,
    project: &str,
    agent_id: &str,
    pending_run_id: &str,
) -> Result<VerifiedAgentRun, AgentIsolationError> {
    owned_agent(conn, owner_user, project, agent_id)?;
    let current = conn
        .query_row(
            "SELECT a.agent_id,a.owner_user,a.project,r.run_id,r.generation,r.expires_at_unix_ms
         FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id
         WHERE r.agent_id=?1 AND r.state='active' AND a.owner_user=?2 AND a.project=?3",
            params![agent_id, owner_user, project],
            verified_row,
        )
        .optional()
        .map_err(unavailable)?
        .ok_or_else(invalid_run)?;
    let pending = pending_agent_run_on_connection(conn, &current, pending_run_id)?;
    let current_scope: Option<String> = conn
        .query_row(
            "SELECT task_goal_id FROM wc_agent_runs WHERE run_id=?1",
            [current.run_id()],
            |r| r.get(0),
        )
        .map_err(unavailable)?;
    let pending_scope: Option<String> = conn
        .query_row(
            "SELECT task_goal_id FROM wc_agent_runs WHERE run_id=?1",
            [pending_run_id],
            |r| r.get(0),
        )
        .map_err(unavailable)?;
    if current_scope.is_some() && current_scope != pending_scope {
        return Err(AgentIsolationError("agent_task_scope_denied"));
    }
    crate::agent_execution_close::require_agent_run_execution_closed_on_connection(
        conn,
        owner_user,
        project,
        &webcodex_core::runner_protocol::AgentRunRef {
            agent_id: current.agent_id().into(),
            agent_run_id: current.run_id().into(),
            generation: current.generation(),
        },
    )?;
    crate::agent_execution_close::ensure_agent_run_execution_open_on_connection(
        conn,
        &webcodex_core::runner_protocol::AgentRunRef {
            agent_id: pending.agent_id().into(),
            agent_run_id: pending.run_id().into(),
            generation: pending.generation(),
        },
    )?;
    conn.execute(
        "UPDATE wc_agent_runs SET state='revoked' WHERE agent_id=?1 AND state='active'",
        params![agent_id],
    )
    .map_err(unavailable)?;
    conn.execute("UPDATE wc_agent_runs SET state='active' WHERE run_id=?1 AND agent_id=?2 AND state='pending'",
        params![pending_run_id,agent_id]).map_err(unavailable)?;
    audit(conn, "activate_run", agent_id, pending_run_id)?;
    Ok(pending)
}

fn issue_run(
    conn: &Connection,
    agent: IsolatedAgentIdentity,
    generation: i64,
    state: &str,
    previous_run_id: Option<&str>,
) -> Result<AgentRunGrant, AgentIsolationError> {
    let now = now_ms();
    let grant = AgentRunGrant {
        agent,
        run_id: format!("wc_agent_run_{}", uuid::Uuid::new_v4().simple()),
        generation,
        expires_at_unix_ms: now + AGENT_RUN_LIFETIME_MS,
        token: new_token(),
    };
    conn.execute(
        "INSERT INTO wc_agent_runs(run_id,agent_id,generation,token_hash,expires_at_unix_ms,state,created_at_unix_ms,previous_run_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![grant.run_id,grant.agent.agent_id,grant.generation,token_hash(&grant.token),
            grant.expires_at_unix_ms,state,now,previous_run_id],
    ).map_err(unavailable)?;
    Ok(grant)
}

pub(crate) fn begin_agent_run_handoff_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
) -> Result<AgentRunGrant, AgentIsolationError> {
    validate_agent_run_on_connection(conn, binding)?;
    let agent = owned_agent(
        conn,
        binding.owner_user(),
        binding.project(),
        binding.agent_id(),
    )?;
    let maximum: i64 = conn
        .query_row(
            "SELECT MAX(generation) FROM wc_agent_runs WHERE agent_id=?1",
            [binding.agent_id()],
            |r| r.get(0),
        )
        .map_err(unavailable)?;
    let generation = maximum
        .checked_add(1)
        .ok_or(AgentIsolationError("agent_run_generation_exhausted"))?;
    conn.execute(
        "UPDATE wc_agent_runs SET state='revoked' WHERE agent_id=?1 AND state='pending'",
        [binding.agent_id()],
    )
    .map_err(unavailable)?;
    let grant = issue_run(conn, agent, generation, "pending", Some(binding.run_id()))?;
    audit(conn, "begin_handoff", binding.agent_id(), &grant.run_id)?;
    Ok(grant)
}

/// Generic management run rotation must not split an existing Goal transfer.
/// Native control owns its pending runs and the composed ownership transition.
fn generic_run_handoff_gate(
    conn: &Connection,
    project: &str,
    agent_id: &str,
) -> Result<(), AgentIsolationError> {
    let managed:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_handoffs WHERE owner_key=?1 AND json_extract(snapshot_json,'$.agent_id')=?2 AND json_extract(snapshot_json,'$.project')=?3 AND json_extract(snapshot_json,'$.source_agent_run_id') IS NOT NULL AND json_extract(snapshot_json,'$.status') IN ('waiting','awaiting_ready','prepared','failed'))",params![host_goal_owner_key(project,agent_id),agent_id,project],|r|r.get(0)).map_err(unavailable)?;
    if managed {
        return Err(AgentIsolationError("agent_handoff_control_required"));
    }
    Ok(())
}

impl Database {
    pub fn agent_host_goal_owner_key(
        &self,
        owner_user: &str,
        project: &str,
        agent_id: &str,
    ) -> Result<String, AgentIsolationError> {
        let conn = self.conn.lock().unwrap();
        owned_agent(&conn, owner_user, project, agent_id)?;
        Ok(host_goal_owner_key(project, agent_id))
    }

    pub fn list_agent_handoffs(
        &self,
        owner_user: &str,
        project: &str,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::HostGoalHandoff>, AgentIsolationError> {
        if !(1..=100).contains(&limit) {
            return Err(AgentIsolationError("agent_isolation_invalid_input"));
        }
        let conn = self.conn.lock().unwrap();
        if let Some(agent) = agent_id {
            owned_agent(&conn, owner_user, project, agent)?;
        }
        let mut statement=conn.prepare("SELECT h.owner_key,h.snapshot_json,a.agent_id FROM wc_host_goal_handoffs h JOIN wc_isolated_agents a ON a.agent_id=json_extract(h.snapshot_json,'$.agent_id') WHERE a.owner_user=?1 AND a.project=?2 AND (?3 IS NULL OR a.agent_id=?3) ORDER BY json_extract(h.snapshot_json,'$.created_at') DESC,h.goal_id LIMIT ?4").map_err(unavailable)?;
        let rows = statement
            .query_map(params![owner_user, project, agent_id, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        rows.into_iter()
            .map(|(owner, value, agent)| {
                if owner != host_goal_owner_key(project, &agent) {
                    return Err(AgentIsolationError("agent_isolation_unavailable"));
                }
                serde_json::from_str(&value)
                    .map_err(|_| AgentIsolationError("agent_isolation_unavailable"))
            })
            .collect()
    }

    /// Trusted runtime callers supply fixed codes only; no request arguments,
    /// attempted paths, credentials, or their hashes enter this audit.
    pub fn record_agent_access_denied(
        &self,
        binding: Option<&VerifiedAgentRun>,
        code: &'static str,
    ) -> Result<(), AgentIsolationError> {
        let conn = self.conn.lock().unwrap();
        audit_code(
            &conn,
            "access_denied",
            binding.map_or("", VerifiedAgentRun::agent_id),
            binding.map_or("", VerifiedAgentRun::run_id),
            code,
        )
    }

    /// Internal broker storage, never a model-facing workspace path.
    pub fn agent_private_root(&self) -> std::path::PathBuf {
        self.state_path
            .parent()
            .expect("database state path has a parent")
            .join("agent-runtime")
    }

    /// Keep an immediate transaction across the synchronous filesystem broker
    /// operation. Other connections cannot revoke or replace the run between
    /// admission and the effect. The closure MUST NOT call Database methods.
    /// Filesystem effects cannot roll back with SQLite; failed audit/commit after
    /// invocation reports an unknown outcome and must never be blindly retried.
    pub fn with_agent_run<T>(
        &self,
        binding: &VerifiedAgentRun,
        action: &str,
        operation: impl FnOnce() -> Result<T, AgentIsolationError>,
    ) -> Result<T, AgentIsolationError> {
        self.with_agent_run_target(binding, action, None, operation)
    }

    fn with_agent_run_target<T>(
        &self,
        binding: &VerifiedAgentRun,
        action: &str,
        target: Option<task_scope::Target<'_>>,
        operation: impl FnOnce() -> Result<T, AgentIsolationError>,
    ) -> Result<T, AgentIsolationError> {
        if action.is_empty()
            || action.len() > 64
            || !action.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        {
            return Err(AgentIsolationError("agent_isolation_invalid_action"));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        enrollment::authorize_enrollment_target_on_connection(
            &tx,
            binding,
            action,
            target.as_ref(),
        )?;
        task_scope::authorize_target(&tx, binding, action, target)?;
        if matches!(action, "private_write" | "goal_plan_publish") {
            agent_run_business_gate_on_connection(&tx, binding)?;
        } else {
            validate_agent_run_on_connection(&tx, binding)?;
        }
        audit_code(
            &tx,
            action,
            binding.agent_id(),
            binding.run_id(),
            "admitted",
        )?;
        let result = operation();
        let code = result
            .as_ref()
            .err()
            .map_or("ok", AgentIsolationError::code);
        audit_code(&tx, action, binding.agent_id(), binding.run_id(), code)
            .map_err(|_| AgentIsolationError("agent_private_operation_outcome_unknown"))?;
        tx.commit()
            .map_err(|_| AgentIsolationError("agent_private_operation_outcome_unknown"))?;
        result
    }

    pub fn init_agent_isolation_tables(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wc_isolated_agents(
                agent_id TEXT PRIMARY KEY, owner_user TEXT NOT NULL, project TEXT NOT NULL,
                parent_agent_id TEXT REFERENCES wc_isolated_agents(agent_id), created_at_unix_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS wc_isolated_agents_owner_project ON wc_isolated_agents(owner_user,project);
             CREATE TABLE IF NOT EXISTS wc_agent_runs(
                run_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL REFERENCES wc_isolated_agents(agent_id),
                generation INTEGER NOT NULL CHECK(generation>0), token_hash TEXT NOT NULL UNIQUE,
                expires_at_unix_ms INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN ('active','pending','revoked')),
                created_at_unix_ms INTEGER NOT NULL, previous_run_id TEXT REFERENCES wc_agent_runs(run_id),
                UNIQUE(agent_id,generation)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS wc_agent_runs_active ON wc_agent_runs(agent_id) WHERE state='active';
             CREATE UNIQUE INDEX IF NOT EXISTS wc_agent_runs_pending ON wc_agent_runs(agent_id) WHERE state='pending';
             CREATE TABLE IF NOT EXISTS wc_agent_isolation_audit(
                event_id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,run_id TEXT NOT NULL,
                action TEXT NOT NULL,code TEXT NOT NULL,created_at_unix_ms INTEGER NOT NULL
             );"
        )?;
        let has_task_goal = conn
            .prepare("PRAGMA table_info(wc_agent_runs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "task_goal_id");
        if !has_task_goal {
            conn.execute("ALTER TABLE wc_agent_runs ADD COLUMN task_goal_id TEXT", [])?;
        }
        let columns = conn
            .prepare("PRAGMA table_info(wc_agent_runs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        for (name, statement) in [
            (
                "handoff_claim_hash",
                "ALTER TABLE wc_agent_runs ADD COLUMN handoff_claim_hash TEXT",
            ),
            (
                "handoff_claim_principal_key",
                "ALTER TABLE wc_agent_runs ADD COLUMN handoff_claim_principal_key TEXT",
            ),
        ] {
            if !columns.iter().any(|column| column == name) {
                conn.execute(statement, [])?;
            }
        }
        conn.execute_batch("CREATE TRIGGER IF NOT EXISTS wc_agent_runs_task_goal_immutable BEFORE UPDATE OF task_goal_id ON wc_agent_runs WHEN OLD.task_goal_id IS NOT NULL AND NEW.task_goal_id IS NOT OLD.task_goal_id BEGIN SELECT RAISE(ABORT,'agent task scope is immutable'); END;")?;
        enrollment::ensure_enrollment_schema(&conn)?;
        crate::agent_execution_close::ensure_schema(&conn)?;
        Ok(())
    }

    /// The caller must have already established trusted management authority for
    /// this exact owner and project. Agent credentials must not reach this API.
    pub fn create_isolated_agent(
        &self,
        owner_user: &str,
        project: &str,
        parent_agent_id: Option<&str>,
    ) -> Result<AgentRunGrant, AgentIsolationError> {
        bounded_identity(owner_user)?;
        bounded_identity(project)?;
        if let Some(parent) = parent_agent_id {
            bounded_identity(parent)?;
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(parent) = parent_agent_id {
            owned_agent(&tx, owner_user, project, parent)?;
        }
        let agent = IsolatedAgentIdentity {
            agent_id: format!("wc_iagent_{}", uuid::Uuid::new_v4().simple()),
            owner_user: owner_user.into(),
            project: project.into(),
            parent_agent_id: parent_agent_id.map(str::to_owned),
            created_at_unix_ms: now_ms(),
        };
        tx.execute(
            "INSERT INTO wc_isolated_agents(agent_id,owner_user,project,parent_agent_id,created_at_unix_ms)
             VALUES(?1,?2,?3,?4,?5)",
            params![agent.agent_id,agent.owner_user,agent.project,agent.parent_agent_id,agent.created_at_unix_ms],
        ).map_err(unavailable)?;
        let grant = issue_run(&tx, agent, 1, "active", None)?;
        audit(&tx, "create_agent", &grant.agent.agent_id, &grant.run_id)?;
        tx.commit().map_err(unavailable)?;
        Ok(grant)
    }

    pub fn authenticate_agent_run(
        &self,
        token: &str,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        if !valid_token_shape(token) {
            let _ = self.record_agent_access_denied(None, "authentication_denied");
            return Err(invalid_run());
        }
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT a.agent_id,a.owner_user,a.project,r.run_id,r.generation,r.expires_at_unix_ms
             FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id
             WHERE r.token_hash=?1 AND r.state='active' AND r.expires_at_unix_ms>?2",
            params![token_hash(token), now_ms()],
            verified_row,
        )
        .optional()
        .map_err(unavailable)?;
        match result {
            Some(binding) => {
                enrollment::enrollment_run_gate(&conn, &binding)?;
                Ok(binding)
            }
            None => {
                let _ = audit_code(&conn, "access_denied", "", "", "authentication_denied");
                Err(invalid_run())
            }
        }
    }

    pub fn validate_agent_run(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<(), AgentIsolationError> {
        let conn = self.conn.lock().unwrap();
        validate_agent_run_on_connection(&conn, binding)
    }

    /// Request a replacement credential; this does not transfer execution
    /// authority. Repeating replaces only an earlier unactivated pending grant.
    pub fn begin_agent_run_handoff(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<AgentRunGrant, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        validate_agent_run_on_connection(&tx, binding)?;
        generic_run_handoff_gate(&tx, binding.project(), binding.agent_id())?;
        let grant = begin_agent_run_handoff_on_connection(&tx, binding)?;
        tx.commit().map_err(unavailable)?;
        Ok(grant)
    }

    /// Trusted management confirmation only. Knowing a pending ID, owning its
    /// parent, or holding either run token does not authorize this transition.
    pub fn activate_agent_run(
        &self,
        owner_user: &str,
        project: &str,
        agent_id: &str,
        pending_run_id: &str,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        owned_agent(&tx, owner_user, project, agent_id)?;
        generic_run_handoff_gate(&tx, project, agent_id)?;
        let task_scoped:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs WHERE agent_id=?1 AND state IN ('active','pending') AND task_goal_id IS NOT NULL)",[agent_id],|r|r.get(0)).map_err(unavailable)?;
        if task_scoped {
            return Err(AgentIsolationError("agent_task_scope_denied"));
        }
        let pending =
            activate_agent_run_on_connection(&tx, owner_user, project, agent_id, pending_run_id)?;
        tx.commit().map_err(unavailable)?;
        Ok(pending)
    }

    /// Trusted management observation needed to prepare an exact recovery CAS;
    /// no Goal/Task, private content, token hash, or run secret is returned.
    pub fn isolated_agent_generation(
        &self,
        owner_user: &str,
        project: &str,
        agent_id: &str,
    ) -> Result<i64, AgentIsolationError> {
        let conn = self.conn.lock().unwrap();
        owned_agent(&conn, owner_user, project, agent_id)?;
        conn.query_row(
            "SELECT MAX(generation) FROM wc_agent_runs WHERE agent_id=?1",
            params![agent_id],
            |row| row.get(0),
        )
        .map_err(unavailable)
    }

    /// Explicit trusted management recovery of one known logical Agent. This
    /// never selects or reassigns a Goal/Task and never restores an old token.
    /// Immediate serialization also fences an in-progress broker operation.
    /// Any future execution adapter must fence/terminate its old execution
    /// carrier before exposing recovery; today model execution is not admitted.
    pub fn recover_isolated_agent(
        &self,
        owner_user: &str,
        project: &str,
        agent_id: &str,
        expected_generation: i64,
    ) -> Result<AgentRunGrant, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let agent = owned_agent(&tx, owner_user, project, agent_id)?;
        generic_run_handoff_gate(&tx, project, agent_id)?;
        let maximum: i64 = tx
            .query_row(
                "SELECT MAX(generation) FROM wc_agent_runs WHERE agent_id=?1",
                params![agent_id],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if maximum != expected_generation {
            return Err(AgentIsolationError("agent_run_generation_changed"));
        }
        // Recovery changes the run credential, not the task authorization. A
        // newer unscoped pending grant must never erase an older confined run.
        let task_scopes = tx.prepare("SELECT DISTINCT task_goal_id FROM wc_agent_runs WHERE agent_id=?1 AND task_goal_id IS NOT NULL LIMIT 2").map_err(unavailable)?.query_map([agent_id], |r|r.get::<_,String>(0)).map_err(unavailable)?.collect::<Result<Vec<_>,_>>().map_err(unavailable)?;
        if task_scopes.len() > 1 {
            return Err(AgentIsolationError("agent_task_scope_denied"));
        }
        let generation = maximum
            .checked_add(1)
            .ok_or(AgentIsolationError("agent_run_generation_exhausted"))?;
        let old_runs = tx.prepare("SELECT agent_id,run_id,generation FROM wc_agent_runs WHERE agent_id=?1 ORDER BY generation LIMIT 1025").map_err(unavailable)?.query_map([agent_id], |r|Ok(webcodex_core::runner_protocol::AgentRunRef {agent_id:r.get(0)?,agent_run_id:r.get(1)?,generation:r.get(2)?})).map_err(unavailable)?.collect::<Result<Vec<_>,_>>().map_err(unavailable)?;
        if old_runs.len() > 1024 {
            return Err(AgentIsolationError("agent_execution_close_limit"));
        }
        tx.execute("UPDATE wc_agent_runs SET state='revoked' WHERE agent_id=?1 AND state IN ('active','pending')",
            params![agent_id]).map_err(unavailable)?;
        let grant = issue_run(&tx, agent, generation, "active", None)?;
        if let Some(goal_id) = task_scopes.first() {
            tx.execute(
                "UPDATE wc_agent_runs SET task_goal_id=?2 WHERE run_id=?1",
                params![grant.run_id, goal_id],
            )
            .map_err(unavailable)?;
            let recovered = VerifiedAgentRun {
                agent_id: grant.agent.agent_id.clone(),
                owner_user: grant.agent.owner_user.clone(),
                project: grant.agent.project.clone(),
                run_id: grant.run_id.clone(),
                generation: grant.generation,
                expires_at_unix_ms: grant.expires_at_unix_ms,
            };
            crate::host_goal::recover_handoff_run_on_connection(&tx, &recovered, goal_id, now_ms())
                .map_err(|e| AgentIsolationError::new(e.code()))?;
        }
        // Retain existing Goal diagnostics first. Nothing above is committed
        // or externally issued until every old carrier has closed in this tx.
        for run in &old_runs {
            crate::agent_execution_close::require_agent_run_execution_closed_on_connection(
                &tx, owner_user, project, run,
            )?;
        }
        audit(&tx, "recover_agent", agent_id, &grant.run_id)?;
        tx.commit().map_err(unavailable)?;
        Ok(grant)
    }

    /// Trusted management revocation, scoped to the exact owned Agent and run.
    pub fn revoke_agent_run(
        &self,
        owner_user: &str,
        project: &str,
        agent_id: &str,
        run_id: &str,
    ) -> Result<(), AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        owned_agent(&tx, owner_user, project, agent_id)?;
        let generation: i64 = tx
            .query_row(
                "SELECT generation FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2",
                params![run_id, agent_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable)?
            .ok_or_else(invalid_run)?;
        crate::agent_execution_close::require_agent_run_execution_closed_on_connection(
            &tx,
            owner_user,
            project,
            &webcodex_core::runner_protocol::AgentRunRef {
                agent_id: agent_id.into(),
                agent_run_id: run_id.into(),
                generation,
            },
        )?;
        let changed = tx
            .execute(
                "UPDATE wc_agent_runs SET state='revoked' WHERE run_id=?1 AND agent_id=?2",
                params![run_id, agent_id],
            )
            .map_err(unavailable)?;
        if changed != 1 {
            return Err(invalid_run());
        }
        audit(&tx, "revoke_run", agent_id, run_id)?;
        tx.commit().map_err(unavailable)?;
        Ok(())
    }
}
