//! Owner-scoped, revision-fenced host goals. Plan publication never authorizes execution.

use super::{Database, GoalStoreError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use webcodex_core::goal::GoalCriterion;
use webcodex_core::host_goal::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostGoalMutation {
    pub goal: HostGoalRecord,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostGoalPlanMutation {
    pub goal: HostGoalRecord,
    pub plan: HostGoalPlanVersion,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostGoalClaimMutation {
    pub goal: HostGoalRecord,
    pub attempt: HostGoalAttempt,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostGoalOperationMutation {
    pub operation: HostGoalOperation,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGoalCheckpointAction {
    Progress,
    Handoff,
    Pause,
    Block,
    Cancel,
    Replan,
}

fn unavailable(error: impl std::fmt::Display) -> GoalStoreError {
    tracing::warn!(error = %error, "host goal store operation failed");
    GoalStoreError::new("goal_store_unavailable", "Host goal storage is unavailable")
}
fn invalid(message: impl Into<String>) -> GoalStoreError {
    GoalStoreError::new("invalid_goal", message)
}
fn conflict(message: impl Into<String>) -> GoalStoreError {
    GoalStoreError::new("goal_conflict", message)
}
fn not_found() -> GoalStoreError {
    GoalStoreError::new("goal_not_found", "Goal was not found")
}
fn bounded(value: &str, max: usize, name: &str) -> Result<(), GoalStoreError> {
    host_goal_bounded(value, max, name).map_err(invalid)
}
fn encode<T: Serialize>(value: &T) -> Result<String, GoalStoreError> {
    serde_json::to_string(value).map_err(unavailable)
}
fn decode<T: DeserializeOwned>(value: String) -> Result<T, GoalStoreError> {
    serde_json::from_str(&value).map_err(unavailable)
}
fn hash(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}
fn request_hash<T: Serialize>(value: &T) -> Result<String, GoalStoreError> {
    Ok(hash(encode(value)?))
}

fn read(
    conn: &Connection,
    owner: &str,
    id: &str,
) -> Result<Option<HostGoalRecord>, GoalStoreError> {
    bounded(owner, 512, "owner")?;
    let row: Option<(String, u64)> = conn
        .query_row(
            "SELECT snapshot_json, revision FROM wc_host_goals WHERE owner_key=?1 AND goal_id=?2",
            params![owner, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    row.map(|(value, revision)| {
        let mut goal: HostGoalRecord = decode(value)?;
        if goal.revision != revision {
            return Err(unavailable("Host goal revision mismatch"));
        }
        goal.owner_key = owner.into();
        Ok(goal)
    })
    .transpose()
}

fn required(
    conn: &Connection,
    owner: &str,
    id: &str,
    revision: u64,
) -> Result<HostGoalRecord, GoalStoreError> {
    let goal = read(conn, owner, id)?.ok_or_else(not_found)?;
    if goal.revision != revision {
        return Err(conflict(
            "Goal changed; read its current revision before retrying",
        ));
    }
    Ok(goal)
}

fn save(conn: &Connection, goal: &mut HostGoalRecord, now: i64) -> Result<(), GoalStoreError> {
    goal.state.validate().map_err(invalid)?;
    let old_revision = goal.revision;
    goal.revision = goal
        .revision
        .checked_add(1)
        .filter(|v| *v <= i64::MAX as u64)
        .ok_or_else(|| conflict("Goal revision exhausted"))?;
    goal.updated_at = now;
    let count = conn.execute(
        "UPDATE wc_host_goals SET phase=?3,revision=?4,updated_at=?5,snapshot_json=?6 WHERE owner_key=?1 AND goal_id=?2 AND revision=?7",
        params![goal.owner_key, goal.goal_id, goal.phase.as_str(), goal.revision, now, encode(goal)?, old_revision],
    ).map_err(unavailable)?;
    if count != 1 {
        return Err(conflict("Goal changed while saving"));
    }
    Ok(())
}

fn read_plan(
    conn: &Connection,
    goal: &HostGoalRecord,
    version: u64,
) -> Result<Option<HostGoalPlanVersion>, GoalStoreError> {
    conn.query_row(
        "SELECT snapshot_json FROM wc_host_goal_plans WHERE goal_id=?1 AND version=?2",
        params![goal.goal_id, version],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(unavailable)?
    .map(decode)
    .transpose()
}

fn current_plan(
    conn: &Connection,
    goal: &HostGoalRecord,
) -> Result<HostGoalPlanVersion, GoalStoreError> {
    read_plan(conn, goal, goal.plan_version)?.ok_or_else(|| conflict("Goal has no saved plan"))
}

fn replay<T: DeserializeOwned>(
    conn: &Connection,
    owner: &str,
    scope: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Option<T>, GoalStoreError> {
    bounded(owner, 512, "owner")?;
    bounded(key, 128, "idempotency_key")?;
    let stored: Option<(String, String)> = conn.query_row(
        "SELECT request_hash,result_json FROM wc_host_goal_replays WHERE owner_key=?1 AND scope=?2 AND key_hash=?3",
        params![owner, scope, hash(key)], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional().map_err(unavailable)?;
    stored
        .map(|(old, result)| {
            if old != fingerprint {
                return Err(GoalStoreError::new(
                    "goal_idempotency_conflict",
                    "Idempotency key was already used with a different request",
                ));
            }
            decode(result)
        })
        .transpose()
}

fn remember<T: Serialize>(
    conn: &Connection,
    owner: &str,
    scope: &str,
    key: &str,
    fingerprint: &str,
    result: &T,
) -> Result<(), GoalStoreError> {
    conn.execute("INSERT INTO wc_host_goal_replays(owner_key,scope,key_hash,request_hash,result_json) VALUES(?1,?2,?3,?4,?5)", params![owner, scope, hash(key), fingerprint, encode(result)?]).map_err(unavailable)?;
    Ok(())
}

fn select_current(
    conn: &Connection,
    goal: &HostGoalRecord,
    now: i64,
) -> Result<(), GoalStoreError> {
    conn.execute("INSERT INTO wc_host_goal_current(owner_key,goal_id,selected_at) VALUES(?1,?2,?3) ON CONFLICT(owner_key) DO UPDATE SET goal_id=excluded.goal_id,selected_at=excluded.selected_at", params![goal.owner_key,goal.goal_id,now]).map_err(unavailable)?;
    Ok(())
}

fn unresolved(conn: &Connection, id: &str) -> Result<bool, GoalStoreError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM wc_host_goal_operations WHERE goal_id=?1 AND terminal=0)",
        [id],
        |r| r.get(0),
    )
    .map_err(unavailable)
}

fn fence_attempt(
    conn: &Connection,
    goal: &mut HostGoalRecord,
    now: i64,
) -> Result<(), GoalStoreError> {
    if let Some(id) = goal.current_attempt_id.take() {
        let value: Option<String> = conn.query_row("SELECT snapshot_json FROM wc_host_goal_attempts WHERE goal_id=?1 AND attempt_id=?2", params![goal.goal_id, id], |r| r.get(0)).optional().map_err(unavailable)?;
        if let Some(value) = value {
            let mut attempt: HostGoalAttempt = decode(value)?;
            if attempt.status == HostGoalAttemptStatus::Running {
                attempt.status = HostGoalAttemptStatus::Interrupted;
                attempt.finished_at = Some(now);
                conn.execute(
                    "UPDATE wc_host_goal_attempts SET snapshot_json=?2 WHERE attempt_id=?1",
                    params![id, encode(&attempt)?],
                )
                .map_err(unavailable)?;
            }
        }
    }
    goal.fence = goal
        .fence
        .checked_add(1)
        .filter(|v| *v <= i64::MAX as u64)
        .ok_or_else(|| conflict("Goal execution fence exhausted"))?;
    Ok(())
}

fn check_binding(
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
) -> Result<(), GoalStoreError> {
    if goal.goal_id != binding.goal_id
        || goal.plan_version != binding.plan_version
        || goal.fence != binding.fence
        || goal.current_node_id.as_deref() != Some(binding.node_id.as_str())
        || goal.current_attempt_id.as_deref() != Some(binding.attempt_id.as_str())
    {
        return Err(conflict(
            "Stale node attempt; read the current goal before continuing",
        ));
    }
    if !goal.confirmed()
        || !matches!(
            goal.phase,
            HostGoalPhase::Executing | HostGoalPhase::Verifying
        )
    {
        return Err(conflict(
            "Execution requires the user's confirmation of the saved current plan",
        ));
    }
    Ok(())
}

fn add_checkpoint(
    conn: &Connection,
    goal: &HostGoalRecord,
    reason: &str,
    now: i64,
) -> Result<HostGoalCheckpoint, GoalStoreError> {
    bounded(reason, 4096, "checkpoint reason")?;
    let mut checkpoint = HostGoalCheckpoint {
        sequence: 0,
        goal_id: goal.goal_id.clone(),
        revision: goal.revision,
        plan_version: goal.plan_version,
        state: goal.state.clone(),
        reason: reason.into(),
        created_at: now,
    };
    conn.execute(
        "INSERT INTO wc_host_goal_checkpoints(goal_id,snapshot_json) VALUES(?1,?2)",
        params![goal.goal_id, encode(&checkpoint)?],
    )
    .map_err(unavailable)?;
    checkpoint.sequence = conn.last_insert_rowid() as u64;
    conn.execute(
        "UPDATE wc_host_goal_checkpoints SET snapshot_json=?2 WHERE sequence=?1",
        params![checkpoint.sequence, encode(&checkpoint)?],
    )
    .map_err(unavailable)?;
    Ok(checkpoint)
}

impl Database {
    pub(super) fn ensure_host_goal_schema(conn: &mut Connection) -> anyhow::Result<()> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_host_goals (
            goal_id TEXT PRIMARY KEY,owner_key TEXT NOT NULL,project TEXT NOT NULL,project_root TEXT NOT NULL,
            phase TEXT NOT NULL,revision INTEGER NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,snapshot_json TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS wc_host_goals_owner ON wc_host_goals(owner_key,updated_at DESC,goal_id);
            CREATE TABLE IF NOT EXISTS wc_host_goal_plans (goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),version INTEGER NOT NULL,snapshot_json TEXT NOT NULL,PRIMARY KEY(goal_id,version));
            CREATE TABLE IF NOT EXISTS wc_host_goal_attempts (attempt_id TEXT PRIMARY KEY,goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),snapshot_json TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS wc_host_goal_attempts_goal ON wc_host_goal_attempts(goal_id);
            CREATE TABLE IF NOT EXISTS wc_host_goal_checkpoints (sequence INTEGER PRIMARY KEY AUTOINCREMENT,goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),snapshot_json TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS wc_host_goal_checkpoints_goal ON wc_host_goal_checkpoints(goal_id,sequence);
            CREATE TABLE IF NOT EXISTS wc_host_goal_operations (sequence INTEGER PRIMARY KEY AUTOINCREMENT,goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),operation_id TEXT NOT NULL,terminal INTEGER NOT NULL,snapshot_json TEXT NOT NULL,UNIQUE(goal_id,operation_id));
            CREATE INDEX IF NOT EXISTS wc_host_goal_operations_goal ON wc_host_goal_operations(goal_id,terminal,sequence);
            CREATE TABLE IF NOT EXISTS wc_host_goal_replays (owner_key TEXT NOT NULL,scope TEXT NOT NULL,key_hash TEXT NOT NULL,request_hash TEXT NOT NULL,result_json TEXT NOT NULL,PRIMARY KEY(owner_key,scope,key_hash));
            CREATE TABLE IF NOT EXISTS wc_host_goal_check_requests (owner_key TEXT NOT NULL,goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),key_hash TEXT NOT NULL,request_hash TEXT NOT NULL,result_json TEXT,PRIMARY KEY(owner_key,goal_id,key_hash));
            CREATE TABLE IF NOT EXISTS wc_host_goal_checkpoint_requests (owner_key TEXT NOT NULL,goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),key_hash TEXT NOT NULL,request_hash TEXT NOT NULL,result_json TEXT,PRIMARY KEY(owner_key,goal_id,key_hash));
            CREATE TABLE IF NOT EXISTS wc_host_goal_current (owner_key TEXT PRIMARY KEY,goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),selected_at INTEGER NOT NULL);")?;
        Ok(())
    }

    pub fn host_goal_plan_root(&self) -> Result<PathBuf, GoalStoreError> {
        if !self.state_path.is_absolute() || self.state_path.file_name().is_none() {
            return Err(invalid(
                "Host goal plans require a persistent database with an absolute path",
            ));
        }
        Ok(self
            .state_path
            .parent()
            .ok_or_else(|| invalid("Database has no persistent parent"))?
            .join("goals"))
    }

    pub fn create_host_goal_at(
        &self,
        owner: &str,
        project: &str,
        project_root: &str,
        objective: &str,
        key: &str,
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        bounded(project, 512, "project")?;
        bounded(project_root, 4096, "project_root")?;
        bounded(objective, 16 * 1024, "objective")?;
        let fingerprint = request_hash(&(project, project_root, objective))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(mut value) =
            replay::<HostGoalMutation>(&tx, owner, "create", key, &fingerprint)?
        {
            value.goal.owner_key = owner.into();
            value.replayed = true;
            return Ok(value);
        }
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM wc_host_goals", [], |r| r.get(0))
            .map_err(unavailable)?;
        if count >= 4096 {
            return Err(invalid("Host goal storage limit reached"));
        }
        let goal = HostGoalRecord {
            goal_id: format!("hgoal_{}", uuid::Uuid::new_v4().simple()), owner_key: owner.into(), project: project.into(), project_root: project_root.into(), objective: objective.into(), phase: HostGoalPhase::Planning, revision: 1, plan_version: 0, acceptance_contract_version: 0, plan_hash: None, plan_materialized: false, confirmation: None, current_node_id: None, current_attempt_id: None, fence: 0, completed_node_ids: Vec::new(), state: HostGoalState { next_step: "Discuss requirements and save a plan with final acceptance criteria; wait for the user's explicit instruction to start.".into(), ..Default::default() }, created_at: now, updated_at: now,
        };
        tx.execute("INSERT INTO wc_host_goals(goal_id,owner_key,project,project_root,phase,revision,created_at,updated_at,snapshot_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8)",params![goal.goal_id,owner,project,project_root,goal.phase.as_str(),goal.revision,now,encode(&goal)?]).map_err(unavailable)?;
        select_current(&tx, &goal, now)?;
        let result = HostGoalMutation {
            goal,
            replayed: false,
        };
        remember(&tx, owner, "create", key, &fingerprint, &result)?;
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn get_host_goal(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<Option<HostGoalRecord>, GoalStoreError> {
        read(&self.conn.lock().unwrap(), owner, id)
    }

    pub fn list_host_goals(
        &self,
        owner: &str,
        project: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HostGoalRecord>, GoalStoreError> {
        bounded(owner, 512, "owner")?;
        if !(1..=HOST_GOAL_MAX_PAGE).contains(&limit) || offset > 4096 {
            return Err(invalid("Invalid host goal page"));
        }
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare("SELECT goal_id FROM wc_host_goals WHERE owner_key=?1 AND (?2 IS NULL OR project=?2) ORDER BY updated_at DESC,goal_id LIMIT ?3 OFFSET ?4").map_err(unavailable)?;
        let ids = statement
            .query_map(params![owner, project, limit as i64, offset as i64], |r| {
                r.get::<_, String>(0)
            })
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        ids.into_iter()
            .map(|id| read(&conn, owner, &id)?.ok_or_else(not_found))
            .collect()
    }

    pub fn current_host_goal(&self, owner: &str) -> Result<Option<HostGoalRecord>, GoalStoreError> {
        bounded(owner, 512, "owner")?;
        let conn = self.conn.lock().unwrap();
        let id: Option<String> = conn
            .query_row(
                "SELECT goal_id FROM wc_host_goal_current WHERE owner_key=?1",
                [owner],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        id.map(|id| read(&conn, owner, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn select_current_host_goal(
        &self,
        owner: &str,
        id: &str,
        now: i64,
    ) -> Result<CurrentHostGoalRef, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        let goal = read(&conn, owner, id)?.ok_or_else(not_found)?;
        select_current(&conn, &goal, now)?;
        Ok(CurrentHostGoalRef {
            goal_id: goal.goal_id,
            project: goal.project,
            project_root: goal.project_root,
            selected_at: now,
        })
    }

    pub fn get_host_goal_plan(
        &self,
        owner: &str,
        id: &str,
        version: Option<u64>,
    ) -> Result<Option<HostGoalPlanVersion>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        let goal = read(&conn, owner, id)?.ok_or_else(not_found)?;
        read_plan(&conn, &goal, version.unwrap_or(goal.plan_version))
    }

    pub fn save_host_goal_plan(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        plan: &HostGoalPlan,
        change_request: Option<&str>,
        key: &str,
        now: i64,
    ) -> Result<HostGoalPlanMutation, GoalStoreError> {
        plan.validate().map_err(invalid)?;
        if let Some(request) = change_request {
            bounded(request, 4096, "explicit user change request")?;
        }
        let fingerprint = request_hash(&(revision, plan, change_request))?;
        let scope = format!("{id}:plan");
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(mut value) =
            replay::<HostGoalPlanMutation>(&tx, owner, &scope, key, &fingerprint)?
        {
            value.goal.owner_key = owner.into();
            value.replayed = true;
            return Ok(value);
        }
        let mut goal = required(&tx, owner, id, revision)?;
        if goal.phase == HostGoalPhase::Cancelled {
            return Err(conflict("Cancelled goal cannot be replanned"));
        }
        if unresolved(&tx, id)? {
            return Err(conflict(
                "Resolve pending operations before changing the plan",
            ));
        }
        let mut acceptance_changes = Vec::new();
        if goal.plan_version > 0 {
            let old = current_plan(&tx, &goal)?;
            if old.plan.objective != plan.objective {
                acceptance_changes.push(
                    "Objective changed; compare this version with the preceding plan.".into(),
                );
            }
            for prior in &old.plan.final_criteria {
                match plan
                    .final_criteria
                    .iter()
                    .find(|criterion| criterion.id() == prior.id())
                {
                    None => {
                        acceptance_changes.push(format!("Removed final criterion: {}", prior.id()))
                    }
                    Some(current) if current != prior => {
                        acceptance_changes.push(format!("Changed final criterion: {}", prior.id()))
                    }
                    Some(_) => {}
                }
            }
            for current in &plan.final_criteria {
                if !old
                    .plan
                    .final_criteria
                    .iter()
                    .any(|prior| prior.id() == current.id())
                {
                    acceptance_changes.push(format!("Added final criterion: {}", current.id()));
                }
            }
            // Reordering identical criteria is not a new acceptance contract.
            if !acceptance_changes.is_empty() {
                if change_request.is_none() {
                    return Err(conflict("Changing the objective or final acceptance contract requires an explicit user change_request and renewed confirmation after publication"));
                }
                goal.acceptance_contract_version += 1;
            }
        }
        fence_attempt(&tx, &mut goal, now)?;
        goal.plan_version += 1;
        if goal.acceptance_contract_version == 0 {
            goal.acceptance_contract_version = 1;
        }
        let version = HostGoalPlanVersion {
            goal_id: id.into(),
            version: goal.plan_version,
            preparation_revision: goal.revision + 1,
            acceptance_contract_version: goal.acceptance_contract_version,
            change_request: change_request.map(str::to_string),
            acceptance_changes,
            hash: request_hash(plan)?,
            plan: plan.clone(),
            materialization: None,
            created_at: now,
        };
        goal.plan_hash = Some(version.hash.clone());
        goal.objective = plan.objective.clone();
        goal.plan_materialized = false;
        goal.confirmation = None;
        goal.phase = HostGoalPhase::Planning;
        goal.current_node_id = None;
        goal.completed_node_ids.clear();
        goal.state.next_step="Materialize and verify both local plan files; then wait for the user to confirm this exact plan.".into();
        tx.execute(
            "INSERT INTO wc_host_goal_plans(goal_id,version,snapshot_json) VALUES(?1,?2,?3)",
            params![id, version.version, encode(&version)?],
        )
        .map_err(unavailable)?;
        save(&tx, &mut goal, now)?;
        add_checkpoint(
            &tx,
            &goal,
            "Saved immutable plan version; execution remains disabled",
            now,
        )?;
        let result = HostGoalPlanMutation {
            goal,
            plan: version,
            replayed: false,
        };
        remember(&tx, owner, &scope, key, &fingerprint, &result)?;
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn materialize_host_goal_plan(
        &self,
        owner: &str,
        id: &str,
        version: u64,
        plan_hash: &str,
        receipt: &HostGoalMaterialization,
        now: i64,
    ) -> Result<HostGoalPlanMutation, GoalStoreError> {
        for (label, value) in [
            ("json_path", &receipt.json_path),
            ("markdown_path", &receipt.markdown_path),
        ] {
            bounded(value, 4096, label)?;
        }
        for value in [&receipt.json_hash, &receipt.markdown_hash] {
            if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("Materialized file hashes must be SHA-256"));
            }
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let mut goal = read(&tx, owner, id)?.ok_or_else(not_found)?;
        if goal.plan_version != version || goal.plan_hash.as_deref() != Some(plan_hash) {
            return Err(conflict("Saved plan was superseded during publication"));
        }
        let mut plan = current_plan(&tx, &goal)?;
        if let Some(previous) = &plan.materialization {
            if previous.json_path != receipt.json_path
                || previous.json_hash != receipt.json_hash
                || previous.markdown_path != receipt.markdown_path
                || previous.markdown_hash != receipt.markdown_hash
            {
                return Err(conflict(
                    "Plan materialization differs from the original publication",
                ));
            }
            return Ok(HostGoalPlanMutation {
                goal,
                plan,
                replayed: true,
            });
        }
        if goal.phase != HostGoalPhase::Planning || goal.revision != plan.preparation_revision {
            return Err(conflict("Goal changed during plan publication; read current state before publishing a new version"));
        }
        plan.materialization = Some(receipt.clone());
        goal.plan_materialized = true;
        goal.phase = HostGoalPhase::AwaitingConfirmation;
        goal.confirmation = None;
        goal.state.next_step="Read the local plan and wait for the user to explicitly confirm starting execution in the conversation.".into();
        tx.execute(
            "UPDATE wc_host_goal_plans SET snapshot_json=?3 WHERE goal_id=?1 AND version=?2",
            params![id, version, encode(&plan)?],
        )
        .map_err(unavailable)?;
        save(&tx, &mut goal, now)?;
        add_checkpoint(
            &tx,
            &goal,
            "Local plan verified; awaiting explicit user confirmation",
            now,
        )?;
        tx.commit().map_err(unavailable)?;
        Ok(HostGoalPlanMutation {
            goal,
            plan,
            replayed: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn confirm_host_goal_plan(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        version: u64,
        plan_hash: &str,
        user_message: &str,
        key: &str,
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        bounded(user_message, 4096, "explicit user confirmation")?;
        let fingerprint = request_hash(&(revision, version, plan_hash, user_message))?;
        let scope = format!("{id}:confirm");
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(mut result) = replay::<HostGoalMutation>(&tx, owner, &scope, key, &fingerprint)?
        {
            result.goal.owner_key = owner.into();
            result.replayed = true;
            return Ok(result);
        }
        let mut goal = required(&tx, owner, id, revision)?;
        if !matches!(
            goal.phase,
            HostGoalPhase::AwaitingConfirmation | HostGoalPhase::Paused | HostGoalPhase::Blocked
        ) || !goal.plan_materialized
            || goal.confirmation.is_some()
            || goal.plan_version != version
            || goal.plan_hash.as_deref() != Some(plan_hash)
        {
            return Err(conflict(
                "Confirmation must identify the published current plan awaiting user approval",
            ));
        }
        goal.confirmation = Some(HostGoalConfirmation {
            plan_version: version,
            plan_hash: plan_hash.into(),
            user_message: user_message.into(),
            confirmed_at: now,
        });
        goal.phase = HostGoalPhase::Ready;
        goal.state.next_step =
            "Claim the first incomplete node and execute the confirmed plan.".into();
        save(&tx, &mut goal, now)?;
        add_checkpoint(&tx, &goal, "User confirmed the saved plan", now)?;
        let result = HostGoalMutation {
            goal,
            replayed: false,
        };
        remember(&tx, owner, &scope, key, &fingerprint, &result)?;
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_host_goal_node(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        node_id: &str,
        rework: bool,
        key: &str,
        now: i64,
    ) -> Result<HostGoalClaimMutation, GoalStoreError> {
        self.claim_host_goal_node_impl(owner, id, revision, node_id, rework, None, None, key, now)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn lookup_host_goal_claim_request(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        node_id: &str,
        rework: bool,
        plan_version: u64,
        takeover_attempt_id: Option<&str>,
        key: &str,
    ) -> Result<Option<HostGoalClaimMutation>, GoalStoreError> {
        let fingerprint = request_hash(&(
            revision,
            node_id,
            rework,
            Some(plan_version),
            takeover_attempt_id,
        ))?;
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        let result = replay::<HostGoalClaimMutation>(
            &conn,
            owner,
            &format!("{id}:claim"),
            key,
            &fingerprint,
        )?;
        Ok(result.map(|mut result| {
            result.goal.owner_key = owner.into();
            result.replayed = true;
            result
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_host_goal_node_for_plan(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        node_id: &str,
        rework: bool,
        plan_version: u64,
        takeover_attempt_id: Option<&str>,
        key: &str,
        now: i64,
    ) -> Result<HostGoalClaimMutation, GoalStoreError> {
        self.claim_host_goal_node_impl(
            owner,
            id,
            revision,
            node_id,
            rework,
            Some(plan_version),
            takeover_attempt_id,
            key,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn claim_host_goal_node_impl(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        node_id: &str,
        rework: bool,
        plan_version: Option<u64>,
        takeover_attempt_id: Option<&str>,
        key: &str,
        now: i64,
    ) -> Result<HostGoalClaimMutation, GoalStoreError> {
        let fingerprint =
            request_hash(&(revision, node_id, rework, plan_version, takeover_attempt_id))?;
        let scope = format!("{id}:claim");
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(mut result) =
            replay::<HostGoalClaimMutation>(&tx, owner, &scope, key, &fingerprint)?
        {
            result.goal.owner_key = owner.into();
            result.replayed = true;
            return Ok(result);
        }
        let mut goal = required(&tx, owner, id, revision)?;
        if let Some(version) = plan_version {
            if goal.plan_version != version
                || goal.current_attempt_id.as_deref() != takeover_attempt_id
            {
                return Err(conflict(
                    "Claim must identify the current plan and exact attempt being taken over",
                ));
            }
        }
        if !goal.confirmed()
            || matches!(
                goal.phase,
                HostGoalPhase::Planning
                    | HostGoalPhase::AwaitingConfirmation
                    | HostGoalPhase::Cancelled
            )
            || (goal.phase == HostGoalPhase::Completed && !rework)
        {
            return Err(conflict(
                "Claim requires the user's confirmation of the published plan",
            ));
        }
        if unresolved(&tx, id)? {
            return Err(conflict(
                "Reconcile unresolved operations before claiming a new attempt",
            ));
        }
        let plan = current_plan(&tx, &goal)?;
        let index = plan
            .plan
            .nodes
            .iter()
            .position(|n| n.id == node_id)
            .ok_or_else(|| invalid("Unknown node"))?;
        if rework {
            goal.completed_node_ids
                .retain(|id| plan.plan.nodes[..index].iter().any(|n| &n.id == id));
        }
        let first = plan
            .plan
            .nodes
            .iter()
            .find(|n| !goal.completed_node_ids.contains(&n.id));
        if first.map(|n| n.id.as_str()) != Some(node_id) {
            return Err(conflict("Serial nodes must pass in order"));
        }
        fence_attempt(&tx, &mut goal, now)?;
        let binding = HostGoalExecutionBinding {
            goal_id: id.into(),
            node_id: node_id.into(),
            attempt_id: format!("hattempt_{}", uuid::Uuid::new_v4().simple()),
            plan_version: goal.plan_version,
            fence: goal.fence,
        };
        let attempt = HostGoalAttempt {
            binding: binding.clone(),
            status: HostGoalAttemptStatus::Running,
            checks: Vec::new(),
            final_checks: Vec::new(),
            started_at: now,
            finished_at: None,
        };
        goal.current_node_id = Some(node_id.into());
        goal.current_attempt_id = Some(binding.attempt_id.clone());
        goal.phase = HostGoalPhase::Executing;
        goal.state.next_step = plan.plan.nodes[index].work.clone();
        tx.execute(
            "INSERT INTO wc_host_goal_attempts(attempt_id,goal_id,snapshot_json) VALUES(?1,?2,?3)",
            params![binding.attempt_id, id, encode(&attempt)?],
        )
        .map_err(unavailable)?;
        save(&tx, &mut goal, now)?;
        select_current(&tx, &goal, now)?;
        add_checkpoint(&tx, &goal, "Claimed serial node attempt", now)?;
        let result = HostGoalClaimMutation {
            goal,
            attempt,
            replayed: false,
        };
        remember(&tx, owner, &scope, key, &fingerprint, &result)?;
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn checkpoint_host_goal(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        binding: Option<&HostGoalExecutionBinding>,
        state: &HostGoalState,
        action: HostGoalCheckpointAction,
        reason: &str,
        key: &str,
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        self.checkpoint_host_goal_impl(
            owner,
            id,
            revision,
            binding,
            state,
            action,
            reason,
            key,
            None,
            &[],
            now,
        )
    }

    pub fn lookup_host_goal_checkpoint_replay(
        &self,
        owner: &str,
        id: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<HostGoalMutation>, GoalStoreError> {
        bounded(key, 128, "idempotency_key")?;
        bounded(request_hash, 128, "request_hash")?;
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        client_request_replay(
            &conn,
            "wc_host_goal_checkpoint_requests",
            owner,
            id,
            key,
            request_hash,
        )
    }

    pub fn reserve_host_goal_checkpoint_request(
        &self,
        owner: &str,
        id: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<HostGoalMutation>, GoalStoreError> {
        bounded(key, 128, "idempotency_key")?;
        bounded(request_hash, 128, "request_hash")?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        read(&tx, owner, id)?.ok_or_else(not_found)?;
        let replay = client_request_replay(
            &tx,
            "wc_host_goal_checkpoint_requests",
            owner,
            id,
            key,
            request_hash,
        )?;
        tx.execute("INSERT OR IGNORE INTO wc_host_goal_checkpoint_requests(owner_key,goal_id,key_hash,request_hash) VALUES(?1,?2,?3,?4)",params![owner,id,hash(key),request_hash]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(replay)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn checkpoint_host_goal_with_request(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        binding: Option<&HostGoalExecutionBinding>,
        state: &HostGoalState,
        action: HostGoalCheckpointAction,
        reason: &str,
        key: &str,
        client_request_hash: &str,
        resolutions: &[HostGoalOperationResolution],
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        bounded(client_request_hash, 128, "request_hash")?;
        self.checkpoint_host_goal_impl(
            owner,
            id,
            revision,
            binding,
            state,
            action,
            reason,
            key,
            Some(client_request_hash),
            resolutions,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn checkpoint_host_goal_impl(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        binding: Option<&HostGoalExecutionBinding>,
        state: &HostGoalState,
        action: HostGoalCheckpointAction,
        reason: &str,
        key: &str,
        client_request_hash: Option<&str>,
        resolutions: &[HostGoalOperationResolution],
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        state.validate().map_err(invalid)?;
        bounded(reason, 4096, "checkpoint reason")?;
        if resolutions.len() > 8 {
            return Err(invalid(
                "At most eight operation resolutions per checkpoint",
            ));
        }
        let fingerprint = request_hash(&(revision, binding, state, action, reason, resolutions))?;
        let scope = format!("{id}:checkpoint");
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(request_hash) = client_request_hash {
            read(&tx, owner, id)?.ok_or_else(not_found)?;
            if let Some(result) = client_request_replay(
                &tx,
                "wc_host_goal_checkpoint_requests",
                owner,
                id,
                key,
                request_hash,
            )? {
                return Ok(result);
            }
            tx.execute("INSERT OR IGNORE INTO wc_host_goal_checkpoint_requests(owner_key,goal_id,key_hash,request_hash) VALUES(?1,?2,?3,?4)",params![owner,id,hash(key),request_hash]).map_err(unavailable)?;
        }
        if let Some(mut result) = replay::<HostGoalMutation>(&tx, owner, &scope, key, &fingerprint)?
        {
            result.goal.owner_key = owner.into();
            result.replayed = true;
            return Ok(result);
        }
        let mut goal = required(&tx, owner, id, revision)?;
        if goal.phase.terminal() {
            return Err(conflict(
                "Terminal goals require explicit rework before progress updates",
            ));
        }
        if let Some(binding) = binding {
            check_binding(&goal, binding)?;
        } else if matches!(
            goal.phase,
            HostGoalPhase::Executing | HostGoalPhase::Verifying
        ) {
            return Err(conflict(
                "Executing goal checkpoints require the current node binding",
            ));
        }
        let mut resolution_ids = std::collections::BTreeSet::new();
        for resolution in resolutions {
            bounded(&resolution.operation_id, 128, "operation_id")?;
            bounded(&resolution.report, 8192, "operator resolution report")?;
            if !resolution.status.terminal() || !resolution_ids.insert(&resolution.operation_id) {
                return Err(invalid(
                    "Operation resolutions require unique IDs and terminal outcomes",
                ));
            }
            let value:Option<String>=tx.query_row("SELECT snapshot_json FROM wc_host_goal_operations WHERE goal_id=?1 AND operation_id=?2",params![id,resolution.operation_id],|row|row.get(0)).optional().map_err(unavailable)?;
            let mut operation: HostGoalOperation = decode(value.ok_or_else(not_found)?)?;
            if operation.tool_name == "coding_agent_start"
                && operation
                    .result_summary
                    .as_deref()
                    .and_then(|summary| serde_json::from_str::<serde_json::Value>(summary).ok())
                    .is_some_and(|value| {
                        value["source"] == "runtime_coding_agent"
                            && value["run_id"].as_str().is_some_and(|id| !id.is_empty())
                    })
            {
                return Err(GoalStoreError::new("goal_run_reconciliation_required","The native coding-agent run must be observed or cancelled through its authoritative run API; manual resolution cannot override it"));
            }
            if operation.job_id.is_some() {
                return Err(GoalStoreError::new("goal_job_reconciliation_required","The original tracked Job must be reconciled from its authoritative status; manual resolution cannot override it"));
            }
            if operation.operation_id.starts_with("check_")
                && resolution.status != HostGoalOperationStatus::Cancelled
            {
                return Err(GoalStoreError::new("goal_check_reconciliation_required","An unresolved internal check without a Job may only be explicitly cancelled; success or failure requires authoritative Job evidence"));
            }
            if !matches!(
                operation.status,
                HostGoalOperationStatus::Intent | HostGoalOperationStatus::Unknown
            ) {
                return Err(conflict("Manual resolution is restricted to unresolved business operations without a Job; internal checks require authoritative Job evidence"));
            }
            operation.status = resolution.status;
            operation.result_summary = Some(resolution.report.clone());
            operation.updated_at = now;
            tx.execute(
                "UPDATE wc_host_goal_operations SET terminal=1,snapshot_json=?2 WHERE sequence=?1",
                params![operation.sequence, encode(&operation)?],
            )
            .map_err(unavailable)?;
        }
        goal.state = state.clone();
        match action {
            HostGoalCheckpointAction::Progress | HostGoalCheckpointAction::Handoff => {}
            HostGoalCheckpointAction::Pause => {
                fence_attempt(&tx, &mut goal, now)?;
                goal.phase = HostGoalPhase::Paused;
            }
            HostGoalCheckpointAction::Block => {
                fence_attempt(&tx, &mut goal, now)?;
                goal.phase = HostGoalPhase::Blocked;
            }
            HostGoalCheckpointAction::Cancel => {
                if unresolved(&tx, id)? {
                    return Err(conflict("Resolve pending operations before cancellation"));
                }
                fence_attempt(&tx, &mut goal, now)?;
                goal.phase = HostGoalPhase::Cancelled;
            }
            HostGoalCheckpointAction::Replan => {
                fence_attempt(&tx, &mut goal, now)?;
                goal.phase = HostGoalPhase::Planning;
                goal.confirmation = None;
            }
        }
        save(&tx, &mut goal, now)?;
        add_checkpoint(&tx, &goal, reason, now)?;
        let result = HostGoalMutation {
            goal,
            replayed: false,
        };
        remember(&tx, owner, &scope, key, &fingerprint, &result)?;
        if let Some(request_hash) = client_request_hash {
            let updated=tx.execute("UPDATE wc_host_goal_checkpoint_requests SET result_json=?5 WHERE owner_key=?1 AND goal_id=?2 AND key_hash=?3 AND request_hash=?4 AND result_json IS NULL",params![owner,id,hash(key),request_hash,encode(&result)?]).map_err(unavailable)?;
            if updated != 1 {
                return Err(conflict("Checkpoint request receipt changed during commit"));
            }
        }
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn validate_host_goal_binding(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
    ) -> Result<HostGoalRecord, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        let goal = read(&conn, owner, &binding.goal_id)?.ok_or_else(not_found)?;
        check_binding(&goal, binding)?;
        Ok(goal)
    }

    pub fn list_host_goal_checkpoints(
        &self,
        owner: &str,
        id: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<HostGoalCheckpoint>, GoalStoreError> {
        if !(1..=HOST_GOAL_MAX_PAGE).contains(&limit) {
            return Err(invalid("History limit must be 1..100"));
        }
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        let mut statement=conn.prepare("SELECT snapshot_json FROM wc_host_goal_checkpoints WHERE goal_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3").map_err(unavailable)?;
        let rows = statement
            .query_map(params![id, after, limit as i64], |r| r.get::<_, String>(0))
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        rows.into_iter().map(decode).collect()
    }

    pub fn get_host_goal_attempt(
        &self,
        owner: &str,
        id: &str,
        attempt_id: &str,
    ) -> Result<Option<HostGoalAttempt>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        conn.query_row(
            "SELECT snapshot_json FROM wc_host_goal_attempts WHERE goal_id=?1 AND attempt_id=?2",
            params![id, attempt_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(unavailable)?
        .map(decode)
        .transpose()
    }

    pub fn latest_host_goal_attempt(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<Option<HostGoalAttempt>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        conn.query_row("SELECT snapshot_json FROM wc_host_goal_attempts WHERE goal_id=?1 ORDER BY rowid DESC LIMIT 1", [id], |r| r.get::<_, String>(0))
            .optional().map_err(unavailable)?.map(decode).transpose()
    }

    /// Attempts use their monotonic per-goal fence as a stable history cursor.
    pub fn list_host_goal_attempts(
        &self,
        owner: &str,
        id: &str,
        after_fence: u64,
        limit: usize,
    ) -> Result<Vec<HostGoalAttempt>, GoalStoreError> {
        if !(1..=HOST_GOAL_MAX_PAGE).contains(&limit) {
            return Err(invalid("History limit must be 1..100"));
        }
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        let mut statement = conn.prepare("SELECT snapshot_json FROM wc_host_goal_attempts WHERE goal_id=?1 AND json_extract(snapshot_json,'$.binding.fence')>?2 ORDER BY rowid LIMIT ?3").map_err(unavailable)?;
        let rows = statement
            .query_map(params![id, after_fence, limit as i64], |r| {
                r.get::<_, String>(0)
            })
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        rows.into_iter().map(decode).collect()
    }

    pub fn latest_host_goal_checkpoint(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<Option<HostGoalCheckpoint>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        conn.query_row("SELECT snapshot_json FROM wc_host_goal_checkpoints WHERE goal_id=?1 ORDER BY sequence DESC LIMIT 1", [id], |r| r.get::<_, String>(0))
            .optional().map_err(unavailable)?.map(decode).transpose()
    }

    pub fn get_host_goal_operation(
        &self,
        owner: &str,
        id: &str,
        operation_id: &str,
    ) -> Result<Option<HostGoalOperation>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        conn.query_row("SELECT snapshot_json FROM wc_host_goal_operations WHERE goal_id=?1 AND operation_id=?2", params![id, operation_id], |r| r.get::<_, String>(0))
            .optional().map_err(unavailable)?.map(decode).transpose()
    }

    /// Exact owned Job association, including terminal history for stop retries.
    pub fn find_host_goal_operation_by_job(
        &self,
        owner: &str,
        id: &str,
        job_id: &str,
    ) -> Result<Option<HostGoalOperation>, GoalStoreError> {
        bounded(job_id, 512, "job_id")?;
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        conn.query_row("SELECT snapshot_json FROM wc_host_goal_operations WHERE goal_id=?1 AND json_extract(snapshot_json,'$.job_id')=?2 ORDER BY sequence DESC LIMIT 1",params![id,job_id],|row|row.get::<_,String>(0)).optional().map_err(unavailable)?.map(decode).transpose()
    }

    pub fn find_host_goal_operation_by_native_run(
        &self,
        owner: &str,
        id: &str,
        run_id: &str,
    ) -> Result<Option<HostGoalOperation>, GoalStoreError> {
        bounded(run_id, 512, "run_id")?;
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        conn.query_row("SELECT snapshot_json FROM wc_host_goal_operations WHERE goal_id=?1 AND json_extract(snapshot_json,'$.tool_name')='coding_agent_start' AND CASE WHEN json_valid(json_extract(snapshot_json,'$.result_summary')) THEN json_extract(json_extract(snapshot_json,'$.result_summary'),'$.source')='runtime_coding_agent' AND json_extract(json_extract(snapshot_json,'$.result_summary'),'$.run_id')=?2 ELSE 0 END ORDER BY sequence DESC LIMIT 1",params![id,run_id],|row|row.get::<_,String>(0)).optional().map_err(unavailable)?.map(decode).transpose()
    }

    pub fn lookup_host_goal_check_replay(
        &self,
        owner: &str,
        id: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<HostGoalMutation>, GoalStoreError> {
        bounded(key, 128, "idempotency_key")?;
        bounded(request_hash, 128, "request_hash")?;
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        check_request_replay(&conn, owner, id, key, request_hash)
    }

    pub fn reserve_host_goal_check_request(
        &self,
        owner: &str,
        id: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<HostGoalMutation>, GoalStoreError> {
        bounded(key, 128, "idempotency_key")?;
        bounded(request_hash, 128, "request_hash")?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        read(&tx, owner, id)?.ok_or_else(not_found)?;
        let replay = check_request_replay(&tx, owner, id, key, request_hash)?;
        tx.execute("INSERT OR IGNORE INTO wc_host_goal_check_requests(owner_key,goal_id,key_hash,request_hash) VALUES(?1,?2,?3,?4)",params![owner,id,hash(key),request_hash]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(replay)
    }

    pub fn latest_passed_host_goal_node_attempt(
        &self,
        owner: &str,
        id: &str,
        node_id: &str,
        plan_version: u64,
    ) -> Result<Option<HostGoalAttempt>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        conn.query_row("SELECT snapshot_json FROM wc_host_goal_attempts WHERE goal_id=?1 AND json_extract(snapshot_json,'$.binding.node_id')=?2 AND json_extract(snapshot_json,'$.binding.plan_version')=?3 AND json_extract(snapshot_json,'$.status')='passed' ORDER BY rowid DESC LIMIT 1",params![id,node_id,plan_version],|r|r.get::<_,String>(0)).optional().map_err(unavailable)?.map(decode).transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_host_goal_operation(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        operation_id: &str,
        tool_name: &str,
        payload_hash: &str,
        now: i64,
    ) -> Result<HostGoalOperationMutation, GoalStoreError> {
        self.begin_host_goal_operation_with_manifest(
            owner,
            binding,
            operation_id,
            tool_name,
            payload_hash,
            &[],
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_host_goal_operation_with_manifest(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        operation_id: &str,
        tool_name: &str,
        payload_hash: &str,
        input_manifest: &[webcodex_core::goal::GoalArtifactSnapshot],
        now: i64,
    ) -> Result<HostGoalOperationMutation, GoalStoreError> {
        if input_manifest.len() > 64 {
            return Err(invalid(
                "Operation input manifest accepts at most 64 artifacts",
            ));
        }
        for artifact in input_manifest {
            webcodex_core::goal::validate_goal_relative_path(&artifact.path).map_err(invalid)?;
            if artifact.sha256.as_ref().is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            }) {
                return Err(invalid("Operation input fingerprints must be SHA-256"));
            }
        }
        bounded(operation_id, 128, "operation_id")?;
        bounded(tool_name, 128, "tool_name")?;
        bounded(payload_hash, 128, "payload_hash")?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let goal = read(&tx, owner, &binding.goal_id)?.ok_or_else(not_found)?;
        let old:Option<String>=tx.query_row("SELECT snapshot_json FROM wc_host_goal_operations WHERE goal_id=?1 AND operation_id=?2",params![binding.goal_id,operation_id],|r|r.get(0)).optional().map_err(unavailable)?;
        if let Some(old) = old {
            let operation: HostGoalOperation = decode(old)?;
            if operation.payload_hash != payload_hash
                || operation.tool_name != tool_name
                || operation.binding != *binding
                || operation.input_manifest != input_manifest
            {
                return Err(GoalStoreError::new(
                    "goal_idempotency_conflict",
                    "Operation ID already identifies another dispatch",
                ));
            }
            return Ok(HostGoalOperationMutation {
                operation,
                replayed: true,
            });
        }
        check_binding(&goal, binding)?;
        if unresolved(&tx, &binding.goal_id)? {
            return Err(conflict(
                "Resolve the previous operation before starting another serial operation",
            ));
        }
        let mut operation = HostGoalOperation {
            sequence: 0,
            operation_id: operation_id.into(),
            binding: binding.clone(),
            payload_hash: payload_hash.into(),
            tool_name: tool_name.into(),
            input_manifest: input_manifest.to_vec(),
            status: HostGoalOperationStatus::Intent,
            job_id: None,
            result_summary: None,
            created_at: now,
            updated_at: now,
        };
        tx.execute("INSERT INTO wc_host_goal_operations(goal_id,operation_id,terminal,snapshot_json) VALUES(?1,?2,0,?3)",params![binding.goal_id,operation_id,encode(&operation)?]).map_err(unavailable)?;
        operation.sequence = tx.last_insert_rowid() as u64;
        tx.execute(
            "UPDATE wc_host_goal_operations SET snapshot_json=?2 WHERE sequence=?1",
            params![operation.sequence, encode(&operation)?],
        )
        .map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(HostGoalOperationMutation {
            operation,
            replayed: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_host_goal_operation(
        &self,
        owner: &str,
        id: &str,
        operation_id: &str,
        status: HostGoalOperationStatus,
        job_id: Option<&str>,
        summary: Option<&str>,
        now: i64,
    ) -> Result<HostGoalOperation, GoalStoreError> {
        if let Some(id) = job_id {
            bounded(id, 512, "job_id")?;
        }
        if let Some(summary) = summary {
            if summary.len() > 8192 || summary.contains('\0') {
                return Err(invalid("Operation summary exceeds 8 KiB"));
            }
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        read(&tx, owner, id)?.ok_or_else(not_found)?;
        let value:Option<String>=tx.query_row("SELECT snapshot_json FROM wc_host_goal_operations WHERE goal_id=?1 AND operation_id=?2",params![id,operation_id],|r|r.get(0)).optional().map_err(unavailable)?;
        let mut operation: HostGoalOperation = decode(value.ok_or_else(not_found)?)?;
        if operation.status.terminal() {
            if operation.status != status
                || operation.job_id.as_deref() != job_id
                || operation.result_summary.as_deref() != summary
            {
                return Err(conflict("Terminal operation result is immutable"));
            }
            return Ok(operation);
        }
        if status == HostGoalOperationStatus::Intent
            || (operation.job_id.is_some() && job_id != operation.job_id.as_deref())
        {
            return Err(conflict(
                "Operation cannot revert to intent or change Job identity",
            ));
        }
        operation.status = status;
        operation.job_id = job_id.map(str::to_string);
        operation.result_summary = summary.map(str::to_string);
        operation.updated_at = now;
        tx.execute(
            "UPDATE wc_host_goal_operations SET terminal=?2,snapshot_json=?3 WHERE sequence=?1",
            params![operation.sequence, status.terminal(), encode(&operation)?],
        )
        .map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(operation)
    }

    pub fn list_host_goal_operations(
        &self,
        owner: &str,
        id: &str,
        after: u64,
        limit: usize,
        pending_only: bool,
    ) -> Result<Vec<HostGoalOperation>, GoalStoreError> {
        if !(1..=HOST_GOAL_MAX_PAGE).contains(&limit) {
            return Err(invalid("History limit must be 1..100"));
        }
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        let mut statement=conn.prepare("SELECT snapshot_json FROM wc_host_goal_operations WHERE goal_id=?1 AND sequence>?2 AND (?3=0 OR terminal=0) ORDER BY sequence LIMIT ?4").map_err(unavailable)?;
        let rows = statement
            .query_map(params![id, after, pending_only, limit as i64], |r| {
                r.get::<_, String>(0)
            })
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        rows.into_iter().map(decode).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn check_host_goal_node(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        revision: u64,
        evidence: &[HostGoalCheckEvidence],
        final_evidence: &[HostGoalCheckEvidence],
        key: &str,
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        self.check_host_goal_node_impl(
            owner,
            binding,
            revision,
            evidence,
            final_evidence,
            key,
            None,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn check_host_goal_node_with_request(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        revision: u64,
        evidence: &[HostGoalCheckEvidence],
        final_evidence: &[HostGoalCheckEvidence],
        key: &str,
        client_request_hash: &str,
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        bounded(client_request_hash, 128, "request_hash")?;
        self.check_host_goal_node_impl(
            owner,
            binding,
            revision,
            evidence,
            final_evidence,
            key,
            Some(client_request_hash),
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn check_host_goal_node_impl(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        revision: u64,
        evidence: &[HostGoalCheckEvidence],
        final_evidence: &[HostGoalCheckEvidence],
        key: &str,
        client_request_hash: Option<&str>,
        now: i64,
    ) -> Result<HostGoalMutation, GoalStoreError> {
        let fingerprint = request_hash(&(binding, revision, evidence, final_evidence))?;
        let scope = format!("{}:check", binding.goal_id);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(request_hash) = client_request_hash {
            read(&tx, owner, &binding.goal_id)?.ok_or_else(not_found)?;
            if let Some(result) =
                check_request_replay(&tx, owner, &binding.goal_id, key, request_hash)?
            {
                return Ok(result);
            }
            tx.execute("INSERT OR IGNORE INTO wc_host_goal_check_requests(owner_key,goal_id,key_hash,request_hash) VALUES(?1,?2,?3,?4)",params![owner,binding.goal_id,hash(key),request_hash]).map_err(unavailable)?;
        }
        if let Some(mut result) = replay::<HostGoalMutation>(&tx, owner, &scope, key, &fingerprint)?
        {
            result.goal.owner_key = owner.into();
            result.replayed = true;
            return Ok(result);
        }
        let mut goal = required(&tx, owner, &binding.goal_id, revision)?;
        check_binding(&goal, binding)?;
        if unresolved(&tx, &goal.goal_id)? {
            return Err(conflict("Resolve pending operations before acceptance"));
        }
        let plan = current_plan(&tx, &goal)?;
        let index = plan
            .plan
            .nodes
            .iter()
            .position(|n| n.id == binding.node_id)
            .ok_or_else(|| invalid("Unknown node"))?;
        validate_evidence(&plan.plan.nodes[index].acceptance, evidence)?;
        let passed = evidence.iter().all(|e| e.passed);
        let last = index + 1 == plan.plan.nodes.len();
        if last && passed {
            validate_evidence(&plan.plan.final_criteria, final_evidence)?;
        } else if !final_evidence.is_empty() {
            return Err(invalid(
                "Final acceptance runs only after the last node passes",
            ));
        }
        let mut attempt = HostGoalAttempt {
            binding: binding.clone(),
            status: if passed && (!last || final_evidence.iter().all(|e| e.passed)) {
                HostGoalAttemptStatus::Passed
            } else {
                HostGoalAttemptStatus::Failed
            },
            checks: evidence.to_vec(),
            final_checks: final_evidence.to_vec(),
            started_at: now,
            finished_at: Some(now),
        };
        let previous:String=tx.query_row("SELECT snapshot_json FROM wc_host_goal_attempts WHERE goal_id=?1 AND attempt_id=?2",params![goal.goal_id,binding.attempt_id],|r|r.get(0)).map_err(unavailable)?;
        attempt.started_at = decode::<HostGoalAttempt>(previous)?.started_at;
        tx.execute(
            "UPDATE wc_host_goal_attempts SET snapshot_json=?2 WHERE attempt_id=?1",
            params![binding.attempt_id, encode(&attempt)?],
        )
        .map_err(unavailable)?;
        if passed {
            if !goal.completed_node_ids.contains(&binding.node_id) {
                goal.completed_node_ids.push(binding.node_id.clone());
            }
            if last && final_evidence.iter().all(|e| e.passed) {
                goal.phase = HostGoalPhase::Completed;
                goal.state.next_step =
                    "All serial nodes and final acceptance criteria passed.".into();
            } else if last {
                goal.phase = HostGoalPhase::Blocked;
                goal.completed_node_ids.retain(|id| id != &binding.node_id);
                goal.state.next_step =
                    "Final acceptance failed; rework the last node and rerun final checks.".into();
            } else {
                goal.phase = HostGoalPhase::Ready;
                goal.state.next_step = format!(
                    "Claim the next serial node: {}",
                    plan.plan.nodes[index + 1].id
                );
            }
        } else {
            goal.phase = HostGoalPhase::Blocked;
            goal.state.next_step = plan.plan.nodes[index].failure_action.clone();
        }
        goal.current_attempt_id = None;
        save(&tx, &mut goal, now)?;
        add_checkpoint(
            &tx,
            &goal,
            "Stored node acceptance and final acceptance evidence",
            now,
        )?;
        let result = HostGoalMutation {
            goal,
            replayed: false,
        };
        remember(&tx, owner, &scope, key, &fingerprint, &result)?;
        if let Some(request_hash) = client_request_hash {
            let updated=tx.execute("UPDATE wc_host_goal_check_requests SET result_json=?5 WHERE owner_key=?1 AND goal_id=?2 AND key_hash=?3 AND request_hash=?4 AND result_json IS NULL",params![owner,binding.goal_id,hash(key),request_hash,encode(&result)?]).map_err(unavailable)?;
            if updated != 1 {
                return Err(conflict("Check request receipt changed during acceptance"));
            }
        }
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }
}

fn check_request_replay(
    conn: &Connection,
    owner: &str,
    id: &str,
    key: &str,
    request_hash: &str,
) -> Result<Option<HostGoalMutation>, GoalStoreError> {
    client_request_replay(
        conn,
        "wc_host_goal_check_requests",
        owner,
        id,
        key,
        request_hash,
    )
}

fn client_request_replay(
    conn: &Connection,
    table: &str,
    owner: &str,
    id: &str,
    key: &str,
    request_hash: &str,
) -> Result<Option<HostGoalMutation>, GoalStoreError> {
    let query=match table {
        "wc_host_goal_check_requests"=>"SELECT request_hash,result_json FROM wc_host_goal_check_requests WHERE owner_key=?1 AND goal_id=?2 AND key_hash=?3",
        "wc_host_goal_checkpoint_requests"=>"SELECT request_hash,result_json FROM wc_host_goal_checkpoint_requests WHERE owner_key=?1 AND goal_id=?2 AND key_hash=?3",
        _=>return Err(invalid("Unknown client receipt domain")),
    };
    let row: Option<(String, Option<String>)> = conn
        .query_row(query, params![owner, id, hash(key)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(unavailable)?;
    match row {
        Some((original, _)) if original != request_hash => Err(GoalStoreError::new(
            "goal_idempotency_conflict",
            "Request key was already used with different client input",
        )),
        Some((_, Some(result))) => {
            let mut result: HostGoalMutation = decode(result)?;
            result.goal.owner_key = owner.into();
            result.replayed = true;
            Ok(Some(result))
        }
        _ => Ok(None),
    }
}

fn validate_evidence(
    criteria: &[GoalCriterion],
    evidence: &[HostGoalCheckEvidence],
) -> Result<(), GoalStoreError> {
    if criteria.len() != evidence.len() {
        return Err(invalid("Acceptance requires evidence for every criterion"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for item in evidence {
        if !seen.insert(&item.criterion_id) {
            return Err(invalid("Duplicate acceptance evidence"));
        }
        let criterion = criteria
            .iter()
            .find(|c| c.id() == item.criterion_id)
            .ok_or_else(|| invalid("Unknown evidence criterion"))?;
        bounded(&item.evidence, 8192, "acceptance evidence")?;
        if item.source_refs.len() > 64 || item.artifacts.len() > 64 {
            return Err(invalid("Too many evidence references"));
        }
        for value in &item.source_refs {
            bounded(value, 4096, "evidence reference")?;
        }
        let review = matches!(criterion, GoalCriterion::Review { .. });
        if review != (item.source == HostGoalEvidenceSource::ModelSelfCheck) {
            return Err(invalid(
                "Evidence source must match the saved acceptance method",
            ));
        }
        if review && item.source_refs.is_empty() {
            return Err(invalid(
                "Model self-checks require explicit source references",
            ));
        }
        for artifact in &item.artifacts {
            webcodex_core::goal::validate_goal_relative_path(&artifact.path).map_err(invalid)?;
            if artifact
                .sha256
                .as_ref()
                .is_some_and(|v| v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(invalid("Artifact fingerprint must be SHA-256"));
            }
        }
    }
    Ok(())
}
