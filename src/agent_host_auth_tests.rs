//! Real HTTP middleware + OAuth verifier + host mapping + private broker.
//! All credentials, windows and databases are synthetic; no browser is opened.
use crate::auth::{AuthContext, AuthKind};
use crate::test_support::{seed_oauth_client_named, seed_user, test_config_oauth2};
use crate::tool_runtime::ToolRuntime;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};
use serde_json::{json, Value};
use std::sync::Arc;

const BOOTSTRAP: &str = "synthetic-host-bootstrap";
const SOURCE: &str = "synthetic-source-chat";

struct Fixture {
    service: Service,
    db: Arc<crate::Database>,
    temp: tempfile::TempDir,
    user: crate::models::UserRecord,
    client: crate::models::OAuthClientRecord,
    oauth: String,
    run: webcodex_store::AgentRunGrant,
}

fn oauth_token(
    db: &crate::Database,
    client: &crate::models::OAuthClientRecord,
    user: &crate::models::UserRecord,
    scopes: &str,
) -> String {
    let token = crate::auth::generate_oauth_access_token();
    let now = chrono::Utc::now().timestamp();
    db.insert_oauth_access_token(&crate::models::OAuthAccessTokenRecord {
        id: uuid::Uuid::new_v4().to_string(),
        token_hash: crate::auth::hash_token(&token),
        client_id: client.client_id.clone(),
        subject_kind: "managed_user".into(),
        subject_id: user.id.clone(),
        user_id: Some(user.id.clone()),
        scopes: scopes.into(),
        resource: None,
        shared_key_hash: None,
        created_at: now,
        expires_at: now + 3600,
        revoked_at: None,
        last_used_at: None,
    })
    .unwrap();
    token
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::Database::open(&temp.path().join("state.db")).unwrap());
        let user = seed_user(&db, "host-owner");
        let (client, _) = seed_oauth_client_named(&db, &user, "Synthetic trusted host");
        let oauth = oauth_token(
            &db,
            &client,
            &user,
            "runtime:read project:read project:write",
        );
        let run = db
            .create_isolated_agent(&user.id, "project-a", None)
            .unwrap();
        let service = Self::service(&db, temp.path());
        Self {
            service,
            db,
            temp,
            user,
            client,
            oauth,
            run,
        }
    }
    fn service(db: &Arc<crate::Database>, dir: &std::path::Path) -> Service {
        let mut config = test_config_oauth2(Some(BOOTSTRAP));
        Arc::get_mut(&mut config).unwrap().data_dir = dir.to_path_buf();
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
    async fn control(&self, token: &str, body: Value) -> (StatusCode, Value) {
        let mut response = TestClient::post("http://localhost/api/admin/agent-isolation")
            .bearer_auth(token)
            .json(&body)
            .send(&self.service)
            .await;
        (
            response.status_code.unwrap_or(StatusCode::OK),
            response.take_json().await.unwrap(),
        )
    }
    fn registration(&self, exclusive: bool) -> Value {
        json!({"action":"bind_host_window","run_token":self.run.token,
            "oauth_client_id":self.client.client_id,"oauth_subject_kind":"managed_user",
            "oauth_subject_id":self.user.id,"window_key":crate::client_window::ClientWindow::from_opaque("openai-session",SOURCE).unwrap().key(),
            "host_metadata_exclusive":exclusive})
    }
    async fn register(&self) {
        let (status, body) = self.control(BOOTSTRAP, self.registration(true)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["registered"], true);
        assert!(!body.to_string().contains(&self.run.token));
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
        if let Some(name) = params
            .get("name")
            .or_else(|| params.get("uri"))
            .and_then(Value::as_str)
        {
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
        let (status, body) = self
            .rpc(
                &self.oauth,
                Some(window),
                "tools/call",
                json!({"name":name,"arguments":arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

fn success(body: Value) -> Value {
    assert_eq!(
        body["result"]["structuredContent"]["success"], true,
        "{body}"
    );
    body["result"]["structuredContent"]["output"].clone()
}
fn denied(body: &Value) {
    assert!(
        body.get("error").is_some() || body["result"]["isError"] == true,
        "{body}"
    );
}

#[test]
fn host_principal_uses_verified_client_and_subject_not_display_or_token_identity() {
    let mut auth = AuthContext::new(AuthKind::OAuth2Token);
    auth.allowed_client_id = Some("client-a".into());
    auth.user_id = Some("subject-a".into());
    auth.token_kind = Some("oauth2".into());
    let original = crate::agent_host_auth::principal_key(&auth).unwrap();
    auth.api_key_id = Some("refreshed-token".into());
    auth.username = Some("display-other".into());
    assert_eq!(
        crate::agent_host_auth::principal_key(&auth).unwrap(),
        original
    );
    auth.allowed_client_id = Some("client-b".into());
    assert_ne!(
        crate::agent_host_auth::principal_key(&auth).unwrap(),
        original
    );
    auth.kind = AuthKind::ApiToken;
    assert!(crate::agent_host_auth::principal_key(&auth).is_none());
    auth.kind = AuthKind::OAuth2Token;
    auth.token_kind = Some(crate::auth::PROJECT_SHARE_OAUTH_TOKEN_KIND.into());
    auth.project_grant_id = Some("project-grant-a".into());
    assert!(crate::agent_host_auth::principal_key(&auth).is_none());
}

#[tokio::test]
async fn host_registration_is_explicit_and_exact_principal_and_window_are_required() {
    let f = Fixture::new();
    let (status, _) = f
        .rpc(
            &f.oauth,
            Some(SOURCE),
            "tools/call",
            json!({"name":"agent_identity","arguments":{}}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = f.control(BOOTSTRAP, f.registration(false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f.control(&f.oauth, f.registration(true)).await;
    assert_ne!(status, StatusCode::OK);
    f.register().await;
    let identity = success(f.call(SOURCE, "agent_identity", json!({})).await);
    assert_eq!(identity["agent_id"], f.run.agent.agent_id);
    assert_eq!(identity["run_id"], f.run.run_id);
    denied(&f.call("unbound-window", "agent_identity", json!({})).await);
    denied(
        &f.call(
            "unbound-window",
            "private-host-sentinel-do-not-log",
            json!({"private":"private-host-sentinel-do-not-log"}),
        )
        .await,
    );
    {
        let conn = f.db.conn_for_tests();
        let codes: Vec<String> = conn
            .prepare("SELECT code FROM wc_agent_isolation_audit")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(codes
            .iter()
            .any(|code| code == "agent_host_operation_denied"));
        assert!(!codes.iter().any(|code| code.contains("sentinel")));
    }
    let (_, unbound_tools) = f
        .rpc(&f.oauth, Some("unbound-window"), "tools/list", json!({}))
        .await;
    let names: Vec<_> = unbound_tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for required in [
        "goal_handoff",
        "goal_read",
        "goal_claim_node",
        "agent_private",
    ] {
        assert!(names.contains(&required));
    }
    assert!(names.contains(&"run_process"));
    let (_, active_tools) = f.rpc(&f.oauth, Some(SOURCE), "tools/list", json!({})).await;
    assert_eq!(
        unbound_tools["result"]["tools"], active_tools["result"]["tools"],
        "the host can cache this catalog across the handoff without listChanged notifications"
    );
    let handoff_spec = unbound_tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "goal_handoff")
        .unwrap();
    assert_eq!(
        handoff_spec["inputSchema"]["properties"]["action"]["enum"],
        json!(["status", "prepare", "commit"])
    );
    let (_, missing) = f
        .rpc(
            &f.oauth,
            None,
            "tools/call",
            json!({"name":"agent_identity","arguments":{"_meta":{"openai/session":SOURCE}}}),
        )
        .await;
    denied(&missing);
    let (other_client, _) = seed_oauth_client_named(&f.db, &f.user, "Synthetic trusted host");
    let token = oauth_token(
        &f.db,
        &other_client,
        &f.user,
        "runtime:read project:read project:write",
    );
    assert_eq!(
        f.rpc(&token, Some(SOURCE), "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    let other_user = seed_user(&f.db, "other-owner");
    let token = oauth_token(
        &f.db,
        &f.client,
        &other_user,
        "runtime:read project:read project:write",
    );
    assert_eq!(
        f.rpc(&token, Some(SOURCE), "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn registered_host_shares_runtime_status_and_respects_original_oauth_scopes() {
    let f = Fixture::new();
    f.register().await;
    success(
        f.call(
            SOURCE,
            "agent_private",
            json!({"action":"write","path":"memory/test.txt","content":"own-memory"}),
        )
        .await,
    );
    for name in ["run_process", "run_shell", "coding_agent_start"] {
        // Missing business arguments still fail the ordinary tool parser.
        denied(&f.call(SOURCE, name, json!({})).await);
    }
    let status = success(
        f.call(SOURCE, "runtime_status", json!({"compact":true}))
            .await,
    );
    assert!(status.get("agents").is_some(), "{status}");
    assert!(status.get("build").is_some(), "{status}");
    let (_, body) = f
        .rpc(
            &f.oauth,
            Some(SOURCE),
            "resources/read",
            json!({"uri":"webcodex://runtime"}),
        )
        .await;
    denied(&body);
    let read_token = oauth_token(&f.db, &f.client, &f.user, "runtime:read project:read");
    let (_, body) = f.rpc(&read_token, Some(SOURCE), "tools/call", json!({"name":"agent_private","arguments":{"action":"write","path":"memory/denied.txt","content":"x"}})).await;
    denied(&body);
    let (_, body) = f
        .rpc(
            &read_token,
            Some(SOURCE),
            "tools/call",
            json!({"name":"agent_private","arguments":{"action":"read","path":"memory/test.txt"}}),
        )
        .await;
    assert_eq!(success(body)["content"], "own-memory");
    let conn = f.db.conn_for_tests();
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM action_events WHERE endpoint='/mcp'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        events, 0,
        "private host calls must not enter the account-wide action ledger"
    );
    drop(conn);
    let mut response = TestClient::post("http://localhost/api/tools/call")
        .bearer_auth(&f.oauth)
        .json(&json!({"tool":"agent_identity","params":{}}))
        .send(&f.service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
    let _: Value = response.take_json().await.unwrap();
    let (_, result) = f.control(BOOTSTRAP, json!({"action":"revoke_host_principal",
        "oauth_client_id":f.client.client_id,"oauth_subject_kind":"managed_user","oauth_subject_id":f.user.id})).await;
    assert_eq!(result["revoked"], true);
    assert_eq!(
        f.rpc(&f.oauth, Some(SOURCE), "tools/list", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

fn task_plan() -> Value {
    let criterion =
        json!({"kind":"review","id":"evidence","description":"Evidence supports result"});
    json!({"objective":"Continue the same task","deliverables":["Result"],"final_criteria":[criterion],
        "nodes":[{"id":"work","title":"Work","work":"Save the result","outputs":["Result"],
            "criterion_ids":["evidence"],"acceptance":[criterion],"failure_action":"Record blocker"}],
        "state_management":{"facts":"Sources","decisions":"Choices","issues":"Blockers","artifacts":"Files",
            "checkpoint_triggers":["After work"],"resume_instructions":"Read exact task state"}})
}

#[tokio::test]
async fn host_http_handoff_redeems_one_time_claim_and_survives_lost_response_and_restart() {
    let mut f = Fixture::new();
    f.register().await;
    let (room_status, room) = f
        .control(
            BOOTSTRAP,
            json!({"action":"create_meeting","project":"project-a",
        "participants":[f.run.agent.agent_id],"brief":"Existing authorized task room"}),
        )
        .await;
    assert_eq!(room_status, StatusCode::OK, "{room}");
    let room_id = room["room_id"].as_str().unwrap().to_owned();
    success(
        f.call(
            SOURCE,
            "agent_meeting",
            json!({"action":"publish","room_id":room_id,"kind":"message",
        "content":"Existing public task progress","idempotency_key":"before-handoff"}),
        )
        .await,
    );
    success(
        f.call(
            SOURCE,
            "agent_private",
            json!({"action":"write","path":"memory/own.txt","content":"retained identity"}),
        )
        .await,
    );
    let started = success(f.call(SOURCE, "goal_mode_start", json!({"project":"project-a","objective":"Continue the same task","idempotency_key":"start"})).await);
    let goal = started["goal"]["goal_id"].as_str().unwrap().to_owned();
    let directory = started["goal"]["task_directory"]
        .as_str()
        .unwrap()
        .to_owned();
    let saved = success(f.call(SOURCE, "goal_plan_save", json!({"goal_id":goal,"expected_revision":started["goal"]["revision"],"idempotency_key":"plan","plan":task_plan()})).await);
    let confirmed = success(f.call(SOURCE, "goal_confirm_plan", json!({"goal_id":goal,"expected_revision":saved["goal"]["revision"],
        "plan_version":1,"plan_sha256":saved["plan"]["hash"],"confirmation":"User approves this exact plan","idempotency_key":"confirm"})).await);
    success(
        f.call(
            SOURCE,
            "goal_read",
            json!({"goal_id":goal,"view":"node","node_id":"work"}),
        )
        .await,
    );
    let claimed = success(
        f.call(
            SOURCE,
            "goal_claim_node",
            json!({"goal_id":goal,"expected_revision":confirmed["goal"]["revision"],
        "plan_version":1,"node_id":"work","idempotency_key":"claim-source"}),
        )
        .await,
    );
    let artifact = format!("{directory}/result.txt");
    let first_file = success(
        f.call(
            SOURCE,
            "agent_private",
            json!({"action":"write","path":artifact,"content":"first part"}),
        )
        .await,
    );
    let checkpoint_args = json!({"goal_id":goal,"expected_revision":claimed["goal"]["revision"],"idempotency_key":"handoff",
        "patch":{"action":"handoff","reason":"Continue in fresh chat","state":{"facts":{"progress":"first part saved"}}}});
    let checkpoint = success(
        f.call(SOURCE, "goal_checkpoint", checkpoint_args.clone())
            .await,
    );
    assert_eq!(checkpoint["handoff"]["status"], "waiting");
    denied(
        &f.call(
            SOURCE,
            "agent_meeting",
            json!({"action":"publish","room_id":room_id,"kind":"message",
        "content":"Must stop while waiting","idempotency_key":"while-waiting"}),
        )
        .await,
    );
    success(
        f.call(
            SOURCE,
            "agent_meeting",
            json!({"action":"read","room_id":room_id}),
        )
        .await,
    );
    assert_eq!(checkpoint["handoff"]["source_agent_run_id"], f.run.run_id);
    assert_eq!(
        success(f.call(SOURCE, "goal_checkpoint", checkpoint_args).await)["handoff"]["handoff_id"],
        checkpoint["handoff"]["handoff_id"]
    );
    denied(&f.call(SOURCE, "agent_private", json!({"action":"write","path":artifact,"content":"late source","expected_sha256":first_file["sha256"]})).await);
    assert!(
        f.db.authenticate_agent_run(&f.run.token).is_ok(),
        "source must not retire before commit"
    );
    let control = json!({"action":"handoff_begin_attempt","owner_user":f.user.id,"project":"project-a","agent_id":f.run.agent.agent_id,
        "goal_id":goal,"handoff_id":checkpoint["handoff"]["handoff_id"]});
    let (status, reserved) = f.control(BOOTSTRAP, control.clone()).await;
    assert_eq!(status, StatusCode::OK, "{reserved}");
    assert_eq!(reserved["handoff"]["attempt_count"], 1);
    let (_, repeated) = f.control(BOOTSTRAP, control).await;
    assert_eq!(
        repeated["handoff"]["attempt_id"],
        reserved["handoff"]["attempt_id"]
    );
    let mut args = json!({"goal_id":goal,"action":"prepare","handoff_id":reserved["handoff"]["handoff_id"],
        "attempt_id":reserved["handoff"]["attempt_id"],"checkpoint_sequence":reserved["handoff"]["checkpoint_sequence"]});
    // Possession of the exact attempt identifier grants no window authority.
    denied(&f.call("candidate", "goal_handoff", args.clone()).await);
    let issuance = json!({"action":"handoff_launch_claim","owner_user":f.user.id,"project":"project-a",
        "agent_id":f.run.agent.agent_id,"goal_id":goal,"handoff_id":args["handoff_id"],"expected_attempt_id":args["attempt_id"]});
    let (status, grant) = f.control(BOOTSTRAP, issuance.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(grant["goal_id"], goal);
    assert_eq!(grant["attempt_id"], args["attempt_id"]);
    assert_eq!(grant["expires_at_ms"], reserved["handoff"]["deadline_at"]);
    let launch_claim = grant["claim_grant"]
        .as_str()
        .expect("one-time control grant")
        .to_owned();
    assert_eq!(
        f.rpc(&launch_claim, Some("candidate"), "tools/list", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "preparation grant must never authenticate as a general run token"
    );
    assert_eq!(
        f.control(BOOTSTRAP, issuance).await.0,
        StatusCode::FORBIDDEN,
        "a lost issuance response cannot rotate/reissue the grant"
    );
    let mut fake = args.clone();
    fake["launch_claim"] = args["attempt_id"].clone();
    denied(&f.call("candidate", "goal_handoff", fake).await);
    args["launch_claim"] = json!(launch_claim);
    denied(&f.call(SOURCE, "goal_handoff", args.clone()).await);
    denied(
        &f.call(
            "candidate",
            "agent_private",
            json!({"action":"read","path":"memory/own.txt"}),
        )
        .await,
    );
    denied(
        &f.call("candidate", "agent_meeting", json!({"action":"list"}))
            .await,
    );
    let prepared = success(f.call("candidate", "goal_handoff", args.clone()).await);
    assert!(!prepared.to_string().contains(&launch_claim));
    denied(
        &f.call(
            "candidate",
            "agent_private",
            json!({"action":"read","path":"memory/own.txt"}),
        )
        .await,
    );
    denied(
        &f.call("candidate", "agent_meeting", json!({"action":"list"}))
            .await,
    );
    let replayed = success(f.call("candidate", "goal_handoff", args.clone()).await);
    assert_eq!(replayed["package_sha256"], prepared["package_sha256"]);
    denied(
        &f.call("different-candidate", "goal_handoff", args.clone())
            .await,
    );
    assert_eq!(prepared["execution_granted"], false);
    assert_eq!(prepared["handoff"]["status"], "prepared");
    assert!(f.db.authenticate_agent_run(&f.run.token).is_ok());
    args["action"] = json!("commit");
    args["package_sha256"] = prepared["package_sha256"].clone();
    denied(&f.call("candidate", "goal_handoff", args.clone()).await);
    args.as_object_mut().unwrap().remove("launch_claim");
    let committed = success(f.call("candidate", "goal_handoff", args.clone()).await);
    assert_eq!(committed["execution_granted"], true);
    assert!(f.db.authenticate_agent_run(&f.run.token).is_err());
    let successor = committed["handoff"]["successor_agent_run_id"].clone();
    success(
        f.call(
            "candidate",
            "agent_meeting",
            json!({"action":"read","room_id":room_id}),
        )
        .await,
    );
    let publication = success(
        f.call(
            "candidate",
            "agent_meeting",
            json!({"action":"publish","room_id":room_id,"kind":"message",
        "content":"New run continues authorized room","idempotency_key":"after-handoff"}),
        )
        .await,
    );
    assert_eq!(publication["author_run_id"], successor);
    denied(
        &f.call(
            SOURCE,
            "agent_meeting",
            json!({"action":"publish","room_id":room_id,"kind":"message",
        "content":"Late old source","idempotency_key":"after-retirement"}),
        )
        .await,
    );
    // Simulate the caller losing the commit response: reconcile with status.
    let status_args = json!({"goal_id":goal,"action":"status","handoff_id":args["handoff_id"],"attempt_id":args["attempt_id"]});
    let observed = success(
        f.call("candidate", "goal_handoff", status_args.clone())
            .await,
    );
    assert_eq!(observed["execution_granted"], true);
    assert_eq!(observed["handoff"]["attempt_count"], 1);
    assert_eq!(
        success(f.call("candidate", "goal_handoff", args).await)["handoff"]
            ["successor_agent_run_id"],
        successor
    );
    denied(
        &f.call(
            SOURCE,
            "agent_private",
            json!({"action":"write","path":"memory/late.txt","content":"old"}),
        )
        .await,
    );
    assert_eq!(
        success(
            f.call(
                "candidate",
                "agent_private",
                json!({"action":"read","path":"memory/own.txt"})
            )
            .await
        )["content"],
        "retained identity"
    );
    denied(&f.call("candidate","agent_private",json!({"action":"write","path":artifact,"content":"unclaimed","expected_sha256":first_file["sha256"]})).await);
    let node = success(
        f.call(
            "candidate",
            "goal_read",
            json!({"goal_id":goal,"view":"node","node_id":"work"}),
        )
        .await,
    );
    success(
        f.call(
            "candidate",
            "goal_claim_node",
            json!({"goal_id":goal,"expected_revision":node["context_read"]["revision"],
        "plan_version":1,"node_id":"work","idempotency_key":"claim-successor"}),
        )
        .await,
    );
    success(f.call("candidate","agent_private",json!({"action":"write","path":artifact,"content":"continued","expected_sha256":first_file["sha256"]})).await);
    f.db = Arc::new(crate::Database::open(&f.temp.path().join("state.db")).unwrap());
    f.service = Fixture::service(&f.db, f.temp.path());
    assert_eq!(
        success(f.call("candidate", "goal_handoff", status_args).await)["execution_granted"],
        true
    );
    assert_eq!(
        success(
            f.call(
                "candidate",
                "agent_private",
                json!({"action":"read","path":artifact})
            )
            .await
        )["content"],
        "continued"
    );
    assert_eq!(
        success(f.call("candidate", "agent_identity", json!({})).await)["agent_id"],
        f.run.agent.agent_id
    );
}
