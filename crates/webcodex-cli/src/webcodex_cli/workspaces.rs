//! Finite, loopback-only human management bridge. Never callable by an Agent.
use serde_json::{json, Value};
use std::{io::Read, path::PathBuf, time::Duration};
use webcodex_admin::ServerHttpOptions;

const INVALID: &str = "workspace_invalid_input";
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    server: String,
    token: PathBuf,
}
pub(crate) fn parse(args: &[String]) -> Result<Options, String> {
    if args.len() != 5
        || args[0] != "--server-url"
        || args[2] != "--token-file"
        || args[4] != "--json"
    {
        return Err(INVALID.into());
    }
    let url = url::Url::parse(&args[1]).map_err(|_| INVALID)?;
    if url.scheme() != "http"
        || !url.host_str().is_some_and(|h| {
            h.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        })
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(INVALID.into());
    }
    Ok(Options {
        server: args[1].clone(),
        token: args[3].clone().into(),
    })
}
pub(crate) fn read_input(reader: impl Read) -> Result<Value, String> {
    let mut bytes = vec![];
    reader
        .take(131073)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() > 131072 {
        return Err(INVALID.into());
    }
    serde_json::from_slice(&bytes).map_err(|_| INVALID.into())
}
pub(crate) async fn run(opts: Options, params: Value) -> Result<String, String> {
    let object = params.as_object().ok_or(INVALID)?;
    let keys: &[&str] = match params["action"].as_str() {
        Some("native_workspaces_read") => &["action", "host_id"],
        Some("native_workspaces_save") => &[
            "action",
            "host_id",
            "owner_user",
            "expected_revision",
            "projects",
        ],
        _ => return Err(INVALID.into()),
    };
    if object.len() != keys.len() || object.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err(INVALID.into());
    }
    let mut bytes = vec![];
    std::fs::File::open(opts.token)
        .map_err(|_| "workspace_credential_unavailable")?
        .take(16385)
        .read_to_end(&mut bytes)
        .map_err(|_| "workspace_credential_unavailable")?;
    if bytes.len() > 16384 {
        return Err("workspace_credential_unavailable".into());
    }
    let token = std::str::from_utf8(&bytes).map_err(|_| INVALID)?.trim();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err("workspace_credential_unavailable".into());
    }
    let (status, _, body) = tokio::time::timeout(
        Duration::from_secs(30),
        super::http_post_json_status(
            &opts.server,
            &ServerHttpOptions {
                no_system_proxy: true,
                ..Default::default()
            },
            "/api/admin/agent-isolation",
            Some(token),
            params,
        ),
    )
    .await
    .map_err(|_| "workspace_operation_indeterminate")?
    .map_err(|_| "workspace_operation_indeterminate")?;
    let body = body.ok_or("workspace_invalid_response")?;
    if !(200..300).contains(&status) {
        // Preserve only the finite server error code, never an arbitrary response body.
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .unwrap_or("workspace_rejected");
        return Ok(json!({"success":false,"output":{"error_kind":code}}).to_string());
    }
    if !body["revision"].is_i64() || !body["projects"].is_array() {
        return Err("workspace_invalid_response".into());
    }
    let result = json!({"success":true,"output":body}).to_string();
    if result.contains(token) {
        return Err("workspace_invalid_response".into());
    }
    Ok(result)
}

#[cfg(test)]
#[path = "tests/workspaces.rs"]
mod tests;
