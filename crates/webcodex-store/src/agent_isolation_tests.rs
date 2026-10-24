use crate::Database;

fn database() -> (tempfile::TempDir, Database) {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("agent-isolation.sqlite3")).unwrap();
    db.init_agent_isolation_tables().unwrap();
    (temp, db)
}

#[test]
fn isolated_agent_authentication_is_server_bound_and_persists_after_reopen() {
    let (temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let binding = db.authenticate_agent_run(&grant.token).unwrap();
    assert_eq!(binding.agent_id(), grant.agent.agent_id);
    assert_eq!(binding.run_id(), grant.run_id);
    assert_eq!(binding.project(), "project-a");
    assert_eq!(binding.owner_user(), "owner-a");
    assert_eq!(binding.generation(), 1);
    assert_eq!(binding.expires_at_unix_ms(), grant.expires_at_unix_ms);
    db.validate_agent_run(&binding).unwrap();
    drop(db);
    let reopened = Database::open(&temp.path().join("agent-isolation.sqlite3")).unwrap();
    reopened.init_agent_isolation_tables().unwrap();
    assert_eq!(
        reopened.authenticate_agent_run(&grant.token).unwrap(),
        binding
    );
    reopened.validate_agent_run(&binding).unwrap();
}

#[test]
fn agent_run_secrets_are_hashed_and_redacted_and_invalid_tokens_fail_closed() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    assert!(grant.token.starts_with("wc_ar_"));
    assert_eq!(grant.token.len(), 70);
    assert!(!format!("{grant:?}").contains(&grant.token));
    let stored: String = db
        .conn_for_tests()
        .query_row(
            "SELECT token_hash FROM wc_agent_runs WHERE run_id=?1",
            [&grant.run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(stored, grant.token);
    assert_eq!(stored.len(), 64);
    for token in [
        "".to_string(),
        "wc_ar_forged".into(),
        format!("wc_ar_{}", "0".repeat(64)),
    ] {
        assert_eq!(
            db.authenticate_agent_run(&token).unwrap_err().code(),
            "agent_run_invalid"
        );
    }
    let columns: Vec<String> = db
        .conn_for_tests()
        .prepare("PRAGMA table_info(wc_agent_isolation_audit)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        columns,
        [
            "event_id",
            "agent_id",
            "run_id",
            "action",
            "code",
            "created_at_unix_ms"
        ]
    );
}

#[test]
fn pending_run_requires_management_activation_and_old_run_is_atomically_revoked() {
    let (_temp, db) = database();
    let original = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let old = db.authenticate_agent_run(&original.token).unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    assert_eq!(pending.agent.agent_id, old.agent_id());
    assert_eq!(pending.generation, 2);
    assert_eq!(
        db.authenticate_agent_run(&pending.token)
            .unwrap_err()
            .code(),
        "agent_run_invalid"
    );
    db.validate_agent_run(&old).unwrap();
    let activated = db
        .activate_agent_run("owner-a", "project-a", old.agent_id(), &pending.run_id)
        .unwrap();
    assert_eq!(
        activated,
        db.authenticate_agent_run(&pending.token).unwrap()
    );
    assert_eq!(
        db.authenticate_agent_run(&original.token)
            .unwrap_err()
            .code(),
        "agent_run_invalid"
    );
    assert_eq!(
        db.validate_agent_run(&old).unwrap_err().code(),
        "agent_run_invalid"
    );
    assert_eq!(
        db.begin_agent_run_handoff(&old).unwrap_err().code(),
        "agent_run_invalid"
    );
    let counts: (i64, i64) = db.conn_for_tests().query_row(
        "SELECT COUNT(*) FILTER(WHERE state='active'), COUNT(*) FILTER(WHERE state='revoked') FROM wc_agent_runs WHERE agent_id=?1",
        [old.agent_id()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(counts, (1, 1));
}

#[test]
fn new_pending_request_replaces_only_unactivated_secret_and_never_restores_old_run() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let old = db.authenticate_agent_run(&grant.token).unwrap();
    let abandoned = db.begin_agent_run_handoff(&old).unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    assert_eq!(pending.generation, 3);
    assert_eq!(
        db.activate_agent_run("owner-a", "project-a", old.agent_id(), &abandoned.run_id)
            .unwrap_err()
            .code(),
        "agent_run_invalid"
    );
    db.validate_agent_run(&old).unwrap();
    let active = db
        .activate_agent_run("owner-a", "project-a", old.agent_id(), &pending.run_id)
        .unwrap();
    db.revoke_agent_run("owner-a", "project-a", active.agent_id(), active.run_id())
        .unwrap();
    assert!(db.authenticate_agent_run(&pending.token).is_err());
    assert!(db.authenticate_agent_run(&grant.token).is_err());
    assert!(db.validate_agent_run(&active).is_err());
}

#[test]
fn different_agents_and_parent_links_never_transfer_run_authority() {
    let (_temp, db) = database();
    let parent = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let child = db
        .create_isolated_agent("owner-a", "project-a", Some(&parent.agent.agent_id))
        .unwrap();
    let parent_binding = db.authenticate_agent_run(&parent.token).unwrap();
    let child_binding = db.authenticate_agent_run(&child.token).unwrap();
    assert_ne!(parent_binding.agent_id(), child_binding.agent_id());
    assert_eq!(
        child.agent.parent_agent_id.as_deref(),
        Some(parent_binding.agent_id())
    );
    let pending_child = db.begin_agent_run_handoff(&child_binding).unwrap();
    assert_eq!(
        db.activate_agent_run(
            "owner-a",
            "project-a",
            parent_binding.agent_id(),
            &pending_child.run_id
        )
        .unwrap_err()
        .code(),
        "agent_run_invalid"
    );
    assert_eq!(
        db.revoke_agent_run(
            "owner-a",
            "project-a",
            parent_binding.agent_id(),
            child_binding.run_id()
        )
        .unwrap_err()
        .code(),
        "agent_run_invalid"
    );
    db.validate_agent_run(&parent_binding).unwrap();
    db.validate_agent_run(&child_binding).unwrap();
    db.activate_agent_run(
        "owner-a",
        "project-a",
        child_binding.agent_id(),
        &pending_child.run_id,
    )
    .unwrap();
    db.validate_agent_run(&parent_binding).unwrap();
    assert!(db.validate_agent_run(&child_binding).is_err());
    assert_eq!(
        db.create_isolated_agent("owner-a", "project-b", Some(parent_binding.agent_id()))
            .unwrap_err()
            .code(),
        "isolated_agent_not_found"
    );
    assert_eq!(
        db.create_isolated_agent("owner-b", "project-a", Some(parent_binding.agent_id()))
            .unwrap_err()
            .code(),
        "isolated_agent_not_found"
    );
}

#[test]
fn management_is_exact_owner_and_project_scoped_and_hides_foreign_agents() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let binding = db.authenticate_agent_run(&grant.token).unwrap();
    let pending = db.begin_agent_run_handoff(&binding).unwrap();
    let foreign = db
        .activate_agent_run("owner-b", "project-a", binding.agent_id(), &pending.run_id)
        .unwrap_err();
    let missing = db
        .activate_agent_run("owner-b", "project-a", "wc_iagent_missing", &pending.run_id)
        .unwrap_err();
    assert_eq!(foreign, missing);
    assert_eq!(
        db.activate_agent_run("owner-a", "project-b", binding.agent_id(), &pending.run_id)
            .unwrap_err(),
        missing
    );
    db.validate_agent_run(&binding).unwrap();
    assert_eq!(
        db.revoke_agent_run("owner-b", "project-a", binding.agent_id(), binding.run_id())
            .unwrap_err(),
        missing
    );
    db.validate_agent_run(&binding).unwrap();
}

#[test]
fn expiry_is_rechecked_for_existing_bindings_and_pending_activation() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let binding = db.authenticate_agent_run(&grant.token).unwrap();
    let pending = db.begin_agent_run_handoff(&binding).unwrap();
    db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET expires_at_unix_ms=0 WHERE agent_id=?1",
            [binding.agent_id()],
        )
        .unwrap();
    assert_eq!(
        db.validate_agent_run(&binding).unwrap_err().code(),
        "agent_run_invalid"
    );
    assert_eq!(
        db.authenticate_agent_run(&grant.token).unwrap_err().code(),
        "agent_run_invalid"
    );
    assert_eq!(
        db.activate_agent_run("owner-a", "project-a", binding.agent_id(), &pending.run_id)
            .unwrap_err()
            .code(),
        "agent_run_invalid"
    );
}

#[test]
fn broker_operation_never_executes_for_revoked_run_and_audit_omits_content() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let binding = db.authenticate_agent_run(&grant.token).unwrap();
    let value = db
        .with_agent_run(&binding, "private_read", || Ok("private-body-fixture"))
        .unwrap();
    assert_eq!(value, "private-body-fixture");
    assert!(db.agent_private_root().is_absolute());
    db.revoke_agent_run("owner-a", "project-a", binding.agent_id(), binding.run_id())
        .unwrap();
    let invoked = std::cell::Cell::new(false);
    assert_eq!(
        db.with_agent_run(&binding, "private_write", || {
            invoked.set(true);
            Ok(())
        })
        .unwrap_err()
        .code(),
        "agent_run_invalid"
    );
    assert!(!invoked.get());
    let events: String = db
        .conn_for_tests()
        .query_row(
            "SELECT group_concat(action || ':' || code) FROM wc_agent_isolation_audit",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(events.contains("private_read:admitted"));
    assert!(events.contains("private_read:ok"));
    assert!(!events.contains("private-body-fixture"));
    assert!(!events.contains("private_write"));
}

#[test]
fn composed_run_activation_rolls_back_with_enclosing_transition() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let old = db.authenticate_agent_run(&grant.token).unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    {
        let mut conn = db.conn_for_tests();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let new = crate::agent_isolation::activate_agent_run_on_connection(
            &tx,
            "owner-a",
            "project-a",
            old.agent_id(),
            &pending.run_id,
        )
        .unwrap();
        crate::agent_isolation::validate_agent_run_on_connection(&tx, &new).unwrap();
        assert!(crate::agent_isolation::validate_agent_run_on_connection(&tx, &old).is_err());
        tx.rollback().unwrap();
    }
    db.validate_agent_run(&old).unwrap();
    assert!(db.authenticate_agent_run(&pending.token).is_err());
    assert_eq!(db.authenticate_agent_run(&grant.token).unwrap(), old);
}

#[test]
fn cross_connection_activation_waits_for_old_write_and_successor_uses_same_space() {
    assert_cross_connection_single_writer(false);
}

#[test]
fn cross_connection_recovery_waits_for_old_write_and_successor_uses_same_space() {
    assert_cross_connection_single_writer(true);
}

fn assert_cross_connection_single_writer(recover: bool) {
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    let (temp, db) = database();
    let db = Arc::new(db);
    let management = Database::open(&temp.path().join("agent-isolation.sqlite3")).unwrap();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let old = db.authenticate_agent_run(&grant.token).unwrap();
    db.ensure_agent_host_context(&old, None).unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    let directory = db.agent_private_root().join(old.agent_id());
    std::fs::create_dir_all(&directory).unwrap();
    let artifact = directory.join("state.txt");
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writing_db = db.clone();
    let writing_binding = old.clone();
    let writing_path = artifact.clone();
    let writer = std::thread::spawn(move || {
        writing_db
            .with_agent_run(&writing_binding, "private_write", || {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                std::fs::write(writing_path, "completed-by-old-run").unwrap();
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (attempted_tx, attempted_rx) = mpsc::channel();
    let (activated_tx, activated_rx) = mpsc::channel();
    let agent_id = old.agent_id().to_owned();
    let activator = std::thread::spawn(move || {
        attempted_tx.send(()).unwrap();
        let next = if recover {
            management
                .recover_isolated_agent("owner-a", "project-a", &agent_id, pending.generation)
                .and_then(|grant| management.authenticate_agent_run(&grant.token))
        } else {
            management.activate_agent_run("owner-a", "project-a", &agent_id, &pending.run_id)
        };
        activated_tx.send(next).unwrap();
    });
    attempted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        activated_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    let next = activated_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    activator.join().unwrap();
    assert_eq!(old.agent_id(), next.agent_id());
    assert_ne!(old.run_id(), next.run_id());
    let old_called = std::cell::Cell::new(false);
    assert!(db
        .with_agent_run(&old, "private_write", || {
            old_called.set(true);
            Ok(())
        })
        .is_err());
    assert!(!old_called.get());
    let body = db
        .with_agent_run(&next, "private_read", || {
            Ok(std::fs::read_to_string(&artifact).unwrap())
        })
        .unwrap();
    assert_eq!(body, "completed-by-old-run");
}

#[test]
fn pending_binding_is_metadata_only_and_exact_predecessor_is_required() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let other = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let old = db.authenticate_agent_run(&grant.token).unwrap();
    let other_binding = db.authenticate_agent_run(&other.token).unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    let pending_binding = {
        let conn = db.conn_for_tests();
        assert!(crate::agent_isolation::pending_agent_run_on_connection(
            &conn,
            &other_binding,
            &pending.run_id
        )
        .is_err());
        crate::agent_isolation::pending_agent_run_on_connection(&conn, &old, &pending.run_id)
            .unwrap()
    };
    let called = std::cell::Cell::new(false);
    assert!(db
        .with_agent_run(&pending_binding, "private_write", || {
            called.set(true);
            Ok(())
        })
        .is_err());
    assert!(!called.get());
    db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET previous_run_id=?1 WHERE run_id=?2",
            rusqlite::params![other_binding.run_id(), pending.run_id],
        )
        .unwrap();
    assert!(db
        .activate_agent_run("owner-a", "project-a", old.agent_id(), &pending.run_id)
        .is_err());
    db.validate_agent_run(&old).unwrap();
}

#[test]
fn audit_failure_after_filesystem_effect_is_reported_as_unknown_outcome() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let binding = db.authenticate_agent_run(&grant.token).unwrap();
    db.ensure_agent_host_context(&binding, None).unwrap();
    db.conn_for_tests().execute_batch("CREATE TRIGGER fail_broker_result_audit BEFORE INSERT ON wc_agent_isolation_audit
        WHEN NEW.action='private_write' AND NEW.code='ok' BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END;").unwrap();
    let effect = std::cell::Cell::new(false);
    let error = db
        .with_agent_run(&binding, "private_write", || {
            effect.set(true);
            Ok(())
        })
        .unwrap_err();
    assert!(effect.get());
    assert_eq!(error.code(), "agent_private_operation_outcome_unknown");
}

#[test]
fn management_recovery_after_reopen_preserves_agent_namespace_and_revokes_all_old_runs() {
    let (temp, db) = database();
    let parent = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", Some(&parent.agent.agent_id))
        .unwrap();
    let old = db.authenticate_agent_run(&grant.token).unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    let root = db.agent_private_root().join(old.agent_id());
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("memory.txt"), "persisted-private-fixture").unwrap();
    db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET expires_at_unix_ms=0 WHERE agent_id=?1",
            [old.agent_id()],
        )
        .unwrap();
    drop(db);
    let reopened = Database::open(&temp.path().join("agent-isolation.sqlite3")).unwrap();
    let recovered = reopened
        .recover_isolated_agent("owner-a", "project-a", old.agent_id(), pending.generation)
        .unwrap();
    let new = reopened.authenticate_agent_run(&recovered.token).unwrap();
    assert_eq!(new.agent_id(), old.agent_id());
    assert_eq!(new.project(), old.project());
    assert_eq!(new.owner_user(), old.owner_user());
    assert_eq!(new.host_goal_owner_key(), old.host_goal_owner_key());
    assert_eq!(recovered.agent.parent_agent_id, grant.agent.parent_agent_id);
    assert_eq!(new.generation(), 3);
    assert_eq!(reopened.agent_private_root().join(new.agent_id()), root);
    assert_eq!(
        std::fs::read_to_string(root.join("memory.txt")).unwrap(),
        "persisted-private-fixture"
    );
    assert!(reopened.authenticate_agent_run(&grant.token).is_err());
    assert!(reopened.authenticate_agent_run(&pending.token).is_err());
    assert!(reopened.validate_agent_run(&old).is_err());
    reopened.authenticate_agent_run(&parent.token).unwrap();
}

#[test]
fn recovery_requires_exact_management_owner_project_and_generation() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let old = db.authenticate_agent_run(&grant.token).unwrap();
    assert_eq!(
        db.isolated_agent_generation("owner-a", "project-a", old.agent_id())
            .unwrap(),
        1
    );
    assert_eq!(
        db.isolated_agent_generation("owner-b", "project-a", old.agent_id())
            .unwrap_err()
            .code(),
        "isolated_agent_not_found"
    );
    let missing = db
        .recover_isolated_agent("owner-b", "project-a", "missing", 1)
        .unwrap_err();
    assert_eq!(
        db.recover_isolated_agent("owner-b", "project-a", old.agent_id(), 1)
            .unwrap_err(),
        missing
    );
    assert_eq!(
        db.recover_isolated_agent("owner-a", "project-b", old.agent_id(), 1)
            .unwrap_err(),
        missing
    );
    assert_eq!(
        db.recover_isolated_agent("owner-a", "project-a", old.agent_id(), 0)
            .unwrap_err()
            .code(),
        "agent_run_generation_changed"
    );
    db.validate_agent_run(&old).unwrap();
    let new = db
        .recover_isolated_agent("owner-a", "project-a", old.agent_id(), 1)
        .unwrap();
    assert_eq!(
        db.recover_isolated_agent("owner-a", "project-a", old.agent_id(), 1)
            .unwrap_err()
            .code(),
        "agent_run_generation_changed"
    );
    db.authenticate_agent_run(&new.token).unwrap();
    assert_eq!(
        db.isolated_agent_generation("owner-a", "project-a", old.agent_id())
            .unwrap(),
        2
    );
}

#[test]
fn failed_recovery_rolls_back_revocation_of_original_and_pending_runs() {
    let (_temp, db) = database();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let old = db.authenticate_agent_run(&grant.token).unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    db.conn_for_tests()
        .execute_batch(
            "CREATE TRIGGER fail_recovery BEFORE INSERT ON wc_agent_runs
        WHEN NEW.generation=3 BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;",
        )
        .unwrap();
    assert_eq!(
        db.recover_isolated_agent("owner-a", "project-a", old.agent_id(), 2)
            .unwrap_err()
            .code(),
        "agent_isolation_unavailable"
    );
    db.validate_agent_run(&old).unwrap();
    let conn = db.conn_for_tests();
    crate::agent_isolation::pending_agent_run_on_connection(&conn, &old, &pending.run_id).unwrap();
}
