use super::*;
use crate::{AgentEnrollmentPolicy, AgentEnrollmentPurpose, NewAgentEnrollment, NewAgentTask};

fn enroll(db: &Database, key: &str, budget: u32) -> VerifiedAgentRun {
    let owner = "local-cli-test-owner";
    let principal = crate::agent_isolation::enrollment_principal_for_execution(owner);
    let task = db
        .create_agent_task(
            &principal,
            NewAgentTask {
                title: key.into(),
                instruction: "Synthetic CLI test".into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some("agent:runner:test".into()),
                idempotency_key: key.into(),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let now = chrono::Utc::now().timestamp_millis();
    let mut issued = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Manager,
                owner_user: owner.into(),
                project: "agent:runner:test".into(),
                principal_key: "fixture-host".into(),
                task_id: task.clone(),
                task_directory: format!("workspace/{task}"),
                brief: "Synthetic explicit assignment".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: budget,
                },
                expires_at_unix_ms: now + 60_000,
            },
            None,
            key,
            now,
        )
        .unwrap();
    let secret = issued.grant.take().unwrap().into_secret();
    db.claim_agent_enrollment(
        "fixture-host",
        &format!("window-{key}"),
        &issued.enrollment_id,
        &secret,
        now,
    )
    .unwrap()
    .binding
}

fn reserve(
    db: &Database,
    agent: &VerifiedAgentRun,
    key: &str,
    fingerprint: &str,
) -> Result<(LocalCliCall, bool), AgentIsolationError> {
    db.reserve_local_cli_call(
        agent,
        key,
        fingerprint,
        "agent:runner:test",
        "runner",
        "fixture-root",
    )
}

#[test]
fn local_cli_receipts_replay_exact_intent_and_persist_without_refunding_budget() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let db = Database::open(&path).unwrap();
    let agent = enroll(&db, "root", 1);
    let (first, created) = reserve(&db, &agent, "one", &"a".repeat(64)).unwrap();
    assert!(created);
    assert!(!reserve(&db, &agent, "one", &"a".repeat(64)).unwrap().1);
    assert_eq!(
        reserve(&db, &agent, "one", &"b".repeat(64))
            .unwrap_err()
            .code(),
        "local_cli_idempotency_conflict"
    );
    assert_eq!(
        reserve(&db, &agent, "two", &"b".repeat(64))
            .unwrap_err()
            .code(),
        "local_cli_budget_exhausted"
    );
    db.record_local_cli_job(&agent, &first.call_id, "detached-fixture", "completed")
        .unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    let (same, created) = reserve(&db, &agent, "one", &"a".repeat(64)).unwrap();
    assert!(!created);
    assert_eq!(same.call_id, first.call_id);
    assert_eq!(same.state, "completed");
    assert_eq!(
        reserve(&db, &agent, "another", &"a".repeat(64))
            .unwrap_err()
            .code(),
        "local_cli_budget_exhausted"
    );
}

#[test]
fn local_cli_receipts_do_not_cross_agent_or_task_and_revoked_runs_are_denied() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let one = enroll(&db, "one", 2);
    let other = enroll(&db, "other", 2);
    let (call, _) = reserve(&db, &one, "run", &"a".repeat(64)).unwrap();
    assert_eq!(
        db.read_local_cli_call(&other, &call.call_id)
            .unwrap_err()
            .code(),
        "local_cli_call_not_found"
    );
    assert!(db
        .record_local_cli_job(&other, &call.call_id, "foreign-job", "completed")
        .is_err());
    db.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE wc_agent_runs SET state='revoked' WHERE run_id=?1",
            [one.run_id()],
        )
        .unwrap();
    assert!(db.read_local_cli_call(&one, &call.call_id).is_err());
    assert!(reserve(&db, &one, "next", &"a".repeat(64)).is_err());
}

#[test]
fn local_cli_unknown_execution_blocks_completion_and_terminal_receipts_do_not_regress() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let agent = enroll(&db, "one", 1);
    let (call, _) = reserve(&db, &agent, "run", &"a".repeat(64)).unwrap();
    assert!(require_quiescent(&db.conn.lock().unwrap(), &call.task_id).is_err());
    db.record_local_cli_job(&agent, &call.call_id, "job", "unknown")
        .unwrap();
    assert!(require_quiescent(&db.conn.lock().unwrap(), &call.task_id).is_err());
    db.record_local_cli_job(&agent, &call.call_id, "job", "stopped")
        .unwrap();
    require_quiescent(&db.conn.lock().unwrap(), &call.task_id).unwrap();
    assert_eq!(
        db.record_local_cli_job(&agent, &call.call_id, "job", "running")
            .unwrap()
            .state,
        "stopped"
    );
    assert_eq!(
        db.record_local_cli_job(&agent, &call.call_id, "replacement", "completed")
            .unwrap_err()
            .code(),
        "local_cli_job_identity_conflict"
    );
}

#[test]
fn local_cli_requires_existing_cli_authority_and_does_not_store_prompt_or_login() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let denied = enroll(&db, "denied", 0);
    assert!(reserve(&db, &denied, "run", &"a".repeat(64)).is_err());
    let agent = enroll(&db, "allowed", 1);
    let (record, _) = reserve(&db, &agent, "private-replay-key", &"a".repeat(64)).unwrap();
    let raw: String = db
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT record_json FROM wc_local_cli_calls WHERE call_id=?1",
            [record.call_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.contains("private-replay-key"));
    assert!(!raw.contains("stdin"));
    assert!(!raw.contains("args"));
}
