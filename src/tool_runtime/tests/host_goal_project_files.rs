//! Pure path/content contracts. Runner I/O and partial publication are tested
//! separately through the real runtime's simulated Runner transport.
use super::super::host_goal_files::{markdown, sha256};
use super::super::host_goal_project_files::{
    handoff_files, metadata_fingerprint, plan_files, task_paths,
};
use serde_json::{json, Value};
use webcodex_core::host_goal::*;

fn goal(task: Option<&str>, root: &str) -> HostGoalRecord {
    HostGoalRecord {
        goal_id: "hgoal_task_test".into(),
        owner_key: "test-owner".into(),
        project: "agent:remote:project".into(),
        project_root: root.into(),
        task_directory: task.map(str::to_owned),
        objective: "Prepare a readable project report".into(),
        phase: HostGoalPhase::Executing,
        revision: 9,
        plan_version: 1,
        acceptance_contract_version: 1,
        plan_hash: Some("a".repeat(64)),
        plan_materialized: true,
        confirmation: None,
        current_node_id: Some("report".into()),
        current_attempt_id: Some("attempt_1".into()),
        fence: 1,
        completed_node_ids: vec![],
        state: HostGoalState::default(),
        created_at: 1,
        updated_at: 9,
    }
}

fn plan(goal: &HostGoalRecord) -> HostGoalPlanVersion {
    let value:HostGoalPlan=serde_json::from_value(json!({
        "objective":goal.objective,"constraints":["Preserve existing files"],"deliverables":["report.pdf"],
        "final_criteria":[{"kind":"review","id":"layout","description":"All actual pages are readable"}],
        "nodes":[{"id":"report","title":"Review report","inputs":["data.csv"],"work":"Render and review the actual report",
            "outputs":["report.pdf"],"criterion_ids":["layout"],"acceptance":[{"kind":"review","id":"layout","description":"All actual pages are readable"}],
            "failure_action":"Repair specific defects and preserve evidence"}],
        "state_management":{"facts":"Retain source facts","decisions":"Retain decisions","issues":"Retain unresolved issues",
            "artifacts":"Use stable file references","checkpoint_triggers":["At handoff"],"resume_instructions":"Read the exact saved checkpoint"}
    })).unwrap();
    HostGoalPlanVersion {
        goal_id: goal.goal_id.clone(),
        version: 1,
        preparation_revision: 2,
        acceptance_contract_version: 1,
        change_request: None,
        acceptance_changes: vec![],
        hash: "a".repeat(64),
        plan: value,
        materialization: None,
        created_at: 3,
    }
}

fn checkpoint(goal: &HostGoalRecord) -> HostGoalCheckpoint {
    HostGoalCheckpoint {
        action: Some("handoff".into()),
        source_context_epoch: Some("retired-chat-A".into()),
        identity_source: Some("window_bound".into()),
        source_fence: Some(1),
        sequence: 42,
        goal_id: goal.goal_id.clone(),
        revision: 8,
        plan_version: 1,
        state: HostGoalState {
            completed_summary: "Original checkpoint summary".into(),
            issues: vec!["Page 2 still needs review".into()],
            next_step: "Resume the original rendering Job".into(),
            ..Default::default()
        },
        reason: "Continue in a new context".into(),
        created_at: 8,
    }
}

#[test]
fn host_goal_project_files_paths_follow_exact_selected_task_without_server_path_semantics() {
    let windows = goal(Some("reports\\September"), "Q:\\Remote Project\\");
    let paths = task_paths(&windows);
    assert_eq!(paths["storage"], "runner_project");
    assert_eq!(
        paths["task_directory"],
        "Q:/Remote Project/reports/September"
    );
    assert_eq!(paths["task_relative_directory"], "reports/September");
    assert_eq!(
        paths["goals_directory"],
        "Q:/Remote Project/reports/September/goals"
    );
    assert_eq!(
        paths["handoff_directory"],
        "Q:/Remote Project/reports/September/handoff"
    );
    let files = plan_files(&windows, &plan(&windows)).unwrap();
    assert_eq!(
        files[0].relative_path,
        "reports/September/goals/hgoal_task_test/plans/v1/plan.json"
    );
    assert_eq!(
        files[0].absolute_path,
        "Q:/Remote Project/reports/September/goals/hgoal_task_test/plans/v1/plan.json"
    );
    let posix = goal(Some("."), "/srv/runner/task/");
    assert_eq!(task_paths(&posix)["task_directory"], "/srv/runner/task");
    assert_eq!(
        plan_files(&posix, &plan(&posix)).unwrap()[0].relative_path,
        "goals/hgoal_task_test/plans/v1/plan.json"
    );
    assert_eq!(
        task_paths(&goal(Some("."), "/"))["goals_directory"],
        "/goals"
    );
    assert_eq!(
        task_paths(&goal(Some("nested"), "\\\\fileserver\\share\\root"))["task_directory"],
        "//fileserver/share/root/nested"
    );
}

#[test]
fn host_goal_project_files_preserve_plan_bytes_and_separate_legacy_storage() {
    let record = goal(Some("selected"), "/actual/runner/root");
    let plan = plan(&record);
    let files = plan_files(&record, &plan).unwrap();
    let value = json!(&plan.plan);
    let mut legacy_bytes = serde_json::to_vec_pretty(&value).unwrap();
    legacy_bytes.push(b'\n');
    assert_eq!(files[0].content.as_bytes(), legacy_bytes);
    assert_eq!(files[0].sha256, sha256(&legacy_bytes));
    assert_eq!(
        serde_json::from_str::<Value>(&files[0].content).unwrap(),
        value
    );
    assert_eq!(
        files[1].content,
        markdown(&value, &record.goal_id, plan.version, &plan.hash)
    );
    let legacy = goal(None, "/actual/runner/root");
    assert!(task_paths(&legacy).is_null());
    assert_eq!(
        plan_files(&legacy, &plan).unwrap_err().output["error_kind"],
        "goal_task_file_legacy_location"
    );
}

#[test]
fn host_goal_project_files_handoff_binds_committed_checkpoint_not_current_state_or_revision() {
    let mut record = goal(Some("selected"), "/remote/root");
    let checkpoint = checkpoint(&record);
    let original = handoff_files(&record, &checkpoint).unwrap();
    assert_eq!(
        original[0].relative_path,
        "selected/handoff/hgoal_task_test/checkpoint-42/handoff.json"
    );
    let archived: Value = serde_json::from_str(&original[0].content).unwrap();
    assert_eq!(archived["schema_version"], 1);
    assert_eq!(archived["checkpoint"]["revision"], 8);
    assert_eq!(
        archived["checkpoint"]["state"]["completed_summary"],
        "Original checkpoint summary"
    );
    assert!(original[1].content.contains("Page 2 still needs review"));
    assert!(original[1].content.contains("retired-chat-A"));
    record.revision = 100;
    record.plan_version = 4;
    record.current_attempt_id = Some("successor_attempt".into());
    record.state.completed_summary = "Later conversation changed the task".into();
    let replay = handoff_files(&record, &checkpoint).unwrap();
    for (first, again) in original.iter().zip(replay.iter()) {
        assert_eq!(first.content, again.content);
        assert_eq!(first.sha256, again.sha256);
    }
}

#[test]
fn host_goal_project_files_refuse_escaping_paths_wrong_identity_and_oversized_exports() {
    for path in [
        "",
        "..",
        "../outside",
        "nested/../outside",
        "C:/other",
        "/other",
        "a//b",
        "a/./b",
    ] {
        let record = goal(Some(path), "/remote/root");
        assert!(plan_files(&record, &plan(&record)).is_err(), "{path}");
    }
    let record = goal(Some("selected"), "/remote/root");
    let mut wrong = plan(&record);
    wrong.goal_id = "different_goal".into();
    assert!(plan_files(&record, &wrong).is_err());
    wrong = plan(&record);
    wrong.version = 0;
    assert!(plan_files(&record, &wrong).is_err());
    let mut point = checkpoint(&record);
    point.goal_id = "different_goal".into();
    assert!(handoff_files(&record, &point).is_err());
    point = checkpoint(&record);
    point.action = Some("checkpoint".into());
    assert!(handoff_files(&record, &point).is_err());
    point = checkpoint(&record);
    point.state.completed_summary = "x".repeat(256 * 1024);
    assert_eq!(
        handoff_files(&record, &point).unwrap_err().output["error_kind"],
        "goal_task_file_too_large"
    );
}

#[test]
fn host_goal_project_files_metadata_cannot_confuse_missing_errors_or_producer_claims() {
    let path = "selected/goals/g/plans/v1/plan.json";
    let hash = sha256(b"actual bytes");
    assert_eq!(
        metadata_fingerprint(path, &json!({"path":path,"exists":false,"missing":true})).unwrap(),
        None
    );
    assert_eq!(
        metadata_fingerprint(
            path,
            &json!({"path":path,"exists":true,"missing":false,"bytes":12,"sha256":hash})
        )
        .unwrap(),
        Some(hash.clone())
    );
    for misleading in [
        json!({"path":path,"success":true,"sha256":hash}),
        json!({"path":path,"exists":false,"missing":true,"error":"permission denied"}),
        json!({"path":"different.json","exists":false,"missing":true}),
        json!({"path":path,"exists":true,"missing":true,"bytes":12,"sha256":hash}),
        json!({"path":path,"exists":true,"missing":false,"bytes":12,"sha256":"not-a-sha"}),
        json!({"path":path,"exists":true,"missing":false,"bytes":256*1024+1,"sha256":hash}),
    ] {
        assert!(
            metadata_fingerprint(path, &misleading).is_err(),
            "{misleading}"
        );
    }
}
