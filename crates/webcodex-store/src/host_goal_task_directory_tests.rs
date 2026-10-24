use super::*;
use sha2::{Digest, Sha256};

#[test]
fn host_goal_task_directory_is_canonical_persistent_and_part_of_creation_identity() {
    let (dir, db) = database();
    let created = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            r"tasks\report-a",
            "Analyze the selected task",
            "task-create",
            NOW,
            None,
        )
        .unwrap();
    assert_eq!(
        created.goal.task_directory.as_deref(),
        Some("tasks/report-a")
    );
    let replay = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            "tasks/report-a",
            "Analyze the selected task",
            "task-create",
            NOW + 1,
            None,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.goal.goal_id, created.goal.goal_id);
    let conflict = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            "tasks/report-b",
            "Analyze the selected task",
            "task-create",
            NOW + 2,
            None,
        )
        .unwrap_err();
    assert_eq!(conflict.code(), "goal_idempotency_conflict");
    let root = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            ".",
            "Analyze root task",
            "root-create",
            NOW + 3,
            None,
        )
        .unwrap();
    assert_eq!(root.goal.task_directory.as_deref(), Some("."));
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(
        db.get_host_goal(OWNER, &created.goal.goal_id)
            .unwrap()
            .unwrap()
            .task_directory
            .as_deref(),
        Some("tasks/report-a")
    );
    assert_eq!(
        db.get_host_goal(OWNER, &root.goal.goal_id)
            .unwrap()
            .unwrap()
            .task_directory
            .as_deref(),
        Some(".")
    );
    let after_restart = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            "tasks/report-a",
            "Analyze the selected task",
            "task-create",
            NOW + 4,
            None,
        )
        .unwrap();
    assert!(after_restart.replayed);
    assert_eq!(after_restart.goal.goal_id, created.goal.goal_id);
    let root_replay = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            ".",
            "Analyze root task",
            "root-create",
            NOW + 4,
            None,
        )
        .unwrap();
    assert!(root_replay.replayed);
    assert_eq!(root_replay.goal.task_directory.as_deref(), Some("."));
}

#[test]
fn host_goal_task_directory_legacy_missing_field_and_request_fingerprint_stay_unchanged() {
    let (_dir, db) = database();
    let legacy = new_goal(&db);
    assert!(legacy.task_directory.is_none());
    let json = serde_json::to_value(&legacy).unwrap();
    assert!(json.get("task_directory").is_none());
    let decoded: HostGoalRecord = serde_json::from_value(json.clone()).unwrap();
    assert!(decoded.task_directory.is_none());
    assert_eq!(serde_json::to_value(decoded).unwrap(), json);
    let old_request = serde_json::to_string(&(
        "project",
        "/workspace/project",
        "Compare documented alternatives",
    ))
    .unwrap();
    let expected = format!("{:x}", Sha256::digest(old_request.as_bytes()));
    let actual: String = db
        .conn_for_tests()
        .query_row(
            "SELECT request_hash FROM wc_host_goal_replays WHERE owner_key=?1 AND scope='create'",
            [OWNER],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        actual, expected,
        "legacy None must retain the original three-field request fingerprint"
    );
    assert!(
        db.create_host_goal_at(
            OWNER,
            "project",
            "/workspace/project",
            "Compare documented alternatives",
            "create",
            NOW + 1
        )
        .unwrap()
        .replayed
    );
    let upgraded_replay = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            ".",
            "Compare documented alternatives",
            "create",
            NOW + 1,
            None,
        )
        .unwrap();
    assert!(upgraded_replay.replayed);
    assert_eq!(upgraded_replay.goal.goal_id, legacy.goal_id);
    assert!(upgraded_replay.goal.task_directory.is_none());
    assert_eq!(
        serde_json::to_value(db.get_host_goal(OWNER, &legacy.goal_id).unwrap().unwrap()).unwrap(),
        json
    );
    for (directory, objective) in [
        ("tasks/new", "Compare documented alternatives"),
        (".", "Changed objective"),
    ] {
        assert_eq!(
            db.create_host_goal_in_task_directory_in_context(
                OWNER,
                "project",
                "/workspace/project",
                directory,
                objective,
                "create",
                NOW + 1,
                None
            )
            .unwrap_err()
            .code(),
            "goal_idempotency_conflict"
        );
    }
    let local = db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            "tasks/current",
            "Compare documented alternatives",
            "local-create",
            NOW,
            None,
        )
        .unwrap()
        .goal;
    let old_plan = db
        .save_host_goal_plan(
            OWNER,
            &legacy.goal_id,
            legacy.revision,
            &plan(),
            None,
            "old-plan",
            NOW,
        )
        .unwrap();
    let local_plan = db
        .save_host_goal_plan(
            OWNER,
            &local.goal_id,
            local.revision,
            &plan(),
            None,
            "local-plan",
            NOW,
        )
        .unwrap();
    assert_eq!(
        old_plan.plan.hash, local_plan.plan.hash,
        "task location is not part of the immutable plan contents or confirmation hash"
    );
    assert_eq!(
        db.get_host_goal_plan(OWNER, &legacy.goal_id, None)
            .unwrap()
            .unwrap()
            .hash,
        old_plan.plan.hash
    );
}

#[test]
fn host_goal_task_directory_rejects_paths_outside_the_project_or_ambiguous_components() {
    let (_dir, db) = database();
    for path in [
        "",
        "../outside",
        "a/../outside",
        "a/./b",
        "a//b",
        "a/",
        "/outside",
        r"\outside",
        r"\\server\share",
        r"C:\outside",
        "C:outside",
        "a\0b",
    ] {
        let error = db
            .create_host_goal_in_task_directory_in_context(
                OWNER,
                "project",
                "/workspace/project",
                path,
                "Analyze",
                "invalid",
                NOW,
                None,
            )
            .unwrap_err();
        assert_eq!(error.code(), "invalid_goal", "{path:?}");
    }
    assert!(db
        .create_host_goal_in_task_directory_in_context(
            OWNER,
            "project",
            "/workspace/project",
            &"a".repeat(1025),
            "Analyze",
            "too-long",
            NOW,
            None
        )
        .is_err());
    assert!(db.list_host_goals(OWNER, None, 100, 0).unwrap().is_empty());
}
