use super::*;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};
use serde_json::Value;
use std::sync::Arc;

#[test]
fn team_decision_schema_scope_and_parser_keep_identity_server_owned() {
    let spec = crate::agent_team_decision::tool_spec();
    assert!(agent_team::supports("team_decision"));
    assert!(!agent_team::supports_execution("team_decision"));
    let variants = spec["inputSchema"]["oneOf"].as_array().unwrap();
    assert_eq!(variants.len(), 5);
    for variant in variants {
        assert_eq!(variant["additionalProperties"], false);
        for forbidden in [
            "agent_id",
            "owner_user",
            "run_id",
            "project",
            "weight",
            "votes",
        ] {
            assert!(variant["properties"].get(forbidden).is_none());
        }
    }
    assert_eq!(
        crate::agent_host_auth::tool_scope("team_decision", &json!({"action":"read"})),
        "project:read"
    );
    assert_eq!(
        crate::agent_host_auth::tool_scope("team_decision", &json!({"action":"review"})),
        "project:write"
    );
    let f = team_fixture();
    let request = json!({"action":"open","root_task_id":f.task,"question":"A fact","context":"Current build","kind":"fact","idempotency_key":"open"});
    let result = agent_team::call(&f.db, &f.run, "team_decision", request.clone());
    assert!(result.success);
    let mut forged = request;
    forged["agent_id"] = json!("forged");
    assert_eq!(
        agent_team::call(&f.db, &f.run, "team_decision", forged)
            .error
            .as_deref(),
        Some("agent_team_invalid_input")
    );
    let assignment = crate::agent_enrollment::assignment(&f.db, &f.run, &json!({}));
    assert!(assignment.output["evidence_review_guidance"]
        .as_str()
        .unwrap()
        .contains("ordinary separate implementation tasks"));
}

async fn call(service: &Service, token: &str, mcp: bool, tool: &str, args: Value) -> Value {
    let mut request = TestClient::post(if mcp {
        "http://localhost/mcp"
    } else {
        "http://localhost/api/tools/call"
    })
    .bearer_auth(token);
    let body = if mcp {
        request = request
            .add_header("mcp-protocol-version", "2026-07-28", true)
            .add_header("mcp-method", "tools/call", true)
            .add_header("mcp-name", tool, true);
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args,"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}})
    } else {
        json!({"tool":tool,"params":args})
    };
    let mut response = request.json(&body).send(service).await;
    let status = response.status_code.unwrap_or(StatusCode::OK);
    let body: Value = response.take_json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    if mcp {
        body["result"]["structuredContent"].clone()
    } else {
        body
    }
}

#[tokio::test]
async fn team_decision_http_mcp_share_review_state_and_exact_replay() {
    let f = team_fixture();
    let pending = f.db.begin_agent_run_handoff(&f.run).unwrap();
    f.db.activate_agent_run(
        "synthetic-owner",
        "agent:runner:repo",
        f.run.agent_id(),
        &pending.run_id,
    )
    .unwrap();
    let db = Arc::new(f.db);
    let mut config = crate::test_support::test_config(Some("synthetic-bootstrap"));
    Arc::get_mut(&mut config).unwrap().data_dir = f._temp.path().to_path_buf();
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
            )
            .push(
                Router::with_path("api/tools/call")
                    .hoop(crate::AuthMiddleware)
                    .post(crate::runtime_http::tools_call),
            ),
    );
    let open = json!({"action":"open","root_task_id":f.task,"question":"Supports X","context":"Version abc","kind":"fact","idempotency_key":"open"});
    let first = call(
        &service,
        &pending.token,
        false,
        "team_decision",
        open.clone(),
    )
    .await;
    assert_eq!(first["success"], true, "{first}");
    let replay = call(&service, &pending.token, true, "team_decision", open).await;
    assert_eq!(replay["output"]["summary"], first["output"]["summary"]);
    let id = first["output"]["summary"]["issue_id"].clone();
    let bad=call(&service,&pending.token,true,"team_decision",json!({"action":"conclude","root_task_id":f.task,"issue_id":id,"expected_revision":1,"status":"verified","selected_stance":"support","conclusion":"Everyone agrees","rationale":"Two agents","idempotency_key":"bad"})).await;
    assert_eq!(bad["error"], "team_decision_verification_required");
    assert_eq!(bad["output"]["state_changed"], false);
    let submitted=call(&service,&pending.token,true,"team_decision",json!({"action":"submit","root_task_id":f.task,"issue_id":id,"expected_revision":1,"stance":"support","claim":"Measured","source":{"origin":{"locator":"experiment","version":"abc"},"location":"run 1","excerpt":"Observed X"},"idempotency_key":"submit"})).await;
    assert_eq!(submitted["success"], true, "{submitted}");
    let reviewed=call(&service,&pending.token,false,"team_decision",json!({"action":"review","root_task_id":f.task,"issue_id":id,"expected_revision":2,"evidence_id":submitted["output"]["evidence_id"],"review":{"disposition":"verified","rationale":"Read original and checked scope","origin":{"locator":"experiment","version":"abc"}},"idempotency_key":"review"})).await;
    assert_eq!(reviewed["success"], true, "{reviewed}");
    let final_result=call(&service,&pending.token,true,"team_decision",json!({"action":"conclude","root_task_id":f.task,"issue_id":id,"expected_revision":3,"status":"verified","selected_stance":"support","conclusion":"X supported","rationale":"Applicable observation","idempotency_key":"conclude"})).await;
    assert_eq!(
        final_result["output"]["summary"]["current_conclusion"]["status"],
        "verified"
    );
    let read = call(
        &service,
        &pending.token,
        false,
        "team_decision",
        json!({"action":"read","root_task_id":f.task,"issue_id":id}),
    )
    .await;
    assert_eq!(read["output"]["summary"], final_result["output"]["summary"]);
    // Direct Agent bearer credentials discover through MCP tools/list. The
    // tool_manifest wrapper belongs to the separate trusted Host adapter.
    let mut discovery=TestClient::post("http://localhost/mcp")
        .bearer_auth(&pending.token)
        .add_header("mcp-protocol-version","2026-07-28",true)
        .add_header("mcp-method","tools/list",true)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}))
        .send(&service).await;
    assert_eq!(
        discovery.status_code.unwrap_or(StatusCode::OK),
        StatusCode::OK
    );
    let discovery: Value = discovery.take_json().await.unwrap();
    let spec = discovery["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "team_decision")
        .unwrap();
    assert_eq!(spec["inputSchema"]["oneOf"].as_array().unwrap().len(), 5);
}
