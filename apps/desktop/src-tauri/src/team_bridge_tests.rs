use super::*;
use serde_json::json;

fn project(id: &str) -> RegisteredProject {
    RegisteredProject {
        id: id.into(),
        name: "Project".into(),
        path: "C:/project".into(),
        revision: "revision".into(),
        enabled: true,
        connected: true,
        agent_status: "online".into(),
        active_jobs: 0,
    }
}

#[test]
fn actions_are_only_the_five_human_office_operations() {
    for action in [
        "policy-get",
        "policy-save",
        "plan-list",
        "plan-read",
        "task-create",
    ] {
        assert_eq!(TeamOfficeAction::parse(action).unwrap().as_str(), action);
    }
    for action in [
        "plan-register",
        "plan-cancel",
        "cli-start",
        "cli-observe",
        "agent-start",
        "agent-observe",
    ] {
        assert_eq!(
            TeamOfficeAction::parse(action).unwrap_err().code,
            "ai_office_action_unavailable"
        );
    }
    for action in [
        "run_shell",
        "tools/call",
        "cli-start --tool run_shell",
        "",
        "POLICY-GET",
    ] {
        assert!(TeamOfficeAction::parse(action).is_err());
    }
}

#[test]
fn params_preserve_fences_and_reject_scope_or_transport_overrides() {
    let params = json!({"project":"agent:runner:one","expected_revision":7,"policy":ai_office::OfficePolicyInput::default()});
    let original = params.clone();
    assert_eq!(
        validate_params(TeamOfficeAction::PolicySave, "runner", &params).unwrap(),
        "agent:runner:one"
    );
    assert_eq!(params, original);
    for foreign in [
        "agent:runner2:one",
        "agent:runner:",
        "agent:runner:one\n",
        " agent:runner:one",
    ] {
        assert!(validate_params(
            TeamOfficeAction::PolicyGet,
            "runner",
            &json!({"project":foreign})
        )
        .is_err());
    }
    for key in [
        "tool",
        "tool_name",
        "params",
        "action",
        "server_url",
        "client_id",
        "token",
        "token_file",
        "config",
        "runner",
    ] {
        let mut overridden = params.clone();
        overridden[key] = json!("override");
        assert!(
            validate_params(TeamOfficeAction::PolicySave, "runner", &overridden).is_err(),
            "{key}"
        );
    }
    assert!(validate_params(
        TeamOfficeAction::TaskCreate,
        "runner",
        &json!({"project":"agent:runner:one","title":"Task","instruction":"x".repeat(65536),"idempotency_key":"same"})
    )
    .is_err());
}

#[test]
fn task_creation_uses_only_its_project_and_never_accepts_identity_or_source_fields() {
    let params = json!({"project":"agent:runner:one","title":"Task","instruction":"Bounded work","idempotency_key":"create-once"});
    assert!(validate_params(TeamOfficeAction::TaskCreate, "runner", &params).is_ok());
    for (key, value) in [
        ("referenced_project_id", "agent:runner:one"),
        ("referenced_project_id", "agent:runner:two"),
        ("assignee_agent_id", "caller-agent"),
        ("owner_user", "caller-owner"),
        ("source_conversation_id", "caller-conversation"),
        ("source_message_id", "caller-message"),
        ("run_id", "caller-run"),
    ] {
        let mut overridden = params.clone();
        overridden[key] = json!(value);
        assert!(
            validate_params(TeamOfficeAction::TaskCreate, "runner", &overridden).is_err(),
            "{key}"
        );
    }
}

#[test]
fn read_pagination_and_policy_cas_parameters_are_forwarded_without_rewriting() {
    for (action, params) in [
        (
            TeamOfficeAction::PlanList,
            json!({"project":"agent:runner:one","offset":20,"limit":20}),
        ),
        (
            TeamOfficeAction::PlanRead,
            json!({"project":"agent:runner:one","root_task_id":"root","attempts_offset":20,"attempts_limit":5}),
        ),
        (
            TeamOfficeAction::PolicySave,
            json!({"project":"agent:runner:one","expected_revision":7,"policy":ai_office::OfficePolicyInput::default()}),
        ),
    ] {
        let original = params.clone();
        assert!(validate_params(action, "runner", &params).is_ok());
        assert_eq!(params, original);
    }
    assert!(validate_params(
        TeamOfficeAction::PolicyGet,
        "runner",
        &json!({"project":"agent:runner:one","expected_revision":8})
    )
    .is_err());
}

#[test]
fn a_matching_prefix_is_not_substituted_for_real_registry_membership() {
    let projects = vec![project("agent:runner:one")];
    assert!(require_registered("agent:runner:one", &projects).is_ok());
    assert_eq!(
        require_registered("agent:runner:unknown", &projects)
            .unwrap_err()
            .code,
        "ai_office_unknown_project"
    );
}

#[test]
fn captured_connection_identity_includes_binary_server_runner_and_credential_file() {
    let captured = TeamConnection {
        binary: "webcodex.exe".into(),
        client_id: "runner".into(),
        connection: RuntimeConnection {
            runner_config: "runner.toml".into(),
            user_token_file: "user.token".into(),
            server_url: "https://server.example".into(),
        },
    };
    for changed in [
        TeamConnection {
            client_id: "other-runner".into(),
            ..captured.clone()
        },
        TeamConnection {
            binary: "other-webcodex.exe".into(),
            ..captured.clone()
        },
        TeamConnection {
            connection: RuntimeConnection {
                user_token_file: "other-user.token".into(),
                ..captured.connection.clone()
            },
            ..captured.clone()
        },
        TeamConnection {
            connection: RuntimeConnection {
                server_url: "https://other.example".into(),
                ..captured.connection.clone()
            },
            ..captured.clone()
        },
    ] {
        assert_ne!(captured, changed);
    }
}

#[test]
fn business_failure_never_becomes_policy_success_or_raw_diagnostic() {
    let failure = successful_team_output(json!({"success":false,"error":"secret diagnostic","output":{"error_kind":"team_policy_revision_conflict"}})).unwrap_err();
    assert_eq!(failure.code, "team_policy_revision_conflict");
    assert!(!failure.message.contains("secret"));
    assert!(successful_team_output(json!({"success":true})).is_err());
    assert!(successful_team_output(json!({"output":{}})).is_err());
    assert_eq!(
        successful_team_output(json!({"success":true,"output":{"root_task_id":"root"}})).unwrap()
            ["root_task_id"],
        "root"
    );
}

#[tokio::test]
async fn offline_native_save_fails_and_preserves_old_local_cache() {
    let data = std::env::temp_dir().join(format!(
        "webcodex-team-bridge-{}-{}",
        std::process::id(),
        NEXT_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&data).unwrap();
    let cache = data.join("ai-office-policies.json");
    std::fs::write(&cache, b"preserved offline cache").unwrap();
    let state = AppState::new(data.clone(), data.join("resources")).unwrap();
    let error = state
        .save_ai_office_policy(
            "agent:runner:one".into(),
            0,
            ai_office::OfficePolicyInput::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "runtime_not_ready");
    assert_eq!(std::fs::read(&cache).unwrap(), b"preserved offline cache");
    drop(state);
    std::fs::remove_file(&cache).unwrap();
    std::fs::remove_dir(&data).unwrap();
}
