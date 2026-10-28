use super::support::*;
use crate::runner_protocol::{
    RunnerCapabilities, RunnerPolicySummary, RunnerRegisterRequest, RunnerResultRequest,
};
use crate::tool_runtime::kernel::{
    HostFileImportTrust, ToolCallContext, ToolCallRequest, ToolTransport,
};
use crate::tool_runtime::ToolRuntime;
use serde_json::json;
use webcodex_core::mcp_gateway::McpGatewayProvider;
use webcodex_core::tool_connection::{
    ConnectionAction, ToolConnectionRequest, ToolConnectionResponse, TOOL_CONNECTION_KIND,
};

async fn connection_runtime() -> ToolRuntime {
    let runtime = test_runtime();
    runtime
        .runner_registry
        .register(crate::test_support::current_runner_registration(
            RunnerRegisterRequest {
                client_id: "connection-runner".into(),
                runner_instance_id: "private-runner-instance".into(),
                runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
                display_name: None,
                owner: Some("alice".into()),
                hostname: None,
                capabilities: RunnerCapabilities {
                    tool_connections: true,
                    ..Default::default()
                },
                agent_execution_resources: Vec::new(),
                host_context: None,
                policy: Some(RunnerPolicySummary {
                    mcp_gateway_providers: Some(Vec::new()),
                    mcp_gateway_revision: Some(0),
                    ..Default::default()
                }),
                process_started_at: None,
                build: None,
                job_concurrency_limit: None,
                job_inventory: None,
                coding_agent_providers: None,
                coding_agent_inventory: None,
            },
        ))
        .await
        .unwrap();
    runtime
}

#[tokio::test]
async fn tool_connection_kernel_roundtrip_projects_safe_result_and_refreshes_inventory() {
    let runtime = connection_runtime().await;
    for (action, revision) in [
        (ConnectionAction::Connect, 1),
        (ConnectionAction::Status, 2),
    ] {
        let mut arguments = json!({
            "client_id":"connection-runner", "action":action, "kind":"mcp", "id":"example"
        });
        if action == ConnectionAction::Connect {
            arguments["entry"] = json!("/not-executed/by-this-server-test/server.py");
            arguments["env"] = json!({"EXAMPLE_TOKEN":"input-secret-must-not-be-returned"});
        }
        let pending = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                let auth = auth_context(Some("alice"), true);
                runtime
                    .call_tool_with_context(
                        ToolCallRequest {
                            tool_name: "tool_connection".into(),
                            arguments,
                        },
                        ToolCallContext {
                            session_id: None,
                            auth: Some(&auth),
                            transport: ToolTransport::Api,
                            window: None,
                            record_oauth_scope_denials: true,
                            host_file_import_trust: HostFileImportTrust::Untrusted,
                        },
                    )
                    .await
            }
        });
        let request = wait_for_runner_request_for_instance(
            &runtime,
            "connection-runner",
            "private-runner-instance",
        )
        .await;
        assert_eq!(request.kind, TOOL_CONNECTION_KIND);
        assert!(request.command.is_empty());
        assert!(request.cwd.is_none());
        let decoded: ToolConnectionRequest =
            serde_json::from_str(request.content.as_deref().unwrap()).unwrap();
        assert_eq!(decoded.action, action);
        assert_eq!(decoded.id.as_deref(), Some("example"));
        if action == ConnectionAction::Connect {
            assert_eq!(
                decoded.env["EXAMPLE_TOKEN"],
                "input-secret-must-not-be-returned"
            );
        }

        let inventory = vec![McpGatewayProvider {
            provider_id: "example".into(),
            provider_instance_id: format!("private-provider-instance-{revision}"),
            name: "Example".into(),
        }];
        // Status can repair a newer complete snapshot after an earlier lost
        // management response; neither operation exposes that synchronization.
        let response: ToolConnectionResponse = serde_json::from_value(json!({
            "success":true, "changed":action == ConnectionAction::Connect,
            "message":"Connection is available",
            "connections":[{"id":"example","kind":"mcp","state":"connected","tool_count":1,"error_code":null}],
            "revision":revision, "inventory":inventory,
        }))
        .unwrap();
        runtime
            .runner_registry
            .complete(RunnerResultRequest {
                client_id: "connection-runner".into(),
                runner_instance_id: "private-runner-instance".into(),
                request_id: request.request_id,
                exit_code: Some(0),
                stdout: Some(serde_json::to_string(&response).unwrap()),
                stderr: None,
                duration_ms: Some(1),
                error: None,
            })
            .await
            .unwrap();
        let outcome = pending.await.unwrap();
        assert!(outcome.success, "{:?}", outcome.error_status);
        let result = outcome.result.expect("typed management result");
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.output["connections"][0]["id"], "example");
        assert_eq!(result.output["connections"][0]["tool_count"], 1);
        for field in [
            "revision",
            "inventory",
            "provider_instance_id",
            "env",
            "headers",
        ] {
            assert!(result.output.get(field).is_none(), "leaked {field}");
        }
        let visible = result.output.to_string();
        assert!(!visible.contains("private-runner-instance"));
        assert!(!visible.contains("private-provider-instance"));
        assert!(!visible.contains("input-secret-must-not-be-returned"));
        let policy = runtime
            .runner_registry
            .get_runner_view("connection-runner")
            .await
            .unwrap()
            .policy
            .unwrap();
        assert_eq!(policy.mcp_gateway_revision, Some(revision));
        assert_eq!(policy.mcp_gateway_providers, Some(inventory));
    }
}

#[tokio::test]
async fn tool_connection_status_for_unavailable_runner_is_an_error() {
    let runtime = test_runtime();
    let auth = auth_context(Some("alice"), true);
    let outcome = runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name: "tool_connection".into(),
                arguments: json!({"client_id":"missing-runner","action":"status"}),
            },
            ToolCallContext {
                session_id: None,
                auth: Some(&auth),
                transport: ToolTransport::Api,
                window: None,
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await;
    assert!(!outcome.success);
    let result = outcome
        .result
        .expect("unavailable Runner is a tool failure");
    assert!(!result.success);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("unavailable")));
}
