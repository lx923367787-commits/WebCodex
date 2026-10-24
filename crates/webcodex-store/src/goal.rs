//! Owner-scoped goal snapshots with immutable acceptance and revision fencing.

use super::Database;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use webcodex_core::goal::{
    GoalRecord, GoalSpec, GoalState, GOAL_MAX_LIST_LIMIT, GOAL_MAX_RECORD_BYTES,
};

const MAX_GOALS: i64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalStoreError {
    code: &'static str,
    message: String,
}

impl GoalStoreError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn code(&self) -> &'static str {
        self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for GoalStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for GoalStoreError {}

fn unavailable(error: impl std::fmt::Display) -> GoalStoreError {
    tracing::warn!(error = %error, "goal store operation failed");
    GoalStoreError::new("goal_store_unavailable", "Goal storage is unavailable")
}

fn invalid(message: impl Into<String>) -> GoalStoreError {
    GoalStoreError::new("invalid_goal", message)
}
fn conflict(message: impl Into<String>) -> GoalStoreError {
    GoalStoreError::new("goal_conflict", message)
}

fn validate_owner(owner: &str) -> Result<(), GoalStoreError> {
    if owner.is_empty() || owner.len() > 512 || owner.contains('\0') {
        return Err(invalid("A bounded stable goal owner is required"));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct GoalMutation {
    pub goal: GoalRecord,
    pub created: bool,
}

fn encode(record: &GoalRecord) -> Result<String, GoalStoreError> {
    record.validate().map_err(invalid)?;
    let value = serde_json::to_string(record).map_err(unavailable)?;
    if value.len() > GOAL_MAX_RECORD_BYTES {
        return Err(invalid("Goal record exceeds the storage bound"));
    }
    Ok(value)
}

fn read_record(
    conn: &Connection,
    owner: &str,
    goal_id: &str,
) -> Result<Option<GoalRecord>, GoalStoreError> {
    let row: Option<(String, Option<String>, i64)> = conn.query_row(
        "SELECT snapshot_json, worker_epoch, revision FROM wc_goals WHERE owner_key = ?1 AND goal_id = ?2",
        params![owner, goal_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(unavailable)?;
    row.map(|(snapshot, worker_epoch, revision)| {
        let mut record: GoalRecord = serde_json::from_str(&snapshot).map_err(unavailable)?;
        if record.revision != revision as u64 {
            return Err(unavailable("Goal revision does not match its snapshot"));
        }
        record.owner_key = owner.to_string();
        record.worker_epoch = worker_epoch;
        Ok(record)
    })
    .transpose()
}

impl Database {
    /// Read an already committed creation before contacting an executor.
    pub fn lookup_goal_replay(
        &self,
        owner: &str,
        spec: &GoalSpec,
        idempotency_key: &str,
    ) -> Result<Option<GoalRecord>, GoalStoreError> {
        validate_owner(owner)?;
        spec.validate().map_err(invalid)?;
        if idempotency_key.trim().is_empty()
            || idempotency_key.len() > 128
            || idempotency_key.contains('\0')
        {
            return Err(invalid(
                "idempotency_key must contain 1..128 bytes without NUL",
            ));
        }
        let digest = format!("{:x}", Sha256::digest(idempotency_key.as_bytes()));
        let conn = self.conn.lock().unwrap();
        let id: Option<String> = conn
            .query_row(
                "SELECT goal_id FROM wc_goals WHERE owner_key = ?1 AND idempotency_digest = ?2",
                params![owner, digest],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        let goal = id
            .map(|id| read_record(&conn, owner, &id))
            .transpose()?
            .flatten();
        if goal.as_ref().is_some_and(|goal| goal.spec != *spec) {
            return Err(GoalStoreError::new(
                "goal_idempotency_conflict",
                "idempotency_key already identifies a different goal specification",
            ));
        }
        Ok(goal)
    }

    pub(super) fn ensure_goal_schema(conn: &mut Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wc_goals (
                goal_id TEXT PRIMARY KEY,
                owner_key TEXT NOT NULL,
                idempotency_digest TEXT NOT NULL,
                project TEXT NOT NULL,
                state TEXT NOT NULL,
                revision INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                worker_epoch TEXT,
                snapshot_json TEXT NOT NULL,
                UNIQUE(owner_key, idempotency_digest)
            );
            CREATE INDEX IF NOT EXISTS idx_wc_goals_owner_created ON wc_goals(owner_key, created_at DESC, goal_id);
            CREATE INDEX IF NOT EXISTS idx_wc_goals_owner_project ON wc_goals(owner_key, project, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_wc_goals_state ON wc_goals(state);",
        )?;
        // Opening a database does not claim workers or mutate execution state.
        Ok(())
    }

    pub fn create_goal_at(
        &self,
        owner: &str,
        spec: GoalSpec,
        idempotency_key: &str,
        now_ms: i64,
    ) -> Result<GoalMutation, GoalStoreError> {
        validate_owner(owner)?;
        spec.validate().map_err(invalid)?;
        if idempotency_key.trim().is_empty()
            || idempotency_key.len() > 128
            || idempotency_key.contains('\0')
        {
            return Err(invalid(
                "idempotency_key must contain 1..128 bytes without NUL",
            ));
        }
        let digest = format!("{:x}", Sha256::digest(idempotency_key.as_bytes()));
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT goal_id FROM wc_goals WHERE owner_key = ?1 AND idempotency_digest = ?2",
                params![owner, digest],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        if let Some(goal_id) = existing {
            let goal = read_record(&tx, owner, &goal_id)?
                .ok_or_else(|| unavailable("Goal disappeared during creation"))?;
            if goal.spec != spec {
                return Err(GoalStoreError::new(
                    "goal_idempotency_conflict",
                    "idempotency_key already identifies a different goal specification",
                ));
            }
            return Ok(GoalMutation {
                goal,
                created: false,
            });
        }
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM wc_goals", [], |row| row.get(0))
            .map_err(unavailable)?;
        if count >= MAX_GOALS {
            return Err(GoalStoreError::new(
                "goal_capacity",
                "Goal storage capacity reached",
            ));
        }
        let deadline_at = now_ms
            .checked_add((spec.limits.total_timeout_secs * 1000) as i64)
            .ok_or_else(|| invalid("Goal deadline is out of range"))?;
        let goal = GoalRecord {
            goal_id: format!("wc_goal_{}", uuid::Uuid::new_v4().simple()),
            revision: 0,
            spec,
            state: GoalState::Queued,
            target: None,
            rounds: Vec::new(),
            created_at: now_ms,
            deadline_at,
            updated_at: now_ms,
            reason: None,
            cancel_requested: false,
            owner_key: owner.to_string(),
            worker_epoch: None,
        };
        let snapshot = encode(&goal)?;
        tx.execute(
            "INSERT INTO wc_goals (goal_id, owner_key, idempotency_digest, project, state, revision, created_at, updated_at, worker_epoch, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![goal.goal_id, owner, digest, goal.spec.project, goal.state.as_str(), goal.revision as i64, now_ms, now_ms, goal.worker_epoch, snapshot],
        ).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(GoalMutation {
            goal,
            created: true,
        })
    }

    pub fn get_goal(
        &self,
        owner: &str,
        goal_id: &str,
    ) -> Result<Option<GoalRecord>, GoalStoreError> {
        validate_owner(owner)?;
        let conn = self.conn.lock().unwrap();
        read_record(&conn, owner, goal_id)
    }

    pub fn list_goals(
        &self,
        owner: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GoalRecord>, GoalStoreError> {
        validate_owner(owner)?;
        if !(1..=GOAL_MAX_LIST_LIMIT).contains(&limit) {
            return Err(invalid("Goal list limit must be 1..100"));
        }
        let conn = self.conn.lock().unwrap();
        let mut statement = conn
            .prepare(
                "SELECT goal_id FROM wc_goals WHERE owner_key = ?1 AND (?2 IS NULL OR project = ?2)
             ORDER BY created_at DESC, goal_id DESC LIMIT ?3",
            )
            .map_err(unavailable)?;
        let ids = statement
            .query_map(params![owner, project, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(unavailable)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(unavailable)?;
        ids.iter()
            .map(|id| {
                read_record(&conn, owner, id)?
                    .ok_or_else(|| unavailable("Goal disappeared during listing"))
            })
            .collect()
    }

    /// All worker and user mutations share one revision fence. The caller must
    /// persist run identities before dispatch and must stop after a lost CAS.
    pub fn save_goal(
        &self,
        owner: &str,
        proposed: &GoalRecord,
        expected_revision: u64,
    ) -> Result<GoalRecord, GoalStoreError> {
        validate_owner(owner)?;
        if expected_revision >= i64::MAX as u64 {
            return Err(conflict("Goal revision exhausted"));
        }
        if proposed.owner_key != owner || proposed.revision != expected_revision {
            return Err(conflict("Goal owner or revision does not match"));
        }
        if proposed
            .worker_epoch
            .as_ref()
            .is_some_and(|epoch| epoch.is_empty() || epoch.len() > 128 || epoch.contains('\0'))
        {
            return Err(invalid("worker_epoch exceeds its bound"));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let current = read_record(&tx, owner, &proposed.goal_id)?
            .ok_or_else(|| GoalStoreError::new("goal_not_found", "Goal was not found"))?;
        if current.revision != expected_revision {
            return Err(conflict("Goal changed; reload its latest revision"));
        }
        // A paused goal may still own an execution with an unknown outcome.
        // Only an explicit terminal goal releases its project to another goal.
        // The same transaction fences concurrent controllers and conceals the
        // identity of the other owner/goal when reporting contention.
        if proposed.worker_epoch.is_some() && !proposed.state.terminal() {
            let occupied: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM wc_goals WHERE project = ?1 AND goal_id <> ?2
                 AND worker_epoch IS NOT NULL AND state NOT IN ('cancelled', 'completed'))",
                    params![proposed.spec.project, proposed.goal_id],
                    |row| row.get(0),
                )
                .map_err(unavailable)?;
            if occupied {
                return Err(GoalStoreError::new("goal_project_busy", "Another goal still owns execution in this project; finish or cancel it before resuming this goal"));
            }
        }
        if proposed.spec != current.spec
            || proposed.created_at != current.created_at
            || proposed.deadline_at != current.deadline_at
        {
            return Err(conflict(
                "Goal acceptance, limits, and deadline are immutable",
            ));
        }
        if current.target.is_some() && proposed.target != current.target {
            return Err(conflict("Goal execution target is immutable once bound"));
        }
        if current.state.terminal() && proposed != &current {
            return Err(conflict(
                "Completed and cancelled goals cannot be changed or resumed",
            ));
        }
        if current.cancel_requested && !proposed.cancel_requested {
            return Err(conflict("Goal cancellation cannot be withdrawn"));
        }
        if proposed.rounds.len() < current.rounds.len()
            || proposed.rounds.len() > current.rounds.len() + 1
        {
            return Err(conflict("Goal rounds cannot be removed or skipped"));
        }
        let immutable_rounds = if proposed.rounds.len() > current.rounds.len() {
            if current
                .rounds
                .last()
                .is_some_and(|round| round.finished_at.is_none())
            {
                return Err(conflict(
                    "The active round must finish before a new round starts",
                ));
            }
            current.rounds.len()
        } else {
            current.rounds.len().saturating_sub(1)
        };
        if proposed.rounds[..immutable_rounds] != current.rounds[..immutable_rounds] {
            return Err(conflict("Previous round evidence is immutable"));
        }
        if let (Some(before), Some(after)) = (
            current.rounds.last(),
            proposed.rounds.get(current.rounds.len().saturating_sub(1)),
        ) {
            if before.number == after.number
                && (before.execution_run_id.is_some()
                    && before.execution_run_id != after.execution_run_id
                    || before.review_run_id.is_some()
                        && before.review_run_id != after.review_run_id)
            {
                return Err(conflict(
                    "A persisted round run identity cannot be replaced",
                ));
            }
        }
        let mut goal = proposed.clone();
        goal.revision = expected_revision + 1;
        let snapshot = encode(&goal)?;
        let changed = tx.execute(
            "UPDATE wc_goals SET state = ?1, revision = ?2, updated_at = ?3, worker_epoch = ?4, snapshot_json = ?5
             WHERE owner_key = ?6 AND goal_id = ?7 AND revision = ?8",
            params![goal.state.as_str(), goal.revision as i64, goal.updated_at, goal.worker_epoch, snapshot, owner, goal.goal_id, expected_revision as i64],
        ).map_err(unavailable)?;
        if changed != 1 {
            return Err(conflict("Goal changed; reload its latest revision"));
        }
        tx.commit().map_err(unavailable)?;
        Ok(goal)
    }

    pub fn claim_goal_worker(
        &self,
        owner: &str,
        goal_id: &str,
        expected_revision: u64,
        worker_epoch: &str,
    ) -> Result<GoalRecord, GoalStoreError> {
        let mut goal = self
            .get_goal(owner, goal_id)?
            .ok_or_else(|| GoalStoreError::new("goal_not_found", "Goal was not found"))?;
        if goal.revision != expected_revision {
            return Err(conflict("Goal changed; reload its latest revision"));
        }
        if !goal.state.active() {
            return Err(conflict("Only active goals can claim a worker"));
        }
        if goal.worker_epoch.is_some() {
            return Err(conflict("Goal already has a controlling worker"));
        }
        goal.worker_epoch = Some(worker_epoch.to_string());
        self.save_goal(owner, &goal, expected_revision)
    }
}
