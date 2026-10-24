use super::*;
use crate::runner_protocol::agent_execution_tests::{close_fixture, start_fixture};
use crate::runner_protocol::*;

fn wire(operation: RunnerAgentExecutionOperation) -> RunnerRequest {
    let request_id = match &operation {
        RunnerAgentExecutionOperation::Start(start) => match &start.binding.carrier {
            AgentExecutionCarrier::Job {
                start_request_id, ..
            } => start_request_id.clone(),
            _ => unreachable!(),
        },
        RunnerAgentExecutionOperation::Close(_) => "close-request".into(),
    };
    let client_id = operation.target().client_id.clone();
    operation
        .into_v2_request(RunnerInvocationMetadata {
            request_id,
            client_id,
            requested_by: "trusted-controller".into(),
            created_at: 1,
        })
        .unwrap()
}

#[test]
fn agent_execution_v1_codec_round_trips_to_dedicated_canonical_operations() {
    for operation in [
        RunnerAgentExecutionOperation::Start(start_fixture()),
        RunnerAgentExecutionOperation::Close(close_fixture()),
    ] {
        let request = wire(operation.clone());
        assert!(request.command.is_empty());
        assert!(request.process.is_none());
        assert!(request.job_context.is_none());
        assert!(request.stdin.is_none());
        let json = serde_json::to_vec(&request).unwrap();
        let decoded: RunnerRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.decode_agent_execution().unwrap(), operation);
        let canonical = decoded.decode_operation().unwrap();
        assert_eq!(canonical.wire_kind(), operation.wire_kind());
        match &canonical {
            RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(value)) => {
                assert_eq!(value.context, isolated_job_context(&value.start));
                assert!(value.context.workflow_session_id.is_none());
            }
            RunnerOperation::AgentExecutionClose(_) => {}
            _ => panic!("isolated operation downgraded"),
        }
        let reencoded = RunnerRequest::from_operation(
            RunnerInvocationMetadata {
                request_id: decoded.request_id.clone(),
                client_id: decoded.client_id.clone(),
                requested_by: decoded.requested_by.clone(),
                created_at: decoded.created_at,
            },
            canonical,
        )
        .unwrap();
        assert_eq!(reencoded.decode_agent_execution().unwrap(), operation);
        let envelope = RunnerEnvelope::Request { request: decoded };
        let encoded = serde_json::to_vec(&envelope).unwrap();
        let RunnerEnvelope::Request { request } = serde_json::from_slice(&encoded).unwrap() else {
            panic!("wrong envelope");
        };
        assert_eq!(request.decode_agent_execution().unwrap(), operation);
        assert!(request.decode_invocation().is_ok());
    }
}

#[test]
fn agent_execution_v1_binding_cannot_be_smuggled_into_legacy_job_context() {
    let start = start_fixture();
    let mut request = wire(RunnerAgentExecutionOperation::Start(start.clone()));
    request.kind = "run_shell".into();
    request.content = None;
    request.job_context = Some(isolated_job_context(&start));
    assert_eq!(
        request.decode_operation().unwrap_err(),
        "agent_execution_requires_dedicated_kind"
    );
    let mut invalid = RunnerJobIsolatedProcessOperation {
        context: isolated_job_context(&start),
        start,
    };
    invalid.context.workflow_session_id = Some("caller-session".into());
    assert!(RunnerRequest::from_operation(
        RunnerInvocationMetadata {
            request_id: "req-1".into(),
            client_id: "runner-1".into(),
            requested_by: "controller".into(),
            created_at: 1,
        },
        RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(invalid))
    )
    .is_err());
}

#[test]
fn agent_execution_v1_codec_cannot_downgrade_to_shell_process_or_missing_kind() {
    let request = wire(RunnerAgentExecutionOperation::Start(start_fixture()));
    for kind in [
        "run_shell",
        "run_process",
        "start_process_job",
        "start_script_job",
    ] {
        let mut downgraded = request.clone();
        downgraded.kind = kind.into();
        assert!(downgraded.decode_operation().is_err(), "{kind}");
        assert!(downgraded.decode_agent_execution().is_err());
    }
    let mut value = serde_json::to_value(request).unwrap();
    value.as_object_mut().unwrap().remove("kind");
    let missing: RunnerRequest = serde_json::from_value(value).unwrap();
    assert_eq!(missing.kind, "run_shell");
    assert!(missing.decode_operation().is_err());
}

#[test]
fn agent_execution_v1_codec_rejects_envelope_retargeting_and_payload_confusion() {
    let request = wire(RunnerAgentExecutionOperation::Start(start_fixture()));
    let mutations: &[fn(&mut RunnerRequest)] = &[
        |r| r.client_id = "other-runner".into(),
        |r| r.request_id = "different-start".into(),
        |r| r.timeout_secs += 1,
        |r| r.command = "must-not-execute".into(),
        |r| r.stdin = Some("not-a-protocol-carrier".into()),
        |r| r.cwd = Some("C:\\private".into()),
        |r| r.job_id = Some("different-job".into()),
        |r| r.max_bytes = Some(1),
        |r| {
            r.process = Some(ShellProcessArgv {
                executable: "bad.exe".into(),
                args: vec![],
            })
        },
        |r| r.content = None,
        |r| r.content = Some(" ".repeat(JOB_INVENTORY_MAX_SERIALIZED_BYTES + 1)),
    ];
    for mutate in mutations {
        let mut bad = request.clone();
        mutate(&mut bad);
        assert!(bad.decode_agent_execution().is_err());
        assert!(bad.decode_operation().is_err());
    }
    let mut close = wire(RunnerAgentExecutionOperation::Close(close_fixture()));
    close.timeout_secs = 2;
    assert!(close.decode_agent_execution().is_err());
}

#[test]
fn agent_execution_v1_codec_unknown_fields_are_rejected_without_echoing_payload() {
    let mut request = wire(RunnerAgentExecutionOperation::Start(start_fixture()));
    let mut content: serde_json::Value =
        serde_json::from_str(request.content.as_ref().unwrap()).unwrap();
    let marker = "synthetic-private-payload-never-echo";
    content["unauthorized_credential"] = serde_json::json!(marker);
    request.content = Some(content.to_string());
    let error = request.decode_agent_execution().unwrap_err();
    assert_eq!(error, "invalid_agent_execution_payload");
    assert!(!error.contains(marker));
    assert!(!format!(
        "{:?}",
        RunnerAgentExecutionOperation::Start(start_fixture())
    )
    .contains("stdin"));
}

#[test]
fn agent_execution_v1_codec_future_kind_and_noncanonical_close_are_rejected() {
    let mut request = wire(RunnerAgentExecutionOperation::Close(close_fixture()));
    request.kind = "close_agent_execution_v2".into();
    assert!(request.decode_agent_execution().is_err());
    assert!(request.decode_operation().is_err());
    let mut close = close_fixture();
    close.members_sha256 = "0".repeat(64);
    assert!(RunnerAgentExecutionOperation::Close(close)
        .into_v2_request(RunnerInvocationMetadata {
            request_id: "close".into(),
            client_id: request.client_id,
            requested_by: "trusted-controller".into(),
            created_at: 1,
        })
        .is_err());
}
