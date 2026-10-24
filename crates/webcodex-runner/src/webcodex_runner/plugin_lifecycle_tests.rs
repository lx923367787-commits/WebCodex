//! Synthetic stdio lifecycle only; never invokes the real chat-agents provider.
use super::*;

struct LifecycleFixture {
    manager: Arc<PluginManager>,
    config: RunnerConfig,
    config_path: PathBuf,
    marker: PathBuf,
    _fake: Arc<FakeBinary>,
    _temp: TempDir,
}

impl LifecycleFixture {
    fn new(scenario: &str, controlled: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let fake = fake_binary();
        let marker = temp.path().join("lifecycle.log");
        let config_path = temp.path().join("runner.toml");
        let provider: PluginProviderConfig = serde_json::from_value(json!({
            "id": if controlled { "chat-agents" } else { "fake" },
            "name": "Synthetic lifecycle fixture",
            "command": fake.path.to_string_lossy(),
            "args": [scenario, marker.to_string_lossy().as_ref()],
            "cwd": temp.path().to_string_lossy(),
            "timeout_secs": 2,
            "runtime_lifecycle": if controlled { Some("chat_agents_v1") } else { None },
        }))
        .unwrap();
        let config = runner_config(
            PluginConfig {
                request_timeout_secs: 2,
                providers: vec![provider],
            },
            ShellConfig::default(),
            temp.path(),
        );
        let manager = Arc::new(PluginManager::new(&config, config_path.clone()));
        let fixture = Self {
            manager,
            config,
            config_path,
            marker,
            _fake: fake,
            _temp: temp,
        };
        fixture.write_candidate(scenario, controlled);
        fixture
    }

    fn write_candidate(&self, scenario: &str, controlled: bool) {
        let selected = if controlled {
            "runtime_lifecycle = \"chat_agents_v1\"\n"
        } else {
            ""
        };
        let id = if controlled { "chat-agents" } else { "fake" };
        fs::write(&self.config_path, format!(
            "server_url = \"http://127.0.0.1:8000\"\ntoken = \"test-token\"\nclient_id = \"plugin-test\"\nproject_registry_dir = {:?}\n[plugins]\nrequest_timeout_secs = 2\n[[plugins.providers]]\nid = \"{id}\"\nname = \"Synthetic lifecycle fixture\"\ncommand = {:?}\nargs = [{scenario:?}, {:?}]\ncwd = {:?}\n{selected}",
            self._temp.path().to_string_lossy(), self._fake.path.to_string_lossy(),
            self.marker.to_string_lossy(), self._temp.path().to_string_lossy(),
        )).unwrap();
    }

    fn entry(&self) -> Arc<ProviderEntry> {
        self.manager
            .committed
            .lock()
            .unwrap()
            .providers
            .values()
            .next()
            .unwrap()
            .clone()
    }

    fn events(&self) -> Vec<String> {
        fs::read_to_string(&self.marker)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn count(&self, prefix: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| event.starts_with(prefix))
            .count()
    }

    fn candidate(&self) -> RunnerConfig {
        let mut config = self.config.clone();
        // Change configuration without changing the private ABI or instance.
        config.plugins.providers[0].name = "Synthetic replacement".into();
        config
    }
}

impl Drop for LifecycleFixture {
    fn drop(&mut self) {
        self.manager.shutdown();
    }
}

fn call(entry: &ProviderEntry) -> Result<PluginToolResult, ProviderFailure> {
    let schema = entry.catalog.get().expect("admitted catalog").tools()[0].schema_observation();
    entry.call_tool("echo", json!({"value":"synthetic"}), &schema)
}

#[test]
fn plugin_lifecycle_candidate_and_check_never_activate() {
    let fixture = LifecycleFixture::new("lifecycle", true);
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("list"), 1);
    assert_eq!(fixture.count("runtime-activate:"), 0);
    assert_eq!(
        call(&fixture.entry()).err().unwrap().code,
        "plugin_runtime_not_active"
    );
    let checked = fixture.manager.handle(PluginGatewayRequest::Check {
        provider_id: "chat-agents".into(),
    });
    let Some(PluginGatewayResponsePayload::Checked { report }) = checked.payload else {
        panic!("check failed")
    };
    assert!(report.ready);
    assert_eq!(fixture.count("initialize"), 2);
    assert_eq!(fixture.count("runtime-activate:"), 0);
    assert_eq!(fixture.count("call"), 0);
    assert!(!fixture.entry().runtime_allows_business());
}

#[test]
fn plugin_lifecycle_startup_activates_committed_instance_once() {
    let fixture = LifecycleFixture::new("lifecycle", true);
    let entry = fixture.entry();
    let original = entry.instance_id.clone();
    assert!(!entry.runtime_allows_business());
    assert!(fixture
        .manager
        .activate_committed_startup()
        .unwrap()
        .activation_failures
        .is_empty());
    assert!(fixture
        .manager
        .activate_committed_startup()
        .unwrap()
        .activation_failures
        .is_empty());
    assert_eq!(fixture.entry().instance_id, original);
    assert_eq!(fixture.count(&format!("runtime-activate:{original}")), 1);
    assert!(entry.runtime_allows_business());
    assert!(call(&entry).is_ok());
    assert_eq!(fixture.count("call"), 1);
}

#[test]
fn plugin_lifecycle_hot_commit_callback_precedes_activation() {
    let fixture = LifecycleFixture::new("lifecycle", true);
    fixture.manager.activate_committed_startup().unwrap();
    let manager = Arc::clone(&fixture.manager);
    let candidate = fixture.candidate();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        manager.apply_config_candidate_and_then(&candidate, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(4)).unwrap();
    let next = fixture.entry();
    assert!(!next.runtime_allows_business());
    assert_eq!(call(&next).err().unwrap().code, "plugin_runtime_not_active");
    assert_eq!(fixture.count("runtime-activate:"), 1);
    release_tx.send(()).unwrap();
    assert!(worker
        .join()
        .unwrap()
        .unwrap()
        .activation_failures
        .is_empty());
    assert_eq!(fixture.count("runtime-activate:"), 2);
    assert!(next.runtime_allows_business());
}

#[test]
fn plugin_lifecycle_reload_stops_old_arc_before_new_activation() {
    let fixture = LifecycleFixture::new("lifecycle", true);
    fixture.manager.activate_committed_startup().unwrap();
    let old = fixture.entry();
    let old_id = old.instance_id.clone();
    let response = fixture.manager.handle(PluginGatewayRequest::Reload);
    let Some(PluginGatewayResponsePayload::Reloaded { failures, .. }) = response.payload else {
        panic!("reload failed")
    };
    assert!(failures.is_empty());
    let new = fixture.entry();
    assert_ne!(old_id, new.instance_id);
    assert!(!old.runtime_allows_business());
    assert!(call(&old).is_err());
    assert!(call(&new).is_ok());
    let events = fixture.events();
    let stopped = events
        .iter()
        .position(|line| line == &format!("runtime-stopped:{old_id}"))
        .unwrap();
    let active = events
        .iter()
        .position(|line| line == &format!("runtime-active:{}", new.instance_id))
        .unwrap();
    assert!(stopped < active);
    assert_eq!(fixture.count("call"), 1);
}

#[test]
fn plugin_lifecycle_missing_capability_and_wrong_instance_fail_closed() {
    for scenario in ["normal", "lifecycle_bad_capability"] {
        let fixture = LifecycleFixture::new(scenario, true);
        assert!(fixture.entry().failed.load(Ordering::SeqCst));
        assert_eq!(
            fixture.entry().error_code.lock().unwrap().as_deref(),
            Some("plugin_runtime_capability_missing")
        );
        assert_eq!(fixture.count("runtime-activate:"), 0);
    }
    let fixture = LifecycleFixture::new("lifecycle_wrong_instance", true);
    let report = fixture.manager.activate_committed_startup().unwrap();
    assert_eq!(report.activation_failures.len(), 1);
    assert_eq!(
        report.activation_failures[0].code,
        "plugin_runtime_response_invalid"
    );
    assert!(!fixture.entry().runtime_allows_business());
    assert!(call(&fixture.entry()).is_err());
}

#[test]
fn plugin_lifecycle_lost_activation_ack_is_never_resent() {
    let fixture = LifecycleFixture::new("lifecycle_lost_activate", true);
    let instance = fixture.entry().instance_id.clone();
    assert_eq!(
        fixture
            .manager
            .activate_committed_startup()
            .unwrap()
            .activation_failures
            .len(),
        1
    );
    let _ = fixture.manager.activate_committed_startup();
    assert_eq!(fixture.entry().instance_id, instance);
    assert_eq!(fixture.count("runtime-activate:"), 1);
    assert_eq!(fixture.count("runtime-active:"), 1);
    assert!(!fixture.entry().runtime_allows_business());
    assert!(call(&fixture.entry()).is_err());
    assert_eq!(fixture.count("call"), 0);
}

#[test]
fn plugin_lifecycle_unknown_stop_requires_all_native_exit_observations() {
    use runtime_lifecycle::{confirms_native_stop, NativeStopObservation};
    assert!(!confirms_native_stop(NativeStopObservation::MissingChild));
    for termination_succeeded in [false, true] {
        for tree_empty in [false, true] {
            for leader_exited in [false, true] {
                assert_eq!(
                    confirms_native_stop(NativeStopObservation::Observed {
                        termination_succeeded,
                        tree_empty,
                        leader_exited
                    }),
                    termination_succeeded && tree_empty && leader_exited
                );
            }
        }
    }
    // This is a pure evidence truth table, not a claim of physical process exit.
}

#[cfg(windows)]
#[test]
fn plugin_lifecycle_unknown_stop_uses_confirmed_native_exit_before_replacement() {
    let fixture = LifecycleFixture::new("lifecycle_stop_unknown", true);
    fixture.manager.activate_committed_startup().unwrap();
    let pid = fixture
        .events()
        .iter()
        .find_map(|line| line.strip_prefix("lifecycle-pid:")?.parse::<u32>().ok())
        .unwrap();
    let candidate = fixture.candidate();
    let report = fixture
        .manager
        .apply_config_candidate_and_then(&candidate, || {})
        .unwrap();
    assert!(report.activation_failures.is_empty());
    assert!(wait_until(Duration::from_secs(2), || {
        !crate::job_manager_tests::process_running(pid)
    }));
    assert_eq!(fixture.count("runtime-activate:"), 2);
    assert_eq!(fixture.count("runtime-stopped:"), 0);
}

#[test]
fn plugin_lifecycle_shutdown_during_hot_commit_prevents_late_activation() {
    let fixture = LifecycleFixture::new("lifecycle", true);
    fixture.manager.activate_committed_startup().unwrap();
    let manager = Arc::clone(&fixture.manager);
    let candidate = fixture.candidate();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        manager.apply_config_candidate_and_then(&candidate, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(4)).unwrap();
    fixture.manager.shutdown();
    release_tx.send(()).unwrap();
    let _ = worker.join().unwrap();
    assert_eq!(fixture.count("runtime-activate:"), 1);
    assert_eq!(fixture.count("runtime-stopped:"), 1);
    assert!(!fixture.entry().runtime_allows_business());
}

#[test]
fn plugin_lifecycle_shutdown_stops_active_runtime_and_none_remains_ordinary() {
    let fixture = LifecycleFixture::new("lifecycle", true);
    fixture.manager.activate_committed_startup().unwrap();
    fixture.manager.shutdown();
    fixture.manager.shutdown();
    assert_eq!(fixture.count("runtime-deactivate:"), 1);
    assert_eq!(fixture.count("runtime-stopped:"), 1);
    let ordinary = LifecycleFixture::new("lifecycle", false);
    assert!(call(&ordinary.entry()).is_ok());
    ordinary.manager.activate_committed_startup().unwrap();
    ordinary.manager.shutdown();
    assert_eq!(ordinary.count("runtime-activate:"), 0);
    assert_eq!(ordinary.count("runtime-deactivate:"), 0);
    assert_eq!(ordinary.count("call"), 1);
}
