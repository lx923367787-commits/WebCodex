use super::*;
use webcodex_core::agent_cli::AgentCliProvider;
use webcodex_core::agent_cli_controls::{
    compose_reviewed_cli_argv, resource_payload_sha256, validate_reviewed_cli_argv,
    ReviewedAgentCliControls, ReviewedControlArg, ReviewedControlFile,
};
use webcodex_core::runner_protocol::{
    compute_members_sha256, AgentExecutionClose, AgentExecutionCloseRef, AgentRunRef,
};

#[test]
fn agent_team_catalog_failure_preserves_denial_and_redacts_unknown_text() {
    let unavailable = inventory_unavailable("agent_cli_launcher_requires_adapter");
    assert_eq!(
        unavailable.output["reason_code"],
        "agent_cli_launcher_requires_adapter"
    );
    assert!(
        !serde_json::to_string(&inventory_unavailable("private path or credential"))
            .unwrap()
            .contains("private path")
    );
    let busy = catalog_unavailable(
        "agent_team_host_catalog_unavailable:profiles_read:plugin_provider_busy",
    );
    assert_eq!(busy.output["stage"], "profiles_read");
    assert_eq!(busy.output["reason_code"], "plugin_provider_busy");
    let private =
        catalog_unavailable("agent_team_host_catalog_unavailable:secret-stage:secret-reason");
    assert!(private.output.get("stage").is_none());
    assert!(!serde_json::to_string(&private).unwrap().contains("secret"));
    for reason in [
        "agent_team_host_provider_unavailable",
        "agent_team_cli_resource_binding_mismatch",
        "workspace_team_new_chat_required",
    ] {
        let result = catalog_unavailable(reason);
        assert!(!result.success);
        assert_eq!(result.output["reason_code"], reason);
        assert_eq!(result.output["dispatched"], false);
        assert_eq!(result.output["state_changed"], false);
    }
    for reason in [
        "private credential value",
        "agent_team_host_provider_unavailable\nsecret",
    ] {
        let result = catalog_unavailable(reason);
        assert_eq!(
            result.output["reason_code"],
            "agent_team_catalog_unavailable"
        );
        assert!(!serde_json::to_string(&result).unwrap().contains(reason));
    }
}

#[test]
fn agent_team_execution_delivery_contract_is_deterministic_and_keeps_task_on_stdin() {
    let brief = "Produce the requested report.\n--model is task text, not argv.";
    let prompt = cli_worker_prompt(brief);
    assert_eq!(prompt, cli_worker_prompt(brief));
    assert!(prompt.starts_with(brief));
    assert!(prompt.ends_with(CLI_PRIVATE_DIRECTORIES));
    assert!(!prompt.contains("publish/manifest.json"));
    assert!(prompt.contains("WEBCODEX_MEMORY_DIR"));
    let (_, args, stdin) = crate::tool_runtime::fixed_cli_process(
        "antigravity",
        "explicit-model",
        Some("high"),
        &prompt,
    )
    .unwrap();
    let input: Value = serde_json::from_str(&stdin).unwrap();
    assert_eq!(input["message"]["content"], prompt);
    assert!(!args.iter().any(|arg| arg.contains("publish/")));
}

// Synthetic consistency subjects only. Matching P/R/F and a valid shape do not
// prove a real provider mechanism or make the current empty loader allowlist pass.
fn controls(id: &str) -> ReviewedAgentCliControls {
    ReviewedAgentCliControls {
        version: 1,
        record_id: format!("synthetic-review-{id}"),
        provider: AgentCliProvider::parse(id).expect("fixture provider must be explicit"),
        cli_version: "test-version".into(),
        executable_sha256: "a".repeat(64),
        resource_set_id: "private-resource-set".into(),
        payload_sha256: resource_payload_sha256(b"synthetic resource tuple, not installed files")
            .unwrap(),
        control_files: vec![ReviewedControlFile {
            path: "input/private-controls.json".into(),
            sha256: "b".repeat(64),
        }],
        loading_method: "synthetic-unverified-loader".into(),
        fixed_args: vec![ReviewedControlArg {
            flag: "--syntheticControlFile".into(),
            value: Some("input/private-controls.json".into()),
        }],
        verification_evidence_sha256: "c".repeat(64),
        independent_review_sha256: "d".repeat(64),
        adapter: None,
    }
}

fn provider(id: &str) -> ObservedCliProvider {
    let controls = controls(id);
    ObservedCliProvider {
        host_cli: None,
        provider_id: id.into(),
        version: controls.cli_version.clone(),
        executable_sha256: controls.executable_sha256.clone(),
        models: vec!["explicit-model".into()],
        efforts: vec!["high".into()],
        model_efforts: Default::default(),
        source: "live_catalog".into(),
        resource_set_id: controls.resource_set_id.clone(),
        resource_manifest_hash: controls.execution_manifest_sha256().unwrap(),
        controls: Some(controls),
    }
}

#[test]
fn model_specific_cli_efforts_are_checked_before_dispatch() {
    let mut observed = provider("codex");
    observed.models = vec!["model-a".into(), "model-b".into()];
    observed.efforts = vec!["low".into(), "high".into()];
    observed.model_efforts = [
        ("model-a".into(), vec!["low".into()]),
        ("model-b".into(), vec![]),
    ]
    .into();
    assert!(cli_selection_discovered(&observed, "model-a", Some("low")));
    assert!(!cli_selection_discovered(
        &observed,
        "model-a",
        Some("high")
    ));
    assert!(cli_selection_discovered(&observed, "model-a", None));
    assert!(cli_selection_discovered(&observed, "model-b", None));
    assert!(!cli_selection_discovered(&observed, "model-b", Some("low")));
    observed.model_efforts.remove("model-a");
    assert!(!cli_selection_discovered(&observed, "model-a", Some("low")));
}

#[test]
fn known_catalog_models_do_not_require_an_effort_override() {
    let mut observed = provider("antigravity");
    observed.models.push("gemini-3.8-flash-high".into());
    assert!(cli_selection_discovered(
        &observed,
        "gemini-3.8-flash-high",
        None
    ));
    assert!(!cli_selection_discovered(&observed, "unknown-high", None));
    assert!(cli_selection_discovered(&observed, "explicit-model", None));
    observed.provider_id = "workbuddy".into();
    assert!(cli_selection_discovered(
        &observed,
        "gemini-3.8-flash-high",
        None
    ));
}

fn catalog(project: &str, providers: Vec<ObservedCliProvider>) -> AgentTeamCatalog {
    AgentTeamCatalog {
        project: project.into(),
        target: AgentExecutionTarget {
            client_id: "test-runner".into(),
            runner_instance_id: "private-runner-instance".into(),
        },
        observed_at_ms: 1_000,
        expires_at_ms: 2_000,
        providers,
        unavailable_providers: vec![],
        host_profile_ids: vec!["explicit-host-profile".into()],
        host_catalog_ref: "private-host-catalog-ref".into(),
        host_profiles_expires_at_ms: 2_000,
        host_profiles_error: None,
        host_default_supported: true,
    }
}

fn run_fixture() -> (tempfile::TempDir, Database, VerifiedAgentRun) {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let grant = db
        .create_isolated_agent("test-owner", "agent:test-runner:project", None)
        .unwrap();
    let binding = db.authenticate_agent_run(&grant.token).unwrap();
    (temp, db, binding)
}

#[test]
fn agent_team_default_selection_does_not_depend_on_expired_host_profiles() {
    let mut observed = catalog("agent:test-runner:project", vec![]);
    observed.host_profiles_expires_at_ms = now_ms() + 60_000;
    assert!(validate_host_selection(&observed, "explicit-host-profile", true).is_ok());
    observed.host_profiles_error = Some("agent_team_host_profiles_expired".into());
    observed.host_profiles_expires_at_ms = 0;
    observed.host_profile_ids.clear();
    assert!(validate_host_selection(&observed, "default", true).is_ok());
    assert!(validate_host_selection(&observed, "default", false).is_ok());
    assert_eq!(
        validate_host_selection(&observed, "explicit-host-profile", false).unwrap_err(),
        "agent_team_host_profiles_expired"
    );
    observed.host_default_supported = false;
    assert_eq!(
        validate_host_selection(&observed, "default", true).unwrap_err(),
        "agent_team_host_default_unavailable"
    );
    // Direct CLI execution still requires its own catalog/controls, but Host
    // support is needed only if this attempt later enters recovery.
    assert!(validate_host_selection(&observed, "default", false).is_ok());
    let projected = catalog_projection(&observed);
    assert_eq!(
        projected["host_profiles_error"],
        "agent_team_host_profiles_expired"
    );
    assert_eq!(projected["default_selection"], "default");
}

#[test]
fn agent_team_start_contracts_normalize_omission_without_inventing_profile_ids() {
    let member: crate::agent_team::AgentStart = serde_json::from_value(json!({
        "root_task_id":"root", "source_task_id":"child", "enrollment_id":"enrollment", "idempotency_key":"stable"
    })).unwrap();
    assert!(member.validate().is_ok());
    assert_eq!(member.thinking_profile_id, "");
    assert!(member.validate().is_ok());
    let cli: crate::agent_team::CliStart = serde_json::from_value(json!({
        "root_task_id":"root", "source_task_id":"child", "call_index":0, "idempotency_key":"stable"
    }))
    .unwrap();
    assert_eq!(cli.fallback_thinking_profile_id, "default");
}

fn assert_controls_unverified(provider: &ObservedCliProvider) {
    assert!(require_provider_controls(provider).is_err());
    let projection = catalog_projection(&catalog(
        "agent:test-runner:project",
        vec![provider.clone()],
    ));
    assert_eq!(
        projection["providers"][0]["execution_controls_verified"],
        false
    );
    assert_eq!(projection["account_availability_verified"], false);
}

#[test]
fn agent_team_execution_controls_require_a_complete_review_for_this_binary() {
    for id in ["workbuddy", "antigravity"] {
        let reviewed = provider(id);
        assert_eq!(
            validate_provider_controls_binding(&reviewed).unwrap(),
            reviewed.controls.as_ref().unwrap()
        );
        assert_eq!(
            require_provider_controls(&reviewed).unwrap_err(),
            "agent_cli_controls_mechanism_unverified"
        );
        let (_, business_args, _) = crate::tool_runtime::fixed_cli_process(
            id,
            "explicit-model",
            Some("high"),
            "Explicit task",
        )
        .unwrap();
        assert_eq!(
            compose_reviewed_cli_argv(reviewed.controls.as_ref().unwrap(), &business_args)
                .unwrap_err(),
            "agent_cli_controls_mechanism_unverified"
        );
        assert_controls_unverified(&reviewed);
        let mut missing = reviewed.clone();
        missing.controls = None;
        assert_eq!(
            validate_provider_controls_binding(&missing).unwrap_err(),
            "agent_cli_delegation_controls_unverified"
        );
        assert_controls_unverified(&missing);
        for changed in [
            "provider",
            "version",
            "executable",
            "manifest",
            "resource",
            "record",
            "payload",
            "verification",
            "review",
        ] {
            let mut invalid = reviewed.clone();
            match changed {
                "provider" => {
                    invalid.provider_id = if id == "workbuddy" {
                        "antigravity"
                    } else {
                        "workbuddy"
                    }
                    .into()
                }
                "version" => invalid.version = "another-cli-version".into(),
                "executable" => invalid.executable_sha256 = "e".repeat(64),
                "manifest" => invalid.resource_manifest_hash = "f".repeat(64),
                "resource" => invalid.resource_set_id = "another-resource-set".into(),
                "record" => invalid
                    .controls
                    .as_mut()
                    .unwrap()
                    .record_id
                    .push_str("-modified"),
                "payload" => invalid.controls.as_mut().unwrap().payload_sha256 = "e".repeat(64),
                "verification" => {
                    invalid
                        .controls
                        .as_mut()
                        .unwrap()
                        .verification_evidence_sha256 = "e".repeat(64)
                }
                "review" => {
                    invalid.controls.as_mut().unwrap().independent_review_sha256 = "e".repeat(64)
                }
                _ => unreachable!(),
            }
            assert!(
                validate_provider_controls_binding(&invalid).is_err(),
                "{id}: {changed}"
            );
            assert_controls_unverified(&invalid);
        }
        for evidence in [
            String::new(),
            "c".repeat(63),
            "g".repeat(64),
            "C".repeat(64),
        ] {
            let mut invalid = reviewed.clone();
            invalid
                .controls
                .as_mut()
                .unwrap()
                .verification_evidence_sha256 = evidence;
            assert!(validate_provider_controls_binding(&invalid).is_err());
            assert_controls_unverified(&invalid);
        }
    }
}

#[test]
fn agent_team_execution_controls_cannot_override_fixed_model_effort_or_stream_protocol() {
    for id in ["workbuddy", "antigravity"] {
        for flag in ["--model", "--effort", "--input-format", "--output-format"] {
            for addition in [flag.to_string(), format!("{flag}=replacement")] {
                let mut invalid = provider(id);
                invalid
                    .controls
                    .as_mut()
                    .unwrap()
                    .fixed_args
                    .push(ReviewedControlArg {
                        flag: addition,
                        value: Some("replacement".into()),
                    });
                assert!(invalid.controls.as_ref().unwrap().validate_shape().is_err());
                assert_controls_unverified(&invalid);
            }
        }
    }
}

#[test]
fn agent_team_execution_controls_cannot_enable_permission_bypass_or_allow_all_tools() {
    for id in ["workbuddy", "antigravity"] {
        for flag in [
            "--dangerously-skip-permissions",
            "--yolo",
            "--allow-all-tools",
            "--allowedTools",
        ] {
            for addition in [flag.to_string(), format!("{flag}=true")] {
                let mut invalid = provider(id);
                invalid
                    .controls
                    .as_mut()
                    .unwrap()
                    .fixed_args
                    .push(ReviewedControlArg {
                        flag: addition,
                        value: Some("true".into()),
                    });
                assert!(invalid.controls.as_ref().unwrap().validate_shape().is_err());
                assert_controls_unverified(&invalid);
            }
        }
    }
}

#[test]
fn agent_team_execution_controls_reject_print_override_and_argv_terminator_but_preserve_reviewed_empty_value(
) {
    for id in ["workbuddy", "antigravity"] {
        for addition in ["--", "--print", "--print=false"] {
            let mut invalid = provider(id);
            invalid
                .controls
                .as_mut()
                .unwrap()
                .fixed_args
                .push(ReviewedControlArg {
                    flag: addition.into(),
                    value: None,
                });
            assert!(invalid.controls.as_ref().unwrap().validate_shape().is_err());
            assert_controls_unverified(&invalid);
        }
    }
    // Some("") is an exact token, not None. Valid argv shape still does not
    // verify a real disable-tools mechanism or enable execution.
    let mut reviewed = provider("workbuddy");
    reviewed.controls.as_mut().unwrap().fixed_args = vec![ReviewedControlArg {
        flag: "--tools".into(),
        value: Some(String::new()),
    }];
    reviewed.resource_manifest_hash = reviewed
        .controls
        .as_ref()
        .unwrap()
        .execution_manifest_sha256()
        .unwrap();
    assert!(validate_provider_controls_binding(&reviewed).is_ok());
    assert_eq!(
        require_provider_controls(&reviewed).unwrap_err(),
        "agent_cli_controls_mechanism_unverified"
    );
    let (_, business_args, _) = crate::tool_runtime::fixed_cli_process(
        "workbuddy",
        "explicit-model",
        Some("high"),
        "Authorized brief",
    )
    .unwrap();
    let record = reviewed.controls.as_ref().unwrap();
    let mut with_empty = business_args.clone();
    with_empty.extend(["--tools".into(), String::new()]);
    assert_eq!(
        validate_reviewed_cli_argv(record, &with_empty).unwrap_err(),
        "agent_cli_controls_mechanism_unverified"
    );
    assert_eq!(
        compose_reviewed_cli_argv(record, &business_args).unwrap_err(),
        "agent_cli_controls_mechanism_unverified"
    );
    let mut absent_value = record.clone();
    absent_value.fixed_args[0].value = None;
    assert!(absent_value.validate_shape().is_ok());
    assert_ne!(
        record.review_sha256().unwrap(),
        absent_value.review_sha256().unwrap()
    );
    assert_ne!(
        record.execution_manifest_sha256().unwrap(),
        absent_value.execution_manifest_sha256().unwrap()
    );
    let mut without_value = business_args;
    without_value.push("--tools".into());
    assert_eq!(
        validate_reviewed_cli_argv(&absent_value, &without_value).unwrap_err(),
        "agent_cli_controls_mechanism_unverified"
    );
    assert_eq!(
        validate_reviewed_cli_argv(record, &without_value).unwrap_err(),
        "agent_cli_controls_argument_override"
    );
    assert_eq!(
        validate_reviewed_cli_argv(&absent_value, &with_empty).unwrap_err(),
        "agent_cli_controls_argument_override"
    );
}

#[test]
fn agent_team_execution_controls_reject_missing_oversized_or_nul_arguments() {
    for arguments in [
        vec![],
        vec![
            ReviewedControlArg {
                flag: "--synthetic-control".into(),
                value: None
            };
            33
        ],
        vec![ReviewedControlArg {
            flag: format!("--{}", "x".repeat(65)),
            value: None,
        }],
        vec![ReviewedControlArg {
            flag: "--synthetic-control".into(),
            value: Some("x".repeat(513)),
        }],
        vec![ReviewedControlArg {
            flag: "--synthetic\0control".into(),
            value: None,
        }],
        vec![ReviewedControlArg {
            flag: "--synthetic-control".into(),
            value: Some("x\0y".into()),
        }],
    ] {
        let mut invalid = provider("workbuddy");
        invalid.controls.as_mut().unwrap().fixed_args = arguments;
        assert!(invalid.controls.as_ref().unwrap().validate_shape().is_err());
        assert_controls_unverified(&invalid);
    }
}

#[test]
fn agent_team_execution_controls_cannot_be_constructed_from_approval_bits_or_raw_argv() {
    let original = serde_json::to_value(controls("workbuddy")).unwrap();
    for field in [
        "direct_agent_denied",
        "workflow_skill_routes_denied",
        "unreviewed_mcp_denied",
        "fail_closed_verified",
        "approved",
        "control_args",
    ] {
        let mut forged = original.clone();
        forged[field] = if field == "control_args" {
            json!(["--model", "replacement"])
        } else {
            json!(true)
        };
        assert!(
            serde_json::from_value::<ReviewedAgentCliControls>(forged).is_err(),
            "{field}"
        );
    }
    let mut raw_argv = original;
    raw_argv["fixed_args"] = json!(["--syntheticControlFile", "input/private-controls.json"]);
    assert!(serde_json::from_value::<ReviewedAgentCliControls>(raw_argv).is_err());
}

#[test]
fn agent_team_execution_control_flag_case_is_preserved_by_the_shared_shape_contract() {
    for id in ["workbuddy", "antigravity"] {
        let reviewed = provider(id);
        let record = reviewed.controls.as_ref().unwrap();
        assert!(validate_provider_controls_binding(&reviewed).is_ok());
        let (_, mut actual, _) = crate::tool_runtime::fixed_cli_process(
            id,
            "explicit-model",
            Some("high"),
            "Explicit task",
        )
        .unwrap();
        actual.extend([
            "--syntheticControlFile".into(),
            "input/private-controls.json".into(),
        ]);
        assert_eq!(
            validate_reviewed_cli_argv(record, &actual).unwrap_err(),
            "agent_cli_controls_mechanism_unverified"
        );
        let flag_index = actual.len() - 2;
        actual[flag_index] = "--syntheticcontrolfile".into();
        assert_eq!(
            validate_reviewed_cli_argv(record, &actual).unwrap_err(),
            "agent_cli_controls_argument_override"
        );
    }
}

#[test]
fn agent_team_execution_catalog_rejects_stale_wrong_scope_and_invalid_hash_facts() {
    let (_temp, _db, binding) = run_fixture();
    let valid = catalog(binding.project(), vec![provider("workbuddy")]);
    assert!(validate_catalog(&_db, &valid, &binding, 1_500).is_ok());
    let mut wrong_project = valid.clone();
    wrong_project.project = "agent:test-runner:other".into();
    assert!(validate_catalog(&_db, &wrong_project, &binding, 1_500).is_err());
    let mut wrong_runner = valid.clone();
    wrong_runner.target.client_id = "other-runner".into();
    assert!(validate_catalog(&_db, &wrong_runner, &binding, 1_500).is_err());
    assert!(validate_catalog(&_db, &valid, &binding, 999).is_err());
    assert!(validate_catalog(&_db, &valid, &binding, 2_000).is_err());
    for malformed in [
        "g".repeat(64),
        "a".repeat(63),
        String::new(),
        "A".repeat(64),
    ] {
        let mut binary = valid.clone();
        binary.providers[0].executable_sha256 = malformed.clone();
        assert!(validate_catalog(&_db, &binary, &binding, 1_500).is_err());
        let mut manifest = valid.clone();
        manifest.providers[0].resource_manifest_hash = malformed;
        assert!(validate_catalog(&_db, &manifest, &binding, 1_500).is_err());
    }
}

#[test]
fn execution_catalog_accepts_more_than_two_registered_ids_without_granting_execution() {
    let (_temp, db, binding) = run_fixture();
    let providers = ["first-cli", "second-cli", "third-cli"]
        .into_iter()
        .map(|id| {
            let mut observed = provider("workbuddy");
            observed.provider_id = id.into();
            observed.controls = None;
            observed
        })
        .collect();
    let observed = catalog(binding.project(), providers);
    validate_catalog(&db, &observed, &binding, 1_500).unwrap();
    assert!(observed
        .providers
        .iter()
        .all(|entry| require_provider_controls(entry).is_err()));
}

#[test]
fn agent_team_execution_catalog_preserves_models_without_inventing_effort_choices() {
    let (_temp, _db, binding) = run_fixture();
    let mut observed = provider("workbuddy");
    observed.efforts.clear();
    let observed = catalog(binding.project(), vec![observed]);
    assert!(validate_catalog(&_db, &observed, &binding, 1_500).is_ok());
    let projected = catalog_projection(&observed);
    assert_eq!(
        projected["providers"][0]["models"],
        json!(["explicit-model"])
    );
    assert_eq!(projected["providers"][0]["efforts"], json!([]));
    assert_eq!(projected["selection_required"], false);
}

#[test]
fn agent_team_execution_fixed_argv_keeps_model_and_effort_literal_and_brief_on_stdin() {
    let model = "selected-model";
    let effort = "high";
    let brief = "Only the worker's authorized brief.\n--model must-stay-input";
    let (binary, args, stdin) =
        crate::tool_runtime::fixed_cli_process("workbuddy", model, Some(effort), brief).unwrap();
    assert_eq!(binary, "codebuddy");
    assert_eq!(
        args,
        [
            "--model",
            model,
            "--effort",
            effort,
            "--print",
            "-y",
            "--input-format",
            "text",
            "--output-format",
            "json",
        ]
    );
    assert_eq!(stdin, brief);
    assert!(!args.iter().any(|arg| arg.contains("must-stay-input")));

    let (binary, args, stdin) =
        crate::tool_runtime::fixed_cli_process("antigravity", model, Some(effort), brief).unwrap();
    assert_eq!(binary, "agy");
    assert_eq!(
        args,
        [
            "--model",
            model,
            "--effort",
            effort,
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]
    );
    assert!(stdin.ends_with('\n'));
    let input: Value = serde_json::from_str(&stdin).unwrap();
    assert_eq!(input, json!({"event":"user","message":{"content":brief}}));
    assert!(!args.iter().any(|arg| arg.contains("must-stay-input")));
    assert!(crate::tool_runtime::fixed_cli_process("workbuddy", model, None, brief).is_ok());
    assert!(
        crate::tool_runtime::fixed_cli_process("unknown-provider", model, Some(effort), brief)
            .is_err()
    );
}

#[test]
fn agent_team_execution_default_effort_is_allowed_with_or_without_override_options() {
    let mut observed = provider("antigravity");
    observed.efforts.clear();
    assert!(cli_selection_discovered(&observed, "explicit-model", None));
    assert!(!cli_selection_discovered(&observed, "unknown-model", None));
    assert!(!cli_selection_discovered(
        &observed,
        "explicit-model",
        Some("none")
    ));
    observed.efforts.push("high".into());
    assert!(cli_selection_discovered(&observed, "explicit-model", None));
    assert!(cli_selection_discovered(
        &observed,
        "explicit-model",
        Some("high")
    ));
    assert!(!cli_selection_discovered(
        &observed,
        "explicit-model",
        Some("default")
    ));
    let (_, args, _) =
        crate::tool_runtime::fixed_cli_process("antigravity", "explicit-model", None, "brief")
            .unwrap();
    assert!(!args.iter().any(|arg| arg == "--effort"));
}

#[test]
fn agent_team_execution_catalog_projection_omits_private_resources_controls_and_evidence() {
    let mut private = provider("workbuddy");
    let record = private.controls.as_mut().unwrap();
    record.record_id = "private-record-id".into();
    record.loading_method = "private-loading-method".into();
    record.fixed_args[0].flag = "--privateControlFlag".into();
    private.resource_manifest_hash = record.execution_manifest_sha256().unwrap();
    let private_hashes = [
        record.executable_sha256.clone(),
        record.payload_sha256.clone(),
        record.control_files[0].sha256.clone(),
        record.verification_evidence_sha256.clone(),
        record.independent_review_sha256.clone(),
        record.review_sha256().unwrap(),
        private.resource_manifest_hash.clone(),
    ];
    assert!(validate_provider_controls_binding(&private).is_ok());
    let internal = catalog("agent:test-runner:project", vec![private]);
    let output = catalog_projection(&internal);
    assert_eq!(output["providers"][0]["models"], json!(["explicit-model"]));
    assert_eq!(output["providers"][0]["efforts"], json!(["high"]));
    assert_eq!(
        output["thinking_profile_ids"],
        json!(["explicit-host-profile"])
    );
    assert_eq!(output["account_availability_verified"], false);
    assert_eq!(output["selection_required"], false);
    assert_eq!(output["providers"][0]["execution_controls_verified"], false);
    assert!(output["providers"][0].get("controls").is_none());
    let serialized = serde_json::to_string(&output).unwrap();
    for hidden in [
        "target",
        "runner_instance_id",
        "private-runner-instance",
        "resource_set_id",
        "resource_manifest_hash",
        "private-resource-set",
        "executable_sha256",
        "record_id",
        "private-record-id",
        "payload_sha256",
        "loading_method",
        "private-loading-method",
        "fixed_args",
        "control_files",
        "privateControlFlag",
        "input/private-controls.json",
        "control_args",
        "evidence_sha256",
        "verification_evidence_sha256",
        "independent_review_sha256",
        "host_catalog_ref",
        "private-host-catalog-ref",
    ] {
        assert!(
            !serialized.contains(hidden),
            "private catalog field escaped: {hidden}"
        );
    }
    for hash in private_hashes {
        assert!(!serialized.contains(&hash));
    }
}

#[test]
fn agent_team_execution_reports_missing_cli_prerequisites_without_dispatch() {
    let mut observed = catalog("agent:test-runner:project", vec![]);
    observed.unavailable_providers.push(UnavailableCliProvider {
        provider_id: "workbuddy".into(),
        reason_code: "agent_isolated_execution_unavailable",
    });
    let output = catalog_projection(&observed);
    assert_eq!(output["providers"], json!([]));
    assert_eq!(
        output["unavailable_providers"][0]["reason_code"],
        "agent_isolated_execution_unavailable"
    );
    let denial = denied("team_plan_required");
    assert!(!denial.success);
    assert_eq!(denial.output["state_changed"], false);
    assert_eq!(denial.output["dispatch_certainty"], "not_started");
    assert!(denial.output["next_action"]
        .as_str()
        .unwrap()
        .contains("do not create a replacement manager"));
}

#[test]
fn agent_team_execution_close_request_id_replays_stably_and_separates_targets() {
    let close = AgentExecutionClose {
        version: 1,
        close_ref: AgentExecutionCloseRef {
            close_id: "original-close".into(),
            source: AgentRunRef {
                agent_id: "original-agent".into(),
                agent_run_id: "original-run".into(),
                generation: 1,
            },
        },
        target: AgentExecutionTarget {
            client_id: "test-runner".into(),
            runner_instance_id: "original-instance".into(),
        },
        deadline_at_ms: 30_000,
        members: vec![],
        members_sha256: compute_members_sha256(&[]).unwrap(),
    };
    let original = close_request_id(&close);
    assert_eq!(close_request_id(&close.clone()), original);
    assert!(original.starts_with("wc_close_"));
    assert_eq!(original.len(), "wc_close_".len() + 64);
    // RunnerRegistry validates control request IDs with its 80-byte ID bound.
    assert!(original.len() <= 80);
    let mut another_client = close.clone();
    another_client.target.client_id = "another-runner".into();
    assert_ne!(close_request_id(&another_client), original);
    let mut another_instance = close.clone();
    another_instance.target.runner_instance_id = "replacement-instance".into();
    assert_ne!(close_request_id(&another_instance), original);
    let mut another_close = close;
    another_close.close_ref.close_id = "other-close".into();
    assert_ne!(close_request_id(&another_close), original);
}

#[test]
fn agent_team_execution_model_arguments_cannot_supply_transport_or_executor_identity() {
    assert!(
        serde_json::from_value::<ExactAttempt>(json!({"source_attempt_id":"original"})).is_ok()
    );
    assert!(serde_json::from_value::<CatalogRequest>(json!({"task_id":"assigned"})).is_ok());
    for field in [
        "project",
        "owner_user",
        "executor",
        "run_id",
        "token",
        "resource_set_id",
        "control_args",
    ] {
        let mut attempt = json!({"source_attempt_id":"original"});
        attempt[field] = json!("forged");
        assert!(serde_json::from_value::<ExactAttempt>(attempt).is_err());
        let mut catalog = json!({"task_id":"assigned"});
        catalog[field] = json!("forged");
        assert!(serde_json::from_value::<CatalogRequest>(catalog).is_err());
    }
}

#[test]
fn agent_team_execution_worker_observation_exposes_cas_without_private_completion_text() {
    let snapshot = webcodex_store::AgentCliWorkerTaskSnapshot {
        execution: webcodex_store::AgentPublicTaskExecution {
            execution_id: "private-execution".into(),
            task_id: "original-worker-task".into(),
            agent_id: "private-worker-agent".into(),
            started_run_id: "private-started-run".into(),
            started_generation: 2,
            state: AgentTaskState::Failed,
            revision: 8,
            started_at_unix_ms: 1000,
            terminal_at_unix_ms: Some(2000),
            completed_run_id: Some("private-completed-run".into()),
            completed_generation: Some(3),
            terminal_result: Some("private-result".into()),
            terminal_reason: Some("private-reason".into()),
            decision_reviews: vec![],
        },
        completion_expected_revision: 7,
    };
    let projection = worker_task_projection(&snapshot);
    assert_eq!(
        projection,
        json!({
            "task_id":"original-worker-task", "state":"failed",
            "revision":8, "completion_expected_revision":7,
            "result":"private-result", "reason":"private-reason"
        })
    );
    let serialized = projection.to_string();
    assert!(serialized.contains("private-result"));
    assert!(!serialized.contains("private-worker-agent"));
    assert!(projection.get("stopped_confirmed").is_none());
    assert!(projection.get("job").is_none());
}

#[test]
fn agent_team_execution_input_material_changes_launch_hash_and_is_not_argv() {
    use webcodex_core::runner_protocol::compute_launch_intent_hash;
    let mut inputs = vec![webcodex_store::AgentEnrollmentSnapshot {
        name: "source.js".into(),
        content: "const x=1;\n".into(),
        sha256: "a".repeat(64),
    }];
    let prompt = cli_worker_input_prompt("Review material", &inputs, None).unwrap();
    assert!(prompt.contains("normal project working directory"));
    let (_, args, stdin) =
        crate::tool_runtime::fixed_cli_process("antigravity", "gemini-fixed", None, &prompt)
            .unwrap();
    assert!(!args.iter().any(|a| a.contains("const x")));
    let process = AgentExecutionProcess {
        executable: "sha256:example".into(),
        args,
    };
    let h = compute_launch_intent_hash(&process, Some(&stdin), 900).unwrap();
    inputs[0].content = "const x=2;\n".into();
    let (_, _, second) = crate::tool_runtime::fixed_cli_process(
        "antigravity",
        "gemini-fixed",
        None,
        &cli_worker_input_prompt("Review material", &inputs, None).unwrap(),
    )
    .unwrap();
    assert_ne!(
        h,
        compute_launch_intent_hash(&process, Some(&second), 900).unwrap()
    );
}

#[test]
fn cli_default_selection_keeps_supported_effort_without_model_discovery() {
    let mut observed = provider("workbuddy");
    observed.models.clear();
    observed.efforts.clear();
    observed.model_efforts.clear();
    observed.source = "configured_default".into();
    let default = webcodex_core::cli_adapter::CliModelSelection::default();
    assert!(cli_selection_discovered(&observed, &default, None));
    assert!(cli_selection_discovered(&observed, &default, Some("high")));
    assert!(!cli_selection_discovered(&observed, "default", None));
    let (_, args, stdin) = crate::tool_runtime::fixed_cli_process(
        "workbuddy",
        &default,
        Some("high"),
        "Independent delegated task",
    )
    .unwrap();
    assert!(args.windows(2).any(|pair| pair == ["--effort", "high"]));
    assert!(!args.iter().any(|arg| arg == "--model"));
    assert_eq!(stdin, "Independent delegated task");
}

#[test]
fn cli_default_effort_support_is_checked_by_the_execution_adapter() {
    let mut adapter = webcodex_core::cli_adapter::cli_adapters(&[])
        .unwrap()
        .into_iter()
        .find(|adapter| adapter.id == "workbuddy")
        .unwrap();
    adapter.invocation.as_mut().unwrap().effort_flag = None;
    let error = crate::tool_runtime::cli_process_with_adapter(
        Some(&adapter),
        "workbuddy",
        webcodex_core::cli_adapter::CliModelSelection::default(),
        Some("high"),
        "Independent delegated task",
    )
    .unwrap_err();
    assert_eq!(error.output["error_kind"], "cli_adapter_effort_unsupported");
}
