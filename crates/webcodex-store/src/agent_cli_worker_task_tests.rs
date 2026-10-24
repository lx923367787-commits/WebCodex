//! Synthetic database tests of the real factory, close ledger and Task methods.
//! No Runner process, model, browser, production database or authorization mock.
use crate::*;
use webcodex_core::runner_protocol::{
    compute_launch_intent_hash, AgentExecutionBinding, AgentExecutionCarrier, AgentExecutionClose,
    AgentExecutionCloseReceipt, AgentExecutionProcess, AgentExecutionStart,
    AgentExecutionStopOutcome, AgentExecutionStopReceipt, AgentExecutionTarget, AgentRunRef,
    ShellCommandExecutionState,
};

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn principal() -> CommunicationPrincipal {
    crate::agent_isolation::enrollment_principal_for_execution("observer-test-owner")
}
fn reference(run: &VerifiedAgentRun) -> AgentRunRef {
    AgentRunRef {
        agent_id: run.agent_id().into(),
        agent_run_id: run.run_id().into(),
        generation: run.generation(),
    }
}
fn task(db: &Database, name: &str) -> String {
    db.create_agent_task(
        &principal(),
        NewAgentTask {
            title: name.into(),
            instruction: format!("Public synthetic assignment {name}"),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some("observer-test-project".into()),
            idempotency_key: name.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}

// This fixture stands at the trusted transport boundary explicitly allowed by
// the close API. Its DTO is built from the real persisted close request, never
// accepted through a ModelAgent input or treated as a bearer authorization.
struct AuthenticatedSyntheticStop(AgentExecutionCloseReceipt);
impl crate::agent_execution_close::AuthenticatedAgentExecutionCloseReceipt
    for AuthenticatedSyntheticStop
{
    fn authenticated_receipt(&self) -> &AgentExecutionCloseReceipt {
        &self.0
    }
}
fn stopped(close: &AgentExecutionClose) -> AuthenticatedSyntheticStop {
    AuthenticatedSyntheticStop(AgentExecutionCloseReceipt {
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

struct Fixture {
    _temp: tempfile::TempDir,
    db: Database,
    requester: VerifiedAgentRun,
    worker: VerifiedAgentRun,
    root: String,
    unrelated: String,
    binding: AgentExecutionBinding,
    permit: AgentCliObservationPermit,
    started: AgentPublicTaskMutation,
    attempt_revision: i64,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(&temp.path().join("observer-task-synthetic.db")).unwrap();
        let root = task(&db, "requester");
        let unrelated = task(&db, "unrelated");
        let issued = db
            .issue_agent_enrollment(
                NewAgentEnrollment {
                    purpose: AgentEnrollmentPurpose::Manager,
                    owner_user: "observer-test-owner".into(),
                    project: "observer-test-project".into(),
                    principal_key: "observer-test-host".into(),
                    task_id: root.clone(),
                    task_directory: "workspace/requester".into(),
                    brief: "Public requester assignment".into(),
                    inputs: vec![],
                    room_ids: vec![],
                    policy: AgentEnrollmentPolicy {
                        max_subagents: 0,
                        max_cli_calls: 1,
                    },
                    expires_at_unix_ms: now() + 60_000,
                },
                None,
                "requester-enrollment",
                now(),
            )
            .unwrap();
        let requester = db
            .claim_agent_enrollment(
                "observer-test-host",
                "requester-window",
                &issued.enrollment_id,
                &issued.grant.unwrap().into_secret(),
                now(),
            )
            .unwrap()
            .binding;
        db.start_agent_public_task(&requester, &root, "requester-start")
            .unwrap();
        db.register_agent_team_plan(
            &requester,
            &root,
            0,
            TeamPlanInput {
                manager: TeamAgentPlan {
                    task_id: root.clone(),
                    cli_calls: vec![TeamCliCall {
                        provider_id: "workbuddy".into(),
                        model: "frozen-synthetic-model".into(),
                        effort: Some("high".into()),
                        instruction: Some("Independent executor assignment".into()),
                    }],
                },
                subagents: vec![],
            },
            "plan",
        )
        .unwrap();
        let preparation = db
            .prepare_agent_cli_worker(
                &requester,
                AgentCliPreparationRequest {
                    root_task_id: root.clone(),
                    source_task_id: root.clone(),
                    call_index: 0,
                    idempotency_key: "original-cli-call".into(),
                    fallback_thinking_profile_id: "frozen-recovery-profile".into(),
                    timeout_secs: Some(120),
                    target: AgentExecutionTarget {
                        client_id: "original-client".into(),
                        runner_instance_id: "original-instance".into(),
                    },
                    carrier: AgentExecutionCarrier::Job {
                        job_id: "original-worker-job".into(),
                        start_request_id: "original-worker-start".into(),
                    },
                },
            )
            .unwrap()
            .preparation
            .unwrap();
        let worker = preparation.worker().clone();
        let mut binding = preparation.binding_identity().clone();
        let started = db
            .start_agent_public_task(&worker, &binding.executor.task_id, "worker-start")
            .unwrap();
        let process = AgentExecutionProcess {
            executable: "C:\\synthetic\\worker.exe".into(),
            args: vec!["synthetic-only".into()],
        };
        binding.launch_intent_hash = compute_launch_intent_hash(&process, None, 120).unwrap();
        binding.resource_manifest_hash = "a".repeat(64);
        let dispatch = db
            .finalize_agent_cli_execution(
                &requester,
                preparation,
                AgentExecutionStart {
                    binding: binding.clone(),
                    resource_set_id: "synthetic-resources".into(),
                    process,
                    stdin: None,
                    timeout_secs: 120,
                },
            )
            .unwrap();
        let permit = dispatch.observation_permit();
        let bound = db
            .bind_agent_cli_execution_job(&permit, &binding.target)
            .unwrap();
        Self {
            _temp: temp,
            db,
            requester,
            worker,
            root,
            unrelated,
            binding,
            permit,
            started,
            attempt_revision: bound.observation_revision,
        }
    }
    fn observe(&self, state: ShellCommandExecutionState) {
        let AgentExecutionCarrier::Job { job_id, .. } = &self.binding.carrier else {
            panic!("synthetic Job carrier expected")
        };
        self.db
            .observe_agent_cli_execution(
                &self.permit,
                &self.binding.target,
                self.attempt_revision,
                TeamCliObservation {
                    job_id: Some(job_id.clone()),
                    dispatch_state: TeamCliDispatchState::Terminal,
                    execution_state: Some(state),
                    error_code: None,
                    result_summary: Some("Bounded synthetic observation".into()),
                },
            )
            .unwrap();
    }
    fn prove_stop(&self) {
        let plan = self
            .db
            .begin_agent_execution_close(
                "observer-test-owner",
                "observer-test-project",
                &reference(&self.requester),
                now() + 60_000,
            )
            .unwrap();
        assert_eq!(plan.requests().len(), 1);
        assert_eq!(plan.requests()[0].members, vec![self.binding.clone()]);
        let progress = self
            .db
            .accept_agent_execution_close_receipt(&stopped(&plan.requests()[0]))
            .unwrap();
        assert!(progress.complete);
    }
    fn assert_others_unchanged(&self) {
        assert_eq!(
            self.db
                .read_agent_task(&principal(), &self.root)
                .unwrap()
                .summary
                .state,
            AgentTaskState::Active
        );
        assert_eq!(
            self.db
                .read_agent_task(&principal(), &self.unrelated)
                .unwrap()
                .summary
                .state,
            AgentTaskState::Ready
        );
    }
    fn current_worker_execution(&self) -> AgentPublicTaskExecution {
        crate::agent_task::public_execution_on_connection(
            &self.db.conn_for_tests(),
            &self.binding.executor.task_id,
        )
        .unwrap()
        .unwrap()
    }
}

#[test]
fn observer_task_terminal_observation_without_physical_proof_does_not_complete() {
    let f = Fixture::new();
    f.observe(ShellCommandExecutionState::Completed);
    let error =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Failed,
            Some("Public bounded result"),
            None,
            "complete",
        )
        .unwrap_err();
    assert_eq!(error.code(), "public_task_execution_stop_proof_required");
    assert_eq!(f.current_worker_execution(), f.started.execution);
    f.assert_others_unchanged();
}

#[test]
fn observer_task_retained_permit_finishes_only_worker_after_runs_are_revoked() {
    let f = Fixture::new();
    f.prove_stop();
    for run in [&f.requester, &f.worker] {
        f.db.revoke_agent_run(
            "observer-test-owner",
            "observer-test-project",
            run.agent_id(),
            run.run_id(),
        )
        .unwrap();
    }
    // Late trusted observation is recorded after real persisted revocations.
    f.observe(ShellCommandExecutionState::Completed);
    assert!(f
        .db
        .complete_agent_public_task(
            &f.worker,
            &f.binding.executor.task_id,
            &f.started.execution.execution_id,
            f.started.execution.revision,
            AgentTaskState::Failed,
            Some("Public bounded result"),
            None,
            "model-complete",
        )
        .is_err());
    assert!(f
        .db
        .agent_cli_observation_permit(&f.requester, &f.binding.requester.cli_attempt_id)
        .is_err());
    let forbidden_success = f.db.complete_agent_cli_worker_task(
        &f.permit, f.started.execution.revision, AgentTaskState::Succeeded,
        Some("Public bounded result"), None, "forbidden-success",
    ).unwrap_err();
    assert_eq!(forbidden_success.code(), "public_task_business_review_required");
    assert_eq!(f.current_worker_execution(), f.started.execution);
    let pending = f.db.read_agent_cli_worker_task(&f.permit).unwrap().unwrap();
    assert_eq!(pending.completion_expected_revision, f.started.execution.revision);
    let complete =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Failed,
            Some("Public bounded result"),
            None,
            "complete",
        )
        .unwrap();
    assert_eq!(complete.task.task_id, f.binding.executor.task_id);
    assert_eq!(complete.execution.state, AgentTaskState::Failed);
    assert_eq!(
        complete.execution.revision,
        f.started.execution.revision + 1
    );
    assert_eq!(
        complete.execution.completed_run_id.as_deref(),
        Some(f.worker.run_id())
    );
    assert!(complete.state_changed);
    assert!(!complete.replayed);
    let reopened = Database::open(&f._temp.path().join("observer-task-synthetic.db")).unwrap();
    let terminal = reopened.read_agent_cli_worker_task(&f.permit).unwrap().unwrap();
    assert_eq!(terminal.execution, complete.execution);
    assert_eq!(terminal.completion_expected_revision, f.started.execution.revision);
    let replay =
        reopened.complete_agent_cli_worker_task(
            &f.permit,
            terminal.completion_expected_revision,
            AgentTaskState::Failed,
            Some("Public bounded result"),
            None,
            "complete",
        )
        .unwrap();
    assert!(replay.replayed);
    assert!(!replay.state_changed);
    assert_eq!(replay.execution, complete.execution);
    f.assert_others_unchanged();
}

#[test]
fn observer_task_timeout_cannot_succeed_and_completion_keeps_cas_and_exact_replay() {
    let f = Fixture::new();
    f.observe(ShellCommandExecutionState::TimedOut);
    f.prove_stop();
    let wrong_outcome =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Succeeded,
            None,
            None,
            "complete",
        )
        .unwrap_err();
    assert_eq!(wrong_outcome.code(), "public_task_business_review_required");
    assert_eq!(f.current_worker_execution(), f.started.execution);
    let wrong_revision =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision + 1,
            AgentTaskState::Failed,
            None,
            Some("Execution timed out"),
            "complete",
        )
        .unwrap_err();
    assert_eq!(wrong_revision.code(), "public_task_revision_conflict");
    assert_eq!(f.current_worker_execution(), f.started.execution);
    let complete =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Failed,
            None,
            Some("Execution timed out"),
            "complete",
        )
        .unwrap();
    assert_eq!(complete.execution.state, AgentTaskState::Failed);
    assert_eq!(
        complete.execution.revision,
        f.started.execution.revision + 1
    );
    let conflict =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Failed,
            None,
            Some("Different completion intent"),
            "complete",
        )
        .unwrap_err();
    assert_eq!(conflict.code(), "public_task_idempotency_conflict");
    let replay =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Failed,
            None,
            Some("Execution timed out"),
            "complete",
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.execution, complete.execution);
    f.assert_others_unchanged();
}
