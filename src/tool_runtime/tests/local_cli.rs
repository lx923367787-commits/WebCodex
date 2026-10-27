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
            invoke(
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
    let denied = invoke(
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
    let started = invoke(&runtime, &binding, args.clone()).await;
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
    let replay = invoke(&runtime, &binding, args.clone()).await;
    assert_eq!(replay.output["call"]["call_id"], id);
    assert!(probe_patch_agent_request(&runtime, "local-runner")
        .await
        .is_none());
    let mut changed = args;
    changed["stdin"] = json!("different intent");
    let denied = invoke(&runtime, &binding, changed).await;
    assert_eq!(
        denied.output["error_kind"],
        "local_cli_idempotency_conflict"
    );
    let record = db.read_local_cli_call(&binding, id).unwrap();
    assert!(record.job_id.is_some());
    assert_eq!(record.project, project);
    update_job(&runtime, &request, "completed", true, Some(0)).await;
    let observed = invoke(&runtime, &binding, json!({"action":"observe","call_id":id})).await;
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
    update_job_text(
        runtime,
        request,
        status,
        finished,
        exit_code,
        "native-result\n",
    )
    .await;
}

async fn update_job_text(
    runtime: &ToolRuntime,
    request: &crate::runner_protocol::RunnerRequest,
    status: &str,
    finished: bool,
    exit_code: Option<i32>,
    stdout: &str,
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
            stdout_chunk: finished.then(|| stdout.into()),
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
    let started = invoke(
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
    let stopping = invoke(&runtime, &binding, json!({"action":"stop","call_id":id})).await;
    assert!(stopping.success, "{:?}", stopping.error);
    assert_eq!(stopping.output["terminal"], false);
    assert_eq!(stopping.output["process_success"], false);
    let stop = wait_for_patch_agent_request(&runtime, "local-runner").await;
    assert_eq!(stop.kind, "stop_job");
    update_job(&runtime, &request, "stopped", true, None).await;
    let observed = invoke(&runtime, &binding, json!({"action":"observe","call_id":id})).await;
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
        let result = invoke(
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

async fn invoke(
    runtime: &ToolRuntime,
    binding: &webcodex_store::VerifiedAgentRun,
    mut input: serde_json::Value,
) -> ToolResult {
    let action = input.as_object_mut().unwrap().remove("action").unwrap();
    let action = match action.as_str().unwrap() {
        "observe" => "status",
        "stop" => "cancel",
        other => other,
    };
    crate::agent_team_execution::call(
        runtime,
        binding,
        "cli",
        json!({"action":action,"request":input}),
    )
    .await
}

#[test]
fn local_cli_has_one_public_unified_tool_and_read_only_discovery_scope() {
    let specs = crate::agent_isolation::tool_specs();
    let tools = specs["tools"].as_array().unwrap();
    assert_eq!(tools.iter().filter(|tool| tool["name"] == "cli").count(), 1);
    assert!(!tools.iter().any(|tool| tool["name"] == "local_cli"));
    assert_eq!(
        crate::agent_host_auth::tool_scope("cli", &json!({"action":"discover"})),
        "project:read"
    );
    assert_eq!(
        crate::agent_host_auth::tool_scope("cli", &json!({"action":"start"})),
        "project:write"
    );
}

struct NativeInventory(webcodex_core::agent_cli::AgentCliInventory);

#[tokio::test]
async fn local_cli_server_and_runner_replacement_keeps_receipt_and_never_restarts_the_job() {
    use crate::runner_protocol::*;
    let (_temp, db, runtime, binding, project) = fixture(1, true).await;
    let input = json!({"action":"start","project":project,"executable":"installed-cli","args":["--model","chosen"],"idempotency_key":"survive-update"});
    let initial = invoke(&runtime, &binding, input.clone()).await;
    assert!(initial.success, "{:?}", initial.error);
    let id = initial.output["call"]["call_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = wait_for_patch_agent_request(&runtime, "local-runner").await;
    let original = db.read_local_cli_call(&binding, &id).unwrap();
    let user = db.get_user_by_id(binding.owner_user()).unwrap().unwrap();
    let next = test_runtime()
        .with_communication_database(db.clone())
        .with_permission_evaluator(PermissionEvaluator::with_mode(AuthorityMode::TrustedAgent));
    next.runner_registry
        .register(RunnerRegisterRequest {
            agent_execution_resources: vec![],
            process_started_at: None,
            build: None,
            job_concurrency_limit: None,
            coding_agent_providers: None,
            coding_agent_inventory: None,
            client_id: "local-runner".into(),
            runner_instance_id: "after-update".into(),
            runner_protocol_generation: RUNNER_PROTOCOL_GENERATION_V2,
            display_name: None,
            owner: Some(user.username),
            hostname: None,
            host_context: None,
            policy: None,
            capabilities: crate::test_support::current_runner_capabilities(RunnerCapabilities {
                local_cli: true,
                detached_process_jobs: true,
                job_state_reconciliation: true,
                file_read: true,
                file_write: true,
                ..Default::default()
            }),
            job_inventory: Some(ShellJobInventory {
                active_complete: true,
                jobs: vec![ShellJobSnapshot {
                    job_id: original.job_id.clone().unwrap(),
                    request_id: request.request_id.clone(),
                    status: "running".into(),
                    update_seq: 1,
                    created_at: request.created_at,
                    started_at: Some(request.created_at),
                    ended_at: None,
                    exit_code: None,
                    duration_ms: None,
                    error: None,
                    command_execution_state: None,
                    context: request.job_context.clone().unwrap(),
                    stdout: Default::default(),
                    stderr: Default::default(),
                    validation_progress: None,
                    activity: Some(ShellJobActivity {
                        state: ShellJobActivityState::Working,
                        phase: ShellJobActivityPhase::ProcessRunning,
                        source: ShellJobActivitySource::RunnerExecution,
                    }),
                }],
            }),
        })
        .await
        .unwrap();
    crate::test_support::apply_project_inventory_snapshot(
        &next.runner_registry,
        "local-runner",
        "after-update",
        vec![named_registered_project(
            "local-runner",
            "repo",
            "repo",
            &original.root,
            2,
        )],
    )
    .await;
    let status = invoke(&next, &binding, json!({"action":"observe","call_id":id})).await;
    assert!(status.success, "{:?}", status.error);
    assert_eq!(
        status.output["job"]["job_id"],
        original.job_id.clone().unwrap()
    );
    assert_eq!(status.output["job"]["status"], "running");
    let history = invoke(&next, &binding, json!({"action":"observe"})).await;
    assert!(history.success);
    assert_eq!(history.output["calls"].as_array().unwrap().len(), 1);
    assert_eq!(history.output["calls"][0]["call_id"], id);
    let replay = invoke(&next, &binding, input).await;
    assert_eq!(replay.output["call_id"], id);
    assert!(
        next.runner_registry
            .poll(RunnerPollRequest {
                client_id: "local-runner".into(),
                runner_instance_id: "after-update".into()
            })
            .await
            .unwrap()
            .is_none(),
        "A recovered job must never be started a second time"
    );
    assert!(
        next.runner_registry
            .poll(RunnerPollRequest {
                client_id: "local-runner".into(),
                runner_instance_id: "inst".into()
            })
            .await
            .is_err(),
        "The retired Runner instance must remain fenced"
    );
    assert_eq!(
        db.read_local_cli_call(&binding, &id).unwrap().job_id,
        original.job_id
    );
}
impl crate::agent_team_execution::AgentTeamAssets for NativeInventory {
    fn catalog<'a>(
        &'a self,
        _: &'a webcodex_store::VerifiedAgentRun,
    ) -> crate::agent_team_execution::AssetFuture<'a, crate::agent_team_execution::AgentTeamCatalog>
    {
        Box::pin(async { Err("isolated resources must not be consulted for a native call".into()) })
    }
    fn native_inventory<'a>(
        &'a self,
        _: &'a webcodex_store::VerifiedAgentRun,
        _: Option<webcodex_core::agent_cli::AgentCliProvider>,
    ) -> crate::agent_team_execution::AssetFuture<'a, webcodex_core::agent_cli::AgentCliInventory>
    {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

#[tokio::test]
async fn local_cli_selected_third_party_adapter_executes_without_isolation_and_checks_provider_result(
) {
    use webcodex_core::agent_cli::{AgentCliCapabilities, AgentCliInstallation, AgentCliInventory};
    use webcodex_core::cli_adapter::*;
    for provider_error in [false, true] {
        let (_temp, db, runtime, binding, project) = fixture(1, true).await;
        let adapter = CliAdapter {
            id: "third-party".into(),
            label: "Third party".into(),
            executables: vec!["third-party-cli".into()],
            user_paths: vec![],
            metadata: CliMetadataRecipe::Help,
            invocation: Some(CliInvocation {
                prefix_args: vec![],
                model_flag: "--engine".into(),
                effort_flag: Some("--thinking".into()),
                effort_required: true,
                fixed_args: vec!["--json".into()],
                input: CliInputFormat::Text,
                result: Some(CliResultContract {
                    format: CliResultFormat::Json,
                    failure_flag: "is_error".into(),
                    terminal_field: Some("type".into()),
                    terminal_value: Some("result".into()),
                }),
            }),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let inventory = AgentCliInventory {
            version: 1,
            target: crate::runner_protocol::AgentExecutionTarget {
                client_id: "local-runner".into(),
                runner_instance_id: "inst".into(),
            },
            observed_at_ms: now,
            expires_at_ms: now + 60_000,
            installations: vec![AgentCliInstallation {
                provider_id: adapter.id.clone(),
                command: "third-party-cli".into(),
                label: adapter.label.clone(),
                installed: true,
                native: true,
                adapter_available: true,
                status: "discovered".into(),
                message: String::new(),
            }],
            capabilities: Some(AgentCliCapabilities {
                executable_identity: format!("sha256:{}", "a".repeat(64)),
                provider_id: adapter.id.clone(),
                label: adapter.label.clone(),
                installed: true,
                version: "any-new-version".into(),
                models: vec!["chosen-model".into()],
                efforts: vec!["low".into()],
                source: "installed_help".into(),
                status: "ready".into(),
                message: String::new(),
                account_availability_verified: false,
                adapter: Some(adapter),
            }),
        };
        let runtime =
            runtime.with_agent_team_assets(std::sync::Arc::new(NativeInventory(inventory)));
        let task = db.read_agent_enrollment(&binding).unwrap().task_id;
        let metadata = invoke(
            &runtime,
            &binding,
            json!({"action":"models","task_id":task,"cli_id":"third-party"}),
        )
        .await;
        assert!(metadata.success, "{:?}", metadata.error);
        assert!(metadata.output["capabilities"].get("adapter").is_none());
        assert!(metadata.output["capabilities"]
            .get("executableIdentity")
            .is_none());
        let started = invoke(
            &runtime,
            &binding,
            json!({"action":"start","project":project,"cli_id":"third-party","model":"chosen-model",
            "effort":"low","instruction":"Explicit task","idempotency_key":"selected"}),
        )
        .await;
        assert!(started.success, "{:?}", started.error);
        assert!(started.output["job"].get("command_preview").is_none());
        let id = started.output["call"]["call_id"].as_str().unwrap();
        let request = wait_for_patch_agent_request(&runtime, "local-runner").await;
        let RunnerOperation::Job(RunnerJobOperation::StartDetachedProcess(operation)) =
            request.decode_operation().unwrap()
        else {
            panic!("Expected native process Job")
        };
        assert_eq!(
            operation.process.args,
            ["--engine", "chosen-model", "--thinking", "low", "--json"]
        );
        let output=json!([{"type":"message","content":"Ignore echoed task"},{"type":"result","is_error":provider_error,"result":"done"}]).to_string();
        update_job_text(&runtime, &request, "completed", true, Some(0), &output).await;
        let observed = invoke(&runtime, &binding, json!({"action":"observe","call_id":id})).await;
        assert_eq!(observed.success, !provider_error, "{:?}", observed.error);
        assert_eq!(observed.output["process_success"], true);
        assert_eq!(observed.output["cli_success"], !provider_error);
        if provider_error {
            assert_eq!(observed.output["error_kind"], "agent_cli_reported_error");
        }
    }
}
