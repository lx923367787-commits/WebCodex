//! One durable transfer per Goal. The runtime owns probes and process quiescence;
//! this transaction owns candidate fencing, retry budgets and execution ownership.
use super::*;
#[path = "agent_host_window.rs"]
mod host_window;
pub use host_window::{AgentHandoffClaimGrant, AgentHostWindowBinding};
#[path = "host_goal_handoff_recovery.rs"]
mod failed_recovery;

// Preserve the caller's (including test) epoch while accounting for lock/SQL
// queueing. Deadlines must be checked at admission, not at request arrival.
fn handoff_now(base: i64, started: std::time::Instant) -> i64 {
    base.saturating_add(started.elapsed().as_millis().min(i64::MAX as u128) as i64)
}

fn handoff_unexpired(h: &HostGoalHandoff, now: i64) -> Result<(), GoalStoreError> {
    if h.deadline_at.is_none_or(|deadline| now >= deadline) {
        return Err(GoalStoreError::new(
            "goal_handoff_expired",
            "The handoff attempt expired before admission",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostGoalHandoff {
    pub handoff_id: String,
    pub goal_id: String,
    pub project: String,
    pub source_context_epoch: String,
    pub checkpoint_sequence: u64,
    pub plan_version: u64,
    pub status: String,
    pub attempt_count: u32,
    pub attempt_id: Option<String>,
    pub candidate_context_epoch: Option<String>,
    pub deadline_at: Option<i64>,
    pub timeout_secs: u64,
    pub prepared_at: Option<i64>,
    pub prepared_package_sha256: Option<String>,
    pub execution_owner_epoch: String,
    pub failure_reason: Option<String>,
    pub created_at: i64,
    pub committed_at: Option<i64>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub source_agent_run_id: Option<String>,
    #[serde(default)]
    pub successor_agent_run_id: Option<String>,
    #[serde(default)]
    pub pending_agent_run_id: Option<String>,
}

fn load(
    conn: &Connection,
    owner: &str,
    id: &str,
) -> Result<Option<HostGoalHandoff>, GoalStoreError> {
    bounded(owner, 512, "owner")?;
    let owned: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_host_goals WHERE owner_key=?1 AND goal_id=?2)",
            params![owner, id],
            |r| r.get(0),
        )
        .map_err(unavailable)?;
    if !owned {
        return Err(not_found());
    }
    conn.query_row(
        "SELECT snapshot_json FROM wc_host_goal_handoffs WHERE owner_key=?1 AND goal_id=?2",
        params![owner, id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(unavailable)?
    .map(decode)
    .transpose()
}

fn persist(conn: &Connection, owner: &str, h: &HostGoalHandoff) -> Result<(), GoalStoreError> {
    conn.execute("INSERT INTO wc_host_goal_handoffs(owner_key,goal_id,snapshot_json) VALUES(?1,?2,?3) ON CONFLICT(owner_key,goal_id) DO UPDATE SET snapshot_json=excluded.snapshot_json", params![owner,h.goal_id,encode(h)?]).map_err(unavailable)?;
    Ok(())
}

/// Composes explicit management recovery with run replacement. The caller owns
/// the IMMEDIATE transaction, so any rejection rolls back credential issuance.
pub(crate) fn recover_handoff_run_on_connection(
    conn: &Connection,
    new_run: &crate::VerifiedAgentRun,
    goal_id: &str,
    now: i64,
) -> Result<(), GoalStoreError> {
    crate::agent_isolation::validate_agent_run_on_connection(conn, new_run)
        .map_err(|e| GoalStoreError::new(e.code(), "Recovery run is unavailable"))?;
    if crate::agent_isolation::agent_run_task_goal_on_connection(conn, new_run)
        .map_err(|e| GoalStoreError::new(e.code(), "Recovery scope is unavailable"))?
        .as_deref()
        != Some(goal_id)
    {
        return Err(GoalStoreError::new(
            "agent_task_scope_denied",
            "Recovery must preserve the exact task scope",
        ));
    }
    let owner = new_run.host_goal_owner_key();
    let mut h = load(conn, &owner, goal_id)?.ok_or_else(|| {
        GoalStoreError::new(
            "goal_handoff_recovery_unavailable",
            "Scoped Goal has no committed handoff",
        )
    })?;
    if h.status != "committed"
        || h.agent_id.as_deref() != Some(new_run.agent_id())
        || h.project != new_run.project()
    {
        return Err(GoalStoreError::new("goal_handoff_recovery_unavailable","Only the same Agent's committed handoff can be explicitly recovered; pending and failed handoffs retain their retry state"));
    }
    let predecessor_matches:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id WHERE r.run_id=?1 AND r.agent_id=?2 AND r.task_goal_id=?3 AND a.owner_user=?4 AND a.project=?5 AND r.run_id<>?6)",params![h.successor_agent_run_id,new_run.agent_id(),goal_id,new_run.owner_user(),new_run.project(),new_run.run_id()],|r|r.get(0)).map_err(unavailable)?;
    if !predecessor_matches {
        return Err(GoalStoreError::new(
            "goal_handoff_run_mismatch",
            "The prior Goal execution owner does not match this Agent and task scope",
        ));
    }
    if unresolved(conn, goal_id)? {
        return Err(GoalStoreError::new(
            "goal_handoff_execution_active",
            "Resolve old execution operations before explicit recovery",
        ));
    }
    let mut goal = read(conn, &owner, goal_id)?
        .filter(|g| g.project == new_run.project())
        .ok_or_else(not_found)?;
    fence_attempt(conn, &mut goal, now)?;
    if matches!(
        goal.phase,
        HostGoalPhase::Executing | HostGoalPhase::Verifying
    ) {
        goal.phase = HostGoalPhase::Ready;
    }
    save(conn, &mut goal, now)?;
    h.successor_agent_run_id = Some(new_run.run_id().into());
    h.execution_owner_epoch = new_run.run_id().into();
    persist(conn, &owner, &h)
}

fn current(
    conn: &Connection,
    owner: &str,
    id: &str,
    handoff_id: &str,
) -> Result<HostGoalHandoff, GoalStoreError> {
    let h = load(conn, owner, id)?.ok_or_else(|| conflict("Goal has no pending handoff"))?;
    if h.handoff_id != handoff_id {
        return Err(GoalStoreError::new(
            "goal_handoff_stale",
            "This handoff has been superseded",
        ));
    }
    Ok(h)
}

fn trusted(context: &HostGoalInvocationContext) -> Result<(), GoalStoreError> {
    if !matches!(
        context.identity_source.as_str(),
        "window_bound" | "agent_run"
    ) {
        return Err(GoalStoreError::new(
            "goal_handoff_identity_required",
            "Reliable handoff requires a server-bound conversation identity",
        ));
    }
    bounded(&context.context_epoch, 128, "context_epoch")
}

pub(super) fn prepare(
    conn: &Connection,
    goal: &HostGoalRecord,
    checkpoint: &HostGoalCheckpoint,
    context: &HostGoalInvocationContext,
    now: i64,
) -> Result<(), GoalStoreError> {
    trusted(context)?;
    let native_source = if context.identity_source == "agent_run" {
        Some(
            crate::agent_isolation::verify_agent_run_context_on_connection(
                conn,
                &goal.owner_key,
                &context.context_epoch,
            )
            .map_err(|e| GoalStoreError::new(e.code(), "Source Agent run is unavailable"))?,
        )
    } else {
        None
    };
    if load(conn, &goal.owner_key, &goal.goal_id)?.is_some_and(|h| h.status != "committed") {
        return Err(conflict(
            "An existing handoff must finish before another can be prepared",
        ));
    }
    let h = HostGoalHandoff {
        handoff_id: format!("hhandoff_{}", uuid::Uuid::new_v4().simple()),
        goal_id: goal.goal_id.clone(),
        project: goal.project.clone(),
        source_context_epoch: context.context_epoch.clone(),
        checkpoint_sequence: checkpoint.sequence,
        plan_version: goal.plan_version,
        status: "waiting".into(),
        attempt_count: 0,
        attempt_id: None,
        candidate_context_epoch: None,
        deadline_at: None,
        timeout_secs: 300,
        prepared_at: None,
        prepared_package_sha256: None,
        execution_owner_epoch: context.context_epoch.clone(),
        failure_reason: None,
        created_at: now,
        committed_at: None,
        agent_id: native_source
            .as_ref()
            .map(|source| source.agent_id().into()),
        source_agent_run_id: native_source.as_ref().map(|source| source.run_id().into()),
        successor_agent_run_id: None,
        pending_agent_run_id: None,
    };
    let changed = conn.execute("UPDATE wc_host_context_states SET state=2,identity_source=COALESCE(identity_source,?3),retirement_goal_id=?4,checkpoint_sequence=?5 WHERE owner_key=?1 AND context_epoch=?2 AND state<3",params![goal.owner_key,context.context_epoch,context.identity_source,goal.goal_id,checkpoint.sequence]).map_err(unavailable)?;
    if changed != 1 {
        return Err(conflict(
            "Source context changed during handoff preparation",
        ));
    }
    persist(conn, &goal.owner_key, &h)
}

#[cfg(test)]
#[path = "host_goal_handoff_tests.rs"]
mod tests;

fn revoke_candidate(
    conn: &Connection,
    owner: &str,
    h: &mut HostGoalHandoff,
    now: i64,
) -> Result<(), GoalStoreError> {
    if let Some(run_id) = h.pending_agent_run_id.take() {
        conn.execute("UPDATE wc_agent_runs SET state='revoked' WHERE run_id=?1 AND agent_id=?2 AND state='pending'",params![run_id,h.agent_id]).map_err(unavailable)?;
    }
    if let Some(epoch) = h.candidate_context_epoch.take() {
        conn.execute("UPDATE wc_host_context_states SET state=3,retired_at=?3,retirement_goal_id=?4,checkpoint_sequence=?5 WHERE owner_key=?1 AND context_epoch=?2",params![owner,epoch,now,h.goal_id,h.checkpoint_sequence]).map_err(unavailable)?;
    }
    h.prepared_at = None;
    h.prepared_package_sha256 = None;
    h.deadline_at = None;
    Ok(())
}

fn stop(
    conn: &Connection,
    owner: &str,
    h: &mut HostGoalHandoff,
    reason: &str,
    now: i64,
) -> Result<(), GoalStoreError> {
    revoke_candidate(conn, owner, h, now)?;
    h.status = "failed".into();
    h.failure_reason = Some(reason.into());
    conn.execute("UPDATE wc_host_context_states SET state=4 WHERE owner_key=?1 AND context_epoch=?2 AND state<3",params![owner,h.source_context_epoch]).map_err(unavailable)?;
    let mut goal = read(conn, owner, &h.goal_id)?.ok_or_else(not_found)?;
    fence_attempt(conn, &mut goal, now)?;
    goal.phase = HostGoalPhase::Blocked;
    goal.state.next_step = "Handoff failed; execution stopped. Checkpoint, artifacts and failure reason are preserved. Explicit user recovery required.".into();
    save(conn, &mut goal, now)?;
    persist(conn, owner, h)
}

fn candidate(
    h: &HostGoalHandoff,
    attempt_id: &str,
    context: &HostGoalInvocationContext,
    now: i64,
) -> Result<(), GoalStoreError> {
    trusted(context)?;
    if context.identity_source == "agent_run"
        && h.pending_agent_run_id.as_deref() != Some(context.context_epoch.as_str())
        && !(h.status == "committed"
            && h.successor_agent_run_id.as_deref() == Some(context.context_epoch.as_str()))
    {
        return Err(GoalStoreError::new(
            "goal_handoff_stale",
            "The pending run is not the current handoff candidate",
        ));
    }
    if h.attempt_id.as_deref() != Some(attempt_id)
        || h.candidate_context_epoch.as_deref() != Some(&context.context_epoch)
    {
        return Err(GoalStoreError::new(
            "goal_handoff_stale",
            "Candidate does not own the current handoff attempt",
        ));
    }
    if h.status == "committed" {
        return Ok(());
    }
    if !matches!(h.status.as_str(), "awaiting_ready" | "prepared")
        || h.deadline_at.is_none_or(|deadline| now >= deadline)
    {
        return Err(GoalStoreError::new(
            "goal_handoff_expired",
            "The handoff attempt is expired or no longer eligible",
        ));
    }
    Ok(())
}

impl Database {
    /// Includes revoked candidates through their permanent context tombstone.
    pub fn host_goal_handoff_for_context(
        &self,
        owner: &str,
        epoch: &str,
    ) -> Result<Option<HostGoalHandoff>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT h.snapshot_json FROM wc_host_goal_handoffs h WHERE h.owner_key=?1 AND (json_extract(h.snapshot_json,'$.source_context_epoch')=?2 OR json_extract(h.snapshot_json,'$.candidate_context_epoch')=?2 OR json_extract(h.snapshot_json,'$.execution_owner_epoch')=?2 OR EXISTS(SELECT 1 FROM wc_host_context_states c WHERE c.owner_key=h.owner_key AND c.context_epoch=?2 AND c.retirement_goal_id=h.goal_id AND c.state>=3)) ORDER BY json_extract(h.snapshot_json,'$.created_at') DESC LIMIT 1",params![owner,epoch], |r| r.get::<_,String>(0)).optional().map_err(unavailable)?.map(decode).transpose()
    }

    pub fn read_host_goal_handoff(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<Option<HostGoalHandoff>, GoalStoreError> {
        load(&self.conn.lock().unwrap(), owner, id)
    }

    pub fn begin_host_goal_handoff_attempt(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        expected_attempt_id: Option<&str>,
        now: i64,
        timeout_secs: u64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        if !(1..=3600).contains(&timeout_secs) {
            return Err(invalid("Handoff timeout must be 1..=3600 seconds"));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let mut h = current(&tx, owner, id, handoff_id)?;
        if matches!(h.status.as_str(), "committed" | "failed")
            || h.deadline_at.is_some_and(|d| now < d)
        {
            return Ok(h);
        }
        if h.attempt_id.as_deref() != expected_attempt_id {
            return Err(GoalStoreError::new(
                "goal_handoff_stale",
                "Observe the current attempt before reserving its successor",
            ));
        }
        if h.attempt_count >= 4 {
            stop(
                &tx,
                owner,
                &mut h,
                "All four handoff startup attempts failed or timed out",
                now,
            )?;
        } else {
            revoke_candidate(&tx, owner, &mut h, now)?;
            h.attempt_count += 1;
            h.attempt_id = Some(uuid::Uuid::new_v4().to_string());
            h.status = "awaiting_ready".into();
            // The configured timeout becomes immutable after the first reservation.
            if h.attempt_count == 1 {
                h.timeout_secs = timeout_secs;
            }
            h.deadline_at = Some(
                now.checked_add((h.timeout_secs * 1000) as i64)
                    .ok_or_else(|| invalid("Handoff deadline overflow"))?,
            );
            if let Some(source_run_id) = h.source_agent_run_id.as_deref() {
                let source = crate::agent_isolation::verify_agent_run_context_on_connection(
                    &tx,
                    owner,
                    source_run_id,
                )
                .map_err(|e| GoalStoreError::new(e.code(), "Source Agent run is unavailable"))?;
                let pending =
                    crate::agent_isolation::begin_agent_run_handoff_on_connection(&tx, &source)
                        .map_err(|e| {
                            GoalStoreError::new(e.code(), "Pending Agent run could not be issued")
                        })?;
                crate::agent_isolation::bind_pending_task_goal_on_connection(
                    &tx,
                    &source,
                    &pending.run_id,
                    id,
                )
                .map_err(|e| {
                    GoalStoreError::new(e.code(), "Pending Agent run cannot be scoped to this Goal")
                })?;
                h.pending_agent_run_id = Some(pending.run_id);
            }
            persist(&tx, owner, &h)?;
        }
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    pub fn bind_host_goal_handoff_candidate(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        attempt_id: &str,
        context: &HostGoalInvocationContext,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        trusted(context)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let mut h = current(&tx, owner, id, handoff_id)?;
        context_gate(&tx, owner, Some(context), false)?;
        if h.source_context_epoch == context.context_epoch
            || h.attempt_id.as_deref() != Some(attempt_id)
            || !matches!(h.status.as_str(), "awaiting_ready" | "prepared")
            || h.deadline_at.is_none_or(|d| now >= d)
        {
            return Err(GoalStoreError::new(
                "goal_handoff_stale",
                "Candidate is not eligible for this active handoff attempt",
            ));
        }
        if h.candidate_context_epoch
            .as_ref()
            .is_some_and(|epoch| epoch != &context.context_epoch)
        {
            return Err(conflict(
                "Another conversation is already bound to this handoff attempt",
            ));
        }
        h.candidate_context_epoch = Some(context.context_epoch.clone());
        persist(&tx, owner, &h)?;
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    /// Internal runtime API: call only after actual checkpoint/package reads and
    /// a safe tool-channel probe. No model-supplied readiness boolean is accepted.
    pub fn record_host_goal_handoff_prepared(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        attempt_id: &str,
        checkpoint_sequence: u64,
        package_sha256: &str,
        context: &HostGoalInvocationContext,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        if package_sha256.len() != 64 || !package_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid(
                "Expected a SHA-256 digest for the verified handoff package",
            ));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let mut h = current(&tx, owner, id, handoff_id)?;
        candidate(&h, attempt_id, context, now)?;
        if checkpoint_sequence != h.checkpoint_sequence
            || read(&tx, owner, id)?.ok_or_else(not_found)?.plan_version != h.plan_version
        {
            return Err(conflict(
                "Prepared checkpoint or plan does not match the immutable handoff package",
            ));
        }
        if h.status != "committed" {
            h.status = "prepared".into();
            h.prepared_at.get_or_insert(now);
            if h.prepared_package_sha256
                .as_deref()
                .is_some_and(|saved| saved != package_sha256)
            {
                return Err(conflict(
                    "Prepared package digest cannot change within one attempt",
                ));
            }
            h.prepared_package_sha256 = Some(package_sha256.into());
            persist(&tx, owner, &h)?;
        }
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    pub fn commit_host_goal_handoff_ready(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        attempt_id: &str,
        checkpoint_sequence: u64,
        package_sha256: &str,
        context: &HostGoalInvocationContext,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        self.commit_host_goal_handoff_ready_impl(
            owner,
            id,
            handoff_id,
            attempt_id,
            checkpoint_sequence,
            package_sha256,
            context,
            now,
            None,
        )
    }

    /// Trusted runtime composition only. The source proof must come from
    /// authentication and pending_run_id from the trusted host-window mapping,
    /// never from model-authored tool arguments or a startup prompt.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_host_goal_handoff_ready_with_run(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        attempt_id: &str,
        checkpoint_sequence: u64,
        package_sha256: &str,
        context: &HostGoalInvocationContext,
        now: i64,
        source: &crate::VerifiedAgentRun,
        pending_run_id: &str,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        self.commit_host_goal_handoff_ready_impl(
            owner,
            id,
            handoff_id,
            attempt_id,
            checkpoint_sequence,
            package_sha256,
            context,
            now,
            Some((source, pending_run_id)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_host_goal_handoff_ready_impl(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        attempt_id: &str,
        checkpoint_sequence: u64,
        package_sha256: &str,
        context: &HostGoalInvocationContext,
        now: i64,
        run: Option<(&crate::VerifiedAgentRun, &str)>,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        let started = std::time::Instant::now();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let h = Self::commit_handoff_on_connection(
            &tx,
            owner,
            id,
            handoff_id,
            attempt_id,
            checkpoint_sequence,
            package_sha256,
            context,
            handoff_now(now, started),
            run,
        )?;
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_handoff_on_connection(
        tx: &Connection,
        owner: &str,
        id: &str,
        handoff_id: &str,
        attempt_id: &str,
        checkpoint_sequence: u64,
        package_sha256: &str,
        context: &HostGoalInvocationContext,
        now: i64,
        run: Option<(&crate::VerifiedAgentRun, &str)>,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        let started = std::time::Instant::now();
        let mut h = current(&tx, owner, id, handoff_id)?;
        candidate(&h, attempt_id, context, handoff_now(now, started))?;
        if checkpoint_sequence != h.checkpoint_sequence
            || h.prepared_package_sha256.as_deref() != Some(package_sha256)
        {
            return Err(conflict(
                "Commit must acknowledge the exact server-verified checkpoint package",
            ));
        }
        if h.status == "committed" {
            if let Some((source, pending)) = run {
                if h.agent_id.as_deref() != Some(source.agent_id())
                    || h.source_agent_run_id.as_deref() != Some(source.run_id())
                    || h.successor_agent_run_id.as_deref() != Some(pending)
                {
                    return Err(conflict(
                        "Committed handoff belongs to another authenticated run transfer",
                    ));
                }
            }
            return Ok(h);
        }
        if h.status != "prepared"
            || h.prepared_at.is_none()
            || checkpoint_sequence != h.checkpoint_sequence
        {
            return Err(GoalStoreError::new(
                "goal_handoff_not_ready",
                "Server-verified preparation is required before execution ownership can switch",
            ));
        }
        if unresolved(&tx, id)? {
            return Err(GoalStoreError::new(
                "goal_handoff_execution_active",
                "Old execution chains have not authoritatively terminated",
            ));
        }
        let mut goal = read(&tx, owner, id)?.ok_or_else(not_found)?;
        if goal.plan_version != h.plan_version {
            return Err(conflict("Goal plan changed during handoff"));
        }
        if context.identity_source == "agent_run" {
            let Some((source, pending_id)) = run else {
                return Err(GoalStoreError::new(
                    "goal_handoff_identity_required",
                    "Native handoff requires the trusted pending run mapping",
                ));
            };
            if pending_id != context.context_epoch
                || h.pending_agent_run_id.as_deref() != Some(pending_id)
                || h.source_agent_run_id.as_deref() != Some(source.run_id())
                || h.agent_id.as_deref() != Some(source.agent_id())
            {
                return Err(GoalStoreError::new(
                    "goal_handoff_run_mismatch",
                    "Native run is not this attempt's registered candidate",
                ));
            }
        } else {
            context_gate(&tx, owner, Some(context), false)?;
        }
        if let Some((source, pending_run_id)) = run {
            if source.host_goal_owner_key() != owner || source.project() != h.project {
                return Err(GoalStoreError::new(
                    "goal_handoff_run_mismatch",
                    "Authenticated run must belong to the exact Goal owner and project",
                ));
            }
            // Only the final commit consumes execution closure. Prepare and
            // candidate readiness must leave source authentication untouched.
            crate::agent_execution_close::require_agent_run_execution_closed_on_connection(
                tx,
                source.owner_user(),
                source.project(),
                &webcodex_core::runner_protocol::AgentRunRef {
                    agent_id: source.agent_id().into(),
                    agent_run_id: source.run_id().into(),
                    generation: source.generation(),
                },
            )
            .map_err(|e| GoalStoreError::new(e.code(), "Original execution has not closed"))?;
            crate::agent_isolation::pending_agent_run_on_connection(&tx, source, pending_run_id)
                .map_err(|e| {
                    GoalStoreError::new(
                        e.code(),
                        "The pending run is not a valid continuation of the authenticated source",
                    )
                })?;
            crate::agent_isolation::bind_pending_task_goal_on_connection(
                &tx,
                source,
                pending_run_id,
                id,
            )
            .map_err(|e| {
                GoalStoreError::new(e.code(), "Successor run could not be confined to this Goal")
            })?;
            handoff_unexpired(&h, handoff_now(now, started))?;
            let active = crate::agent_isolation::activate_agent_run_on_connection(
                &tx,
                source.owner_user(),
                source.project(),
                source.agent_id(),
                pending_run_id,
            )
            .map_err(|e| GoalStoreError::new(e.code(), "Atomic agent run activation failed"))?;
            h.agent_id = Some(active.agent_id().into());
            h.source_agent_run_id = Some(source.run_id().into());
            h.successor_agent_run_id = Some(active.run_id().into());
        }
        let switched_at = handoff_now(now, started);
        handoff_unexpired(&h, switched_at)?;
        fence_attempt(&tx, &mut goal, now)?;
        if matches!(
            goal.phase,
            HostGoalPhase::Executing | HostGoalPhase::Verifying
        ) {
            goal.phase = HostGoalPhase::Ready;
        }
        save(&tx, &mut goal, now)?;
        let changed = tx.execute("UPDATE wc_host_context_states SET state=3,retired_at=?3 WHERE owner_key=?1 AND context_epoch=?2 AND state=2 AND retirement_goal_id=?4 AND checkpoint_sequence=?5",params![owner,h.source_context_epoch,now,id,h.checkpoint_sequence]).map_err(unavailable)?;
        if changed != 1 {
            return Err(conflict(
                "Source execution changed before ownership transfer",
            ));
        }
        h.status = "committed".into();
        h.pending_agent_run_id = None;
        h.execution_owner_epoch = context.context_epoch.clone();
        h.committed_at = Some(now);
        let receipt = HostGoalResumeStarted {
            context_epoch: context.context_epoch.clone(),
            identity_source: context.identity_source.clone(),
            started_at: now,
        };
        tx.execute("UPDATE wc_host_context_states SET resume_started_json=?3 WHERE owner_key=?1 AND context_epoch=?2",params![owner,h.source_context_epoch,encode(&receipt)?]).map_err(unavailable)?;
        persist(&tx, owner, &h)?;
        // Any timeout here still rolls back the entire caller-owned transaction.
        handoff_unexpired(&h, handoff_now(now, started))?;
        Ok(h)
    }

    pub fn fail_host_goal_handoff_attempt(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        attempt_id: &str,
        reason: &str,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        bounded(reason, 4096, "handoff failure reason")?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let mut h = current(&tx, owner, id, handoff_id)?;
        // Lost responses are reconciled before accepting any failure report.
        if h.status == "committed" || h.status == "failed" {
            return Ok(h);
        }
        if h.attempt_id.as_deref() != Some(attempt_id) {
            return Err(GoalStoreError::new(
                "goal_handoff_stale",
                "A previous attempt cannot fail the current handoff",
            ));
        }
        if h.attempt_count >= 4 {
            stop(&tx, owner, &mut h, reason, now)?;
        } else {
            revoke_candidate(&tx, owner, &mut h, now)?;
            h.status = "waiting".into();
            h.failure_reason = Some(reason.into());
            persist(&tx, owner, &h)?;
        }
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    pub fn abort_host_goal_handoff(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        reason: &str,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        bounded(reason, 4096, "handoff failure reason")?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let mut h = current(&tx, owner, id, handoff_id)?;
        if h.status != "committed" && h.status != "failed" {
            stop(&tx, owner, &mut h, reason, now)?;
        }
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }

    /// Abort only the attempt whose asynchronous preflight observed the failure.
    /// A stale result returns the authoritative state without stopping a successor.
    pub fn abort_host_goal_handoff_attempt(
        &self,
        owner: &str,
        id: &str,
        handoff_id: &str,
        expected_attempt_id: Option<&str>,
        reason: &str,
        now: i64,
    ) -> Result<HostGoalHandoff, GoalStoreError> {
        bounded(reason, 4096, "handoff failure reason")?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let mut h = current(&tx, owner, id, handoff_id)?;
        if matches!(h.status.as_str(), "committed" | "failed")
            || h.attempt_id.as_deref() != expected_attempt_id
        {
            return Ok(h);
        }
        stop(&tx, owner, &mut h, reason, now)?;
        tx.commit().map_err(unavailable)?;
        Ok(h)
    }
}
