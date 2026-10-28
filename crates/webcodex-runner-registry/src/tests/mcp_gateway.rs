use super::*;
use crate::mcp_gateway::{
    McpGatewayDispatchState, McpGatewayProvider, McpGatewayRequest, McpGatewayResponse,
    McpGatewayResponsePayload,
};
use crate::runner_protocol::{
    RunnerPolicySummary, RunnerPollRequest, RunnerRegisterRequest, RunnerResultPayload,
    RunnerResultRequest,
};

fn bridge_provider(provider_instance_id: &str) -> McpGatewayProvider {
    McpGatewayProvider {
        provider_id: "provider".to_string(),
        provider_instance_id: provider_instance_id.to_string(),
        name: "Provider".to_string(),
    }
}

async fn register_bridge_runner(registry: &RunnerRegistry) {
    registry
        .register(RunnerRegisterRequest {
            agent_execution_resources: Vec::new(),
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "bridge-instance".to_string(),
            runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
            display_name: None,
            owner: Some("alice".to_string()),
            hostname: None,
            capabilities: crate::test_support::current_runner_capabilities(RunnerCapabilities {
                tool_connections: true,
                ..Default::default()
            }),
            host_context: None,
            policy: Some(RunnerPolicySummary {
                mcp_gateway_providers: Some(vec![bridge_provider("provider-instance")]),
                mcp_gateway_revision: Some(0),
                ..Default::default()
            }),
            process_started_at: None,
            build: None,
            job_concurrency_limit: None,
            job_inventory: None,
            coding_agent_providers: None,
            coding_agent_inventory: None,
        })
        .await
        .unwrap();
}

fn list_request(provider_instance_id: &str) -> McpGatewayRequest {
    McpGatewayRequest::ToolsList {
        provider_id: "provider".to_string(),
        provider_instance_id: provider_instance_id.to_string(),
    }
}

fn bridge_registration(
    client_id: &str,
    runner_instance_id: &str,
    providers: Option<Vec<McpGatewayProvider>>,
) -> RunnerRegisterRequest {
    current_runner_registration(RunnerRegisterRequest {
        agent_execution_resources: Vec::new(),
        client_id: client_id.to_string(),
        runner_instance_id: runner_instance_id.to_string(),
        runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
        display_name: None,
        owner: Some("alice".to_string()),
        hostname: None,
        capabilities: RunnerCapabilities::default(),
        host_context: None,
        policy: providers.map(|providers| RunnerPolicySummary {
            mcp_gateway_providers: Some(providers),
            ..Default::default()
        }),
        process_started_at: None,
        build: None,
        job_concurrency_limit: None,
        job_inventory: None,
        coding_agent_providers: None,
        coding_agent_inventory: None,
    })
}

fn managed_bridge_registration(
    client_id: &str,
    runner_instance_id: &str,
    providers: Option<Vec<McpGatewayProvider>>,
    revision: Option<u64>,
) -> RunnerRegisterRequest {
    let mut registration = bridge_registration(client_id, runner_instance_id, providers);
    registration.capabilities.tool_connections = true;
    if let Some(policy) = registration.policy.as_mut() {
        policy.mcp_gateway_revision = revision;
    }
    registration
}

async fn current_bridge_inventory(registry: &RunnerRegistry) -> Vec<McpGatewayProvider> {
    registry
        .get_runner_view("bridge-runner")
        .await
        .unwrap()
        .policy
        .unwrap()
        .mcp_gateway_providers
        .unwrap()
}

#[tokio::test]
async fn bridge_inventory_replacement_adds_removes_and_preserves_runner_state() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    registry.set_last_seen_for_test("bridge-runner", 123).await;
    let before = registry.get_runner_view("bridge-runner").await.unwrap();
    let inventory = vec![
        bridge_provider("replacement-instance"),
        McpGatewayProvider {
            provider_id: "added-provider".into(),
            provider_instance_id: "added-instance".into(),
            name: "Added provider".into(),
        },
    ];
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            1,
            inventory.clone(),
        )
        .await
        .unwrap());
    assert_eq!(current_bridge_inventory(&registry).await, inventory);
    let after = registry.get_runner_view("bridge-runner").await.unwrap();
    assert_eq!(after.last_seen, before.last_seen);
    assert_eq!(after.registered_at, before.registered_at);
    assert_eq!(after.runner_instance_id, before.runner_instance_id);
    assert_eq!(after.owner, before.owner);
    assert_eq!(after.capabilities, before.capabilities);
    assert_eq!(after.transport, before.transport);

    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            2,
            Vec::new(),
        )
        .await
        .unwrap());
    assert!(current_bridge_inventory(&registry).await.is_empty());
}

#[tokio::test]
async fn bridge_inventory_replacement_rejects_wrong_owner_instance_and_invalid_inventory() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let bob = auth_context(Some("bob"), false);
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&bob),
            1,
            Vec::new(),
        )
        .await
        .is_err());
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "stale-instance",
            Some(&alice),
            1,
            Vec::new(),
        )
        .await
        .unwrap_err()
        .contains("stale Runner"));
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            1,
            vec![bridge_provider("one"), bridge_provider("two")],
        )
        .await
        .unwrap_err()
        .contains("invalid MCP gateway provider inventory"));
    assert_eq!(
        current_bridge_inventory(&registry).await,
        vec![bridge_provider("provider-instance")]
    );
    assert_eq!(
        registry
            .get_runner_view("bridge-runner")
            .await
            .unwrap()
            .policy
            .unwrap()
            .mcp_gateway_revision,
        Some(0)
    );
}

#[tokio::test]
async fn bridge_inventory_replacement_uses_runner_revision_not_response_order() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let inventory = vec![
        bridge_provider("new-instance"),
        McpGatewayProvider {
            provider_id: "second".into(),
            provider_instance_id: "second-instance".into(),
            name: "Second".into(),
        },
    ];
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            3,
            inventory.clone(),
        )
        .await
        .unwrap());
    assert!(!registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            2,
            Vec::new(),
        )
        .await
        .unwrap());
    let mut reordered = inventory.clone();
    reordered.reverse();
    assert!(!registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            3,
            reordered,
        )
        .await
        .unwrap());
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            3,
            Vec::new(),
        )
        .await
        .unwrap_err()
        .contains("revision conflicts"));
    assert_eq!(current_bridge_inventory(&registry).await, inventory);
}

#[tokio::test]
async fn bridge_inventory_revision_survives_reconnect_and_resets_for_new_process() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let inventory = vec![bridge_provider("new-provider-instance")];
    registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            7,
            inventory.clone(),
        )
        .await
        .unwrap();
    registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(inventory.clone()),
            Some(7),
        ))
        .await
        .unwrap();
    assert!(!registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            6,
            Vec::new(),
        )
        .await
        .unwrap());
    assert!(registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(Vec::new()),
            None,
        ))
        .await
        .unwrap_err()
        .contains("cannot change MCP gateway provider inventory"));
    assert_eq!(current_bridge_inventory(&registry).await, inventory);

    registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "next-process",
            Some(Vec::new()),
            Some(0),
        ))
        .await
        .unwrap();
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            8,
            inventory.clone(),
        )
        .await
        .is_err());
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "next-process",
            Some(&alice),
            1,
            inventory.clone(),
        )
        .await
        .unwrap());
    assert_eq!(current_bridge_inventory(&registry).await, inventory);
}

#[tokio::test]
async fn bridge_inventory_replacement_can_add_first_provider_without_policy() {
    let registry = RunnerRegistry::default();
    registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "bridge-instance",
            None,
            None,
        ))
        .await
        .unwrap();
    let alice = auth_context(Some("alice"), false);
    assert!(!registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            0,
            Vec::new(),
        )
        .await
        .unwrap());
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            1,
            vec![bridge_provider("first-instance")],
        )
        .await
        .unwrap());
    assert_eq!(
        current_bridge_inventory(&registry).await,
        vec![bridge_provider("first-instance")]
    );
}

#[tokio::test]
async fn bridge_registration_inventory_is_bounded_and_exact() {
    let registry = RunnerRegistry::default();
    registry
        .register(bridge_registration(
            "valid-bridge-runner",
            "valid-instance",
            Some(vec![bridge_provider("provider-instance")]),
        ))
        .await
        .unwrap();
    let view = registry
        .get_runner_view("valid-bridge-runner")
        .await
        .unwrap();
    assert_eq!(
        view.policy
            .as_ref()
            .and_then(|policy| policy.mcp_gateway_providers.as_ref())
            .unwrap(),
        &vec![bridge_provider("provider-instance")]
    );

    for (name, providers) in [
        (
            "duplicate-id",
            vec![
                bridge_provider("provider-instance-a"),
                McpGatewayProvider {
                    provider_id: "provider".to_string(),
                    provider_instance_id: "provider-instance-b".to_string(),
                    name: "Provider B".to_string(),
                },
            ],
        ),
        (
            "duplicate-instance",
            vec![
                bridge_provider("provider-instance"),
                McpGatewayProvider {
                    provider_id: "provider-b".to_string(),
                    provider_instance_id: "provider-instance".to_string(),
                    name: "Provider B".to_string(),
                },
            ],
        ),
    ] {
        assert!(registry
            .register(bridge_registration(
                name,
                &format!("{name}-instance"),
                Some(providers),
            ))
            .await
            .unwrap_err()
            .contains("invalid MCP gateway provider inventory"));
    }

    let excessive = (0..=crate::mcp_gateway::MCP_GATEWAY_MAX_PROVIDERS)
        .map(|index| McpGatewayProvider {
            provider_id: format!("provider-{index}"),
            provider_instance_id: format!("instance-{index}"),
            name: format!("Provider {index}"),
        })
        .collect();
    assert!(registry
        .register(bridge_registration(
            "excessive",
            "excessive-instance",
            Some(excessive),
        ))
        .await
        .is_err());
    assert!(registry
        .register(bridge_registration(
            "malformed",
            "malformed-instance",
            Some(vec![McpGatewayProvider {
                provider_id: "Bad Provider".to_string(),
                provider_instance_id: "instance".to_string(),
                name: "Bad".to_string(),
            }]),
        ))
        .await
        .is_err());
    registry
        .register(bridge_registration(
            "no-gateway-inventory",
            "no-gateway-inventory-instance",
            None,
        ))
        .await
        .expect(
            "Runner without gateway inventory remains valid but cannot receive gateway requests",
        );
}

#[tokio::test]
async fn bridge_registration_recovers_lost_management_response_without_revision_rollback() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let replacement = vec![bridge_provider("after-lost-response")];
    registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(replacement.clone()),
            Some(4),
        ))
        .await
        .expect("reconnect recovers the current Runner's complete inventory");
    assert_eq!(current_bridge_inventory(&registry).await, replacement);

    registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(vec![bridge_provider("obsolete-provider")]),
            Some(2),
        ))
        .await
        .expect("a late registration preserves the latest inventory while reconnecting");
    assert_eq!(current_bridge_inventory(&registry).await, replacement);
    assert_eq!(
        registry
            .get_runner_view("bridge-runner")
            .await
            .unwrap()
            .policy
            .unwrap()
            .mcp_gateway_revision,
        Some(4)
    );
    assert!(registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(Vec::new()),
            Some(4),
        ))
        .await
        .unwrap_err()
        .contains("revision conflicts"));
    registry
        .register(managed_bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(replacement.clone()),
            Some(4),
        ))
        .await
        .expect("an identical revision is idempotent");
    assert_eq!(current_bridge_inventory(&registry).await, replacement);
}

#[tokio::test]
async fn bridge_revision_registration_requires_capability_and_complete_snapshot() {
    let registry = RunnerRegistry::default();
    let mut registration =
        bridge_registration("bridge-runner", "bridge-instance", Some(Vec::new()));
    registration.policy.as_mut().unwrap().mcp_gateway_revision = Some(1);
    assert!(registry
        .register(registration)
        .await
        .unwrap_err()
        .contains("requires tool_connections capability"));
    let mut registration = managed_bridge_registration(
        "bridge-runner",
        "bridge-instance",
        Some(Vec::new()),
        Some(1),
    );
    registration.policy.as_mut().unwrap().mcp_gateway_providers = None;
    assert!(registry
        .register(registration)
        .await
        .unwrap_err()
        .contains("requires a complete provider inventory"));
    assert!(registry.get_runner_view("bridge-runner").await.is_none());
}

#[tokio::test]
async fn legacy_bridge_registration_and_management_cannot_change_inventory() {
    let registry = RunnerRegistry::default();
    registry
        .register(bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(vec![bridge_provider("legacy-instance")]),
        ))
        .await
        .unwrap();
    assert!(registry
        .register(bridge_registration(
            "bridge-runner",
            "bridge-instance",
            Some(Vec::new()),
        ))
        .await
        .unwrap_err()
        .contains("cannot change MCP gateway provider inventory"));
    let alice = auth_context(Some("alice"), false);
    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            1,
            Vec::new(),
        )
        .await
        .unwrap_err()
        .contains("capability_unavailable"));
    assert_eq!(
        current_bridge_inventory(&registry).await,
        vec![bridge_provider("legacy-instance")]
    );
}

#[tokio::test]
async fn bridge_enqueue_rechecks_owner_and_exact_runner_instance() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;

    let bob = auth_context(Some("bob"), false);
    assert!(registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "bridge-instance",
            list_request("provider-instance"),
            Some(&bob),
            "bob".to_string(),
        )
        .await
        .unwrap_err()
        .contains("unavailable"));

    let alice = auth_context(Some("alice"), false);
    assert!(registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "stale-instance",
            list_request("provider-instance"),
            Some(&alice),
            "alice".to_string(),
        )
        .await
        .unwrap_err()
        .contains("stale Runner"));

    assert!(registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "bridge-instance",
            list_request("stale-provider-instance"),
            Some(&alice),
            "alice".to_string(),
        )
        .await
        .unwrap_err()
        .contains("stale provider"));

    let inner = registry.inner.lock().await;
    assert!(inner.pending_by_id.is_empty());
    assert!(inner.mcp_gateway_waiters.is_empty());
}

#[tokio::test]
async fn bridge_dequeue_rechecks_exact_runner_instance_after_replacement() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let (_request_id, receiver) = registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "bridge-instance",
            list_request("provider-instance"),
            Some(&alice),
            "test".to_string(),
        )
        .await
        .unwrap();

    // Simulate the narrow invariant violation between admission and dequeue.
    // Normal replacement registration already drains synchronous requests, but
    // dequeue itself must carry the exact process fence rather than relying on
    // that separate lifecycle path forever.
    {
        let mut inner = registry.inner.lock().await;
        inner
            .runners
            .get_mut("bridge-runner")
            .unwrap()
            .runner_instance_id = "replacement-instance".to_string();
    }
    let polled = registry
        .poll(RunnerPollRequest {
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "replacement-instance".to_string(),
        })
        .await
        .unwrap();
    assert!(
        polled.is_none(),
        "replacement Runner must not receive stale bridge work"
    );
    let response = receiver.await.unwrap();
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(response.error.as_ref().unwrap().code, "stale_runner");
}

#[tokio::test]
async fn bridge_dequeue_rejects_missing_exact_target_fence() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let (request_id, mut receiver) = registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "bridge-instance",
            list_request("provider-instance"),
            Some(&alice),
            "test".to_string(),
        )
        .await
        .unwrap();

    // Dequeue must fail closed even if admission's exact target fence is lost.
    {
        let mut inner = registry.inner.lock().await;
        let pending = inner.pending_by_id.get_mut(&request_id).unwrap();
        pending.expected_mcp_gateway_runner_instance_id = None;
        pending.expected_mcp_gateway_provider_id = None;
        pending.expected_mcp_gateway_provider_instance_id = None;
    }
    let polled = registry
        .poll(RunnerPollRequest {
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "bridge-instance".to_string(),
        })
        .await
        .unwrap();
    assert!(
        polled.is_none(),
        "unfenced bridge work must not be dispatched"
    );
    let response = receiver.try_recv().unwrap();
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(response.error.as_ref().unwrap().code, "stale_provider");
    let inner = registry.inner.lock().await;
    assert!(!inner.pending_by_id.contains_key(&request_id));
    assert!(!inner.mcp_gateway_waiters.contains_key(&request_id));
}

#[tokio::test]
async fn bridge_dequeue_rechecks_exact_provider_instance_after_inventory_change() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let (_request_id, receiver) = registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "bridge-instance",
            list_request("provider-instance"),
            Some(&alice),
            "test".to_string(),
        )
        .await
        .unwrap();

    assert!(registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            1,
            vec![bridge_provider("replacement-provider-instance")],
        )
        .await
        .unwrap());
    let polled = registry
        .poll(RunnerPollRequest {
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "bridge-instance".to_string(),
        })
        .await
        .unwrap();
    assert!(
        polled.is_none(),
        "changed provider lease must not receive stale work"
    );
    let response = receiver.await.unwrap();
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(response.error.as_ref().unwrap().code, "stale_provider");
}

#[tokio::test]
async fn dispatched_bridge_disconnect_is_outcome_unknown_and_not_replayed() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let (request_id, receiver) = registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "bridge-instance",
            McpGatewayRequest::ToolsCall {
                provider_id: "provider".to_string(),
                provider_instance_id: "provider-instance".to_string(),
                name: "effect".to_string(),
                arguments: serde_json::json!({}),
                expected_schema: crate::mcp_gateway::McpGatewaySchemaObservation {
                    input_schema: serde_json::json!({"type": "object"}),
                    output_schema: None,
                    annotations: None,
                },
            },
            Some(&alice),
            "test".to_string(),
        )
        .await
        .unwrap();
    let request = registry
        .poll(RunnerPollRequest {
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "bridge-instance".to_string(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.request_id, request_id);
    assert!(request.mcp_gateway.is_some());

    registry
        .reconcile_disconnect("bridge-runner", "bridge-instance")
        .await;
    let response = receiver.await.unwrap();
    assert_eq!(
        response.dispatch_state,
        McpGatewayDispatchState::OutcomeUnknown
    );
    assert_eq!(response.error.as_ref().unwrap().code, "runner_unavailable");

    let inner = registry.inner.lock().await;
    assert!(inner.pending_by_id.is_empty());
    assert!(inner.mcp_gateway_waiters.is_empty());
    assert!(inner
        .queues_by_runner
        .get("bridge-runner")
        .is_none_or(|queue| queue.is_empty()));
}

#[tokio::test]
async fn typed_bridge_result_is_correlated_once() {
    let registry = RunnerRegistry::default();
    register_bridge_runner(&registry).await;
    let alice = auth_context(Some("alice"), false);
    let (request_id, receiver) = registry
        .enqueue_mcp_gateway(
            "bridge-runner",
            "bridge-instance",
            list_request("provider-instance"),
            Some(&alice),
            "test".to_string(),
        )
        .await
        .unwrap();
    registry
        .poll(RunnerPollRequest {
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "bridge-instance".to_string(),
        })
        .await
        .unwrap()
        .unwrap();
    // A replacement changes future routing, not the identity or delivery of
    // an already dispatched request. Its correlated result still settles once.
    registry
        .replace_mcp_gateway_inventory(
            "bridge-runner",
            "bridge-instance",
            Some(&alice),
            1,
            vec![bridge_provider("next-provider-instance")],
        )
        .await
        .unwrap();
    let payload = RunnerResultPayload {
        result: RunnerResultRequest {
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "bridge-instance".to_string(),
            request_id: request_id.clone(),
            exit_code: None,
            stdout: None,
            stderr: None,
            duration_ms: None,
            error: None,
        },
        command_execution_state: None,
        mcp_gateway: Some(McpGatewayResponse::success(
            McpGatewayResponsePayload::Tools { tools: Vec::new() },
        )),
        plugin_gateway: None,
        coding_agent: None,
    };
    registry.complete(payload.clone()).await.unwrap();
    let response = receiver.await.unwrap();
    assert!(matches!(
        response.payload,
        Some(McpGatewayResponsePayload::Tools { .. })
    ));
    assert!(registry
        .complete(payload)
        .await
        .unwrap_err()
        .contains("unknown"));
    assert!(registry
        .poll(RunnerPollRequest {
            client_id: "bridge-runner".to_string(),
            runner_instance_id: "bridge-instance".to_string(),
        })
        .await
        .unwrap()
        .is_none());
}
