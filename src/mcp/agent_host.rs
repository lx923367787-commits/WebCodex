//! OAuth host requests terminate in this narrow broker, never the legacy
//! shared-user tool dispatcher. An exact outer host window selects its run;
//! configured native hosts may enroll a new independent Agent on first use.
//! A handoff candidate needs either an exact control-plane registration or
//! the current one-time preparation grant delivered by the trusted launcher;
//! an attempt UUID by itself is correlation, never authority.
use super::response::{mcp_stateless_result, rpc_error, rpc_result};
use super::{protocol, JsonRpcRequest, McpOutcome, McpProtocolEra};
use crate::auth::{AuthContext, AuthKind};
use crate::tool_runtime::{ToolResult, ToolRuntime};
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_store::AgentHostWindowBinding;

#[path = "agent_host_contract.rs"]
mod contract;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
    #[serde(rename = "_meta")]
    _meta: Option<Value>,
}
fn empty_object() -> Value {
    json!({})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayCall {
    tool: String,
    arguments: Value,
}

fn status_arguments_valid(arguments: &Value) -> bool {
    matches!(
        crate::tool_runtime::ToolCall::from_tool_name("runtime_status", arguments.clone()),
        Ok(crate::tool_runtime::ToolCall::RuntimeStatus { .. })
    )
}

fn connection_status(
    db: &crate::Database,
    binding: &webcodex_store::VerifiedAgentRun,
) -> ToolResult {
    let (_, project_access) = crate::agent_enrollment::project_access(db, binding);
    let assignment = db.read_agent_enrollment(binding).ok().map(|assignment| {
        json!({
            "task_id":assignment.task_id,"task_directory":assignment.task_directory,
            "purpose":assignment.purpose,"policy":assignment.policy
        })
    });
    ToolResult::ok(json!({
        "connected":true,"scope":"current_window","runtime_exposure":"isolated_agent",
        "agent_state":"active","agent":{"agent_id":binding.agent_id(),
            "run_id":binding.run_id(),"generation":binding.generation(),"project":binding.project()},
        "assignment":assignment,"project_access":project_access,"shared_runtime_access":true,
        "privacy_mode":"soft_convention","technical_isolation":false,
        "next_action":"Read agent_assignment for this chat's own task and three-folder privacy convention. Use tool_manifest to discover the available runtime tools."
    }))
}

pub(super) async fn handle(
    runtime: &ToolRuntime,
    request: JsonRpcRequest,
    auth: &AuthContext,
    era: McpProtocolEra,
) -> McpOutcome {
    let denied = |code: &'static str| {
        if let Some(db) = runtime.isolation_database() {
            let _ = db.record_agent_access_denied(None, code);
        }
        let mut error = rpc_error(request.id.clone(), -32003, code);
        if code == "unknown_tool" {
            error["error"]["data"] = json!({"recovery":"Call tool_manifest to inspect the current tool names and input contracts. Ordinary runtime capabilities use the same project authorization."});
        }
        McpOutcome::Ok(error)
    };
    let Some(db) = runtime.isolation_database() else {
        return denied("agent_isolation_unavailable");
    };
    if !crate::agent_host_auth::authorized(db, auth) {
        return denied("agent_host_authorization_required");
    }
    let Some(principal) = crate::agent_host_auth::principal_key(auth) else {
        return denied("agent_host_authorization_required");
    };
    if !auth.has_scope("runtime:read") {
        return denied("agent_host_scope_denied");
    }
    // Only the explicitly trusted host's outer metadata is accepted. We do
    // not infer a window from OAuth credentials, HTTP cookies, transport IDs,
    // names or a value supplied inside model tool arguments.
    let window = crate::client_window::stateless_mcp_window(&request.params);
    let stateless = era == McpProtocolEra::Stateless2026;
    match request.method.as_str() {
        "server/discover" if stateless => McpOutcome::Ok(rpc_result(
            request.id,
            protocol::server_discover_payload(
                json!({"tools":{"listChanged":false}}),
                "isolated-agent",
            ),
        )),
        "initialize" if !stateless => McpOutcome::Ok(rpc_result(
            request.id,
            protocol::legacy_initialize_payload(&request.params, "isolated-agent"),
        )),
        "ping" if !stateless => McpOutcome::Ok(rpc_result(request.id, json!({}))),
        "tools/list" => {
            // Hosts may cache tools/list for the entire chat/connection. Keep
            // this data-free isolated catalog stable (listChanged is false),
            // so the successor can use the same tools after commit. Discovery
            // grants nothing: business calls require an active run. Only the
            // configured first-use entry points may enroll a fresh window.
            let specs = if auth.is_native_host() {
                contract::native_catalog()
            } else {
                crate::agent_isolation::tool_specs()
            };
            McpOutcome::Ok(rpc_result(
                request.id,
                if stateless {
                    mcp_stateless_result(specs, true)
                } else {
                    specs
                },
            ))
        }
        "tools/call" => {
            let Some(window) = window.identity else {
                return denied("agent_host_window_required");
            };
            let mut call: Call = match serde_json::from_value(request.params.clone()) {
                Ok(call) => call,
                Err(_) => return denied("agent_invalid_input"),
            };
            // An existing chat may retain the pre-upgrade connector catalog.
            // Its gateway is only an envelope for this same isolated broker;
            // the outer trusted window and target's scope checks stay intact.
            if call.name == "call_runtime_tool" {
                let inner: GatewayCall = match serde_json::from_value(call.arguments) {
                    Ok(inner) => inner,
                    Err(_) => return denied("agent_invalid_input"),
                };
                let catalog = if auth.is_native_host() {
                    contract::native_catalog()
                } else {
                    crate::agent_isolation::tool_specs()
                };
                let native_contract = auth.is_native_host()
                    && inner.tool == "tool_manifest"
                    && contract::discover(&catalog, &inner.arguments).is_ok();
                if !inner.arguments.is_object()
                    || (!native_contract
                        && !catalog["tools"].as_array().is_some_and(|specs| {
                            specs.iter().any(|spec| spec["name"] == inner.tool)
                        }))
                {
                    return denied("unknown_tool");
                }
                call.name = inner.tool;
                call.arguments = inner.arguments;
            }
            // The verified native host already passed runtime:read. Cached
            // connectors need exact public contracts before prepare can bind
            // a Pending window, and again after commit for other isolated tools.
            // This branch never resolves a run or enters the legacy manifest.
            let native_contract = auth.is_native_host() && call.name == "tool_manifest";
            let native_status = auth.is_native_host() && call.name == "runtime_status";
            if native_status && !status_arguments_valid(&call.arguments) {
                return denied("agent_invalid_input");
            }
            if !native_contract
                && !native_status
                && !auth.has_scope(crate::agent_host_auth::tool_scope(
                    &call.name,
                    &call.arguments,
                ))
            {
                return denied("agent_host_scope_denied");
            }
            let browser_image = call.name == "agent_process"
                && call.arguments.get("action").and_then(Value::as_str) == Some("browser_image");
            let result = if native_contract {
                match contract::discover(&contract::native_catalog(), &call.arguments) {
                    Ok(metadata) => crate::tool_runtime::ToolResult::ok(metadata),
                    Err(code) => crate::agent_isolation::denial(code),
                }
            } else if auth.is_native_host() && call.name == "agent_projects" {
                if !call.arguments.as_object().is_some_and(|a| a.is_empty()) {
                    return denied("agent_invalid_input");
                }
                match auth
                    .native_host
                    .as_ref()
                    .and_then(|host| db.native_workspace_choices(host).ok())
                {
                    Some(catalog) => ToolResult::ok(
                        json!({"projects":catalog.projects,"state_changed":false,"next_action":"Select the project named by the user with agent_select_project. If the name is missing or ambiguous, ask the user; never guess."}),
                    ),
                    None => crate::agent_isolation::denial("workspace_not_authorized"),
                }
            } else if auth.is_native_host() && call.name == "agent_select_project" {
                let selected = call
                    .arguments
                    .as_object()
                    .filter(|a| a.len() == 1)
                    .and_then(|a| a.get("project"))
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty() && p.len() <= 256);
                let Some(selected) = selected else {
                    return denied("agent_invalid_input");
                };
                let Some(host) = auth.native_host.as_ref() else {
                    return denied("agent_host_authorization_required");
                };
                match db.select_native_host_workspace(
                    host,
                    window.key(),
                    Some(selected),
                    chrono::Utc::now().timestamp_millis(),
                ) {
                    Ok(binding) => {
                        let mut result = connection_status(db, &binding);
                        result.output["project"] =
                            serde_json::to_value(db.agent_native_workspace(&binding).ok())
                                .unwrap_or(Value::Null);
                        result
                    }
                    Err(error) => crate::agent_isolation::denial(error.code()),
                }
            } else if call.name == "agent_attach" {
                crate::agent_enrollment::attach(db, &principal, window.key(), call.arguments)
            } else if call.name == "goal_handoff" {
                crate::agent_goal_handoff::call_host(db, &principal, window.key(), call.arguments)
            } else {
                let coding_entry = if auth.is_native_host() && call.name == "work_on_project" {
                    let parsed = match crate::tool_runtime::ToolCall::from_tool_name(
                        "work_on_project",
                        call.arguments.clone(),
                    ) {
                        Ok(parsed) => parsed,
                        Err(_) => return denied("invalid_work_on_project_arguments"),
                    };
                    if let Err(error) =
                        crate::tool_runtime::ToolRuntime::validate_work_on_project_call(&parsed)
                    {
                        let error = super::resources::mcp_isolated_browser_result(
                            &call.name,
                            browser_image,
                            error,
                        );
                        return McpOutcome::Ok(rpc_result(
                            request.id,
                            if stateless {
                                mcp_stateless_result(error, false)
                            } else {
                                error
                            },
                        ));
                    }
                    Some(parsed)
                } else {
                    None
                };
                // Bootstrap only after complete canonical input validation.
                // Explicit coding targets are mapped inside the enrollment transaction.
                // Selector-free identity/status calls retain their existing
                // sole-project selection behavior.
                let first_use = native_status
                    || (auth.is_native_host()
                        && matches!(call.name.as_str(), "agent_identity" | "agent_assignment")
                        && call
                            .arguments
                            .as_object()
                            .is_some_and(|args| args.is_empty()));
                let binding = if let Some(crate::tool_runtime::ToolCall::WorkOnProject {
                    project,
                    client_id,
                    path,
                    ..
                }) = coding_entry
                {
                    match auth.native_host.as_ref() {
                        Some(host) => db.ensure_native_host_coding_window(
                            host,
                            window.key(),
                            &project,
                            client_id.as_deref(),
                            path.as_deref(),
                            chrono::Utc::now().timestamp_millis(),
                        ),
                        None => return denied("agent_host_authorization_required"),
                    }
                } else if first_use {
                    match auth.native_host.as_ref() {
                        Some(host) => db.ensure_native_host_window(
                            host,
                            window.key(),
                            chrono::Utc::now().timestamp_millis(),
                        ),
                        None => return denied("agent_host_authorization_required"),
                    }
                } else {
                    db.resolve_agent_host_window(
                        &principal,
                        window.key(),
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .and_then(|binding| match binding {
                        Some(AgentHostWindowBinding::Active(binding)) => Ok(binding),
                        _ => Err(webcodex_store::AgentIsolationError::new(
                            "agent_host_window_not_active",
                        )),
                    })
                };
                match binding {
                    Ok(binding) => {
                        let connection =
                            native_status.then(|| connection_status(db, &binding).output);
                        let mut effective = AuthContext::new(AuthKind::ModelAgent);
                        effective.agent_run = Some(binding);
                        let mut result = runtime
                            .isolated_model_call_async(&call.name, call.arguments, Some(&effective))
                            .await;
                        // Preserve normal Runner/build/job observability. The
                        // Agent connection facts supplement it instead of
                        // replacing runtime_status with an identity-only view.
                        if result.success {
                            if let (Some(output), Some(Value::Object(connection))) =
                                (result.output.as_object_mut(), connection)
                            {
                                for (name, value) in connection {
                                    output.entry(name).or_insert(value);
                                }
                            }
                        }
                        result
                    }
                    Err(error) if error.code() == "workspace_selection_required" => ToolResult::ok(
                        json!({"connected":true,"agent_state":"project_selection_required","state_changed":false,"next_action":"Call agent_projects. Ask the user which named project to use, then call agent_select_project with its exact id. No project has been selected and no task has started."}),
                    ),
                    Err(error)
                        if native_status && error.code() == "agent_host_enrollment_required" =>
                    {
                        ToolResult::ok(json!({
                            "connected":true,"scope":"current_window","runtime_exposure":"isolated_agent",
                            "agent_state":"enrollment_required","shared_runtime_access":false,
                            "next_action":"The operator must enable automatic enrollment for this host, or supply an exact agent_attach enrollment grant."
                        }))
                    }
                    Err(error) => {
                        let mut result = crate::agent_isolation::denial(error.code());
                        if auth.is_native_host() && error.code() == "agent_host_window_not_active" {
                            result.output["recovery"] = json!("A fresh chat can call work_on_project with an authorized runtime project, or client_id + path, directly. Optional runtime_status/agent_identity can initialize the sole named project. Retired or revoked windows are never reactivated this way.");
                        }
                        result
                    }
                }
            };
            if !result.success {
                // Preserve a fixed security event without recording unknown
                // tool names, host IDs, arguments or private error payloads.
                let _ = db.record_agent_access_denied(None, "agent_host_operation_denied");
            }
            let result =
                super::resources::mcp_isolated_browser_result(&call.name, browser_image, result);
            McpOutcome::Ok(rpc_result(
                request.id,
                if stateless {
                    mcp_stateless_result(result, false)
                } else {
                    result
                },
            ))
        }
        _ => denied("agent_method_not_isolated"),
    }
}
