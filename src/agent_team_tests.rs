use crate::agent_team;
use serde_json::json;
use sha2::{Digest, Sha256};
use webcodex_store::{
    AgentEnrollmentPolicy, AgentEnrollmentPurpose, NewAgentEnrollment, VerifiedAgentRun,
};

mod decision;

struct TeamFixture {
    _temp: tempfile::TempDir,
    db: crate::Database,
    run: VerifiedAgentRun,
    task: String,
}

fn team_fixture() -> TeamFixture {
    let temp = tempfile::tempdir().unwrap();
    let db = crate::Database::open(&temp.path().join("state.db")).unwrap();
    let mut digest = Sha256::new();
    digest.update(b"webcodex.communication.principal.v1\0managed-user\0synthetic-owner");
    let owner = webcodex_store::CommunicationPrincipal {
        kind: "managed-user".into(),
        digest: format!("wc_commprincipal_{:x}", digest.finalize()),
    };
    let task = db
        .create_agent_task(
            &owner,
            webcodex_store::NewAgentTask {
                title: "Public task".into(),
                instruction: "Explicit public task".into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some("agent:runner:repo".into()),
                idempotency_key: "task".into(),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let now = chrono::Utc::now().timestamp_millis();
    let mut grant = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Manager,
                owner_user: "synthetic-owner".into(),
                project: "agent:runner:repo".into(),
                principal_key: "synthetic-host".into(),
                task_id: task.clone(),
                task_directory: "workspace/assigned".into(),
                brief: "Explicit task".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 1,
                    max_cli_calls: 1,
                },
                expires_at_unix_ms: now + 60_000,
            },
            None,
            "issue",
            now,
        )
        .unwrap();
    let secret = grant.grant.take().unwrap().into_secret();
    let run = db
        .claim_agent_enrollment(
            "synthetic-host",
            "window",
            &grant.enrollment_id,
            &secret,
            now,
        )
        .unwrap()
        .binding;
    TeamFixture {
        _temp: temp,
        db,
        run,
        task,
    }
}

#[test]
fn enrolled_model_team_entry_does_not_dispatch_or_consume_slots() {
    let fixture = team_fixture();
    let db = &fixture.db;
    let run = &fixture.run;
    let task = &fixture.task;
    for tool in ["team_cli_start", "team_agent_start"] {
        let request = if tool == "team_cli_start" {
            json!({"root_task_id":task,"source_task_id":task,"call_index":0,"fallback_thinking_profile_id":"explicit-profile","idempotency_key":"cli"})
        } else {
            json!({"root_task_id":task,"source_task_id":"explicit-child-task","enrollment_id":"explicit-member-enrollment","thinking_profile_id":"explicit-profile","idempotency_key":"agent"})
        };
        let result = agent_team::call(&db, &run, tool, request);
        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some(if tool == "team_agent_start" {
                "agent_team_async_execution_required"
            } else {
                "team_execution_backend_unavailable"
            })
        );
        assert_eq!(result.output["state_changed"], false);
        assert_eq!(result.output["dispatch_certainty"], "not_started");
    }
    let count = db
        .conn_for_tests()
        .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 0);
    let member_count = db
        .conn_for_tests()
        .query_row("SELECT COUNT(*) FROM wc_team_agent_attempts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(member_count, 0);
    let forged = agent_team::call(
        &db,
        &run,
        "team_policy_get",
        json!({"task_id":task,"owner_user":"another-user"}),
    );
    assert!(!forged.success);
    assert_eq!(forged.error.as_deref(), Some("agent_team_invalid_input"));
    assert!(!agent_team::supports("team_policy_save"));
    db.revoke_agent_run(
        run.owner_user(),
        run.project(),
        run.agent_id(),
        run.run_id(),
    )
    .unwrap();
    assert!(!agent_team::call(&db, &run, "team_policy_get", json!({"task_id":task})).success);
}

#[tokio::test]
async fn enrolled_cli_start_reports_missing_plan_before_catalog_or_reservation() {
    let f = team_fixture();
    let db = std::sync::Arc::new(f.db);
    db.create_user(&crate::models::UserRecord {
        id: "synthetic-owner".into(),
        username: "synthetic-owner".into(),
        created_at: chrono::Utc::now().timestamp(),
        disabled: 0,
        display_name: None,
        role: "user".into(),
        disabled_at: None,
        updated_at: None,
    })
    .unwrap();
    let runtime =
        crate::tool_runtime::ToolRuntime::new_for_tests().with_communication_database(db.clone());
    let mut auth = crate::auth::AuthContext::new(crate::auth::AuthKind::ModelAgent);
    auth.agent_run = Some(f.run);
    // No asset source/Runner is configured. The local plan prerequisite must
    // still be diagnosed without querying either catalog or reserving work.
    let result = runtime
        .isolated_model_call_async(
            "team_cli_start",
            json!({
                "root_task_id":f.task,"source_task_id":f.task,"call_index":0,
                "fallback_thinking_profile_id":"explicit-profile","idempotency_key":"cli-planless"
            }),
            Some(&auth),
        )
        .await;
    assert!(!result.success);
    assert_eq!(result.output["error_kind"], "team_plan_required");
    assert_eq!(result.output["dispatch_certainty"], "not_started");
    assert_eq!(result.output["state_changed"], false);
    assert!(result.output["next_action"].is_string());
    assert_eq!(
        db.list_agent_team_receipts(auth.agent_run.as_ref().unwrap(), &f.task, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    assert_eq!(
        db.read_agent_team_task(auth.agent_run.as_ref().unwrap(), &f.task)
            .unwrap()
            .state,
        "ready"
    );
}

fn member_request(task: &str) -> serde_json::Value {
    json!({
        "root_task_id": task,
        "title": "Inspect public changes",
        "brief": "Review the explicit input snapshot and report findings.",
        "inputs": [{"name":"brief.txt","content":"Authorized input only."}],
        "room_ids": [],
        "max_cli_calls": 1,
        "idempotency_key": "stable-member-request"
    })
}

fn task_count(db: &crate::Database) -> i64 {
    db.conn_for_tests()
        .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn enrolled_manager_member_creation_replays_public_receipt_and_private_directory() {
    let f = team_fixture();
    let request = member_request(&f.task);
    let created = agent_team::call(&f.db, &f.run, "team_member_create", request.clone());
    assert!(created.success, "{:?}", created.error);
    assert_eq!(created.output["replayed"], false);
    assert_ne!(created.output["task_id"], f.task);
    assert_ne!(created.output["agent_id"], f.run.agent_id());
    let mut keys: Vec<_> = created
        .output
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["agent_id", "enrollment_id", "replayed", "run_id", "task_id"]
    );
    let enrollment_id = created.output["enrollment_id"].as_str().unwrap();
    let (directory, policy): (String, String) = f
        .db
        .conn_for_tests()
        .query_row(
            "SELECT task_directory,policy_json FROM wc_agent_enrollments WHERE enrollment_id=?1",
            [enrollment_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(directory.starts_with("workspace/member-"));
    assert!(!directory.contains("stable-member-request"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&policy).unwrap(),
        json!({"max_subagents":0,"max_cli_calls":1})
    );
    let replay = agent_team::call(&f.db, &f.run, "team_member_create", request);
    assert!(replay.success, "{:?}", replay.error);
    assert_eq!(replay.output["replayed"], true);
    for field in ["task_id", "enrollment_id", "agent_id", "run_id"] {
        assert_eq!(replay.output[field], created.output[field]);
    }
    assert_eq!(task_count(&f.db), 2);
}

#[test]
fn enrolled_manager_member_creation_rejects_forged_scope_and_invalid_budget() {
    let f = team_fixture();
    for field in [
        "owner_user",
        "project",
        "agent_id",
        "run_id",
        "task_directory",
        "policy",
    ] {
        let mut request = member_request(&f.task);
        request[field] = json!("caller-supplied");
        let result = agent_team::call(&f.db, &f.run, "team_member_create", request);
        assert_eq!(result.error.as_deref(), Some("agent_team_invalid_input"));
    }
    let mut wrong_root = member_request(&f.task);
    wrong_root["root_task_id"] = json!("another-task");
    assert!(!agent_team::call(&f.db, &f.run, "team_member_create", wrong_root).success);
    let mut invalid_budget = member_request(&f.task);
    invalid_budget["max_cli_calls"] = json!(2);
    assert!(!agent_team::call(&f.db, &f.run, "team_member_create", invalid_budget).success);
    assert_eq!(task_count(&f.db), 1);
    let valid = member_request(&f.task);
    assert!(agent_team::call(&f.db, &f.run, "team_member_create", valid.clone()).success);
    let mut changed_intent = valid.clone();
    changed_intent["brief"] = json!("A different task with the same key");
    assert!(!agent_team::call(&f.db, &f.run, "team_member_create", changed_intent).success);
    let mut beyond_member_cap = valid;
    beyond_member_cap["idempotency_key"] = json!("second-member");
    assert!(!agent_team::call(&f.db, &f.run, "team_member_create", beyond_member_cap).success);
    assert_eq!(task_count(&f.db), 2);
}

fn claimed_assignee(
    f: &TeamFixture,
    purpose: AgentEnrollmentPurpose,
    label: &str,
) -> (VerifiedAgentRun, String) {
    assert!(matches!(
        purpose,
        AgentEnrollmentPurpose::Member | AgentEnrollmentPurpose::Manager
    ));
    let mut digest = Sha256::new();
    digest.update(b"webcodex.communication.principal.v1\0managed-user\0synthetic-owner");
    let owner = webcodex_store::CommunicationPrincipal {
        kind: "managed-user".into(),
        digest: format!("wc_commprincipal_{:x}", digest.finalize()),
    };
    let task =
        f.db.create_agent_task(
            &owner,
            webcodex_store::NewAgentTask {
                title: format!("{label} task"),
                instruction: format!("Explicit {label} task"),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(f.run.project().into()),
                idempotency_key: format!("{label}-task"),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let now = chrono::Utc::now().timestamp_millis();
    let mut enrollment =
        f.db.issue_agent_enrollment(
            NewAgentEnrollment {
                purpose,
                owner_user: f.run.owner_user().into(),
                project: f.run.project().into(),
                principal_key: "synthetic-host".into(),
                task_id: task.clone(),
                task_directory: format!("workspace/{label}"),
                brief: format!("Explicit {label} task"),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 1,
                },
                expires_at_unix_ms: now + 60_000,
            },
            if purpose == AgentEnrollmentPurpose::Member {
                Some(&f.run)
            } else {
                None
            },
            &format!("{label}-enrollment"),
            now,
        )
        .unwrap();
    let grant = enrollment.grant.take().unwrap().into_secret();
    let member =
        f.db.claim_agent_enrollment(
            "synthetic-host",
            &format!("{label}-window"),
            &enrollment.enrollment_id,
            &grant,
            now,
        )
        .unwrap()
        .binding;
    (member, task)
}

#[test]
fn enrolled_member_cannot_create_another_member() {
    let f = team_fixture();
    let (member, task) = claimed_assignee(&f, AgentEnrollmentPurpose::Member, "member");
    let result = agent_team::call(&f.db, &member, "team_member_create", member_request(&task));
    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some("agent_role_denied"));
    assert_eq!(task_count(&f.db), 2);
}

#[test]
fn member_cli_worker_cas_is_readable_by_the_exact_root_manager_with_a_fresh_permit() {
    use crate::agent_team_execution::read_worker_task_projection;
    use webcodex_core::runner_protocol::{
        compute_launch_intent_hash, AgentExecutionCarrier, AgentExecutionProcess,
        AgentExecutionStart, AgentExecutionTarget,
    };
    use webcodex_store::{AgentCliPreparationRequest, TeamAgentPlan, TeamCliCall, TeamPlanInput};

    let f = team_fixture();
    let (caller, member_task) = claimed_assignee(&f, AgentEnrollmentPurpose::Member, "member");
    let (unrelated_manager, _) =
        claimed_assignee(&f, AgentEnrollmentPurpose::Manager, "unrelated-manager");
    assert_ne!(caller.agent_id(), f.run.agent_id());
    assert_eq!(unrelated_manager.owner_user(), f.run.owner_user());
    assert_eq!(unrelated_manager.project(), f.run.project());
    let revision =
        f.db.agent_team_policy(&f.run, &f.task)
            .unwrap()
            .policy
            .revision;
    f.db.register_agent_team_plan(
        &f.run,
        &f.task,
        revision,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: f.task.clone(),
                cli_calls: vec![],
            },
            subagents: vec![TeamAgentPlan {
                task_id: member_task.clone(),
                cli_calls: vec![TeamCliCall {
                    inputs: vec![],
                    provider_id: "workbuddy".into(),
                    model: "synthetic-explicit-model".into(),
                    effort: Some("high".into()),
                    instruction: Some(
                        "Private worker instruction must not appear in the CAS projection.".into(),
                    ),
                }],
            }],
        },
        "member-cli-plan",
    )
    .unwrap();
    let prepared =
        f.db.prepare_agent_cli_worker(
            &caller,
            AgentCliPreparationRequest {
                root_task_id: f.task.clone(),
                source_task_id: member_task,
                call_index: 0,
                idempotency_key: "member-cli-slot".into(),
                fallback_thinking_profile_id: "synthetic-explicit-profile".into(),
                timeout_secs: Some(120),
                target: AgentExecutionTarget {
                    client_id: "runner".into(),
                    runner_instance_id: "synthetic-instance".into(),
                },
                carrier: AgentExecutionCarrier::Job {
                    job_id: "synthetic-worker-job".into(),
                    start_request_id: "synthetic-worker-request".into(),
                },
            },
        )
        .unwrap();
    let preparation = prepared.preparation.unwrap();
    let worker = preparation.worker().clone();
    let mut identity = preparation.binding_identity().clone();
    assert_eq!(identity.requester.run.agent_id, caller.agent_id());
    assert_eq!(identity.requester.root_task_id, f.task);
    assert_ne!(worker.agent_id(), caller.agent_id());
    assert_ne!(worker.agent_id(), f.run.agent_id());
    let worker_task = identity.executor.task_id.clone();
    // Synthetic Store admission data only: no Runner, process or CLI is started.
    let process = AgentExecutionProcess {
        executable: "C:\\synthetic\\never-launched-worker.exe".into(),
        args: vec!["synthetic-task".into()],
    };
    identity.launch_intent_hash = compute_launch_intent_hash(&process, None, 120).unwrap();
    identity.resource_manifest_hash = "a".repeat(64);
    let dispatch =
        f.db.finalize_agent_cli_execution(
            &caller,
            preparation,
            AgentExecutionStart {
                workspace_source: None,
                binding: identity,
                resource_set_id: "synthetic-resources".into(),
                process,
                stdin: None,
                timeout_secs: 120,
            },
        )
        .unwrap();
    let attempt = dispatch.binding().requester.cli_attempt_id.clone();

    // A finalized attempt does not imply that its public Task execution exists.
    // Each call obtains its own current-caller permit, including the root Manager.
    for current in [&caller, &f.run] {
        assert_eq!(
            read_worker_task_projection(&f.db, current, &attempt).unwrap(),
            None
        );
    }
    assert!(read_worker_task_projection(&f.db, &unrelated_manager, &attempt).is_err());

    let started =
        f.db.start_agent_public_task(&worker, &worker_task, "worker-public-start")
            .unwrap();
    let expected = json!({
        "task_id":worker_task,
        "state":started.execution.state,
        "revision":started.execution.revision,
        "completion_expected_revision":started.execution.revision,
    });
    for current in [&caller, &f.run] {
        let projection = read_worker_task_projection(&f.db, current, &attempt)
            .unwrap()
            .unwrap();
        assert_eq!(projection, expected);
        assert_eq!(projection.as_object().unwrap().len(), 4);
        for private in [
            "terminal_result",
            "terminal_reason",
            "instruction",
            "brief",
            "goal",
            "memory",
            "logs",
            "started_run_id",
            "agent_id",
        ] {
            assert!(
                projection.get(private).is_none(),
                "private field escaped: {private}"
            );
        }
        assert!(!projection
            .to_string()
            .contains("Private worker instruction"));
    }
    assert!(read_worker_task_projection(&f.db, &unrelated_manager, &attempt).is_err());
}

#[test]
fn enrolled_public_task_lifecycle_uses_exact_task_revision_and_replay() {
    let f = team_fixture();
    let empty = agent_team::call(
        &f.db,
        &f.run,
        "team_task_execution_read",
        json!({"task_id":f.task}),
    );
    assert!(empty.success);
    assert!(empty.output.is_null());
    let start_request = json!({"task_id":f.task,"idempotency_key":"start-public"});
    let started = agent_team::call(&f.db, &f.run, "team_task_start", start_request.clone());
    assert!(started.success, "{:?}", started.error);
    let replay = agent_team::call(&f.db, &f.run, "team_task_start", start_request);
    assert!(replay.success);
    assert_eq!(replay.output["replayed"], true);
    assert_eq!(replay.output["execution"], started.output["execution"]);
    assert!(
        !agent_team::call(
            &f.db,
            &f.run,
            "team_task_execution_read",
            json!({"task_id":"another-task"})
        )
        .success
    );
    let mut completion = json!({
        "task_id":f.task,
        "execution_id":started.output["execution"]["execution_id"],
        "expected_revision":99,
        "outcome":"succeeded",
        "terminal_result":"Completed the assigned public task.",
        "completion_key":"complete-public"
    });
    let stale = agent_team::call(&f.db, &f.run, "team_task_complete", completion.clone());
    assert_eq!(
        stale.error.as_deref(),
        Some("public_task_revision_conflict")
    );
    completion["expected_revision"] = started.output["execution"]["revision"].clone();
    let completed = agent_team::call(&f.db, &f.run, "team_task_complete", completion.clone());
    assert!(completed.success, "{:?}", completed.error);
    assert_eq!(completed.output["execution"]["state"], "succeeded");
    let replay = agent_team::call(&f.db, &f.run, "team_task_complete", completion);
    assert!(replay.success);
    assert_eq!(replay.output["replayed"], true);
    let read = agent_team::call(
        &f.db,
        &f.run,
        "team_task_execution_read",
        json!({"task_id":f.task}),
    );
    assert!(read.success);
    assert_eq!(read.output, completed.output["execution"]);
}

#[test]
fn enrolled_public_task_tools_reject_identity_overrides_and_nonterminal_outcome() {
    let f = team_fixture();
    let forged = agent_team::call(
        &f.db,
        &f.run,
        "team_task_start",
        json!({"task_id":f.task,"idempotency_key":"start","agent_id":"someone-else"}),
    );
    assert_eq!(forged.error.as_deref(), Some("agent_team_invalid_input"));
    let invalid = agent_team::call(
        &f.db,
        &f.run,
        "team_task_complete",
        json!({"task_id":f.task,"execution_id":"execution","expected_revision":1,"outcome":"active","completion_key":"complete"}),
    );
    assert_eq!(
        invalid.error.as_deref(),
        Some("invalid_public_task_completion")
    );
}

#[test]
fn enrolled_team_specs_separate_sync_lifecycle_from_async_execution() {
    let specs = agent_team::tool_specs();
    for tool in [
        "team_member_create",
        "team_task_start",
        "team_task_execution_read",
        "team_task_complete",
    ] {
        assert!(agent_team::supports(tool));
        assert!(!agent_team::supports_execution(tool));
        let spec = specs.iter().find(|spec| spec["name"] == tool).unwrap();
        assert_eq!(spec["inputSchema"]["additionalProperties"], false);
        for forbidden in [
            "owner_user",
            "project",
            "agent_id",
            "run_id",
            "task_directory",
        ] {
            assert!(spec["inputSchema"]["properties"].get(forbidden).is_none());
        }
    }
    assert!(agent_team::supports_execution("team_cli_start"));
    assert!(agent_team::supports_execution("team_agent_start"));
    assert!(!agent_team::supports_execution("run_process"));
}

#[test]
fn enrolled_member_dispatch_requires_exact_public_receipt_and_explicit_profile() {
    let request = json!({
        "root_task_id":"root-task",
        "source_task_id":"child-task",
        "enrollment_id":"member-enrollment",
        "thinking_profile_id":"discovered-profile",
        "idempotency_key":"stable-dispatch"
    });
    let parsed: agent_team::AgentStart = serde_json::from_value(request.clone()).unwrap();
    assert!(parsed.validate().is_ok());
    for field in ["enrollment_id", "thinking_profile_id"] {
        let mut missing = request.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<agent_team::AgentStart>(missing).is_err());
        let mut blank = request.clone();
        blank[field] = json!(" ");
        assert!(serde_json::from_value::<agent_team::AgentStart>(blank)
            .unwrap()
            .validate()
            .is_err());
    }
    for field in [
        "owner_user",
        "project",
        "agent_id",
        "run_id",
        "task_directory",
        "claim_grant",
    ] {
        let mut forged = request.clone();
        forged[field] = json!("caller-controlled");
        assert!(serde_json::from_value::<agent_team::AgentStart>(forged).is_err());
    }
    let mut same_task = request;
    same_task["source_task_id"] = same_task["root_task_id"].clone();
    assert!(serde_json::from_value::<agent_team::AgentStart>(same_task)
        .unwrap()
        .validate()
        .is_err());
    let specs = agent_team::tool_specs();
    let spec = specs
        .iter()
        .find(|spec| spec["name"] == "team_agent_start")
        .unwrap();
    assert!(spec["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .contains(&json!("enrollment_id")));
}
