use super::*;
use std::sync::{Arc, Barrier};

fn database() -> (tempfile::TempDir, Database) {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    (temp, db)
}
fn task(db: &Database, name: &str) -> String {
    db.create_agent_task(
        &managed_user_principal("user-id"),
        crate::NewAgentTask {
            title: name.into(),
            instruction: format!("Public task {name}"),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some("project".into()),
            idempotency_key: name.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}
fn request(db: &Database, task: &str, purpose: AgentEnrollmentPurpose) -> NewAgentEnrollment {
    NewAgentEnrollment {
        purpose,
        owner_user: "user-id".into(),
        project: "project".into(),
        principal_key: "oauth-principal".into(),
        task_id: self::task(db, task),
        task_directory: format!("workspace/{task}"),
        brief: "Only the explicitly assigned work".into(),
        inputs: vec![AgentEnrollmentInput {
            name: "approved-input".into(),
            content: "immutable authorized snapshot".into(),
        }],
        room_ids: vec![],
        policy: AgentEnrollmentPolicy {
            max_subagents: if purpose == AgentEnrollmentPurpose::Manager {
                4
            } else {
                0
            },
            max_cli_calls: if matches!(
                purpose,
                AgentEnrollmentPurpose::Fallback | AgentEnrollmentPurpose::CliWorker
            ) {
                0
            } else {
                2
            },
        },
        expires_at_unix_ms: now_ms() + 60_000,
    }
}
fn issue(
    db: &Database,
    request: NewAgentEnrollment,
    source: Option<&VerifiedAgentRun>,
    key: &str,
) -> (AgentEnrollmentIssued, String) {
    let mut issued = db
        .issue_agent_enrollment(request, source, key, now_ms())
        .unwrap();
    let secret = issued.grant.take().unwrap().into_secret();
    (issued, secret)
}
fn manager(db: &Database) -> VerifiedAgentRun {
    let (issued, secret) = issue(
        db,
        request(&db, "manager-task", AgentEnrollmentPurpose::Manager),
        None,
        "manager",
    );
    db.claim_agent_enrollment(
        "oauth-principal",
        "manager-window",
        &issued.enrollment_id,
        &secret,
        now_ms(),
    )
    .unwrap()
    .binding
}

#[test]
fn enrollment_requires_real_owned_live_public_task_at_issue_and_claim() {
    let (_temp, db) = database();
    let original = request(&db, "public-task", AgentEnrollmentPurpose::Manager);
    for (field, value) in [
        ("task", "arbitrary-label"),
        ("owner", "another-user"),
        ("project", "another-project"),
    ] {
        let mut bad = original.clone();
        match field {
            "task" => bad.task_id = value.into(),
            "owner" => bad.owner_user = value.into(),
            _ => bad.project = value.into(),
        }
        assert_eq!(
            db.issue_agent_enrollment(bad, None, field, now_ms())
                .unwrap_err()
                .code(),
            "agent_enrollment_task_unavailable"
        );
    }
    assert_eq!(
        db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_isolated_agents", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let (issued, secret) = issue(&db, original.clone(), None, "valid");
    db.conn_for_tests().execute("UPDATE wc_agent_tasks SET state='failed', terminal_attempt_id='synthetic-terminal', terminal_at_unix_ms=?1 WHERE task_id=?2", params![now_ms(), original.task_id]).unwrap();
    assert_eq!(
        db.claim_agent_enrollment(
            "oauth-principal",
            "window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .unwrap_err()
        .code(),
        "agent_enrollment_task_unavailable"
    );
    assert_eq!(
        db.issue_agent_enrollment(original, None, "after-terminal", now_ms())
            .unwrap_err()
            .code(),
        "agent_enrollment_task_unavailable"
    );
    assert_eq!(
        db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_agent_host_windows", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn enrollment_terminal_task_stops_business_but_preserves_identity_and_memory() {
    let (_temp, db) = database();
    let manager = manager(&db);
    let assignment = db.read_agent_enrollment(&manager).unwrap();
    db.conn_for_tests().execute("UPDATE wc_agent_tasks SET state='failed', terminal_attempt_id='synthetic-terminal', terminal_at_unix_ms=?1 WHERE task_id=?2", params![now_ms(), assignment.task_id]).unwrap();
    assert_eq!(
        db.authorize_agent_assignment(
            &manager,
            &assignment.task_id,
            AgentAssignmentOperation::PlanManage
        )
        .unwrap_err()
        .code(),
        "agent_enrollment_task_unavailable"
    );
    assert!(db
        .authorize_agent_assignment(
            &manager,
            &assignment.task_id,
            AgentAssignmentOperation::Read
        )
        .is_ok());
    db.with_agent_run_for_path(&manager, "private_write", "memory/own.txt", true, || Ok(()))
        .unwrap();
    assert!(db
        .with_agent_run_for_path(
            &manager,
            "private_write",
            &format!("tasks/{}/status", assignment.task_id),
            true,
            || Ok(())
        )
        .is_err());
}

#[test]
fn enrollment_cli_worker_has_independent_identity_and_requires_execution_controller() {
    let (_temp, db) = database();
    let manager = manager(&db);
    let (member, secret) = issue(
        &db,
        request(&db, "member", AgentEnrollmentPurpose::Member),
        Some(&manager),
        "member",
    );
    let member = db
        .claim_agent_enrollment(
            "oauth-principal",
            "member",
            &member.enrollment_id,
            &secret,
            now_ms(),
        )
        .unwrap()
        .binding;
    let spec = request(&db, "worker", AgentEnrollmentPurpose::CliWorker);
    assert_eq!(spec.policy.max_subagents, 0);
    assert_eq!(spec.policy.max_cli_calls, 0);
    let (worker, secret) = issue(&db, spec, Some(&member), "worker");
    assert_ne!(worker.agent_id, member.agent_id());
    assert_ne!(worker.agent_id, manager.agent_id());
    assert_eq!(
        db.claim_agent_enrollment(
            "oauth-principal",
            "worker-window",
            &worker.enrollment_id,
            &secret,
            now_ms()
        )
        .unwrap_err()
        .code(),
        "agent_cli_worker_controller_required"
    );
    assert_eq!(
        db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut recursive = request(&db, "recursive-worker", AgentEnrollmentPurpose::CliWorker);
    recursive.policy.max_cli_calls = 1;
    assert!(db
        .issue_agent_enrollment(recursive, Some(&member), "recursive", now_ms())
        .is_err());
}

#[test]
fn enrollment_role_upgrade_preserves_existing_grant_and_immutability() {
    let (_temp, db) = database();
    let (issued, secret) = issue(
        &db,
        request(&db, "old-task", AgentEnrollmentPurpose::Manager),
        None,
        "old",
    );
    {
        let conn = db.conn_for_tests();
        let schema: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name='wc_agent_enrollments'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let old = schema
            .replacen("wc_agent_enrollments", "old_enrollments", 1)
            .replace(
                "('manager','member','fallback','cli_worker')",
                "('manager','member','fallback')",
            );
        conn.execute_batch(&old).unwrap();
        conn.execute_batch("INSERT INTO old_enrollments SELECT * FROM wc_agent_enrollments; DROP TABLE wc_agent_enrollments; ALTER TABLE old_enrollments RENAME TO wc_agent_enrollments;").unwrap();
        ensure_enrollment_schema(&conn).unwrap();
        assert!(conn
            .execute(
                "UPDATE wc_agent_enrollments SET purpose='cli_worker' WHERE enrollment_id=?1",
                [&issued.enrollment_id]
            )
            .is_err());
    }
    db.claim_agent_enrollment(
        "oauth-principal",
        "upgraded",
        &issued.enrollment_id,
        &secret,
        now_ms(),
    )
    .unwrap();
}

#[test]
fn enrollment_status_reconciles_original_binding_after_deadline_without_reissuing() {
    let (_temp, db) = database();
    let mut spec = request(&db, "status", AgentEnrollmentPurpose::Manager);
    spec.expires_at_unix_ms = now_ms() + 600;
    let (issued, secret) = issue(&db, spec, None, "status");
    let initial = db
        .agent_enrollment_status("user-id", "project", &issued.enrollment_id)
        .unwrap();
    assert_eq!(initial.state, "issued");
    assert!(initial.window_binding_ref.is_none());
    db.claim_agent_enrollment(
        "oauth-principal",
        "actual-outer-private-window",
        &issued.enrollment_id,
        &secret,
        now_ms(),
    )
    .unwrap();
    let attached = db
        .agent_enrollment_status("user-id", "project", &issued.enrollment_id)
        .unwrap();
    assert_eq!(attached.generation, 1);
    assert_eq!(attached.run_id, issued.run_id);
    assert_eq!(attached.state, "claimed");
    assert!(attached.window_enabled);
    assert!(attached
        .window_binding_ref
        .as_deref()
        .unwrap()
        .starts_with("wc_host_binding_"));
    let encoded = serde_json::to_string(&attached).unwrap();
    for hidden in [
        &secret,
        "oauth-principal",
        "actual-outer-private-window",
        "run_token",
    ] {
        assert!(!encoded.contains(hidden));
    }
    std::thread::sleep(std::time::Duration::from_millis(700));
    let expired = db
        .agent_enrollment_status("user-id", "project", &issued.enrollment_id)
        .unwrap();
    assert!(expired.claim_expired);
    assert_eq!(expired.window_binding_ref, attached.window_binding_ref);
    assert!(db
        .claim_agent_enrollment(
            "oauth-principal",
            "new-window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .is_err());
    assert!(db
        .agent_enrollment_status("other-owner", "project", &issued.enrollment_id)
        .is_err());
    db.revoke_agent_enrollment("user-id", "project", &issued.enrollment_id)
        .unwrap();
    let revoked = db
        .agent_enrollment_status("user-id", "project", &issued.enrollment_id)
        .unwrap();
    assert_eq!(revoked.state, "revoked");
    assert_eq!(revoked.run_state, "revoked");
    assert!(!revoked.window_enabled);
    assert_eq!(revoked.window_binding_ref, attached.window_binding_ref);
}

#[test]
fn enrollment_first_claim_needs_no_existing_window_and_never_issues_run_secret() {
    let (temp, db) = database();
    let spec = request(&db, "task-one", AgentEnrollmentPurpose::Manager);
    let (issued, secret) = issue(&db, spec.clone(), None, "first");
    assert!(secret.starts_with("wc_en_"));
    assert!(!format!("{issued:?}").contains(&secret));
    assert!(db
        .agent_enrollment_claim_authorized("oauth-principal", now_ms())
        .unwrap());
    assert!(db.authenticate_agent_run(&secret).is_err());
    let state: String = db
        .conn_for_tests()
        .query_row(
            "SELECT state FROM wc_agent_runs WHERE run_id=?1",
            [&issued.run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "pending");
    let stored: String = db
        .conn_for_tests()
        .query_row(
            "SELECT secret_hash FROM wc_agent_enrollments WHERE enrollment_id=?1",
            [&issued.enrollment_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(stored, secret);
    let replay = db
        .issue_agent_enrollment(spec, None, "first", now_ms())
        .unwrap();
    assert!(replay.replayed);
    assert!(replay.grant.is_none());
    let claim = db
        .claim_agent_enrollment(
            "oauth-principal",
            "first-window",
            &issued.enrollment_id,
            &secret,
            now_ms(),
        )
        .unwrap();
    assert!(!claim.replayed);
    assert_eq!(claim.binding.agent_id(), issued.agent_id);
    assert_eq!(
        claim.projection.inputs[0].content,
        "immutable authorized snapshot"
    );
    let public = serde_json::to_string(&claim.projection).unwrap();
    for hidden in [
        "owner_user",
        "principal_key",
        "secret_hash",
        "run_token",
        "wc_ar_",
    ] {
        assert!(!public.contains(hidden));
    }
    let again = db
        .claim_agent_enrollment(
            "oauth-principal",
            "first-window",
            &issued.enrollment_id,
            &secret,
            now_ms(),
        )
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.binding, claim.binding);
    assert!(db
        .claim_agent_enrollment(
            "oauth-principal",
            "second-window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .is_err());
    drop(db);
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    assert_eq!(
        db.read_agent_enrollment(&claim.binding).unwrap(),
        claim.projection
    );
    assert!(matches!(
        db.resolve_agent_host_window("oauth-principal", "first-window", now_ms())
            .unwrap(),
        Some(crate::AgentHostWindowBinding::Active(_))
    ));
}

#[test]
fn enrollment_pending_cannot_be_laundered_through_active_run_or_recovery() {
    let (_temp, db) = database();
    let (issued, _) = issue(
        &db,
        request(&db, "task", AgentEnrollmentPurpose::Manager),
        None,
        "pending",
    );
    let recovered = db
        .recover_isolated_agent("user-id", "project", &issued.agent_id, 1)
        .unwrap();
    assert!(db.authenticate_agent_run(&recovered.token).is_err());
    let binding = VerifiedAgentRun {
        agent_id: recovered.agent.agent_id,
        owner_user: "user-id".into(),
        project: "project".into(),
        run_id: recovered.run_id,
        generation: 2,
        expires_at_unix_ms: recovered.expires_at_unix_ms,
    };
    assert!(db.validate_agent_run(&binding).is_err());
    let conn = db.conn_for_tests();
    assert!(verify_agent_run_context_on_connection(
        &conn,
        &binding.host_goal_owner_key(),
        binding.run_id()
    )
    .is_err());
}

#[test]
fn enrollment_changed_intent_wrong_principal_foreign_and_tombstoned_windows_are_denied() {
    let (_temp, db) = database();
    let spec = request(&db, "task", AgentEnrollmentPurpose::Manager);
    let (issued, secret) = issue(&db, spec.clone(), None, "same-key");
    let mut changed = spec;
    changed.brief = "changed".into();
    assert_eq!(
        db.issue_agent_enrollment(changed, None, "same-key", now_ms())
            .unwrap_err()
            .code(),
        "agent_enrollment_idempotency_conflict"
    );
    assert!(db
        .claim_agent_enrollment(
            "wrong-principal",
            "window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .is_err());
    assert!(db
        .claim_agent_enrollment(
            "oauth-principal",
            "window",
            &issued.enrollment_id,
            "wc_hc_forged",
            now_ms()
        )
        .is_err());
    let legacy = db.create_isolated_agent("legacy", "project", None).unwrap();
    db.conn_for_tests().execute("INSERT INTO wc_agent_host_windows(principal_key,window_key,run_id,enabled) VALUES('another-principal','old-window',?1,0)",[&legacy.run_id]).unwrap();
    assert!(db
        .claim_agent_enrollment(
            "oauth-principal",
            "old-window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .is_err());
    db.claim_agent_enrollment(
        "oauth-principal",
        "fresh-window",
        &issued.enrollment_id,
        &secret,
        now_ms(),
    )
    .unwrap();
}

#[test]
fn enrollment_new_agents_do_not_inherit_source_private_authority_and_roles_stay_bounded() {
    let (_temp, db) = database();
    let manager = manager(&db);
    let (issued, secret) = issue(
        &db,
        request(&db, "member-task", AgentEnrollmentPurpose::Member),
        Some(&manager),
        "member",
    );
    let member = db
        .claim_agent_enrollment(
            "oauth-principal",
            "member-window",
            &issued.enrollment_id,
            &secret,
            now_ms(),
        )
        .unwrap()
        .binding;
    let member_task = db.read_agent_enrollment(&member).unwrap().task_id;
    let manager_task = db.read_agent_enrollment(&manager).unwrap().task_id;
    assert_ne!(member.agent_id(), manager.agent_id());
    assert_ne!(member.host_goal_owner_key(), manager.host_goal_owner_key());
    assert!(db
        .authorize_agent_assignment(&member, &manager_task, AgentAssignmentOperation::Read)
        .is_err());
    assert!(db
        .authorize_agent_assignment(&member, &member_task, AgentAssignmentOperation::Delegate)
        .is_err());
    assert!(db
        .authorize_agent_assignment(&member, &member_task, AgentAssignmentOperation::PlanManage)
        .is_err());
    db.authorize_agent_assignment(&member, &member_task, AgentAssignmentOperation::UseCli)
        .unwrap();
    assert!(db
        .authorize_agent_assignment(
            &manager,
            &manager_task,
            AgentAssignmentOperation::ManagePolicy
        )
        .is_err());
    let (fallback, secret) = issue(
        &db,
        request(&db, "fallback-task", AgentEnrollmentPurpose::Fallback),
        Some(&manager),
        "fallback",
    );
    let fallback = db
        .claim_agent_enrollment(
            "oauth-principal",
            "fallback-window",
            &fallback.enrollment_id,
            &secret,
            now_ms(),
        )
        .unwrap()
        .binding;
    let fallback_task = db.read_agent_enrollment(&fallback).unwrap().task_id;
    for op in [
        AgentAssignmentOperation::Delegate,
        AgentAssignmentOperation::UseCli,
        AgentAssignmentOperation::PlanManage,
    ] {
        assert!(db
            .authorize_agent_assignment(&fallback, &fallback_task, op)
            .is_err());
    }
    let mut over = request(&db, "over", AgentEnrollmentPurpose::Member);
    over.policy.max_cli_calls = 3;
    assert!(db
        .issue_agent_enrollment(over, Some(&manager), "over", now_ms())
        .is_err());
    assert!(db
        .issue_agent_enrollment(
            request(&db, "recursive", AgentEnrollmentPurpose::Fallback),
            Some(&member),
            "recursive",
            now_ms()
        )
        .is_err());
    let conn = db.conn_for_tests();
    authorize_agent_delegated_task_on_connection(&conn, &manager, &member_task).unwrap();
    assert!(
        authorize_agent_delegated_task_on_connection(&conn, &manager, "some-other-user-task")
            .is_err()
    );
}

#[test]
fn enrollment_source_revocation_and_draining_are_rechecked_at_claim() {
    let (_temp, db) = database();
    let manager = manager(&db);
    let (issued, secret) = issue(
        &db,
        request(&db, "member-task", AgentEnrollmentPurpose::Member),
        Some(&manager),
        "member",
    );
    db.ensure_agent_host_context(&manager, Some((1000, 2000)))
        .unwrap();
    db.observe_agent_host_context(&manager, "drain", 2001, false)
        .unwrap();
    assert_eq!(
        db.claim_agent_enrollment(
            "oauth-principal",
            "new-window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .unwrap_err()
        .code(),
        "agent_enrollment_stale_source"
    );
    assert!(db
        .issue_agent_enrollment(
            request(&db, "another", AgentEnrollmentPurpose::Member),
            Some(&manager),
            "another",
            now_ms()
        )
        .is_err());
    db.revoke_agent_run(
        manager.owner_user(),
        manager.project(),
        manager.agent_id(),
        manager.run_id(),
    )
    .unwrap();
    assert!(db
        .claim_agent_enrollment(
            "oauth-principal",
            "new-window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .is_err());
    let bound: i64 = db
        .conn_for_tests()
        .query_row(
            "SELECT COUNT(*) FROM wc_agent_host_windows WHERE window_key='new-window'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bound, 0);
}

#[test]
fn enrollment_principal_revocation_invalidates_unclaimed_grant_without_prior_window() {
    let (_temp, db) = database();
    let (issued, secret) = issue(
        &db,
        request(&db, "task", AgentEnrollmentPurpose::Manager),
        None,
        "manager",
    );
    db.revoke_agent_host_principal("oauth-principal").unwrap();
    assert!(db
        .agent_enrollment_claim_authorized("oauth-principal", now_ms())
        .is_err());
    assert!(db
        .claim_agent_enrollment(
            "oauth-principal",
            "new-window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .is_err());
    let state: String = db
        .conn_for_tests()
        .query_row(
            "SELECT state FROM wc_agent_runs WHERE run_id=?1",
            [issued.run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "revoked");
}

#[test]
fn enrollment_room_membership_commits_with_claim_and_failure_rolls_everything_back() {
    let (_temp, db) = database();
    let member = db
        .create_isolated_agent("user-id", "project", None)
        .unwrap();
    let room = db
        .create_agent_meeting("project", &[member.agent.agent_id], "shared brief")
        .unwrap();
    let mut spec = request(&db, "task", AgentEnrollmentPurpose::Manager);
    spec.room_ids.push(room.clone());
    let (issued, secret) = issue(&db, spec, None, "manager");
    db.conn_for_tests().execute_batch("CREATE TRIGGER deny_first_membership BEFORE INSERT ON wc_agent_meeting_members BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(db
        .claim_agent_enrollment(
            "oauth-principal",
            "window",
            &issued.enrollment_id,
            &secret,
            now_ms()
        )
        .is_err());
    let conn = db.conn_for_tests();
    for table in ["wc_agent_host_windows", "wc_agent_enrollment_assignments"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
    conn.execute_batch("DROP TRIGGER deny_first_membership;")
        .unwrap();
    drop(conn);
    let claim = db
        .claim_agent_enrollment(
            "oauth-principal",
            "window",
            &issued.enrollment_id,
            &secret,
            now_ms(),
        )
        .unwrap();
    assert_eq!(
        db.list_agent_meetings(&claim.binding, 10).unwrap()[0].room_id,
        room
    );
}

#[test]
fn enrollment_concurrent_windows_have_one_winner() {
    let (temp, db) = database();
    let (issued, secret) = issue(
        &db,
        request(&db, "task", AgentEnrollmentPurpose::Manager),
        None,
        "manager",
    );
    let a = Database::open(&temp.path().join("state.db")).unwrap();
    let b = Database::open(&temp.path().join("state.db")).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let id = issued.enrollment_id;
    let results = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            a.claim_agent_enrollment("oauth-principal", "one", &id, &secret, now_ms())
        });
        let two = scope.spawn(|| {
            barrier.wait();
            b.claim_agent_enrollment("oauth-principal", "two", &id, &secret, now_ms())
        });
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
}

#[test]
fn enrollment_two_independent_agents_cannot_claim_same_active_task() {
    let (temp, db) = database();
    let (first, first_secret) = issue(
        &db,
        request(&db, "task", AgentEnrollmentPurpose::Manager),
        None,
        "one",
    );
    let (second, second_secret) = issue(
        &db,
        request(&db, "task", AgentEnrollmentPurpose::Manager),
        None,
        "two",
    );
    let a = Database::open(&temp.path().join("state.db")).unwrap();
    let b = Database::open(&temp.path().join("state.db")).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            a.claim_agent_enrollment(
                "oauth-principal",
                "one",
                &first.enrollment_id,
                &first_secret,
                now_ms(),
            )
        });
        let two = scope.spawn(|| {
            barrier.wait();
            b.claim_agent_enrollment(
                "oauth-principal",
                "two",
                &second.enrollment_id,
                &second_secret,
                now_ms(),
            )
        });
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let (other, secret) = issue(
        &db,
        request(&db, "other-task", AgentEnrollmentPurpose::Manager),
        None,
        "other",
    );
    db.claim_agent_enrollment(
        "oauth-principal",
        "other",
        &other.enrollment_id,
        &secret,
        now_ms(),
    )
    .unwrap();
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    db.revoke_agent_enrollment("user-id", "project", &winner.projection.enrollment_id)
        .unwrap();
    assert!(db.validate_agent_run(&winner.binding).is_err());
    let (loser, secret) = if winner.projection.enrollment_id == first.enrollment_id {
        (second, second_secret)
    } else {
        (first, first_secret)
    };
    db.claim_agent_enrollment(
        "oauth-principal",
        "new-after-revoke",
        &loser.enrollment_id,
        &secret,
        now_ms(),
    )
    .unwrap();
}

#[test]
fn enrollment_deadline_is_resampled_after_sqlite_lock_wait() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (temp, db) = database();
    let mut spec = request(&db, "task", AgentEnrollmentPurpose::Manager);
    spec.expires_at_unix_ms = now_ms() + 300;
    let (issued, secret) = issue(&db, spec, None, "deadline");
    let waiter = Database::open(&temp.path().join("state.db")).unwrap();
    let mut lock = db.conn_for_tests();
    let tx = lock
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let before = now_ms();
    let handle = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        waiter.claim_agent_enrollment(
            "oauth-principal",
            "late",
            &issued.enrollment_id,
            &secret,
            before,
        )
    });
    started_rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(400));
    tx.commit().unwrap();
    drop(lock);
    assert!(handle.join().unwrap().is_err());
    let count: i64 = db
        .conn_for_tests()
        .query_row("SELECT COUNT(*) FROM wc_agent_host_windows", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn enrollment_path_goal_and_immutable_policy_intersections_survive_recovery() {
    let (_temp, db) = database();
    let manager = manager(&db);
    assert_eq!(
        db.agent_enrollment_task_directory(&manager)
            .unwrap()
            .as_deref(),
        Some("workspace/manager-task")
    );
    assert!(db
        .authorize_agent_goal_directory(&manager, "workspace/other-task")
        .is_err());
    let before = std::cell::Cell::new(false);
    assert!(db
        .with_agent_run(&manager, "private_read", || {
            before.set(true);
            Ok(())
        })
        .is_err());
    assert!(!before.get());
    for path in [
        "workspace",
        "workspace/other-task/file",
        "tasks/other-task/x",
        "goals/foreign/x",
        "",
    ] {
        assert!(
            db.with_agent_run_for_path(&manager, "private_read", path, false, || Ok(()))
                .is_err(),
            "{path}"
        );
    }
    for path in [
        "memory/note",
        "workspace/manager-task/input",
        &format!(
            "tasks/{}/status",
            db.read_agent_enrollment(&manager).unwrap().task_id
        ),
    ] {
        db.with_agent_run_for_path(&manager, "private_read", path, false, || Ok(()))
            .unwrap();
    }
    let context = db.ensure_agent_host_context(&manager, None).unwrap();
    let owner = manager.host_goal_owner_key();
    assert!(db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "agent-private://self",
            "workspace/other-task",
            "wrong",
            "wrong",
            now_ms(),
            Some(&context)
        )
        .is_err());
    let goal = db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "agent-private://self",
            "workspace/manager-task",
            "assigned",
            "one",
            now_ms(),
            Some(&context),
        )
        .unwrap()
        .goal;
    db.read_host_goal_for_agent_run(&manager, &goal.goal_id)
        .unwrap();
    assert!(db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "agent-private://self",
            "workspace/manager-task",
            "second",
            "two",
            now_ms(),
            Some(&context)
        )
        .is_err());
    assert!(db
        .conn_for_tests()
        .execute(
            "UPDATE wc_agent_enrollments SET purpose='fallback' WHERE agent_id=?1",
            [manager.agent_id()]
        )
        .is_err());
    let recovered = db
        .recover_isolated_agent("user-id", "project", manager.agent_id(), 1)
        .unwrap();
    let new = db.authenticate_agent_run(&recovered.token).unwrap();
    assert_eq!(
        db.read_agent_enrollment(&new).unwrap().purpose,
        AgentEnrollmentPurpose::Manager
    );
    db.read_host_goal_for_agent_run(&new, &goal.goal_id)
        .unwrap();
    assert!(db
        .with_agent_run_for_path(
            &new,
            "private_read",
            "workspace/other-task/x",
            false,
            || Ok(())
        )
        .is_err());
}

// Append to agent_enrollment_tests.rs; reuse database(), request(), issue(),
// managed_user_principal() and now_ms(). All records are disposable fixtures.

const MANAGER_BINDING_RELAY_DIGEST: &str =
    "ee9d4a75bc0b42f36f345aa96231e0db1ab00488122f34ebc99d6db055b6603e";
const MANAGER_BINDING_PACKAGE_DIGEST: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn manager_binding_host(db: &Database, index: u32) -> (String, String, i64) {
    let grant = db
        .register_native_agent_host(
            &format!("tunnel_{index:032x}"),
            MANAGER_BINDING_RELAY_DIGEST,
        )
        .unwrap();
    let principal = db.native_agent_host_principal(&grant.host_id).unwrap();
    (grant.host_id, principal, grant.generation)
}

fn manager_binding_pending(
    db: &Database,
    principal: &str,
    name: &str,
) -> (NewAgentEnrollment, AgentEnrollmentIssued, String) {
    let mut input = request(db, name, AgentEnrollmentPurpose::Manager);
    input.principal_key = principal.into();
    let (issued, secret) = issue(db, input.clone(), None, name);
    (input, issued, secret)
}

fn manager_binding_claim(
    db: &Database,
    principal: &str,
    window: &str,
    issued: &AgentEnrollmentIssued,
    secret: &str,
) -> VerifiedAgentRun {
    db.claim_agent_enrollment(principal, window, &issued.enrollment_id, secret, now_ms())
        .unwrap()
        .binding
}

fn manager_binding_snapshot(db: &Database) -> std::collections::BTreeMap<String, Vec<String>> {
    let conn = db.conn_for_tests();
    let schema: Vec<String> = conn
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
        .unwrap()
        .query_map([], |r| {
            Ok(format!(
                "{:?}",
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?
                )
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut snapshot = std::collections::BTreeMap::from([("<schema>".into(), schema)]);
    for table in tables {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
            .unwrap();
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                let cells = (0..columns)
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(format!("{cells:?}"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        snapshot.insert(table, rows);
    }
    snapshot
}

fn manager_binding_readonly<T>(db: &Database, read: impl FnOnce() -> T) -> T {
    let before = manager_binding_snapshot(db);
    let changes = || {
        db.conn_for_tests()
            .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before_changes = changes();
    let result = read();
    assert_eq!(manager_binding_snapshot(db), before);
    assert_eq!(
        changes(),
        before_changes,
        "binding lookup attempted a write"
    );
    result
}

fn manager_binding_read(
    db: &Database,
    root: &str,
    host: &str,
) -> Result<Option<AgentHostManagerBinding>, AgentIsolationError> {
    manager_binding_readonly(db, || {
        db.read_agent_host_manager_binding("user-id", "project", root, host)
    })
}

fn manager_binding_prepare_handoff(
    db: &Database,
    source: &VerifiedAgentRun,
    principal: &str,
    directory: &str,
    candidate_window: &str,
) -> crate::HostGoalHandoff {
    let context = db.ensure_agent_host_context(source, None).unwrap();
    let owner = source.host_goal_owner_key();
    let goal = db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            "project",
            "/private",
            directory,
            "Continue this Manager assignment",
            "manager-goal",
            now_ms(),
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
        crate::HostGoalCheckpointAction::Handoff,
        "continue same assignment",
        "manager-handoff",
        "manager-handoff-request",
        &[],
        now_ms(),
        Some(&context),
        &[],
    )
    .unwrap();
    let waiting = db
        .read_host_goal_handoff(&owner, &goal.goal_id)
        .unwrap()
        .unwrap();
    let begun = db
        .begin_host_goal_handoff_attempt(
            &owner,
            &goal.goal_id,
            &waiting.handoff_id,
            waiting.attempt_id.as_deref(),
            now_ms(),
            2,
        )
        .unwrap();
    db.bind_handoff_host_window(
        principal,
        candidate_window,
        &begun.goal_id,
        &begun.handoff_id,
        begun.attempt_id.as_deref().unwrap(),
        now_ms(),
    )
    .unwrap();
    db.prepare_agent_handoff_for_host_window(
        principal,
        candidate_window,
        &begun.goal_id,
        &begun.handoff_id,
        begun.attempt_id.as_deref().unwrap(),
        begun.checkpoint_sequence,
        MANAGER_BINDING_PACKAGE_DIGEST,
        now_ms(),
    )
    .unwrap()
}

#[test]
fn host_manager_binding_initial_claim_and_host_rotation_return_only_safe_metadata() {
    let (_temp, db) = database();
    let (host, principal, generation) = manager_binding_host(&db, 1);
    let mut input = request(&db, "binding-initial", AgentEnrollmentPurpose::Manager);
    input.principal_key = principal.clone();
    input.expires_at_unix_ms = now_ms() + 5_000;
    let (issued, secret) = issue(&db, input.clone(), None, "binding-initial");
    assert!(manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .is_none());
    let run = manager_binding_claim(&db, &principal, "initial-window", &issued, &secret);
    db.start_agent_public_task(&run, &input.task_id, "start")
        .unwrap();
    db.ensure_agent_host_context(&run, None).unwrap();
    let binding = manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .unwrap();
    assert_eq!(binding.root_task_id, input.task_id);
    assert_eq!(binding.host_id, host);
    assert_eq!(binding.manager_agent_id, run.agent_id());
    assert_eq!(binding.manager_run_id, run.run_id());
    assert_eq!(binding.generation, run.generation());
    assert_eq!(binding.owner_user, "user-id");
    assert_eq!(binding.project, "project");
    assert_eq!(binding.enrollment_id, issued.enrollment_id);
    assert_eq!(binding.native_host_generation, generation);
    assert_eq!(binding.run_expires_at_ms, run.expires_at_unix_ms());
    assert!(!binding.window_binding_ref.is_empty());
    assert_ne!(binding.window_binding_ref, "initial-window");
    let encoded = serde_json::to_value(&binding).unwrap();
    let mut keys: Vec<_> = encoded
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    let mut expected = vec![
        "root_task_id",
        "host_id",
        "manager_agent_id",
        "manager_run_id",
        "generation",
        "owner_user",
        "project",
        "enrollment_id",
        "native_host_generation",
        "run_expires_at_ms",
        "window_binding_ref",
    ];
    expected.sort();
    assert_eq!(keys, expected);
    let serialized = serde_json::to_string(&binding).unwrap();
    assert!(!serialized.contains(&secret));
    assert!(!serialized.contains(&principal));
    assert!(!serialized.contains("initial-window"));
    let (same_host, same_principal, next_generation) = manager_binding_host(&db, 1);
    assert_eq!(same_host, host);
    assert_eq!(same_principal, principal);
    let rotated = manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .unwrap();
    assert_eq!(rotated.native_host_generation, next_generation);
    assert_eq!(next_generation, generation + 1);
    assert_ne!(rotated.native_host_generation, rotated.generation);
    assert_eq!(rotated.manager_run_id, run.run_id());
    assert_eq!(rotated.window_binding_ref, binding.window_binding_ref);
    // Exercise real expiration without modifying immutable enrollment fields.
    // Future execution is bounded by one monotonic deadline; no CLI is started.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while now_ms() <= input.expires_at_unix_ms {
        assert!(
            std::time::Instant::now() < deadline,
            "test clock did not reach grant expiry"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let status = db
        .agent_enrollment_status("user-id", "project", &issued.enrollment_id)
        .unwrap();
    assert!(status.claim_expired);
    assert!(status.run_expires_at_unix_ms > now_ms());
    let after_claim_expiry = manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(after_claim_expiry).unwrap(),
        serde_json::to_value(rotated).unwrap()
    );
}

#[test]
fn host_manager_binding_uses_exact_root_host_scope_and_manager_role() {
    let (_temp, db) = database();
    let (host_a, principal_a, _) = manager_binding_host(&db, 2);
    let (host_b, principal_b, _) = manager_binding_host(&db, 3);
    let (a, a_issued, a_secret) = manager_binding_pending(&db, &principal_a, "binding-root-a");
    let manager_a = manager_binding_claim(&db, &principal_a, "root-a-window", &a_issued, &a_secret);
    let (b, b_issued, b_secret) = manager_binding_pending(&db, &principal_a, "binding-root-b");
    let manager_b = manager_binding_claim(&db, &principal_a, "root-b-window", &b_issued, &b_secret);
    assert_eq!(
        manager_binding_read(&db, &a.task_id, &host_a)
            .unwrap()
            .unwrap()
            .manager_agent_id,
        manager_a.agent_id()
    );
    assert_eq!(
        manager_binding_read(&db, &b.task_id, &host_a)
            .unwrap()
            .unwrap()
            .manager_agent_id,
        manager_b.agent_id()
    );
    assert!(manager_binding_read(&db, &a.task_id, &host_b)
        .unwrap()
        .is_none());
    assert!(manager_binding_read(&db, &b.task_id, "unknown-host")
        .unwrap()
        .is_none());
    for (owner, project) in [("another-owner", "project"), ("user-id", "another-project")] {
        assert!(manager_binding_readonly(&db, || db
            .read_agent_host_manager_binding(owner, project, &a.task_id, &host_a))
        .unwrap()
        .is_none());
    }
    assert!(manager_binding_readonly(&db, || db
        .read_agent_host_manager_binding("", "project", &a.task_id, &host_a,))
    .is_err());
    let (c, c_issued, c_secret) = manager_binding_pending(&db, &principal_b, "binding-root-c");
    let manager_c = manager_binding_claim(&db, &principal_b, "root-c-window", &c_issued, &c_secret);
    assert_eq!(
        manager_binding_read(&db, &c.task_id, &host_b)
            .unwrap()
            .unwrap()
            .manager_agent_id,
        manager_c.agent_id()
    );
    assert!(manager_binding_read(&db, &c.task_id, &host_a)
        .unwrap()
        .is_none());
    let mut member = request(&db, "binding-member", AgentEnrollmentPurpose::Member);
    member.principal_key = principal_a.clone();
    let (member_issued, member_secret) =
        issue(&db, member.clone(), Some(&manager_a), "binding-member");
    manager_binding_claim(
        &db,
        &principal_a,
        "member-window",
        &member_issued,
        &member_secret,
    );
    assert!(manager_binding_read(&db, &member.task_id, &host_a)
        .unwrap()
        .is_none());
}

#[test]
fn host_manager_binding_higher_pending_run_never_displaces_open_active_run() {
    let (_temp, db) = database();
    let (host, principal, _) = manager_binding_host(&db, 4);
    let (input, issued, secret) = manager_binding_pending(&db, &principal, "binding-pending");
    let run = manager_binding_claim(&db, &principal, "pending-source-window", &issued, &secret);
    let before = manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .unwrap();
    let pending = db.begin_agent_run_handoff(&run).unwrap();
    assert!(pending.generation > run.generation());
    assert!(db.authenticate_agent_run(&pending.token).is_err());
    let current = manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(current).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert_eq!(
        db.conn_for_tests()
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_host_windows WHERE run_id=?1",
                [&pending.run_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn host_manager_binding_native_handoff_drains_then_reopens_one_current_tuple() {
    let (temp, db) = database();
    let (host, principal, _) = manager_binding_host(&db, 5);
    let (input, issued, secret) = manager_binding_pending(&db, &principal, "binding-native");
    let run = manager_binding_claim(&db, &principal, "native-source-window", &issued, &secret);
    db.start_agent_public_task(&run, &input.task_id, "start")
        .unwrap();
    let before = manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .unwrap();
    let handoff = manager_binding_prepare_handoff(
        &db,
        &run,
        &principal,
        &input.task_directory,
        "native-successor-window",
    );
    assert_eq!(handoff.status, "prepared");
    assert!(manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .is_none());
    let reopened = Database::open(&temp.path().join("state.db")).unwrap();
    assert!(manager_binding_read(&reopened, &input.task_id, &host)
        .unwrap()
        .is_none());
    let committed = reopened
        .commit_agent_handoff_for_host_window(
            &principal,
            "native-successor-window",
            &handoff.goal_id,
            &handoff.handoff_id,
            handoff.attempt_id.as_deref().unwrap(),
            handoff.checkpoint_sequence,
            MANAGER_BINDING_PACKAGE_DIGEST,
            now_ms(),
        )
        .unwrap();
    assert_eq!(committed.status, "committed");
    let current = manager_binding_read(&reopened, &input.task_id, &host)
        .unwrap()
        .unwrap();
    assert_eq!(current.manager_agent_id, before.manager_agent_id);
    assert_eq!(current.generation, before.generation + 1);
    assert_eq!(
        Some(current.manager_run_id.as_str()),
        handoff.pending_agent_run_id.as_deref()
    );
    assert_ne!(current.manager_run_id, before.manager_run_id);
    assert_ne!(current.window_binding_ref, before.window_binding_ref);
    assert_eq!(current.enrollment_id, before.enrollment_id);
    assert_eq!(current.root_task_id, before.root_task_id);
    assert!(reopened.validate_agent_run(&run).is_err());
    let again = Database::open(&temp.path().join("state.db")).unwrap();
    assert_eq!(
        serde_json::to_value(
            manager_binding_read(&again, &input.task_id, &host)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(current).unwrap()
    );
}

#[test]
fn host_manager_binding_lifecycle_and_original_public_execution_are_not_bypassed() {
    for mode in [
        "host-disabled",
        "window-disabled",
        "terminal",
        "cancelled",
        "revoked",
        "expired",
        "closed",
        "context-meter-only",
        "context-state-only",
        "context-warning",
        "context-source-mismatch",
        "context-null-source",
        "plan-missing-cancel",
        "plan-null-cancel",
        "plan-zero-cancel",
        "plan-false-cancel",
    ] {
        let (_temp, db) = database();
        let (host, principal, _) = manager_binding_host(&db, 6);
        let (input, issued, secret) = manager_binding_pending(&db, &principal, "binding-lifecycle");
        let run = manager_binding_claim(&db, &principal, "lifecycle-window", &issued, &secret);
        let started = db
            .start_agent_public_task(&run, &input.task_id, "start")
            .unwrap();
        assert!(manager_binding_read(&db, &input.task_id, &host)
            .unwrap()
            .is_some());
        match mode {
            "host-disabled" => db.revoke_native_agent_host(&host).unwrap(),
            "window-disabled" => {
                db.conn_for_tests()
                    .execute(
                        "UPDATE wc_agent_host_windows SET enabled=0 WHERE run_id=?1",
                        [run.run_id()],
                    )
                    .unwrap();
            }
            "terminal" => {
                db.complete_agent_public_task(
                    &run,
                    &input.task_id,
                    &started.execution.execution_id,
                    started.execution.revision,
                    crate::AgentTaskState::Failed,
                    None,
                    Some("Explicit task closure"),
                    "complete",
                )
                .unwrap();
            }
            "cancelled" => {
                db.register_agent_team_plan(
                    &run,
                    &input.task_id,
                    0,
                    crate::TeamPlanInput {
                        manager: crate::TeamAgentPlan {
                            task_id: input.task_id.clone(),
                            cli_calls: vec![],
                        },
                        subagents: vec![],
                    },
                    "plan",
                )
                .unwrap();
                db.mark_team_plan_cancelled(&managed_user_principal("user-id"), &input.task_id)
                    .unwrap();
            }
            "revoked" => db
                .revoke_agent_run("user-id", "project", run.agent_id(), run.run_id())
                .unwrap(),
            "expired" => {
                db.conn_for_tests()
                    .execute(
                        "UPDATE wc_agent_runs SET expires_at_unix_ms=?2 WHERE run_id=?1",
                        params![run.run_id(), now_ms() - 1],
                    )
                    .unwrap();
            }
            "closed" => {
                db.begin_agent_execution_close(
                    "user-id",
                    "project",
                    &webcodex_core::runner_protocol::AgentRunRef {
                        agent_id: run.agent_id().into(),
                        agent_run_id: run.run_id().into(),
                        generation: run.generation(),
                    },
                    now_ms() + 60_000,
                )
                .unwrap();
            }
            "context-meter-only"
            | "context-state-only"
            | "context-warning"
            | "context-source-mismatch"
            | "context-null-source" => {
                db.ensure_agent_host_context(&run, None).unwrap();
                let owner = run.host_goal_owner_key();
                match mode {
                    "context-warning" => {
                        db.observe_agent_host_context(&run, "warning-fixture", 600_000, false)
                            .unwrap();
                        assert_eq!(
                            db.read_host_context_state(&owner, run.run_id())
                                .unwrap()
                                .unwrap()
                                .state,
                            "warning"
                        );
                    }
                    "context-meter-only" => {
                        db.conn_for_tests().execute(
                            "DELETE FROM wc_host_context_states WHERE owner_key=?1 AND context_epoch=?2",
                            params![owner,run.run_id()],
                        ).unwrap();
                    }
                    "context-state-only" => {
                        db.conn_for_tests().execute(
                            "DELETE FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2",
                            params![owner,run.run_id()],
                        ).unwrap();
                    }
                    "context-source-mismatch" => {
                        db.conn_for_tests().execute(
                            "UPDATE wc_host_context_states SET identity_source='window_bound' WHERE owner_key=?1 AND context_epoch=?2",
                            params![owner,run.run_id()],
                        ).unwrap();
                    }
                    "context-null-source" => {
                        db.conn_for_tests().execute(
                            "UPDATE wc_host_context_states SET identity_source=NULL WHERE owner_key=?1 AND context_epoch=?2",
                            params![owner,run.run_id()],
                        ).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            "plan-missing-cancel"
            | "plan-null-cancel"
            | "plan-zero-cancel"
            | "plan-false-cancel" => {
                db.register_agent_team_plan(
                    &run,
                    &input.task_id,
                    0,
                    crate::TeamPlanInput {
                        manager: crate::TeamAgentPlan {
                            task_id: input.task_id.clone(),
                            cli_calls: vec![],
                        },
                        subagents: vec![],
                    },
                    "plan",
                )
                .unwrap();
                // Preserve every production trigger while exercising valid JSON
                // whose missing/null/integer values are not Boolean false.
                let expression = match mode {
                    "plan-missing-cancel" => "json_remove(record_json,'$.cancel_requested')",
                    "plan-null-cancel" => "json_set(record_json,'$.cancel_requested',NULL)",
                    "plan-zero-cancel" => "json_set(record_json,'$.cancel_requested',0)",
                    "plan-false-cancel" => {
                        "json_set(record_json,'$.cancel_requested',json('false'))"
                    }
                    _ => unreachable!(),
                };
                db.conn_for_tests().execute(
                    &format!("UPDATE wc_team_plans SET record_json={expression} WHERE root_task_id=?1"),
                    [&input.task_id],
                ).unwrap();
            }
            _ => unreachable!(),
        }
        let result = manager_binding_read(&db, &input.task_id, &host);
        let allowed = matches!(
            mode,
            "context-warning" | "context-null-source" | "plan-false-cancel"
        );
        assert_eq!(result.unwrap().is_some(), allowed, "{mode}");
    }

    // Real revocation releases the assignment, but does not replace the public
    // Task execution's original logical Manager with an unrelated new Agent.
    let (_temp, db) = database();
    let (host, principal, _) = manager_binding_host(&db, 7);
    let (input, issued, secret) =
        manager_binding_pending(&db, &principal, "binding-original-owner");
    let original =
        manager_binding_claim(&db, &principal, "original-owner-window", &issued, &secret);
    db.start_agent_public_task(&original, &input.task_id, "start")
        .unwrap();
    db.revoke_agent_enrollment("user-id", "project", &issued.enrollment_id)
        .unwrap();
    let (replacement_issued, replacement_secret) =
        issue(&db, input.clone(), None, "replacement-manager");
    let replacement = manager_binding_claim(
        &db,
        &principal,
        "replacement-window",
        &replacement_issued,
        &replacement_secret,
    );
    assert_ne!(replacement.agent_id(), original.agent_id());
    db.validate_agent_run(&replacement).unwrap();
    assert!(manager_binding_read(&db, &input.task_id, &host)
        .unwrap()
        .is_none());
}

#[test]
fn host_manager_binding_failed_native_commit_never_exposes_a_mixed_tuple() {
    let (temp, db) = database();
    let (host, principal, _) = manager_binding_host(&db, 8);
    let (input, issued, secret) = manager_binding_pending(&db, &principal, "binding-rollback");
    let source = manager_binding_claim(&db, &principal, "rollback-source-window", &issued, &secret);
    let handoff = manager_binding_prepare_handoff(
        &db,
        &source,
        &principal,
        &input.task_directory,
        "rollback-candidate-window",
    );
    let observer = Database::open(&temp.path().join("state.db")).unwrap();
    assert!(manager_binding_read(&observer, &input.task_id, &host)
        .unwrap()
        .is_none());
    // A disposable failure-injection trigger, never a forged authority event.
    // It remains installed; no production or fixture trigger is dropped.
    db.conn_for_tests()
        .execute_batch(
            "CREATE TRIGGER manager_binding_abort_commit BEFORE UPDATE OF state ON wc_agent_runs
         WHEN OLD.state='pending' AND NEW.state='active'
         BEGIN SELECT RAISE(ABORT,'synthetic B6 transaction failure'); END;",
        )
        .unwrap();
    let before = manager_binding_snapshot(&db);
    assert!(db
        .commit_agent_handoff_for_host_window(
            &principal,
            "rollback-candidate-window",
            &handoff.goal_id,
            &handoff.handoff_id,
            handoff.attempt_id.as_deref().unwrap(),
            handoff.checkpoint_sequence,
            MANAGER_BINDING_PACKAGE_DIGEST,
            now_ms(),
        )
        .is_err());
    assert_eq!(manager_binding_snapshot(&db), before);
    assert!(manager_binding_read(&observer, &input.task_id, &host)
        .unwrap()
        .is_none());
    assert_eq!(manager_binding_snapshot(&observer), before);
    db.validate_agent_run(&source).unwrap();
    let pending_state: String = db
        .conn_for_tests()
        .query_row(
            "SELECT state FROM wc_agent_runs WHERE run_id=?1",
            [handoff.pending_agent_run_id.as_ref().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending_state, "pending");
    assert_eq!(
        db.read_host_goal_handoff(&source.host_goal_owner_key(), &handoff.goal_id)
            .unwrap()
            .unwrap()
            .status,
        "prepared"
    );
}
