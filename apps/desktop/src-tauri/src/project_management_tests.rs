use super::*;

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
    let resources = directory.join("test-resources");
    let cancellation = CancellationContext::never();
    let mut core = DesktopCore::new(data.clone(), resources.clone()).unwrap();
    let result: DesktopResult<()> = async {
        core.configure_local_setup(Some(&first.to_string_lossy()), &cancellation)
            .await?;
        let initial = core.config.project.clone();
        let client = stored_runner_client_id(&core.config);
        let connection = connection_from_config(&core.config).unwrap();
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
