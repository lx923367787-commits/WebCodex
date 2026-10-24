use super::*;

fn temporary_path(label: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "webcodex-office-{label}-{}-{}",
            std::process::id(),
            NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ))
        .join(POLICY_FILE)
}

fn project(id: &str) -> OfficeProject {
    OfficeProject {
        id: id.into(),
        name: "Project".into(),
        path: "C:/project".into(),
        connected: true,
        active_jobs: 5,
        policy: OfficePolicy::default(),
    }
}

fn call(model: &str) -> OfficeCliCall {
    OfficeCliCall {
        provider_id: "workbuddy".into(),
        model: model.into(),
        effort: None,
    }
}

fn agent(id: &str, cli_calls: Vec<OfficeCliCall>) -> OfficeAgentPlan {
    OfficeAgentPlan {
        id: id.into(),
        task: format!("Implement {id}"),
        cli_calls,
    }
}

fn custom_policy() -> OfficePolicy {
    let mut settings = OfficePolicyInput::default();
    settings.mode = PolicyMode::Custom;
    settings.subagents = CountRange { min: 1, max: 2 };
    settings.cli_per_agent = CountRange { min: 1, max: 2 };
    settings.providers[0].model = Some("model-a".into());
    settings.providers[1].enabled = false;
    OfficePolicy {
        revision: 2,
        settings,
    }
}

#[test]
fn policy_round_trip_cas_and_connection_scoping_preserve_previous_state() {
    let path = temporary_path("cas");
    let first = save_policy(
        &path,
        "server-a\nrunner",
        project("id"),
        0,
        OfficePolicyInput::default(),
    )
    .unwrap();
    assert_eq!(first.revision, 1);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        save_policy(
            &path,
            "server-a\nrunner",
            project("id"),
            0,
            OfficePolicyInput::default()
        )
        .unwrap_err()
        .code,
        "ai_office_revision_conflict"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    save_policy(
        &path,
        "server-b\nrunner",
        project("id"),
        0,
        OfficePolicyInput::default(),
    )
    .unwrap();
    let store = read_store(&path).unwrap();
    assert_eq!(store.projects.len(), 2);
    assert!(
        !store.projects[&store_key("server-a\nrunner", "id")]
            .project
            .connected
    );
    assert_eq!(
        store.projects[&store_key("server-a\nrunner", "id")]
            .project
            .active_jobs,
        0
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(path.parent().unwrap()).unwrap();
}

#[test]
fn corrupt_store_is_preserved_and_not_replaced_by_save() {
    let path = temporary_path("corrupt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"broken original").unwrap();
    assert_eq!(
        save_policy(
            &path,
            "server",
            project("id"),
            0,
            OfficePolicyInput::default()
        )
        .unwrap_err()
        .code,
        "ai_office_store_invalid"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"broken original");
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(path.parent().unwrap()).unwrap();
}

#[test]
fn minimum_counts_apply_to_complete_plan_including_manager() {
    let policy = custom_policy();
    let plan = OfficePlan {
        manager: agent("manager", vec![]),
        subagents: vec![],
    };
    let result = evaluate_plan(&policy, &plan).unwrap();
    assert!(!result.admitted);
    assert_eq!(
        result.violations.iter().map(|v| v.code).collect::<Vec<_>>(),
        vec!["subagent_count", "cli_count"]
    );
    let plan = OfficePlan {
        manager: agent("manager", vec![call("model-a")]),
        subagents: vec![agent("child", vec![call("model-a")])],
    };
    let result = evaluate_plan(&policy, &plan).unwrap();
    assert!(result.admitted);
    assert_eq!(result.cli_call_count, 2);
    assert_eq!(result.enforcement, "demo_plan_only");
}

#[test]
fn custom_plan_rejects_disabled_provider_and_model_effort_mismatch() {
    let mut policy = custom_policy();
    policy.settings.providers[0].effort = Some("high".into());
    let plan = OfficePlan {
        manager: agent("manager", vec![call("other-model")]),
        subagents: vec![agent(
            "child",
            vec![OfficeCliCall {
                provider_id: "antigravity".into(),
                model: "example".into(),
                effort: None,
            }],
        )],
    };
    let result = evaluate_plan(&policy, &plan).unwrap();
    let codes = result.violations.iter().map(|v| v.code).collect::<Vec<_>>();
    assert!(codes.contains(&"model_mismatch"));
    assert!(codes.contains(&"effort_mismatch"));
    assert!(codes.contains(&"provider_disabled"));
}

#[test]
fn auto_leaves_plan_choice_free_but_requires_explicit_model() {
    let mut policy = custom_policy();
    policy.settings.mode = PolicyMode::Auto;
    let plan = OfficePlan {
        manager: agent("solo", vec![]),
        subagents: vec![],
    };
    assert!(evaluate_plan(&policy, &plan).unwrap().admitted);
    let plan = OfficePlan {
        manager: agent("solo", vec![call("")]),
        subagents: vec![],
    };
    assert!(!evaluate_plan(&policy, &plan).unwrap().admitted);
}

#[test]
fn invalid_policy_ranges_and_duplicate_provider_ids_fail_before_save() {
    let mut policy = OfficePolicyInput::default();
    policy.subagents.min = 10;
    assert!(validate_policy(&policy).is_err());
    policy = OfficePolicyInput::default();
    policy.providers[1].provider_id = "workbuddy".into();
    assert!(validate_policy(&policy).is_err());
    policy = OfficePolicyInput::default();
    policy.cli_per_agent.max = 32;
    assert!(validate_policy(&policy).is_ok());
    policy.cli_per_agent.max = 33;
    assert!(validate_policy(&policy).is_err());
}

#[test]
fn duplicate_agent_ids_and_oversized_plans_are_rejected() {
    let policy = OfficePolicy::default();
    let mut plan = OfficePlan {
        manager: agent("same", vec![]),
        subagents: vec![agent("same", vec![])],
    };
    assert!(!evaluate_plan(&policy, &plan).unwrap().admitted);
    plan.subagents = (0..33)
        .map(|i| agent(&format!("agent-{i}"), vec![]))
        .collect();
    assert_eq!(
        evaluate_plan(&policy, &plan).unwrap_err().code,
        "ai_office_plan_too_large"
    );
}

#[test]
fn wire_policy_is_flat_and_matches_frontend_dto() {
    let policy = OfficePolicy::default();
    let value = serde_json::to_value(&policy).unwrap();
    assert_eq!(value["mode"], "auto");
    assert_eq!(value["revision"], 0);
    assert!(value.get("settings").is_none());
    assert_eq!(
        serde_json::from_value::<OfficePolicy>(value).unwrap(),
        policy
    );
}

#[test]
fn native_policy_decodes_bare_human_response_and_preserves_revision_and_settings() {
    let mut expected = custom_policy();
    expected.revision = 8;
    let response = serde_json::json!({"success":true,"error":null,"output":expected});
    assert_eq!(server_policy_from_result(response).unwrap(), expected);
    let initial = serde_json::json!({"success":true,"output":OfficePolicy::default()});
    assert_eq!(server_policy_from_result(initial).unwrap().revision, 0);
}

#[test]
fn native_policy_rejects_legacy_wrappers_and_requires_read_only_reconciliation() {
    for output in [
        serde_json::json!({"project":"agent:runner:one","policy":OfficePolicy::default(),"enforcement":"managed_tasks"}),
        serde_json::json!({"policy":OfficePolicy::default()}),
        serde_json::json!(null),
        serde_json::json!({"revision":1}),
    ] {
        let error = server_policy_from_result(serde_json::json!({"success":true,"output":output}))
            .unwrap_err();
        assert_eq!(error.code, "webcodex_contract_invalid");
        assert!(error.next_action.contains("只读"));
        assert!(error.next_action.contains("不要自动重发"));
    }
    let mut mixed = serde_json::to_value(OfficePolicy::default()).unwrap();
    mixed["project"] = serde_json::json!("agent:other:project");
    assert!(server_policy_from_result(serde_json::json!({"success":true,"output":mixed})).is_err());
    let missing_output =
        server_policy_from_result(serde_json::json!({"success":true})).unwrap_err();
    assert!(missing_output.next_action.contains("只读"));
}

#[test]
fn native_policy_keeps_server_revision_value_and_business_error_checks() {
    let policy = serde_json::to_value(OfficePolicy::default()).unwrap();
    for revision in [
        serde_json::json!(-1),
        serde_json::json!(u64::MAX),
        serde_json::json!("8"),
    ] {
        let mut invalid = policy.clone();
        invalid["revision"] = revision;
        assert!(
            server_policy_from_result(serde_json::json!({"success":true,"output":invalid}))
                .is_err()
        );
    }
    let mut invalid = policy;
    invalid["subagents"] = serde_json::json!({"min":3,"max":2});
    assert!(
        server_policy_from_result(serde_json::json!({"success":true,"output":invalid})).is_err()
    );
    let conflict = server_policy_from_result(serde_json::json!({"success":false,"error":"private diagnostic","output":{"error_kind":"team_policy_revision_conflict"}})).unwrap_err();
    assert_eq!(conflict.code, "team_policy_revision_conflict");
    assert!(!conflict.message.contains("private"));
}

#[test]
fn native_policy_transport_contract_failure_is_read_only_and_other_errors_are_preserved() {
    // team_office_call may reject an envelope before the policy decoder runs.
    let error = policy_response_error(DesktopError::new(
        "webcodex_contract_invalid",
        "Invalid envelope",
        "Old generic retry advice",
    ));
    assert_eq!(error.code, "webcodex_contract_invalid");
    assert!(error.next_action.contains("只读"));
    assert!(error.next_action.contains("不要自动重发"));
    for code in [
        "project_connection_changed",
        "runtime_not_ready",
        "team_policy_revision_conflict",
    ] {
        let original = DesktopError::new(code, "Known failure", "Original next action");
        assert_eq!(policy_response_error(original.clone()), original);
    }
}

#[test]
fn preview_uses_shared_rules_with_separate_persistence_and_fixed_project_ids() {
    let base = temporary_path("preview");
    let dir = base.parent().unwrap();
    let initial = preview_request(dir, serde_json::json!({"op":"policies"})).unwrap();
    assert_eq!(initial.as_array().unwrap().len(), 3);
    let settings = custom_policy().settings;
    let saved = preview_request(dir, serde_json::json!({"op":"save_policy","args":{"projectId":"demo-idle","expectedRevision":0,"policy":settings}})).unwrap();
    assert_eq!(saved["revision"], 1);
    assert!(
        !base.exists(),
        "preview must never open the real desktop policy filename"
    );
    let result = preview_request(dir, serde_json::json!({"op":"evaluate_plan","args":{"projectId":"demo-idle","expectedRevision":1,"plan":{"manager":{"id":"manager","task":"Review","cli_calls":[]},"subagents":[]}}})).unwrap();
    assert_eq!(result["admitted"], false);
    assert_eq!(result["enforcement"], "demo_plan_only");
    assert!(preview_request(dir, serde_json::json!({"op":"save_policy","args":{"projectId":"real-project","expectedRevision":0,"policy":OfficePolicyInput::default()}})).is_err());
    let reloaded = preview_request(dir, serde_json::json!({"op":"policies"})).unwrap();
    assert_eq!(reloaded[2]["policy"]["revision"], 1);
    std::fs::remove_file(dir.join("ai-office-preview-policies.json")).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
