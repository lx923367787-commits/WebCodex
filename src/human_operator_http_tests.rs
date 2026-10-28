//! Real Salvo AuthMiddleware, production handlers, SQLite credentials and
//! RunnerRegistry. The Runner transport peer supplies controlled terminal
//! frames through Registry::complete; HTTP responses are never stubbed. These
//! tests cover HTTP authority, routing effects and the config CAS wire/result
//! contract, not the Runner process's separate TOML reload implementation.
use crate::runner_protocol::{
    RunnerCapabilities, RunnerConfigAction, RunnerConfigExecutionState,
    RunnerConfigOperationRequest, RunnerConfigOperationResponse, RunnerPollRequest,
    RunnerProjectSummary, RunnerRegisterRequest, RunnerRequest, RunnerResultRequest,
    RUNNER_CONFIG_REQUEST_KIND,
};
use crate::test_support::{
    apply_project_inventory_snapshot, current_runner_registration, seed_oauth_client_named,
    seed_user, test_config_oauth2,
};
use crate::tool_runtime::permissions::{AuthorityMode, PermissionEvaluator};
use crate::tool_runtime::ToolRuntime;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const RUNNER: &str = "human-operator-runner";
const FOREIGN: &str = "foreign-operator-runner";
const INSTANCE: &str = "human-operator-instance";
const PROJECT: &str = "agent:human-operator-runner:owned-repo";
const FULL_SCOPES: &str = "runtime:read project:read project:write runner:manage";

fn mint_pat(db: &crate::Database, user: &crate::models::UserRecord, scopes: &str) -> String {
    let token = crate::auth::generate_api_token();
    db.insert_api_key(
        &crate::models::ApiKeyRecord {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: user.id.clone(),
            name: "synthetic-human-operator".into(),
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

fn registration(client_id: &str, owner: &str) -> RunnerRegisterRequest {
    current_runner_registration(RunnerRegisterRequest {
        agent_execution_resources: Vec::new(),
        client_id: client_id.into(),
        runner_instance_id: INSTANCE.into(),
        runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
        display_name: None,
        owner: Some(owner.into()),
        hostname: None,
        host_context: None,
        capabilities: RunnerCapabilities {
            runner_config_control: true,
            tool_connections: false,
            ..Default::default()
        },
        policy: None,
        process_started_at: None,
        build: None,
        job_concurrency_limit: None,
        job_inventory: None,
        coding_agent_providers: None,
        coding_agent_inventory: None,
    })
}

fn summary(id: &str, path: &std::path::Path) -> RunnerProjectSummary {
    RunnerProjectSummary {
        id: id.into(),
        name: Some(id.into()),
        path: path.to_string_lossy().into_owned(),
        allow_patch: true,
        kind: Some("repo".into()),
        registration_source: None,
        description: None,
        hooks: vec![],
        disabled: false,
        revision: Some(format!("sha256:{}", "a".repeat(64))),
        git_branch: None,
        git_head: None,
        git_dirty: None,
        updated_at: 1,
        shell_profile: None,
    }
}

struct Fixture {
    service: Service,
    db: Arc<crate::Database>,
    registry: Arc<crate::runner_http::RunnerRegistry>,
    user: crate::models::UserRecord,
    full: String,
    model: String,
    native: String,
    oauth_host: String,
    temp: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::Database::open(&temp.path().join("state.db")).unwrap());
        let user = seed_user(&db, "human-operator-owner");
        let other = seed_user(&db, "human-operator-foreign");
        let full = mint_pat(&db, &user, FULL_SCOPES);
        let model = db
            .create_isolated_agent(&user.id, PROJECT, None)
            .unwrap()
            .token;
        let binding = db.authenticate_agent_run(&model).unwrap();
        let native = db
            .register_native_agent_host("tunnel_0123456789abcdef0123456789abcdef", &"a".repeat(64))
            .unwrap()
            .into_secret();
        let (client, _) = seed_oauth_client_named(&db, &user, "Synthetic operator boundary host");
        let oauth_host = crate::auth::generate_oauth_access_token();
        let now = chrono::Utc::now().timestamp();
        db.insert_oauth_access_token(&crate::models::OAuthAccessTokenRecord {
            id: uuid::Uuid::new_v4().to_string(),
            token_hash: crate::auth::hash_token(&oauth_host),
            client_id: client.client_id.clone(),
            subject_kind: "managed_user".into(),
            subject_id: user.id.clone(),
            user_id: Some(user.id.clone()),
            scopes: FULL_SCOPES.into(),
            resource: None,
            shared_key_hash: None,
            created_at: now,
            expires_at: now + 3600,
            revoked_at: None,
            last_used_at: None,
        })
        .unwrap();
        let principal =
            crate::agent_host_auth::principal(&client.client_id, "managed_user", &user.id).unwrap();
        db.register_agent_host_window(&principal, &"b".repeat(64), &binding)
            .unwrap();

        let registry = Arc::new(crate::runner_http::RunnerRegistry::default());
        for (client_id, owner, project) in [
            (RUNNER, user.username.as_str(), "owned-repo"),
            (FOREIGN, other.username.as_str(), "foreign-repo"),
        ] {
            let path = temp.path().join(project);
            std::fs::create_dir(&path).unwrap();
            registry
                .register(registration(client_id, owner))
                .await
                .unwrap();
            apply_project_inventory_snapshot(
                &registry,
                client_id,
                INSTANCE,
                vec![summary(project, &path)],
            )
            .await;
        }
        let runtime = Arc::new(
            ToolRuntime::new_for_tests_with_runner_registry(registry.clone())
                .with_communication_database(db.clone())
                .with_permission_evaluator(PermissionEvaluator::with_mode(
                    AuthorityMode::TrustedAgent,
                )),
        );
        let mut config = test_config_oauth2(Some("synthetic-operator-bootstrap"));
        Arc::get_mut(&mut config).unwrap().data_dir = temp.path().into();
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
                )
                .push(
                    Router::with_path("api")
                        .hoop(crate::AuthMiddleware)
                        .push(
                            Router::with_path("projects/list")
                                .post(crate::runtime_http::projects_list),
                        )
                        .push(
                            Router::with_path("projects/resolve-or-register")
                                .post(crate::runtime_http::projects_resolve_or_register),
                        )
                        .push(
                            Router::with_path("runners/config/check")
                                .post(crate::runtime_http::runner_config_check),
                        )
                        .push(
                            Router::with_path("runners/config/reload")
                                .post(crate::runtime_http::runner_config_reload),
                        )
                        .push(
                            Router::with_path("runtime/status")
                                .post(crate::runtime_http::runtime_status),
                        )
                        .push(
                            Router::with_path("tools/call").post(crate::runtime_http::tools_call),
                        ),
                ),
        );
        Self {
            service,
            db,
            registry,
            user,
            full,
            model,
            native,
            oauth_host,
            temp,
        }
    }

    fn pat(&self, scopes: &str) -> String {
        mint_pat(&self.db, &self.user, scopes)
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let mut response = TestClient::post(format!("http://localhost{path}"))
            .bearer_auth(token)
            .json(&body)
            .send(&self.service)
            .await;
        (
            response.status_code.unwrap_or(StatusCode::OK),
            response.take_json().await.unwrap(),
        )
    }

    async fn assert_no_dispatch(&self) {
        for client_id in [RUNNER, FOREIGN] {
            assert!(
                self.registry
                    .poll(RunnerPollRequest {
                        client_id: client_id.into(),
                        runner_instance_id: INSTANCE.into(),
                    })
                    .await
                    .unwrap()
                    .is_none(),
                "denied operation reached a Runner queue"
            );
        }
    }

    async fn wait_request(&self) -> RunnerRequest {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(request) = self
                    .registry
                    .poll(RunnerPollRequest {
                        client_id: RUNNER.into(),
                        runner_instance_id: INSTANCE.into(),
                    })
                    .await
                    .unwrap()
                {
                    return request;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("authorized HTTP operation never reached the real RunnerRegistry")
    }

    async fn complete(&self, request: &RunnerRequest, value: &Value) {
        self.registry
            .complete(RunnerResultRequest {
                client_id: RUNNER.into(),
                runner_instance_id: INSTANCE.into(),
                request_id: request.request_id.clone(),
                exit_code: Some(0),
                stdout: Some(value.to_string()),
                stderr: None,
                duration_ms: Some(1),
                error: None,
            })
            .await
            .unwrap();
    }

    async fn config_exchange(
        &self,
        token: &str,
        action: RunnerConfigAction,
        expected: Option<u64>,
        terminal: RunnerConfigOperationResponse,
    ) -> (StatusCode, Value) {
        terminal.validate().unwrap();
        let path = match action {
            RunnerConfigAction::Check => "/api/runners/config/check",
            RunnerConfigAction::Reload => "/api/runners/config/reload",
        };
        let body = match expected {
            Some(value) => json!({"client_id":RUNNER,"expected_generation":value}),
            None => json!({"client_id":RUNNER}),
        };
        let peer = async {
            let request = self.wait_request().await;
            assert_eq!(request.kind, RUNNER_CONFIG_REQUEST_KIND);
            assert_eq!(request.client_id, RUNNER);
            assert!(
                request.command.is_empty()
                    && request.cwd.is_none()
                    && request.path.is_none()
                    && request.stdin.is_none()
            );
            let operation: RunnerConfigOperationRequest =
                serde_json::from_str(request.content.as_deref().unwrap()).unwrap();
            assert_eq!(operation.action, action);
            assert_eq!(operation.expected_generation, expected);
            self.complete(&request, &serde_json::to_value(terminal).unwrap())
                .await;
        };
        let (response, ()) = tokio::join!(self.post(token, path, body), peer);
        self.assert_no_dispatch().await;
        response
    }
}

fn completed(action: RunnerConfigAction, generation: u64) -> RunnerConfigOperationResponse {
    RunnerConfigOperationResponse {
        action,
        execution_state: RunnerConfigExecutionState::Completed,
        valid: Some(true),
        current_generation: Some(generation),
        error_code: None,
        error_field: None,
        error_reason: None,
        restart_required: false,
        restart_required_fields: vec![],
    }
}

#[tokio::test]
async fn human_operator_exact_read_scopes_return_owned_registry_state() {
    let f = Fixture::new().await;
    let (status, list) = f
        .post(&f.pat("project:read"), "/api/projects/list", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["success"], true);
    assert_eq!(list["output"]["operator_user_id"], f.user.id);
    let projects = list["output"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["id"], PROJECT);
    assert!(!list.to_string().contains("foreign-repo"));
    let (status, runtime) = f
        .post(&f.pat("runtime:read"), "/api/runtime/status", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{runtime}");
    assert_eq!(runtime["success"], true);
    assert_eq!(runtime["output"]["service"], "webcodex");
    assert_eq!(runtime["output"]["projects"]["count"], 1);
    let clients = runtime["output"]["agents"]["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["client_id"], RUNNER);
    assert!(!runtime.to_string().contains(FOREIGN));
    f.assert_no_dispatch().await;
}

#[tokio::test]
async fn human_operator_resolve_commits_exact_project_into_real_registry() {
    let f = Fixture::new().await;
    let path = f.temp.path().join("newly-resolved");
    std::fs::create_dir(&path).unwrap();
    let path = path.to_string_lossy().into_owned();
    let token = f.pat("project:write");
    let peer = async {
        let request = f.wait_request().await;
        assert_eq!(request.kind, "resolve_or_register_project");
        assert_eq!(request.client_id, RUNNER);
        let payload: Value = serde_json::from_str(request.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({"path":path}));
        f.complete(&request, &json!({
            "id":"agent:human-operator-runner:newly-resolved","agent_project_id":"newly-resolved",
            "client_id":RUNNER,"name":"newly-resolved","path":path,"kind":"auto_registered",
            "registration_source":"auto_registered","allow_patch":true,"disabled":false,
            "revision":format!("sha256:{}","c".repeat(64)),"outcome":"registered",
            "registered":true,"created_config":true,"changed":true,"recovered":false
        })).await;
    };
    let (response, ()) = tokio::join!(
        f.post(
            &token,
            "/api/projects/resolve-or-register",
            json!({"client_id":RUNNER,"path":path})
        ),
        peer
    );
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    assert_eq!(response.1["success"], true);
    assert_eq!(response.1["output"]["registered"], true);
    let (_, list) = f
        .post(&f.pat("project:read"), "/api/projects/list", json!({}))
        .await;
    let projects = list["output"]["projects"].as_array().unwrap();
    assert_eq!(
        projects.len(),
        2,
        "Server routing projection was not committed"
    );
    let added = projects
        .iter()
        .find(|p| p["id"] == "agent:human-operator-runner:newly-resolved")
        .unwrap();
    assert_eq!(added["path"], path);
    assert!(!list.to_string().contains(FOREIGN));
    f.assert_no_dispatch().await;
}

#[tokio::test]
async fn human_operator_config_scopes_and_generation_cas_contract_are_preserved() {
    let f = Fixture::new().await;
    let (status, check) = f
        .config_exchange(
            &f.pat("runtime:read"),
            RunnerConfigAction::Check,
            None,
            completed(RunnerConfigAction::Check, 7),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{check}");
    assert_eq!(check["success"], true);
    assert_eq!(check["output"]["current_generation"], 7);
    let manage = f.pat("runner:manage");
    let (status, reload) = f
        .config_exchange(
            &manage,
            RunnerConfigAction::Reload,
            Some(7),
            completed(RunnerConfigAction::Reload, 8),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reload}");
    assert_eq!(reload["success"], true);
    assert_eq!(reload["output"]["current_generation"], 8);
    let stale = RunnerConfigOperationResponse {
        action: RunnerConfigAction::Reload,
        execution_state: RunnerConfigExecutionState::NotStarted,
        valid: None,
        current_generation: Some(8),
        error_code: Some("config_generation_conflict".into()),
        error_field: None,
        error_reason: None,
        restart_required: false,
        restart_required_fields: vec![],
    };
    let (_, stale) = f
        .config_exchange(&manage, RunnerConfigAction::Reload, Some(7), stale)
        .await;
    assert_eq!(stale["success"], false);
    assert_eq!(stale["output"]["execution_state"], "not_started");
    assert_eq!(stale["output"]["error_code"], "config_generation_conflict");
    assert_eq!(stale["output"]["current_generation"], 8);
    let (_, invalid) = f
        .config_exchange(
            &manage,
            RunnerConfigAction::Reload,
            Some(8),
            completed(RunnerConfigAction::Reload, 8),
        )
        .await;
    assert_eq!(
        invalid["success"], false,
        "same-generation success must not be accepted"
    );
    assert_eq!(invalid["output"]["error_code"], "invalid_runner_response");
    assert_eq!(invalid["output"]["execution_state"], "outcome_unknown");
}

#[tokio::test]
async fn human_operator_missing_scopes_and_invalid_cas_never_dispatch() {
    let f = Fixture::new().await;
    for (path, required, body) in [
        ("/api/projects/list", "project:read", json!({})),
        (
            "/api/projects/resolve-or-register",
            "project:write",
            json!({"client_id":RUNNER,"path":f.temp.path().to_string_lossy()}),
        ),
        (
            "/api/runners/config/check",
            "runtime:read",
            json!({"client_id":RUNNER}),
        ),
        (
            "/api/runners/config/reload",
            "runner:manage",
            json!({"client_id":RUNNER,"expected_generation":1}),
        ),
        ("/api/runtime/status", "runtime:read", json!({})),
    ] {
        let remaining = FULL_SCOPES
            .split_whitespace()
            .filter(|scope| *scope != required)
            .collect::<Vec<_>>()
            .join(" ");
        let (status, response) = f.post(&f.pat(&remaining), path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {response}");
        assert_ne!(response["success"], true);
        f.assert_no_dispatch().await;
    }
    for body in [
        json!({"client_id":RUNNER,"expected_generation":0}),
        json!({"client_id":RUNNER}),
        json!({"client_id":RUNNER,"expected_generation":1,"config_path":"untrusted.toml"}),
    ] {
        let (status, response) = f.post(&f.full, "/api/runners/config/reload", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert_ne!(response["success"], true);
        f.assert_no_dispatch().await;
    }
}

#[tokio::test]
async fn human_operator_foreign_runner_denials_leave_both_queues_empty() {
    let f = Fixture::new().await;
    for (path, body) in [
        (
            "/api/projects/resolve-or-register",
            json!({"client_id":FOREIGN,"path":f.temp.path().to_string_lossy()}),
        ),
        ("/api/runners/config/check", json!({"client_id":FOREIGN})),
        (
            "/api/runners/config/reload",
            json!({"client_id":FOREIGN,"expected_generation":1}),
        ),
    ] {
        let (status, response) = f.post(&f.full, path, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {response}");
        assert_eq!(response["success"], false);
        if path.contains("/config/") {
            assert_eq!(response["output"]["error_code"], "runner_unavailable");
            assert_eq!(response["output"]["execution_state"], "not_started");
        }
        assert!(!response.to_string().contains("foreign-repo"));
        f.assert_no_dispatch().await;
    }
    let (status, response) = f
        .post(&f.full, "/api/projects/list", json!({"client_id":FOREIGN}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["output"]["count"], 0);
    assert_eq!(response["output"]["projects"], json!([]));
}

#[tokio::test]
async fn human_operator_exception_does_not_admit_model_run_native_oauth_or_bootstrap() {
    let f = Fixture::new().await;
    for token in [
        f.model.as_str(),
        f.native.as_str(),
        f.oauth_host.as_str(),
        "synthetic-operator-bootstrap",
    ] {
        for (path, body) in [
            ("/api/projects/list", json!({})),
            (
                "/api/projects/resolve-or-register",
                json!({"client_id":RUNNER,"path":f.temp.path().to_string_lossy()}),
            ),
            ("/api/runners/config/check", json!({"client_id":RUNNER})),
            (
                "/api/runners/config/reload",
                json!({"client_id":RUNNER,"expected_generation":1}),
            ),
            ("/api/runtime/status", json!({})),
        ] {
            let (status, response) = f.post(token, path, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {response}");
            assert_ne!(response["success"], true);
            assert!(!response.to_string().contains("owned-repo"));
            f.assert_no_dispatch().await;
        }
    }
    for (path, body) in [
        (
            "/api/tools/call",
            json!({"tool":"runtime_status","params":{}}),
        ),
        (
            "/mcp",
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
        ),
    ] {
        let (status, response) = f.post(&f.full, path, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {response}");
        assert_eq!(response["error"], "agent_identity_required");
        assert_ne!(response["success"], true);
        f.assert_no_dispatch().await;
    }
}
