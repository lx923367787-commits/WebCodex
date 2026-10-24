use super::*;
const NOW: i64 = 1_000_000;
const PRINCIPAL: &str = "oauth-principal-test";
const SOURCE_WINDOW: &str = "source-window-test";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fixture() -> (
    tempfile::TempDir,
    Database,
    VerifiedAgentRun,
    HostGoalHandoff,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    db.init_agent_isolation_tables().unwrap();
    let grant = db.create_isolated_agent("owner", "project", None).unwrap();
    let source = db.authenticate_agent_run(&grant.token).unwrap();
    let context = db.ensure_agent_host_context(&source, None).unwrap();
    let owner = source.host_goal_owner_key();
    db.register_agent_host_window(PRINCIPAL, SOURCE_WINDOW, &source)
        .unwrap();
    let goal = db
        .create_host_goal_at_in_context(
            &owner,
            "project",
            "/private",
            "Authorized task only",
            "create",
            NOW,
            Some(&context),
        )
        .unwrap()
        .goal;
    db.bind_host_goal_window_in_context(
        &owner,
        source.run_id(),
        &crate::HostGoalWindowBinding {
            goal_id: goal.goal_id.clone(),
            project: goal.project.clone(),
            project_root: goal.project_root.clone(),
            context_epoch: source.run_id().into(),
        },
        Some(&context),
    )
    .unwrap();
    db.checkpoint_host_goal_with_request_in_context(
        &owner,
        &goal.goal_id,
        goal.revision,
        None,
        &goal.state,
        HostGoalCheckpointAction::Handoff,
        "continue same task",
        "handoff",
        "handoff-request",
        &[],
        NOW,
        Some(&context),
        &[],
    )
    .unwrap();
    let h = db
        .read_host_goal_handoff(&owner, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(h.agent_id.as_deref(), Some(source.agent_id()));
    assert_eq!(h.source_agent_run_id.as_deref(), Some(source.run_id()));
    assert_eq!(h.source_context_epoch, source.run_id());
    (dir, db, source, h)
}

fn begin(
    db: &Database,
    source: &VerifiedAgentRun,
    h: &HostGoalHandoff,
    now: i64,
) -> HostGoalHandoff {
    db.begin_host_goal_handoff_attempt(
        &source.host_goal_owner_key(),
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref(),
        now,
        2,
    )
    .unwrap()
}
fn bind_prepare(db: &Database, h: &HostGoalHandoff, window: &str, now: i64) {
    let attempt = h.attempt_id.as_deref().unwrap();
    db.bind_handoff_host_window(PRINCIPAL, window, &h.goal_id, &h.handoff_id, attempt, now)
        .unwrap();
    db.with_pending_agent_host_handoff(
        PRINCIPAL,
        window,
        &h.goal_id,
        &h.handoff_id,
        attempt,
        now,
        |source, current| {
            assert_eq!(current.checkpoint_sequence, h.checkpoint_sequence);
            assert_eq!(source.run_id(), h.source_agent_run_id.as_deref().unwrap());
            Ok(())
        },
    )
    .unwrap();
    db.prepare_agent_handoff_for_host_window(
        PRINCIPAL,
        window,
        &h.goal_id,
        &h.handoff_id,
        attempt,
        h.checkpoint_sequence,
        DIGEST,
        now,
    )
    .unwrap();
}
fn commit(
    db: &Database,
    h: &HostGoalHandoff,
    window: &str,
    now: i64,
) -> Result<HostGoalHandoff, GoalStoreError> {
    db.commit_agent_handoff_for_host_window(
        PRINCIPAL,
        window,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        h.checkpoint_sequence,
        DIGEST,
        now,
    )
}

#[test]
fn native_host_handoff_explicit_registration_prepares_and_switches_same_agent_after_restart() {
    let (dir, db, source, h) = fixture();
    let owner = source.host_goal_owner_key();
    assert!(db.validate_agent_run(&source).is_ok());
    assert_eq!(
        db.read_host_context_state(&owner, source.run_id())
            .unwrap()
            .unwrap()
            .state,
        "draining"
    );
    db.with_source_agent_handoff(&source, &h.goal_id, h.checkpoint_sequence, || Ok(()))
        .unwrap();
    assert!(db
        .with_source_agent_handoff(&source, &h.goal_id, h.checkpoint_sequence + 1, || Ok(()))
        .is_err());
    let h = begin(&db, &source, &h, NOW);
    let pending = h.pending_agent_run_id.clone().unwrap();
    assert!(db
        .resolve_agent_host_window(PRINCIPAL, "unregistered", NOW)
        .unwrap()
        .is_none());
    assert!(db
        .prepare_agent_handoff_for_host_window(
            PRINCIPAL,
            "unregistered",
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            h.checkpoint_sequence,
            DIGEST,
            NOW
        )
        .is_err());
    assert_eq!(
        db.activate_agent_run(
            source.owner_user(),
            source.project(),
            source.agent_id(),
            &pending
        )
        .unwrap_err()
        .code(),
        "agent_handoff_control_required"
    );
    bind_prepare(&db, &h, "candidate", NOW + 1);
    assert!(matches!(
        db.resolve_agent_host_window(PRINCIPAL, "candidate", NOW + 1)
            .unwrap(),
        Some(AgentHostWindowBinding::Pending { .. })
    ));
    assert!(db.validate_agent_run(&source).is_ok());
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let complete = commit(&db, &h, "candidate", NOW + 2).unwrap();
    assert_eq!(complete.status, "committed");
    assert_eq!(complete.goal_id, h.goal_id);
    let Some(AgentHostWindowBinding::Active(run)) = db
        .resolve_agent_host_window(PRINCIPAL, "candidate", NOW + 2)
        .unwrap()
    else {
        panic!("candidate was not activated")
    };
    assert_eq!(run.agent_id(), source.agent_id());
    assert_ne!(run.run_id(), source.run_id());
    assert_eq!(run.run_id(), pending);
    assert_eq!(
        db.agent_run_task_goal(&run).unwrap().as_deref(),
        Some(h.goal_id.as_str())
    );
    assert!(db.validate_agent_run(&source).is_err());
    assert!(db
        .resolve_agent_host_window(PRINCIPAL, SOURCE_WINDOW, NOW + 2)
        .is_err());
    assert_eq!(
        db.read_host_context_state(&owner, source.run_id())
            .unwrap()
            .unwrap()
            .state,
        "retired"
    );
    assert_eq!(commit(&db, &h, "candidate", NOW + 10000).unwrap(), complete);
    assert_eq!(
        db.fail_host_goal_handoff_attempt(
            &owner,
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            "lost response",
            NOW + 10000
        )
        .unwrap(),
        complete
    );
    assert_eq!(
        db.list_agent_handoffs("owner", "project", Some(source.agent_id()), 10)
            .unwrap()
            .len(),
        1
    );
    let meta = db
        .read_host_goal_handoff_control_metadata(&owner, &h.goal_id)
        .unwrap();
    assert!(meta.get("objective").is_none());
    assert!(meta["checkpoint_revision"].is_number());
}

#[test]
fn native_host_handoff_retry_fences_old_pending_and_fourth_success_is_the_only_new_active_run() {
    let (_dir, db, source, mut h) = fixture();
    let owner = source.host_goal_owner_key();
    let mut prior = Vec::new();
    for n in 1..=4 {
        h = begin(&db, &source, &h, NOW + n * 10);
        assert_eq!(h.attempt_count, n as u32);
        let window = format!("candidate-{n}");
        bind_prepare(&db, &h, &window, NOW + n * 10 + 1);
        let run = h.pending_agent_run_id.clone().unwrap();
        prior.push(run);
        if n < 4 {
            h = db
                .fail_host_goal_handoff_attempt(
                    &owner,
                    &h.goal_id,
                    &h.handoff_id,
                    h.attempt_id.as_deref().unwrap(),
                    "not ready",
                    NOW + n * 10 + 2,
                )
                .unwrap();
            assert!(db
                .resolve_agent_host_window(PRINCIPAL, &window, NOW + n * 10 + 2)
                .is_err());
            assert!(db.validate_agent_run(&source).is_ok());
        } else {
            assert_eq!(
                commit(&db, &h, &window, NOW + n * 10 + 2).unwrap().status,
                "committed"
            );
        }
    }
    let conn = db.conn_for_tests();
    for old in &prior[..3] {
        let state: String = conn
            .query_row(
                "SELECT state FROM wc_agent_runs WHERE run_id=?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "revoked");
    }
    let active: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wc_agent_runs WHERE agent_id=?1 AND state='active'",
            [source.agent_id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active, 1);
}

#[test]
fn native_host_handoff_four_timeouts_survive_restart_and_cannot_launch_fifth() {
    let (dir, mut db, source, mut h) = fixture();
    let owner = source.host_goal_owner_key();
    for n in 0..4 {
        h = begin(&db, &source, &h, NOW + n * 2000);
        bind_prepare(&db, &h, &format!("candidate-{n}"), NOW + n * 2000 + 1);
        drop(db);
        db = Database::open(&dir.path().join("state.db")).unwrap();
    }
    let failed = begin(&db, &source, &h, NOW + 8000);
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.attempt_count, 4);
    assert!(failed.pending_agent_run_id.is_none());
    assert_eq!(begin(&db, &source, &failed, NOW + 10000), failed);
    assert!(commit(&db, &h, "candidate-3", NOW + 8001).is_err());
    assert_eq!(
        db.read_host_context_state(&owner, source.run_id())
            .unwrap()
            .unwrap()
            .state,
        "stopped"
    );
    assert!(db
        .read_host_goal_handoff_snapshot(&owner, &h.goal_id, source.run_id())
        .unwrap()
        .checkpoint
        .is_some());
    let pending: u64 = db
        .conn_for_tests()
        .query_row(
            "SELECT COUNT(*) FROM wc_agent_runs WHERE agent_id=?1 AND state='pending'",
            [source.agent_id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
}

#[test]
fn native_host_handoff_mapping_is_exclusive_principal_bound_revocable_and_concurrent_safe() {
    let (_dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    let attempt = h.attempt_id.as_deref().unwrap();
    assert!(db
        .bind_handoff_host_window(
            "other-principal",
            "other-window",
            &h.goal_id,
            &h.handoff_id,
            attempt,
            NOW
        )
        .is_err());
    assert!(db
        .bind_handoff_host_window(
            PRINCIPAL,
            SOURCE_WINDOW,
            &h.goal_id,
            &h.handoff_id,
            attempt,
            NOW
        )
        .is_err());
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            db.bind_handoff_host_window(PRINCIPAL, "a", &h.goal_id, &h.handoff_id, attempt, NOW)
        });
        let b = scope.spawn(|| {
            db.bind_handoff_host_window(PRINCIPAL, "b", &h.goal_id, &h.handoff_id, attempt, NOW)
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let winner = if results[0].is_ok() { "a" } else { "b" };
    assert!(matches!(
        db.resolve_agent_host_window(PRINCIPAL, winner, NOW)
            .unwrap(),
        Some(AgentHostWindowBinding::Pending { .. })
    ));
    db.revoke_agent_host_principal(PRINCIPAL).unwrap();
    assert!(!db.agent_host_principal_authorized(PRINCIPAL).unwrap());
    assert!(db
        .resolve_agent_host_window(PRINCIPAL, winner, NOW)
        .unwrap()
        .is_none());
    assert!(commit(&db, &h, winner, NOW).is_err());
    assert!(db.validate_agent_run(&source).is_ok());
    assert_eq!(
        db.read_host_goal_handoff(&source.host_goal_owner_key(), &h.goal_id)
            .unwrap()
            .unwrap()
            .attempt_count,
        1
    );
}

#[test]
fn native_host_handoff_abort_and_expired_callback_revoke_pending_without_switching_source() {
    let (_dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    bind_prepare(&db, &h, "old", NOW + 1);
    assert!(commit(&db, &h, "old", NOW + 2000).is_err());
    let next = begin(&db, &source, &h, NOW + 2000);
    assert_ne!(next.pending_agent_run_id, h.pending_agent_run_id);
    assert!(db
        .bind_handoff_host_window(
            PRINCIPAL,
            "late",
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            NOW + 2001
        )
        .is_err());
    let failed = db
        .abort_host_goal_handoff_attempt(
            &source.host_goal_owner_key(),
            &next.goal_id,
            &next.handoff_id,
            next.attempt_id.as_deref(),
            "unsafe old execution",
            NOW + 2002,
        )
        .unwrap();
    assert_eq!(failed.status, "failed");
    assert!(failed.pending_agent_run_id.is_none());
    assert!(db.validate_agent_run(&source).is_ok());
}

#[test]
fn native_host_handoff_successor_can_handoff_again_without_changing_goal_agent_or_scope() {
    let (_dir, db, source, h) = fixture();
    let first = begin(&db, &source, &h, NOW);
    bind_prepare(&db, &first, "second-window", NOW + 1);
    commit(&db, &first, "second-window", NOW + 2).unwrap();
    let Some(AgentHostWindowBinding::Active(second)) = db
        .resolve_agent_host_window(PRINCIPAL, "second-window", NOW + 2)
        .unwrap()
    else {
        panic!("missing successor")
    };
    let context = db.ensure_agent_host_context(&second, None).unwrap();
    let owner = second.host_goal_owner_key();
    let goal = db
        .read_host_goal_for_agent_run(&second, &h.goal_id)
        .unwrap()
        .0;
    db.checkpoint_host_goal_with_request_in_context(
        &owner,
        &goal.goal_id,
        goal.revision,
        None,
        &goal.state,
        HostGoalCheckpointAction::Handoff,
        "next conversation",
        "handoff-second",
        "handoff-second",
        &[],
        NOW + 3,
        Some(&context),
        &[],
    )
    .unwrap();
    let next = db
        .read_host_goal_handoff(&owner, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_ne!(next.handoff_id, first.handoff_id);
    assert_eq!(next.source_agent_run_id.as_deref(), Some(second.run_id()));
    assert_eq!(next.attempt_count, 0);
    db.with_source_agent_handoff(&second, &goal.goal_id, next.checkpoint_sequence, || Ok(()))
        .unwrap();
    let next = begin(&db, &second, &next, NOW + 4);
    bind_prepare(&db, &next, "third-window", NOW + 5);
    let complete = commit(&db, &next, "third-window", NOW + 6).unwrap();
    let Some(AgentHostWindowBinding::Active(third)) = db
        .resolve_agent_host_window(PRINCIPAL, "third-window", NOW + 6)
        .unwrap()
    else {
        panic!("missing second successor")
    };
    assert_eq!(third.agent_id(), source.agent_id());
    assert_eq!(complete.goal_id, h.goal_id);
    assert_eq!(
        db.agent_run_task_goal(&third).unwrap().as_deref(),
        Some(h.goal_id.as_str())
    );
    assert!(db.validate_agent_run(&second).is_err());
}

#[test]
fn native_host_handoff_generic_management_cannot_replace_source_or_native_pending() {
    let (_dir, db, source, h) = fixture();
    let owner = source.host_goal_owner_key();
    assert_eq!(h.status, "waiting");
    assert_eq!(db.agent_run_task_goal(&source).unwrap(), None);
    assert_eq!(
        db.begin_agent_run_handoff(&source).unwrap_err().code(),
        "agent_handoff_control_required"
    );
    assert_eq!(
        db.activate_agent_run(
            source.owner_user(),
            source.project(),
            source.agent_id(),
            "not-issued"
        )
        .unwrap_err()
        .code(),
        "agent_handoff_control_required"
    );
    assert_eq!(
        db.isolated_agent_generation(source.owner_user(), source.project(), source.agent_id())
            .unwrap(),
        source.generation()
    );
    assert!(db.validate_agent_run(&source).is_ok());
    let h = begin(&db, &source, &h, NOW);
    let pending = h.pending_agent_run_id.as_deref().unwrap();
    assert_eq!(
        db.begin_agent_run_handoff(&source).unwrap_err().code(),
        "agent_handoff_control_required"
    );
    assert!(crate::agent_isolation::pending_agent_run_on_connection(
        &db.conn_for_tests(),
        &source,
        pending
    )
    .is_ok());
    assert_eq!(
        db.read_host_goal_handoff(&owner, &h.goal_id)
            .unwrap()
            .unwrap()
            .pending_agent_run_id,
        h.pending_agent_run_id
    );
    bind_prepare(&db, &h, "candidate", NOW + 1);
    assert_eq!(
        db.activate_agent_run(
            source.owner_user(),
            source.project(),
            source.agent_id(),
            pending
        )
        .unwrap_err()
        .code(),
        "agent_handoff_control_required"
    );
    assert!(db.validate_agent_run(&source).is_ok());
    assert_eq!(
        db.read_host_context_state(&owner, source.run_id())
            .unwrap()
            .unwrap()
            .state,
        "draining"
    );
    assert_eq!(
        commit(&db, &h, "candidate", NOW + 2).unwrap().status,
        "committed"
    );
    let (_dir, db, source, h) = fixture();
    db.abort_host_goal_handoff(
        &source.host_goal_owner_key(),
        &h.goal_id,
        &h.handoff_id,
        "explicit stop",
        NOW,
    )
    .unwrap();
    assert_eq!(
        db.begin_agent_run_handoff(&source).unwrap_err().code(),
        "agent_handoff_control_required"
    );
    assert_eq!(
        db.activate_agent_run(
            source.owner_user(),
            source.project(),
            source.agent_id(),
            "not-issued"
        )
        .unwrap_err()
        .code(),
        "agent_handoff_control_required"
    );
    assert!(db.validate_agent_run(&source).is_ok());
}

#[test]
fn native_host_handoff_recovery_cannot_bypass_waiting_or_failed_before_pending_issuance() {
    for failed in [false, true] {
        let (_dir, db, source, h) = fixture();
        let owner = source.host_goal_owner_key();
        if failed {
            db.abort_host_goal_handoff(
                &owner,
                &h.goal_id,
                &h.handoff_id,
                "stop before launch",
                NOW,
            )
            .unwrap();
        }
        let before = db
            .read_host_goal_handoff(&owner, &h.goal_id)
            .unwrap()
            .unwrap();
        let source_state = db.read_host_context_state(&owner, source.run_id()).unwrap();
        assert_eq!(before.attempt_count, 0);
        assert!(before.pending_agent_run_id.is_none());
        assert_eq!(db.agent_run_task_goal(&source).unwrap(), None);
        assert_eq!(
            db.recover_isolated_agent(
                source.owner_user(),
                source.project(),
                source.agent_id(),
                source.generation()
            )
            .unwrap_err()
            .code(),
            "agent_handoff_control_required"
        );
        assert!(db.validate_agent_run(&source).is_ok());
        assert_eq!(
            db.isolated_agent_generation(source.owner_user(), source.project(), source.agent_id())
                .unwrap(),
            source.generation()
        );
        assert_eq!(
            db.read_host_goal_handoff(&owner, &h.goal_id)
                .unwrap()
                .unwrap(),
            before
        );
        assert_eq!(
            db.read_host_context_state(&owner, source.run_id()).unwrap(),
            source_state
        );
        let called = std::cell::Cell::new(false);
        assert!(db
            .with_agent_run_for_path(
                &source,
                "private_write",
                &format!("workspace/{}/effect.txt", h.goal_id),
                true,
                || {
                    called.set(true);
                    Ok(())
                }
            )
            .is_err());
        assert!(!called.get());
    }
}

fn issue_claim(db: &Database, source: &VerifiedAgentRun, h: &HostGoalHandoff, now: i64) -> String {
    let grant = db
        .issue_agent_handoff_claim(
            &source.host_goal_owner_key(),
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            now,
        )
        .unwrap();
    assert_eq!(format!("{grant:?}"), "AgentHandoffClaimGrant([REDACTED])");
    grant.into_secret()
}
fn claim_window(
    db: &Database,
    h: &HostGoalHandoff,
    window: &str,
    grant: &str,
    now: i64,
) -> Result<AgentHostWindowBinding, AgentIsolationError> {
    db.claim_agent_handoff_host_window(
        PRINCIPAL,
        window,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        grant,
        now,
    )
}

#[test]
fn native_host_claim_is_one_time_hashed_pending_only_and_same_window_replays() {
    let (_dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    let token = issue_claim(&db, &source, &h, NOW);
    assert!(valid_claim(&token));
    assert!(db.authenticate_agent_run(&token).is_err());
    assert_eq!(
        db.issue_agent_handoff_claim(
            &source.host_goal_owner_key(),
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            NOW
        )
        .unwrap_err()
        .code(),
        "goal_handoff_claim_already_issued"
    );
    let stored:(String,String)=db.conn_for_tests().query_row("SELECT handoff_claim_hash,handoff_claim_principal_key FROM wc_agent_runs WHERE run_id=?1",[h.pending_agent_run_id.as_deref().unwrap()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert!(stored.0 == claim_digest(&token));
    assert_eq!(stored.1, PRINCIPAL);
    let public = serde_json::to_string(
        &db.read_host_goal_handoff(&source.host_goal_owner_key(), &h.goal_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(!public.contains(&token));
    assert!(!public.contains(&stored.0));
    assert!(!public.contains("handoff_claim"));
    assert!(claim_window(&db, &h, SOURCE_WINDOW, &token, NOW).is_err());
    assert!(claim_window(&db, &h, "candidate", h.attempt_id.as_deref().unwrap(), NOW).is_err());
    let bound = claim_window(&db, &h, "candidate", &token, NOW + 1).unwrap();
    assert!(matches!(bound, AgentHostWindowBinding::Pending { .. }));
    assert!(db.validate_agent_run(&source).is_ok());
    let fake = format!("wc_hc_{}", "0".repeat(64));
    assert!(claim_window(&db, &h, "candidate", &fake, NOW + 1).is_err());
    assert_eq!(
        claim_window(&db, &h, "candidate", &token, NOW + 1).unwrap(),
        bound
    );
    assert!(claim_window(&db, &h, "other-candidate", &token, NOW + 1).is_err());
    db.prepare_agent_handoff_for_host_window(
        PRINCIPAL,
        "candidate",
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        h.checkpoint_sequence,
        DIGEST,
        NOW + 1,
    )
    .unwrap();
    let complete = commit(&db, &h, "candidate", NOW + 2).unwrap();
    assert_eq!(complete.status, "committed");
    assert!(claim_window(&db, &h, "candidate", &token, NOW + 3).is_err());
    assert_eq!(commit(&db, &h, "candidate", NOW + 3).unwrap(), complete);
    assert_eq!(complete.attempt_count, 1);
    assert_eq!(complete.deadline_at, h.deadline_at);
    assert!(db.validate_agent_run(&source).is_err());
}

#[test]
fn native_host_claim_rejects_foreign_pending_active_disabled_and_revoked_window_tombstones() {
    let (_dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    let token = issue_claim(&db, &source, &h, NOW);
    let other_grant = db.create_isolated_agent("owner", "project", None).unwrap();
    let other = db.authenticate_agent_run(&other_grant.token).unwrap();
    let context = db.ensure_agent_host_context(&other, None).unwrap();
    let owner = other.host_goal_owner_key();
    db.register_agent_host_window("other-principal", "other-source-window", &other)
        .unwrap();
    assert!(claim_window(&db, &h, "other-source-window", &token, NOW).is_err());
    assert!(db
        .claim_agent_handoff_host_window(
            "other-principal",
            "fresh",
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            &token,
            NOW
        )
        .is_err());
    let other_goal = db
        .create_host_goal_at_in_context(
            &owner,
            "project",
            "/private",
            "Other task",
            "create",
            NOW,
            Some(&context),
        )
        .unwrap()
        .goal;
    db.checkpoint_host_goal_with_request_in_context(
        &owner,
        &other_goal.goal_id,
        other_goal.revision,
        None,
        &other_goal.state,
        HostGoalCheckpointAction::Handoff,
        "handoff",
        "handoff",
        "handoff",
        &[],
        NOW,
        Some(&context),
        &[],
    )
    .unwrap();
    let other_h = db
        .read_host_goal_handoff(&owner, &other_goal.goal_id)
        .unwrap()
        .unwrap();
    let other_h = begin(&db, &other, &other_h, NOW);
    db.bind_handoff_host_window(
        "other-principal",
        "other-pending-window",
        &other_h.goal_id,
        &other_h.handoff_id,
        other_h.attempt_id.as_deref().unwrap(),
        NOW,
    )
    .unwrap();
    assert!(claim_window(&db, &h, "other-pending-window", &token, NOW).is_err());
    db.revoke_agent_run(
        other.owner_user(),
        other.project(),
        other.agent_id(),
        other_h.pending_agent_run_id.as_deref().unwrap(),
    )
    .unwrap();
    assert!(claim_window(&db, &h, "other-pending-window", &token, NOW).is_err());
    db.revoke_agent_host_principal("other-principal").unwrap();
    assert!(claim_window(&db, &h, "other-pending-window", &token, NOW).is_err());
    assert!(claim_window(&db, &h, "other-source-window", &token, NOW).is_err());
    assert!(matches!(
        claim_window(&db, &h, "fresh-candidate", &token, NOW + 1).unwrap(),
        AgentHostWindowBinding::Pending { .. }
    ));
}

#[test]
fn native_host_claim_survives_restart_cannot_be_reissued_and_old_attempt_never_redeems() {
    let (dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    let token = issue_claim(&db, &source, &h, NOW);
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(
        db.issue_agent_handoff_claim(
            &source.host_goal_owner_key(),
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            NOW + 1
        )
        .unwrap_err()
        .code(),
        "goal_handoff_claim_already_issued"
    );
    assert!(claim_window(&db, &h, "late", &token, NOW + 2000).is_err());
    assert!(db
        .resolve_agent_host_window(PRINCIPAL, "late", NOW + 2000)
        .unwrap()
        .is_none());
    let next = begin(&db, &source, &h, NOW + 2000);
    let next_token = issue_claim(&db, &source, &next, NOW + 2000);
    assert!(token != next_token);
    assert!(claim_window(&db, &h, "late", &token, NOW + 2001).is_err());
    assert!(claim_window(&db, &next, "late", &token, NOW + 2001).is_err());
    assert!(matches!(
        claim_window(&db, &next, "fresh", &next_token, NOW + 2001).unwrap(),
        AgentHostWindowBinding::Pending { .. }
    ));
    assert_eq!(next.attempt_count, 2);
}

#[test]
fn native_host_claim_concurrent_different_connections_has_one_winner_without_extending_deadline() {
    let (dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    let token = issue_claim(&db, &source, &h, NOW);
    let second = Database::open(&dir.path().join("state.db")).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            claim_window(&db, &h, "a", &token, NOW + 1)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            claim_window(&second, &h, "b", &token, NOW + 1)
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let current = db
        .read_host_goal_handoff(&source.host_goal_owner_key(), &h.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.deadline_at, h.deadline_at);
    assert_eq!(current.attempt_count, 1);
    assert_eq!(current.status, "awaiting_ready");
    assert!(current.prepared_at.is_none());
    assert!(db.validate_agent_run(&source).is_ok());
}

#[test]
fn native_host_claim_issuance_preserves_null_fence_compatibility_but_rejects_active_attempts_and_manual_bindings(
) {
    for active in [false, true] {
        let (_dir, db, source, h) = fixture();
        let h = begin(&db, &source, &h, NOW);
        db.conn_for_tests().execute("UPDATE wc_host_goal_checkpoints SET snapshot_json=json_remove(snapshot_json,'$.source_fence') WHERE sequence=?1",[h.checkpoint_sequence]).unwrap();
        if active {
            db.conn_for_tests().execute("INSERT INTO wc_host_goal_attempts(attempt_id,goal_id,snapshot_json) VALUES('orphan-running',?1,'{\"status\":\"running\"}')",[&h.goal_id]).unwrap();
        }
        let result = db.issue_agent_handoff_claim(
            &source.host_goal_owner_key(),
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            NOW,
        );
        assert_eq!(result.is_ok(), !active);
    }
    let (_dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    db.bind_handoff_host_window(
        PRINCIPAL,
        "explicit-candidate",
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        NOW,
    )
    .unwrap();
    assert_eq!(
        db.issue_agent_handoff_claim(
            &source.host_goal_owner_key(),
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            NOW
        )
        .unwrap_err()
        .code(),
        "goal_handoff_claim_already_bound"
    );
}

#[test]
fn native_host_claim_source_revocation_or_checkpoint_change_never_consumes_a_new_window() {
    let (_dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    let token = issue_claim(&db, &source, &h, NOW);
    db.revoke_agent_host_principal(PRINCIPAL).unwrap();
    assert!(claim_window(&db, &h, "candidate", &token, NOW + 1).is_err());
    db.register_agent_host_window(PRINCIPAL, SOURCE_WINDOW, &source)
        .unwrap();
    db.conn_for_tests().execute("UPDATE wc_host_goal_checkpoints SET snapshot_json=json_set(snapshot_json,'$.plan_version',999) WHERE sequence=?1",[h.checkpoint_sequence]).unwrap();
    assert!(claim_window(&db, &h, "candidate", &token, NOW + 1).is_err());
    assert!(db
        .resolve_agent_host_window(PRINCIPAL, "candidate", NOW + 1)
        .unwrap()
        .is_none());
    let (_dir, db, source, h) = fixture();
    let h = begin(&db, &source, &h, NOW);
    let token = issue_claim(&db, &source, &h, NOW);
    db.revoke_agent_run(
        source.owner_user(),
        source.project(),
        source.agent_id(),
        source.run_id(),
    )
    .unwrap();
    assert!(claim_window(&db, &h, "candidate", &token, NOW + 1).is_err());
    assert!(db
        .resolve_agent_host_window(PRINCIPAL, "candidate", NOW + 1)
        .unwrap()
        .is_none());
}

static QUEUED_SQLITE_CALL: std::sync::OnceLock<(std::sync::Mutex<bool>, std::sync::Condvar)> =
    std::sync::OnceLock::new();
fn signal_sqlite_wait(attempt: i32) -> bool {
    let (entered, wake) = QUEUED_SQLITE_CALL
        .get_or_init(|| (std::sync::Mutex::new(false), std::sync::Condvar::new()));
    *entered.lock().unwrap() = true;
    wake.notify_all();
    std::thread::sleep(std::time::Duration::from_millis(1));
    attempt < 5000
}

fn queued_past_deadline(
    db: &Database,
    path: &std::path::Path,
    operation: impl FnOnce() -> Result<(), String> + Send,
) -> Result<(), String> {
    let blocker = Database::open(&path.to_path_buf()).unwrap();
    let mut connection = blocker.conn_for_tests();
    let held = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (entered, wake) = QUEUED_SQLITE_CALL
        .get_or_init(|| (std::sync::Mutex::new(false), std::sync::Condvar::new()));
    *entered.lock().unwrap() = false;
    db.conn_for_tests()
        .busy_handler(Some(signal_sqlite_wait))
        .unwrap();
    std::thread::scope(|scope| {
        let call = scope.spawn(operation);
        let (entered, timed) = wake
            .wait_timeout_while(
                entered.lock().unwrap(),
                std::time::Duration::from_secs(2),
                |entered| !*entered,
            )
            .unwrap();
        assert!(
            *entered && !timed.timed_out(),
            "request did not reach the SQLite writer lock"
        );
        drop(entered);
        // The request clock is exactly 30 ms before its deadline. Start this
        // deliberate 80 ms hold only after SQLite confirms the call is queued.
        // Scheduler delays can only extend the hold, never make it authorize.
        std::thread::sleep(std::time::Duration::from_millis(80));
        held.rollback().unwrap();
        call.join().unwrap()
    })
}

#[test]
fn native_host_claim_lock_wait_cannot_authorize_issue_claim_read_prepare_or_commit_after_deadline()
{
    for stage in [
        "issue",
        "claim",
        "lease",
        "prepare",
        "commit",
        "direct_commit",
    ] {
        let (dir, db, source, h) = fixture();
        let h = begin(&db, &source, &h, NOW);
        let owner = source.host_goal_owner_key();
        let token = if stage == "issue" {
            None
        } else {
            Some(issue_claim(&db, &source, &h, NOW))
        };
        if !matches!(stage, "issue" | "claim") {
            claim_window(&db, &h, "candidate", token.as_deref().unwrap(), NOW + 1).unwrap();
        }
        if matches!(stage, "commit" | "direct_commit") {
            db.prepare_agent_handoff_for_host_window(
                PRINCIPAL,
                "candidate",
                &h.goal_id,
                &h.handoff_id,
                h.attempt_id.as_deref().unwrap(),
                h.checkpoint_sequence,
                DIGEST,
                NOW + 2,
            )
            .unwrap();
        }
        let requested_now = h.deadline_at.unwrap() - 30;
        let read_called = std::sync::atomic::AtomicBool::new(false);
        let result = queued_past_deadline(&db, &dir.path().join("state.db"), || {
            let attempt = h.attempt_id.as_deref().unwrap();
            match stage {
                "issue" => db
                    .issue_agent_handoff_claim(
                        &owner,
                        &h.goal_id,
                        &h.handoff_id,
                        attempt,
                        requested_now,
                    )
                    .map(|_| ())
                    .map_err(|e| e.code().into()),
                "claim" => claim_window(
                    &db,
                    &h,
                    "candidate",
                    token.as_deref().unwrap(),
                    requested_now,
                )
                .map(|_| ())
                .map_err(|e| e.code().into()),
                "lease" => db
                    .with_pending_agent_host_handoff(
                        PRINCIPAL,
                        "candidate",
                        &h.goal_id,
                        &h.handoff_id,
                        attempt,
                        requested_now,
                        |_, _| {
                            read_called.store(true, std::sync::atomic::Ordering::SeqCst);
                            Ok(())
                        },
                    )
                    .map_err(|e| e.code().into()),
                "prepare" => db
                    .prepare_agent_handoff_for_host_window(
                        PRINCIPAL,
                        "candidate",
                        &h.goal_id,
                        &h.handoff_id,
                        attempt,
                        h.checkpoint_sequence,
                        DIGEST,
                        requested_now,
                    )
                    .map(|_| ())
                    .map_err(|e| e.code().into()),
                "commit" => commit(&db, &h, "candidate", requested_now)
                    .map(|_| ())
                    .map_err(|e| e.code().into()),
                _ => {
                    let context = HostGoalInvocationContext {
                        context_epoch: h.pending_agent_run_id.clone().unwrap(),
                        identity_source: "agent_run".into(),
                    };
                    db.commit_host_goal_handoff_ready_with_run(
                        &owner,
                        &h.goal_id,
                        &h.handoff_id,
                        attempt,
                        h.checkpoint_sequence,
                        DIGEST,
                        &context,
                        requested_now,
                        &source,
                        h.pending_agent_run_id.as_deref().unwrap(),
                    )
                    .map(|_| ())
                    .map_err(|e| e.code().into())
                }
            }
        });
        assert!(result.is_err(), "{stage} authorized after queued deadline");
        assert!(
            !matches!(
                result.as_ref().err().map(String::as_str),
                Some("agent_host_binding_unavailable" | "goal_store_unavailable")
            ),
            "queue did not drain normally"
        );
        assert!(db.validate_agent_run(&source).is_ok());
        assert!(!read_called.load(std::sync::atomic::Ordering::SeqCst));
        let current = db
            .read_host_goal_handoff(&owner, &h.goal_id)
            .unwrap()
            .unwrap();
        assert_ne!(current.status, "committed");
        assert_eq!(current.deadline_at, h.deadline_at);
        assert_eq!(current.attempt_count, 1);
        if matches!(stage, "issue" | "claim") {
            assert!(db
                .resolve_agent_host_window(PRINCIPAL, "candidate", NOW + 1)
                .unwrap()
                .is_none());
        }
        if stage == "issue" {
            let issued: bool = db
                .conn_for_tests()
                .query_row(
                    "SELECT handoff_claim_hash IS NOT NULL FROM wc_agent_runs WHERE run_id=?1",
                    [h.pending_agent_run_id.as_deref().unwrap()],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!issued);
        }
        if stage == "prepare" {
            assert!(current.prepared_at.is_none());
        }
    }
}
