//! Real middleware/runtime/SQLite and a real loopback HTTP server. The explicit
//! CLI test also runs the compiled human Office client against that server.
use crate::test_support::{current_runner_registration, seed_user, test_config};
use crate::tool_runtime::ToolRuntime;
use salvo::conn::{Acceptor, Listener, TcpListener};
use salvo::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const PROJECT: &str = "agent:runner:repo";
struct Fixture {
    service: Service,
    db: Arc<crate::Database>,
    pat: String,
    read_only: String,
    foreign: String,
    model: String,
    temp: tempfile::TempDir,
}
fn mint_pat(db: &crate::Database, user: &crate::models::UserRecord, scopes: &str) -> String {
    let token = crate::auth::generate_api_token();
    db.insert_api_key(
        &crate::models::ApiKeyRecord {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: user.id.clone(),
            name: "synthetic-office".into(),
            key_prefix: crate::auth::token_prefix(&token),
            created_at: chrono::Utc::now().timestamp(),
            last_used_at: None,
            revoked_at: None,
            scopes: scopes.into(),
            expires_at: None,
            kind: "user".into(),
            allowed_client_id: None,
        },
        &crate::auth::hash_token(&token),
    )
    .unwrap();
    token
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::Database::open(&temp.path().join("state.db")).unwrap());
        let user = seed_user(&db, "office-user");
        let other = seed_user(&db, "other-user");
        let pat = mint_pat(&db, &user, "runtime:read project:read project:write");
        let read_only = mint_pat(&db, &user, "runtime:read project:read");
        let foreign = mint_pat(&db, &other, "runtime:read project:read project:write");
        let model = db
            .create_isolated_agent(&user.id, PROJECT, None)
            .unwrap()
            .token;
        let registry = Arc::new(crate::runner_http::RunnerRegistry::default());
        registry
            .register(current_runner_registration(
                crate::runner_protocol::RunnerRegisterRequest {
                    agent_execution_resources: Vec::new(),
                    process_started_at: None,
                    build: None,
                    job_concurrency_limit: None,
                    job_inventory: None,
                    coding_agent_providers: None,
                    coding_agent_inventory: None,
                    client_id: "runner".into(),
                    runner_instance_id: "instance".into(),
                    runner_protocol_generation:
                        crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
                    display_name: None,
                    owner: Some(user.username),
                    hostname: None,
                    host_context: None,
                    capabilities: Default::default(),
                    policy: None,
                },
            ))
            .await
            .unwrap();
        crate::test_support::apply_project_inventory_snapshot(
            &registry,
            "runner",
            "instance",
            vec![crate::runner_protocol::RunnerProjectSummary {
                id: "repo".into(),
                name: Some("repo".into()),
                path: temp.path().to_string_lossy().into_owned(),
                allow_patch: true,
                kind: Some("repo".into()),
                registration_source: None,
                description: None,
                hooks: vec![],
                disabled: false,
                revision: None,
                git_branch: None,
                git_head: None,
                git_dirty: None,
                updated_at: 1,
                shell_profile: None,
            }],
        )
        .await;
        let runtime = Arc::new(
            ToolRuntime::new_for_tests_with_runner_registry(registry)
                .with_communication_database(db.clone()),
        );
        let mut config = test_config(Some("office-bootstrap"));
        Arc::get_mut(&mut config).unwrap().data_dir = temp.path().into();
        let service = Service::new(
            Router::new()
                .hoop(affix_state::inject(config))
                .hoop(affix_state::inject(db.clone()))
                .hoop(affix_state::inject(runtime))
                .push(
                    Router::with_path("api")
                        .hoop(crate::AuthMiddleware)
                        .push(Router::with_path("office/control").post(crate::office_http::control))
                        .push(
                            Router::with_path("tools/call").post(crate::runtime_http::tools_call),
                        ),
                ),
        );
        Self {
            service,
            db,
            pat,
            read_only,
            foreign,
            model,
            temp,
        }
    }
}
struct RunningServer {
    base: String,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}
impl Drop for RunningServer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl RunningServer {
    async fn start(service: Service) -> Self {
        let acceptor = TcpListener::new("127.0.0.1:0").try_bind().await.unwrap();
        let address = acceptor.holdings()[0]
            .local_addr
            .clone()
            .into_std()
            .unwrap();
        let task = tokio::spawn(async move { Server::new(acceptor).try_serve(service).await });
        Self {
            base: format!("http://{address}"),
            task: Some(task),
        }
    }
    async fn close(mut self) {
        let task = self.task.take().unwrap();
        task.abort();
        let _ = task.await;
        let address = self.base.strip_prefix("http://").unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
async fn post(
    client: &reqwest::Client,
    server: &RunningServer,
    token: &str,
    body: Value,
) -> (u16, Value) {
    let response = client
        .post(format!("{}/api/office/control", server.base))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

#[tokio::test]
async fn office_http_real_server_accepts_human_config_and_public_task_creation_only() {
    let _env = crate::auth::AuthEnvGuard::auth_required();
    let fixture = Fixture::new().await;
    let server = RunningServer::start(Service::new(fixture.service.router())).await;
    let client = client();
    let (status, policy) = post(
        &client,
        &server,
        &fixture.pat,
        json!({"action":"policy-get","project":PROJECT}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(policy["success"], true);
    assert_eq!(policy["output"]["revision"], 0);
    let policy = serde_json::to_value(webcodex_store::TeamPolicyInput::default()).unwrap();
    let (status, saved) = post(
        &client,
        &server,
        &fixture.pat,
        json!({"action":"policy-save","project":PROJECT,"expected_revision":0,"policy":policy}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(saved["output"]["revision"], 1);
    let request = json!({"action":"task-create","project":PROJECT,"title":"Public root","instruction":"Explicit public brief","idempotency_key":"root-once"});
    let (status, created) = post(&client, &server, &fixture.pat, request.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(created["output"]["created"], true);
    assert_eq!(
        created["output"]["task"]["summary"]["referenced_project_id"],
        PROJECT
    );
    let (_, replayed) = post(&client, &server, &fixture.pat, request).await;
    assert_eq!(replayed["output"]["replayed"], true);
    assert_eq!(
        created["output"]["task"]["summary"]["task_id"],
        replayed["output"]["task"]["summary"]["task_id"]
    );
    for action in ["cli-start", "agent-start", "plan-register", "plan-cancel"] {
        let (status, body) = post(
            &client,
            &server,
            &fixture.pat,
            json!({"action":action,"project":PROJECT}),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["success"], false);
    }
    assert_eq!(
        fixture
            .db
            .conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(client);
    server.close().await;
}

#[tokio::test]
async fn office_http_real_server_rejects_model_bootstrap_foreign_scope_and_origin() {
    let _env = crate::auth::AuthEnvGuard::auth_required();
    let fixture = Fixture::new().await;
    let server = RunningServer::start(Service::new(fixture.service.router())).await;
    let client = client();
    for token in [&fixture.model, "office-bootstrap", &fixture.foreign] {
        let (status, _) = post(
            &client,
            &server,
            token,
            json!({"action":"policy-get","project":PROJECT}),
        )
        .await;
        assert_eq!(status, 403);
    }
    let (status, _) = post(
        &client,
        &server,
        &fixture.read_only,
        json!({"action":"task-create","project":PROJECT,
        "title":"Denied","instruction":"Denied","idempotency_key":"denied"}),
    )
    .await;
    assert_eq!(status, 403);
    let response = client
        .post(format!("{}/api/office/control", server.base))
        .bearer_auth(&fixture.pat)
        .header("origin", "https://foreign.example")
        .json(&json!({"action":"policy-get","project":PROJECT}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);
    let response = client
        .post(format!("{}/api/tools/call", server.base))
        .bearer_auth(&fixture.pat)
        .json(&json!({"tool":"team_policy_get","params":{"project":PROJECT}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
    drop(client);
    server.close().await;
}

#[tokio::test]
#[ignore = "requires explicit WEBCODEX_TEST_OFFICE_CLI pointing to the compiled human CLI; run for bridge acceptance"]
async fn office_cli_real_server_bridge_uses_real_pat_and_preserves_task_receipt() {
    use tokio::io::AsyncWriteExt;
    let _env = crate::auth::AuthEnvGuard::auth_required();
    let binary = std::path::PathBuf::from(
        std::env::var_os("WEBCODEX_TEST_OFFICE_CLI").expect("compiled CLI path is required"),
    );
    assert!(binary.is_absolute() && binary.is_file());
    let fixture = Fixture::new().await;
    let server = RunningServer::start(Service::new(fixture.service.router())).await;
    let config = fixture.temp.path().join("runner.toml");
    let token_file = fixture.temp.path().join("user-token");
    std::fs::write(
        &config,
        format!(
            "server_url={:?}\nclient_id=\"runner\"\ntoken=\"not-the-human-pat\"\n",
            server.base
        ),
    )
    .unwrap();
    std::fs::write(&token_file, &fixture.pat).unwrap();
    let mut command = tokio::process::Command::new(&binary);
    command
        .args(["office", "task-create", "--config"])
        .arg(&config)
        .args([
            "--server-url",
            &server.base,
            "--client-id",
            "runner",
            "--token-file",
        ])
        .arg(&token_file)
        .arg("--json")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(json!({"project":PROJECT,"title":"Actual CLI root","instruction":"Public CLI brief","idempotency_key":"actual-cli"}).to_string().as_bytes()).await.unwrap();
    stdin.shutdown().await.unwrap();
    drop(stdin);
    let result = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "Office CLI failed; credentials are intentionally not printed"
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["success"], true);
    assert_eq!(value["output"]["created"], true);
    assert_eq!(
        value["output"]["task"]["summary"]["referenced_project_id"],
        PROJECT
    );
    assert!(!String::from_utf8_lossy(&result.stdout).contains(&fixture.pat));
    assert_eq!(
        fixture
            .db
            .conn_for_tests()
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_tasks WHERE referenced_project_id=?1",
                [PROJECT],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    server.close().await;
}
