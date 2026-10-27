//! Bootstrap-issued first enrollment for a new logical Agent. The grant is
//! never a bearer run token and can bind only one fresh trusted host window.
use super::*;
#[path = "agent_enrollment_cli_resume.rs"]
mod cli_resume;
pub(crate) use cli_resume::recover_cancelled_cli_worker_on_connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Instant;

const MAX_INPUT_BYTES: usize = 256 * 1024;
const MAX_ENROLLMENT_LIFETIME_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentEnrollmentPurpose {
    Manager,
    Member,
    Fallback,
    CliWorker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentEnrollmentPolicy {
    pub max_subagents: u32,
    pub max_cli_calls: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentEnrollmentInput {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NewAgentEnrollment {
    pub purpose: AgentEnrollmentPurpose,
    pub owner_user: String,
    pub project: String,
    pub principal_key: String,
    pub task_id: String,
    pub task_directory: String,
    pub brief: String,
    pub inputs: Vec<AgentEnrollmentInput>,
    pub room_ids: Vec<String>,
    pub policy: AgentEnrollmentPolicy,
    pub expires_at_unix_ms: i64,
}

pub struct AgentEnrollmentGrant(String);
impl AgentEnrollmentGrant {
    pub fn into_secret(self) -> String {
        self.0
    }
}
impl fmt::Debug for AgentEnrollmentGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AgentEnrollmentGrant([REDACTED])")
    }
}

#[derive(Debug)]
pub struct AgentEnrollmentIssued {
    pub enrollment_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub expires_at_unix_ms: i64,
    pub replayed: bool,
    pub grant: Option<AgentEnrollmentGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentEnrollmentSnapshot {
    pub name: String,
    pub content: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentEnrollmentProjection {
    pub enrollment_id: String,
    pub agent_id: String,
    pub project: String,
    pub task_id: String,
    pub task_directory: String,
    pub purpose: AgentEnrollmentPurpose,
    pub policy: AgentEnrollmentPolicy,
    pub brief: String,
    pub inputs: Vec<AgentEnrollmentSnapshot>,
    pub room_ids: Vec<String>,
}

#[derive(Debug)]
pub struct AgentEnrollmentClaim {
    pub binding: VerifiedAgentRun,
    pub projection: AgentEnrollmentProjection,
    pub replayed: bool,
}

/// Exact initial enrollment facts for trusted delivery reconciliation. This
/// never follows a newer run or returns the raw host session/claim secret.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentEnrollmentStatus {
    pub enrollment_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub generation: i64,
    pub task_id: String,
    pub state: String,
    pub run_state: String,
    pub window_binding_ref: Option<String>,
    pub window_enabled: bool,
    pub principal_revoked: bool,
    pub claimed_at_unix_ms: Option<i64>,
    pub claim_expires_at_unix_ms: i64,
    pub claim_expired: bool,
    pub delivery_state: String,
    pub run_expires_at_unix_ms: i64,
}

/// Trusted Host discovery metadata, not an authentication or dispatch permit.
/// No credential, window key, task directory or private context is projected.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentHostManagerBinding {
    pub root_task_id: String,
    pub host_id: String,
    pub manager_agent_id: String,
    pub manager_run_id: String,
    pub generation: i64,
    pub owner_user: String,
    pub project: String,
    pub enrollment_id: String,
    pub native_host_generation: i64,
    pub run_expires_at_ms: i64,
    pub window_binding_ref: String,
}

fn window_binding_ref(principal: &str, window: &str, run: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"webcodex.agent-host-binding-ref.v1\0");
    for part in [principal, window, run] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("wc_host_binding_{:x}", hash.finalize())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAssignmentOperation {
    Read,
    Delegate,
    UseCli,
    ManagePolicy,
    PlanManage,
    TaskStart,
    TaskComplete,
    ProjectFiles,
}

#[derive(Debug, Clone)]
pub struct AgentEnrollmentAuthority {
    pub agent_id: String,
    pub project: String,
    pub task_id: String,
    pub task_directory: String,
    pub purpose: AgentEnrollmentPurpose,
    pub policy: AgentEnrollmentPolicy,
    /// Store-only bridge for an already authorized exact task. Never create a
    /// user AuthContext or expose this principal to a generic runtime caller.
    pub(crate) communication_principal: crate::CommunicationPrincipal,
}

fn denied() -> AgentIsolationError {
    AgentIsolationError::new("agent_enrollment_denied")
}
fn stale_source() -> AgentIsolationError {
    AgentIsolationError::new("agent_enrollment_stale_source")
}
fn invalid() -> AgentIsolationError {
    AgentIsolationError::new("agent_enrollment_invalid_input")
}
fn unavailable_enrollment(_: impl fmt::Display) -> AgentIsolationError {
    AgentIsolationError::new("agent_enrollment_unavailable")
}
fn digest(domain: &[u8], value: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(value);
    format!("{:x}", hash.finalize())
}
fn now_after(now: i64, started: Instant) -> i64 {
    now.saturating_add(started.elapsed().as_millis().min(i64::MAX as u128) as i64)
        .max(now_ms())
}
fn secret_hash(secret: &str) -> String {
    digest(
        b"webcodex.initial-enrollment.secret.v1\0",
        secret.as_bytes(),
    )
}
fn valid_secret(secret: &str) -> bool {
    secret.strip_prefix("wc_en_").is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn key(value: &str, max: usize) -> Result<(), AgentIsolationError> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn private_parts(path: &str) -> Result<Vec<&str>, AgentIsolationError> {
    let parts: Vec<_> = path.split('/').collect();
    if path.len() > 1024
        || parts.len() > 16
        || parts
            .iter()
            .any(|part| !webcodex_core::agent_private_path::component_valid(part))
    {
        return Err(AgentIsolationError::new("agent_assignment_path_denied"));
    }
    Ok(parts)
}
fn normalize(
    mut request: NewAgentEnrollment,
    now: i64,
) -> Result<NewAgentEnrollment, AgentIsolationError> {
    for value in [
        &request.owner_user,
        &request.project,
        &request.principal_key,
        &request.task_id,
    ] {
        key(value, 512)?;
    }
    // Every enrollment brief is task text, not an identity key. Preserve
    // paragraphs verbatim while keeping the same byte and NUL bounds.
    if request.brief.trim().is_empty() || request.brief.len() > 8192 || request.brief.contains('\0')
    {
        return Err(invalid());
    }
    let parts = private_parts(&request.task_directory)?;
    if parts.len() < 2 || parts[0] != "workspace" {
        return Err(invalid());
    }
    if request.policy.max_subagents > 32
        || request.policy.max_cli_calls > 32
        || (request.purpose != AgentEnrollmentPurpose::Manager && request.policy.max_subagents != 0)
        || (matches!(
            request.purpose,
            AgentEnrollmentPurpose::Fallback | AgentEnrollmentPurpose::CliWorker
        ) && request.policy.max_cli_calls != 0)
        || request.expires_at_unix_ms <= now
        || request.expires_at_unix_ms > now.saturating_add(MAX_ENROLLMENT_LIFETIME_MS)
        || request.inputs.len() > 8
        || request.room_ids.len() > 64
    {
        return Err(invalid());
    }
    let mut names = BTreeSet::new();
    let mut bytes = 0usize;
    for input in &request.inputs {
        key(&input.name, 128)?;
        bytes = bytes.saturating_add(input.content.len());
        if input.content.len() > 64 * 1024
            || input.content.contains('\0')
            || !names.insert(input.name.clone())
        {
            return Err(invalid());
        }
    }
    if bytes > MAX_INPUT_BYTES {
        return Err(invalid());
    }
    request.inputs.sort_by(|a, b| a.name.cmp(&b.name));
    request.room_ids.sort();
    if request.room_ids.windows(2).any(|p| p[0] == p[1]) {
        return Err(invalid());
    }
    for room in &request.room_ids {
        key(room, 128)?;
    }
    Ok(request)
}

pub(super) fn ensure_enrollment_schema(conn: &Connection) -> anyhow::Result<()> {
    // SQLite cannot widen a CHECK in place. Enrollment has no incoming table
    // foreign keys; preserve its rows and unique constraints atomically.
    let schema: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='wc_agent_enrollments'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(schema) = schema.filter(|sql| sql.contains("('manager','member','fallback')")) {
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let widened = schema
            .replacen(
                "wc_agent_enrollments",
                "wc_agent_enrollments_role_upgrade",
                1,
            )
            .replace(
                "('manager','member','fallback')",
                "('manager','member','fallback','cli_worker')",
            );
        tx.execute_batch(&widened)?;
        tx.execute_batch(
            "INSERT INTO wc_agent_enrollments_role_upgrade SELECT * FROM wc_agent_enrollments;
            DROP TABLE wc_agent_enrollments;
            ALTER TABLE wc_agent_enrollments_role_upgrade RENAME TO wc_agent_enrollments;",
        )?;
        tx.commit()?;
    }
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_agent_enrollments(
      enrollment_id TEXT PRIMARY KEY, owner_user TEXT NOT NULL,project TEXT NOT NULL,principal_key TEXT NOT NULL,
      agent_id TEXT NOT NULL UNIQUE REFERENCES wc_isolated_agents(agent_id),run_id TEXT NOT NULL UNIQUE REFERENCES wc_agent_runs(run_id),
      purpose TEXT NOT NULL CHECK(purpose IN ('manager','member','fallback','cli_worker')),task_id TEXT NOT NULL,task_directory TEXT NOT NULL,policy_json TEXT NOT NULL,
      request_json TEXT NOT NULL,request_hash TEXT NOT NULL,idempotency_hash TEXT NOT NULL,secret_hash TEXT NOT NULL UNIQUE,
      source_agent_id TEXT,source_run_id TEXT,source_generation INTEGER,
      expires_at_unix_ms INTEGER NOT NULL,state TEXT NOT NULL CHECK(state IN ('issued','claimed','revoked')),
      window_key TEXT,created_at_unix_ms INTEGER NOT NULL,claimed_at_unix_ms INTEGER,
      UNIQUE(owner_user,project,idempotency_hash));
      CREATE TABLE IF NOT EXISTS wc_agent_enrollment_assignments(
      agent_id TEXT PRIMARY KEY REFERENCES wc_isolated_agents(agent_id),owner_user TEXT NOT NULL,project TEXT NOT NULL,task_id TEXT NOT NULL,
      state TEXT NOT NULL CHECK(state IN ('active','released')));
      CREATE UNIQUE INDEX IF NOT EXISTS wc_agent_enrollment_task_owner ON wc_agent_enrollment_assignments(owner_user,project,task_id) WHERE state='active';
      CREATE TABLE IF NOT EXISTS wc_agent_enrollment_goals(agent_id TEXT PRIMARY KEY REFERENCES wc_isolated_agents(agent_id),goal_id TEXT NOT NULL UNIQUE REFERENCES wc_host_goals(goal_id));
      CREATE TABLE IF NOT EXISTS wc_agent_enrollment_principal_revocations(principal_key TEXT PRIMARY KEY,revoked_at_unix_ms INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS wc_agent_host_windows(principal_key TEXT NOT NULL,window_key TEXT NOT NULL,run_id TEXT NOT NULL UNIQUE REFERENCES wc_agent_runs(run_id),enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),PRIMARY KEY(principal_key,window_key));
      CREATE TRIGGER IF NOT EXISTS wc_agent_enrollment_immutable BEFORE UPDATE OF owner_user,project,principal_key,agent_id,run_id,purpose,task_id,task_directory,policy_json,request_json,request_hash,idempotency_hash,secret_hash,source_agent_id,source_run_id,source_generation,expires_at_unix_ms ON wc_agent_enrollments BEGIN SELECT RAISE(ABORT,'agent enrollment authority is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS wc_agent_enrollment_goal_immutable BEFORE UPDATE ON wc_agent_enrollment_goals BEGIN SELECT RAISE(ABORT,'agent enrollment Goal is immutable'); END;")?;
    for (name, definition) in [
        ("delivery_state", "TEXT NOT NULL DEFAULT 'issued'"),
        ("delivery_key_hash", "TEXT"),
        ("delivery_secret_hash", "TEXT"),
        ("delivery_expires_at_unix_ms", "INTEGER"),
        ("delivery_source_run_id", "TEXT"),
        ("delivery_source_generation", "INTEGER"),
    ] {
        let mut stmt = conn.prepare("PRAGMA table_info(wc_agent_enrollments)")?;
        let found = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|n| n == name);
        if !found {
            conn.execute_batch(&format!(
                "ALTER TABLE wc_agent_enrollments ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    Ok(())
}

struct Row {
    id: String,
    agent_id: String,
    run_id: String,
    request: NewAgentEnrollment,
    request_hash: String,
    secret_hash: String,
    source_agent_id: Option<String>,
    source_run_id: Option<String>,
    source_generation: Option<i64>,
    state: String,
    window: Option<String>,
}
struct Meta {
    owner: String,
    project: String,
    task_id: String,
    directory: String,
    purpose: AgentEnrollmentPurpose,
    policy: AgentEnrollmentPolicy,
    state: String,
}
fn meta(conn: &Connection, agent: &str) -> Result<Option<Meta>, AgentIsolationError> {
    let value=conn.query_row("SELECT owner_user,project,task_id,task_directory,purpose,policy_json,state FROM wc_agent_enrollments WHERE agent_id=?1",[agent],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?))).optional().map_err(unavailable_enrollment)?;
    value
        .map(|r| {
            Ok(Meta {
                owner: r.0,
                project: r.1,
                task_id: r.2,
                directory: r.3,
                purpose: match r.4.as_str() {
                    "manager" => AgentEnrollmentPurpose::Manager,
                    "member" => AgentEnrollmentPurpose::Member,
                    "fallback" => AgentEnrollmentPurpose::Fallback,
                    "cli_worker" => AgentEnrollmentPurpose::CliWorker,
                    _ => return Err(denied()),
                },
                policy: serde_json::from_str(&r.5).map_err(unavailable_enrollment)?,
                state: r.6,
            })
        })
        .transpose()
}
fn row(conn: &Connection, id: &str) -> Result<Option<Row>, AgentIsolationError> {
    let raw=conn.query_row("SELECT enrollment_id,agent_id,run_id,request_json,request_hash,COALESCE(delivery_secret_hash,secret_hash),source_agent_id,COALESCE(delivery_source_run_id,source_run_id),COALESCE(delivery_source_generation,source_generation),state,window_key,COALESCE(delivery_expires_at_unix_ms,expires_at_unix_ms) FROM wc_agent_enrollments WHERE enrollment_id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<i64>>(8)?,r.get::<_,String>(9)?,r.get::<_,Option<String>>(10)?,r.get::<_,i64>(11)?))).optional().map_err(unavailable_enrollment)?;
    raw.map(|r| {
        Ok(Row {
            id: r.0,
            agent_id: r.1,
            run_id: r.2,
            request: {
                let mut request: NewAgentEnrollment =
                    serde_json::from_str(&r.3).map_err(unavailable_enrollment)?;
                request.expires_at_unix_ms = r.11;
                request
            },
            request_hash: r.4,
            secret_hash: r.5,
            source_agent_id: r.6,
            source_run_id: r.7,
            source_generation: r.8,
            state: r.9,
            window: r.10,
        })
    })
    .transpose()
}
fn for_agent(conn: &Connection, agent: &str) -> Result<Option<Row>, AgentIsolationError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT enrollment_id FROM wc_agent_enrollments WHERE agent_id=?1",
            [agent],
            |r| r.get(0),
        )
        .optional()
        .map_err(unavailable_enrollment)?;
    id.map(|id| row(conn, &id)).transpose().map(Option::flatten)
}
fn projection(row: &Row) -> AgentEnrollmentProjection {
    let r = &row.request;
    AgentEnrollmentProjection {
        enrollment_id: row.id.clone(),
        agent_id: row.agent_id.clone(),
        project: r.project.clone(),
        task_id: r.task_id.clone(),
        task_directory: r.task_directory.clone(),
        purpose: r.purpose,
        policy: r.policy.clone(),
        brief: r.brief.clone(),
        inputs: r
            .inputs
            .iter()
            .map(|i| AgentEnrollmentSnapshot {
                name: i.name.clone(),
                content: i.content.clone(),
                sha256: format!("{:x}", Sha256::digest(i.content.as_bytes())),
            })
            .collect(),
        room_ids: r.room_ids.clone(),
    }
}
fn principal_valid(conn: &Connection, principal: &str) -> Result<(), AgentIsolationError> {
    let revoked:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_principal_revocations WHERE principal_key=?1)",[principal],|r|r.get(0)).map_err(unavailable_enrollment)?;
    if revoked {
        Err(denied())
    } else {
        Ok(())
    }
}
fn assignment_active(conn: &Connection, agent: &str) -> Result<bool, AgentIsolationError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments WHERE agent_id=?1 AND state='active')",[agent],|r|r.get(0)).map_err(unavailable_enrollment)
}

pub(crate) fn managed_user_principal(owner: &str) -> crate::CommunicationPrincipal {
    let mut hash = Sha256::new();
    hash.update(b"webcodex.communication.principal.v1\0managed-user\0");
    hash.update(owner.as_bytes());
    crate::CommunicationPrincipal {
        kind: "managed-user".into(),
        digest: format!("wc_commprincipal_{:x}", hash.finalize()),
    }
}

/// An enrollment references the existing public task ledger. A bounded label,
/// another owner's task, a terminal task, or a legacy execution is insufficient.
pub(crate) fn require_enrollment_public_task_on_connection(
    conn: &Connection,
    owner: &str,
    project: &str,
    task_id: &str,
) -> Result<(), AgentIsolationError> {
    let principal = managed_user_principal(owner);
    let live: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM wc_agent_tasks WHERE task_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3 AND referenced_project_id=?4 AND state IN ('ready','active') AND assignee_agent_id IS NULL AND latest_attempt_id IS NULL)",
        params![task_id, principal.kind, principal.digest, project],
        |row| row.get(0),
    ).map_err(unavailable_enrollment)?;
    if live {
        Ok(())
    } else {
        Err(AgentIsolationError::new(
            "agent_enrollment_task_unavailable",
        ))
    }
}

/// Called from every existing run authentication/revalidation primitive. This
/// is only an additional restriction and never authenticates a pending run.
pub(super) fn enrollment_run_gate(
    conn: &Connection,
    binding: &VerifiedAgentRun,
) -> Result<(), AgentIsolationError> {
    if let Some(row) = meta(conn, binding.agent_id())? {
        if row.state != "claimed"
            || row.project != binding.project()
            || row.owner != binding.owner_user()
        {
            return Err(denied());
        }
    }
    Ok(())
}

fn authority(
    conn: &Connection,
    binding: &VerifiedAgentRun,
) -> Result<AgentEnrollmentAuthority, AgentIsolationError> {
    validate_agent_run_on_connection(conn, binding)?;
    let row = meta(conn, binding.agent_id())?.ok_or_else(denied)?;
    if row.state != "claimed" {
        return Err(denied());
    }
    let communication_principal = managed_user_principal(&row.owner);
    Ok(AgentEnrollmentAuthority {
        agent_id: binding.agent_id().into(),
        project: row.project,
        task_id: row.task_id,
        task_directory: row.directory,
        purpose: row.purpose,
        policy: row.policy,
        communication_principal,
    })
}

pub(crate) fn authorize_agent_assignment_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    task_id: &str,
    operation: AgentAssignmentOperation,
) -> Result<AgentEnrollmentAuthority, AgentIsolationError> {
    let auth = authority(conn, binding)?;
    if auth.task_id != task_id {
        return Err(denied());
    }
    if operation != AgentAssignmentOperation::Read {
        agent_run_business_gate_on_connection(conn, binding)?;
        require_enrollment_public_task_on_connection(
            conn,
            binding.owner_user(),
            &auth.project,
            task_id,
        )?;
        if !assignment_active(conn, binding.agent_id())? {
            return Err(AgentIsolationError::new("agent_assignment_released"));
        }
    }
    let allowed = match operation {
        AgentAssignmentOperation::Read => true,
        AgentAssignmentOperation::PlanManage => auth.purpose == AgentEnrollmentPurpose::Manager,
        AgentAssignmentOperation::Delegate => {
            auth.purpose == AgentEnrollmentPurpose::Manager && auth.policy.max_subagents > 0
        }
        AgentAssignmentOperation::UseCli => {
            matches!(
                auth.purpose,
                AgentEnrollmentPurpose::Manager | AgentEnrollmentPurpose::Member
            ) && auth.policy.max_cli_calls > 0
        }
        AgentAssignmentOperation::ManagePolicy => false,
        AgentAssignmentOperation::TaskStart | AgentAssignmentOperation::TaskComplete => true,
        AgentAssignmentOperation::ProjectFiles => matches!(
            auth.purpose,
            AgentEnrollmentPurpose::Manager | AgentEnrollmentPurpose::Member
        ),
    };
    if !allowed {
        return Err(AgentIsolationError::new("agent_role_denied"));
    }
    Ok(auth)
}

pub(crate) fn authorize_agent_delegated_task_on_connection(
    conn: &Connection,
    manager: &VerifiedAgentRun,
    child_task_id: &str,
) -> Result<(), AgentIsolationError> {
    let own = authority(conn, manager)?;
    authorize_agent_assignment_on_connection(
        conn,
        manager,
        &own.task_id,
        AgentAssignmentOperation::Read,
    )?;
    if own.purpose != AgentEnrollmentPurpose::Manager {
        return Err(AgentIsolationError::new("agent_role_denied"));
    }
    // Expiry prevents claiming or admitting new work; it must not erase the
    // manager's historical delegation when reading the original plan/receipts.
    // Plan admission separately checks the live enrollment in the budget query.
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments WHERE source_agent_id=?1 AND owner_user=?2 AND project=?3 AND task_id=?4 AND purpose='member' AND state IN ('claimed','issued'))",params![manager.agent_id(),manager.owner_user(),manager.project(),child_task_id],|r|r.get(0)).map_err(unavailable_enrollment)?;
    if exists {
        Ok(())
    } else {
        Err(denied())
    }
}

/// Plan admission must intersect the child's own immutable ceiling, not only
/// its manager's aggregate budget. Ambiguous reservations are never guessed.
pub(crate) fn delegated_cli_budget_on_connection(
    conn: &Connection,
    manager: &VerifiedAgentRun,
    child_task_id: &str,
) -> Result<u32, AgentIsolationError> {
    authorize_agent_delegated_task_on_connection(conn, manager, child_task_id)?;
    let mut statement = conn.prepare(
        "SELECT policy_json FROM wc_agent_enrollments WHERE source_agent_id=?1 AND owner_user=?2 AND project=?3 AND task_id=?4 AND purpose='member' AND (state='claimed' OR (state='issued' AND (delivery_state='reserved' OR COALESCE(delivery_expires_at_unix_ms,expires_at_unix_ms)>?5))) LIMIT 2",
    ).map_err(unavailable_enrollment)?;
    let policies = statement
        .query_map(
            params![
                manager.agent_id(),
                manager.owner_user(),
                manager.project(),
                child_task_id,
                now_ms()
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(unavailable_enrollment)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable_enrollment)?;
    if policies.len() != 1 {
        return Err(AgentIsolationError::new(
            "agent_delegated_assignment_ambiguous",
        ));
    }
    let policy: AgentEnrollmentPolicy =
        serde_json::from_str(&policies[0]).map_err(unavailable_enrollment)?;
    Ok(policy.max_cli_calls)
}

fn source_valid(
    conn: &Connection,
    request: &NewAgentEnrollment,
    source: Option<&VerifiedAgentRun>,
) -> Result<(), AgentIsolationError> {
    match (request.purpose, source) {
        (AgentEnrollmentPurpose::Manager, None) => Ok(()),
        (
            AgentEnrollmentPurpose::Member
            | AgentEnrollmentPurpose::Fallback
            | AgentEnrollmentPurpose::CliWorker,
            Some(source),
        ) => {
            if source.owner_user() != request.owner_user || source.project() != request.project {
                return Err(stale_source());
            }
            let own = authority(conn, source).map_err(|_| stale_source())?;
            let operation = match request.purpose {
                AgentEnrollmentPurpose::Member => AgentAssignmentOperation::Delegate,
                AgentEnrollmentPurpose::CliWorker => AgentAssignmentOperation::UseCli,
                _ => AgentAssignmentOperation::PlanManage,
            };
            let auth =
                authorize_agent_assignment_on_connection(conn, source, &own.task_id, operation)
                    .map_err(|_| stale_source())?;
            let registered:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE principal_key=?1 AND run_id=?2 AND enabled=1)",params![request.principal_key,source.run_id()],|r|r.get(0)).map_err(unavailable_enrollment)?;
            if !registered
                || request.task_id == auth.task_id
                || request.policy.max_subagents > auth.policy.max_subagents
                || request.policy.max_cli_calls > auth.policy.max_cli_calls
            {
                return Err(stale_source());
            }
            Ok(())
        }
        _ => Err(stale_source()),
    }
}
fn stored_source(
    conn: &Connection,
    row: &Row,
) -> Result<Option<VerifiedAgentRun>, AgentIsolationError> {
    match (
        &row.source_agent_id,
        &row.source_run_id,
        row.source_generation,
    ) {
        (None, None, None) => Ok(None),
        (Some(agent), Some(run), Some(generation)) => {
            let owner = host_goal_owner_key(&row.request.project, agent);
            let binding = verify_agent_run_context_on_connection(conn, &owner, run)
                .map_err(|_| stale_source())?;
            if binding.generation() != generation {
                return Err(stale_source());
            }
            Ok(Some(binding))
        }
        _ => Err(stale_source()),
    }
}
fn rooms_valid(conn: &Connection, request: &NewAgentEnrollment) -> Result<(), AgentIsolationError> {
    for room in &request.room_ids {
        let found:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_meeting_rooms WHERE room_id=?1 AND project=?2)",params![room,request.project],|r|r.get(0)).map_err(unavailable_enrollment)?;
        if !found {
            return Err(denied());
        }
    }
    Ok(())
}

impl Database {
    /// Trusted control only. Resolve the exact Root/Host association in one
    /// read snapshot. The partial unique active-run index is the current-run
    /// pointer; enrollment.run_id records only the initial claim. A pending
    /// successor never supersedes that pointer. Callers must still authenticate
    /// and fence each actual dispatch, and must not dispatch from cached facts
    /// when this read returns None or an error.
    pub fn read_agent_host_manager_binding(
        &self,
        owner_user: &str,
        project: &str,
        root_task_id: &str,
        host_id: &str,
    ) -> Result<Option<AgentHostManagerBinding>, AgentIsolationError> {
        for value in [owner_user, project, root_task_id, host_id] {
            key(value, 512)?;
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(unavailable_enrollment)?;
        let principal = managed_user_principal(owner_user);
        let mut statement = tx
            .prepare(
                "SELECT e.enrollment_id,a.agent_id,r.run_id,r.generation,
                    r.expires_at_unix_ms,h.generation,h.principal_key,w.window_key
             FROM wc_agent_enrollment_assignments assignment
             JOIN wc_agent_enrollments e ON e.agent_id=assignment.agent_id
               AND e.owner_user=assignment.owner_user AND e.project=assignment.project
               AND e.task_id=assignment.task_id
             JOIN wc_isolated_agents a ON a.agent_id=e.agent_id
               AND a.owner_user=e.owner_user AND a.project=e.project
             JOIN wc_agent_runs r ON r.agent_id=a.agent_id AND r.state='active'
             JOIN wc_native_agent_hosts h ON h.principal_key=e.principal_key
             JOIN wc_agent_host_windows w ON w.run_id=r.run_id
               AND w.principal_key=h.principal_key
             JOIN wc_agent_tasks t ON t.task_id=assignment.task_id
             LEFT JOIN wc_agent_public_executions x ON x.task_id=t.task_id
             LEFT JOIN wc_team_plans p ON p.root_task_id=t.task_id
             WHERE assignment.owner_user=?1 AND assignment.project=?2
               AND assignment.task_id=?3 AND assignment.state='active'
               AND e.state='claimed' AND e.purpose='manager'
               AND e.source_agent_id IS NULL AND e.source_run_id IS NULL
               AND e.source_generation IS NULL
               AND h.host_id=?4 AND h.enabled=1 AND h.generation>0 AND w.enabled=1
               AND r.generation>0 AND r.expires_at_unix_ms>?5
               AND r.execution_close_id IS NULL
               AND NOT EXISTS(SELECT 1 FROM wc_agent_enrollment_principal_revocations
                              WHERE principal_key=h.principal_key)
               AND t.owner_principal_kind=?6 AND t.owner_principal_digest=?7
               AND t.referenced_project_id=?2 AND t.state IN ('ready','active')
               AND t.assignee_agent_id IS NULL AND t.latest_attempt_id IS NULL
               AND (x.task_id IS NULL OR (x.agent_id=a.agent_id
                    AND x.state='active' AND t.state='active'))
               AND (p.root_task_id IS NULL OR (p.owner_principal_kind=?6
                    AND p.owner_principal_digest=?7 AND p.project_id=?2
                    AND json_extract(p.record_json,'$.root_task_id')=?3
                    AND json_extract(p.record_json,'$.project_id')=?2
                    AND json_type(p.record_json,'$.cancel_requested')='false'
                    AND json_extract(p.record_json,'$.status')='active'))
             LIMIT 2",
            )
            .map_err(unavailable_enrollment)?;
        let mut rows = statement
            .query(params![
                owner_user,
                project,
                root_task_id,
                host_id,
                now_ms(),
                principal.kind,
                principal.digest
            ])
            .map_err(unavailable_enrollment)?;
        let Some(row) = rows.next().map_err(unavailable_enrollment)? else {
            return Ok(None);
        };
        let enrollment_id: String = row.get(0).map_err(unavailable_enrollment)?;
        let agent_id: String = row.get(1).map_err(unavailable_enrollment)?;
        let run_id: String = row.get(2).map_err(unavailable_enrollment)?;
        let generation: i64 = row.get(3).map_err(unavailable_enrollment)?;
        let run_expires_at_ms: i64 = row.get(4).map_err(unavailable_enrollment)?;
        let native_host_generation: i64 = row.get(5).map_err(unavailable_enrollment)?;
        let host_principal: String = row.get(6).map_err(unavailable_enrollment)?;
        let window: String = row.get(7).map_err(unavailable_enrollment)?;
        if rows.next().map_err(unavailable_enrollment)?.is_some() {
            return Err(denied());
        }
        // The mutation gate initializes identity_source. Discovery must not do
        // that: read its lifecycle restrictions without changing context state.
        // No context is valid for a newly claimed Manager; partial bookkeeping
        // or draining/retired/failed context is never dispatchable.
        let owner = host_goal_owner_key(project, &agent_id);
        let context_open: bool = tx
            .query_row(
                "SELECT (NOT EXISTS(SELECT 1 FROM wc_host_context_meters
                                WHERE owner_key=?1 AND context_epoch=?2)
                     AND NOT EXISTS(SELECT 1 FROM wc_host_context_states
                                    WHERE owner_key=?1 AND context_epoch=?2))
                    OR EXISTS(SELECT 1 FROM wc_host_context_states s
                              JOIN wc_host_context_meters m USING(owner_key,context_epoch)
                              WHERE s.owner_key=?1 AND s.context_epoch=?2
                                AND s.state IN (0,1)
                                AND (s.identity_source IS NULL OR s.identity_source='agent_run'))",
                params![owner, run_id],
                |row| row.get(0),
            )
            .map_err(unavailable_enrollment)?;
        if !context_open || run_expires_at_ms <= now_ms() {
            return Ok(None);
        }
        Ok(Some(AgentHostManagerBinding {
            root_task_id: root_task_id.into(),
            host_id: host_id.into(),
            manager_agent_id: agent_id,
            manager_run_id: run_id.clone(),
            generation,
            owner_user: owner_user.into(),
            project: project.into(),
            enrollment_id,
            native_host_generation,
            run_expires_at_ms,
            window_binding_ref: window_binding_ref(&host_principal, &window, &run_id),
        }))
    }

    /// Trusted control only: read the exact initial dispatch binding, including
    /// after expiration/revocation. No mutation, renewal or latest-run lookup.
    pub fn agent_enrollment_status(
        &self,
        owner_user: &str,
        project: &str,
        enrollment_id: &str,
    ) -> Result<AgentEnrollmentStatus, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(unavailable_enrollment)?;
        let enrolled = row(&tx, enrollment_id)?.ok_or_else(denied)?;
        if enrolled.request.owner_user != owner_user || enrolled.request.project != project {
            return Err(denied());
        }
        let (generation, run_state, run_expiry): (i64, String, i64) = tx.query_row(
            "SELECT generation,state,expires_at_unix_ms FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2",
            params![enrolled.run_id, enrolled.agent_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).map_err(unavailable_enrollment)?;
        let claimed: Option<i64> = tx
            .query_row(
                "SELECT claimed_at_unix_ms FROM wc_agent_enrollments WHERE enrollment_id=?1",
                [enrollment_id],
                |r| r.get(0),
            )
            .map_err(unavailable_enrollment)?;
        let mut binding_ref = None;
        let mut window_enabled = false;
        if let Some(window) = enrolled.window.as_deref() {
            let enabled: Option<bool> = tx.query_row(
                "SELECT enabled FROM wc_agent_host_windows WHERE principal_key=?1 AND window_key=?2 AND run_id=?3",
                params![enrolled.request.principal_key, window, enrolled.run_id], |r|r.get(0),
            ).optional().map_err(unavailable_enrollment)?;
            if let Some(enabled) = enabled {
                binding_ref = Some(window_binding_ref(
                    &enrolled.request.principal_key,
                    window,
                    &enrolled.run_id,
                ));
                window_enabled = enabled;
            }
        }
        let principal_revoked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_principal_revocations WHERE principal_key=?1)", [&enrolled.request.principal_key], |r|r.get(0)).map_err(unavailable_enrollment)?;
        Ok(AgentEnrollmentStatus {
            enrollment_id: enrolled.id,
            agent_id: enrolled.agent_id,
            run_id: enrolled.run_id,
            generation,
            task_id: enrolled.request.task_id,
            state: enrolled.state,
            run_state,
            window_binding_ref: binding_ref,
            window_enabled,
            principal_revoked,
            claimed_at_unix_ms: claimed,
            claim_expires_at_unix_ms: enrolled.request.expires_at_unix_ms,
            claim_expired: enrolled.request.expires_at_unix_ms > 0
                && now_ms() >= enrolled.request.expires_at_unix_ms,
            delivery_state: tx
                .query_row(
                    "SELECT delivery_state FROM wc_agent_enrollments WHERE enrollment_id=?1",
                    [enrollment_id],
                    |r| r.get(0),
                )
                .map_err(unavailable_enrollment)?,
            run_expires_at_unix_ms: run_expiry,
        })
    }

    /// Trusted Bootstrap control only. This reserves a new identity, never a
    /// source Agent's identity, Memory, Goal, task lease, or private files.
    pub fn issue_agent_enrollment(
        &self,
        request: NewAgentEnrollment,
        source: Option<&VerifiedAgentRun>,
        idempotency_key: &str,
        now: i64,
    ) -> Result<AgentEnrollmentIssued, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_enrollment)?;
        let issued = issue_agent_enrollment_on_connection(
            &tx,
            request,
            source,
            idempotency_key,
            now,
            false,
        )?;
        tx.commit().map_err(unavailable_enrollment)?;
        Ok(issued)
    }

    pub fn agent_enrollment_claim_authorized(
        &self,
        principal: &str,
        now: i64,
    ) -> Result<bool, AgentIsolationError> {
        key(principal, 512)?;
        let started = Instant::now();
        let conn = self.conn.lock().unwrap();
        principal_valid(&conn, principal)?;
        conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.run_id=e.run_id WHERE e.principal_key=?1 AND e.state='issued' AND e.delivery_state!='reserved' AND COALESCE(e.delivery_expires_at_unix_ms,e.expires_at_unix_ms)>?2 AND r.state='pending' AND r.expires_at_unix_ms>?2)",params![principal,now_after(now,started)],|r|r.get(0)).map_err(unavailable_enrollment)
    }

    pub fn claim_agent_enrollment(
        &self,
        principal: &str,
        window: &str,
        enrollment_id: &str,
        claim_secret: &str,
        now: i64,
    ) -> Result<AgentEnrollmentClaim, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_enrollment)?;
        let claim = claim_agent_enrollment_on_connection(
            &tx,
            principal,
            window,
            enrollment_id,
            claim_secret,
            now,
        )?;
        tx.commit().map_err(unavailable_enrollment)?;
        Ok(claim)
    }

    pub fn read_agent_enrollment(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<AgentEnrollmentProjection, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(unavailable_enrollment)?;
        validate_agent_run_on_connection(&tx, binding)?;
        let row = for_agent(&tx, binding.agent_id())?.ok_or_else(denied)?;
        if row.state != "claimed" {
            return Err(denied());
        }
        Ok(projection(&row))
    }

    pub fn authorize_agent_assignment(
        &self,
        binding: &VerifiedAgentRun,
        task_id: &str,
        operation: AgentAssignmentOperation,
    ) -> Result<AgentEnrollmentAuthority, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_enrollment)?;
        authorize_agent_assignment_on_connection(&tx, binding, task_id, operation)
    }

    /// Explicit trusted control; no model-reported terminal status may call it.
    /// With no admitted executor this waits out every broker transaction. A
    /// future executor must additionally prove all old Jobs terminated first.
    pub fn revoke_agent_enrollment(
        &self,
        owner: &str,
        project: &str,
        id: &str,
    ) -> Result<(), AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_enrollment)?;
        let row = row(&tx, id)?
            .filter(|row| row.request.owner_user == owner && row.request.project == project)
            .ok_or_else(denied)?;
        revoke_on_connection(&tx, &row)?;
        tx.commit().map_err(unavailable_enrollment)
    }
}

fn revoke_on_connection(conn: &Connection, row: &Row) -> Result<(), AgentIsolationError> {
    if row.state == "claimed" {
        crate::agent_execution_close::require_agent_assignment_execution_closed_on_connection(
            conn,
            &row.request.owner_user,
            &row.request.project,
            &row.agent_id,
        )?;
    }
    conn.execute(
        "UPDATE wc_agent_enrollments SET state='revoked' WHERE enrollment_id=?1",
        [&row.id],
    )
    .map_err(unavailable_enrollment)?;
    conn.execute(
        "UPDATE wc_agent_runs SET state='revoked' WHERE agent_id=?1",
        [&row.agent_id],
    )
    .map_err(unavailable_enrollment)?;
    conn.execute("UPDATE wc_agent_host_windows SET enabled=0 WHERE run_id IN (SELECT run_id FROM wc_agent_runs WHERE agent_id=?1)",[&row.agent_id]).map_err(unavailable_enrollment)?;
    conn.execute(
        "UPDATE wc_agent_enrollment_assignments SET state='released' WHERE agent_id=?1",
        [&row.agent_id],
    )
    .map_err(unavailable_enrollment)?;
    audit(conn, "enrollment_revoke", &row.agent_id, &row.run_id)
}

/// Must run in the existing host-principal revocation transaction, including
/// a principal with no prior window. Otherwise an outstanding initial grant
/// could bind its first window after the revocation returned.
pub(crate) fn revoke_agent_enrollments_for_principal_on_connection(
    conn: &Connection,
    principal: &str,
) -> Result<(), AgentIsolationError> {
    conn.execute("INSERT OR IGNORE INTO wc_agent_enrollment_principal_revocations(principal_key,revoked_at_unix_ms) VALUES(?1,?2)",params![principal,now_ms()]).map_err(unavailable_enrollment)?;
    let ids=conn.prepare("SELECT enrollment_id FROM wc_agent_enrollments WHERE principal_key=?1 AND state='issued'").map_err(unavailable_enrollment)?.query_map([principal],|r|r.get::<_,String>(0)).map_err(unavailable_enrollment)?.collect::<Result<Vec<_>,_>>().map_err(unavailable_enrollment)?;
    for id in ids {
        revoke_on_connection(conn, &row(conn, &id)?.ok_or_else(denied)?)?;
    }
    Ok(())
}

/// An intersection with the existing run task scope, not a replacement for it.
pub(super) fn authorize_enrollment_goal_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    goal_id: Option<&str>,
) -> Result<(), AgentIsolationError> {
    let Some(row) = meta(conn, binding.agent_id())? else {
        return Ok(());
    };
    validate_agent_run_on_connection(conn, binding)?;
    if row.state != "claimed" {
        return Err(denied());
    }
    if let Some(goal_id) = goal_id {
        let own:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_goals WHERE agent_id=?1 AND goal_id=?2)",params![binding.agent_id(),goal_id],|r|r.get(0)).map_err(unavailable_enrollment)?;
        if !own {
            return Err(AgentIsolationError::new("agent_assignment_goal_denied"));
        }
    }
    Ok(())
}

pub(super) fn authorize_enrollment_target_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    action: &str,
    target: Option<&task_scope::Target<'_>>,
) -> Result<(), AgentIsolationError> {
    let Some(row) = meta(conn, binding.agent_id())? else {
        return Ok(());
    };
    validate_agent_run_on_connection(conn, binding)?;
    let writing = matches!(target, Some(task_scope::Target::Path { write: true, .. }))
        || matches!(action, "goal_plan_publish" | "goal_checkpoint_publish");
    if writing && !assignment_active(conn, binding.agent_id())? {
        return Err(AgentIsolationError::new("agent_assignment_released"));
    }
    let personal = matches!(target, Some(task_scope::Target::Path { path, .. }) if path.starts_with("memory/") || path.starts_with("identity/"));
    if writing && !personal {
        require_enrollment_public_task_on_connection(
            conn,
            binding.owner_user(),
            binding.project(),
            &row.task_id,
        )?;
    }
    match target {
        Some(task_scope::Target::Goal(goal)) => {
            authorize_enrollment_goal_on_connection(conn, binding, Some(goal))
        }
        Some(task_scope::Target::Path { path, write }) => {
            let parts = private_parts(path)?;
            match parts[0] {
                "memory" | "identity" => Ok(()),
                "workspace" => {
                    let task = private_parts(&row.directory)?;
                    if !parts.starts_with(&task) || (*write && parts.len() == task.len()) {
                        return Err(AgentIsolationError::new("agent_assignment_path_denied"));
                    }
                    if *write {
                        let goal: Option<String> = conn
                            .query_row(
                                "SELECT goal_id FROM wc_agent_enrollment_goals WHERE agent_id=?1",
                                [binding.agent_id()],
                                |r| r.get(0),
                            )
                            .optional()
                            .map_err(unavailable_enrollment)?;
                        task_scope::workspace_write_gate(
                            conn,
                            binding,
                            &goal.ok_or_else(|| {
                                AgentIsolationError::new("agent_assignment_goal_required")
                            })?,
                        )?;
                    }
                    Ok(())
                }
                "tasks" if parts.len() >= 2 && parts[1] == row.task_id => Ok(()),
                "goals" | "checkpoints" | "handoff" if !write && parts.len() >= 2 => {
                    authorize_enrollment_goal_on_connection(conn, binding, Some(parts[1]))
                }
                _ => Err(AgentIsolationError::new("agent_assignment_path_denied")),
            }
        }
        None => Err(AgentIsolationError::new("agent_assignment_target_required")),
    }
}

pub(crate) fn authorize_agent_goal_creation_on_connection(
    conn: &Connection,
    owner: &str,
    context: Option<&webcodex_core::host_goal::HostGoalInvocationContext>,
    directory: Option<&str>,
) -> Result<(), AgentIsolationError> {
    let Some(context) = context.filter(|context| context.identity_source == "agent_run") else {
        return Ok(());
    };
    let binding = verify_agent_run_context_on_connection(conn, owner, &context.context_epoch)?;
    let Some(row) = meta(conn, binding.agent_id())? else {
        return Ok(());
    };
    if directory != Some(row.directory.as_str()) {
        return Err(AgentIsolationError::new(
            "agent_assignment_directory_denied",
        ));
    }
    if !assignment_active(conn, binding.agent_id())? {
        return Err(AgentIsolationError::new("agent_assignment_released"));
    }
    Ok(())
}

pub(crate) fn bind_agent_enrollment_goal_on_connection(
    conn: &Connection,
    owner: &str,
    context: Option<&webcodex_core::host_goal::HostGoalInvocationContext>,
    goal_id: &str,
) -> Result<(), AgentIsolationError> {
    let Some(context) = context.filter(|context| context.identity_source == "agent_run") else {
        return Ok(());
    };
    let binding = verify_agent_run_context_on_connection(conn, owner, &context.context_epoch)?;
    let Some(row) = meta(conn, binding.agent_id())? else {
        return Ok(());
    };
    let owned:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goals WHERE goal_id=?1 AND owner_key=?2 AND project=?3 AND json_extract(snapshot_json,'$.task_directory')=?4)",params![goal_id,owner,binding.project(),row.directory],|r|r.get(0)).map_err(unavailable_enrollment)?;
    if !owned {
        return Err(AgentIsolationError::new("agent_assignment_goal_denied"));
    }
    let previous: Option<String> = conn
        .query_row(
            "SELECT goal_id FROM wc_agent_enrollment_goals WHERE agent_id=?1",
            [binding.agent_id()],
            |r| r.get(0),
        )
        .optional()
        .map_err(unavailable_enrollment)?;
    if previous
        .as_deref()
        .is_some_and(|previous| previous != goal_id)
    {
        return Err(AgentIsolationError::new(
            "agent_assignment_goal_already_bound",
        ));
    }
    conn.execute(
        "INSERT OR IGNORE INTO wc_agent_enrollment_goals(agent_id,goal_id) VALUES(?1,?2)",
        params![binding.agent_id(), goal_id],
    )
    .map_err(unavailable_enrollment)?;
    Ok(())
}

impl Database {
    /// Trusted product control can address an exact current manager run
    /// without knowing its anonymized host window. Never chooses a latest run.
    pub fn agent_enrollment_source_run(
        &self,
        principal: &str,
        owner_user: &str,
        project: &str,
        agent_id: &str,
        run_id: &str,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        self.agent_enrollment_source_run_for_purpose(
            principal,
            owner_user,
            project,
            agent_id,
            run_id,
            AgentEnrollmentPurpose::Fallback,
        )
    }

    pub fn agent_enrollment_source_run_for_purpose(
        &self,
        principal: &str,
        owner_user: &str,
        project: &str,
        agent_id: &str,
        run_id: &str,
        purpose: AgentEnrollmentPurpose,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(unavailable_enrollment)?;
        principal_valid(&tx, principal)?;
        let binding = verify_agent_run_context_on_connection(
            &tx,
            &host_goal_owner_key(project, agent_id),
            run_id,
        )?;
        if binding.owner_user() != owner_user || binding.agent_id() != agent_id {
            return Err(stale_source());
        }
        let registered: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE principal_key=?1 AND run_id=?2 AND enabled=1)",
            params![principal, run_id], |row| row.get(0),
        ).map_err(unavailable_enrollment)?;
        if !registered {
            return Err(stale_source());
        }
        let assigned = authority(&tx, &binding)?;
        authorize_agent_assignment_on_connection(
            &tx,
            &binding,
            &assigned.task_id,
            match purpose {
                AgentEnrollmentPurpose::Member => AgentAssignmentOperation::Delegate,
                AgentEnrollmentPurpose::CliWorker => AgentAssignmentOperation::UseCli,
                _ => AgentAssignmentOperation::PlanManage,
            },
        )?;
        Ok(binding)
    }

    pub fn agent_enrollment_task_directory(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<Option<String>, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(unavailable_enrollment)?;
        validate_agent_run_on_connection(&tx, binding)?;
        let Some(row) = meta(&tx, binding.agent_id())? else {
            return Ok(None);
        };
        if row.state != "claimed" || !assignment_active(&tx, binding.agent_id())? {
            return Err(denied());
        }
        Ok(Some(row.directory))
    }
    pub fn authorize_agent_goal_directory(
        &self,
        binding: &VerifiedAgentRun,
        directory: &str,
    ) -> Result<(), AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(unavailable_enrollment)?;
        validate_agent_run_on_connection(&tx, binding)?;
        let context = webcodex_core::host_goal::HostGoalInvocationContext {
            context_epoch: binding.run_id().into(),
            identity_source: "agent_run".into(),
        };
        authorize_agent_goal_creation_on_connection(
            &tx,
            &binding.host_goal_owner_key(),
            Some(&context),
            Some(directory),
        )
    }
}

#[cfg(test)]
#[path = "agent_enrollment_tests.rs"]
mod tests;

pub(crate) fn reserve_agent_enrollment_on_connection(
    conn: &Connection,
    request: NewAgentEnrollment,
    source: Option<&VerifiedAgentRun>,
    idempotency_key: &str,
    now: i64,
) -> Result<AgentEnrollmentIssued, AgentIsolationError> {
    issue_agent_enrollment_on_connection(conn, request, source, idempotency_key, now, true)
}

pub(crate) fn enrollment_projection_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
) -> Result<AgentEnrollmentProjection, AgentIsolationError> {
    validate_agent_run_on_connection(conn, binding)?;
    let enrolled = for_agent(conn, binding.agent_id())?.ok_or_else(denied)?;
    if enrolled.state != "claimed" {
        return Err(denied());
    }
    Ok(projection(&enrolled))
}

pub(crate) struct AgentEnrollmentDispatchProjection {
    pub projection: AgentEnrollmentProjection,
    pub state: String,
    pub principal_key: String,
}

pub(crate) fn validate_agent_enrollment_dispatch_on_connection(
    conn: &Connection,
    source: &VerifiedAgentRun,
    enrollment_id: &str,
    task_id: &str,
    purpose: AgentEnrollmentPurpose,
) -> Result<AgentEnrollmentDispatchProjection, AgentIsolationError> {
    let enrolled = row(conn, enrollment_id)?.ok_or_else(denied)?;
    if enrolled.source_agent_id.as_deref() != Some(source.agent_id())
        || enrolled.request.task_id != task_id
        || enrolled.request.purpose != purpose
        || enrolled.state == "revoked"
    {
        return Err(denied());
    }
    source_valid(conn, &enrolled.request, Some(source))?;
    crate::agent_isolation::ensure_agent_run_execution_open_on_connection(
        conn,
        &webcodex_core::runner_protocol::AgentRunRef {
            agent_id: source.agent_id().into(),
            agent_run_id: source.run_id().into(),
            generation: source.generation(),
        },
    )?;
    let state = conn
        .query_row(
            "SELECT delivery_state FROM wc_agent_enrollments WHERE enrollment_id=?1",
            [enrollment_id],
            |r| r.get(0),
        )
        .map_err(unavailable_enrollment)?;
    Ok(AgentEnrollmentDispatchProjection {
        projection: projection(&enrolled),
        state,
        principal_key: enrolled.request.principal_key,
    })
}

pub(crate) fn agent_enrollment_source_principal_on_connection(
    conn: &Connection,
    source: &VerifiedAgentRun,
) -> Result<String, AgentIsolationError> {
    validate_agent_run_on_connection(conn, source)?;
    let own = for_agent(conn, source.agent_id())?.ok_or_else(denied)?;
    if own.state != "claimed" {
        return Err(denied());
    }
    Ok(own.request.principal_key)
}

/// The queue observer must first persist a trusted exact-fence no-send result
/// in this same transaction. Ordinary Unknown or a terminal process result is
/// insufficient; this helper independently joins the original queue evidence.
pub(crate) fn reset_unstarted_agent_enrollment_delivery_on_connection(
    conn: &Connection,
    enrollment_id: &str,
    delivery_key: &str,
    _now: i64,
) -> Result<(), AgentIsolationError> {
    key(delivery_key, 128)?;
    let enrolled = row(conn, enrollment_id)?.ok_or_else(denied)?;
    if enrolled.state != "issued" || enrolled.window.is_some() {
        return Err(denied());
    }
    let delivery_hash = digest(
        b"webcodex.enrollment.delivery.v1\0",
        delivery_key.as_bytes(),
    );
    let eligible:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.run_id=e.run_id WHERE e.enrollment_id=?1 AND e.delivery_state='delivery_unknown' AND e.delivery_key_hash=?2 AND r.state='pending' AND r.previous_run_id IS NULL AND NOT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments a WHERE a.agent_id=e.agent_id) AND NOT EXISTS(SELECT 1 FROM wc_agent_host_windows w WHERE w.run_id=e.run_id))",params![enrollment_id,delivery_hash],|r|r.get(0)).map_err(unavailable_enrollment)?;
    if !eligible {
        return Err(denied());
    }
    let proof:Option<(String,String,String)>=conn.query_row("SELECT queue_kind,queue_id,no_start_code FROM wc_agent_host_dispatch_bindings WHERE enrollment_id=?1 AND delivery_attempt_id=?2 AND no_start_attempt_id=?2",params![enrollment_id,delivery_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(unavailable_enrollment)?;
    let (kind, queue, code) = proof.ok_or_else(denied)?;
    if code != "browser_worker_busy:launched=false"
        && !crate::team_agent::profile_unavailable_evidence(&code)
    {
        return Err(denied());
    }
    let principal = managed_user_principal(&enrolled.request.owner_user);
    let valid=match kind.as_str(){
        "member"=>conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_agent_attempts WHERE team_agent_attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3 AND project=?4 AND state IN ('queued','cancelled','confirmed_not_started') AND json_extract(record_json,'$.terminal_evidence')=?5 AND json_extract(record_json,'$.request_id') IS NULL AND json_extract(record_json,'$.response_observed')=0)",params![queue,principal.kind,principal.digest,enrolled.request.project,code],|r|r.get::<_,bool>(0)),
        "fallback"=>conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_cli_fallbacks WHERE fallback_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3 AND project=?4 AND state IN ('queued','cancelled','confirmed_not_started','blocked') AND terminal_evidence=?5 AND request_id IS NULL AND response_observed=0)",params![queue,principal.kind,principal.digest,enrolled.request.project,code],|r|r.get::<_,bool>(0)),
        _=>return Err(denied()),
    }.map_err(unavailable_enrollment)?;
    if !valid {
        return Err(denied());
    }
    conn.execute("UPDATE wc_agent_enrollments SET delivery_state='reserved',delivery_key_hash=NULL,delivery_secret_hash=NULL,delivery_expires_at_unix_ms=NULL,delivery_source_run_id=NULL,delivery_source_generation=NULL WHERE enrollment_id=?1 AND delivery_key_hash=?2",params![enrollment_id,delivery_hash]).map_err(unavailable_enrollment)?;
    audit(
        conn,
        "enrollment_no_send_reset",
        &enrolled.agent_id,
        &enrolled.run_id,
    )?;
    Ok(())
}

/// Called by a trusted launcher in its queue delivery transaction. The initial
/// creation source stays immutable; this delivery explicitly binds a current
/// run of that same logical manager, never an inferred latest run.
pub(crate) fn issue_delivery_grant_on_connection(
    conn: &Connection,
    source: &VerifiedAgentRun,
    enrollment_id: &str,
    delivery_key: &str,
    now: i64,
) -> Result<AgentEnrollmentIssued, AgentIsolationError> {
    key(delivery_key, 128)?;
    let enrolled = row(conn, enrollment_id)?.ok_or_else(denied)?;
    if enrolled.source_agent_id.as_deref() != Some(source.agent_id())
        || enrolled.request.purpose == AgentEnrollmentPurpose::CliWorker
    {
        return Err(denied());
    }
    source_valid(conn, &enrolled.request, Some(source))?;
    crate::agent_isolation::ensure_agent_run_execution_open_on_connection(
        conn,
        &webcodex_core::runner_protocol::AgentRunRef {
            agent_id: source.agent_id().into(),
            agent_run_id: source.run_id().into(),
            generation: source.generation(),
        },
    )?;
    require_enrollment_public_task_on_connection(
        conn,
        &enrolled.request.owner_user,
        &enrolled.request.project,
        &enrolled.request.task_id,
    )?;
    rooms_valid(conn, &enrolled.request)?;
    let (state,existing):(String,Option<String>)=conn.query_row("SELECT delivery_state,delivery_key_hash FROM wc_agent_enrollments WHERE enrollment_id=?1",[enrollment_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(unavailable_enrollment)?;
    let delivery_hash = digest(
        b"webcodex.enrollment.delivery.v1\0",
        delivery_key.as_bytes(),
    );
    if state != "reserved" {
        if existing.as_deref() != Some(&delivery_hash) || enrolled.state == "revoked" {
            return Err(denied());
        }
        return Ok(AgentEnrollmentIssued {
            enrollment_id: enrolled.id,
            agent_id: enrolled.agent_id,
            run_id: enrolled.run_id,
            expires_at_unix_ms: enrolled.request.expires_at_unix_ms,
            replayed: true,
            grant: None,
        });
    }
    if enrolled.state != "issued" {
        return Err(denied());
    }
    let secret = random_secret("wc_en_", b"webcodex.initial-enrollment.random.v1\0");
    let expiry = now.max(now_ms()).saturating_add(5 * 60 * 1000);
    conn.execute("UPDATE wc_agent_enrollments SET delivery_state='delivery_unknown',delivery_key_hash=?2,delivery_secret_hash=?3,delivery_expires_at_unix_ms=?4,delivery_source_run_id=?5,delivery_source_generation=?6 WHERE enrollment_id=?1 AND delivery_state='reserved'",params![enrollment_id,delivery_hash,secret_hash(&secret),expiry,source.run_id(),source.generation()]).map_err(unavailable_enrollment)?;
    // A never-admitted pending run may wait in a queue beyond its initial TTL.
    let changed=conn.execute("UPDATE wc_agent_runs SET expires_at_unix_ms=?2 WHERE run_id=?1 AND state='pending' AND previous_run_id IS NULL AND execution_close_id IS NULL",params![enrolled.run_id,now.max(now_ms()).saturating_add(AGENT_RUN_LIFETIME_MS)]).map_err(unavailable_enrollment)?;
    if changed != 1 {
        return Err(denied());
    }
    Ok(AgentEnrollmentIssued {
        enrollment_id: enrolled.id,
        agent_id: enrolled.agent_id,
        run_id: enrolled.run_id,
        expires_at_unix_ms: expiry,
        replayed: false,
        grant: Some(AgentEnrollmentGrant(secret)),
    })
}

impl Database {
    pub fn issue_agent_enrollment_delivery(
        &self,
        source: &VerifiedAgentRun,
        enrollment_id: &str,
        delivery_key: &str,
        now: i64,
    ) -> Result<AgentEnrollmentIssued, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_enrollment)?;
        let issued =
            issue_delivery_grant_on_connection(&tx, source, enrollment_id, delivery_key, now)?;
        tx.commit().map_err(unavailable_enrollment)?;
        Ok(issued)
    }
}

/// Non-browser initial activation. No host-window row or run bearer credential
/// is returned; the trusted controller keeps only this verified exact identity.
pub(crate) fn activate_cli_worker_on_connection(
    conn: &Connection,
    enrollment_id: &str,
    now: i64,
) -> Result<VerifiedAgentRun, AgentIsolationError> {
    let enrolled = row(conn, enrollment_id)?.ok_or_else(denied)?;
    if enrolled.request.purpose != AgentEnrollmentPurpose::CliWorker
        || enrolled.state != "issued"
        || enrolled.request.policy.max_subagents != 0
        || enrolled.request.policy.max_cli_calls != 0
    {
        return Err(denied());
    }
    let source = stored_source(conn, &enrolled)?.ok_or_else(denied)?;
    source_valid(conn, &enrolled.request, Some(&source))?;
    require_enrollment_public_task_on_connection(
        conn,
        &enrolled.request.owner_user,
        &enrolled.request.project,
        &enrolled.request.task_id,
    )?;
    rooms_valid(conn, &enrolled.request)?;
    let occupied:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments WHERE owner_user=?1 AND project=?2 AND task_id=?3 AND state='active')",params![enrolled.request.owner_user,enrolled.request.project,enrolled.request.task_id],|r|r.get(0)).map_err(unavailable_enrollment)?;
    if occupied {
        return Err(AgentIsolationError::new("agent_assignment_active"));
    }
    let changed=conn.execute("UPDATE wc_agent_runs SET state='active' WHERE run_id=?1 AND agent_id=?2 AND state='pending' AND previous_run_id IS NULL AND expires_at_unix_ms>?3",params![enrolled.run_id,enrolled.agent_id,now.max(now_ms())]).map_err(unavailable_enrollment)?;
    if changed != 1 {
        return Err(denied());
    }
    conn.execute("INSERT INTO wc_agent_enrollment_assignments(agent_id,owner_user,project,task_id,state) VALUES(?1,?2,?3,?4,'active')",params![enrolled.agent_id,enrolled.request.owner_user,enrolled.request.project,enrolled.request.task_id]).map_err(unavailable_enrollment)?;
    for room in &enrolled.request.room_ids {
        conn.execute(
            "INSERT INTO wc_agent_meeting_members(room_id,agent_id) VALUES(?1,?2)",
            params![room, enrolled.agent_id],
        )
        .map_err(unavailable_enrollment)?;
    }
    conn.execute("UPDATE wc_agent_enrollments SET state='claimed',claimed_at_unix_ms=?2,delivery_state='controller_activated' WHERE enrollment_id=?1",params![enrollment_id,now.max(now_ms())]).map_err(unavailable_enrollment)?;
    let binding = verify_agent_run_context_on_connection(
        conn,
        &host_goal_owner_key(&enrolled.request.project, &enrolled.agent_id),
        &enrolled.run_id,
    )?;
    audit(
        conn,
        "cli_worker_activate",
        binding.agent_id(),
        binding.run_id(),
    )?;
    Ok(binding)
}
pub(crate) fn issue_agent_enrollment_on_connection(
    conn: &Connection,
    request: NewAgentEnrollment,
    source: Option<&VerifiedAgentRun>,
    idempotency_key: &str,
    now: i64,
    deferred: bool,
) -> Result<AgentEnrollmentIssued, AgentIsolationError> {
    let started = Instant::now();
    key(idempotency_key, 128)?;
    let mut request = request;
    if deferred {
        request.expires_at_unix_ms =
            now_after(now, started).saturating_add(MAX_ENROLLMENT_LIFETIME_MS);
    }
    let mut request = normalize(request, now_after(now, started))?;
    if deferred {
        request.expires_at_unix_ms = 0;
    }
    let encoded = serde_json::to_string(&request).map_err(unavailable_enrollment)?;
    let intent = serde_json::to_vec(&(
        &request,
        source.map(|s| {
            (
                s.agent_id(),
                if deferred { "" } else { s.run_id() },
                if deferred { 0 } else { s.generation() },
            )
        }),
    ))
    .map_err(unavailable_enrollment)?;
    let request_hash = digest(b"webcodex.initial-enrollment.intent.v1\0", &intent);
    let idem = digest(
        b"webcodex.initial-enrollment.key.v1\0",
        idempotency_key.as_bytes(),
    );
    principal_valid(conn, &request.principal_key)?;
    if let Some(source) = source {
        crate::agent_isolation::ensure_agent_run_execution_open_on_connection(
            conn,
            &webcodex_core::runner_protocol::AgentRunRef {
                agent_id: source.agent_id().into(),
                agent_run_id: source.run_id().into(),
                generation: source.generation(),
            },
        )?;
    }
    require_enrollment_public_task_on_connection(
        conn,
        &request.owner_user,
        &request.project,
        &request.task_id,
    )?;
    source_valid(conn, &request, source)?;
    rooms_valid(conn, &request)?;
    let existing:Option<String>=conn.query_row("SELECT enrollment_id FROM wc_agent_enrollments WHERE owner_user=?1 AND project=?2 AND idempotency_hash=?3",params![request.owner_user,request.project,idem],|r|r.get(0)).optional().map_err(unavailable_enrollment)?;
    if let Some(id) = existing {
        let existing = row(conn, &id)?.ok_or_else(denied)?;
        if existing.request_hash != request_hash {
            return Err(AgentIsolationError::new(
                "agent_enrollment_idempotency_conflict",
            ));
        }
        if existing.state == "revoked"
            || (!deferred && now_after(now, started) >= request.expires_at_unix_ms)
        {
            return Err(denied());
        }
        return Ok(AgentEnrollmentIssued {
            enrollment_id: id,
            agent_id: existing.agent_id,
            run_id: existing.run_id,
            expires_at_unix_ms: request.expires_at_unix_ms,
            replayed: true,
            grant: None,
        });
    }
    if !deferred && now_after(now, started) >= request.expires_at_unix_ms {
        return Err(denied());
    }
    if request.purpose == AgentEnrollmentPurpose::Member {
        let source = source.ok_or_else(denied)?;
        let own = authority(conn, source)?;
        let count:i64=conn.query_row("SELECT COUNT(*) FROM wc_agent_enrollments WHERE source_agent_id=?1 AND purpose='member' AND state!='revoked'",[source.agent_id()],|r|r.get(0)).map_err(unavailable_enrollment)?;
        if count >= i64::from(own.policy.max_subagents) {
            return Err(AgentIsolationError::new("agent_member_budget_exhausted"));
        }
    }
    let agent = IsolatedAgentIdentity {
        agent_id: format!("wc_iagent_{}", uuid::Uuid::new_v4().simple()),
        owner_user: request.owner_user.clone(),
        project: request.project.clone(),
        parent_agent_id: source.map(|s| s.agent_id().into()),
        created_at_unix_ms: now_after(now, started),
    };
    conn.execute("INSERT INTO wc_isolated_agents(agent_id,owner_user,project,parent_agent_id,created_at_unix_ms) VALUES(?1,?2,?3,?4,?5)",params![agent.agent_id,agent.owner_user,agent.project,agent.parent_agent_id,agent.created_at_unix_ms]).map_err(unavailable_enrollment)?;
    if let Some(source) = source {
        crate::native_workspaces::inherit_team_target_on_connection(conn, source, &agent.agent_id)?;
    }
    let run = issue_run(conn, agent, 1, "pending", None)?;
    let secret = random_secret("wc_en_", b"webcodex.initial-enrollment.random.v1\0");
    let id = format!("wc_enrollment_{}", uuid::Uuid::new_v4().simple());
    let purpose = match request.purpose {
        AgentEnrollmentPurpose::Manager => "manager",
        AgentEnrollmentPurpose::Member => "member",
        AgentEnrollmentPurpose::Fallback => "fallback",
        AgentEnrollmentPurpose::CliWorker => "cli_worker",
    };
    conn.execute("INSERT INTO wc_agent_enrollments(enrollment_id,owner_user,project,principal_key,agent_id,run_id,purpose,task_id,task_directory,request_json,request_hash,idempotency_hash,secret_hash,source_agent_id,source_run_id,source_generation,expires_at_unix_ms,state,created_at_unix_ms,policy_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,'issued',?18,?19)",params![id,request.owner_user,request.project,request.principal_key,run.agent.agent_id,run.run_id,purpose,request.task_id,request.task_directory,encoded,request_hash,idem,secret_hash(&secret),source.map(|s|s.agent_id()),source.map(|s|s.run_id()),source.map(|s|s.generation()),request.expires_at_unix_ms,now_after(now,started),serde_json::to_string(&request.policy).map_err(unavailable_enrollment)?]).map_err(unavailable_enrollment)?;
    if deferred {
        conn.execute(
            "UPDATE wc_agent_enrollments SET delivery_state='reserved' WHERE enrollment_id=?1",
            [&id],
        )
        .map_err(unavailable_enrollment)?;
    }
    audit(conn, "enrollment_issue", &run.agent.agent_id, &run.run_id)?;
    if !deferred && now_after(now, started) >= request.expires_at_unix_ms {
        return Err(denied());
    }

    // run.token is deliberately dropped here. Only a bound host, never
    // possession of this claim secret, can acquire the run's authority.
    Ok(AgentEnrollmentIssued {
        enrollment_id: id,
        agent_id: run.agent.agent_id,
        run_id: run.run_id,
        expires_at_unix_ms: request.expires_at_unix_ms,
        replayed: false,
        grant: if deferred {
            None
        } else {
            Some(AgentEnrollmentGrant(secret))
        },
    })
}

pub(crate) fn claim_agent_enrollment_on_connection(
    conn: &Connection,
    principal: &str,
    window: &str,
    enrollment_id: &str,
    claim_secret: &str,
    now: i64,
) -> Result<AgentEnrollmentClaim, AgentIsolationError> {
    let started = Instant::now();
    key(principal, 512)?;
    key(window, 512)?;
    key(enrollment_id, 128)?;
    if !valid_secret(claim_secret) {
        return Err(denied());
    }
    principal_valid(conn, principal)?;
    let authorized:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments WHERE enrollment_id=?1 AND principal_key=?2 AND COALESCE(delivery_secret_hash,secret_hash)=?3 AND delivery_state!='reserved')",params![enrollment_id,principal,secret_hash(claim_secret)],|r|r.get(0)).map_err(unavailable_enrollment)?;
    if !authorized {
        return Err(denied());
    }
    let enrolled = row(conn, enrollment_id)?.ok_or_else(denied)?;
    // CLI workers are activated by the trusted execution controller while
    // freezing the original CLI attempt binding, never by a chat window.
    if enrolled.request.purpose == AgentEnrollmentPurpose::CliWorker {
        return Err(AgentIsolationError::new(
            "agent_cli_worker_controller_required",
        ));
    }
    if enrolled.request.principal_key != principal
        || enrolled.secret_hash != secret_hash(claim_secret)
        || enrolled.state == "revoked"
        || now_after(now, started) >= enrolled.request.expires_at_unix_ms
    {
        return Err(denied());
    }
    require_enrollment_public_task_on_connection(
        conn,
        &enrolled.request.owner_user,
        &enrolled.request.project,
        &enrolled.request.task_id,
    )?;
    if enrolled.state == "claimed" {
        let exact:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE principal_key=?1 AND window_key=?2 AND run_id=?3 AND enabled=1)",params![principal,window,enrolled.run_id],|r|r.get(0)).map_err(unavailable_enrollment)?;
        if enrolled.window.as_deref() != Some(window) || !exact {
            return Err(denied());
        }
        let binding = verify_agent_run_context_on_connection(
            conn,
            &host_goal_owner_key(&enrolled.request.project, &enrolled.agent_id),
            &enrolled.run_id,
        )?;
        return Ok(AgentEnrollmentClaim {
            binding,
            projection: projection(&enrolled),
            replayed: true,
        });
    }
    let source = stored_source(conn, &enrolled)?;
    if let Some(source) = &source {
        crate::agent_isolation::ensure_agent_run_execution_open_on_connection(
            conn,
            &webcodex_core::runner_protocol::AgentRunRef {
                agent_id: source.agent_id().into(),
                agent_run_id: source.run_id().into(),
                generation: source.generation(),
            },
        )?;
    }
    source_valid(conn, &enrolled.request, source.as_ref())?;
    rooms_valid(conn, &enrolled.request)?;
    crate::agent_host_dispatch::validate_host_enrollment_claim_on_connection(conn, &enrolled.id)?;
    let used: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE window_key=?1)",
            [window],
            |r| r.get(0),
        )
        .map_err(unavailable_enrollment)?;
    if used {
        return Err(denied());
    }
    let pending:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2 AND state='pending' AND previous_run_id IS NULL AND expires_at_unix_ms>?3)",params![enrolled.run_id,enrolled.agent_id,now_after(now,started)],|r|r.get(0)).map_err(unavailable_enrollment)?;
    crate::agent_isolation::ensure_agent_run_execution_open_on_connection(
        conn,
        &webcodex_core::runner_protocol::AgentRunRef {
            agent_id: enrolled.agent_id.clone(),
            agent_run_id: enrolled.run_id.clone(),
            generation: 1,
        },
    )?;
    if !pending {
        return Err(denied());
    }
    let occupied:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments WHERE owner_user=?1 AND project=?2 AND task_id=?3 AND state='active')",params![enrolled.request.owner_user,enrolled.request.project,enrolled.request.task_id],|r|r.get(0)).map_err(unavailable_enrollment)?;
    if occupied {
        return Err(AgentIsolationError::new("agent_assignment_active"));
    }
    conn.execute("INSERT INTO wc_agent_enrollment_assignments(agent_id,owner_user,project,task_id,state) VALUES(?1,?2,?3,?4,'active')",params![enrolled.agent_id,enrolled.request.owner_user,enrolled.request.project,enrolled.request.task_id]).map_err(unavailable_enrollment)?;
    for room in &enrolled.request.room_ids {
        conn.execute(
            "INSERT INTO wc_agent_meeting_members(room_id,agent_id) VALUES(?1,?2)",
            params![room, enrolled.agent_id],
        )
        .map_err(unavailable_enrollment)?;
    }
    conn.execute("INSERT INTO wc_agent_host_windows(principal_key,window_key,run_id,enabled) VALUES(?1,?2,?3,1)",params![principal,window,enrolled.run_id]).map_err(unavailable_enrollment)?;
    conn.execute(
        "UPDATE wc_agent_runs SET state='active' WHERE run_id=?1 AND state='pending'",
        [&enrolled.run_id],
    )
    .map_err(unavailable_enrollment)?;
    conn.execute("UPDATE wc_agent_enrollments SET state='claimed',window_key=?2,claimed_at_unix_ms=?3 WHERE enrollment_id=?1 AND state='issued'",params![enrolled.id,window,now_after(now,started)]).map_err(unavailable_enrollment)?;
    let binding = verify_agent_run_context_on_connection(
        conn,
        &host_goal_owner_key(&enrolled.request.project, &enrolled.agent_id),
        &enrolled.run_id,
    )?;
    audit(
        conn,
        "enrollment_claim",
        binding.agent_id(),
        binding.run_id(),
    )?;
    if now_after(now, started) >= enrolled.request.expires_at_unix_ms {
        return Err(denied());
    }
    let projection = projection(&enrolled);

    Ok(AgentEnrollmentClaim {
        binding,
        projection,
        replayed: false,
    })
}
