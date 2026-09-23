use super::goals::{goal_auth, Behavior, Fixture};
use super::host_goal_check_recovery::prepare;
use crate::tool_runtime::host_goal_context::{self as context, RequestScope};
use crate::tool_runtime::{ToolCall, ToolResult};
use serde_json::json;
use webcodex_core::host_goal::{HostGoalExecutionBinding, HostGoalOperationStatus, HostGoalRecord};

async fn resolve(
    fixture: &Fixture,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
) -> ToolResult {
    let mut metadata = serde_json::to_value(binding).unwrap();
    metadata["operation_id"] = json!("checkpoint-request");
    let scope = RequestScope::new(
        goal.owner_key.clone(),
        None,
        &json!({"goal_context":metadata}),
    )
    .unwrap();
    context::scope(scope,fixture.runtime.dispatch_host_goal_tool(ToolCall::GoalCheckpoint {
        goal_id:goal.goal_id.clone(),expected_revision:goal.revision,idempotency_key:"operator-resolution".into(),context_epoch:None,
        patch:json!({"reason":"User inspected the abandoned write","operation_resolutions":[{"operation_id":"live-write","status":"succeeded","evidence":"result.txt contains the expected output","source_refs":["result.txt"],"user_confirmation":"I inspected the completed file; continue"}]}),
    },Some(&goal_auth()))).await
}

#[tokio::test]
async fn host_goal_active_operations_live_intent_cannot_be_resolved_until_scope_ends() {
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    let tracker_scope = RequestScope::new(goal.owner_key.clone(), None, &json!({})).unwrap();
    let tracked_goal = goal.goal_id.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(context::scope(tracker_scope, async move {
        context::track_active_operation(&tracked_goal, "live-write").unwrap();
        ready_tx.send(()).unwrap();
        let _ = release_rx.await;
    }));
    ready_rx.await.unwrap();
    fixture
        .db
        .begin_host_goal_operation(
            &goal.owner_key,
            &binding,
            "live-write",
            "write_file",
            "payload",
            7,
        )
        .unwrap();
    assert!(context::operation_is_active(&goal.goal_id, "live-write"));
    let denied = resolve(&fixture, &goal, &binding).await;
    assert!(!denied.success);
    assert_eq!(denied.output["error_kind"], "goal_operation_still_active");
    assert_eq!(
        fixture
            .db
            .get_host_goal_operation(&goal.owner_key, &goal.goal_id, "live-write")
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Intent
    );
    release_tx.send(()).unwrap();
    handle.await.unwrap();
    assert!(!context::operation_is_active(&goal.goal_id, "live-write"));
    // The exact original checkpoint is retryable after its active request ends.
    let resolved = resolve(&fixture, &goal, &binding).await;
    assert!(resolved.success, "{:?}", resolved);
    assert_eq!(
        fixture
            .db
            .get_host_goal_operation(&goal.owner_key, &goal.goal_id, "live-write")
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Succeeded
    );
}

#[tokio::test]
async fn host_goal_active_operations_cancelled_scope_releases_lease_for_abandoned_intent() {
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    let tracker_scope = RequestScope::new(goal.owner_key.clone(), None, &json!({})).unwrap();
    let tracked_goal = goal.goal_id.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(context::scope(tracker_scope, async move {
        context::track_active_operation(&tracked_goal, "live-write").unwrap();
        ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    }));
    ready_rx.await.unwrap();
    fixture
        .db
        .begin_host_goal_operation(
            &goal.owner_key,
            &binding,
            "live-write",
            "write_file",
            "payload",
            7,
        )
        .unwrap();
    assert!(context::operation_is_active(&goal.goal_id, "live-write"));
    handle.abort();
    assert!(handle.await.unwrap_err().is_cancelled());
    assert!(!context::operation_is_active(&goal.goal_id, "live-write"));
    let resolved = resolve(&fixture, &goal, &binding).await;
    assert!(resolved.success, "{:?}", resolved);
    assert_eq!(
        fixture
            .db
            .get_host_goal_operation(&goal.owner_key, &goal.goal_id, "live-write")
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Succeeded
    );
}

#[tokio::test]
async fn host_goal_active_operations_stop_recovery_requires_exact_tracked_job_but_not_live_attempt()
{
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    fixture
        .db
        .begin_host_goal_operation(
            &goal.owner_key,
            &binding,
            "background",
            "run_process",
            "payload",
            7,
        )
        .unwrap();
    fixture
        .db
        .finish_host_goal_operation(
            &goal.owner_key,
            &goal.goal_id,
            "background",
            HostGoalOperationStatus::Running,
            Some("job-owned-exact"),
            None,
            8,
        )
        .unwrap();
    let paused = fixture
        .db
        .checkpoint_host_goal(
            &goal.owner_key,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &goal.state,
            webcodex_store::HostGoalCheckpointAction::Pause,
            "Pause while the original Job is being stopped",
            "pause",
            9,
        )
        .unwrap()
        .goal;
    assert!(paused.current_attempt_id.is_none());
    // Recovery must remain available even if the reviewable plan copy is damaged.
    let plan = fixture
        .db
        .get_host_goal_plan(&goal.owner_key, &goal.goal_id, None)
        .unwrap()
        .unwrap();
    std::fs::remove_file(&plan.materialization.unwrap().markdown_path).unwrap();
    let window =
        crate::client_window::ClientWindow::from_opaque("openai-session", "stop-recovery").unwrap();
    fixture
        .db
        .bind_host_goal_window(
            &goal.owner_key,
            window.key(),
            &webcodex_store::HostGoalWindowBinding {
                goal_id: goal.goal_id.clone(),
                project: goal.project.clone(),
                project_root: goal.project_root.clone(),
                context_epoch: format!("chat_{}", window.key()),
            },
        )
        .unwrap();
    let auth = goal_auth();
    for (job, allowed) in [
        ("job-owned-exact", true),
        ("job-owned", false),
        ("job-unrelated", false),
    ] {
        let payload = json!({"project":goal.project,"job_id":job,"confirm":true});
        let scope = RequestScope::new(goal.owner_key.clone(), Some(&window), &payload).unwrap();
        let result = context::scope(
            scope,
            fixture.runtime.host_goal_admit_effect(
                "stop_job",
                Some(&goal.project),
                &payload,
                Some(&auth),
                true,
            ),
        )
        .await;
        if allowed {
            assert!(result.is_ok(), "{:?}", result);
        } else {
            assert_eq!(
                result.unwrap_err().output["error_kind"],
                "goal_job_mismatch"
            );
        }
    }
    assert_eq!(
        fixture
            .db
            .list_host_goal_operations(&goal.owner_key, &goal.goal_id, 0, 100, false)
            .unwrap()
            .len(),
        1,
        "Stopping recovery must not create a second operation"
    );
    let payload = json!({"project":goal.project,"job_id":"ordinary-job","confirm":true});
    let ordinary = RequestScope::new(goal.owner_key.clone(), None, &payload).unwrap();
    assert!(context::scope(
        ordinary,
        fixture.runtime.host_goal_admit_effect(
            "stop_job",
            Some(&goal.project),
            &payload,
            Some(&auth),
            true
        )
    )
    .await
    .is_ok());

    // The exception does not grant the normal concrete stop_job authority.
    let mut restricted = goal_auth();
    restricted.scopes.retain(|scope| scope != "job:run");
    let denied=fixture.runtime.call_tool_with_context(
        crate::tool_runtime::kernel::ToolCallRequest{tool_name:"stop_job".into(),arguments:json!({"project":goal.project,"job_id":"job-owned-exact","confirm":true})},
        crate::tool_runtime::kernel::ToolCallContext{transport:crate::tool_runtime::kernel::ToolTransport::Mcp,session_id:None,auth:Some(&restricted),window:Some(&window),record_oauth_scope_denials:true,host_file_import_trust:crate::tool_runtime::kernel::HostFileImportTrust::Untrusted},
    ).await;
    assert!(
        matches!(
            denied.error_status,
            Some(
                crate::tool_runtime::kernel::ToolCallErrorStatus::InsufficientScope {
                    required_scope: Some("job:run"),
                    ..
                }
            )
        ),
        "{:?}",
        denied
    );
}
