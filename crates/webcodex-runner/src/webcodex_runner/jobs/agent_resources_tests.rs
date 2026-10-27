use super::*;
use webcodex_core::runner_protocol::*;

fn path(name: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:/resource-fixture/{name}"))
    } else {
        PathBuf::from(format!("/resource-fixture/{name}"))
    }
}

fn profile() -> AgentExecutionResourceProfile {
    AgentExecutionResourceProfile {
        id: "worker-v1".into(),
        provider: None,
        reviewed_controls: None,
        user_https_proxy: false,
        codebuddy_config_directory: None,
        executable: path("worker.exe"),
        executable_sha256: "a".repeat(64),
        files: vec![
            AgentExecutionResourceFile {
                source: path("data.json"),
                destination: "input/data.json".into(),
                sha256: "b".repeat(64),
            },
            AgentExecutionResourceFile {
                source: path("runtime.dll"),
                destination: "bin/runtime.dll".into(),
                sha256: "c".repeat(64),
            },
        ],
    }
}

fn validated(profile: AgentExecutionResourceProfile) -> ValidatedResourceProfiles {
    validate_profiles(&AgentExecutionResourcesConfig {
        profiles: vec![profile],
    })
    .unwrap()
}

fn start(profiles: &ValidatedResourceProfiles) -> AgentExecutionStart {
    let process = AgentExecutionProcess {
        executable: format!("sha256:{}", "a".repeat(64)),
        args: vec!["--bounded-task".into()],
    };
    AgentExecutionStart {
        workspace_source: None,
        binding: AgentExecutionBinding {
            version: 1,
            project: "project".into(),
            requester: AgentExecutionRequester {
                enrollment_id: "requester-enrollment".into(),
                run: AgentRunRef {
                    agent_id: "requester".into(),
                    agent_run_id: "requester-run".into(),
                    generation: 1,
                },
                root_task_id: "root-task".into(),
                source_task_id: "source-task".into(),
                call_index: 0,
                cli_attempt_id: "attempt".into(),
            },
            executor: AgentExecutionExecutor {
                enrollment_id: "worker-enrollment".into(),
                run: AgentRunRef {
                    agent_id: "worker".into(),
                    agent_run_id: "worker-run".into(),
                    generation: 1,
                },
                task_id: "worker-task".into(),
                cli_session_scope_id: "worker-session".into(),
            },
            target: AgentExecutionTarget {
                client_id: "runner".into(),
                runner_instance_id: "instance".into(),
            },
            carrier: AgentExecutionCarrier::Job {
                job_id: "job".into(),
                start_request_id: "request".into(),
            },
            launch_intent_hash: compute_launch_intent_hash(&process, None, 30).unwrap(),
            resource_manifest_hash: profiles.manifest("worker-v1").unwrap().into(),
        },
        resource_set_id: "worker-v1".into(),
        process,
        stdin: None,
        timeout_secs: 30,
    }
}

#[test]
fn missing_profiles_do_not_enable_execution() {
    let config: AgentExecutionResourcesConfig = serde_json::from_str("{}").unwrap();
    let empty = validate_profiles(&config).unwrap();
    assert!(empty.is_empty());
    assert!(empty.descriptors().is_empty());
    assert_eq!(empty.manifest("worker-v1"), None);
    assert_eq!(
        empty.verify_sources().unwrap_err(),
        "agent_resources_profile_required"
    );
    let configured = validated(profile());
    assert!(selected_profile(&empty, &start(&configured)).is_err());
}

#[test]
fn resource_manifest_is_stable_for_file_order_and_binds_authorized_content() {
    let original = profile();
    let baseline = validated(original.clone());
    let mut reordered = original.clone();
    reordered.files.reverse();
    assert_eq!(
        baseline.manifest("worker-v1"),
        validated(reordered).manifest("worker-v1")
    );
    let mut changed = original.clone();
    changed.files[0].sha256 = "d".repeat(64);
    assert_ne!(
        baseline.manifest("worker-v1"),
        validated(changed).manifest("worker-v1")
    );
    let mut changed = original;
    changed.files[0].source = path("other.json");
    assert_ne!(
        baseline.manifest("worker-v1"),
        validated(changed).manifest("worker-v1")
    );
}

#[test]
fn descriptors_export_only_path_free_configuration_facts() {
    let profiles = validated(profile());
    let descriptors = profiles.descriptors();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].resource_set_id, "worker-v1");
    assert_eq!(
        descriptors[0].manifest_hash,
        profiles.manifest("worker-v1").unwrap()
    );
    assert_eq!(
        descriptors[0].executable_identity,
        format!("sha256:{}", "a".repeat(64))
    );
    let exported = serde_json::to_value(&descriptors[0]).unwrap();
    assert_eq!(exported.as_object().unwrap().len(), 4);
    assert_eq!(exported["project_workspace_copy"], cfg!(windows));
    let encoded = serde_json::to_string(&exported).unwrap();
    for private_value in ["resource-fixture", "worker.exe", "data.json", "runtime.dll"] {
        assert!(!encoded.contains(private_value));
    }
}

#[test]
fn descriptors_are_ordered_and_bind_the_configured_executable_digest() {
    let mut second = profile();
    second.id = "other-worker".into();
    second.executable_sha256 = "d".repeat(64);
    let profiles = validate_profiles(&AgentExecutionResourcesConfig {
        profiles: vec![profile(), second],
    })
    .unwrap();
    let descriptors = profiles.descriptors();
    assert_eq!(descriptors[0].resource_set_id, "other-worker");
    assert_eq!(
        descriptors[0].executable_identity,
        format!("sha256:{}", "d".repeat(64))
    );
    assert_eq!(descriptors[1].resource_set_id, "worker-v1");
    assert_ne!(descriptors[0].manifest_hash, descriptors[1].manifest_hash);
}

#[test]
fn provider_mapping_is_explicit_unique_and_manifest_bound() {
    let original = profile();
    let baseline = validated(original.clone());
    let mut first = original.clone();
    first.provider = Some(AgentCliProvider::WorkBuddy);
    let bound = validated(first.clone());
    assert_eq!(
        bound.descriptors()[0].provider,
        Some(AgentCliProvider::WorkBuddy)
    );
    assert_ne!(baseline.manifest("worker-v1"), bound.manifest("worker-v1"));
    let mut second = first.clone();
    second.id = "worker-v2".into();
    assert_eq!(
        validate_profiles(&AgentExecutionResourcesConfig {
            profiles: vec![first, second.clone()]
        })
        .unwrap_err(),
        "agent_resources_duplicate_provider"
    );
    second.provider = Some(AgentCliProvider::Antigravity);
    assert!(validate_profiles(&AgentExecutionResourcesConfig {
        profiles: vec![original, second]
    })
    .is_ok());
    assert_eq!(
        pin_catalog_executable(&baseline, AgentCliProvider::WorkBuddy).unwrap_err(),
        "agent_cli_provider_unconfigured"
    );
}

#[test]
fn absent_provider_preserves_the_previous_manifest_encoding() {
    let p = profile();
    let profiles = validated(p.clone());
    let files = &profiles.0[&p.id].files;
    let mut hash = Sha256::new();
    hash.update(b"webcodex.agent_execution.resources.v1\0");
    hash.update(serde_json::to_vec(&(1u32, &p.id, files)).unwrap());
    assert_eq!(
        profiles.manifest(&p.id).unwrap(),
        format!("{:x}", hash.finalize())
    );
    let config = serde_json::json!({"id":"worker-v1", "executable":path("worker.exe"), "executable_sha256":"a".repeat(64)});
    assert_eq!(
        serde_json::from_value::<AgentExecutionResourceProfile>(config.clone())
            .unwrap()
            .provider,
        None
    );
    let mut unsupported = config;
    unsupported["provider"] = serde_json::json!("arbitrary-cli");
    assert!(serde_json::from_value::<AgentExecutionResourceProfile>(unsupported).is_err());
}

#[test]
fn user_https_proxy_defaults_off_and_preserves_previous_payloads() {
    let parsed: AgentExecutionResourceProfile = serde_json::from_value(serde_json::json!({
        "id": "worker-v1", "executable": path("worker.exe"),
        "executable_sha256": "a".repeat(64)
    }))
    .unwrap();
    assert!(!parsed.user_https_proxy);
    for provider in [None, Some(AgentCliProvider::WorkBuddy)] {
        let mut p = profile();
        p.provider = provider;
        let profiles = validated(p.clone());
        let files = &profiles.0[&p.id].files;
        let encoded = match provider {
            None => serde_json::to_vec(&(1u32, &p.id, files)),
            Some(provider) => serde_json::to_vec(&(1u32, &p.id, files, provider)),
        }
        .unwrap();
        assert_eq!(
            profiles.manifest(&p.id).unwrap(),
            resource_payload_sha256(&encoded).unwrap()
        );
    }
    let profiles = validated(profile());
    assert_eq!(
        profiles.cleanup_requirements(&start(&profiles)).unwrap(),
        (false, false)
    );
}

#[test]
fn profile_rejects_duplicate_ids_and_non_native_or_unpinned_executable() {
    let config = AgentExecutionResourcesConfig {
        profiles: vec![profile(), profile()],
    };
    assert!(validate_profiles(&config).is_err());
    for executable in [PathBuf::from("worker.exe"), path("worker.cmd")] {
        let mut p = profile();
        p.executable = executable;
        assert!(validate_profiles(&AgentExecutionResourcesConfig { profiles: vec![p] }).is_err());
    }
    for hash in ["", "a", &"A".repeat(64), &"g".repeat(64)] {
        let mut p = profile();
        p.executable_sha256 = hash.into();
        assert!(validate_profiles(&AgentExecutionResourcesConfig { profiles: vec![p] }).is_err());
    }
}

#[test]
fn resource_destinations_reject_traversal_aliases_and_writable_scope() {
    for name in [
        "../outside",
        "input/../private",
        "input\\..\\private",
        "input//file",
        "input/./file",
        "C:/outside",
        "//host/share",
        "input/file:stream",
        "input/name.",
        "input/name ",
        "input/NUL.txt",
        "workspace/input",
        "input",
    ] {
        assert!(destination(Path::new(name)).is_err(), "accepted {name}");
    }
    assert_eq!(
        destination(Path::new("input/sub/data.json")).unwrap(),
        "input/sub/data.json"
    );
}

#[test]
fn resource_destinations_reject_case_and_parent_file_collisions() {
    for name in [
        "BIN/program.exe",
        "bin/PROGRAM.exe",
        "input/DATA.json",
        "input/data.json/child",
    ] {
        let mut p = profile();
        p.files.push(AgentExecutionResourceFile {
            source: path("extra"),
            destination: name.into(),
            sha256: "d".repeat(64),
        });
        assert!(
            validate_profiles(&AgentExecutionResourcesConfig { profiles: vec![p] }).is_err(),
            "accepted {name}"
        );
    }
}

#[test]
fn profile_rejects_model_supplied_environment_sid_and_directory_grants() {
    let base = serde_json::json!({"id":"worker-v1", "executable":path("worker.exe"), "executable_sha256":"a".repeat(64)});
    for key in [
        "sid",
        "env",
        "env_from_env",
        "allowed_roots",
        "writable_roots",
        "recursive",
    ] {
        let mut value = base.clone();
        value[key] = serde_json::json!({"secret":"should-not-be-used"});
        assert!(
            serde_json::from_value::<AgentExecutionResourceProfile>(value).is_err(),
            "accepted {key}"
        );
    }
    let file = serde_json::json!({"source":path("file"), "destination":"input/file", "sha256":"b".repeat(64), "recursive":true});
    assert!(serde_json::from_value::<AgentExecutionResourceFile>(file).is_err());
}

#[test]
fn selected_resource_requires_manifest_exact_executable_identity_and_valid_launch_hash() {
    let profiles = validated(profile());
    let request = start(&profiles);
    assert!(selected_profile(&profiles, &request).is_ok());
    let mut bad = request.clone();
    bad.resource_set_id = "unknown".into();
    assert!(selected_profile(&profiles, &bad).is_err());
    let mut bad = request.clone();
    bad.binding.resource_manifest_hash = "d".repeat(64);
    assert_eq!(
        selected_profile(&profiles, &bad).unwrap_err(),
        "agent_resources_manifest_mismatch"
    );
    for executable in [
        format!("sha256:{}", "d".repeat(64)),
        format!("SHA256:{}", "a".repeat(64)),
        path("worker.exe").to_str().unwrap().into(),
        path("other.exe").to_str().unwrap().into(),
    ] {
        let mut bad = request.clone();
        bad.process.executable = executable;
        bad.binding.launch_intent_hash =
            compute_launch_intent_hash(&bad.process, None, bad.timeout_secs).unwrap();
        assert_eq!(
            selected_profile(&profiles, &bad).unwrap_err(),
            "agent_resources_executable_mismatch"
        );
    }
    let mut bad = request;
    bad.process.args.push("changed-after-authorization".into());
    assert!(selected_profile(&profiles, &bad).is_err());
}

#[test]
fn complete_environment_contains_only_fixed_worker_paths_and_no_credentials() {
    let root = path("worker-one");
    let env = environment(&root, &path("Windows")).unwrap();
    assert_eq!(env.len(), 14);
    for (key, value) in &env {
        assert!(!key.contains("TOKEN") && !key.contains("KEY") && !key.contains("PROXY"));
        if !matches!(key.as_str(), "SystemRoot" | "WINDIR") {
            assert!(Path::new(value).starts_with(&root));
        }
    }
    assert_eq!(env["PATH"], root.join("bin").to_str().unwrap());
    assert_eq!(env["HOME"], root.join("home").to_str().unwrap());
    assert_ne!(env["WEBCODEX_SESSION_HOME"], env["HOME"]);
    assert_ne!(env["XDG_CACHE_HOME"], env["XDG_STATE_HOME"]);
    assert_ne!(env["TEMP"], env["LOCALAPPDATA"]);
    assert!(environment(&path("bad;directory"), &path("Windows")).is_err());
}

#[test]
fn worker_environment_does_not_reuse_another_workers_home_or_session() {
    let first = environment(&path("worker-one"), &path("Windows")).unwrap();
    let second = environment(&path("worker-two"), &path("Windows")).unwrap();
    for key in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CACHE_HOME",
        "TEMP",
        "WEBCODEX_SESSION_HOME",
    ] {
        assert_ne!(first[key], second[key]);
    }
}

#[cfg(windows)]
#[test]
fn windows_source_paths_reject_unc_devices_streams_and_lexical_traversal() {
    for text in [
        "\\\\server\\share\\worker.exe",
        "\\\\?\\C:\\worker.exe",
        "C:worker.exe",
        "C:/x/../worker.exe",
        "C:/x/./worker.exe",
        "C:/x//worker.exe",
        "C:/x/worker.exe:stream",
        "C:/x./worker.exe",
        "C:/x /worker.exe",
    ] {
        assert!(absolute_path(Path::new(text)).is_err(), "accepted {text}");
    }
    assert!(absolute_path(Path::new("C:/Program Files/Worker/worker.exe")).is_ok());
}

#[cfg(windows)]
#[test]
fn resource_root_initialization_rejects_unsafe_paths_before_filesystem_access() {
    for text in [
        "relative-resource-root",
        "\\\\server\\share\\resources",
        "\\\\?\\C:\\resources",
        "C:resources",
        "C:/parent/../resources",
        "C:/parent/./resources",
        "C:/parent/resources:stream",
        "C:/parent/resources.",
    ] {
        let expected = absolute_path(Path::new(text)).unwrap_err();
        assert_eq!(
            initialize_resource_root(Path::new(text)).unwrap_err(),
            expected
        );
        assert_eq!(
            pin_existing_resource_root(Path::new(text)).unwrap_err(),
            expected
        );
    }
}

#[cfg(not(windows))]
#[test]
fn non_windows_resource_preparation_is_explicitly_unsupported() {
    let profiles = validated(profile());
    assert_eq!(
        profiles.verify_sources().unwrap_err(),
        "agent_resources_platform_unsupported"
    );
    assert_eq!(
        prepare(&profiles, &start(&profiles), &path("job")).unwrap_err(),
        "agent_resources_platform_unsupported"
    );
    assert_eq!(
        initialize_resource_root(&path("job")).unwrap_err(),
        "agent_resources_platform_unsupported"
    );
    assert_eq!(
        pin_existing_resource_root(&path("job")).unwrap_err(),
        "agent_resources_platform_unsupported"
    );
    let mut configured = profile();
    configured.provider = Some(AgentCliProvider::WorkBuddy);
    assert_eq!(
        pin_catalog_executable(&validated(configured), AgentCliProvider::WorkBuddy).unwrap_err(),
        "agent_resources_platform_unsupported"
    );
    assert_eq!(
        read_delivery_bundle(&start(&profiles).binding, &path("job")).unwrap_err(),
        "agent_resources_platform_unsupported"
    );
    assert_eq!(
        cleanup_native_auth(&start(&profiles).binding, &path("job")).unwrap_err(),
        "agent_resources_platform_unsupported"
    );
}

#[cfg(windows)]
#[path = "agent_resource_snapshot_tests.rs"]
mod snapshots;

#[path = "agent_resource_controls_tests.rs"]
mod controls;
