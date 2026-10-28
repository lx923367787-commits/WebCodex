//! Explicit trusted-control OAuth host bindings. Neither host metadata nor an
//! attempt UUID alone can register a candidate or grant run authentication.
use super::*;
use crate::{AgentIsolationError, VerifiedAgentRun};

/// A one-time narrow invitation, never a run credential or execution grant.
/// Only the trusted launch controller deliberately exposes it to its target.
pub struct AgentHandoffClaimGrant(String);
impl AgentHandoffClaimGrant {
    pub fn into_secret(self) -> String {
        self.0
    }
}
impl std::fmt::Debug for AgentHandoffClaimGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentHandoffClaimGrant([REDACTED])")
    }
}

fn claim_digest(grant: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"webcodex.host-handoff.claim-hash.v1\0");
    digest.update(grant.as_bytes());
    format!("{:x}", digest.finalize())
}

fn valid_claim(grant: &str) -> bool {
    grant.strip_prefix("wc_hc_").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

#[cfg(test)]
#[path = "agent_host_window_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentHostWindowBinding {
    Active(VerifiedAgentRun),
    Pending {
        source: VerifiedAgentRun,
        pending_run_id: String,
        goal_id: String,
        handoff_id: String,
        attempt_id: String,
    },
}

fn unavailable_host(_: impl std::fmt::Display) -> AgentIsolationError {
    AgentIsolationError::new("agent_host_binding_unavailable")
}
fn denied_host() -> AgentIsolationError {
    AgentIsolationError::new("agent_host_binding_denied")
}
fn goal_error(error: AgentIsolationError) -> GoalStoreError {
    GoalStoreError::new(error.code(), "Trusted host run binding is unavailable")
}
fn key(value: &str) -> Result<(), AgentIsolationError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(denied_host());
    }
    Ok(())
}
fn schema(conn: &Connection) -> Result<(), AgentIsolationError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_agent_host_windows (principal_key TEXT NOT NULL,window_key TEXT NOT NULL,run_id TEXT NOT NULL UNIQUE REFERENCES wc_agent_runs(run_id),enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),PRIMARY KEY(principal_key,window_key));").map_err(unavailable_host)
}

fn resolve(
    conn: &Connection,
    principal: &str,
    window: &str,
    now: i64,
) -> Result<Option<AgentHostWindowBinding>, AgentIsolationError> {
    key(principal)?;
    key(window)?;
    let row:Option<(String,String,String,String)>=conn.query_row("SELECT r.run_id,r.state,a.project,a.agent_id FROM wc_agent_host_windows w JOIN wc_agent_runs r ON r.run_id=w.run_id JOIN wc_isolated_agents a ON a.agent_id=r.agent_id WHERE w.principal_key=?1 AND w.window_key=?2 AND w.enabled=1",params![principal,window],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(unavailable_host)?;
    let Some((run_id, state, project, agent_id)) = row else {
        return Ok(None);
    };
    let owner = crate::agent_isolation::host_goal_owner_key(&project, &agent_id);
    if state == "active" {
        return crate::agent_isolation::verify_agent_run_context_on_connection(
            conn, &owner, &run_id,
        )
        .map(|run| Some(AgentHostWindowBinding::Active(run)));
    }
    if state != "pending" {
        return Err(denied_host());
    }
    let value:Option<String>=conn.query_row("SELECT snapshot_json FROM wc_host_goal_handoffs WHERE owner_key=?1 AND json_extract(snapshot_json,'$.pending_agent_run_id')=?2",params![owner,run_id],|r|r.get(0)).optional().map_err(unavailable_host)?;
    let h: HostGoalHandoff =
        serde_json::from_str(&value.ok_or_else(denied_host)?).map_err(unavailable_host)?;
    if !matches!(h.status.as_str(), "awaiting_ready" | "prepared")
        || h.deadline_at.is_none_or(|deadline| now >= deadline)
        || h.candidate_context_epoch.as_deref() != Some(run_id.as_str())
        || h.agent_id.as_deref() != Some(agent_id.as_str())
    {
        return Err(denied_host());
    }
    let source = crate::agent_isolation::verify_agent_run_context_on_connection(
        conn,
        &owner,
        h.source_agent_run_id.as_deref().ok_or_else(denied_host)?,
    )?;
    crate::agent_isolation::pending_agent_run_on_connection(conn, &source, &run_id)?;
    Ok(Some(AgentHostWindowBinding::Pending {
        source,
        pending_run_id: run_id,
        goal_id: h.goal_id,
        handoff_id: h.handoff_id,
        attempt_id: h.attempt_id.ok_or_else(denied_host)?,
    }))
}

fn exact_pending(
    conn: &Connection,
    principal: &str,
    window: &str,
    goal: &str,
    handoff: &str,
    attempt: &str,
    now: i64,
) -> Result<(VerifiedAgentRun, String, HostGoalHandoff), AgentIsolationError> {
    let Some(AgentHostWindowBinding::Pending {
        source,
        pending_run_id,
        goal_id,
        handoff_id,
        attempt_id,
    }) = resolve(conn, principal, window, now)?
    else {
        return Err(denied_host());
    };
    if goal_id != goal || handoff_id != handoff || attempt_id != attempt {
        return Err(denied_host());
    }
    let h = load(conn, &source.host_goal_owner_key(), goal)
        .map_err(|e| AgentIsolationError::new(e.code()))?
        .ok_or_else(denied_host)?;
    Ok((source, pending_run_id, h))
}

fn claim_attempt(
    conn: &Connection,
    owner: &str,
    goal: &str,
    handoff: &str,
    attempt: &str,
    now: i64,
) -> Result<(HostGoalHandoff, VerifiedAgentRun, VerifiedAgentRun), GoalStoreError> {
    let h = current(conn, owner, goal, handoff)?;
    if h.attempt_id.as_deref() != Some(attempt) {
        return Err(GoalStoreError::new(
            "goal_handoff_stale",
            "The handoff attempt is no longer current",
        ));
    }
    if !matches!(h.status.as_str(), "awaiting_ready" | "prepared") {
        return Err(GoalStoreError::new(
            "goal_handoff_claim_not_pending",
            "Only a current pending attempt can be claimed",
        ));
    }
    if h.deadline_at.is_none_or(|deadline| now >= deadline) {
        return Err(GoalStoreError::new(
            "goal_handoff_expired",
            "The handoff attempt has expired",
        ));
    }
    let source_id = h.source_agent_run_id.as_deref().ok_or_else(|| {
        GoalStoreError::new(
            "native_agent_handoff_required",
            "A native source run is required",
        )
    })?;
    let source =
        crate::agent_isolation::verify_agent_run_context_on_connection(conn, owner, source_id)
            .map_err(goal_error)?;
    if h.agent_id.as_deref() != Some(source.agent_id())
        || h.project != source.project()
        || h.source_context_epoch != source.run_id()
    {
        return Err(GoalStoreError::new(
            "goal_handoff_run_mismatch",
            "Source run does not match this handoff",
        ));
    }
    let pending_id = h.pending_agent_run_id.as_deref().ok_or_else(|| {
        GoalStoreError::new(
            "goal_handoff_claim_not_pending",
            "This attempt has no pending run",
        )
    })?;
    let pending =
        crate::agent_isolation::pending_agent_run_on_connection(conn, &source, pending_id)
            .map_err(goal_error)?;
    let scoped:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs WHERE run_id=?1 AND state='pending' AND task_goal_id=?2)",params![pending_id,goal],|r|r.get(0)).map_err(unavailable)?;
    let checkpoint_current:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goals g JOIN wc_host_goal_checkpoints c ON c.goal_id=g.goal_id WHERE g.owner_key=?1 AND g.goal_id=?2 AND g.project=?3 AND c.sequence=?4 AND json_extract(g.snapshot_json,'$.plan_version')=?5 AND json_extract(c.snapshot_json,'$.plan_version')=?5 AND json_extract(c.snapshot_json,'$.revision')=g.revision AND json_extract(c.snapshot_json,'$.source_context_epoch')=?6 AND json_extract(c.snapshot_json,'$.identity_source')='agent_run' AND (json_extract(c.snapshot_json,'$.source_fence')=json_extract(g.snapshot_json,'$.fence') OR (json_extract(c.snapshot_json,'$.source_fence') IS NULL AND json_extract(g.snapshot_json,'$.current_attempt_id') IS NULL AND g.phase NOT IN ('executing','verifying') AND NOT EXISTS(SELECT 1 FROM wc_host_goal_attempts a WHERE a.goal_id=g.goal_id AND json_extract(a.snapshot_json,'$.status')='running'))) AND json_extract(c.snapshot_json,'$.action')='handoff')",params![owner,goal,h.project,h.checkpoint_sequence,h.plan_version,source_id],|r|r.get(0)).map_err(unavailable)?;
    if !scoped || !checkpoint_current {
        return Err(GoalStoreError::new(
            "goal_handoff_checkpoint_mismatch",
            "Pending run scope or checkpoint changed",
        ));
    }
    Ok((h, source, pending))
}

fn claim_denied() -> AgentIsolationError {
    AgentIsolationError::new("agent_handoff_claim_denied")
}

impl Database {
    /// Bootstrap-only issuance to the trusted launch controller. Raw material
    /// is returned exactly once, never recoverable from state/status endpoints.
    pub fn issue_agent_handoff_claim(
        &self,
        owner: &str,
        goal: &str,
        handoff: &str,
        attempt: &str,
        now: i64,
    ) -> Result<AgentHandoffClaimGrant, GoalStoreError> {
        let started = std::time::Instant::now();
        let mut conn = self.conn.lock().unwrap();
        schema(&conn).map_err(goal_error)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let (h, source, pending) = claim_attempt(
            &tx,
            owner,
            goal,
            handoff,
            attempt,
            handoff_now(now, started),
        )?;
        let principal: Option<String> = tx
            .query_row(
                "SELECT principal_key FROM wc_agent_host_windows WHERE run_id=?1 AND enabled=1",
                [source.run_id()],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        let principal = principal.ok_or_else(|| {
            GoalStoreError::new(
                "agent_host_authorization_required",
                "Source has no enabled trusted host registration",
            )
        })?;
        let issued:bool=tx.query_row("SELECT handoff_claim_hash IS NOT NULL OR handoff_claim_principal_key IS NOT NULL FROM wc_agent_runs WHERE run_id=?1",[pending.run_id()],|r|r.get(0)).map_err(unavailable)?;
        if issued {
            return Err(GoalStoreError::new("goal_handoff_claim_already_issued","This attempt already received its one-time launch grant; reconcile status or wait for its deadline"));
        }
        let already_bound: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE run_id=?1)",
                [pending.run_id()],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
        if h.candidate_context_epoch.is_some() || already_bound {
            return Err(GoalStoreError::new(
                "goal_handoff_claim_already_bound",
                "This candidate already has an explicit host-window registration",
            ));
        }
        let secret = crate::agent_isolation::new_handoff_claim_token();
        let changed=tx.execute("UPDATE wc_agent_runs SET handoff_claim_hash=?2,handoff_claim_principal_key=?3 WHERE run_id=?1 AND state='pending' AND handoff_claim_hash IS NULL AND handoff_claim_principal_key IS NULL",params![pending.run_id(),claim_digest(&secret),principal]).map_err(unavailable)?;
        if changed != 1 {
            return Err(conflict("Handoff claim issuance changed concurrently"));
        }
        handoff_unexpired(&h, handoff_now(now, started))?;
        tx.commit().map_err(unavailable)?;
        Ok(AgentHandoffClaimGrant(secret))
    }

    /// Redeem only a task-specific one-time invitation on an authenticated,
    /// trusted host callback. This binds Pending only; it never grants execution.
    pub fn claim_agent_handoff_host_window(
        &self,
        principal: &str,
        window: &str,
        goal: &str,
        handoff: &str,
        attempt: &str,
        grant: &str,
        now: i64,
    ) -> Result<AgentHostWindowBinding, AgentIsolationError> {
        let started = std::time::Instant::now();
        key(principal)?;
        key(window)?;
        if !valid_claim(grant) {
            return Err(claim_denied());
        }
        let mut conn = self.conn.lock().unwrap();
        schema(&conn)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_host)?;
        let owner:Option<String>=tx.query_row("SELECT owner_key FROM wc_host_goal_handoffs WHERE goal_id=?1 AND json_extract(snapshot_json,'$.handoff_id')=?2",params![goal,handoff],|r|r.get(0)).optional().map_err(unavailable_host)?;
        let owner = owner.ok_or_else(claim_denied)?;
        let (h, source, pending) = claim_attempt(
            &tx,
            &owner,
            goal,
            handoff,
            attempt,
            handoff_now(now, started),
        )
        .map_err(|_| claim_denied())?;
        let stored:(Option<String>,Option<String>)=tx.query_row("SELECT handoff_claim_hash,handoff_claim_principal_key FROM wc_agent_runs WHERE run_id=?1",[pending.run_id()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(unavailable_host)?;
        if stored.0.as_deref() != Some(claim_digest(grant).as_str())
            || stored.1.as_deref() != Some(principal)
        {
            return Err(claim_denied());
        }
        let source_enabled:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE principal_key=?1 AND run_id=?2 AND enabled=1)",params![principal,source.run_id()],|r|r.get(0)).map_err(unavailable_host)?;
        if !source_enabled {
            return Err(claim_denied());
        }
        // A window tombstone is global to the trusted host namespace, not just
        // this OAuth principal. Disabled and revoked bindings cannot be reused.
        let mut statement=tx.prepare("SELECT principal_key,run_id,enabled FROM wc_agent_host_windows WHERE window_key=?1").map_err(unavailable_host)?;
        let windows = statement
            .query_map([window], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })
            .map_err(unavailable_host)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable_host)?;
        drop(statement);
        if windows
            .iter()
            .any(|(p, run, enabled)| p != principal || run != pending.run_id() || !enabled)
        {
            return Err(claim_denied());
        }
        if !windows.is_empty() && h.candidate_context_epoch.as_deref() != Some(pending.run_id()) {
            return Err(claim_denied());
        }
        let result = Self::bind_handoff_host_window_on_connection(
            &tx,
            principal,
            window,
            goal,
            handoff,
            attempt,
            handoff_now(now, started),
        )
        .map_err(|_| claim_denied())?;
        if !matches!(result, AgentHostWindowBinding::Pending { .. }) {
            return Err(claim_denied());
        }
        handoff_unexpired(&h, handoff_now(now, started)).map_err(|_| claim_denied())?;
        tx.commit().map_err(unavailable_host)?;
        Ok(result)
    }

    /// Trusted Bootstrap control only. The HTTP adapter must separately prove
    /// exclusive trustworthy host metadata and derive both keys server-side.
    pub fn register_agent_host_window(
        &self,
        principal_key: &str,
        window_key: &str,
        source: &VerifiedAgentRun,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        key(principal_key)?;
        key(window_key)?;
        let mut conn = self.conn.lock().unwrap();
        schema(&conn)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_host)?;
        crate::agent_isolation::validate_agent_run_on_connection(&tx, source)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT run_id FROM wc_agent_host_windows WHERE principal_key=?1 AND window_key=?2",
                params![principal_key, window_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable_host)?;
        if existing
            .as_deref()
            .is_some_and(|run| run != source.run_id())
        {
            return Err(denied_host());
        }
        tx.execute("INSERT INTO wc_agent_host_windows(principal_key,window_key,run_id,enabled) VALUES(?1,?2,?3,1) ON CONFLICT(principal_key,window_key) DO UPDATE SET enabled=1 WHERE wc_agent_host_windows.run_id=excluded.run_id",params![principal_key,window_key,source.run_id()]).map_err(|_|denied_host())?;
        tx.commit().map_err(unavailable_host)?;
        Ok(source.clone())
    }

    pub fn agent_host_principal_authorized(
        &self,
        principal_key: &str,
    ) -> Result<bool, AgentIsolationError> {
        key(principal_key)?;
        let conn = self.conn.lock().unwrap();
        schema(&conn)?;
        conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE principal_key=?1 AND enabled=1)",[principal_key],|r|r.get(0)).map_err(unavailable_host)
    }

    /// Revokes host-channel admission only; never erases run scope or retries.
    pub fn revoke_agent_host_principal(
        &self,
        principal_key: &str,
    ) -> Result<(), AgentIsolationError> {
        key(principal_key)?;
        let mut conn = self.conn.lock().unwrap();
        schema(&conn)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_host)?;
        crate::agent_isolation::revoke_agent_enrollments_for_principal_on_connection(
            &tx,
            principal_key,
        )?;
        tx.execute(
            "UPDATE wc_agent_host_windows SET enabled=0 WHERE principal_key=?1",
            [principal_key],
        )
        .map_err(unavailable_host)?;
        tx.commit().map_err(unavailable_host)?;
        Ok(())
    }

    pub fn resolve_agent_host_window(
        &self,
        principal_key: &str,
        window_key: &str,
        now: i64,
    ) -> Result<Option<AgentHostWindowBinding>, AgentIsolationError> {
        let started = std::time::Instant::now();
        let mut conn = self.conn.lock().unwrap();
        schema(&conn)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(unavailable_host)?;
        resolve(&tx, principal_key, window_key, handoff_now(now, started))
    }

    /// Bootstrap control only, after a trusted launch callback has proved the
    /// exact destination host window. Never call from model prepare or use UUID
    /// possession as authorization. A run can be mapped to only one window.
    pub fn bind_handoff_host_window(
        &self,
        principal_key: &str,
        window_key: &str,
        goal_id: &str,
        handoff_id: &str,
        attempt_id: &str,
        now: i64,
    ) -> Result<AgentHostWindowBinding, AgentIsolationError> {
        let started = std::time::Instant::now();
        key(principal_key)?;
        key(window_key)?;
        let mut conn = self.conn.lock().unwrap();
        schema(&conn)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_host)?;
        let result = Self::bind_handoff_host_window_on_connection(
            &tx,
            principal_key,
            window_key,
            goal_id,
            handoff_id,
            attempt_id,
            handoff_now(now, started),
        )?;
        tx.commit().map_err(unavailable_host)?;
        Ok(result)
    }

    fn bind_handoff_host_window_on_connection(
        tx: &Connection,
        principal_key: &str,
        window_key: &str,
        goal_id: &str,
        handoff_id: &str,
        attempt_id: &str,
        now: i64,
    ) -> Result<AgentHostWindowBinding, AgentIsolationError> {
        let started = std::time::Instant::now();
        let value:Option<(String,String)>=tx.query_row("SELECT owner_key,snapshot_json FROM wc_host_goal_handoffs WHERE goal_id=?1 AND json_extract(snapshot_json,'$.handoff_id')=?2",params![goal_id,handoff_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(unavailable_host)?;
        let (owner, value) = value.ok_or_else(denied_host)?;
        let mut h: HostGoalHandoff = serde_json::from_str(&value).map_err(unavailable_host)?;
        if h.attempt_id.as_deref() != Some(attempt_id) {
            return Err(denied_host());
        }
        if h.status == "committed" {
            let existing = resolve(&tx, principal_key, window_key, now)?.ok_or_else(denied_host)?;
            if matches!(&existing,AgentHostWindowBinding::Active(run) if h.successor_agent_run_id.as_deref()==Some(run.run_id()))
            {
                return Ok(existing);
            }
            return Err(denied_host());
        }
        if !matches!(h.status.as_str(), "awaiting_ready" | "prepared")
            || h.deadline_at
                .is_none_or(|deadline| handoff_now(now, started) >= deadline)
        {
            return Err(denied_host());
        }
        let source_id = h.source_agent_run_id.as_deref().ok_or_else(denied_host)?;
        let source =
            crate::agent_isolation::verify_agent_run_context_on_connection(&tx, &owner, source_id)?;
        let source_registered:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_host_windows WHERE principal_key=?1 AND run_id=?2 AND enabled=1)",params![principal_key,source_id],|r|r.get(0)).map_err(unavailable_host)?;
        if !source_registered {
            return Err(denied_host());
        }
        let pending_id = h.pending_agent_run_id.clone().ok_or_else(denied_host)?;
        crate::agent_isolation::pending_agent_run_on_connection(&tx, &source, &pending_id)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT run_id FROM wc_agent_host_windows WHERE principal_key=?1 AND window_key=?2",
                params![principal_key, window_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable_host)?;
        if old.as_deref().is_some_and(|run| run != pending_id) {
            return Err(denied_host());
        }
        tx.execute("INSERT INTO wc_agent_host_windows(principal_key,window_key,run_id,enabled) VALUES(?1,?2,?3,1) ON CONFLICT(principal_key,window_key) DO UPDATE SET enabled=1 WHERE wc_agent_host_windows.run_id=excluded.run_id",params![principal_key,window_key,pending_id]).map_err(|_|denied_host())?;
        h.candidate_context_epoch = Some(pending_id.clone());
        persist(&tx, &owner, &h).map_err(|e| AgentIsolationError::new(e.code()))?;
        handoff_unexpired(&h, handoff_now(now, started)).map_err(|_| denied_host())?;
        let result = resolve(&tx, principal_key, window_key, handoff_now(now, started))?
            .ok_or_else(denied_host)?;
        Ok(result)
    }

    pub fn with_pending_agent_host_handoff<T>(
        &self,
        principal: &str,
        window: &str,
        goal: &str,
        handoff: &str,
        attempt: &str,
        now: i64,
        operation: impl FnOnce(&VerifiedAgentRun, &HostGoalHandoff) -> Result<T, AgentIsolationError>,
    ) -> Result<T, AgentIsolationError> {
        let started = std::time::Instant::now();
        let mut conn = self.conn.lock().unwrap();
        schema(&conn)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_host)?;
        let (source, _, h) = exact_pending(
            &tx,
            principal,
            window,
            goal,
            handoff,
            attempt,
            handoff_now(now, started),
        )?;
        let result = operation(&source, &h)?;
        handoff_unexpired(&h, handoff_now(now, started))
            .map_err(|e| AgentIsolationError::new(e.code()))?;
        tx.commit().map_err(unavailable_host)?;
        Ok(result)
    }

    pub fn with_source_agent_handoff<T>(
        &self,
        source: &VerifiedAgentRun,
        goal_id: &str,
        checkpoint_sequence: u64,
        operation: impl FnOnce() -> Result<T, AgentIsolationError>,
    ) -> Result<T, AgentIsolationError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable_host)?;
        crate::agent_isolation::validate_agent_run_on_connection(&tx, source)?;
        let h = load(&tx, &source.host_goal_owner_key(), goal_id)
            .map_err(|e| AgentIsolationError::new(e.code()))?
            .ok_or_else(denied_host)?;
        if h.source_agent_run_id.as_deref() != Some(source.run_id())
            || h.agent_id.as_deref() != Some(source.agent_id())
            || h.checkpoint_sequence != checkpoint_sequence
            || !matches!(h.status.as_str(), "waiting" | "awaiting_ready" | "prepared")
        {
            return Err(denied_host());
        }
        let result = operation()?;
        tx.commit()
            .map_err(|_| AgentIsolationError::new("agent_private_operation_outcome_unknown"))?;
        Ok(result)
    }

    pub fn prepare_agent_handoff_for_host_window(
        &self,
        principal: &str,
        window: &str,
        goal: &str,
        handoff: &str,
        attempt: &str,
        checkpoint_sequence: u64,
        package_sha256: &str,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        let started = std::time::Instant::now();
        if package_sha256.len() != 64 || !package_sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid("Expected verified handoff SHA256"));
        }
        let mut conn = self.conn.lock().unwrap();
        schema(&conn).map_err(goal_error)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let (source, _, mut h) = exact_pending(
            &tx,
            principal,
            window,
            goal,
            handoff,
            attempt,
            handoff_now(now, started),
        )
        .map_err(goal_error)?;
        if h.checkpoint_sequence != checkpoint_sequence
            || read(&tx, &source.host_goal_owner_key(), goal)?
                .ok_or_else(not_found)?
                .plan_version
                != h.plan_version
        {
            return Err(conflict("Handoff package version changed"));
        }
        if h.prepared_package_sha256
            .as_deref()
            .is_some_and(|old| old != package_sha256)
        {
            return Err(conflict("Prepared package digest cannot change"));
        }
        h.status = "prepared".into();
        h.prepared_at.get_or_insert(handoff_now(now, started));
        h.prepared_package_sha256 = Some(package_sha256.into());
        persist(&tx, &source.host_goal_owner_key(), &h)?;
        handoff_unexpired(&h, handoff_now(now, started))?;
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    pub fn commit_agent_handoff_for_host_window(
        &self,
        principal: &str,
        window: &str,
        goal: &str,
        handoff: &str,
        attempt: &str,
        checkpoint_sequence: u64,
        package_sha256: &str,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        let started = std::time::Instant::now();
        let mut conn = self.conn.lock().unwrap();
        schema(&conn).map_err(goal_error)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let binding = resolve(&tx, principal, window, handoff_now(now, started))
            .map_err(goal_error)?
            .ok_or_else(|| goal_error(denied_host()))?;
        let committing = matches!(&binding, AgentHostWindowBinding::Pending { .. });
        let h = match binding {
            AgentHostWindowBinding::Active(run) => {
                let h = current(&tx, &run.host_goal_owner_key(), goal, handoff)?;
                if h.status != "committed"
                    || h.attempt_id.as_deref() != Some(attempt)
                    || h.successor_agent_run_id.as_deref() != Some(run.run_id())
                    || h.checkpoint_sequence != checkpoint_sequence
                    || h.prepared_package_sha256.as_deref() != Some(package_sha256)
                {
                    return Err(conflict("Active run is not this committed handoff receipt"));
                }
                h
            }
            AgentHostWindowBinding::Pending {
                source,
                pending_run_id,
                goal_id,
                handoff_id,
                attempt_id,
            } => {
                if goal_id != goal || handoff_id != handoff || attempt_id != attempt {
                    return Err(conflict("Pending host mapping is for another attempt"));
                }
                let context = HostGoalInvocationContext {
                    context_epoch: pending_run_id.clone(),
                    identity_source: "agent_run".into(),
                };
                Self::commit_handoff_on_connection(
                    &tx,
                    &source.host_goal_owner_key(),
                    goal,
                    handoff,
                    attempt,
                    checkpoint_sequence,
                    package_sha256,
                    &context,
                    handoff_now(now, started),
                    Some((&source, &pending_run_id)),
                )?
            }
        };
        if committing {
            handoff_unexpired(&h, handoff_now(now, started))?;
        }
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    pub fn host_goal_handoff_failure_code(value: Option<&str>) -> Option<&'static str> {
        match value {
            Some("launch_failed") => Some("launch_failed"),
            Some("thinking_policy_changed") => Some("thinking_policy_changed"),
            Some("goal_not_active") => Some("goal_not_active"),
            Some("old_execution_not_quiescent") => Some("old_execution_not_quiescent"),
            Some("All four handoff startup attempts failed or timed out") => {
                Some("attempts_exhausted")
            }
            Some(_) => Some("handoff_failed"),
            None => None,
        }
    }

    /// Lifecycle data only: never deserialize or return objective, plan, state,
    /// checkpoint reason, paths or freeform failure text from mixed snapshots.
    /// The caller must first authorize the owning task/project in this connection.
    pub(crate) fn host_goal_lifecycle_metadata_on_connection(
        conn: &Connection,
        owner: &str,
        id: &str,
    ) -> Result<serde_json::Value, GoalStoreError> {
        conn.query_row(
            "SELECT g.goal_id,g.project,g.phase,g.revision,g.created_at,g.updated_at,
                json_extract(g.snapshot_json,'$.fence'),json_extract(g.snapshot_json,'$.plan_version'),
                c.sequence,json_extract(c.snapshot_json,'$.revision'),json_extract(c.snapshot_json,'$.source_fence'),json_extract(c.snapshot_json,'$.created_at'),
                json_extract(h.snapshot_json,'$.status'),json_extract(h.snapshot_json,'$.attempt_count'),
                json_extract(h.snapshot_json,'$.created_at'),json_extract(h.snapshot_json,'$.committed_at'),
                json_extract(h.snapshot_json,'$.deadline_at'),json_extract(h.snapshot_json,'$.failure_reason')
             FROM wc_host_goals g
             LEFT JOIN wc_host_goal_handoffs h ON h.goal_id=g.goal_id AND h.owner_key=g.owner_key
             LEFT JOIN wc_host_goal_checkpoints c ON c.goal_id=g.goal_id AND c.sequence=COALESCE(json_extract(h.snapshot_json,'$.checkpoint_sequence'),(SELECT MAX(sequence) FROM wc_host_goal_checkpoints WHERE goal_id=g.goal_id))
             WHERE g.owner_key=?1 AND g.goal_id=?2",
            params![owner,id],
            |row| {
                let failure: Option<String> = row.get(17)?;
                Ok(serde_json::json!({
                    "goal_id":row.get::<_,String>(0)?,"project":row.get::<_,String>(1)?,
                    "goal_phase":row.get::<_,String>(2)?,"goal_revision":row.get::<_,u64>(3)?,
                    "created_at":row.get::<_,i64>(4)?,"updated_at":row.get::<_,i64>(5)?,
                    "goal_fence":row.get::<_,u64>(6)?,"plan_version":row.get::<_,u64>(7)?,
                    "checkpoint_sequence":row.get::<_,Option<u64>>(8)?,
                    "checkpoint_revision":row.get::<_,Option<u64>>(9)?,
                    "source_fence":row.get::<_,Option<u64>>(10)?,
                    "checkpoint_created_at":row.get::<_,Option<i64>>(11)?,
                    "handoff_status":row.get::<_,Option<String>>(12)?,
                    "handoff_attempt_count":row.get::<_,Option<u32>>(13)?,
                    "handoff_created_at":row.get::<_,Option<i64>>(14)?,
                    "handoff_committed_at":row.get::<_,Option<i64>>(15)?,
                    "handoff_deadline_at":row.get::<_,Option<i64>>(16)?,
                    "handoff_failure_code":Self::host_goal_handoff_failure_code(failure.as_deref()),
                }))
            },
        ).optional().map_err(unavailable)?.ok_or_else(not_found)
    }

    pub fn read_host_goal_handoff_control_metadata(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<serde_json::Value, GoalStoreError> {
        Self::host_goal_lifecycle_metadata_on_connection(&self.conn.lock().unwrap(), owner, id)
    }
}
