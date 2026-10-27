//! Synthetic local credentials only; no real tunnel, API key or login.
use crate::auth::{AuthContext, AuthKind};
use crate::test_support::test_config_oauth2;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};
use serde_json::json;
use std::sync::Arc;

const CONTROL: &str = "synthetic-native-host-control";
const TUNNEL: &str = "tunnel_0123456789abcdef0123456789abcdef";
const DIGEST: &str = "ee9d4a75bc0b42f36f345aa96231e0db1ab00488122f34ebc99d6db055b6603e";

#[handler]
async fn unreachable_handler(res: &mut Response) {
    res.render("UNEXPECTED DISPATCH");
}

#[tokio::test]
async fn native_host_token_has_no_user_admin_runner_or_internal_dispatch_authority() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(crate::Database::open(&dir.path().join("state.db")).unwrap());
    let secret = db
        .register_native_agent_host(TUNNEL, DIGEST)
        .unwrap()
        .into_secret();
    let config = test_config_oauth2(Some(CONTROL));
    let mut auth = crate::auth::authenticate(&config, Some(&db), &secret)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(auth.kind, AuthKind::NativeHost);
    assert!(auth.user_id.is_none() && auth.username.is_none() && auth.role.is_none());
    assert!(auth.agent_run.is_none());
    assert!(!auth.is_admin() && !auth.is_bootstrap() && !auth.has_scope("admin"));
    assert!(
        crate::auth::authenticate_bearer(&config, Some(&db), Some(&secret))
            .await
            .is_none()
    );
    // Defend against an accidental internal projection adding user fields.
    auth.kind = AuthKind::ApiToken;
    auth.is_bootstrap = true;
    auth.scopes = vec!["admin".into()];
    auth.user_id = Some("forged-user".into());
    assert!(!auth.is_admin() && !auth.is_bootstrap() && !auth.is_admin_caller());
    assert!(crate::tool_runtime::runtime_observation_principal(Some(&auth)).is_err());
    let runtime =
        crate::tool_runtime::ToolRuntime::new_for_tests().with_communication_database(db.clone());
    use crate::tool_runtime::kernel::{
        HostFileImportTrust, ToolCallContext, ToolCallRequest, ToolTransport,
    };
    let outcome = runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name: "read_file".into(),
                arguments: json!({"path":"memory/private.txt"}),
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(&auth),
                window: None,
                record_oauth_scope_denials: false,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await;
    assert!(!outcome.success);
}

#[tokio::test]
async fn native_host_http_rejects_non_mcp_and_unproven_metadata_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(crate::Database::open(&dir.path().join("state.db")).unwrap());
    let grant = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let secret = grant.into_secret();
    let config = test_config_oauth2(Some(CONTROL));
    let runtime = Arc::new(
        crate::tool_runtime::ToolRuntime::new_for_tests().with_communication_database(db.clone()),
    );
    let service = Service::new(
        Router::new()
            .hoop(affix_state::inject(config))
            .hoop(affix_state::inject(db.clone()))
            .hoop(affix_state::inject(runtime))
            .hoop(affix_state::inject(
                crate::connector_runtime::ConnectorRuntimeSlot::default(),
            ))
            .hoop(crate::AuthMiddleware)
            .push(Router::with_path("mcp").post(crate::mcp::mcp_post))
            .push(
                Router::with_path("api/admin/agent-isolation")
                    .post(crate::agent_isolation::control),
            )
            .push(Router::with_path("api/tools/call").post(unreachable_handler))
            .push(Router::with_path("api/users/list").post(unreachable_handler)),
    );
    for path in [
        "mcp",
        "api/admin/agent-isolation",
        "api/tools/call",
        "api/users/list",
    ] {
        let mut response = TestClient::post(format!("http://localhost/{path}"))
            .bearer_auth(&secret)
            .json(
                &json!({"action":"native_host_register","name":"agent_attach",
                "_meta":{"openai/session":"forged-session"},"host_metadata_exclusive":true}),
            )
            .send(&service)
            .await;
        assert_ne!(
            response.status_code.unwrap_or(StatusCode::OK),
            StatusCode::OK,
            "{path}"
        );
        assert!(!response.take_string().await.unwrap().contains("UNEXPECTED"));
    }
    let mut discovery = TestClient::post("http://localhost/mcp")
        .bearer_auth(&secret)
        .add_header("mcp-protocol-version", "2026-07-28", true)
        .add_header("mcp-method", "tools/list", true)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}))
        .send(&service).await;
    assert_eq!(
        discovery.status_code.unwrap_or(StatusCode::OK),
        StatusCode::OK
    );
    let catalog: serde_json::Value = discovery.take_json().await.unwrap();
    assert!(catalog.get("result").is_some(), "{catalog}");
    let (_, business) = rpc(
        &service,
        &secret,
        Some("unbound-window"),
        "agent_identity",
        json!({}),
    )
    .await;
    assert!(
        business.get("error").is_some()
            || business["result"]["structuredContent"]["success"] == false,
        "{business}"
    );
    let count: i64 = db
        .conn_for_tests()
        .query_row("SELECT COUNT(*) FROM wc_agent_host_windows", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

async fn rpc(
    service: &Service,
    token: &str,
    window: Option<&str>,
    name: &str,
    args: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut params = json!({"name":name,"arguments":args,"_meta":{
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{}}});
    if let Some(window) = window {
        params["_meta"]["openai/session"] = json!(window);
    }
    let mut response = TestClient::post("http://localhost/mcp")
        .bearer_auth(token)
        .add_header("mcp-protocol-version", "2026-07-28", true)
        .add_header("mcp-method", "tools/call", true)
        .add_header("mcp-name", name, true)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}))
        .send(service)
        .await;
    (
        response.status_code.unwrap_or(StatusCode::OK),
        response.take_json().await.unwrap(),
    )
}

#[tokio::test]
async fn native_host_exact_enrollment_outer_window_rotation_and_revocation() {
    use webcodex_store::{AgentEnrollmentPolicy, AgentEnrollmentPurpose, NewAgentEnrollment};
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(crate::Database::open(&dir.path().join("state.db")).unwrap());
    let grant = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let host_id = grant.host_id.clone();
    let token = grant.into_secret();
    let principal = db.native_agent_host_principal(&host_id).unwrap();
    let reserve = |task: &str| {
        use sha2::{Digest, Sha256};
        let owner = webcodex_store::CommunicationPrincipal {
            kind: "managed-user".into(),
            digest: format!(
                "wc_commprincipal_{:x}",
                Sha256::digest(
                    b"webcodex.communication.principal.v1\0managed-user\0synthetic-owner"
                )
            ),
        };
        let task_id = db
            .create_agent_task(
                &owner,
                webcodex_store::NewAgentTask {
                    title: task.into(),
                    instruction: format!("Public task {task}"),
                    assignee_agent_id: None,
                    source_conversation_id: None,
                    source_message_id: None,
                    referenced_project_id: Some("native-project".into()),
                    idempotency_key: task.into(),
                },
            )
            .unwrap()
            .task
            .summary
            .task_id;
        let mut enrollment = db
            .issue_agent_enrollment(
                NewAgentEnrollment {
                    purpose: AgentEnrollmentPurpose::Manager,
                    owner_user: "synthetic-owner".into(),
                    project: "native-project".into(),
                    principal_key: principal.clone(),
                    task_id,
                    task_directory: format!("workspace/{task}"),
                    brief: format!("Only task {task}"),
                    inputs: vec![],
                    room_ids: vec![],
                    policy: AgentEnrollmentPolicy {
                        max_subagents: 1,
                        max_cli_calls: 0,
                    },
                    expires_at_unix_ms: chrono::Utc::now().timestamp_millis() + 120_000,
                },
                None,
                task,
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap();
        let secret = enrollment.grant.take().unwrap().into_secret();
        (enrollment, secret)
    };
    let (first, first_claim) = reserve("first-task");
    let (second, second_claim) = reserve("second-task");
    let mut config = test_config_oauth2(Some(CONTROL));
    Arc::get_mut(&mut config).unwrap().data_dir = dir.path().into();
    let runtime = Arc::new(
        crate::tool_runtime::ToolRuntime::new_for_tests().with_communication_database(db.clone()),
    );
    let service = Service::new(
        Router::new()
            .hoop(affix_state::inject(config))
            .hoop(affix_state::inject(db.clone()))
            .hoop(affix_state::inject(runtime))
            .hoop(affix_state::inject(
                crate::connector_runtime::ConnectorRuntimeSlot::default(),
            ))
            .push(
                Router::with_path("mcp")
                    .hoop(crate::AuthMiddleware)
                    .post(crate::mcp::mcp_post),
            ),
    );
    let (_, missing) = rpc(
        &service,
        &token,
        None,
        "agent_attach",
        json!({"enrollment_id":first.enrollment_id,"claim_grant":first_claim,
        "_meta":{"openai/session":"window-a"}}),
    )
    .await;
    assert!(missing.get("error").is_some());
    let (status, claimed) = rpc(
        &service,
        &token,
        Some("window-a"),
        "agent_attach",
        json!({"enrollment_id":first.enrollment_id,"claim_grant":first_claim}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        claimed["result"]["structuredContent"]["success"], true,
        "{claimed}"
    );
    let (_, unknown) = rpc(
        &service,
        &token,
        Some("unbound-window"),
        "agent_identity",
        json!({"_meta":{"openai/session":"window-a"}}),
    )
    .await;
    assert!(
        unknown.get("error").is_some()
            || unknown["result"]["structuredContent"]["success"] == false,
        "{unknown}"
    );
    let (_, cross_window) = rpc(
        &service,
        &token,
        Some("window-b"),
        "agent_attach",
        json!({"enrollment_id":first.enrollment_id,"claim_grant":first_claim}),
    )
    .await;
    assert_ne!(cross_window["result"]["structuredContent"]["success"], true);
    let (_, claimed_b) = rpc(
        &service,
        &token,
        Some("window-b"),
        "agent_attach",
        json!({"enrollment_id":second.enrollment_id,"claim_grant":second_claim}),
    )
    .await;
    assert_eq!(
        claimed_b["result"]["structuredContent"]["success"], true,
        "{claimed_b}"
    );
    let (_,write)=rpc(&service,&token,Some("window-a"),"agent_private",json!({"action":"write","path":"memory/note.txt","content":"synthetic private native note"})).await;
    assert_eq!(
        write["result"]["structuredContent"]["success"], true,
        "{write}"
    );
    let current = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    assert_eq!(current.host_id, host_id);
    let current = current.into_secret();
    assert_ne!(
        rpc(
            &service,
            &token,
            Some("window-a"),
            "agent_identity",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, read) = rpc(
        &service,
        &current,
        Some("window-a"),
        "agent_private",
        json!({"action":"read","path":"memory/note.txt"}),
    )
    .await;
    assert_eq!(
        read["result"]["structuredContent"]["success"], true,
        "{read}"
    );
    let (_, other) = rpc(
        &service,
        &current,
        Some("window-b"),
        "agent_private",
        json!({"action":"read","path":"memory/note.txt"}),
    )
    .await;
    assert_ne!(other["result"]["structuredContent"]["success"], true);
    let (pending, pending_claim) = reserve("pending-task");
    db.revoke_native_agent_host(&host_id).unwrap();
    assert!(db
        .claim_agent_enrollment(
            &principal,
            "new-window",
            &pending.enrollment_id,
            &pending_claim,
            chrono::Utc::now().timestamp_millis()
        )
        .is_err());
    assert_ne!(
        rpc(
            &service,
            &current,
            Some("window-a"),
            "agent_identity",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!db.agent_host_principal_authorized(&principal).unwrap());
    let audit = db
        .conn_for_tests()
        .prepare("SELECT summary_json FROM action_events")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(audit
        .iter()
        .all(|s| !s.contains("synthetic private native note") && !s.contains(&token)));
}

#[test]
fn native_control_rejects_remote_assertions_and_unverified_binary() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::Database::open(&dir.path().join("state.db")).unwrap();
    let mut auth = AuthContext::new(AuthKind::Bootstrap);
    auth.is_bootstrap = true;
    let binary = dir.path().join("pretend-tunnel-client.exe");
    std::fs::write(&binary, b"fake version 0.0.12").unwrap();
    let body = json!({"action":"native_host_register","tunnel_id":TUNNEL,"binary_path":binary});
    assert_eq!(
        super::native_agent_host::try_control(&db, &auth, &body, false)
            .unwrap()
            .unwrap_err(),
        "native_host_local_control_required"
    );
    assert_eq!(
        super::native_agent_host::try_control(&db, &auth, &body, true)
            .unwrap()
            .unwrap_err(),
        "native_host_binary_untrusted"
    );
    let injected = json!({"action":"native_host_register","tunnel_id":TUNNEL,"binary_path":binary,"host_metadata_exclusive":true});
    assert_eq!(
        super::native_agent_host::try_control(&db, &auth, &injected, true)
            .unwrap()
            .unwrap_err(),
        "invalid_native_host_control"
    );
}
