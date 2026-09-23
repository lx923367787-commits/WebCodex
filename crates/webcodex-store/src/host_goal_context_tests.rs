use super::{Database, HostGoalWindowBinding};

const OWNER: &str = "context-owner-a";
const OTHER: &str = "context-owner-b";

fn database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    (dir, db)
}

#[test]
fn host_goal_context_tests_thresholds_and_event_dedup_preserve_real_reads() {
    let (_dir, db) = database();
    let initial = db
        .observe_host_context(OWNER, "conversation-a", "first", 23_999, false, None)
        .unwrap();
    assert_eq!(initial.level, "normal");
    assert!(!initial.newly_crossed);
    let warning = db
        .observe_host_context(OWNER, "conversation-a", "second", 1, false, None)
        .unwrap();
    assert_eq!(warning.level, "warning");
    assert!(warning.newly_crossed);
    assert_eq!(warning.observed_tokens, 24_000);
    let replay = db
        .observe_host_context(OWNER, "conversation-a", "second", 1, false, None)
        .unwrap();
    assert_eq!(replay.observed_tokens, warning.observed_tokens);
    assert_eq!(replay.sample_count, 2);
    assert!(!replay.newly_crossed);
    let below = db
        .observe_host_context(OWNER, "conversation-a", "actual-reread", 7_999, true, None)
        .unwrap();
    assert_eq!(below.observed_tokens, 31_999);
    assert!(below.unmeasured_modalities);
    assert_eq!(below.level, "warning");
    assert!(!below.newly_crossed);
    let handoff = db
        .observe_host_context(OWNER, "conversation-a", "fourth", 1, false, None)
        .unwrap();
    assert_eq!(handoff.level, "handoff_recommended");
    assert!(handoff.newly_crossed);
    assert_eq!(handoff.observed_tokens, 32_000);
    assert_eq!(handoff.sample_count, 4);
    let continued = db
        .observe_host_context(OWNER, "conversation-a", "continued", 200, false, None)
        .unwrap();
    assert!(!continued.newly_crossed);
    assert_eq!(continued.observed_tokens, 32_200);
}

#[test]
fn host_goal_context_tests_restart_owner_and_conversation_isolation() {
    let (dir, db) = database();
    db.observe_host_context(OWNER, "same-conversation", "read", 25_000, true, None)
        .unwrap();
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let replay = db
        .observe_host_context(OWNER, "same-conversation", "read", 25_000, false, None)
        .unwrap();
    assert_eq!(replay.observed_tokens, 25_000);
    assert_eq!(replay.sample_count, 1);
    assert!(!replay.newly_crossed);
    assert!(replay.unmeasured_modalities);
    let other = db
        .observe_host_context(OTHER, "same-conversation", "read", 17, false, None)
        .unwrap();
    assert_eq!(other.observed_tokens, 17);
    assert!(!other.unmeasured_modalities);
    let fresh = db
        .observe_host_context(OWNER, "new-conversation", "read", 30, false, None)
        .unwrap();
    assert_eq!(fresh.observed_tokens, 30);
    assert_eq!(fresh.level, "normal");
}

#[test]
fn host_goal_context_tests_threshold_configuration_is_persistent_and_validated() {
    let (_dir, db) = database();
    for invalid in [
        (0, 1000),
        (999, 2000),
        (1000, 1000),
        (2000, 1000),
        (10_000_001, 20_000_000),
        (10_000_000, 20_000_001),
    ] {
        assert!(db
            .observe_host_context(OWNER, "config", "invalid", 1, false, Some(invalid))
            .is_err());
    }
    let first = db
        .observe_host_context(OWNER, "config", "first", 1500, false, Some((1000, 2000)))
        .unwrap();
    assert_eq!(first.level, "warning");
    assert!(first.newly_crossed);
    let next = db
        .observe_host_context(OWNER, "config", "second", 500, false, None)
        .unwrap();
    assert_eq!(next.warning_tokens, 1000);
    assert_eq!(next.handoff_tokens, 2000);
    assert_eq!(next.level, "handoff_recommended");
    let changed = db
        .observe_host_context(
            OWNER,
            "config",
            "change-config",
            0,
            false,
            Some((3000, 4000)),
        )
        .unwrap();
    assert_eq!(changed.level, "normal");
    assert_eq!(changed.observed_tokens, 2000);
    assert!(db
        .observe_host_context(OWNER, "", "missing-epoch", 1, false, None)
        .is_err());
}

#[test]
fn host_goal_context_tests_window_switch_never_resets_meter_or_unbinds_new_goal() {
    let (dir, db) = database();
    let first = HostGoalWindowBinding {
        goal_id: "goal-a".into(),
        project: "project-a".into(),
        project_root: "/projects/a".into(),
        context_epoch: "conversation".into(),
    };
    let second = HostGoalWindowBinding {
        goal_id: "goal-b".into(),
        project: "project-b".into(),
        project_root: "/projects/b".into(),
        context_epoch: "conversation".into(),
    };
    db.bind_host_goal_window(OWNER, "window", &first).unwrap();
    db.observe_host_context(OWNER, "conversation", "first-goal", 5000, false, None)
        .unwrap();
    db.bind_host_goal_window(OWNER, "window", &second).unwrap();
    db.unbind_host_goal_window(OWNER, "window", "goal-a")
        .unwrap();
    assert_eq!(
        db.host_goal_window(OWNER, "window").unwrap(),
        Some(second.clone())
    );
    assert!(db.host_goal_window(OTHER, "window").unwrap().is_none());
    let after = db
        .observe_host_context(OWNER, "conversation", "second-goal", 7000, false, None)
        .unwrap();
    assert_eq!(after.observed_tokens, 12_000);
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(db.host_goal_window(OWNER, "window").unwrap(), Some(second));
    db.unbind_host_goal_window(OTHER, "window", "goal-b")
        .unwrap();
    assert!(db.host_goal_window(OWNER, "window").unwrap().is_some());
    db.unbind_host_goal_window(OWNER, "window", "goal-b")
        .unwrap();
    assert!(db.host_goal_window(OWNER, "window").unwrap().is_none());
}

#[test]
fn host_goal_context_tests_accumulation_saturates_without_integer_overflow() {
    let (_dir, db) = database();
    let large = db
        .observe_host_context(OWNER, "saturation", "first", u64::MAX, false, None)
        .unwrap();
    assert_eq!(large.observed_tokens, 1_000_000_000_000);
    let larger = db
        .observe_host_context(OWNER, "saturation", "second", u64::MAX, false, None)
        .unwrap();
    assert_eq!(larger.observed_tokens, large.observed_tokens);
    assert_eq!(larger.sample_count, 2);
}

#[test]
fn host_goal_context_tests_panel_reads_latest_chat_without_counting_or_consuming_warning() {
    let (dir, db) = database();
    let goal = db
        .create_host_goal_at(
            OWNER,
            "project",
            &dir.path().to_string_lossy(),
            "Long running analysis",
            "create",
            1,
        )
        .unwrap()
        .goal;
    assert!(db
        .read_host_goal_context_meter(OWNER, &goal.goal_id)
        .unwrap()
        .is_none());
    db.observe_host_context(OWNER, "chat-one", "first", 23_999, false, None)
        .unwrap();
    db.record_host_goal_context_ref(OWNER, &goal.goal_id, "chat-one")
        .unwrap();
    let one = db
        .read_host_goal_context_meter(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(one.observed_tokens, 23_999);
    assert_eq!(one.sample_count, 1);
    assert_eq!(
        db.read_host_goal_context_meter(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap(),
        one
    );
    let warning = db
        .observe_host_context(OWNER, "chat-one", "second", 1, false, None)
        .unwrap();
    assert!(warning.newly_crossed);
    let panel = db
        .read_host_goal_context_meter(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(panel.level, "warning");
    assert!(!panel.newly_crossed);
    assert_eq!(panel.sample_count, 2);
    assert!(db
        .read_host_goal_context_meter(OTHER, &goal.goal_id)
        .unwrap()
        .is_none());
    db.observe_host_context(OTHER, "chat-one", "other", 15, false, None)
        .unwrap();
    assert!(db
        .record_host_goal_context_ref(OTHER, &goal.goal_id, "chat-one")
        .is_err());
    db.observe_host_context(OWNER, "chat-two", "first", 120, false, None)
        .unwrap();
    db.record_host_goal_context_ref(OWNER, &goal.goal_id, "chat-two")
        .unwrap();
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let recovered = db
        .read_host_goal_context_meter(OWNER, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.context_epoch, "chat-two");
    assert_eq!(recovered.observed_tokens, 120);
    db.record_host_goal_context_ref(OWNER, &goal.goal_id, "chat-one")
        .unwrap();
    assert_eq!(
        db.read_host_goal_context_meter(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .observed_tokens,
        24_000
    );
}

#[test]
fn host_goal_context_tests_node_read_receipts_are_revision_node_and_conversation_specific() {
    let (dir, db) = database();
    let goal = db
        .create_host_goal_at(
            OWNER,
            "project",
            &dir.path().to_string_lossy(),
            "Read before resuming",
            "create",
            1,
        )
        .unwrap()
        .goal;
    assert!(!db
        .host_goal_context_read_matches(OWNER, "chat-a", &goal.goal_id, goal.revision, 0, "node-a")
        .unwrap());
    db.ack_host_goal_context_read(OWNER, "chat-a", &goal.goal_id, goal.revision, 0, "node-a")
        .unwrap();
    assert!(db
        .host_goal_context_read_matches(OWNER, "chat-a", &goal.goal_id, goal.revision, 0, "node-a")
        .unwrap());
    assert!(!db
        .host_goal_context_read_matches(OWNER, "chat-b", &goal.goal_id, goal.revision, 0, "node-a")
        .unwrap());
    assert!(!db
        .host_goal_context_read_matches(OWNER, "chat-a", &goal.goal_id, goal.revision, 0, "node-b")
        .unwrap());
    assert!(!db
        .host_goal_context_read_matches(OTHER, "chat-a", &goal.goal_id, goal.revision, 0, "node-a")
        .unwrap());
    assert!(db
        .ack_host_goal_context_read(OTHER, "chat-a", &goal.goal_id, goal.revision, 0, "node-a")
        .is_err());
    assert!(db
        .ack_host_goal_context_read(OWNER, "chat-a", &goal.goal_id, goal.revision, 1, "node-a")
        .is_err());
    let current = db
        .checkpoint_host_goal(
            OWNER,
            &goal.goal_id,
            goal.revision,
            None,
            &goal.state,
            super::HostGoalCheckpointAction::Progress,
            "Read updated requirements",
            "progress",
            2,
        )
        .unwrap()
        .goal;
    assert!(!db
        .host_goal_context_read_matches(
            OWNER,
            "chat-a",
            &goal.goal_id,
            current.revision,
            0,
            "node-a"
        )
        .unwrap());
    assert!(!db
        .host_goal_context_read_matches(OWNER, "chat-a", &goal.goal_id, goal.revision, 0, "node-a")
        .unwrap());
    db.ack_host_goal_context_read(
        OWNER,
        "chat-a",
        &goal.goal_id,
        current.revision,
        0,
        "node-a",
    )
    .unwrap();
    db.ack_host_goal_context_read(OWNER, "chat-a", &goal.goal_id, goal.revision, 0, "node-a")
        .unwrap();
    assert!(db
        .host_goal_context_read_matches(
            OWNER,
            "chat-a",
            &goal.goal_id,
            current.revision,
            0,
            "node-a"
        )
        .unwrap());
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    assert!(db
        .host_goal_context_read_matches(
            OWNER,
            "chat-a",
            &goal.goal_id,
            current.revision,
            0,
            "node-a"
        )
        .unwrap());
}
