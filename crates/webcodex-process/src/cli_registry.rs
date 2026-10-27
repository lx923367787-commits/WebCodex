//! Shared, read-only local installation discovery for Desktop and Runner.
//! The registry owns product differences; consumers render/use discovered IDs.
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use webcodex_core::cli_adapter::{cli_adapters, CliAdapter, CliPathRoot};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallation {
    pub provider_id: String,
    pub label: String,
    pub installed: bool,
    pub native: bool,
    pub adapter_available: bool,
    pub status: &'static str,
    pub message: &'static str,
    /// Controller-only launch subject; never accepted back from a model/UI.
    #[serde(skip)]
    pub executable: PathBuf,
}

/// Operator-owned adapter configuration, never a path supplied by a remote
/// tool call. No credentials or account files belong in these definitions.
pub fn configured_cli_adapters() -> Result<Vec<CliAdapter>, String> {
    let path = std::env::var_os("WEBCODEX_CLI_ADAPTERS_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "XDG_CONFIG_HOME"
            })
            .map(|root| PathBuf::from(root).join("webcodex/cli-adapters.json"))
        });
    let Some(path) = path else {
        return cli_adapters(&[]);
    };
    if !path.is_absolute() {
        return Err("cli_adapter_config_absolute_path_required".into());
    }
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return cli_adapters(&[]),
        Err(_) => return Err("cli_adapter_config_unreadable".into()),
    };
    if !metadata.is_file() || metadata.len() > 256 * 1024 {
        return Err("cli_adapter_config_limit".into());
    }
    let bytes = std::fs::read(&path).map_err(|_| "cli_adapter_config_unreadable")?;
    if bytes.len() > 256 * 1024 {
        return Err("cli_adapter_config_limit".into());
    }
    let definitions: Vec<CliAdapter> =
        serde_json::from_slice(&bytes).map_err(|_| "cli_adapter_config_invalid")?;
    cli_adapters(&definitions)
}

pub fn discover_cli_installations(
    additions: &[CliAdapter],
) -> Result<Vec<CliInstallation>, String> {
    let lookup = |key| std::env::var_os(key).map(PathBuf::from);
    let mut roots = BTreeMap::new();
    if let Some(path) = lookup(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        roots.insert("home", path);
    }
    if let Some(path) = lookup("LOCALAPPDATA") {
        roots.insert("local_app_data", path);
    }
    if let Some(path) = lookup("APPDATA") {
        roots.insert("app_data", path);
    }
    let paths = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).take(128).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut definitions = configured_cli_adapters()?;
    for addition in additions {
        addition.validate()?;
        if let Some(existing) = definitions.iter_mut().find(|entry| entry.id == addition.id) {
            *existing = addition.clone();
        } else {
            definitions.push(addition.clone());
        }
    }
    if definitions.len() > webcodex_core::cli_adapter::MAX_CLI_ADAPTERS {
        return Err("cli_adapter_limit".into());
    }
    discover_in(&definitions, &paths, &roots)
}

fn discover_in(
    adapters: &[CliAdapter],
    paths: &[PathBuf],
    roots: &BTreeMap<&str, PathBuf>,
) -> Result<Vec<CliInstallation>, String> {
    let mut result = Vec::new();
    for adapter in adapters {
        adapter.validate()?;
        let mut candidates = adapter
            .user_paths
            .iter()
            .filter_map(|location| {
                let key = match location.root {
                    CliPathRoot::Home => "home",
                    CliPathRoot::LocalAppData => "local_app_data",
                    CliPathRoot::AppData => "app_data",
                };
                roots.get(key).map(|root| root.join(&location.path))
            })
            .collect::<Vec<_>>();
        for directory in paths.iter().filter(|p| p.is_absolute()) {
            candidates.extend(adapter.executables.iter().map(|name| directory.join(name)));
        }
        // Prefer a native executable even if PATH also contains a shell shim.
        // Resolve an installer's symlink once; later execution pins real bytes.
        let found: Vec<_> = candidates
            .into_iter()
            .filter(|p| p.is_absolute() && p.is_file())
            .filter_map(|p| p.canonicalize().ok())
            .collect();
        let Some(executable) = found.iter().find(|p| native(p)).or_else(|| found.first()) else {
            continue;
        };
        let is_native = native(executable);
        let adapter_available = adapter.invocation.is_some();
        let (status, message) = if !is_native {
            (
                "launcher_requires_adapter",
                "已发现 CLI 启动器；需要对应的原生或 Node 适配器。",
            )
        } else if !adapter_available {
            (
                "adapter_required",
                "已发现本机 CLI；调用适配和模型能力尚未就绪。",
            )
        } else {
            ("discovered", "已发现本机 CLI；选择后查询模型与调用能力。")
        };
        result.push(CliInstallation {
            provider_id: adapter.id.clone(),
            label: adapter.label.clone(),
            installed: true,
            native: is_native,
            adapter_available,
            status,
            message,
            executable: native_path(executable),
        });
    }
    Ok(result)
}

fn native(path: &Path) -> bool {
    if cfg!(windows) {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    } else {
        !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("cmd" | "bat" | "ps1")
        )
    }
}
fn native_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Some(text) = path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        if !text.starts_with("UNC\\") {
            return PathBuf::from(text);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use webcodex_core::cli_adapter::CliMetadataRecipe;
    #[test]
    fn discovery_lists_only_present_clis_and_accepts_a_third_party_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = CliAdapter {
            id: "custom-ai".into(),
            label: "Custom AI".into(),
            executables: vec!["custom-ai.exe".into()],
            user_paths: vec![],
            metadata: CliMetadataRecipe::Help,
            invocation: None,
        };
        assert!(
            discover_in(&[adapter.clone()], &[dir.path().into()], &BTreeMap::new())
                .unwrap()
                .is_empty()
        );
        std::fs::write(
            dir.path().join("custom-ai.exe"),
            b"metadata fixture; never executed",
        )
        .unwrap();
        let found = discover_in(&[adapter], &[dir.path().into()], &BTreeMap::new()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provider_id, "custom-ai");
        assert_eq!(found[0].status, "adapter_required");
        assert!(!found[0].adapter_available);
    }
}
