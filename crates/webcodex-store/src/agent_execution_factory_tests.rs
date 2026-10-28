use super::*;
use webcodex_core::runner_protocol::{compute_launch_intent_hash, AgentExecutionProcess};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Database,
    manager: VerifiedAgentRun,
    root: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_instruction("Independent CLI task")
    }
    fn with_instruction(instruction: &str) -> Self {
        Self::with_inputs(instruction, vec![])
    }
    fn with_inputs(instruction: &str, inputs: Vec<crate::AgentEnrollmentInput>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("synthetic.db")).unwrap();
        let root = db
            .create_agent_task(
                &crate::agent_isolation::enrollment_principal_for_execution("owner"),
                crate::NewAgentTask {
                    title: "Manager task".into(),
                    instruction: "Public manager task".into(),
                    assignee_agent_id: None,
                    source_conversation_id: None,
                    source_message_id: None,
                    referenced_project_id: Some("project".into()),
                    idempotency_key: "root".into(),
                },
            )
            .unwrap()
            .task
            .summary
            .task_id;
        let issued = db
            .issue_agent_enrollment(
                NewAgentEnrollment {
                    purpose: AgentEnrollmentPurpose::Manager,
                    owner_user: "owner".into(),
                    project: "project".into(),
                    principal_key: "principal".into(),
                    task_id: root.clone(),
                    task_directory: "workspace/root".into(),
                    brief: "Public task".into(),
                    inputs: vec![],
                    room_ids: vec![],
                    policy: AgentEnrollmentPolicy {
                        max_subagents: 1,
                        max_cli_calls: 2,
                    },
                    expires_at_unix_ms: now_unix_ms() + 60_000,
                },
                None,
                "manager",
                now_unix_ms(),
            )
            .unwrap();
        let manager = db
            .claim_agent_enrollment(
                "principal",
                "window-manager",
                &issued.enrollment_id,
                &issued.grant.unwrap().into_secret(),
                now_unix_ms(),
            )
            .unwrap()
            .binding;
        db.register_agent_team_plan(
            &manager,
            &root,
            0,
            TeamPlanInput {
                manager: TeamAgentPlan {
                    task_id: root.clone(),
                    cli_calls: vec![TeamCliCall {
                        inputs,
                        provider_id: "workbuddy".into(),
                        model: "frozen-model".into(),
                        effort: Some("high".into()),
                        instruction: Some(instruction.into()),
                    }],
                },
                subagents: vec![],
            },
            "plan",
        )
        .unwrap();
        Self {
            _dir: dir,
            db,
            manager,
            root,
        }
    }
    fn request(&self) -> AgentCliPreparationRequest {
        AgentCliPreparationRequest {
            root_task_id: self.root.clone(),
            source_task_id: self.root.clone(),
            call_index: 0,
            idempotency_key: "cli".into(),
            fallback_thinking_profile_id: "frozen-profile".into(),
            timeout_secs: Some(120),
            target: AgentExecutionTarget {
                client_id: "client-1".into(),
                runner_instance_id: "runner-1".into(),
            },
            carrier: AgentExecutionCarrier::Job {
                job_id: "job-1".into(),
                start_request_id: "request-1".into(),
            },
        }
    }
    fn member() -> AgentMemberRequest {
        AgentMemberRequest {
            title: "Child task".into(),
            brief: "Public child task".into(),
            task_directory: "workspace/child".into(),
            inputs: vec![],
            room_ids: vec![],
            policy: AgentEnrollmentPolicy {
                max_subagents: 0,
                max_cli_calls: 1,
            },
        }
    }
}
fn start(preparation: &AgentCliPreparation) -> AgentExecutionStart {
    let mut binding = preparation.binding_identity().clone();
    let process = AgentExecutionProcess {
        executable: "C:\\synthetic\\worker.exe".into(),
        args: vec!["run".into()],
    };
    binding.launch_intent_hash = compute_launch_intent_hash(&process, None, 120).unwrap();
    binding.resource_manifest_hash = "a".repeat(64);
    AgentExecutionStart {
        workspace_source: None,
        binding,
        resource_set_id: "resources-1".into(),
        process,
        stdin: None,
        timeout_secs: 120,
    }
}

#[test]
fn factory_member_is_atomic_bounded_and_grant_starts_at_delivery() {
    let f = Fixture::new();
    let receipt =
        f.db.create_agent_member(&f.manager, Fixture::member(), "child")
            .unwrap();
    let replay =
        f.db.create_agent_member(&f.manager, Fixture::member(), "child")
            .unwrap();
    assert!(replay.replayed);
    assert_eq!(receipt.agent_id, replay.agent_id);
    let status =
        f.db.agent_enrollment_status("owner", "project", &receipt.enrollment_id)
            .unwrap();
    assert_eq!(status.delivery_state, "reserved");
    assert_eq!(status.claim_expires_at_unix_ms, 0);
    assert!(!status.claim_expired);
    assert!(!serde_json::to_string(&receipt).unwrap().contains("wc_en_"));
    let before: i64 =
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r.get(0))
            .unwrap();
    assert!(f
        .db
        .create_agent_member(&f.manager, Fixture::member(), "over-cap")
        .is_ok());
    assert_eq!(
        before + 1,
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
    );
    let issued =
        f.db.issue_agent_enrollment_delivery(
            &f.manager,
            &receipt.enrollment_id,
            "delivery",
            now_unix_ms(),
        )
        .unwrap();
    let secret = issued.grant.unwrap().into_secret();
    assert!(f
        .db
        .issue_agent_enrollment_delivery(
            &f.manager,
            &receipt.enrollment_id,
            "delivery",
            now_unix_ms()
        )
        .unwrap()
        .grant
        .is_none());
    let claim =
        f.db.claim_agent_enrollment(
            "principal",
            "child-window",
            &receipt.enrollment_id,
            &secret,
            now_unix_ms(),
        )
        .unwrap();
    assert_eq!(claim.binding.agent_id(), receipt.agent_id);
}

#[test]
fn factory_cli_preserves_multiline_frozen_instruction_and_identity_on_replay() {
    let instruction = format!(
        "Review architecture.\r\n\tKeep every byte.\n{}",
        "x".repeat(8000)
    );
    let f = Fixture::with_instruction(&instruction);
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let worker = first.preparation.unwrap();
    let own = f.db.read_agent_enrollment(worker.worker()).unwrap();
    assert_eq!(own.brief, instruction);
    assert_eq!(own.purpose, AgentEnrollmentPurpose::CliWorker);
    assert_eq!(own.policy.max_subagents, 0);
    assert_eq!(own.policy.max_cli_calls, 0);
    let replay =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    assert_eq!(
        worker.binding_identity(),
        replay.preparation.unwrap().binding_identity()
    );
    let mut member = Fixture::member();
    member.brief = "A label\0with a NUL".into();
    assert_eq!(
        f.db.create_agent_member(&f.manager, member, "invalid-member")
            .unwrap_err()
            .code(),
        "agent_enrollment_invalid_input"
    );
}

#[test]
fn factory_cli_keeps_original_slot_and_independent_identity_on_replay() {
    let f = Fixture::new();
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let a = first.preparation.unwrap();
    let second =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let b = second.preparation.unwrap();
    assert_eq!(a.binding_identity(), b.binding_identity());
    assert_ne!(a.worker().agent_id(), f.manager.agent_id());
    assert_ne!(a.binding_identity().executor.task_id, f.root);
    let own = f.db.read_agent_enrollment(a.worker()).unwrap();
    assert_eq!(own.purpose, AgentEnrollmentPurpose::CliWorker);
    assert_eq!(own.policy.max_cli_calls, 0);
    assert_eq!(own.policy.max_subagents, 0);
    assert_eq!(
        1,
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
    );
    assert_eq!(
        1,
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_agent_host_windows", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
    );
    assert!(f
        .db
        .claim_team_cli_dispatch(
            &crate::agent_isolation::enrollment_principal_for_execution("owner"),
            &first.receipt.attempt_id
        )
        .is_err());
    let launch = start(&a);
    let permit =
        f.db.finalize_agent_cli_execution(&f.manager, a, launch.clone())
            .unwrap();
    assert_eq!(permit.binding(), &launch.binding);
    assert!(f
        .db
        .finalize_agent_cli_execution(&f.manager, b, launch)
        .is_err());
    let replay =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    assert_eq!(replay.phase, "finalized");
    assert!(replay.preparation.is_none());
}

#[test]
fn factory_close_cancels_prepared_without_dispatch_and_late_finalize_fails() {
    let f = Fixture::new();
    let prepared =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let p = prepared.preparation.unwrap();
    let launch = start(&p);
    let close =
        f.db.begin_agent_execution_close(
            "owner",
            "project",
            &run_ref(&f.manager),
            now_unix_ms() + 60_000,
        )
        .unwrap();
    assert!(close.requests().is_empty());
    assert!(f
        .db
        .finalize_agent_cli_execution(&f.manager, p, launch)
        .is_err());
    let phase: String =
        f.db.conn_for_tests()
            .query_row(
                "SELECT execution_preparation_state FROM wc_team_cli_attempts WHERE attempt_id=?1",
                [&prepared.receipt.attempt_id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(phase, "cancelled");
    assert_eq!(
        1,
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
    );
}

#[test]
fn factory_forged_target_or_wrong_task_leaves_no_new_identity() {
    let f = Fixture::new();
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let before: i64 =
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_isolated_agents", [], |r| r.get(0))
            .unwrap();
    let mut request = f.request();
    request.target.client_id = "other-client".into();
    assert!(f.db.prepare_agent_cli_worker(&f.manager, request).is_err());
    let mut request = f.request();
    request.source_task_id = "wc_agent_task_other".into();
    assert!(f.db.prepare_agent_cli_worker(&f.manager, request).is_err());
    assert_eq!(
        before,
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_isolated_agents", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
    );
    let p = first.preparation.unwrap();
    let mut launch = start(&p);
    launch.binding.executor.task_id = f.root.clone();
    assert!(f
        .db
        .finalize_agent_cli_execution(&f.manager, p, launch)
        .is_err());
}

#[test]
fn factory_long_queue_delivery_renews_only_the_original_unadmitted_run() {
    let f = Fixture::new();
    let receipt =
        f.db.create_agent_member(&f.manager, Fixture::member(), "child")
            .unwrap();
    f.db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET expires_at_unix_ms=?2 WHERE run_id=?1",
            params![receipt.run_id, now_unix_ms() - 86_400_000],
        )
        .unwrap();
    let issued =
        f.db.issue_agent_enrollment_delivery(
            &f.manager,
            &receipt.enrollment_id,
            "delivery",
            now_unix_ms(),
        )
        .unwrap();
    assert_eq!(issued.agent_id, receipt.agent_id);
    assert_eq!(issued.run_id, receipt.run_id);
    let claim =
        f.db.claim_agent_enrollment(
            "principal",
            "child-window",
            &receipt.enrollment_id,
            &issued.grant.unwrap().into_secret(),
            now_unix_ms(),
        )
        .unwrap();
    assert_eq!(claim.binding.run_id(), receipt.run_id);
    assert!(claim.binding.expires_at_unix_ms() > now_unix_ms());
}

#[test]
fn factory_reserved_member_survives_explicit_manager_handoff_without_new_identity() {
    let f = Fixture::new();
    let receipt =
        f.db.create_agent_member(&f.manager, Fixture::member(), "child")
            .unwrap();
    let pending = f.db.begin_agent_run_handoff(&f.manager).unwrap();
    f.db.begin_agent_execution_close(
        "owner",
        "project",
        &run_ref(&f.manager),
        now_unix_ms() + 60_000,
    )
    .unwrap();
    let next =
        f.db.activate_agent_run("owner", "project", f.manager.agent_id(), &pending.run_id)
            .unwrap();
    f.db.register_agent_host_window("principal", "next-manager-window", &next)
        .unwrap();
    assert!(f
        .db
        .issue_agent_enrollment_delivery(
            &f.manager,
            &receipt.enrollment_id,
            "delivery",
            now_unix_ms()
        )
        .is_err());
    let issued = f
        .db
        .issue_agent_enrollment_delivery(&next, &receipt.enrollment_id, "delivery", now_unix_ms())
        .unwrap();
    assert_eq!(issued.agent_id, receipt.agent_id);
    assert_eq!(issued.run_id, receipt.run_id);
    let source:(String,String)=f.db.conn_for_tests().query_row("SELECT source_run_id,delivery_source_run_id FROM wc_agent_enrollments WHERE enrollment_id=?1",[&receipt.enrollment_id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(source.0, f.manager.run_id());
    assert_eq!(source.1, next.run_id());
    let claim =
        f.db.claim_agent_enrollment(
            "principal",
            "child-after-handoff",
            &receipt.enrollment_id,
            &issued.grant.unwrap().into_secret(),
            now_unix_ms(),
        )
        .unwrap();
    assert_eq!(claim.binding.agent_id(), receipt.agent_id);
}

#[test]
fn factory_retained_observation_permit_reconciles_revoked_runs_without_send_authority() {
    let f = Fixture::new();
    let p =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap();
    let launch = start(&p);
    let permit =
        f.db.finalize_agent_cli_execution(&f.manager, p, launch.clone())
            .unwrap();
    let watch = permit.observation_permit();
    let bound =
        f.db.bind_agent_cli_execution_job(&watch, &launch.binding.target)
            .unwrap();
    // Synthetic persisted revocation models late observations only; this is not
    // a fabricated physical-stop receipt and cannot complete any public Task.
    f.db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET state='revoked' WHERE run_id IN (?1,?2)",
            params![
                launch.binding.requester.run.agent_run_id,
                launch.binding.executor.run.agent_run_id
            ],
        )
        .unwrap();
    assert!(f
        .db
        .authorize_agent_assignment(&f.manager, &f.root, AgentAssignmentOperation::TaskComplete)
        .is_err());
    assert!(f
        .db
        .agent_cli_observation_permit(&f.manager, &bound.attempt_id)
        .is_err());
    let observation = TeamCliObservation {
        job_id: bound.job_id.clone(),
        dispatch_state: TeamCliDispatchState::Terminal,
        execution_state: Some(ShellCommandExecutionState::Completed),
        error_code: None,
        result_summary: Some("bounded trusted result".into()),
    };
    let mut wrong = launch.binding.target.clone();
    wrong.runner_instance_id = "other-instance".into();
    assert!(f
        .db
        .observe_agent_cli_execution(
            &watch,
            &wrong,
            bound.observation_revision,
            observation.clone()
        )
        .is_err());
    let terminal =
        f.db.observe_agent_cli_execution(
            &watch,
            &launch.binding.target,
            bound.observation_revision,
            observation,
        )
        .unwrap();
    assert_eq!(terminal.dispatch_state, TeamCliDispatchState::Terminal);
    assert_eq!(terminal.attempt_id, bound.attempt_id);
    assert_eq!(terminal.execution_binding, Some(launch.binding));
}

// Append inside existing agent_execution_factory_tests.rs module after P1.
// These are uncompiled drafts using that module's real Fixture/start helpers.
#[test]
fn factory_cancelled_resume_preserves_worker_identity_and_slot() {
    let f = Fixture::new();
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let p = first.preparation.unwrap();
    let old = p.binding_identity().clone();
    let pending = f.db.begin_agent_run_handoff(&f.manager).unwrap();
    f.db.begin_agent_execution_close(
        "owner",
        "project",
        &run_ref(&f.manager),
        now_unix_ms() + 60_000,
    )
    .unwrap();
    let next =
        f.db.activate_agent_run("owner", "project", f.manager.agent_id(), &pending.run_id)
            .unwrap();
    f.db.register_agent_host_window("principal", "next-window", &next)
        .unwrap();
    let fence =
        f.db.cancelled_agent_cli_preparation_hash(&next, &first.receipt.attempt_id)
            .unwrap();
    assert_eq!(fence, old.identity_hash().unwrap());
    assert!(f
        .db
        .resume_cancelled_agent_cli_preparation(&f.manager, &first.receipt.attempt_id, &fence)
        .is_err());
    assert!(f
        .db
        .resume_cancelled_agent_cli_preparation(&next, &first.receipt.attempt_id, &"0".repeat(64))
        .is_err());
    let resumed =
        f.db.resume_cancelled_agent_cli_preparation(&next, &first.receipt.attempt_id, &fence)
            .unwrap()
            .preparation
            .unwrap();
    let b = resumed.binding_identity();
    assert_eq!(b.executor.run.agent_id, old.executor.run.agent_id);
    assert_eq!(b.executor.task_id, old.executor.task_id);
    assert_eq!(b.executor.enrollment_id, old.executor.enrollment_id);
    assert_eq!(b.executor.run.generation, old.executor.run.generation + 1);
    assert_eq!(b.requester.cli_attempt_id, old.requester.cli_attempt_id);
    assert_ne!(b.executor.run.agent_run_id, old.executor.run.agent_run_id);
    let replay =
        f.db.resume_cancelled_agent_cli_preparation(&next, &first.receipt.attempt_id, &fence)
            .unwrap()
            .preparation
            .unwrap();
    assert_eq!(replay.binding_identity(), b);
    assert_eq!(
        1,
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
    );
    assert_eq!(
        2,
        f.db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_isolated_agents", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
    );
    let launch = start(&resumed);
    let permit =
        f.db.finalize_agent_cli_execution(&next, resumed, launch.clone())
            .unwrap();
    assert_eq!(permit.binding(), &launch.binding);
    assert!(f
        .db
        .resume_cancelled_agent_cli_preparation(&next, &first.receipt.attempt_id, &fence)
        .is_err());
    assert!(f
        .db
        .finalize_agent_cli_execution(&next, replay, launch)
        .is_err());
}

// Append to canonical agent_execution_factory_tests.rs. Reuses its real Fixture,
// request(), start(), run_ref() and now_unix_ms(); introduces no production hook.

fn resume_regression_successor(
    db: &Database,
    current: &VerifiedAgentRun,
    window: &str,
) -> VerifiedAgentRun {
    let pending = db.begin_agent_run_handoff(current).unwrap();
    let plan = db
        .begin_agent_execution_close(
            "owner",
            "project",
            &run_ref(current),
            now_unix_ms() + 60_000,
        )
        .unwrap();
    assert!(
        plan.requests().is_empty(),
        "only a never-dispatched preparation exists"
    );
    assert!(plan.blocked_attempt_ids().is_empty());
    assert!(
        db.agent_execution_close_status("owner", "project", &run_ref(current))
            .unwrap()
            .complete
    );
    let next = db
        .activate_agent_run("owner", "project", current.agent_id(), &pending.run_id)
        .unwrap();
    db.register_agent_host_window("principal", window, &next)
        .unwrap();
    next
}

#[derive(Debug, PartialEq, Eq)]
struct ResumeRegressionSnapshot {
    record_json: String,
    preparation_json: String,
    history_json: String,
    request_key_hash: String,
    request_fingerprint: String,
    worker_runs: i64,
    active_worker_runs: i64,
    attempts: i64,
    agents: i64,
    tasks: i64,
}
fn resume_regression_snapshot(
    db: &Database,
    attempt_id: &str,
    worker_agent_id: &str,
) -> ResumeRegressionSnapshot {
    db.conn_for_tests()
        .query_row(
            "SELECT record_json,execution_preparation_json,execution_preparation_history_json,
              request_key_hash,request_fingerprint,
              (SELECT COUNT(*) FROM wc_agent_runs WHERE agent_id=?2),
              (SELECT COUNT(*) FROM wc_agent_runs WHERE agent_id=?2 AND state='active'),
              (SELECT COUNT(*) FROM wc_team_cli_attempts),
              (SELECT COUNT(*) FROM wc_isolated_agents),
              (SELECT COUNT(*) FROM wc_agent_tasks)
             FROM wc_team_cli_attempts WHERE attempt_id=?1",
            params![attempt_id, worker_agent_id],
            |r| {
                Ok(ResumeRegressionSnapshot {
                    record_json: r.get(0)?,
                    preparation_json: r.get(1)?,
                    history_json: r.get(2)?,
                    request_key_hash: r.get(3)?,
                    request_fingerprint: r.get(4)?,
                    worker_runs: r.get(5)?,
                    active_worker_runs: r.get(6)?,
                    attempts: r.get(7)?,
                    agents: r.get(8)?,
                    tasks: r.get(9)?,
                })
            },
        )
        .unwrap()
}

#[test]
fn factory_resume_regression_two_connections_create_only_one_successor_worker_run() {
    let f = Fixture::new();
    let original =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap();
    let identity = original.binding_identity().clone();
    let attempt = identity.requester.cli_attempt_id.clone();
    let fence = identity.identity_hash().unwrap();
    let next = resume_regression_successor(&f.db, &f.manager, "concurrent-next-window");
    let before = resume_regression_snapshot(&f.db, &attempt, &identity.executor.run.agent_id);
    let cancelled: TeamCliAttemptRecord = serde_json::from_str(&before.record_json).unwrap();
    assert_eq!(cancelled.dispatch_state, TeamCliDispatchState::Terminal);
    assert_eq!(
        cancelled.execution_state,
        Some(ShellCommandExecutionState::NotStarted)
    );
    assert_eq!(before.worker_runs, 1);
    assert_eq!(before.active_worker_runs, 0);

    // Two Database objects mean two separate SQLite connections and mutexes.
    // Open the second before racing; schema setup is not part of the race.
    let second_db = Database::open(&f._dir.path().join("synthetic.db")).unwrap();
    let barrier = std::sync::Barrier::new(3);
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            barrier.wait();
            f.db.resume_cancelled_agent_cli_preparation(&next, &attempt, &fence)
        });
        let right = scope.spawn(|| {
            barrier.wait();
            second_db.resume_cancelled_agent_cli_preparation(&next, &attempt, &fence)
        });
        barrier.wait();
        (
            left.join().unwrap().unwrap(),
            right.join().unwrap().unwrap(),
        )
    });
    let left = left.preparation.unwrap();
    let right = right.preparation.unwrap();
    assert_eq!(left.binding_identity(), right.binding_identity());
    let resumed = left.binding_identity();
    assert_eq!(
        resumed.executor.run.agent_id,
        identity.executor.run.agent_id
    );
    assert_eq!(resumed.executor.task_id, identity.executor.task_id);
    assert_eq!(
        resumed.executor.enrollment_id,
        identity.executor.enrollment_id
    );
    assert_eq!(
        resumed.executor.run.generation,
        identity.executor.run.generation + 1
    );
    assert_eq!(resumed.requester.cli_attempt_id, attempt);
    assert_eq!(resumed.requester.run, run_ref(&next));

    let after = resume_regression_snapshot(&f.db, &attempt, &identity.executor.run.agent_id);
    let record: TeamCliAttemptRecord = serde_json::from_str(&after.record_json).unwrap();
    let history: Vec<AgentExecutionBinding> = serde_json::from_str(&after.history_json).unwrap();
    assert_eq!(after.worker_runs, before.worker_runs + 1);
    assert_eq!(after.active_worker_runs, 1);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.agents, before.agents);
    assert_eq!(after.tasks, before.tasks);
    assert_eq!(after.request_key_hash, before.request_key_hash);
    assert_eq!(after.request_fingerprint, before.request_fingerprint);
    assert_eq!(history, vec![identity]);
    assert_eq!(
        record.observation_revision,
        cancelled.observation_revision + 1
    );
    assert_eq!(record.dispatch_state, TeamCliDispatchState::Reserved);
    assert!(record.execution_binding.is_none());
    assert!(record.job_id.is_none());
    assert_eq!(
        after,
        resume_regression_snapshot(&second_db, &attempt, &resumed.executor.run.agent_id)
    );
}

#[test]
fn factory_resume_regression_stale_round_cas_and_revoked_requester_are_rejected() {
    let f = Fixture::new();
    let initial =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap()
            .binding_identity()
            .clone();
    let attempt = initial.requester.cli_attempt_id.clone();
    let first_fence = initial.identity_hash().unwrap();
    let second_requester = resume_regression_successor(&f.db, &f.manager, "second-window");
    let second =
        f.db.resume_cancelled_agent_cli_preparation(&second_requester, &attempt, &first_fence)
            .unwrap()
            .preparation
            .unwrap()
            .binding_identity()
            .clone();
    let third_requester = resume_regression_successor(&f.db, &second_requester, "third-window");
    let second_fence =
        f.db.cancelled_agent_cli_preparation_hash(&third_requester, &attempt)
            .unwrap();
    assert_eq!(second_fence, second.identity_hash().unwrap());
    assert_ne!(first_fence, second_fence);
    let before = resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id);
    let mut wrong_fence = second_fence.clone();
    wrong_fence.replace_range(
        ..1,
        if second_fence.starts_with('0') {
            "1"
        } else {
            "0"
        },
    );
    for fence in [&first_fence, &wrong_fence] {
        assert!(f
            .db
            .resume_cancelled_agent_cli_preparation(&third_requester, &attempt, fence)
            .is_err());
        assert_eq!(
            before,
            resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id)
        );
    }
    assert!(f
        .db
        .resume_cancelled_agent_cli_preparation(&second_requester, &attempt, &second_fence)
        .is_err());
    assert_eq!(
        before,
        resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id)
    );
    let third =
        f.db.resume_cancelled_agent_cli_preparation(&third_requester, &attempt, &second_fence)
            .unwrap()
            .preparation
            .unwrap();
    assert_eq!(
        third.binding_identity().executor.run.agent_id,
        initial.executor.run.agent_id
    );
    assert_eq!(
        third.binding_identity().executor.run.generation,
        initial.executor.run.generation + 2
    );
    let after = resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id);
    assert_eq!(after.worker_runs, before.worker_runs + 1);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.agents, before.agents);
    assert_eq!(after.tasks, before.tasks);
    assert_eq!(after.request_key_hash, before.request_key_hash);
    let history: Vec<AgentExecutionBinding> = serde_json::from_str(&after.history_json).unwrap();
    assert_eq!(history, vec![initial, second]);
}

#[test]
fn factory_resume_regression_finalized_and_unknown_never_reopen_original_slot() {
    let f = Fixture::new();
    let initial =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap()
            .binding_identity()
            .clone();
    let attempt = initial.requester.cli_attempt_id.clone();
    let fence = initial.identity_hash().unwrap();
    let next = resume_regression_successor(&f.db, &f.manager, "finalized-next-window");
    let resumed =
        f.db.resume_cancelled_agent_cli_preparation(&next, &attempt, &fence)
            .unwrap()
            .preparation
            .unwrap();
    let launch = start(&resumed);
    let dispatch =
        f.db.finalize_agent_cli_execution(&next, resumed, launch.clone())
            .unwrap();
    let watch = dispatch.observation_permit();
    let finalized = resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id);
    let record: TeamCliAttemptRecord = serde_json::from_str(&finalized.record_json).unwrap();
    assert_eq!(record.dispatch_state, TeamCliDispatchState::Dispatching);
    assert!(record.execution_binding.is_some());
    assert!(f
        .db
        .resume_cancelled_agent_cli_preparation(&next, &attempt, &fence)
        .is_err());
    assert!(f
        .db
        .cancelled_agent_cli_preparation_hash(&next, &attempt)
        .is_err());
    assert_eq!(
        finalized,
        resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id)
    );

    f.db.observe_agent_cli_execution(
        &watch,
        &launch.binding.target,
        record.observation_revision,
        TeamCliObservation {
            job_id: None,
            dispatch_state: TeamCliDispatchState::Unknown,
            execution_state: Some(ShellCommandExecutionState::OutcomeUnknown),
            error_code: None,
            result_summary: None,
        },
    )
    .unwrap();
    let unknown = resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id);
    let record: TeamCliAttemptRecord = serde_json::from_str(&unknown.record_json).unwrap();
    assert_eq!(record.dispatch_state, TeamCliDispatchState::Unknown);
    assert_eq!(record.execution_binding, Some(launch.binding));
    assert!(f
        .db
        .resume_cancelled_agent_cli_preparation(&next, &attempt, &fence)
        .is_err());
    assert!(f
        .db
        .cancelled_agent_cli_preparation_hash(&next, &attempt)
        .is_err());
    assert_eq!(
        unknown,
        resume_regression_snapshot(&f.db, &attempt, &initial.executor.run.agent_id)
    );
    assert_eq!(unknown.worker_runs, finalized.worker_runs);
    assert_eq!(unknown.attempts, finalized.attempts);
    assert_eq!(unknown.request_key_hash, finalized.request_key_hash);
}

// Append only to agent_execution_factory_tests.rs. Reuses its Fixture, start,
// run_ref, resume_regression_successor and resume_regression_snapshot helpers.

#[test]
fn factory_delivery_recovery_preserves_historical_worker_close_after_resume_and_handoff() {
    use webcodex_core::runner_protocol::{
        AgentExecutionCloseReceipt, AgentExecutionStopOutcome, AgentExecutionStopReceipt,
    };

    struct OriginalRunnerReceipt(AgentExecutionCloseReceipt);
    impl crate::agent_execution_close::AuthenticatedAgentExecutionCloseReceipt
        for OriginalRunnerReceipt
    {
        fn authenticated_receipt(&self) -> &AgentExecutionCloseReceipt {
            &self.0
        }
    }

    let f = Fixture::new();
    f.db.start_agent_public_task(&f.manager, &f.root, "manager-start")
        .unwrap();
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap();
    let first_binding = first.binding_identity().clone();
    let attempt = first_binding.requester.cli_attempt_id.clone();
    let fence = first_binding.identity_hash().unwrap();
    let second_manager =
        resume_regression_successor(&f.db, &f.manager, "delivery-history-second-window");
    let first_tombstone = || {
        f.db.conn_for_tests()
            .query_row(
                "SELECT execution_close_id,execution_closed_at_unix_ms,state
             FROM wc_agent_runs WHERE run_id=?1 AND agent_id=?2 AND generation=?3",
                params![
                    first_binding.executor.run.agent_run_id,
                    first_binding.executor.run.agent_id,
                    first_binding.executor.run.generation
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap()
    };
    let original_tombstone = first_tombstone();
    assert_eq!(original_tombstone.2, "revoked");
    let cancelled =
        resume_regression_snapshot(&f.db, &attempt, &first_binding.executor.run.agent_id);
    let cancelled_record: TeamCliAttemptRecord =
        serde_json::from_str(&cancelled.record_json).unwrap();
    assert_eq!(
        cancelled_record.dispatch_state,
        TeamCliDispatchState::Terminal
    );
    assert_eq!(
        cancelled_record.execution_state,
        Some(ShellCommandExecutionState::NotStarted)
    );
    assert!(cancelled_record.execution_binding.is_none());

    let resumed =
        f.db.resume_cancelled_agent_cli_preparation(&second_manager, &attempt, &fence)
            .unwrap()
            .preparation
            .unwrap();
    let second_worker = resumed.worker().clone();
    assert_eq!(
        second_worker.agent_id(),
        first_binding.executor.run.agent_id
    );
    assert_ne!(
        second_worker.run_id(),
        first_binding.executor.run.agent_run_id
    );
    assert_eq!(
        second_worker.generation(),
        first_binding.executor.run.generation + 1
    );
    let launch = start(&resumed);
    assert_eq!(
        launch.binding.executor.task_id,
        first_binding.executor.task_id
    );
    assert_eq!(
        launch.binding.executor.enrollment_id,
        first_binding.executor.enrollment_id
    );
    let dispatch =
        f.db.finalize_agent_cli_execution(&second_manager, resumed, launch.clone())
            .unwrap();
    let worker_started =
        f.db.start_agent_public_task(
            &second_worker,
            &launch.binding.executor.task_id,
            "worker-start",
        )
        .unwrap();
    let observer = dispatch.observation_permit();
    let bound =
        f.db.bind_agent_cli_execution_job(&observer, &launch.binding.target)
            .unwrap();
    let AgentExecutionCarrier::Job { job_id, .. } = &launch.binding.carrier else {
        panic!("original Job fixture expected")
    };
    f.db.observe_agent_cli_execution(
        &observer,
        &launch.binding.target,
        bound.observation_revision,
        TeamCliObservation {
            job_id: Some(job_id.clone()),
            dispatch_state: TeamCliDispatchState::Terminal,
            execution_state: Some(ShellCommandExecutionState::Completed),
            error_code: None,
            result_summary: Some("Public result from resumed worker W2".into()),
        },
    )
    .unwrap();

    // This snapshot also detects writes to any unrelated table through SQLite's
    // per-connection change counter, plus schema creation and delivery rows.
    let read_snapshot = || {
        let attempt_snapshot =
            resume_regression_snapshot(&f.db, &attempt, &first_binding.executor.run.agent_id);
        let persisted: (String, i64, i64, i64, i64, i64) = f.db.conn_for_tests().query_row(
            "SELECT
             (SELECT json_group_array(json_object('run_id',run_id,'agent_id',agent_id,
                'generation',generation,'state',state,'close_id',execution_close_id,
                'closed_at',execution_closed_at_unix_ms,'plan',execution_close_plan_json,
                'receipts',execution_close_receipts_json)) FROM (SELECT * FROM wc_agent_runs ORDER BY run_id)),
             (SELECT COUNT(*) FROM sqlite_schema),
             (SELECT COUNT(*) FROM wc_agent_meeting_rooms),
             (SELECT COUNT(*) FROM wc_agent_meeting_artifacts),
             (SELECT COUNT(*) FROM wc_agent_meeting_entries),total_changes()",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        ).unwrap();
        (attempt_snapshot, persisted)
    };
    let before = read_snapshot();
    assert!(f
        .db
        .read_agent_cli_delivery_stop_evidence(&second_manager, &launch.binding)
        .is_err());
    assert_eq!(read_snapshot(), before);

    let second_plan =
        f.db.begin_agent_execution_close(
            "owner",
            "project",
            &run_ref(&second_manager),
            now_unix_ms() + 60_000,
        )
        .unwrap();
    let second_runs: Vec<webcodex_core::runner_protocol::AgentRunRef> =
        serde_json::from_value(serde_json::to_value(&second_plan).unwrap()["runs"].clone())
            .unwrap();
    assert!(second_runs.contains(&first_binding.executor.run));
    assert!(second_runs.contains(&launch.binding.executor.run));
    assert!(second_runs.contains(&run_ref(&second_manager)));
    assert_eq!(second_plan.requests().len(), 1);
    let second_close = second_plan.requests()[0].clone();
    assert_eq!(second_close.members, vec![launch.binding.clone()]);
    assert_ne!(second_close.close_ref.close_id, original_tombstone.0);
    assert_eq!(first_tombstone(), original_tombstone);
    let before = read_snapshot();
    assert!(f
        .db
        .read_agent_cli_delivery_stop_evidence(&second_manager, &launch.binding)
        .is_err());
    assert_eq!(read_snapshot(), before);

    let accepted = OriginalRunnerReceipt(AgentExecutionCloseReceipt {
        version: 1,
        close_ref: second_close.close_ref.clone(),
        target: second_close.target.clone(),
        deadline_at_ms: second_close.deadline_at_ms,
        members_sha256: second_close.members_sha256.clone(),
        observation_revision: 1,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: second_close
            .members
            .iter()
            .map(|member| AgentExecutionStopReceipt {
                binding_sha256: member.binding_hash().unwrap(),
                carrier: member.carrier.clone(),
                observation_revision: 1,
                outcome: AgentExecutionStopOutcome::TreeExited {
                    leader_exit_code: Some(0),
                    leader_handle_signaled: true,
                    membership_complete: true,
                    member_handles_signaled: true,
                    worker_finished: true,
                },
            })
            .collect(),
    });
    assert!(
        f.db.accept_agent_execution_close_receipt(&accepted)
            .unwrap()
            .complete
    );
    assert_eq!(first_tombstone(), original_tombstone);
    let pending = f.db.begin_agent_run_handoff(&second_manager).unwrap();
    let third_manager =
        f.db.activate_agent_run(
            "owner",
            "project",
            second_manager.agent_id(),
            &pending.run_id,
        )
        .unwrap();
    f.db.register_agent_host_window("principal", "delivery-history-third-window", &third_manager)
        .unwrap();
    assert_eq!(third_manager.agent_id(), f.manager.agent_id());
    assert_eq!(third_manager.generation(), f.manager.generation() + 2);

    let before = read_snapshot();
    let recovered =
        f.db.read_agent_cli_delivery_stop_evidence(&third_manager, &launch.binding)
            .unwrap();
    assert_eq!(recovered.binding(), &launch.binding);
    assert_eq!(recovered.close(), &second_close);
    assert_eq!(recovered.receipt(), &accepted.0);
    assert_eq!(recovered.binding().executor.run, run_ref(&second_worker));
    assert_ne!(recovered.close().close_ref.close_id, original_tombstone.0);
    assert_eq!(first_tombstone(), original_tombstone);
    assert!(f
        .db
        .read_agent_cli_delivery_stop_evidence(&second_manager, &launch.binding)
        .is_err());
    assert_eq!(read_snapshot(), before);
    assert_eq!(before.1 .2, 0);
    assert_eq!(before.1 .3, 0);
    assert_eq!(before.1 .4, 0);
    assert_eq!(before.0.attempts, 1);
    assert_eq!(before.0.agents, 2);
    assert_eq!(before.0.worker_runs, 2);
    assert_eq!(
        f.db.read_agent_cli_worker_task(&observer)
            .unwrap()
            .unwrap()
            .execution,
        worker_started.execution
    );
}

#[test]
fn factory_cli_carries_exact_frozen_input_snapshot_without_host_file_access() {
    let f = Fixture::with_inputs(
        "Review supplied data",
        vec![crate::AgentEnrollmentInput {
            name: "source.js".into(),
            content: "export const answer = 42;\n".into(),
        }],
    );
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap();
    let own = f.db.read_agent_enrollment(first.worker()).unwrap();
    assert_eq!(own.inputs.len(), 1);
    assert_eq!(own.inputs[0].name, "source.js");
    assert_eq!(own.inputs[0].content, "export const answer = 42;\n");
    assert_eq!(
        own.inputs[0].sha256,
        webcodex_core::agent_cli::sha256(own.inputs[0].content.as_bytes())
    );
    let replay =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap();
    assert_eq!(
        f.db.read_agent_enrollment(replay.worker()).unwrap().inputs,
        own.inputs
    );
}
