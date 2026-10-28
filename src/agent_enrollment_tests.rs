//! First-use enrollment through production HTTP authentication and MCP.
//! Only synthetic credentials, users, task inputs and temporary DBs are used.
use crate::test_support::{seed_oauth_client_named, seed_user, test_config_oauth2};
use crate::tool_runtime::ToolRuntime;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};
use serde_json::{json, Value};
use std::sync::Arc;

const CONTROL: &str = "synthetic-enrollment-bootstrap";
const PROJECT: &str = "enrollment-project";

mod auto_enrollment;
mod collaboration;
mod host_dispatch;

struct Fixture {
    service: Service,
    db: Arc<crate::Database>,
    temp: tempfile::TempDir,
    user: crate::models::UserRecord,
    client: crate::models::OAuthClientRecord,
    oauth: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::Database::open(&temp.path().join("state.db")).unwrap());
        let user = seed_user(&db, "enrollment-owner");
        let (client, _) = seed_oauth_client_named(&db, &user, "Synthetic enrollment host");
        let oauth = crate::auth::generate_oauth_access_token();
        let now = chrono::Utc::now().timestamp();
        db.insert_oauth_access_token(&crate::models::OAuthAccessTokenRecord {
            id: uuid::Uuid::new_v4().to_string(),
            token_hash: crate::auth::hash_token(&oauth),
            client_id: client.client_id.clone(),
            subject_kind: "managed_user".into(),
            subject_id: user.id.clone(),
            user_id: Some(user.id.clone()),
            scopes: "runtime:read project:read project:write".into(),
            resource: None,
            shared_key_hash: None,
            created_at: now,
            expires_at: now + 3600,
            revoked_at: None,
            last_used_at: None,
        })
        .unwrap();
        let service = Self::service(&db, temp.path());
        Self {
            service,
            db,
            temp,
            user,
            client,
            oauth,
        }
    }

    fn service(db: &Arc<crate::Database>, directory: &std::path::Path) -> Service {
        let mut config = test_config_oauth2(Some(CONTROL));
        Arc::get_mut(&mut config).unwrap().data_dir = directory.into();
        let runtime =
            Arc::new(ToolRuntime::new_for_tests().with_communication_database(db.clone()));
        Service::new(
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
                )
                .push(
                    Router::with_path("api")
                        .hoop(crate::AuthMiddleware)
                        .push(
                            Router::with_path("admin/agent-isolation")
                                .post(crate::agent_isolation::control),
                        )
                        .push(
                            Router::with_path("tools/call").post(crate::runtime_http::tools_call),
                        ),
                ),
        )
    }

    fn reservation(&self, task: &str, key: &str) -> Value {
        self.reservation_for_project(task, key, PROJECT)
    }

    fn reservation_for_project(&self, task: &str, key: &str, project: &str) -> Value {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"webcodex.communication.principal.v1\0managed-user\0");
        hash.update(self.user.id.as_bytes());
        let principal = webcodex_store::CommunicationPrincipal {
            kind: "managed-user".into(),
            digest: format!("wc_commprincipal_{:x}", hash.finalize()),
        };
        let task_id = self
            .db
            .create_agent_task(
                &principal,
                webcodex_store::NewAgentTask {
                    title: task.into(),
                    instruction: format!("Public task {task}"),
                    assignee_agent_id: None,
                    source_conversation_id: None,
                    source_message_id: None,
                    referenced_project_id: Some(project.into()),
                    idempotency_key: task.into(),
                },
            )
            .unwrap()
            .task
            .summary
            .task_id;
        json!({"action":"agent_enrollment_reserve","owner_user":self.user.id,
            "project":project,"purpose":"manager","oauth_client_id":self.client.client_id,
            "oauth_subject_kind":"managed_user","oauth_subject_id":self.user.id,
            "host_metadata_exclusive":true,"task_id":task_id,"task_directory":format!("workspace/{task}"),
            "brief":"Only the explicitly assigned task is authorized.",
            "inputs":[{"name":"brief.txt","content":"Published task input"}],"room_ids":[],
            "policy":{"max_subagents":2,"max_cli_calls":3},
            "expires_at_unix_ms":chrono::Utc::now().timestamp_millis()+300_000,
            "idempotency_key":key})
    }

    async fn control(&self, token: &str, body: Value) -> (StatusCode, Value) {
        let mut response = TestClient::post("http://localhost/api/admin/agent-isolation")
            .bearer_auth(token)
            .json(&body)
            .send(&self.service)
            .await;
        let status = response.status_code.unwrap_or(StatusCode::OK);
        if status == StatusCode::OK {
            assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        }
        (status, response.take_json().await.unwrap())
    }

    async fn reserve(&self, body: Value) -> Value {
        let (status, grant) = self.control(CONTROL, body).await;
        assert_eq!(status, StatusCode::OK);
        assert!(grant.get("run_token").is_none());
        assert!(grant["claim_grant"].as_str().unwrap().starts_with("wc_en_"));
        grant
    }

    async fn rpc(
        &self,
        token: &str,
        window: Option<&str>,
        method: &str,
        mut params: Value,
    ) -> (StatusCode, Value) {
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{}});
        if let Some(window) = window {
            params["_meta"]["openai/session"] = json!(window);
        }
        let mut request = TestClient::post("http://localhost/mcp")
            .bearer_auth(token)
            .add_header("mcp-protocol-version", "2026-07-28", true)
            .add_header("mcp-method", method, true);
        if let Some(name) = params["name"].as_str() {
            request = request.add_header("mcp-name", name, true);
        }
        let mut response = request
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send(&self.service)
            .await;
        (
            response.status_code.unwrap_or(StatusCode::OK),
            response.take_json().await.unwrap(),
        )
    }

    async fn call(&self, window: &str, name: &str, arguments: Value) -> Value {
        let (status, response) = self
            .rpc(
                &self.oauth,
                Some(window),
                "tools/call",
                json!({"name":name,"arguments":arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        response
    }

    async fn attach(&self, window: &str, grant: &Value) -> Value {
        success(
            self.call(
                window,
                "agent_attach",
                json!({"enrollment_id":grant["enrollment_id"],"claim_grant":grant["claim_grant"]}),
            )
            .await,
        )
    }

    fn source_window(window: &str) -> String {
        crate::client_window::ClientWindow::from_opaque("openai-session", window)
            .unwrap()
            .key()
            .to_owned()
    }
}

fn success(body: Value) -> Value {
    assert_eq!(
        body["result"]["structuredContent"]["success"], true,
        "operation should succeed; error code: {}",
        body["result"]["structuredContent"]["error"]
    );
    body["result"]["structuredContent"]["output"].clone()
}
fn denied(body: &Value) {
    assert!(body.get("error").is_some() || body["result"]["isError"] == true);
}

#[tokio::test]
async fn enrollment_http_cached_gateway_attaches_native_window_without_bypassing_isolation() {
    let f = Fixture::new();
    let native =
        f.db.register_native_agent_host(&format!("tunnel_{}", "c".repeat(32)), &"d".repeat(64))
            .unwrap();
    let mut request = f.reservation("cached-window-task", "cached-window-start");
    request["host_id"] = json!(native.host_id);
    for key in [
        "oauth_client_id",
        "oauth_subject_kind",
        "oauth_subject_id",
        "host_metadata_exclusive",
    ] {
        request.as_object_mut().unwrap().remove(key);
    }
    let grant = f.reserve(request).await;
    let token = native.into_secret();
    let identity =
        json!({"name":"call_runtime_tool","arguments":{"tool":"agent_identity","arguments":{}}});
    let attach = json!({"name":"call_runtime_tool","arguments":{"tool":"agent_attach","arguments":{
        "enrollment_id":grant["enrollment_id"],"claim_grant":grant["claim_grant"]
    }}});
    denied(
        &f.rpc(
            &token,
            Some("cached-window"),
            "tools/call",
            identity.clone(),
        )
        .await
        .1,
    );
    let mut spoof = attach.clone();
    spoof["arguments"]["_meta"] = json!({"openai/session":"cached-window"});
    denied(
        &f.rpc(&token, Some("other-window"), "tools/call", spoof)
            .await
            .1,
    );
    denied(&f.rpc(&token, None, "tools/call", attach.clone()).await.1);
    let attached = success(
        f.rpc(&token, Some("cached-window"), "tools/call", attach.clone())
            .await
            .1,
    );
    assert_eq!(attached["agent_id"], grant["agent_id"]);
    assert_eq!(
        success(
            f.rpc(
                &token,
                Some("cached-window"),
                "tools/call",
                identity.clone()
            )
            .await
            .1
        )["run_id"],
        grant["run_id"]
    );
    denied(
        &f.rpc(&token, Some("other-window"), "tools/call", attach)
            .await
            .1,
    );
    for tool in ["run_shell", "call_runtime_tool"] {
        denied(
            &f.rpc(
                &token,
                Some("cached-window"),
                "tools/call",
                json!({
                    "name":"call_runtime_tool","arguments":{"tool":tool,"arguments":{}}
                }),
            )
            .await
            .1,
        );
    }
    f.db.revoke_agent_run(
        &f.user.id,
        PROJECT,
        grant["agent_id"].as_str().unwrap(),
        grant["run_id"].as_str().unwrap(),
    )
    .unwrap();
    denied(
        &f.rpc(&token, Some("cached-window"), "tools/call", identity)
            .await
            .1,
    );
}

#[tokio::test]
async fn native_contract_discovery_unblocks_claim_prepare_without_granting_business_access() {
    let f = Fixture::new();
    let host =
        f.db.register_native_agent_host(&format!("tunnel_{}", "c".repeat(32)), &"d".repeat(64))
            .unwrap();
    let principal = f.db.native_agent_host_principal(&host.host_id).unwrap();
    let token = host.into_secret();
    let manifest = json!({"name":"tool_manifest","arguments":{"tool_name":"goal_handoff"}});
    let (_, list) = f.rpc(&token, None, "tools/list", json!({})).await;
    let spec = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|spec| spec["name"] == "goal_handoff")
        .unwrap();
    let contract = success(
        f.rpc(
            &token,
            Some("contract-candidate"),
            "tools/call",
            manifest.clone(),
        )
        .await
        .1,
    );
    assert_eq!(contract["contract"]["input_schema"], spec["inputSchema"]);
    assert!(contract
        .pointer("/contract/input_schema/properties/launch_claim")
        .is_some());
    assert!(contract
        .pointer("/contract/input_schema/properties/context_epoch")
        .is_none());
    assert_eq!(contract["execution_granted"], false);
    assert_eq!(contract["state_changed"], false);
    let via_gateway = json!({"name":"call_runtime_tool","arguments":{
        "tool":"tool_manifest","arguments":{"tool_name":"goal_read"}}});
    assert_eq!(
        success(
            f.rpc(
                &token,
                Some("contract-candidate"),
                "tools/call",
                via_gateway.clone()
            )
            .await
            .1
        )["contract"]["name"],
        "goal_read"
    );
    denied(
        &f.rpc(
            &f.oauth,
            Some("contract-candidate"),
            "tools/call",
            manifest.clone(),
        )
        .await
        .1,
    );
    for call in [
        json!({"name":"call_runtime_tool","arguments":{"tool":"tool_manifest","arguments":{"tool_name":"call_runtime_tool"}}}),
        json!({"name":"call_runtime_tool","arguments":{"tool":"call_runtime_tool","arguments":{}}}),
        json!({"name":"agent_identity","arguments":{}}),
        json!({"name":"goal_read","arguments":{"goal_id":"unrelated-goal"}}),
        json!({"name":"agent_private","arguments":{"action":"read","path":"memory/own.txt"}}),
    ] {
        denied(
            &f.rpc(&token, Some("contract-candidate"), "tools/call", call)
                .await
                .1,
        );
    }
    assert!(f
        .db
        .resolve_agent_host_window(
            &principal,
            &Fixture::source_window("contract-candidate"),
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap()
        .is_none());

    // A real synthetic source publishes its private handoff package through
    // the existing adapter; metadata discovery never creates this identity.
    let grant =
        f.db.create_isolated_agent(&f.user.id, PROJECT, None)
            .unwrap();
    let source = f.db.authenticate_agent_run(&grant.token).unwrap();
    f.db.register_agent_host_window(&principal, "contract-source-window", &source)
        .unwrap();
    let start = crate::agent_goal_adapter::call(
        &f.db,
        &source,
        "goal_mode_start",
        json!({
        "project":PROJECT,"task_directory":"workspace/contract-task","objective":"Continue exact task","idempotency_key":"contract-start"}),
    );
    assert!(start.success);
    let goal_id = start.output["goal"]["goal_id"].as_str().unwrap();
    let published = crate::agent_goal_adapter::call(
        &f.db,
        &source,
        "goal_checkpoint",
        json!({
        "goal_id":goal_id,"expected_revision":start.output["goal"]["revision"],"idempotency_key":"contract-handoff",
        "patch":{"action":"handoff","reason":"Continue this task"}}),
    );
    assert!(published.success);
    let handoff = &published.output["handoff"];
    let (_, begun) = f.control(CONTROL, json!({"action":"handoff_begin_attempt","owner_user":f.user.id,"project":PROJECT,
        "agent_id":source.agent_id(),"goal_id":goal_id,"handoff_id":handoff["handoff_id"],"expected_attempt_id":null})).await;
    let (_, claim) = f
        .control(
            CONTROL,
            json!({"action":"handoff_launch_claim","owner_user":f.user.id,"project":PROJECT,
        "agent_id":source.agent_id(),"goal_id":goal_id,"handoff_id":handoff["handoff_id"],
        "expected_attempt_id":begun["handoff"]["attempt_id"]}),
        )
        .await;
    let prepare = json!({"goal_id":goal_id,"action":"prepare","handoff_id":handoff["handoff_id"],
        "attempt_id":begun["handoff"]["attempt_id"],"checkpoint_sequence":handoff["checkpoint_sequence"]});
    denied(
        &f.rpc(
            &token,
            Some("contract-candidate"),
            "tools/call",
            json!({"name":"goal_handoff","arguments":prepare}),
        )
        .await
        .1,
    );
    let mut prepare = prepare;
    prepare["launch_claim"] = claim["claim_grant"].clone();
    let other =
        f.db.register_native_agent_host(&format!("tunnel_{}", "e".repeat(32)), &"d".repeat(64))
            .unwrap();
    let other_id = other.host_id.clone();
    let other_token = other.into_secret();
    // A different authenticated host can read the same public contract but
    // cannot consume this source principal's one-time claim.
    success(
        f.rpc(
            &other_token,
            Some("other-candidate"),
            "tools/call",
            manifest.clone(),
        )
        .await
        .1,
    );
    denied(
        &f.rpc(
            &other_token,
            Some("other-candidate"),
            "tools/call",
            json!({"name":"goal_handoff","arguments":prepare}),
        )
        .await
        .1,
    );
    f.db.revoke_native_agent_host(&other_id).unwrap();
    denied(
        &f.rpc(
            &other_token,
            Some("other-candidate"),
            "tools/call",
            manifest.clone(),
        )
        .await
        .1,
    );
    let prepared = success(
        f.rpc(
            &token,
            Some("contract-candidate"),
            "tools/call",
            json!({
        "name":"call_runtime_tool","arguments":{"tool":"goal_handoff","arguments":prepare}}),
        )
        .await
        .1,
    );
    assert_eq!(prepared["handoff"]["status"], "prepared");
    assert_eq!(prepared["handoff"]["agent_id"], source.agent_id());
    assert_eq!(prepared["handoff"]["goal_id"], goal_id);
    assert_eq!(prepared["execution_granted"], false);
    assert_eq!(
        success(
            f.rpc(
                &token,
                Some("contract-candidate"),
                "tools/call",
                via_gateway
            )
            .await
            .1
        )["contract"]["name"],
        "goal_read"
    );
    denied(
        &f.rpc(
            &token,
            Some("contract-candidate"),
            "tools/call",
            json!({"name":"goal_read","arguments":{"goal_id":goal_id}}),
        )
        .await
        .1,
    );
    denied(
        &f.rpc(
            &token,
            Some("another-window"),
            "tools/call",
            json!({"name":"goal_handoff","arguments":prepare}),
        )
        .await
        .1,
    );
}

#[tokio::test]
async fn enrollment_http_first_use_has_no_existing_window_and_survives_restart() {
    let mut f = Fixture::new();
    assert_eq!(
        f.rpc(&f.oauth, Some("new-manager"), "tools/list", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let request = f.reservation("root-task", "root-start");
    let grant = f.reserve(request.clone()).await;
    denied(
        &f.call(
            "new-manager",
            "agent_private",
            json!({"action":"read","path":"memory/own.txt"}),
        )
        .await,
    );
    let (_, before) = f
        .rpc(&f.oauth, Some("new-manager"), "tools/list", json!({}))
        .await;
    let first = f.attach("new-manager", &grant).await;
    assert_eq!(first["agent_id"], grant["agent_id"]);
    assert_eq!(first["assignment"]["task_id"], request["task_id"]);
    assert_eq!(
        first["assignment"]["inputs"][0]["content"],
        "Published task input"
    );
    assert_eq!(
        first["assignment"]["inputs"][0]["sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let (_, after) = f
        .rpc(&f.oauth, Some("new-manager"), "tools/list", json!({}))
        .await;
    assert_eq!(before["result"]["tools"], after["result"]["tools"]);
    let (status, repeated) = f.control(CONTROL, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated["enrollment_id"], grant["enrollment_id"]);
    assert_eq!(repeated["replayed"], true);
    assert!(repeated["claim_grant"].is_null());
    f.db = Arc::new(crate::Database::open(&f.temp.path().join("state.db")).unwrap());
    f.service = Fixture::service(&f.db, f.temp.path());
    let replay = f.attach("new-manager", &grant).await;
    assert_eq!(replay["run_id"], first["run_id"]);
    assert_eq!(replay["replayed"], true);
    let assignment = success(f.call("new-manager", "agent_assignment", json!({})).await);
    assert_eq!(assignment["assignment"], first["assignment"]);
    assert!(!assignment
        .to_string()
        .contains(grant["claim_grant"].as_str().unwrap()));
    denied(
        &f.call(
            "second-window",
            "agent_attach",
            json!({"enrollment_id":grant["enrollment_id"],"claim_grant":grant["claim_grant"]}),
        )
        .await,
    );
}

#[tokio::test]
async fn enrollment_http_identity_and_credential_audiences_fail_closed() {
    let f = Fixture::new();
    let request = f.reservation("task-one", "identity-start");
    assert_ne!(f.control(&f.oauth, request.clone()).await.0, StatusCode::OK);
    let mut bad = request.clone();
    bad["host_metadata_exclusive"] = json!(false);
    assert_ne!(f.control(CONTROL, bad).await.0, StatusCode::OK);
    let other = seed_user(&f.db, "unrelated-enrollment-owner");
    let mut bad = request.clone();
    bad["owner_user"] = json!(other.id);
    assert_ne!(f.control(CONTROL, bad).await.0, StatusCode::OK);
    for path in [
        "workspace/con",
        "workspace/NUL.txt",
        "workspace/.SNAPSHOT-hidden",
    ] {
        let mut bad = request.clone();
        bad["task_directory"] = json!(path);
        assert_ne!(f.control(CONTROL, bad).await.0, StatusCode::OK);
    }
    let grant = f.reserve(request).await;
    assert_eq!(
        f.rpc(
            grant["claim_grant"].as_str().unwrap(),
            Some("new"),
            "tools/list",
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, missing_meta) = f.rpc(&f.oauth, None, "tools/call", json!({"name":"agent_attach","arguments":{
        "enrollment_id":grant["enrollment_id"],"claim_grant":grant["claim_grant"],"_meta":{"openai/session":"new"}}})).await;
    denied(&missing_meta);
    denied(&f.call("new", "agent_attach", json!({"enrollment_id":grant["enrollment_id"],"claim_grant":grant["claim_grant"],"purpose":"manager"})).await);
    f.attach("new", &grant).await;
    for name in ["run_process", "run_shell", "coding_agent_start"] {
        denied(&f.call("new", name, json!({})).await);
    }
    let status = success(
        f.call("new", "runtime_status", json!({"compact":true}))
            .await,
    );
    assert!(status.get("agents").is_some(), "{status}");
    let conn = f.db.conn_for_tests();
    let retained: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM action_events WHERE endpoint='/mcp'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, 0);
}

#[tokio::test]
async fn enrollment_http_native_host_uses_registered_principal_and_rejects_mixed_identity() {
    let f = Fixture::new();
    let native =
        f.db.register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
            .unwrap();
    let host_id = native.host_id.clone();
    let native_token = native.into_secret();
    let mut request = f.reservation("native-task", "native-start");
    request["host_id"] = json!(host_id);
    assert_ne!(f.control(CONTROL, request.clone()).await.0, StatusCode::OK);
    for key in [
        "oauth_client_id",
        "oauth_subject_kind",
        "oauth_subject_id",
        "host_metadata_exclusive",
    ] {
        request.as_object_mut().unwrap().remove(key);
    }
    let mut missing = request.clone();
    missing["host_id"] = json!("unregistered-host");
    assert_ne!(f.control(CONTROL, missing).await.0, StatusCode::OK);
    let grant = f.reserve(request).await;
    let (status, attached) = f.rpc(&native_token, Some("native-new-window"), "tools/call", json!({
        "name":"agent_attach", "arguments":{"enrollment_id":grant["enrollment_id"], "claim_grant":grant["claim_grant"]}
    })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(success(attached)["agent_id"], grant["agent_id"]);
    let query = json!({"action":"agent_enrollment_status","owner_user":f.user.id,"project":PROJECT,"enrollment_id":grant["enrollment_id"]});
    assert_ne!(
        f.control(&native_token, query.clone()).await.0,
        StatusCode::OK
    );
    assert_ne!(f.control(&f.oauth, query.clone()).await.0, StatusCode::OK);
    let (status, bound) = f.control(CONTROL, query).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bound["generation"], 1);
    assert_eq!(bound["run_id"], grant["run_id"]);
    assert_eq!(bound["state"], "claimed");
    assert_eq!(bound["window_enabled"], true);
    assert!(bound["window_binding_ref"]
        .as_str()
        .unwrap()
        .starts_with("wc_host_binding_"));
    assert!(!bound.to_string().contains(&native_token));
    assert!(!bound.to_string().contains("native-new-window"));
    assert!(!bound
        .to_string()
        .contains(grant["claim_grant"].as_str().unwrap()));
    assert_ne!(
        f.control(&native_token, json!({"action":"revoke_agent"}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn enrollment_http_member_has_new_identity_own_memory_and_authorized_room_only() {
    let f = Fixture::new();
    let manager = f.reserve(f.reservation("root-task", "manager")).await;
    f.attach("manager", &manager).await;
    success(
        f.call(
            "manager",
            "agent_private",
            json!({"action":"write","path":"memory/own.txt","content":"manager-private"}),
        )
        .await,
    );
    let (status, room) = f
        .control(
            CONTROL,
            json!({"action":"create_meeting","project":PROJECT,
        "participants":[manager["agent_id"]],"brief":"Published project brief"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut request = f.reservation("child-task", "member");
    request["purpose"] = json!("member");
    request["source_agent_id"] = manager["agent_id"].clone();
    request["source_run_id"] = manager["run_id"].clone();
    request["policy"] = json!({"max_subagents":0,"max_cli_calls":0});
    request["room_ids"] = json!([room["room_id"]]);
    let mut ambiguous = request.clone();
    ambiguous["source_window_key"] = json!(Fixture::source_window("manager"));
    assert_ne!(f.control(CONTROL, ambiguous).await.0, StatusCode::OK);
    let mut stale = request.clone();
    stale["source_run_id"] = json!("unknown-source-run");
    assert_ne!(f.control(CONTROL, stale).await.0, StatusCode::OK);
    let member = f.reserve(request).await;
    assert_ne!(member["agent_id"], manager["agent_id"]);
    denied(
        &f.call(
            "member",
            "agent_meeting",
            json!({"action":"read","room_id":room["room_id"]}),
        )
        .await,
    );
    f.attach("member", &member).await;
    denied(
        &f.call(
            "member",
            "agent_private",
            json!({"action":"read","path":"memory/own.txt"}),
        )
        .await,
    );
    success(
        f.call(
            "member",
            "agent_private",
            json!({"action":"write","path":"memory/own.txt","content":"member-private"}),
        )
        .await,
    );
    assert_eq!(
        success(
            f.call(
                "manager",
                "agent_private",
                json!({"action":"read","path":"memory/own.txt"})
            )
            .await
        )["content"],
        "manager-private"
    );
    success(
        f.call(
            "member",
            "agent_meeting",
            json!({"action":"read","room_id":room["room_id"]}),
        )
        .await,
    );
    denied(
        &f.call(
            "member",
            "agent_private",
            json!({"action":"write","path":"workspace/other-task/result.txt","content":"denied"}),
        )
        .await,
    );
    denied(&f.call("member", "agent_private", json!({"action":"write","path":"workspace/child-task/result.txt","content":"not-yet-claimed"})).await);
    denied(
        &f.call(
            "member",
            "agent_assignment",
            json!({"agent_id":manager["agent_id"]}),
        )
        .await,
    );
}

#[tokio::test]
async fn enrollment_http_task_has_one_agent_owner_and_revocation_blocks_replay() {
    let f = Fixture::new();
    let first = f.reserve(f.reservation("same-task", "first")).await;
    let second = f.reserve(f.reservation("same-task", "second")).await;
    f.attach("first", &first).await;
    denied(
        &f.call(
            "second",
            "agent_attach",
            json!({"enrollment_id":second["enrollment_id"],"claim_grant":second["claim_grant"]}),
        )
        .await,
    );
    let (status, _) = f
        .control(
            CONTROL,
            json!({"action":"agent_enrollment_revoke","owner_user":f.user.id,
        "project":PROJECT,"enrollment_id":first["enrollment_id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    f.attach("second", &second).await;
    denied(
        &f.call(
            "first",
            "agent_private",
            json!({"action":"write","path":"memory/late.txt","content":"revoked"}),
        )
        .await,
    );
    denied(
        &f.call(
            "first",
            "agent_attach",
            json!({"enrollment_id":first["enrollment_id"],"claim_grant":first["claim_grant"]}),
        )
        .await,
    );
}

fn enrollment_lifecycle_plan() -> Value {
    let criterion = json!({"kind":"review","id":"sources","description":"The work preserves its explicitly supplied sources"});
    json!({"objective":"Continue the enrolled task","deliverables":["workspace/lifecycle-task/result.txt"],
        "final_criteria":[criterion],
        "nodes":[{"id":"work","title":"Prepare task result","work":"Use the authorized input and retain the task result",
            "outputs":["workspace/lifecycle-task/result.txt"],"criterion_ids":["sources"],"acceptance":[criterion],
            "failure_action":"Keep the checkpoint and explain missing evidence"}],
        "state_management":{"facts":"Preserve supplied sources","decisions":"Explain changes","issues":"Keep blockers",
            "artifacts":"Keep task-local references","checkpoint_triggers":["Before changing host windows"],
            "resume_instructions":"Read this exact Goal and node before claiming the next attempt"}})
}

#[tokio::test]
async fn enrollment_http_goal_to_native_handoff_preserves_assignment_memory_and_meeting() {
    let mut f = Fixture::new();
    // A synthetic existing room participant permits creating an explicitly
    // authorized room before the new logical Agent's first enrollment.
    let steward =
        f.db.create_isolated_agent(&f.user.id, PROJECT, None)
            .unwrap();
    let (status, room) = f.control(CONTROL, json!({"action":"create_meeting","project":PROJECT,
        "participants":[steward.agent.agent_id],"brief":"Only explicitly published task progress is shared"})).await;
    assert_eq!(status, StatusCode::OK);
    let room_id = room["room_id"].as_str().unwrap().to_owned();
    let mut request = f.reservation("lifecycle-task", "lifecycle-manager");
    request["room_ids"] = json!([room_id]);
    let grant = f.reserve(request).await;
    let first = f.attach("enrolled-source", &grant).await;
    let assignment = first["assignment"].clone();
    let agent_id = first["agent_id"].clone();
    let old_run = first["run_id"].clone();
    success(
        f.call(
            "enrolled-source",
            "agent_private",
            json!({"action":"write","path":"memory/own.txt",
        "content":"Preserved same-Agent memory"}),
        )
        .await,
    );
    success(f.call("enrolled-source", "agent_meeting", json!({"action":"publish","room_id":room_id,
        "kind":"message","content":"Authorized initial progress","idempotency_key":"room-before-handoff"})).await);

    let artifact = "workspace/lifecycle-task/result.txt";
    denied(
        &f.call(
            "enrolled-source",
            "agent_private",
            json!({"action":"write","path":artifact,"content":"before-goal"}),
        )
        .await,
    );
    // Omitting task_directory must use the immutable assignment directory.
    let started = success(
        f.call(
            "enrolled-source",
            "goal_mode_start",
            json!({"project":PROJECT,
        "objective":"Continue the enrolled task","idempotency_key":"goal"}),
        )
        .await,
    );
    let goal_id = started["goal"]["goal_id"].as_str().unwrap().to_owned();
    let restored = success(
        f.call(
            "enrolled-source",
            "goal_bootstrap",
            json!({"activate":true}),
        )
        .await,
    );
    assert_eq!(restored["goal"]["goal_id"], goal_id);
    let identity = success(f.call("enrolled-source", "agent_identity", json!({})).await);
    assert_eq!(identity["assignment_goal_id"], goal_id);
    let compatible=success(f.call("enrolled-source","bind_chat_task_goal",json!({"request_id":"old-tracking-label","project":"display-project","goal_id":goal_id})).await);
    assert_eq!(compatible["goal_id"], goal_id);
    assert_eq!(compatible["state_changed"], false);
    assert_eq!(compatible["execution_permission_granted"], false);
    denied(
        &f.call(
            "enrolled-source",
            "bind_chat_task_goal",
            json!({"goal_id":"other-goal"}),
        )
        .await,
    );
    assert_eq!(
        success(
            f.call("enrolled-source", "bind_chat_task_goal", json!({}))
                .await
        )["goal_id"],
        goal_id
    );
    // Initial run scope is intentionally distinct from Task/Goal association.
    assert!(identity["task_goal_id"].is_null());
    assert_eq!(
        started["goal"]["task_directory"],
        "workspace/lifecycle-task"
    );
    denied(
        &f.call(
            "enrolled-source",
            "agent_private",
            json!({"action":"write","path":artifact,"content":"before-confirmation"}),
        )
        .await,
    );
    let published = success(f.call("enrolled-source", "goal_plan_save", json!({"goal_id":goal_id,
        "expected_revision":started["goal"]["revision"],"idempotency_key":"plan","plan":enrollment_lifecycle_plan()})).await);
    let confirmed = success(f.call("enrolled-source", "goal_confirm_plan", json!({"goal_id":goal_id,
        "expected_revision":published["goal"]["revision"],"plan_version":1,"plan_sha256":published["plan"]["hash"],
        "confirmation":"I read and approve this exact saved plan","idempotency_key":"confirm"})).await);
    denied(
        &f.call(
            "enrolled-source",
            "agent_private",
            json!({"action":"write","path":artifact,"content":"before-claim"}),
        )
        .await,
    );
    success(
        f.call(
            "enrolled-source",
            "goal_read",
            json!({"goal_id":goal_id,"view":"node","node_id":"work"}),
        )
        .await,
    );
    let claimed = success(f.call("enrolled-source", "goal_claim_node", json!({"goal_id":goal_id,
        "expected_revision":confirmed["goal"]["revision"],"plan_version":1,"node_id":"work","idempotency_key":"claim-source"})).await);
    let saved = success(
        f.call(
            "enrolled-source",
            "agent_private",
            json!({"action":"write","path":artifact,"content":"Original saved task result"}),
        )
        .await,
    );
    for filename in ["README.md", ".gitignore"] {
        success(
            f.call(
                "enrolled-source",
                "agent_private",
                json!({
            "action":"write","path":format!("workspace/lifecycle-task/{filename}"),
            "content":"Authorized source filename"}),
            )
            .await,
        );
    }
    denied(&f.call("enrolled-source", "agent_private", json!({"action":"write","path":"workspace/another-task/result.txt","content":"outside-assignment"})).await);

    let checkpoint = success(
        f.call(
            "enrolled-source",
            "goal_checkpoint",
            json!({"goal_id":goal_id,
        "expected_revision":claimed["goal"]["revision"],"idempotency_key":"handoff-checkpoint",
        "patch":{"action":"handoff","reason":"Continue the same task in a fresh trusted window",
            "state":{"facts":{"progress":"Original result saved"},"artifact_refs":[artifact]}}}),
        )
        .await,
    );
    assert_eq!(checkpoint["handoff"]["status"], "waiting");
    denied(
        &f.call(
            "enrolled-source",
            "agent_private",
            json!({"action":"write","path":artifact,
        "content":"late-old-source","expected_sha256":saved["sha256"]}),
        )
        .await,
    );

    let (status, reserved) = f.control(CONTROL, json!({"action":"handoff_begin_attempt","owner_user":f.user.id,
        "project":PROJECT,"agent_id":agent_id,"goal_id":goal_id,"handoff_id":checkpoint["handoff"]["handoff_id"]})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, launch) = f.control(CONTROL, json!({"action":"handoff_launch_claim","owner_user":f.user.id,
        "project":PROJECT,"agent_id":agent_id,"goal_id":goal_id,"handoff_id":reserved["handoff"]["handoff_id"],
        "expected_attempt_id":reserved["handoff"]["attempt_id"]})).await;
    assert_eq!(status, StatusCode::OK);
    let cap = launch["claim_grant"].as_str().unwrap();
    assert!(cap.starts_with("wc_hc_"));
    assert!(!cap.starts_with("wc_en_"));
    let mut prepare = json!({"goal_id":goal_id,"action":"prepare","handoff_id":reserved["handoff"]["handoff_id"],
        "attempt_id":reserved["handoff"]["attempt_id"],"checkpoint_sequence":reserved["handoff"]["checkpoint_sequence"],
        "launch_claim":cap});
    denied(
        &f.call("enrolled-successor", "agent_assignment", json!({}))
            .await,
    );
    let prepared = success(
        f.call("enrolled-successor", "goal_handoff", prepare.clone())
            .await,
    );
    assert_eq!(prepared["execution_granted"], false);
    assert!(!prepared.to_string().contains(cap));
    denied(
        &f.call(
            "enrolled-successor",
            "agent_private",
            json!({"action":"read","path":"memory/own.txt"}),
        )
        .await,
    );
    prepare["action"] = json!("commit");
    prepare["package_sha256"] = prepared["package_sha256"].clone();
    prepare.as_object_mut().unwrap().remove("launch_claim");
    let committed = success(f.call("enrolled-successor", "goal_handoff", prepare).await);
    assert_eq!(committed["execution_granted"], true);
    assert_ne!(committed["handoff"]["successor_agent_run_id"], old_run);

    let identity = success(
        f.call("enrolled-successor", "agent_identity", json!({}))
            .await,
    );
    assert_eq!(identity["agent_id"], agent_id);
    assert_eq!(
        success(
            f.call("enrolled-successor", "agent_assignment", json!({}))
                .await
        )["assignment"],
        assignment
    );
    assert_eq!(
        success(
            f.call(
                "enrolled-successor",
                "agent_private",
                json!({"action":"read","path":"memory/own.txt"})
            )
            .await
        )["content"],
        "Preserved same-Agent memory"
    );
    success(
        f.call(
            "enrolled-successor",
            "agent_meeting",
            json!({"action":"read","room_id":room_id}),
        )
        .await,
    );
    success(f.call("enrolled-successor", "agent_meeting", json!({"action":"publish","room_id":room_id,"kind":"message",
        "content":"Successor continues the same authorized task","idempotency_key":"room-after-handoff"})).await);
    denied(
        &f.call("enrolled-source", "agent_assignment", json!({}))
            .await,
    );
    denied(
        &f.call(
            "enrolled-source",
            "agent_private",
            json!({"action":"write","path":"memory/late.txt","content":"retired"}),
        )
        .await,
    );
    denied(
        &f.call(
            "enrolled-successor",
            "agent_private",
            json!({"action":"write","path":artifact,
        "content":"before-successor-claim","expected_sha256":saved["sha256"]}),
        )
        .await,
    );
    let node = success(
        f.call(
            "enrolled-successor",
            "goal_read",
            json!({"goal_id":goal_id,"view":"node","node_id":"work"}),
        )
        .await,
    );
    success(f.call("enrolled-successor", "goal_claim_node", json!({"goal_id":goal_id,
        "expected_revision":node["context_read"]["revision"],"plan_version":1,"node_id":"work","idempotency_key":"claim-successor"})).await);
    success(
        f.call(
            "enrolled-successor",
            "agent_private",
            json!({"action":"write","path":artifact,
        "content":"Continued without identity expansion","expected_sha256":saved["sha256"]}),
        )
        .await,
    );

    f.db = Arc::new(crate::Database::open(&f.temp.path().join("state.db")).unwrap());
    f.service = Fixture::service(&f.db, f.temp.path());
    assert_eq!(
        success(
            f.call("enrolled-successor", "agent_assignment", json!({}))
                .await
        )["assignment"],
        assignment
    );
    assert_eq!(
        success(
            f.call(
                "enrolled-successor",
                "agent_private",
                json!({"action":"read","path":artifact})
            )
            .await
        )["content"],
        "Continued without identity expansion"
    );
    success(
        f.call(
            "enrolled-successor",
            "agent_meeting",
            json!({"action":"read","room_id":room_id}),
        )
        .await,
    );
    denied(&f.call("enrolled-source", "agent_identity", json!({})).await);
}
