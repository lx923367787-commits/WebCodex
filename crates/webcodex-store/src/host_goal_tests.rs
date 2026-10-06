use super::{Database, HostGoalCheckpointAction};
use webcodex_core::goal::GoalCriterion;
use webcodex_core::host_goal::*;

const OWNER: &str = "host-owner-a";
const OTHER: &str = "host-owner-b";
const NOW: i64 = 1_234_500;

#[path = "host_goal_repair_tests.rs"]
mod repair_tests;
#[path = "host_goal_review_tests.rs"]
mod review_tests;

#[path = "host_goal_task_directory_tests.rs"]
mod task_directory_tests;

fn database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    (dir, db)
}

fn plan() -> HostGoalPlan {
    let criterion = GoalCriterion::Review {
        id: "analysis".into(),
        description: "Reasoning is supported by the cited source".into(),
    };
    HostGoalPlan {
        objective: "Compare documented alternatives".into(),
        constraints: vec!["Use primary sources".into()],
        deliverables: vec!["Written comparison".into()],
        verification_paths: vec![],
        final_criteria: vec![criterion.clone()],
        nodes: vec![HostGoalNode {
            id: "research".into(),
            title: "Read and compare".into(),
            inputs: vec!["Primary source".into()],
            verification_paths: vec![],
            work: "Read the source and compare alternatives".into(),
            outputs: vec!["Comparison".into()],
            criterion_ids: vec!["analysis".into()],
            acceptance: vec![criterion],
            failure_action: "Record unsupported claims and research them".into(),
            review: None,
            review_reason: None,
            review_max_rounds: None,
            delivery_kind: None,
            managed_artifacts: None,
        }],
        state_management: HostGoalStateManagement {
            facts: "Cite source for facts".into(),
            decisions: "Explain decisions".into(),
            issues: "Record unresolved evidence".into(),
            artifacts: "Save source references".into(),
            checkpoint_triggers: vec!["After research".into()],
            resume_instructions: "Read current node and open issues".into(),
        },
    }
}

fn new_goal(db: &Database) -> HostGoalRecord {
    db.create_host_goal_at(
        OWNER,
        "project",
        "/workspace/project",
        "Compare documented alternatives",
        "create",
        NOW,
    )
    .unwrap()
    .goal
}

fn published(db: &Database) -> (HostGoalRecord, HostGoalPlanVersion) {
    let goal = new_goal(db);
    let saved = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &plan(),
            None,
            "plan",
            NOW + 1,
        )
        .unwrap();
    let receipt = HostGoalMaterialization {
        json_path: "/state/goals/plan.json".into(),
        json_hash: "a".repeat(64),
        markdown_path: "/state/goals/plan.md".into(),
        markdown_hash: "b".repeat(64),
        verified_at: NOW + 2,
    };
    let result = db
        .materialize_host_goal_plan(OWNER, &goal.goal_id, 1, &saved.plan.hash, &receipt, NOW + 2)
        .unwrap();
    (result.goal, result.plan)
}

fn confirmed(db: &Database) -> HostGoalRecord {
    let (goal, version) = published(db);
    db.confirm_host_goal_plan(
        OWNER,
        &goal.goal_id,
        goal.revision,
        1,
        &version.hash,
        "我已经查看计划，现在开始执行",
        "confirm",
        NOW + 3,
    )
    .unwrap()
    .goal
}

fn claimed(db: &Database) -> (HostGoalRecord, HostGoalExecutionBinding) {
    let goal = confirmed(db);
    let claim = db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            goal.revision,
            "research",
            false,
            "claim",
            NOW + 4,
        )
        .unwrap();
    prepare_review(db, &claim.attempt.binding, None);
    (claim.goal, claim.attempt.binding)
}

fn prepare_review(db: &Database, binding: &HostGoalExecutionBinding, epoch: Option<&str>) {
    prepare_review_named(db, binding, epoch, "store-test-review");
}

fn prepare_review_named(
    db: &Database,
    binding: &HostGoalExecutionBinding,
    epoch: Option<&str>,
    packet_id: &str,
) -> Vec<HostGoalCheckEvidence> {
    let packet = HostGoalReviewPacket {
        packet_id: packet_id.into(),
        binding: binding.clone(),
        context_epoch: epoch.map(str::to_string),
        manifest: vec![webcodex_core::goal::GoalArtifactSnapshot {
            path: "source.txt".into(),
            sha256: Some("a".repeat(64)),
        }],
        state_sha256: None,
        machine_checks: vec![],
        materials: vec![HostGoalReviewMaterial {
            id: "source".into(),
            path: "source.txt".into(),
            sha256: "a".repeat(64),
            kind: "text".into(),
            preview_path: None,
            preview_sha256: None,
            render_source_sha256: None,
            render_dependencies_sha256: None,
            content: Some(
                "The two actual documented modes provide the comparison evidence.".into(),
            ),
        }],
        required_scopes: vec!["source".into()],
        delivered_scopes: vec!["source".into()],
        created_at: NOW,
    };
    let mut verdict = evidence(true);
    verdict[0].review_packet_id = Some(packet_id.into());
    db.prepare_host_goal_review(OWNER, binding, &packet, NOW)
        .unwrap();
    db.submit_host_goal_review(
        OWNER,
        binding,
        &packet.packet_id,
        epoch,
        &packet.manifest,
        &verdict,
        NOW,
    )
    .unwrap();
    verdict
}

fn evidence(passed: bool) -> Vec<HostGoalCheckEvidence> {
    vec![HostGoalCheckEvidence {
        criterion_id: "analysis".into(),
        passed,
        source: HostGoalEvidenceSource::ModelSelfCheck,
        evidence: "source.txt: The comparison follows the two documented modes with the stated limitations".into(),
        source_refs: vec!["https://example.org/documentation".into()],
        artifacts: vec![webcodex_core::goal::GoalArtifactSnapshot {
            path: "source.txt".into(),
            sha256: Some("a".repeat(64)),
        }],
        review_packet_id: Some("store-test-review".into()),
        reviewed_scopes: vec!["source".into()],
        findings: vec![],
    }]
}

#[test]
fn host_goal_tests_checkpoint_revision_lookup_preserves_original_snapshot_and_owner_scope() {
    let (dir, db) = database();
    let goal = new_goal(&db);
    assert!(db
        .host_goal_checkpoint_for_revision(OWNER, &goal.goal_id, goal.revision)
        .unwrap()
        .is_none());
    let mut first_state = goal.state.clone();
    first_state.completed_summary = "Original conversation result".into();
    first_state
        .facts
        .insert("source".into(), "original snapshot".into());
    let first = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            None,
            &first_state,
            HostGoalCheckpointAction::Progress,
            "Original saved state",
            "checkpoint-one",
            NOW + 1,
        )
        .unwrap();
    let mut second_state = first.goal.state.clone();
    second_state.completed_summary = "Later conversation result".into();
    second_state
        .facts
        .insert("source".into(), "later snapshot".into());
    let second = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            first.goal.revision,
            None,
            &second_state,
            HostGoalCheckpointAction::Progress,
            "Later saved state",
            "checkpoint-two",
            NOW + 2,
        )
        .unwrap();
    let replay = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            None,
            &first_state,
            HostGoalCheckpointAction::Progress,
            "Original saved state",
            "checkpoint-one",
            NOW + 3,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.goal.revision, first.goal.revision);
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let original = db
        .host_goal_checkpoint_for_revision(OWNER, &goal.goal_id, replay.goal.revision)
        .unwrap()
        .unwrap();
    let later = db
        .host_goal_checkpoint_for_revision(OWNER, &goal.goal_id, second.goal.revision)
        .unwrap()
        .unwrap();
    assert_eq!(original.state, first_state);
    assert_eq!(later.state, second_state);
    assert!(later.sequence > original.sequence);
    assert_eq!(
        db.latest_host_goal_checkpoint(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap(),
        later
    );
    assert!(db
        .host_goal_checkpoint_for_revision(OWNER, &goal.goal_id, second.goal.revision + 1)
        .unwrap()
        .is_none());
    assert_eq!(
        db.host_goal_checkpoint_for_revision(OTHER, &goal.goal_id, first.goal.revision)
            .unwrap_err()
            .code(),
        "goal_not_found"
    );
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .revision,
        second.goal.revision,
        "History reads must not mutate current state"
    );
}

#[test]
fn host_goal_tests_published_plan_still_requires_matching_user_confirmation() {
    let (_dir, db) = database();
    let (goal, version) = published(&db);
    assert_eq!(goal.phase, HostGoalPhase::AwaitingConfirmation);
    assert!(!goal.confirmed());
    assert!(db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            goal.revision,
            "research",
            false,
            "early",
            NOW
        )
        .is_err());
    assert!(db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            2,
            &version.hash,
            "Start",
            "wrong-version",
            NOW
        )
        .is_err());
    assert!(db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            1,
            &"c".repeat(64),
            "Start",
            "wrong-hash",
            NOW
        )
        .is_err());
    assert!(db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            1,
            &version.hash,
            " ",
            "empty",
            NOW
        )
        .is_err());
    let result = db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            1,
            &version.hash,
            "Start the plan",
            "confirm",
            NOW,
        )
        .unwrap();
    assert_eq!(result.goal.phase, HostGoalPhase::Ready);
    assert!(result.goal.confirmed());
    let replay = db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            1,
            &version.hash,
            "Start the plan",
            "confirm",
            NOW + 1,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.goal, result.goal);
}

#[test]
fn host_goal_tests_owner_isolation_replay_payload_conflict_and_cas() {
    let (_dir, db) = database();
    let goal = new_goal(&db);
    assert!(db.get_host_goal(OTHER, &goal.goal_id).unwrap().is_none());
    assert!(db.current_host_goal(OTHER).unwrap().is_none());
    assert!(db.get_host_goal_plan(OTHER, &goal.goal_id, None).is_err());
    assert!(db
        .select_current_host_goal(OTHER, &goal.goal_id, NOW)
        .is_err());
    let replay = db
        .create_host_goal_at(
            OWNER,
            "project",
            "/workspace/project",
            "Compare documented alternatives",
            "create",
            NOW + 1,
        )
        .unwrap();
    assert!(replay.replayed);
    let error = db
        .create_host_goal_at(
            OWNER,
            "project",
            "/different/root",
            "Compare documented alternatives",
            "create",
            NOW,
        )
        .unwrap_err();
    assert_eq!(error.code(), "goal_idempotency_conflict");
    let saved = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &plan(),
            None,
            "plan",
            NOW,
        )
        .unwrap();
    let replay = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &plan(),
            None,
            "plan",
            NOW + 1,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.plan.version, saved.plan.version);
    assert!(db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &plan(),
            None,
            "new-key",
            NOW
        )
        .is_err());
    let mut changed = plan();
    changed.nodes[0].work = "Different implementation".into();
    assert_eq!(
        db.save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &changed,
            None,
            "plan",
            NOW
        )
        .unwrap_err()
        .code(),
        "goal_idempotency_conflict"
    );
}

#[test]
fn host_goal_tests_restart_preserves_draft_current_pointer_and_history() {
    let (dir, db) = database();
    let goal = new_goal(&db);
    let state = HostGoalState {
        decisions: vec!["Use official sources".into()],
        issues: vec!["Need the intended audience".into()],
        plan_draft: Some(serde_json::json!({"outline":["research"]})),
        ..Default::default()
    };
    let saved = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            None,
            &state,
            HostGoalCheckpointAction::Handoff,
            "Planning discussion saved",
            "draft",
            NOW + 1,
        )
        .unwrap();
    let path = db.host_goal_plan_root().unwrap();
    assert!(path.ends_with("goals"));
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let restored = db.current_host_goal(OWNER).unwrap().unwrap();
    assert_eq!(restored, saved.goal);
    assert_eq!(restored.phase, HostGoalPhase::Planning);
    assert_eq!(
        db.list_host_goal_checkpoints(OWNER, &goal.goal_id, 0, 10)
            .unwrap()[0]
            .state,
        state
    );
    assert!(db
        .list_host_goal_checkpoints(OTHER, &goal.goal_id, 0, 10)
        .is_err());
    assert!(db
        .list_host_goal_checkpoints(OWNER, &goal.goal_id, 0, 101)
        .is_err());
    assert!(!serde_json::to_string(&restored).unwrap().contains(OWNER));
}

#[test]
fn host_goal_tests_replan_invalidates_confirmation_and_old_attempt() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    let mut changed = plan();
    changed.nodes[0].work = "Read another primary source first".into();
    let saved = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &changed,
            None,
            "replan",
            NOW + 5,
        )
        .unwrap();
    assert_eq!(saved.goal.plan_version, 2);
    assert_eq!(saved.goal.phase, HostGoalPhase::Planning);
    assert!(saved.goal.confirmation.is_none());
    assert!(!saved.goal.plan_materialized);
    assert!(db.validate_host_goal_binding(OWNER, &binding).is_err());
    assert_eq!(
        db.get_host_goal_attempt(OWNER, &goal.goal_id, &binding.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        HostGoalAttemptStatus::Interrupted
    );
    assert!(db
        .get_host_goal_plan(OWNER, &goal.goal_id, Some(1))
        .unwrap()
        .is_some());
    let mut weaker = changed.clone();
    weaker.final_criteria[0] = GoalCriterion::Review {
        id: "analysis".into(),
        description: "Model says done".into(),
    };
    assert!(db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            saved.goal.revision,
            &weaker,
            None,
            "weaken",
            NOW + 6
        )
        .is_err());
}

#[test]
fn host_goal_tests_pending_operation_blocks_takeover_replay_never_redispatches() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    let op = db
        .begin_host_goal_operation(
            OWNER,
            &binding,
            "operation",
            "read_file",
            "payload-a",
            NOW + 5,
        )
        .unwrap();
    assert!(!op.replayed);
    let replay = db
        .begin_host_goal_operation(
            OWNER,
            &binding,
            "operation",
            "read_file",
            "payload-a",
            NOW + 6,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        db.begin_host_goal_operation(OWNER, &binding, "operation", "read_file", "payload-b", NOW)
            .unwrap_err()
            .code(),
        "goal_idempotency_conflict"
    );
    assert!(db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            goal.revision,
            "research",
            false,
            "takeover",
            NOW + 6
        )
        .is_err());
    assert!(db
        .check_host_goal_node(
            OWNER,
            &binding,
            goal.revision,
            &evidence(true),
            &evidence(true),
            "early-check",
            NOW
        )
        .is_err());
    db.finish_host_goal_operation(
        OWNER,
        &goal.goal_id,
        "operation",
        HostGoalOperationStatus::Unknown,
        None,
        Some("Response was lost"),
        NOW + 6,
    )
    .unwrap();
    let paused = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            HostGoalCheckpointAction::Pause,
            "Pause for recovery",
            "pause",
            NOW + 7,
        )
        .unwrap();
    assert!(db.validate_host_goal_binding(OWNER, &binding).is_err());
    db.finish_host_goal_operation(
        OWNER,
        &goal.goal_id,
        "operation",
        HostGoalOperationStatus::Succeeded,
        None,
        Some("Original output recovered"),
        NOW + 8,
    )
    .unwrap();
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .revision,
        paused.goal.revision
    );
    let new = db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            paused.goal.revision,
            "research",
            false,
            "takeover",
            NOW + 9,
        )
        .unwrap();
    assert_ne!(new.attempt.binding.attempt_id, binding.attempt_id);
    assert!(db
        .begin_host_goal_operation(
            OWNER,
            &binding,
            "stale-operation",
            "write_file",
            "payload",
            NOW + 10
        )
        .is_err());
    assert!(
        db.begin_host_goal_operation(
            OWNER,
            &binding,
            "operation",
            "read_file",
            "payload-a",
            NOW + 10
        )
        .unwrap()
        .replayed
    );
}

#[test]
fn host_goal_tests_acceptance_cannot_skip_criteria_or_forge_machine_source() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    assert!(db
        .check_host_goal_node(OWNER, &binding, goal.revision, &[], &[], "missing", NOW)
        .is_err());
    let mut wrong = evidence(true);
    wrong[0].source = HostGoalEvidenceSource::Automated;
    assert!(db
        .check_host_goal_node(
            OWNER,
            &binding,
            goal.revision,
            &wrong,
            &wrong,
            "wrong-source",
            NOW
        )
        .is_err());
    let mut unreferenced = evidence(true);
    unreferenced[0].source_refs.clear();
    assert!(db
        .check_host_goal_node(
            OWNER,
            &binding,
            goal.revision,
            &unreferenced,
            &unreferenced,
            "no-source",
            NOW
        )
        .is_err());
    let failed = db
        .check_host_goal_node(
            OWNER,
            &binding,
            goal.revision,
            &evidence(true),
            &evidence(false),
            "failed-final",
            NOW + 5,
        )
        .unwrap();
    assert_eq!(failed.goal.phase, HostGoalPhase::Blocked);
    assert!(failed.goal.completed_node_ids.is_empty());
    let next = db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            failed.goal.revision,
            "research",
            true,
            "retry",
            NOW + 6,
        )
        .unwrap();
    let revised_evidence = prepare_review_named(&db, &next.attempt.binding, None, "rework-review");
    let completed = db
        .check_host_goal_node(
            OWNER,
            &next.attempt.binding,
            next.goal.revision,
            &revised_evidence,
            &revised_evidence,
            "pass",
            NOW + 7,
        )
        .unwrap();
    assert_eq!(completed.goal.phase, HostGoalPhase::Completed);
    assert!(db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            completed.goal.revision,
            None,
            &completed.goal.state,
            HostGoalCheckpointAction::Progress,
            "Bypass",
            "terminal",
            NOW
        )
        .is_err());
    assert!(db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            completed.goal.revision,
            "research",
            false,
            "implicit-restart",
            NOW
        )
        .is_err());
}

#[test]
fn host_goal_tests_serial_order_and_plan_structure_are_enforced() {
    let (_dir, db) = database();
    let goal = new_goal(&db);
    let mut value = plan();
    value.nodes[0].criterion_ids = vec!["missing".into()];
    assert!(value.validate().is_err());
    value = plan();
    value.state_management.checkpoint_triggers.clear();
    assert!(value.validate().is_err());
    value = plan();
    value.final_criteria.clear();
    assert!(value.validate().is_err());
    value = plan();
    let mut next = value.nodes[0].clone();
    next.id = "write".into();
    value.nodes.push(next);
    let saved = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &value,
            None,
            "serial-plan",
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
    let materialized = db
        .materialize_host_goal_plan(OWNER, &goal.goal_id, 1, &saved.plan.hash, &receipt, NOW + 2)
        .unwrap();
    let ready = db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            materialized.goal.revision,
            1,
            &saved.plan.hash,
            "Start",
            "confirm",
            NOW + 3,
        )
        .unwrap()
        .goal;
    assert!(db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            ready.revision,
            "write",
            false,
            "skip",
            NOW + 4
        )
        .is_err());
    let first = db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            ready.revision,
            "research",
            false,
            "first",
            NOW + 4,
        )
        .unwrap();
    prepare_review(&db, &first.attempt.binding, None);
    let passed = db
        .check_host_goal_node(
            OWNER,
            &first.attempt.binding,
            first.goal.revision,
            &evidence(true),
            &[],
            "first-check",
            NOW + 5,
        )
        .unwrap();
    assert_eq!(passed.goal.phase, HostGoalPhase::Ready);
    let second = db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            passed.goal.revision,
            "write",
            false,
            "second",
            NOW + 6,
        )
        .unwrap();
    assert_eq!(second.attempt.binding.node_id, "write");
}

#[test]
fn host_goal_tests_failed_checkpoint_is_atomic() {
    let (_dir, db) = database();
    let goal = new_goal(&db);
    db.conn_for_tests().execute_batch("CREATE TRIGGER fail_host_checkpoint BEFORE INSERT ON wc_host_goal_checkpoints BEGIN SELECT RAISE(ABORT,'simulated disk failure'); END;").unwrap();
    let state = HostGoalState {
        next_step: "Must not be persisted".into(),
        ..Default::default()
    };
    assert!(db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            None,
            &state,
            HostGoalCheckpointAction::Handoff,
            "Save and switch",
            "fail",
            NOW + 1
        )
        .is_err());
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id).unwrap().unwrap(),
        goal
    );
    assert!(db
        .list_host_goal_checkpoints(OWNER, &goal.goal_id, 0, 100)
        .unwrap()
        .is_empty());
}

#[test]
fn host_goal_tests_late_plan_materialization_cannot_revive_cancelled_goal() {
    let (_dir, db) = database();
    let goal = new_goal(&db);
    let prepared = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &plan(),
            None,
            "prepare",
            NOW + 1,
        )
        .unwrap();
    let cancelled = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            prepared.goal.revision,
            None,
            &prepared.goal.state,
            HostGoalCheckpointAction::Cancel,
            "User cancelled",
            "cancel",
            NOW + 2,
        )
        .unwrap();
    let receipt = HostGoalMaterialization {
        json_path: "/state/plan.json".into(),
        json_hash: "a".repeat(64),
        markdown_path: "/state/plan.md".into(),
        markdown_hash: "b".repeat(64),
        verified_at: NOW + 3,
    };
    assert!(db
        .materialize_host_goal_plan(
            OWNER,
            &goal.goal_id,
            prepared.plan.version,
            &prepared.plan.hash,
            &receipt,
            NOW + 3
        )
        .is_err());
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id).unwrap().unwrap(),
        cancelled.goal
    );
    assert_eq!(
        db.latest_host_goal_checkpoint(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .reason,
        "User cancelled"
    );
}

#[test]
fn host_goal_tests_latest_history_is_owner_scoped_and_retains_terminal_evidence() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    db.begin_host_goal_operation(OWNER, &binding, "op", "read_file", "payload", NOW + 5)
        .unwrap();
    let completed = db
        .finish_host_goal_operation(
            OWNER,
            &goal.goal_id,
            "op",
            HostGoalOperationStatus::Succeeded,
            None,
            Some("Read source"),
            NOW + 6,
        )
        .unwrap();
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "op")
            .unwrap(),
        Some(completed)
    );
    assert!(db
        .get_host_goal_operation(OTHER, &goal.goal_id, "op")
        .is_err());
    db.check_host_goal_node(
        OWNER,
        &binding,
        goal.revision,
        &evidence(true),
        &evidence(true),
        "pass",
        NOW + 7,
    )
    .unwrap();
    let latest = db
        .latest_host_goal_attempt(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.status, HostGoalAttemptStatus::Passed);
    assert_eq!(latest.checks.len(), 1);
    assert_eq!(latest.final_checks.len(), 1);
    assert!(db.latest_host_goal_attempt(OTHER, &goal.goal_id).is_err());
}

#[test]
fn host_goal_tests_explicit_contract_change_preserves_history_and_requires_new_confirmation() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    let old_plan = db
        .get_host_goal_plan(OWNER, &goal.goal_id, None)
        .unwrap()
        .unwrap();
    let completed = db
        .check_host_goal_node(
            OWNER,
            &binding,
            goal.revision,
            &evidence(true),
            &evidence(true),
            "complete-original",
            NOW + 5,
        )
        .unwrap()
        .goal;
    let mut revised = plan();
    revised.objective = "Deliver a comparison with one additional source".into();
    revised.final_criteria.push(GoalCriterion::Review {
        id: "additional-source".into(),
        description: "The comparison includes the requested extra primary source".into(),
    });
    revised.nodes[0]
        .criterion_ids
        .push("additional-source".into());
    assert!(db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            completed.revision,
            &revised,
            None,
            "no-request",
            NOW + 6
        )
        .is_err());
    assert!(db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            completed.revision,
            &revised,
            Some(" "),
            "empty-request",
            NOW + 6
        )
        .is_err());
    let request = "请修改目标，增加一个官方来源作为最终验收项";
    let prepared = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            completed.revision,
            &revised,
            Some(request),
            "change-contract",
            NOW + 6,
        )
        .unwrap();
    assert_eq!(prepared.goal.goal_id, goal.goal_id);
    assert_eq!(prepared.goal.acceptance_contract_version, 2);
    assert_eq!(prepared.plan.acceptance_contract_version, 2);
    assert_eq!(prepared.plan.change_request.as_deref(), Some(request));
    assert_eq!(prepared.plan.acceptance_changes.len(), 2);
    assert!(prepared.goal.completed_node_ids.is_empty());
    assert!(prepared.goal.confirmation.is_none());
    assert!(db.validate_host_goal_binding(OWNER, &binding).is_err());
    assert_eq!(
        db.get_host_goal_plan(OWNER, &goal.goal_id, Some(1))
            .unwrap()
            .unwrap(),
        old_plan
    );
    assert_eq!(
        db.list_host_goal_attempts(OWNER, &goal.goal_id, 0, 1)
            .unwrap()[0]
            .status,
        HostGoalAttemptStatus::Passed
    );
    assert!(db
        .list_host_goal_attempts(OWNER, &goal.goal_id, binding.fence, 1)
        .unwrap()
        .is_empty());
    let receipt = HostGoalMaterialization {
        json_path: "/state/goals/v2/plan.json".into(),
        json_hash: "c".repeat(64),
        markdown_path: "/state/goals/v2/plan.md".into(),
        markdown_hash: "d".repeat(64),
        verified_at: NOW + 7,
    };
    let published = db
        .materialize_host_goal_plan(
            OWNER,
            &goal.goal_id,
            prepared.plan.version,
            &prepared.plan.hash,
            &receipt,
            NOW + 7,
        )
        .unwrap()
        .goal;
    assert_eq!(published.phase, HostGoalPhase::AwaitingConfirmation);
    assert!(db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            published.revision,
            "research",
            true,
            "premature",
            NOW + 8
        )
        .is_err());
    assert!(db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            published.revision,
            1,
            &old_plan.hash,
            "Start",
            "old-contract",
            NOW + 8
        )
        .is_err());
    let ready = db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            published.revision,
            2,
            &prepared.plan.hash,
            "我看完新计划，现在开始",
            "confirm-v2",
            NOW + 8,
        )
        .unwrap()
        .goal;
    assert_eq!(ready.phase, HostGoalPhase::Ready);
    assert!(ready.confirmed());
}

#[test]
fn host_goal_tests_handoff_saves_state_without_advancing_node_or_execution_authority() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    let mut state = goal.state.clone();
    state.completed_summary =
        "Collected primary sources; comparison still needs verification".into();
    state.next_step = "Finish comparison and run the node acceptance checks".into();
    let saved = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &state,
            HostGoalCheckpointAction::Handoff,
            "Saved before switching conversations",
            "handoff",
            NOW + 5,
        )
        .unwrap()
        .goal;
    assert_eq!(saved.phase, HostGoalPhase::Executing);
    assert_eq!(saved.current_node_id, goal.current_node_id);
    assert_eq!(saved.current_attempt_id, goal.current_attempt_id);
    assert_eq!(saved.fence, goal.fence);
    assert_eq!(saved.completed_node_ids, goal.completed_node_ids);
    assert_eq!(saved.confirmation, goal.confirmation);
    assert_eq!(saved.state, state);
    assert!(db.validate_host_goal_binding(OWNER, &binding).is_ok());
}

#[test]
fn host_goal_tests_replan_preserves_pending_operation_and_fences_original_attempt() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    let operation = db
        .begin_host_goal_operation(
            OWNER,
            &binding,
            "pending",
            "run_process",
            "payload",
            NOW + 5,
        )
        .unwrap()
        .operation;
    let replanning = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            HostGoalCheckpointAction::Replan,
            "Discuss revised implementation",
            "replan",
            NOW + 6,
        )
        .unwrap()
        .goal;
    assert_eq!(replanning.phase, HostGoalPhase::Planning);
    assert!(replanning.confirmation.is_none());
    assert!(db.validate_host_goal_binding(OWNER, &binding).is_err());
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "pending")
            .unwrap(),
        Some(operation)
    );
    assert_eq!(
        db.list_host_goal_operations(OWNER, &goal.goal_id, 0, 100, true)
            .unwrap()
            .len(),
        1
    );
    assert!(db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            replanning.revision,
            &plan(),
            None,
            "blocked-plan",
            NOW + 7
        )
        .is_err());
    db.finish_host_goal_operation(
        OWNER,
        &goal.goal_id,
        "pending",
        HostGoalOperationStatus::Succeeded,
        None,
        Some("Recovered original result"),
        NOW + 8,
    )
    .unwrap();
    let saved = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            replanning.revision,
            &plan(),
            None,
            "after-recovery",
            NOW + 9,
        )
        .unwrap();
    assert_eq!(saved.goal.plan_version, 2);
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "pending")
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Succeeded
    );
}

#[test]
fn host_goal_tests_client_check_receipt_replays_after_completion_and_restart() {
    let (dir, db) = database();
    let (goal, binding) = claimed(&db);
    assert!(db
        .reserve_host_goal_check_request(OWNER, &goal.goal_id, "request", "client-hash")
        .unwrap()
        .is_none());
    assert_eq!(
        db.reserve_host_goal_check_request(OWNER, &goal.goal_id, "request", "changed-client-hash")
            .unwrap_err()
            .code(),
        "goal_idempotency_conflict"
    );
    let complete = db
        .check_host_goal_node_with_request(
            OWNER,
            &binding,
            goal.revision,
            &evidence(true),
            &evidence(true),
            "request",
            "client-hash",
            NOW + 5,
        )
        .unwrap();
    assert_eq!(complete.goal.phase, HostGoalPhase::Completed);
    assert!(complete.goal.current_attempt_id.is_none());
    assert!(db.validate_host_goal_binding(OWNER, &binding).is_err());
    let replay = db
        .lookup_host_goal_check_replay(OWNER, &goal.goal_id, "request", "client-hash")
        .unwrap()
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.goal, complete.goal);
    assert!(db
        .lookup_host_goal_check_replay(OTHER, &goal.goal_id, "request", "client-hash")
        .is_err());
    assert_eq!(
        db.lookup_host_goal_check_replay(OWNER, &goal.goal_id, "request", "different")
            .unwrap_err()
            .code(),
        "goal_idempotency_conflict"
    );
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(
        db.lookup_host_goal_check_replay(OWNER, &goal.goal_id, "request", "client-hash")
            .unwrap()
            .unwrap()
            .goal,
        complete.goal
    );
}

#[test]
fn host_goal_tests_check_receipt_and_acceptance_commit_atomically() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    db.reserve_host_goal_check_request(OWNER, &goal.goal_id, "request", "client-hash")
        .unwrap();
    db.conn_for_tests().execute_batch("CREATE TRIGGER fail_check_receipt BEFORE UPDATE ON wc_host_goal_check_requests BEGIN SELECT RAISE(ABORT,'simulated receipt failure'); END;").unwrap();
    assert!(db
        .check_host_goal_node_with_request(
            OWNER,
            &binding,
            goal.revision,
            &evidence(true),
            &evidence(true),
            "request",
            "client-hash",
            NOW + 5
        )
        .is_err());
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id).unwrap().unwrap(),
        goal
    );
    assert!(db
        .lookup_host_goal_check_replay(OWNER, &goal.goal_id, "request", "client-hash")
        .unwrap()
        .is_none());
    assert_eq!(
        db.get_host_goal_attempt(OWNER, &goal.goal_id, &binding.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        HostGoalAttemptStatus::Running
    );
    db.conn_for_tests()
        .execute_batch("DROP TRIGGER fail_check_receipt;")
        .unwrap();
    assert_eq!(
        db.check_host_goal_node_with_request(
            OWNER,
            &binding,
            goal.revision,
            &evidence(true),
            &evidence(true),
            "request",
            "client-hash",
            NOW + 6
        )
        .unwrap()
        .goal
        .phase,
        HostGoalPhase::Completed
    );
}

#[test]
fn host_goal_tests_check_command_retains_manifest_and_rejects_changed_replay() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    let original = vec![webcodex_core::goal::GoalArtifactSnapshot {
        path: "result.txt".into(),
        sha256: Some("a".repeat(64)),
    }];
    let op = db
        .begin_host_goal_operation_with_manifest(
            OWNER,
            &binding,
            "check-operation",
            "run_detached_process",
            "fingerprint",
            &original,
            NOW + 5,
        )
        .unwrap();
    assert_eq!(op.operation.input_manifest, original);
    let mut changed = original.clone();
    changed[0].sha256 = Some("b".repeat(64));
    assert_eq!(
        db.begin_host_goal_operation_with_manifest(
            OWNER,
            &binding,
            "check-operation",
            "run_detached_process",
            "fingerprint",
            &changed,
            NOW + 6
        )
        .unwrap_err()
        .code(),
        "goal_idempotency_conflict"
    );
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "check-operation")
            .unwrap()
            .unwrap()
            .input_manifest,
        original
    );
}

#[test]
fn host_goal_tests_claim_exact_request_replays_before_takeover_admission() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let claim = db
        .claim_host_goal_node_for_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            "research",
            false,
            goal.plan_version,
            None,
            "wire-claim",
            NOW + 4,
        )
        .unwrap();
    let replay = db
        .lookup_host_goal_claim_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            "research",
            false,
            goal.plan_version,
            None,
            "wire-claim",
        )
        .unwrap()
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt, claim.attempt);
    assert_eq!(
        db.lookup_host_goal_claim_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            "research",
            false,
            goal.plan_version,
            Some(&claim.attempt.binding.attempt_id),
            "wire-claim"
        )
        .unwrap_err()
        .code(),
        "goal_idempotency_conflict"
    );
    assert!(db
        .claim_host_goal_node_for_plan(
            OWNER,
            &goal.goal_id,
            claim.goal.revision,
            "research",
            false,
            goal.plan_version,
            None,
            "forgot-takeover",
            NOW + 5
        )
        .is_err());
    let takeover = db
        .claim_host_goal_node_for_plan(
            OWNER,
            &goal.goal_id,
            claim.goal.revision,
            "research",
            false,
            goal.plan_version,
            Some(&claim.attempt.binding.attempt_id),
            "takeover",
            NOW + 5,
        )
        .unwrap();
    assert_ne!(
        takeover.attempt.binding.attempt_id,
        claim.attempt.binding.attempt_id
    );
}

#[test]
fn host_goal_tests_checkpoint_raw_receipt_survives_missing_state_and_changed_current_state() {
    let (_dir, db) = database();
    let goal = new_goal(&db);
    db.reserve_host_goal_checkpoint_request(
        OWNER,
        &goal.goal_id,
        "raw-checkpoint",
        "raw-patch-with-no-state",
    )
    .unwrap();
    let saved = db
        .checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            None,
            &goal.state,
            HostGoalCheckpointAction::Handoff,
            "Save the current state",
            "raw-checkpoint",
            "raw-patch-with-no-state",
            &[],
            NOW + 1,
        )
        .unwrap();
    let mut later_state = saved.goal.state.clone();
    later_state.next_step = "A different conversation added a requirement".into();
    let later = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            saved.goal.revision,
            None,
            &later_state,
            HostGoalCheckpointAction::Progress,
            "Later progress",
            "later",
            NOW + 2,
        )
        .unwrap();
    let replay = db
        .lookup_host_goal_checkpoint_replay(
            OWNER,
            &goal.goal_id,
            "raw-checkpoint",
            "raw-patch-with-no-state",
        )
        .unwrap()
        .unwrap();
    assert_eq!(replay.goal, saved.goal);
    assert!(replay.replayed);
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id).unwrap().unwrap(),
        later.goal
    );
    assert_eq!(
        db.lookup_host_goal_checkpoint_replay(
            OWNER,
            &goal.goal_id,
            "raw-checkpoint",
            "different-patch"
        )
        .unwrap_err()
        .code(),
        "goal_idempotency_conflict"
    );
    assert!(db
        .lookup_host_goal_checkpoint_replay(
            OTHER,
            &goal.goal_id,
            "raw-checkpoint",
            "raw-patch-with-no-state"
        )
        .is_err());
}

#[test]
fn host_goal_tests_plan_file_union_is_bounded_before_publication() {
    let mut value = plan();
    value.nodes.clear();
    for group in 0..3 {
        let mut node = plan().nodes.remove(0);
        node.id = format!("node-{group}");
        node.acceptance = (0..if group < 2 { 32 } else { 1 })
            .map(|index| GoalCriterion::FileExists {
                id: format!("file-{group}-{index}"),
                description: "Required file".into(),
                path: format!("output/{group}-{index}.txt"),
            })
            .collect();
        value.nodes.push(node);
    }
    assert!(value.validate().unwrap_err().contains("64 distinct"));
    value.nodes.pop();
    assert!(value.validate().is_ok());
}

#[test]
fn host_goal_tests_unconfirmed_paused_plan_can_be_confirmed_but_confirmed_pause_cannot() {
    let (_dir, db) = database();
    let (goal, version) = published(&db);
    let paused = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            None,
            &goal.state,
            HostGoalCheckpointAction::Pause,
            "Pause before user confirmation",
            "pause-unconfirmed",
            NOW + 3,
        )
        .unwrap()
        .goal;
    let confirmed = db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            paused.revision,
            version.version,
            &version.hash,
            "I read this version, begin now",
            "confirm-paused",
            NOW + 4,
        )
        .unwrap()
        .goal;
    assert_eq!(confirmed.phase, HostGoalPhase::Ready);
    let paused_again = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            confirmed.revision,
            None,
            &confirmed.state,
            HostGoalCheckpointAction::Pause,
            "Pause confirmed work",
            "pause-confirmed",
            NOW + 5,
        )
        .unwrap()
        .goal;
    assert!(db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            paused_again.revision,
            version.version,
            &version.hash,
            "Start",
            "duplicate-new-confirm",
            NOW + 6
        )
        .is_err());
    assert!(db
        .claim_host_goal_node(
            OWNER,
            &goal.goal_id,
            paused_again.revision,
            "research",
            false,
            "resume-confirmed",
            NOW + 6
        )
        .is_ok());
}

#[test]
fn host_goal_tests_command_requires_explicit_confined_verification_paths() {
    let mut value = plan();
    value.nodes[0].acceptance = vec![GoalCriterion::Command {
        id: "tests".into(),
        description: "Test inputs".into(),
        program: "cargo".into(),
        args: vec!["test".into()],
        cwd: None,
        timeout_secs: 60,
    }];
    assert!(value.validate().unwrap_err().contains("verification_paths"));
    value.verification_paths = vec!["../outside.rs".into()];
    assert!(value.validate().is_err());
    value.verification_paths = vec!["src/lib.rs".into(), "Cargo.toml".into()];
    assert!(value.validate().is_ok());
}

#[test]
fn host_goal_tests_operation_resolution_is_atomic_with_binding_state_and_receipt() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    db.begin_host_goal_operation(
        OWNER,
        &binding,
        "business-effect",
        "write_file",
        "payload",
        NOW + 5,
    )
    .unwrap();
    let original = db
        .finish_host_goal_operation(
            OWNER,
            &goal.goal_id,
            "business-effect",
            HostGoalOperationStatus::Unknown,
            None,
            Some("Delivery interrupted"),
            NOW + 6,
        )
        .unwrap();
    let resolution=HostGoalOperationResolution{operation_id:"business-effect".into(),status:HostGoalOperationStatus::Succeeded,report:serde_json::json!({"source":"user_reconciliation_report","evidence":"User inspected the file","source_refs":["result.txt"],"user_confirmation":"I confirmed the write"}).to_string()};
    let mut stale = binding.clone();
    stale.fence += 1;
    assert!(db
        .checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&stale),
            &goal.state,
            HostGoalCheckpointAction::Handoff,
            "Resolve",
            "stale",
            "request-a",
            std::slice::from_ref(&resolution),
            NOW + 7
        )
        .is_err());
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "business-effect")
            .unwrap(),
        Some(original.clone())
    );
    let mut invalid = goal.state.clone();
    invalid.next_step = "x".repeat(4097);
    assert!(db
        .checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &invalid,
            HostGoalCheckpointAction::Handoff,
            "Resolve",
            "invalid",
            "request-b",
            std::slice::from_ref(&resolution),
            NOW + 7
        )
        .is_err());
    db.conn_for_tests().execute_batch("CREATE TRIGGER resolution_receipt_failure BEFORE UPDATE ON wc_host_goal_checkpoint_requests BEGIN SELECT RAISE(ABORT,'simulated receipt failure'); END;").unwrap();
    assert!(db
        .checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            HostGoalCheckpointAction::Handoff,
            "Resolve",
            "receipt-fails",
            "request-c",
            std::slice::from_ref(&resolution),
            NOW + 7
        )
        .is_err());
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "business-effect")
            .unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id).unwrap().unwrap(),
        goal
    );
    db.conn_for_tests()
        .execute_batch("DROP TRIGGER resolution_receipt_failure;")
        .unwrap();
    assert!(db
        .checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            HostGoalCheckpointAction::Handoff,
            "Resolve",
            "duplicate",
            "request-d",
            &[resolution.clone(), resolution.clone()],
            NOW + 7
        )
        .is_err());
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "business-effect")
            .unwrap(),
        Some(original)
    );
    let saved = db
        .checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            HostGoalCheckpointAction::Handoff,
            "Resolve",
            "accepted",
            "request-e",
            &[resolution],
            NOW + 8,
        )
        .unwrap();
    assert_eq!(saved.goal.phase, HostGoalPhase::Executing);
    assert_eq!(
        db.get_host_goal_operation(OWNER, &goal.goal_id, "business-effect")
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Succeeded
    );
    assert!(
        db.lookup_host_goal_checkpoint_replay(OWNER, &goal.goal_id, "accepted", "request-e")
            .unwrap()
            .unwrap()
            .replayed
    );
}

#[test]
fn host_goal_tests_internal_check_and_tracked_job_cannot_be_manually_accepted() {
    for (id, job) in [
        ("check_internal", None),
        ("business_job", Some("job-existing")),
    ] {
        let (_dir, db) = database();
        let (goal, binding) = claimed(&db);
        db.begin_host_goal_operation(
            OWNER,
            &binding,
            id,
            "run_detached_process",
            "payload",
            NOW + 5,
        )
        .unwrap();
        let original = db
            .finish_host_goal_operation(
                OWNER,
                &goal.goal_id,
                id,
                HostGoalOperationStatus::Unknown,
                job,
                Some("Interrupted"),
                NOW + 6,
            )
            .unwrap();
        let report = HostGoalOperationResolution {
            operation_id: id.into(),
            status: HostGoalOperationStatus::Succeeded,
            report: "User says done".into(),
        };
        let refusal = db
            .checkpoint_host_goal_with_request(
                OWNER,
                &goal.goal_id,
                goal.revision,
                Some(&binding),
                &goal.state,
                HostGoalCheckpointAction::Progress,
                "Resolve",
                "manual",
                "request",
                &[report],
                NOW + 7,
            )
            .unwrap_err();
        assert_eq!(
            refusal.code(),
            if job.is_some() {
                "goal_job_reconciliation_required"
            } else {
                "goal_check_reconciliation_required"
            }
        );
        assert_eq!(
            db.get_host_goal_operation(OWNER, &goal.goal_id, id)
                .unwrap(),
            Some(original)
        );
    }
}

#[test]
fn host_goal_tests_shared_and_node_recovery_budgets_are_checked_before_saving() {
    let mut value = plan();
    value.objective = "x".repeat(8192);
    assert!(value
        .validate()
        .unwrap_err()
        .contains("Shared plan requirements"));
    value = plan();
    value.nodes[0].work = "x".repeat(8192);
    assert!(value.validate().unwrap_err().contains("recovery budget"));
    value = plan();
    value.objective = "x".repeat(6000);
    value.nodes[0].work = "x".repeat(6000);
    assert!(value.validate().unwrap_err().contains("recovery budget"));
    value = plan();
    value.objective = "x".repeat(3000);
    value.nodes[0].work = "x".repeat(3000);
    assert!(value.validate().is_ok());
}

#[test]
fn host_goal_tests_abandoned_business_intent_can_be_explicitly_resolved_atomically() {
    let (dir, db) = database();
    let (goal, binding) = claimed(&db);
    let intent = db
        .begin_host_goal_operation(
            OWNER,
            &binding,
            "write-before-crash",
            "write_file",
            "payload",
            NOW + 5,
        )
        .unwrap()
        .operation;
    assert_eq!(intent.status, HostGoalOperationStatus::Intent);
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let resolution=HostGoalOperationResolution{operation_id:intent.operation_id.clone(),status:HostGoalOperationStatus::Succeeded,report:serde_json::json!({"source":"user_reconciliation_report","evidence":"User inspected result.txt after the old Server process exited","source_refs":["result.txt"],"user_confirmation":"The original write completed; continue"}).to_string()};
    // Runtime must first prove this old operation has no active request guard.
    // The store owns the remaining revision, binding and atomic-commit checks.
    let saved = db
        .checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            HostGoalCheckpointAction::Handoff,
            "User reconciled pre-crash intent",
            "reconcile-intent",
            "exact-request",
            &[resolution],
            NOW + 6,
        )
        .unwrap();
    assert_eq!(saved.goal.phase, HostGoalPhase::Executing);
    let resolved = db
        .get_host_goal_operation(OWNER, &goal.goal_id, &intent.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, HostGoalOperationStatus::Succeeded);
    assert!(resolved
        .result_summary
        .unwrap()
        .contains("user_reconciliation_report"));
    assert!(db
        .list_host_goal_operations(OWNER, &goal.goal_id, 0, 100, true)
        .unwrap()
        .is_empty());
    assert!(
        db.lookup_host_goal_checkpoint_replay(
            OWNER,
            &goal.goal_id,
            "reconcile-intent",
            "exact-request"
        )
        .unwrap()
        .unwrap()
        .replayed
    );
}

#[test]
fn host_goal_tests_find_operation_by_job_is_exact_owned_and_preserves_terminal_history() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    db.begin_host_goal_operation(
        OWNER,
        &binding,
        "job-operation",
        "run_process",
        "payload",
        NOW + 5,
    )
    .unwrap();
    let running = db
        .finish_host_goal_operation(
            OWNER,
            &goal.goal_id,
            "job-operation",
            HostGoalOperationStatus::Running,
            Some("job-owned-exact"),
            None,
            NOW + 6,
        )
        .unwrap();
    assert_eq!(
        db.find_host_goal_operation_by_job(OWNER, &goal.goal_id, "job-owned-exact")
            .unwrap(),
        Some(running)
    );
    assert!(db
        .find_host_goal_operation_by_job(OWNER, &goal.goal_id, "job-owned")
        .unwrap()
        .is_none());
    assert!(db
        .find_host_goal_operation_by_job(OWNER, &goal.goal_id, "unrelated-job")
        .unwrap()
        .is_none());
    assert!(db
        .find_host_goal_operation_by_job(OTHER, &goal.goal_id, "job-owned-exact")
        .is_err());
    let cancelled = db
        .finish_host_goal_operation(
            OWNER,
            &goal.goal_id,
            "job-operation",
            HostGoalOperationStatus::Cancelled,
            Some("job-owned-exact"),
            Some("Authoritative stop confirmed"),
            NOW + 7,
        )
        .unwrap();
    assert_eq!(
        db.find_host_goal_operation_by_job(OWNER, &goal.goal_id, "job-owned-exact")
            .unwrap(),
        Some(cancelled)
    );
}

#[test]
fn host_goal_tests_unresolved_internal_check_allows_only_explicit_cancellation() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    db.begin_host_goal_operation(
        OWNER,
        &binding,
        "check_uncertain",
        "run_detached_process",
        "payload",
        NOW + 5,
    )
    .unwrap();
    for status in [
        HostGoalOperationStatus::Succeeded,
        HostGoalOperationStatus::Failed,
    ] {
        let resolution = HostGoalOperationResolution {
            operation_id: "check_uncertain".into(),
            status,
            report: "User inspected the unknown attempt".into(),
        };
        let error = db
            .checkpoint_host_goal_with_request(
                OWNER,
                &goal.goal_id,
                goal.revision,
                Some(&binding),
                &goal.state,
                HostGoalCheckpointAction::Progress,
                "Resolve internal check",
                "disallowed",
                "request",
                &[resolution],
                NOW + 6,
            )
            .unwrap_err();
        assert_eq!(error.code(), "goal_check_reconciliation_required");
    }
    let cancelled=db.checkpoint_host_goal_with_request(OWNER,&goal.goal_id,goal.revision,Some(&binding),&goal.state,HostGoalCheckpointAction::Progress,"Explicitly abandon uncertain check","cancel-check","cancel-request",&[HostGoalOperationResolution{operation_id:"check_uncertain".into(),status:HostGoalOperationStatus::Cancelled,report:serde_json::json!({"source":"user_reconciliation_report","user_confirmation":"I verified this uncertain check; cancel it and create a new attempt"}).to_string()}],NOW+7).unwrap();
    assert_eq!(cancelled.goal.phase, HostGoalPhase::Executing);
    let operation = db
        .get_host_goal_operation(OWNER, &goal.goal_id, "check_uncertain")
        .unwrap()
        .unwrap();
    assert_eq!(operation.status, HostGoalOperationStatus::Cancelled);
    assert!(operation.job_id.is_none());
    assert!(db
        .latest_host_goal_attempt(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap()
        .checks
        .is_empty());
}

#[test]
fn host_goal_tests_native_run_lookup_and_resolution_require_exact_trusted_source() {
    for (tool, source, trusted) in [
        ("coding_agent_start", "runtime_coding_agent", true),
        ("plugin_tool", "runtime_coding_agent", false),
        ("coding_agent_start", "plugin_gateway", false),
    ] {
        let (_dir, db) = database();
        let (goal, binding) = claimed(&db);
        db.begin_host_goal_operation(OWNER, &binding, "native-op", tool, "payload", NOW + 5)
            .unwrap();
        let summary=serde_json::json!({"source":source,"run_id":"native-run-exact","state":"running","execution_state":"started"}).to_string();
        let original = db
            .finish_host_goal_operation(
                OWNER,
                &goal.goal_id,
                "native-op",
                HostGoalOperationStatus::Unknown,
                None,
                Some(&summary),
                NOW + 6,
            )
            .unwrap();
        assert_eq!(
            db.find_host_goal_operation_by_native_run(OWNER, &goal.goal_id, "native-run-exact")
                .unwrap(),
            if trusted {
                Some(original.clone())
            } else {
                None
            }
        );
        assert!(db
            .find_host_goal_operation_by_native_run(OWNER, &goal.goal_id, "native-run")
            .unwrap()
            .is_none());
        assert!(db
            .find_host_goal_operation_by_native_run(OTHER, &goal.goal_id, "native-run-exact")
            .is_err());
        let resolution = HostGoalOperationResolution {
            operation_id: "native-op".into(),
            status: HostGoalOperationStatus::Succeeded,
            report: "User inspected the result".into(),
        };
        let result = db.checkpoint_host_goal_with_request(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            HostGoalCheckpointAction::Progress,
            "Resolve",
            "resolution",
            "request",
            &[resolution],
            NOW + 7,
        );
        if trusted {
            assert_eq!(
                result.unwrap_err().code(),
                "goal_run_reconciliation_required"
            );
            assert_eq!(
                db.get_host_goal_operation(OWNER, &goal.goal_id, "native-op")
                    .unwrap(),
                Some(original)
            );
        } else {
            assert!(result.is_ok());
        }
    }
}

#[test]
fn host_goal_tests_node_command_paths_override_or_inherit_plan_defaults() {
    let mut value = plan();
    value.nodes[0].acceptance = vec![GoalCriterion::Command {
        id: "tests".into(),
        description: "Test current inputs".into(),
        program: "cargo".into(),
        args: vec!["test".into()],
        cwd: None,
        timeout_secs: 60,
    }];
    value.nodes[0].verification_paths = vec!["src/lib.rs".into()];
    assert!(value.verification_paths.is_empty());
    assert!(value.validate().is_ok());
    value.nodes[0].verification_paths.clear();
    assert!(value.validate().is_err());
    value.verification_paths = vec!["Cargo.toml".into()];
    assert!(value.validate().is_ok());
    value.nodes[0].verification_paths = vec!["../outside.rs".into()];
    assert!(value.validate().is_err());
}

fn managed_context(db: &Database, epoch: &str) -> HostGoalInvocationContext {
    let context = HostGoalInvocationContext {
        context_epoch: epoch.into(),
        identity_source: "explicit_context_only".into(),
    };
    db.ensure_host_context_identity(OWNER, &context).unwrap();
    context
}

fn managed_claim(
    db: &Database,
    goal: &HostGoalRecord,
    context: &HostGoalInvocationContext,
    key: &str,
) -> super::HostGoalClaimMutation {
    db.claim_host_goal_node_for_plan_in_context(
        OWNER,
        &goal.goal_id,
        goal.revision,
        "research",
        false,
        goal.plan_version,
        goal.current_attempt_id.as_deref(),
        key,
        NOW + 10,
        Some(context),
    )
    .unwrap()
}

fn managed_handoff(
    db: &Database,
    claim: &super::HostGoalClaimMutation,
    context: &HostGoalInvocationContext,
    key: &str,
    pending: &[HostGoalOperation],
) -> Result<super::HostGoalMutation, super::GoalStoreError> {
    db.checkpoint_host_goal_with_request_in_context(
        OWNER,
        &claim.goal.goal_id,
        claim.goal.revision,
        Some(&claim.attempt.binding),
        &claim.goal.state,
        HostGoalCheckpointAction::Handoff,
        "Synthetic test-driver handoff",
        key,
        key,
        &[],
        NOW + 20,
        Some(context),
        pending,
    )
}

#[test]
fn host_goal_tests_managed_handoff_atomic_retirement_replay_and_new_epoch_binding() {
    let (dir, db) = database();
    let goal = confirmed(&db);
    let a = managed_context(&db, "source-a");
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    let handed = managed_handoff(&db, &claim, &a, "handoff-a", &[]).unwrap();
    let source = db
        .read_host_context_state(OWNER, "source-a")
        .unwrap()
        .unwrap();
    assert_eq!(source.state, "retired");
    assert_eq!(
        source.retirement_goal_id.as_deref(),
        Some(goal.goal_id.as_str())
    );
    let checkpoint = db
        .latest_host_goal_checkpoint(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(source.checkpoint_sequence, Some(checkpoint.sequence));
    assert_eq!(checkpoint.action.as_deref(), Some("handoff"));
    assert_eq!(checkpoint.source_context_epoch.as_deref(), Some("source-a"));
    assert_eq!(checkpoint.source_fence, Some(claim.goal.fence));
    assert!(
        managed_handoff(&db, &claim, &a, "handoff-a", &[])
            .unwrap()
            .replayed
    );
    assert_eq!(
        db.latest_host_goal_checkpoint(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .sequence,
        checkpoint.sequence
    );
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    db.ensure_host_context_identity(OWNER, &a).unwrap();
    assert_eq!(
        db.gate_host_context(OWNER, Some(&a), true)
            .unwrap_err()
            .code(),
        "goal_chat_retired"
    );
    assert_eq!(
        db.create_host_goal_at_in_context(
            OWNER,
            "project",
            "/workspace/project",
            "Escape retired context",
            "escape",
            NOW,
            Some(&a)
        )
        .unwrap_err()
        .code(),
        "goal_chat_retired"
    );
    let b = managed_context(&db, "target-b");
    assert_eq!(
        db.begin_host_goal_operation_with_manifest_in_context(
            OWNER,
            &claim.attempt.binding,
            "steal",
            "write_file",
            "hash",
            &[],
            NOW,
            Some(&b),
            false
        )
        .unwrap_err()
        .code(),
        "goal_context_attempt_mismatch"
    );
    let next = managed_claim(&db, &handed.goal, &b, "claim-b");
    assert!(next.goal.fence > claim.goal.fence);
    assert_eq!(next.attempt.context_epoch.as_deref(), Some("target-b"));
    db.record_host_goal_context_ref(OWNER, &goal.goal_id, "target-b")
        .unwrap();
    assert_eq!(
        db.read_host_context_state(OWNER, "source-a")
            .unwrap()
            .unwrap(),
        source
    );
    db.begin_host_goal_operation_with_manifest_in_context(
        OWNER,
        &next.attempt.binding,
        "next-op",
        "write_file",
        "hash",
        &[],
        NOW,
        Some(&b),
        false,
    )
    .unwrap();
}

#[test]
fn host_goal_tests_managed_handoff_failure_rolls_back_checkpoint_retirement_and_receipt() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let a = managed_context(&db, "source-a");
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    let before = db
        .latest_host_goal_checkpoint(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    db.conn_for_tests().execute_batch("CREATE TRIGGER handoff_failure BEFORE UPDATE OF state ON wc_host_context_states WHEN NEW.state=3 BEGIN SELECT RAISE(ABORT,'injected retirement write failure'); END;").unwrap();
    assert!(managed_handoff(&db, &claim, &a, "handoff-a", &[]).is_err());
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .revision,
        claim.goal.revision
    );
    assert_eq!(
        db.latest_host_goal_checkpoint(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap(),
        before
    );
    assert_eq!(
        db.read_host_context_state(OWNER, "source-a")
            .unwrap()
            .unwrap()
            .state,
        "active"
    );
    assert!(db
        .lookup_host_goal_checkpoint_replay(OWNER, &goal.goal_id, "handoff-a", "handoff-a")
        .unwrap()
        .is_none());
    db.conn_for_tests()
        .execute_batch("DROP TRIGGER handoff_failure;")
        .unwrap();
    managed_handoff(&db, &claim, &a, "handoff-a", &[]).unwrap();
}

#[test]
fn host_goal_tests_managed_handoff_unknown_and_changed_job_snapshots_block() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let a = managed_context(&db, "source-a");
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    let intent = db
        .begin_host_goal_operation_with_manifest_in_context(
            OWNER,
            &claim.attempt.binding,
            "work",
            "exec_command",
            "hash",
            &[],
            NOW,
            Some(&a),
            false,
        )
        .unwrap()
        .operation;
    assert_eq!(
        managed_handoff(&db, &claim, &a, "handoff-a", &[intent])
            .unwrap_err()
            .code(),
        "goal_handoff_reconciliation_required"
    );
    let running = db
        .finish_host_goal_operation(
            OWNER,
            &goal.goal_id,
            "work",
            HostGoalOperationStatus::Running,
            Some("job-original"),
            Some("authoritative job"),
            NOW + 1,
        )
        .unwrap();
    assert!(managed_handoff(&db, &claim, &a, "handoff-a", &[]).is_err());
    let handed = managed_handoff(&db, &claim, &a, "handoff-a", &[running.clone()]).unwrap();
    let b = managed_context(&db, "target-b");
    assert!(db
        .claim_host_goal_node_for_plan_in_context(
            OWNER,
            &goal.goal_id,
            handed.goal.revision,
            "research",
            false,
            goal.plan_version,
            handed.goal.current_attempt_id.as_deref(),
            "claim-b",
            NOW + 30,
            Some(&b)
        )
        .is_err());
    assert_eq!(
        db.find_host_goal_operation_by_job(OWNER, &goal.goal_id, "job-original")
            .unwrap()
            .unwrap()
            .operation_id,
        "work"
    );
    db.finish_host_goal_operation(
        OWNER,
        &goal.goal_id,
        "work",
        HostGoalOperationStatus::Succeeded,
        Some("job-original"),
        Some("original job completed"),
        NOW + 30,
    )
    .unwrap();
    let next = managed_claim(&db, &handed.goal, &b, "claim-b");
    assert!(next.goal.fence > claim.goal.fence);
    assert_eq!(
        db.list_host_goal_operations(OWNER, &goal.goal_id, 0, 100, false)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn host_goal_tests_draining_blocks_dispatch_claim_and_control_but_allows_acceptance() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let a = HostGoalInvocationContext {
        context_epoch: "draining-a".into(),
        identity_source: "explicit_context_only".into(),
    };
    db.observe_host_context(
        OWNER,
        &a.context_epoch,
        "initial",
        0,
        false,
        Some((3000, 4000)),
    )
    .unwrap();
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    db.bind_host_goal_window_in_context(
        OWNER,
        "source-window",
        &super::HostGoalWindowBinding {
            goal_id: goal.goal_id.clone(),
            project: goal.project.clone(),
            project_root: goal.project_root.clone(),
            context_epoch: a.context_epoch.clone(),
        },
        Some(&a),
    )
    .unwrap();
    db.observe_host_context(OWNER, &a.context_epoch, "limit", 4000, false, None)
        .unwrap();
    assert_eq!(
        db.begin_host_goal_operation_with_manifest_in_context(
            OWNER,
            &claim.attempt.binding,
            "new-work",
            "write_file",
            "hash",
            &[],
            NOW,
            Some(&a),
            false
        )
        .unwrap_err()
        .code(),
        "goal_context_draining"
    );
    assert_eq!(
        db.select_current_host_goal_in_context(OWNER, &goal.goal_id, NOW, Some(&a))
            .unwrap_err()
            .code(),
        "goal_context_draining"
    );
    assert_eq!(
        db.claim_host_goal_node_for_plan_in_context(
            OWNER,
            &goal.goal_id,
            claim.goal.revision,
            "research",
            false,
            goal.plan_version,
            Some(&claim.attempt.binding.attempt_id),
            "claim-again",
            NOW,
            Some(&a)
        )
        .unwrap_err()
        .code(),
        "goal_context_draining"
    );
    prepare_review(&db, &claim.attempt.binding, Some(&a.context_epoch));
    let done = db
        .check_host_goal_node_with_request_in_context(
            OWNER,
            &claim.attempt.binding,
            claim.goal.revision,
            &evidence(true),
            &evidence(true),
            "accept",
            "accept",
            NOW + 50,
            Some(&a),
        )
        .unwrap();
    assert_eq!(done.goal.phase, HostGoalPhase::Completed);
    assert_eq!(
        db.get_host_goal_attempt(OWNER, &goal.goal_id, &claim.attempt.binding.attempt_id)
            .unwrap()
            .unwrap()
            .context_epoch,
        Some(a.context_epoch)
    );
}

#[test]
fn host_goal_tests_twenty_synthetic_cold_recoveries_preserve_goal_and_do_not_repeat_work() {
    let (dir, mut db) = database();
    let mut goal = confirmed(&db);
    let goal_id = goal.goal_id.clone();
    for index in 0..20 {
        let epoch = format!("synthetic-test-driver-{index}");
        let context = managed_context(&db, &epoch);
        let claim = managed_claim(&db, &goal, &context, &format!("claim-{index}"));
        let operation_id = format!("work-{index}");
        db.begin_host_goal_operation_with_manifest_in_context(
            OWNER,
            &claim.attempt.binding,
            &operation_id,
            "test_driver_write",
            "hash",
            &[],
            NOW,
            Some(&context),
            false,
        )
        .unwrap();
        db.finish_host_goal_operation(
            OWNER,
            &goal_id,
            &operation_id,
            HostGoalOperationStatus::Succeeded,
            None,
            Some("synthetic complete"),
            NOW,
        )
        .unwrap();
        goal = managed_handoff(&db, &claim, &context, &format!("handoff-{index}"), &[])
            .unwrap()
            .goal;
        assert_eq!(goal.goal_id, goal_id);
        assert!(serde_json::to_vec(&goal).unwrap().len() < 4096);
        drop(db);
        db = Database::open(&dir.path().join("state.db")).unwrap();
        assert_eq!(
            db.read_host_context_state(OWNER, &epoch)
                .unwrap()
                .unwrap()
                .state,
            "retired"
        );
    }
    let operations = db
        .list_host_goal_operations(OWNER, &goal_id, 0, 100, false)
        .unwrap();
    assert_eq!(operations.len(), 20);
    assert!(operations
        .iter()
        .all(|op| op.status == HostGoalOperationStatus::Succeeded));
    assert_eq!(
        db.list_host_goal_attempts(OWNER, &goal_id, 0, 100)
            .unwrap()
            .len(),
        20
    );
}

#[test]
fn host_goal_tests_two_new_context_claims_have_one_winner() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let a = managed_context(&db, "source-a");
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    let handed = managed_handoff(&db, &claim, &a, "handoff-a", &[]).unwrap();
    let b = managed_context(&db, "target-b");
    let c = managed_context(&db, "target-c");
    let db = std::sync::Arc::new(db);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let handles = [b, c]
        .into_iter()
        .enumerate()
        .map(|(i, ctx)| {
            let db = db.clone();
            let barrier = barrier.clone();
            let goal = handed.goal.clone();
            std::thread::spawn(move || {
                barrier.wait();
                db.claim_host_goal_node_for_plan_in_context(
                    OWNER,
                    &goal.goal_id,
                    goal.revision,
                    "research",
                    false,
                    goal.plan_version,
                    goal.current_attempt_id.as_deref(),
                    &format!("compete-{i}"),
                    NOW,
                    Some(&ctx),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        db.list_host_goal_attempts(OWNER, &goal.goal_id, 0, 100)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn host_goal_tests_legacy_checkpoint_and_attempt_provenance_stays_unknown() {
    let (_dir, db) = database();
    let (goal, binding) = claimed(&db);
    assert_eq!(
        db.get_host_goal_attempt(OWNER, &goal.goal_id, &binding.attempt_id)
            .unwrap()
            .unwrap()
            .context_epoch,
        None
    );
    let checkpoint = db
        .latest_host_goal_checkpoint(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.source_context_epoch, None);
    assert_eq!(checkpoint.source_fence, None);
}

#[test]
fn host_goal_tests_dispatch_and_retirement_serialize_across_database_connections() {
    let (dir, db) = database();
    let goal = confirmed(&db);
    let a = managed_context(&db, "source-a");
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    let other = Database::open(&dir.path().join("state.db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let dispatch_context = a.clone();
    let binding = claim.attempt.binding.clone();
    let dispatch_barrier = barrier.clone();
    let thread = std::thread::spawn(move || {
        dispatch_barrier.wait();
        other.begin_host_goal_operation_with_manifest_in_context(
            OWNER,
            &binding,
            "racing-work",
            "write_file",
            "hash",
            &[],
            NOW,
            Some(&dispatch_context),
            false,
        )
    });
    barrier.wait();
    let handoff = managed_handoff(&db, &claim, &a, "handoff-a", &[]);
    let dispatch = thread.join().unwrap();
    assert_ne!(handoff.is_ok(), dispatch.is_ok());
    let state = db
        .read_host_context_state(OWNER, "source-a")
        .unwrap()
        .unwrap();
    if handoff.is_ok() {
        assert_eq!(state.state, "retired");
        assert_eq!(
            db.count_unresolved_host_goal_operations(OWNER, &goal.goal_id)
                .unwrap(),
            0
        );
        assert_eq!(dispatch.unwrap_err().code(), "goal_chat_retired");
    } else {
        assert_eq!(state.state, "active");
        assert_eq!(
            db.count_unresolved_host_goal_operations(OWNER, &goal.goal_id)
                .unwrap(),
            1
        );
        assert_eq!(
            handoff.unwrap_err().code(),
            "goal_handoff_reconciliation_required"
        );
    }
}

#[test]
fn host_goal_tests_latest_business_evidence_is_bounded_and_not_first_page_only() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let a = managed_context(&db, "source-a");
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    for index in 0..102 {
        let id = if index == 101 {
            "check_internal".to_owned()
        } else {
            format!("work-{index}")
        };
        db.begin_host_goal_operation_with_manifest_in_context(
            OWNER,
            &claim.attempt.binding,
            &id,
            "test_driver_write",
            "hash",
            &[],
            NOW,
            Some(&a),
            false,
        )
        .unwrap();
        db.finish_host_goal_operation(
            OWNER,
            &goal.goal_id,
            &id,
            HostGoalOperationStatus::Succeeded,
            None,
            Some("complete"),
            NOW,
        )
        .unwrap();
    }
    let latest = db
        .latest_successful_host_goal_business_operation(
            OWNER,
            &goal.goal_id,
            &claim.attempt.binding.attempt_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(latest.operation_id, "work-100");
    assert_eq!(
        db.count_unresolved_host_goal_operations(OWNER, &goal.goal_id)
            .unwrap(),
        0
    );
    assert!(db
        .latest_successful_host_goal_business_operation(
            OTHER,
            &goal.goal_id,
            &claim.attempt.binding.attempt_id
        )
        .is_err());
}

#[test]
fn host_goal_tests_draining_closeout_cannot_mutate_another_owned_goal() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let other = db
        .create_host_goal_at(
            OWNER,
            "project",
            "/workspace/project",
            "Another goal",
            "other-goal",
            NOW,
        )
        .unwrap()
        .goal;
    let a = HostGoalInvocationContext {
        context_epoch: "draining-source".into(),
        identity_source: "explicit_context_only".into(),
    };
    db.observe_host_context(
        OWNER,
        &a.context_epoch,
        "initial",
        0,
        false,
        Some((3000, 4000)),
    )
    .unwrap();
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    db.bind_host_goal_window_in_context(
        OWNER,
        "source-window",
        &super::HostGoalWindowBinding {
            goal_id: goal.goal_id.clone(),
            project: goal.project.clone(),
            project_root: goal.project_root.clone(),
            context_epoch: a.context_epoch.clone(),
        },
        Some(&a),
    )
    .unwrap();
    db.observe_host_context(OWNER, &a.context_epoch, "drain", 4000, false, None)
        .unwrap();
    for (index, action) in [
        HostGoalCheckpointAction::Progress,
        HostGoalCheckpointAction::Pause,
        HostGoalCheckpointAction::Cancel,
        HostGoalCheckpointAction::Block,
        HostGoalCheckpointAction::Handoff,
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("escape-{index}");
        let result = db.checkpoint_host_goal_with_request_in_context(
            OWNER,
            &other.goal_id,
            other.revision,
            None,
            &other.state,
            action,
            "Cross-goal closeout attempt",
            &key,
            &key,
            &[],
            NOW,
            Some(&a),
            &[],
        );
        assert_eq!(result.unwrap_err().code(), "goal_context_draining");
    }
    assert_eq!(
        db.get_host_goal(OWNER, &other.goal_id)
            .unwrap()
            .unwrap()
            .revision,
        other.revision
    );
    managed_handoff(&db, &claim, &a, "allowed-source-handoff", &[]).unwrap();
}

#[test]
fn host_goal_tests_handoff_projection_is_coherent_during_claims_on_another_connection() {
    let (dir, db) = database();
    let goal = confirmed(&db);
    let a = managed_context(&db, "source-a");
    let claim = managed_claim(&db, &goal, &a, "claim-a");
    let handed = managed_handoff(&db, &claim, &a, "handoff-a", &[]).unwrap();
    let expected_checkpoint = db
        .latest_host_goal_checkpoint(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    let writer = Database::open(&dir.path().join("state.db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = barrier.clone();
    let thread = std::thread::spawn(move || {
        writer_barrier.wait();
        let mut current = handed.goal;
        for index in 0..30 {
            let context = managed_context(&writer, &format!("reader-race-{index}"));
            current = managed_claim(
                &writer,
                &current,
                &context,
                &format!("reader-race-claim-{index}"),
            )
            .goal;
        }
    });
    barrier.wait();
    for _ in 0..100 {
        let snapshot = db
            .read_host_goal_handoff_snapshot(OWNER, &goal.goal_id, "source-a")
            .unwrap();
        let attempt = snapshot.attempt.unwrap();
        assert_eq!(
            snapshot.goal.current_attempt_id.as_deref(),
            Some(attempt.binding.attempt_id.as_str())
        );
        assert_eq!(snapshot.goal.fence, attempt.binding.fence);
        assert_eq!(snapshot.goal.plan_version, attempt.binding.plan_version);
        assert_eq!(snapshot.source.unwrap().state, "retired");
        assert_eq!(snapshot.checkpoint.unwrap(), expected_checkpoint);
        assert_eq!(snapshot.unresolved_operation_count, 0);
    }
    thread.join().unwrap();
    assert!(db
        .read_host_goal_handoff_snapshot(OTHER, &goal.goal_id, "source-a")
        .is_err());
}

#[test]
fn host_goal_tests_known_host_epoch_cannot_be_impersonated_by_explicit_identity() {
    let (_dir, db) = database();
    let goal = confirmed(&db);
    let host = HostGoalInvocationContext {
        context_epoch: "chat_known_host_hash".into(),
        identity_source: "window_bound".into(),
    };
    db.ensure_host_context_identity(OWNER, &host).unwrap();
    let claim = managed_claim(&db, &goal, &host, "host-claim");
    let forged = HostGoalInvocationContext {
        context_epoch: host.context_epoch.clone(),
        identity_source: "explicit_context_only".into(),
    };
    db.ensure_host_context_identity(OWNER, &forged).unwrap();
    assert_eq!(
        db.gate_host_context(OWNER, Some(&forged), true)
            .unwrap_err()
            .code(),
        "goal_context_identity_mismatch"
    );
    assert_eq!(
        db.begin_host_goal_operation_with_manifest_in_context(
            OWNER,
            &claim.attempt.binding,
            "forged-operation",
            "write_file",
            "hash",
            &[],
            NOW,
            Some(&forged),
            false
        )
        .unwrap_err()
        .code(),
        "goal_context_identity_mismatch"
    );
    assert_eq!(
        db.read_host_context_state(OWNER, &host.context_epoch)
            .unwrap()
            .unwrap()
            .identity_source
            .as_deref(),
        Some("window_bound")
    );
    let observed = db
        .read_host_goal_handoff_snapshot(OWNER, &goal.goal_id, &host.context_epoch)
        .unwrap();
    assert_eq!(
        observed.attempt.unwrap().identity_source.as_deref(),
        Some("window_bound")
    );
    assert_eq!(
        observed.source.unwrap().identity_source.as_deref(),
        Some("window_bound")
    );
    db.validate_host_goal_binding_in_context(OWNER, &claim.attempt.binding, Some(&host))
        .unwrap();
    let unsupported = HostGoalInvocationContext {
        context_epoch: "invalid-source".into(),
        identity_source: "model_claimed_host".into(),
    };
    assert_eq!(
        db.ensure_host_context_identity(OWNER, &unsupported)
            .unwrap_err()
            .code(),
        "goal_context_identity_mismatch"
    );
    assert!(db
        .read_host_context_state(OWNER, &unsupported.context_epoch)
        .unwrap()
        .is_none());
}
