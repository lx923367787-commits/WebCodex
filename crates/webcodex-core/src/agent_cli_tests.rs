use super::*;

#[test]
fn manifest_v2_paths_only_preserves_v1_digest_requirements() {
    let v2 = parse_delivery_manifest(
        br#"{"version":2,"summary":"done","files":[{"path":"files/result.txt"}]}"#,
    )
    .unwrap();
    assert_eq!(v2.files[0].path, "files/result.txt");
    assert!(v2.files[0].expected.is_none());
    let v1 = format!(
        r#"{{"version":1,"summary":"done","files":[{{"path":"files/result.txt","sha256":"{}","bytes":8}}]}}"#,
        "a".repeat(64)
    );
    assert_eq!(
        parse_delivery_manifest(v1.as_bytes()).unwrap().files[0].expected,
        Some((8, "a".repeat(64)))
    );
    for bytes in [
        &br#"{"version":3,"summary":"done","files":[]}"#[..],
        &br#"{"version":2,"summary":"","files":[]}"#[..],
        &br#"{"version":2,"summary":"done","files":[{"path":"files/../auth"}]}"#[..],
        &br#"{"version":2,"summary":"done","files":[{"path":"files/x","bytes":1}]}"#[..],
        &br#"{"version":1,"summary":"done","files":[{"path":"files/x"}]}"#[..],
    ] {
        assert!(parse_delivery_manifest(bytes).is_err());
    }
}
use crate::runner_operation::{RunnerInvocationMetadata, RunnerOperation};
use crate::runner_protocol::*;
use serde_json::json;

fn binding() -> AgentExecutionBinding {
    AgentExecutionBinding {
        version: 1,
        project: "project".into(),
        requester: AgentExecutionRequester {
            enrollment_id: "source-enrollment".into(),
            run: AgentRunRef {
                agent_id: "source".into(),
                agent_run_id: "source-run".into(),
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
            cli_session_scope_id: "worker-private-scope".into(),
        },
        target: target(),
        carrier: AgentExecutionCarrier::Job {
            job_id: "job".into(),
            start_request_id: "start-request".into(),
        },
        launch_intent_hash: "a".repeat(64),
        resource_manifest_hash: "b".repeat(64),
    }
}
fn target() -> AgentExecutionTarget {
    AgentExecutionTarget {
        client_id: "runner".into(),
        runner_instance_id: "instance".into(),
    }
}
fn metadata() -> RunnerInvocationMetadata {
    RunnerInvocationMetadata {
        request_id: "read-request".into(),
        client_id: "runner".into(),
        requested_by: "trusted-controller".into(),
        created_at: 1,
    }
}
fn query() -> AgentCliCatalogQuery {
    AgentCliCatalogQuery {
        version: 1,
        target: target(),
        provider: AgentCliProvider::WorkBuddy,
        deadline_at_ms: 40_000,
    }
}
fn resource() -> AgentExecutionResourceDescriptor {
    AgentExecutionResourceDescriptor {
        host_cli: None,
        project_workspace_copy: false,
        reviewed_controls: None,
        resource_set_id: "workbuddy-resource".into(),
        manifest_hash: "b".repeat(64),
        executable_identity: format!("sha256:{}", "c".repeat(64)),
        provider: Some(AgentCliProvider::WorkBuddy),
    }
}
fn catalog() -> AgentCliCatalog {
    let r = resource();
    AgentCliCatalog {
        version: 1,
        target: target(),
        resource_set_id: r.resource_set_id,
        resource_manifest_hash: r.manifest_hash,
        executable_identity: r.executable_identity,
        provider: AgentCliProvider::WorkBuddy,
        cli_version: "1.2.3".into(),
        models: vec!["model-1".into()],
        efforts: vec!["high".into()],
        model_efforts: Default::default(),
        source: "installed_help".into(),
        status: "ready".into(),
        message: "metadata only".into(),
        observed_at_ms: 1_000,
        expires_at_ms: 61_000,
        account_availability_verified: false,
        nested_delegation_verified_disabled: false,
    }
}
fn bundle() -> AgentCliDeliveryBundle {
    let body = "中文 report\r\nexact newline\n";
    AgentCliDeliveryBundle {
        version: 1,
        binding_sha256: binding().binding_hash().unwrap(),
        summary: "explicit task delivery".into(),
        files: vec![AgentCliDeliveryFile {
            path: "files/report.txt".into(),
            sha256: sha256(body.as_bytes()),
            bytes: body.len() as u64,
            text: body.into(),
        }],
    }
}

#[test]
fn agent_cli_catalog_binds_provider_resource_and_exact_instance() {
    catalog().validate_for(&query(), &resource()).unwrap();
    let mut bad = catalog();
    bad.target.runner_instance_id = "replacement".into();
    assert!(bad.validate_for(&query(), &resource()).is_err());
    let mut bad = catalog();
    bad.source = "live_catalog".into();
    assert!(bad.validate_for(&query(), &resource()).is_err());
    bad.status = "error".into();
    assert!(bad.validate_for(&query(), &resource()).is_err());
    let mut bad = resource();
    bad.provider = Some(AgentCliProvider::Antigravity);
    assert!(catalog().validate_for(&query(), &bad).is_err());
    let mut bad = resource();
    bad.provider = None;
    assert!(catalog().validate_for(&query(), &bad).is_err());
    let mut bad = catalog();
    bad.executable_identity = format!("sha256:{}", "d".repeat(64));
    assert!(bad.validate_for(&query(), &resource()).is_err());
}
#[test]
fn agent_cli_metadata_cannot_assert_execution_or_account_readiness() {
    let mut bad = catalog();
    bad.account_availability_verified = true;
    assert!(bad.validate_for(&query(), &resource()).is_err());
    bad.account_availability_verified = false;
    bad.nested_delegation_verified_disabled = true;
    assert!(bad.validate_for(&query(), &resource()).is_err());
    let mut bad = catalog();
    bad.models.clear();
    assert!(bad.validate_for(&query(), &resource()).is_err());
    bad.status = "error".into();
    bad.source = "unavailable".into();
    bad.validate_for(&query(), &resource()).unwrap();
}
#[test]
fn agent_cli_catalog_bounds_duplicates_controls_and_expiry() {
    let mut bad = catalog();
    bad.models = vec!["model".into(); 257];
    assert!(bad.validate_for(&query(), &resource()).is_err());
    bad.models = vec!["model".into(); 2];
    assert!(bad.validate_for(&query(), &resource()).is_err());
    bad.models = vec!["model\nsecret".into()];
    assert!(bad.validate_for(&query(), &resource()).is_err());
    let mut bad = catalog();
    bad.expires_at_ms += 1;
    assert!(bad.validate_for(&query(), &resource()).is_err());
}
#[test]
fn agent_cli_fixed_query_rejects_commands_and_paths_but_accepts_registry_ids() {
    let mut value = serde_json::to_value(query()).unwrap();
    value["command"] = json!("anything");
    assert!(serde_json::from_value::<AgentCliCatalogQuery>(value).is_err());
    let mut value = serde_json::to_value(query()).unwrap();
    value["provider"] = json!("arbitrary-program");
    assert_eq!(
        serde_json::from_value::<AgentCliCatalogQuery>(value)
            .unwrap()
            .provider
            .id(),
        "arbitrary-program"
    );
    for invalid in ["../program", "cmd.exe /c command", "", "a;b"] {
        let mut value = serde_json::to_value(query()).unwrap();
        value["provider"] = json!(invalid);
        assert!(serde_json::from_value::<AgentCliCatalogQuery>(value).is_err());
    }
    let mut bad = query();
    bad.deadline_at_ms = 0;
    assert!(bad.validate().is_err());
}

#[test]
fn cli_inventory_has_a_scoped_read_only_codec_and_expiring_typed_results() {
    let query = AgentCliInventoryQuery {
        version: 1,
        native_only: false,
        target: target(),
        deadline_at_ms: 40_000,
        provider: None,
    };
    let wire = RunnerRequest::from_operation(
        metadata(),
        RunnerOperation::AgentCliInventory(query.clone()),
    )
    .unwrap();
    assert!(matches!(
        wire.decode_operation().unwrap(),
        RunnerOperation::AgentCliInventory(_)
    ));
    let mut injected = wire.clone();
    injected.command = "arbitrary execution".into();
    assert!(injected.decode_operation().is_err());
    let mut retargeted = wire;
    retargeted.client_id = "another-runner".into();
    assert!(retargeted.decode_operation().is_err());
    let mut inventory = AgentCliInventory {
        version: 1,
        target: target(),
        observed_at_ms: 1_000,
        expires_at_ms: 61_000,
        capabilities: None,
        installations: vec![AgentCliInstallation {
            command: "third-party-cli".into(),
            execution_ready: Some(false),
            execution_mode: None,
            reason_code: Some("cli_resource_not_registered".into()),
            provider_id: "third-party-cli".into(),
            label: "Third Party CLI".into(),
            installed: true,
            native: true,
            adapter_available: false,
            status: "adapter_required".into(),
            message: "Needs a local adapter".into(),
        }],
    };
    inventory.validate_for(&query, 2_000).unwrap();
    assert!(inventory.validate_for(&query, 61_000).is_err());
    inventory
        .installations
        .push(inventory.installations[0].clone());
    assert!(inventory.validate_for(&query, 2_000).is_err());
}
#[test]
fn agent_cli_control_codec_cannot_carry_generic_execution_or_retarget() {
    let wire = RunnerRequest::from_operation(metadata(), RunnerOperation::AgentCliCatalog(query()))
        .unwrap();
    assert!(matches!(
        wire.decode_operation().unwrap(),
        RunnerOperation::AgentCliCatalog(_)
    ));
    assert_eq!(wire.timeout_secs, 40);
    let mut bad = wire.clone();
    bad.command = "echo unexpected".into();
    assert!(bad.decode_operation().is_err());
    let mut bad = wire.clone();
    bad.path = Some("private-path".into());
    assert!(bad.decode_operation().is_err());
    let mut bad = wire.clone();
    bad.client_id = "another-runner".into();
    assert!(bad.decode_operation().is_err());
    let mut bad = wire;
    bad.timeout_secs = 300;
    assert!(bad.decode_operation().is_err());
    let q = AgentCliDeliveryQuery {
        version: 1,
        target: target(),
        binding: binding(),
        deadline_at_ms: 40_000,
    };
    let wire =
        RunnerRequest::from_operation(metadata(), RunnerOperation::AgentCliDelivery(q)).unwrap();
    assert!(matches!(
        wire.decode_operation().unwrap(),
        RunnerOperation::AgentCliDelivery(_)
    ));
}
#[test]
fn agent_cli_delivery_paths_are_only_explicit_portable_publish_files() {
    for suffix in ["¹", "²", "³"] {
        assert!(validate_delivery_path(&format!("files/COM{suffix}.txt")).is_err());
        assert!(validate_delivery_path(&format!("files/lpt{suffix}")).is_err());
    }
    for path in ["files/report.md", "files/nested/结果.rs"] {
        validate_delivery_path(path).unwrap();
    }
    for path in [
        "/private",
        "C:/private",
        "files/../home",
        "files//a",
        "files/a/./b",
        "files/a\\b",
        "files/a:stream",
        "files/NUL.txt",
        "files/CON",
        "files/LPT1.txt",
        "files/a.",
        "files/a ",
        "files/",
    ] {
        assert!(validate_delivery_path(path).is_err(), "{path}");
    }
}
#[test]
fn agent_cli_manifest_requires_explicit_nonempty_delivery_and_limits() {
    let mut manifest = AgentCliDeliveryManifest {
        version: 1,
        summary: "report only".into(),
        files: vec![],
    };
    manifest.validate().unwrap();
    manifest.summary.clear();
    assert!(manifest.validate().is_err());
    manifest.summary = "x".repeat(AGENT_CLI_DELIVERY_SUMMARY_MAX_BYTES + 1);
    assert!(manifest.validate().is_err());
    manifest.summary.clear();
    manifest.files.push(AgentCliDeliveryManifestFile {
        path: "files/result".into(),
        sha256: "a".repeat(64),
        bytes: AGENT_CLI_DELIVERY_FILE_MAX_BYTES as u64 + 1,
    });
    assert!(manifest.validate().is_err());
    manifest.files[0].bytes = 1;
    manifest.validate().unwrap();
    let mut duplicate = manifest.files[0].clone();
    duplicate.path = "FILES/result".into();
    manifest.files.push(duplicate);
    assert!(manifest.validate().is_err());
}
#[test]
fn agent_cli_bundle_keeps_exact_utf8_bytes_and_checks_content_identity() {
    let original = bundle();
    let bytes = original.canonical_bytes(&binding()).unwrap();
    let decoded: AgentCliDeliveryBundle = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, original);
    let mut bad = original.clone();
    bad.files[0].text = bad.files[0].text.replace("\r\n", "\n");
    assert!(bad.canonical_bytes(&binding()).is_err());
    let mut bad = original.clone();
    bad.binding_sha256 = "d".repeat(64);
    assert!(bad.canonical_bytes(&binding()).is_err());
    let mut bad = original;
    bad.files[0].text.push('\0');
    assert!(bad.canonical_bytes(&binding()).is_err());
}
#[test]
fn agent_cli_snapshot_dto_requires_original_binding_and_whole_bundle_hash() {
    let b = binding();
    let data = bundle();
    let bytes = data.canonical_bytes(&b).unwrap();
    let mut snapshot = AgentCliDeliverySnapshot {
        version: 1,
        binding: b.clone(),
        artifact_sha256: sha256(&bytes),
        bundle: data,
    };
    assert_eq!(snapshot.validate_for(&b).unwrap(), bytes);
    snapshot.artifact_sha256 = "f".repeat(64);
    assert!(snapshot.validate_for(&b).is_err());
    snapshot.artifact_sha256 = sha256(&bytes);
    snapshot.binding.executor.task_id = "another-task".into();
    assert!(snapshot.validate_for(&b).is_err());
    let mut value = serde_json::to_value(bundle()).unwrap();
    value["author"] = json!("forged");
    assert!(serde_json::from_value::<AgentCliDeliveryBundle>(value).is_err());
}

#[test]
fn agent_cli_delivery_reader_can_change_instance_only_with_original_execution_identity_intact() {
    let mut query = AgentCliDeliveryQuery {
        version: 1,
        target: target(),
        binding: binding(),
        deadline_at_ms: 40_000,
    };
    query.target.runner_instance_id = "new-reader-instance".into();
    query.validate().unwrap();
    assert_eq!(query.binding.target.runner_instance_id, "instance");
    query.target.client_id = "another-client".into();
    assert!(query.validate().is_err());
}

#[test]
fn agent_cli_delivery_wire_bound_preserves_a_valid_near_limit_artifact() {
    let original = binding();
    let text = "\"".repeat(262_000);
    let files = ["files/a", "files/b"]
        .into_iter()
        .map(|path| AgentCliDeliveryFile {
            path: path.into(),
            sha256: sha256(text.as_bytes()),
            bytes: text.len() as u64,
            text: text.clone(),
        })
        .collect();
    let bundle = AgentCliDeliveryBundle {
        version: 1,
        binding_sha256: original.binding_hash().unwrap(),
        summary: String::new(),
        files,
    };
    let artifact = bundle.canonical_bytes(&original).unwrap();
    assert!(artifact.len() <= AGENT_CLI_DELIVERY_MAX_BYTES);
    let snapshot = AgentCliDeliverySnapshot {
        version: 1,
        binding: original.clone(),
        artifact_sha256: sha256(&artifact),
        bundle,
    };
    let wire = serde_json::to_vec(&snapshot).unwrap();
    assert!(wire.len() > AGENT_CLI_DELIVERY_MAX_BYTES);
    assert!(wire.len() <= AGENT_CLI_DELIVERY_RESPONSE_MAX_BYTES);
    let decoded: AgentCliDeliverySnapshot = serde_json::from_slice(&wire).unwrap();
    assert_eq!(decoded.validate_for(&original).unwrap(), artifact);
}
