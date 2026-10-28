use super::*;

fn attempt(provider: &str) -> TeamCliAttemptRecord {
    TeamCliAttemptRecord {
        inputs: vec![],
        attempt_id: "wc_team_cli_attempt_test".into(),
        project_id: "agent:runner:repo".into(),
        root_task_id: "wc_agent_task_root".into(),
        source_task_id: "wc_agent_task_child".into(),
        call_index: 0,
        provider_id: provider.into(),
        model: "discovered-model".into(),
        effort: Some("high".into()),
        instruction: "Keep $(literal), `quotes` and 雪 unchanged.\nDo the assigned work.".into(),
        fallback_thinking_profile_id: "thinking_discovered".into(),
        timeout_secs: Some(900),
        dispatch_state: TeamCliDispatchState::Reserved,
        job_id: None,
        execution_binding: None,
        execution_state: None,
        error_code: None,
        result_summary: None,
        observation_revision: 0,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    }
}

#[test]
fn team_cli_native_argv_keeps_instructions_in_provider_specific_stdin() {
    let work = attempt("workbuddy");
    let (executable, args, stdin) = cli_process(&work).unwrap();
    assert_eq!(executable, "codebuddy");
    assert_eq!(stdin, work.instruction);
    assert!(args.windows(2).any(|a| a == ["--input-format", "text"]));
    assert!(args.iter().all(|a| !a.contains("$(literal)")));
    assert!(!args
        .iter()
        .any(|a| a.contains("skip-permission") || a.contains("bypass")));
    let (executable, args, stdin) = cli_process(&attempt("antigravity")).unwrap();
    assert_eq!(executable, "agy");
    assert!(args
        .windows(2)
        .any(|a| a == ["--input-format", "stream-json"]));
    let envelope: Value = serde_json::from_str(&stdin).unwrap();
    assert_eq!(envelope["event"], "user");
    assert_eq!(envelope["message"]["content"], work.instruction);
}

#[test]
fn team_cli_default_omits_reasoning_override_and_rejects_invalid_selections() {
    assert!(cli_process(&attempt("shell")).is_err());
    let mut request = attempt("workbuddy");
    request.effort = None;
    let (_, args, _) = cli_process(&request).unwrap();
    assert!(!args.iter().any(|arg| arg == "--effort"));
    request.provider_id = "codex".into();
    let (_, args, _) = cli_process(&request).unwrap();
    assert!(!args
        .iter()
        .any(|arg| arg.contains("model_reasoning_effort")));
    request.provider_id = "antigravity".into();
    let (_, args, stdin) = cli_process(&request).unwrap();
    assert_eq!(&args[..2], &["--model", "discovered-model"]);
    assert!(!args.iter().any(|arg| arg == "--effort"));
    assert_eq!(
        serde_json::from_str::<Value>(&stdin).unwrap()["message"]["content"],
        request.instruction
    );
    request.effort = Some(" ".into());
    assert!(cli_process(&request).is_err());
}

#[test]
fn team_cli_failure_requires_a_provider_result_error_envelope() {
    assert_eq!(
        structured_failure_code(
            "workbuddy",
            r#"{"type":"result","is_error":true,"error":{"code":"insufficient_quota"}}"#
        ),
        Some("insufficient_quota".into())
    );
    assert_eq!(
        structured_failure_code(
            "antigravity",
            r#"{"event":"result","result":{"status":"ERROR","error_code":"network_unavailable"}}"#
        ),
        Some("network_unavailable".into())
    );
    for prose in [
        "quota_exhausted",
        r#"{"error_code":"quota_exhausted"}"#,
        r#"{"type":"result","is_error":false,"result":"quota_exhausted"}"#,
        r#"{"event":"result","result":{"status":"ERROR","error":"permission denied"}}"#,
    ] {
        assert_eq!(structured_failure_code("workbuddy", prose), None);
        assert_eq!(structured_failure_code("antigravity", prose), None);
    }
}

#[test]
fn team_request_parsers_reject_a_frontend_recovery_exemption() {
    assert!(parse::<CliStart>(json!({"project":"agent:r:p","root_task_id":"root","source_task_id":"source","call_index":0,"idempotency_key":"same","fallback_thinking_profile_id":"thinking_high","fallback":true})).is_err());
    assert!(parse::<RootRequest>(
        json!({"project":"agent:r:p","root_task_id":"root","bypass_limit":true})
    )
    .is_err());
}

#[test]
fn plan_read_defaults_cli_pagination_without_changing_cancel_parser() {
    let minimal = json!({"project":"agent:r:p","root_task_id":"root"});
    let request = parse::<PlanReadRequest>(minimal.clone()).unwrap();
    assert_eq!((request.attempts_offset, request.attempts_limit), (0, 20));
    let explicit = json!({"project":"agent:r:p","root_task_id":"root","attempts_offset":20,"attempts_limit":7});
    let request = parse::<PlanReadRequest>(explicit.clone()).unwrap();
    assert_eq!((request.attempts_offset, request.attempts_limit), (20, 7));
    assert!(parse::<RootRequest>(explicit).is_err());
    assert!(parse::<RootRequest>(minimal).is_ok());
}

struct TerminalTeamFixture {
    _temporary: tempfile::TempDir,
    runtime: ToolRuntime,
    db: std::sync::Arc<crate::Database>,
    principal: webcodex_store::CommunicationPrincipal,
    attempt: TeamCliAttemptRecord,
}

#[tokio::test]
async fn team_plan_read_exposes_later_attempts_with_whole_plan_completion_counts() {
    let f = TerminalTeamFixture::new().await;
    let project = f.attempt.project_id.clone();
    let root =
        f.db.create_agent_task(
            &f.principal,
            webcodex_store::NewAgentTask {
                title: "Paged task".into(),
                instruction: "Perform the frozen work".into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(project.clone()),
                idempotency_key: "paged-root".into(),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let calls = (0..30).map(|index| json!({"provider_id":"workbuddy","model":"selected-model","effort":"high","instruction":format!("Independent work {index}")})).collect::<Vec<_>>();
    f.db.register_team_plan(
        &f.principal,
        &project,
        &root,
        0,
        serde_json::from_value(
            json!({"manager":{"task_id":root,"cli_calls":calls},"subagents":[]}),
        )
        .unwrap(),
        "paged-plan",
    )
    .unwrap();
    let mut ids = Vec::new();
    for index in 0..25 {
        ids.push(
            f.db.reserve_team_cli_attempt(
                &f.principal,
                &project,
                &root,
                &root,
                index,
                &format!("paged-{index}"),
                "original-profile",
                Some(900),
            )
            .unwrap()
            .attempt
            .attempt_id,
        );
    }
    let result = f
        .runtime
        .dispatch_team_tool(
            None,
            "team_plan_read",
            json!({
                "project":project,"root_task_id":root,"attempts_offset":20,"attempts_limit":20
            }),
        )
        .await;
    assert!(result.success, "{:?}", result.error);
    let attempts = result.output["attempts"]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 5);
    assert_eq!(attempts[0]["attempt_id"], ids[20]);
    assert_eq!(attempts[4]["attempt_id"], ids[24]);
    assert_eq!(result.output["attempts"]["total_count"], 25);
    assert_eq!(result.output["completion"]["consumed_cli_calls"], 25);
    assert_eq!(result.output["completion"]["confirmed_cli_calls"], 0);
    assert_eq!(result.output["completion"]["success_gate"], "not_required");
    let invalid = f
        .runtime
        .dispatch_team_tool(
            None,
            "team_plan_read",
            json!({
                "project":project,"root_task_id":root,"attempts_limit":21
            }),
        )
        .await;
    assert!(!invalid.success);
    assert!(invalid.output.get("attempts").is_none());
}

impl TerminalTeamFixture {
    /// Real registry/DB transitions with an in-memory fake Runner. No process,
    /// network endpoint, CLI model or host chat is started by this fixture.
    async fn new() -> Self {
        use crate::runner_http::{ShellJobStartMetadata, StructuredJobExecution};
        use crate::runner_protocol::{
            RunnerCapabilities, RunnerPollRequest, RunnerRegisterRequest, ShellProcessArgv,
        };
        use webcodex_store::NewAgentTask;
        let temporary = tempfile::tempdir().unwrap();
        let db = std::sync::Arc::new(
            crate::Database::open(&temporary.path().join("replay.db")).unwrap(),
        );
        let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
        let principal = crate::tool_runtime::communication::communication_principal(None).unwrap();
        let client = "replay-runner";
        let project = "agent:replay-runner:repo";
        runtime
            .runner_registry
            .register(RunnerRegisterRequest {
                agent_execution_resources: Vec::new(),
                client_id: client.into(),
                runner_instance_id: "instance".into(),
                runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
                process_started_at: None,
                build: None,
                job_concurrency_limit: None,
                job_inventory: None,
                coding_agent_providers: None,
                coding_agent_inventory: None,
                display_name: None,
                owner: None,
                hostname: None,
                host_context: None,
                policy: None,
                capabilities: crate::test_support::current_runner_capabilities(
                    RunnerCapabilities {
                        jobs: true,
                        async_jobs: true,
                        async_shell_jobs: true,
                        ..Default::default()
                    },
                ),
            })
            .await
            .unwrap();
        crate::test_support::apply_project_inventory_snapshot(
            &runtime.runner_registry,
            client,
            "instance",
            vec![serde_json::from_value(
                json!({"id":"repo","path":"/test/project","updated_at":1}),
            )
            .unwrap()],
        )
        .await;
        let task = |key: &str| {
            db.create_agent_task(
                &principal,
                NewAgentTask {
                    title: key.into(),
                    instruction: "Finish the frozen assignment".into(),
                    assignee_agent_id: None,
                    source_conversation_id: None,
                    source_message_id: None,
                    referenced_project_id: Some(project.into()),
                    idempotency_key: key.into(),
                },
            )
            .unwrap()
            .task
            .summary
            .task_id
        };
        let root = task("root");
        let source = task("source");
        db.register_team_plan(&principal, project, &root, 0, serde_json::from_value(json!({
            "manager":{"task_id":root,"cli_calls":[]},
            "subagents":[{"task_id":source,"cli_calls":[{"provider_id":"workbuddy","model":"selected-model","effort":"high","instruction":"Independently verify the delegated subtask"}]}]
        })).unwrap(), "plan").unwrap();
        let reserved = db
            .reserve_team_cli_attempt(
                &principal,
                project,
                &root,
                &source,
                0,
                "original-key",
                "original-profile",
                Some(900),
            )
            .unwrap()
            .attempt;
        db.claim_team_cli_dispatch(&principal, &reserved.attempt_id)
            .unwrap();
        let job = runtime.runner_registry.start_job_with_metadata(
            serde_json::from_value(json!({"op":"start","client_id":client,"cwd":"/test/project","command":"","timeout_secs":900})).unwrap(),
            "test-coordinator".into(), ShellJobStartMetadata {
                project_id: Some(project.into()), project_cwd: Some(".".into()), shell: Some("direct_argv".into()),
                structured_execution: Some(StructuredJobExecution::Process(ShellProcessArgv {
                    executable: "codebuddy".into(), args: vec!["--print".into()],
                })), stdin: Some("frozen task".into()), ..Default::default()
            }).await.unwrap();
        let request = runtime
            .runner_registry
            .poll(RunnerPollRequest {
                client_id: client.into(),
                runner_instance_id: "instance".into(),
            })
            .await
            .unwrap()
            .unwrap();
        for (sequence, status, finished) in [(1, "running", false), (2, "failed", true)] {
            runtime.runner_registry.update_job(serde_json::from_value(json!({
                "client_id":client,"agent_instance_id":"instance","job_id":job.job_id,"request_id":request.request_id,
                "update_seq":sequence,"status":status,"finished":finished,
                "exit_code":if finished {json!(1)}else{Value::Null},
                "duration_ms":if finished {json!(25)}else{Value::Null},
                "error":if finished {json!("Provider unavailable")}else{Value::Null},
                "command_execution_state":if finished {json!("completed")}else{Value::Null}
            })).unwrap()).await.unwrap();
        }
        let bound = db
            .bind_team_cli_job(&principal, &reserved.attempt_id, &job.job_id)
            .unwrap();
        let attempt = db
            .reconcile_team_cli_attempt(
                &principal,
                &bound.attempt_id,
                bound.observation_revision,
                TeamCliObservation {
                    job_id: Some(job.job_id),
                    dispatch_state: TeamCliDispatchState::Terminal,
                    execution_state: Some(ShellCommandExecutionState::Completed),
                    error_code: Some("quota_exhausted".into()),
                    result_summary: None,
                },
            )
            .unwrap();
        Self {
            _temporary: temporary,
            runtime,
            db,
            principal,
            attempt,
        }
    }
}

#[tokio::test]
async fn team_cli_terminal_replay_repairs_missing_recovery_once_without_a_new_cli_job() {
    let fixture = TerminalTeamFixture::new().await;
    assert!(fixture
        .db
        .list_cli_fallbacks(&fixture.principal, &fixture.attempt.root_task_id)
        .unwrap()
        .is_empty());
    let first = fixture
        .runtime
        .team_replay_receipt(fixture.attempt.clone(), None)
        .await;
    assert_eq!(first.output["dispatched"], false);
    assert_eq!(
        first.output["recovery_enqueue"]["success"], true,
        "{:?}",
        first.output
    );
    let recovery = first.output["recovery_enqueue"]["output"]["fallback_id"].clone();
    assert!(recovery.is_string());
    let second = fixture
        .runtime
        .team_replay_receipt(fixture.attempt.clone(), None)
        .await;
    assert_eq!(
        second.output["recovery_enqueue"]["output"]["fallback_id"],
        recovery
    );
    assert_eq!(
        fixture
            .db
            .list_cli_fallbacks(&fixture.principal, &fixture.attempt.root_task_id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .runtime
            .runner_registry
            .list_jobs(Some(100))
            .await
            .len(),
        1
    );
    assert_eq!(
        fixture
            .db
            .read_team_cli_attempt(&fixture.principal, &fixture.attempt.attempt_id)
            .unwrap(),
        fixture.attempt
    );
}

#[tokio::test]
async fn team_cli_terminal_replay_exposes_recovery_failure_without_redispatch() {
    let fixture = TerminalTeamFixture::new().await;
    fixture
        .runtime
        .runner_registry
        .remove_job_record(fixture.attempt.job_id.as_ref().unwrap())
        .await;
    let receipt = fixture
        .runtime
        .team_replay_receipt(fixture.attempt.clone(), None)
        .await;
    assert!(!receipt.success);
    assert_eq!(receipt.output["recovery_enqueue"]["success"], false);
    assert_eq!(
        receipt.output["recovery_enqueue"]["error"],
        "cli_fallback_job_unavailable"
    );
    assert_eq!(receipt.output["dispatched"], false);
    assert!(fixture
        .runtime
        .runner_registry
        .list_jobs(Some(100))
        .await
        .is_empty());
    assert!(fixture
        .db
        .list_cli_fallbacks(&fixture.principal, &fixture.attempt.root_task_id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn team_cli_watcher_repairs_an_already_terminal_attempt_without_restarting_execution() {
    let fixture = TerminalTeamFixture::new().await;
    fixture
        .runtime
        .watch_team_cli(fixture.attempt.clone(), None)
        .await;
    assert_eq!(
        fixture
            .db
            .list_cli_fallbacks(&fixture.principal, &fixture.attempt.root_task_id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .runtime
            .runner_registry
            .list_jobs(Some(100))
            .await
            .len(),
        1
    );
}
