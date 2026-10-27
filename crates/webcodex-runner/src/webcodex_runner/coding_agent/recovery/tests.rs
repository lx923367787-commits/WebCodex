use super::*;

fn snapshot() -> CodingAgentRunSnapshot {
    CodingAgentRunSnapshot {
        run_id: "wc_agent_run_takeover_evidence".into(),
        intent_fingerprint: "intent".into(),
        authority_fingerprint: "auth_fixture".into(),
        runtime_project_id: "agent:test:repo".into(),
        provider_id: "configurable-provider".into(),
        provider_instance_id: "instance".into(),
        state: CodingAgentRunState::Running,
        execution_state: CodingAgentExecutionState::Started,
        observation_revision: 1,
        created_at: now(),
        updated_at: now(),
        terminal: None,
    }
}

#[test]
fn evidence_survives_reopen_without_reasoning_and_marks_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let store = DurableRunStore::new(temp.path().join("store"));
    let original = snapshot();
    fs::create_dir_all(store.run_dir(&original.run_id)).unwrap();
    let entry = RunEntry::new(original.clone());
    entry.restore_evidence(&store, false);
    entry.push_event(event(
        CodingAgentEventKind::Reasoning,
        Some("PRIVATE_REASONING".into()),
        None,
        None,
    ));
    entry.push_event(event(
        CodingAgentEventKind::AgentMessage,
        Some("Partial result".into()),
        None,
        None,
    ));
    for sequence in 0..260 {
        entry.push_event(event(
            CodingAgentEventKind::ToolActivity,
            Some(format!("test-result-{sequence}")),
            Some(format!("operation-{sequence}")),
            Some(
                if sequence == 259 {
                    "in_progress"
                } else {
                    "completed"
                }
                .into(),
            ),
        ));
    }
    assert!(!entry.evidence_failed.load(Ordering::Acquire));
    let durable = fs::read_to_string(store.run_dir(&original.run_id).join(EVIDENCE_FILE)).unwrap();
    assert!(!durable.contains("PRIVATE_REASONING"));
    let recovered = RunEntry::new(original);
    recovered.restore_evidence(&store, true);
    let observed = recovered.observe(Some(0), 64, 0).unwrap();
    assert!(observed.history_lost);
    assert_eq!(observed.first_retained_sequence, 7);
    assert_eq!(observed.events[0].sequence, 7);
    assert!(observed.has_more);
    let last = recovered.observe(Some(261), 64, 0).unwrap();
    assert_eq!(last.events.len(), 1);
    assert_eq!(last.events[0].status.as_deref(), Some("in_progress"));
}

#[test]
fn missing_or_corrupt_evidence_is_never_presented_as_complete() {
    let temp = tempfile::tempdir().unwrap();
    let store = DurableRunStore::new(temp.path().to_path_buf());
    let entry = RunEntry::new(snapshot());
    entry.restore_evidence(&store, true);
    assert!(entry.observe(None, 64, 0).unwrap().history_lost);
    fs::create_dir_all(store.run_dir(&snapshot().run_id)).unwrap();
    fs::write(
        store.run_dir(&snapshot().run_id).join(EVIDENCE_FILE),
        b"broken",
    )
    .unwrap();
    let recovered = RunEntry::new(snapshot());
    recovered.restore_evidence(&store, true);
    assert!(recovered.observe(None, 64, 0).unwrap().history_lost);
}

#[test]
fn corrupt_run_state_preserves_tombstone_and_blocks_new_workspace_admission() {
    let temp = tempfile::tempdir().unwrap();
    let store = DurableRunStore::new(temp.path().join("store"));
    let id = "wc_agent_run_corrupt_takeover";
    fs::create_dir_all(store.run_dir(id)).unwrap();
    fs::write(store.state_path(id), b"not valid durable state").unwrap();
    let manager =
        CodingAgentManager::with_store(&AcpConfig::default(), store.root.clone()).unwrap();
    assert!(manager.store.recovery_incomplete.load(Ordering::Acquire));
    let request = webcodex_core::coding_agent::CodingAgentStartRequest {
        run_id: "wc_agent_run_new_after_corruption".into(),
        intent_fingerprint: "intent".into(),
        authority_fingerprint: "auth_fixture".into(),
        runtime_project_id: "agent:test:repo".into(),
        project_root: temp.path().to_string_lossy().into_owned(),
        provider_id: "provider".into(),
        provider_instance_id: "instance".into(),
        instruction: "must not run".into(),
        config: BTreeMap::new(),
        timeout_secs: 10,
    };
    let result = manager.handle(CodingAgentRequest::Start(request), temp.path());
    assert_eq!(
        result.error.unwrap().code,
        "coding_agent_durable_state_unavailable"
    );
    assert_eq!(manager.worker_count(), 0);
    assert_eq!(
        fs::read(store.state_path(id)).unwrap(),
        b"not valid durable state"
    );
}

#[test]
fn error_classification_distinguishes_quota_rate_auth_and_unknown() {
    for (error, expected) in [
        (json!({"code":429}), "rate_limited"),
        (
            json!({"code":429,"data":{"code":"insufficient_quota"}}),
            "quota_exhausted",
        ),
        (json!({"data":{"status":401}}), "authentication_error"),
        (json!({"code":403}), "permission_denied"),
        (json!({"code":503}), "provider_unavailable"),
        (
            json!({"code":-32000,"message":"maybe quota or network"}),
            "prompt_error",
        ),
    ] {
        assert_eq!(prompt_error_kind(&error), expected);
    }
}

#[test]
fn tool_receipts_preserve_identity_partial_status_and_test_output() {
    let update = json!({"params":{"update":{"sessionUpdate":"tool_call_update",
        "toolCallId":"test-123","status":"completed",
        "content":[{"type":"content","content":{"type":"text","text":"3 tests passed"}}]}}});
    let normalized = normalize_update(&update).unwrap();
    assert_eq!(normalized.label.as_deref(), Some("test-123"));
    assert_eq!(normalized.status.as_deref(), Some("completed"));
    assert!(normalized.text.unwrap().contains("3 tests passed"));
}
