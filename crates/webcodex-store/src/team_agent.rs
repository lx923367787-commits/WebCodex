//! Durable ordinary subagent admission from an already frozen team plan.
use crate::communication::{
    digest_text, now_unix_ms, store_error, validate_communication_principal,
    validate_idempotency_key,
};
use crate::{
    CommunicationPrincipal, CommunicationStoreError, Database, TeamPlanRecord, TeamPlanStatus,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

type Result<T> = std::result::Result<T, CommunicationStoreError>;
const MAX_ATTEMPTS: i64 = 65536;
const MAX_BUSY_DISPATCHES: u32 = 4;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamAgentState {
    Queued,
    Starting,
    Running,
    Unknown,
    Cancelled,
    ConfirmedNotStarted,
    VerifiedTerminal,
}
impl TeamAgentState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Unknown => "unknown",
            Self::Cancelled => "cancelled",
            Self::ConfirmedNotStarted => "confirmed_not_started",
            Self::VerifiedTerminal => "verified_terminal",
        }
    }
    pub const fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Unknown)
    }
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::ConfirmedNotStarted | Self::VerifiedTerminal
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamAgentRecord {
    pub team_agent_attempt_id: String,
    pub project: String,
    pub root_task_id: String,
    pub source_task_id: String,
    pub instruction: String,
    pub thinking_profile_id: String,
    pub state: TeamAgentState,
    pub dispatch_attempt_id: Option<String>,
    pub dispatch_count: u32,
    pub request_id: Option<String>,
    pub cancel_requested: bool,
    pub response_observed: bool,
    pub terminal_evidence: Option<String>,
    pub revision: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamAgentReservation {
    pub record: TeamAgentRecord,
    pub created: bool,
    pub replayed: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct TeamAgentClaim {
    pub record: TeamAgentRecord,
    pub dispatch_permitted: bool,
    pub task: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamAgentPage {
    pub records: Vec<TeamAgentRecord>,
    pub next_cursor: Option<String>,
}

/// Query position only. Each page independently applies its principal/scope
/// filter; possessing or editing this cursor never grants execution authority.
/// These append-only record tables preserve rowids across ordinary DB reopen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateScanCursor {
    scope: String,
    pub(crate) after_rowid: i64,
    pub(crate) through_rowid: i64,
}

impl CandidateScanCursor {
    pub(crate) fn new(scope: String, through_rowid: i64) -> Self {
        Self {
            scope,
            after_rowid: 0,
            through_rowid,
        }
    }

    pub(crate) fn parse(value: &str, scope: &str) -> Option<Self> {
        if value.len() > 512 {
            return None;
        }
        let parsed: Self = serde_json::from_str(value).ok()?;
        (parsed.scope == scope
            && parsed.after_rowid > 0
            && parsed.after_rowid < parsed.through_rowid)
            .then_some(parsed)
    }

    pub(crate) fn next(&self, after_rowid: i64) -> serde_json::Result<String> {
        serde_json::to_string(&Self {
            after_rowid,
            ..self.clone()
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TeamAgentObservation {
    Dispatched {
        request_id: String,
        response_observed: bool,
    },
    Unknown {
        request_id: Option<String>,
    },
    CancelRequested {
        request_id: Option<String>,
    },
    Busy {
        evidence: String,
    },
    /// Trusted Service preflight proved no request was created for this send.
    ProfileUnavailable {
        evidence: String,
    },
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamAgentResolution {
    ConfirmedNotStarted,
    VerifiedTerminal,
}

pub(crate) fn init_team_agent_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_team_agent_attempts (
        team_agent_attempt_id TEXT PRIMARY KEY,
        root_task_id TEXT NOT NULL REFERENCES wc_team_plans(root_task_id),
        source_task_id TEXT NOT NULL REFERENCES wc_agent_tasks(task_id),
        project TEXT NOT NULL, owner_principal_kind TEXT NOT NULL, owner_principal_digest TEXT NOT NULL,
        request_key_hash TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('queued','starting','running','unknown','cancelled','confirmed_not_started','verified_terminal')),
        record_json TEXT NOT NULL, created_at_unix_ms INTEGER NOT NULL,
        UNIQUE(root_task_id,source_task_id),
        UNIQUE(owner_principal_kind,owner_principal_digest,request_key_hash));
        CREATE INDEX IF NOT EXISTS idx_team_agent_scope ON wc_team_agent_attempts(owner_principal_kind,owner_principal_digest,project,state,created_at_unix_ms);
        CREATE INDEX IF NOT EXISTS idx_team_agent_candidate_scan ON wc_team_agent_attempts(owner_principal_kind,owner_principal_digest,project);")?;
    crate::agent_host_dispatch::init_agent_host_dispatch_tables(conn)?;
    Ok(())
}

fn error(code: &'static str) -> CommunicationStoreError {
    CommunicationStoreError::new(code, code)
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn evidence(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(error("team_agent_invalid_evidence"));
    }
    Ok(())
}

pub(crate) fn profile_unavailable_evidence(value: &str) -> bool {
    matches!(
        value,
        "discover_thinking_profiles_first"
            | "thinking_profiles_expired_refresh_required"
            | "thinking_profile_not_discovered"
    )
}
fn encode(record: &TeamAgentRecord) -> Result<String> {
    serde_json::to_string(record).map_err(|_| error("team_agent_store_invalid"))
}

struct Source {
    instruction: String,
    live: bool,
}
fn source_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    root: &str,
    source: &str,
) -> Result<Source> {
    validate_communication_principal(principal)?;
    if !identifier(root)
        || !identifier(source)
        || root == source
        || project.len() > 256
        || !(project.starts_with("agent:")
            || crate::native_workspaces::is_native_workspace_id(project))
        || project.chars().any(char::is_control)
    {
        return Err(error("team_agent_invalid_source"));
    }
    let encoded: String = conn.query_row("SELECT record_json FROM wc_team_plans WHERE root_task_id=?1 AND project_id=?2 AND owner_principal_kind=?3 AND owner_principal_digest=?4",params![root,project,principal.kind,principal.digest],|r|r.get(0)).optional().map_err(store_error)?.ok_or_else(||error("team_agent_source_unavailable"))?;
    let plan: TeamPlanRecord =
        serde_json::from_str(&encoded).map_err(|_| error("team_agent_store_invalid"))?;
    if plan.root_task_id != root
        || plan.project_id != project
        || plan.plan.manager.task_id != root
        || plan.cancel_requested != (plan.status == TeamPlanStatus::Cancelled)
        || plan
            .plan
            .subagents
            .iter()
            .filter(|member| member.task_id == source)
            .count()
            != 1
    {
        return Err(error("team_agent_source_unavailable"));
    }
    let owned = |id: &str| -> Result<(String, String)> {
        conn.query_row("SELECT instruction,state FROM wc_agent_tasks WHERE task_id=?1 AND referenced_project_id=?2 AND owner_principal_kind=?3 AND owner_principal_digest=?4",params![id,project,principal.kind,principal.digest],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?.ok_or_else(||error("team_agent_source_unavailable"))
    };
    let (_, root_state) = owned(root)?;
    let (instruction, source_state) = owned(source)?;
    for state in [&root_state, &source_state] {
        if !matches!(state.as_str(), "ready" | "active" | "succeeded" | "failed") {
            return Err(error("team_agent_store_invalid"));
        }
    }
    Ok(Source {
        instruction,
        live: !plan.cancel_requested
            && matches!(root_state.as_str(), "ready" | "active")
            && matches!(source_state.as_str(), "ready" | "active"),
    })
}

pub(crate) fn team_agent_record_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    id: &str,
) -> Result<TeamAgentRecord> {
    validate_communication_principal(principal)?;
    let row:(String,String,String,String,String)=conn.query_row("SELECT root_task_id,source_task_id,project,state,record_json FROM wc_team_agent_attempts WHERE team_agent_attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3",params![id,principal.kind,principal.digest],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(store_error)?.ok_or_else(||error("team_agent_unavailable"))?;
    let record: TeamAgentRecord =
        serde_json::from_str(&row.4).map_err(|_| error("team_agent_store_invalid"))?;
    if record.team_agent_attempt_id != id
        || record.root_task_id != row.0
        || record.source_task_id != row.1
        || record.project != row.2
        || record.state.as_str() != row.3
        || record.revision == 0
        || record.dispatch_count > MAX_BUSY_DISPATCHES
        || (record.state.active() && record.dispatch_attempt_id.is_none())
    {
        return Err(error("team_agent_store_invalid"));
    }
    source_on(
        conn,
        principal,
        &record.project,
        &record.root_task_id,
        &record.source_task_id,
    )?;
    Ok(record)
}
fn save(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    record: &mut TeamAgentRecord,
) -> Result<()> {
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| error("team_agent_store_invalid"))?;
    record.updated_at_unix_ms = now_unix_ms();
    conn.execute("UPDATE wc_team_agent_attempts SET state=?4,record_json=?5 WHERE team_agent_attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3",params![record.team_agent_attempt_id,principal.kind,principal.digest,record.state.as_str(),encode(record)?]).map_err(store_error)?;
    Ok(())
}
fn intent(
    project: &str,
    root: &str,
    source: &str,
    key: &str,
    profile: &str,
) -> Result<(String, String)> {
    let key = validate_idempotency_key(key)?;
    if !identifier(profile) {
        return Err(error("team_agent_profile_required"));
    }
    let serialized = serde_json::to_string(&(project, root, source, profile))
        .map_err(|_| error("team_agent_invalid_request"))?;
    Ok((
        digest_text("webcodex.team.agent.key.v1", &key),
        digest_text("webcodex.team.agent.intent.v1", &serialized),
    ))
}
fn replay_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    key: &str,
    fingerprint: &str,
) -> Result<Option<TeamAgentRecord>> {
    let row:Option<(String,String)>=conn.query_row("SELECT team_agent_attempt_id,request_fingerprint FROM wc_team_agent_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND request_key_hash=?3",params![principal.kind,principal.digest,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?;
    let Some((id, saved)) = row else {
        return Ok(None);
    };
    if saved != fingerprint {
        return Err(error("team_agent_idempotency_conflict"));
    }
    team_agent_record_on_connection(conn, principal, &id).map(Some)
}
fn claim(record: TeamAgentRecord, dispatch_permitted: bool) -> TeamAgentClaim {
    let task=format!("You are the ordinary subagent assigned this frozen team-plan task. The manager is the existing root task, not a new manager. IDs are correlation, not authority. Follow the task's existing permissions. Route every external Agent CLI call through team_cli_start for this root/source and a planned call_index; do not bypass its budget by launching an Agent CLI through a shell. Do not create unplanned agents. Verify the task's acceptance conditions and report evidence; a response is not business verification.\nproject={}\nroot_task_id={}\nsource_task_id={}\nTask instruction:\n{}",record.project,record.root_task_id,record.source_task_id,record.instruction);
    TeamAgentClaim {
        record,
        dispatch_permitted,
        task,
    }
}

pub(crate) fn reserve_team_agent_attempt_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    root: &str,
    source: &str,
    key: &str,
    profile: &str,
) -> Result<TeamAgentReservation> {
    let (key, fingerprint) = intent(project, root, source, key, profile)?;
    let task = source_on(conn, principal, project, root, source)?;
    if let Some(record) = replay_on(conn, principal, &key, &fingerprint)? {
        return Ok(TeamAgentReservation {
            record,
            created: false,
            replayed: true,
        });
    }
    if !task.live {
        return Err(error("team_agent_source_not_live"));
    }
    let occupied:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_agent_attempts WHERE root_task_id=?1 AND source_task_id=?2)",params![root,source],|r|r.get(0)).map_err(store_error)?;
    if occupied {
        return Err(error("team_agent_already_reserved"));
    }
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM wc_team_agent_attempts", [], |r| {
            r.get(0)
        })
        .map_err(store_error)?;
    if count >= MAX_ATTEMPTS {
        return Err(error("team_agent_store_full"));
    }
    let now = now_unix_ms();
    let record = TeamAgentRecord {
        team_agent_attempt_id: format!("wc_team_agent_attempt_{}", Uuid::new_v4().simple()),
        project: project.into(),
        root_task_id: root.into(),
        source_task_id: source.into(),
        instruction: task.instruction,
        thinking_profile_id: profile.into(),
        state: TeamAgentState::Queued,
        dispatch_attempt_id: None,
        dispatch_count: 0,
        request_id: None,
        cancel_requested: false,
        response_observed: false,
        terminal_evidence: None,
        revision: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
    };
    conn.execute(
        "INSERT INTO wc_team_agent_attempts VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            record.team_agent_attempt_id,
            root,
            source,
            project,
            principal.kind,
            principal.digest,
            key,
            fingerprint,
            record.state.as_str(),
            encode(&record)?,
            now
        ],
    )
    .map_err(store_error)?;
    Ok(TeamAgentReservation {
        record,
        created: true,
        replayed: false,
    })
}

pub(crate) fn claim_team_agent_attempt_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    id: &str,
) -> Result<TeamAgentClaim> {
    let mut record = team_agent_record_on_connection(conn, principal, id)?;
    let source = source_on(
        conn,
        principal,
        &record.project,
        &record.root_task_id,
        &record.source_task_id,
    )?;
    if !source.live && !record.state.terminal() && !record.cancel_requested {
        record.cancel_requested = true;
        if record.state == TeamAgentState::Queued {
            record.state = TeamAgentState::Cancelled;
        }
        save(conn, principal, &mut record)?;
    }
    let dispatch_permitted = record.state == TeamAgentState::Queued && !record.cancel_requested;
    if dispatch_permitted {
        record.state = TeamAgentState::Starting;
        record.dispatch_attempt_id = Some(Uuid::new_v4().to_string());
        record.dispatch_count += 1;
        record.terminal_evidence = None;
        save(conn, principal, &mut record)?;
    }
    Ok(claim(record, dispatch_permitted))
}

pub(crate) fn team_agent_can_deliver_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    id: &str,
) -> Result<bool> {
    let record = team_agent_record_on_connection(conn, principal, id)?;
    Ok(source_on(
        conn,
        principal,
        &record.project,
        &record.root_task_id,
        &record.source_task_id,
    )?
    .live)
}

pub(crate) fn lookup_team_agent_attempt_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    root: &str,
    source: &str,
    key: &str,
    profile: &str,
) -> Result<Option<TeamAgentRecord>> {
    let (key, fingerprint) = intent(project, root, source, key, profile)?;
    source_on(conn, principal, project, root, source)?;
    replay_on(conn, principal, &key, &fingerprint)
}

impl Database {
    pub fn lookup_team_agent_attempt(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root: &str,
        source: &str,
        key: &str,
        profile: &str,
    ) -> Result<Option<TeamAgentRecord>> {
        lookup_team_agent_attempt_on_connection(
            &self.conn.lock().unwrap(),
            principal,
            project,
            root,
            source,
            key,
            profile,
        )
    }
    pub fn reserve_team_agent_attempt(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root: &str,
        source: &str,
        key: &str,
        profile: &str,
    ) -> Result<TeamAgentReservation> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let result = reserve_team_agent_attempt_on_connection(
            &tx, principal, project, root, source, key, profile,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(result)
    }
    pub fn read_team_agent_attempt(
        &self,
        principal: &CommunicationPrincipal,
        id: &str,
    ) -> Result<TeamAgentRecord> {
        team_agent_record_on_connection(&self.conn.lock().unwrap(), principal, id)
    }
    pub fn list_team_agent_attempts(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root: Option<&str>,
        active_only: bool,
        limit: usize,
    ) -> Result<Vec<TeamAgentRecord>> {
        Ok(self
            .list_team_agent_attempts_page(principal, project, root, active_only, limit, None)?
            .records)
    }

    /// Visit each eligible record at most once per captured high-water cycle.
    /// A caller persists `next_cursor` after processing a page and starts a new
    /// cycle with None only when the current page returns no continuation.
    pub fn list_team_agent_attempts_page(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root: Option<&str>,
        active_only: bool,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<TeamAgentPage> {
        validate_communication_principal(principal)?;
        if !(1..=100).contains(&limit) {
            return Err(error("team_agent_invalid_limit"));
        }
        let conn = self.conn.lock().unwrap();
        let scope = digest_text(
            "webcodex.team-agent.scan.v1",
            &serde_json::to_string(&(
                &principal.kind,
                &principal.digest,
                project,
                root,
                active_only,
            ))
            .map_err(|_| error("team_agent_invalid_cursor"))?,
        );
        let cursor = match cursor {
            Some(value) => CandidateScanCursor::parse(value, &scope)
                .ok_or_else(|| error("team_agent_invalid_cursor"))?,
            None => CandidateScanCursor::new(scope, conn.query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM wc_team_agent_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project=?3 AND (?4 IS NULL OR root_task_id=?4) AND (?5=0 OR state IN ('queued','starting','running','unknown'))",
                params![principal.kind, principal.digest, project, root, active_only], |row| row.get(0)).map_err(store_error)?),
        };
        let mut stmt=conn.prepare("SELECT rowid,team_agent_attempt_id FROM wc_team_agent_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project=?3 AND (?4 IS NULL OR root_task_id=?4) AND (?5=0 OR state IN ('queued','starting','running','unknown')) AND rowid>?6 AND rowid<=?7 ORDER BY rowid LIMIT ?8").map_err(store_error)?;
        let mut ids = stmt
            .query_map(
                params![
                    principal.kind,
                    principal.digest,
                    project,
                    root,
                    active_only,
                    cursor.after_rowid,
                    cursor.through_rowid,
                    (limit + 1) as i64
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(store_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(store_error)?;
        let next_cursor = if ids.len() > limit {
            ids.truncate(limit);
            Some(
                cursor
                    .next(ids[limit - 1].0)
                    .map_err(|_| error("team_agent_invalid_cursor"))?,
            )
        } else {
            None
        };
        let records = ids
            .into_iter()
            .map(|(_, id)| team_agent_record_on_connection(&conn, principal, &id))
            .collect::<Result<Vec<_>>>()?;
        Ok(TeamAgentPage {
            records,
            next_cursor,
        })
    }
    pub fn claim_team_agent_attempt(
        &self,
        principal: &CommunicationPrincipal,
        id: &str,
    ) -> Result<TeamAgentClaim> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let record = team_agent_record_on_connection(&tx, principal, id)?;
        crate::agent_host_dispatch::require_legacy_host_queue_claim_on_connection(
            &tx,
            "member",
            id,
            &record.source_task_id,
        )
        .map_err(|e| error(e.code()))?;
        let result = claim_team_agent_attempt_on_connection(&tx, principal, id)?;
        tx.commit().map_err(store_error)?;
        Ok(result)
    }
    pub fn record_team_agent_observation(
        &self,
        principal: &CommunicationPrincipal,
        id: &str,
        fence: &str,
        observation: TeamAgentObservation,
    ) -> Result<TeamAgentRecord> {
        let no_start = match &observation {
            TeamAgentObservation::Busy { evidence }
            | TeamAgentObservation::ProfileUnavailable { evidence } => Some(evidence.clone()),
            _ => None,
        };
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let mut record = team_agent_record_on_connection(&tx, principal, id)?;
        if record.dispatch_attempt_id.as_deref() != Some(fence) {
            return Err(error("team_agent_stale_dispatch"));
        }
        if !record.state.active() {
            return Err(error("team_agent_not_active"));
        }
        let request = match &observation {
            TeamAgentObservation::Dispatched { request_id, .. } => Some(request_id),
            TeamAgentObservation::Unknown { request_id }
            | TeamAgentObservation::CancelRequested { request_id } => request_id.as_ref(),
            TeamAgentObservation::Busy { .. } | TeamAgentObservation::ProfileUnavailable { .. } => {
                None
            }
        };
        if let Some(request) = request {
            let parsed =
                Uuid::parse_str(request).map_err(|_| error("team_agent_invalid_request_id"))?;
            if parsed.to_string() != *request
                || record
                    .request_id
                    .as_ref()
                    .is_some_and(|saved| saved != request)
            {
                return Err(error("team_agent_request_conflict"));
            }
            record.request_id = Some(request.clone());
        }
        match observation {
            TeamAgentObservation::Dispatched {
                response_observed, ..
            } => {
                record.state = TeamAgentState::Running;
                record.response_observed |= response_observed;
            }
            TeamAgentObservation::Unknown { .. } => record.state = TeamAgentState::Unknown,
            TeamAgentObservation::CancelRequested { .. } => record.cancel_requested = true,
            TeamAgentObservation::Busy { evidence: proof } => {
                if proof != "browser_worker_busy:launched=false"
                    || record.state != TeamAgentState::Starting
                    || record.request_id.is_some()
                    || record.response_observed
                {
                    return Err(error("team_agent_not_proven_unstarted"));
                }
                record.terminal_evidence = Some(proof);
                if record.cancel_requested {
                    record.state = TeamAgentState::Cancelled;
                } else if record.dispatch_count >= MAX_BUSY_DISPATCHES {
                    record.state = TeamAgentState::ConfirmedNotStarted;
                } else {
                    record.state = TeamAgentState::Queued;
                    record.dispatch_attempt_id = None;
                }
            }
            TeamAgentObservation::ProfileUnavailable { evidence: proof } => {
                if !profile_unavailable_evidence(&proof)
                    || record.state != TeamAgentState::Starting
                    || record.request_id.is_some()
                    || record.response_observed
                    || record.dispatch_count == 0
                {
                    return Err(error("team_agent_not_proven_unstarted"));
                }
                record.state = if record.cancel_requested {
                    TeamAgentState::Cancelled
                } else {
                    TeamAgentState::Queued
                };
                record.dispatch_attempt_id = None;
                record.dispatch_count -= 1;
                record.terminal_evidence = Some(proof);
            }
        }
        let source = source_on(
            &tx,
            principal,
            &record.project,
            &record.root_task_id,
            &record.source_task_id,
        )?;
        if !source.live {
            record.cancel_requested = true;
            if record.state == TeamAgentState::Queued {
                record.state = TeamAgentState::Cancelled;
            }
        }
        save(&tx, principal, &mut record)?;
        if let Some(code) = no_start.as_deref() {
            crate::agent_host_dispatch::record_host_queue_no_start_on_connection(
                &tx, "member", id, fence, code,
            )
            .map_err(|e| error(e.code()))?;
        }
        tx.commit().map_err(store_error)?;
        Ok(record)
    }
    pub fn resolve_team_agent_attempt(
        &self,
        principal: &CommunicationPrincipal,
        id: &str,
        fence: &str,
        resolution: TeamAgentResolution,
        proof: &str,
    ) -> Result<TeamAgentRecord> {
        evidence(proof)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let mut record = team_agent_record_on_connection(&tx, principal, id)?;
        if record.dispatch_attempt_id.as_deref() != Some(fence) {
            return Err(error("team_agent_stale_dispatch"));
        }
        let state = match resolution {
            TeamAgentResolution::ConfirmedNotStarted => TeamAgentState::ConfirmedNotStarted,
            TeamAgentResolution::VerifiedTerminal => TeamAgentState::VerifiedTerminal,
        };
        if record.state == state && record.terminal_evidence.as_deref() == Some(proof) {
            return Ok(record);
        }
        if !record.state.active()
            || (state == TeamAgentState::ConfirmedNotStarted
                && (record.request_id.is_some() || record.response_observed))
            || (state == TeamAgentState::VerifiedTerminal && record.request_id.is_none())
        {
            return Err(error("team_agent_resolution_rejected"));
        }
        record.state = state;
        record.terminal_evidence = Some(proof.into());
        save(&tx, principal, &mut record)?;
        crate::agent_host_dispatch::record_host_queue_resolution_on_connection(
            &tx,
            "member",
            id,
            fence,
            state == TeamAgentState::VerifiedTerminal,
            proof,
        )
        .map_err(|e| error(e.code()))?;
        tx.commit().map_err(store_error)?;
        Ok(record)
    }
    pub fn cancel_root_team_agents(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root: &str,
    ) -> Result<Vec<TeamAgentRecord>> {
        validate_communication_principal(principal)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let owned:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_plans WHERE root_task_id=?1 AND project_id=?2 AND owner_principal_kind=?3 AND owner_principal_digest=?4)",params![root,project,principal.kind,principal.digest],|r|r.get(0)).map_err(store_error)?;
        if !owned {
            return Err(error("team_agent_source_unavailable"));
        }
        let ids = {
            let mut stmt=tx.prepare("SELECT team_agent_attempt_id FROM wc_team_agent_attempts WHERE root_task_id=?1 AND project=?2 AND owner_principal_kind=?3 AND owner_principal_digest=?4 LIMIT 33").map_err(store_error)?;
            let rows = stmt
                .query_map(
                    params![root, project, principal.kind, principal.digest],
                    |r| r.get::<_, String>(0),
                )
                .map_err(store_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(store_error)?;
            rows
        };
        let mut result = Vec::new();
        for id in ids {
            let mut record = team_agent_record_on_connection(&tx, principal, &id)?;
            if !record.state.terminal() {
                record.cancel_requested = true;
                if record.state == TeamAgentState::Queued {
                    record.state = TeamAgentState::Cancelled;
                }
                save(&tx, principal, &mut record)?;
            }
            result.push(record);
        }
        tx.commit().map_err(store_error)?;
        Ok(result)
    }
}

#[cfg(test)]
#[path = "team_agent_tests.rs"]
mod tests;
