//! Private Desktop producer for the existing chat-agents process. Material is
//! passive: the Runner's committed lifecycle protocol alone activates its pump.
use super::*;
use crate::bundled_tools::{bootstrap_credential_file, BootstrapCredentialFile};
use crate::webcodex::RuntimeConnection;
use serde::Serialize;
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
    env_file: PathBuf,
    control_path: PathBuf,
    desktop_state: PathBuf,
    // Local owned handle generation, not a Server Runner instance identity.
    owned_runner: (u32, u64),
}

#[derive(Clone, Serialize)]
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
        let directory = source
            .control_path
            .parent()
            .ok_or_else(|| error("host_control_path_invalid"))?;
        crate::bundled_tools::prepare_host_control_directory(directory)?;
        let lock = lock_owner(&directory.join("control.lock"))?;
        // Never overwrite a carrier left by an unrelated or unconfirmed owner.
        // Graceful teardown removes ours; a stale file requires explicit recovery.
        if source.control_path.exists() {
            return Err(error("host_control_carrier_conflict"));
        }
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

fn read_bounded(path: &Path, limit: u64) -> DesktopResult<Vec<u8>> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| error("host_control_source_unavailable"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(error("host_control_source_invalid"));
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
        // One exact Human project-list response supplies both actor and the
        // entire scope. No policy reads, UI selection, or connected filtering.
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
        let (owner, projects) = scope_from_inventory(
            inventory.operator_user_id,
            &source.client_id,
            inventory.projects,
        )?;
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

    fn fixture() -> (PathBuf, Source) {
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
