use super::*;
use crate::auth::{AuthContext, AuthKind, SCOPE_AGENT_RESULT};
use crate::runner_http::RunnerRegistry;
use crate::runner_protocol::RUNNER_AGENT_EXECUTION_CLOSE_RECEIPT_PATH;
use salvo::test::{ResponseExt, TestClient};
use salvo::Service;
use serde_json::json;

fn receipt_request(client_id: &str) -> serde_json::Value {
    json!({
        "request_id":"unregistered-close-request",
        "receipt":{
            "version":1,
            "close_ref":{"close_id":"close-test","source":{"agent_id":"agent-test","agent_run_id":"run-test","generation":1}},
            "target":{"client_id":client_id,"runner_instance_id":"instance-test"},
            "deadline_at_ms":123456789,
            "members_sha256":"a".repeat(64),
            "observation_revision":1,
            "admission_closed":true,
            "tombstone_persisted":true,
            "receipts":[]
        }
    })
}

fn agent_auth(client_id: &str, result_scope: bool) -> AuthContext {
    let mut auth = AuthContext::new(AuthKind::AgentToken);
    auth.token_kind = Some("agent".into());
    auth.allowed_client_id = Some(client_id.into());
    if result_scope {
        auth.scopes.push(SCOPE_AGENT_RESULT.into());
    }
    auth
}

async fn post_receipt(
    auth: Option<AuthContext>,
    body: serde_json::Value,
) -> (StatusCode, RunnerAgentExecutionCloseReceiptResponse) {
    let mut router = Router::new()
        .hoop(affix_state::inject(Arc::new(RunnerRegistry::default())))
        .push(
            Router::with_path(RUNNER_AGENT_EXECUTION_CLOSE_RECEIPT_PATH.trim_start_matches('/'))
                .post(runner_agent_execution_close_receipt),
        );
    if let Some(auth) = auth {
        router = router.hoop(affix_state::inject(auth));
    }
    let service = Service::new(router);
    let mut response = TestClient::post(&format!(
        "http://localhost{RUNNER_AGENT_EXECUTION_CLOSE_RECEIPT_PATH}"
    ))
    .json(&body)
    .send(&service)
    .await;
    let status = response.status_code.unwrap_or(StatusCode::OK);
    (status, response.take_json().await.unwrap())
}

#[tokio::test]
async fn runner_close_receipt_requires_authenticated_runner_result_authority() {
    let mut user = AuthContext::new(AuthKind::ApiToken);
    user.token_kind = Some("user".into());
    user.scopes.push(SCOPE_AGENT_RESULT.into());
    for auth in [
        None,
        Some(user),
        Some(agent_auth("runner-a", false)),
        Some(agent_auth("runner-b", true)),
    ] {
        let (status, response) = post_receipt(auth, receipt_request("runner-a")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!response.stored);
    }
}

#[tokio::test]
async fn runner_close_receipt_unknown_target_never_acknowledges_storage() {
    let (status, response) = post_receipt(
        Some(agent_auth("runner-a", true)),
        receipt_request("runner-a"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!response.stored);
    assert!(response.error.is_some());
}

#[tokio::test]
async fn runner_close_receipt_rejects_extra_identity_fields() {
    let mut body = receipt_request("runner-a");
    body["connection_id"] = json!("caller-selected-connection");
    let (status, response) = post_receipt(Some(agent_auth("runner-a", true)), body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!response.stored);
    assert_eq!(
        response.error.as_deref(),
        Some("runner_close_receipt_invalid_input")
    );
}

#[tokio::test]
async fn runner_close_receipt_requires_store_commit_before_stored_ack() {
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(crate::Database::open(&temp.path().join("close.db")).unwrap());
    for with_store in [false, true] {
        let registry = Arc::new(RunnerRegistry::default());
        let registration = serde_json::from_value(json!({
            "client_id":"runner-a",
            "agent_instance_id":"instance-test",
            "agent_protocol_generation":crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2.get(),
            "capabilities":crate::test_support::current_runner_capabilities(crate::runner_protocol::RunnerCapabilities::default())
        })).unwrap();
        registry.register(registration).await.unwrap();
        let mut auth = AuthContext::new(AuthKind::Bootstrap);
        auth.is_bootstrap = true;
        let mut router = Router::new()
            .hoop(affix_state::inject(registry))
            .hoop(affix_state::inject(auth))
            .push(
                Router::with_path(
                    RUNNER_AGENT_EXECUTION_CLOSE_RECEIPT_PATH.trim_start_matches('/'),
                )
                .post(runner_agent_execution_close_receipt),
            );
        if with_store {
            router = router.hoop(affix_state::inject(db.clone()));
        }
        let mut response = TestClient::post(&format!(
            "http://localhost{RUNNER_AGENT_EXECUTION_CLOSE_RECEIPT_PATH}"
        ))
        .json(&receipt_request("runner-a"))
        .send(&Service::new(router))
        .await;
        assert_eq!(
            response.status_code.unwrap_or(StatusCode::OK),
            if with_store {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        );
        let response: RunnerAgentExecutionCloseReceiptResponse =
            response.take_json().await.unwrap();
        assert!(!response.stored);
        assert_eq!(
            response.error.as_deref(),
            Some(if with_store {
                "runner_close_receipt_rejected"
            } else {
                "runner_close_receipt_unavailable"
            })
        );
    }
}
