use super::*;
use crate::runner_protocol::AgentExecutionResourceDescriptor;

#[test]
fn codex_native_config_effort_is_quoted_and_cannot_retarget_an_override() {
    let adapter = crate::cli_adapter::cli_adapters(&[])
        .unwrap()
        .into_iter()
        .find(|a| a.id == "codex")
        .unwrap();
    let mut value = record(AgentCliProvider::parse("codex").unwrap());
    value.loading_method = "registered-native-cli-v1".into();
    value.adapter = Some(adapter.clone());
    let (base, _) = adapter
        .invocation("observed-model", Some("high"), "task")
        .unwrap();
    assert_eq!(base[4], "model_reasoning_effort=\"high\"");
    let args = compose_reviewed_cli_argv(&value, &base).unwrap();
    validate_reviewed_cli_argv(&value, &args).unwrap();
    let mut forged = args.clone();
    forged[4] = "sandbox_mode=\"danger-full-access\"".into();
    assert!(validate_reviewed_cli_argv(&value, &forged).is_err());
    let mut unquoted = args;
    unquoted[4] = "model_reasoning_effort=high".into();
    assert!(validate_reviewed_cli_argv(&value, &unquoted).is_err());
    value.fixed_args.push(ReviewedControlArg {
        flag: "--config".into(),
        value: Some("override".into()),
    });
    assert!(value.validate_shape().is_err());
}

#[test]
fn registered_native_adapter_uses_its_own_bound_protocol_without_a_product_enum() {
    let provider = AgentCliProvider::parse("third-party-cli").unwrap();
    let mut adapter = crate::cli_adapter::cli_adapters(&[]).unwrap().remove(0);
    adapter.id = provider.id().into();
    adapter.label = "Synthetic third-party CLI".into();
    let mut value = record(provider);
    value.loading_method = "registered-native-cli-v1".into();
    value.adapter = Some(adapter.clone());
    let (base, _) = adapter
        .invocation("observed-model", Some("high"), "task")
        .unwrap();
    let args = compose_reviewed_cli_argv(&value, &base).unwrap();
    validate_reviewed_cli_argv(&value, &args).unwrap();
    let original_hash = value.review_sha256().unwrap();
    value
        .adapter
        .as_mut()
        .unwrap()
        .invocation
        .as_mut()
        .unwrap()
        .model_flag = "--changed-model".into();
    assert_ne!(original_hash, value.review_sha256().unwrap());
    assert!(validate_reviewed_cli_argv(&value, &args).is_err());
    value.adapter.as_mut().unwrap().id = "another-cli".into();
    assert!(value.require_supported_loading().is_err());
    assert_eq!(
        serde_json::to_string(&provider).unwrap(),
        "\"third-party-cli\""
    );
    assert!(AgentCliProvider::parse("cmd.exe /c arbitrary").is_err());
}

#[test]
fn antigravity_mapping_binds_binary_templates_and_tool_policy() {
    let mut value = record(AgentCliProvider::Antigravity);
    value.cli_version = ANTIGRAVITY_FILES_VERSION.into();
    value.executable_sha256 = ANTIGRAVITY_FILES_EXE_SHA256.into();
    value.loading_method = ANTIGRAVITY_FILES_LOADING.into();
    value.fixed_args = antigravity_file_worker_args();
    value.control_files = antigravity_file_worker_control_files();
    value.require_supported_loading().unwrap();
    let args = compose_reviewed_cli_argv(&value, &base(value.provider)).unwrap();
    validate_reviewed_cli_argv(&value, &args).unwrap();
    let mut pinned_model = base(value.provider);
    pinned_model[1] = "gemini-3.8-flash-high".into();
    pinned_model.drain(2..4);
    let pinned = compose_reviewed_cli_argv(&value, &pinned_model).unwrap();
    validate_reviewed_cli_argv(&value, &pinned).unwrap();
    assert!(!pinned.iter().any(|arg| arg == "--effort"));
    let mut override_args = pinned.clone();
    override_args.extend(["--effort".into(), "low".into()]);
    assert!(validate_reviewed_cli_argv(&value, &override_args).is_err());
    for field in ["version", "binary", "agent", "controls", "provider"] {
        let mut changed = value.clone();
        match field {
            "version" => changed.cli_version = "1.4.8".into(),
            "binary" => changed.executable_sha256 = "f".repeat(64),
            "agent" => changed.fixed_args[0].value = Some("default".into()),
            "controls" => changed.control_files[0].sha256 = "f".repeat(64),
            _ => changed.provider = AgentCliProvider::WorkBuddy,
        }
        assert!(changed.require_supported_loading().is_err(), "{field}");
    }
}

fn record(provider: AgentCliProvider) -> ReviewedAgentCliControls {
    ReviewedAgentCliControls {
        version: 1,
        record_id: "synthetic-review".into(),
        provider,
        cli_version: "synthetic-1.0".into(),
        executable_sha256: "a".repeat(64),
        resource_set_id: "synthetic-resource".into(),
        payload_sha256: resource_payload_sha256(b"synthetic pinned payload").unwrap(),
        control_files: vec![ReviewedControlFile {
            path: "input/controls.json".into(),
            sha256: "b".repeat(64),
        }],
        loading_method: "synthetic-unverified-loader".into(),
        fixed_args: vec![ReviewedControlArg {
            flag: "--control-file".into(),
            value: Some("input/controls.json".into()),
        }],
        verification_evidence_sha256: "c".repeat(64),
        independent_review_sha256: "d".repeat(64),
        adapter: None,
    }
}

fn base(provider: AgentCliProvider) -> Vec<String> {
    let mut args = vec!["--model", "vendor/model-v1", "--effort", "high"];
    match provider {
        AgentCliProvider::WorkBuddy => args.extend([
            "--print",
            "--input-format",
            "text",
            "--output-format",
            "json",
        ]),
        AgentCliProvider::Antigravity => args.extend([
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]),
        _ => panic!("legacy fixture requires an explicitly selected built-in recipe"),
    }
    args.into_iter().map(String::from).collect()
}

fn descriptor(record: &ReviewedAgentCliControls) -> AgentExecutionResourceDescriptor {
    AgentExecutionResourceDescriptor {
        project_workspace_copy: false,
        resource_set_id: record.resource_set_id.clone(),
        manifest_hash: record.execution_manifest_sha256().unwrap(),
        executable_identity: format!("sha256:{}", record.executable_sha256),
        provider: Some(record.provider),
        reviewed_controls: Some(record.clone()),
    }
}

// Synthetic digests test the mapping only; this is never written as a profile.
fn codebuddy_files_record() -> ReviewedAgentCliControls {
    let mut value = record(AgentCliProvider::WorkBuddy);
    value.cli_version = CODEBUDDY_FILES_VERSION.into();
    value.executable_sha256 = CODEBUDDY_FILES_EXE_SHA256.into();
    value.loading_method = CODEBUDDY_FILES_LOADING.into();
    value.control_files = codebuddy_file_worker_control_files();
    value.fixed_args = codebuddy_file_worker_args();
    value
}

#[test]
fn codebuddy_files_mapping_is_exact_version_binary_and_policy() {
    let value = codebuddy_files_record();
    descriptor(&value).validate().unwrap();
    value.require_supported_loading().unwrap();
    let argv = compose_reviewed_cli_argv(&value, &base(value.provider)).unwrap();
    validate_reviewed_cli_argv(&value, &argv).unwrap();
    for field in [
        "version", "binary", "provider", "loader", "settings", "mcp", "args",
    ] {
        let mut changed = value.clone();
        match field {
            "version" => changed.cli_version = "2.162.1".into(),
            "binary" => changed.executable_sha256 = "a".repeat(64),
            "provider" => changed.provider = AgentCliProvider::Antigravity,
            "loader" => changed.loading_method = "other-loader".into(),
            "settings" => changed.control_files[1].sha256 = "a".repeat(64),
            "mcp" => changed.control_files[0].sha256 = "a".repeat(64),
            _ => changed.fixed_args[6].value = Some("bypassPermissions".into()),
        }
        assert!(changed.require_supported_loading().is_err(), "{field}");
    }
}

#[test]
fn codebuddy_node_mapping_binds_version_runtime_and_fixed_controls() {
    let mut value = codebuddy_files_record();
    value.cli_version = CODEBUDDY_NODE_FILES_VERSION.into();
    value.executable_sha256 = CODEBUDDY_NODE_EXE_SHA256.into();
    value.loading_method = CODEBUDDY_NODE_FILES_LOADING.into();
    let argv = compose_reviewed_cli_argv(&value, &base(value.provider)).unwrap();
    validate_reviewed_cli_argv(&value, &argv).unwrap();
    for mutation in ["version", "binary", "loader", "tools"] {
        let mut changed = value.clone();
        match mutation {
            "version" => changed.cli_version = CODEBUDDY_FILES_VERSION.into(),
            "binary" => changed.executable_sha256 = CODEBUDDY_FILES_EXE_SHA256.into(),
            "loader" => changed.loading_method = CODEBUDDY_FILES_LOADING.into(),
            _ => changed.fixed_args[0].value = Some("Read,Write,Bash".into()),
        }
        assert!(changed.require_supported_loading().is_err(), "{mutation}");
    }
}

#[test]
fn codebuddy_files_csv_exception_cannot_add_tools_or_escape_binding() {
    let value = codebuddy_files_record();
    for csv in [
        "Read,Glob,Grep,Edit,Write,Bash",
        "Read,Write",
        "default",
        "",
    ] {
        let mut changed = value.clone();
        changed.fixed_args[0].value = Some(csv.into());
        assert!(changed.require_supported_loading().is_err());
    }
    let mut wrong_flag = value.clone();
    wrong_flag.fixed_args[0].flag = "--other".into();
    assert!(wrong_flag.validate_shape().is_err());
    let observed = descriptor(&value);
    let mut changed_payload = value.clone();
    changed_payload.payload_sha256 = "f".repeat(64);
    assert!(changed_payload
        .validate_resource(
            &observed.resource_set_id,
            observed.provider,
            &observed.executable_identity,
            &observed.manifest_hash
        )
        .is_err());
}

#[test]
fn codebuddy_files_argv_keeps_empty_source_and_rejects_extra_effects() {
    let value = codebuddy_files_record();
    let mut args = compose_reviewed_cli_argv(&value, &base(value.provider)).unwrap();
    let position = args.iter().position(|a| a == "--setting-sources").unwrap();
    assert_eq!(args[position + 1], "");
    args.extend(["--tools".into(), "default".into()]);
    assert!(validate_reviewed_cli_argv(&value, &args).is_err());
    let mut unknown = value;
    unknown.loading_method = "synthetic-unverified-loader".into();
    assert!(compose_reviewed_cli_argv(&unknown, &base(unknown.provider)).is_err());
}

#[test]
fn reviewed_controls_shape_and_hash_are_not_execution_approval() {
    for provider in [AgentCliProvider::WorkBuddy, AgentCliProvider::Antigravity] {
        let value = record(provider);
        value.validate_shape().unwrap();
        descriptor(&value).validate().unwrap();
        let argv = compose_shape(&value, &base(provider)).unwrap();
        for result in [
            value.require_supported_loading(),
            compose_reviewed_cli_argv(&value, &base(provider)).map(|_| ()),
            validate_reviewed_cli_argv(&value, &argv),
        ] {
            assert_eq!(
                result.unwrap_err(),
                "agent_cli_controls_mechanism_unverified"
            );
        }
    }
}

#[test]
fn reviewed_controls_empty_argument_is_distinct_from_flag_only() {
    let mut empty = record(AgentCliProvider::WorkBuddy);
    empty.fixed_args = vec![ReviewedControlArg {
        flag: "--tools".into(),
        value: Some(String::new()),
    }];
    let mut absent = empty.clone();
    absent.fixed_args[0].value = None;
    let empty_argv = compose_shape(&empty, &base(empty.provider)).unwrap();
    let absent_argv = compose_shape(&absent, &base(absent.provider)).unwrap();
    assert_eq!(&empty_argv[9..], &["--tools", ""]);
    assert_eq!(&absent_argv[9..], &["--tools"]);
    assert_ne!(
        empty.review_sha256().unwrap(),
        absent.review_sha256().unwrap()
    );
    assert_ne!(
        empty.execution_manifest_sha256().unwrap(),
        absent.execution_manifest_sha256().unwrap()
    );
    let encoded = serde_json::to_value(&empty).unwrap();
    assert_eq!(encoded["fixed_args"][0]["value"], "");
    assert!(serde_json::to_value(&absent).unwrap()["fixed_args"][0]["value"].is_null());
}

#[test]
fn reviewed_controls_preserve_camelcase_but_reject_casefold_overrides() {
    let mut value = record(AgentCliProvider::Antigravity);
    value.fixed_args = vec![ReviewedControlArg {
        flag: "--disallowedTools".into(),
        value: Some("Shell".into()),
    }];
    let argv = compose_shape(&value, &base(value.provider)).unwrap();
    assert_eq!(&argv[8..], &["--disallowedTools", "Shell"]);
    let original_hash = value.review_sha256().unwrap();
    value.fixed_args[0].flag = "--disallowedtools".into();
    assert_ne!(value.review_sha256().unwrap(), original_hash);
    value.fixed_args.push(ReviewedControlArg {
        flag: "--DISALLOWEDTOOLS".into(),
        value: None,
    });
    assert_eq!(
        value.validate_shape().unwrap_err(),
        "agent_cli_controls_argument_override"
    );
    for reserved in RESERVED_FLAGS {
        value.fixed_args = vec![ReviewedControlArg {
            flag: reserved.to_ascii_uppercase(),
            value: None,
        }];
        assert_eq!(
            value.validate_shape().unwrap_err(),
            "agent_cli_controls_argument_override",
            "{reserved}"
        );
    }
}

#[test]
fn reviewed_controls_reject_option_injection_paths_and_unbound_files() {
    let original = record(AgentCliProvider::WorkBuddy);
    for flag in [
        "",
        "tools",
        "-tools",
        "--",
        "--tools=all",
        "--tools other",
        "--@response",
        "--工具",
    ] {
        let mut value = original.clone();
        value.fixed_args[0].flag = flag.into();
        assert!(value.validate_shape().is_err(), "{flag}");
    }
    for argument in [
        "--yolo",
        "@response",
        "/host/file",
        "C:\\private\\file",
        " file",
        "file\n",
        "input/missing.json",
        "input/../controls.json",
        "..",
        "x=y",
        "a b",
    ] {
        let mut value = original.clone();
        value.fixed_args[0].value = Some(argument.into());
        assert!(value.validate_shape().is_err(), "{argument}");
    }
    for path in [
        "controls.json",
        "input/../x",
        "input/CON.json",
        "input/AUX",
        "input/trailing.",
        "input//x",
        "input/x\\y",
    ] {
        let mut value = original.clone();
        value.control_files[0].path = path.into();
        value.fixed_args[0].value = None;
        assert!(value.validate_shape().is_err(), "{path}");
    }
}

#[test]
fn reviewed_controls_business_argv_is_exact_and_cannot_be_appended_twice() {
    for provider in [AgentCliProvider::WorkBuddy, AgentCliProvider::Antigravity] {
        let value = record(provider);
        for (index, injected) in [(1, "--yolo"), (1, "@model"), (3, "--model"), (3, "")] {
            let mut changed = base(provider);
            changed[index] = injected.into();
            assert!(compose_shape(&value, &changed).is_err());
        }
        let composed = compose_shape(&value, &base(provider)).unwrap();
        assert!(compose_shape(&value, &composed).is_err());
        let mut appended = composed;
        appended.extend(["--model".into(), "override".into()]);
        assert_eq!(
            validate_reviewed_cli_argv(&value, &appended).unwrap_err(),
            "agent_cli_controls_argument_override"
        );
        let mut changed = base(provider);
        changed[4] = "--resume".into();
        assert!(compose_shape(&value, &changed).is_err());
    }
}

#[test]
fn reviewed_controls_hashes_bind_every_subject_change_without_self_reference() {
    let original = record(AgentCliProvider::WorkBuddy);
    let r = original.review_sha256().unwrap();
    let f = original.execution_manifest_sha256().unwrap();
    assert_eq!(
        f,
        final_reviewed_manifest_sha256(&original.payload_sha256, &r).unwrap()
    );
    assert_ne!(r, original.payload_sha256);
    assert_ne!(f, r);
    for field in [
        "provider", "version", "exe", "resource", "payload", "file", "method", "argument",
        "evidence", "review",
    ] {
        let mut value = original.clone();
        match field {
            "provider" => value.provider = AgentCliProvider::Antigravity,
            "version" => value.cli_version = "synthetic-2".into(),
            "exe" => value.executable_sha256 = "e".repeat(64),
            "resource" => value.resource_set_id = "other".into(),
            "payload" => value.payload_sha256 = "f".repeat(64),
            "file" => value.control_files[0].sha256 = "e".repeat(64),
            "method" => value.loading_method = "other-loader".into(),
            "argument" => value.fixed_args[0].value = None,
            "evidence" => value.verification_evidence_sha256 = "e".repeat(64),
            _ => value.independent_review_sha256 = "e".repeat(64),
        }
        assert_ne!(value.review_sha256().unwrap(), r, "{field}");
        assert_ne!(value.execution_manifest_sha256().unwrap(), f, "{field}");
    }
    let json = serde_json::to_value(&original).unwrap();
    for absent in ["review_sha256", "manifest_hash", "approved", "issuer"] {
        assert!(json.get(absent).is_none());
    }
}

#[test]
fn reviewed_controls_resource_subject_cannot_be_downgraded_or_retargeted() {
    let original = descriptor(&record(AgentCliProvider::WorkBuddy));
    for field in [
        "provider_none",
        "provider_other",
        "resource",
        "exe",
        "manifest",
    ] {
        let mut value = original.clone();
        match field {
            "provider_none" => value.provider = None,
            "provider_other" => value.provider = Some(AgentCliProvider::Antigravity),
            "resource" => value.resource_set_id = "other".into(),
            "exe" => value.executable_identity = format!("sha256:{}", "f".repeat(64)),
            _ => value.manifest_hash = "f".repeat(64),
        }
        assert!(value.validate().is_err(), "{field}");
    }
    let mut legacy = original;
    legacy.reviewed_controls = None;
    legacy.provider = None;
    legacy.validate().unwrap();
    assert!(serde_json::to_value(legacy)
        .unwrap()
        .get("reviewed_controls")
        .is_none());
}

#[test]
fn reviewed_controls_are_bounded_sorted_and_strictly_typed() {
    let original = record(AgentCliProvider::WorkBuddy);
    for field in ["approved", "issuer", "manifest_hash"] {
        let mut json = serde_json::to_value(&original).unwrap();
        json[field] = serde_json::json!(true);
        assert!(serde_json::from_value::<ReviewedAgentCliControls>(json).is_err());
    }
    let mut json = serde_json::to_value(&original).unwrap();
    json["fixed_args"][0]["approved"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ReviewedAgentCliControls>(json).is_err());
    let mut value = original.clone();
    value.fixed_args = (0..=REVIEWED_CONTROL_MAX_ARGS)
        .map(|i| ReviewedControlArg {
            flag: format!("--flag-{i}"),
            value: None,
        })
        .collect();
    assert!(value.validate_shape().is_err());
    value = original.clone();
    value.control_files.push(value.control_files[0].clone());
    assert!(value.validate_shape().is_err());
    value = original.clone();
    value.control_files.insert(
        0,
        ReviewedControlFile {
            path: "input/z.json".into(),
            sha256: "f".repeat(64),
        },
    );
    assert!(value.validate_shape().is_err());
    value = original;
    value.payload_sha256 = "A".repeat(64);
    assert!(value.validate_shape().is_err());
    assert!(resource_payload_sha256(&[]).is_err());
    assert!(resource_payload_sha256(&vec![0; RESOURCE_PAYLOAD_MAX_BYTES + 1]).is_err());
}
