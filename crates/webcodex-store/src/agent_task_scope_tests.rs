use super::*;

struct Scoped {
    dir: tempfile::TempDir,
    db: Database,
    owner: String,
    run: crate::VerifiedAgentRun,
    old: crate::VerifiedAgentRun,
    context: HostGoalInvocationContext,
    goal: HostGoalRecord,
    other: HostGoalRecord,
}

fn scoped() -> Scoped {
    let (dir, db) = database();
    db.init_agent_isolation_tables().unwrap();
    let old_grant = db.create_isolated_agent("user", "project", None).unwrap();
    let old = db.authenticate_agent_run(&old_grant.token).unwrap();
    let owner = old.host_goal_owner_key();
    let source = HostGoalInvocationContext {
        context_epoch: "source-window".into(),
        identity_source: "window_bound".into(),
    };
    db.ensure_host_context_identity(&owner, &source).unwrap();
    let goal = db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "/private",
            "workspace/saved-task",
            "original task",
            "task",
            NOW,
            Some(&source),
        )
        .unwrap()
        .goal;
    let other = db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "/private",
            "workspace/other-task",
            "other task",
            "other",
            NOW,
            Some(&source),
        )
        .unwrap()
        .goal;
    // Persist an actual replay for the other Goal; a successor must not recover it.
    db.checkpoint_host_goal_with_request_in_context(
        &owner,
        &other.goal_id,
        other.revision,
        None,
        &other.state,
        HostGoalCheckpointAction::Progress,
        "private other progress",
        "other-checkpoint",
        "other-request",
        &[],
        NOW,
        Some(&source),
        &[],
    )
    .unwrap();
    let saved = db
        .save_host_goal_plan_in_context(
            &owner,
            &goal.goal_id,
            goal.revision,
            &plan(),
            None,
            "plan",
            NOW,
            Some(&source),
        )
        .unwrap();
    let receipt = HostGoalMaterialization {
        json_path: "goals/plan.json".into(),
        json_hash: "a".repeat(64),
        markdown_path: "goals/plan.md".into(),
        markdown_hash: "b".repeat(64),
        verified_at: NOW,
    };
    let published = db
        .materialize_host_goal_plan_in_context(
            &owner,
            &goal.goal_id,
            1,
            &saved.plan.hash,
            &receipt,
            NOW,
            Some(&source),
        )
        .unwrap();
    let confirmed = db
        .confirm_host_goal_plan_in_context(
            &owner,
            &goal.goal_id,
            published.goal.revision,
            1,
            &saved.plan.hash,
            "Start",
            "confirm",
            NOW,
            Some(&source),
        )
        .unwrap()
        .goal;
    db.checkpoint_host_goal_with_request_in_context(
        &owner,
        &goal.goal_id,
        confirmed.revision,
        None,
        &confirmed.state,
        HostGoalCheckpointAction::Handoff,
        "transfer task",
        "handoff",
        "handoff-request",
        &[],
        NOW,
        Some(&source),
        &[],
    )
    .unwrap();
    let h = db
        .read_host_goal_handoff(&owner, &goal.goal_id)
        .unwrap()
        .unwrap();
    let h = db
        .begin_host_goal_handoff_attempt(&owner, &goal.goal_id, &h.handoff_id, None, NOW, 300)
        .unwrap();
    let candidate = HostGoalInvocationContext {
        context_epoch: "candidate-window".into(),
        identity_source: "window_bound".into(),
    };
    db.ensure_host_context_identity(&owner, &candidate).unwrap();
    let attempt = h.attempt_id.as_deref().unwrap();
    db.bind_host_goal_handoff_candidate(
        &owner,
        &goal.goal_id,
        &h.handoff_id,
        attempt,
        &candidate,
        NOW + 1,
    )
    .unwrap();
    let digest = "a".repeat(64);
    db.record_host_goal_handoff_prepared(
        &owner,
        &goal.goal_id,
        &h.handoff_id,
        attempt,
        h.checkpoint_sequence,
        &digest,
        &candidate,
        NOW + 1,
    )
    .unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    db.commit_host_goal_handoff_ready_with_run(
        &owner,
        &goal.goal_id,
        &h.handoff_id,
        attempt,
        h.checkpoint_sequence,
        &digest,
        &candidate,
        NOW + 2,
        &old,
        &pending.run_id,
    )
    .unwrap();
    let run = db.authenticate_agent_run(&pending.token).unwrap();
    let context = db.ensure_agent_host_context(&run, None).unwrap();
    let goal = db
        .read_host_goal_for_agent_run(&run, &goal.goal_id)
        .unwrap()
        .0;
    Scoped {
        dir,
        db,
        owner,
        run,
        old,
        context,
        goal,
        other,
    }
}

fn claim(s: &Scoped) -> crate::HostGoalClaimMutation {
    s.db.ack_agent_host_goal_context_read(
        &s.run,
        &s.goal.goal_id,
        s.goal.revision,
        s.goal.plan_version,
        "research",
    )
    .unwrap();
    s.db.claim_host_goal_node_for_plan_in_context(
        &s.owner,
        &s.goal.goal_id,
        s.goal.revision,
        "research",
        false,
        s.goal.plan_version,
        s.goal.current_attempt_id.as_deref(),
        "new-claim",
        NOW + 3,
        Some(&s.context),
    )
    .unwrap()
}

#[test]
fn agent_task_scope_only_authorized_paths_and_goal_are_readable_after_restart() {
    let s = scoped();
    assert_eq!(
        s.db.agent_run_task_goal(&s.run).unwrap().as_deref(),
        Some(s.goal.goal_id.as_str())
    );
    for path in [
        "workspace/saved-task/result.txt".into(),
        "memory".into(),
        "memory/private.json".into(),
        format!("goals/{}/plans/v1.json", s.goal.goal_id),
        format!("checkpoints/{}/state.json", s.goal.goal_id),
        format!("handoff/{}/task.json", s.goal.goal_id),
    ] {
        assert!(
            s.db.with_agent_run_for_path(&s.run, "private_read", &path, false, || Ok(()))
                .is_ok(),
            "{path}"
        );
    }
    for path in [
        "".into(),
        "identity/memory.json".into(),
        "cli_sessions/private.json".into(),
        "tasks/private.json".into(),
        "memories/private.json".into(),
        "memory/../goals/private.json".into(),
        "workspace/other-task/result.txt".into(),
        "workspace/saved-task-other/result.txt".into(),
        "workspace/saved-task/../other-task/data".into(),
        "workspace\\saved-task\\data".into(),
        format!("goals/{}/plan.json", s.other.goal_id),
        "/workspace/saved-task/x".into(),
        "workspace/saved-task/x:stream".into(),
    ] {
        let called = std::cell::Cell::new(false);
        assert!(
            s.db.with_agent_run_for_path(&s.run, "private_read", &path, false, || {
                called.set(true);
                Ok(())
            })
            .is_err(),
            "{path}"
        );
        assert!(!called.get());
    }
    assert!(s
        .db
        .read_host_goal_for_agent_run(&s.run, &s.other.goal_id)
        .is_err());
    assert!(s
        .db
        .ack_agent_host_goal_context_read(&s.run, &s.other.goal_id, s.other.revision, 0, "research")
        .is_err());
    assert!(s
        .db
        .reserve_host_goal_checkpoint_request_in_context(
            &s.owner,
            &s.other.goal_id,
            "other-checkpoint",
            "other-request",
            Some(&s.context)
        )
        .is_err());
    assert!(s
        .db
        .create_host_goal_at_in_context(
            &s.owner,
            "project",
            "/private",
            "new forbidden goal",
            "new",
            NOW,
            Some(&s.context)
        )
        .is_err());
    assert!(s
        .db
        .with_agent_run(&s.run, "private_read", || Ok(()))
        .is_err());
    assert!(s
        .db
        .with_agent_run_for_goal(&s.run, "goal_plan_verify", &s.other.goal_id, || Ok(()))
        .is_err());
    s.db.unbind_host_goal_window(&s.owner, s.run.run_id(), &s.goal.goal_id)
        .unwrap();
    let db = Database::open(&s.dir.path().join("state.db")).unwrap();
    assert_eq!(
        db.agent_run_task_goal(&s.run).unwrap().as_deref(),
        Some(s.goal.goal_id.as_str())
    );
    assert!(db
        .with_agent_run(&s.run, "private_read", || Ok(()))
        .is_err());
    assert!(db
        .with_agent_run_for_path(
            &s.old,
            "private_read",
            "workspace/saved-task/result.txt",
            false,
            || Ok(())
        )
        .is_err());
    assert!(db
        .conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET task_goal_id=NULL WHERE run_id=?1",
            [s.run.run_id()]
        )
        .is_err());
    let pending = db.begin_agent_run_handoff(&s.run).unwrap();
    assert!(db
        .activate_agent_run(
            s.run.owner_user(),
            s.run.project(),
            s.run.agent_id(),
            &pending.run_id
        )
        .is_err());
}

#[test]
fn agent_task_scope_workspace_writes_require_current_attempt_and_stop_on_pause_or_stale_fence() {
    let s = scoped();
    let target = s.dir.path().join("effect.txt");
    let effect = || {
        std::fs::write(&target, "effect").map_err(|_| crate::AgentIsolationError::new("test_io"))
    };
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "workspace/saved-task/result.txt",
            true,
            effect
        )
        .is_err());
    assert!(!target.exists());
    // Internal, target-specific publication is allowed before business execution.
    assert!(s
        .db
        .with_agent_run_for_goal(&s.run, "goal_plan_publish", &s.goal.goal_id, || Ok(()))
        .is_ok());
    let claimed = claim(&s);
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "workspace/saved-task",
            true,
            || Ok(())
        )
        .is_err());
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_read",
            "workspace/SAVED-TASK/output.txt",
            false,
            || Ok(())
        )
        .is_err());
    s.db.with_agent_run_for_path(
        &s.run,
        "private_write",
        "workspace/saved-task/result.txt",
        true,
        effect,
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "effect");
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            &format!("goals/{}/plans/evil.json", s.goal.goal_id),
            true,
            || Ok(())
        )
        .is_err());
    s.db.conn_for_tests().execute("UPDATE wc_host_goal_attempts SET snapshot_json=json_set(snapshot_json,'$.binding.fence',999) WHERE attempt_id=?1",[&claimed.attempt.binding.attempt_id]).unwrap();
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "workspace/saved-task/result.txt",
            true,
            || Ok(())
        )
        .is_err());
    s.db.conn_for_tests()
        .execute(
            "UPDATE wc_host_goal_attempts SET snapshot_json=?2 WHERE attempt_id=?1",
            rusqlite::params![
                claimed.attempt.binding.attempt_id,
                serde_json::to_string(&claimed.attempt).unwrap()
            ],
        )
        .unwrap();
    s.db.checkpoint_host_goal_with_request_in_context(
        &s.owner,
        &s.goal.goal_id,
        claimed.goal.revision,
        Some(&claimed.attempt.binding),
        &claimed.goal.state,
        HostGoalCheckpointAction::Pause,
        "pause",
        "pause",
        "pause-request",
        &[],
        NOW + 4,
        Some(&s.context),
        &[],
    )
    .unwrap();
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "workspace/saved-task/result.txt",
            true,
            || Ok(())
        )
        .is_err());
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_read",
            "workspace/saved-task/result.txt",
            false,
            || Ok(())
        )
        .is_ok());
}

#[test]
fn agent_task_scope_new_native_goals_get_stable_exclusive_default_directories() {
    let (_dir, db) = database();
    db.init_agent_isolation_tables().unwrap();
    let grant = db.create_isolated_agent("user", "project", None).unwrap();
    let run = db.authenticate_agent_run(&grant.token).unwrap();
    let context = db.ensure_agent_host_context(&run, None).unwrap();
    let owner = run.host_goal_owner_key();
    let first = db
        .create_host_goal_at_in_context(
            &owner,
            "project",
            "/private",
            "first",
            "one",
            NOW,
            Some(&context),
        )
        .unwrap();
    let second = db
        .create_host_goal_at_in_context(
            &owner,
            "project",
            "/private",
            "second",
            "two",
            NOW,
            Some(&context),
        )
        .unwrap();
    assert_eq!(
        first.goal.task_directory,
        Some(format!("workspace/{}", first.goal.goal_id))
    );
    assert_ne!(first.goal.task_directory, second.goal.task_directory);
    assert_eq!(
        db.create_host_goal_at_in_context(
            &owner,
            "project",
            "/private",
            "first",
            "one",
            NOW,
            Some(&context)
        )
        .unwrap()
        .goal,
        first.goal
    );
    let explicit = db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "/private",
            "workspace",
            "explicit",
            "three",
            NOW,
            Some(&context),
        )
        .unwrap();
    assert_eq!(explicit.goal.task_directory.as_deref(), Some("workspace"));
    assert!(db.with_agent_run(&run, "private_read", || Ok(())).is_ok());
}

#[test]
fn agent_task_scope_rejects_legacy_broad_or_overlapping_directories_without_binding_pending_run() {
    for (target, other, allowed) in [
        (None, None, false),
        (Some("workspace"), None, false),
        (Some("workspace/task"), Some("workspace/task"), false),
        (Some("workspace/task"), Some("workspace/task/sub"), false),
        (Some("workspace/task/sub"), Some("workspace/task"), false),
        (Some("workspace/task"), Some("workspace/TASK"), false),
        (Some("workspace/task"), Some("workspace/task-other"), true),
    ] {
        let (_dir, db) = database();
        db.init_agent_isolation_tables().unwrap();
        let grant = db.create_isolated_agent("user", "project", None).unwrap();
        let source = db.authenticate_agent_run(&grant.token).unwrap();
        let owner = source.host_goal_owner_key();
        let goal = match target {
            Some(path) => {
                db.create_host_goal_in_task_directory_in_context(
                    &owner, "project", "/private", path, "target", "target", NOW, None,
                )
                .unwrap()
                .goal
            }
            None => {
                db.create_host_goal_at(&owner, "project", "/private", "target", "target", NOW)
                    .unwrap()
                    .goal
            }
        };
        if let Some(path) = other {
            db.create_host_goal_in_task_directory_in_context(
                &owner, "project", "/private", path, "other", "other", NOW, None,
            )
            .unwrap();
        }
        let pending = db.begin_agent_run_handoff(&source).unwrap();
        let mut conn = db.conn_for_tests();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let result = crate::agent_isolation::bind_pending_task_goal_on_connection(
            &tx,
            &source,
            &pending.run_id,
            &goal.goal_id,
        );
        assert_eq!(
            result.is_ok(),
            allowed,
            "target={target:?}, other={other:?}"
        );
        let scope: Option<String> = tx
            .query_row(
                "SELECT task_goal_id FROM wc_agent_runs WHERE run_id=?1",
                [&pending.run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scope, allowed.then_some(goal.goal_id));
    }
}

#[test]
fn agent_task_scope_filesystem_effect_holds_the_same_lock_as_run_revocation() {
    let s = scoped();
    claim(&s);
    let second = Database::open(&s.dir.path().join("state.db")).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (revoked_tx, revoked_rx) = std::sync::mpsc::channel();
    let run = s.run.clone();
    let other_run = run.clone();
    let database = &s.db;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            database.with_agent_run_for_path(
                &run,
                "private_write",
                "workspace/saved-task/output.txt",
                true,
                || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
            )
        });
        entered_rx.recv().unwrap();
        let revoker = scope.spawn(move || {
            second
                .revoke_agent_run(
                    other_run.owner_user(),
                    other_run.project(),
                    other_run.agent_id(),
                    other_run.run_id(),
                )
                .unwrap();
            revoked_tx.send(()).unwrap();
        });
        assert!(revoked_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
        release_tx.send(()).unwrap();
        writer.join().unwrap().unwrap();
        revoker.join().unwrap();
        revoked_rx.recv().unwrap();
    });
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "workspace/saved-task/output.txt",
            true,
            || Ok(())
        )
        .is_err());
}

#[test]
fn agent_task_scope_trusted_recovery_preserves_scope_over_a_newer_unscoped_pending_run() {
    let s = scoped();
    let original_claim = claim(&s);
    let pending = s.db.begin_agent_run_handoff(&s.run).unwrap();
    let scope: Option<String> =
        s.db.conn_for_tests()
            .query_row(
                "SELECT task_goal_id FROM wc_agent_runs WHERE run_id=?1",
                [&pending.run_id],
                |r| r.get(0),
            )
            .unwrap();
    assert!(scope.is_none());
    let recovered =
        s.db.recover_isolated_agent(
            s.run.owner_user(),
            s.run.project(),
            s.run.agent_id(),
            pending.generation,
        )
        .unwrap();
    let run = s.db.authenticate_agent_run(&recovered.token).unwrap();
    let context = s.db.ensure_agent_host_context(&run, None).unwrap();
    let goal =
        s.db.read_host_goal_for_agent_run(&run, &s.goal.goal_id)
            .unwrap()
            .0;
    assert_eq!(goal.phase, HostGoalPhase::Ready);
    assert!(goal.fence > original_claim.goal.fence);
    assert!(goal.current_attempt_id.is_none());
    assert_eq!(
        s.db.get_host_goal_attempt(
            &s.owner,
            &s.goal.goal_id,
            &original_claim.attempt.binding.attempt_id
        )
        .unwrap()
        .unwrap()
        .status,
        HostGoalAttemptStatus::Interrupted
    );
    let h =
        s.db.read_host_goal_handoff(&s.owner, &s.goal.goal_id)
            .unwrap()
            .unwrap();
    assert_eq!(h.successor_agent_run_id.as_deref(), Some(run.run_id()));
    assert_eq!(h.execution_owner_epoch, run.run_id());
    assert!(s
        .db
        .with_agent_run_for_path(
            &run,
            "private_write",
            "workspace/saved-task/output.txt",
            true,
            || Ok(())
        )
        .is_err());
    assert_eq!(
        s.db.claim_host_goal_node_for_plan_in_context(
            &s.owner,
            &goal.goal_id,
            goal.revision,
            "research",
            false,
            goal.plan_version,
            None,
            "recovered-claim",
            NOW + 9,
            Some(&context)
        )
        .unwrap_err()
        .code(),
        "goal_context_read_required"
    );
    s.db.ack_agent_host_goal_context_read(
        &run,
        &goal.goal_id,
        goal.revision,
        goal.plan_version,
        "research",
    )
    .unwrap();
    s.db.claim_host_goal_node_for_plan_in_context(
        &s.owner,
        &goal.goal_id,
        goal.revision,
        "research",
        false,
        goal.plan_version,
        None,
        "recovered-claim",
        NOW + 9,
        Some(&context),
    )
    .unwrap();
    let output = s.dir.path().join("recovered-output.txt");
    s.db.with_agent_run_for_path(
        &run,
        "private_write",
        "workspace/saved-task/output.txt",
        true,
        || {
            std::fs::write(&output, "recovered task")
                .map_err(|_| crate::AgentIsolationError::new("test_io"))
        },
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(output).unwrap(), "recovered task");
    assert_eq!(
        s.db.agent_run_task_goal(&run).unwrap().as_deref(),
        Some(s.goal.goal_id.as_str())
    );
    assert!(s.db.validate_agent_run(&s.run).is_err());
    assert!(s.db.authenticate_agent_run(&pending.token).is_err());
    assert!(s
        .db
        .with_agent_run_for_path(
            &run,
            "private_read",
            "workspace/saved-task/output.txt",
            false,
            || Ok(())
        )
        .is_ok());
    assert!(s
        .db
        .with_agent_run_for_path(
            &run,
            "private_read",
            "memory/private.json",
            false,
            || Ok(())
        )
        .is_ok());
    assert!(s
        .db
        .read_host_goal_for_agent_run(&run, &s.other.goal_id)
        .is_err());
    let db = Database::open(&s.dir.path().join("state.db")).unwrap();
    assert_eq!(
        db.agent_run_task_goal(&run).unwrap().as_deref(),
        Some(s.goal.goal_id.as_str())
    );
    assert!(db.with_agent_run(&run, "private_read", || Ok(())).is_err());
    assert!(db
        .with_agent_run_for_path(
            &run,
            "private_write",
            "workspace/saved-task/output.txt",
            true,
            || Ok(())
        )
        .is_ok());
}

#[test]
fn agent_task_scope_recovery_rejects_unfinished_handoff_wrong_predecessor_and_active_work_atomically(
) {
    for status in ["awaiting_ready", "failed"] {
        let s = scoped();
        s.db.conn_for_tests().execute("UPDATE wc_host_goal_handoffs SET snapshot_json=json_set(snapshot_json,'$.status',?2,'$.attempt_count',4) WHERE goal_id=?1",rusqlite::params![s.goal.goal_id,status]).unwrap();
        let before =
            s.db.read_host_goal_handoff(&s.owner, &s.goal.goal_id)
                .unwrap()
                .unwrap();
        let error =
            s.db.recover_isolated_agent(
                s.run.owner_user(),
                s.run.project(),
                s.run.agent_id(),
                s.run.generation(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "agent_handoff_control_required");
        assert!(s.db.validate_agent_run(&s.run).is_ok());
        assert_eq!(
            s.db.read_host_goal_handoff(&s.owner, &s.goal.goal_id)
                .unwrap()
                .unwrap(),
            before
        );
        assert_eq!(
            s.db.isolated_agent_generation(s.run.owner_user(), s.run.project(), s.run.agent_id())
                .unwrap(),
            s.run.generation()
        );
    }
    let s = scoped();
    s.db.conn_for_tests().execute("UPDATE wc_host_goal_handoffs SET snapshot_json=json_set(snapshot_json,'$.successor_agent_run_id',?2) WHERE goal_id=?1",rusqlite::params![s.goal.goal_id,s.old.run_id()]).unwrap();
    assert_eq!(
        s.db.recover_isolated_agent(
            s.run.owner_user(),
            s.run.project(),
            s.run.agent_id(),
            s.run.generation()
        )
        .unwrap_err()
        .code(),
        "goal_handoff_run_mismatch"
    );
    assert!(s.db.validate_agent_run(&s.run).is_ok());
    let s = scoped();
    let current = claim(&s);
    s.db.begin_host_goal_operation_with_manifest_in_context(
        &s.owner,
        &current.attempt.binding,
        "pending",
        "write_file",
        "payload",
        &[],
        NOW,
        Some(&s.context),
        false,
    )
    .unwrap();
    assert_eq!(
        s.db.recover_isolated_agent(
            s.run.owner_user(),
            s.run.project(),
            s.run.agent_id(),
            s.run.generation()
        )
        .unwrap_err()
        .code(),
        "goal_handoff_execution_active"
    );
    assert!(s.db.validate_agent_run(&s.run).is_ok());
    assert_eq!(
        s.db.get_host_goal(&s.owner, &s.goal.goal_id)
            .unwrap()
            .unwrap()
            .revision,
        current.goal.revision
    );
}

#[test]
fn agent_task_scope_recovery_goal_write_failure_rolls_back_run_scope_and_fence() {
    let s = scoped();
    let current = claim(&s);
    let before =
        s.db.read_host_goal_handoff(&s.owner, &s.goal.goal_id)
            .unwrap()
            .unwrap();
    s.db.conn_for_tests().execute_batch("CREATE TRIGGER reject_recovery_handoff BEFORE UPDATE ON wc_host_goal_handoffs BEGIN SELECT RAISE(ABORT,'injected recovery failure'); END;").unwrap();
    assert!(s
        .db
        .recover_isolated_agent(
            s.run.owner_user(),
            s.run.project(),
            s.run.agent_id(),
            s.run.generation()
        )
        .is_err());
    assert!(s.db.validate_agent_run(&s.run).is_ok());
    assert_eq!(
        s.db.get_host_goal(&s.owner, &s.goal.goal_id)
            .unwrap()
            .unwrap(),
        current.goal
    );
    assert_eq!(
        s.db.get_host_goal_attempt(
            &s.owner,
            &s.goal.goal_id,
            &current.attempt.binding.attempt_id
        )
        .unwrap()
        .unwrap(),
        current.attempt
    );
    assert_eq!(
        s.db.read_host_goal_handoff(&s.owner, &s.goal.goal_id)
            .unwrap()
            .unwrap(),
        before
    );
    assert_eq!(
        s.db.agent_run_task_goal(&s.run).unwrap().as_deref(),
        Some(s.goal.goal_id.as_str())
    );
}

#[test]
fn agent_task_scope_unscoped_explicit_goal_binding_requires_claim_for_workspace_writes() {
    for legacy_root in [false, true] {
        let (_dir, db) = database();
        db.init_agent_isolation_tables().unwrap();
        let grant = db.create_isolated_agent("user", "project", None).unwrap();
        let run = db.authenticate_agent_run(&grant.token).unwrap();
        let owner = run.host_goal_owner_key();
        let context = db.ensure_agent_host_context(&run, None).unwrap();
        assert!(db
            .with_agent_run_for_path(
                &run,
                "private_write",
                "workspace/input.txt",
                true,
                || Ok(())
            )
            .is_ok());
        let goal = if legacy_root {
            db.create_host_goal_in_task_directory_in_context(
                &owner,
                "project",
                "/private",
                "workspace",
                "goal",
                "create",
                NOW,
                Some(&context),
            )
            .unwrap()
            .goal
        } else {
            db.create_host_goal_at_in_context(
                &owner,
                "project",
                "/private",
                "goal",
                "create",
                NOW,
                Some(&context),
            )
            .unwrap()
            .goal
        };
        db.bind_host_goal_window_in_context(
            &owner,
            run.run_id(),
            &crate::HostGoalWindowBinding {
                goal_id: goal.goal_id.clone(),
                project: goal.project.clone(),
                project_root: goal.project_root.clone(),
                context_epoch: run.run_id().into(),
            },
            Some(&context),
        )
        .unwrap();
        let output = format!("{}/output.txt", goal.task_directory.as_deref().unwrap());
        assert!(db
            .with_agent_run_for_path(&run, "private_write", &output, true, || Ok(()))
            .is_err());
        assert!(db
            .with_agent_run_for_path(&run, "private_write", "memory/notes.txt", true, || Ok(()))
            .is_ok());
        let saved = db
            .save_host_goal_plan_in_context(
                &owner,
                &goal.goal_id,
                goal.revision,
                &plan(),
                None,
                "plan",
                NOW,
                Some(&context),
            )
            .unwrap();
        let receipt = HostGoalMaterialization {
            json_path: "goals/plan.json".into(),
            json_hash: "a".repeat(64),
            markdown_path: "goals/plan.md".into(),
            markdown_hash: "b".repeat(64),
            verified_at: NOW,
        };
        let published = db
            .materialize_host_goal_plan_in_context(
                &owner,
                &goal.goal_id,
                1,
                &saved.plan.hash,
                &receipt,
                NOW,
                Some(&context),
            )
            .unwrap();
        assert!(db
            .with_agent_run_for_path(&run, "private_write", &output, true, || Ok(()))
            .is_err());
        let confirmed = db
            .confirm_host_goal_plan_in_context(
                &owner,
                &goal.goal_id,
                published.goal.revision,
                1,
                &saved.plan.hash,
                "Start",
                "confirm",
                NOW,
                Some(&context),
            )
            .unwrap()
            .goal;
        assert!(db
            .with_agent_run_for_path(&run, "private_write", &output, true, || Ok(()))
            .is_err());
        db.ack_agent_host_goal_context_read(&run, &goal.goal_id, confirmed.revision, 1, "research")
            .unwrap();
        let claimed = db
            .claim_host_goal_node_for_plan_in_context(
                &owner,
                &goal.goal_id,
                confirmed.revision,
                "research",
                false,
                1,
                None,
                "claim",
                NOW,
                Some(&context),
            )
            .unwrap();
        assert!(db
            .with_agent_run_for_path(&run, "private_write", &output, true, || Ok(()))
            .is_ok());
        db.conn_for_tests().execute("UPDATE wc_host_goal_windows SET binding_json=json_set(binding_json,'$.project','wrong-project') WHERE owner_key=?1 AND window_key=?2",rusqlite::params![owner,run.run_id()]).unwrap();
        assert!(db
            .with_agent_run_for_path(&run, "private_write", &output, true, || Ok(()))
            .is_err());
        db.conn_for_tests().execute("UPDATE wc_host_goal_windows SET binding_json=json_set(binding_json,'$.project','project') WHERE owner_key=?1 AND window_key=?2",rusqlite::params![owner,run.run_id()]).unwrap();
        if !legacy_root {
            assert!(db
                .with_agent_run_for_path(
                    &run,
                    "private_write",
                    "workspace/outside.txt",
                    true,
                    || Ok(())
                )
                .is_err());
        }
        let paused = db
            .checkpoint_host_goal_with_request_in_context(
                &owner,
                &goal.goal_id,
                claimed.goal.revision,
                Some(&claimed.attempt.binding),
                &claimed.goal.state,
                HostGoalCheckpointAction::Pause,
                "pause",
                "pause",
                "pause",
                &[],
                NOW,
                Some(&context),
                &[],
            )
            .unwrap()
            .goal;
        assert!(db
            .with_agent_run_for_path(&run, "private_write", &output, true, || Ok(()))
            .is_err());
        db.checkpoint_host_goal_with_request_in_context(
            &owner,
            &goal.goal_id,
            paused.revision,
            None,
            &paused.state,
            HostGoalCheckpointAction::Exit,
            "exit",
            "exit",
            "exit",
            &[],
            NOW,
            Some(&context),
            &[],
        )
        .unwrap();
        assert!(db.host_goal_window(&owner, run.run_id()).unwrap().is_none());
        assert_eq!(db.agent_run_task_goal(&run).unwrap(), None);
        assert!(db
            .with_agent_run_for_path(&run, "private_write", "workspace/outside.txt", true, || Ok(
                ()
            ))
            .is_ok());
    }
}

#[test]
fn agent_task_scope_own_memory_permission_keeps_active_run_and_lifecycle_fences() {
    let s = scoped();
    // Memory is explicitly retained while task business still requires a claim.
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "memory/notes.json",
            true,
            || Ok(())
        )
        .is_ok());
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "workspace/saved-task/result.txt",
            true,
            || Ok(())
        )
        .is_err());
    assert!(s
        .db
        .with_agent_run_for_path(&s.run, "private_read", "memory", false, || Ok(()))
        .is_ok());
    assert!(s
        .db
        .with_agent_run_for_path(&s.run, "private_write", "memory", true, || Ok(()))
        .is_err());
    for write in [false, true] {
        assert!(s
            .db
            .with_agent_run_for_path(
                &s.old,
                if write {
                    "private_write"
                } else {
                    "private_read"
                },
                "memory/notes.json",
                write,
                || Ok(())
            )
            .is_err());
    }
    s.db.checkpoint_host_goal_with_request_in_context(
        &s.owner,
        &s.goal.goal_id,
        s.goal.revision,
        None,
        &s.goal.state,
        HostGoalCheckpointAction::Pause,
        "pause task",
        "pause-memory",
        "pause-memory",
        &[],
        NOW,
        Some(&s.context),
        &[],
    )
    .unwrap();
    assert!(s
        .db
        .with_agent_run_for_path(
            &s.run,
            "private_write",
            "memory/notes.json",
            true,
            || Ok(())
        )
        .is_ok());
    s.db.conn_for_tests()
        .execute(
            "UPDATE wc_host_context_states SET state=2 WHERE owner_key=?1 AND context_epoch=?2",
            rusqlite::params![s.owner, s.run.run_id()],
        )
        .unwrap();
    assert_eq!(
        s.db.with_agent_run_for_path(
            &s.run,
            "private_write",
            "memory/notes.json",
            true,
            || Ok(())
        )
        .unwrap_err()
        .code(),
        "goal_context_draining"
    );
}

#[test]
fn agent_task_scope_generic_activation_cannot_split_a_prepared_task_run_from_goal_ownership() {
    // This fixture obtains its scoped run through a real store ready proof and
    // composed commit; the internal activation path remains available there.
    let s = scoped();
    let before =
        s.db.read_host_goal_handoff(&s.owner, &s.goal.goal_id)
            .unwrap()
            .unwrap();
    assert_eq!(before.status, "committed");
    assert_eq!(
        before.successor_agent_run_id.as_deref(),
        Some(s.run.run_id())
    );
    let pending = s.db.begin_agent_run_handoff(&s.run).unwrap();
    {
        let mut conn = s.db.conn_for_tests();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        crate::agent_isolation::bind_pending_task_goal_on_connection(
            &tx,
            &s.run,
            &pending.run_id,
            &s.goal.goal_id,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let pending_scope: Option<String> =
        s.db.conn_for_tests()
            .query_row(
                "SELECT task_goal_id FROM wc_agent_runs WHERE run_id=?1",
                [&pending.run_id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(pending_scope.as_deref(), Some(s.goal.goal_id.as_str()));
    assert_eq!(
        s.db.activate_agent_run(
            s.run.owner_user(),
            s.run.project(),
            s.run.agent_id(),
            &pending.run_id
        )
        .unwrap_err()
        .code(),
        "agent_task_scope_denied"
    );
    assert!(s.db.validate_agent_run(&s.run).is_ok());
    assert!(s.db.authenticate_agent_run(&pending.token).is_err());
    assert_eq!(
        s.db.read_host_goal_handoff(&s.owner, &s.goal.goal_id)
            .unwrap()
            .unwrap(),
        before
    );
}

#[test]
fn agent_task_scope_legacy_omitted_directory_replays_original_workspace_receipt() {
    let (dir, db) = database();
    db.init_agent_isolation_tables().unwrap();
    let grant = db.create_isolated_agent("user", "project", None).unwrap();
    let run = db.authenticate_agent_run(&grant.token).unwrap();
    let context = db.ensure_agent_host_context(&run, None).unwrap();
    let owner = run.host_goal_owner_key();
    let original = db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "/private",
            "workspace",
            "legacy objective",
            "same-wire-request",
            NOW,
            Some(&context),
        )
        .unwrap();
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let replay = db
        .create_host_goal_at_in_context(
            &owner,
            "project",
            "/private",
            "legacy objective",
            "same-wire-request",
            NOW + 1,
            Some(&context),
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.goal, original.goal);
    assert_eq!(replay.goal.task_directory.as_deref(), Some("workspace"));
    assert_eq!(
        db.list_host_goals(&owner, Some("project"), 10, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.create_host_goal_at_in_context(
            &owner,
            "project",
            "/private",
            "different objective",
            "same-wire-request",
            NOW + 2,
            Some(&context)
        )
        .unwrap_err()
        .code(),
        "goal_idempotency_conflict"
    );
    assert_eq!(db.agent_run_task_goal(&run).unwrap(), None);
    let s = scoped();
    assert_eq!(
        s.db.create_host_goal_at_in_context(
            &s.owner,
            "project",
            "/private",
            "original task",
            "task",
            NOW + 2,
            Some(&s.context)
        )
        .unwrap_err()
        .code(),
        "agent_task_scope_denied"
    );
    assert_eq!(
        s.db.agent_run_task_goal(&s.run).unwrap().as_deref(),
        Some(s.goal.goal_id.as_str())
    );
}
