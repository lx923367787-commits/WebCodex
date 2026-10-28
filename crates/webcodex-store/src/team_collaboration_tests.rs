use super::*;
use crate::{AgentEnrollmentPolicy, AgentEnrollmentPurpose, NewAgentEnrollment, NewAgentTask};

struct Fixture {
    temp: tempfile::TempDir,
    db: Database,
    host: String,
    manager: VerifiedAgentRun,
    member: VerifiedAgentRun,
    root: String,
    child: String,
}
const OWNER: &str = "event-owner";
const PROJECT: &str = "event-project";
fn task(db: &Database, label: &str) -> String {
    let principal = crate::agent_isolation::enrollment_principal_for_execution(OWNER);
    db.create_agent_task(
        &principal,
        NewAgentTask {
            title: label.into(),
            instruction: "Public fixture task".into(),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some(PROJECT.into()),
            idempotency_key: label.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}
fn enroll(
    db: &Database,
    principal: &str,
    task: &str,
    source: Option<&VerifiedAgentRun>,
) -> VerifiedAgentRun {
    let mut issued = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: if source.is_none() {
                    AgentEnrollmentPurpose::Manager
                } else {
                    AgentEnrollmentPurpose::Member
                },
                owner_user: OWNER.into(),
                project: PROJECT.into(),
                principal_key: principal.into(),
                task_id: task.into(),
                task_directory: format!("workspace/{task}"),
                brief: "Public fixture".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: if source.is_none() { 1 } else { 0 },
                    max_cli_calls: 0,
                },
                expires_at_unix_ms: now() + 60000,
            },
            source,
            task,
            now(),
        )
        .unwrap();
    db.claim_agent_enrollment(
        principal,
        task,
        &issued.enrollment_id,
        &issued.grant.take().unwrap().into_secret(),
        now(),
    )
    .unwrap()
    .binding
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("events.db")).unwrap();
    let host = db
        .register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), &"b".repeat(64))
        .unwrap()
        .host_id;
    let principal = db.native_agent_host_principal(&host).unwrap();
    let root = task(&db, "root");
    let manager = enroll(&db, &principal, &root, None);
    let child = task(&db, "child");
    let member = enroll(&db, &principal, &child, Some(&manager));
    Fixture {
        temp,
        db,
        host,
        manager,
        member,
        root,
        child,
    }
}
fn call(db: &Database, run: &VerifiedAgentRun, value: Value) -> Result<Value> {
    db.team_collaboration(run, serde_json::from_value(value).unwrap())
}
fn publish(f: &Fixture, key: &str) -> Value {
    call(&f.db,&f.manager,json!({"action":"publish","message":"Public protocol changed; review the published artifact.","idempotency_key":key})).unwrap()
}
fn candidates(f: &Fixture) -> Value {
    f.db.team_continuation_candidates(OWNER, PROJECT, &f.host)
        .unwrap()
}
fn claim(f: &Fixture) -> (TeamContinuationTarget, String) {
    let c = candidates(f);
    let target = serde_json::from_value(c["candidates"][0]["target"].clone()).unwrap();
    let c =
        f.db.team_continuation_claim(OWNER, PROJECT, &f.host, &target)
            .unwrap();
    assert_eq!(c["claimed"], true);
    (target, c["attempt"]["attempt_id"].as_str().unwrap().into())
}
fn transition(f: &Fixture, t: &TeamContinuationTarget, id: &str, action: &str) -> Value {
    f.db.team_continuation_transition(OWNER, PROJECT, &f.host, t, id, action)
        .unwrap()
}

#[test]
fn framework_recovery_event_has_distinct_provenance_and_stable_replay() {
    let f = fixture();
    let first =
        f.db.publish_team_framework_event(
            OWNER,
            PROJECT,
            &f.root,
            "build-fixture",
            "Framework capability restored; existing user task remains the authority.",
            vec![f.child.clone()],
            "build-fixture",
        )
        .unwrap();
    let replay =
        f.db.publish_team_framework_event(
            OWNER,
            PROJECT,
            &f.root,
            "build-fixture",
            "Framework capability restored; existing user task remains the authority.",
            vec![f.child.clone()],
            "build-fixture",
        )
        .unwrap();
    assert_eq!(first["sequence"], replay["sequence"]);
    assert_eq!(replay["replayed"], true);
    assert!(f
        .db
        .publish_team_framework_event(
            "other",
            PROJECT,
            &f.root,
            "build-fixture",
            "Not authorized",
            vec![],
            "key"
        )
        .is_err());
    let read = call(&f.db, &f.member, json!({"action":"read"})).unwrap();
    assert_eq!(read["events"][0]["origin"], "framework");
    assert_eq!(
        read["events"][0]["author_agent_id"],
        "framework:build-fixture"
    );
}

#[test]
fn actual_public_task_completion_atomically_notifies_original_manager() {
    let f = fixture();
    let started =
        f.db.start_agent_public_task(&f.member, &f.child, "start")
            .unwrap();
    f.db.complete_agent_public_task(
        &f.member,
        &f.child,
        &started.execution.execution_id,
        started.execution.revision,
        crate::AgentTaskState::Succeeded,
        Some("Synthetic framework fixture delivered"),
        None,
        "complete",
    )
    .unwrap();
    let read = call(&f.db, &f.manager, json!({"action":"read"})).unwrap();
    assert_eq!(read["events"].as_array().unwrap().len(), 1);
    assert_eq!(read["events"][0]["origin"], "framework");
    assert!(read["events"][0]["message"]
        .as_str()
        .unwrap()
        .contains(&started.execution.execution_id));
}

#[test]
fn event_before_wait_is_not_lost_and_replay_never_duplicates_delivery() {
    let f = fixture();
    let event = publish(&f, "same-key");
    assert_eq!(publish(&f, "same-key")["sequence"], event["sequence"]);
    let changed = call(
        &f.db,
        &f.manager,
        json!({"action":"publish","message":"Different body","idempotency_key":"same-key"}),
    )
    .unwrap_err();
    assert_eq!(changed, "team_collaboration_idempotency_conflict");
    let waiting = call(
        &f.db,
        &f.member,
        json!({"action":"wait","reason":"Waiting for protocol"}),
    )
    .unwrap();
    assert_eq!(waiting["execution_state"], "queued");
    assert_eq!(waiting["pending_events"], 1);
    assert!(call(
        &f.db,
        &f.member,
        json!({"action":"acknowledge","through_sequence":event["sequence"]})
    )
    .is_err());
    assert!(call(
        &f.db,
        &f.member,
        json!({"action":"read","after_sequence":event["sequence"]})
    )
    .is_err());
    let read = call(&f.db, &f.member, json!({"action":"read"})).unwrap();
    assert_eq!(read["events"].as_array().unwrap().len(), 1);
    assert_eq!(read["events"][0]["origin"], "agent");
    let waiting=call(&f.db,&f.member,json!({"action":"wait","reason":"Waiting for next revision","through_sequence":read["next_sequence"]})).unwrap();
    assert_eq!(waiting["execution_state"], "waiting_dependencies");
    assert!(candidates(&f)["candidates"].as_array().unwrap().is_empty());
}

#[test]
fn event_scope_is_original_team_and_publication_rolls_back_with_delivery_failure() {
    let f = fixture();
    let other_task = task(&f.db, "foreign");
    let principal = f.db.native_agent_host_principal(&f.host).unwrap();
    let other = enroll(&f.db, &principal, &other_task, None);
    assert!(call(&f.db,&f.manager,json!({"action":"publish","message":"No foreign delivery","recipients":[other_task],"idempotency_key":"foreign"})).is_err());
    f.db.conn_for_tests().execute_batch("CREATE TRIGGER fail_delivery BEFORE INSERT ON wc_team_event_deliveries BEGIN SELECT RAISE(ABORT,'fixture delivery failure'); END;").unwrap();
    assert!(call(
        &f.db,
        &f.manager,
        json!({"action":"publish","message":"Atomic event","idempotency_key":"atomic"})
    )
    .is_err());
    let conn = f.db.conn_for_tests();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM wc_team_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    conn.execute_batch("DROP TRIGGER fail_delivery").unwrap();
    drop(conn);
    publish(&f, "atomic");
    assert!(
        call(&f.db, &other, json!({"action":"read"})).unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        candidates(&f)["candidates"][0]["target"]["task_id"],
        f.child
    );
}

#[test]
fn coalesced_dispatch_is_once_and_unknown_survives_restart_without_resend() {
    let f = fixture();
    publish(&f, "one");
    let second = publish(&f, "two");
    let (target, id) = claim(&f);
    assert_eq!(
        f.db.team_continuation_claim(OWNER, PROJECT, &f.host, &target)
            .unwrap()["claimed"],
        false
    );
    assert_eq!(
        transition(&f, &target, &id, "dispatch")["send_permitted"],
        true
    );
    assert_eq!(
        transition(&f, &target, &id, "dispatch")["send_permitted"],
        false
    );
    f.db.conn_for_tests()
        .execute(
            "UPDATE wc_team_event_attempts SET lease_until=0 WHERE attempt_id=?1",
            [&id],
        )
        .unwrap();
    assert_eq!(
        candidates(&f)["candidates"][0]["attempt"]["state"],
        "unknown"
    );
    let reopened = Database::open(&f.temp.path().join("events.db")).unwrap();
    assert_eq!(
        reopened
            .team_continuation_claim(OWNER, PROJECT, &f.host, &target)
            .unwrap()["claimed"],
        false
    );
    assert_eq!(
        reopened
            .team_continuation_transition(OWNER, PROJECT, &f.host, &target, &id, "dispatch")
            .unwrap()["send_permitted"],
        false
    );
    let third = publish(&f, "three");
    assert!(third["sequence"].as_i64() > second["sequence"].as_i64());
    call(&f.db, &f.member, json!({"action":"read"})).unwrap();
    call(
        &f.db,
        &f.member,
        json!({"action":"acknowledge","through_sequence":second["sequence"]}),
    )
    .unwrap();
    let next = candidates(&f);
    assert_eq!(next["candidates"][0]["pending_events"], 1);
    let (_, new) = claim(&f);
    assert_ne!(new, id);
}

#[test]
fn pre_send_retry_is_bounded_and_stale_target_cannot_cross_send_fence() {
    let f = fixture();
    publish(&f, "one");
    for _ in 0..4 {
        let (target, id) = claim(&f);
        transition(&f, &target, &id, "not_sent");
    }
    assert!(candidates(&f)["candidates"].as_array().unwrap().is_empty());
    assert_eq!(
        call(&f.db, &f.member, json!({"action":"status"})).unwrap()["execution_state"],
        "blocked"
    );
    publish(&f, "meaningful-new-event");
    let (target, id) = claim(&f);
    let mut wrong = target.clone();
    wrong.generation += 1;
    assert!(f
        .db
        .team_continuation_transition(OWNER, PROJECT, &f.host, &wrong, &id, "dispatch")
        .is_err());
    f.db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET state='revoked' WHERE run_id=?1",
            [&target.run_id],
        )
        .unwrap();
    assert!(f
        .db
        .team_continuation_transition(OWNER, PROJECT, &f.host, &target, &id, "dispatch")
        .is_err());
    assert!(candidates(&f)["candidates"].as_array().unwrap().is_empty());
    assert_eq!(
        f.db.conn_for_tests()
            .query_row(
                "SELECT state FROM wc_team_event_attempts WHERE attempt_id=?1",
                [&id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "stale"
    );
}

#[test]
fn delivered_is_not_running_and_public_task_state_is_separate() {
    let f = fixture();
    publish(&f, "one");
    let (target, id) = claim(&f);
    transition(&f, &target, &id, "dispatch");
    transition(&f, &target, &id, "delivered");
    let before = call(&f.db, &f.member, json!({"action":"status"})).unwrap();
    assert_ne!(before["execution_state"], "running");
    transition(&f, &target, &id, "running");
    let during = call(&f.db, &f.member, json!({"action":"status"})).unwrap();
    assert_eq!(during["execution_state"], "running");
    assert_eq!(during["task_lifecycle"], "ready");
    transition(&f, &target, &id, "idle");
    assert_ne!(
        call(&f.db, &f.member, json!({"action":"status"})).unwrap()["execution_state"],
        "running"
    );
    assert_eq!(
        f.db.team_continuation_candidates("wrong-owner", PROJECT, &f.host)
            .unwrap()["candidates"],
        json!([])
    );
    assert_ne!(f.root, f.child);
}

#[test]
fn public_team_status_exposes_waits_without_leaking_other_team_or_private_state() {
    let f = fixture();
    let principal = f.db.native_agent_host_principal(&f.host).unwrap();
    let other_root = task(&f.db, "other-root");
    let _other = enroll(&f.db, &principal, &other_root, None);
    call(&f.db, &f.member, json!({"action":"wait","reason":"waiting for public protocol","next_step":"implement after protocol"})).unwrap();
    let view = call(&f.db, &f.manager, json!({"action":"status"})).unwrap();
    let team = view["team"].as_array().unwrap();
    assert_eq!(team.len(), 2);
    assert_eq!(view["root_task_id"], f.root);
    assert!(!team.iter().any(|s| s["task_id"] == other_root));
    let member = team.iter().find(|s| s["task_id"] == f.child).unwrap();
    assert_eq!(member["execution_state"], "waiting_dependencies");
    assert_eq!(member["wait_reason"], "waiting for public protocol");
    assert_eq!(member["next_step"], "implement after protocol");
    assert!(member.get("goal").is_none());
    assert!(member.get("memory").is_none());
    publish(&f, "queue-status");
    let view = call(&f.db, &f.member, json!({"action":"status"})).unwrap();
    assert_eq!(view["execution_state"], "queued");
    assert_eq!(view["team"].as_array().unwrap().len(), 2);
}

#[test]
fn route_target_validation_is_read_only_and_rejects_other_host_or_stale_run() {
    let f = fixture();
    publish(&f, "route-target");
    let candidates = candidates(&f);
    let current: TeamContinuationTarget =
        serde_json::from_value(candidates["candidates"][0]["target"].clone()).unwrap();
    assert_eq!(
        f.db.team_continuation_validate_target(OWNER, PROJECT, &f.host, &current)
            .unwrap()["current"],
        true
    );
    assert!(f
        .db
        .team_continuation_validate_target(OWNER, PROJECT, "other-host", &current)
        .is_err());
    let mut stale = current.clone();
    stale.generation += 1;
    assert!(f
        .db
        .team_continuation_validate_target(OWNER, PROJECT, &f.host, &stale)
        .is_err());
    let conn = f.db.conn.lock().unwrap();
    let attempts: i64 = conn
        .query_row("SELECT COUNT(*) FROM wc_team_event_attempts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(attempts, 0);
}

#[test]
fn public_task_activity_is_scoped_read_only_and_missing_observation_is_not_running() {
    let f = fixture();
    let principal = crate::agent_isolation::enrollment_principal_for_execution(OWNER);
    assert_eq!(
        f.db.public_team_activity(&principal, PROJECT, &f.child)
            .unwrap()["execution_state"],
        "idle"
    );
    let count: i64 =
        f.db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_team_activity", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 0);
    assert!(f
        .db
        .public_team_activity(&principal, "other-project", &f.child)
        .is_err());
    assert!(f
        .db
        .public_team_activity(
            &crate::agent_isolation::enrollment_principal_for_execution("other-owner"),
            PROJECT,
            &f.child
        )
        .is_err());
    call(
        &f.db,
        &f.member,
        json!({"action":"wait","reason":"public dependency","next_step":"review protocol"}),
    )
    .unwrap();
    let view =
        f.db.public_team_activity(&principal, PROJECT, &f.child)
            .unwrap();
    assert_eq!(view["execution_state"], "waiting_dependencies");
    assert_eq!(view["wait_reason"], "public dependency");
    assert!(view.get("goal").is_none());
    assert!(view.get("memory").is_none());
}

#[test]
fn old_event_acknowledgement_does_not_clear_newer_blocked_work() {
    let f = fixture();
    let first = publish(&f, "first")["sequence"].as_i64().unwrap();
    call(&f.db, &f.member, json!({"action":"read"})).unwrap();
    let latest = publish(&f, "later")["sequence"].as_i64().unwrap();
    let candidate = candidates(&f);
    let target: TeamContinuationTarget =
        serde_json::from_value(candidate["candidates"][0]["target"].clone()).unwrap();
    f.db.team_continuation_observe(OWNER, PROJECT, &f.host, &target, "draft")
        .unwrap();
    assert_eq!(
        call(
            &f.db,
            &f.member,
            json!({"action":"acknowledge","through_sequence":first})
        )
        .unwrap()["execution_state"],
        "blocked"
    );
    call(
        &f.db,
        &f.member,
        json!({"action":"read","after_sequence":first}),
    )
    .unwrap();
    assert_eq!(
        call(
            &f.db,
            &f.member,
            json!({"action":"acknowledge","through_sequence":latest})
        )
        .unwrap()["execution_state"],
        "idle"
    );
}
