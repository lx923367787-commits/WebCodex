//! A repair gate is committed with the failure and released with acceptance.
use super::*;

pub(super) fn read_repair(
    conn: &Connection,
    id: &str,
) -> Result<Option<HostGoalRepair>, GoalStoreError> {
    conn.query_row(
        "SELECT snapshot_json FROM wc_host_goal_repairs WHERE goal_id=?1",
        [id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(unavailable)?
    .map(decode)
    .transpose()
}

fn write_repair(conn: &Connection, record: &HostGoalRepair) -> Result<(), GoalStoreError> {
    conn.execute(
        "INSERT INTO wc_host_goal_repairs(goal_id,snapshot_json) VALUES(?1,?2)
        ON CONFLICT(goal_id) DO UPDATE SET snapshot_json=excluded.snapshot_json",
        params![record.goal_id, encode(record)?],
    )
    .map_err(unavailable)?;
    Ok(())
}

fn excerpt(value: &str) -> String {
    let mut end = value.len().min(4096);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].replace('\0', "")
}

pub(super) fn fail(
    conn: &Connection,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
    evidence: &str,
    now: i64,
) -> Result<(), GoalStoreError> {
    let failure = excerpt(evidence);
    let mut record = match read_repair(conn, &goal.goal_id)?.filter(HostGoalRepair::active) {
        Some(record) => {
            if record.node_id != binding.node_id || record.plan_version != binding.plan_version {
                return Err(conflict(
                    "The unresolved repair belongs to another node or plan",
                ));
            }
            record
        }
        None => HostGoalRepair {
            repair_id: String::new(),
            goal_id: goal.goal_id.clone(),
            node_id: binding.node_id.clone(),
            plan_version: binding.plan_version,
            status: HostGoalRepairStatus::Required,
            failure: failure.clone(),
            rounds: Vec::new(),
            operations: 0,
            started_at: now,
            deadline_at: now.saturating_add(HOST_GOAL_REPAIR_TIMEOUT_MS),
            verified_at: None,
        },
    };
    if record.status == HostGoalRepairStatus::Exhausted {
        return Ok(());
    }
    // Multiple failed checks in one verdict are one failed repair round.
    if let Some(round) = record
        .rounds
        .last_mut()
        .filter(|round| round.finished_at.is_none())
    {
        round.failure = Some(failure.clone());
        round.finished_at = Some(now);
    }
    if record.status != HostGoalRepairStatus::Required || record.repair_id.is_empty() {
        record.repair_id = format!("hrepair_{}", uuid::Uuid::new_v4().simple());
    }
    record.status = HostGoalRepairStatus::Required;
    record.failure = failure;
    write_repair(conn, &record)
}

pub(super) fn admit(
    conn: &Connection,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
    supplied: Option<&HostGoalRepairContext>,
    now: i64,
    mutation: bool,
) -> Result<HostGoalOperationPurpose, GoalStoreError> {
    let Some(mut record) = read_repair(conn, &goal.goal_id)?.filter(HostGoalRepair::active) else {
        if supplied.is_some() {
            return Err(GoalStoreError::new(
                "goal_repair_stale",
                "This repair has already passed or no longer exists; refresh the goal",
            ));
        }
        return Ok(HostGoalOperationPurpose::Business);
    };
    if record.node_id != binding.node_id || record.plan_version != binding.plan_version {
        return Err(GoalStoreError::new(
            "goal_repair_required",
            "Repair and verify the affected node before starting other business",
        ));
    }
    let supplied = supplied.filter(|value| value.repair_id == record.repair_id)
        .ok_or_else(|| GoalStoreError::new("goal_repair_required",
            "Ordinary business is suspended. Read recovery, diagnose the failure and attach its current repair identity to repair calls; then rerun goal_check_node"))?;
    if record.exhausted(now) && (mutation || record.status != HostGoalRepairStatus::Repairing) {
        return Err(GoalStoreError::new("goal_repair_exhausted",
            "Repair reached its original time, operation or round budget. Preserve evidence and request the missing operator decision; do not reset identity or acceptance"));
    }
    if record.status == HostGoalRepairStatus::Required {
        let diagnosis = supplied.diagnosis.as_deref().ok_or_else(|| {
            GoalStoreError::new(
                "goal_repair_diagnosis_required",
                "Describe the observed cause and targeted repair before making repair changes",
            )
        })?;
        bounded(diagnosis, 4096, "repair diagnosis")?;
        record.rounds.push(HostGoalRepairRound {
            diagnosis: diagnosis.to_owned(),
            started_at: now,
            finished_at: None,
            failure: None,
        });
        record.status = HostGoalRepairStatus::Repairing;
    }
    if mutation {
        record.operations += 1;
    }
    write_repair(conn, &record)?;
    Ok(HostGoalOperationPurpose::Repair)
}

pub(super) fn exhaust(conn: &Connection, goal_id: &str) -> Result<(), GoalStoreError> {
    if let Some(mut record) = read_repair(conn, goal_id)?.filter(HostGoalRepair::active) {
        record.status = HostGoalRepairStatus::Exhausted;
        write_repair(conn, &record)?;
    }
    Ok(())
}

pub(super) fn verified(
    conn: &Connection,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
    now: i64,
) -> Result<(), GoalStoreError> {
    let Some(mut record) = read_repair(conn, &goal.goal_id)?.filter(HostGoalRepair::active) else {
        return Ok(());
    };
    if record.node_id != binding.node_id || record.plan_version != binding.plan_version {
        return Err(conflict(
            "Acceptance cannot release another node's repair gate",
        ));
    }
    record.status = HostGoalRepairStatus::Verified;
    record.verified_at = Some(now);
    if let Some(round) = record
        .rounds
        .last_mut()
        .filter(|round| round.finished_at.is_none())
    {
        round.finished_at = Some(now);
    }
    write_repair(conn, &record)
}

impl Database {
    pub fn get_host_goal_repair(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<Option<HostGoalRepair>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        read(&conn, owner, id)?.ok_or_else(not_found)?;
        read_repair(&conn, id)
    }

    pub fn admit_host_goal_repair_check(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        supplied: Option<&HostGoalRepairContext>,
        now: i64,
        context: Option<&HostGoalInvocationContext>,
    ) -> Result<(), GoalStoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let goal = read(&tx, owner, &binding.goal_id)?.ok_or_else(not_found)?;
        context_goal_gate(&tx, owner, context, &goal.goal_id, true)?;
        check_binding(&goal, binding)?;
        check_binding_context(&tx, binding, context)?;
        if unresolved(&tx, &goal.goal_id)? {
            return Err(conflict(
                "Observe the original operation before beginning repair verification",
            ));
        }
        admit(&tx, &goal, binding, supplied, now, false)?;
        tx.commit().map_err(unavailable)
    }

    pub fn record_host_goal_repair_failure(
        &self,
        owner: &str,
        binding: &HostGoalExecutionBinding,
        evidence: &str,
        now: i64,
        context: Option<&HostGoalInvocationContext>,
    ) -> Result<(), GoalStoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let goal = read(&tx, owner, &binding.goal_id)?.ok_or_else(not_found)?;
        context_goal_gate(&tx, owner, context, &goal.goal_id, true)?;
        check_binding(&goal, binding)?;
        check_binding_context(&tx, binding, context)?;
        fail(&tx, &goal, binding, evidence, now)?;
        tx.commit().map_err(unavailable)
    }
}
