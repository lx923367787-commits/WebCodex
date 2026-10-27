//! End-to-end request authorization with the production middleware, handlers,
//! SQLite store and private-file broker. Every state directory is synthetic.
use crate::test_support::{seed_user, test_config};
use crate::tool_runtime::ToolRuntime;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};
use serde_json::{json, Value};
use std::sync::Arc;

const BOOTSTRAP: &str = "synthetic-isolation-bootstrap";
const OWNER: &str = "synthetic-owner";
const PROJECT: &str = "synthetic-project";

struct Fixture {
    service: Service,
    db: Arc<crate::Database>,
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new(auth_enabled: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::Database::open(&temp.path().join("state.db")).unwrap());
        let mut config = test_config(auth_enabled.then_some(BOOTSTRAP));
        Arc::get_mut(&mut config).unwrap().data_dir = temp.path().to_path_buf();
        let runtime =
            Arc::new(ToolRuntime::new_for_tests().with_communication_database(db.clone()));
        let router = Router::new()
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
                    .push(Router::with_path("tools/call").post(crate::runtime_http::tools_call))
                    .push(Router::with_path("tools/list").post(crate::runtime_http::tools_list))
                    .push(
                        Router::with_path("admin/agent-isolation")
                            .post(crate::agent_isolation::control),
                    )
                    .push(
                        Router::with_path("projects/run_shell")
                            .post(crate::runtime_http::projects_run_shell),
                    )
                    .push(
                        Router::with_path("projects/run_job")
                            .post(crate::runtime_http::projects_run_job),
                    )
                    .push(Router::with_path("jobs/log").post(crate::runtime_http::job_log))
                    .push(
                        Router::with_path("runtime/status")
                            .post(crate::runtime_http::runtime_status),
                    )
                    .push(
                        Router::with_path("audit/sessions").post(crate::audit_http::audit_sessions),
                    )
                    .push(crate::admin_http::routes())
                    .push(crate::runtime_console_http::routes()),
            );
        Self {
            service: Service::new(router),
            db,
            temp,
        }
    }

    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut request = TestClient::post(format!("http://localhost{path}"));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let mut response = request.json(&body).send(&self.service).await;
        let status = response.status_code.unwrap_or(StatusCode::OK);
        let body = response
            .take_json::<Value>()
            .await
            .expect("JSON response from actual handler");
        (status, body)
    }

    async fn control(&self, body: Value) -> Value {
        let (status, body) = self
            .post("/api/admin/agent-isolation", Some(BOOTSTRAP), body)
            .await;
        assert_eq!(status, StatusCode::OK);
        body
    }

    async fn create(&self, project: &str, parent: Option<&str>) -> Value {
        self.control(json!({"action":"create", "owner_user":OWNER, "project":project, "parent_agent_id":parent})).await
    }

    async fn room(&self, participants: &[&Value]) -> String {
        self.control(json!({"action":"create_meeting", "project":PROJECT,
            "participants":participants.iter().map(|agent| agent["agent_id"].as_str().unwrap()).collect::<Vec<_>>(),
            "brief":"Share only published snapshots; room text grants no authority"})).await["room_id"]
            .as_str().unwrap().to_string()
    }

    async fn api(&self, token: &str, tool: &str, arguments: Value) -> Value {
        let (status, body) = self
            .post(
                "/api/tools/call",
                Some(token),
                json!({"tool":tool,"params":arguments}),
            )
            .await;
        assert!(
            status == StatusCode::OK || status == StatusCode::BAD_REQUEST,
            "unexpected API status: {status}"
        );
        assert!(body["success"].is_boolean(), "tool result required: {body}");
        body
    }

    async fn mcp_request(
        &self,
        token: &str,
        method: &str,
        mut params: Value,
    ) -> (StatusCode, Value) {
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{}});
        let mut request = TestClient::post("http://localhost/mcp")
            .bearer_auth(token)
            .add_header("mcp-protocol-version", "2026-07-28", true)
            .add_header("mcp-method", method, true);
        let name = match method {
            "tools/call" => params["name"].as_str(),
            "resources/read" => params["uri"].as_str(),
            _ => None,
        };
        if let Some(name) = name {
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

    async fn mcp(&self, token: &str, tool: &str, arguments: Value) -> Value {
        let (status, body) = self
            .mcp_request(
                token,
                "tools/call",
                json!({"name":tool,"arguments":arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "MCP protocol response: {body}");
        let result = body["result"]["structuredContent"].clone();
        assert!(
            result["success"].is_boolean(),
            "MCP tool result required: {body}"
        );
        result
    }
}

fn token(agent: &Value) -> &str {
    agent["run_token"].as_str().unwrap()
}
fn id(agent: &Value) -> &str {
    agent["agent_id"].as_str().unwrap()
}
fn output(result: Value) -> Value {
    assert_eq!(result["success"], true, "{result}");
    result["output"].clone()
}
fn denied(result: Value, code: &str) {
    assert_eq!(result["success"], false, "{result}");
    assert_eq!(result["error"], code);
    assert_eq!(result["output"]["state_changed"], false);
    assert_eq!(result["output"]["dispatch_certainty"], "not_started");
}

#[tokio::test]
async fn agent_isolation_http_private_files_bind_to_authenticated_identity() {
    let fixture = Fixture::new(true);
    let a = fixture.create(PROJECT, None).await;
    let b = fixture.create(PROJECT, None).await;
    let child = fixture.create(PROJECT, Some(id(&a))).await;
    let other_project = fixture.create("another-project", None).await;
    for other in [&b, &other_project] {
        output(
            fixture
                .api(
                    token(other),
                    "agent_private",
                    json!({"action":"write",
            "path":"identity/self.txt","content":"this Agent's own namespace"}),
                )
                .await,
        );
    }
    let secret = "synthetic private memory A";
    for area in [
        "identity",
        "goals",
        "tasks",
        "memory",
        "checkpoints",
        "handoff",
        "workspace",
        "cli_sessions",
    ] {
        output(
            fixture
                .api(
                    token(&a),
                    "agent_private",
                    json!({"action":"write","path":format!("{area}/note.txt"),"content":secret}),
                )
                .await,
        );
    }
    assert_eq!(
        output(
            fixture
                .mcp(
                    token(&a),
                    "agent_private",
                    json!({"action":"read","path":"memory/note.txt"})
                )
                .await
        )["content"],
        secret
    );
    output(
        fixture
            .api(
                token(&child),
                "agent_private",
                json!({"action":"write",
        "path":"memory/child.txt","content":"child's private work"}),
            )
            .await,
    );
    denied(
        fixture
            .api(
                token(&a),
                "agent_private",
                json!({"action":"read","path":"memory/child.txt"}),
            )
            .await,
        "agent_private_file_not_found",
    );
    let conn = fixture.db.conn_for_tests();
    let summaries = conn
        .prepare("SELECT summary_json FROM action_events")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(conn);
    assert!(
        summaries.iter().all(|summary| !summary.contains(secret)),
        "private text entered action audit"
    );
    let listing = output(
        fixture
            .api(
                token(&a),
                "agent_private",
                json!({"action":"list","path":"memory"}),
            )
            .await,
    );
    assert_eq!(listing[0]["path"], "memory/note.txt");
    let matches = output(
        fixture
            .mcp(
                token(&a),
                "agent_private",
                json!({"action":"search","path":"memory","query":"private memory"}),
            )
            .await,
    );
    assert_eq!(matches[0]["text"], secret);

    for other in [&b, &child, &other_project] {
        denied(
            fixture
                .api(
                    token(other),
                    "agent_private",
                    json!({"action":"read","path":"memory/note.txt"}),
                )
                .await,
            "agent_private_file_not_found",
        );
        let own_listing = output(
            fixture
                .api(
                    token(other),
                    "agent_private",
                    json!({"action":"list","path":"memory"}),
                )
                .await,
        );
        assert!(own_listing
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["path"] != "memory/note.txt"));
        assert_eq!(
            output(
                fixture
                    .mcp(
                        token(other),
                        "agent_private",
                        json!({"action":"search","path":"","query":secret})
                    )
                    .await
            ),
            json!([])
        );
    }
    for path in [
        format!("../{}/private/memory/note.txt", id(&a)),
        "memory/../../outside.txt".into(),
        fixture
            .temp
            .path()
            .join("state.db")
            .to_string_lossy()
            .into_owned(),
        "memory/note.txt:stream".into(),
    ] {
        denied(
            fixture
                .mcp(
                    token(&b),
                    "agent_private",
                    json!({"action":"read","path":path}),
                )
                .await,
            "agent_private_path_invalid",
        );
    }
    for field in ["agent_id", "owner_user", "project", "run_id"] {
        let mut arguments = json!({"action":"read","path":"memory/note.txt"});
        arguments[field] = json!(id(&a));
        denied(
            fixture.api(token(&b), "agent_private", arguments).await,
            "agent_invalid_input",
        );
    }
    output(
        fixture
            .api(
                token(&b),
                "agent_private",
                json!({"action":"write","path":"memory/note.txt","content":"B's separate memory"}),
            )
            .await,
    );
    assert_eq!(
        output(
            fixture
                .api(
                    token(&a),
                    "agent_private",
                    json!({"action":"read","path":"memory/note.txt"})
                )
                .await
        )["content"],
        secret
    );
}

#[tokio::test]
async fn agent_isolation_http_blocked_execution_never_reaches_legacy_dispatch() {
    let fixture = Fixture::new(true);
    let a = fixture.create(PROJECT, None).await;
    let sentinel = fixture.temp.path().join("must-not-execute.txt");
    let arguments = json!({"project":PROJECT,"command":format!("echo forbidden > {}",sentinel.display()),
        "script":"raise RuntimeError('must not execute')","path":sentinel,"action":"run"});
    for (tool, code) in [
        ("run_shell", "execution_isolation_unavailable"),
        ("run_script", "execution_isolation_unavailable"),
        ("process_start", "execution_isolation_unavailable"),
        ("plugin_tool", "execution_isolation_unavailable"),
        ("mcp_tool", "execution_isolation_unavailable"),
        ("coding_agent_start", "execution_isolation_unavailable"),
        ("coding_agent_resume", "execution_isolation_unavailable"),
        ("goal_start", "execution_isolation_unavailable"),
        ("workspace_checkpoint_list", "agent_tool_not_isolated"),
        ("checkpoint", "agent_tool_not_isolated"),
        ("cli_resume", "agent_tool_not_isolated"),
        ("export_session", "agent_tool_not_isolated"),
        // The file adapter is now admitted, but still rejects arbitrary legacy
        // shell arguments before any project or filesystem dispatch.
        ("read_file", "workspace_invalid_input"),
    ] {
        denied(fixture.api(token(&a), tool, arguments.clone()).await, code);
        denied(fixture.mcp(token(&a), tool, arguments.clone()).await, code);
    }
    assert!(!sentinel.exists());
    for route in [
        "/api/projects/run_shell",
        "/api/projects/run_job",
        "/api/jobs/log",
        "/api/runtime/status",
        "/api/admin/dashboard",
        "/api/admin/agent-isolation",
        "/api/audit/sessions",
    ] {
        let (status, body) = fixture
            .post(
                route,
                Some(token(&a)),
                json!({"action":"create","owner_user":OWNER,"project":PROJECT}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{route}: {body}");
        assert_eq!(body["error"], "agent_credential_audience_denied");
    }
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn agent_isolation_http_model_routes_never_accept_user_bootstrap_or_open_mode() {
    let _env = crate::auth::AuthEnvGuard::auth_required();
    let fixture = Fixture::new(true);
    let user = seed_user(&fixture.db, "same-owner-legacy-user");
    let legacy_token = crate::auth::generate_api_token();
    fixture
        .db
        .insert_api_key(
            &crate::models::ApiKeyRecord {
                id: uuid::Uuid::new_v4().to_string(),
                user_id: user.id,
                name: "legacy".into(),
                key_prefix: crate::auth::token_prefix(&legacy_token),
                created_at: chrono::Utc::now().timestamp(),
                last_used_at: None,
                revoked_at: None,
                scopes: "runtime:read project:read project:write job:run account:manage".into(),
                expires_at: None,
                kind: crate::models::TOKEN_KIND_USER.into(),
                allowed_client_id: None,
            },
            &crate::auth::hash_token(&legacy_token),
        )
        .unwrap();
    for token in [
        None,
        Some(BOOTSTRAP),
        Some(legacy_token.as_str()),
        Some("wc_ar_invalid"),
    ] {
        for (path, body) in [
            (
                "/api/tools/call",
                json!({"tool":"agent_identity","params":{}}),
            ),
            ("/api/tools/list", json!({})),
            (
                "/mcp",
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
            ),
        ] {
            let (status, body) = fixture.post(path, token, body).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {body}");
            assert_eq!(body["error"], "agent_identity_required");
        }
    }
    _env.enable_open_anonymous();
    let (status, _) = fixture
        .post(
            "/api/tools/call",
            None,
            json!({"tool":"agent_identity","params":{}}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let open = Fixture::new(false);
    for path in ["/api/tools/call", "/api/tools/list", "/mcp"] {
        let (status, body) = open.post(path, None, json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "agent_identity_required");
    }
}

#[tokio::test]
async fn agent_isolation_http_inventory_exposes_only_broker_tools() {
    let fixture = Fixture::new(true);
    let a = fixture.create(PROJECT, None).await;
    let identity = output(fixture.api(token(&a), "agent_identity", json!({})).await);
    assert_eq!(identity["agent_id"], id(&a));
    assert_eq!(identity["project"], PROJECT);
    assert_eq!(identity["execution"]["allowed"], false);
    let (status, api_list) = fixture
        .post("/api/tools/list", Some(token(&a)), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, mcp_list) = fixture
        .mcp_request(token(&a), "tools/list", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    for list in [&api_list, &mcp_list["result"]] {
        let mut names = list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            [
                "agent_assignment",
                "agent_attach",
                "agent_identity",
                "agent_meeting",
                "agent_private",
                "agent_project_files",
                "apply_text_edits",
                "goal_bootstrap",
                "goal_checkpoint",
                "goal_claim_node",
                "goal_confirm_plan",
                "goal_handoff",
                "goal_mode_start",
                "goal_plan_save",
                "goal_read",
                "read_file",
                "read_files",
                "search_project_texts",
                "team_agent_start",
                "team_cli_catalog",
                "team_cli_close",
                "team_cli_delivery_collect",
                "team_cli_delivery_read",
                "team_cli_delivery_reject_unpublished",
                "team_cli_delivery_review",
                "team_cli_observe",
                "team_cli_start",
                "team_member_create",
                "team_plan_list",
                "team_plan_read",
                "team_plan_register",
                "team_policy_get",
                "team_receipt_list",
                "team_task_complete",
                "team_task_execution_read",
                "team_task_read",
                "team_task_start"
            ]
        );
    }
    let (_, resource) = fixture
        .mcp_request(
            token(&a),
            "resources/read",
            json!({"uri":"file:///private/memory/note.txt"}),
        )
        .await;
    assert!(
        resource.get("error").is_some(),
        "run credentials cannot read legacy resources: {resource}"
    );
}

#[tokio::test]
async fn agent_isolation_http_route_aliases_cannot_bypass_credential_audiences() {
    let fixture = Fixture::new(true);
    let a = fixture.create(PROJECT, None).await;
    output(
        fixture
            .api(
                token(&a),
                "agent_private",
                json!({"action":"write",
        "path":"memory/alias.txt","content":"private alias sentinel"}),
            )
            .await,
    );
    for path in [
        "/mcp/",
        "/%6dcp",
        "/api/tools/call/",
        "/api//tools/call",
        "/api/%74ools/call",
        "/api/tools/call%2f",
    ] {
        for credential in [BOOTSTRAP, token(&a)] {
            let args = json!({"action":"read","path":"memory/alias.txt"});
            let body = if path.contains("mcp") || path.contains("%6dcp") {
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"agent_private","arguments":args}})
            } else {
                json!({"tool":"agent_private","params":args})
            };
            let mut response = TestClient::post(format!("http://localhost{path}"))
                .bearer_auth(credential)
                .json(&body)
                .send(&fixture.service)
                .await;
            let status = response.status_code.unwrap_or(StatusCode::OK);
            assert!(
                status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
                "alias escaped pre-dispatch rejection: {path} {status}"
            );
            if status == StatusCode::BAD_REQUEST {
                let body: Value = response.take_json().await.unwrap();
                assert_eq!(
                    body["error"], "noncanonical_authenticated_route",
                    "{path}: {body}"
                );
                assert!(!body.to_string().contains("private alias sentinel"));
            }
        }
    }
    assert_eq!(
        output(
            fixture
                .api(
                    token(&a),
                    "agent_private",
                    json!({"action":"read","path":"memory/alias.txt"})
                )
                .await
        )["content"],
        "private alias sentinel"
    );
}

#[tokio::test]
async fn agent_isolation_http_meeting_publications_keep_authors_snapshots_and_exact_reviews() {
    let fixture = Fixture::new(true);
    let a = fixture.create(PROJECT, None).await;
    let b = fixture.create(PROJECT, None).await;
    let outsider = fixture.create(PROJECT, None).await;
    let foreign = fixture.create("another-project", None).await;
    let room = fixture.room(&[&a, &b]).await;
    let publication = json!({"action":"publish","room_id":room,"kind":"artifact","content":"Review the published copy",
        "artifact_content":"snapshot version one","idempotency_key":"v1"});
    let first = output(
        fixture
            .api(token(&a), "agent_meeting", publication.clone())
            .await,
    );
    assert_eq!(first["author_agent_id"], id(&a));
    assert_eq!(first["author_run_id"], a["run_id"]);
    assert_eq!(first["artifact"]["version"], 1);
    let artifact = first["artifact"]["artifact_id"].as_str().unwrap();
    let mut replay = output(
        fixture
            .mcp(token(&a), "agent_meeting", publication.clone())
            .await,
    );
    let mut first_snapshot = first.clone();
    // Transport observations change on a retry; every persisted entry field
    // must remain identical, including server author, version and timestamp.
    replay.as_object_mut().unwrap().remove("context_meter");
    first_snapshot
        .as_object_mut()
        .unwrap()
        .remove("context_meter");
    assert_eq!(replay, first_snapshot);
    let second = output(
        fixture
            .api(
                token(&a),
                "agent_meeting",
                json!({"action":"publish","room_id":room,"kind":"artifact",
        "artifact_id":artifact,"artifact_content":"snapshot version two","idempotency_key":"v2"}),
            )
            .await,
    );
    assert_eq!(second["artifact"]["version"], 2);
    let reviewed = output(fixture.mcp(token(&b), "agent_meeting", json!({"action":"publish","room_id":room,"kind":"review",
        "review_artifact_id":artifact,"review_version":1,"content":"Review remains bound to version one","idempotency_key":"review-v1"})).await);
    assert_eq!(reviewed["author_agent_id"], id(&b));
    assert_eq!(reviewed["review_target"]["version"], 1);
    let saved = output(
        fixture
            .api(
                token(&b),
                "agent_meeting",
                json!({"action":"artifact","room_id":room,"artifact_id":artifact,"version":1}),
            )
            .await,
    );
    assert_eq!(saved["bytes"], json!(b"snapshot version one".to_vec()));
    let page = output(
        fixture
            .mcp(
                token(&b),
                "agent_meeting",
                json!({"action":"read","room_id":room}),
            )
            .await,
    );
    assert_eq!(page["entries"].as_array().unwrap().len(), 3);
    denied(
        fixture
            .api(
                token(&b),
                "agent_meeting",
                json!({"action":"publish","room_id":room,"kind":"artifact",
        "artifact_id":artifact,"artifact_content":"overwrite A","idempotency_key":"stolen"}),
            )
            .await,
        "meeting_publish_denied",
    );
    denied(fixture.api(token(&b), "agent_meeting", json!({"action":"publish","room_id":room,"kind":"review",
        "review_artifact_id":artifact,"review_version":99,"content":"not a real version","idempotency_key":"bad-review"})).await, "meeting_publish_denied");
    for field in [
        "author_agent_id",
        "author_run_id",
        "created_at_unix_ms",
        "artifact_version",
    ] {
        let mut forged = publication.clone();
        forged[field] = json!(id(&a));
        denied(
            fixture.mcp(token(&b), "agent_meeting", forged).await,
            "agent_invalid_input",
        );
    }
    for other in [&outsider, &foreign] {
        denied(
            fixture
                .api(
                    token(other),
                    "agent_meeting",
                    json!({"action":"read","room_id":room}),
                )
                .await,
            "meeting_access_denied",
        );
        denied(
            fixture
                .api(
                    token(other),
                    "agent_meeting",
                    json!({"action":"read","room_id":"missing-room"}),
                )
                .await,
            "meeting_access_denied",
        );
        denied(
            fixture
                .mcp(token(other), "agent_meeting", publication.clone())
                .await,
            "meeting_publish_denied",
        );
    }
    for action in ["update", "delete"] {
        denied(
            fixture
                .api(
                    token(&b),
                    "agent_meeting",
                    json!({"action":action,"room_id":room,"artifact_id":artifact}),
                )
                .await,
            "agent_invalid_input",
        );
    }
}

#[tokio::test]
async fn agent_isolation_http_handoff_revokes_old_token_and_preserves_private_memory() {
    let fixture = Fixture::new(true);
    let a = fixture.create(PROJECT, None).await;
    output(fixture.api(token(&a), "agent_private", json!({"action":"write","path":"memory/continue.txt","content":"persisted for this logical Agent"})).await);
    let pending = fixture
        .control(json!({"action":"begin_handoff","run_token":token(&a)}))
        .await;
    let (status, _) = fixture
        .post(
            "/api/tools/call",
            Some(token(&pending)),
            json!({"tool":"agent_identity","params":{}}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    output(fixture.api(token(&a), "agent_identity", json!({})).await);
    fixture
        .control(
            json!({"action":"activate","owner_user":OWNER,"project":PROJECT,
        "agent_id":id(&a),"pending_run_id":pending["run_id"]}),
        )
        .await;
    for path in ["/api/tools/call", "/mcp"] {
        let (status, body) = fixture
            .post(
                path,
                Some(token(&a)),
                json!({"tool":"agent_identity","params":{}}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "agent_identity_required");
    }
    let identity = output(
        fixture
            .mcp(token(&pending), "agent_identity", json!({}))
            .await,
    );
    assert_eq!(identity["agent_id"], id(&a));
    assert_eq!(identity["generation"], 2);
    assert_eq!(
        output(
            fixture
                .api(
                    token(&pending),
                    "agent_private",
                    json!({"action":"read","path":"memory/continue.txt"})
                )
                .await
        )["content"],
        "persisted for this logical Agent"
    );
    denied(
        fixture
            .api(
                token(&pending),
                "agent_identity",
                json!({"agent_id":"forged","generation":1}),
            )
            .await,
        "agent_invalid_input",
    );
}

// Dropping a failed test must close its listener too; a successful test also
// explicitly waits for graceful shutdown below.
struct AbortServerOnDrop(tokio::task::JoinHandle<std::io::Result<()>>);
impl Drop for AbortServerOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn network_json(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (u16, Value) {
    let response = client
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap())
}

#[tokio::test]
async fn agent_isolation_loopback_server_enforces_identity_at_real_network_boundary() {
    use salvo::conn::{Acceptor, Listener, TcpListener};
    use std::time::Duration;

    let fixture = Fixture::new(true);
    let acceptor = TcpListener::new("127.0.0.1:0").try_bind().await.unwrap();
    let address = acceptor.holdings()[0]
        .local_addr
        .clone()
        .into_std()
        .unwrap();
    let server = Server::new(acceptor);
    let handle = server.handle();
    let service = Service::new(fixture.service.router());
    let mut task = AbortServerOnDrop(tokio::spawn(async move { server.try_serve(service).await }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let base = format!("http://{address}");
    let (status, a) = network_json(
        &client,
        &base,
        "/api/admin/agent-isolation",
        BOOTSTRAP,
        json!({"action":"create","owner_user":OWNER,"project":PROJECT}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, b) = network_json(
        &client,
        &base,
        "/api/admin/agent-isolation",
        BOOTSTRAP,
        json!({"action":"create","owner_user":OWNER,"project":PROJECT}),
    )
    .await;
    assert_eq!(status, 200);

    let (status, written) = network_json(&client, &base, "/api/tools/call", token(&a),
        json!({"tool":"agent_private","params":{"action":"write","path":"memory/network.txt","content":"network-private-sentinel"}})).await;
    assert_eq!(status, 200);
    output(written);
    let (status, read) = network_json(
        &client,
        &base,
        "/api/tools/call",
        token(&a),
        json!({"tool":"agent_private","params":{"action":"read","path":"memory/network.txt"}}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(output(read)["content"], "network-private-sentinel");
    // Verify real file persistence through the same native broker, independent
    // of the HTTP response object.
    let binding = fixture.db.authenticate_agent_run(token(&a)).unwrap();
    let files = crate::agent_private_files::AgentPrivateFiles::new(fixture.db.agent_private_root())
        .unwrap();
    assert_eq!(
        files.read(&binding, "memory/network.txt").unwrap().bytes,
        b"network-private-sentinel"
    );

    let (status, foreign) = network_json(
        &client,
        &base,
        "/api/tools/call",
        token(&b),
        json!({"tool":"agent_private","params":{"action":"read","path":"memory/network.txt"}}),
    )
    .await;
    assert_eq!(status, 400);
    denied(foreign, "agent_private_access_denied");
    let (status, forged) = network_json(&client, &base, "/api/tools/call", token(&b),
        json!({"tool":"agent_private","params":{"action":"read","path":"memory/network.txt","agent_id":id(&a)}})).await;
    assert_eq!(status, 400);
    denied(forged, "agent_invalid_input");
    let sentinel = fixture.temp.path().join("network-cli-must-not-run.txt");
    let (status, execution) = network_json(&client, &base, "/api/tools/call", token(&a),
        json!({"tool":"run_process","params":{"command":"cmd.exe",
            "args":["/c",format!("echo must-not-run > \"{}\"", sentinel.display())],"output_path":sentinel}})).await;
    assert_eq!(status, 400);
    denied(execution, "execution_isolation_unavailable");
    assert!(!sentinel.exists());
    for route in [
        "/api/jobs/log",
        "/api/artifacts/private/memory/network.txt",
        "/api/tools/call",
    ] {
        let response = client
            .get(format!("{base}{route}"))
            .bearer_auth(token(&b))
            .send()
            .await
            .unwrap();
        assert!(
            !response.status().is_success(),
            "GET unexpectedly exposed protected resources: {route}"
        );
        assert!(!response
            .text()
            .await
            .unwrap()
            .contains("network-private-sentinel"));
    }
    drop(client);
    handle.stop_graceful(Some(Duration::from_secs(1)));
    tokio::time::timeout(Duration::from_secs(3), &mut task.0)
        .await
        .expect("loopback server did not stop")
        .expect("loopback task failed")
        .expect("loopback server failed");
    assert!(
        tokio::net::TcpStream::connect(address).await.is_err(),
        "test listener remained open"
    );
}
