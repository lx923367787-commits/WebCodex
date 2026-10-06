//! The model driver uses the real kernel, store and Runner protocol. No user
//! resume, database repair, or fabricated passing evidence advances this test.
use super::goals::{goal_auth, Behavior, Fixture};
use super::host_goal_check_recovery::prepare_managed;
use crate::tool_runtime::kernel::{
    HostFileImportTrust, ToolCallContext, ToolCallRequest, ToolTransport,
};
use crate::tool_runtime::{ToolCall, ToolResult};
use serde_json::json;
use webcodex_core::host_goal::*;

#[test]
fn host_goal_repair_context_cannot_override_acceptance_or_budgets() {
    use crate::tool_runtime::host_goal_context::RequestScope;
    let value = json!({"goal_context":{"goal_id":"hgoal_example","node_id":"node","attempt_id":"attempt",
        "plan_version":1,"fence":1,"operation_id":"repair-op","context_epoch":"chat",
        "repair":{"repair_id":"hrepair_example","diagnosis":"Correct the failed import, then repeat original checks"}}});
    assert!(RequestScope::new("owner".into(), None, &value).is_ok());
    for field in ["passed", "max_rounds", "approved", "criteria"] {
        let mut forged = value.clone();
        forged["goal_context"]["repair"][field] = json!(true);
        assert!(
            RequestScope::new("owner".into(), None, &forged).is_err(),
            "{field}"
        );
    }
}

async fn call(
    fixture: &Fixture,
    binding: &HostGoalExecutionBinding,
    invocation: &HostGoalInvocationContext,
    operation: &str,
    repair: Option<&HostGoalRepairContext>,
    call: ToolCall,
) -> ToolResult {
    let mut arguments = serde_json::to_value(&call).unwrap()["params"].clone();
    let mut metadata = serde_json::to_value(binding).unwrap();
    metadata["operation_id"] = json!(operation);
    metadata["context_epoch"] = json!(invocation.context_epoch);
    if let Some(repair) = repair {
        metadata["repair"] = json!(repair);
    }
    arguments["goal_context"] = metadata;
    fixture
        .runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name: call.tool_name().into(),
                arguments,
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(&goal_auth()),
                window: None,
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await
        .result
        .unwrap()
}

fn execute(project: &str, key: &str) -> ToolCall {
    ToolCall::CodingAgentStart { project: project.into(), provider_id: "executor".into(), idempotency_key: key.into(),
        instruction: "Diagnose the failed execution, correct result.txt and preserve the original acceptance contract".into(),
        config: None, timeout_secs: Some(10), recording_session_id: None }
}

fn verify(goal: &HostGoalRecord, binding: &HostGoalExecutionBinding, key: &str) -> ToolCall {
    ToolCall::GoalCheckNode {
        goal_id: goal.goal_id.clone(),
        node_id: binding.node_id.clone(),
        attempt_id: binding.attempt_id.clone(),
        expected_revision: goal.revision,
        plan_version: goal.plan_version,
        idempotency_key: key.into(),
        evidence: None,
    }
}

#[tokio::test]
async fn host_goal_repair_kernel_suspends_business_repairs_and_verifies_before_release() {
    let (fixture, goal, binding, invocation) =
        prepare_managed(Behavior::ExecutionFailsOnce, false).await;
    let started = call(
        &fixture,
        &binding,
        &invocation,
        "business-run",
        None,
        execute(&fixture.project, "first-run"),
    )
    .await;
    assert!(started.success, "{started:?}");
    let failed = call(
        &fixture,
        &binding,
        &invocation,
        "observe-failure",
        None,
        verify(&goal, &binding, "first-check"),
    )
    .await;
    assert!(
        !failed.success,
        "A failed executor must suspend even plausible output: {failed:?}"
    );
    assert_eq!(failed.output["error_kind"], "goal_repair_required");
    assert_eq!(failed.output["recovery"]["business_suspended"], true);
    assert!(
        fixture.command_jobs().is_empty(),
        "Verification must not erase a newly observed execution failure"
    );
    let blocked = call(
        &fixture,
        &binding,
        &invocation,
        "premature-business",
        None,
        execute(&fixture.project, "must-not-run"),
    )
    .await;
    assert_eq!(blocked.output["error_kind"], "goal_repair_required");
    let record = fixture
        .db
        .get_host_goal_repair(&goal.owner_key, &goal.goal_id)
        .unwrap()
        .unwrap();
    let repair = HostGoalRepairContext { repair_id: record.repair_id,
        diagnosis: Some("The first executor terminated with fixture_execution_failure; perform a targeted replacement execution, then verify its real output".into()) };
    let repaired = call(
        &fixture,
        &binding,
        &invocation,
        "repair-run",
        Some(&repair),
        execute(&fixture.project, "second-run"),
    )
    .await;
    assert!(repaired.success, "{repaired:?}");
    assert_eq!(repaired.output["recovery"]["business_suspended"], true);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let checked = loop {
        let result = call(
            &fixture,
            &binding,
            &invocation,
            "verify-repair",
            Some(&repair),
            verify(&goal, &binding, "repair-check"),
        )
        .await;
        if result.output["error_kind"] != "goal_check_pending" {
            break result;
        }
        assert!(tokio::time::Instant::now() < deadline, "{result:?}");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert!(checked.success, "{checked:?}");
    assert_eq!(checked.output["goal"]["phase"], "completed");
    assert_eq!(checked.output["recovery"]["business_suspended"], false);
    assert!(!fixture.command_jobs().is_empty());
    let stored = fixture
        .db
        .get_host_goal_repair(&goal.owner_key, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, HostGoalRepairStatus::Verified);
    assert_eq!(stored.rounds.len(), 1);
}
