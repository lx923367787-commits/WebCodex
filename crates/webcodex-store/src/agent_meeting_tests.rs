use crate::agent_isolation::VerifiedAgentRun;
use crate::agent_meeting::*;
use crate::Database;
use std::sync::{Arc, Barrier};

fn agent(db: &Database, project: &str) -> VerifiedAgentRun {
    let grant = db
        .create_isolated_agent("meeting-test-owner", project, None)
        .unwrap();
    db.authenticate_agent_run(&grant.token).unwrap()
}

fn meeting(db: &Database, agents: &[&VerifiedAgentRun]) -> String {
    db.create_agent_meeting(
        agents[0].project(),
        &agents
            .iter()
            .map(|agent| agent.agent_id().to_string())
            .collect::<Vec<_>>(),
        "Only explicitly published work is shared",
    )
    .unwrap()
}

fn message(key: &str) -> NewAgentMeetingEntry {
    NewAgentMeetingEntry {
        kind: AgentMeetingKind::Message,
        content: format!("Published message {key}"),
        artifact_id: None,
        artifact_bytes: None,
        review_target: None,
        idempotency_key: key.to_string(),
    }
}

fn artifact(key: &str, bytes: &[u8], target: Option<&str>) -> NewAgentMeetingEntry {
    NewAgentMeetingEntry {
        kind: AgentMeetingKind::Artifact,
        content: "Published artifact snapshot".to_string(),
        artifact_id: target.map(str::to_string),
        artifact_bytes: Some(bytes.to_vec()),
        review_target: None,
        idempotency_key: key.to_string(),
    }
}

fn review(key: &str, target: AgentMeetingArtifactTarget) -> NewAgentMeetingEntry {
    NewAgentMeetingEntry {
        kind: AgentMeetingKind::Review,
        content: "Review of precisely this version".to_string(),
        artifact_id: None,
        artifact_bytes: None,
        review_target: Some(target),
        idempotency_key: key.to_string(),
    }
}

fn unavailable<T: std::fmt::Debug>(result: anyhow::Result<T>) {
    assert_eq!(result.unwrap_err().to_string(), "agent_meeting_unavailable");
}

#[test]
fn agent_meeting_snapshots_are_shared_versioned_and_durable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("meeting.db");
    let db = Database::open(&path).unwrap();
    let a = agent(&db, "project-one");
    let b = agent(&db, "project-one");
    let room = meeting(&db, &[&a, &b]);
    let source = temp.path().join("private-source.txt");
    std::fs::write(&source, b"immutable published v1").unwrap();
    let first = db
        .publish_agent_meeting_entry(
            &a,
            &room,
            artifact("artifact-v1", &std::fs::read(&source).unwrap(), None),
        )
        .unwrap();
    let v1 = first.artifact.clone().unwrap();
    std::fs::write(&source, b"private changes after publication").unwrap();
    assert_eq!(first.author_agent_id, a.agent_id());
    assert_eq!(first.author_run_id, a.run_id());
    assert_eq!(first.author_generation, a.generation());
    assert!(first.created_at_unix_ms > 0);
    assert_eq!(v1.version, 1);
    let second = db
        .publish_agent_meeting_entry(
            &a,
            &room,
            artifact("artifact-v2", b"published v2", Some(&v1.artifact_id)),
        )
        .unwrap();
    assert_eq!(second.artifact.as_ref().unwrap().version, 2);
    let review = db
        .publish_agent_meeting_entry(&b, &room, review("review-v1", v1.clone()))
        .unwrap();
    assert_eq!(review.author_agent_id, b.agent_id());
    assert_eq!(review.review_target, Some(v1.clone()));
    drop(db);

    let reopened = Database::open(&path).unwrap();
    assert_eq!(
        reopened
            .read_agent_meeting_artifact(&b, &room, &v1.artifact_id, 1)
            .unwrap()
            .bytes,
        b"immutable published v1"
    );
    assert_eq!(
        reopened
            .read_agent_meeting_artifact(&b, &room, &v1.artifact_id, 2)
            .unwrap()
            .bytes,
        b"published v2"
    );
    let page = reopened.read_agent_meeting(&b, &room, 0, 50).unwrap();
    assert_eq!(page.entries, vec![first, second, review]);
    assert!(page.next_after_sequence.is_none());
}

#[test]
fn agent_meeting_membership_project_and_artifact_authorship_are_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("meeting.db")).unwrap();
    let a = agent(&db, "project-one");
    let b = agent(&db, "project-one");
    let outsider = agent(&db, "project-one");
    let foreign = agent(&db, "project-two");
    let room = meeting(&db, &[&a, &b]);
    let first = db
        .publish_agent_meeting_entry(&a, &room, artifact("v1", b"A's work", None))
        .unwrap();
    let target = first.artifact.unwrap();
    unavailable(db.publish_agent_meeting_entry(
        &b,
        &room,
        artifact("stolen", b"B's overwrite", Some(&target.artifact_id)),
    ));
    unavailable(db.publish_agent_meeting_entry(
        &b,
        &room,
        artifact("missing", b"B's overwrite", Some("missing-artifact")),
    ));
    for denied in [&outsider, &foreign] {
        unavailable(db.read_agent_meeting(denied, &room, 0, 50));
        unavailable(db.read_agent_meeting(denied, "missing-room", 0, 50));
        unavailable(db.read_agent_meeting_artifact(denied, &room, &target.artifact_id, 1));
        unavailable(db.publish_agent_meeting_entry(denied, &room, message("denied")));
        assert!(db.list_agent_meetings(denied, 50).unwrap().is_empty());
    }
    unavailable(db.create_agent_meeting(
        "project-one",
        &[a.agent_id().to_string(), foreign.agent_id().to_string()],
        "invalid",
    ));
    unavailable(db.create_agent_meeting("project-one", &["missing-agent".to_string()], "invalid"));
    let other_room = meeting(&db, &[&a, &b]);
    unavailable(db.publish_agent_meeting_entry(
        &b,
        &other_room,
        review("wrong-room", target.clone()),
    ));
    unavailable(db.read_agent_meeting_artifact(&b, &other_room, &target.artifact_id, 1));
    unavailable(db.publish_agent_meeting_entry(
        &b,
        &room,
        review(
            "wrong-version",
            AgentMeetingArtifactTarget {
                artifact_id: target.artifact_id.clone(),
                version: 2,
            },
        ),
    ));
    assert_eq!(
        db.read_agent_meeting(&a, &room, 0, 50)
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[test]
fn agent_meeting_parent_relationship_never_grants_membership() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("meeting.db")).unwrap();
    let parent = agent(&db, "project-one");
    let child_grant = db
        .create_isolated_agent(
            parent.owner_user(),
            parent.project(),
            Some(parent.agent_id()),
        )
        .unwrap();
    let child = db.authenticate_agent_run(&child_grant.token).unwrap();
    let room = meeting(&db, &[&child]);
    db.publish_agent_meeting_entry(&child, &room, message("child-only"))
        .unwrap();
    unavailable(db.read_agent_meeting(&parent, &room, 0, 50));
    unavailable(db.publish_agent_meeting_entry(&parent, &room, message("parent-cannot-post")));
}

#[test]
fn agent_meeting_handoff_preserves_authorized_rooms_for_task_scoped_successor() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("meeting.db")).unwrap();
    let a = agent(&db, "project-one");
    let b = agent(&db, "project-one");
    let outsider = agent(&db, "project-one");
    let foreign = agent(&db, "project-two");
    let room = meeting(&db, &[&a, &b]);
    let published = db
        .publish_agent_meeting_entry(
            &a,
            &room,
            artifact("published-v1", b"published member-only snapshot", None),
        )
        .unwrap();
    let target = published.artifact.clone().unwrap();
    let pending = db.begin_agent_run_handoff(&a).unwrap();
    // Trusted fixture assignment applies only to the newly issued pending
    // grant. This test covers membership continuity; the complete composed
    // handoff protocol has separate integration coverage.
    assert_eq!(
        db.conn_for_tests()
            .execute(
                "UPDATE wc_agent_runs SET task_goal_id=?2 WHERE run_id=?1 AND state='pending'",
                rusqlite::params![pending.run_id, format!("hgoal_{}", "a".repeat(32))],
            )
            .unwrap(),
        1
    );
    assert!(db.authenticate_agent_run(&pending.token).is_err());
    let current = {
        let mut conn = db.conn_for_tests();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        // Use the composed handoff's internal activation primitive. Public
        // generic activation must continue rejecting task-scoped grants.
        let current = crate::agent_isolation::activate_agent_run_on_connection(
            &tx,
            a.owner_user(),
            a.project(),
            a.agent_id(),
            &pending.run_id,
        )
        .unwrap();
        tx.commit().unwrap();
        current
    };
    assert_eq!(db.authenticate_agent_run(&pending.token).unwrap(), current);
    assert_eq!(current.agent_id(), a.agent_id());
    assert_ne!(current.run_id(), a.run_id());
    assert_eq!(current.generation(), a.generation() + 1);
    db.ensure_agent_host_context(&current, None).unwrap();
    assert_eq!(
        db.conn_for_tests()
            .query_row(
                "SELECT task_goal_id IS NOT NULL FROM wc_agent_runs WHERE run_id=?1",
                [current.run_id()],
                |row| row.get::<_, bool>(0),
            )
            .unwrap(),
        true
    );
    assert_eq!(
        db.list_agent_meetings(&current, 50).unwrap()[0].room_id,
        room
    );
    assert_eq!(
        db.read_agent_meeting(&current, &room, 0, 50)
            .unwrap()
            .entries,
        vec![published.clone()]
    );
    assert_eq!(
        db.read_agent_meeting_artifact(&current, &room, &target.artifact_id, 1)
            .unwrap()
            .bytes,
        b"published member-only snapshot"
    );
    assert_eq!(
        db.publish_agent_meeting_entry(
            &current,
            &room,
            artifact("published-v1", b"published member-only snapshot", None)
        )
        .unwrap(),
        published
    );
    let mut report_input = message("successor-report");
    report_input.kind = AgentMeetingKind::Report;
    let report = db
        .publish_agent_meeting_entry(&current, &room, report_input)
        .unwrap();
    let reviewed = db
        .publish_agent_meeting_entry(&current, &room, review("successor-review", target.clone()))
        .unwrap();
    for entry in [&report, &reviewed] {
        assert_eq!(entry.author_agent_id, a.agent_id());
        assert_eq!(entry.author_run_id, current.run_id());
        assert_eq!(entry.author_generation, current.generation());
    }
    assert_eq!(reviewed.review_target, Some(target.clone()));
    let before_denials = db
        .read_agent_meeting(&current, &room, 0, 50)
        .unwrap()
        .entries;
    assert_eq!(before_denials.len(), 3);
    unavailable(db.list_agent_meetings(&a, 50));
    for denied in [&a, &outsider, &foreign] {
        unavailable(db.read_agent_meeting(denied, &room, 0, 50));
        unavailable(db.read_agent_meeting_artifact(denied, &room, &target.artifact_id, 1));
        unavailable(db.publish_agent_meeting_entry(denied, &room, message("denied")));
        unavailable(db.publish_agent_meeting_entry(
            denied,
            &room,
            artifact("published-v1", b"published member-only snapshot", None),
        ));
    }
    for denied in [&outsider, &foreign] {
        assert!(db.list_agent_meetings(denied, 50).unwrap().is_empty());
    }
    let mut forged = serde_json::to_value(message("forged-author")).unwrap();
    forged["author_agent_id"] = serde_json::json!(b.agent_id());
    assert!(serde_json::from_value::<NewAgentMeetingEntry>(forged).is_err());
    assert_eq!(
        db.read_agent_meeting(&current, &room, 0, 50)
            .unwrap()
            .entries,
        before_denials
    );
    assert_eq!(db.list_agent_meetings(&b, 50).unwrap().len(), 1);
    let unscoped = db
        .publish_agent_meeting_entry(&b, &room, message("unscoped-member"))
        .unwrap();
    assert_eq!(unscoped.author_agent_id, b.agent_id());
    assert_eq!(
        db.read_agent_meeting(&b, &room, 0, 50)
            .unwrap()
            .entries
            .len(),
        4
    );
}

#[test]
fn agent_meeting_stale_run_cannot_read_write_or_replay() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("meeting.db")).unwrap();
    let old = agent(&db, "project-one");
    let room = meeting(&db, &[&old]);
    let entry = db
        .publish_agent_meeting_entry(&old, &room, artifact("first", b"snapshot", None))
        .unwrap();
    let target = entry.artifact.as_ref().unwrap();
    let pending = db.begin_agent_run_handoff(&old).unwrap();
    assert!(db.read_agent_meeting(&old, &room, 0, 50).is_ok());
    let current = db
        .activate_agent_run(
            old.owner_user(),
            old.project(),
            old.agent_id(),
            &pending.run_id,
        )
        .unwrap();
    unavailable(db.read_agent_meeting(&old, &room, 0, 50));
    unavailable(db.list_agent_meetings(&old, 50));
    unavailable(db.read_agent_meeting_artifact(&old, &room, &target.artifact_id, 1));
    unavailable(db.publish_agent_meeting_entry(&old, &room, artifact("first", b"snapshot", None)));
    unavailable(db.publish_agent_meeting_entry(&old, &room, message("stale-write")));
    assert_eq!(
        db.publish_agent_meeting_entry(&current, &room, artifact("first", b"snapshot", None))
            .unwrap(),
        entry
    );
    db.revoke_agent_run(
        current.owner_user(),
        current.project(),
        current.agent_id(),
        current.run_id(),
    )
    .unwrap();
    unavailable(db.read_agent_meeting(&current, &room, 0, 50));
}

#[test]
fn agent_meeting_draining_and_retired_contexts_cannot_publish_or_replay() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("meeting.db")).unwrap();
    let a = agent(&db, "project-one");
    let b = agent(&db, "project-one");
    let room = meeting(&db, &[&a, &b]);
    // A normal run need not enable Goal bookkeeping just to use a meeting.
    let original = db
        .publish_agent_meeting_entry(&a, &room, message("before-drain"))
        .unwrap();
    db.ensure_agent_host_context(&a, Some((1000, 2000)))
        .unwrap();
    let owner = a.host_goal_owner_key();
    db.observe_host_context(&owner, a.run_id(), "filled-context", 2000, false, None)
        .unwrap();
    for input in [message("after-drain"), message("before-drain")] {
        let error = db
            .publish_agent_meeting_entry(&a, &room, input)
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::AgentIsolationError>()
                .unwrap()
                .code(),
            "goal_context_draining"
        );
    }
    assert_eq!(
        db.read_agent_meeting(&a, &room, 0, 50).unwrap().entries,
        vec![original]
    );
    db.conn_for_tests()
        .execute(
            "UPDATE wc_host_context_states SET state=3 WHERE owner_key=?1 AND context_epoch=?2",
            rusqlite::params![owner, a.run_id()],
        )
        .unwrap();
    let error = db
        .publish_agent_meeting_entry(&a, &room, message("after-retirement"))
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<crate::AgentIsolationError>()
            .unwrap()
            .code(),
        "goal_chat_retired"
    );
    // Another member's independently active context is unaffected.
    db.publish_agent_meeting_entry(&b, &room, message("other-active-agent"))
        .unwrap();
    assert_eq!(
        db.read_agent_meeting(&b, &room, 0, 50)
            .unwrap()
            .entries
            .len(),
        2
    );
}

#[test]
fn agent_meeting_input_cannot_supply_identity_paths_or_versions() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("meeting.db")).unwrap();
    let a = agent(&db, "project-one");
    let room = meeting(&db, &[&a]);
    for field in [
        "author_agent_id",
        "author_run_id",
        "author_generation",
        "created_at_unix_ms",
        "version",
        "source_path",
        "source_url",
    ] {
        let mut input = serde_json::to_value(message("forged")).unwrap();
        input[field] = serde_json::json!("forged");
        assert!(
            serde_json::from_value::<NewAgentMeetingEntry>(input).is_err(),
            "{field}"
        );
    }
    for reference in [
        r"C:\private\notes.txt",
        "/private/notes.txt",
        r"\\host\share\notes.txt",
        "file:///private/notes.txt",
        "https://example.test/private",
        "../agent-b/notes",
    ] {
        assert_eq!(
            db.publish_agent_meeting_entry(
                &a,
                &room,
                artifact(reference, reference.as_bytes(), None)
            )
            .unwrap_err()
            .to_string(),
            "agent_meeting_artifact_reference_not_allowed"
        );
    }
    let mut oversized = message("oversized");
    oversized.content = "x".repeat(MAX_AGENT_MEETING_CONTENT_BYTES + 1);
    assert!(db
        .publish_agent_meeting_entry(&a, &room, oversized)
        .is_err());
    assert!(db
        .publish_agent_meeting_entry(
            &a,
            &room,
            artifact(
                "oversized-bytes",
                &vec![0; MAX_AGENT_MEETING_ARTIFACT_BYTES + 1],
                None
            )
        )
        .is_err());
    let mut missing_snapshot = artifact("no-bytes", b"bytes", None);
    missing_snapshot.artifact_bytes = None;
    assert!(db
        .publish_agent_meeting_entry(&a, &room, missing_snapshot)
        .is_err());
    let mut missing_review_target = message("no-target");
    missing_review_target.kind = AgentMeetingKind::Review;
    assert!(db
        .publish_agent_meeting_entry(&a, &room, missing_review_target)
        .is_err());
    assert!(db
        .read_agent_meeting(&a, &room, 0, 50)
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn agent_meeting_append_only_storage_and_metadata_audit() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("meeting.db")).unwrap();
    let a = agent(&db, "project-one");
    let room = meeting(&db, &[&a]);
    let mut input = artifact("snapshot", b"private-looking snapshot bytes", None);
    input.content = "published prose that must not enter audit".to_string();
    db.publish_agent_meeting_entry(&a, &room, input).unwrap();
    let conn = db.conn_for_tests();
    for sql in [
        "UPDATE wc_agent_meeting_entries SET content='overwrite'",
        "DELETE FROM wc_agent_meeting_entries",
        "UPDATE wc_agent_meeting_artifacts SET bytes=X'00'",
        "DELETE FROM wc_agent_meeting_artifacts",
    ] {
        assert!(conn.execute(sql, []).is_err());
    }
    let columns = conn
        .prepare("PRAGMA table_info(wc_agent_meeting_audit)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "audit_id",
            "room_id",
            "entry_id",
            "agent_id",
            "run_id",
            "generation",
            "event_kind",
            "created_at_unix_ms"
        ]
    );
    let audit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM wc_agent_meeting_audit", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(audit_count, 2);
}

#[test]
fn agent_meeting_concurrent_versions_sequences_and_replay_are_atomic() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("meeting.db");
    let db = Database::open(&path).unwrap();
    let a = agent(&db, "project-one");
    let b = agent(&db, "project-one");
    let room = meeting(&db, &[&a, &b]);
    let first = db
        .publish_agent_meeting_entry(&a, &room, artifact("first", b"version one", None))
        .unwrap();
    let artifact_id = first.artifact.unwrap().artifact_id;
    let connections = (0..6)
        .map(|_| Database::open(&path).unwrap())
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(connections.len()));
    let workers = connections
        .into_iter()
        .enumerate()
        .map(|(index, connection)| {
            let binding = a.clone();
            let room = room.clone();
            let artifact_id = artifact_id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let update = connection
                    .publish_agent_meeting_entry(
                        &binding,
                        &room,
                        artifact(
                            &format!("update-{index}"),
                            format!("content-{index}").as_bytes(),
                            Some(&artifact_id),
                        ),
                    )
                    .unwrap();
                let replay = connection
                    .publish_agent_meeting_entry(&binding, &room, message("same-operation"))
                    .unwrap();
                (update, replay)
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let mut versions = results
        .iter()
        .map(|(update, _)| update.artifact.as_ref().unwrap().version)
        .collect::<Vec<_>>();
    versions.sort();
    assert_eq!(versions, [2, 3, 4, 5, 6, 7]);
    assert!(results.iter().all(|(_, replay)| replay == &results[0].1));
    let entries = db.read_agent_meeting(&b, &room, 0, 50).unwrap().entries;
    assert_eq!(entries.len(), 8);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        (1..=8).collect::<Vec<_>>()
    );
    let mut conflict = message("same-operation");
    conflict.content = "different effect".to_string();
    assert_eq!(
        db.publish_agent_meeting_entry(&a, &room, conflict)
            .unwrap_err()
            .to_string(),
        "agent_meeting_idempotency_conflict"
    );
    // Another member may reuse a key, but the author is always their binding.
    let member_entry = db
        .publish_agent_meeting_entry(&b, &room, message("same-operation"))
        .unwrap();
    assert_eq!(member_entry.author_agent_id, b.agent_id());
    assert_ne!(member_entry.entry_id, results[0].1.entry_id);
    let page = db.read_agent_meeting(&b, &room, 0, 2).unwrap();
    assert_eq!(page.next_after_sequence, Some(2));
    assert_eq!(
        db.read_agent_meeting(&b, &room, 2, 2).unwrap().entries[0].sequence,
        3
    );
}
