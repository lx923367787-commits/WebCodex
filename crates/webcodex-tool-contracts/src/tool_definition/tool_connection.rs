use super::{
    adaptive_runtime_direct, def, model_spec, ToolDefinition, ToolVisibility::ModelVisible,
    TOOL_CATEGORY_RUNTIME,
};
use crate::metadata::{
    ToolPathHint::None as NoPath, ToolRisk::RunControl, RUNNER_MANAGE, TOOL_PROVIDER_RUNNER,
};
use serde_json::{json, Value};

fn input_schema() -> Value {
    json!({"type":"object","properties":{
        "client_id":{"type":"string","description":"Runner that owns this local tool; use the current project's Runner."},
        "action":{"type":"string","enum":["connect","update","remove","status","reconnect"]},
        "kind":{"type":"string","enum":["mcp","plugin"],"default":"mcp"},
        "id":{"type":"string","description":"Stable user-chosen connection name. Required except when listing all connections."},
        "entry":{"type":"string","description":"Absolute program, Python/JS script or MCP JSON config path; or local HTTP MCP endpoint. Only connect/update."},
        "args":{"type":"array","items":{"type":"string"}},
        "cwd":{"type":"string"},
        "env":{"type":"object","additionalProperties":{"type":"string"}},
        "headers":{"type":"object","additionalProperties":{"type":"string"}}
    },"required":["client_id","action"],"additionalProperties":false})
}

pub(super) const DEFINITIONS: &[ToolDefinition] = &[adaptive_runtime_direct(model_spec(
    def("tool_connection", super::ToolAuditPolicy::TYPED_CANONICAL, ModelVisible,
        TOOL_CATEGORY_RUNTIME, None, TOOL_PROVIDER_RUNNER,
        super::ToolSemanticContract { effect: super::ToolEffect::Mutate, risk: RunControl,
            approval: super::ToolApprovalPolicy::None, idempotency: super::ToolIdempotency::NonIdempotent },
        Some(RUNNER_MANAGE), false, NoPath, false, false, super::ToolSessionEvidencePolicy::NONE),
    "Connect a user-owned local tool once and persist it on its Runner. connect validates and reuses an identical connection; update validates the new path before replacing only this connection. remove forgets the connection without deleting tool files; status lists saved connections; reconnect repairs one connection. MCP and native plugins are called through the existing mcp_tool/plugin_tool after connection. No repeated registration is needed across chats or restarts. If a management response is lost, inspect status before repeating it.",
    input_schema), 27)];
