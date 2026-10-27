//! Fresh native chats get distinct configured Agents; discovery and malformed
//! calls never manufacture a binding. All identities and credentials are synthetic.
use super::*;

async fn native_call(
    f: &Fixture,
    token: &str,
    window: &str,
    name: &str,
    arguments: Value,
) -> Value {
    f.rpc(
        token,
        Some(window),
        "tools/call",
        json!({"name":name,"arguments":arguments}),
    )
    .await
    .1
}

fn config(f: &Fixture, host_id: &str) -> Value {
    json!({"action":"configure_native_host_auto_enrollment","host_id":host_id,
        "owner_user":f.user.id,"project":PROJECT,
        "policy":{"max_subagents":2,"max_cli_calls":3}})
}

#[tokio::test]
async fn named_workspaces_native_discovery_selection_and_folder_authority() {
    let f = Fixture::new();
    let host =
        f.db.register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
            .unwrap();
    let a = format!("wc_workspace_{}", "a".repeat(32));
    let b = format!("wc_workspace_{}", "b".repeat(32));
    let projects = json!([
        {"id":a,"name":"EAS","enabled":true,"folders":[{"project":"agent:runner:code","name":"Code","path":"D:/code"},{"project":"agent:runner:docs","name":"Docs","path":"E:/docs"}]},
        {"id":b,"name":"Web","enabled":true,"folders":[{"project":"agent:runner:web","name":"Web","path":"D:/web"}]}
    ]);
    let control = json!({"action":"native_workspaces_save","host_id":host.host_id,"owner_user":f.user.id,"expected_revision":0,"projects":projects});
    assert_ne!(f.control(&f.oauth, control.clone()).await.0, StatusCode::OK);
    assert_eq!(f.control(CONTROL, control).await.0, StatusCode::OK);
    let token = host.into_secret();
    let status = success(native_call(&f, &token, "fresh", "runtime_status", json!({})).await);
    assert_eq!(status["agent_state"], "project_selection_required");
    let list = success(native_call(&f, &token, "fresh", "agent_projects", json!({})).await);
    assert_eq!(list["projects"].as_array().unwrap().len(), 2);
    assert_eq!(list["state_changed"], false);
    denied(
        &native_call(
            &f,
            &token,
            "fresh",
            "agent_select_project",
            json!({"project":a,"owner_user":"forged"}),
        )
        .await,
    );
    let selected = success(
        native_call(
            &f,
            &token,
            "fresh",
            "agent_select_project",
            json!({"project":a}),
        )
        .await,
    );
    assert_eq!(selected["agent_state"], "active");
    let assignment = success(native_call(&f, &token, "fresh", "agent_assignment", json!({})).await);
    assert_eq!(
        assignment["named_project"]["folders"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    denied(
        &native_call(
            &f,
            &token,
            "fresh",
            "agent_select_project",
            json!({"project":b}),
        )
        .await,
    );
    denied(
        &native_call(
            &f,
            &token,
            "fresh",
            "agent_project_files",
            json!({"action":"read","folder":"agent:runner:web","path":"secret.txt"}),
        )
        .await,
    );
    denied(&native_call(&f,&token,"fresh","agent_project_files",json!({"action":"write","folder":"agent:runner:code","path":"x.txt","content":"x","owner_user":"forged"})).await);
}

#[tokio::test]
async fn named_workspace_explicit_team_policy_reaches_new_native_chat() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let host =
        f.db.register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
            .unwrap();
    let project = format!("wc_workspace_{}", "a".repeat(32));
    let code = "agent:runner:code";
    let docs = "agent:runner:docs";
    let control = json!({
        "action":"native_workspaces_save","host_id":host.host_id,"owner_user":f.user.id,
        "expected_revision":0,"projects":[{
            "id":project,"name":"EAS","enabled":true,"team_project":docs,
            "folders":[
                {"project":code,"name":"Code","path":"D:/code"},
                {"project":docs,"name":"Docs","path":"E:/docs"}
            ]
        }]
    });
    assert_eq!(f.control(CONTROL, control).await.0, StatusCode::OK);
    let token = host.into_secret();
    let old = success(
        native_call(
            &f,
            &token,
            "before-save",
            "agent_select_project",
            json!({"project":project}),
        )
        .await,
    );
    assert_eq!(old["assignment"]["policy"]["max_subagents"], 0);
    assert_eq!(old["assignment"]["policy"]["max_cli_calls"], 0);

    // Seed operator policies under the same managed-user principal as real saves.
    let mut hash = Sha256::new();
    hash.update(b"webcodex.communication.principal.v1\0managed-user\0");
    hash.update(f.user.id.as_bytes());
    let principal = webcodex_store::CommunicationPrincipal {
        kind: "managed-user".into(),
        digest: format!("wc_commprincipal_{:x}", hash.finalize()),
    };
    f.db.save_team_policy(
        &principal,
        code,
        0,
        webcodex_store::TeamPolicyInput::default(),
    )
    .unwrap();
    let mut settings = webcodex_store::TeamPolicyInput::default();
    settings.mode = webcodex_store::TeamPolicyMode::Custom;
    settings.subagents = webcodex_store::TeamCountRange { min: 1, max: 3 };
    settings.cli_per_agent = webcodex_store::TeamCountRange { min: 1, max: 2 };
    settings.providers[0].model = Some("selected-folder-model".into());
    let saved =
        f.db.save_team_policy(&principal, docs, 0, settings)
            .unwrap();

    let selected = success(
        native_call(
            &f,
            &token,
            "after-save",
            "agent_select_project",
            json!({"project":project}),
        )
        .await,
    );
    assert_ne!(selected["agent"]["agent_id"], old["agent"]["agent_id"]);
    assert_eq!(selected["assignment"]["policy"]["max_subagents"], 3);
    assert_eq!(selected["assignment"]["policy"]["max_cli_calls"], 8);
    let assignment =
        success(native_call(&f, &token, "after-save", "agent_assignment", json!({})).await);
    assert_eq!(assignment["named_project"]["team_project"], docs);
    assert_eq!(
        assignment["assignment"]["policy"],
        selected["assignment"]["policy"]
    );
    let observed = success(
        native_call(
            &f,
            &token,
            "after-save",
            "team_policy_get",
            json!({"task_id":assignment["assignment"]["task_id"]}),
        )
        .await,
    );
    assert_eq!(observed["policy"], serde_json::to_value(saved).unwrap());
    assert_eq!(observed["max_subagents"], 3);
    assert_eq!(observed["max_cli_calls"], 8);
    let unchanged =
        success(native_call(&f, &token, "before-save", "runtime_status", json!({})).await);
    assert_eq!(unchanged["assignment"], old["assignment"]);
}

#[tokio::test]
async fn native_auto_enrollment_reports_connection_without_configuration_and_requires_control() {
    let f = Fixture::new();
    let host =
        f.db.register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
            .unwrap();
    let host_id = host.host_id.clone();
    let principal = f.db.native_agent_host_principal(&host_id).unwrap();
    let token = host.into_secret();
    let status = success(
        native_call(
            &f,
            &token,
            "fresh",
            "runtime_status",
            json!({"compact":true}),
        )
        .await,
    );
    assert_eq!(status["connected"], true);
    assert_eq!(status["agent_state"], "enrollment_required");
    assert!(status.get("agent").is_none());
    assert!(f
        .db
        .resolve_agent_host_window(
            &principal,
            &Fixture::source_window("fresh"),
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap()
        .is_none());
    assert_ne!(
        f.control(&f.oauth, config(&f, &host_id)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.control(CONTROL, config(&f, &host_id)).await.0,
        StatusCode::OK
    );
    let status = success(native_call(&f, &token, "fresh", "runtime_status", json!({})).await);
    assert_eq!(status["agent_state"], "active");
    assert_eq!(status["assignment"]["policy"]["max_cli_calls"], 3);
}

#[tokio::test]
async fn native_auto_enrollment_keeps_chats_separate_replays_exactly_and_never_revives_runs() {
    let f = Fixture::new();
    let host =
        f.db.register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
            .unwrap();
    assert_eq!(
        f.control(CONTROL, config(&f, &host.host_id)).await.0,
        StatusCode::OK
    );
    let token = host.into_secret();
    let first = success(native_call(&f, &token, "first", "runtime_status", json!({})).await);
    let repeated = success(
        native_call(
            &f,
            &token,
            "first",
            "call_runtime_tool",
            json!({"tool":"runtime_status","arguments":{"summary_only":true}}),
        )
        .await,
    );
    assert_eq!(first["agent"], repeated["agent"]);
    assert_eq!(first["assignment"], repeated["assignment"]);
    let second = success(native_call(&f, &token, "second", "agent_identity", json!({})).await);
    assert_ne!(first["agent"]["agent_id"], second["agent_id"]);
    success(native_call(&f, &token, "first", "agent_private", json!({"action":"write","path":"memory/own.txt","content":"first-only-private-sentinel"})).await);
    let second_read = native_call(
        &f,
        &token,
        "second",
        "agent_private",
        json!({"action":"read","path":"memory/own.txt"}),
    )
    .await;
    denied(&second_read);
    assert!(!second_read
        .to_string()
        .contains("first-only-private-sentinel"));
    assert!(first.get("runners").is_none() && first.get("projects").is_none());
    f.db.revoke_agent_run(
        &f.user.id,
        PROJECT,
        first["agent"]["agent_id"].as_str().unwrap(),
        first["agent"]["run_id"].as_str().unwrap(),
    )
    .unwrap();
    denied(&native_call(&f, &token, "first", "runtime_status", json!({})).await);
    assert_eq!(
        success(native_call(&f, &token, "second", "agent_identity", json!({})).await)["agent_id"],
        second["agent_id"]
    );
}

#[tokio::test]
async fn native_auto_enrollment_discovery_and_invalid_calls_never_create_an_agent() {
    let f = Fixture::new();
    let host =
        f.db.register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
            .unwrap();
    let principal = f.db.native_agent_host_principal(&host.host_id).unwrap();
    assert_eq!(
        f.control(CONTROL, config(&f, &host.host_id)).await.0,
        StatusCode::OK
    );
    let token = host.into_secret();
    let manifest = success(
        native_call(
            &f,
            &token,
            "discover",
            "call_runtime_tool",
            json!({"tool":"tool_manifest","arguments":{"include_recommended_flows":true}}),
        )
        .await,
    );
    assert!(manifest["contracts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "runtime_status"));
    assert_eq!(manifest["state_changed"], false);
    assert!(!manifest.to_string().contains(&f.user.id));
    for (name, args) in [
        ("runtime_status", json!({"compact":"true"})),
        ("runtime_status", json!({"client_id":"another-runner"})),
        ("agent_identity", json!({"agent_id":"another-agent"})),
        ("goal_mode_start", json!({})),
        (
            "call_runtime_tool",
            json!({"tool":"run_shell","arguments":{}}),
        ),
    ] {
        denied(&native_call(&f, &token, "discover", name, args).await);
    }
    assert!(f
        .db
        .resolve_agent_host_window(
            &principal,
            &Fixture::source_window("discover"),
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap()
        .is_none());
    denied(
        &f.rpc(
            &f.oauth,
            Some("oauth-fresh"),
            "tools/call",
            json!({"name":"agent_identity","arguments":{}}),
        )
        .await
        .1,
    );
}
