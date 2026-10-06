//! Fixture-only runtime recovery: the new chat observes the original native Run.
use super::*;
use webcodex_core::host_goal::HostGoalOperationStatus;

fn node_read(goal: &Value) -> ToolCall {
    ToolCall::GoalRead {
        goal_id: id(goal),
        round_offset: None,
        round_limit: None,
        history_after: None,
        history_limit: None,
        history_kind: None,
        view: Some("node".into()),
        node_id: Some("research".into()),
        section: None,
        cursor: None,
        source_context_epoch: None,
    }
}

fn native_successor_epoch(fixture: &super::super::goals::Fixture) -> String {
    format!("{}-successor", native_host_epoch(fixture))
}

async fn target_call(fixture: &super::super::goals::Fixture, call: ToolCall) -> ToolResult {
    let epoch = native_successor_epoch(fixture);
    dispatch_in_chat(
        &fixture.runtime,
        &super::super::goals::goal_auth(),
        call,
        &epoch,
        None,
    )
    .await
}

#[tokio::test]
async fn host_goals_handoff_running_native_run_is_observed_cancelled_and_reconciled_before_claim() {
    let (fixture, goal, binding, _) =
        native_host_setup(super::super::goals::Behavior::RunningUntilCancelled).await;
    let auth = super::super::goals::goal_auth();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    let started = native_host_call(
        &fixture,
        native_start(&fixture.project, "handoff-original-run"),
        Some(&binding),
    )
    .await;
    success(&started);
    let run_id = started.output["run_id"].as_str().unwrap().to_owned();
    let saved=native_host_call(&fixture,ToolCall::GoalCheckpoint {
        goal_id:id(&goal),expected_revision:revision(&goal),idempotency_key:"native-handoff".into(),
        context_epoch:Some("native-chat".into()),patch:json!({"action":"handoff","reason":"Successor must observe the same running native Run"}),
    },Some(&binding)).await;
    success(&saved);
    assert_eq!(
        fixture
            .db
            .read_host_context_state(&owner, &native_host_epoch(&fixture))
            .unwrap()
            .unwrap()
            .state,
        "retired"
    );
    let pending = fixture
        .db
        .get_host_goal_operation(&owner, &id(&goal), "checkpoint-operation")
        .unwrap()
        .unwrap();
    assert_eq!(pending.status, HostGoalOperationStatus::Running);
    assert_eq!(
        serde_json::from_str::<Value>(pending.result_summary.as_deref().unwrap()).unwrap()
            ["run_id"],
        run_id
    );

    let denied = native_host_call(
        &fixture,
        ToolCall::CodingAgentCancel {
            run_id: run_id.clone(),
        },
        None,
    )
    .await;
    assert_eq!(denied.output["error_kind"], "goal_chat_retired");
    let bootstrap = target_call(
        &fixture,
        ToolCall::GoalBootstrap {
            goal_id: Some(id(&goal)),
            project: None,
            context_epoch: Some("native-successor".into()),
            activate: true,
        },
    )
    .await;
    success(&bootstrap);
    assert_eq!(bootstrap.output["goal"]["goal_id"], goal["goal_id"]);
    let observed = target_call(
        &fixture,
        ToolCall::CodingAgentObserve {
            run_id: run_id.clone(),
            after_observation_token: None,
            wait_secs: Some(0),
        },
    )
    .await;
    success(&observed);
    assert_eq!(observed.output["run_id"], run_id);
    assert_eq!(observed.output["state"], "running");
    let read = target_call(&fixture, node_read(&goal)).await;
    success(&read);
    let claim_call = |revision| ToolCall::GoalClaimNode {
        goal_id: id(&goal),
        node_id: "research".into(),
        expected_revision: revision,
        plan_version: 1,
        idempotency_key: "native-successor-claim".into(),
        context_epoch: Some("native-successor".into()),
        rework: false,
        takeover_attempt_id: Some(binding["attempt_id"].as_str().unwrap().into()),
    };
    let waiting = target_call(&fixture, claim_call(revision(&read.output["goal"]))).await;
    assert_eq!(waiting.output["error_kind"], "goal_run_pending");
    assert_eq!(
        fixture
            .db
            .get_host_goal(&owner, &id(&goal))
            .unwrap()
            .unwrap()
            .current_attempt_id
            .as_deref(),
        binding["attempt_id"].as_str()
    );

    let cancelled = target_call(
        &fixture,
        ToolCall::CodingAgentCancel {
            run_id: run_id.clone(),
        },
    )
    .await;
    success(&cancelled);
    assert_eq!(cancelled.output["run_id"], run_id);
    assert_eq!(cancelled.output["state"], "cancelled");
    let terminal = target_call(
        &fixture,
        ToolCall::CodingAgentObserve {
            run_id: run_id.clone(),
            after_observation_token: None,
            wait_secs: Some(0),
        },
    )
    .await;
    success(&terminal);
    assert_eq!(terminal.output["run_id"], run_id);
    assert_eq!(terminal.output["state"], "cancelled");
    let read = target_call(&fixture, node_read(&goal)).await;
    success(&read);
    let successor = target_call(&fixture, claim_call(revision(&read.output["goal"]))).await;
    success(&successor);
    assert_eq!(successor.output["goal"]["goal_id"], goal["goal_id"]);
    assert!(
        successor.output["binding"]["fence"].as_u64().unwrap() > binding["fence"].as_u64().unwrap()
    );
    let operations = fixture
        .db
        .list_host_goal_operations(&owner, &id(&goal), 0, 100, false)
        .unwrap();
    assert_eq!(
        operations.len(),
        1,
        "No replacement run may be dispatched during recovery"
    );
    assert_eq!(operations[0].operation_id, "checkpoint-operation");
    assert_eq!(operations[0].status, HostGoalOperationStatus::Cancelled);
    let receipt: Value =
        serde_json::from_str(operations[0].result_summary.as_deref().unwrap()).unwrap();
    assert_eq!(receipt["run_id"], run_id);
    let attempt = fixture
        .db
        .latest_host_goal_attempt(&owner, &id(&goal))
        .unwrap()
        .unwrap();
    assert_eq!(
        attempt.context_epoch.as_deref(),
        Some(native_successor_epoch(&fixture).as_str())
    );
    assert!(
        attempt.checks.is_empty(),
        "Run termination is not node acceptance"
    );
}
