//! Private Desktop producer for the existing chat-agents process. Material is
//! passive: the Runner's committed lifecycle protocol alone activates its pump.
use super::*;
use crate::bundled_tools::{bootstrap_credential_file, BootstrapCredentialFile};
use crate::webcodex::RuntimeConnection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Read;

const MAX_CONTROL_BYTES: u64 = 64 * 1024;
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

pub(super) fn carrier_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join("secrets")
        .join("chat-agents")
        .join("control.json")
}

fn error(code: &str) -> DesktopError {
    DesktopError::new(
        code,
        "Automatic Host control is not ready",
        "Check the owned local runtime and its complete registered-project inventory.",
    )
}

#[derive(Clone, PartialEq, Eq)]
struct Source {
    binary: PathBuf,
    connection: RuntimeConnection,
    client_id: String,
    native_host_id: Option<String>,
    env_file: PathBuf,
    control_path: PathBuf,
    desktop_state: PathBuf,
    // Local owned handle generation, not a Server Runner instance identity.
    owned_runner: (u32, u64),
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Carrier {
    schema_version: u32,
    server_url: String,
    bootstrap_token_file: PathBuf,
    source_ref: String,
    owner_user: String,
    projects: Vec<String>,
}

struct Publication {
    source: Source,
    carrier: Carrier,
    cached_projects: Vec<String>,
    written: Vec<u8>,
    credential: BootstrapCredentialFile,
    // Keep this stable sidecar locked through carrier withdrawal and credential
    // destruction. A later Desktop owner cannot publish before this one exits.
    _lock: File,
}

impl Publication {
    fn create(source: Source, owner_user: String, projects: Vec<String>) -> DesktopResult<Self> {
        let lock = prepare_owner(
            &source.control_path,
            Some(&source.connection.server_url),
            Some(&source.env_file),
        )?;
        let credential = bootstrap_credential_file(&source.env_file)?;
        let carrier = Carrier {
            schema_version: 2,
            server_url: source.connection.server_url.clone(),
            bootstrap_token_file: credential.path().to_owned(),
            source_ref: format!(
                "desktop-local-bootstrap:{}",
                source.env_file.to_string_lossy()
            ),
            owner_user,
            projects: projects.clone(),
        };
        let mut publication = Self {
            source,
            carrier,
            cached_projects: projects,
            written: Vec::new(),
            credential,
            _lock: lock,
        };
        publication.write()?;
        Ok(publication)
    }

    fn write(&mut self) -> DesktopResult<()> {
        if !self.written.is_empty()
            && read_bounded(&self.source.control_path, MAX_CONTROL_BYTES)? != self.written
        {
            return Err(error("host_control_carrier_changed"));
        }
        let next = serde_json::to_vec(&self.carrier).map_err(|_| error("host_control_invalid"))?;
        if next.len() as u64 > MAX_CONTROL_BYTES {
            return Err(error("host_control_invalid"));
        }
        if next == self.written {
            return Ok(());
        }
        // Retain recovery ownership if an atomic writer reports an error after
        // replacement (for example directory fsync).
        self.written = next;
        write_atomic_file(&self.source.control_path, &self.written)
            .map_err(|_| error("host_control_persist_failed"))
    }

    fn pause(&mut self) -> DesktopResult<()> {
        self.carrier.projects.clear();
        self.write()
    }

    fn update(&mut self, projects: Vec<String>) -> DesktopResult<()> {
        self.cached_projects = projects.clone();
        self.carrier.projects = projects;
        self.write()
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        // The OS owner lock excludes another cooperating producer. The content
        // check also preserves a carrier independently replaced by an operator.
        if !self.written.is_empty()
            && read_bounded(&self.source.control_path, MAX_CONTROL_BYTES)
                .is_ok_and(|current| current == self.written)
        {
            let _ = std::fs::remove_file(&self.source.control_path);
        }
        // Keep the field observably alive until after carrier withdrawal.
        let _ = self.credential.path();
    }
}

fn lock_owner(path: &Path) -> DesktopResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| error("host_control_owner_busy"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(error("host_control_owner_busy"));
        }
    }
    Ok(file)
}

/// Reserve this Desktop's publication slot before starting its Runner or
/// publishing control material. An available OS lock proves the former
/// cooperating publisher has released ownership; no PID/liveness guess is used.
/// A crash-left carrier is preserved only when its source matches the saved
/// runtime. Its credential is neither opened, reused, nor deleted.
pub(super) fn prepare_owner(
    control_path: &Path,
    expected_server_url: Option<&str>,
    expected_env_file: Option<&Path>,
) -> DesktopResult<File> {
    let directory = control_path
        .parent()
        .ok_or_else(|| error("host_control_path_invalid"))?;
    crate::bundled_tools::prepare_host_control_directory(directory)?;
    let lock = lock_owner(&directory.join("control.lock"))?;
    match std::fs::symlink_metadata(control_path) {
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(lock),
        Err(_) => return Err(error("host_control_source_unavailable")),
        Ok(_) => {}
    }

    let original = read_bounded(control_path, MAX_CONTROL_BYTES)?;
    let carrier: Carrier =
        serde_json::from_slice(&original).map_err(|_| error("host_control_carrier_conflict"))?;
    let source_env = carrier
        .source_ref
        .strip_prefix("desktop-local-bootstrap:")
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .and_then(|path| path.canonicalize().ok());
    let expected_env = expected_env_file.and_then(|path| path.canonicalize().ok());
    if carrier.schema_version != 2
        || expected_server_url != Some(carrier.server_url.as_str())
        || source_env.is_none()
        || expected_env.is_none()
        || source_env != expected_env
    {
        return Err(error("host_control_carrier_conflict"));
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| error("host_control_recovery_failed"))?
        .as_nanos();
    let backup = directory.join(format!("control.stale-{}-{nonce}.json", std::process::id()));
    if backup
        .try_exists()
        .map_err(|_| error("host_control_recovery_failed"))?
    {
        return Err(error("host_control_recovery_failed"));
    }
    // Match the ordinary publication write guard: preserve an operator change
    // instead of moving bytes that were not validated above.
    if read_bounded(control_path, MAX_CONTROL_BYTES)? != original {
        return Err(error("host_control_carrier_changed"));
    }
    std::fs::rename(control_path, &backup).map_err(|_| error("host_control_recovery_failed"))?;
    sync_state_directory(directory).map_err(|_| error("host_control_recovery_failed"))?;
    Ok(lock)
}

fn read_bounded(path: &Path, limit: u64) -> DesktopResult<Vec<u8>> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| error("host_control_source_unavailable"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(error("host_control_source_invalid"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(error("host_control_source_invalid"));
        }
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| error("host_control_source_unavailable"))?;
    if bytes.len() as u64 > limit {
        return Err(error("host_control_source_invalid"));
    }
    Ok(bytes)
}

#[derive(Default)]
pub(super) struct HostControlState {
    publication: Option<Publication>,
    refresh: Option<CancellationSignal>,
    last_error: Option<String>,
}

impl HostControlState {
    pub(super) fn revoke(&mut self) {
        self.cancel_refresh();
        self.publication = None;
    }

    fn cancel_refresh(&self) {
        if let Some(refresh) = &self.refresh {
            refresh.cancel();
        }
    }
}

fn scope_from_inventory(
    owner: Option<String>,
    client_id: &str,
    projects: Vec<crate::models::RegisteredProject>,
) -> DesktopResult<(String, Vec<String>)> {
    let owner = owner
        .filter(|id| !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .ok_or_else(|| error("host_control_operator_unavailable"))?;
    let mut seen = BTreeSet::new();
    let mut enabled = Vec::new();
    let prefix = format!("agent:{client_id}:");
    for project in projects {
        if !project.id.starts_with(&prefix)
            || project.id.len() <= prefix.len()
            || project.id.len() > 128
            || !project
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
            || !seen.insert(project.id.clone())
        {
            return Err(error("host_control_inventory_invalid"));
        }
        if project.enabled {
            enabled.push(project.id);
        }
    }
    if enabled.len() > 128 {
        return Err(error("host_control_inventory_limit"));
    }
    enabled.sort();
    Ok((owner, enabled))
}

fn append_named_scopes(
    projects: &mut Vec<String>,
    catalog: &Value,
    expected_host: &str,
    folders: &[crate::models::RegisteredProject],
) -> DesktopResult<()> {
    if catalog.get("host_id").and_then(Value::as_str) != Some(expected_host) {
        return Err(error("project_connection_changed"));
    }
    let named = catalog
        .get("projects")
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= 32)
        .ok_or_else(|| error("host_control_inventory_invalid"))?;
    for workspace in named {
        if workspace.get("enabled").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let id = workspace
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| {
                id.strip_prefix("wc_workspace_").is_some_and(|s| {
                    s.len() == 32
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            })
            .ok_or_else(|| error("host_control_inventory_invalid"))?;
        let granted = workspace
            .get("folders")
            .and_then(Value::as_array)
            .ok_or_else(|| error("host_control_inventory_invalid"))?;
        // A team must have one unambiguous, currently registered execution root.
        if granted.len() == 1
            && folders.iter().any(|folder| {
                folder.enabled
                    && granted[0].get("project").and_then(Value::as_str) == Some(folder.id.as_str())
                    && granted[0].get("path").and_then(Value::as_str) == Some(folder.path.as_str())
            })
        {
            projects.push(id.to_owned());
        }
    }
    projects.sort();
    projects.dedup();
    if projects.len() > 128 {
        return Err(error("host_control_inventory_limit"));
    }
    Ok(())
}

impl AppState {
    async fn host_control_source(&self) -> DesktopResult<Source> {
        if self.operations.current().is_some() {
            return Err(error("desktop_operation_busy"));
        }
        let slot = self.core.lock().await;
        let core = slot
            .as_ref()
            .ok_or_else(|| error("desktop_operation_busy"))?;
        let local_full = core.config.topology.as_ref().is_some_and(|topology| {
            topology.experience == Experience::Full
                && matches!(topology.server, ServerTopology::Local)
                && matches!(topology.runner, RunnerTopology::Local)
        });
        if !local_full {
            return Err(error("host_control_runtime_unavailable"));
        }
        let connection = project_management::connection_from_config(&core.config)
            .ok_or_else(|| error("host_control_runtime_unavailable"))?;
        let url = url::Url::parse(&connection.server_url)
            .map_err(|_| error("host_control_source_invalid"))?;
        let loopback = url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !matches!(url.scheme(), "http" | "https")
            || !loopback
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(error("host_control_source_invalid"));
        }
        let env_file = core
            .config
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.server_env_file.as_ref())
            .filter(|path| path.is_file())
            .ok_or_else(|| error("host_control_source_unavailable"))?
            .canonicalize()
            .map_err(|_| error("host_control_source_unavailable"))?;
        let client_id = stored_runner_client_id(&core.config)
            .ok_or_else(|| error("host_control_source_invalid"))?;
        let source = Source {
            binary: core.adapter.binaries()?.webcodex.clone(),
            connection,
            client_id,
            native_host_id: core
                .config
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.native_host_id.clone()),
            env_file,
            control_path: carrier_path(&core.data_dir),
            desktop_state: core.config_path.clone(),
            owned_runner: (0, 0),
        };
        drop(slot);
        let owned_runner = self
            .supervisor
            .lock()
            .await
            .owned_process_identity(ProcessKind::LocalRunner)
            .ok_or_else(|| error("host_control_runtime_unavailable"))?;
        let content = read_bounded(&source.connection.runner_config, MAX_CONFIG_BYTES)?;
        let content =
            std::str::from_utf8(&content).map_err(|_| error("host_control_source_invalid"))?;
        if !crate::host_control_config::validate_content(
            content,
            &source.control_path,
            &source.desktop_state,
        )? {
            return Err(error("host_control_provider_unconfigured"));
        }
        if self.operations.current().is_some() {
            return Err(error("desktop_operation_busy"));
        }
        Ok(Source {
            owned_runner,
            ..source
        })
    }

    pub(super) async fn revoke_host_control(&self) {
        let mut state = self.host_control.lock().await;
        state.revoke();
    }

    pub(super) async fn pause_host_control(&self, code: &str) {
        let mut state = self.host_control.lock().await;
        state.cancel_refresh();
        let mut stop_owner = None;
        if let Some(publication) = state.publication.as_mut() {
            if publication.pause().is_err() {
                stop_owner = Some(publication.source.owned_runner);
                state.publication = None;
            }
        }
        if state.last_error.as_deref() != Some(code) {
            self.activity.push(ActivityEventKind::OperationFailed, "host_control", ActivityLevel::Warning,
                format!("Automatic Host control is paused ({code}); cached project inventory is retained."));
            state.last_error = Some(code.into());
        }
        drop(state);
        if let Some(expected) = stop_owner {
            let mut supervisor = self.supervisor.lock().await;
            if supervisor.owned_process_identity(ProcessKind::LocalRunner) == Some(expected) {
                // If stale scopes cannot be withdrawn from disk, stop only the
                // exact owned Runner whose provider could still consume them.
                supervisor.stop(ProcessKind::LocalRunner).await;
            }
            self.activity.push(
                ActivityEventKind::OperationFailed,
                "host_control",
                ActivityLevel::Error,
                "Private Host control could not be paused safely; its owned Runner was stopped.",
            );
        }
    }

    pub(super) async fn maintain_host_control_owner(&self) {
        let expected = self
            .host_control
            .lock()
            .await
            .publication
            .as_ref()
            .map(|value| value.source.clone());
        let Some(expected) = expected else {
            return;
        };
        match self.host_control_source().await {
            Ok(current) if current == expected => {}
            Err(error) if error.code == "desktop_operation_busy" => {}
            _ => self.revoke_host_control().await,
        }
    }

    async fn refresh_host_control(&self, cancellation: &CancellationContext) -> DesktopResult<()> {
        cancellation.check()?;
        let source = self.host_control_source().await?;
        // One exact Human project-list response supplies the actor and registered
        // roots. Named scopes below must match this same host and inventory.
        let inventory = WebCodexAdapter::list_registry_inventory_with_binary(
            &source.binary,
            &source.connection,
            &source.client_id,
            cancellation,
        )
        .await?;
        cancellation.check()?;
        if self.host_control_source().await? != source {
            return Err(error("project_connection_changed"));
        }
        inventory.require_complete()?;
        let (owner, mut projects) = scope_from_inventory(
            inventory.operator_user_id,
            &source.client_id,
            inventory.projects.clone(),
        )?;
        if let Some(host) = &source.native_host_id {
            let catalog = self.named_workspaces(None).await?;
            append_named_scopes(&mut projects, &catalog, host, &inventory.projects)?;
            if self.host_control_source().await? != source {
                return Err(error("project_connection_changed"));
            }
        }
        let mut state = self.host_control.lock().await;
        cancellation.check()?;
        if state.publication.as_ref().is_some_and(|existing| {
            existing.source != source || existing.carrier.owner_user != owner
        }) {
            state.publication = None;
        }
        match state.publication.as_mut() {
            Some(existing) => existing.update(projects)?,
            None => state.publication = Some(Publication::create(source, owner, projects)?),
        }
        if state.last_error.take().is_some() {
            self.activity.push(
                ActivityEventKind::OperationStarted,
                "host_control",
                ActivityLevel::Info,
                "Private Host control material is prepared; the Runner owns provider activation.",
            );
        }
        Ok(())
    }

    pub async fn publish_host_control(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = self.shutdown_signal.cancelled() => break,
                _ = interval.tick() => {}
            }
            let signal = CancellationSignal::new();
            let cancellation =
                CancellationContext::new(signal.clone(), self.shutdown_signal.clone());
            self.host_control.lock().await.refresh = Some(signal.clone());
            let refresh = self.refresh_host_control(&cancellation);
            tokio::pin!(refresh);
            let result = tokio::select! {
                value = &mut refresh => value,
                _ = tokio::time::sleep(Duration::from_secs(40)) => {
                    signal.cancel();
                    let _ = refresh.await;
                    Err(error("host_control_refresh_timeout"))
                }
                _ = self.shutdown_signal.cancelled() => {
                    signal.cancel();
                    let _ = refresh.await;
                    Err(error("desktop_operation_cancelled"))
                }
            };
            self.host_control.lock().await.refresh = None;
            if let Err(failure) = result {
                if matches!(
                    failure.code.as_str(),
                    "host_control_runtime_unavailable" | "project_connection_changed"
                ) {
                    self.revoke_host_control().await;
                }
                self.pause_host_control(&failure.code).await;
            }
        }
        self.revoke_host_control().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RegisteredProject;

    fn project(id: &str, enabled: bool) -> RegisteredProject {
        RegisteredProject {
            id: id.into(),
            name: id.into(),
            path: "/project".into(),
            revision: "1".into(),
            enabled,
            connected: false,
            agent_status: "idle".into(),
            active_jobs: 0,
        }
    }

    #[test]
    fn named_team_scopes_require_the_exact_host_and_registered_folder() {
        let id = format!("wc_workspace_{}", "a".repeat(32));
        let catalog = serde_json::json!({"host_id":"host", "projects":[
            {"id":id,"enabled":true,"folders":[{"project":"agent:runner:code","path":"/project"}]}
        ]});
        let folders = vec![project("agent:runner:code", true)];
        let mut scopes = vec![];
        append_named_scopes(&mut scopes, &catalog, "host", &folders).unwrap();
        assert_eq!(scopes, vec![id]);
        assert!(append_named_scopes(&mut vec![], &catalog, "other-host", &folders).is_err());
        let mut moved = folders.clone();
        moved[0].path = "/replacement".into();
        let mut rejected = vec![];
        append_named_scopes(&mut rejected, &catalog, "host", &moved).unwrap();
        assert!(rejected.is_empty());
        moved[0] = project("agent:runner:code", false);
        append_named_scopes(&mut rejected, &catalog, "host", &moved).unwrap();
        assert!(rejected.is_empty());
    }

    pub(super) fn fixture() -> (PathBuf, Source) {
        let root = std::env::temp_dir().join(format!(
            "webcodex-host-control-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let env_file = root.join("server.env");
        std::fs::write(&env_file, "WEBCODEX_TOKEN=synthetic-bootstrap-fixture\n").unwrap();
        let source = Source {
            binary: root.join("webcodex"),
            connection: RuntimeConnection {
                runner_config: root.join("runner.toml"),
                user_token_file: root.join("user-token"),
                server_url: "http://127.0.0.1:19500".into(),
            },
            client_id: "runner".into(),
            native_host_id: None,
            env_file: env_file.canonicalize().unwrap(),
            control_path: carrier_path(&root),
            desktop_state: root.join("desktop-state.json"),
            owned_runner: (100, 1),
        };
        (root, source)
    }

    #[test]
    fn one_inventory_actor_covers_all_enabled_projects_without_connected_filter() {
        let (owner, projects) = scope_from_inventory(
            Some("human-user".into()),
            "runner",
            vec![
                project("agent:runner:b", true),
                project("agent:runner:a", true),
                project("agent:runner:c", false),
            ],
        )
        .unwrap();
        assert_eq!(owner, "human-user");
        assert_eq!(projects, vec!["agent:runner:a", "agent:runner:b"]);
        assert!(scope_from_inventory(None, "runner", vec![]).is_err());
        assert!(scope_from_inventory(Some(" \n".into()), "runner", vec![]).is_err());
    }

    #[test]
    fn ambiguous_or_other_client_scope_is_never_silently_deduplicated() {
        assert!(scope_from_inventory(
            Some("human-user".into()),
            "runner",
            vec![project("agent:other:a", true)]
        )
        .is_err());
        assert!(scope_from_inventory(
            Some("human-user".into()),
            "runner",
            vec![
                project("agent:runner:a", true),
                project("agent:runner:a", false)
            ]
        )
        .is_err());
    }

    #[test]
    fn carrier_pause_preserves_cache_and_credential_until_owned_runtime_is_dropped() {
        let (root, source) = fixture();
        let path = source.control_path.clone();
        let mut publication =
            Publication::create(source, "human-user".into(), vec!["agent:runner:a".into()])
                .unwrap();
        let credential = publication.credential.path().to_owned();
        let content = std::fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&content).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 6);
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["owner_user"], "human-user");
        assert!(!String::from_utf8(content)
            .unwrap()
            .contains("synthetic-bootstrap-fixture"));
        publication.pause().unwrap();
        assert_eq!(publication.cached_projects, vec!["agent:runner:a"]);
        assert!(credential.is_file());
        let paused: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(paused["projects"], serde_json::json!([]));
        publication.update(vec!["agent:runner:b".into()]).unwrap();
        assert_eq!(publication.credential.path(), credential);
        drop(publication);
        assert!(!path.exists());
        assert!(!credential.exists());
        assert!(path.parent().unwrap().join("control.lock").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn withdrawing_owned_carrier_does_not_delete_an_operator_replacement() {
        let (root, source) = fixture();
        let path = source.control_path.clone();
        let publication = Publication::create(source, "human-user".into(), vec![]).unwrap();
        write_atomic_file(&path, b"operator replacement").unwrap();
        drop(publication);
        assert_eq!(std::fs::read(&path).unwrap(), b"operator replacement");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reused_pid_does_not_match_another_owned_handle_generation() {
        let (root, first) = fixture();
        let mut second = first.clone();
        second.owned_runner.1 += 1;
        assert!(first != second);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[path = "host_control_recovery_tests.rs"]
mod recovery_tests;
