use super::Database;
use std::sync::{Arc, Barrier};
use webcodex_core::goal::{
    GoalArtifactSnapshot, GoalCheckEvidence, GoalCriterion, GoalExecutionTarget, GoalLimits,
    GoalRound, GoalSpec, GoalState, GoalVerdict,
};

const OWNER: &str = "owner-fingerprint-a";
const OTHER: &str = "owner-fingerprint-b";
const NOW: i64 = 1_000_000;

fn spec() -> GoalSpec {
    GoalSpec {
        project: "test-project".into(),
        provider_id: "executor".into(),
        objective: "Produce the accepted artifact".into(),
        criteria: vec![GoalCriterion::FileContains {
            id: "content".into(),
            description: "Artifact contains the result".into(),
            path: "result.txt".into(),
            text: "accepted".into(),
        }],
        artifact_paths: vec!["result.txt".into()],
        reviewer_provider_id: None,
        limits: GoalLimits::default(),
        config: Default::default(),
    }
}

fn target() -> GoalExecutionTarget {
    GoalExecutionTarget {
        project_root: "/work/test".into(),
        runner_instance_id: "runner-instance".into(),
        provider_instance_id: "provider-instance".into(),
        reviewer_provider_instance_id: None,
    }
}

fn database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    (dir, db)
}

#[test]
fn goal_tests_restart_preserves_private_worker_and_acceptance() {
    let (dir, db) = database();
    let mut goal = db
        .create_goal_at(OWNER, spec(), "create", NOW)
        .unwrap()
        .goal;
    goal.worker_epoch = Some("worker-epoch".into());
    goal.target = Some(target());
    goal.state = GoalState::Running;
    goal.rounds.push(GoalRound::new(1, NOW));
    goal.rounds[0].execution_run_id = Some("run-one".into());
    let saved = db.save_goal(OWNER, &goal, goal.revision).unwrap();
    let public = serde_json::to_string(&saved).unwrap();
    assert!(!public.contains(OWNER));
    assert!(!public.contains("worker-epoch"));
    assert!(!public.contains("owner_key"));
    assert!(!public.contains("worker_epoch"));
    drop(db);
    let reopened = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.get_goal(OWNER, &saved.goal_id).unwrap(),
        Some(saved)
    );
}

#[test]
fn goal_tests_owner_isolation_and_idempotence() {
    let (_dir, db) = database();
    let first = db.create_goal_at(OWNER, spec(), "create", NOW).unwrap();
    assert!(first.created);
    assert_eq!(
        db.lookup_goal_replay(OWNER, &spec(), "create").unwrap(),
        Some(first.goal.clone())
    );
    assert!(db
        .lookup_goal_replay(OTHER, &spec(), "create")
        .unwrap()
        .is_none());
    let retry = db.create_goal_at(OWNER, spec(), "create", NOW + 1).unwrap();
    assert!(!retry.created);
    assert_eq!(first.goal, retry.goal);
    assert!(db.get_goal(OTHER, &first.goal.goal_id).unwrap().is_none());
    assert!(db.list_goals(OTHER, None, 20).unwrap().is_empty());
    let other = db.create_goal_at(OTHER, spec(), "create", NOW).unwrap();
    assert_ne!(first.goal.goal_id, other.goal.goal_id);
    let mut changed = spec();
    changed.objective = "Different work".into();
    assert_eq!(
        db.lookup_goal_replay(OWNER, &changed, "create")
            .unwrap_err()
            .code(),
        "goal_idempotency_conflict"
    );
    assert_eq!(
        db.create_goal_at(OWNER, changed, "create", NOW)
            .unwrap_err()
            .code(),
        "goal_idempotency_conflict"
    );
    assert!(db
        .list_goals(OWNER, Some("different"), 20)
        .unwrap()
        .is_empty());
    assert_eq!(
        db.list_goals(OWNER, Some("test-project"), 20)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn goal_tests_cas_allows_only_one_round_dispatch_identity() {
    let (_dir, db) = database();
    let goal = db
        .create_goal_at(OWNER, spec(), "create", NOW)
        .unwrap()
        .goal;
    let db = Arc::new(db);
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|index| {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            let mut goal = goal.clone();
            std::thread::spawn(move || {
                goal.state = GoalState::Running;
                goal.rounds.push(GoalRound::new(1, NOW));
                goal.rounds[0].execution_run_id = Some(format!("run-{index}"));
                barrier.wait();
                db.save_goal(OWNER, &goal, 0)
            })
        })
        .collect();
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome
                .as_ref()
                .is_err_and(|error| error.code() == "goal_conflict"))
            .count(),
        1
    );
    assert_eq!(
        db.get_goal(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .rounds
            .len(),
        1
    );
}

#[test]
fn goal_tests_criteria_target_and_deadline_are_immutable() {
    let (_dir, db) = database();
    let goal = db
        .create_goal_at(OWNER, spec(), "create", NOW)
        .unwrap()
        .goal;
    let mut changed = goal.clone();
    changed.spec.criteria.clear();
    assert!(db.save_goal(OWNER, &changed, goal.revision).is_err());
    changed = goal.clone();
    changed.deadline_at += 1;
    assert!(db.save_goal(OWNER, &changed, goal.revision).is_err());
    changed = goal;
    changed.target = Some(target());
    let saved = db.save_goal(OWNER, &changed, changed.revision).unwrap();
    changed = saved.clone();
    changed.target.as_mut().unwrap().project_root = "/other/project".into();
    assert!(db.save_goal(OWNER, &changed, saved.revision).is_err());
    changed.target = None;
    assert!(db.save_goal(OWNER, &changed, saved.revision).is_err());
}

fn completed_round() -> GoalRound {
    let mut round = GoalRound::new(1, NOW);
    round.execution_run_id = Some("run-one".into());
    round.execution_state = Some(webcodex_core::coding_agent::CodingAgentRunState::Completed);
    round.checks.push(GoalCheckEvidence {
        criterion_id: "content".into(),
        passed: true,
        evidence: "result.txt contains the required text at final SHA-256".into(),
        error: None,
    });
    round.manifest_before_checks.push(GoalArtifactSnapshot {
        path: "result.txt".into(),
        sha256: Some("a".repeat(64)),
    });
    round.manifest_final = round.manifest_before_checks.clone();
    round.verdict = Some(GoalVerdict::Passed);
    round.finished_at = Some(NOW + 10);
    round
}

#[test]
fn goal_tests_completion_rejects_missing_failed_or_stale_evidence() {
    let (_dir, db) = database();
    let mut goal = db
        .create_goal_at(OWNER, spec(), "create", NOW)
        .unwrap()
        .goal;
    goal.state = GoalState::Completed;
    assert!(db.save_goal(OWNER, &goal, 0).is_err());
    goal.target = Some(target());
    goal.rounds.push(completed_round());
    let mut missing = goal.clone();
    missing.rounds[0].checks.clear();
    assert!(db.save_goal(OWNER, &missing, 0).is_err());
    let mut failed = goal.clone();
    failed.rounds[0].checks[0].passed = false;
    assert!(db.save_goal(OWNER, &failed, 0).is_err());
    let mut empty = goal.clone();
    empty.rounds[0].checks[0].evidence.clear();
    assert!(db.save_goal(OWNER, &empty, 0).is_err());
    let mut error = goal.clone();
    error.rounds[0].checks[0].error = Some("checker unavailable".into());
    assert!(db.save_goal(OWNER, &error, 0).is_err());
    let mut stale = goal.clone();
    stale.rounds[0].manifest_final[0].sha256 = Some("b".repeat(64));
    assert!(db.save_goal(OWNER, &stale, 0).is_err());
    let mut absent = goal.clone();
    absent.rounds[0].manifest_final[0].sha256 = None;
    absent.rounds[0].manifest_before_checks = absent.rounds[0].manifest_final.clone();
    assert!(db.save_goal(OWNER, &absent, 0).is_err());
    let saved = db.save_goal(OWNER, &goal, 0).unwrap();
    assert_eq!(saved.state, GoalState::Completed);
    let mut revived = saved.clone();
    revived.state = GoalState::Running;
    assert!(db.save_goal(OWNER, &revived, saved.revision).is_err());
}

#[test]
fn goal_tests_cancel_cannot_revive_and_old_round_evidence_is_immutable() {
    let (_dir, db) = database();
    let mut goal = db
        .create_goal_at(OWNER, spec(), "create", NOW)
        .unwrap()
        .goal;
    goal.state = GoalState::Checking;
    goal.rounds.push(completed_round());
    goal.rounds[0].verdict = Some(GoalVerdict::NeedsWork);
    let mut goal = db.save_goal(OWNER, &goal, 0).unwrap();
    goal.state = GoalState::Running;
    goal.rounds.push(GoalRound::new(2, NOW + 20));
    let mut goal = db.save_goal(OWNER, &goal, goal.revision).unwrap();
    let mut tampered = goal.clone();
    tampered.rounds[0].checks[0].evidence = "replaced evidence".into();
    assert!(db.save_goal(OWNER, &tampered, goal.revision).is_err());
    goal.cancel_requested = true;
    goal.state = GoalState::Cancelled;
    let saved = db.save_goal(OWNER, &goal, goal.revision).unwrap();
    let mut revived = saved.clone();
    revived.cancel_requested = false;
    revived.state = GoalState::Queued;
    assert!(db.save_goal(OWNER, &revived, saved.revision).is_err());
}

#[test]
fn goal_tests_rejects_unbounded_or_unconfined_specs() {
    for path in [
        "../secret",
        "sub/../../secret",
        "/absolute",
        "C:\\secret",
        "\\\\host\\share",
        "dir//file",
        "dir/./file",
    ] {
        let mut value = spec();
        value.artifact_paths = vec![path.into()];
        assert!(value.validate().is_err(), "accepted {path}");
    }
    let mut value = spec();
    value.criteria.push(value.criteria[0].clone());
    assert!(value.validate().is_err());
    value = spec();
    value.criteria = vec![GoalCriterion::Review {
        id: "review".into(),
        description: "Check quality".into(),
    }];
    assert!(value.validate().is_err());
    value.reviewer_provider_id = Some("reviewer".into());
    assert!(value.validate().is_ok());
    value.limits.max_rounds = 51;
    assert!(value.validate().is_err());
}

#[test]
fn goal_tests_opening_database_does_not_claim_or_change_active_work() {
    let (dir, db) = database();
    let goal = db
        .create_goal_at(OWNER, spec(), "create", NOW)
        .unwrap()
        .goal;
    let claimed = db
        .claim_goal_worker(OWNER, &goal.goal_id, 0, "first-worker")
        .unwrap();
    assert!(db
        .claim_goal_worker(OWNER, &goal.goal_id, claimed.revision, "second-worker")
        .is_err());
    let second = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(
        second.get_goal(OWNER, &goal.goal_id).unwrap(),
        Some(claimed)
    );
}

#[test]
fn goal_tests_project_claim_is_exclusive_and_cancellation_releases_it() {
    let (_dir, db) = database();
    let first = db.create_goal_at(OWNER, spec(), "first", NOW).unwrap().goal;
    let second = db
        .create_goal_at(OTHER, spec(), "second", NOW)
        .unwrap()
        .goal;
    let db = Arc::new(db);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [(OWNER, first), (OTHER, second)]
        .into_iter()
        .map(|(owner, mut goal)| {
            let db = db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                goal.worker_epoch = Some(format!("worker-{owner}"));
                barrier.wait();
                (
                    owner,
                    goal.clone(),
                    db.save_goal(owner, &goal, goal.revision),
                )
            })
        })
        .collect();
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|(_, _, result)| result.is_ok())
            .count(),
        1
    );
    let (winner_owner, _, winner) = outcomes
        .iter()
        .find(|(_, _, result)| result.is_ok())
        .unwrap();
    let (loser_owner, loser, rejected) = outcomes
        .iter()
        .find(|(_, _, result)| result.is_err())
        .unwrap();
    assert_eq!(rejected.as_ref().unwrap_err().code(), "goal_project_busy");
    assert!(!rejected
        .as_ref()
        .unwrap_err()
        .message()
        .contains(winner_owner));
    let mut cancelled = winner.as_ref().unwrap().clone();
    cancelled.state = GoalState::Cancelled;
    cancelled.cancel_requested = true;
    db.save_goal(winner_owner, &cancelled, cancelled.revision)
        .unwrap();
    assert!(db.save_goal(loser_owner, loser, loser.revision).is_ok());
}

#[test]
fn goal_tests_paused_project_remains_owned_but_other_projects_can_execute() {
    let (_dir, db) = database();
    let first = db.create_goal_at(OWNER, spec(), "first", NOW).unwrap().goal;
    let mut first = db
        .claim_goal_worker(OWNER, &first.goal_id, first.revision, "worker-one")
        .unwrap();
    first.state = GoalState::Paused;
    db.save_goal(OWNER, &first, first.revision).unwrap();
    let second = db
        .create_goal_at(OWNER, spec(), "second", NOW)
        .unwrap()
        .goal;
    assert_eq!(
        db.claim_goal_worker(OWNER, &second.goal_id, second.revision, "worker-two")
            .unwrap_err()
            .code(),
        "goal_project_busy"
    );
    let mut other_project = spec();
    other_project.project = "different-project".into();
    let third = db
        .create_goal_at(OWNER, other_project, "third", NOW)
        .unwrap()
        .goal;
    assert!(db
        .claim_goal_worker(OWNER, &third.goal_id, third.revision, "worker-three")
        .is_ok());
}

#[test]
fn goal_tests_completed_goal_releases_project_for_next_goal() {
    let (_dir, db) = database();
    let first = db.create_goal_at(OWNER, spec(), "first", NOW).unwrap().goal;
    let mut first = db
        .claim_goal_worker(OWNER, &first.goal_id, first.revision, "worker-one")
        .unwrap();
    first.target = Some(target());
    first.rounds.push(completed_round());
    first.state = GoalState::Completed;
    db.save_goal(OWNER, &first, first.revision).unwrap();
    let second = db
        .create_goal_at(OWNER, spec(), "second", NOW)
        .unwrap()
        .goal;
    assert!(db
        .claim_goal_worker(OWNER, &second.goal_id, second.revision, "worker-two")
        .is_ok());
}
