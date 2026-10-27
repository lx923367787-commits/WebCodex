//! Narrow desktop bridge to project-scoped team tools on one saved Runner.
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use webcodex_admin::ServerHttpOptions;

use super::http_post_json_status;

const INPUT_LIMIT: usize = 64 * 1024;
const CONFIG_LIMIT: usize = 1024 * 1024;
const TOKEN_LIMIT: usize = 16 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const INVALID_INPUT: &str =
    "office_invalid_input: Expected bounded JSON parameters for the selected project";
const CONNECTION_CHANGED: &str =
    "office_connection_changed: Refresh the saved Runner connection before continuing";
const UNAUTHORIZED: &str = "office_unauthorized: A saved user API token is required";
const INDETERMINATE: &str =
    "office_operation_indeterminate: The request outcome is unknown; observe before retrying";
const INVALID_RESPONSE: &str =
    "office_invalid_response: Server returned an invalid tool response; observe before retrying";

pub(crate) const USAGE: &str = "Usage: webcodex office ACTION --config PATH --server-url URL --client-id ID --token-file PATH --json\n\nActions: policy-get, policy-save, plan-list, plan-read, task-create\n\nReads one JSON parameter object (at most 64 KiB) from stdin. The project must belong to the exact saved Runner. This human configuration/read bridge uses only the saved user API token; it cannot dispatch Agents or CLI jobs. Tool results, including business errors, are returned intact as JSON. Requests are never automatically retried.\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    PolicyGet,
    PolicySave,
    PlanList,
    PlanRead,
    TaskCreate,
}

impl Action {
    fn parse(action: &str) -> Option<Self> {
        Some(match action {
            "policy-get" => Self::PolicyGet,
            "policy-save" => Self::PolicySave,
            "plan-list" => Self::PlanList,
            "plan-read" => Self::PlanRead,
            "task-create" => Self::TaskCreate,
            _ => return None,
        })
    }

    fn action(self) -> &'static str {
        match self {
            Self::PolicyGet => "policy-get",
            Self::PolicySave => "policy-save",
            Self::PlanList => "plan-list",
            Self::PlanRead => "plan-read",
            Self::TaskCreate => "task-create",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    action: Action,
    config: PathBuf,
    server_url: String,
    client_id: String,
    token_file: PathBuf,
}

pub(crate) fn parse(args: &[String]) -> Result<Options, String> {
    let action = args
        .first()
        .and_then(|value| Action::parse(value))
        .ok_or(INVALID_INPUT)?;
    let mut fields = std::collections::BTreeMap::new();
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        let key = args[index].as_str();
        if key == "--json" {
            if json {
                return Err(INVALID_INPUT.into());
            }
            json = true;
            index += 1;
            continue;
        }
        if !matches!(
            key,
            "--config" | "--server-url" | "--client-id" | "--token-file"
        ) {
            return Err(INVALID_INPUT.into());
        }
        let value = args
            .get(index + 1)
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or(INVALID_INPUT)?;
        if fields.insert(key, value.clone()).is_some() {
            return Err(INVALID_INPUT.into());
        }
        index += 2;
    }
    if !json {
        return Err(INVALID_INPUT.into());
    }
    let mut required = |key| fields.remove(key).ok_or_else(|| INVALID_INPUT.to_string());
    Ok(Options {
        action,
        config: required("--config")?.into(),
        server_url: required("--server-url")?,
        client_id: required("--client-id")?,
        token_file: required("--token-file")?.into(),
    })
}

fn read_bounded(reader: impl Read, limit: usize, error: &'static str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| error)?;
    if bytes.len() > limit {
        return Err(error.into());
    }
    Ok(bytes)
}

fn read_file(path: &Path, limit: usize, error: &'static str) -> Result<Vec<u8>, String> {
    read_bounded(std::fs::File::open(path).map_err(|_| error)?, limit, error)
}

pub(crate) fn read_input(reader: impl Read) -> Result<Value, String> {
    let bytes = read_bounded(reader, INPUT_LIMIT, INVALID_INPUT)?;
    let input: Value = serde_json::from_slice(&bytes).map_err(|_| INVALID_INPUT)?;
    if !input.is_object() {
        return Err(INVALID_INPUT.into());
    }
    Ok(input)
}

#[derive(Deserialize)]
struct Connection {
    server_url: String,
    client_id: String,
}

fn validate_connection(
    opts: &Options,
    connection: &Connection,
) -> Result<ServerHttpOptions, String> {
    if opts.client_id.is_empty()
        || opts.client_id != connection.client_id
        || opts.server_url.trim_end_matches('/') != connection.server_url.trim_end_matches('/')
    {
        return Err(CONNECTION_CHANGED.into());
    }
    let url = url::Url::parse(&opts.server_url).map_err(|_| CONNECTION_CHANGED)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CONNECTION_CHANGED.into());
    }
    Ok(ServerHttpOptions {
        no_system_proxy: url.host_str().is_some_and(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }),
        ..Default::default()
    })
}

fn prepare_params(opts: &Options, mut params: Value) -> Result<Value, String> {
    let object = params.as_object_mut().ok_or(INVALID_INPUT)?;
    let project = object
        .get("project")
        .and_then(Value::as_str)
        .ok_or(INVALID_INPUT)?;
    let prefix = format!("agent:{}:", opts.client_id);
    if !project.starts_with(&prefix)
        || project.len() <= prefix.len()
        || project.len() > 256
        || project.chars().any(char::is_control)
        || project.trim() != project
    {
        return Err(CONNECTION_CHANGED.into());
    }
    if object.contains_key("action") {
        return Err(INVALID_INPUT.into());
    }
    object.insert("action".into(), json!(opts.action.action()));
    Ok(params)
}

fn render_response(status: u16, response: Option<Value>, token: &str) -> Result<String, String> {
    if status == 401 {
        return Err(UNAUTHORIZED.into());
    }
    if status == 403 {
        return Err("office_forbidden: The saved user lacks access to this operation".into());
    }
    let body = response.ok_or(INVALID_RESPONSE)?;
    if body.get("success").and_then(Value::as_bool).is_none()
        || body.get("output").is_none()
        || body
            .get("error")
            .is_some_and(|error| !error.is_null() && !error.is_string())
        || (!(200..300).contains(&status) && body["success"] == true)
    {
        return Err(INVALID_RESPONSE.into());
    }
    let output = body.to_string();
    if output.contains(token) {
        return Err(INVALID_RESPONSE.into());
    }
    // A valid ToolResult is a completed transport, including success:false.
    // Preserve the complete business result for native admission/recovery UI.
    Ok(output)
}

pub(crate) async fn run(opts: Options, params: Value) -> Result<String, String> {
    let contents = read_file(&opts.config, CONFIG_LIMIT, CONNECTION_CHANGED)?;
    let contents = std::str::from_utf8(&contents).map_err(|_| CONNECTION_CHANGED)?;
    let connection: Connection = toml::from_str(contents).map_err(|_| CONNECTION_CHANGED)?;
    let server_http = validate_connection(&opts, &connection)?;
    let params = prepare_params(&opts, params)?;
    let token = read_file(&opts.token_file, TOKEN_LIMIT, UNAUTHORIZED)?;
    let token = std::str::from_utf8(&token)
        .map_err(|_| UNAUTHORIZED)?
        .trim();
    // Unlike setup/admin paths, this bridge never accepts a bootstrap, Runner,
    // shared key, inline token, or environment fallback credential.
    if !token.starts_with("wc_pat_")
        || token.len() <= "wc_pat_".len()
        || token.chars().any(char::is_whitespace)
    {
        return Err(UNAUTHORIZED.into());
    }
    // Exactly one send. A dropped response may follow a committed mutation.
    let (status, _, body) = tokio::time::timeout(
        REQUEST_TIMEOUT,
        http_post_json_status(
            &opts.server_url,
            &server_http,
            "/api/office/control",
            Some(token),
            params,
        ),
    )
    .await
    .map_err(|_| INDETERMINATE)?
    .map_err(|_| INDETERMINATE)?;
    render_response(status, body, token)
}

#[cfg(test)]
#[path = "tests/office.rs"]
mod tests;
