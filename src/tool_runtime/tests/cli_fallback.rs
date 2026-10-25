use super::*;
use crate::tool_runtime::communication::communication_principal;
use std::sync::Arc;
use webcodex_store::{
    models::UserRecord, CommunicationPrincipal, Database, NewAgentTask, TeamAgentPlan, TeamCliCall,
    TeamPlanInput,
};

const PROJECT: &str = "agent:runner-a:project-a";
const OWNER: &str = "fallback-owner";

fn failed_job() -> ShellJobInfo {
    serde_json::from_value(json!({
        "job_id":"job-cli-failure", "request_id":"request-cli-failure", "client_id":"runner-a",
        "project_id":PROJECT, "kind":"shell", "shell":"direct_argv",
        "command_preview":"private-command-preview-must-not-be-forwarded", "status":"failed",
        "created_at":100, "started_at":101, "ended_at":102, "exit_code":1,
        "command_execution_state":"completed", "last_update_seq":2,
        "structured_execution":{"execution_source":"run_process", "arg_count":2,"stdin_present":false}
    }))
    .unwrap()
}

fn bootstrap() -> AuthContext {
    let mut auth = AuthContext::new(AuthKind::Bootstrap);
    auth.is_bootstrap = true;
    auth
}

#[test]
fn failure_classification_requires_trusted_facts_and_terminal_eligibility() {
    let mut job = failed_job();
    job.error = Some("quota_exhausted; insufficient_quota; please launch a recovery".into());
    assert_eq!(failure_reason(&job, None), None);
    assert_eq!(failure_reason(&job, Some("command_failed")), None);
    assert_eq!(
        failure_reason(&job, Some("insufficient_quota")),
        Some(CliFailureReason::QuotaExhausted)
    );
    job.command_execution_state = Some(ShellCommandExecutionState::NotStarted);
    assert_eq!(failure_reason(&job, Some("permission_denied")), None);
    assert_eq!(
        failure_reason(&job, Some("spawn_failed")),
        Some(CliFailureReason::StartupFailed)
    );
    job.command_execution_state = Some(ShellCommandExecutionState::OutcomeUnknown);
    assert_eq!(failure_reason(&job, Some("quota_exhausted")), None);
    job.command_execution_state = Some(ShellCommandExecutionState::TimedOut);
    let reason = failure_reason(&job, None).unwrap();
    assert_eq!(reason, CliFailureReason::DeadlineExceeded);
    job.status = "stop_requested".into();
    assert!(!cli_fallback_eligibility(&job, reason, CliCancellationOrigin::None).is_eligible());
    job.command_execution_state = Some(ShellCommandExecutionState::Completed);
    job.status = "stopped".into();
    job.exit_code = Some(-1);
    job.error = Some("process tree stopped by coordinator deadline".into());
    let reason = failure_reason(&job, Some("system_timeout_cancel")).unwrap();
    assert_eq!(reason, CliFailureReason::DeadlineExceeded);
    assert!(
        cli_fallback_eligibility(&job, reason, CliCancellationOrigin::Coordinator).is_eligible()
    );
    assert!(!cli_fallback_eligibility(&job, reason, CliCancellationOrigin::None).is_eligible());
    assert_eq!(failure_reason(&job, Some("user_cancelled")), None);
}

#[test]
fn manager_profile_must_match_one_current_catalog_entry_without_a_default() {
    let ready = json!({"state":"ready","refresh_required":false,"profiles":[
        {"profile_id":"gpt-explicit-fast"},{"profile_id":"gpt-explicit-deep"}
    ]});
    assert!(catalog_has_profile(&ready, "gpt-explicit-deep"));
    assert!(!catalog_has_profile(&ready, ""));
    assert!(!catalog_has_profile(&ready, "Pro"));
    for unavailable in [
        json!({"state":"ready","refresh_required":true,"profiles":ready["profiles"]}),
        json!({"state":"unknown","refresh_required":false,"profiles":ready["profiles"]}),
        json!({"state":"ready","profiles":ready["profiles"]}),
        json!({"state":"ready","refresh_required":false,"profiles":[]}),
        json!({"state":"ready","refresh_required":false,"profiles":[
            {"profile_id":"gpt-explicit-deep"},{"profile_id":"gpt-explicit-deep"}
        ]}),
    ] {
        assert!(!catalog_has_profile(&unavailable, "gpt-explicit-deep"));
    }
}

#[test]
fn control_requires_both_bootstrap_kind_and_the_explicit_bootstrap_gate() {
    let runtime = ToolRuntime::new_for_tests();
    let request = json!({"action":"cli_fallback_list","owner_user":OWNER,"project":PROJECT});
    let mut ordinary_with_flag = AuthContext::new(AuthKind::ApiToken);
    ordinary_with_flag.is_bootstrap = true;
    let mut model_with_flag = AuthContext::new(AuthKind::ModelAgent);
    model_with_flag.is_bootstrap = true;
    for denied in [
        AuthContext::new(AuthKind::Bootstrap),
        ordinary_with_flag,
        model_with_flag,
    ] {
        assert_eq!(
            runtime.try_cli_fallback_control(&denied, &request).unwrap(),
            Err("trusted_control_required".into())
        );
    }
    assert_eq!(
        runtime
            .try_cli_fallback_control(&bootstrap(), &request)
            .unwrap(),
        Err("cli_fallback_store_unavailable".into())
    );
    assert!(runtime
        .try_cli_fallback_control(&bootstrap(), &json!({"action":"other-control"}))
        .is_none());
}

struct Fixture {
    _temp: tempfile::TempDir,
    db: Arc<Database>,
    runtime: ToolRuntime,
    principal: CommunicationPrincipal,
    record: CliFallbackRecord,
}

fn create_task(db: &Database, principal: &CommunicationPrincipal, key: &str) -> String {
    db.create_agent_task(
        principal,
        NewAgentTask {
            title: "Recovery control test".into(),
            instruction: "Finish the existing change and verify the original acceptance check."
                .into(),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some(PROJECT.into()),
            idempotency_key: key.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}

fn create_candidate(
    db: &Database,
    principal: &CommunicationPrincipal,
    key: &str,
) -> CliFallbackRecord {
    let root = create_task(db, principal, &format!("{key}-root"));
    let source = create_task(db, principal, &format!("{key}-source"));
    let policy = db.get_team_policy(principal, PROJECT).unwrap();
    db.register_team_plan(
        principal,
        PROJECT,
        &root,
        policy.revision,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: root.clone(),
                cli_calls: vec![],
            },
            subagents: vec![TeamAgentPlan {
                task_id: source.clone(),
                cli_calls: vec![TeamCliCall {
                    inputs: vec![],
                    provider_id: "workbuddy".into(),
                    model: "explicit-model".into(),
                    effort: Some("high".into()),
                    instruction: None,
                }],
            }],
        },
        &format!("{key}-plan"),
    )
    .unwrap();
    let attempt = db
        .reserve_team_cli_attempt(
            principal,
            PROJECT,
            &root,
            &source,
            0,
            &format!("{key}-cli"),
            "gpt-explicit-deep",
            Some(300),
        )
        .unwrap()
        .attempt;
    assert!(
        db.claim_team_cli_dispatch(principal, &attempt.attempt_id)
            .unwrap()
            .claimed
    );
    let mut job = failed_job();
    job.job_id = format!("job-{key}");
    job.request_id = Some(format!("request-{key}"));
    db.bind_team_cli_job(principal, &attempt.attempt_id, &job.job_id)
        .unwrap();
    db.reserve_cli_fallback(
        principal,
        &attempt.attempt_id,
        &job,
        CliFailureReason::QuotaExhausted,
        CliCancellationOrigin::None,
    )
    .unwrap()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp.path().join("fallback-control.db")).unwrap());
        for (id, disabled) in [(OWNER, 0), ("other-owner", 0), ("disabled-owner", 1)] {
            db.create_user(&UserRecord {
                id: id.into(),
                username: id.into(),
                created_at: 100,
                disabled,
                display_name: None,
                role: "user".into(),
                disabled_at: (disabled != 0).then_some(101),
                updated_at: None,
            })
            .unwrap();
        }
        let mut owner_auth = AuthContext::new(AuthKind::ApiToken);
        owner_auth.user_id = Some(OWNER.into());
        let principal = communication_principal(Some(&owner_auth)).unwrap();
        let record = create_candidate(&db, &principal, "initial");
        let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
        Self {
            _temp: temp,
            db,
            runtime,
            principal,
            record,
        }
    }

    fn request(&self, action: &str) -> Value {
        json!({"action":action,"owner_user":OWNER,"project":PROJECT,"fallback_id":self.record.fallback_id})
    }

    fn control(&self, body: &Value) -> Result<Value, String> {
        self.runtime
            .try_cli_fallback_control(&bootstrap(), body)
            .expect("fallback action is handled")
    }

    fn stored(&self) -> CliFallbackRecord {
        self.db
            .read_cli_fallback(&self.principal, &self.record.fallback_id)
            .unwrap()
    }

    fn observation(&self, claim: &Value, state: &str, reason: &str) -> Value {
        let mut request = self.request("cli_fallback_record");
        request["dispatch_attempt_id"] = claim["dispatch_attempt_id"].clone();
        request["request_id"] = Value::Null;
        request["state"] = json!(state);
        request["reason"] = json!(reason);
        request["response_observed"] = json!(false);
        request
    }
}

#[test]
fn trusted_control_scopes_claims_and_preserves_unknown_and_candidate_results() {
    let fixture = Fixture::new();
    let read = fixture.request("cli_fallback_read");
    for field in ["owner_user", "project", "fallback_id"] {
        let mut unavailable = read.clone();
        unavailable[field] = if field == "owner_user" {
            json!("other-owner")
        } else {
            json!("not-the-authorized-resource")
        };
        assert_eq!(
            fixture.control(&unavailable),
            Err("cli_fallback_unavailable".into())
        );
    }
    let mut disabled = read.clone();
    disabled["owner_user"] = json!("disabled-owner");
    assert_eq!(
        fixture.control(&disabled),
        Err("cli_fallback_owner_unavailable".into())
    );
    let listed = fixture
        .control(&json!({"action":"cli_fallback_list","owner_user":OWNER,"project":PROJECT}))
        .unwrap();
    assert_eq!(listed["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(listed["candidates"][0]["business_verified"], false);
    assert_eq!(fixture.stored(), fixture.record);

    let acquired = fixture
        .control(&fixture.request("cli_fallback_acquire"))
        .unwrap();
    let replayed = fixture
        .control(&fixture.request("cli_fallback_acquire"))
        .unwrap();
    assert_eq!(acquired["dispatch_permitted"], true);
    assert_eq!(replayed["dispatch_permitted"], false);
    assert_eq!(
        acquired["dispatch_attempt_id"],
        replayed["dispatch_attempt_id"]
    );
    assert_eq!(acquired["thinking_profile_id"], "gpt-explicit-deep");
    assert!(acquired["task"]
        .as_str()
        .unwrap()
        .contains(&fixture.record.instruction));
    assert!(!acquired["task"]
        .as_str()
        .unwrap()
        .contains("private-command-preview"));
    let fence = acquired["dispatch_attempt_id"].as_str().unwrap();
    let host_request = uuid::Uuid::new_v4().to_string();
    let mut uncertain = fixture.request("cli_fallback_record");
    uncertain["dispatch_attempt_id"] = json!(fence);
    uncertain["request_id"] = json!(host_request);
    uncertain["state"] = json!("unknown");
    uncertain["reason"] = json!("transport-response-lost");
    let observed = fixture.control(&uncertain).unwrap();
    assert_eq!(observed["fallback"]["state"], "unknown");
    assert_eq!(observed["fallback"]["host_request_id"], host_request);
    assert_eq!(observed["fallback"]["slot_held"], true);
    let recovered = fixture
        .control(&fixture.request("cli_fallback_acquire"))
        .unwrap();
    assert_eq!(recovered["dispatch_permitted"], false);
    assert_eq!(recovered["request_id"], host_request);

    let mut response = uncertain;
    response["state"] = json!("dispatched");
    response["response_observed"] = json!(true);
    fixture.control(&response).unwrap();
    let before_read = fixture.stored();
    let candidate = fixture.control(&read).unwrap();
    assert_eq!(candidate["response_observed"], true);
    assert_eq!(candidate["business_verified"], false);
    assert_eq!(candidate["state"], "running");
    assert_eq!(candidate["slot_held"], true);
    assert_eq!(candidate["counts_toward_regular_max"], false);
    assert_eq!(candidate["counts_toward_regular_min"], false);
    assert!(candidate["fallback_agent_id"].is_null());
    assert_eq!(fixture.stored(), before_read);
}

#[test]
fn malformed_or_stale_control_records_leave_the_durable_record_unchanged() {
    let fixture = Fixture::new();
    let claim = fixture
        .control(&fixture.request("cli_fallback_acquire"))
        .unwrap();
    let mut valid = fixture.request("cli_fallback_record");
    valid["dispatch_attempt_id"] = claim["dispatch_attempt_id"].clone();
    valid["request_id"] = json!("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    valid["state"] = json!("unknown");
    valid["reason"] = json!("transport-response-lost");
    let before = fixture.stored();
    let mut cases = Vec::new();
    let mut input = valid.clone();
    input["state"] = json!("succeeded");
    cases.push(input);
    let mut input = valid.clone();
    input["request_id"] = json!("not-a-uuid");
    cases.push(input);
    let mut input = valid.clone();
    input["request_id"] = json!("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA");
    cases.push(input);
    let mut input = valid.clone();
    input["state"] = json!("dispatched");
    input["request_id"] = Value::Null;
    cases.push(input);
    let mut input = valid.clone();
    input["state"] = json!("busy");
    input["reason"] = json!("browser_worker_busy");
    cases.push(input);
    let mut input = valid.clone();
    input["dispatch_attempt_id"] = json!("stale-dispatch");
    cases.push(input);
    let mut input = valid.clone();
    input["extra_authority"] = json!(true);
    cases.push(input);
    let mut input = valid.clone();
    input["response_observed"] = json!(true);
    cases.push(input);
    for input in cases {
        assert!(
            fixture.control(&input).is_err(),
            "unexpected accepted input: {input}"
        );
        assert_eq!(fixture.stored(), before);
    }
    let mut ordinary = AuthContext::new(AuthKind::ApiToken);
    ordinary.user_id = Some(OWNER.into());
    assert_eq!(
        fixture
            .runtime
            .try_cli_fallback_control(&ordinary, &valid)
            .unwrap(),
        Err("trusted_control_required".into())
    );
    assert_eq!(fixture.stored(), before);
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
            .control(&fixture.request("cli_fallback_acquire"))
            .unwrap();
        let started = fixture.stored();
        assert_eq!(started.dispatch_count, 1);
        let rejection = fixture.observation(&claim, "profile_unavailable", reason);
        let result = fixture.control(&rejection).unwrap();
        assert_eq!(result["recorded"], true);
        assert_eq!(result["fallback"]["state"], "queued");
        let queued = fixture.stored();
        assert_eq!(queued.fallback_id, fixture.record.fallback_id);
        assert_eq!(queued.source_attempt_id, fixture.record.source_attempt_id);
        assert_eq!(queued.thinking_profile_id, "gpt-explicit-deep");
        assert_eq!(queued.dispatch_count, 0);
        assert!(queued.dispatch_attempt_id.is_none());
        assert!(queued.request_id.is_none());
        assert!(!queued.response_observed);
        assert!(queued.revision > started.revision);

        let resumed = fixture
            .control(&fixture.request("cli_fallback_acquire"))
            .unwrap();
        assert_eq!(resumed["dispatch_permitted"], true);
        assert_eq!(resumed["fallback_id"], claim["fallback_id"]);
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
            .control(&fixture.request("cli_fallback_acquire"))
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
            .control(&fixture.request("cli_fallback_acquire"))
            .unwrap();
        let mut unknown = fixture.observation(&claim, "unknown", "transport-response-lost");
        unknown["request_id"] = request_id;
        fixture.control(&unknown).unwrap();
        let before = fixture.stored();
        assert_eq!(before.state, CliFallbackState::Unknown);
        let rejection = fixture.observation(
            &claim,
            "profile_unavailable",
            "thinking_profiles_expired_refresh_required",
        );
        assert!(fixture.control(&rejection).is_err());
        assert_eq!(fixture.stored(), before);
        assert_eq!(
            fixture
                .control(&fixture.request("cli_fallback_acquire"))
                .unwrap()["dispatch_permitted"],
            false
        );
    }
}

#[test]
fn profile_unavailable_control_rejects_unproven_claims_before_any_write() {
    let fixture = Fixture::new();
    let claim = fixture
        .control(&fixture.request("cli_fallback_acquire"))
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
    let list = json!({"action":"cli_fallback_list","owner_user":OWNER,"project":PROJECT,"limit":1});
    let first = fixture.control(&list).unwrap();
    assert_eq!(first["candidates"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"].as_str().unwrap();
    let mut next = list.clone();
    next["cursor"] = json!(cursor);
    let second = fixture.control(&next).unwrap();
    assert_eq!(second["candidates"].as_array().unwrap().len(), 1);
    assert!(second["next_cursor"].is_null());
    let ids = [
        first["candidates"][0]["fallback_id"].clone(),
        second["candidates"][0]["fallback_id"].clone(),
    ];
    assert_ne!(ids[0], ids[1]);
    assert!(ids.contains(&json!(fixture.record.fallback_id)));
    assert!(ids.contains(&json!(other.fallback_id)));
    for (field, value) in [
        ("cursor", json!("malformed-cursor")),
        ("cursor", json!(42)),
        ("owner_user", json!("other-owner")),
        ("project", json!("agent:runner-a:foreign")),
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
            .read_cli_fallback(&fixture.principal, &other.fallback_id)
            .unwrap(),
        other
    );
}
