use super::*;
use webcodex_core::mcp_gateway::{McpGatewayRequest, McpGatewayResponsePayload};

fn request(action: Action, id: &str, entry: Option<&Path>) -> ToolConnectionRequest {
    ToolConnectionRequest {
        action,
        kind: Kind::Mcp,
        id: Some(id.into()),
        entry: entry.map(|v| v.to_string_lossy().into_owned()),
        args: vec![],
        cwd: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
    }
}

#[test]
fn connection_config_edit_preserves_unrelated_values_and_comments() {
    let mut doc:DocumentMut="# keep this comment\nserver_url='http://localhost:1'\n[mcp]\nrequest_timeout_secs=17\n[[mcp.providers]]\nid='other'\nname='other'\nexecutable='/kept/tool'\n".parse().unwrap();
    let mut table = Table::new();
    table["id"] = toml_edit::value("new");
    table["name"] = toml_edit::value("new");
    replace_provider(&mut doc, "mcp", "new", table).unwrap();
    assert_eq!(
        find_provider(&doc, "mcp", "other").unwrap()["executable"].as_str(),
        Some("/kept/tool")
    );
    assert!(doc.to_string().starts_with("# keep this comment"));
    assert!(remove_provider(&mut doc, "mcp", "new"));
    assert!(!remove_provider(&mut doc, "mcp", "new"));
    assert_eq!(doc["mcp"]["request_timeout_secs"].as_integer(), Some(17));
}

#[test]
fn connection_json_import_selects_one_server_and_keeps_parameters() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("mcp.json");
    std::fs::write(&file,r#"{"mcpServers":{"one":{"command":"python","args":["server.py"],"cwd":"work","env":{"TEST":"value"}},"two":{"url":"http://localhost:8123/mcp"}}}"#).unwrap();
    assert!(read_entry_config(&file, "missing").is_err());
    let result = read_entry_config(&file, "one").unwrap();
    assert_eq!(result.args, vec!["server.py"]);
    assert_eq!(
        result.cwd,
        Some(temp.path().join("work").to_string_lossy().into_owned())
    );
    assert_eq!(result.env["TEST"], "value");
    assert_eq!(
        read_entry_config(&file, "two").unwrap().url.as_deref(),
        Some("http://localhost:8123/mcp")
    );
}

#[test]
fn connection_save_rejects_changed_file_without_overwriting_it() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("runner.toml");
    std::fs::write(&file, "newer").unwrap();
    assert!(save_config(&file, "older", "replacement").is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "newer");
}

#[test]
#[ignore = "Starts local fixture processes to verify persisted connections end to end"]
fn runner_real_process_tool_connection_connect_update_restore_remove() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("runner.toml");
    std::fs::write(&config,"# keep\nserver_url='http://127.0.0.1:1'\nclient_id='connection-test'\ntoken='test-only'\n[policy]\nallow_cwd_anywhere=true\n").unwrap();
    let binary = temp
        .path()
        .join(format!("connector{}", std::env::consts::EXE_SUFFIX));
    let output =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/webcodex_runner/fake_mcp_gateway.rs"),
            )
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime =
        ReloadableRunnerConfig::new(config::load_config(&config).unwrap(), config.clone());
    let connected = runtime.manage_tool_connection(request(Action::Connect, "mine", Some(&binary)));
    assert!(connected.success, "{}", connected.message);
    assert!(connected.changed);
    let first = connected.inventory[0].clone();
    let repeated = runtime.manage_tool_connection(request(Action::Connect, "mine", Some(&binary)));
    assert!(repeated.success, "{}", repeated.message);
    assert!(!repeated.changed);
    assert_eq!(repeated.inventory[0], first);
    let listed = runtime.mcp_gateway().handle(McpGatewayRequest::ToolsList {
        provider_id: first.provider_id.clone(),
        provider_instance_id: first.provider_instance_id.clone(),
    });
    let Some(McpGatewayResponsePayload::Tools { tools }) = listed.payload else {
        panic!("{listed:?}")
    };
    let result = runtime.mcp_gateway().handle(McpGatewayRequest::ToolsCall {
        provider_id: first.provider_id.clone(),
        provider_instance_id: first.provider_instance_id.clone(),
        name: tools[0].name.clone(),
        arguments: serde_json::json!({"value":"hello"}),
        expected_schema: tools[0].schema_observation(),
    });
    assert!(result.error.is_none(), "{result:?}");
    let saved = std::fs::read_to_string(&config).unwrap();
    let bad = runtime.manage_tool_connection(request(
        Action::Update,
        "mine",
        Some(&temp.path().join("missing.exe")),
    ));
    assert!(!bad.success);
    assert_eq!(bad.inventory[0], first);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), saved);
    let alias =
        runtime.manage_tool_connection(request(Action::Connect, "another-name", Some(&binary)));
    assert!(alias.success, "{}", alias.message);
    assert!(!alias.changed);
    assert_eq!(alias.connections[0].id, "mine");
    assert_eq!(alias.inventory.len(), 1);
    let mut other_request = request(Action::Connect, "other", Some(&binary));
    other_request.args = vec!["normal".into()];
    let other = runtime.manage_tool_connection(other_request);
    assert!(other.success);
    let other_id = other
        .inventory
        .iter()
        .find(|p| p.provider_id == "other")
        .unwrap()
        .provider_instance_id
        .clone();
    let updated_directory = temp.path().join("version-two");
    std::fs::create_dir(&updated_directory).unwrap();
    let updated_binary =
        updated_directory.join(format!("connector-v2{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&binary, &updated_binary).unwrap();
    let updated =
        runtime.manage_tool_connection(request(Action::Update, "mine", Some(&updated_binary)));
    assert!(updated.success, "{}", updated.message);
    assert_eq!(
        config::load_config(&config)
            .unwrap()
            .mcp_gateway
            .providers
            .iter()
            .find(|p| p.id == "mine")
            .unwrap()
            .cwd
            .as_deref(),
        Some(updated_directory.to_string_lossy().as_ref())
    );
    assert_ne!(
        updated
            .inventory
            .iter()
            .find(|p| p.provider_id == "mine")
            .unwrap()
            .provider_instance_id,
        first.provider_instance_id
    );
    assert_eq!(
        updated
            .inventory
            .iter()
            .find(|p| p.provider_id == "other")
            .unwrap()
            .provider_instance_id,
        other_id
    );
    drop(runtime);
    let runtime =
        ReloadableRunnerConfig::new(config::load_config(&config).unwrap(), config.clone());
    let restored = runtime.manage_tool_connection(request(Action::Status, "mine", None));
    assert_eq!(restored.connections.len(), 1);
    let removed = runtime.manage_tool_connection(request(Action::Remove, "mine", None));
    assert!(removed.success);
    assert!(removed.changed);
    assert!(binary.exists());
    assert!(updated_binary.exists());
    drop(runtime);
    let runtime =
        ReloadableRunnerConfig::new(config::load_config(&config).unwrap(), config.clone());
    assert!(!runtime
        .mcp_gateway()
        .provider_inventory()
        .iter()
        .any(|p| p.provider_id == "mine"));
    assert!(std::fs::read_to_string(config)
        .unwrap()
        .starts_with("# keep"));
}

#[test]
fn connection_update_keeps_existing_launch_options() {
    let temp = tempfile::tempdir().unwrap();
    let new = temp.path().join("new.exe");
    std::fs::write(&new, "fixture").unwrap();
    let old: DocumentMut = r#"id="mine"
name="mine"
executable="old.exe"
args=["--port","1234"]
timeout_secs=45
profile="local"
[env]
LOCAL_SETTING="kept"
"#
    .parse()
    .unwrap();
    let updated = connection_table(
        &request(Action::Update, "mine", Some(&new)),
        Some(old.as_table()),
    )
    .unwrap();
    assert_eq!(updated["timeout_secs"].as_integer(), Some(45));
    assert_eq!(updated["profile"].as_str(), Some("local"));
    assert_eq!(
        updated["args"].as_array().unwrap().get(1).unwrap().as_str(),
        Some("1234")
    );
    assert_eq!(updated["env"]["LOCAL_SETTING"].as_str(), Some("kept"));
}

#[test]
#[ignore = "Starts local native plugin fixture processes"]
fn runner_real_process_tool_connection_native_persistence_and_invalid_update() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("runner.toml");
    std::fs::write(&path,"server_url='http://127.0.0.1:1'\nclient_id='connection-test'\ntoken='test-only'\n[policy]\nallow_cwd_anywhere=true\n").unwrap();
    let binary = temp
        .path()
        .join(format!("plugin{}", std::env::consts::EXE_SUFFIX));
    let output =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/webcodex_runner/fake_plugin.rs"))
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = ReloadableRunnerConfig::new(config::load_config(&path).unwrap(), path.clone());
    let mut connect = request(Action::Connect, "native", Some(&binary));
    connect.kind = Kind::Plugin;
    connect.args = vec![
        "normal".into(),
        temp.path().join("marker").to_string_lossy().into_owned(),
    ];
    let response = runtime.manage_tool_connection(connect.clone());
    assert!(response.success, "{}", response.message);
    let repeated = runtime.manage_tool_connection(connect.clone());
    assert!(repeated.success);
    assert!(!repeated.changed);
    let saved = std::fs::read_to_string(&path).unwrap();
    let mut invalid = connect;
    invalid.action = Action::Update;
    invalid.cwd = Some("relative/path".into());
    assert!(!runtime.manage_tool_connection(invalid).success);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
    drop(runtime);
    let runtime = ReloadableRunnerConfig::new(config::load_config(&path).unwrap(), path.clone());
    assert_eq!(runtime.plugins().provider_views().len(), 1);
    let mut remove = request(Action::Remove, "native", None);
    remove.kind = Kind::Plugin;
    assert!(runtime.manage_tool_connection(remove).success);
    drop(runtime);
    let runtime = ReloadableRunnerConfig::new(config::load_config(&path).unwrap(), path.clone());
    assert!(runtime.plugins().provider_views().is_empty());
    assert!(binary.exists());
}

#[test]
fn connection_reusing_interpreter_keeps_script_argument() {
    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("python.exe");
    std::fs::write(&program, "fixture").unwrap();
    let mut old = Table::new();
    old["id"] = toml_edit::value("mine");
    old["name"] = toml_edit::value("mine");
    old["executable"] = toml_edit::value(program.to_string_lossy().as_ref());
    let mut args = Array::new();
    args.push("server.py");
    args.push("--flag");
    old["args"] = toml_edit::value(args);
    let updated =
        connection_table(&request(Action::Update, "mine", Some(&program)), Some(&old)).unwrap();
    assert_eq!(
        updated["args"].as_array().unwrap().get(0).unwrap().as_str(),
        Some("server.py")
    );
}

#[test]
fn connection_preserves_legacy_display_name_and_optional_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("tool.exe");
    std::fs::write(&program, "fixture").unwrap();
    let mut old = Table::new();
    old["id"] = toml_edit::value("mine");
    old["name"] = toml_edit::value("My Local Tool");
    old["executable"] = toml_edit::value(program.to_string_lossy().as_ref());
    old["args"] = toml_edit::value(Array::new());
    let table = connection_table(
        &request(Action::Connect, "mine", Some(&program)),
        Some(&old),
    )
    .unwrap();
    assert!(same_provider_config(Kind::Mcp, &old, &table));
    assert_eq!(table["name"].as_str(), Some("My Local Tool"));
    assert!(table.get("cwd").is_none());
    assert!(table.get("env_from_env").is_none());
}

#[test]
fn connection_explicit_environment_overrides_automatic_forwarding() {
    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("tool.exe");
    std::fs::write(&program, "fixture").unwrap();
    let mut connect = request(Action::Connect, "mine", Some(&program));
    connect.env.insert("PATH".into(), "custom-path".into());
    let table = connection_table(&connect, None).unwrap();
    assert!(table
        .get("env_from_env")
        .and_then(|v| v.get("PATH"))
        .is_none());
    let config: config::McpGatewayProviderConfig = toml::from_str(&table.to_string()).unwrap();
    config::validate_mcp_gateway_config(&config::McpGatewayConfig {
        request_timeout_secs: 30,
        providers: vec![config],
    })
    .unwrap();
}
