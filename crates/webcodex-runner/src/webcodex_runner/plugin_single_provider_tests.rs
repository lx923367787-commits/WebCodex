use super::*;

fn connection_config(fixture: &Fixture) -> RunnerConfig {
    let plugins = fixture.manager.committed.lock().unwrap().config.clone();
    runner_config(plugins, ShellConfig::default(), fixture._temp.path())
}

fn count_marker(path: &Path, marker: &str) -> usize {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| *line == marker)
        .count()
}

fn call_current(manager: &PluginManager, provider: &PluginProviderView) -> PluginGatewayResponse {
    let schema = current_tools(manager, provider)[0].schema_observation();
    manager.handle(PluginGatewayRequest::ToolsCall {
        provider_id: provider.provider_id.clone(),
        provider_instance_id: provider.provider_instance_id.clone(),
        name: "echo".into(),
        arguments: json!({"value":"single-provider"}),
        expected_schema: schema,
    })
}

#[test]
fn single_provider_connect_reuses_unchanged_and_updates_only_target() {
    let fixture = Fixture::new("normal", 10);
    let mut config = connection_config(&fixture);
    let mut second = config.plugins.providers[0].clone();
    second.id = "second".into();
    let second_marker = fixture._temp.path().join("second.log");
    second.args[1] = second_marker.to_string_lossy().into_owned();
    config.plugins.providers.push(second);
    let candidate = fixture.manager.prepare(&config, "second").unwrap();
    assert!(candidate.changed());
    assert_eq!(candidate.tool_count(), 1);
    let second = fixture.manager.commit(candidate);
    assert_eq!(fixture.marker_count("start"), 1);
    assert_eq!(count_marker(&second_marker, "start"), 1);

    let unchanged = fixture.manager.prepare(&config, "second").unwrap();
    assert!(!unchanged.changed());
    assert_eq!(
        fixture.manager.commit(unchanged).provider_instance_id,
        second.provider_instance_id
    );
    assert_eq!(count_marker(&second_marker, "start"), 1);

    config.plugins.providers[0].name = "Updated first".into();
    let replacement = fixture.manager.prepare(&config, "fake").unwrap();
    assert!(replacement.changed());
    let updated = fixture.manager.commit(replacement);
    assert_ne!(
        updated.provider_instance_id,
        fixture.provider.provider_instance_id
    );
    assert_eq!(updated.name, "Updated first");
    assert_eq!(fixture.marker_count("start"), 2);
    let still_second = current_providers(&fixture.manager)
        .into_iter()
        .find(|provider| provider.provider_id == "second")
        .unwrap();
    assert_eq!(
        still_second.provider_instance_id,
        second.provider_instance_id
    );
    assert_eq!(count_marker(&second_marker, "start"), 1);
    assert!(call_current(&fixture.manager, &still_second)
        .error
        .is_none());

    // A later generic reload recognizes the exact committed provider inputs;
    // the incremental operation has not manufactured stale environment state.
    fixture
        .manager
        .apply_config_candidate_and_then(&config, || {})
        .unwrap();
    assert_eq!(fixture.marker_count("start"), 2);
    assert_eq!(count_marker(&second_marker, "start"), 1);
}

#[test]
fn single_provider_failed_or_discarded_candidate_preserves_active_and_serializes_save() {
    let fixture = Fixture::new("normal", 10);
    let mut config = connection_config(&fixture);
    config.plugins.providers[0].args[0] = "bad_version".into();
    assert!(
        matches!(fixture.manager.prepare(&config, "fake"), Err(code) if code == "plugin_protocol_version_mismatch")
    );
    assert_eq!(
        current_providers(&fixture.manager)[0].provider_instance_id,
        fixture.provider.provider_instance_id
    );
    assert!(fixture.call().error.is_none());

    config.plugins.providers[0].args[0] = "candidate_discard".into();
    let candidate = fixture.manager.prepare_reconnect(&config, "fake").unwrap();
    assert!(candidate.changed());
    assert!(
        matches!(fixture.manager.prepare(&config, "fake"), Err(code) if code == "plugin_reload_busy")
    );
    assert!(
        matches!(fixture.manager.prepare_remove("fake"), Err(code) if code == "plugin_reload_busy")
    );
    assert_eq!(
        fixture
            .manager
            .handle(PluginGatewayRequest::Reload)
            .error
            .unwrap()
            .code,
        "plugin_reload_busy"
    );
    let candidate_pid = fixture.marker_pid("candidate-pid:").unwrap();
    drop(candidate); // Simulates the caller failing to persist its configuration.
    assert!(wait_until(Duration::from_secs(2), || {
        !crate::job_manager_tests::process_running(candidate_pid)
    }));
    assert_eq!(
        current_providers(&fixture.manager)[0].provider_instance_id,
        fixture.provider.provider_instance_id
    );
    assert!(fixture.call().error.is_none());
    config.plugins.providers[0].args[0] = "normal".into();
    let unchanged = fixture.manager.prepare(&config, "fake").unwrap();
    assert!(!unchanged.changed());
}

#[test]
fn single_provider_replacement_and_remove_drain_started_calls_and_reject_stale_calls() {
    for remove in [false, true] {
        let fixture = Fixture::new("single_block_call", 10);
        let config = connection_config(&fixture);
        let manager = Arc::clone(&fixture.manager);
        let provider = fixture.provider.clone();
        let old = manager
            .resolve_provider(&provider.provider_id, &provider.provider_instance_id)
            .unwrap();
        let schema = fixture.schema.clone().unwrap();
        let running = std::thread::spawn(move || call_current(&manager, &provider));
        assert!(wait_until(Duration::from_secs(3), || fixture
            .marker_count("call")
            == 1));

        if remove {
            let candidate = fixture.manager.prepare_remove("fake").unwrap();
            assert!(candidate.changed());
            assert!(fixture.manager.commit_remove(candidate));
            assert!(current_providers(&fixture.manager).is_empty());
        } else {
            let candidate = fixture.manager.prepare_reconnect(&config, "fake").unwrap();
            let replacement = fixture.manager.commit(candidate);
            assert_ne!(
                replacement.provider_instance_id,
                fixture.provider.provider_instance_id
            );
        }
        assert_eq!(fixture.list().error.unwrap().code, "stale_plugin_provider");
        // Even an Arc acquired before replacement cannot start new business.
        assert!(
            matches!(old.call_tool("echo", json!({"value":"late"}), &schema), Err(error) if error.dispatch_state == PluginDispatchState::NotStarted)
        );
        fs::write(fixture.marker.with_extension("release"), b"continue").unwrap();
        let result = running.join().unwrap();
        assert!(
            result.error.is_none(),
            "in-flight call was interrupted: {:?}",
            result.error
        );
        assert_eq!(fixture.marker_count("call"), 1);
        drop(old);
        if !remove {
            let replacement = current_providers(&fixture.manager).remove(0);
            assert!(call_current(&fixture.manager, &replacement).error.is_none());
            assert_eq!(fixture.marker_count("call"), 2);
        }
    }
}

#[test]
fn single_provider_failed_connection_recovers_only_for_a_new_call() {
    let fixture = Fixture::new("crash", 10);
    let config = connection_config(&fixture);
    let failed = fixture.call();
    assert_eq!(failed.dispatch_state, PluginDispatchState::OutcomeUnknown);
    assert_eq!(fixture.marker_count("call"), 1);
    let candidate = fixture.manager.prepare(&config, "fake").unwrap();
    assert!(candidate.changed());
    let replacement = fixture.manager.commit(candidate);
    assert_ne!(
        replacement.provider_instance_id,
        fixture.provider.provider_instance_id
    );
    assert_eq!(
        fixture.marker_count("call"),
        1,
        "connection recovery replayed business"
    );
    let next = call_current(&fixture.manager, &replacement);
    assert_eq!(next.dispatch_state, PluginDispatchState::OutcomeUnknown);
    assert_eq!(fixture.marker_count("call"), 2);
}

#[test]
fn single_provider_later_discovery_recovers_saved_connection_without_replaying_call() {
    let fixture = Fixture::new("crash", 10);
    assert_eq!(
        fixture.call().dispatch_state,
        PluginDispatchState::OutcomeUnknown
    );
    write_runner_toml(
        &fixture._temp.path().join("runner.toml"),
        fixture._temp.path(),
        &fixture._fake.path,
        &fixture.marker,
        "normal",
    );
    let recovered = fixture.list();
    assert_eq!(recovered.error.unwrap().code, "stale_plugin_provider");
    assert_eq!(fixture.marker_count("call"), 1);
    let current = current_providers(&fixture.manager).remove(0);
    assert_eq!(current.status, "ready");
    assert_ne!(
        current.provider_instance_id,
        fixture.provider.provider_instance_id
    );
    assert_eq!(current_tools(&fixture.manager, &current).len(), 1);
    assert!(call_current(&fixture.manager, &current).error.is_none());
    assert_eq!(fixture.marker_count("call"), 2);
}

#[test]
fn single_provider_literals_override_environment_and_filter_runner_credentials() {
    let fixture = Fixture::new("normal", 10);
    let mut config = connection_config(&fixture);
    let provider = &mut config.plugins.providers[0];
    provider.args[0] = "execution_context".into();
    provider
        .args
        .push(fixture._temp.path().to_string_lossy().into_owned());
    provider
        .env
        .insert("WEBCODEX_PLUGIN_TEST_ENV".into(), "profile-ready".into());
    // This private caller bypasses config validation deliberately: the runtime
    // still filters reserved names rather than exporting Runner credentials.
    provider
        .env
        .insert("WEBCODEX_AGENT_TOKEN".into(), "must-not-export".into());
    let candidate = fixture.manager.prepare(&config, "fake").unwrap();
    let installed = fixture.manager.commit(candidate);
    assert_eq!(fixture.marker_count("profile-env-ok"), 1);
    assert_eq!(fixture.marker_count("sensitive-env-cleared"), 1);
    assert_eq!(fixture.marker_count("sensitive-env-leaked"), 0);
    assert!(call_current(&fixture.manager, &installed).error.is_none());
    assert!(!fixture.manager.prepare(&config, "fake").unwrap().changed());
    config.plugins.providers[0]
        .env
        .insert("WEBCODEX_PLUGIN_TEST_ENV".into(), "changed".into());
    assert!(fixture.manager.prepare(&config, "fake").unwrap().changed());
}

#[test]
fn single_provider_management_leaves_managed_runtime_lifecycle_to_existing_owner() {
    let fixture = Fixture::new("normal", 10);
    let mut config = connection_config(&fixture);
    let marker = fixture._temp.path().join("lifecycle.log");
    config.plugins.providers[0].id = "chat-agents".into();
    config.plugins.providers[0].args =
        vec!["lifecycle".into(), marker.to_string_lossy().into_owned()];
    config.plugins.providers[0].runtime_lifecycle =
        Some(crate::webcodex_runner::config::PluginRuntimeLifecycle::ChatAgentsV1);
    let manager = PluginManager::new(&config, fixture._temp.path().join("managed.toml"));
    let original = current_providers(&manager).remove(0);
    assert!(
        matches!(manager.prepare_reconnect(&config, "chat-agents"), Err(code) if code == "plugin_runtime_lifecycle_managed")
    );
    assert!(
        matches!(manager.prepare_remove("chat-agents"), Err(code) if code == "plugin_runtime_lifecycle_managed")
    );
    config.plugins.providers[0].runtime_lifecycle = None;
    assert!(
        matches!(manager.prepare(&config, "chat-agents"), Err(code) if code == "plugin_runtime_lifecycle_managed")
    );
    assert_eq!(
        current_providers(&manager)[0].provider_instance_id,
        original.provider_instance_id
    );
    assert_eq!(count_marker(&marker, "start"), 1);
}
