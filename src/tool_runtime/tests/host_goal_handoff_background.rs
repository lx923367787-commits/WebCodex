//! Native Run receipts must prove the old execution chain ended before a new
//! host may acquire this Goal. These are synthetic Runner protocol tests.
use super::*;
use webcodex_core::host_goal::HostGoalOperationStatus;

async fn handoff_call(
    fixture: &super::super::goals::Fixture,
    auth: &AuthContext,
    window: Option<&crate::client_window::ClientWindow>,
    arguments: Value,
) -> ToolResult {
    let outcome = fixture
        .runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name: "goal_handoff".into(),
                arguments,
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(auth),
                window,
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await;
    assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
    outcome.result.unwrap()
}

async fn reserve_after_native_start(
    behavior: super::super::goals::Behavior,
) -> (super::super::goals::Fixture, Value, String, ToolResult) {
    let (fixture, goal, binding, _) = native_host_setup(behavior).await;
    let started = native_host_call(
        &fixture,
        native_start(&fixture.project, "handoff-original-run"),
        Some(&binding),
    )
    .await;
    success(&started);
    let run_id = started.output["run_id"].as_str().unwrap().to_owned();
    let saved = native_host_call(&fixture,ToolCall::GoalCheckpoint {
        goal_id:id(&goal),expected_revision:revision(&goal),idempotency_key:"native-handoff".into(),
        context_epoch:None,patch:json!({"action":"handoff","reason":"Stop the exact original native Run before transfer"}),
    },Some(&binding)).await;
    success(&saved);
    let mut auth = super::super::goals::goal_auth();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    let h = fixture
        .db
        .read_host_goal_handoff(&owner, &id(&goal))
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .db
            .read_host_context_state(&owner, &native_host_epoch(&fixture))
            .unwrap()
            .unwrap()
            .state,
        "draining"
    );
    auth.role = Some("admin".into());
    auth.scopes.push(crate::auth::SCOPE_ADMIN.into());
    let reserved = handoff_call(
        &fixture,
        &auth,
        None,
        json!({"goal_id":id(&goal),"action":"begin_attempt","handoff_id":h.handoff_id}),
    )
    .await;
    success(&reserved);
    (fixture, goal, run_id, reserved)
}

#[tokio::test]
async fn host_goals_handoff_stops_original_native_run_before_reserving_successor() {
    let (fixture, goal, run_id, reserved) =
        reserve_after_native_start(super::super::goals::Behavior::RunningUntilCancelled).await;
    assert_eq!(reserved.output["handoff"]["status"], "awaiting_ready");
    assert_eq!(reserved.output["handoff"]["attempt_count"], 1);
    let auth = super::super::goals::goal_auth();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    let operations = fixture
        .db
        .list_host_goal_operations(&owner, &id(&goal), 0, 100, false)
        .unwrap();
    assert_eq!(
        operations.len(),
        1,
        "quiescence must not start a replacement Run"
    );
    assert_eq!(operations[0].status, HostGoalOperationStatus::Cancelled);
    let receipt: Value =
        serde_json::from_str(operations[0].result_summary.as_deref().unwrap()).unwrap();
    assert_eq!(receipt["run_id"], run_id);
    assert!(
        fixture
            .db
            .latest_host_goal_attempt(&owner, &id(&goal))
            .unwrap()
            .unwrap()
            .checks
            .is_empty(),
        "native termination is not business acceptance"
    );
    assert_eq!(reserved.output["execution_granted"], false);
    let candidate = crate::client_window::ClientWindow::from_opaque(
        "openai-session",
        &format!("{}-successor", native_host_epoch(&fixture)),
    )
    .unwrap();
    let h = &reserved.output["handoff"];
    let prepared = handoff_call(
        &fixture,
        &auth,
        Some(&candidate),
        json!({"goal_id":id(&goal),"action":"prepare","handoff_id":h["handoff_id"],
            "attempt_id":h["attempt_id"],"checkpoint_sequence":h["checkpoint_sequence"]}),
    )
    .await;
    success(&prepared);
    assert_eq!(prepared.output["execution_granted"], false);
    let committed = handoff_call(
        &fixture,
        &auth,
        Some(&candidate),
        json!({"goal_id":id(&goal),"action":"commit","handoff_id":h["handoff_id"],
            "attempt_id":h["attempt_id"],"checkpoint_sequence":h["checkpoint_sequence"],
            "package_sha256":prepared.output["package_sha256"]}),
    )
    .await;
    success(&committed);
    assert_eq!(committed.output["execution_granted"], true);
    assert_eq!(
        fixture
            .db
            .read_host_context_state(&owner, &native_host_epoch(&fixture))
            .unwrap()
            .unwrap()
            .state,
        "retired"
    );
}

#[tokio::test]
async fn host_goals_handoff_cannot_confirm_old_native_chain_termination_fails_closed() {
    for behavior in [
        super::super::goals::Behavior::CancelStillRunning,
        super::super::goals::Behavior::CancelUnknown,
    ] {
        let (fixture, goal, run_id, reserved) = reserve_after_native_start(behavior).await;
        let auth = super::super::goals::goal_auth();
        let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
        assert_eq!(reserved.output["handoff"]["status"], "failed");
        assert_eq!(
            reserved.output["handoff"]["attempt_count"], 0,
            "no candidate may launch without authoritative old-chain quiescence"
        );
        assert_eq!(reserved.output["execution_granted"], false);
        assert_eq!(
            fixture
                .db
                .get_host_goal(&owner, &id(&goal))
                .unwrap()
                .unwrap()
                .phase,
            webcodex_core::host_goal::HostGoalPhase::Blocked
        );
        assert_eq!(
            fixture
                .db
                .read_host_context_state(&owner, &native_host_epoch(&fixture))
                .unwrap()
                .unwrap()
                .state,
            "stopped"
        );
        let operations = fixture
            .db
            .list_host_goal_operations(&owner, &id(&goal), 0, 100, false)
            .unwrap();
        assert_eq!(operations.len(), 1);
        assert!(
            !operations[0].status.terminal(),
            "unknown termination cannot become a fabricated cancelled receipt"
        );
        assert!(operations[0]
            .result_summary
            .as_deref()
            .unwrap()
            .contains(&run_id));
        assert!(fixture
            .db
            .latest_host_goal_checkpoint(&owner, &id(&goal))
            .unwrap()
            .is_some());
        let h = &reserved.output["handoff"];
        let mut scheduler = auth.clone();
        scheduler.role = Some("admin".into());
        scheduler.scopes.push(crate::auth::SCOPE_ADMIN.into());
        let replay = handoff_call(
            &fixture,
            &scheduler,
            None,
            json!({"goal_id":id(&goal),"action":"begin_attempt","handoff_id":h["handoff_id"]}),
        )
        .await;
        success(&replay);
        assert_eq!(
            replay.output["handoff"], *h,
            "failed handoff never automatically resumes or launches"
        );
    }
}
