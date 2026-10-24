//! OAuth host requests terminate in this narrow broker, never the legacy
//! shared-user tool dispatcher. Only a registered outer host window selects a
//! run. A fresh candidate needs either an exact control-plane registration or
//! the current one-time preparation grant delivered by the trusted launcher;
//! an attempt UUID by itself is correlation, never authority.
use super::response::{
    mcp_runtime_tool_result_fallback, mcp_stateless_result, rpc_error, rpc_result,
};
use super::{protocol, JsonRpcRequest, McpOutcome, McpProtocolEra};
use crate::auth::{AuthContext, AuthKind};
use crate::tool_runtime::ToolRuntime;
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
        McpOutcome::Ok(rpc_error(request.id.clone(), -32003, code))
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
            // grants nothing: every call below still requires an active run,
            // except the explicitly scoped handoff preparation protocol.
            let specs = crate::agent_isolation::tool_specs();
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
                let catalog = crate::agent_isolation::tool_specs();
                let exact_native_contract = auth.is_native_host()
                    && inner.tool == "tool_manifest"
                    && contract::exact(&catalog, &inner.arguments).is_ok();
                if !inner.arguments.is_object()
                    || (!exact_native_contract
                        && !catalog["tools"].as_array().is_some_and(|specs| {
                            specs.iter().any(|spec| spec["name"] == inner.tool)
                        }))
                {
                    return denied("agent_tool_not_isolated");
                }
                call.name = inner.tool;
                call.arguments = inner.arguments;
            }
            // The verified native host already passed runtime:read. Cached
            // connectors need exact public contracts before prepare can bind
            // a Pending window, and again after commit for other isolated tools.
            // This branch never resolves a run or enters the legacy manifest.
            let native_contract = auth.is_native_host() && call.name == "tool_manifest";
            if !native_contract
                && !auth.has_scope(crate::agent_host_auth::tool_scope(
                    &call.name,
                    &call.arguments,
                ))
            {
                return denied("agent_host_scope_denied");
            }
            let result = if native_contract {
                match contract::exact(&crate::agent_isolation::tool_specs(), &call.arguments) {
                    Ok(metadata) => crate::tool_runtime::ToolResult::ok(metadata),
                    Err(code) => crate::agent_isolation::denial(code),
                }
            } else if call.name == "agent_attach" {
                crate::agent_enrollment::attach(db, &principal, window.key(), call.arguments)
            } else if call.name == "goal_handoff" {
                crate::agent_goal_handoff::call_host(db, &principal, window.key(), call.arguments)
            } else {
                match db.resolve_agent_host_window(
                    &principal,
                    window.key(),
                    chrono::Utc::now().timestamp_millis(),
                ) {
                    Ok(Some(AgentHostWindowBinding::Active(binding))) => {
                        let mut effective = AuthContext::new(AuthKind::ModelAgent);
                        effective.agent_run = Some(binding);
                        runtime
                            .isolated_model_call_async(&call.name, call.arguments, Some(&effective))
                            .await
                    }
                    _ => crate::agent_isolation::denial("agent_host_window_not_active"),
                }
            };
            if !result.success {
                // Preserve a fixed security event without recording unknown
                // tool names, host IDs, arguments or private error payloads.
                let _ = db.record_agent_access_denied(None, "agent_host_operation_denied");
            }
            let result = mcp_runtime_tool_result_fallback(result);
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
