use super::*;
use crate::{AgentEnrollmentPolicy, AgentEnrollmentPurpose, NewAgentEnrollment, NewAgentTask};
use webcodex_core::runner_protocol::{AgentExecutionTarget, AgentRunRef};

const PROJECT: &str = "agent:runner:program-fixture";
struct Observed {
    binding: AgentProgramBinding,
    job: webcodex_core::runner_protocol::ShellJobInfo,
}
impl AuthenticatedAgentProgramObservation for Observed {
    fn binding(&self) -> &AgentProgramBinding {
        &self.binding
    }
    fn job(&self) -> &webcodex_core::runner_protocol::ShellJobInfo {
        &self.job
    }
}
fn enrolled(db: &Database, owner: &str) -> VerifiedAgentRun {
    let principal = crate::agent_isolation::enrollment_principal_for_execution(owner);
    let task = db
        .create_agent_task(
            &principal,
            NewAgentTask {
                title: "Program fixture".into(),
                instruction: "Synthetic ordinary program only".into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(PROJECT.into()),
                idempotency_key: owner.into(),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let mut e = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Manager,
                owner_user: owner.into(),
                project: PROJECT.into(),
                principal_key: owner.into(),
                task_id: task.clone(),
                task_directory: format!("workspace/{task}"),
                brief: "Run synthetic test".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 0,
                },
                expires_at_unix_ms: now() + 60000,
            },
            None,
            owner,
            now(),
        )
        .unwrap();
    db.claim_agent_enrollment(
        owner,
        owner,
        &e.enrollment_id,
        &e.grant.take().unwrap().into_secret(),
        now(),
    )
    .unwrap()
    .binding
}
fn start(db: &Database, run: &VerifiedAgentRun, suffix: &str) -> AgentProgramStart {
    let e = db.read_agent_enrollment(run).unwrap();
    let mut start = AgentProgramStart {
        binding: AgentProgramBinding {
            local_service_port: None,
            resume_from_program: None,
            version: 1,
            program_id: format!("program-{suffix}"),
            enrollment_id: e.enrollment_id,
            task_id: e.task_id,
            run: AgentRunRef {
                agent_id: run.agent_id().into(),
                agent_run_id: run.run_id().into(),
                generation: run.generation(),
            },
            project: run.project().into(),
            target: AgentExecutionTarget {
                client_id: "runner".into(),
                runner_instance_id: "runner-instance".into(),
            },
            job_id: format!("job-{suffix}"),
            start_request_id: format!("request-{suffix}"),
            workspace_project: PROJECT.into(),
            workspace_root: "E:\\synthetic-authorized-project".into(),
            relative_cwd: "app".into(),
            resource_set_id: "python".into(),
            resource_manifest_hash: "a".repeat(64),
            launch_intent_hash: "b".repeat(64),
            expires_at_unix_ms: run.expires_at_unix_ms(),
        },
        args: vec!["-m".into(), "unittest".into()],
        stdin: None,
        timeout_secs: 60,
    };
    start.binding.launch_intent_hash = start.compute_launch_hash().unwrap();
    start
}

#[test]
fn ordinary_program_handoff_waits_for_real_terminal_evidence_and_closes_new_admission() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let run = enrolled(&db, "owner");
    let start = start(&db, &run, "first");
    db.prepare_agent_program(&run, start.clone(), "key")
        .unwrap();
    db.claim_agent_program_dispatch(&run, &start).unwrap();
    let plan = db
        .begin_agent_execution_close("owner", PROJECT, &start.binding.run, now() + 30000)
        .unwrap();
    let progress = db
        .agent_execution_close_status("owner", PROJECT, &start.binding.run)
        .unwrap();
    assert!(!progress.complete);
    assert_eq!(progress.unresolved_attempts, 1);
    assert!(db.claim_agent_program_dispatch(&run, &start).is_err());
    let job=serde_json::from_value(serde_json::json!({"job_id":start.binding.job_id,"client_id":"runner","request_id":start.binding.start_request_id,"command_preview":"synthetic fixture","status":"lost","created_at":0,"command_execution_state":"outcome_unknown"})).unwrap();
    let mut observed = Observed {
        binding: start.binding.clone(),
        job,
    };
    assert_eq!(
        db.record_agent_program_observation(&run, &observed)
            .unwrap()
            .state,
        "dispatch_fenced"
    );
    assert!(
        !db.agent_execution_close_status("owner", PROJECT, &start.binding.run)
            .unwrap()
            .complete
    );
    observed.job.status = "stopped".into();
    observed.job.exit_code = Some(1);
    observed.job.command_execution_state =
        Some(webcodex_core::runner_protocol::ShellCommandExecutionState::Completed);
    assert_eq!(
        db.record_agent_program_observation(&run, &observed)
            .unwrap()
            .state,
        "terminal"
    );
    assert!(
        db.agent_execution_close_status("owner", PROJECT, &start.binding.run)
            .unwrap()
            .complete
    );
    assert_eq!(plan.close_ref().source, start.binding.run);
}

#[test]
fn ordinary_program_resume_requires_stopped_same_task_and_fences_network_changes() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let run = enrolled(&db, "owner");
    let first = start(&db, &run, "first");
    db.prepare_agent_program(&run, first.clone(), "first")
        .unwrap();
    db.claim_agent_program_dispatch(&run, &first).unwrap();
    let mut next = start(&db, &run, "next");
    next.binding.resume_from_program = Some(first.binding.program_id.clone());
    next.binding.launch_intent_hash = next.compute_launch_hash().unwrap();
    assert!(db
        .prepare_agent_program(&run, next.clone(), "next")
        .is_err());
    let job=serde_json::from_value(serde_json::json!({"job_id":first.binding.job_id,"client_id":"runner","request_id":first.binding.start_request_id,"command_preview":"fixture","status":"stopped","created_at":0,"exit_code":1,"command_execution_state":"completed"})).unwrap();
    db.record_agent_program_observation(
        &run,
        &Observed {
            binding: first.binding.clone(),
            job,
        },
    )
    .unwrap();
    let mut wrong = next.clone();
    wrong.binding.relative_cwd = "other".into();
    wrong.binding.launch_intent_hash = wrong.compute_launch_hash().unwrap();
    assert!(db.prepare_agent_program(&run, wrong, "wrong").is_err());
    db.prepare_agent_program(&run, next.clone(), "next")
        .unwrap();
    let mut changed = next;
    changed.binding.local_service_port = Some(18080);
    changed.binding.launch_intent_hash = changed.compute_launch_hash().unwrap();
    assert_eq!(
        db.prepare_agent_program(&run, changed, "next").unwrap_err(),
        "agent_program_idempotency_conflict"
    );
    let other = enrolled(&db, "other");
    let mut foreign = start(&db, &other, "foreign");
    foreign.binding.resume_from_program = Some(first.binding.program_id);
    foreign.binding.launch_intent_hash = foreign.compute_launch_hash().unwrap();
    assert!(db
        .prepare_agent_program(&other, foreign, "foreign")
        .is_err());
}

#[test]
fn ordinary_program_pre_dispatch_close_is_cancellation_not_fake_process_success() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let run = enrolled(&db, "owner");
    let start = start(&db, &run, "first");
    db.prepare_agent_program(&run, start.clone(), "key")
        .unwrap();
    db.begin_agent_execution_close("owner", PROJECT, &start.binding.run, now() + 30000)
        .unwrap();
    assert!(
        db.agent_execution_close_status("owner", PROJECT, &start.binding.run)
            .unwrap()
            .complete
    );
    let receipt = db
        .agent_program_start(&run, &start.binding.program_id)
        .unwrap()
        .1;
    assert_eq!(receipt.state, "cancelled");
    assert!(receipt.terminal.is_none());
    assert!(db.claim_agent_program_dispatch(&run, &start).is_err());
}

#[test]
fn ordinary_program_zero_cli_budget_preserves_identity_and_reserves_no_cli_slot() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let run = enrolled(&db, "owner");
    let first = start(&db, &run, "first");
    let receipt = db
        .prepare_agent_program(&run, first.clone(), "same-key")
        .unwrap();
    assert!(!receipt.replayed);
    let replay = db
        .prepare_agent_program(&run, start(&db, &run, "different-generated-id"), "same-key")
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(receipt.program_id, replay.program_id);
    assert_eq!(receipt.job_id, replay.job_id);
    let mut changed = first.clone();
    changed.args.push("changed".into());
    changed.binding.launch_intent_hash = changed.compute_launch_hash().unwrap();
    assert_eq!(
        db.prepare_agent_program(&run, changed, "same-key")
            .unwrap_err(),
        "agent_program_idempotency_conflict"
    );
    assert!(db.claim_agent_program_dispatch(&run, &first).unwrap());
    assert!(!db.claim_agent_program_dispatch(&run, &first).unwrap());
    for table in ["wc_team_cli_attempts", "wc_team_agent_attempts"] {
        assert_eq!(
            db.conn_for_tests()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let reopened = Database::open(&temp.path().join("state.db")).unwrap();
    assert!(!reopened.claim_agent_program_dispatch(&run, &first).unwrap());
    assert_eq!(
        reopened
            .agent_program_start(&run, &first.binding.program_id)
            .unwrap()
            .1
            .state,
        "dispatch_fenced"
    );
}

#[test]
fn ordinary_program_cross_agent_source_and_retired_executor_are_denied() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let run = enrolled(&db, "owner");
    let other = enrolled(&db, "other");
    let first = start(&db, &run, "first");
    db.prepare_agent_program(&run, first.clone(), "key")
        .unwrap();
    assert!(db
        .agent_program_start(&other, &first.binding.program_id)
        .is_err());
    assert!(db.claim_agent_program_dispatch(&other, &first).is_err());
    let mut wrong = start(&db, &run, "wrong");
    wrong.binding.workspace_project = "agent:runner:foreign".into();
    wrong.binding.launch_intent_hash = wrong.compute_launch_hash().unwrap();
    assert!(db.prepare_agent_program(&run, wrong, "wrong").is_err());
    let replacement = db
        .recover_isolated_agent("owner", PROJECT, run.agent_id(), run.generation())
        .unwrap();
    let current = db.authenticate_agent_run(&replacement.token).unwrap();
    assert!(db.claim_agent_program_dispatch(&run, &first).is_err());
    assert!(db.claim_agent_program_dispatch(&current, &first).is_err());
    assert_eq!(
        db.agent_program_start(&current, &first.binding.program_id)
            .unwrap()
            .1
            .state,
        "cancelled"
    );
}

struct BrowserObserved {
    action: webcodex_core::agent_browser::AgentBrowserAction,
    receipt: webcodex_core::agent_browser::AgentBrowserReceipt,
}
impl AuthenticatedAgentBrowserReceipt for BrowserObserved {
    fn action(&self) -> &webcodex_core::agent_browser::AgentBrowserAction {
        &self.action
    }
    fn receipt(&self) -> &webcodex_core::agent_browser::AgentBrowserReceipt {
        &self.receipt
    }
}
#[test]
fn browser_action_fence_survives_restart_and_keeps_foreign_tasks_out() {
    use webcodex_core::agent_browser::{AgentBrowserReceipt, BrowserStep};
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let db = Database::open(&path).unwrap();
    let run = enrolled(&db, "owner");
    let foreign = enrolled(&db, "foreign");
    let mut service = start(&db, &run, "browser");
    service.binding.local_service_port = Some(8088);
    service.binding.launch_intent_hash = service.compute_launch_hash().unwrap();
    db.prepare_agent_program(&run, service.clone(), "service")
        .unwrap();
    let steps = vec![
        BrowserStep::Navigate { path: "/ui".into() },
        BrowserStep::Click {
            accept_dialog: false,
            selector: "#send".into(),
        },
    ];
    assert!(db
        .prepare_agent_browser(&run, &service.binding.program_id, steps.clone(), 10, "key")
        .is_err());
    db.claim_agent_program_dispatch(&run, &service).unwrap();
    let record = db
        .prepare_agent_browser(&run, &service.binding.program_id, steps.clone(), 10, "key")
        .unwrap();
    assert!(db
        .prepare_agent_browser(
            &foreign,
            &service.binding.program_id,
            steps.clone(),
            10,
            "foreign"
        )
        .is_err());
    assert!(db
        .read_agent_browser(&foreign, &record.action.action_id)
        .is_err());
    assert!(db.claim_agent_browser(&run, &record.action).unwrap());
    assert!(!db.claim_agent_browser(&run, &record.action).unwrap());
    assert!(db
        .prepare_agent_browser(
            &run,
            &service.binding.program_id,
            vec![BrowserStep::Screenshot {}],
            10,
            "key"
        )
        .is_err());
    drop(db);
    let db = Database::open(&path).unwrap();
    let replay = db
        .prepare_agent_browser(&run, &service.binding.program_id, steps, 10, "key")
        .unwrap();
    assert_eq!(replay.action, record.action);
    assert!(!db.claim_agent_browser(&run, &record.action).unwrap());
    let receipt = AgentBrowserReceipt {
        action_id: record.action.action_id.clone(),
        identity_hash: record.action.identity_hash().unwrap(),
        state: "outcome_unknown".into(),
        process_id: None,
        exit_code: None,
        result: None,
        error: Some("agent_browser_original_execution_unsettled".into()),
    };
    let mut observed = BrowserObserved {
        action: record.action.clone(),
        receipt,
    };
    db.record_agent_browser(&run, &observed).unwrap();
    assert_eq!(
        pending_on(&db.conn.lock().unwrap(), run.run_id()).unwrap(),
        2
    );
    observed.receipt.state = "failed".into();
    assert!(
        db.record_agent_browser(&run, &observed).is_err(),
        "missing exit/stop evidence must not settle a browser action"
    );
    observed.receipt.process_id = Some(42);
    observed.receipt.exit_code = Some(1);
    assert_eq!(
        db.record_agent_browser(&run, &observed).unwrap().state,
        "terminal"
    );
    assert_eq!(
        pending_on(&db.conn.lock().unwrap(), run.run_id()).unwrap(),
        1
    );
}
#[test]
fn closing_run_cancels_only_pre_dispatch_browser_actions() {
    use webcodex_core::agent_browser::BrowserStep;
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let run = enrolled(&db, "owner");
    let mut service = start(&db, &run, "browser-close");
    service.binding.local_service_port = Some(8088);
    service.binding.launch_intent_hash = service.compute_launch_hash().unwrap();
    db.prepare_agent_program(&run, service.clone(), "service")
        .unwrap();
    db.claim_agent_program_dispatch(&run, &service).unwrap();
    let pending = db
        .prepare_agent_browser(
            &run,
            &service.binding.program_id,
            vec![BrowserStep::Navigate { path: "/".into() }],
            10,
            "pending",
        )
        .unwrap();
    db.begin_agent_execution_close("owner", PROJECT, &service.binding.run, now() + 30000)
        .unwrap();
    assert_eq!(
        db.read_agent_browser(&run, &pending.action.action_id)
            .unwrap()
            .state,
        "cancelled"
    );
    assert!(!db.claim_agent_browser(&run, &pending.action).unwrap());
}
