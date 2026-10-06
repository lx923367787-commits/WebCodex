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
async fn host_goals_new_epoch_uses_explicit_policy_and_published_plan_replays_after_retirement() {
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
    success(&retired);
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
async fn host_goals_handoff_atomic_retirement_requires_a_new_attempt_and_keeps_source_evidence() {
    let f = Fixture::new().await;
    let claim = executing(&f).await;
    let goal = &claim.output["goal"];
    let binding = &claim.output["binding"];
    let mut identityless_binding = binding.clone();
    identityless_binding["operation_id"] = json!("missing-epoch");
    let identityless = f.runtime.call_tool_with_context(ToolCallRequest {
        tool_name: "write_project_file".into(),
        arguments: json!({"project":f.project,"path":"must-not-write.txt","content":"x","goal_context":identityless_binding}),
    }, ToolCallContext {
        transport: ToolTransport::Mcp, session_id: None, auth: Some(&f.auth), window: None,
        record_oauth_scope_denials: true, host_file_import_trust: HostFileImportTrust::Untrusted,
    }).await.result.unwrap();
    assert_eq!(identityless.output["error_kind"], "goal_context_missing");
    let principal = crate::tool_runtime::coding_agent::goal_owner_key(Some(&f.auth)).unwrap();
    f.db.observe_host_context(
        &principal,
        "chat-original",
        "synthetic-drain",
        800000,
        false,
        None,
    )
    .unwrap();
    let denied = raw(
        &f,
        "chat-original",
        "write_project_file",
        json!({"project":f.project,"path":"must-not-write.txt","content":"x",
        "goal_context":wrap(binding,"chat-original","drained-write")}),
    )
    .await;
    assert_eq!(denied.output["error_kind"], "goal_context_draining");
    assert!(!f.temp.path().join("must-not-write.txt").exists());
    let draining = raw(
        &f,
        "chat-original",
        "goal_bootstrap",
        json!({"goal_id":id(goal),"activate":false}),
    )
    .await;
    success(&draining);
    assert_workflow_overview(&draining.output);
    assert_eq!(
        draining.output["context_meter"]["lifecycle_state"],
        "draining"
    );
    assert!(draining.output["context_meter"]["notice"]
        .as_str()
        .unwrap()
        .contains("no new business"));
    let handoff = json!({"goal_id":id(goal),"expected_revision":revision(goal),"idempotency_key":"handoff-once",
        "patch":{"action":"handoff","reason":"continue locally"},"goal_context":wrap(binding,"chat-original","handoff")});
    let lease = crate::tool_runtime::host_goal_context::lease_active_operation(
        &id(goal),
        "still-synchronous",
    );
    let waiting = raw(&f, "chat-original", "goal_checkpoint", handoff.clone()).await;
    assert_eq!(waiting.output["error_kind"], "goal_operation_still_active");
    assert_ne!(
        f.db.read_host_context_state(&principal, "chat-original")
            .unwrap()
            .unwrap()
            .state,
        "retired"
    );
    drop(lease);
    let saved = raw(&f, "chat-original", "goal_checkpoint", handoff.clone()).await;
    success(&saved);
    let replay = raw(&f, "chat-original", "goal_checkpoint", handoff).await;
    success(&replay);
    assert_eq!(replay.output["replayed"], true);
    let denied = raw(
        &f,
        "chat-original",
        "goal_bootstrap",
        json!({"goal_id":id(goal),"activate":true}),
    )
    .await;
    assert_eq!(denied.output["error_kind"], "goal_chat_retired");
    let retired = raw(
        &f,
        "chat-original",
        "goal_bootstrap",
        json!({"goal_id":id(goal),"activate":false}),
    )
    .await;
    success(&retired);
    assert_workflow_overview(&retired.output);
    assert_eq!(
        retired.output["context_meter"]["lifecycle_state"],
        "retired"
    );
    assert!(retired.output["context_meter"]["notice"]
        .as_str()
        .unwrap()
        .contains("Do not repeat business operations or reactivate"));
    let replay = raw(
        &f,
        "chat-original",
        "goal_mode_start",
        json!({
            "project":f.project,"objective":"Produce a sourced analysis","idempotency_key":"start"
        }),
    )
    .await;
    success(&replay);
    assert_workflow_overview(&replay.output);
    assert_eq!(replay.output["replayed"], true);
    assert_eq!(replay.output["goal"], retired.output["goal"]);
    assert_eq!(replay.output["context_meter"]["lifecycle_state"], "retired");
    let query = json!({"goal_id":id(goal),"view":"handoff","source_context_epoch":"chat-original"});
    let before = raw(&f, "chat-original", "goal_read", query.clone()).await;
    success(&before);
    assert_eq!(
        before.output["source_context"]["lifecycle_state"],
        "retired"
    );
    assert_eq!(
        before.output["checkpoint"]["source_fence"],
        binding["fence"]
    );
    assert_eq!(before.output["business_verified"], false);
    success(
        &raw(
            &f,
            "chat-b",
            "goal_bootstrap",
            json!({"goal_id":id(goal),"activate":true}),
        )
        .await,
    );
    let theft = raw(
        &f,
        "chat-b",
        "write_project_file",
        json!({"project":f.project,"path":"must-not-write.txt","content":"x",
        "goal_context":wrap(binding,"chat-b","stolen-binding")}),
    )
    .await;
    assert_eq!(theft.output["error_kind"], "goal_context_attempt_mismatch");
    let read = raw(
        &f,
        "chat-b",
        "goal_read",
        json!({"goal_id":id(goal),"view":"node","node_id":"research"}),
    )
    .await;
    success(&read);
    let b = raw(&f,"chat-b","goal_claim_node",json!({"goal_id":id(goal),"node_id":"research","expected_revision":revision(&read.output["goal"]),
        "plan_version":1,"idempotency_key":"b-claim","takeover_attempt_id":binding["attempt_id"]})).await;
    success(&b);
    assert!(b.output["binding"]["fence"].as_u64().unwrap() > binding["fence"].as_u64().unwrap());
    let after = raw(&f, "chat-b", "goal_read", query.clone()).await;
    success(&after);
    assert_eq!(after.output["checkpoint"], before.output["checkpoint"]);
    assert_eq!(after.output["current_attempt"]["context_epoch"], "chat-b");
    assert_eq!(
        after.output["business_verified"], false,
        "bootstrap/claim alone must not count as business"
    );
    assert_eq!(b.output["context_meter"]["warning_tokens"], 600000);
    assert_eq!(b.output["context_meter"]["handoff_tokens"], 800000);
    let retired = raw(
        &f,
        "chat-original",
        "write_project_file",
        json!({"project":f.project,"path":"must-not-write.txt","content":"x",
        "goal_context":wrap(&b.output["binding"],"chat-original","retired-with-new-binding")}),
    )
    .await;
    assert_eq!(retired.output["error_kind"], "goal_chat_retired");
    assert!(!f.temp.path().join("must-not-write.txt").exists());
}

#[tokio::test]
async fn host_goals_runner_handoff_files_keep_exact_checkpoint_on_retired_replay() {
    let f = Fixture::new().await;
    let goal = f.start_in_task(Some("task-output")).await;
    let first_arguments = json!({"goal_id":id(&goal),"expected_revision":revision(&goal),
        "idempotency_key":"task-handoff-one","patch":{"action":"handoff","reason":"Preserve first exact state",
            "state":{"completed_summary":"First checkpoint conclusion","facts":{"stage":"first"}}}});
    let first = raw(
        &f,
        "chat-original",
        "goal_checkpoint",
        first_arguments.clone(),
    )
    .await;
    success(&first);
    let receipt = &first.output["handoff_artifacts"];
    let sequence = receipt["checkpoint_sequence"].as_u64().unwrap();
    let expected = f
        .temp
        .path()
        .join("task-output/handoff")
        .join(id(&goal))
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
    let principal = crate::tool_runtime::coding_agent::goal_owner_key(Some(&f.auth)).unwrap();
    let stored = f
        .db
        .host_goal_checkpoint_for_revision(&principal, &id(&goal), revision(&first.output["goal"]))
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot["checkpoint"],
        serde_json::to_value(&stored).unwrap()
    );
    assert!(snapshot.to_string().contains("First checkpoint conclusion"));
    assert!(!f.server.path().join("handoff").exists());
    let successor = raw(
        &f,
        "task-successor",
        "goal_bootstrap",
        json!({"goal_id":id(&goal),"activate":true}),
    )
    .await;
    success(&successor);
    let second = raw(&f,"task-successor","goal_checkpoint",json!({"goal_id":id(&goal),
        "expected_revision":revision(&successor.output["goal"]),"idempotency_key":"task-handoff-two",
        "patch":{"action":"handoff","reason":"A later state must have a separate snapshot",
            "state":{"completed_summary":"Second distinct checkpoint conclusion","facts":{"stage":"second"}}}})).await;
    success(&second);
    assert_ne!(
        second.output["handoff_artifacts"]["checkpoint_sequence"],
        receipt["checkpoint_sequence"]
    );
    assert_ne!(
        second.output["handoff_artifacts"]["json_path"],
        receipt["json_path"]
    );
    let replay = raw(
        &f,
        "chat-original",
        "goal_checkpoint",
        first_arguments.clone(),
    )
    .await;
    success(&replay);
    assert_eq!(replay.output["replayed"], true);
    assert_eq!(
        replay.output["handoff_artifacts"]["checkpoint_sequence"],
        sequence
    );
    assert_eq!(std::fs::read(&json_path).unwrap(), first_bytes);
    assert_eq!(std::fs::read(&markdown_path).unwrap(), first_markdown);
    std::fs::write(&json_path, b"external edit must be preserved").unwrap();
    let conflict = raw(&f, "chat-original", "goal_checkpoint", first_arguments).await;
    assert!(!conflict.success);
    assert_eq!(conflict.output["error_kind"], "goal_handoff_files_pending");
    assert_eq!(conflict.output["checkpoint_saved"], true);
    assert_eq!(
        std::fs::read(&json_path).unwrap(),
        b"external edit must be preserved"
    );
    assert_eq!(std::fs::read(&markdown_path).unwrap(), first_markdown);
}

#[tokio::test]
async fn host_goals_draining_allows_only_existing_acceptance_and_retired_blocks_activation_even_with_forged_epoch(
) {
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
    assert_eq!(forged.output["error_kind"], "goal_chat_retired");
    assert_eq!(forged.output["context_meter"]["context_epoch"], original);
    let read=kernel_call(&f,&window,"goal_read",json!({"goal_id":id(&start.output["goal"]),"view":"handoff","source_context_epoch":original})).await;
    success(&read);
    assert_eq!(
        read.output["source_context"]["identity_source"],
        "window_bound"
    );
}
