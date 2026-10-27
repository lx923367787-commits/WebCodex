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

#[test]
fn host_goal_mcp_handoff_selector_remains_separate_from_caller_identity() {
    let canonical = registered_tool_specs()
        .into_iter()
        .find(|tool| tool.name == "goal_read")
        .unwrap();
    let mut payload = json!({"tools":[{"name":"goal_read","inputSchema":canonical.input_schema}]});
    add_stateless_workflow_recorder_metadata(&mut payload, ModelSurface::LocalCoding);
    let schema = &payload["tools"][0]["inputSchema"];
    let request = json!({
        "goal_id":"hgoal_example","view":"handoff",
        "source_context_epoch":"source-A","context_epoch":"observer-B"
    });
    webcodex_tool_contracts::test_support::validate_schema_instance(&request, schema).unwrap();
    assert_eq!(
        schema["properties"]["source_context_epoch"],
        canonical.input_schema["properties"]["source_context_epoch"]
    );
    assert_eq!(
        schema["properties"]["context_epoch"],
        webcodex_tool_contracts::context_epoch_input_schema()
    );
    assert_eq!(schema["additionalProperties"], false);
}

async fn call_host_goal_over_mcp(
    runtime: &ToolRuntime,
    auth: &crate::auth::AuthContext,
    tools: &Value,
    name: &str,
    mut arguments: Value,
    epoch: &str,
) -> Value {
    arguments
        .as_object_mut()
        .unwrap()
        .entry("context_epoch")
        .or_insert_with(|| json!(epoch));
    // Exercise both admitted identity modes: the direct surface receives real
    // MCP host metadata, while the gateway fixture retains weaker explicit keys.
    let host_metadata = tools
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "goal_mode_start")
        && !(name == "goal_handoff"
            && matches!(
                arguments["action"].as_str(),
                Some("begin_attempt" | "fail_attempt" | "abort")
            ));
    let direct = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name);
    let (public_name, arguments) = if direct.is_some() {
        (name, arguments)
    } else {
        (
            "call_runtime_tool",
            json!({"tool":name,"arguments":arguments}),
        )
    };
    let exposed = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == public_name)
        .unwrap();
    webcodex_tool_contracts::test_support::validate_schema_instance(
        &arguments,
        &exposed["inputSchema"],
    )
    .unwrap_or_else(|error| panic!("public {name} input: {error}"));
    let root = runtime
        .execution()
        .resolve_project_input_for_auth("agent:goal-mcp-runner:project", Some(auth))
        .await
        .unwrap()
        .config
        .path;
    let mut params = mcp_2026_params(json!({"name":public_name,"arguments":arguments}));
    if host_metadata {
        params["_meta"]["openai/session"] = json!(epoch);
    }
    // The convenience handle_mcp_request test wrapper deliberately supplies no
    // window. Use the same metadata parser and dispatcher as production mcp_post.
    let window = crate::client_window::stateless_mcp_window(&params);
    let outcome = crate::test_support::runner_project_files::during(
        runtime,
        "goal-mcp-runner",
        "instance",
        std::path::Path::new(&root),
        Box::pin(handle_mcp_request_with_lifecycle(
            runtime,
            None,
            rpc("tools/call", Some(json!(name)), params),
            Some(auth),
            McpProtocolEra::Stateless2026,
            HostFileImportTrust::Untrusted,
            window.identity.as_ref(),
            None,
            None,
            crate::config::mcp_compact_schemas_enabled(),
            None,
        )),
    )
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
    let overview = result["output"].get("workflow_overview");
    if result["success"] == true && matches!(name, "goal_mode_start" | "goal_bootstrap") {
        let overview = overview
            .and_then(Value::as_str)
            .expect("final MCP entry overview");
        assert!(overview.contains("New goal: read inputs -> goal_plan_save"));
        assert!(overview.contains("Resume: read the same Goal's local state"));
        assert!(
            overview.contains("new chat does not require reconfirming an unchanged confirmed plan")
        );
        assert!(result["output"].get("context_read").is_none());
        let bytes = serde_json::to_vec(&result["output"]).unwrap().len();
        assert!(bytes <= 8192, "{name} output: {bytes}");
        eprintln!("host_goal_entry route={public_name} tool={name} output_bytes={bytes} result_bytes={} mcp_bytes={} overview_bytes={}",
            serde_json::to_vec(&result).unwrap().len(), serde_json::to_vec(&response).unwrap().len(), overview.len());
        eprintln!("host_goal_entry_sample={result}");
    } else {
        assert!(
            overview.is_none(),
            "workflow maps belong only to successful entries"
        );
    }
    result
}

#[tokio::test]
async fn host_goal_mcp_plan_confirmation_claim_and_fresh_conversation_restore() {
    host_goal_mcp_workflow(ModelSurface::LocalCoding).await;
}

#[tokio::test]
async fn host_goal_mcp_gateway_plan_confirmation_claim_and_fresh_conversation_restore() {
    host_goal_mcp_workflow(ModelSurface::AdaptiveRuntime).await;
}

async fn host_goal_mcp_workflow(surface: ModelSurface) {
    use std::sync::Arc;
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("goals.db");
    let runtime = test_runtime_with_surface(surface)
        .with_communication_database(Arc::new(crate::Database::open(&database_path).unwrap()));
    let mut auth = mcp_export_api_auth("goal-mcp-token", "goal-mcp-owner");
    auth.user_id = Some("goal-mcp-user".into());
    auth.scopes.push(crate::auth::SCOPE_PROJECT_WRITE.into());
    runtime
        .runner_registry
        .register_with_auth(
            RunnerRegisterRequest {
                agent_execution_resources: Vec::new(),
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
    assert!(!tools.to_string().contains("workflow_overview\":\"Map only"));
    if surface == ModelSurface::AdaptiveRuntime {
        assert!(!tools
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "goal_mode_start"));
    }
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
    assert_eq!(
        started["output"]["context_meter"]["identity_source"],
        if surface == ModelSurface::LocalCoding {
            "window_bound"
        } else {
            "explicit_context_only"
        }
    );
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
    assert_eq!(resumed["output"]["confirmation_required"], true);
    assert_eq!(
        resumed["output"]["workflow_overview"],
        started["output"]["workflow_overview"]
    );
    let confirmed=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_confirm_plan",json!({
        "goal_id":id,"expected_revision":resumed["output"]["goal"]["revision"],"plan_version":goal["plan_version"],"plan_sha256":goal["plan_hash"],
        "confirmation":"I reviewed the saved local plan. Start this version now.","idempotency_key":"confirm"
    }),"chat-two").await;
    assert_eq!(confirmed["success"], true, "{confirmed}");
    assert_eq!(confirmed["output"]["goal"]["phase"], "ready");
    let ready = call_host_goal_over_mcp(
        &runtime,
        &auth,
        tools,
        "goal_bootstrap",
        json!({"goal_id":id}),
        "ready-chat",
    )
    .await;
    assert_eq!(ready["output"]["goal"], confirmed["output"]["goal"]);
    assert_eq!(ready["output"]["confirmation_required"], false);
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
    let transferred_progress = if surface == ModelSurface::LocalCoding {
        assert_eq!(handoff["success"], true, "{handoff}");
        let status = call_host_goal_over_mcp(
            &runtime,
            &auth,
            tools,
            "goal_handoff",
            json!({"goal_id":id,"action":"status"}),
            "chat-two",
        )
        .await;
        assert_eq!(status["success"], true, "{status}");
        assert_eq!(status["output"]["handoff"]["status"], "waiting");
        assert_eq!(status["output"]["execution_granted"], false);
        let mut scheduler = auth.clone();
        scheduler.scopes.push(crate::auth::SCOPE_ADMIN.into());
        let reserved=call_host_goal_over_mcp(&runtime,&scheduler,tools,"goal_handoff",
            json!({"goal_id":id,"action":"begin_attempt","handoff_id":status["output"]["handoff"]["handoff_id"]}),
            "local-scheduler").await;
        assert_eq!(reserved["success"], true, "{reserved}");
        let h = &reserved["output"]["handoff"];
        assert_eq!(h["status"], "awaiting_ready");
        let prepared = call_host_goal_over_mcp(
            &runtime,
            &auth,
            tools,
            "goal_handoff",
            json!({"goal_id":id,"action":"prepare","handoff_id":h["handoff_id"],
                "attempt_id":h["attempt_id"],"checkpoint_sequence":h["checkpoint_sequence"]}),
            "chat-three",
        )
        .await;
        assert_eq!(prepared["success"], true, "{prepared}");
        assert_eq!(prepared["output"]["execution_granted"], false);
        assert_eq!(prepared["output"]["package"]["goal_id"], id);
        let committed = call_host_goal_over_mcp(
            &runtime,
            &auth,
            tools,
            "goal_handoff",
            json!({"goal_id":id,"action":"commit","handoff_id":h["handoff_id"],
                "attempt_id":h["attempt_id"],"checkpoint_sequence":h["checkpoint_sequence"],
                "package_sha256":prepared["output"]["package_sha256"]}),
            "chat-three",
        )
        .await;
        assert_eq!(committed["success"], true, "{committed}");
        assert_eq!(committed["output"]["execution_granted"], true);
        assert_eq!(committed["output"]["handoff"]["status"], "committed");
        let retired = call_host_goal_over_mcp(
            &runtime,
            &auth,
            tools,
            "goal_bootstrap",
            json!({"goal_id":id,"activate":true,"context_epoch":"forged-new-epoch"}),
            "chat-two",
        )
        .await;
        assert_eq!(retired["success"], false, "{retired}");
        assert_eq!(
            retired["output"]["error_kind"],
            "goal_handoff_context_revoked"
        );
        Some(prepared["output"]["package"]["progress"].clone())
    } else {
        assert_eq!(handoff["success"], false, "{handoff}");
        assert_eq!(
            handoff["output"]["error_kind"],
            "goal_handoff_identity_required"
        );
        // Explicit epochs can save ordinary progress, but cannot authorize a transfer.
        let saved_progress=call_host_goal_over_mcp(&runtime,&auth,tools,"goal_checkpoint",json!({
            "goal_id":id,"expected_revision":checkpoint["output"]["goal"]["revision"],"idempotency_key":"save-before-reopen",
            "goal_context":checkpoint_context,
            "patch":{"reason":"Save progress before reopening durable state","state":{"next_step":"Read source two","facts":{"detail_two":null}}}
        }),"chat-two").await;
        assert_eq!(saved_progress["success"], true, "{saved_progress}");
        None
    };
    let registry = runtime.runner_registry.clone();
    drop(runtime);
    let restored = ToolRuntime::new_for_tests_with_runner_registry(registry)
        .with_runtime_exposure(RuntimeExposure::Runtime(surface))
        .with_communication_database(Arc::new(crate::Database::open(&database_path).unwrap()));
    if surface == ModelSurface::LocalCoding {
        let durable = call_host_goal_over_mcp(
            &restored,
            &auth,
            tools,
            "goal_handoff",
            json!({"goal_id":id,"action":"status"}),
            "chat-three",
        )
        .await;
        assert_eq!(durable["success"], true, "{durable}");
        assert_eq!(durable["output"]["execution_granted"], true);
        assert_eq!(durable["output"]["handoff"]["attempt_count"], 1);
    }
    let recovery = call_host_goal_over_mcp(
        &restored,
        &auth,
        tools,
        "goal_bootstrap",
        json!({"goal_id":id}),
        "chat-three",
    )
    .await;
    assert_eq!(recovery["success"], true, "{recovery}");
    assert_eq!(recovery["output"]["goal"]["goal_id"], id);
    assert_eq!(
        recovery["output"]["goal"]["phase"],
        if surface == ModelSurface::LocalCoding {
            "ready"
        } else {
            "executing"
        }
    );
    assert_eq!(recovery["output"]["confirmation_required"], false);
    assert_eq!(
        recovery["output"]["goal"]["confirmation"],
        confirmed["output"]["goal"]["confirmation"]
    );
    assert_eq!(
        recovery["output"]["workflow_overview"],
        started["output"]["workflow_overview"]
    );
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
    for _ in 0..if transferred_progress.is_some() {
        0
    } else {
        128
    } {
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
    if transferred_progress.is_none() {
        assert!(
            pages > 1,
            "The ordinary recovery path preserves multiple bounded UTF-8 state pages"
        );
    }
    let restored_state: Value =
        transferred_progress.unwrap_or_else(|| serde_json::from_str(&state_text).unwrap());
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
