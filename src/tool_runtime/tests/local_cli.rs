use super::super::*;
use super::support::*;
use crate::runner_protocol::RunnerCapabilities;
use serde_json::json;
use webcodex_core::runner_operation::{RunnerJobOperation, RunnerOperation};

async fn fixture(
    budget: u32,
    capable: bool,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::Database>,
    ToolRuntime,
    webcodex_store::VerifiedAgentRun,
    String,
) {
    let temp = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(crate::Database::open(&temp.path().join("state.db")).unwrap());
    let user = crate::test_support::seed_user(&db, "local-cli-owner");
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let runtime = test_runtime()
        .with_communication_database(db.clone())
        .with_permission_evaluator(PermissionEvaluator::with_mode(AuthorityMode::TrustedAgent));
    register_agent_with_projects(
        &runtime,
        "local-runner",
        Some(&user.username),
        RunnerCapabilities {
            local_cli: capable,
            detached_process_jobs: true,
            file_read: true,
            file_write: true,
            shell: true,
            structured_process_argv: true,
            structured_execution_jobs: true,
            ..Default::default()
        },
        vec![named_registered_project(
            "local-runner",
            "repo",
            "repo",
            &root.to_string_lossy(),
            1,
        )],
    )
    .await;
    let project = runner_project_runtime_id("local-runner", "repo");
    let issued = db
        .register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
        .unwrap();
    let host = db
        .authenticate_native_agent_host(&issued.into_secret())
        .unwrap();
    db.configure_native_host_auto_enrollment(
        host.host_id(),
        &user.id,
        &project,
        webcodex_store::AgentEnrollmentPolicy {
            max_subagents: 0,
            max_cli_calls: budget,
        },
    )
    .unwrap();
    let binding = db
        .ensure_native_host_window(
            &host,
            &"c".repeat(64),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    (temp, db, runtime, binding, project)
}

#[tokio::test]
async fn local_cli_discovery_is_project_bound_and_uses_the_runner_protocol() {
    let (_temp, _, runtime, binding, project) = fixture(1, true).await;
    let pending = tokio::spawn({
        let runtime = runtime.clone();
        let binding = binding.clone();
        let project = project.clone();
        async move {
            super::super::local_cli::call(
                &runtime,
                &binding,
                json!({"action":"discover","project":project,"commands":["new-cli"]}),
            )
            .await
        }
    });
    let request = wait_for_patch_agent_request(&runtime, "local-runner").await;
    assert_eq!(request.kind, "local_cli_discover");
    assert!(request.command.is_empty());
    assert!(matches!(
        request.decode_operation().unwrap(),
        RunnerOperation::LocalCliDiscover(_)
    ));
    complete_patch_agent_request(&runtime,"local-runner",&request.request_id,0,
        r#"{"programs":[{"command":"new-cli","executable":"C:/Users/Test/bin/new-cli.exe","reason_code":null}],"execution_mode":"runner_user"}"#,"").await;
    let found = pending.await.unwrap();
    assert!(found.success, "{:?}", found.error);
    assert_eq!(
        found.output["inventory"]["programs"][0]["command"],
        "new-cli"
    );
    let denied = super::super::local_cli::call(
        &runtime,
        &binding,
        json!({"action":"discover","project":"agent:foreign:repo","commands":["new-cli"]}),
    )
    .await;
    assert!(!denied.success);
    assert!(probe_patch_agent_request(&runtime, "local-runner")
        .await
        .is_none());
}

#[tokio::test]
async fn local_cli_start_accepts_native_argv_without_a_provider_or_frozen_plan_and_replays() {
    let (_temp, db, runtime, binding, project) = fixture(2, true).await;
    let args = json!({"action":"start","project":project,"executable":"custom-ai-cli",
        "args":["--model","locally-discovered-model","--effort","low"],"stdin":"task; literal input",
        "model":"locally-discovered-model","effort":"low","idempotency_key":"one"});
    let started = super::super::local_cli::call(&runtime, &binding, args.clone()).await;
    assert!(started.success, "{:?} {}", started.error, started.output);
    let id = started.output["call"]["call_id"].as_str().unwrap();
    assert!(!id.is_empty());
    let request = wait_for_patch_agent_request(&runtime, "local-runner").await;
    let RunnerOperation::Job(RunnerJobOperation::StartDetachedProcess(operation)) =
        request.decode_operation().unwrap()
    else {
        panic!("Expected owned native process Job")
    };
    assert_eq!(operation.process.executable, "custom-ai-cli");
    assert_eq!(operation.stdin.as_deref(), Some("task; literal input"));
    assert_eq!(
        operation.process.args,
        ["--model", "locally-discovered-model", "--effort", "low"]
    );
    let replay = super::super::local_cli::call(&runtime, &binding, args.clone()).await;
    assert_eq!(replay.output["call"]["call_id"], id);
    assert!(probe_patch_agent_request(&runtime, "local-runner")
        .await
        .is_none());
    let mut changed = args;
    changed["stdin"] = json!("different intent");
    let denied = super::super::local_cli::call(&runtime, &binding, changed).await;
    assert_eq!(
        denied.output["error_kind"],
        "local_cli_idempotency_conflict"
    );
    let record = db.read_local_cli_call(&binding, id).unwrap();
    assert!(record.job_id.is_some());
    assert_eq!(record.project, project);
    update_job(&runtime, &request, "completed", true, Some(0)).await;
    let observed =
        super::super::local_cli::call(&runtime, &binding, json!({"action":"observe","call_id":id}))
            .await;
    assert!(observed.success, "{:?}", observed.error);
    assert_eq!(observed.output["process_success"], true);
    assert_eq!(observed.output["task_acceptance_verified"], false);
    assert!(observed.output["stdout_tail"]
        .as_str()
        .unwrap()
        .contains("native-result"));
}

async fn update_job(
    runtime: &ToolRuntime,
    request: &crate::runner_protocol::RunnerRequest,
    status: &str,
    finished: bool,
    exit_code: Option<i32>,
) {
    runtime
        .runner_registry
        .update_job(crate::runner_protocol::RunnerJobUpdateRequest {
            client_id: "local-runner".into(),
            runner_instance_id: "inst".into(),
            job_id: request.job_id.clone().unwrap(),
            request_id: Some(request.request_id.clone()),
            update_seq: None,
            status: status.into(),
            stdout_chunk: finished.then(|| "native-result\n".into()),
            stderr_chunk: None,
            stdout_tail: None,
            stderr_tail: None,
            log_snapshot: None,
            exit_code,
            duration_ms: finished.then_some(5),
            error: None,
            command_execution_state: finished
                .then_some(crate::runner_protocol::ShellCommandExecutionState::Completed),
            validation_progress: None,
            activity: None,
            finished,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn local_cli_stop_acknowledgement_requires_terminal_runner_evidence() {
    let (_temp, _, runtime, binding, project) = fixture(1, true).await;
    let started = super::super::local_cli::call(
        &runtime,
        &binding,
        json!({"action":"start","project":project,
        "executable":"custom-ai-cli","args":["--model","explicit"],"idempotency_key":"stop-me"}),
    )
    .await;
    assert!(started.success, "{:?}", started.error);
    let id = started.output["call"]["call_id"].as_str().unwrap();
    let request = wait_for_patch_agent_request(&runtime, "local-runner").await;
    update_job(&runtime, &request, "running", false, None).await;
    let stopping =
        super::super::local_cli::call(&runtime, &binding, json!({"action":"stop","call_id":id}))
            .await;
    assert!(stopping.success, "{:?}", stopping.error);
    assert_eq!(stopping.output["terminal"], false);
    assert_eq!(stopping.output["process_success"], false);
    let stop = wait_for_patch_agent_request(&runtime, "local-runner").await;
    assert_eq!(stop.kind, "stop_job");
    update_job(&runtime, &request, "stopped", true, None).await;
    let observed =
        super::super::local_cli::call(&runtime, &binding, json!({"action":"observe","call_id":id}))
            .await;
    assert!(observed.success, "{:?}", observed.error);
    assert_eq!(observed.output["terminal"], true);
    assert_eq!(observed.output["process_success"], false);
}

#[tokio::test]
async fn local_cli_start_keeps_existing_budget_and_reports_old_runner() {
    for (budget, capable, expected) in [
        (0, true, "agent_role_denied"),
        (1, false, "local_cli_runner_upgrade_required"),
    ] {
        let (_temp, _, runtime, binding, project) = fixture(budget, capable).await;
        let result = super::super::local_cli::call(
            &runtime,
            &binding,
            json!({"action":"start","project":project,
            "executable":"arbitrary-cli","args":["--help"],"idempotency_key":"one"}),
        )
        .await;
        assert_eq!(result.output["error_kind"], expected, "{:?}", result.error);
        assert!(runtime.runner_registry.list_jobs(Some(10)).await.is_empty());
        assert!(probe_patch_agent_request(&runtime, "local-runner")
            .await
            .is_none());
    }
}
