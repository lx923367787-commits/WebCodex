use super::*;
use crate::agent_execution_close::*;
use rusqlite::params;
use serde_json::json;
use std::sync::{Arc, Barrier};
use webcodex_core::runner_protocol::*;

struct AuthenticatedFixture(AgentExecutionCloseReceipt);
impl AuthenticatedAgentExecutionCloseReceipt for AuthenticatedFixture {
    fn authenticated_receipt(&self) -> &AgentExecutionCloseReceipt {
        &self.0
    }
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn reference(run: &VerifiedAgentRun) -> AgentRunRef {
    AgentRunRef {
        agent_id: run.agent_id().into(),
        agent_run_id: run.run_id().into(),
        generation: run.generation(),
    }
}
fn new_task(db: &Database, key: &str) -> String {
    db.create_agent_task(
        &crate::agent_isolation::enrollment_principal_for_execution("owner"),
        NewAgentTask {
            title: key.into(),
            instruction: "Synthetic explicit task".into(),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some("project".into()),
            idempotency_key: key.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}
fn enrolled(
    db: &Database,
    task: &str,
    name: &str,
    purpose: &str,
    source: Option<&VerifiedAgentRun>,
) -> (VerifiedAgentRun, String) {
    let grant = db
        .create_isolated_agent("owner", "project", source.map(|s| s.agent_id()))
        .unwrap();
    let run = db.authenticate_agent_run(&grant.token).unwrap();
    let enrollment = format!("enrollment-{name}");
    let spec = NewAgentEnrollment {
        purpose: match purpose {
            "manager" => AgentEnrollmentPurpose::Manager,
            "member" => AgentEnrollmentPurpose::Member,
            "fallback" => AgentEnrollmentPurpose::Fallback,
            _ => AgentEnrollmentPurpose::CliWorker,
        },
        owner_user: "owner".into(),
        project: "project".into(),
        principal_key: "test-host".into(),
        task_id: task.into(),
        task_directory: format!("workspace/{name}"),
        brief: "Synthetic approved brief".into(),
        inputs: vec![],
        room_ids: vec![],
        policy: AgentEnrollmentPolicy {
            max_subagents: 0,
            max_cli_calls: if purpose == "manager" { 2 } else { 0 },
        },
        expires_at_unix_ms: now() + 60_000,
    };
    let conn = db.conn.lock().unwrap();
    conn.execute("INSERT INTO wc_agent_enrollments(enrollment_id,owner_user,project,principal_key,agent_id,run_id,purpose,task_id,task_directory,policy_json,request_json,request_hash,idempotency_hash,secret_hash,source_agent_id,source_run_id,source_generation,expires_at_unix_ms,state,created_at_unix_ms) VALUES(?1,'owner','project','test-host',?2,?3,?4,?5,?6,?7,?8,?9,?9,?9,?10,?11,?12,?13,'claimed',?14)",params![enrollment,run.agent_id(),run.run_id(),purpose,task,spec.task_directory,serde_json::to_string(&spec.policy).unwrap(),serde_json::to_string(&spec).unwrap(),name,source.map(|s|s.agent_id()),source.map(|s|s.run_id()),source.map(|s|s.generation()),spec.expires_at_unix_ms,now()]).unwrap();
    conn.execute(
        "INSERT INTO wc_agent_enrollment_assignments VALUES(?1,'owner','project',?2,'active')",
        params![run.agent_id(), task],
    )
    .unwrap();
    (run, enrollment)
}
struct Fixture {
    _temp: tempfile::TempDir,
    db: Arc<Database>,
    requester: VerifiedAgentRun,
    worker: VerifiedAgentRun,
    binding: AgentExecutionBinding,
}
fn fixture() -> Fixture {
    fixture_with_binding(true)
}
fn fixture_with_binding(bind: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(&temp.path().join("state.db")).unwrap());
    let task = new_task(&db, "requester");
    let worker_task = new_task(&db, "worker");
    let (requester, requester_enrollment) = enrolled(&db, &task, "requester", "manager", None);
    let (worker, worker_enrollment) =
        enrolled(&db, &worker_task, "worker", "cli_worker", Some(&requester));
    let principal = crate::agent_isolation::enrollment_principal_for_execution("owner");
    db.register_team_plan(
        &principal,
        "project",
        &task,
        0,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: task.clone(),
                cli_calls: vec![
                    TeamCliCall {
                        provider_id: "workbuddy".into(),
                        model: "synthetic-model".into(),
                        effort: Some("high".into()),
                        instruction: Some("First explicit subtask".into()),
                    },
                    TeamCliCall {
                        provider_id: "workbuddy".into(),
                        model: "synthetic-model".into(),
                        effort: Some("high".into()),
                        instruction: Some("Second explicit subtask".into()),
                    },
                ],
            },
            subagents: vec![],
        },
        "plan",
    )
    .unwrap();
    let attempt = db
        .reserve_team_cli_attempt(
            &principal,
            "project",
            &task,
            &task,
            0,
            "slot",
            "synthetic-profile",
            Some(60),
        )
        .unwrap()
        .attempt;
    // The integrated factory migration owns the preparation columns.
    let binding = AgentExecutionBinding {
        version: 1,
        project: "project".into(),
        requester: AgentExecutionRequester {
            enrollment_id: requester_enrollment,
            run: reference(&requester),
            root_task_id: task.clone(),
            source_task_id: task,
            call_index: 0,
            cli_attempt_id: attempt.attempt_id.clone(),
        },
        executor: AgentExecutionExecutor {
            enrollment_id: worker_enrollment,
            run: reference(&worker),
            task_id: worker_task,
            cli_session_scope_id: "scope-worker".into(),
        },
        target: AgentExecutionTarget {
            client_id: "original-client".into(),
            runner_instance_id: "original-instance".into(),
        },
        carrier: AgentExecutionCarrier::Job {
            job_id: "original-job".into(),
            start_request_id: "original-start".into(),
        },
        launch_intent_hash: "a".repeat(64),
        resource_manifest_hash: "b".repeat(64),
    };
    let mut record = serde_json::to_value(attempt).unwrap();
    if bind {
        record["execution_binding"] = json!(binding);
        record["dispatch_state"] = "dispatching".into();
    }
    db.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE wc_team_cli_attempts SET record_json=?2 WHERE attempt_id=?1",
            params![binding.requester.cli_attempt_id, record.to_string()],
        )
        .unwrap();
    Fixture {
        _temp: temp,
        db,
        requester,
        worker,
        binding,
    }
}
fn receipt(close: &AgentExecutionClose) -> AuthenticatedFixture {
    AuthenticatedFixture(AgentExecutionCloseReceipt {
        version: 1,
        close_ref: close.close_ref.clone(),
        target: close.target.clone(),
        deadline_at_ms: close.deadline_at_ms,
        members_sha256: close.members_sha256.clone(),
        observation_revision: 2,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: close
            .members
            .iter()
            .map(|binding| AgentExecutionStopReceipt {
                binding_sha256: binding.binding_hash().unwrap(),
                carrier: binding.carrier.clone(),
                observation_revision: 1,
                outcome: AgentExecutionStopOutcome::TreeExited {
                    leader_exit_code: None,
                    leader_handle_signaled: true,
                    membership_complete: true,
                    member_handles_signaled: true,
                    worker_finished: true,
                },
            })
            .collect(),
    })
}
fn begin(f: &Fixture) -> AgentExecutionClosePlan {
    f.db.begin_agent_execution_close("owner", "project", &reference(&f.requester), now() + 30_000)
        .unwrap()
}

#[test]
fn close_fences_both_runs_and_needs_exact_original_runner_proof() {
    let f = fixture();
    f.db.validate_agent_execution_admission(&f.binding).unwrap();
    let plan = begin(&f);
    assert_eq!(plan.requests().len(), 1);
    assert!(f.db.validate_agent_execution_admission(&f.binding).is_err());
    assert!(f
        .db
        .recover_isolated_agent("owner", "project", f.requester.agent_id(), 1)
        .is_err());
    assert!(f
        .db
        .revoke_agent_run("owner", "project", f.worker.agent_id(), f.worker.run_id())
        .is_err());
    let mut wrong = receipt(&plan.requests()[0]);
    wrong.0.target.runner_instance_id = "replacement".into();
    assert!(f.db.accept_agent_execution_close_receipt(&wrong).is_err());
    let mut missing = receipt(&plan.requests()[0]);
    missing.0.receipts.clear();
    assert!(f.db.accept_agent_execution_close_receipt(&missing).is_err());
    let proof = receipt(&plan.requests()[0]);
    assert!(
        f.db.accept_agent_execution_close_receipt(&proof)
            .unwrap()
            .complete
    );
    assert!(
        f.db.accept_agent_execution_close_receipt(&proof)
            .unwrap()
            .complete
    );
    let mut stale = receipt(&plan.requests()[0]);
    stale.0.observation_revision = 1;
    assert_eq!(
        f.db.accept_agent_execution_close_receipt(&stale)
            .unwrap_err()
            .code(),
        "agent_execution_close_receipt_stale"
    );
    let newer =
        f.db.recover_isolated_agent("owner", "project", f.requester.agent_id(), 1)
            .unwrap();
    assert_eq!(newer.agent.agent_id, f.requester.agent_id());
    assert_eq!(newer.generation, 2);
    assert!(f.db.validate_agent_execution_admission(&f.binding).is_err());
}

#[test]
fn close_is_durable_across_reopen_and_cannot_reopen_by_sql() {
    let f = fixture();
    let plan = begin(&f);
    let reopened = Database::open(&f._temp.path().join("state.db")).unwrap();
    let again = reopened
        .begin_agent_execution_close("owner", "project", &reference(&f.requester), now() + 90_000)
        .unwrap();
    assert_eq!(again.close_ref(), plan.close_ref());
    assert_eq!(again.requests(), plan.requests());
    assert!(reopened
        .validate_agent_execution_admission(&f.binding)
        .is_err());
    assert!(reopened
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE wc_agent_runs SET execution_close_id=NULL WHERE run_id=?1",
            [f.worker.run_id()]
        )
        .is_err());
    assert!(
        reopened
            .accept_agent_execution_close_receipt(&receipt(&plan.requests()[0]))
            .unwrap()
            .complete
    );
    assert!(
        f.db.agent_execution_close_status("owner", "project", &reference(&f.requester))
            .unwrap()
            .complete
    );
}

#[test]
fn empty_close_is_persisted_and_prepare_does_not_revoke_source() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let grant = db.create_isolated_agent("owner", "project", None).unwrap();
    let run = db.authenticate_agent_run(&grant.token).unwrap();
    let pending = db.begin_agent_run_handoff(&run).unwrap();
    db.validate_agent_run(&run).unwrap();
    db.activate_agent_run("owner", "project", run.agent_id(), &pending.run_id)
        .unwrap();
    assert!(
        db.agent_execution_close_status("owner", "project", &reference(&run))
            .unwrap()
            .complete
    );
    assert!(db.authenticate_agent_run(&grant.token).is_err());
}

#[test]
fn closing_executor_alone_includes_its_carrier_without_closing_requester() {
    let f = fixture();
    let plan =
        f.db.begin_agent_execution_close("owner", "project", &reference(&f.worker), now() + 30_000)
            .unwrap();
    assert_eq!(plan.requests()[0].members, vec![f.binding.clone()]);
    {
        let conn = f.db.conn.lock().unwrap();
        ensure_agent_run_execution_open_on_connection(&conn, &reference(&f.requester)).unwrap();
    }
    assert!(
        f.db.accept_agent_execution_close_receipt(&receipt(&plan.requests()[0]))
            .unwrap()
            .complete
    );
    // Parent closure may consume the already durable child exit fact.
    assert!(begin(&f).requests().is_empty());
    assert!(
        f.db.agent_execution_close_status("owner", "project", &reference(&f.requester))
            .unwrap()
            .complete
    );
}

#[test]
fn reserved_preparation_is_cancelled_and_worker_can_never_finalize() {
    let f = fixture_with_binding(false);
    {
        let conn = f.db.conn.lock().unwrap();
        let mut prepared = f.binding.clone();
        prepared.launch_intent_hash.clear();
        prepared.resource_manifest_hash.clear();
        conn.execute("UPDATE wc_team_cli_attempts SET record_json=json_set(record_json,'$.execution_binding',NULL,'$.dispatch_state','reserved'),execution_preparation_json=?2,execution_preparation_state='prepared' WHERE attempt_id=?1",params![f.binding.requester.cli_attempt_id,serde_json::to_string(&prepared).unwrap()]).unwrap();
    }
    assert_eq!(
        f.db.recover_isolated_agent("owner", "project", f.requester.agent_id(), 1)
            .unwrap_err()
            .code(),
        "agent_execution_close_required"
    );
    f.db.validate_agent_run(&f.requester).unwrap();
    let plan = begin(&f);
    assert!(plan.requests().is_empty());
    assert!(
        f.db.agent_execution_close_status("owner", "project", &reference(&f.requester))
            .unwrap()
            .complete
    );
    let conn = f.db.conn.lock().unwrap();
    assert!(ensure_agent_run_execution_open_on_connection(&conn, &reference(&f.worker)).is_err());
    let state:String=conn.query_row("SELECT json_extract(record_json,'$.dispatch_state') FROM wc_team_cli_attempts WHERE attempt_id=?1",[&f.binding.requester.cli_attempt_id],|r|r.get(0)).unwrap();
    assert_eq!(state, "terminal");
    let prepared_state: String = conn
        .query_row(
            "SELECT execution_preparation_state FROM wc_team_cli_attempts WHERE attempt_id=?1",
            [&f.binding.requester.cli_attempt_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(prepared_state, "cancelled");
    let worker_state: String = conn
        .query_row(
            "SELECT state FROM wc_agent_runs WHERE run_id=?1",
            [f.worker.run_id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(worker_state, "revoked");
}

#[test]
fn unknown_without_binding_blocks_close_completion_and_keeps_assignment() {
    let f = fixture_with_binding(false);
    f.db.conn.lock().unwrap().execute("UPDATE wc_team_cli_attempts SET record_json=json_set(record_json,'$.execution_binding',NULL,'$.dispatch_state','unknown') WHERE attempt_id=?1",[&f.binding.requester.cli_attempt_id]).unwrap();
    let plan = begin(&f);
    assert_eq!(
        plan.blocked_attempt_ids(),
        &[f.binding.requester.cli_attempt_id.clone()]
    );
    assert!(
        !f.db
            .agent_execution_close_status("owner", "project", &reference(&f.requester))
            .unwrap()
            .complete
    );
    let conn = f.db.conn.lock().unwrap();
    assert!(require_agent_assignment_execution_closed_on_connection(
        &conn,
        "owner",
        "project",
        f.worker.agent_id()
    )
    .is_err());
    let state: String = conn
        .query_row(
            "SELECT state FROM wc_agent_enrollment_assignments WHERE agent_id=?1",
            [f.worker.agent_id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "active");
}

#[test]
fn close_serializes_with_an_independent_connection_admission() {
    let f = fixture();
    let ready = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let path = f._temp.path().join("state.db");
    let binding = f.binding.clone();
    let r = ready.clone();
    let q = release.clone();
    let other = Database::open(&path).unwrap();
    let thread = std::thread::spawn(move || {
        let mut conn = other.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        validate_agent_execution_admission_on_connection(&tx, &binding).unwrap();
        r.wait();
        q.wait();
        tx.commit().unwrap();
    });
    ready.wait();
    let db = f.db.clone();
    let source = reference(&f.requester);
    let closer = std::thread::spawn(move || {
        db.begin_agent_execution_close("owner", "project", &source, now() + 30_000)
            .unwrap()
    });
    release.wait();
    thread.join().unwrap();
    let plan = closer.join().unwrap();
    assert_eq!(plan.requests()[0].members, vec![f.binding.clone()]);
    assert!(f.db.validate_agent_execution_admission(&f.binding).is_err());
}

#[test]
fn proof_cannot_cross_generation_or_changed_frozen_binding() {
    let f = fixture();
    let plan = begin(&f);
    let mut wrong = receipt(&plan.requests()[0]);
    wrong.0.close_ref.source.generation += 1;
    assert!(f.db.accept_agent_execution_close_receipt(&wrong).is_err());
    // The combined factory migration now rejects mutation before a changed
    // binding can enter a stored receipt comparison. Keep that stronger gate.
    assert!(f.db.conn.lock().unwrap().execute("UPDATE wc_team_cli_attempts SET record_json=json_set(record_json,'$.execution_binding.resource_manifest_hash',?2) WHERE attempt_id=?1",params![f.binding.requester.cli_attempt_id,"c".repeat(64)]).is_err());
    let mut altered = receipt(&plan.requests()[0]);
    altered.0.receipts[0].binding_sha256 = "c".repeat(64);
    assert!(f.db.accept_agent_execution_close_receipt(&altered).is_err());
    assert!(
        f.db.accept_agent_execution_close_receipt(&receipt(&plan.requests()[0]))
            .unwrap()
            .complete
    );
}

#[test]
fn pending_handoff_keeps_source_active_but_cannot_activate_before_close_proof() {
    let f = fixture();
    let successor = f.db.begin_agent_run_handoff(&f.requester).unwrap();
    f.db.validate_agent_run(&f.requester).unwrap();
    assert!(f
        .db
        .activate_agent_run(
            "owner",
            "project",
            f.requester.agent_id(),
            &successor.run_id
        )
        .is_err());
    f.db.validate_agent_run(&f.requester).unwrap();
    let plan = begin(&f);
    assert!(f
        .db
        .activate_agent_run(
            "owner",
            "project",
            f.requester.agent_id(),
            &successor.run_id
        )
        .is_err());
    f.db.accept_agent_execution_close_receipt(&receipt(&plan.requests()[0]))
        .unwrap();
    let active =
        f.db.activate_agent_run(
            "owner",
            "project",
            f.requester.agent_id(),
            &successor.run_id,
        )
        .unwrap();
    assert_eq!(active.agent_id(), f.requester.agent_id());
    assert_eq!(active.run_id(), successor.run_id);
    assert!(f.db.validate_agent_run(&f.requester).is_err());
}

#[test]
fn all_original_targets_are_required_and_worker_task_scope_uses_physical_proof() {
    let f = fixture();
    let task = new_task(&f.db, "worker-two");
    let (worker, enrollment) =
        enrolled(&f.db, &task, "worker-two", "cli_worker", Some(&f.requester));
    let mut second = f.binding.clone();
    second.requester.call_index = 1;
    second.requester.cli_attempt_id = "wc_team_cli_attempt_second".into();
    second.executor = AgentExecutionExecutor {
        enrollment_id: enrollment,
        run: reference(&worker),
        task_id: task.clone(),
        cli_session_scope_id: "scope-two".into(),
    };
    second.target = AgentExecutionTarget {
        client_id: "second-client".into(),
        runner_instance_id: "second-instance".into(),
    };
    second.carrier = AgentExecutionCarrier::Job {
        job_id: "second-job".into(),
        start_request_id: "second-request".into(),
    };
    {
        let conn = f.db.conn.lock().unwrap();
        let source: String = conn
            .query_row(
                "SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1",
                [&f.binding.requester.cli_attempt_id],
                |r| r.get(0),
            )
            .unwrap();
        let mut record: serde_json::Value = serde_json::from_str(&source).unwrap();
        record["attempt_id"] = json!(second.requester.cli_attempt_id);
        record["call_index"] = 1.into();
        record["execution_binding"] = json!(second);
        conn.execute("INSERT INTO wc_team_cli_attempts(attempt_id,root_task_id,source_task_id,project_id,owner_principal_kind,owner_principal_digest,call_index,request_key_hash,request_fingerprint,record_json,created_at_unix_ms) SELECT ?2,root_task_id,source_task_id,project_id,owner_principal_kind,owner_principal_digest,1,'second-key','second-fingerprint',?3,created_at_unix_ms FROM wc_team_cli_attempts WHERE attempt_id=?1",params![f.binding.requester.cli_attempt_id,second.requester.cli_attempt_id,record.to_string()]).unwrap();
        assert!(require_agent_task_execution_quiescent_on_connection(
            &conn, "owner", "project", &task
        )
        .is_err());
    }
    f.db.validate_agent_execution_admission(&second).unwrap();
    let plan = begin(&f);
    assert_eq!(plan.requests().len(), 2);
    let first = &plan.requests()[0];
    let last = &plan.requests()[1];
    let state =
        f.db.accept_agent_execution_close_receipt(&receipt(first))
            .unwrap();
    assert!(!state.complete);
    assert_eq!(state.accepted_targets, 1);
    assert!(f
        .db
        .recover_isolated_agent("owner", "project", f.requester.agent_id(), 1)
        .is_err());
    {
        let conn = f.db.conn.lock().unwrap();
        assert!(
            agent_execution_binding_stop_proven_on_connection(&conn, &first.members[0]).unwrap()
        );
        assert!(
            !agent_execution_binding_stop_proven_on_connection(&conn, &last.members[0]).unwrap()
        );
        require_agent_task_execution_quiescent_on_connection(
            &conn,
            "owner",
            "project",
            &first.members[0].executor.task_id,
        )
        .unwrap();
        assert!(require_agent_task_execution_quiescent_on_connection(
            &conn,
            "owner",
            "project",
            &f.binding.requester.source_task_id
        )
        .is_err());
    }
    assert!(
        f.db.accept_agent_execution_close_receipt(&receipt(last))
            .unwrap()
            .complete
    );
    let conn = f.db.conn.lock().unwrap();
    require_agent_task_execution_quiescent_on_connection(
        &conn,
        "owner",
        "project",
        &f.binding.requester.source_task_id,
    )
    .unwrap();
}

#[test]
fn undelivered_reserved_member_survives_but_explicit_delivery_follows_new_source_run() {
    let f = fixture();
    let task = new_task(&f.db, "queued-member");
    let (member, enrollment) =
        enrolled(&f.db, &task, "queued-member", "member", Some(&f.requester));
    {
        let conn = f.db.conn.lock().unwrap();
        // The integrated enrollment migration owns the delivery columns.
        conn.execute("UPDATE wc_agent_enrollments SET state='issued',delivery_state='reserved' WHERE enrollment_id=?1",[&enrollment]).unwrap();
        conn.execute(
            "UPDATE wc_agent_runs SET state='pending' WHERE run_id=?1",
            [member.run_id()],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM wc_agent_enrollment_assignments WHERE agent_id=?1",
            [member.agent_id()],
        )
        .unwrap();
    }
    let plan = begin(&f);
    {
        let conn = f.db.conn.lock().unwrap();
        ensure_agent_run_execution_open_on_connection(&conn, &reference(&member)).unwrap();
    }
    f.db.accept_agent_execution_close_receipt(&receipt(&plan.requests()[0]))
        .unwrap();
    let next =
        f.db.recover_isolated_agent("owner", "project", f.requester.agent_id(), 1)
            .unwrap();
    let next = f.db.authenticate_agent_run(&next.token).unwrap();
    {
        let conn = f.db.conn.lock().unwrap();
        conn.execute("UPDATE wc_agent_enrollments SET delivery_state='delivery_unknown',delivery_source_run_id=?2,delivery_source_generation=?3 WHERE enrollment_id=?1",params![enrollment,next.run_id(),next.generation()]).unwrap();
        let historical: String = conn
            .query_row(
                "SELECT source_run_id FROM wc_agent_enrollments WHERE enrollment_id=?1",
                [&enrollment],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(historical, f.requester.run_id());
    }
    let next_plan =
        f.db.begin_agent_execution_close("owner", "project", &reference(&next), now() + 30_000)
            .unwrap();
    assert!(next_plan.requests().is_empty());
    let conn = f.db.conn.lock().unwrap();
    assert!(ensure_agent_run_execution_open_on_connection(&conn, &reference(&member)).is_err());
}
