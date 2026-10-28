use super::*;
use crate::runner_operation::{RunnerInvocationMetadata, RunnerOperation};
use crate::runner_protocol::RunnerRequest;

fn start() -> AgentProgramStart {
    let mut value = AgentProgramStart {
        binding: AgentProgramBinding {
            local_service_port: None,
            resume_from_program: None,
            version: 1,
            program_id: "wc_program_fixture".into(),
            enrollment_id: "wc_enrollment_fixture".into(),
            task_id: "wc_agent_task_fixture".into(),
            run: AgentRunRef {
                agent_id: "wc_iagent_fixture".into(),
                agent_run_id: "wc_agent_run_fixture".into(),
                generation: 1,
            },
            project: "logical-project".into(),
            target: AgentExecutionTarget {
                client_id: "runner".into(),
                runner_instance_id: "instance".into(),
            },
            job_id: "job-fixture".into(),
            start_request_id: "request-fixture".into(),
            workspace_project: "agent:runner:project".into(),
            workspace_root: "E:\\authorized-project".into(),
            relative_cwd: "app".into(),
            resource_set_id: "python".into(),
            resource_manifest_hash: "a".repeat(64),
            launch_intent_hash: "b".repeat(64),
            expires_at_unix_ms: 1_900_000_000_000,
        },
        args: vec!["-m".into(), "unittest".into()],
        stdin: None,
        timeout_secs: 60,
    };
    value.binding.launch_intent_hash = value.compute_launch_hash().unwrap();
    value
}
fn metadata() -> RunnerInvocationMetadata {
    RunnerInvocationMetadata {
        request_id: "request-fixture".into(),
        client_id: "runner".into(),
        requested_by: "verified-agent".into(),
        created_at: 100,
    }
}

#[test]
fn program_service_and_resume_are_fenced_and_inspection_is_read_only_wire() {
    let ordinary = start();
    let mut service = ordinary.clone();
    service.binding.local_service_port = Some(18080);
    assert!(service.validate().is_err());
    service.binding.launch_intent_hash = service.compute_launch_hash().unwrap();
    service.validate().unwrap();
    let mut changed = service.clone();
    changed.binding.resume_from_program = Some("previous-program".into());
    assert!(changed.validate().is_err());
    changed.binding.launch_intent_hash = changed.compute_launch_hash().unwrap();
    changed.validate().unwrap();
    let query = AgentProgramInspect {
        target: changed.binding.target.clone(),
        binding: changed.binding,
    };
    let wire = RunnerRequest::from_operation(
        metadata(),
        RunnerOperation::AgentProgramInspect(query.clone()),
    )
    .unwrap();
    assert_eq!(wire.kind, AGENT_PROGRAM_INSPECT_KIND);
    let RunnerOperation::AgentProgramInspect(decoded) = wire.decode_operation().unwrap() else {
        panic!("inspection required")
    };
    assert_eq!(decoded, query);
    let mut mixed = wire;
    mixed.command = "shell bypass".into();
    assert!(mixed.decode_operation().is_err());
}

#[test]
fn ordinary_program_round_trip_has_own_identity_and_no_cli_slot_or_worker() {
    let input = start();
    input.validate().unwrap();
    let operation = RunnerOperation::Job(input.clone().into_job());
    let wire = RunnerRequest::from_operation(metadata(), operation.clone()).unwrap();
    assert_eq!(wire.kind, AGENT_PROGRAM_START_KIND);
    let RunnerOperation::Job(crate::runner_operation::RunnerJobOperation::StartProcess(decoded)) =
        wire.decode_operation().unwrap()
    else {
        panic!("ordinary program job required")
    };
    assert_eq!(AgentProgramStart::from_job(&decoded).unwrap(), input);
    let encoded = serde_json::to_string(&wire).unwrap();
    for forbidden in [
        "cli_attempt_id",
        "cli_session_scope_id",
        "call_index",
        "provider_id",
    ] {
        assert!(!encoded.contains(forbidden));
    }
}

#[test]
fn ordinary_program_rejects_changed_launch_and_mixed_generic_wire_fields() {
    let mut changed = start();
    changed.args.push("different.py".into());
    assert!(changed.validate().is_err());
    let operation = RunnerOperation::Job(start().into_job());
    let wire = RunnerRequest::from_operation(metadata(), operation).unwrap();
    let mut mixed = wire.clone();
    mixed.command = "cmd /c echo bypass".into();
    assert!(mixed.decode_operation().is_err());
    let mut generic = wire.clone();
    generic.kind = "start_process_job".into();
    assert!(generic.decode_operation().is_err());
    let mut wrong = wire.clone();
    wrong.client_id = "another-runner".into();
    assert!(wrong.decode_operation().is_err());
    let mut wrong = wire;
    wrong.request_id = "another-request".into();
    assert!(wrong.decode_operation().is_err());
}

#[test]
fn ordinary_program_paths_and_limits_are_explicit() {
    for path in [
        "../private",
        "app/../secret",
        "/root",
        "C:/secret",
        "app\\secret",
        "app//next",
    ] {
        assert!(validate_relative_cwd(path).is_err(), "{path}");
    }
    assert!(validate_relative_cwd(".").is_ok());
    assert!(validate_relative_cwd("公开项目/tests").is_ok());
    let mut invalid = start();
    invalid.timeout_secs = 0;
    assert!(invalid.compute_launch_hash().is_err());
    invalid.timeout_secs = 86401;
    assert!(invalid.compute_launch_hash().is_err());
    let mut invalid = start();
    invalid.args = vec!["x".into(); 257];
    assert!(invalid.compute_launch_hash().is_err());
}
