//! A model-facing sparse success must not erase the canonical Operation receipt.
use super::*;
use crate::runner_protocol::{
    RunnerResultPayload, RunnerResultRequest, ShellCommandExecutionState,
};
use webcodex_core::host_goal::HostGoalOperationStatus;

#[tokio::test]
async fn host_goals_sync_process_and_script_preserve_receipts_before_sparse_projection() {
    let mut fixture = Fixture::new().await;
    fixture.auth.scopes.push("job:run".into());
    let initial = fixture.start().await;
    let saved = fixture.save_plan(&initial, plan(), "sync-plan").await;
    success(&saved);
    let confirmed = fixture.confirm(&saved.output["goal"], "sync-confirm").await;
    success(&confirmed);
    let claimed = fixture.claim(&confirmed.output["goal"], "sync-claim").await;
    success(&claimed);
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    let goal_id = id(&claimed.output["goal"]);

    for (tool, mut arguments) in [
        (
            "run_process",
            json!({"executable":"fixture-command","args":[]}),
        ),
        (
            "run_script",
            json!({"language":"bash","script":"printf done","args":[]}),
        ),
    ] {
        let operation_id = format!("sync-{tool}");
        let mut binding = claimed.output["binding"].clone();
        binding["operation_id"] = json!(operation_id);
        binding["context_epoch"] = json!("chat-original");
        arguments["project"] = json!(fixture.project);
        arguments["timeout_secs"] = json!(10);
        arguments["sync_wait_secs"] = json!(10);
        arguments["goal_context"] = binding;
        let request = fixture.runtime.call_tool_with_context(
            ToolCallRequest {
                tool_name: tool.into(),
                arguments,
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(&fixture.auth),
                window: None,
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        );
        let complete = async {
            let work = crate::test_support::runner_project_files::next_business_request(
                &fixture.runtime,
                "host-goal-runner",
                "inst",
                fixture.temp.path(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            )
            .await;
            assert_eq!(work.kind, tool);
            assert!(
                work.job_id.is_none(),
                "exercise the synchronous receipt path"
            );
            fixture
                .runtime
                .runner_registry
                .complete(RunnerResultPayload {
                    result: RunnerResultRequest {
                        client_id: "host-goal-runner".into(),
                        runner_instance_id: "inst".into(),
                        request_id: work.request_id,
                        exit_code: Some(0),
                        stdout: Some("done\n".into()),
                        stderr: Some(String::new()),
                        duration_ms: Some(1),
                        error: None,
                    },
                    command_execution_state: Some(ShellCommandExecutionState::Completed),
                    mcp_gateway: None,
                    plugin_gateway: None,
                    coding_agent: None,
                })
                .await
                .unwrap();
        };
        let (outcome, ()) = tokio::join!(request, complete);
        assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
        let result = outcome.result.unwrap();
        success(&result);
        assert_eq!(result.output["stdout_tail"], "done\n");
        for omitted in [
            "execution_state",
            "exit_code",
            "job_id",
            "command_completed",
        ] {
            assert!(result.output.get(omitted).is_none(), "{tool}: {omitted}");
        }
        let receipt = fixture
            .db
            .get_host_goal_operation(&owner, &goal_id, &operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.status, HostGoalOperationStatus::Succeeded, "{tool}");
        assert!(receipt.job_id.is_none());
        let canonical: Value =
            serde_json::from_str(receipt.result_summary.as_ref().unwrap()).unwrap();
        assert_eq!(canonical["execution_state"], "completed");
        assert_eq!(canonical["exit_code"], 0);
        assert_eq!(canonical["command_completed"], true);
        assert_eq!(
            fixture
                .db
                .count_unresolved_host_goal_operations(&owner, &goal_id)
                .unwrap(),
            0
        );
    }

    let current = fixture.db.get_host_goal(&owner, &goal_id).unwrap().unwrap();
    let handoff = fixture
        .call_in_chat(ToolCall::GoalCheckpoint {
            goal_id,
            expected_revision: current.revision,
            idempotency_key: "after-sync-handoff".into(),
            patch: json!({"action":"handoff","reason":"Both synchronous operations have canonical completion receipts"}),
            context_epoch: Some("chat-original".into()),
        }, "chat-original", Some(&claimed.output["binding"]))
        .await;
    success(&handoff);
}
