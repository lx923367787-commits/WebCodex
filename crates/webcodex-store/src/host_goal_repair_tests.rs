use super::*;

fn fail_operation(db: &Database, goal: &HostGoalRecord, binding: &HostGoalExecutionBinding) {
    db.begin_host_goal_operation(
        OWNER,
        binding,
        "failed-build",
        "run_process",
        "build-hash",
        NOW + 10,
    )
    .unwrap();
    db.finish_host_goal_operation(
        OWNER,
        &goal.goal_id,
        "failed-build",
        HostGoalOperationStatus::Failed,
        None,
        Some("Compiler reports an unresolved import"),
        NOW + 11,
    )
    .unwrap();
}

fn repair_context(db: &Database, goal: &HostGoalRecord) -> HostGoalRepairContext {
    HostGoalRepairContext {
        repair_id: db.get_host_goal_repair(OWNER, &goal.goal_id).unwrap().unwrap().repair_id,
        diagnosis: Some("The failing import references the old module; update that import and rerun the original acceptance checks".into()),
    }
}

fn serial_goal(db: &Database) -> (HostGoalRecord, HostGoalExecutionBinding) {
    let goal = new_goal(db);
    let mut contract = plan();
    let mut next = contract.nodes[0].clone();
    next.id = "delivery".into();
    contract.nodes.push(next);
    let saved = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &contract,
            None,
            "serial",
            NOW + 1,
        )
        .unwrap();
    let receipt = HostGoalMaterialization {
        json_path: "/state/plan.json".into(),
        json_hash: "a".repeat(64),
        markdown_path: "/state/plan.md".into(),
        markdown_hash: "b".repeat(64),
        verified_at: NOW + 2,
    };
    let published = db
        .materialize_host_goal_plan(OWNER, &goal.goal_id, 1, &saved.plan.hash, &receipt, NOW + 2)
        .unwrap();
    let confirmed = db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            published.goal.revision,
            1,
            &saved.plan.hash,
            "Start the saved plan",
            "confirm",
            NOW + 3,
        )
        .unwrap();
    let claim = db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            confirmed.goal.revision,
            "research",
            false,
            "claim",
            NOW + 4,
        )
        .unwrap();
    (claim.goal, claim.attempt.binding)
}

#[test]
fn host_goal_repair_failure_blocks_business_until_original_acceptance_passes() {
    let (_dir, db) = database();
    let (goal, binding) = serial_goal(&db);
    fail_operation(&db, &goal, &binding);
    let before = db
        .get_host_goal_repair(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(before.status, HostGoalRepairStatus::Required);
    assert_eq!(
        db.begin_host_goal_operation(
            OWNER,
            &binding,
            "continue-business",
            "run_process",
            "next",
            NOW + 12
        )
        .unwrap_err()
        .code(),
        "goal_repair_required"
    );
    assert_eq!(
        db.claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            goal.revision,
            "delivery",
            true,
            "skip-repair",
            NOW + 12
        )
        .unwrap_err()
        .code(),
        "goal_repair_required"
    );
    let repair = repair_context(&db, &goal);
    let operation = db
        .begin_host_goal_operation_with_repair_in_context(
            OWNER,
            &binding,
            "repair-import",
            "apply_text_edits",
            "fix",
            &[],
            NOW + 13,
            None,
            false,
            Some(&repair),
        )
        .unwrap();
    assert_eq!(
        operation.operation.purpose,
        HostGoalOperationPurpose::Repair
    );
    db.finish_host_goal_operation(
        OWNER,
        &goal.goal_id,
        "repair-import",
        HostGoalOperationStatus::Succeeded,
        None,
        Some("Import repaired"),
        NOW + 14,
    )
    .unwrap();
    assert_eq!(
        db.begin_host_goal_operation(
            OWNER,
            &binding,
            "still-unverified",
            "run_process",
            "next",
            NOW + 15
        )
        .unwrap_err()
        .code(),
        "goal_repair_required"
    );
    let accepted = prepare_review_named(&db, &binding, None, "repair-verification");
    let result = db
        .check_host_goal_node(
            OWNER,
            &binding,
            goal.revision,
            &accepted,
            &[],
            "verify-repair",
            NOW + 16,
        )
        .unwrap();
    assert_eq!(result.goal.phase, HostGoalPhase::Ready);
    let verified = db
        .get_host_goal_repair(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(verified.status, HostGoalRepairStatus::Verified);
    assert_eq!(verified.rounds.len(), 1);
    assert_eq!(verified.verified_at, Some(NOW + 16));
    let next = db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            result.goal.revision,
            "delivery",
            false,
            "resume-business",
            NOW + 17,
        )
        .unwrap();
    let business = db
        .begin_host_goal_operation(
            OWNER,
            &next.attempt.binding,
            "deliver",
            "run_process",
            "next",
            NOW + 18,
        )
        .unwrap();
    assert_eq!(
        business.operation.purpose,
        HostGoalOperationPurpose::Business
    );
}

#[test]
fn host_goal_repair_round_budget_survives_reclaims_restart_and_replanning() {
    let (dir, db) = database();
    let (mut goal, mut binding) = claimed(&db);
    fail_operation(&db, &goal, &binding);
    for number in 0..HOST_GOAL_REPAIR_MAX_ROUNDS {
        let repair = repair_context(&db, &goal);
        db.admit_host_goal_repair_check(OWNER, &binding, Some(&repair), NOW + 20, None)
            .unwrap();
        let failed = db
            .check_host_goal_node(
                OWNER,
                &binding,
                goal.revision,
                &evidence(false),
                &[],
                &format!("repair-fails-{number}"),
                NOW + 21,
            )
            .unwrap();
        assert_eq!(failed.goal.phase, HostGoalPhase::Blocked);
        let reclaimed = db
            .claim_host_goal_node(
                OWNER,
                &goal.goal_id,
                failed.goal.revision,
                "research",
                true,
                &format!("reclaim-{number}"),
                NOW + 22,
            )
            .unwrap();
        goal = reclaimed.goal;
        binding = reclaimed.attempt.binding;
    }
    let record = db
        .get_host_goal_repair(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.rounds.len(), HOST_GOAL_REPAIR_MAX_ROUNDS);
    let repair = repair_context(&db, &goal);
    assert_eq!(
        db.begin_host_goal_operation_with_repair_in_context(
            OWNER,
            &binding,
            "fourth-repair",
            "apply_text_edits",
            "fix",
            &[],
            NOW + 30,
            None,
            false,
            Some(&repair)
        )
        .unwrap_err()
        .code(),
        "goal_repair_exhausted"
    );
    assert_eq!(
        db.save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &plan(),
            None,
            "reset-budget",
            NOW + 31
        )
        .unwrap_err()
        .code(),
        "goal_repair_required"
    );
    drop(db);
    let reopened = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .get_host_goal_repair(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap(),
        record
    );
    assert!(reopened.get_host_goal_repair(OTHER, &goal.goal_id).is_err());
}

#[test]
fn host_goal_repair_diagnosis_identity_and_deadline_are_enforced() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    fail_operation(&db, &goal, &binding);
    let mut repair = repair_context(&db, &goal);
    repair.diagnosis = None;
    assert_eq!(
        db.begin_host_goal_operation_with_repair_in_context(
            OWNER,
            &binding,
            "no-diagnosis",
            "apply_text_edits",
            "fix",
            &[],
            NOW + 12,
            None,
            false,
            Some(&repair)
        )
        .unwrap_err()
        .code(),
        "goal_repair_diagnosis_required"
    );
    assert!(db
        .get_host_goal_repair(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap()
        .rounds
        .is_empty());
    repair.diagnosis = Some("Correct the unresolved import and rerun the failing build".into());
    let record = db
        .get_host_goal_repair(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        db.begin_host_goal_operation_with_repair_in_context(
            OWNER,
            &binding,
            "late-repair",
            "apply_text_edits",
            "fix",
            &[],
            record.deadline_at,
            None,
            false,
            Some(&repair)
        )
        .unwrap_err()
        .code(),
        "goal_repair_exhausted"
    );
    repair.repair_id = "hrepair_other".into();
    assert_eq!(
        db.begin_host_goal_operation_with_repair_in_context(
            OWNER,
            &binding,
            "wrong-repair",
            "apply_text_edits",
            "fix",
            &[],
            NOW + 12,
            None,
            false,
            Some(&repair)
        )
        .unwrap_err()
        .code(),
        "goal_repair_required"
    );
}

#[test]
fn host_goal_repair_operation_budget_stops_changes_but_allows_final_verification() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    fail_operation(&db, &goal, &binding);
    let repair = repair_context(&db, &goal);
    for index in 0..HOST_GOAL_REPAIR_MAX_OPERATIONS {
        let id = format!("repair-{index}");
        db.begin_host_goal_operation_with_repair_in_context(
            OWNER,
            &binding,
            &id,
            "apply_text_edits",
            "fix",
            &[],
            NOW + 12,
            None,
            false,
            Some(&repair),
        )
        .unwrap();
        db.finish_host_goal_operation(
            OWNER,
            &goal.goal_id,
            &id,
            HostGoalOperationStatus::Succeeded,
            None,
            Some("Targeted correction"),
            NOW + 13,
        )
        .unwrap();
    }
    assert_eq!(
        db.begin_host_goal_operation_with_repair_in_context(
            OWNER,
            &binding,
            "too-many",
            "apply_text_edits",
            "fix",
            &[],
            NOW + 14,
            None,
            false,
            Some(&repair)
        )
        .unwrap_err()
        .code(),
        "goal_repair_exhausted"
    );
    db.admit_host_goal_repair_check(OWNER, &binding, Some(&repair), NOW + 15, None)
        .unwrap();
    let accepted = prepare_review_named(&db, &binding, None, "last-verification");
    let result = db
        .check_host_goal_node(
            OWNER,
            &binding,
            goal.revision,
            &accepted,
            &accepted,
            "verify",
            NOW + 16,
        )
        .unwrap();
    assert_eq!(result.goal.phase, HostGoalPhase::Completed);
}
