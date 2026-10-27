use super::*;
use webcodex_core::coding_agent::{
    CodingAgentEvent, CodingAgentEventKind, CodingAgentObserveResult,
};

#[tokio::test]
async fn agent_task_quota_failure_hands_partial_work_to_replacement_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(crate::db::Database::open(&temp.path().join("takeover.db")).unwrap());
    let runtime = runtime_with_agent_task_db(db.clone());
    let auth = auth_context(Some("owner"), false);
    let project = register_coding_agent_task_runner(
        &runtime,
        "runner",
        "instance",
        "owner",
        "repo",
        temp.path(),
        CodingAgentRunInventory::default(),
    )
    .await;
    let agents: Vec<_> = ["first", "replacement"]
        .into_iter()
        .map(|name| {
            let result = runtime.communication.create_agent_identity(
                Some(&auth),
                name.into(),
                name.into(),
                None,
                vec![],
                format!("create-{name}"),
            );
            assert!(result.success);
            result.output["agent"]["agent_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let created = runtime.create_agent_task(
        Some(&auth),
        "Preserve work".into(),
        "Create result.txt containing exactly partial+remaining; verify its contents.".into(),
        Some(agents[0].clone()),
        None,
        None,
        Some(project.clone()),
        "task".into(),
    );
    assert!(created.success);
    let task_id = created.output["task"]["summary"]["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    let result_path = temp.path().join("result.txt");
    let mock = tokio::spawn({
        let runtime = runtime.clone();
        let result_path = result_path.clone();
        async move {
            let mut runs = std::collections::HashMap::new();
            let mut starts = 0;
            for _ in 0..4 {
                let request =
                    wait_for_runner_request_for_instance(&runtime, "runner", "instance").await;
                let payload = match request.coding_agent.unwrap() {
                    CodingAgentRequest::Start(start) => {
                        starts += 1;
                        let now = chrono::Utc::now().timestamp();
                        if starts == 1 {
                            std::fs::write(&result_path, "partial").unwrap();
                        } else {
                            assert!(
                                start.instruction.contains("saved: partial")
                                    && start.instruction.contains("quota_exhausted")
                            );
                            assert_eq!(std::fs::read_to_string(&result_path).unwrap(), "partial");
                            std::fs::write(&result_path, "partial+remaining").unwrap();
                        }
                        let run = CodingAgentRunSnapshot {
                            run_id: start.run_id.clone(),
                            intent_fingerprint: start.intent_fingerprint,
                            authority_fingerprint: start.authority_fingerprint,
                            runtime_project_id: start.runtime_project_id,
                            provider_id: start.provider_id,
                            provider_instance_id: start.provider_instance_id,
                            state: if starts == 1 {
                                CodingAgentRunState::Failed
                            } else {
                                CodingAgentRunState::Completed
                            },
                            execution_state: CodingAgentExecutionState::Completed,
                            observation_revision: 3,
                            created_at: now,
                            updated_at: now,
                            terminal: Some(CodingAgentTerminal {
                                stop_reason: (starts == 2).then(|| "end_turn".into()),
                                error_code: (starts == 1).then(|| "quota_exhausted".into()),
                                message: None,
                                completed_at: now,
                            }),
                        };
                        runs.insert(start.run_id, run.clone());
                        CodingAgentResponsePayload::Start { run }
                    }
                    CodingAgentRequest::Observe(observe) => CodingAgentResponsePayload::Observe {
                        observation: CodingAgentObserveResult {
                            run: runs[&observe.run_id].clone(),
                            events: vec![CodingAgentEvent {
                                sequence: 1,
                                kind: CodingAgentEventKind::FileChange,
                                text: Some("result.txt saved: partial".into()),
                                label: Some("edit-1".into()),
                                status: Some("completed".into()),
                                usage: None,
                            }],
                            first_retained_sequence: 1,
                            next_sequence: 1,
                            has_more: false,
                            history_lost: false,
                        },
                    },
                    _ => panic!("no cancellation or extra dispatch expected"),
                };
                runtime
                    .runner_registry
                    .complete(RunnerResultPayload {
                        result: RunnerResultRequest {
                            client_id: "runner".into(),
                            runner_instance_id: "instance".into(),
                            request_id: request.request_id,
                            exit_code: None,
                            stdout: None,
                            stderr: None,
                            duration_ms: None,
                            error: None,
                        },
                        command_execution_state: None,
                        mcp_gateway: None,
                        plugin_gateway: None,
                        coding_agent: Some(CodingAgentResponse::success(payload)),
                    })
                    .await
                    .unwrap();
            }
            assert_eq!(starts, 2);
        }
    });
    for (index, agent) in agents.iter().enumerate() {
        if index > 0 {
            assert!(
                runtime
                    .assign_agent_task(Some(&auth), task_id.clone(), agent.clone())
                    .success
            );
        }
        let attempt = runtime.start_agent_task_attempt(
            Some(&auth),
            task_id.clone(),
            agent.clone(),
            format!("attempt-{index}"),
        );
        assert!(attempt.success);
        let id = attempt.output["attempt"]["attempt_id"]
            .as_str()
            .unwrap()
            .to_string();
        let fence = attempt.output["attempt_fence"]
            .as_str()
            .unwrap()
            .to_string();
        if index == 1 {
            let unavailable = runtime
                .start_agent_task_coding_run(
                    Some(&auth),
                    project.clone(),
                    task_id.clone(),
                    id.clone(),
                    agent.clone(),
                    fence.clone(),
                    1,
                    "unavailable-provider".into(),
                    None,
                    Some(30),
                )
                .await;
            assert!(!unavailable.success);
            assert_eq!(std::fs::read_to_string(&result_path).unwrap(), "partial");
        }
        let started = runtime
            .start_agent_task_coding_run(
                Some(&auth),
                project.clone(),
                task_id.clone(),
                id.clone(),
                agent.clone(),
                fence,
                1,
                "codex".into(),
                None,
                Some(30),
            )
            .await;
        assert!(started.success, "{:?}", started.output);
        let reconciled = runtime
            .reconcile_agent_task_coding_run(Some(&auth), task_id.clone(), id.clone())
            .await;
        assert!(reconciled.success, "{:?}", reconciled.output);
        assert_eq!(
            reconciled.output["task_state"],
            if index == 0 { "failed" } else { "succeeded" }
        );
        assert!(reconciled.output["handoff_brief"]
            .as_str()
            .unwrap()
            .contains("original_instruction"));
        if index == 0 {
            let offline = runtime_with_agent_task_db(db.clone());
            let recovered = offline
                .reconcile_agent_task_coding_run(Some(&auth), task_id.clone(), id)
                .await;
            assert!(recovered.success, "{:?}", recovered.output);
            assert!(recovered.output["handoff_brief"]
                .as_str()
                .unwrap()
                .contains("saved: partial"));
        }
    }
    mock.await.unwrap();
    assert_eq!(
        std::fs::read_to_string(result_path).unwrap(),
        "partial+remaining"
    );
}
