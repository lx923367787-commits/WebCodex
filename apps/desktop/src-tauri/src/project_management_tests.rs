use super::*;

fn native_fixture_resources(directory: &Path) -> PathBuf {
    let source = PathBuf::from(std::env::var_os("WEBCODEX_DESKTOP_BIN_DIR").unwrap());
    let resources = directory.join("resources");
    let runtime = resources.join("webcodex-runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    for name in ["webcodex", "webcodex-server", "webcodex-runner"] {
        let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(source.join(&name), runtime.join(name)).unwrap();
    }
    resources
}

#[tokio::test]
#[ignore = "requires source-matched dogfood binaries; isolated loopback service and temporary state"]
async fn native_zero_project_services_restart_without_granting_directories() {
    assert!(std::env::var_os("WEBCODEX_DESKTOP_BIN_DIR").is_some());
    let directory = std::env::temp_dir().join(format!(
        "webcodex-zero-project-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data = directory.join("state");
    let resources = native_fixture_resources(&directory);
    let cancellation = CancellationContext::never();
    let mut core = DesktopCore::new(data.clone(), resources.clone()).unwrap();
    let result: DesktopResult<()> = async {
        let initial = core.configure_local_setup(None, &cancellation).await?;
        assert!(initial.readiness.services_ready());
        assert!(initial.readiness.runtime_ready);
        assert!(initial.project.is_none());
        assert!(core.registry_projects(&cancellation).await?.is_empty());
        let connection = connection_from_config(&core.config).unwrap();
        // The real server accepts the client identity, but rejects a task with
        // no Agent/project binding. Never print this fixture's bearer credential.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let url = url::Url::parse(&connection.server_url).unwrap();
        let address = format!("{}:{}", url.host_str().unwrap(), url.port_or_known_default().unwrap());
        let token = std::fs::read_to_string(&connection.user_token_file).unwrap();
        let body = r#"{"project":"missing-project","path":"README.md"}"#;
        let response = tokio::time::timeout(Duration::from_secs(5), async {
            let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
            let request = format!("POST /api/projects/read_file HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", token.trim(), body.len());
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        }).await.unwrap();
        if !response.starts_with("HTTP/1.1 401") || !response.contains("agent_identity_required") {
            return Err(project_error("fixture_unbound_task_not_rejected"));
        }
        drop(token);
        let before = std::fs::read(&connection.runner_config).unwrap();
        let policy: toml_edit::DocumentMut = std::str::from_utf8(&before).unwrap().parse().unwrap();
        assert_eq!(
            policy["policy"]["require_explicit_roots"].as_bool(),
            Some(true)
        );
        assert!(policy["policy"]["allowed_roots"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            policy["policy"]["allow_cwd_anywhere"].as_bool(),
            Some(false)
        );
        assert!(
            !data.join("workspace").exists(),
            "startup must not create a management project"
        );
        core.stop_local_runtime(&cancellation).await?;
        let stopped = core.refresh_runtime_status(&cancellation).await?;
        assert_eq!(stopped.readiness.server, ServerReadiness::Stopped);
        assert_eq!(stopped.readiness.runner, RunnerReadiness::Stopped);
        core = DesktopCore::new(data.clone(), resources.clone())?;
        let resumed = core.resume_saved_runtime(&cancellation).await?;
        assert!(resumed.readiness.services_ready());
        assert!(resumed.project.is_none());
        assert_eq!(connection_from_config(&core.config).unwrap(), connection);
        assert_eq!(std::fs::read(&connection.runner_config).unwrap(), before);
        assert!(core.registry_projects(&cancellation).await?.is_empty());
        // Registering a real source folder later preserves the service identity
        // and gives precisely that folder authority, without HOME fallback.
        let project = directory.join("source");
        std::fs::create_dir_all(&project).unwrap();
        let server_pid = core.process_snapshot(ProcessKind::LocalServer).await.unwrap().pid;
        let runner_pid = core.process_snapshot(ProcessKind::LocalRunner).await.unwrap().pid;
        core.add_project(&project.to_string_lossy(), &cancellation)
            .await?;
        assert!(core.snapshot.readiness.services_ready());
        assert!(core.config.project.is_none());
        assert_eq!(connection_from_config(&core.config).unwrap(), connection);
        assert_eq!(core.process_snapshot(ProcessKind::LocalServer).await.unwrap().pid, server_pid);
        assert_eq!(core.process_snapshot(ProcessKind::LocalRunner).await.unwrap().pid, runner_pid);
        let policy: toml_edit::DocumentMut = std::fs::read_to_string(&connection.runner_config)
            .unwrap()
            .parse()
            .unwrap();
        let roots = policy["policy"]["allowed_roots"].as_array().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(
            std::fs::canonicalize(roots.get(0).unwrap().as_str().unwrap()).unwrap(),
            std::fs::canonicalize(&project).unwrap()
        );
        // Upgrade/recovery must not turn the legacy displayed folder into a
        // prerequisite for the Server or Runner, even if it became unavailable.
        let target = observed_project(&mut core, "source", &cancellation).await?;
        core.switch_registered_project(&target.id, &target.revision, &cancellation).await?;
        let legacy_selection = core.config.project.clone();
        core.stop_local_runtime(&cancellation).await?;
        std::fs::rename(&project, directory.join("source-unavailable")).unwrap();
        core = DesktopCore::new(data.clone(), resources.clone())?;
        let recovered = core.resume_saved_runtime(&cancellation).await?;
        assert!(recovered.readiness.services_ready());
        assert_eq!(core.config.project, legacy_selection);
        assert_eq!(connection_from_config(&core.config).unwrap(), connection);
        Ok(())
    }
    .await;
    core.supervisor.lock().await.stop_all().await;
    drop(core);
    // This tree was uniquely created by this test; no installed runtime is used.
    if directory.exists() {
        std::fs::remove_dir_all(directory).unwrap();
    }
    result.unwrap();
}

async fn observed_project(
    core: &mut DesktopCore,
    name: &str,
    cancellation: &CancellationContext,
) -> DesktopResult<RegisteredProject> {
    core.registry_projects(cancellation)
        .await?
        .into_iter()
        .find(|p| Path::new(&p.path).file_name() == Some(std::ffi::OsStr::new(name)))
        .ok_or_else(|| project_error("fixture_project_missing"))
}

#[tokio::test]
#[ignore = "requires source-matched CLI/Server/Runner binaries; uses isolated loopback runtime and temporary projects"]
async fn native_project_management_preserves_files_and_empty_selection_across_restart() {
    assert!(
        std::env::var_os("WEBCODEX_DESKTOP_BIN_DIR").is_some(),
        "Supply verified runtime binaries explicitly"
    );
    let directory = std::env::temp_dir().join(format!(
        "webcodex-project-management-native-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let first = directory.join("first");
    let second = directory.join("second");
    let third = directory.join("third");
    for path in [&first, &second, &third] {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(
            path.join("keep.txt"),
            "user files survive project management",
        )
        .unwrap();
    }
    let data = directory.join("app-data");
    let resources = native_fixture_resources(&directory);
    let cancellation = CancellationContext::never();
    let mut core = DesktopCore::new(data.clone(), resources.clone()).unwrap();
    let result: DesktopResult<()> = async {
        core.configure_local_setup(Some(&first.to_string_lossy()), &cancellation)
            .await?;
        let initial = core.config.project.clone();
        let client = stored_runner_client_id(&core.config);
        let connection = connection_from_config(&core.config).unwrap();
        let server_pid = core
            .process_snapshot(ProcessKind::LocalServer)
            .await
            .unwrap()
            .pid;
        let runner_pid = core
            .process_snapshot(ProcessKind::LocalRunner)
            .await
            .unwrap()
            .pid;
        core.add_project(&second.to_string_lossy(), &cancellation)
            .await?;
        if core.config.project != initial {
            return Err(project_error("fixture_add_changed_selection"));
        }
        let target = observed_project(&mut core, "second", &cancellation).await?;
        core.switch_registered_project(&target.id, &target.revision, &cancellation)
            .await?;
        let target = observed_project(&mut core, "second", &cancellation).await?;
        core.mutate_project(
            &target.id,
            &target.revision,
            ProjectAction::Disable,
            "disable-second",
            &cancellation,
        )
        .await?;
        if core.config.project.is_some()
            || observed_project(&mut core, "second", &cancellation)
                .await?
                .enabled
        {
            return Err(project_error("fixture_disable_not_persisted"));
        }
        assert_eq!(
            core.process_snapshot(ProcessKind::LocalServer)
                .await
                .unwrap()
                .pid,
            server_pid
        );
        assert_eq!(
            core.process_snapshot(ProcessKind::LocalRunner)
                .await
                .unwrap()
                .pid,
            runner_pid
        );
        core.stop_local_runtime(&cancellation).await?;
        core = DesktopCore::new(data.clone(), resources.clone())?;
        core.resume_saved_runtime(&cancellation).await?;
        if core.config.project.is_some()
            || observed_project(&mut core, "second", &cancellation)
                .await?
                .enabled
        {
            return Err(project_error("fixture_restart_reactivated_project"));
        }
        if connection_from_config(&core.config).as_ref() != Some(&connection)
            || stored_runner_client_id(&core.config) != client
        {
            return Err(project_error("fixture_restart_replaced_connection"));
        }
        let target = observed_project(&mut core, "second", &cancellation).await?;
        core.mutate_project(
            &target.id,
            &target.revision,
            ProjectAction::Enable,
            "enable-second",
            &cancellation,
        )
        .await?;
        let target = observed_project(&mut core, "first", &cancellation).await?;
        core.switch_registered_project(&target.id, &target.revision, &cancellation)
            .await?;
        let target = observed_project(&mut core, "first", &cancellation).await?;
        core.mutate_project(
            &target.id,
            &target.revision,
            ProjectAction::Unregister,
            "remove-current",
            &cancellation,
        )
        .await?;
        let target = observed_project(&mut core, "second", &cancellation).await?;
        core.mutate_project(
            &target.id,
            &target.revision,
            ProjectAction::Unregister,
            "remove-last",
            &cancellation,
        )
        .await?;
        core.stop_local_runtime(&cancellation).await?;
        core = DesktopCore::new(data.clone(), resources.clone())?;
        core.resume_saved_runtime(&cancellation).await?;
        if core.config.project.is_some() || !core.registry_projects(&cancellation).await?.is_empty()
        {
            return Err(project_error("fixture_restart_registered_default_project"));
        }
        core.add_project(&third.to_string_lossy(), &cancellation)
            .await?;
        let target = observed_project(&mut core, "third", &cancellation).await?;
        core.switch_registered_project(&target.id, &target.revision, &cancellation)
            .await?;
        if core
            .config
            .project
            .as_ref()
            .and_then(|p| p.runtime_project_id.as_deref())
            != Some(target.id.as_str())
        {
            return Err(project_error("fixture_cannot_select_after_empty_registry"));
        }
        Ok(())
    }
    .await;
    let stopped = core.stop_local_runtime(&cancellation).await;
    drop(core);
    for path in [&first, &second, &third] {
        assert_eq!(
            std::fs::read_to_string(path.join("keep.txt")).unwrap(),
            "user files survive project management"
        );
    }
    // Owned unique temporary tree only; no user project or installed runtime is used.
    std::fs::remove_dir_all(&directory).unwrap();
    stopped.unwrap();
    result.unwrap();
}

fn project() -> RegisteredProject {
    RegisteredProject {
        id: "agent:runner:project".into(),
        name: "Project".into(),
        path: "C:/project".into(),
        revision: "sha256:current".into(),
        enabled: true,
        connected: true,
        agent_status: "online".into(),
        active_jobs: 0,
    }
}

#[test]
fn project_management_selection_uses_observed_revision_and_runner_status() {
    let p = project();
    assert!(select_observed(vec![p.clone()], &p.id, &p.revision).is_ok());
    assert_eq!(
        select_observed(vec![p.clone()], &p.id, "stale")
            .unwrap_err()
            .code,
        "project_revision_conflict"
    );
    assert!(select_observed(vec![p.clone()], "agent:other:project", &p.revision).is_err());
    let mut offline = p.clone();
    offline.agent_status = "offline".into();
    assert_eq!(
        select_observed(vec![offline], &p.id, &p.revision)
            .unwrap_err()
            .code,
        "project_agent_unavailable"
    );
}

#[test]
fn project_management_unknown_outcome_cannot_restore_automatic_selection() {
    for code in [
        "project_operation_indeterminate",
        "webcodex_command_failed",
        "webcodex_contract_invalid",
    ] {
        assert!(!definitely_rejected(&project_error(code)));
    }
    for code in [
        "project_revision_conflict",
        "project_active_jobs_conflict",
        "project_forbidden",
    ] {
        assert!(definitely_rejected(&project_error(code)));
    }
}

#[test]
fn project_management_request_cannot_supply_paths_credentials_or_unknown_actions() {
    for extra in ["token", "envFile", "serverUrl", "path"] {
        let mut value = serde_json::json!({"projectId":"agent:runner:project","expectedRevision":"sha256:current","action":"unregister"});
        value[extra] = serde_json::json!("caller-value");
        assert!(serde_json::from_value::<crate::commands::ProjectMutationRequest>(value).is_err());
    }
    assert!(serde_json::from_value::<ProjectAction>(serde_json::json!("delete_files")).is_err());
}

#[tokio::test]
async fn project_management_cleared_current_selection_survives_reopen_without_losing_connection() {
    let directory = std::env::temp_dir().join(format!(
        "webcodex-project-selection-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let config_path = directory.join("runner.toml");
    let token_path = directory.join("user-token");
    std::fs::write(&config_path, "client_id=\"runner\"").unwrap();
    std::fs::write(&token_path, "fixture-token").unwrap();
    let mut core = DesktopCore::new(directory.clone(), directory.clone()).unwrap();
    core.config.topology = Some(RuntimeTopology {
        experience: Experience::Full,
        server: ServerTopology::Local,
        runner: RunnerTopology::Local,
        exposure: Exposure::None,
        enrollment: Enrollment::ManagedPairing,
    });
    core.config.project = Some(ProjectSelection {
        path: directory.to_string_lossy().into(),
        allowed_root: directory.to_string_lossy().into(),
        is_git_repository: false,
        runtime_project_id: Some("agent:runner:project".into()),
    });
    core.config.runtime = Some(crate::models::StoredRuntime {
        native_host_id: None,
        server_url: "http://127.0.0.1:1".into(),
        server_env_file: None,
        runner_config: Some(config_path),
        user_token_file: Some(token_path),
        runner_client_id: None,
        project_id: Some("project".into()),
        runtime_project_id: Some("agent:runner:project".into()),
    });
    let before = connection_from_config(&core.config).unwrap();
    let client = stored_runner_client_id(&core.config).unwrap();
    clear_project_selection(&mut core.config, &client);
    core.save_config().await.unwrap();
    drop(core);
    let reopened = DesktopCore::new(directory.clone(), directory.clone()).unwrap();
    assert!(reopened.config.project.is_none());
    assert!(identity_from_config(&reopened.config).is_none());
    assert_eq!(connection_from_config(&reopened.config).unwrap(), before);
    assert_eq!(
        stored_runner_client_id(&reopened.config).as_deref(),
        Some("runner")
    );
    assert!(reopened
        .config
        .runtime
        .as_ref()
        .unwrap()
        .runtime_project_id
        .is_none());
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
}
