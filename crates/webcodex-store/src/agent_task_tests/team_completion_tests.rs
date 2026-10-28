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
        settings.providers = ["workbuddy", "antigravity"]
            .into_iter()
            .map(|id| crate::TeamProviderPreference {
                provider_id: id.into(),
                enabled: true,
                model: None,
                effort: None,
            })
            .collect();
        settings.mode = mode;
        settings.cli_per_agent.min = 1;
        settings.cli_per_agent.max = 1;
        let policy = db.save_team_policy(&owner, PROJECT, 0, settings).unwrap();
        db.register_team_plan(&owner,PROJECT,&task,policy.revision,serde_json::from_value(serde_json::json!({
            "manager":{"task_id":task,"cli_calls":[{"provider_id":"workbuddy","model":"observed-model","effort":"high","instruction":"Independent completion test"}]},"subagents":[]
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
fn team_completion_allows_zero_cli_and_preserves_terminal_replay() {
    let f = Fixture::new(TeamPolicyMode::Custom);
    let completed = f
        .complete(AgentTaskState::Succeeded, "independent-work")
        .unwrap();
    assert!(completed.state_changed);
    assert_eq!(completed.task.state, AgentTaskState::Succeeded);
    f.db.mark_team_plan_cancelled(&f.owner, &f.task).unwrap();
    assert!(
        f.complete(AgentTaskState::Succeeded, "independent-work")
            .unwrap()
            .replayed
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
        .is_ok());
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
fn team_completion_does_not_require_planned_members_to_be_dispatched() {
    let f = Fixture::new(TeamPolicyMode::Custom);
    let progress =
        f.db.read_team_completion(&f.owner, PROJECT, &f.task)
            .unwrap();
    assert_eq!(progress.required_subagents, 0);
    assert_eq!(progress.confirmed_cli_calls, 0);
    assert!(progress.members.iter().all(|m| m.required_cli_calls == 0));
    assert!(f
        .complete(AgentTaskState::Succeeded, "result-verified")
        .is_ok());
}
