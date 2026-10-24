//! Synthetic subject consistency only. These records are not provider evidence
//! and cannot pass the production loading-mechanism gate.
use super::*;
use webcodex_core::agent_cli_controls::{ReviewedControlArg, ReviewedControlFile};

fn codebuddy_files_profile() -> AgentExecutionResourceProfile {
    use webcodex_core::agent_cli_controls::*;
    let mut p = profile();
    p.provider = Some(AgentCliProvider::WorkBuddy);
    p.executable_sha256 = CODEBUDDY_FILES_EXE_SHA256.into();
    p.files = codebuddy_file_worker_control_files()
        .into_iter()
        .map(|file| AgentExecutionResourceFile {
            source: path(file.path.strip_prefix("input/").unwrap()),
            destination: file.path.into(),
            sha256: file.sha256,
        })
        .collect();
    let payload = validated(p.clone()).manifest(&p.id).unwrap().to_owned();
    p.reviewed_controls = Some(ReviewedAgentCliControls {
        version: 1,
        record_id: "synthetic-mapping-test-not-approval".into(),
        provider: AgentCliProvider::WorkBuddy,
        cli_version: CODEBUDDY_FILES_VERSION.into(),
        executable_sha256: CODEBUDDY_FILES_EXE_SHA256.into(),
        resource_set_id: p.id.clone(),
        payload_sha256: payload,
        control_files: codebuddy_file_worker_control_files(),
        loading_method: CODEBUDDY_FILES_LOADING.into(),
        fixed_args: codebuddy_file_worker_args(),
        verification_evidence_sha256: "d".repeat(64),
        independent_review_sha256: "e".repeat(64),
    });
    p
}

fn codebuddy_files_start(profiles: &ValidatedResourceProfiles) -> AgentExecutionStart {
    use webcodex_core::agent_cli_controls::compose_reviewed_cli_argv;
    let mut request = start(profiles);
    let profile = &profiles.0["worker-v1"];
    request.process.executable = profile.executable_identity.clone();
    let base = [
        "--model",
        "input/empty-mcp.json",
        "--effort",
        "low",
        "--print",
        "--input-format",
        "text",
        "--output-format",
        "json",
    ]
    .map(str::to_owned);
    request.process.args =
        compose_reviewed_cli_argv(profile.reviewed_controls.as_ref().unwrap(), &base).unwrap();
    request.binding.launch_intent_hash =
        compute_launch_intent_hash(&request.process, None, 30).unwrap();
    request
}

#[test]
fn proxy_opt_in_requires_the_exact_reviewed_codebuddy_files_contract() {
    for mutation in ["review", "provider", "executable", "loading", "fixed-args"] {
        let mut p = codebuddy_files_profile();
        p.user_https_proxy = true;
        match mutation {
            "review" => p.reviewed_controls = None,
            "provider" => p.provider = Some(AgentCliProvider::Antigravity),
            "executable" => p.executable_sha256 = "f".repeat(64),
            "loading" => p.reviewed_controls.as_mut().unwrap().loading_method = "unreviewed".into(),
            _ => {
                p.reviewed_controls.as_mut().unwrap().fixed_args.pop();
            }
        }
        assert_eq!(
            failure(p),
            "agent_resources_proxy_requires_reviewed_codebuddy_files",
            "accepted {mutation}"
        );
    }
    let mut legacy = profile();
    legacy.user_https_proxy = true;
    assert_eq!(
        failure(legacy),
        "agent_resources_proxy_requires_reviewed_codebuddy_files"
    );
}

#[test]
fn proxy_payload_requires_a_matching_review_and_changes_the_frozen_manifest() {
    let p = codebuddy_files_profile();
    let baseline = validated(p.clone());
    let original_start = codebuddy_files_start(&baseline);
    assert_eq!(
        baseline.cleanup_requirements(&original_start).unwrap(),
        (true, false)
    );
    let mut proxy = p;
    proxy.user_https_proxy = true;
    assert_eq!(
        failure(proxy.clone()),
        "agent_resources_control_payload_mismatch"
    );
    let payload = payload_manifest(
        &proxy.id,
        proxy.provider,
        &baseline.0[&proxy.id].files,
        true,
    )
    .unwrap();
    let encoded = serde_json::to_vec(&(
        1u32,
        &proxy.id,
        &baseline.0[&proxy.id].files,
        proxy.provider,
        "codebuddy-user-https-proxy-v1",
    ))
    .unwrap();
    assert_eq!(payload, resource_payload_sha256(&encoded).unwrap());
    assert_ne!(
        payload,
        proxy.reviewed_controls.as_ref().unwrap().payload_sha256
    );
    proxy.reviewed_controls.as_mut().unwrap().payload_sha256 = payload;
    let profiles = validated(proxy);
    assert_ne!(
        profiles.manifest("worker-v1"),
        baseline.manifest("worker-v1")
    );
    assert_eq!(
        profiles.cleanup_requirements(&original_start).unwrap_err(),
        "agent_resources_manifest_mismatch"
    );
    assert_eq!(
        profiles
            .cleanup_requirements(&codebuddy_files_start(&profiles))
            .unwrap(),
        (true, true)
    );
    let mut invalid = profiles.clone();
    invalid.0.get_mut("worker-v1").unwrap().user_https_proxy = false;
    assert_eq!(
        invalid
            .cleanup_requirements(&codebuddy_files_start(&profiles))
            .unwrap_err(),
        "agent_resources_control_payload_mismatch"
    );
}

#[test]
fn codebuddy_files_materialize_root_controls_without_changing_intent_or_publish_cwd() {
    let profiles = validated(codebuddy_files_profile());
    let request = codebuddy_files_start(&profiles);
    let original = request.process.args.clone();
    let original_hash = request.binding.launch_intent_hash.clone();
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("worker");
    std::fs::create_dir_all(root.join("input")).unwrap();
    std::fs::create_dir_all(root.join("workspace/publish")).unwrap();
    let args = materialized_execution_args(&profiles.0["worker-v1"], &request, &root).unwrap();
    assert_eq!(
        args[1], "input/empty-mcp.json",
        "do not rewrite model/business operands"
    );
    for (flag, name) in [
        ("--settings", "worker-settings.json"),
        ("--mcp-config", "empty-mcp.json"),
    ] {
        let index = args.iter().position(|arg| arg == flag).unwrap() + 1;
        assert_eq!(
            args[index],
            absolute_path(&root.join("input").join(name)).unwrap()
        );
        assert_ne!(
            args[index],
            absolute_path(&root.join("workspace/input").join(name)).unwrap()
        );
    }
    assert_eq!(request.process.args, original);
    assert_eq!(request.binding.launch_intent_hash, original_hash);
    assert_eq!(
        compute_launch_intent_hash(&request.process, None, 30).unwrap(),
        original_hash
    );
    assert!(root.join("workspace/publish").is_dir());
}

#[test]
fn codebuddy_files_materialization_rejects_replaced_control_or_argv() {
    let p = codebuddy_files_profile();
    let profiles = validated(p.clone());
    let mut request = codebuddy_files_start(&profiles);
    let index = request
        .process
        .args
        .iter()
        .position(|arg| arg == "--settings")
        .unwrap()
        + 1;
    request.process.args[index] = "input/other.json".into();
    assert!(
        materialized_execution_args(&profiles.0["worker-v1"], &request, &path("root")).is_err()
    );
    let mut changed = p;
    changed.files[0].sha256 = "f".repeat(64);
    assert!(validate_profiles(&AgentExecutionResourcesConfig {
        profiles: vec![changed]
    })
    .is_err());
}

fn subject(p: &AgentExecutionResourceProfile) -> ReviewedAgentCliControls {
    let mut unreviewed = p.clone();
    unreviewed.reviewed_controls = None;
    let baseline = validated(unreviewed);
    ReviewedAgentCliControls {
        version: 1,
        record_id: "synthetic-unverified-record".into(),
        provider: p.provider.unwrap(),
        cli_version: "synthetic-0.0.0".into(),
        executable_sha256: p.executable_sha256.clone(),
        resource_set_id: p.id.clone(),
        payload_sha256: baseline.manifest(&p.id).unwrap().into(),
        control_files: vec![ReviewedControlFile {
            path: "input/data.json".into(),
            sha256: p.files[0].sha256.clone(),
        }],
        loading_method: "synthetic-unverified-mechanism".into(),
        fixed_args: vec![ReviewedControlArg {
            flag: "--fixture-control".into(),
            value: Some("input/data.json".into()),
        }],
        verification_evidence_sha256: "d".repeat(64),
        independent_review_sha256: "e".repeat(64),
    }
}

fn reviewed_profile() -> AgentExecutionResourceProfile {
    let mut p = profile();
    p.provider = Some(AgentCliProvider::WorkBuddy);
    p.reviewed_controls = Some(subject(&p));
    p
}

fn reviewed_start(profiles: &ValidatedResourceProfiles) -> AgentExecutionStart {
    let mut request = start(profiles);
    request.process.args = [
        "--model",
        "fixture-model",
        "--effort",
        "low",
        "--print",
        "--input-format",
        "text",
        "--output-format",
        "json",
        "--fixture-control",
        "input/data.json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    request.binding.launch_intent_hash = compute_launch_intent_hash(
        &request.process,
        request.stdin.as_deref(),
        request.timeout_secs,
    )
    .unwrap();
    request
}

fn failure(p: AgentExecutionResourceProfile) -> String {
    validate_profiles(&AgentExecutionResourcesConfig { profiles: vec![p] }).unwrap_err()
}

#[test]
fn reviewed_facts_bind_recomputed_payload_subject_and_final_manifest() {
    let p = reviewed_profile();
    let review = p.reviewed_controls.clone().unwrap();
    let profiles = validated(p.clone());
    let descriptor = &profiles.descriptors()[0];
    descriptor.validate().unwrap();
    assert_eq!(descriptor.reviewed_controls.as_ref(), Some(&review));
    assert_eq!(
        descriptor.manifest_hash,
        review.execution_manifest_sha256().unwrap()
    );
    assert_ne!(descriptor.manifest_hash, review.payload_sha256);
    assert_eq!(
        descriptor.manifest_hash,
        final_reviewed_manifest_sha256(&review.payload_sha256, &review.review_sha256().unwrap())
            .unwrap()
    );
    let public = serde_json::to_string(descriptor).unwrap();
    assert!(!public.contains("resource-fixture") && !public.contains("\"source\""));
    let parsed: AgentExecutionResourceProfile = serde_json::from_value(serde_json::json!({
        "id":p.id, "provider":"workbuddy", "reviewed_controls":review,
        "executable":p.executable, "executable_sha256":p.executable_sha256,
        "files":[{"source":path("data.json"),"destination":"input/data.json","sha256":"b".repeat(64)},
                 {"source":path("runtime.dll"),"destination":"bin/runtime.dll","sha256":"c".repeat(64)}]
    })).unwrap();
    assert_eq!(validated(parsed).descriptors(), profiles.descriptors());
}

#[test]
fn provider_without_review_retains_metadata_but_cannot_start_business() {
    let mut p = profile();
    p.provider = Some(AgentCliProvider::WorkBuddy);
    let profiles = validated(p);
    assert_eq!(
        profiles.descriptors()[0].provider,
        Some(AgentCliProvider::WorkBuddy)
    );
    assert!(profiles.descriptors()[0].reviewed_controls.is_none());
    assert_eq!(
        profiles.validate_execution(&start(&profiles)).unwrap_err(),
        "agent_resources_control_review_required"
    );
    let ordinary = validated(profile());
    ordinary.validate_execution(&start(&ordinary)).unwrap();
}

#[test]
fn reviewed_subject_does_not_enable_unknown_loading_or_materialization() {
    let profiles = validated(reviewed_profile());
    let request = reviewed_start(&profiles);
    assert_eq!(
        profiles.validate_execution(&request).unwrap_err(),
        "agent_cli_controls_mechanism_unverified"
    );
    assert_eq!(
        profiles.cleanup_requirements(&request).unwrap_err(),
        "agent_cli_controls_mechanism_unverified"
    );
    // selected_profile fails before any filesystem operation on every platform.
    assert_eq!(
        prepare(&profiles, &request, &path("must-not-be-created")).unwrap_err(),
        "agent_cli_controls_mechanism_unverified"
    );
    for args in [
        vec!["--fixture-control".into(), "input/data.json".into()],
        [
            request.process.args.clone(),
            vec!["--fixture-control".into(), "input/data.json".into()],
        ]
        .concat(),
        [
            request.process.args.clone(),
            vec!["--fixture-control=other".into()],
        ]
        .concat(),
    ] {
        let mut changed = request.clone();
        changed.process.args = args;
        changed.binding.launch_intent_hash =
            compute_launch_intent_hash(&changed.process, None, changed.timeout_secs).unwrap();
        assert_eq!(
            profiles.validate_execution(&changed).unwrap_err(),
            "agent_cli_controls_argument_override"
        );
    }
}

#[test]
fn changing_payload_or_omitting_readonly_control_members_rejects_old_review() {
    let mut wrong_payload = reviewed_profile();
    wrong_payload
        .reviewed_controls
        .as_mut()
        .unwrap()
        .payload_sha256 = "f".repeat(64);
    assert_eq!(
        failure(wrong_payload),
        "agent_resources_control_payload_mismatch"
    );
    for index in [0, 1] {
        let mut changed = reviewed_profile();
        changed.files[index].sha256 = "f".repeat(64);
        assert_eq!(failure(changed), "agent_resources_control_payload_mismatch");
    }
    let mut changed = reviewed_profile();
    changed.files[0].source = path("different-source.json");
    assert_eq!(failure(changed), "agent_resources_control_payload_mismatch");
    for missing in [false, true] {
        let mut changed = reviewed_profile();
        let review = changed.reviewed_controls.as_mut().unwrap();
        if missing {
            review.control_files[0].path = "input/missing.json".into();
            review.fixed_args[0].value = Some("input/missing.json".into());
        } else {
            review.control_files[0].sha256 = "f".repeat(64);
        }
        assert_eq!(failure(changed), "agent_resources_control_file_mismatch");
    }
}

#[test]
fn reviewed_identity_is_not_repaired_by_rehashing_another_payload() {
    for field in ["provider", "resource", "executable"] {
        let mut changed = reviewed_profile();
        match field {
            "provider" => changed.provider = Some(AgentCliProvider::Antigravity),
            "resource" => changed.id = "another-resource".into(),
            _ => changed.executable_sha256 = "f".repeat(64),
        }
        let payload = subject(&changed).payload_sha256;
        changed.reviewed_controls.as_mut().unwrap().payload_sha256 = payload;
        assert_eq!(failure(changed), "agent_cli_controls_resource_mismatch");
    }
}

#[test]
fn changing_review_version_or_evidence_cannot_reuse_frozen_manifest() {
    let original = validated(reviewed_profile());
    let request = reviewed_start(&original);
    for field in ["version", "evidence", "review", "record", "loading"] {
        let mut changed = reviewed_profile();
        let review = changed.reviewed_controls.as_mut().unwrap();
        match field {
            "version" => review.cli_version = "synthetic-0.0.1".into(),
            "evidence" => review.verification_evidence_sha256 = "f".repeat(64),
            "review" => review.independent_review_sha256 = "f".repeat(64),
            "record" => review.record_id = "another-synthetic-record".into(),
            _ => review.loading_method = "another-unverified-mechanism".into(),
        }
        let profiles = validated(changed);
        assert_ne!(
            profiles.manifest("worker-v1"),
            original.manifest("worker-v1")
        );
        assert_eq!(
            profiles.validate_execution(&request).unwrap_err(),
            "agent_resources_manifest_mismatch"
        );
    }
    let mut changed = reviewed_profile();
    changed.reviewed_controls.as_mut().unwrap().version = 2;
    assert_eq!(failure(changed), "agent_cli_controls_unsupported_version");
}

#[test]
fn reviewed_input_rejects_approval_claims_and_writable_control_locations() {
    let p = reviewed_profile();
    let review = serde_json::to_value(p.reviewed_controls.unwrap()).unwrap();
    for name in [
        "approved",
        "issuer",
        "source",
        "review_sha256",
        "manifest_hash",
    ] {
        let mut forged = review.clone();
        forged[name] = serde_json::json!("self-reported");
        assert!(serde_json::from_value::<ReviewedAgentCliControls>(forged).is_err());
    }
    for path in [
        "home/settings.json",
        "workspace/settings.json",
        "bin/settings.json",
        "C:/host/settings.json",
        "input/../home/settings.json",
    ] {
        let mut changed = reviewed_profile();
        changed.reviewed_controls.as_mut().unwrap().control_files[0].path = path.into();
        assert!(validate_profiles(&AgentExecutionResourcesConfig {
            profiles: vec![changed]
        })
        .is_err());
    }
}
