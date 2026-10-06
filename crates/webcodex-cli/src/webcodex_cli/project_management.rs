//! Operator project controls for one exact saved Runner connection.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use webcodex_admin::ServerHttpOptions;

use super::{http_post_json_status, read_optional_token};

pub(crate) const USAGE: &str = "Usage: webcodex project <list|enable|disable|unregister> --config PATH --server-url URL --client-id ID --token-file PATH [OPTIONS]\n\nList or manage projects on one exact saved Runner. Mutations require an admin credential and preserve all source files.\n\nOptions:\n  --project ID             Exact runtime project id (mutations only)\n  --expected-revision SHA  Revision from project list (mutations only)\n  --idempotency-key KEY    Reuse identical input/key after an uncertain mutation\n  --json                   Machine-readable output\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    pub action: String,
    pub config: PathBuf,
    pub server_url: String,
    pub client_id: String,
    pub token_file: PathBuf,
    pub project: Option<String>,
    pub expected_revision: Option<String>,
    pub idempotency_key: Option<String>,
}

pub(crate) fn parse(args: &[String]) -> Result<Options, String> {
    let action = args
        .first()
        .filter(|s| matches!(s.as_str(), "list" | "enable" | "disable" | "unregister"))
        .ok_or("invalid project management action")?
        .clone();
    let mut fields = std::collections::BTreeMap::new();
    let mut index = 1;
    while index < args.len() {
        let key = args[index].as_str();
        if key == "--json" {
            index += 1;
            continue;
        }
        if !matches!(
            key,
            "--config"
                | "--server-url"
                | "--client-id"
                | "--token-file"
                | "--project"
                | "--expected-revision"
                | "--idempotency-key"
        ) {
            return Err("unknown project management option".into());
        }
        let value = args
            .get(index + 1)
            .filter(|v| !v.is_empty() && !v.starts_with("--"))
            .ok_or_else(|| format!("{key} requires a value"))?;
        if fields.insert(key, value.clone()).is_some() {
            return Err(format!("duplicate option: {key}"));
        }
        index += 2;
    }
    let mut required = |key| {
        fields
            .remove(key)
            .ok_or_else(|| format!("project {action} requires {key}"))
    };
    let mut result = Options {
        action: action.clone(),
        config: required("--config")?.into(),
        server_url: required("--server-url")?,
        client_id: required("--client-id")?,
        token_file: required("--token-file")?.into(),
        project: None,
        expected_revision: None,
        idempotency_key: None,
    };
    if action != "list" {
        result.project = Some(required("--project")?);
        result.expected_revision = Some(required("--expected-revision")?);
        result.idempotency_key = Some(required("--idempotency-key")?);
    }
    if !fields.is_empty() {
        return Err("project list does not accept mutation arguments".into());
    }
    Ok(result)
}

#[derive(Deserialize)]
struct Connection {
    server_url: String,
    client_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Project {
    id: String,
    name: String,
    path: String,
    revision: String,
    enabled: bool,
    connected: bool,
    agent_status: String,
    active_jobs: u64,
}

fn validate_connection(opts: &Options, connection: &Connection) -> Result<(), String> {
    if opts.client_id != connection.client_id
        || opts.server_url.trim_end_matches('/') != connection.server_url.trim_end_matches('/')
    {
        return Err(
            "project_connection_changed: Saved Runner identity changed; refresh before continuing"
                .into(),
        );
    }
    if opts
        .project
        .as_ref()
        .is_some_and(|id| !id.starts_with(&format!("agent:{}:", opts.client_id)))
    {
        return Err("project_connection_changed: Project belongs to another Runner".into());
    }
    Ok(())
}

fn response_error(status: u16, body: &Value) -> String {
    let code = match status {
        401 => "unauthorized",
        403 => "forbidden",
        _ => match body.pointer("/error/code").and_then(Value::as_str) {
            Some(
                code @ ("revision_conflict"
                | "active_jobs_conflict"
                | "idempotency_conflict"
                | "unsupported_runner_version"
                | "agent_unavailable"
                | "invalid_request"),
            ) => code,
            _ => "operation_indeterminate",
        },
    };
    format!("project_{code}: Refresh the project list before retrying")
}

pub(crate) async fn run(opts: Options) -> Result<String, String> {
    let contents = std::fs::read_to_string(&opts.config)
        .map_err(|_| "project_connection_changed: Cannot read saved Runner configuration")?;
    let connection: Connection = toml::from_str(&contents)
        .map_err(|_| "project_connection_changed: Invalid saved Runner configuration")?;
    validate_connection(&opts, &connection)?;
    let token = read_optional_token(&Some(opts.token_file.clone()), "--token-file")?
        .ok_or("project_unauthorized: A credential file is required")?;
    let url = url::Url::parse(&opts.server_url)
        .map_err(|_| "project_connection_changed: Invalid Server URL")?;
    let server_http = ServerHttpOptions {
        no_system_proxy: url.host_str().is_some_and(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }),
        ..Default::default()
    };
    let (path, body) = if opts.action == "list" {
        (
            "/api/projects/list".to_string(),
            json!({"client_id":opts.client_id}),
        )
    } else {
        (
            format!("/api/admin/projects/{}", opts.action),
            json!({
                "project":opts.project,"expected_revision":opts.expected_revision,
                "idempotency_key":opts.idempotency_key,"confirm":true
            }),
        )
    };
    // Do not automatically retry: transport failure can follow a committed mutation.
    let (status, _, body) =
        http_post_json_status(&opts.server_url, &server_http, &path, Some(&token), body)
            .await
            .map_err(|_| {
                "project_operation_indeterminate: Refresh the project list before retrying"
            })?;
    let body = body.ok_or("project_operation_indeterminate: Invalid Server response")?;
    if !(200..300).contains(&status) {
        return Err(response_error(status, &body));
    }
    if opts.action == "list" {
        if body["success"] != true {
            return Err(response_error(status, &body));
        }
        let projects: Vec<Project> = serde_json::from_value(body["output"]["projects"].clone())
            .map_err(|_| "project_operation_indeterminate: Invalid project inventory")?;
        if projects
            .iter()
            .any(|p| !p.id.starts_with(&format!("agent:{}:", opts.client_id)))
        {
            return Err("project_connection_changed: Inventory belongs to another Runner".into());
        }
        Ok(json!({"client_id":opts.client_id,"projects":projects}).to_string())
    } else {
        if body["project"].as_str() != opts.project.as_deref() || body["operation"] != opts.action {
            return Err("project_operation_indeterminate: Mutation receipt does not match the requested project".into());
        }
        Ok(body.to_string())
    }
}

#[cfg(test)]
#[path = "tests/project_management.rs"]
mod tests;
