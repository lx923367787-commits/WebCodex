use super::super::host_goal_files::{materialize, sha256, verify};
use serde_json::{json, Value};
use std::fs;

fn plan() -> Value {
    json!({
        "objective": "Create a source-backed comparison",
        "constraints": ["Primary sources"],
        "deliverables": ["Comparison document"],
        "final_criteria": [{"kind":"review","id":"evidence","description":"Each conclusion has a cited source"}],
        "state_management": {"facts":"Record sources","decisions":"Explain choices","issues":"Record open questions","artifacts":"Keep paths","checkpoint_triggers":["After research"],"resume_instructions":"Read current progress"},
        "nodes": [{"id":"research","title":"Read sources","inputs":["Official docs"],"work":"Compare documented alternatives","outputs":["Comparison"],"criterion_ids":["evidence"],"acceptance":[{"kind":"review","id":"evidence","description":"Each conclusion has a cited source"}],"failure_action":"Research missing evidence"}]
    })
}

#[test]
fn host_goal_files_materialization_is_reviewable_complete_and_replayable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("goals");
    let value = plan();
    let hash = sha256(&serde_json::to_vec(&value).unwrap());
    let first = materialize(&root, "hgoal_test", 1, &hash, &value).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&first.json_path).unwrap()).unwrap(),
        value
    );
    let markdown = fs::read_to_string(&first.markdown_path).unwrap();
    for required in [
        "最终目标",
        "最终验收目标与方式",
        "State 管理与恢复规则",
        "逐步执行与节点验收",
        "明确通知开始执行此版本",
        "保存计划不代表已经确认执行",
    ] {
        assert!(
            markdown.contains(required),
            "Missing reviewable plan section: {required}"
        );
    }
    assert!(markdown.contains(&hash));
    verify(&root, "hgoal_test", 1, &first).unwrap();
    let before = fs::metadata(&first.json_path).unwrap().modified().unwrap();
    let replay = materialize(&root, "hgoal_test", 1, &hash, &value).unwrap();
    assert_eq!(first.json_sha256, replay.json_sha256);
    assert_eq!(first.markdown_sha256, replay.markdown_sha256);
    assert_eq!(
        before,
        fs::metadata(&first.json_path).unwrap().modified().unwrap()
    );
    let versions = fs::read_dir(root.join("hgoal_test").join("plans"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        versions.len(),
        1,
        "Publication must not leave staging directories"
    );
    assert_eq!(versions[0].file_name(), "v1");
}

#[test]
fn host_goal_files_tamper_blocks_verification_and_is_not_silently_overwritten() {
    for file_name in ["plan.json", "plan.md"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("goals");
        let artifacts = materialize(&root, "hgoal_test", 1, "fixed-hash", &plan()).unwrap();
        let changed = root.join("hgoal_test/plans/v1").join(file_name);
        fs::write(&changed, b"unexpected local edit").unwrap();
        assert!(verify(&root, "hgoal_test", 1, &artifacts).is_err());
        assert!(materialize(&root, "hgoal_test", 1, "fixed-hash", &plan()).is_err());
        assert_eq!(fs::read(&changed).unwrap(), b"unexpected local edit");
    }
}

#[test]
fn host_goal_files_partial_or_mismatched_publication_never_passes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("goals");
    let first = materialize(&root, "hgoal_test", 1, "fixed-hash", &plan()).unwrap();
    fs::remove_file(&first.markdown_path).unwrap();
    assert!(verify(&root, "hgoal_test", 1, &first).is_err());
    assert!(materialize(&root, "hgoal_test", 1, "fixed-hash", &plan()).is_err());
    let second = materialize(&root, "hgoal_test", 2, "fixed-hash", &plan()).unwrap();
    assert!(verify(&root, "hgoal_test", 1, &second).is_err());
    let mut changed = plan();
    changed["objective"] = json!("Changed after publication");
    assert!(materialize(&root, "hgoal_test", 2, "fixed-hash", &changed).is_err());
}

#[test]
fn host_goal_files_identity_cannot_escape_local_plan_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("goals");
    for invalid in [
        "",
        "..",
        "../escape",
        "..\\escape",
        "C:\\escape",
        "/escape",
        "goal/other",
        "goal\\other",
    ] {
        assert!(materialize(&root, invalid, 1, "hash", &plan()).is_err());
    }
    assert!(materialize(&root, "valid_goal", 0, "hash", &plan()).is_err());
    assert!(!root.exists());
}

#[test]
fn host_goal_files_verify_missing_root_does_not_create_directories() {
    let dir = tempfile::tempdir().unwrap();
    let existing_root = dir.path().join("existing");
    let artifacts = materialize(&existing_root, "hgoal_test", 1, "hash", &plan()).unwrap();
    let missing_root = dir.path().join("missing");
    assert!(verify(&missing_root, "hgoal_test", 1, &artifacts).is_err());
    assert!(!missing_root.exists(), "Verification is read-only");
}

#[test]
fn host_goal_files_regular_file_cannot_replace_version_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("goals");
    let first = materialize(&root, "hgoal_test", 1, "hash", &plan()).unwrap();
    let blocked = root.join("hgoal_test/plans/v2");
    fs::write(&blocked, b"not a directory").unwrap();
    assert!(materialize(&root, "hgoal_test", 2, "hash", &plan()).is_err());
    assert!(verify(&root, "hgoal_test", 2, &first).is_err());
    assert_eq!(fs::read(&blocked).unwrap(), b"not a directory");
}

#[cfg(unix)]
#[test]
fn host_goal_files_rejects_symlinked_plan_roots() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let root = dir.path().join("goals");
    std::os::unix::fs::symlink(&outside, &root).unwrap();
    assert!(materialize(&root, "hgoal_test", 1, "hash", &plan()).is_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}
