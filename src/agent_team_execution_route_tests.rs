use super::*;
use crate::auth::AuthKind;
use serde_json::json;

struct RouteFixture {
    _temp: tempfile::TempDir,
    db: Arc<Database>,
    runtime: ToolRuntime,
    auth: AuthContext,
}

impl RouteFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp.path().join("state.db")).unwrap());
        let grant = db
            .create_isolated_agent("route-owner", "agent:route-runner:project", None)
            .unwrap();
        let mut auth = AuthContext::new(AuthKind::ModelAgent);
        auth.agent_run = Some(db.authenticate_agent_run(&grant.token).unwrap());
        let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
        Self {
            _temp: temp,
            db,
            runtime,
            auth,
        }
    }

    fn binding(&self) -> &VerifiedAgentRun {
        self.auth.agent_run.as_ref().unwrap()
    }

    fn samples(&self) -> i64 {
        self.db
            .conn_for_tests()
            .query_row(
                "SELECT samples FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2",
                rusqlite::params![self.binding().host_goal_owner_key(), self.binding().run_id()],
                |row| row.get(0),
            )
            .unwrap()
    }
}

#[tokio::test]
async fn agent_team_execution_route_non_execution_keeps_sync_identity_and_one_meter_sample() {
    let fixture = RouteFixture::new();
    let result = fixture
        .runtime
        .isolated_model_call_async("agent_identity", json!({}), Some(&fixture.auth))
        .await;
    assert!(result.success);
    assert_eq!(result.output["agent_id"], fixture.binding().agent_id());
    assert_eq!(result.output["run_id"], fixture.binding().run_id());
    assert!(result.output["context_meter"].is_object());
    assert_eq!(fixture.samples(), 1);
}

#[tokio::test]
async fn agent_team_execution_route_requires_a_current_verified_run_before_dispatch() {
    let fixture = RouteFixture::new();
    let account = AuthContext::new(AuthKind::ApiToken);
    for auth in [None, Some(&account)] {
        let result = fixture
            .runtime
            .isolated_model_call_async("team_cli_start", json!({}), auth)
            .await;
        assert!(!result.success);
        assert_eq!(result.output["error_kind"], "agent_identity_required");
    }
    let run = fixture.binding();
    fixture
        .db
        .revoke_agent_run(
            run.owner_user(),
            run.project(),
            run.agent_id(),
            run.run_id(),
        )
        .unwrap();
    let revoked = fixture
        .runtime
        .isolated_model_call_async("team_cli_start", json!({}), Some(&fixture.auth))
        .await;
    assert!(!revoked.success);
    assert_eq!(revoked.output["error_kind"], "agent_run_invalid");
}

#[tokio::test]
async fn agent_team_execution_route_applies_the_input_bound_and_counts_denial_once() {
    let fixture = RouteFixture::new();
    let result = fixture
        .runtime
        .isolated_model_call_async(
            "team_cli_start",
            json!({"instruction": "x".repeat(1024 * 1024)}),
            Some(&fixture.auth),
        )
        .await;
    assert!(!result.success);
    assert_eq!(result.output["error_kind"], "agent_input_too_large");
    assert_eq!(result.output["state_changed"], false);
    assert_eq!(fixture.samples(), 1);
}

#[test]
fn agent_team_execution_route_sync_entry_does_not_gain_async_execution_authority() {
    let fixture = RouteFixture::new();
    for name in [
        "team_cli_start",
        "team_cli_observe",
        "team_cli_close",
        "team_cli_catalog",
        "team_cli_delivery_read",
        "team_cli_delivery_review",
        "team_cli_delivery_reject_unpublished",
        "team_cli_delivery_collect",
        "team_agent_start",
    ] {
        let result = fixture
            .runtime
            .isolated_model_call(name, json!({}), Some(&fixture.auth));
        assert!(!result.success, "sync broker must not invoke {name}");
        assert_eq!(result.output["state_changed"], false);
    }
    let attempts: i64 = fixture
        .db
        .conn_for_tests()
        .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(attempts, 0);
}

#[tokio::test]
async fn agent_team_execution_route_keeps_existing_private_write_business_gate() {
    let fixture = RouteFixture::new();
    fixture
        .db
        .ensure_agent_host_context(fixture.binding(), None)
        .unwrap();
    fixture
        .db
        .conn_for_tests()
        .execute(
            "UPDATE wc_host_context_states SET state=2 WHERE owner_key=?1 AND context_epoch=?2",
            rusqlite::params![
                fixture.binding().host_goal_owner_key(),
                fixture.binding().run_id()
            ],
        )
        .unwrap();
    for (tool, arguments) in [
        (
            "agent_private",
            json!({"action":"write","path":"memory/blocked.txt","content":"blocked"}),
        ),
        (
            "team_cli_delivery_collect",
            json!({"source_attempt_id":"original-attempt"}),
        ),
    ] {
        let result = fixture
            .runtime
            .isolated_model_call_async(tool, arguments, Some(&fixture.auth))
            .await;
        assert!(!result.success);
        assert_eq!(
            result.output["error_kind"], "goal_context_draining",
            "{tool}"
        );
        assert_eq!(result.output["state_changed"], false);
    }
    assert_eq!(fixture.samples(), 0);
}

#[test]
fn agent_team_execution_route_post_effect_meter_failure_preserves_the_original_receipt() {
    let fixture = RouteFixture::new();
    let run = fixture.binding();
    fixture
        .db
        .revoke_agent_run(
            run.owner_user(),
            run.project(),
            run.agent_id(),
            run.run_id(),
        )
        .unwrap();
    let result = finish_metered_call(
        &fixture.db,
        run,
        &json!({}),
        ToolResult::ok(json!({"receipt":"already-committed"})),
    );
    assert!(result.success);
    assert_eq!(result.output["receipt"], "already-committed");
    assert!(result.output.get("context_meter").is_none());
}
