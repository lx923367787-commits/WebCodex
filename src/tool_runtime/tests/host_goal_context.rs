use super::*;

fn context(epoch: Option<&str>) -> Value {
    let mut value = json!({
        "goal_id": "hgoal_one", "node_id": "node_one", "attempt_id": "hattempt_one",
        "plan_version": 1, "fence": 1, "operation_id": "operation_one",
    });
    if let Some(epoch) = epoch {
        value["context_epoch"] = json!(epoch);
    }
    value
}

#[test]
fn binding_requires_complete_bounded_metadata_without_extra_authority_fields() {
    let parsed = RequestScope::new(
        "owner".into(),
        None,
        &json!({"goal_context": context(Some("epoch"))}),
    )
    .unwrap();
    assert_eq!(parsed.goal_context.unwrap().binding().node_id, "node_one");
    for field in [
        "goal_id",
        "node_id",
        "attempt_id",
        "plan_version",
        "fence",
        "operation_id",
    ] {
        let mut value = context(None);
        value.as_object_mut().unwrap().remove(field);
        assert!(
            RequestScope::new("owner".into(), None, &json!({"goal_context": value})).is_err(),
            "{field}"
        );
    }
    let mut extra = context(None);
    extra["approved"] = json!(true);
    assert!(RequestScope::new("owner".into(), None, &json!({"goal_context": extra})).is_err());
    assert!(RequestScope::new(
        "owner".into(),
        None,
        &json!({"context_epoch": "a", "goal_context": context(Some("b"))})
    )
    .is_err());
    assert!(RequestScope::new("owner".into(), None, &json!({"context_epoch": " "})).is_err());
    assert!(RequestScope::new(
        "owner".into(),
        None,
        &json!({"context_epoch": "x".repeat(129)})
    )
    .is_err());
    // Planning and bootstrap can supply a conversation epoch without inventing
    // a node attempt that does not exist yet.
    assert!(RequestScope::new(
        "owner".into(),
        None,
        &json!({"context_epoch": "planning-chat"})
    )
    .is_ok());
}

#[tokio::test]
async fn only_host_chat_identities_supply_implicit_epoch_and_cannot_be_reset() {
    for source in ["mcp", "http-cookie", "test"] {
        let window = ClientWindow::from_opaque(source, "transport-id").unwrap();
        let request = RequestScope::new("owner".into(), Some(&window), &json!({})).unwrap();
        scope(request, async {
            assert!(current_window().is_none());
            assert!(current_epoch().is_none());
            set_epoch("explicit-chat").unwrap();
            assert_eq!(current_epoch().as_deref(), Some("explicit-chat"));
        })
        .await;
    }
    for source in ["openai-session", "openai-conversation"] {
        let window = ClientWindow::from_opaque(source, "private-host-chat-id").unwrap();
        let expected = format!("chat_{}", window.key());
        let request = RequestScope::new(
            "owner".into(),
            Some(&window),
            &json!({"context_epoch": "ignore-this-reset"}),
        )
        .unwrap();
        scope(request, async {
            assert_eq!(current_epoch().as_deref(), Some(expected.as_str()));
            assert_eq!(current_window().as_deref(), Some(window.key()));
            assert!(!current_epoch().unwrap().contains("private-host-chat-id"));
            assert!(set_epoch("different-chat").is_err());
            set_epoch(&expected).unwrap();
        })
        .await;
    }
    assert!(current_epoch().is_none());
    assert!(set_epoch("outside-request").is_err());
}

#[tokio::test]
async fn concurrent_request_scopes_do_not_share_identity_binding_or_operation_receipts() {
    let a = RequestScope::new(
        "owner-a".into(),
        None,
        &json!({"goal_context": context(Some("chat-a"))}),
    )
    .unwrap();
    let b = RequestScope::new("owner-b".into(), None, &json!({"context_epoch": "chat-b"})).unwrap();
    tokio::join!(
        scope(a, async {
            mark_operation_admitted("hgoal_one", "operation_one");
            tokio::task::yield_now().await;
            assert_eq!(current_owner().as_deref(), Some("owner-a"));
            assert_eq!(current_epoch().as_deref(), Some("chat-a"));
            assert_eq!(current_binding().unwrap().node_id, "node_one");
            assert_eq!(
                admitted_operation(),
                Some(("hgoal_one".into(), "operation_one".into()))
            );
        }),
        scope(b, async {
            tokio::task::yield_now().await;
            assert_eq!(current_owner().as_deref(), Some("owner-b"));
            assert_eq!(current_epoch().as_deref(), Some("chat-b"));
            assert!(current_binding().is_none());
            assert!(admitted_operation().is_none());
        })
    );
    assert!(current_binding().is_none());
    assert!(admitted_operation().is_none());
}

#[tokio::test]
async fn operation_fingerprint_uses_original_payload_and_ignores_only_invocation_metadata() {
    async fn fingerprint(arguments: Value, tool: &str) -> String {
        scope(
            RequestScope::new("owner".into(), None, &arguments).unwrap(),
            async { current_payload_hash(tool).unwrap() },
        )
        .await
    }
    let first = fingerprint(json!({"project":"p","args":{"a":1,"b":2},"goal_context":context(Some("chat-one")),"recording_session_id":"one"}), "plugin_tool").await;
    let same = fingerprint(serde_json::from_str(r#"{"recording_session_id":"two","args":{"b":2,"a":1},"project":"p","context_epoch":"chat-two"}"#).unwrap(), "plugin_tool").await;
    assert_eq!(first, same);
    let changed = fingerprint(json!({"project":"p","args":{"a":1,"b":3}}), "plugin_tool").await;
    assert_ne!(first, changed);
    let different_tool =
        fingerprint(json!({"project":"p","args":{"a":1,"b":2}}), "run_process").await;
    assert_ne!(first, different_tool);
    assert!(current_payload_hash("outside_scope").is_none());
}

#[test]
fn estimates_use_o200k_for_arguments_and_final_text_including_errors() {
    let arguments = json!({"path": "中文报告.md"});
    let output = json!({"success": false, "error": "读取失败: file missing"});
    let encoder = tiktoken_rs::o200k_base_singleton();
    let expected = encoder.encode_ordinary(&arguments.to_string()).len()
        + encoder.encode_ordinary(&output.to_string()).len();
    let estimated = estimate_visible(&arguments, &output);
    assert_eq!(estimated.tokens, expected as u64);
    assert!(!estimated.unmeasured_modalities);
}

#[test]
fn native_media_and_embedded_base64_do_not_become_text_tokens() {
    let media = |size: usize| {
        json!({"content": [
            {"type": "image", "mimeType": "image/png", "data": "a".repeat(size)},
            {"type": "audio", "mimeType": "audio/wav", "data": "b".repeat(size)},
            {"type": "resource", "resource": {"uri": "file:///report.pdf", "mimeType": "application/pdf", "blob": "c".repeat(size)}},
            {"type": "text", "text": "Figure description"},
        ]})
    };
    let short = estimate_visible(&json!({}), &media(10));
    let long = estimate_visible(&json!({}), &media(100_000));
    assert_eq!(short.tokens, long.tokens);
    assert!(long.unmeasured_modalities);
    let embedded = estimate_visible(
        &json!({"content_base64": "abcd".repeat(1000)}),
        &json!({"url": "data:image/png;base64,abcdefgh"}),
    );
    assert!(embedded.unmeasured_modalities);
    assert!(embedded.tokens < 100);
}

#[test]
fn structured_results_deduplicate_only_their_equivalent_text_copy() {
    let structured = json!({"success": true, "output": {"state": "planning", "next_step": "Discuss requirements"}});
    let once = estimate_visible(&json!({}), &json!({"structuredContent": structured}));
    let twice = estimate_visible(
        &json!({}),
        &json!({"structuredContent": structured, "content": [
            {"type": "text", "text": serde_json::to_string_pretty(&structured).unwrap()},
        ]}),
    );
    assert_eq!(once.tokens, twice.tokens);
    let unique = estimate_visible(
        &json!({}),
        &json!({"structuredContent": structured, "content": [
            {"type": "text", "text": structured.to_string()},
            {"type": "text", "text": "This additional evidence must remain counted."},
        ]}),
    );
    assert!(unique.tokens > once.tokens);
    let hidden = estimate_visible(
        &json!({}),
        &json!({"structuredContent": structured, "_meta": {"hidden_view": "x".repeat(10000)}}),
    );
    assert_eq!(hidden.tokens, once.tokens);
}

#[tokio::test]
async fn meter_persists_across_new_requests_and_reopen_but_deduplicates_this_invocation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let db = Database::open(&path).unwrap();
    let request =
        RequestScope::new("owner".into(), None, &json!({"context_epoch": "chat-one"})).unwrap();
    let first = scope(request, async {
        let first = observe_response(
            &db,
            &json!({"tool": "goal_bootstrap"}),
            &json!({"recovery": "Continue node one"}),
            None,
        )
        .unwrap();
        let replay = observe_response(
            &db,
            &json!({"tool": "goal_bootstrap"}),
            &json!({"recovery": "Continue node one"}),
            None,
        )
        .unwrap();
        assert_eq!(first["observed_tokens"], replay["observed_tokens"]);
        assert_eq!(replay["sample_count"], 1);
        assert_eq!(replay["coverage"], "partial");
        assert_eq!(replay["retry_accounting"], "per_server_invocation");
        first["observed_tokens"].as_u64().unwrap()
    })
    .await;
    drop(db);
    let db = Database::open(&path).unwrap();
    // A real re-read is a new invocation even for the same goal/operation.
    let request =
        RequestScope::new("owner".into(), None, &json!({"context_epoch": "chat-one"})).unwrap();
    scope(request, async {
        let next = observe_response(
            &db,
            &json!({"tool": "goal_bootstrap"}),
            &json!({"recovery": "Continue node one"}),
            None,
        )
        .unwrap();
        assert_eq!(next["observed_tokens"].as_u64(), Some(first * 2));
        assert_eq!(next["sample_count"], 2);
    })
    .await;
    let request =
        RequestScope::new("owner".into(), None, &json!({"context_epoch": "chat-two"})).unwrap();
    scope(request, async {
        assert_eq!(
            observe_response(&db, &json!({}), &json!({}), None).unwrap()["sample_count"],
            1
        );
    })
    .await;
}

#[tokio::test]
async fn thresholds_trigger_full_then_short_advice_without_claiming_checkpoint_saved() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(&directory.path().join("state.db")).unwrap();
    for (epoch, initial, level) in [
        ("warn", 599_999, "warning"),
        ("handoff", 799_999, "handoff_recommended"),
    ] {
        db.observe_host_context("owner", epoch, "seed", initial, false, None)
            .unwrap();
        let request =
            RequestScope::new("owner".into(), None, &json!({"context_epoch": epoch})).unwrap();
        scope(request, async {
            let full =
                observe_response(&db, &json!({}), &json!({"progress": "read complete"}), None)
                    .unwrap();
            assert_eq!(full["level"], level);
            assert_eq!(full["newly_crossed"], true);
            assert!(full["notice"]
                .as_str()
                .unwrap()
                .contains("partial observation"));
            assert!(!full["notice"].as_str().unwrap().contains("has been saved"));
            let brief = observe_response(&db, &json!({}), &json!({}), None).unwrap();
            assert_eq!(brief["newly_crossed"], false);
            assert!(
                brief["notice"].as_str().unwrap().len() < full["notice"].as_str().unwrap().len()
            );
        })
        .await;
    }
    let missing = observe_response(&db, &json!({}), &json!({}), None).unwrap();
    assert_eq!(missing["level"], "unavailable");
    assert!(missing["observed_tokens"].is_null());
    assert!(missing["context_epoch"].is_null());
    assert_eq!(missing["warning_tokens"], 600_000);
    assert_eq!(missing["handoff_tokens"], 800_000);
    assert!(observe_response(&db, &json!({}), &json!({}), Some((32_000, 24_000))).is_err());
}

#[tokio::test]
async fn handoff_blocks_other_goal_leases_in_the_same_owner_epoch_only() {
    let request =
        RequestScope::new("owner".into(), None, &json!({"context_epoch":"chat-a"})).unwrap();
    let lease = scope(request, async {
        lease_active_operation("other-goal", "plan-write")
    })
    .await;
    scope(
        RequestScope::new("owner".into(), None, &json!({"context_epoch":"chat-a"})).unwrap(),
        async {
            assert!(guarded_goal_handoff("source-goal", || ()).is_err());
            track_active_operation("source-goal", "business").unwrap();
            assert!(operation_is_active("source-goal", "business"));
        },
    )
    .await;
    scope(
        RequestScope::new(
            "another-owner".into(),
            None,
            &json!({"context_epoch":"chat-a"}),
        )
        .unwrap(),
        async {
            assert!(guarded_goal_handoff("source-goal", || ()).is_ok());
        },
    )
    .await;
    drop(lease);
    scope(
        RequestScope::new("owner".into(), None, &json!({"context_epoch":"chat-a"})).unwrap(),
        async {
            assert!(guarded_goal_handoff("source-goal", || ()).is_ok());
        },
    )
    .await;
}
