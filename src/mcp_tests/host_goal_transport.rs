use super::*;
use crate::runner_protocol::RunnerProjectSummary;

fn goal_context() -> Value {
    json!({
        "goal_id":"hgoal_test","node_id":"step","attempt_id":"attempt",
        "plan_version":1,"fence":1,"operation_id":"operation-1","context_epoch":"conversation-1"
    })
}

#[test]
fn host_goal_gateway_forwards_wrapper_metadata_without_provider_argument_changes() {
    for stateless in [false, true] {
        let provider_arguments = json!({"action":"call","binding":"opaque","arguments":{"goal_context":"provider-value"}});
        let (name, arguments) = crate::mcp::tools::unwrap_adaptive_runtime_gateway_arguments(
            json!({
                "tool":"plugin_tool","arguments":provider_arguments,
                "goal_context":goal_context(),"context_epoch":"conversation-1"
            }),
            stateless,
        )
        .unwrap();
        assert_eq!(name, "plugin_tool");
        assert_eq!(arguments["goal_context"], goal_context());
        assert_eq!(arguments["context_epoch"], "conversation-1");
        assert_eq!(
            arguments["arguments"],
            json!({"goal_context":"provider-value"})
        );
        for field in ["goal_context", "context_epoch"] {
            let mut duplicate = json!({"tool":"plugin_tool","arguments":{}});
            duplicate[field] = json!("outer-private-value");
            duplicate["arguments"][field] = json!("inner-private-value");
            let error =
                crate::mcp::tools::unwrap_adaptive_runtime_gateway_arguments(duplicate, stateless)
                    .unwrap_err();
            assert!(error.contains("both outside and inside"));
            assert!(!error.contains("private-value"));
        }
    }
}

#[test]
fn host_goal_specialized_rendering_keeps_native_provider_content_and_context_notice() {
    let content = json!([
        {"type":"text","text":"Provider text"},
        {"type":"image","mimeType":"image/png","data":"AAAA"},
        {"type":"resource_link","uri":"file:///artifact","name":"artifact"}
    ]);
    let structured = json!({"provider_owned":{"content":"unchanged"}});
    for is_error in [false, true] {
        let output = json!({
            "content":content,"structuredContent":structured,"isError":is_error,
            "dispatch_certainty":"completed",
            "context_meter":{"notice":"Save progress and open a new conversation.","level":"handoff_recommended"}
        });
        let result = if is_error {
            ToolResult::err_with_output("Provider error", output)
        } else {
            ToolResult::ok(output)
        };
        let rendered = crate::mcp::tools::specialized_runtime_result_for_mcp("plugin_tool", result);
        assert_eq!(rendered["structuredContent"], structured);
        assert_eq!(rendered["isError"], is_error);
        assert_eq!(
            &rendered["content"].as_array().unwrap()[..3],
            content.as_array().unwrap()
        );
        assert!(rendered["content"][3]["text"]
            .as_str()
            .unwrap()
            .contains("Save progress"));
        assert_eq!(
            rendered["_meta"]["webcodex/context_meter"]["level"],
            "handoff_recommended"
        );
        assert!(rendered.get("dispatch_certainty").is_none());
    }
}

#[test]
fn host_goal_specialized_rendering_preserves_gateway_error_shape() {
    for (tool, output) in [
        (
            "plugin_tool",
            json!({"error":{"code":"plugin_unavailable","message":"Unavailable"},"dispatch_certainty":"not_started"}),
        ),
        (
            "ssh_resource",
            json!({"error_kind":"ssh_resource_invalid","dispatch_state":"not_started"}),
        ),
    ] {
        let rendered = crate::mcp::tools::specialized_runtime_result_for_mcp(
            tool,
            ToolResult::err_with_output("Unavailable", output),
        );
        assert_eq!(rendered["isError"], true);
        assert_eq!(
            rendered["structuredContent"]["dispatchState"],
            "not_started"
        );
        assert!(rendered["structuredContent"]["error"]["code"].is_string());
    }
    let denied = ToolResult::err_with_output(
        "Goal not confirmed",
        json!({"error_kind":"goal_confirmation_required","dispatch_certainty":"not_started"}),
    );
    let rendered = crate::mcp::tools::specialized_runtime_result_for_mcp("plugin_tool", denied);
    assert_eq!(
        rendered["structuredContent"]["output"]["error_kind"],
        "goal_confirmation_required"
    );
}

#[test]
fn host_goal_mcp_schema_wrappers_are_closed_and_do_not_become_business_inputs() {
    let mut payload = json!({"tools":[
        {"name":"read_file","inputSchema":{"type":"object","properties":{"project":{"type":"string"}},"additionalProperties":false}},
        {"name":"plugin_tool","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"call_runtime_tool","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}
    ]});
    add_stateless_workflow_recorder_metadata(&mut payload, ModelSurface::LocalCoding);
    for tool in payload["tools"].as_array().unwrap() {
        assert_eq!(
            tool["inputSchema"]["properties"]["goal_context"],
            webcodex_tool_contracts::goal_context_input_schema()
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["context_epoch"],
            webcodex_tool_contracts::context_epoch_input_schema()
        );
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
    let canonical = registered_tool_specs()
        .into_iter()
        .find(|tool| tool.name == "read_file")
        .unwrap();
    assert!(canonical.input_schema["properties"]
        .get("goal_context")
        .is_none());
}

async fn call_host_goal_over_mcp(
    runtime: &ToolRuntime,
    auth: &crate::auth::AuthContext,
    tools: &Value,
    name: &str,
    mut arguments: Value,
    epoch: &str,
) -> Value {
    arguments["context_epoch"] = json!(epoch);
    let exposed = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap();
    webcodex_tool_contracts::test_support::validate_schema_instance(
        &arguments,
        &exposed["inputSchema"],
    )
    .unwrap_or_else(|error| panic!("public {name} input: {error}"));
    let outcome = Box::pin(handle_mcp_request(
        runtime,
        rpc(
            "tools/call",
            Some(json!(name)),
            mcp_2026_params(json!({
                "name":name,"arguments":arguments
            })),
        ),
        Some(auth),
    ))
    .await;
    let McpOutcome::Ok(response) = outcome else {
        panic!("{name} MCP request must return a tool result")
    };
    let canonical = registered_tool_specs()
        .into_iter()
        .find(|tool| tool.name == name)
        .unwrap();
    let result = response["result"]["structuredContent"].clone();
    webcodex_tool_contracts::test_support::validate_schema_instance(
        &result,
        &canonical.output_schema,
    )
    .unwrap_or_else(|error| panic!("public {name} output: {error}; {result}"));
    result
}

#[tokio::test]
async fn host_goal_mcp_plan_confirmation_claim_and_fresh_conversation_restore() {
    use std::sync::Arc;
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("goals.db");
    let runtime = test_runtime()
        .with_communication_database(Arc::new(crate::Database::open(&database_path).unwrap()));
    let mut auth = mcp_export_api_auth("goal-mcp-token", "goal-mcp-owner");
    auth.user_id = Some("goal-mcp-user".into());
    auth.scopes.push(crate::auth::SCOPE_PROJECT_WRITE.into());
    runtime
        .runner_registry
        .register_with_auth(
            RunnerRegisterRequest {
                client_id: "goal-mcp-runner".into(),
                runner_instance_id: "instance".into(),
                runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
                process_started_at: None,
                build: None,
                job_concurrency_limit: None,
                job_inventory: None,
                coding_agent_providers: None,
                coding_agent_inventory: None,
                display_name: None,
                owner: auth.username.clone(),
                hostname: None,
                host_context: None,
                capabilities: crate::test_support::current_runner_capabilities(
                    RunnerCapabilities {
                        file_read: true,
                        file_write: true,
                        git: true,
                        shell: true,
                        internal_posix_script: true,
                        ..Default::default()
                    },
                ),
                policy: None,
            },
            Some(&crate::test_support::runner_access(&auth)),
        )
        .await
        .unwrap();
    crate::test_support::apply_project_inventory_snapshot(
        &runtime.runner_registry,
        "goal-mcp-runner",
        "instance",
        vec![RunnerProjectSummary {
            id: "project".into(),
            name: Some("project".into()),
            path: temp.path().to_string_lossy().into_owned(),
            allow_patch: true,
            kind: Some("repo".into()),
            registration_source: None,
            description: None,
            hooks: vec![],
            disabled: false,
            revision: None,
            git_branch: None,
            git_head: None,
            git_dirty: None,
            updated_at: 1,
            shell_profile: None,
        }],
    )
    .await;
    let project = "agent:goal-mcp-runner:project";
    let listed = Box::pin(handle_mcp_request(
        &runtime,
        rpc("tools/list", Some(json!(1)), mcp_2026_params(json!({}))),
        Some(&auth),
    ))
    .await;
    let McpOutcome::Ok(listed) = listed else {
        panic!("MCP tool discovery failed")
    };
    let tools = &listed["result"]["tools"];
    let started = call_host_goal_over_mcp(
        &runtime,
        &auth,
        tools,
        "goal_mode_start",
        json!({
            "project":project,"objective":"Produce a sourced analysis","idempotency_key":"start"
        }),
        "chat-one",
    )
    .await;
    assert_eq!(started["success"], true, "{started}");
    let id = started["output"]["goal"]["goal_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(id.starts_with("hgoal_"));
    let plan = json!({
        "objective":"Produce a sourced analysis","deliverables":["Sourced conclusion in state"],
        "final_criteria":[{"kind":"review","id":"final","description":"Conclusion has its source"}],
        "nodes":[{"id":"research","title":"Research","work":"Read sources and record conclusions","inputs":["User question"],
            "outputs":["Sourced conclusion"],"criterion_ids":["final"],"acceptance":[{"kind":"review","id":"sources","description":"Research includes sources"}],"failure_action":"Record missing sources and continue research"}],
        "state_management":{"facts":"Retain supported facts","decisions":"Record rationale","issues":"Preserve blockers","artifacts":"Preserve source references", "checkpoint_triggers":["Before changing conversation"],"resume_instructions":"Read state and plan before continuing"}
    });
    let saved=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_plan_save",json!({
        "goal_id":id,"expected_revision":started["output"]["goal"]["revision"],"idempotency_key":"save","plan":plan
    }),"chat-one").await;
    assert_eq!(saved["success"], true, "{saved}");
    let goal = &saved["output"]["goal"];
    assert_eq!(goal["phase"], "awaiting_confirmation");
    assert!(goal["confirmation"].is_null());
    let artifacts = &saved["output"]["plan_artifacts"];
    for key in ["json_path", "markdown_path"] {
        let path = std::path::Path::new(artifacts[key].as_str().unwrap());
        assert!(path
            .canonicalize()
            .unwrap()
            .starts_with(temp.path().join("goals").canonicalize().unwrap()));
        assert!(path.is_file());
    }
    let early=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_claim_node",json!({
        "goal_id":id,"node_id":"research","expected_revision":goal["revision"],"plan_version":goal["plan_version"],"idempotency_key":"too-early"
    }),"chat-one").await;
    assert_eq!(
        early["success"], false,
        "Saving alone must not start a node"
    );
    let resumed = call_host_goal_over_mcp(
        &runtime,
        &auth,
        tools,
        "goal_bootstrap",
        json!({}),
        "chat-two",
    )
    .await;
    assert_eq!(resumed["success"], true, "{resumed}");
    assert_eq!(resumed["output"]["goal"]["goal_id"], id);
    assert_eq!(resumed["output"]["goal"]["phase"], "awaiting_confirmation");
    let confirmed=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_confirm_plan",json!({
        "goal_id":id,"expected_revision":resumed["output"]["goal"]["revision"],"plan_version":goal["plan_version"],"plan_sha256":goal["plan_hash"],
        "confirmation":"I reviewed the saved local plan. Start this version now.","idempotency_key":"confirm"
    }),"chat-two").await;
    assert_eq!(confirmed["success"], true, "{confirmed}");
    assert_eq!(confirmed["output"]["goal"]["phase"], "ready");
    let summary = call_host_goal_over_mcp(
        &runtime,
        &auth,
        tools,
        "goal_read",
        json!({"goal_id":id}),
        "chat-two",
    )
    .await;
    assert_eq!(summary["success"], true, "{summary}");
    assert!(summary["output"].get("context_read").is_none());
    let unread=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_claim_node",json!({
        "goal_id":id,"node_id":"research","expected_revision":summary["output"]["goal"]["revision"],"plan_version":goal["plan_version"],"idempotency_key":"claim-before-node-read"
    }),"chat-two").await;
    assert_eq!(
        unread["success"], false,
        "A summary must not satisfy node context admission"
    );
    assert_eq!(unread["output"]["error_kind"], "goal_context_read_required");
    let read = call_host_goal_over_mcp(
        &runtime,
        &auth,
        tools,
        "goal_read",
        json!({"goal_id":id,"view":"node","node_id":"research"}),
        "chat-two",
    )
    .await;
    assert_eq!(read["success"], true, "{read}");
    assert_eq!(read["output"]["projection"], "node_context");
    assert_eq!(read["output"]["context_read"]["complete"], true);
    let claimed=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_claim_node",json!({
        "goal_id":id,"node_id":"research","expected_revision":read["output"]["goal"]["revision"],"plan_version":goal["plan_version"],"idempotency_key":"claim"
    }),"chat-two").await;
    assert_eq!(claimed["success"], true, "{claimed}");
    assert_eq!(claimed["output"]["goal"]["phase"], "executing");
    assert_eq!(claimed["output"]["binding"]["goal_id"], id);
    let mut checkpoint_context = claimed["output"]["binding"].clone();
    checkpoint_context["operation_id"] = json!("checkpoint-effect");
    let checkpoint=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_checkpoint",json!({
        "goal_id":id,"expected_revision":claimed["output"]["goal"]["revision"],"idempotency_key":"checkpoint",
        "goal_context":checkpoint_context,
        "patch":{"reason":"First source reviewed","state":{"facts":{"source_one":"inspected","detail_one":"中文".repeat(600),"detail_two":"材料".repeat(600),"detail_three":"证据".repeat(600)},"completed_summary":"Reviewed the first source","issues":["Need a second source"],"next_step":"Prepare the next read"}}
    }),"chat-two").await;
    assert_eq!(checkpoint["success"], true, "{checkpoint}");
    checkpoint_context["operation_id"] = json!("handoff-effect");
    let handoff=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_checkpoint",json!({
        "goal_id":id,"expected_revision":checkpoint["output"]["goal"]["revision"],"idempotency_key":"handoff",
        "goal_context":checkpoint_context,
        "patch":{"action":"handoff","reason":"Change conversation","state":{"next_step":"Read source two","facts":{"detail_two":null}}}
    }),"chat-two").await;
    assert_eq!(handoff["success"], true, "{handoff}");
    let registry = runtime.runner_registry.clone();
    drop(runtime);
    let restored = ToolRuntime::new_for_tests_with_runner_registry(registry)
        .with_communication_database(Arc::new(crate::Database::open(&database_path).unwrap()));
    let recovery = call_host_goal_over_mcp(
        &restored,
        &auth,
        tools,
        "goal_bootstrap",
        json!({}),
        "chat-three",
    )
    .await;
    assert_eq!(recovery["success"], true, "{recovery}");
    assert_eq!(recovery["output"]["goal"]["goal_id"], id);
    assert_eq!(
        recovery["output"]["progress_summary"]["completed_summary"],
        "Reviewed the first source"
    );
    assert_eq!(
        recovery["output"]["progress_summary"]["issues"],
        json!(["Need a second source"])
    );
    assert_eq!(recovery["output"]["goal"]["current_node_id"], "research");
    assert_eq!(recovery["output"]["progress_summary"]["partial"], true);
    let mut state_text = String::new();
    let mut cursor = None;
    let mut pages = 0;
    for _ in 0..128 {
        let mut arguments = json!({"goal_id":id,"view":"detail","section":"state"});
        if let Some(cursor) = cursor {
            arguments["cursor"] = json!(cursor);
        }
        let detail = call_host_goal_over_mcp(
            &restored,
            &auth,
            tools,
            "goal_read",
            arguments,
            "chat-three",
        )
        .await;
        pages += 1;
        assert_eq!(detail["success"], true, "{detail}");
        assert_eq!(detail["output"]["encoding"], "json_text");
        assert!(serde_json::to_vec(&detail["output"]).unwrap().len() <= 8192);
        state_text.push_str(detail["output"]["text"].as_str().unwrap());
        cursor = detail["output"]["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert!(
        cursor.is_none(),
        "State detail must reach a terminal cursor within its bounded size"
    );
    assert!(
        pages > 1,
        "The persisted state must be recovered through multiple bounded UTF-8 pages"
    );
    let restored_state: Value = serde_json::from_str(&state_text).unwrap();
    assert_eq!(restored_state["next_step"], "Read source two");
    assert_eq!(
        restored_state["facts"]["source_one"], "inspected",
        "A partial update must preserve prior facts"
    );
    assert_eq!(restored_state["facts"]["detail_one"], "中文".repeat(600));
    assert!(
        restored_state["facts"].get("detail_two").is_none(),
        "Null deletes the selected fact instead of persisting a null"
    );
    assert_eq!(restored_state["facts"]["detail_three"], "证据".repeat(600));
    assert_eq!(restored_state["issues"], json!(["Need a second source"]));
}
