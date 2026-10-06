use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};

fn legacy_node() -> serde_json::Value {
    json!({"id":"one","title":"Replace exact text","inputs":[],"verification_paths":[],"work":"Replace an exact occurrence","outputs":["result.txt"],"criterion_ids":["check"],"acceptance":[{"kind":"file_contains","id":"check","description":"Exact replacement is present","path":"result.txt","text":"replacement"}],"failure_action":"Repair the exact replacement"})
}

#[test]
fn host_goal_review_accepts_only_integer_zero_or_one() {
    for accepted in [json!(0), json!(1)] {
        let mut value = legacy_node();
        value["review"] = accepted;
        assert!(serde_json::from_value::<HostGoalNode>(value).is_ok());
    }
    for rejected in [
        json!(false),
        json!(true),
        json!(0.0),
        json!(1.0),
        json!(-1),
        json!(2),
        json!("1"),
        json!(null),
    ] {
        let mut value = legacy_node();
        value["review"] = rejected.clone();
        assert!(
            serde_json::from_value::<HostGoalNode>(value).is_err(),
            "accepted {rejected}"
        );
    }
}

#[test]
fn host_goal_review_historical_serialization_does_not_add_default_fields_or_change_hash() {
    let value = legacy_node();
    let original = serde_json::to_vec(&value).unwrap();
    let node: HostGoalNode = serde_json::from_value(value.clone()).unwrap();
    let serialized = serde_json::to_value(&node).unwrap();
    assert_eq!(serialized, value);
    assert_eq!(
        Sha256::digest(original),
        Sha256::digest(serde_json::to_vec(&serialized).unwrap())
    );
    assert!(node.review.is_none());
    assert_eq!(
        host_goal_review_policy(&node).review,
        1,
        "missing historical record is conservative, never fabricated approval"
    );
}

#[test]
fn host_goal_review_lightweight_is_machine_checked_and_layout_overrides_zero() {
    let mut node: HostGoalNode = serde_json::from_value(legacy_node()).unwrap();
    node.delivery_kind = Some(HostGoalDeliveryKind::Lightweight);
    assert_eq!(host_goal_review_policy(&node).review, 0);
    node.review = Some(1);
    assert_eq!(host_goal_review_policy(&node).review, 1);
    node.review = Some(0);
    node.work = "Change one title in the slide deck".into();
    assert_eq!(host_goal_review_policy(&node).review, 1);
    assert!(host_goal_review_policy(&node).visual);
    node.work = "Replace exact text".into();
    node.managed_artifacts = Some(vec![HostGoalManagedArtifact {
        path: "report.xlsx".into(),
        kind: HostGoalDeliveryKind::Data,
    }]);
    assert_eq!(host_goal_review_policy(&node).review, 1);
    assert!(host_goal_review_policy(&node).visual);
    for extension in ["webp", "gif", "bmp", "tif", "tiff", "xlsm", "pptm", "docm"] {
        node.managed_artifacts = None;
        node.outputs = vec![format!("actual.{extension}")];
        assert!(host_goal_review_policy(&node).visual, "{extension}");
        assert_eq!(host_goal_review_policy(&node).review, 1);
    }
}

#[test]
fn host_goal_review_read_only_layout_input_stays_lightweight_but_edits_and_cross_file_work_do_not()
{
    let mut node: HostGoalNode = serde_json::from_value(legacy_node()).unwrap();
    node.title = "Read existing values".into();
    node.review = Some(0);
    node.delivery_kind = Some(HostGoalDeliveryKind::Lightweight);
    for work in [
        "仅读取图表统计值",
        "Read the existing PDF page count",
        "Read a chart label",
    ] {
        node.work = work.into();
        assert_eq!(host_goal_review_policy(&node).review, 0, "{work}");
        assert!(!host_goal_review_policy(&node).visual, "{work}");
    }
    for work in [
        "修改PPT的一个标题",
        "Edit a Word document title",
        "Generate an Excel report",
    ] {
        node.work = work.into();
        assert_eq!(host_goal_review_policy(&node).review, 1, "{work}");
        assert!(host_goal_review_policy(&node).visual, "{work}");
    }
    node.work = "跨文件修改复杂编程任务".into();
    assert_eq!(host_goal_review_policy(&node).review, 1);
    assert!(!host_goal_review_policy(&node).visual);
}
