use super::config::{self, ReloadableRunnerConfig, RunnerConfig};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    sync::atomic::Ordering,
};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table};
use webcodex_core::tool_connection::{
    ConnectionAction as Action, ConnectionKind as Kind, ToolConnectionRequest,
    ToolConnectionResponse, ToolConnectionStatus,
};

impl ReloadableRunnerConfig {
    pub(crate) fn manage_tool_connection(
        &self,
        request: ToolConnectionRequest,
    ) -> ToolConnectionResponse {
        let _guard = self.reload_lock.lock().unwrap_or_else(|p| p.into_inner());
        let outcome = request
            .validate()
            .and_then(|_| self.change_tool_connection(&request));
        let (success, changed, selected_id, message) = match outcome {
            Ok((changed, selected_id)) => (
                true,
                changed,
                selected_id,
                match request.action {
                    Action::Status => "Connection status",
                    Action::Remove => "Connection removed; tool files were preserved",
                    _ if changed => "Connection saved and active",
                    _ => "Connection unchanged",
                }
                .to_string(),
            ),
            Err(error) => (false, false, request.id.clone(), error),
        };
        if changed {
            self.connection_revision.fetch_add(1, Ordering::SeqCst);
        }
        let mut connections = self.mcp_gateway().connection_status();
        connections.extend(self.plugins().provider_views().into_iter().map(|v| {
            ToolConnectionStatus {
                id: v.provider_id,
                kind: Kind::Plugin,
                state: v.status,
                tool_count: None,
                error_code: v.error_code,
            }
        }));
        if let Some(id) = &selected_id {
            connections.retain(|v| &v.id == id && v.kind == request.kind);
        }
        ToolConnectionResponse {
            success,
            changed,
            message,
            connections,
            revision: self.connection_revision.load(Ordering::SeqCst),
            inventory: self.mcp_gateway().provider_inventory(),
        }
    }

    fn change_tool_connection(
        &self,
        request: &ToolConnectionRequest,
    ) -> Result<(bool, Option<String>), String> {
        if request.action == Action::Status {
            return Ok((false, request.id.clone()));
        }
        let id = request.id.as_deref().ok_or("connection id is required")?;
        let before = std::fs::read_to_string(&self.path)
            .map_err(|_| "Cannot read Runner connection configuration")?;
        let mut document: DocumentMut = before
            .parse()
            .map_err(|_| "Runner configuration contains invalid TOML")?;
        let section = if request.kind == Kind::Mcp {
            "mcp"
        } else {
            "plugins"
        };
        let existing = find_provider(&document, section, id).cloned();
        if matches!(request.action, Action::Update | Action::Reconnect) && existing.is_none() {
            return Err("Connection does not exist; use connect first".into());
        }
        if request.action == Action::Remove {
            let removed = remove_provider(&mut document, section, id);
            if request.kind == Kind::Plugin {
                let prepared = self.plugins().prepare_remove(id)?;
                let changed = removed || prepared.changed();
                if removed {
                    save_config(&self.path, &before, &document.to_string())?;
                }
                self.plugins().commit_remove(prepared);
                return Ok((changed, Some(id.into())));
            }
            if removed {
                save_config(&self.path, &before, &document.to_string())?;
            }
            return Ok((self.mcp_gateway().remove(id) || removed, Some(id.into())));
        }
        if request.action != Action::Reconnect {
            let mut table = connection_table(request, existing.as_ref())?;
            if request.action == Action::Connect && existing.is_none() {
                let same_entry = |candidate: &Table| {
                    let normalized = |table: &Table| {
                        let mut table = table.clone();
                        table.remove("id");
                        table.remove("name");
                        table.to_string().parse::<toml::Value>().ok()
                    };
                    normalized(candidate) == normalized(&table)
                };
                if let Some(other) = document
                    .get(section)
                    .and_then(|v| v.get("providers"))
                    .and_then(Item::as_array_of_tables)
                    .and_then(|providers| providers.iter().find(|p| same_entry(p)))
                {
                    let mut reuse = request.clone();
                    reuse.id = Some(
                        other
                            .get("id")
                            .and_then(Item::as_str)
                            .ok_or("Stored connection has no id")?
                            .into(),
                    );
                    return self.change_tool_connection(&reuse);
                }
            }
            if request.action == Action::Connect {
                if let Some(old) = &existing {
                    if !same_provider_config(request.kind, old, &table) {
                        return Err(
                            "This connection name already exists; use update to replace its entry"
                                .into(),
                        );
                    }
                    table = old.clone();
                }
            }
            replace_provider(&mut document, section, id, table)?;
        }
        let candidate: RunnerConfig = config::parse_config(&document.to_string()).map_err(|_| "Connection configuration is invalid; check the entry, working directory and environment settings")?;
        let file_changed = document.to_string() != before;
        if request.kind == Kind::Mcp {
            config::validate_mcp_gateway_config(&candidate.mcp_gateway)
                .map_err(|_| "Invalid MCP connection settings")?;
            let provider = candidate
                .mcp_gateway
                .providers
                .iter()
                .find(|p| p.id == id)
                .ok_or("MCP connection missing")?;
            let prior = self
                .mcp_gateway()
                .provider_inventory()
                .into_iter()
                .find(|p| p.provider_id == id);
            let prepared = if matches!(request.action, Action::Update | Action::Reconnect) {
                self.mcp_gateway().prepare_reconnect(provider)?
            } else {
                self.mcp_gateway().prepare(provider)?
            };
            let changed = prior.as_ref() != Some(&prepared.inventory()) || file_changed;
            if file_changed {
                save_config(&self.path, &before, &document.to_string())?;
            }
            self.mcp_gateway().commit(prepared);
            Ok((changed, Some(id.into())))
        } else {
            let prepared = if matches!(request.action, Action::Update | Action::Reconnect) {
                self.plugins().prepare_reconnect(&candidate, id)?
            } else {
                self.plugins().prepare(&candidate, id)?
            };
            let changed = prepared.changed() || file_changed;
            if file_changed {
                save_config(&self.path, &before, &document.to_string())?;
            }
            self.plugins().commit(prepared);
            Ok((changed, Some(id.into())))
        }
    }
}

fn find_provider<'a>(document: &'a DocumentMut, section: &str, id: &str) -> Option<&'a Table> {
    document
        .get(section)?
        .get("providers")?
        .as_array_of_tables()?
        .iter()
        .find(|p| p.get("id").and_then(Item::as_str) == Some(id))
}

fn remove_provider(document: &mut DocumentMut, section: &str, id: &str) -> bool {
    let Some(providers) = document
        .get_mut(section)
        .and_then(|v| v.get_mut("providers"))
        .and_then(Item::as_array_of_tables_mut)
    else {
        return false;
    };
    let before = providers.len();
    providers.retain(|p| p.get("id").and_then(Item::as_str) != Some(id));
    providers.len() != before
}

fn replace_provider(
    document: &mut DocumentMut,
    section: &str,
    id: &str,
    table: Table,
) -> Result<(), String> {
    if document.get(section).is_none() {
        document[section] = Item::Table(Table::new());
    }
    let section = document[section]
        .as_table_mut()
        .ok_or("Invalid provider section")?;
    if !section.contains_key("providers") {
        section["providers"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let providers = section["providers"]
        .as_array_of_tables_mut()
        .ok_or("Providers must be an array of tables")?;
    let index = providers
        .iter()
        .position(|p| p.get("id").and_then(Item::as_str) == Some(id));
    if let Some(index) = index {
        *providers.get_mut(index).expect("provider index") = table;
    } else {
        providers.push(table);
    }
    Ok(())
}

/// Save only after admission. Keep comments/unrelated fields, and refuse a concurrent edit.
fn save_config(path: &Path, before: &str, after: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).map_err(|_| "Cannot re-read Runner configuration")? != before {
        return Err(
            "Runner configuration changed while connecting; retry with the current configuration"
                .into(),
        );
    }
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or("Runner configuration has no directory")?,
    )
    .map_err(|_| "Cannot prepare connection configuration")?;
    if let Ok(metadata) = std::fs::metadata(path) {
        std::fs::set_permissions(file.path(), metadata.permissions())
            .map_err(|_| "Cannot preserve configuration permissions")?;
    }
    file.write_all(after.as_bytes())
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| "Cannot save connection configuration")?;
    if std::fs::read_to_string(path).map_err(|_| "Cannot re-read Runner configuration")? != before {
        return Err(
            "Runner configuration changed while connecting; retry with the current configuration"
                .into(),
        );
    }
    file.persist(path)
        .map_err(|_| "Cannot replace connection configuration")?;
    Ok(())
}

#[derive(Default)]
struct Entry {
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    url: Option<String>,
    headers: BTreeMap<String, String>,
}

fn connection_table(
    request: &ToolConnectionRequest,
    existing: Option<&Table>,
) -> Result<Table, String> {
    let id = request.id.as_deref().ok_or("connection id is required")?;
    let entry = request.entry.as_deref().ok_or("entry is required")?;
    let mut parsed = if entry.starts_with("http://") || entry.starts_with("https://") {
        Entry {
            url: Some(entry.to_string()),
            ..Default::default()
        }
    } else if Path::new(entry)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        read_entry_config(Path::new(entry), id)?
    } else {
        Entry {
            command: entry.to_string(),
            ..Default::default()
        }
    };
    if let Some(old) = existing {
        if parsed.args.is_empty()
            && request.args.is_empty()
            && !Path::new(entry)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            if let Some(args) = old.get("args").and_then(Item::as_array) {
                parsed.args = args
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                let program = old
                    .get("executable")
                    .or_else(|| old.get("command"))
                    .and_then(Item::as_str)
                    .unwrap_or_default();
                let stem = Path::new(program)
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if Path::new(entry).extension().is_some_and(|e| {
                    ["py", "js", "mjs", "cjs"]
                        .iter()
                        .any(|x| e.eq_ignore_ascii_case(x))
                }) && (stem.starts_with("python") || stem == "node")
                    && parsed.args.first().is_some_and(|v| {
                        Path::new(v).extension().is_some_and(|e| {
                            ["py", "js", "mjs", "cjs"]
                                .iter()
                                .any(|x| e.eq_ignore_ascii_case(x))
                        })
                    })
                {
                    parsed.args.remove(0);
                } else if Path::new(entry)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("ps1"))
                    && stem == "powershell"
                    && parsed.args.len() >= 3
                    && parsed.args[1] == "-File"
                {
                    parsed.args.drain(..3);
                }
            }
        }
        if parsed.cwd.is_none() {
            let old_cwd = old.get("cwd").and_then(Item::as_str);
            let program = old
                .get("executable")
                .or_else(|| old.get("command"))
                .and_then(Item::as_str)
                .unwrap_or_default();
            let stem = Path::new(program)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let args = old.get("args").and_then(Item::as_array);
            let old_entry = if stem.starts_with("python") || stem == "node" {
                args.and_then(|a| a.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or(program)
            } else if stem == "powershell" {
                args.and_then(|a| a.get(2))
                    .and_then(|v| v.as_str())
                    .unwrap_or(program)
            } else {
                program
            };
            let was_default =
                old_cwd.is_some_and(|cwd| Path::new(old_entry).parent() == Some(Path::new(cwd)));
            if !was_default || request.action != Action::Update {
                parsed.cwd = old_cwd.map(str::to_string);
            }
        }
        for (key, values) in [("env", &mut parsed.env), ("headers", &mut parsed.headers)] {
            if let Some(table) = old.get(key).and_then(Item::as_table_like) {
                for (name, value) in table.iter() {
                    if let Some(value) = value.as_str() {
                        values
                            .entry(name.to_string())
                            .or_insert_with(|| value.to_string());
                    }
                }
            }
        }
    }
    parsed.args.extend(request.args.clone());
    if request.cwd.is_some() {
        parsed.cwd = request.cwd.clone();
    }
    parsed.env.extend(request.env.clone());
    parsed.headers.extend(request.headers.clone());
    if let Some(url) = &parsed.url {
        if request.kind == Kind::Plugin {
            return Err(
                "Native plugins use a local process; choose mcp for an HTTP endpoint".into(),
            );
        }
        let url = url::Url::parse(url).map_err(|_| "Invalid MCP URL")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("An HTTP or HTTPS MCP address is required".into());
        }
    } else {
        let (command, args) = resolve_command(&parsed.command, &parsed.args)?;
        parsed.command = command;
        parsed.args = args;
    }
    if parsed.cwd.is_none()
        && parsed.url.is_none()
        && (existing.is_none() || request.action == Action::Update)
    {
        let original = Path::new(entry);
        parsed.cwd = original
            .parent()
            .filter(|p| p.is_absolute())
            .map(|p| p.to_string_lossy().into_owned());
    }
    let mut table = existing.cloned().unwrap_or_default();
    for key in [
        "executable",
        "command",
        "url",
        "args",
        "cwd",
        "env",
        "headers",
    ] {
        table.remove(key);
    }
    if request.kind == Kind::Mcp
        && existing.is_none()
        && !table.contains_key("env_from_env")
        && !table.contains_key("url")
        && parsed.url.is_none()
    {
        let mut forwarded = Table::new();
        for name in [
            "PATH",
            "PATHEXT",
            "SYSTEMROOT",
            "WINDIR",
            "TEMP",
            "TMP",
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            if std::env::var_os(name).is_some()
                && !parsed.env.keys().any(|key| env_name_equal(key, name))
            {
                forwarded[name] = toml_edit::value(name);
            }
        }
        table["env_from_env"] = Item::Table(forwarded);
    }
    if let Some(forwarded) = table
        .get_mut("env_from_env")
        .and_then(Item::as_table_like_mut)
    {
        let overridden: Vec<String> = forwarded
            .iter()
            .filter(|(key, _)| {
                parsed
                    .env
                    .keys()
                    .any(|literal| env_name_equal(literal, key))
            })
            .map(|(key, _)| key.to_string())
            .collect();
        for key in overridden {
            forwarded.remove(&key);
        }
    }
    table["id"] = toml_edit::value(id);
    if !table.contains_key("name") {
        table["name"] = toml_edit::value(id);
    }
    if let Some(url) = parsed.url {
        table["url"] = toml_edit::value(url);
    } else {
        table[if request.kind == Kind::Mcp {
            "executable"
        } else {
            "command"
        }] = toml_edit::value(parsed.command);
    }
    if !parsed.args.is_empty() {
        let mut args = Array::new();
        for arg in parsed.args {
            args.push(arg);
        }
        table["args"] = toml_edit::value(args);
    }
    if let Some(cwd) = parsed.cwd {
        table["cwd"] = toml_edit::value(cwd);
    }
    for (name, values) in [("env", parsed.env), ("headers", parsed.headers)] {
        if !values.is_empty() {
            let mut values_table = Table::new();
            for (key, value) in values {
                values_table[&key] = toml_edit::value(value);
            }
            table[name] = Item::Table(values_table);
        }
    }
    Ok(table)
}

fn env_name_equal(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn same_provider_config(kind: Kind, old: &Table, new: &Table) -> bool {
    match kind {
        Kind::Mcp => toml::from_str::<config::McpGatewayProviderConfig>(&old.to_string())
            .ok()
            .zip(toml::from_str::<config::McpGatewayProviderConfig>(&new.to_string()).ok())
            .is_some_and(|(a, b)| a == b),
        Kind::Plugin => toml::from_str::<config::PluginProviderConfig>(&old.to_string())
            .ok()
            .zip(toml::from_str::<config::PluginProviderConfig>(&new.to_string()).ok())
            .is_some_and(|(a, b)| a == b),
    }
}

fn read_entry_config(path: &Path, id: &str) -> Result<Entry, String> {
    if !path.is_absolute() {
        return Err("Use an absolute configuration path".into());
    }
    let file = std::fs::File::open(path).map_err(|_| "Cannot read MCP startup configuration")?;
    if !file
        .metadata()
        .map_err(|_| "Cannot inspect startup configuration")?
        .is_file()
    {
        return Err("Startup configuration must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take((webcodex_core::tool_connection::TOOL_CONNECTION_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read MCP startup configuration")?;
    if bytes.len() > webcodex_core::tool_connection::TOOL_CONNECTION_MAX_BYTES {
        return Err("MCP startup configuration is too large".into());
    }
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "MCP startup configuration is not valid JSON")?;
    if let Some(servers) = value.get("mcpServers").and_then(Value::as_object) {
        value = servers
            .get(id)
            .or_else(|| {
                (servers.len() == 1)
                    .then(|| servers.values().next())
                    .flatten()
            })
            .cloned()
            .ok_or(
                "Configuration contains several servers; use the server name as connection id",
            )?;
    }
    let object = value
        .as_object()
        .ok_or("Expected one MCP server configuration")?;
    let string_map = |key: &str| -> Result<BTreeMap<String, String>, String> {
        match object.get(key) {
            None => Ok(BTreeMap::new()),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| format!("{key} must contain string values")),
        }
    };
    let mut result = Entry {
        command: object
            .get("command")
            .or_else(|| object.get("executable"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        args: object
            .get("args")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|_| "args must be strings")?
            .unwrap_or_default(),
        cwd: object
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string),
        env: string_map("env")?,
        url: object
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        headers: string_map("headers")?,
    };
    let base = path
        .parent()
        .ok_or("Configuration has no parent directory")?;
    if let Some(cwd) = &result.cwd {
        if Path::new(cwd).is_relative() {
            result.cwd = Some(base.join(cwd).to_string_lossy().into_owned());
        }
    }
    if result.command.contains('/') || result.command.contains('\\') {
        if Path::new(&result.command).is_relative() {
            result.command = base.join(&result.command).to_string_lossy().into_owned();
        }
    }
    Ok(result)
}

fn resolve_command(command: &str, args: &[String]) -> Result<(String, Vec<String>), String> {
    if command.trim().is_empty() {
        return Err("A startup command or local MCP URL is required".into());
    }
    let path = Path::new(command);
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (program, mut prefix) = match extension.as_str() {
        "py" => (find_program("python")?, vec![absolute_file(path)?]),
        "js" | "mjs" | "cjs" => (find_program("node")?, vec![absolute_file(path)?]),
        "ps1" => (
            find_program("powershell")?,
            vec!["-NoProfile".into(), "-File".into(), absolute_file(path)?],
        ),
        _ => (
            if path.is_absolute() {
                absolute_file(path)?
            } else {
                find_program(command)?
            },
            Vec::new(),
        ),
    };
    prefix.extend_from_slice(args);
    Ok((program, prefix))
}

fn absolute_file(path: &Path) -> Result<String, String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("Tool entry must be an existing absolute file path".into());
    }
    Ok(path.to_string_lossy().into_owned())
}

fn find_program(name: &str) -> Result<String, String> {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
        if cfg!(windows) {
            let candidate = dir.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
    }
    Err(format!(
        "Required runtime {name} is not installed or is absent from PATH"
    ))
}

#[cfg(test)]
#[path = "tool_connection_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_connection_nx_real_tests.rs"]
mod nx_real_tests;
