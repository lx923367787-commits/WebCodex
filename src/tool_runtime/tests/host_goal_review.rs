//! Real runtime/Runner file and Job boundaries with deliberately simulated
//! model verdicts. Actual visual quality is exercised separately in the PDF E2E.
use super::goals::{goal_auth, Behavior, Fixture};
use super::host_goal_check_recovery::prepare_with_review as prepare_legacy_review;
use crate::tool_runtime::{ToolCall, ToolResult};
use serde_json::{json, Value};
use std::time::Duration;
use webcodex_core::host_goal::*;

async fn prepare_with_review(
    behavior: Behavior,
    mutate_during_final: bool,
    review_required: bool,
) -> (Fixture, HostGoalRecord, HostGoalExecutionBinding) {
    let (fixture, goal, old) = Box::pin(prepare_legacy_review(
        behavior,
        mutate_during_final,
        review_required,
    ))
    .await;
    let context = HostGoalInvocationContext {
        context_epoch: "review-repair-chat".into(),
        identity_source: "explicit_context_only".into(),
    };
    fixture
        .db
        .ensure_host_context_identity(&goal.owner_key, &context)
        .unwrap();
    let claim = fixture
        .db
        .claim_host_goal_node_for_plan_in_context(
            &goal.owner_key,
            &goal.goal_id,
            goal.revision,
            &old.node_id,
            false,
            goal.plan_version,
            Some(&old.attempt_id),
            "review-context",
            7,
            Some(&context),
        )
        .unwrap();
    (fixture, claim.goal, claim.attempt.binding)
}

async fn check(
    fixture: &Fixture,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
    key: &str,
    evidence: Option<Value>,
) -> ToolResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let mut metadata = json!(binding);
        metadata["operation_id"] = json!(key);
        metadata["context_epoch"] = json!("review-repair-chat");
        if let Some(repair) = fixture
            .db
            .get_host_goal_repair(&goal.owner_key, &goal.goal_id)
            .unwrap()
            .filter(HostGoalRepair::active)
        {
            metadata["repair"] = json!({"repair_id":repair.repair_id,
                "diagnosis":"Inspect the saved review findings, correct the cited source, then recheck the original machine and review requirements"});
        }
        let scope = crate::tool_runtime::host_goal_context::RequestScope::new(
            goal.owner_key.clone(),
            None,
            &json!({"goal_context":metadata}),
        )
        .unwrap();
        let result = crate::tool_runtime::host_goal_context::scope(
            scope,
            Box::pin(fixture.runtime.dispatch_with_auth(
                ToolCall::GoalCheckNode {
                    goal_id: goal.goal_id.clone(),
                    node_id: binding.node_id.clone(),
                    attempt_id: binding.attempt_id.clone(),
                    expected_revision: goal.revision,
                    plan_version: goal.plan_version,
                    idempotency_key: key.into(),
                    evidence: evidence.clone(),
                },
                Some(&goal_auth()),
            )),
        )
        .await;
        if result.output["error_kind"] != "goal_check_pending" {
            return result;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Review checks did not resolve: {result:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn verdict(packet: &Value, passed: bool) -> Value {
    json!([{
        "criterion_id":"__delivery_review", "passed":passed, "source":"model_self_check",
        "review_packet_id":packet["packet_id"], "artifacts":packet["manifest"],
        "reviewed_scopes":packet["required_scopes"], "source_refs":["result.txt"],
        "evidence":if passed {
            "Reviewed the actual result.txt text and successful command evidence in this packet; the corrected conclusion matches its supplied source and no unresolved issue remains."
        } else {
            "Reviewed the actual result.txt text and command evidence; line 1 omits the needed supporting explanation, so the conclusion cannot yet be delivered."
        },
        "findings":if passed {json!([])} else {json!([{
            "location":"result.txt:1", "impact":"Conclusion lacks support for the reported result",
            "required_change":"Include the supporting calculation and rerun the actual checks"
        }])}
    }])
}

fn ledger(fixture: &Fixture, goal: &HostGoalRecord) -> HostGoalReviewRecord {
    fixture
        .db
        .read_host_goal_review(&goal.owner_key, &goal.goal_id, "verify")
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn host_goal_review_generation_stays_pending_and_first_fresh_verdict_completes() {
    let (fixture, goal, binding) = prepare_with_review(Behavior::Accept, false, true).await;
    let pending = check(&fixture, &goal, &binding, "prepare", None).await;
    assert!(pending.success, "{pending:?}");
    assert_eq!(pending.output["goal"]["phase"], "executing");
    let packet = &pending.output["review_packet"];
    assert!(packet["materials"][0]["content"]
        .as_str()
        .unwrap()
        .contains("accepted final result"));
    assert_eq!(packet["required_scopes"], packet["delivered_scopes"]);
    assert!(ledger(&fixture, &goal).rounds.is_empty());
    let jobs = fixture.command_jobs();
    assert_eq!(
        jobs.len(),
        1,
        "Machine validation still runs before model review"
    );
    let completed = check(
        &fixture,
        &goal,
        &binding,
        "first-verdict",
        Some(verdict(packet, true)),
    )
    .await;
    assert!(completed.success, "{completed:?}");
    assert_eq!(completed.output["goal"]["phase"], "completed");
    let reviewed = ledger(&fixture, &goal);
    assert_eq!(reviewed.rounds.len(), 1);
    assert!(reviewed.rounds[0].passed);
    assert_eq!(
        fixture.command_jobs(),
        jobs,
        "Same-version Job is recovered, not redispatched"
    );
}

#[tokio::test]
async fn host_goal_review_failed_feedback_requires_new_material_and_preserves_both_rounds() {
    let (fixture, goal, binding) = prepare_with_review(Behavior::Accept, false, true).await;
    let first = check(&fixture, &goal, &binding, "prepare-one", None).await;
    assert!(first.success, "{first:?}");
    let rejected = check(
        &fixture,
        &goal,
        &binding,
        "reject-one",
        Some(verdict(&first.output["review_packet"], false)),
    )
    .await;
    assert!(rejected.success, "{rejected:?}");
    assert_eq!(rejected.output["goal"]["phase"], "executing");
    std::fs::write(
        fixture.temp.path().join("result.txt"),
        "accepted final result; calculation 960 / 1000 = 96%\n",
    )
    .unwrap();
    let second = check(&fixture, &goal, &binding, "prepare-two", None).await;
    assert!(second.success, "{second:?}");
    assert_ne!(
        first.output["review_packet"]["manifest"],
        second.output["review_packet"]["manifest"]
    );
    assert_ne!(
        first.output["review_packet"]["packet_id"],
        second.output["review_packet"]["packet_id"]
    );
    let completed = check(
        &fixture,
        &goal,
        &binding,
        "accept-two",
        Some(verdict(&second.output["review_packet"], true)),
    )
    .await;
    assert!(completed.success, "{completed:?}");
    assert_eq!(completed.output["goal"]["phase"], "completed");
    let reviewed = ledger(&fixture, &goal);
    assert_eq!(reviewed.rounds.len(), 2);
    assert!(!reviewed.rounds[0].passed);
    assert_eq!(
        reviewed.rounds[0].evidence[0].findings[0].location,
        "result.txt:1"
    );
    assert!(reviewed.rounds[1].passed);
    assert_eq!(
        fixture.command_jobs().len(),
        2,
        "Changed source must receive fresh command validation"
    );
}

#[tokio::test]
async fn host_goal_review_late_old_verdict_cannot_be_restamped_after_actual_file_edit() {
    let (fixture, goal, binding) = prepare_with_review(Behavior::Accept, false, true).await;
    let first = check(&fixture, &goal, &binding, "prepare-original", None).await;
    assert!(first.success, "{first:?}");
    std::fs::write(
        fixture.temp.path().join("result.txt"),
        "a different unreviewed artifact\n",
    )
    .unwrap();
    let late = check(
        &fixture,
        &goal,
        &binding,
        "late-old-verdict",
        Some(verdict(&first.output["review_packet"], true)),
    )
    .await;
    assert!(!late.success);
    assert_eq!(late.output["error_kind"], "goal_review_stale");
    assert!(ledger(&fixture, &goal).rounds.is_empty());
    assert_eq!(
        fixture
            .db
            .get_host_goal(&goal.owner_key, &goal.goal_id)
            .unwrap()
            .unwrap()
            .phase,
        HostGoalPhase::Executing
    );
}

#[tokio::test]
async fn host_goal_review_three_failures_remain_blocked_and_material_reads_do_not_spend_rounds() {
    let (fixture, goal, binding) = prepare_with_review(Behavior::Accept, false, true).await;
    for number in 1..=3 {
        let prepared = check(
            &fixture,
            &goal,
            &binding,
            &format!("prepare-{number}"),
            None,
        )
        .await;
        assert!(prepared.success, "{prepared:?}");
        let repeated = check(
            &fixture,
            &goal,
            &binding,
            &format!("prepare-{number}"),
            None,
        )
        .await;
        assert!(repeated.success, "{repeated:?}");
        assert_eq!(
            prepared.output["review_packet"]["packet_id"],
            repeated.output["review_packet"]["packet_id"]
        );
        assert_eq!(ledger(&fixture, &goal).rounds.len(), number - 1);
        let rejected = check(
            &fixture,
            &goal,
            &binding,
            &format!("reject-{number}"),
            Some(verdict(&prepared.output["review_packet"], false)),
        )
        .await;
        assert!(rejected.success, "{rejected:?}");
        assert_eq!(rejected.output["goal"]["phase"], "executing");
    }
    let exhausted = check(&fixture, &goal, &binding, "prepare-four", None).await;
    assert!(
        !exhausted.success,
        "Round limit must block new material: {exhausted:?}"
    );
    let reviewed = ledger(&fixture, &goal);
    assert_eq!(reviewed.rounds.len(), 3);
    assert!(reviewed
        .rounds
        .iter()
        .all(|round| !round.passed && !round.evidence[0].findings.is_empty()));
    assert_eq!(
        fixture
            .db
            .get_host_goal(&goal.owner_key, &goal.goal_id)
            .unwrap()
            .unwrap()
            .phase,
        HostGoalPhase::Executing
    );
}
