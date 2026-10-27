//! Completion gates exercise both authoritative Succeeded write paths.
use super::*;
use crate::{
    TeamCliAttemptRecord, TeamCliDispatchState, TeamCliObservation, TeamPolicyInput, TeamPolicyMode,
};
use webcodex_core::runner_protocol::ShellCommandExecutionState;

const PROJECT: &str = "agent:special:reference-only";

struct Fixture {
    _temporary: tempfile::TempDir,
    db: Database,
    owner: CommunicationPrincipal,
    assignee: String,
    task: String,
    started: AgentTaskAttemptStartMutation,
    now: i64,
}
impl Fixture {
    fn new(mode: TeamPolicyMode) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let db = Database::open(&temporary.path().join("completion.db")).unwrap();
        let owner = principal('a');
        let assignee = agent(&db, &owner, "team-worker");
        let task = create_assigned_task(&db, &owner, &assignee, "team-root");
        let mut settings = TeamPolicyInput::default();
        settings.providers = ["workbuddy", "antigravity"].into_iter().map(|id| crate::TeamProviderPreference { provider_id:id.into(), enabled:true, model:None, effort:None }).collect();
        settings.mode = mode;
        settings.cli_per_agent.min = 1;
        settings.cli_per_agent.max = 1;
        let policy = db.save_team_policy(&owner, PROJECT, 0, settings).unwrap();
        db.register_team_plan(&owner,PROJECT,&task,policy.revision,serde_json::from_value(serde_json::json!({
            "manager":{"task_id":task,"cli_calls":[{"provider_id":"workbuddy","model":"observed-model","effort":"high"}]},"subagents":[]
        })).unwrap(),"plan").unwrap();
        let now = wall_now_ms();
        let started = start(&db, &owner, &task, &assignee, "attempt", now);
        Self {
            _temporary: temporary,
            db,
            owner,
            assignee,
            task,
            started,
            now,
        }
    }
    fn complete(
        &self,
        outcome: AgentTaskState,
        key: &str,
    ) -> Result<AgentTaskAttemptCompletionMutation, super::super::CommunicationStoreError> {
        self.db.complete_agent_task_attempt_at(
            &self.owner,
            &self.task,
            &self.started.attempt.attempt_id,
            &self.assignee,
            &self.started.attempt_fence,
            self.started.attempt.attempt_controller_generation,
            outcome,
            Some("Verified result"),
            None,
            key,
            self.now + 10,
        )
    }
    fn reserve_cli(&self) -> TeamCliAttemptRecord {
        self.db
            .reserve_team_cli_attempt(
                &self.owner,
                PROJECT,
                &self.task,
                &self.task,
                0,
                "cli-once",
                "observed-profile",
                Some(900),
            )
            .unwrap()
            .attempt
    }
    fn bind_cli(&self, attempt: &TeamCliAttemptRecord) -> TeamCliAttemptRecord {
        self.db
            .claim_team_cli_dispatch(&self.owner, &attempt.attempt_id)
            .unwrap();
        self.db
            .bind_team_cli_job(&self.owner, &attempt.attempt_id, "actual-retained-job")
            .unwrap()
    }
    fn observe_cli(
        &self,
        attempt: &TeamCliAttemptRecord,
        state: ShellCommandExecutionState,
    ) -> TeamCliAttemptRecord {
        self.db
            .reconcile_team_cli_attempt(
                &self.owner,
                &attempt.attempt_id,
                attempt.observation_revision,
                TeamCliObservation {
                    job_id: attempt.job_id.clone(),
                    dispatch_state: if state == ShellCommandExecutionState::OutcomeUnknown {
                        TeamCliDispatchState::Unknown
                    } else {
                        TeamCliDispatchState::Terminal
                    },
                    execution_state: Some(state),
                    error_code: Some("provider_failed".into()),
                    result_summary: None,
                },
            )
            .unwrap()
    }
    fn assert_active(&self) {
        let task = self.db.read_agent_task(&self.owner, &self.task).unwrap();
        assert_eq!(task.summary.state, AgentTaskState::Active);
        assert_eq!(
            task.summary.latest_attempt.unwrap().state,
            AgentTaskAttemptState::Active
        );
    }
}

#[test]
fn team_completion_hooks_do_not_count_planned_reserved_bound_or_unknown_cli_calls() {
    let f = Fixture::new(TeamPolicyMode::Custom);
    assert!(f
        .complete(AgentTaskState::Succeeded, "same-completion")
        .is_err());
    f.assert_active();
    let reserved = f.reserve_cli();
    assert!(f
        .complete(AgentTaskState::Succeeded, "same-completion")
        .is_err());
    f.assert_active();
    let bound = f.bind_cli(&reserved);
    assert!(f
        .complete(AgentTaskState::Succeeded, "same-completion")
        .is_err());
    f.assert_active();
    let unknown = f.observe_cli(&bound, ShellCommandExecutionState::OutcomeUnknown);
    assert!(f
        .complete(AgentTaskState::Succeeded, "same-completion")
        .is_err());
    f.assert_active();
    // A failed invocation still counts once it has trustworthy terminal
    // execution evidence; success of the CLI is not the numeric quota.
    f.observe_cli(&unknown, ShellCommandExecutionState::Completed);
    let completed = f
        .complete(AgentTaskState::Succeeded, "same-completion")
        .unwrap();
    assert!(completed.state_changed);
    assert_eq!(completed.task.state, AgentTaskState::Succeeded);
    f.db.mark_team_plan_cancelled(&f.owner, &f.task).unwrap();
    let replay = f
        .complete(AgentTaskState::Succeeded, "same-completion")
        .unwrap();
    assert!(replay.replayed);
    assert!(
        !replay.state_changed,
        "exact completion replay precedes the new gate"
    );
}

#[test]
fn team_completion_hooks_preserve_failed_outcomes_auto_and_not_started_denials() {
    let failed = Fixture::new(TeamPolicyMode::Custom);
    failed
        .db
        .mark_team_plan_cancelled(&failed.owner, &failed.task)
        .unwrap();
    assert!(failed
        .complete(AgentTaskState::Succeeded, "success-denied")
        .is_err());
    assert_eq!(
        failed
            .complete(AgentTaskState::Failed, "failed")
            .unwrap()
            .task
            .state,
        AgentTaskState::Failed
    );
    let auto = Fixture::new(TeamPolicyMode::Auto);
    assert_eq!(
        auto.complete(AgentTaskState::Succeeded, "auto")
            .unwrap()
            .task
            .state,
        AgentTaskState::Succeeded
    );
    let unstarted = Fixture::new(TeamPolicyMode::Custom);
    let bound = unstarted.bind_cli(&unstarted.reserve_cli());
    unstarted.observe_cli(&bound, ShellCommandExecutionState::NotStarted);
    assert!(unstarted
        .complete(AgentTaskState::Succeeded, "not-started")
        .is_err());
    unstarted.assert_active();
}

#[test]
fn team_completion_hooks_gate_acp_success_atomically_and_preserve_terminal_replay() {
    let f = Fixture::new(TeamPolicyMode::Custom);
    let intent = coding_binding_intent("team-completed");
    prepare_coding_binding(
        &f.db,
        &f.owner,
        &f.task,
        &f.assignee,
        &f.started,
        &intent,
        f.now + 1,
    );
    f.db.claim_agent_task_coding_run_dispatch(
        &f.owner,
        &f.task,
        &f.started.attempt.attempt_id,
        &f.assignee,
        &f.started.attempt_fence,
        f.started.attempt.attempt_controller_generation,
        &intent.binding_intent_fingerprint,
    )
    .unwrap();
    let observed = coding_observation(&intent, "completed", "completed", 7);
    f.db.record_agent_task_coding_run_observation(
        &f.owner,
        &f.task,
        &f.started.attempt.attempt_id,
        &observed,
    )
    .unwrap();
    assert!(f
        .db
        .terminalize_agent_task_coding_run(
            &f.owner,
            &f.task,
            &f.started.attempt.attempt_id,
            &observed,
            Some("result"),
            None
        )
        .is_err());
    f.assert_active();
    let bound = f.bind_cli(&f.reserve_cli());
    f.observe_cli(&bound, ShellCommandExecutionState::TimedOut);
    let completed =
        f.db.terminalize_agent_task_coding_run(
            &f.owner,
            &f.task,
            &f.started.attempt.attempt_id,
            &observed,
            Some("result"),
            None,
        )
        .unwrap();
    assert_eq!(completed.task.state, AgentTaskState::Succeeded);
    assert!(completed.state_changed);
    f.db.mark_team_plan_cancelled(&f.owner, &f.task).unwrap();
    let replay =
        f.db.terminalize_agent_task_coding_run(
            &f.owner,
            &f.task,
            &f.started.attempt.attempt_id,
            &observed,
            Some("result"),
            None,
        )
        .unwrap();
    assert_eq!(replay.task.state, AgentTaskState::Succeeded);
    assert!(!replay.state_changed);
}

#[test]
fn team_completion_hooks_allow_acp_failure_with_unmet_minimum() {
    let f = Fixture::new(TeamPolicyMode::Custom);
    let intent = coding_binding_intent("team-failed");
    prepare_coding_binding(
        &f.db,
        &f.owner,
        &f.task,
        &f.assignee,
        &f.started,
        &intent,
        f.now + 1,
    );
    f.db.claim_agent_task_coding_run_dispatch(
        &f.owner,
        &f.task,
        &f.started.attempt.attempt_id,
        &f.assignee,
        &f.started.attempt_fence,
        f.started.attempt.attempt_controller_generation,
        &intent.binding_intent_fingerprint,
    )
    .unwrap();
    let observed = coding_observation(&intent, "failed", "completed", 7);
    let failed =
        f.db.terminalize_agent_task_coding_run(
            &f.owner,
            &f.task,
            &f.started.attempt.attempt_id,
            &observed,
            None,
            Some("Provider failed"),
        )
        .unwrap();
    assert_eq!(failed.task.state, AgentTaskState::Failed);
    assert!(failed.state_changed);
}

#[test]
fn team_completion_hooks_root_requires_observed_child_dispatch_while_child_checks_only_own_cli() {
    use crate::{TeamAgentObservation, TeamAgentState};
    let temporary = tempfile::tempdir().unwrap();
    let db = Database::open(&temporary.path().join("root-child-gate.db")).unwrap();
    let owner = principal('b');
    let manager = agent(&db, &owner, "manager");
    let early_worker = agent(&db, &owner, "early-worker");
    let dispatch_worker = agent(&db, &owner, "dispatch-worker");
    let root = create_assigned_task(&db, &owner, &manager, "root");
    let early_child = create_assigned_task(&db, &owner, &early_worker, "early-child");
    let dispatch_child = create_assigned_task(&db, &owner, &dispatch_worker, "dispatch-child");
    let mut settings = TeamPolicyInput::default();
    settings.providers = ["workbuddy", "antigravity"].into_iter().map(|id| crate::TeamProviderPreference { provider_id:id.into(), enabled:true, model:None, effort:None }).collect();
    settings.mode = TeamPolicyMode::Custom;
    settings.subagents.min = 1;
    settings.subagents.max = 2;
    settings.cli_per_agent.min = 1;
    settings.cli_per_agent.max = 1;
    let policy = db.save_team_policy(&owner, PROJECT, 0, settings).unwrap();
    let member = |task_id: &str| {
        serde_json::json!({
            "task_id":task_id,"cli_calls":[{"provider_id":"workbuddy","model":"observed-model","effort":"high"}]
        })
    };
    db.register_team_plan(&owner, PROJECT, &root, policy.revision,
        serde_json::from_value(serde_json::json!({"manager":member(&root),"subagents":[member(&early_child),member(&dispatch_child)]})).unwrap(),
        "three-member-plan").unwrap();
    let now = wall_now_ms();
    let root_start = start(&db, &owner, &root, &manager, "root-attempt", now);
    let early_start = start(
        &db,
        &owner,
        &early_child,
        &early_worker,
        "child-attempt",
        now,
    );
    let actual_cli = |source: &str| {
        let record = db
            .reserve_team_cli_attempt(
                &owner,
                PROJECT,
                &root,
                source,
                0,
                &format!("cli-{source}"),
                "observed-profile",
                Some(900),
            )
            .unwrap()
            .attempt;
        db.claim_team_cli_dispatch(&owner, &record.attempt_id)
            .unwrap();
        let bound = db
            .bind_team_cli_job(
                &owner,
                &record.attempt_id,
                &format!("retained-job-{source}"),
            )
            .unwrap();
        db.reconcile_team_cli_attempt(
            &owner,
            &record.attempt_id,
            bound.observation_revision,
            TeamCliObservation {
                job_id: bound.job_id,
                dispatch_state: TeamCliDispatchState::Terminal,
                execution_state: Some(ShellCommandExecutionState::Completed),
                error_code: None,
                result_summary: None,
            },
        )
        .unwrap();
    };

    // A member may finish its own assignment while manager and another
    // member's numeric obligations remain unsatisfied in the same frozen plan.
    let finish_child = || {
        db.complete_agent_task_attempt_at(
            &owner,
            &early_child,
            &early_start.attempt.attempt_id,
            &early_worker,
            &early_start.attempt_fence,
            early_start.attempt.attempt_controller_generation,
            AgentTaskState::Succeeded,
            Some("Child's own work verified"),
            None,
            "finish-child",
            now + 10,
        )
    };
    assert_eq!(
        finish_child().unwrap_err().code(),
        "team_minimum_execution_unmet",
        "the JSON member association must enforce this child's own minimum"
    );
    actual_cli(&early_child);
    let child_completed = finish_child().unwrap();
    assert_eq!(child_completed.task.state, AgentTaskState::Succeeded);
    let early_progress = db.read_team_completion(&owner, PROJECT, &root).unwrap();
    assert!(!early_progress.minimums_satisfied);
    assert_eq!(early_progress.confirmed_cli_calls, 1);
    assert_eq!(early_progress.confirmed_subagents, 0);

    actual_cli(&root);
    actual_cli(&dispatch_child);
    let agent_record = db
        .reserve_team_agent_attempt(
            &owner,
            PROJECT,
            &root,
            &dispatch_child,
            "dispatch-child-once",
            "manager-selected-profile",
        )
        .unwrap()
        .record;
    assert_eq!(agent_record.state, TeamAgentState::Queued);
    let finish_root = || {
        db.complete_agent_task_attempt_at(
            &owner,
            &root,
            &root_start.attempt.attempt_id,
            &manager,
            &root_start.attempt_fence,
            root_start.attempt.attempt_controller_generation,
            AgentTaskState::Succeeded,
            Some("Team's work verified"),
            None,
            "finish-root",
            now + 20,
        )
    };
    let ready_calls = db.read_team_completion(&owner, PROJECT, &root).unwrap();
    assert!(ready_calls.members.iter().all(|member| member.minimum_met));
    assert_eq!(ready_calls.confirmed_cli_calls, 3);
    assert_eq!(
        finish_root().unwrap_err().code(),
        "team_minimum_execution_unmet"
    );
    assert_eq!(
        db.read_agent_task(&owner, &root).unwrap().summary.state,
        AgentTaskState::Active
    );

    let claimed = db
        .claim_team_agent_attempt(&owner, &agent_record.team_agent_attempt_id)
        .unwrap();
    let fence = claimed.record.dispatch_attempt_id.unwrap();
    let request_id = uuid::Uuid::new_v4().to_string();
    let running = db
        .record_team_agent_observation(
            &owner,
            &agent_record.team_agent_attempt_id,
            &fence,
            TeamAgentObservation::Dispatched {
                request_id: request_id.clone(),
                response_observed: false,
            },
        )
        .unwrap();
    assert_eq!(running.state, TeamAgentState::Running);
    assert!(running.request_id.is_some());
    assert_eq!(
        finish_root().unwrap_err().code(),
        "team_minimum_execution_unmet"
    );
    assert_eq!(
        db.read_agent_task(&owner, &root).unwrap().summary.state,
        AgentTaskState::Active
    );

    db.record_team_agent_observation(
        &owner,
        &agent_record.team_agent_attempt_id,
        &fence,
        TeamAgentObservation::Dispatched {
            request_id,
            response_observed: true,
        },
    )
    .unwrap();
    let completed = finish_root().unwrap();
    assert_eq!(completed.task.state, AgentTaskState::Succeeded);
    assert!(completed.state_changed);
    assert_eq!(
        db.read_team_completion(&owner, PROJECT, &root)
            .unwrap()
            .confirmed_subagents,
        1
    );
}
