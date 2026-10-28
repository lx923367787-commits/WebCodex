use super::*;
use webcodex_core::runner_protocol::{
    RunnerConfigAction, RunnerConfigOperationRequest, RunnerPollRequest, RunnerRegisterRequest,
    ShellCommandExecutionState, RUNNER_CONFIG_REQUEST_KIND,
};

fn registration(instance: &str, runner_config_control: bool) -> RunnerRegisterRequest {
    current_runner_registration(RunnerRegisterRequest {
        agent_execution_resources: Vec::new(),
        client_id: "runner-config-control".to_string(),
        runner_instance_id: instance.to_string(),
        runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
        display_name: None,
        owner: Some("alice".to_string()),
        hostname: None,
        capabilities: RunnerCapabilities {
            runner_config_control,
            ..Default::default()
        },
        host_context: None,
        policy: None,
        process_started_at: None,
        build: None,
        job_concurrency_limit: None,
        job_inventory: None,
        coding_agent_providers: None,
        coding_agent_inventory: None,
    })
}

fn alice() -> RunnerAccess {
    auth_context(Some("alice"), false)
}

fn connection_status() -> webcodex_core::tool_connection::ToolConnectionRequest {
    webcodex_core::tool_connection::ToolConnectionRequest {
        action: webcodex_core::tool_connection::ConnectionAction::Status,
        kind: webcodex_core::tool_connection::ConnectionKind::Mcp,
        id: None,
        entry: None,
        args: Vec::new(),
        cwd: None,
        env: Default::default(),
        headers: Default::default(),
    }
}

#[tokio::test]
async fn tool_connection_enqueue_requires_own_capability_and_polls_exact_payload() {
    let registry = RunnerRegistry::default();
    registry
        .register(registration("instance-a", true))
        .await
        .unwrap();
    let auth = alice();
    assert!(registry
        .enqueue_tool_connection(
            "runner-config-control",
            "instance-a",
            connection_status(),
            Some(&auth),
            "test".into(),
        )
        .await
        .unwrap_err()
        .contains("capability_unavailable"));

    // Connection management has its own feature; generic config reload is not
    // a prerequisite and must not be selected by the shared dequeue fence.
    let mut registration = registration("instance-a", true);
    registration.capabilities.tool_connections = true;
    registry.register(registration).await.unwrap();
    {
        let mut inner = registry.inner.lock().await;
        let mut capabilities = RunnerCapabilities::default();
        capabilities.tool_connections = true;
        inner
            .runners
            .get_mut("runner-config-control")
            .unwrap()
            .runner_features = RunnerFeatureSet::try_from_registration(
            &crate::test_support::current_runner_capabilities(capabilities),
        )
        .unwrap();
    }
    let bob = auth_context(Some("bob"), false);
    assert!(registry
        .enqueue_tool_connection(
            "runner-config-control",
            "instance-a",
            connection_status(),
            Some(&bob),
            "test".into(),
        )
        .await
        .is_err());
    assert!(registry
        .enqueue_tool_connection(
            "runner-config-control",
            "old-instance",
            connection_status(),
            Some(&auth),
            "test".into(),
        )
        .await
        .unwrap_err()
        .contains("runner_replaced"));
    let (request_id, _receiver) = registry
        .enqueue_tool_connection(
            "runner-config-control",
            "instance-a",
            connection_status(),
            Some(&auth),
            "test".into(),
        )
        .await
        .unwrap();
    let request = registry
        .poll(RunnerPollRequest {
            client_id: "runner-config-control".into(),
            runner_instance_id: "instance-a".into(),
        })
        .await
        .unwrap()
        .expect("connection management reaches the exact Runner");
    assert_eq!(request.request_id, request_id);
    assert_eq!(
        request.kind,
        webcodex_core::tool_connection::TOOL_CONNECTION_KIND
    );
    assert!(request.command.is_empty());
    assert!(request.cwd.is_none());
    assert!(request.path.is_none());
    let parsed: webcodex_core::tool_connection::ToolConnectionRequest =
        serde_json::from_str(request.content.as_deref().unwrap()).unwrap();
    assert_eq!(parsed, connection_status());
}

#[tokio::test]
async fn tool_connection_dequeue_rejects_replaced_runner_and_lost_capability() {
    for replace_instance in [true, false] {
        let registry = RunnerRegistry::default();
        let mut registration = registration("instance-a", false);
        registration.capabilities.tool_connections = true;
        registry.register(registration).await.unwrap();
        let auth = alice();
        let (_request_id, receiver) = registry
            .enqueue_tool_connection(
                "runner-config-control",
                "instance-a",
                connection_status(),
                Some(&auth),
                "test".into(),
            )
            .await
            .unwrap();
        {
            // Isolate dequeue validation from registration's earlier draining.
            let mut inner = registry.inner.lock().await;
            let runner = inner.runners.get_mut("runner-config-control").unwrap();
            if replace_instance {
                runner.runner_instance_id = "instance-b".into();
            } else {
                runner.runner_features =
                    RunnerFeatureSet::try_from_registration(
                        &crate::test_support::current_runner_capabilities(
                            RunnerCapabilities::default(),
                        ),
                    )
                    .unwrap();
            }
        }
        assert!(registry
            .poll(RunnerPollRequest {
                client_id: "runner-config-control".into(),
                runner_instance_id: if replace_instance {
                    "instance-b"
                } else {
                    "instance-a"
                }
                .into(),
            })
            .await
            .unwrap()
            .is_none());
        let response = receiver.await.unwrap();
        assert!(!response.success);
        assert_eq!(response.request_dispatched, Some(false));
        assert_eq!(
            response.command_execution_state,
            Some(ShellCommandExecutionState::NotStarted)
        );
        let expected = if replace_instance {
            "runner_replaced"
        } else {
            "capability_unavailable"
        };
        assert!(response
            .error
            .as_deref()
            .is_some_and(|error| error.contains(expected)));
        let inner = registry.inner.lock().await;
        assert!(inner.pending_by_id.is_empty());
        assert!(inner
            .queues_by_runner
            .get("runner-config-control")
            .is_none_or(|queue| queue.is_empty()));
    }
}

#[tokio::test]
async fn runner_config_enqueue_requires_capability_exact_instance_and_closed_payload() {
    let registry = RunnerRegistry::default();
    registry
        .register(registration("instance-a", false))
        .await
        .unwrap();
    let auth = alice();
    let check = RunnerConfigOperationRequest {
        action: RunnerConfigAction::Check,
        expected_generation: None,
    };

    let unsupported = registry
        .enqueue_runner_config(
            "runner-config-control",
            "instance-a",
            check.clone(),
            Some(&auth),
            "test".to_string(),
        )
        .await
        .unwrap_err();
    assert!(unsupported.contains("capability_unavailable"));

    registry
        .register(registration("instance-a", true))
        .await
        .unwrap();
    let replaced = registry
        .enqueue_runner_config(
            "runner-config-control",
            "instance-other",
            check,
            Some(&auth),
            "test".to_string(),
        )
        .await
        .unwrap_err();
    assert!(replaced.contains("runner_replaced"));

    let reload = RunnerConfigOperationRequest {
        action: RunnerConfigAction::Reload,
        expected_generation: Some(1),
    };
    let (_request_id, _receiver) = registry
        .enqueue_runner_config(
            "runner-config-control",
            "instance-a",
            reload.clone(),
            Some(&auth),
            "test".to_string(),
        )
        .await
        .unwrap();
    let request = registry
        .poll(RunnerPollRequest {
            client_id: "runner-config-control".to_string(),
            runner_instance_id: "instance-a".to_string(),
        })
        .await
        .unwrap()
        .expect("exact Runner should receive config request");
    assert_eq!(request.kind, RUNNER_CONFIG_REQUEST_KIND);
    assert!(request.path.is_none());
    assert!(request.cwd.is_none());
    assert!(request.stdin.is_none());
    assert!(request.command.is_empty());
    let parsed: RunnerConfigOperationRequest =
        serde_json::from_str(request.content.as_deref().unwrap()).unwrap();
    assert_eq!(parsed, reload);
}

#[tokio::test]
async fn runner_config_dequeue_never_retargets_replacement_runner() {
    let registry = RunnerRegistry::default();
    registry
        .register(registration("instance-a", true))
        .await
        .unwrap();
    let auth = alice();
    let (_request_id, receiver) = registry
        .enqueue_runner_config(
            "runner-config-control",
            "instance-a",
            RunnerConfigOperationRequest {
                action: RunnerConfigAction::Reload,
                expected_generation: Some(1),
            },
            Some(&auth),
            "test".to_string(),
        )
        .await
        .unwrap();

    // Bypass normal registration draining to prove the dequeue fence itself
    // protects a stable client_id recycled by a later Runner process.
    {
        let mut inner = registry.inner.lock().await;
        inner
            .runners
            .get_mut("runner-config-control")
            .unwrap()
            .runner_instance_id = "instance-b".to_string();
    }
    let polled = registry
        .poll(RunnerPollRequest {
            client_id: "runner-config-control".to_string(),
            runner_instance_id: "instance-b".to_string(),
        })
        .await
        .unwrap();
    assert!(
        polled.is_none(),
        "replacement Runner must not receive config operation"
    );
    let response = receiver.await.unwrap();
    assert!(!response.success);
    assert_eq!(response.request_dispatched, Some(false));
    assert_eq!(
        response.command_execution_state,
        Some(ShellCommandExecutionState::NotStarted)
    );
    assert!(response
        .error
        .as_deref()
        .is_some_and(|error| error.contains("runner_replaced")));

    let inner = registry.inner.lock().await;
    assert!(inner.pending_by_id.is_empty());
    assert!(inner
        .queues_by_runner
        .get("runner-config-control")
        .is_none_or(|queue| queue.is_empty()));
}
