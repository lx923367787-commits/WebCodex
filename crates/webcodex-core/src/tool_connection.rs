//! Persisted, Runner-owned connections to the user's local tools.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TOOL_CONNECTION_CAPABILITY: &str = "tool_connections_v1";
pub const TOOL_CONNECTION_KIND: &str = "tool_connection";
pub const TOOL_CONNECTION_MAX_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionAction {
    Connect,
    Update,
    Remove,
    Status,
    Reconnect,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    #[default]
    Mcp,
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolConnectionRequest {
    pub action: ConnectionAction,
    #[serde(default)]
    pub kind: ConnectionKind,
    #[serde(default)]
    pub id: Option<String>,
    /// Absolute program/script/config path, or a local HTTP MCP endpoint.
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl ToolConnectionRequest {
    pub fn validate(&self) -> Result<(), String> {
        if serde_json::to_vec(self)
            .map_err(|_| "invalid connection request")?
            .len()
            > TOOL_CONNECTION_MAX_BYTES
        {
            return Err("connection request is too large".into());
        }
        if let Some(id) = &self.id {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            {
                return Err(
                    "id must contain 1..64 letters, numbers, dots, dashes or underscores".into(),
                );
            }
        }
        match self.action {
            ConnectionAction::Connect | ConnectionAction::Update => {
                if self.id.is_none() || self.entry.as_ref().is_none_or(|s| s.trim().is_empty()) {
                    return Err("connect/update require id and entry".into());
                }
            }
            ConnectionAction::Remove | ConnectionAction::Reconnect if self.id.is_none() => {
                return Err("remove/reconnect require id".into())
            }
            _ => {}
        }
        if !matches!(
            self.action,
            ConnectionAction::Connect | ConnectionAction::Update
        ) && (self.entry.is_some()
            || !self.args.is_empty()
            || self.cwd.is_some()
            || !self.env.is_empty()
            || !self.headers.is_empty())
        {
            return Err("entry/args/cwd/env/headers are only used by connect/update".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConnectionStatus {
    pub id: String,
    pub kind: ConnectionKind,
    pub state: String,
    pub tool_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConnectionResponse {
    pub success: bool,
    pub changed: bool,
    pub message: String,
    pub connections: Vec<ToolConnectionStatus>,
    /// Internal synchronization fields. The model receives only the safe view above.
    pub revision: u64,
    pub inventory: Vec<crate::mcp_gateway::McpGatewayProvider>,
}
