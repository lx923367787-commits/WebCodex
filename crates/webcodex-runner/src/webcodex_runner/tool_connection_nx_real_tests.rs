//! Explicit, read-only NX acceptance through the real Runner connection path.
//! Never starts the desktop/host carrier or edits an existing Runner/NX project.
use super::*;
use serde_json::json;
use std::fs::OpenOptions;
use std::path::PathBuf;
use webcodex_core::mcp_gateway::{
    McpGatewayContent, McpGatewayProvider, McpGatewayRequest, McpGatewayResponse,
    McpGatewayResponsePayload, McpGatewayTool,
};

const PROVIDER_ID: &str = "nx-readonly-acceptance";

fn operation(action: Action, entry: Option<&Path>) -> ToolConnectionRequest {
    ToolConnectionRequest {
        action,
        kind: Kind::Mcp,
        id: Some(PROVIDER_ID.into()),
        entry: entry.map(|path| path.to_string_lossy().into_owned()),
        args: vec![],
        cwd: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
    }
}

fn check(report: &mut Value, name: &'static str, passed: bool) -> Result<(), &'static str> {
    report["checks"][name] = json!(passed);
    if passed {
        Ok(())
    } else {
        Err(name)
    }
}

fn provider(runtime: &ReloadableRunnerConfig) -> Result<McpGatewayProvider, &'static str> {
    runtime
        .mcp_gateway()
        .provider_inventory()
        .into_iter()
        .find(|entry| entry.provider_id == PROVIDER_ID)
        .ok_or("provider_not_published")
}

fn payload(
    response: McpGatewayResponse,
    report: &mut Value,
) -> Result<McpGatewayResponsePayload, &'static str> {
    if let Some(error) = response.error {
        // Gateway-owned codes only. Never copy downstream messages or results.
        if error.code.len() <= 96
            && error
                .code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            report["gateway_error_code"] = json!(error.code);
        }
        return Err("gateway_request_failed");
    }
    response.payload.ok_or("gateway_payload_missing")
}

fn list_tools(
    runtime: &ReloadableRunnerConfig,
    report: &mut Value,
) -> Result<Vec<McpGatewayTool>, &'static str> {
    let current = provider(runtime)?;
    match payload(
        runtime.mcp_gateway().handle(McpGatewayRequest::ToolsList {
            provider_id: current.provider_id,
            provider_instance_id: current.provider_instance_id,
        }),
        report,
    )? {
        McpGatewayResponsePayload::Tools { tools } => Ok(tools),
        _ => Err("gateway_tools_payload_invalid"),
    }
}

fn identity(value: &Value) -> Option<Value> {
    let session = value.get("session_id")?.as_str()?;
    // NX bridge session IDs are UUIDs. Preserve only that identity and generation.
    uuid::Uuid::parse_str(session).ok()?;
    let generation = value.get("model_generation")?.as_u64()?;
    Some(json!({"session_id": session, "model_generation": generation}))
}

fn read_identity(
    runtime: &ReloadableRunnerConfig,
    tools: &[McpGatewayTool],
    name: &'static str,
    report: &mut Value,
) -> Result<Value, &'static str> {
    if !matches!(name, "nx_status" | "nx_get_workspace") {
        return Err("non_readonly_tool_forbidden");
    }
    let tool = tools
        .iter()
        .find(|tool| tool.name == name)
        .ok_or("required_nx_tool_missing")?;
    let current = provider(runtime)?;
    let result = match payload(
        runtime.mcp_gateway().handle(McpGatewayRequest::ToolsCall {
            provider_id: current.provider_id,
            provider_instance_id: current.provider_instance_id,
            name: name.into(),
            arguments: json!({}),
            expected_schema: tool.schema_observation(),
        }),
        report,
    )? {
        McpGatewayResponsePayload::ToolResult { result } if !result.is_error => result,
        _ => return Err("nx_readonly_tool_failed"),
    };
    if let Some(value) = result.structured_content.as_ref().and_then(identity) {
        return Ok(value);
    }
    for item in &result.content {
        let McpGatewayContent::Text { text } = item;
        if let Ok(value) = serde_json::from_str::<Value>(text) {
            if let Some(value) = identity(&value) {
                return Ok(value);
            }
        }
    }
    Err("nx_identity_fields_missing")
}

fn run_acceptance(report: &mut Value, report_path: &Path) -> Result<(), &'static str> {
    let python =
        PathBuf::from(std::env::var_os("WEBCODEX_NX_MCP_PYTHON").ok_or("nx_python_env_missing")?);
    let nx_root =
        PathBuf::from(std::env::var_os("WEBCODEX_NX_MCP_ROOT").ok_or("nx_root_env_missing")?);
    if !python.is_absolute() || !python.is_file() || !nx_root.is_absolute() || !nx_root.is_dir() {
        return Err("nx_entry_paths_invalid");
    }
    let nx_root = nx_root.canonicalize().map_err(|_| "nx_root_unavailable")?;
    let report_parent = report_path
        .parent()
        .ok_or("report_parent_missing")?
        .canonicalize()
        .map_err(|_| "report_parent_unavailable")?;
    if report_parent.starts_with(&nx_root) {
        return Err("report_must_be_outside_nx_project");
    }

    let temp = tempfile::tempdir().map_err(|_| "temporary_directory_failed")?;
    let path = temp.path().join("runner.toml");
    std::fs::write(&path, "# isolated NX acceptance only\nserver_url='http://127.0.0.1:1'\nclient_id='nx-readonly-acceptance'\ntoken='isolated-test-only'\n[mcp]\nrequest_timeout_secs=60\n[policy]\nallow_cwd_anywhere=true\n")
        .map_err(|_| "temporary_config_write_failed")?;
    let loaded = config::load_config(&path).map_err(|_| "temporary_config_load_failed")?;
    let runtime = ReloadableRunnerConfig::new(loaded, path.clone());
    let mut connect = operation(Action::Connect, Some(&python));
    connect.args = vec!["-m".into(), "nx_topology_mcp".into()];
    connect.cwd = Some(nx_root.to_string_lossy().into_owned());
    connect
        .env
        .insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    connect.env.insert("PYTHONUTF8".into(), "1".into());
    connect.env.insert("PYTHONUNBUFFERED".into(), "1".into());
    for name in ["TEMP", "TMP"] {
        connect
            .env
            .insert(name.into(), temp.path().to_string_lossy().into_owned());
    }
    let connected = runtime.manage_tool_connection(connect.clone());
    check(report, "connect", connected.success && connected.changed)?;
    // The gateway accepts initialize only when the returned version matches this.
    report["protocol"] =
        json!({"version":"2025-06-18", "validated_by":"Runner initialize exact-match"});
    let first = provider(&runtime)?;
    let saved = std::fs::read_to_string(&path).map_err(|_| "persisted_config_read_failed")?;
    let persisted = config::load_config(&path).map_err(|_| "persisted_config_load_failed")?;
    check(
        report,
        "persist",
        persisted.mcp_gateway.providers.len() == 1
            && persisted.mcp_gateway.providers[0].id == PROVIDER_ID
            && persisted.mcp_gateway.providers[0].args == ["-m", "nx_topology_mcp"],
    )?;
    let repeated = runtime.manage_tool_connection(connect);
    check(
        report,
        "duplicate_reuses_provider",
        repeated.success
            && !repeated.changed
            && provider(&runtime)? == first
            && std::fs::read_to_string(&path).map_err(|_| "duplicate_config_read_failed")? == saved,
    )?;

    let tools = list_tools(&runtime, report)?;
    report["tool_count"] = json!(tools.len());
    check(
        report,
        "tools_list",
        !tools.is_empty()
            && ["nx_status", "nx_get_workspace"]
                .iter()
                .all(|name| tools.iter().any(|tool| tool.name == *name)),
    )?;
    let before = read_identity(&runtime, &tools, "nx_status", report)?;
    report["nx_before"] = before.clone();
    check(report, "nx_status_before", true)?;
    let workspace = read_identity(&runtime, &tools, "nx_get_workspace", report)?;
    report["nx_workspace_identity"] = workspace.clone();
    check(report, "nx_get_workspace_readonly", workspace == before)?;

    drop(runtime);
    let restored_config = config::load_config(&path).map_err(|_| "restored_config_load_failed")?;
    let runtime = ReloadableRunnerConfig::new(restored_config, path.clone());
    let status = runtime.manage_tool_connection(operation(Action::Status, None));
    check(
        report,
        "runtime_recreated_from_persisted_config",
        status.success
            && status
                .connections
                .iter()
                .any(|connection| connection.id == PROVIDER_ID)
            && provider(&runtime)?.provider_instance_id != first.provider_instance_id,
    )?;
    let tools = list_tools(&runtime, report)?;
    report["restored_tool_count"] = json!(tools.len());
    let restored_identity = read_identity(&runtime, &tools, "nx_status", report)?;
    report["nx_after_restore"] = restored_identity.clone();
    check(
        report,
        "restored_status_readonly",
        restored_identity == before,
    )?;

    let restored_provider = provider(&runtime)?;
    let before_update = std::fs::read_to_string(&path).map_err(|_| "before_update_read_failed")?;
    let missing = temp.path().join("nonexistent-nx-python.exe");
    let failed = runtime.manage_tool_connection(operation(Action::Update, Some(&missing)));
    check(
        report,
        "failed_update_keeps_provider_and_config",
        !failed.success
            && !failed.changed
            && provider(&runtime)? == restored_provider
            && std::fs::read_to_string(&path).map_err(|_| "after_update_read_failed")?
                == before_update,
    )?;
    let after = read_identity(&runtime, &tools, "nx_status", report)?;
    report["nx_after"] = after.clone();
    check(
        report,
        "nx_session_and_generation_unchanged",
        after == before,
    )?;

    let removed = runtime.manage_tool_connection(operation(Action::Remove, None));
    check(
        report,
        "remove_only_temporary_config",
        removed.success
            && removed.changed
            && runtime.mcp_gateway().provider_inventory().is_empty()
            && config::load_config(&path)
                .map_err(|_| "removed_config_load_failed")?
                .mcp_gateway
                .providers
                .is_empty()
            && python.is_file()
            && nx_root.is_dir(),
    )?;
    drop(runtime);
    let runtime = ReloadableRunnerConfig::new(
        config::load_config(&path).map_err(|_| "final_config_load_failed")?,
        path,
    );
    check(
        report,
        "removed_connection_stays_removed",
        runtime.mcp_gateway().provider_inventory().is_empty(),
    )?;
    drop(runtime);
    Ok(())
}

#[test]
#[ignore = "Explicit real NX acceptance: requires an existing NX bridge and WEBCODEX_NX_MCP_PYTHON, WEBCODEX_NX_MCP_ROOT, WEBCODEX_NX_ACCEPTANCE_REPORT; only nx_status/nx_get_workspace"]
fn runner_real_process_nx_tool_connection_readonly_acceptance() {
    let report_path = PathBuf::from(
        std::env::var_os("WEBCODEX_NX_ACCEPTANCE_REPORT")
            .expect("WEBCODEX_NX_ACCEPTANCE_REPORT must name a new absolute JSON file"),
    );
    assert!(
        report_path.is_absolute(),
        "NX acceptance report path must be absolute"
    );
    let nx_root = PathBuf::from(
        std::env::var_os("WEBCODEX_NX_MCP_ROOT").expect("WEBCODEX_NX_MCP_ROOT is required"),
    )
    .canonicalize()
    .expect("NX MCP project root must exist");
    let parent = report_path
        .parent()
        .expect("report parent is required")
        .canonicalize()
        .expect("report parent must already exist");
    assert!(
        !parent.starts_with(&nx_root),
        "acceptance reports must remain outside the NX project"
    );
    // Do not overwrite prior evidence. The parent directory must already exist.
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&report_path)
        .expect("cannot create a new NX acceptance report");
    let mut report = json!({
        "schema":"webcodex_nx_readonly_acceptance/1",
        "passed":false,
        "scope":"isolated temporary Runner configuration; existing NX bridge",
        "allowed_tool_calls":["nx_status","nx_get_workspace"],
        "topology_saved":false,
        "checks":{}
    });
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_acceptance(&mut report, &report_path)
    }));
    let outcome = outcome.unwrap_or(Err("acceptance_panicked"));
    report["passed"] = json!(outcome.is_ok());
    if let Err(code) = outcome {
        report["failure_code"] = json!(code);
    }
    serde_json::to_writer_pretty(&mut output, &report).expect("cannot serialize safe NX evidence");
    output
        .write_all(b"\n")
        .and_then(|_| output.sync_all())
        .expect("cannot save NX acceptance evidence");
    assert!(
        outcome.is_ok(),
        "read-only NX acceptance failed; inspect the sanitized JSON report"
    );
}
