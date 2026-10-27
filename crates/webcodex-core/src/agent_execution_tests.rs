use super::*;
use serde_json::json;

pub(crate) fn start_fixture() -> AgentExecutionStart {
    let process = AgentExecutionProcess {
        executable: "worker.exe".into(),
        args: vec!["--task".into(), "bounded work".into()],
    };
    let launch_intent_hash = compute_launch_intent_hash(&process, None, 30).unwrap();
    AgentExecutionStart {
        workspace_source: None,
        binding: AgentExecutionBinding {
            version: 1,
            project: "project-a".into(),
            requester: AgentExecutionRequester {
                enrollment_id: "requester-enrollment".into(),
                run: AgentRunRef {
                    agent_id: "requester-agent".into(),
                    agent_run_id: "requester-run".into(),
                    generation: 1,
                },
                root_task_id: "root-task".into(),
                source_task_id: "source-task".into(),
                call_index: 0,
                cli_attempt_id: "attempt-001".into(),
            },
            executor: AgentExecutionExecutor {
                enrollment_id: "worker-enrollment".into(),
                run: AgentRunRef {
                    agent_id: "worker-agent".into(),
                    agent_run_id: "worker-run".into(),
                    generation: 1,
                },
                task_id: "worker-task".into(),
                cli_session_scope_id: "worker-session".into(),
            },
            target: AgentExecutionTarget {
                client_id: "runner-1".into(),
                runner_instance_id: "instance-1".into(),
            },
            carrier: AgentExecutionCarrier::Job {
                job_id: "job-1".into(),
                start_request_id: "req-1".into(),
            },
            launch_intent_hash,
            resource_manifest_hash: "a".repeat(64),
        },
        resource_set_id: "resources-1".into(),
        process,
        stdin: None,
        timeout_secs: 30,
    }
}

#[test]
fn workspace_source_is_bound_to_launch_and_exact_runner() {
    let mut start = start_fixture();
    let original = start.binding.launch_intent_hash.clone();
    start.workspace_source = Some(AgentExecutionWorkspaceSource {
        project: "agent:runner-1:repo".into(),
        root: "C:/authorized/repo".into(),
    });
    assert!(start.validate().is_err());
    start.binding.launch_intent_hash = compute_workspace_launch_intent_hash(
        &start.process,
        start.stdin.as_deref(),
        start.timeout_secs,
        start.workspace_source.as_ref(),
    )
    .unwrap();
    assert_ne!(original, start.binding.launch_intent_hash);
    start.validate().unwrap();
    start.workspace_source.as_mut().unwrap().root = "C:/another/repo".into();
    assert!(start.validate().is_err());
    start.workspace_source.as_mut().unwrap().project = "agent:other:repo".into();
    start.binding.launch_intent_hash = compute_workspace_launch_intent_hash(
        &start.process,
        start.stdin.as_deref(),
        start.timeout_secs,
        start.workspace_source.as_ref(),
    )
    .unwrap();
    assert_eq!(
        start.validate().unwrap_err(),
        "agent_workspace_source_runner_mismatch"
    );
    start.workspace_source.as_mut().unwrap().root = "C:/state/agent-runtime/other/private".into();
    assert!(start.workspace_source.as_ref().unwrap().validate().is_err());
}

pub(crate) fn close_fixture() -> AgentExecutionClose {
    let member = start_fixture().binding;
    let target = member.target.clone();
    let source = member.requester.run.clone();
    let members = vec![member];
    AgentExecutionClose {
        version: 1,
        close_ref: AgentExecutionCloseRef {
            close_id: "close-1".into(),
            source,
        },
        target,
        deadline_at_ms: 1_000_000,
        members_sha256: compute_members_sha256(&members).unwrap(),
        members,
    }
}

fn receipt_fixture(close: &AgentExecutionClose) -> AgentExecutionCloseReceipt {
    AgentExecutionCloseReceipt {
        version: 1,
        close_ref: close.close_ref.clone(),
        target: close.target.clone(),
        deadline_at_ms: close.deadline_at_ms,
        members_sha256: close.members_sha256.clone(),
        observation_revision: 2,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: close
            .members
            .iter()
            .map(|member| AgentExecutionStopReceipt {
                binding_sha256: member.binding_hash().unwrap(),
                carrier: member.carrier.clone(),
                observation_revision: 1,
                outcome: AgentExecutionStopOutcome::TreeExited {
                    leader_exit_code: None,
                    leader_handle_signaled: true,
                    membership_complete: true,
                    member_handles_signaled: true,
                    worker_finished: true,
                },
            })
            .collect(),
    }
}

fn add_member(close: &mut AgentExecutionClose, index: usize) {
    let mut member = start_fixture().binding;
    member.requester.cli_attempt_id = format!("attempt-{index:03}");
    member.carrier = AgentExecutionCarrier::Job {
        job_id: format!("job-{index}"),
        start_request_id: format!("req-{index}"),
    };
    close.members.push(member);
    close.members_sha256 = compute_members_sha256(&close.members).unwrap();
}

#[test]
fn agent_execution_v1_fixed_hash_vectors_use_typed_order_and_domain_separation() {
    let start = start_fixture();
    assert_eq!(
        start.binding.launch_intent_hash,
        "f5b587bedd856f840f5d802c0340874d7b7b5d32ade283108d29346d54cf4c9f"
    );
    assert_eq!(
        start.binding.identity_hash().unwrap(),
        "204eb7796107c8271f30fb56962334142ee5aa7a57d7f35e83d573536d258d78"
    );
    assert_eq!(
        start.binding.binding_hash().unwrap(),
        "81eb2c99b38da41630e83304955d2dafd85d284e9724a52d387afc9c053e2c94"
    );
    assert_eq!(
        close_fixture().members_sha256,
        "996e00d27284f5a3b3460607da3a037f5b81b625e1c2a6b4a00db942ac64b6c8"
    );
}

#[test]
fn agent_execution_v1_worker_identity_is_independent_and_bounded() {
    let original = start_fixture().binding;
    original.validate().unwrap();
    for field in ["agent", "run", "enrollment", "source_task", "root_task"] {
        let mut value = original.clone();
        match field {
            "agent" => value.executor.run.agent_id = value.requester.run.agent_id.clone(),
            "run" => value.executor.run.agent_run_id = value.requester.run.agent_run_id.clone(),
            "enrollment" => value.executor.enrollment_id = value.requester.enrollment_id.clone(),
            "source_task" => value.executor.task_id = value.requester.source_task_id.clone(),
            _ => value.executor.task_id = value.requester.root_task_id.clone(),
        }
        assert!(value.validate().is_err(), "{field}");
    }
    for id in [
        String::new(),
        " ".into(),
        "bad\nidentity".into(),
        "中".repeat(171),
    ] {
        let mut value = original.clone();
        value.executor.cli_session_scope_id = id;
        assert!(value.validate().is_err());
    }
    let mut value = original;
    value.executor.run.generation = 0;
    assert!(value.validate().is_err());
    value.executor.run.generation = 1;
    value.requester.call_index = 32;
    assert!(value.validate().is_err());
    value.requester.call_index = 31;
    value.executor.cli_session_scope_id = "x".repeat(AGENT_EXECUTION_ID_MAX_BYTES);
    value.validate().unwrap();
}

#[test]
fn agent_execution_v1_strict_nested_shapes_and_versions() {
    let original = serde_json::to_value(start_fixture()).unwrap();
    for pointer in [
        "",
        "/binding",
        "/binding/requester",
        "/binding/requester/run",
        "/binding/executor",
        "/binding/target",
        "/binding/carrier",
        "/process",
    ] {
        let mut value = original.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(true));
        assert!(
            serde_json::from_value::<AgentExecutionStart>(value).is_err(),
            "{pointer}"
        );
    }
    let mut value = start_fixture();
    value.binding.version = 2;
    assert!(value.validate().is_err());
    let mut close = close_fixture();
    close.version = 2;
    assert!(close.validate().is_err());
    let close = close_fixture();
    let original = serde_json::to_value(receipt_fixture(&close)).unwrap();
    for pointer in [
        "",
        "/close_ref",
        "/close_ref/source",
        "/receipts/0",
        "/receipts/0/outcome",
    ] {
        let mut value = original.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(true));
        assert!(
            serde_json::from_value::<AgentExecutionCloseReceipt>(value).is_err(),
            "{pointer}"
        );
    }
    let mut fake = original;
    fake["receipts"][0]["outcome"] = json!({"kind":"not_started"});
    assert!(serde_json::from_value::<AgentExecutionCloseReceipt>(fake).is_err());
}

#[test]
fn agent_execution_v1_launch_hash_validates_exact_input_and_existing_limits() {
    let original = start_fixture();
    original.validate().unwrap();
    let mut value = original.clone();
    value.process.args.push("changed".into());
    assert!(value.validate().is_err());
    for input in [Some("x\0y"), Some("x")] {
        assert!(compute_launch_intent_hash(&original.process, input, 0).is_err());
    }
    assert!(compute_launch_intent_hash(
        &original.process,
        Some(&"x".repeat(PROCESS_STDIN_MAX_BYTES + 1)),
        30
    )
    .is_err());
    assert!(compute_launch_intent_hash(
        &original.process,
        None,
        STRUCTURED_EXECUTION_TIMEOUT_MAX_SECS + 1
    )
    .is_err());
    let mut process = original.process.clone();
    process.args = vec![String::new(); PROCESS_ARG_MAX_COUNT + 1];
    assert!(compute_launch_intent_hash(&process, None, 30).is_err());
    process.args = vec!["x".repeat(PROCESS_ARG_MAX_BYTES + 1)];
    assert!(compute_launch_intent_hash(&process, None, 30).is_err());
    assert_ne!(
        compute_launch_intent_hash(&original.process, None, 30).unwrap(),
        compute_launch_intent_hash(&original.process, Some(""), 30).unwrap()
    );
    let mut value = original;
    value.stdin = Some("private-input-sentinel".into());
    value.process.args.push("private-argument-sentinel".into());
    let debug = format!("{value:?}");
    assert!(!debug.contains("private-input-sentinel"));
    assert!(!debug.contains("private-argument-sentinel"));
}

#[test]
fn agent_execution_v1_hashes_are_acyclic_and_carrier_fingerprints_remain_bound() {
    let mut binding = start_fixture().binding;
    let identity = binding.identity_hash().unwrap();
    let complete = binding.binding_hash().unwrap();
    binding.launch_intent_hash = "b".repeat(64);
    binding.resource_manifest_hash = "c".repeat(64);
    assert_eq!(identity, binding.identity_hash().unwrap());
    assert_ne!(complete, binding.binding_hash().unwrap());
    binding.carrier = AgentExecutionCarrier::CodingAgent {
        coding_agent_run_id: "wc_agent_run_coding-1".into(),
        provider_id: "provider-1".into(),
        provider_instance_id: "provider-instance-1".into(),
        intent_fingerprint: "d".repeat(64),
        authority_fingerprint: format!("auth_{}", "e".repeat(64)),
    };
    let identity = binding.identity_hash().unwrap();
    let complete = binding.binding_hash().unwrap();
    if let AgentExecutionCarrier::CodingAgent {
        intent_fingerprint,
        authority_fingerprint,
        ..
    } = &mut binding.carrier
    {
        *intent_fingerprint = "f".repeat(64);
        *authority_fingerprint = format!("auth_{}", "a".repeat(64));
    }
    assert_eq!(identity, binding.identity_hash().unwrap());
    assert_ne!(complete, binding.binding_hash().unwrap());
    binding.resource_manifest_hash = String::new();
    assert_eq!(identity, binding.identity_hash().unwrap());
    assert!(binding.binding_hash().is_err());
    let mut start = start_fixture();
    start.binding.carrier = binding.carrier;
    assert!(start.validate().is_err());
}

#[test]
fn agent_execution_v1_close_members_are_exact_sorted_unique_and_bounded() {
    let mut close = close_fixture();
    add_member(&mut close, 2);
    close.validate().unwrap();
    let mut value = close.clone();
    value.members.reverse();
    assert!(compute_members_sha256(&value.members).is_err());
    let mut value = close.clone();
    value.members[1].carrier = value.members[0].carrier.clone();
    assert!(compute_members_sha256(&value.members).is_err());
    let mut value = close.clone();
    value.members[1].project = "other-project".into();
    assert!(compute_members_sha256(&value.members).is_err());
    let mut value = close.clone();
    value.target.runner_instance_id = "replacement".into();
    assert!(value.validate().is_err());
    let mut value = close;
    value.deadline_at_ms = 0;
    assert!(value.validate().is_err());
    let mut value = close_fixture();
    value.members = vec![value.members[0].clone(); JOB_INVENTORY_MAX_JOBS + 1];
    assert!(compute_members_sha256(&value.members).is_err());
    // An ancestor source is shape-valid. Only Store can prove the actual lineage.
    let mut value = close_fixture();
    value.close_ref.source.agent_id = "ancestor-agent".into();
    value.close_ref.source.agent_run_id = "ancestor-run".into();
    value.validate().unwrap();
    value.members.clear();
    value.members_sha256 = compute_members_sha256(&[]).unwrap();
    value.validate().unwrap();
}

#[test]
fn agent_execution_v1_close_preserves_real_coding_agent_fingerprint_contract() {
    let mut close = close_fixture();
    add_member(&mut close, 2);
    close.members[1].carrier = AgentExecutionCarrier::CodingAgent {
        coding_agent_run_id: "wc_agent_run_existing".into(),
        provider_id: "codebuddy".into(),
        provider_instance_id: "provider-instance".into(),
        intent_fingerprint: "existing-intent-v1".into(),
        authority_fingerprint: format!("auth_{}", "7".repeat(64)),
    };
    close.members_sha256 = compute_members_sha256(&close.members).unwrap();
    close.validate().unwrap();
    receipt_fixture(&close).validate_for(&close).unwrap();
    let mut unsupported_start = start_fixture();
    unsupported_start.binding = close.members[1].clone();
    assert!(unsupported_start.validate().is_err());
    if let AgentExecutionCarrier::CodingAgent {
        authority_fingerprint,
        ..
    } = &mut close.members[1].carrier
    {
        *authority_fingerprint = "7".repeat(64);
    }
    assert!(compute_members_sha256(&close.members).is_err());
}

#[test]
fn agent_execution_v1_member_serialization_and_sha256_limits_are_enforced() {
    let pad = |prefix: &str| {
        format!(
            "{prefix}{}",
            "x".repeat(AGENT_EXECUTION_ID_MAX_BYTES - prefix.len())
        )
    };
    let mut members = Vec::new();
    for index in 0..JOB_INVENTORY_MAX_JOBS {
        let mut member = start_fixture().binding;
        member.project = pad("project");
        member.requester.enrollment_id = pad("requester-enrollment");
        member.requester.run.agent_id = pad("requester-agent");
        member.requester.run.agent_run_id = pad("requester-run");
        member.requester.root_task_id = pad("root-task");
        member.requester.source_task_id = pad("source-task");
        member.requester.cli_attempt_id = pad(&format!("attempt-{index:03}"));
        member.executor.enrollment_id = pad("executor-enrollment");
        member.executor.run.agent_id = pad("executor-agent");
        member.executor.run.agent_run_id = pad("executor-run");
        member.executor.task_id = pad("executor-task");
        member.executor.cli_session_scope_id = pad("executor-session");
        member.target.client_id = pad("runner");
        member.target.runner_instance_id = pad("instance");
        member.carrier = AgentExecutionCarrier::Job {
            job_id: pad(&format!("job-{index:03}")),
            start_request_id: pad(&format!("request-{index:03}")),
        };
        member.validate().unwrap();
        members.push(member);
    }
    assert!(serde_json::to_vec(&members).unwrap().len() > JOB_INVENTORY_MAX_SERIALIZED_BYTES);
    assert!(compute_members_sha256(&members).is_err());
    for digest in ["A".repeat(64), "a".repeat(63), "g".repeat(64)] {
        let mut binding = start_fixture().binding;
        binding.resource_manifest_hash = digest;
        assert!(binding.validate().is_err());
    }
}

#[test]
fn agent_execution_v1_receipts_require_whole_set_and_true_tree_proofs() {
    let mut close = close_fixture();
    add_member(&mut close, 2);
    let original = receipt_fixture(&close);
    original.validate_for(&close).unwrap();
    let encoded = serde_json::to_value(&original).unwrap();
    assert!(encoded["receipts"][0]["outcome"]["leader_exit_code"].is_null());
    for pointer in [
        "/admission_closed",
        "/tombstone_persisted",
        "/receipts/0/outcome/leader_handle_signaled",
        "/receipts/0/outcome/membership_complete",
        "/receipts/0/outcome/member_handles_signaled",
        "/receipts/0/outcome/worker_finished",
    ] {
        let mut value = encoded.clone();
        *value.pointer_mut(pointer).unwrap() = json!(false);
        assert!(
            serde_json::from_value::<AgentExecutionCloseReceipt>(value)
                .unwrap()
                .validate_for(&close)
                .is_err(),
            "{pointer}"
        );
    }
    let mut value = original.clone();
    value.receipts.pop();
    assert!(value.validate_for(&close).is_err());
    let mut value = original.clone();
    value.receipts.reverse();
    assert!(value.validate_for(&close).is_err());
    let mut value = original.clone();
    value.receipts.push(value.receipts[0].clone());
    assert!(value.validate_for(&close).is_err());
    let mut value = original.clone();
    value.deadline_at_ms += 1;
    assert!(value.validate_for(&close).is_err());
    let mut value = original.clone();
    value.close_ref.close_id = "new-close".into();
    assert!(value.validate_for(&close).is_err());
    let mut value = original.clone();
    value.target.runner_instance_id = "new-instance".into();
    assert!(value.validate_for(&close).is_err());
    let mut value = original.clone();
    value.observation_revision = 0;
    assert!(value.validate_for(&close).is_err());
    let mut value = original;
    value.receipts[0].outcome = AgentExecutionStopOutcome::NeverCreated {
        request_cancelled: true,
        worker_finished: true,
    };
    value.validate_for(&close).unwrap();
    value.receipts[0].outcome = AgentExecutionStopOutcome::NeverCreated {
        request_cancelled: false,
        worker_finished: true,
    };
    assert!(value.validate_for(&close).is_err());
    value.receipts[0].outcome = AgentExecutionStopOutcome::NeverCreated {
        request_cancelled: true,
        worker_finished: false,
    };
    assert!(value.validate_for(&close).is_err());
}

#[test]
fn agent_execution_v1_close_receipt_uses_dedicated_transport_not_stdout() {
    let close = close_fixture();
    let receipt = receipt_fixture(&close);
    let frame = RunnerEnvelope::AgentExecutionCloseReceipt {
        request_id: "close-request".into(),
        receipt,
    };
    let value = serde_json::to_value(&frame).unwrap();
    assert_eq!(value["type"], "agent_execution_close_receipt");
    assert!(value.get("stdout").is_none());
    assert!(value.get("result").is_none());
    let RunnerEnvelope::AgentExecutionCloseReceipt {
        request_id,
        receipt,
    } = serde_json::from_value(value).unwrap()
    else {
        panic!("wrong receipt type")
    };
    assert_eq!(request_id, "close-request");
    receipt.validate_for(&close).unwrap();
    assert!(
        serde_json::from_value::<RunnerAgentExecutionCloseReceiptResponse>(json!({"success":true}))
            .is_err()
    );
    assert!(
        !serde_json::from_value::<RunnerAgentExecutionCloseReceiptResponse>(
            json!({"stored":false})
        )
        .unwrap()
        .stored
    );
}

#[test]
fn agent_execution_v1_capability_is_an_explicit_non_baseline_registration_bit() {
    assert!(!RunnerCapabilities::default().agent_isolated_execution_v1());
    let configured: RunnerCapabilities = serde_json::from_value(json!({
        "agent_isolated_execution_v1":true
    }))
    .unwrap();
    assert!(configured.agent_isolated_execution_v1());
    assert_eq!(
        serde_json::to_value(configured).unwrap()["agent_isolated_execution_v1"],
        true
    );
    assert!(!RUNNER_PROTOCOL_GENERATION_V2_BASELINE_CAPABILITY_NAMES
        .contains(&RUNNER_CAPABILITY_AGENT_ISOLATED_EXECUTION_V1));
}

#[test]
fn agent_execution_resource_facts_are_closed_and_use_opaque_executable_identity() {
    let fact = AgentExecutionResourceDescriptor {
        project_workspace_copy: false,
        reviewed_controls: None,
        provider: None,
        resource_set_id: "fixture".into(),
        manifest_hash: "a".repeat(64),
        executable_identity: format!("sha256:{}", "b".repeat(64)),
    };
    fact.validate().unwrap();
    let mut invalid = fact.clone();
    invalid.executable_identity = "C:/private/program.exe".into();
    assert!(invalid.validate().is_err());
    let mut value = serde_json::to_value(fact).unwrap();
    value["source_path"] = json!("C:/private/program.exe");
    assert!(serde_json::from_value::<AgentExecutionResourceDescriptor>(value).is_err());
}
