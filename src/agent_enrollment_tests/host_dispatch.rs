//! Real control HTTP, authentication middleware, NativeHost MCP attach and SQLite.
//! No model, browser, process launcher, mock control response or external server.
use super::{success, Fixture, CONTROL};
use salvo::http::StatusCode;
use serde_json::{json, Value};
use webcodex_store::{
    AgentEnrollmentPolicy, AgentEnrollmentPurpose, AgentHostQueueKind, AgentMemberReceipt,
    AgentMemberRequest, CommunicationPrincipal, TeamAgentPlan, TeamPlanInput, VerifiedAgentRun,
};

const PROJECT: &str = "agent:runner:host-control";
const PROFILE: &str = "manager-selected-profile";
const CHILD_BRIEF: &str = "Execute only the assigned member work and report acceptance evidence.";

struct NativeManager {
    host_id: String,
    token: String,
    root: String,
    run: VerifiedAgentRun,
}

async fn manager(f: &Fixture, label: &str) -> NativeManager {
    let host =
        f.db.register_native_agent_host(
            &format!("tunnel_{}", uuid::Uuid::new_v4().simple()),
            &"b".repeat(64),
        )
        .unwrap();
    let host_id = host.host_id.clone();
    let token = host.into_secret();
    let mut request = f.reservation_for_project(label, &format!("manager-{label}"), PROJECT);
    request["host_id"] = json!(host_id);
    for key in [
        "oauth_client_id",
        "oauth_subject_kind",
        "oauth_subject_id",
        "host_metadata_exclusive",
    ] {
        request.as_object_mut().unwrap().remove(key);
    }
    let root = request["task_id"].as_str().unwrap().to_owned();
    let issued = f.reserve(request).await;
    let (code, attached) = f
        .rpc(
            &token,
            Some(&format!("native-manager-{label}")),
            "tools/call",
            json!({
                "name":"agent_attach", "arguments":{
                    "enrollment_id":issued["enrollment_id"],"claim_grant":issued["claim_grant"]}
            }),
        )
        .await;
    assert_eq!(code, StatusCode::OK);
    let attached = success(attached);
    let principal = f.db.native_agent_host_principal(&host_id).unwrap();
    let run =
        f.db.agent_enrollment_source_run_for_purpose(
            &principal,
            &f.user.id,
            PROJECT,
            attached["agent_id"].as_str().unwrap(),
            attached["run_id"].as_str().unwrap(),
            AgentEnrollmentPurpose::Member,
        )
        .unwrap();
    assert_eq!(attached["generation"].as_i64(), Some(run.generation()));
    assert_eq!(attached["assignment"]["task_id"], root);
    NativeManager {
        host_id,
        token,
        root,
        run,
    }
}

fn member(f: &Fixture, manager: &NativeManager) -> AgentMemberReceipt {
    let child =
        f.db.create_agent_member(
            &manager.run,
            AgentMemberRequest {
                title: "Explicit host child".into(),
                brief: CHILD_BRIEF.into(),
                task_directory: "workspace/host-control-child".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 0,
                },
            },
            "create-host-child",
        )
        .unwrap();
    f.db.register_agent_team_plan(
        &manager.run,
        &manager.root,
        0,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: manager.root.clone(),
                cli_calls: vec![],
            },
            subagents: vec![TeamAgentPlan {
                task_id: child.task_id.clone(),
                cli_calls: vec![],
            }],
        },
        "host-plan",
    )
    .unwrap();
    child
}

fn scope(f: &Fixture, manager: &NativeManager, action: &str) -> Value {
    json!({"action":action,"owner_user":f.user.id,"project":PROJECT,"host_id":manager.host_id,
        "manager_agent_id":manager.run.agent_id(),"manager_run_id":manager.run.run_id(),
        "expected_generation":manager.run.generation()})
}

fn reserve_request(f: &Fixture, manager: &NativeManager, child: &AgentMemberReceipt) -> Value {
    let mut request = scope(f, manager, "agent_host_member_reserve");
    request["root_task_id"] = json!(manager.root);
    request["child_task_id"] = json!(child.task_id);
    request["enrollment_id"] = json!(child.enrollment_id);
    request["idempotency_key"] = json!("member-queue");
    request["thinking_profile_id"] = json!(PROFILE);
    request
}

fn queue_request(f: &Fixture, manager: &NativeManager, queue: &str, action: &str) -> Value {
    let mut request = scope(f, manager, action);
    request["queue_kind"] = json!("member");
    request["queue_id"] = json!(queue);
    request
}

async fn bind(f: &Fixture, manager: &NativeManager, child: &AgentMemberReceipt) -> Value {
    let (code, value) = f.control(CONTROL, reserve_request(f, manager, child)).await;
    assert_eq!(code, StatusCode::OK);
    assert_public(&value);
    value
}

fn assert_public(value: &Value) {
    let text = value.to_string();
    assert!(!text.contains("wc_en_"));
    assert!(!text.contains("wc_ar_"));
    assert!(value.get("claim_grant").is_none());
}

fn assert_control_denied(status: StatusCode, value: &Value, expected_error: &str) {
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value["error"], expected_error);
    assert_public(value);
}

fn unclaimed_child_snapshot(f: &Fixture, child: &AgentMemberReceipt) -> Value {
    let status =
        f.db.agent_enrollment_status(&f.user.id, PROJECT, &child.enrollment_id)
            .unwrap();
    assert_eq!(status.agent_id, child.agent_id);
    assert_eq!(status.task_id, child.task_id);
    assert_eq!(status.state, "issued");
    assert_eq!(status.run_state, "pending");
    assert_eq!(status.delivery_state, "delivery_unknown");
    assert!(!status.claim_expired);
    assert!(status.claimed_at_unix_ms.is_none());
    assert!(status.window_binding_ref.is_none());
    assert!(!status.window_enabled);
    // The fixture connection is released before the next HTTP await. These
    // exact child rows must remain unbound even if an error response is lost.
    {
        let conn = f.db.conn_for_tests();
        let active_assignments: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_enrollment_assignments WHERE owner_user=?1 AND project=?2 AND agent_id=?3 AND task_id=?4 AND state='active'",
                [f.user.id.as_str(), PROJECT, child.agent_id.as_str(), child.task_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let enabled_windows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_host_windows WHERE run_id=?1 AND enabled=1",
                [status.run_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_assignments, 0);
        assert_eq!(enabled_windows, 0);
    }
    let mut snapshot = serde_json::to_value(status).unwrap();
    // claim_expired is computed from wall time; the assertions above separately
    // ensure expiry did not cause the rejection instead of cancellation.
    snapshot.as_object_mut().unwrap().remove("claim_expired");
    snapshot
}

fn owner(f: &Fixture) -> CommunicationPrincipal {
    // Same deterministic owner attribution already used by Fixture.reservation;
    // this is Store fixture setup, never an injected production AuthContext.
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"webcodex.communication.principal.v1\0managed-user\0");
    hash.update(f.user.id.as_bytes());
    CommunicationPrincipal {
        kind: "managed-user".into(),
        digest: format!("wc_commprincipal_{:x}", hash.finalize()),
    }
}

#[tokio::test]
async fn host_dispatch_http_prepare_read_claim_replay_and_native_attach_are_exact() {
    let f = Fixture::new();
    let manager = manager(&f, "first").await;
    let child = member(&f, &manager);
    let reserved = bind(&f, &manager, &child).await;
    let queue = reserved["binding"]["queue_id"].as_str().unwrap();
    assert_eq!(
        bind(&f, &manager, &child).await["binding"],
        reserved["binding"]
    );
    for action in ["agent_host_dispatch_prepare", "agent_host_dispatch_read"] {
        let (code, value) = f
            .control(CONTROL, queue_request(&f, &manager, queue, action))
            .await;
        assert_eq!(code, StatusCode::OK);
        assert_public(&value);
        assert_eq!(value["instruction"], CHILD_BRIEF);
        assert_eq!(value["source_task_id"], child.task_id);
        assert_eq!(value["binding"]["root_task_id"], manager.root);
        assert_eq!(value["binding"]["task_id"], child.task_id);
        assert_eq!(value["binding"]["thinking_profile_id"], PROFILE);
        assert_eq!(value["queue_state"], "queued");
        assert_eq!(value["delivery_state"], "reserved");
        assert_eq!(value["dispatch_count"], 0);
        assert!(value["dispatch_attempt_id"].is_null());
    }
    let request = queue_request(&f, &manager, queue, "agent_host_dispatch_claim");
    let (code, first) = f.control(CONTROL, request.clone()).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(first["dispatch_permitted"], true);
    let grant = first["claim_grant"].as_str().unwrap();
    assert!(grant.starts_with("wc_en_"));
    assert!(first["expires_at_unix_ms"].as_i64().unwrap() > chrono::Utc::now().timestamp_millis());
    assert_public(&first["status"]);
    assert_eq!(first["status"]["delivery_state"], "delivery_unknown");
    assert_eq!(first["status"]["dispatch_count"], 1);
    let (code, repeated) = f.control(CONTROL, request).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(repeated["dispatch_permitted"], false);
    assert!(repeated["claim_grant"].is_null() && repeated["expires_at_unix_ms"].is_null());
    assert_eq!(
        first["status"]["dispatch_attempt_id"],
        repeated["status"]["dispatch_attempt_id"]
    );
    assert!(!repeated.to_string().contains(grant));
    let (code, attached) = f.rpc(&manager.token, Some("fresh-member-window"), "tools/call", json!({
        "name":"agent_attach","arguments":{"enrollment_id":child.enrollment_id,"claim_grant":grant}
    })).await;
    assert_eq!(code, StatusCode::OK);
    let attached = success(attached);
    assert_eq!(attached["agent_id"], child.agent_id);
    assert_eq!(attached["assignment"]["task_id"], child.task_id);
    let (code, observed) = f
        .control(
            CONTROL,
            queue_request(&f, &manager, queue, "agent_host_dispatch_read"),
        )
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_public(&observed);
    assert_eq!(observed["delivery_state"], "claimed");
    assert_eq!(observed["enrollment_status"]["run_id"], attached["run_id"]);
    assert_eq!(
        observed["enrollment_status"]["generation"],
        attached["generation"]
    );
    assert_eq!(observed["enrollment_status"]["window_enabled"], true);
    assert_eq!(observed["business_verified"], false);
}

#[tokio::test]
async fn host_dispatch_http_rejects_wrong_root_generation_and_real_nonbootstrap_credentials() {
    let f = Fixture::new();
    let first = manager(&f, "scope-one").await;
    let child = member(&f, &first);
    let reserved = bind(&f, &first, &child).await;
    let queue = reserved["binding"]["queue_id"].as_str().unwrap();
    let queue_before =
        f.db.read_agent_host_dispatch(&first.run, AgentHostQueueKind::Member, queue)
            .unwrap();
    let other = manager(&f, "scope-two").await;
    let mut wrong_root = reserve_request(&f, &first, &child);
    wrong_root["root_task_id"] = json!(other.root);
    let (code, error) = f.control(CONTROL, wrong_root).await;
    assert_control_denied(code, &error, "agent_enrollment_denied");
    for action in [
        "agent_host_dispatch_prepare",
        "agent_host_dispatch_read",
        "agent_host_dispatch_claim",
    ] {
        let mut wrong_generation = queue_request(&f, &first, queue, action);
        wrong_generation["expected_generation"] = json!(first.run.generation() + 1);
        let (code, error) = f.control(CONTROL, wrong_generation).await;
        assert_control_denied(code, &error, "agent_host_dispatch_generation_mismatch");
        let (code, error) = f
            .control(CONTROL, queue_request(&f, &other, queue, action))
            .await;
        assert_control_denied(code, &error, "agent_host_dispatch_denied");
    }
    let pat = crate::auth::generate_api_token();
    f.db.insert_api_key(
        &crate::models::ApiKeyRecord {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: f.user.id.clone(),
            name: "synthetic-host-control-pat".into(),
            key_prefix: crate::auth::token_prefix(&pat),
            created_at: chrono::Utc::now().timestamp(),
            last_used_at: None,
            revoked_at: None,
            scopes: "admin".into(),
            expires_at: None,
            kind: crate::models::TOKEN_KIND_USER.into(),
            allowed_client_id: None,
        },
        &crate::auth::hash_token(&pat),
    )
    .unwrap();
    let model =
        f.db.create_isolated_agent(&f.user.id, PROJECT, None)
            .unwrap();
    f.db.authenticate_agent_run(&model.token).unwrap();
    for (token, expected_error) in [
        (&f.oauth, "insufficient_scope"),
        (&first.token, "native_host_credential_audience_denied"),
        (&pat, "route requires bootstrap authority"),
        (&model.token, "agent_credential_audience_denied"),
    ] {
        let (code, value) = f
            .control(
                token,
                queue_request(&f, &first, queue, "agent_host_dispatch_claim"),
            )
            .await;
        assert_control_denied(code, &value, expected_error);
        if expected_error == "insufficient_scope" {
            assert_eq!(
                value["error_description"],
                "route requires bootstrap authority"
            );
        }
        if expected_error == "route requires bootstrap authority" {
            assert_eq!(value["status"], 403);
        }
    }
    let (code, unchanged) = f
        .control(
            CONTROL,
            queue_request(&f, &first, queue, "agent_host_dispatch_read"),
        )
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(unchanged["queue_state"], "queued");
    assert_eq!(unchanged["dispatch_count"], 0);
    assert_eq!(unchanged["delivery_state"], "reserved");
    assert_eq!(
        serde_json::to_value(
            f.db.read_agent_host_dispatch(&first.run, AgentHostQueueKind::Member, queue)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(queue_before).unwrap()
    );
}

#[tokio::test]
async fn host_dispatch_http_cancel_blocks_new_delivery_and_rejects_unredeemed_grant() {
    for issue_first in [false, true] {
        let f = Fixture::new();
        let manager = manager(&f, "cancel").await;
        let child = member(&f, &manager);
        let reserved = bind(&f, &manager, &child).await;
        let queue = reserved["binding"]["queue_id"].as_str().unwrap();
        let request = queue_request(&f, &manager, queue, "agent_host_dispatch_claim");
        let first = if issue_first {
            let (code, issued) = f.control(CONTROL, request.clone()).await;
            assert_eq!(code, StatusCode::OK);
            assert_eq!(issued["dispatch_permitted"], true);
            Some(issued)
        } else {
            None
        };
        f.db.cancel_root_team_agents(&owner(&f), PROJECT, &manager.root)
            .unwrap();
        let (code, cancelled) = f.control(CONTROL, request).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(cancelled["dispatch_permitted"], false);
        assert!(cancelled["claim_grant"].is_null());
        assert!(cancelled["expires_at_unix_ms"].is_null());
        assert_eq!(cancelled["status"]["cancel_requested"], true);
        assert_eq!(cancelled["status"]["business_verified"], false);
        if let Some(first) = first {
            assert_eq!(
                cancelled["status"]["dispatch_attempt_id"],
                first["status"]["dispatch_attempt_id"]
            );
            assert_eq!(cancelled["status"]["dispatch_count"], 1);
            let before = unclaimed_child_snapshot(&f, &child);
            let (code, response) = f.rpc(&manager.token, Some("cancelled-child-window"), "tools/call", json!({
                "name":"agent_attach","arguments":{"enrollment_id":child.enrollment_id,"claim_grant":first["claim_grant"]}
            })).await;
            // MCP transports a tool-level authorization denial with HTTP 200;
            // an RPC internal error or 5xx must not satisfy this assertion.
            assert_eq!(code, StatusCode::OK);
            assert!(response.get("error").is_none());
            assert_eq!(response["result"]["isError"], true);
            let denied = &response["result"]["structuredContent"];
            assert_eq!(denied["success"], false);
            assert_eq!(denied["error"], "agent_host_enrollment_claim_denied");
            assert_eq!(
                denied["output"]["error_kind"],
                "agent_host_enrollment_claim_denied"
            );
            assert_eq!(denied["output"]["state_changed"], false);
            assert_eq!(denied["output"]["dispatch_certainty"], "not_started");
            assert_public(&response);
            assert_eq!(unclaimed_child_snapshot(&f, &child), before);
        } else {
            assert_eq!(cancelled["status"]["queue_state"], "cancelled");
            assert_eq!(cancelled["status"]["dispatch_count"], 0);
        }
    }
}

#[tokio::test]
async fn host_dispatch_http_original_queue_receipts_preserve_unknown_and_reject_stale_fences() {
    let f = Fixture::new();
    let manager = manager(&f, "receipt").await;
    let child = member(&f, &manager);
    let reserved = bind(&f, &manager, &child).await;
    let queue = reserved["binding"]["queue_id"].as_str().unwrap();
    let (code, claim) = f
        .control(
            CONTROL,
            queue_request(&f, &manager, queue, "agent_host_dispatch_claim"),
        )
        .await;
    assert_eq!(code, StatusCode::OK);
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut receipt = json!({"action":"team_agent_record","owner_user":f.user.id,"project":PROJECT,
        "team_agent_attempt_id":queue,"dispatch_attempt_id":claim["status"]["dispatch_attempt_id"],
        "request_id":request_id,"state":"unknown","reason":"synthetic-response-lost","response_observed":false});
    let (code, recorded) = f.control(CONTROL, receipt.clone()).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(recorded["recorded"], true);
    let before =
        f.db.read_agent_host_dispatch(&manager.run, AgentHostQueueKind::Member, queue)
            .unwrap();
    assert_eq!(before.queue_state, "unknown");
    assert_eq!(before.request_id.as_deref(), Some(request_id.as_str()));
    receipt["dispatch_attempt_id"] = json!(uuid::Uuid::new_v4().to_string());
    let (code, error) = f.control(CONTROL, receipt.clone()).await;
    assert_control_denied(code, &error, "team_agent_record_rejected");
    let after =
        f.db.read_agent_host_dispatch(&manager.run, AgentHostQueueKind::Member, queue)
            .unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    let (code, replayed) = f
        .control(
            CONTROL,
            queue_request(&f, &manager, queue, "agent_host_dispatch_claim"),
        )
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(replayed["dispatch_permitted"], false);
    assert!(replayed["claim_grant"].is_null());
    receipt["dispatch_attempt_id"] = claim["status"]["dispatch_attempt_id"].clone();
    receipt["state"] = json!("dispatched");
    receipt["response_observed"] = json!(true);
    let (code, candidate) = f.control(CONTROL, receipt).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(candidate["attempt"]["business_verified"], false);
    assert_eq!(
        f.db.read_agent_team_task(&manager.run, &manager.root)
            .unwrap()
            .state,
        "ready"
    );
}
