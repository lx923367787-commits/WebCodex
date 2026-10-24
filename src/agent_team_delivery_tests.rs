use super::*;

fn review_request() -> Value {
    json!({
        "source_attempt_id":"wc_team_cli_attempt_original",
        "expected_worker_revision":1,
        "artifact_id":"wc_martifact_published",
        "artifact_version":2,
        "decision":"accept",
        "content":"The published result answers each requested item.\nChecked the stated calculation against the included inputs.",
        "completion_key":"review-exact-version"
    })
}

fn unpublished_rejection_request() -> Value {
    json!({
        "source_attempt_id":"wc_team_cli_attempt_original",
        "expected_worker_revision":1,
        "reason":"The stopped worker did not publish the required deliverable; return this worker task for follow-up.",
        "completion_key":"reject-missing-delivery"
    })
}

#[test]
fn delivery_read_accepts_only_an_exact_attempt_reference() {
    assert!(matches!(
        parse(
            "team_cli_delivery_read",
            json!({"source_attempt_id":"original-attempt"})
        ),
        Ok(DeliveryRequest::Read(_))
    ));
    for invalid in [
        json!({}),
        json!(null),
        json!("original-attempt"),
        json!({"source_attempt_id":1}),
    ] {
        assert_eq!(parse("team_cli_delivery_read", invalid), Err(INVALID_INPUT));
    }
    assert_eq!(parse("review", review_request()), Err(UNKNOWN_OPERATION));
}

#[test]
fn delivery_inputs_cannot_supply_identity_paths_logs_or_an_approval_flag() {
    for (tool, request) in [
        (
            "team_cli_delivery_read",
            json!({"source_attempt_id":"original-attempt"}),
        ),
        ("team_cli_delivery_review", review_request()),
        (
            "team_cli_delivery_reject_unpublished",
            unpublished_rejection_request(),
        ),
    ] {
        for forbidden in [
            "room_id",
            "path",
            "project",
            "owner_user",
            "agent_id",
            "run_id",
            "worker_task_id",
            "raw_logs",
            "permit",
            "reviewed",
            "stopped",
            "stop_proof",
            "terminal_result",
        ] {
            let mut forged = request.clone();
            forged[forbidden] = json!(true);
            assert_eq!(
                parse(tool, forged),
                Err(INVALID_INPUT),
                "{tool}: {forbidden}"
            );
        }
    }
}

#[test]
fn delivery_review_requires_every_reference_and_an_explicit_decision() {
    let request = review_request();
    for field in request.as_object().unwrap().keys() {
        let mut missing = request.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert_eq!(
            parse("team_cli_delivery_review", missing),
            Err(INVALID_INPUT),
            "{field}"
        );
    }
    let mut rejected = request.clone();
    rejected["decision"] = json!("reject");
    assert!(matches!(
        parse("team_cli_delivery_review", rejected),
        Ok(DeliveryRequest::Review(DeliveryReview {
            decision: ReviewDecision::Reject,
            ..
        }))
    ));
    for decision in [
        json!(true),
        json!("reviewed"),
        json!("succeeded"),
        json!("ACCEPT"),
        json!(null),
    ] {
        let mut invalid = request.clone();
        invalid["decision"] = decision;
        assert_eq!(
            parse("team_cli_delivery_review", invalid),
            Err(INVALID_INPUT)
        );
    }
}

#[test]
fn delivery_review_rejects_invalid_revisions_and_unbounded_identifiers() {
    for field in ["expected_worker_revision", "artifact_version"] {
        for invalid in [json!(0), json!(-1), json!(1.5), json!("1"), json!(null)] {
            let mut request = review_request();
            request[field] = invalid;
            assert_eq!(
                parse("team_cli_delivery_review", request),
                Err(INVALID_INPUT)
            );
        }
    }
    for field in ["source_attempt_id", "artifact_id", "completion_key"] {
        for invalid in [
            String::new(),
            " ".into(),
            "x\ny".into(),
            "x".repeat(MAX_ID_BYTES + 1),
        ] {
            let mut request = review_request();
            request[field] = json!(invalid);
            assert_eq!(
                parse("team_cli_delivery_review", request),
                Err(INVALID_INPUT)
            );
        }
    }
}

#[test]
fn delivery_review_content_is_bounded_in_utf8_bytes_and_preserves_multiline_grounds() {
    assert!(parse("team_cli_delivery_review", review_request()).is_ok());
    let exact_bytes = format!("{}ab", "验".repeat(2730));
    assert_eq!(exact_bytes.len(), MAX_REVIEW_BYTES);
    let mut request = review_request();
    request["content"] = json!(exact_bytes);
    assert!(parse("team_cli_delivery_review", request).is_ok());
    for invalid in [
        String::new(),
        " \n\t".into(),
        "proof\0hidden".into(),
        "proof\u{1b}[31m".into(),
        "验".repeat(2731),
    ] {
        let mut request = review_request();
        request["content"] = json!(invalid);
        assert_eq!(
            parse("team_cli_delivery_review", request),
            Err(INVALID_INPUT)
        );
    }
}

#[test]
fn delivery_schemas_keep_the_operations_narrow() {
    let specs = tool_specs();
    assert_eq!(specs.len(), 3);
    for spec in &specs {
        let name = spec["name"].as_str().unwrap();
        assert!(supports(name));
        assert_eq!(spec["inputSchema"]["additionalProperties"], false);
        for forbidden in [
            "room_id", "path", "project", "agent_id", "raw_logs", "permit", "reviewed",
        ] {
            assert!(spec["inputSchema"]["properties"].get(forbidden).is_none());
        }
    }
    let review = specs
        .iter()
        .find(|spec| spec["name"] == "team_cli_delivery_review")
        .unwrap();
    assert_eq!(
        review["inputSchema"]["properties"]["decision"]["enum"],
        json!(["accept", "reject"])
    );
    assert!(!supports("team_cli_start"));
}

#[test]
fn delivery_read_preserves_the_complete_published_text_and_exact_reference() {
    use sha2::{Digest, Sha256};
    use webcodex_store::{AgentCliDeliveryReference, AgentCliDeliveryView, AgentTaskState};

    let text = "Line one\r\nLine two\n结果";
    let bundle = format!(
        "\n{}\r\n",
        serde_json::to_string_pretty(&json!({
            "version":1,
            "binding_sha256":"b".repeat(64),
            "summary":"Explicit candidate for review, not an acceptance decision.",
            "files":[{"path":"result.txt","sha256":format!("{:x}", Sha256::digest(text.as_bytes())),"bytes":text.len(),"text":text}]
        })).unwrap()
    );
    let artifact_sha256 = format!("{:x}", Sha256::digest(bundle.as_bytes()));
    let view = AgentCliDeliveryView {
        delivery: AgentCliDeliveryReference {
            source_attempt_id: "wc_team_cli_attempt_original".into(),
            room_id: "server-selected-room".into(),
            entry_id: "server-selected-entry".into(),
            artifact: AgentMeetingArtifactTarget {
                artifact_id: "wc_martifact_original".into(),
                version: 1,
            },
            artifact_sha256: artifact_sha256.clone(),
            worker_agent_id: "worker-agent".into(),
            worker_run_id: "worker-run".into(),
            worker_generation: 1,
            published_at_unix_ms: 1000,
        },
        artifact_content: bundle.clone(),
        worker_task_id: "worker-task".into(),
        worker_revision: 1,
        worker_state: AgentTaskState::Active,
        review: None,
    };
    let result = store_output(Ok(view));
    assert!(result.success);
    assert_eq!(
        result.output["artifact_content"].as_str(),
        Some(bundle.as_str())
    );
    assert_eq!(
        result.output["delivery"]["artifact_sha256"],
        artifact_sha256
    );
    assert_eq!(result.output["worker_state"], "active");
    assert_eq!(result.output["worker_revision"], 1);
    assert!(result.output["review"].is_null());
    let read_spec = tool_specs()
        .into_iter()
        .find(|spec| spec["name"] == "team_cli_delivery_read")
        .unwrap();
    assert!(read_spec["description"]
        .as_str()
        .unwrap()
        .contains("untrusted candidate data"));
}

#[test]
fn delivery_store_errors_do_not_publish_private_diagnostics_or_claim_no_commit() {
    let result = store_output::<Value>(Err(CommunicationStoreError::new(
        "agent_cli_delivery_denied",
        "private diagnostic, credential or worker path must not cross the model boundary",
    )));
    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some("agent_cli_delivery_denied"));
    assert!(result.output.is_null());
}

#[test]
fn unpublished_rejection_requires_only_an_exact_attempt_revision_reason_and_key() {
    let request = unpublished_rejection_request();
    assert!(matches!(
        parse("team_cli_delivery_reject_unpublished", request.clone()),
        Ok(DeliveryRequest::RejectUnpublished(_))
    ));
    for field in request.as_object().unwrap().keys() {
        let mut missing = request.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert_eq!(
            parse("team_cli_delivery_reject_unpublished", missing),
            Err(INVALID_INPUT),
            "{field}"
        );
    }
    for forbidden in [
        "decision",
        "accept",
        "artifact_id",
        "artifact_version",
        "artifact_content",
        "auto_failed",
        "worker_state",
        "reviewed",
        "stop_proof",
    ] {
        let mut forged = request.clone();
        forged[forbidden] = json!(true);
        assert_eq!(
            parse("team_cli_delivery_reject_unpublished", forged),
            Err(INVALID_INPUT),
            "{forbidden}"
        );
    }
    for revision in [json!(0), json!(-1), json!(1.5), json!("1"), json!(null)] {
        let mut invalid = request.clone();
        invalid["expected_worker_revision"] = revision;
        assert_eq!(
            parse("team_cli_delivery_reject_unpublished", invalid),
            Err(INVALID_INPUT)
        );
    }
    for field in ["source_attempt_id", "completion_key"] {
        for value in [
            String::new(),
            " ".into(),
            "x\ny".into(),
            "x".repeat(MAX_ID_BYTES + 1),
        ] {
            let mut invalid = request.clone();
            invalid[field] = json!(value);
            assert_eq!(
                parse("team_cli_delivery_reject_unpublished", invalid),
                Err(INVALID_INPUT)
            );
        }
    }
}

#[test]
fn unpublished_rejection_reason_is_nonempty_bounded_in_bytes_and_not_normalized() {
    let exact_bytes = format!("{}a", "退".repeat(1365));
    assert_eq!(exact_bytes.len(), MAX_REJECTION_REASON_BYTES);
    let mut request = unpublished_rejection_request();
    request["reason"] = json!(exact_bytes);
    assert!(parse("team_cli_delivery_reject_unpublished", request).is_ok());
    let original = "  No required delivery was published.\r\nReturn the stopped worker task.  ";
    let mut request = unpublished_rejection_request();
    request["reason"] = json!(original);
    let Ok(DeliveryRequest::RejectUnpublished(parsed)) =
        parse("team_cli_delivery_reject_unpublished", request)
    else {
        panic!("valid explicit rejection was rejected");
    };
    assert_eq!(parsed.reason, original);
    for value in [
        String::new(),
        " \n\t".into(),
        "reason\0hidden".into(),
        "x".repeat(4097),
        "退".repeat(1366),
    ] {
        let mut invalid = unpublished_rejection_request();
        invalid["reason"] = json!(value);
        assert_eq!(
            parse("team_cli_delivery_reject_unpublished", invalid),
            Err(INVALID_INPUT)
        );
    }
}

#[test]
fn unpublished_rejection_schema_exposes_no_success_or_artifact_override() {
    let specs = tool_specs();
    let spec = specs
        .iter()
        .find(|spec| spec["name"] == "team_cli_delivery_reject_unpublished")
        .unwrap();
    let properties = spec["inputSchema"]["properties"].as_object().unwrap();
    let mut fields: Vec<_> = properties.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "completion_key",
            "expected_worker_revision",
            "reason",
            "source_attempt_id"
        ]
    );
    assert_eq!(spec["inputSchema"]["required"].as_array().unwrap().len(), 4);
    assert_eq!(properties["expected_worker_revision"]["minimum"], 1);
    assert_eq!(
        properties["reason"]["maxLength"],
        MAX_REJECTION_REASON_BYTES
    );
    assert_eq!(spec["inputSchema"]["additionalProperties"], false);
}

#[test]
fn unpublished_rejection_returns_the_store_receipt_without_claiming_parent_completion() {
    use webcodex_store::{AgentCliDeliveryRejectionResult, AgentTaskState};

    let receipt = AgentCliDeliveryRejectionResult {
        source_attempt_id: "wc_team_cli_attempt_original".into(),
        worker_task_id: "wc_agent_task_worker".into(),
        worker_revision: 2,
        worker_state: AgentTaskState::Failed,
        replayed: true,
    };
    let expected = serde_json::to_value(&receipt).unwrap();
    let result = store_output(Ok(receipt));
    assert!(result.success);
    assert_eq!(result.output, expected);
    assert!(result.output.get("artifact").is_none());
    assert!(result.output.get("caller_completed").is_none());
    assert!(result.output.get("root_completed").is_none());
}
