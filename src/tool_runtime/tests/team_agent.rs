use super::*;
use std::sync::Arc;
use webcodex_store::{
    models::UserRecord, CommunicationPrincipal, Database, NewAgentTask, TeamAgentPlan,
    TeamPlanInput,
};

const OWNER: &str = "owner";
const PROJECT: &str = "agent:runner:ordinary";

fn create_candidate(
    db: &Database,
    principal: &CommunicationPrincipal,
    key: &str,
) -> TeamAgentRecord {
    let task = |name: &str| {
        db.create_agent_task(
            principal,
            NewAgentTask {
                title: name.into(),
                instruction: format!("Stored instruction {name}"),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(PROJECT.into()),
                idempotency_key: format!("{key}-{name}"),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id
    };
    let root = task("root");
    let child = task("child");
    db.register_team_plan(
        principal,
        PROJECT,
        &root,
        0,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: root.clone(),
                cli_calls: vec![],
            },
            subagents: vec![TeamAgentPlan {
                task_id: child.clone(),
                cli_calls: vec![],
            }],
        },
        &format!("{key}-plan"),
    )
    .unwrap();
    db.reserve_team_agent_attempt(
        principal,
        PROJECT,
        &root,
        &child,
        &format!("{key}-spawn"),
        "manager-choice",
    )
    .unwrap()
    .record
}

struct Fixture {
    _temporary: tempfile::TempDir,
    db: Arc<Database>,
    runtime: ToolRuntime,
    principal: CommunicationPrincipal,
    record: TeamAgentRecord,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temporary.path().join("team-control.db")).unwrap());
        for (owner, disabled) in [(OWNER, 0), ("other", 0), ("disabled", 1)] {
            db.create_user(&UserRecord {
                id: owner.into(),
                username: owner.into(),
                created_at: 1,
                disabled,
                display_name: None,
                role: "user".into(),
                disabled_at: (disabled != 0).then_some(2),
                updated_at: None,
            })
            .unwrap();
        }
        let mut auth = AuthContext::new(AuthKind::ApiToken);
        auth.user_id = Some(OWNER.into());
        let principal =
            crate::tool_runtime::communication::communication_principal(Some(&auth)).unwrap();
        let record = create_candidate(&db, &principal, "initial");
        let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
        Self {
            _temporary: temporary,
            db,
            runtime,
            principal,
            record,
        }
    }

    fn request(&self, action: &str) -> Value {
        json!({"action":action,"owner_user":OWNER,"project":PROJECT,"team_agent_attempt_id":self.record.team_agent_attempt_id})
    }

    fn control(&self, request: &Value) -> Result<Value, String> {
        let mut auth = AuthContext::new(AuthKind::Bootstrap);
        auth.is_bootstrap = true;
        self.runtime.try_team_agent_control(&auth, request).unwrap()
    }

    fn stored(&self) -> TeamAgentRecord {
        self.db
            .read_team_agent_attempt(&self.principal, &self.record.team_agent_attempt_id)
            .unwrap()
    }

    fn observation(&self, claim: &Value, state: &str, reason: &str) -> Value {
        let mut request = self.request("team_agent_record");
        request["dispatch_attempt_id"] = claim["dispatch_attempt_id"].clone();
        request["request_id"] = Value::Null;
        request["state"] = json!(state);
        request["reason"] = json!(reason);
        request["response_observed"] = json!(false);
        request
    }
}

#[test]
fn ordinary_team_control_requires_explicit_bootstrap_not_a_model_token() {
    let runtime = ToolRuntime::new_for_tests();
    let request =
        json!({"action":"team_agent_list","owner_user":"owner","project":"agent:runner:repo"});
    for kind in [
        AuthKind::ApiToken,
        AuthKind::ModelAgent,
        AuthKind::Bootstrap,
    ] {
        let mut auth = AuthContext::new(kind);
        auth.is_bootstrap = kind != AuthKind::Bootstrap;
        assert_eq!(
            runtime.try_team_agent_control(&auth, &request).unwrap(),
            Err("trusted_control_required".into())
        );
    }
    let mut auth = AuthContext::new(AuthKind::Bootstrap);
    auth.is_bootstrap = true;
    assert_eq!(
        runtime.try_team_agent_control(&auth, &request).unwrap(),
        Err("team_agent_store_unavailable".into())
    );
    assert!(runtime
        .try_team_agent_control(&auth, &json!({"action":"cli_fallback_list"}))
        .is_none());
}

#[test]
fn ordinary_team_start_rejects_prompt_override_and_accepts_missing_profile() {
    let good = json!({"project":"agent:runner:repo","root_task_id":"root","source_task_id":"child","idempotency_key":"key","thinking_profile_id":"profile-explicit"});
    assert!(serde_json::from_value::<Start>(good.clone()).is_ok());
    let mut supplied = good.clone();
    supplied["prompt"] = json!("Caller text must not replace AgentTask instruction");
    assert!(serde_json::from_value::<Start>(supplied).is_err());
    let mut missing = good;
    missing
        .as_object_mut()
        .unwrap()
        .remove("thinking_profile_id");
    assert_eq!(
        serde_json::from_value::<Start>(missing)
            .unwrap()
            .thinking_profile_id,
        ""
    );
    assert!(serde_json::from_value::<ControlRequest>(json!({"action":"team_agent_acquire","owner_user":"owner","project":"agent:runner:repo","team_agent_attempt_id":"id","dispatch_permitted":true})).is_err());
}

#[test]
fn ordinary_projection_never_labels_candidate_response_as_business_verified() {
    let record = TeamAgentRecord {
        team_agent_attempt_id: "attempt".into(),
        project: "agent:runner:repo".into(),
        root_task_id: "root".into(),
        source_task_id: "child".into(),
        instruction: "private assignment".into(),
        thinking_profile_id: "manager-selection".into(),
        state: TeamAgentState::Running,
        dispatch_attempt_id: Some(uuid::Uuid::new_v4().to_string()),
        dispatch_count: 1,
        request_id: Some(uuid::Uuid::new_v4().to_string()),
        cancel_requested: false,
        response_observed: true,
        terminal_evidence: None,
        revision: 2,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 2,
    };
    let value = team_agent_projection(&record);
    assert_eq!(value["business_verified"], false);
    assert_eq!(value["counts_toward_regular_min"], true);
    assert_eq!(value["counts_toward_regular_max"], true);
    assert!(value.get("instruction").is_none());
    assert!(value.get("dispatch_attempt_id").is_none());
}

#[test]
fn ordinary_control_keeps_owner_project_fence_and_candidate_boundaries() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let principal = &fixture.principal;
    let record = &fixture.record;
    let control = |request: &Value| fixture.control(request);
    let request = |action: &str| fixture.request(action);
    let read = request("team_agent_read");
    for (field, value) in [("owner_user", "other"), ("project", "agent:runner:foreign")] {
        let mut foreign = read.clone();
        foreign[field] = json!(value);
        assert_eq!(control(&foreign), Err("team_agent_unavailable".into()));
    }
    let mut disabled = read.clone();
    disabled["owner_user"] = json!("disabled");
    assert_eq!(
        control(&disabled),
        Err("team_agent_owner_unavailable".into())
    );
    let first = control(&request("team_agent_acquire")).unwrap();
    let second = control(&request("team_agent_acquire")).unwrap();
    assert_eq!(first["dispatch_permitted"], true);
    assert_eq!(second["dispatch_permitted"], false);
    assert_eq!(first["dispatch_attempt_id"], second["dispatch_attempt_id"]);
    assert!(first["task"]
        .as_str()
        .unwrap()
        .contains("Stored instruction child"));
    let mut observed = request("team_agent_record");
    observed["dispatch_attempt_id"] = first["dispatch_attempt_id"].clone();
    observed["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    observed["state"] = json!("dispatched");
    observed["reason"] = json!("chat_state_observed");
    observed["response_observed"] = json!(true);
    let mut forged = observed.clone();
    forged["task"] = json!("caller-supplied replacement");
    assert!(control(&forged).is_err());
    assert_eq!(control(&observed).unwrap()["recorded"], true);
    let before = db
        .read_team_agent_attempt(&principal, &record.team_agent_attempt_id)
        .unwrap();
    let candidate = control(&read).unwrap();
    assert_eq!(candidate["business_verified"], false);
    assert_eq!(candidate["response_observed"], true);
    assert_eq!(
        db.read_team_agent_attempt(&principal, &record.team_agent_attempt_id)
            .unwrap(),
        before
    );
}

#[test]
fn profile_unavailable_control_requeues_only_the_proven_unstarted_dispatch() {
    for reason in [
        "discover_thinking_profiles_first",
        "thinking_profiles_expired_refresh_required",
        "thinking_profile_not_discovered",
    ] {
        let fixture = Fixture::new();
        let claim = fixture
            .control(&fixture.request("team_agent_acquire"))
            .unwrap();
        let started = fixture.stored();
        assert_eq!(started.dispatch_count, 1);
        let rejection = fixture.observation(&claim, "profile_unavailable", reason);
        let result = fixture.control(&rejection).unwrap();
        assert_eq!(result["recorded"], true);
        assert_eq!(result["attempt"]["state"], "queued");
        let queued = fixture.stored();
        assert_eq!(
            queued.team_agent_attempt_id,
            fixture.record.team_agent_attempt_id
        );
        assert_eq!(queued.source_task_id, fixture.record.source_task_id);
        assert_eq!(queued.thinking_profile_id, "manager-choice");
        assert_eq!(queued.dispatch_count, 0);
        assert!(queued.dispatch_attempt_id.is_none());
        assert!(queued.request_id.is_none());
        assert!(!queued.response_observed);
        assert!(queued.revision > started.revision);

        let resumed = fixture
            .control(&fixture.request("team_agent_acquire"))
            .unwrap();
        assert_eq!(resumed["dispatch_permitted"], true);
        assert_eq!(
            resumed["team_agent_attempt_id"],
            claim["team_agent_attempt_id"]
        );
        assert_eq!(resumed["thinking_profile_id"], claim["thinking_profile_id"]);
        assert_ne!(resumed["dispatch_attempt_id"], claim["dispatch_attempt_id"]);
        let before = fixture.stored();
        assert_eq!(before.dispatch_count, 1);
        assert!(
            fixture.control(&rejection).is_err(),
            "old fence must not release the new dispatch"
        );
        assert_eq!(fixture.stored(), before);
        let replay = fixture
            .control(&fixture.request("team_agent_acquire"))
            .unwrap();
        assert_eq!(replay["dispatch_permitted"], false);
        assert_eq!(
            replay["dispatch_attempt_id"],
            resumed["dispatch_attempt_id"]
        );
    }
}

#[test]
fn profile_unavailable_control_cannot_requeue_an_unknown_dispatch() {
    for request_id in [Value::Null, json!(uuid::Uuid::new_v4().to_string())] {
        let fixture = Fixture::new();
        let claim = fixture
            .control(&fixture.request("team_agent_acquire"))
            .unwrap();
        let mut unknown = fixture.observation(&claim, "unknown", "transport-response-lost");
        unknown["request_id"] = request_id;
        fixture.control(&unknown).unwrap();
        let before = fixture.stored();
        assert_eq!(before.state, TeamAgentState::Unknown);
        let rejection = fixture.observation(
            &claim,
            "profile_unavailable",
            "thinking_profiles_expired_refresh_required",
        );
        assert!(fixture.control(&rejection).is_err());
        assert_eq!(fixture.stored(), before);
        assert_eq!(
            fixture
                .control(&fixture.request("team_agent_acquire"))
                .unwrap()["dispatch_permitted"],
            false
        );
    }
}

#[test]
fn profile_unavailable_control_rejects_unproven_claims_before_any_write() {
    let fixture = Fixture::new();
    let claim = fixture
        .control(&fixture.request("team_agent_acquire"))
        .unwrap();
    let valid = fixture.observation(
        &claim,
        "profile_unavailable",
        "thinking_profile_not_discovered",
    );
    let before = fixture.stored();
    for (field, value) in [
        ("request_id", json!(uuid::Uuid::new_v4().to_string())),
        ("response_observed", json!(true)),
        ("reason", json!("model_unavailable")),
        ("thinking_profile_id", json!("unapproved-replacement")),
    ] {
        let mut rejected = valid.clone();
        rejected[field] = value;
        assert!(fixture.control(&rejected).is_err(), "{field}");
        assert_eq!(fixture.stored(), before, "{field}");
    }
}

#[test]
fn candidate_control_pagination_preserves_cursor_scope_without_mutating_work() {
    let fixture = Fixture::new();
    let other = create_candidate(&fixture.db, &fixture.principal, "second");
    let list = json!({"action":"team_agent_list","owner_user":OWNER,"project":PROJECT,"limit":1});
    let first = fixture.control(&list).unwrap();
    assert_eq!(first["candidates"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"].as_str().unwrap();
    let mut next = list.clone();
    next["cursor"] = json!(cursor);
    let second = fixture.control(&next).unwrap();
    assert_eq!(second["candidates"].as_array().unwrap().len(), 1);
    assert!(second["next_cursor"].is_null());
    let ids = [
        first["candidates"][0]["team_agent_attempt_id"].clone(),
        second["candidates"][0]["team_agent_attempt_id"].clone(),
    ];
    assert_ne!(ids[0], ids[1]);
    assert!(ids.contains(&json!(fixture.record.team_agent_attempt_id)));
    assert!(ids.contains(&json!(other.team_agent_attempt_id)));
    for (field, value) in [
        ("cursor", json!("malformed-cursor")),
        ("cursor", json!(42)),
        ("owner_user", json!("other")),
        ("project", json!("agent:runner:foreign")),
    ] {
        let mut invalid = next.clone();
        invalid[field] = value;
        assert!(
            fixture.control(&invalid).is_err(),
            "{field}: no foreign candidates may be returned"
        );
    }
    assert_eq!(fixture.stored(), fixture.record);
    assert_eq!(
        fixture
            .db
            .read_team_agent_attempt(&fixture.principal, &other.team_agent_attempt_id)
            .unwrap(),
        other
    );
}
