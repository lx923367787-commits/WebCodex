use super::*;

const OWNER: &str = "handoff-owner";
const START: i64 = 1_000_000;
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn identity(epoch: &str) -> HostGoalInvocationContext {
    HostGoalInvocationContext {
        context_epoch: epoch.into(),
        identity_source: "window_bound".into(),
    }
}

fn fixture() -> (tempfile::TempDir, Database, HostGoalRecord, HostGoalHandoff) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let (g, h) = saved_handoff(&db, OWNER);
    (dir, db, g, h)
}

fn saved_handoff(db: &Database, owner: &str) -> (HostGoalRecord, HostGoalHandoff) {
    let source = identity("source");
    db.ensure_host_context_identity(owner, &source).unwrap();
    let g = db
        .create_host_goal_in_task_directory_in_context(
            owner,
            "project",
            "/workspace",
            "workspace/task-output",
            "Retain original Goal and artifacts",
            "create",
            START,
            Some(&source),
        )
        .unwrap()
        .goal;
    let mut state = g.state.clone();
    state.next_step = "Continue the preserved work".into();
    let g = db
        .checkpoint_host_goal_with_request_in_context(
            owner,
            &g.goal_id,
            g.revision,
            None,
            &state,
            HostGoalCheckpointAction::Handoff,
            "Saved task-only handoff",
            "checkpoint",
            "request",
            &[],
            START,
            Some(&source),
            &[],
        )
        .unwrap()
        .goal;
    let h = db
        .read_host_goal_handoff(owner, &g.goal_id)
        .unwrap()
        .unwrap();
    (g, h)
}

fn begin(db: &Database, h: &HostGoalHandoff, now: i64) -> HostGoalHandoff {
    db.begin_host_goal_handoff_attempt(
        OWNER,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref(),
        now,
        3,
    )
    .unwrap()
}

fn prepare_candidate(
    db: &Database,
    h: &HostGoalHandoff,
    epoch: &str,
    now: i64,
) -> HostGoalInvocationContext {
    let c = identity(epoch);
    db.ensure_host_context_identity(OWNER, &c).unwrap();
    db.bind_host_goal_handoff_candidate(
        OWNER,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        &c,
        now,
    )
    .unwrap();
    db.record_host_goal_handoff_prepared(
        OWNER,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        h.checkpoint_sequence,
        DIGEST,
        &c,
        now,
    )
    .unwrap();
    c
}

fn commit(
    db: &Database,
    h: &HostGoalHandoff,
    c: &HostGoalInvocationContext,
    now: i64,
) -> Result<HostGoalHandoff, GoalStoreError> {
    db.commit_host_goal_handoff_ready(
        OWNER,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        h.checkpoint_sequence,
        DIGEST,
        c,
        now,
    )
}

#[test]
fn handoff_first_takeover_requires_server_preparation_then_atomically_retires_source() {
    let (_dir, db, g, h) = fixture();
    assert_eq!(
        db.read_host_context_state(OWNER, "source")
            .unwrap()
            .unwrap()
            .state,
        "draining"
    );
    assert_eq!(h.status, "waiting");
    assert!(db
        .gate_host_goal_execution(OWNER, &g.goal_id, Some(&identity("source")))
        .is_err());
    let h = begin(&db, &h, START);
    let c = identity("successor");
    db.ensure_host_context_identity(OWNER, &c).unwrap();
    db.bind_host_goal_handoff_candidate(
        OWNER,
        &g.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        &c,
        START + 1,
    )
    .unwrap();
    assert!(commit(&db, &h, &c, START + 1).is_err());
    assert!(db
        .gate_host_goal_execution(OWNER, &g.goal_id, Some(&c))
        .is_err());
    db.record_host_goal_handoff_prepared(
        OWNER,
        &g.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        h.checkpoint_sequence,
        DIGEST,
        &c,
        START + 2,
    )
    .unwrap();
    let final_h = commit(&db, &h, &c, START + 3).unwrap();
    assert_eq!(final_h.status, "committed");
    assert_eq!(final_h.goal_id, g.goal_id);
    assert_eq!(
        db.read_host_context_state(OWNER, "source")
            .unwrap()
            .unwrap()
            .state,
        "retired"
    );
    assert!(db
        .gate_host_goal_execution(OWNER, &g.goal_id, Some(&c))
        .is_ok());
    assert!(db
        .gate_host_goal_execution(OWNER, &g.goal_id, Some(&identity("source")))
        .is_err());
    assert!(db
        .gate_host_goal_execution(OWNER, &g.goal_id, None)
        .is_err());
    assert!(db
        .gate_host_goal_execution(OWNER, &g.goal_id, Some(&identity("other")))
        .is_err());
}

#[test]
fn handoff_three_failures_then_fourth_success_preserves_budget_on_restart() {
    let (dir, mut db, _g, mut h) = fixture();
    for n in 1..=4 {
        h = begin(&db, &h, START + n * 10);
        assert_eq!(h.attempt_count, n as u32);
        let c = prepare_candidate(&db, &h, &format!("candidate-{n}"), START + n * 10 + 1);
        if n < 4 {
            h = db
                .fail_host_goal_handoff_attempt(
                    OWNER,
                    &h.goal_id,
                    &h.handoff_id,
                    h.attempt_id.as_deref().unwrap(),
                    "launch did not become ready",
                    START + n * 10 + 2,
                )
                .unwrap();
            assert!(commit(&db, &h, &c, START + n * 10 + 3).is_err());
            drop(db);
            db = Database::open(&dir.path().join("state.db")).unwrap();
            assert_eq!(
                db.read_host_goal_handoff(OWNER, &h.goal_id)
                    .unwrap()
                    .unwrap(),
                h
            );
        } else {
            assert_eq!(
                commit(&db, &h, &c, START + n * 10 + 2).unwrap().status,
                "committed"
            );
        }
    }
}

#[test]
fn handoff_four_failed_starts_stop_and_never_allocate_a_fifth() {
    let (dir, db, g, mut h) = fixture();
    for n in 1..=4 {
        h = begin(&db, &h, START + n * 10);
        h = db
            .fail_host_goal_handoff_attempt(
                OWNER,
                &g.goal_id,
                &h.handoff_id,
                h.attempt_id.as_deref().unwrap(),
                "startup failed",
                START + n * 10 + 1,
            )
            .unwrap();
    }
    assert_eq!(h.status, "failed");
    assert_eq!(h.attempt_count, 4);
    assert_eq!(begin(&db, &h, START + 100_000), h);
    let preserved = db.get_host_goal(OWNER, &g.goal_id).unwrap().unwrap();
    assert_eq!(preserved.phase, HostGoalPhase::Blocked);
    assert_eq!(preserved.objective, g.objective);
    assert!(db
        .read_host_goal_handoff_snapshot(OWNER, &g.goal_id, "source")
        .unwrap()
        .checkpoint
        .is_some());
    assert_eq!(
        db.read_host_context_state(OWNER, "source")
            .unwrap()
            .unwrap()
            .state,
        "stopped"
    );
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(begin(&db, &h, START + 200_000), h);
    assert!(db
        .gate_host_goal_execution(OWNER, &g.goal_id, None)
        .is_err());
}

#[test]
fn handoff_timeout_duplicate_callbacks_and_late_candidates_are_fenced() {
    let (_dir, db, _g, h) = fixture();
    let first = begin(&db, &h, START);
    assert_eq!(begin(&db, &first, START + 1), first);
    let c = prepare_candidate(&db, &first, "late", START + 1);
    assert!(commit(&db, &first, &c, START + 3000).is_err());
    let second = begin(&db, &first, START + 3000);
    assert_eq!(second.attempt_count, 2);
    assert!(commit(&db, &first, &c, START + 3001).is_err());
    assert!(db
        .fail_host_goal_handoff_attempt(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            first.attempt_id.as_deref().unwrap(),
            "late failure",
            START + 3001
        )
        .is_err());
    assert_eq!(begin(&db, &first, START + 3001), second);
    assert!(db
        .begin_host_goal_handoff_attempt(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            first.attempt_id.as_deref(),
            START + 6000,
            3
        )
        .is_err());
    assert_eq!(
        db.read_host_context_state(OWNER, "late")
            .unwrap()
            .unwrap()
            .state,
        "retired"
    );
}

#[test]
fn handoff_concurrent_confirmation_and_lost_response_only_commit_once() {
    let (_dir, db, _g, h) = fixture();
    let h = begin(&db, &h, START);
    let c = prepare_candidate(&db, &h, "winner", START + 1);
    let db = std::sync::Arc::new(db);
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| commit(&db, &h, &c, START + 2).unwrap());
        let b = scope.spawn(|| commit(&db, &h, &c, START + 2).unwrap());
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results[0], results[1]);
    let actual = db
        .read_host_goal_handoff(OWNER, &h.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(actual.status, "committed");
    assert_eq!(begin(&db, &actual, START + 100_000), actual);
    assert_eq!(
        db.fail_host_goal_handoff_attempt(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            "client response lost",
            START + 100_000
        )
        .unwrap(),
        actual
    );
    assert_eq!(
        db.get_host_goal(OWNER, &h.goal_id).unwrap().unwrap().fence,
        1
    );
}

#[test]
fn handoff_requires_trusted_identity_and_exact_package_and_rejects_competing_candidate() {
    let (_dir, db, _g, h) = fixture();
    let h = begin(&db, &h, START);
    let c = prepare_candidate(&db, &h, "winner", START + 1);
    let other = identity("other");
    db.ensure_host_context_identity(OWNER, &other).unwrap();
    assert!(db
        .bind_host_goal_handoff_candidate(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            &other,
            START + 2
        )
        .is_err());
    let forged = HostGoalInvocationContext {
        identity_source: "explicit_context_only".into(),
        ..c.clone()
    };
    assert!(commit(&db, &h, &forged, START + 2).is_err());
    assert!(db
        .commit_host_goal_handoff_ready(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            h.checkpoint_sequence,
            &"b".repeat(64),
            &c,
            START + 2
        )
        .is_err());
    assert!(db
        .record_host_goal_handoff_prepared(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            h.checkpoint_sequence + 1,
            DIGEST,
            &c,
            START + 2
        )
        .is_err());
    assert!(db
        .read_host_goal_handoff("other-owner", &h.goal_id)
        .is_err());
}

#[test]
fn handoff_outstanding_cli_or_background_operation_blocks_transfer() {
    let (_dir, db, _g, h) = fixture();
    let h = begin(&db, &h, START);
    let c = prepare_candidate(&db, &h, "candidate", START + 1);
    // An authoritative unresolved receipt survives restart and is a hard gate,
    // including when the in-memory request guard has already disappeared.
    db.conn.lock().unwrap().execute("INSERT INTO wc_host_goal_operations(goal_id,operation_id,terminal,snapshot_json) VALUES(?1,'old-cli',0,'{}')",[&h.goal_id]).unwrap();
    assert_eq!(
        commit(&db, &h, &c, START + 2).unwrap_err().code(),
        "goal_handoff_execution_active"
    );
    assert_eq!(
        db.read_host_context_state(OWNER, "source")
            .unwrap()
            .unwrap()
            .state,
        "draining"
    );
    assert!(db
        .gate_host_goal_execution(OWNER, &h.goal_id, Some(&c))
        .is_err());
}

#[test]
fn handoff_and_agent_run_activation_commit_or_rollback_together_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    db.init_agent_isolation_tables().unwrap();
    let old_grant = db.create_isolated_agent(OWNER, "project", None).unwrap();
    let source = db.authenticate_agent_run(&old_grant.token).unwrap();
    let owner = source.host_goal_owner_key();
    let (_g, h) = saved_handoff(&db, &owner);
    let pending = db.begin_agent_run_handoff(&source).unwrap();
    assert_eq!(pending.agent.agent_id, source.agent_id());
    assert_ne!(pending.run_id, source.run_id());
    let h = db
        .begin_host_goal_handoff_attempt(&owner, &h.goal_id, &h.handoff_id, None, START, 3)
        .unwrap();
    let c = identity("same-agent-new-window");
    db.ensure_host_context_identity(&owner, &c).unwrap();
    db.bind_host_goal_handoff_candidate(
        &owner,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        &c,
        START + 1,
    )
    .unwrap();
    db.record_host_goal_handoff_prepared(
        &owner,
        &h.goal_id,
        &h.handoff_id,
        h.attempt_id.as_deref().unwrap(),
        h.checkpoint_sequence,
        DIGEST,
        &c,
        START + 1,
    )
    .unwrap();
    db.conn.lock().unwrap().execute_batch("CREATE TRIGGER fail_source_retirement BEFORE UPDATE OF state ON wc_host_context_states WHEN NEW.state=3 AND OLD.context_epoch='source' BEGIN SELECT RAISE(ABORT,'injected source retirement failure'); END;").unwrap();
    let transfer = || {
        db.commit_host_goal_handoff_ready_with_run(
            &owner,
            &h.goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            h.checkpoint_sequence,
            DIGEST,
            &c,
            START + 2,
            &source,
            &pending.run_id,
        )
    };
    assert!(transfer().is_err());
    assert!(db.authenticate_agent_run(&old_grant.token).is_ok());
    assert!(db.authenticate_agent_run(&pending.token).is_err());
    let pending_scope: Option<String> = db
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT task_goal_id FROM wc_agent_runs WHERE run_id=?1",
            [&pending.run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(pending_scope.is_none());
    assert_eq!(
        db.read_host_goal_handoff(&owner, &h.goal_id)
            .unwrap()
            .unwrap()
            .status,
        "prepared"
    );
    db.conn
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_source_retirement;")
        .unwrap();
    let committed = transfer().unwrap();
    assert_eq!(transfer().unwrap(), committed);
    assert_eq!(committed.agent_id.as_deref(), Some(source.agent_id()));
    assert_eq!(
        committed.source_agent_run_id.as_deref(),
        Some(source.run_id())
    );
    assert_eq!(
        committed.successor_agent_run_id.as_deref(),
        Some(pending.run_id.as_str())
    );
    assert!(db.authenticate_agent_run(&old_grant.token).is_err());
    assert_eq!(
        db.agent_run_task_goal(&db.authenticate_agent_run(&pending.token).unwrap())
            .unwrap()
            .as_deref(),
        Some(h.goal_id.as_str())
    );
    assert_eq!(
        db.authenticate_agent_run(&pending.token)
            .unwrap()
            .agent_id(),
        source.agent_id()
    );
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(
        db.read_host_goal_handoff(&owner, &h.goal_id)
            .unwrap()
            .unwrap(),
        committed
    );
    assert!(db.authenticate_agent_run(&old_grant.token).is_err());
    assert_eq!(
        db.authenticate_agent_run(&pending.token)
            .unwrap()
            .generation(),
        source.generation() + 1
    );
}

#[test]
fn handoff_owner_cannot_be_bypassed_by_goal_selection_or_an_unrelated_context() {
    let (_dir, db, _g, h) = fixture();
    let outsider = identity("outsider");
    db.ensure_host_context_identity(OWNER, &outsider).unwrap();
    assert!(db
        .select_current_host_goal_in_context(OWNER, &h.goal_id, START, Some(&outsider))
        .is_err());
    let h = begin(&db, &h, START);
    let c = prepare_candidate(&db, &h, "successor", START + 1);
    commit(&db, &h, &c, START + 2).unwrap();
    assert!(db
        .select_current_host_goal_in_context(OWNER, &h.goal_id, START + 3, Some(&outsider))
        .is_err());
    assert!(db
        .select_current_host_goal_in_context(OWNER, &h.goal_id, START + 3, None)
        .is_err());
    assert!(db
        .select_current_host_goal_in_context(OWNER, &h.goal_id, START + 3, Some(&c))
        .is_ok());
    let other = db
        .create_host_goal_at_in_context(
            OWNER,
            "project",
            "/workspace",
            "Independent authorized Goal",
            "other",
            START + 3,
            Some(&outsider),
        )
        .unwrap()
        .goal;
    assert!(db
        .select_current_host_goal_in_context(OWNER, &other.goal_id, START + 4, Some(&outsider))
        .is_ok());
    assert_eq!(
        db.host_goal_handoff_for_context(OWNER, "source")
            .unwrap()
            .unwrap()
            .goal_id,
        h.goal_id
    );
}

#[test]
fn handoff_checkpoint_without_a_trusted_context_is_rejected_without_a_partial_save() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let g = db
        .create_host_goal_at(
            OWNER,
            "project",
            "/workspace",
            "Keep task state",
            "create",
            START,
        )
        .unwrap()
        .goal;
    let weak = HostGoalInvocationContext {
        context_epoch: "model-declared".into(),
        identity_source: "explicit_context_only".into(),
    };
    db.ensure_host_context_identity(OWNER, &weak).unwrap();
    for context in [None, Some(&weak)] {
        let result = db.checkpoint_host_goal_with_request_in_context(
            OWNER,
            &g.goal_id,
            g.revision,
            None,
            &g.state,
            HostGoalCheckpointAction::Handoff,
            "No trustworthy caller",
            "handoff",
            "request",
            &[],
            START + 1,
            context,
            &[],
        );
        assert_eq!(result.unwrap_err().code(), "goal_handoff_identity_required");
        assert_eq!(
            db.get_host_goal(OWNER, &g.goal_id)
                .unwrap()
                .unwrap()
                .revision,
            g.revision
        );
        assert!(db
            .read_host_goal_handoff(OWNER, &g.goal_id)
            .unwrap()
            .is_none());
        assert!(db
            .latest_host_goal_checkpoint(OWNER, &g.goal_id)
            .unwrap()
            .is_none());
    }
}

#[test]
fn handoff_late_async_abort_cannot_stop_a_newer_attempt() {
    let (_dir, db, _g, h) = fixture();
    let first = begin(&db, &h, START);
    let second = begin(&db, &first, START + 3000);
    assert_eq!(second.attempt_count, 2);
    for old_attempt in [None, first.attempt_id.as_deref()] {
        assert_eq!(
            db.abort_host_goal_handoff_attempt(
                OWNER,
                &h.goal_id,
                &h.handoff_id,
                old_attempt,
                "Late preflight failure",
                START + 3001
            )
            .unwrap(),
            second
        );
    }
    assert_eq!(
        db.read_host_goal_handoff(OWNER, &h.goal_id)
            .unwrap()
            .unwrap(),
        second
    );
    let stopped = db
        .abort_host_goal_handoff_attempt(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            second.attempt_id.as_deref(),
            "Current preflight failed",
            START + 3002,
        )
        .unwrap();
    assert_eq!(stopped.status, "failed");
    assert_eq!(stopped.attempt_count, 2);

    let (_dir, db, _g, h) = fixture();
    let stopped = db
        .abort_host_goal_handoff_attempt(
            OWNER,
            &h.goal_id,
            &h.handoff_id,
            None,
            "Initial preflight failed",
            START,
        )
        .unwrap();
    assert_eq!(stopped.status, "failed");
    assert_eq!(stopped.attempt_count, 0);
}
