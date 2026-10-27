use super::*;
use crate::{AgentEnrollmentPolicy, AgentEnrollmentPurpose, NewAgentEnrollment};

const PROJECT: &str = "agent:runner:public-task";
fn fixture() -> (
    tempfile::TempDir,
    Database,
    crate::VerifiedAgentRun,
    String,
    CommunicationPrincipal,
) {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let owner = "public-task-test-user";
    let principal = crate::agent_isolation::managed_user_principal(owner);
    let task = db
        .create_agent_task(
            &principal,
            NewAgentTask {
                title: "Public task".into(),
                instruction: "Explicit public brief".into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(PROJECT.into()),
                idempotency_key: "task".into(),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let now = now_unix_ms();
    let mut issued = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Manager,
                owner_user: owner.into(),
                project: PROJECT.into(),
                principal_key: "test-principal".into(),
                task_id: task.clone(),
                task_directory: "workspace/task".into(),
                brief: "Do only this task".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 2,
                },
                expires_at_unix_ms: now + 60_000,
            },
            None,
            "enroll",
            now,
        )
        .unwrap();
    let secret = issued.grant.take().unwrap().into_secret();
    let run = db
        .claim_agent_enrollment(
            "test-principal",
            "new-window",
            &issued.enrollment_id,
            &secret,
            now,
        )
        .unwrap()
        .binding;
    (temp, db, run, task, principal)
}
fn finish(
    db: &Database,
    run: &crate::VerifiedAgentRun,
    task: &str,
    execution: &AgentPublicTaskExecution,
    outcome: AgentTaskState,
    key: &str,
) -> Result<AgentPublicTaskMutation, CommunicationStoreError> {
    db.complete_agent_public_task(
        run,
        task,
        &execution.execution_id,
        execution.revision,
        outcome,
        Some("Explicit public result"),
        None,
        key,
    )
}

#[test]
fn public_task_lifecycle_uses_real_task_without_legacy_identity_or_attempt() {
    let (_temp, db, run, task, principal) = fixture();
    let started = db.start_agent_public_task(&run, &task, "start").unwrap();
    assert_eq!(started.task.state, AgentTaskState::Active);
    assert_eq!(started.execution.agent_id, run.agent_id());
    assert!(started.task.latest_attempt.is_none());
    assert_eq!(
        db.read_agent_task(&principal, &task)
            .unwrap()
            .summary
            .public_execution,
        Some(started.execution.clone())
    );
    let identities: i64 = db
        .conn_for_tests()
        .query_row("SELECT COUNT(*) FROM wc_agent_identities", [], |r| r.get(0))
        .unwrap();
    assert_eq!(identities, 0);
    let secret = format!("wc_pat_{}", "a".repeat(64));
    assert!(db
        .complete_agent_public_task(
            &run,
            &task,
            &started.execution.execution_id,
            1,
            AgentTaskState::Succeeded,
            Some(&secret),
            None,
            "secret"
        )
        .is_err());
    let legacy = db
        .create_agent_identity(
            &principal,
            crate::NewAgentIdentity {
                handle: "legacy".into(),
                display_name: "Legacy".into(),
                description: "Separate test identity".into(),
                specialty_labels: vec![],
                idempotency_key: "legacy".into(),
            },
        )
        .unwrap()
        .agent
        .agent_id;
    assert!(db.assign_agent_task(&principal, &task, &legacy).is_err());
    assert!(db
        .start_agent_task_attempt(&principal, &task, &legacy, "legacy-start")
        .is_err());
    let done = finish(
        &db,
        &run,
        &task,
        &started.execution,
        AgentTaskState::Succeeded,
        "complete",
    )
    .unwrap();
    assert_eq!(done.task.state, AgentTaskState::Succeeded);
    assert_eq!(done.execution.revision, 2);
    assert!(done.task.latest_attempt.is_none());
    assert_eq!(
        db.read_agent_team_task(&run, &task).unwrap().state,
        "succeeded"
    );
    let markers:(Option<String>,Option<String>,String)=db.conn_for_tests().query_row("SELECT assignee_agent_id,latest_attempt_id,terminal_attempt_id FROM wc_agent_tasks WHERE task_id=?1",[&task],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(markers, (None, None, started.execution.execution_id));
    assert_eq!(
        db.conn_for_tests()
            .query_row("SELECT COUNT(*) FROM wc_agent_task_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn public_task_replays_are_exact_and_revoked_run_cannot_complete_or_replay() {
    let (_temp, db, run, task, _principal) = fixture();
    let started = db.start_agent_public_task(&run, &task, "start").unwrap();
    assert!(
        db.start_agent_public_task(&run, &task, "start")
            .unwrap()
            .replayed
    );
    assert!(db
        .start_agent_public_task(&run, &task, "different")
        .is_err());
    assert!(db
        .start_agent_public_task(
            &run,
            "wc_agent_task_00000000000000000000000000000000",
            "foreign"
        )
        .is_err());
    assert!(db
        .complete_agent_public_task(
            &run,
            &task,
            &started.execution.execution_id,
            2,
            AgentTaskState::Failed,
            None,
            None,
            "stale"
        )
        .is_err());
    let done = finish(
        &db,
        &run,
        &task,
        &started.execution,
        AgentTaskState::Failed,
        "complete",
    )
    .unwrap();
    let replay = finish(
        &db,
        &run,
        &task,
        &started.execution,
        AgentTaskState::Failed,
        "complete",
    )
    .unwrap();
    assert!(replay.replayed && !replay.state_changed);
    assert_eq!(replay.execution, done.execution);
    assert!(finish(
        &db,
        &run,
        &task,
        &started.execution,
        AgentTaskState::Succeeded,
        "complete"
    )
    .is_err());
    let old_start = db.start_agent_public_task(&run, &task, "start").unwrap();
    assert_eq!(old_start.task.state, AgentTaskState::Failed);
    assert!(!old_start.state_changed);
    db.revoke_agent_run(
        run.owner_user(),
        run.project(),
        run.agent_id(),
        run.run_id(),
    )
    .unwrap();
    assert!(finish(
        &db,
        &run,
        &task,
        &started.execution,
        AgentTaskState::Failed,
        "complete"
    )
    .is_err());
    assert!(db.read_agent_public_task_execution(&run, &task).is_err());
}

#[test]
fn public_task_success_keeps_real_quota_gate_and_auto_does_not_replace_stop_proof() {
    let (_temp, db, run, task, principal) = fixture();
    let policy = db
        .save_team_policy(
            &principal,
            PROJECT,
            0,
            crate::TeamPolicyInput {
                mode: crate::TeamPolicyMode::Custom,
                subagents: crate::TeamCountRange { min: 0, max: 0 },
                cli_per_agent: crate::TeamCountRange { min: 1, max: 1 },
                ..crate::TeamPolicyInput::default()
            },
        )
        .unwrap();
    let plan = crate::TeamPlanInput {
        manager: crate::TeamAgentPlan {
            task_id: task.clone(),
            cli_calls: vec![crate::TeamCliCall {
                inputs: vec![],
                provider_id: "workbuddy".into(),
                model: "observed-model".into(),
                effort: Some("high".into()),
                instruction: None,
            }],
        },
        subagents: vec![],
    };
    db.register_team_plan(
        &principal,
        PROJECT,
        &task,
        policy.revision,
        plan.clone(),
        "plan",
    )
    .unwrap();
    let started = db.start_agent_public_task(&run, &task, "start").unwrap();
    assert_eq!(
        finish(
            &db,
            &run,
            &task,
            &started.execution,
            AgentTaskState::Succeeded,
            "unproven"
        )
        .unwrap_err()
        .code(),
        "team_minimum_execution_unmet"
    );
    // A separate Auto plan has no custom minima; it still cannot interpret a
    // timeout receipt as proof that the physical execution tree has stopped.
    let (_auto_temp, db, run, task, principal) = fixture();
    let mut plan = plan;
    plan.manager.task_id = task.clone();
    db.register_team_plan(&principal, PROJECT, &task, 0, plan, "auto-plan")
        .unwrap();
    let started = db.start_agent_public_task(&run, &task, "start").unwrap();
    let attempt = db
        .reserve_team_cli_attempt(
            &principal,
            PROJECT,
            &task,
            &task,
            0,
            "call",
            "profile-high",
            Some(60),
        )
        .unwrap()
        .attempt;
    db.claim_team_cli_dispatch(&principal, &attempt.attempt_id)
        .unwrap();
    let bound = db
        .bind_team_cli_job(&principal, &attempt.attempt_id, "job-test")
        .unwrap();
    db.reconcile_team_cli_attempt(
        &principal,
        &attempt.attempt_id,
        bound.observation_revision,
        crate::TeamCliObservation {
            job_id: bound.job_id,
            dispatch_state: crate::TeamCliDispatchState::Terminal,
            execution_state: Some(
                webcodex_core::runner_protocol::ShellCommandExecutionState::TimedOut,
            ),
            error_code: None,
            result_summary: None,
        },
    )
    .unwrap();
    for outcome in [AgentTaskState::Succeeded, AgentTaskState::Failed] {
        assert_eq!(
            finish(
                &db,
                &run,
                &task,
                &started.execution,
                outcome,
                "no-physical-proof"
            )
            .unwrap_err()
            .code(),
            "public_task_execution_stop_proof_required"
        );
    }
    assert_eq!(
        db.read_agent_task(&principal, &task).unwrap().summary.state,
        AgentTaskState::Active
    );
    assert_eq!(
        db.read_agent_public_task_execution(&run, &task)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}
