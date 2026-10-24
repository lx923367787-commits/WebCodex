//! Synthetic host conversations exercise the real kernel and durable store.
//! These tests provide no evidence about ChatGPT webpage automation.
use super::*;

async fn raw(f: &Fixture, epoch: &str, tool: &str, mut arguments: Value) -> ToolResult {
    arguments["context_epoch"] = json!(epoch);
    let future = f.runtime.call_tool_with_context(
        ToolCallRequest {
            tool_name: tool.into(),
            arguments,
        },
        ToolCallContext {
            transport: ToolTransport::Mcp,
            session_id: None,
            auth: Some(&f.auth),
            window: None,
            record_oauth_scope_denials: true,
            host_file_import_trust: HostFileImportTrust::Untrusted,
        },
    );
    let outcome = crate::test_support::runner_project_files::during(
        &f.runtime,
        "host-goal-runner",
        "inst",
        f.temp.path(),
        future,
    )
    .await;
    assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
    outcome.result.unwrap()
}

async fn executing(f: &Fixture) -> ToolResult {
    let goal = f.start().await;
    let saved = f.save_plan(&goal, plan(), "handoff-plan").await;
    success(&saved);
    let confirmed = f.confirm(&saved.output["goal"], "handoff-confirm").await;
    success(&confirmed);
    let claim = f.claim(&confirmed.output["goal"], "handoff-claim").await;
    success(&claim);
    claim
}

fn wrap(binding: &Value, epoch: &str, operation: &str) -> Value {
    let mut result = binding.clone();
    result["context_epoch"] = json!(epoch);
    result["operation_id"] = json!(operation);
    result
}

#[tokio::test]
async fn host_goals_explicit_policy_cannot_authorize_handoff_and_plan_replay_stays_immutable() {
    let f = Fixture::new().await;
    let started = raw(
        &f,
        "small-chat",
        "goal_mode_start",
        json!({"project":f.project,"objective":"small test","idempotency_key":"small-start",
        "context_warning_tokens":3000,"context_handoff_tokens":4000}),
    )
    .await;
    success(&started);
    assert_eq!(started.output["context_meter"]["warning_tokens"], 3000);
    assert_eq!(started.output["context_meter"]["handoff_tokens"], 4000);
    let f = Fixture::new().await;
    let goal = f.start().await;
    let saved = f.save_plan(&goal, plan(), "replay-published").await;
    success(&saved);
    let before = std::fs::read(
        saved.output["plan_artifacts"]["json_path"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let retired=raw(&f,"chat-original","goal_checkpoint",json!({"goal_id":id(&goal),"expected_revision":revision(&saved.output["goal"]),
        "idempotency_key":"published-handoff","patch":{"action":"handoff","reason":"transfer before confirmation"}})).await;
    assert_eq!(
        retired.output["error_kind"],
        "goal_handoff_identity_required"
    );
    let replay = f.save_plan(&goal, plan(), "replay-published").await;
    success(&replay);
    assert_eq!(replay.output["replayed"], true);
    assert_eq!(
        std::fs::read(
            saved.output["plan_artifacts"]["json_path"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        before
    );
}

#[tokio::test]
async fn host_goals_handoff_waits_for_inflight_requests_and_keeps_source_checkpoint_after_transfer()
{
    let f = super::takeover::TakeoverFixture::new().await;
    let binding = f.operation("missing-identity");
    let identityless = f.inner.runtime.call_tool_with_context(ToolCallRequest {
        tool_name:"write_project_file".into(),
        arguments:json!({"project":f.inner.project,"path":"must-not-write.txt","content":"x","goal_context":binding}),
    },ToolCallContext {
        transport:ToolTransport::Mcp,session_id:None,auth:Some(&f.inner.auth),window:None,
        record_oauth_scope_denials:true,host_file_import_trust:HostFileImportTrust::Untrusted,
    }).await.result.unwrap();
    assert_eq!(identityless.output["error_kind"], "goal_context_missing");
    let lease = crate::tool_runtime::host_goal_context::lease_active_operation(
        &id(&f.goal),
        "still-synchronous",
    );
    let waiting = f.checkpoint().await;
    assert_eq!(waiting.output["error_kind"], "goal_operation_still_active");
    assert!(f
        .inner
        .db
        .read_host_goal_handoff(&f.owner(), &id(&f.goal))
        .unwrap()
        .is_none());
    drop(lease);
    let fields = f.reserve().await;
    let h = f.stored();
    let before = f
        .inner
        .db
        .read_host_goal_handoff_snapshot(&f.owner(), &id(&f.goal), &h.source_context_epoch)
        .unwrap();
    assert_eq!(before.source.as_ref().unwrap().state, "draining");
    assert_eq!(
        before.checkpoint.as_ref().unwrap().source_fence,
        f.binding["fence"].as_u64()
    );
    assert!(before.resume_started.is_none());
    let forbidden = kernel_call(
        &f.inner,
        &f.source,
        "goal_bootstrap",
        json!({"goal_id":id(&f.goal),"activate":true,"context_epoch":"forged-fresh-chat"}),
    )
    .await;
    assert_eq!(forbidden.output["error_kind"], "goal_handoff_pending");
    let prepared = f.prepare(&fields).await;
    success(&prepared);
    assert_eq!(f.stored().execution_owner_epoch, h.source_context_epoch);
    success(&f.commit(&fields, &prepared).await);
    let committed = f
        .inner
        .db
        .read_host_goal_handoff_snapshot(&f.owner(), &id(&f.goal), &h.source_context_epoch)
        .unwrap();
    assert_eq!(
        committed.checkpoint, before.checkpoint,
        "handoff evidence must remain the exact original checkpoint"
    );
    assert_eq!(committed.source.as_ref().unwrap().state, "retired");
    assert_eq!(
        committed.resume_started.as_ref().unwrap().identity_source,
        "window_bound"
    );
    let read = kernel_call(
        &f.inner,
        &f.candidate,
        "goal_read",
        json!({"goal_id":id(&f.goal),"view":"node","node_id":"research"}),
    )
    .await;
    success(&read);
    let claimed = kernel_call(&f.inner,&f.candidate,"goal_claim_node",
        json!({"goal_id":id(&f.goal),"node_id":"research","expected_revision":revision(&read.output["goal"]),
            "plan_version":1,"idempotency_key":"successor-claim"})).await;
    success(&claimed);
    assert!(
        claimed.output["binding"]["fence"].as_u64().unwrap() > f.binding["fence"].as_u64().unwrap()
    );
    assert_eq!(claimed.output["context_meter"]["warning_tokens"], 600000);
    assert_eq!(claimed.output["context_meter"]["handoff_tokens"], 800000);
    let mut stolen = claimed.output["binding"].clone();
    stolen["operation_id"] = json!("retired-using-new-binding");
    let retired = kernel_call(&f.inner,&f.source,"write_project_file",
        json!({"project":f.inner.project,"path":"must-not-write.txt","content":"x","goal_context":stolen})).await;
    assert_eq!(retired.output["error_kind"], "goal_handoff_context_revoked");
    assert!(!f.inner.temp.path().join("must-not-write.txt").exists());
}

#[tokio::test]
async fn host_goals_runner_handoff_files_keep_exact_checkpoint_on_waiting_replay() {
    let f = super::takeover::TakeoverFixture::new().await;
    let first = f.checkpoint().await;
    success(&first);
    let receipt = &first.output["handoff_artifacts"];
    let sequence = receipt["checkpoint_sequence"].as_u64().unwrap();
    let expected = f
        .inner
        .temp
        .path()
        .join("task-output/handoff")
        .join(id(&f.goal))
        .join(format!("checkpoint-{sequence}"))
        .join("handoff.json");
    let json_path = std::path::PathBuf::from(receipt["json_path"].as_str().unwrap());
    let markdown_path = std::path::PathBuf::from(receipt["markdown_path"].as_str().unwrap());
    assert!(json_path.is_absolute() && markdown_path.is_absolute());
    assert_eq!(
        json_path.canonicalize().unwrap(),
        expected.canonicalize().unwrap()
    );
    let first_bytes = std::fs::read(&json_path).unwrap();
    let first_markdown = std::fs::read(&markdown_path).unwrap();
    let snapshot: Value = serde_json::from_slice(&first_bytes).unwrap();
    let stored = f
        .inner
        .db
        .host_goal_checkpoint_for_revision(
            &f.owner(),
            &id(&f.goal),
            revision(&first.output["goal"]),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot["checkpoint"],
        serde_json::to_value(&stored).unwrap()
    );
    assert!(snapshot
        .to_string()
        .contains("Saved evidence remains available"));
    assert!(!f.inner.server.path().join("handoff").exists());
    let replay = f.checkpoint().await;
    success(&replay);
    assert_eq!(replay.output["replayed"], true);
    assert_eq!(
        replay.output["handoff_artifacts"]["checkpoint_sequence"],
        sequence
    );
    assert_eq!(std::fs::read(&json_path).unwrap(), first_bytes);
    std::fs::write(&json_path, b"external edit must be preserved").unwrap();
    let conflict = f.checkpoint().await;
    assert_eq!(conflict.output["error_kind"], "goal_handoff_files_pending");
    assert_eq!(conflict.output["checkpoint_saved"], true);
    assert_eq!(
        std::fs::read(&json_path).unwrap(),
        b"external edit must be preserved"
    );
    assert_eq!(std::fs::read(&markdown_path).unwrap(), first_markdown);
    // Restore only the synthetic fixture's exact bytes to exercise a second
    // transfer without treating the corrupted file as an authorized replacement.
    std::fs::write(&json_path, &first_bytes).unwrap();
    let fields = f.reserve().await;
    let prepared = f.prepare(&fields).await;
    success(&prepared);
    success(&f.commit(&fields, &prepared).await);
    let current = f
        .inner
        .db
        .get_host_goal(&f.owner(), &id(&f.goal))
        .unwrap()
        .unwrap();
    let second = kernel_call(&f.inner,&f.candidate,"goal_checkpoint",
        json!({"goal_id":id(&f.goal),"expected_revision":current.revision,"idempotency_key":"second-handoff",
            "patch":{"action":"handoff","reason":"Second transfer preserves its own exact snapshot",
                "state":{"completed_summary":"Second distinct checkpoint conclusion"}}})).await;
    success(&second);
    assert_ne!(
        second.output["handoff_artifacts"]["checkpoint_sequence"],
        sequence
    );
    assert_ne!(
        second.output["handoff_artifacts"]["json_path"],
        receipt["json_path"]
    );
    assert_eq!(std::fs::read(&json_path).unwrap(), first_bytes);
    assert_eq!(std::fs::read(&markdown_path).unwrap(), first_markdown);
    assert_eq!(
        f.checkpoint().await.output["error_kind"],
        "goal_handoff_context_revoked"
    );
}

#[tokio::test]
async fn host_goals_draining_allows_existing_acceptance_and_handoff_blocks_forged_epoch_activation()
{
    let f = Fixture::new().await;
    let claim = executing(&f).await;
    let recorded = raw(
        &f,
        "chat-original",
        "goal_checkpoint",
        json!({"goal_id":id(&claim.output["goal"]),
            "expected_revision":revision(&claim.output["goal"]),
            "idempotency_key":"record-before-drain",
            "goal_context":wrap(&claim.output["binding"],"chat-original","record-before-drain"),
            "patch":{"reason":"Persist the sourced conclusion before context draining",
                "state":{"completed_summary":"The recorded source supports this conclusion",
                    "facts":{"source":"https://example.test/source"},
                    "next_step":"Complete the prepared source-backed review"}}}),
    )
    .await;
    success(&recorded);
    let prepared = raw(
        &f,
        "chat-original",
        "goal_check_node",
        json!({"goal_id":id(&claim.output["goal"]),"node_id":"research",
            "attempt_id":claim.output["binding"]["attempt_id"],
            "expected_revision":revision(&recorded.output["goal"]),"plan_version":1,
            "idempotency_key":"prepare-before-drain"}),
    )
    .await;
    success(&prepared);
    assert_eq!(prepared.output["goal"]["phase"], "executing");
    assert!(prepared.output["review_packet"]["materials"]
        .to_string()
        .contains("The recorded source supports this conclusion"));
    let principal = crate::tool_runtime::coding_agent::goal_owner_key(Some(&f.auth)).unwrap();
    f.db.observe_host_context(&principal, "chat-original", "drain", 800000, false, None)
        .unwrap();
    let other = raw(
        &f,
        "chat-original",
        "goal_mode_start",
        json!({"project":f.project,"objective":"escape","idempotency_key":"escape"}),
    )
    .await;
    assert_eq!(other.output["error_kind"], "goal_context_draining");
    let checked = raw(
        &f,
        "chat-original",
        "goal_check_node",
        json!({"goal_id":id(&claim.output["goal"]),"node_id":"research",
            "attempt_id":claim.output["binding"]["attempt_id"],
            "expected_revision":revision(&recorded.output["goal"]),"plan_version":1,
            "idempotency_key":"draining-check",
            "evidence":prepared_review_evidence(&prepared.output["review_packet"])}),
    )
    .await;
    success(&checked);
    assert_eq!(checked.output["goal"]["phase"], "completed");

    let f = Fixture::new().await;
    let window =
        crate::client_window::ClientWindow::from_opaque("openai-session", "fixed-real-host")
            .unwrap();
    let start = kernel_call(
        &f,
        &window,
        "goal_mode_start",
        json!({"project":f.project,"objective":"planning handoff","idempotency_key":"host-start"}),
    )
    .await;
    success(&start);
    let original = start.output["context_meter"]["context_epoch"]
        .as_str()
        .unwrap();
    let handoff=kernel_call(&f,&window,"goal_checkpoint",json!({"goal_id":id(&start.output["goal"]),"expected_revision":revision(&start.output["goal"]),
        "idempotency_key":"host-handoff","patch":{"action":"handoff","reason":"new host"}})).await;
    success(&handoff);
    let forged=kernel_call(&f,&window,"goal_bootstrap",json!({"goal_id":id(&start.output["goal"]),"activate":true,"context_epoch":"fake-fresh-chat"})).await;
    assert_eq!(forged.output["error_kind"], "goal_handoff_pending");
    let read = kernel_call(
        &f,
        &window,
        "goal_handoff",
        json!({"goal_id":id(&start.output["goal"]),"action":"status"}),
    )
    .await;
    success(&read);
    assert_eq!(read.output["handoff"]["source_context_epoch"], original);
    assert_eq!(read.output["handoff"]["status"], "waiting");
    assert_eq!(read.output["execution_granted"], false);
}
