//! Public Goal ingress contracts. No provider processes or external services run here.
use super::support::*;
use crate::auth::{AuthContext, AuthKind};
use crate::tool_runtime::kernel::{
    HostFileImportTrust, ToolCallContext, ToolCallErrorStatus, ToolCallOutcome, ToolCallRequest,
    ToolTransport,
};
use crate::tool_runtime::{registered_tool_specs, ToolCall, ToolRuntime};
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};
use serde_json::{json, Value};
use std::sync::Arc;
use webcodex_core::goal::{GoalLimits, GoalRound, GoalSpec, GoalState};
use webcodex_tool_contracts::test_support::validate_schema_instance;

const GOAL_TOOLS: &[&str] = &[
    "goal_start",
    "goal_read",
    "goal_list",
    "goal_cancel",
    "goal_resume",
];
const EXECUTION_SCOPES: &[&str] = &[
    "coding_agent:run",
    "project:write",
    "project:read",
    "job:run",
    "job:detach",
];

fn scoped_auth(scopes: &[&str]) -> AuthContext {
    let mut auth = auth_context(Some("goal-contract-owner"), false);
    auth.scopes = scopes.iter().map(|scope| (*scope).to_string()).collect();
    auth
}

fn goal_input() -> Value {
    json!({
        "project":"agent:goal-contract-runner:fixture","provider_id":"codex",
        "objective":"Produce the verified result file",
        "criteria":[{"kind":"file_contains","id":"contents","description":"Result contains ready","path":"result.txt","text":"ready"}],
        "artifact_paths":["result.txt"],"idempotency_key":"goal-contract-start"
    })
}

async fn kernel_call(
    runtime: &ToolRuntime,
    name: &str,
    arguments: Value,
    auth: Option<&AuthContext>,
    transport: ToolTransport,
) -> ToolCallOutcome {
    runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name: name.to_string(),
                arguments,
            },
            ToolCallContext {
                transport,
                session_id: None,
                auth,
                window: None,
                record_oauth_scope_denials: false,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await
}

#[test]
fn goal_contracts_openapi_and_oauth_derive_from_the_same_registered_tools() {
    let api = crate::openapi::build_openapi_spec();
    let action = &api["components"]["schemas"]["ToolCallRequest"];
    assert_eq!(action["additionalProperties"], false);
    let properties = action["properties"].as_object().unwrap();
    let tools_description = properties["tool"]["description"].as_str().unwrap();
    let specs = registered_tool_specs();
    for name in GOAL_TOOLS {
        let spec = spec_named(&specs, name);
        assert!(
            tools_description.contains(name),
            "OpenAPI does not admit {name}"
        );
        let definition =
            crate::tool_runtime::tool_definition::lookup_tool_definition(name).unwrap();
        assert_eq!(
            crate::auth::scopes::oauth_scope_policy_for_runtime_tool(name),
            definition.metadata.authority
        );
        for field in spec.input_schema["properties"].as_object().unwrap().keys() {
            assert!(
                properties.contains_key(field),
                "{name}: missing flattened field {field}"
            );
        }
    }
    for field in [
        "objective",
        "criteria",
        "artifact_paths",
        "reviewer_provider_id",
        "limits",
        "goal_id",
        "round_offset",
        "round_limit",
    ] {
        assert!(properties.contains_key(field), "missing Goal field {field}");
    }
    assert!(!tools_description.contains("goal_complete"));
    for field in ["state", "worker_epoch", "owner_key", "verdict"] {
        let mut forged = goal_input();
        forged[field] = json!("completed");
        assert!(
            ToolCall::from_tool_name("goal_start", forged).is_err(),
            "{field}"
        );
    }
    let mut forged = goal_input();
    forged["criteria"][0]["passed"] = json!(true);
    assert!(ToolCall::from_tool_name("goal_start", forged).is_err());
    assert!(
        ToolCall::from_tool_name("goal_complete", json!({"goal_id":"wc_goal_unknown"})).is_err()
    );
}

#[tokio::test]
async fn goal_contracts_kernel_denies_missing_execution_scopes_before_admission() {
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(crate::Database::open(&temp.path().join("goals.db")).unwrap());
    let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
    for transport in [ToolTransport::Api, ToolTransport::Mcp] {
        for name in GOAL_TOOLS {
            let args = match *name {
                "goal_start" => goal_input(),
                "goal_list" => json!({}),
                _ => json!({"goal_id":"wc_goal_unknown"}),
            };
            let outcome = kernel_call(&runtime, name, args, None, transport).await;
            assert!(
                matches!(
                    outcome.error_status,
                    Some(ToolCallErrorStatus::InsufficientScope { .. })
                ),
                "{name} cannot infer a Goal principal from missing authentication"
            );
        }
        for name in ["goal_start", "goal_resume"] {
            let args = if name == "goal_start" {
                goal_input()
            } else {
                json!({"goal_id":"wc_goal_unknown"})
            };
            for missing in EXECUTION_SCOPES {
                let scopes = EXECUTION_SCOPES
                    .iter()
                    .copied()
                    .filter(|scope| scope != missing)
                    .collect::<Vec<_>>();
                let mut auth = scoped_auth(&scopes);
                for kind in [AuthKind::ApiToken, AuthKind::OAuth2Token] {
                    auth.kind = kind;
                    let outcome =
                        kernel_call(&runtime, name, args.clone(), Some(&auth), transport).await;
                    assert!(
                        matches!(outcome.error_status,Some(ToolCallErrorStatus::InsufficientScope{required_scope:Some(scope),..}) if scope==*missing),
                        "{name}/{missing}: {:?}",
                        outcome.error_status
                    );
                    assert!(outcome.result.is_none());
                }
            }
            for auth in [
                None,
                Some(open_auth_context()),
                Some(shared_key_auth_context("goal-contract-shared")),
            ] {
                let outcome =
                    kernel_call(&runtime, name, args.clone(), auth.as_ref(), transport).await;
                assert!(
                    matches!(
                        outcome.error_status,
                        Some(ToolCallErrorStatus::InsufficientScope { .. })
                    ),
                    "{name}: {:?}",
                    outcome.error_status
                );
            }
        }
    }
    let owner =
        crate::tool_runtime::coding_agent::goal_owner_key(Some(&scoped_auth(EXECUTION_SCOPES)))
            .unwrap();
    assert!(
        db.list_goals(&owner, None, 100).unwrap().is_empty(),
        "denied starts must not create durable work"
    );
}

#[tokio::test]
async fn goal_contracts_actual_read_and_list_results_match_schemas_without_disclosing_foreign_goals(
) {
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(crate::Database::open(&temp.path().join("goals.db")).unwrap());
    let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
    let auth = scoped_auth(&["coding_agent:run"]);
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    let mut input = goal_input();
    input.as_object_mut().unwrap().remove("idempotency_key");
    let mut spec: GoalSpec = serde_json::from_value(input).unwrap();
    spec.limits = GoalLimits {
        max_rounds: 5,
        ..Default::default()
    };
    let mut goal = db.create_goal_at(&owner, spec, "seed", 1000).unwrap().goal;
    goal.state = GoalState::Paused;
    for number in 1..=3 {
        let mut round = GoalRound::new(number, 1000);
        round.execution_output = format!("PRIVATE_EXECUTION_OUTPUT_{number}");
        round.finished_at = Some(1001);
        goal.rounds.push(round);
        goal = db.save_goal(&owner, &goal, goal.revision).unwrap();
    }
    let specs = registered_tool_specs();
    for (args, offset, count, next) in [
        (json!({"goal_id":goal.goal_id}), 2, 1, Value::Null),
        (
            json!({"goal_id":goal.goal_id,"round_offset":0,"round_limit":2}),
            0,
            2,
            json!(2),
        ),
    ] {
        let outcome =
            kernel_call(&runtime, "goal_read", args, Some(&auth), ToolTransport::Api).await;
        assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
        let result = outcome.result.unwrap();
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.output["total_rounds"], 3);
        assert_eq!(result.output["round_offset"], offset);
        assert_eq!(
            result.output["goal"]["rounds"].as_array().unwrap().len(),
            count
        );
        assert_eq!(result.output["next_round_offset"], next);
        assert_eq!(result.output["delivery_freshness"], "not_completed");
        assert_eq!(result.output["delivery_ready"], false);
        validate_schema_instance(
            &serde_json::to_value(result).unwrap(),
            &spec_named(&specs, "goal_read").output_schema,
        )
        .unwrap();
    }
    let result = kernel_call(
        &runtime,
        "goal_list",
        json!({}),
        Some(&auth),
        ToolTransport::Api,
    )
    .await
    .result
    .unwrap();
    assert!(result.success, "{:?}", result.error);
    assert_eq!(result.output["goals"][0]["round_count"], 3);
    assert!(!result
        .output
        .to_string()
        .contains("PRIVATE_EXECUTION_OUTPUT"));
    validate_schema_instance(
        &serde_json::to_value(result).unwrap(),
        &spec_named(&specs, "goal_list").output_schema,
    )
    .unwrap();
    let mut foreign = auth.clone();
    foreign.user_id = Some("foreign-user".into());
    foreign.username = Some("foreign-user".into());
    let mut errors = Vec::new();
    for id in [&goal.goal_id, "wc_goal_00000000000000000000000000000000"] {
        let result = kernel_call(
            &runtime,
            "goal_read",
            json!({"goal_id":id}),
            Some(&foreign),
            ToolTransport::Api,
        )
        .await
        .result
        .unwrap();
        assert!(!result.success);
        errors.push((result.output["error_kind"].clone(), result.error));
    }
    assert_eq!(
        errors[0], errors[1],
        "foreign Goal existence must remain hidden"
    );
}

#[tokio::test]
async fn goal_contracts_mcp_oauth_inventory_filters_execution_and_preserves_paging_schema() {
    for (scopes, can_execute) in [(EXECUTION_SCOPES, true), (&["coding_agent:run"][..], false)] {
        let runtime = Arc::new(
            ToolRuntime::new_for_tests()
                .with_model_surface(crate::model_surface::ModelSurface::LocalCoding),
        );
        let mut auth = scoped_auth(scopes);
        // MCP deliberately projects scopes for delegated OAuth discovery;
        // API-token discovery is broader, with execution still kernel-guarded.
        auth.kind = AuthKind::OAuth2Token;
        auth.token_kind = Some("oauth2".into());
        auth.scopes.push("runtime:read".into());
        let router = Router::new()
            .hoop(affix_state::inject(runtime))
            .hoop(affix_state::inject(auth))
            .hoop(affix_state::inject(
                crate::connector_runtime::ConnectorRuntimeSlot::default(),
            ))
            .push(Router::with_path("mcp").post(crate::mcp::mcp_post));
        let service = Service::new(router);
        let mut response = TestClient::post("http://localhost/mcp")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .send(&service)
            .await;
        let body: Value = response.take_json().await.unwrap();
        let tools = body["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("MCP response: {body}"));
        for name in GOAL_TOOLS {
            let tool = tools.iter().find(|tool| tool["name"] == *name);
            let expected = can_execute || !matches!(*name, "goal_start" | "goal_resume");
            assert_eq!(tool.is_some(), expected, "{name} with {scopes:?}");
            if let Some(tool) = tool {
                assert_eq!(tool["inputSchema"]["additionalProperties"], false);
                if *name == "goal_read" {
                    assert_eq!(
                        tool["inputSchema"]["properties"]["round_limit"]["maximum"],
                        5
                    );
                    assert!(tool["inputSchema"]["properties"]
                        .get("round_offset")
                        .is_some());
                }
            }
        }
        assert!(!tools.iter().any(|tool| tool["name"] == "goal_complete"));
    }
}
