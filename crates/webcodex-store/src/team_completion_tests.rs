use super::*;
use crate::{
    NewAgentTask, TeamAgentObservation, TeamAgentPlan, TeamCliCall, TeamCliDispatchState,
    TeamCliObservation, TeamCountRange, TeamPlanInput, TeamPolicyInput,
    COMMUNICATION_PRINCIPAL_DIGEST_PREFIX,
};
use webcodex_core::runner_protocol::ShellCommandExecutionState;

const PROJECT: &str = "agent:runner:completion";
fn owner(hex: char) -> CommunicationPrincipal {
    CommunicationPrincipal {
        kind: "user".into(),
        digest: format!(
            "{COMMUNICATION_PRINCIPAL_DIGEST_PREFIX}{}",
            hex.to_string().repeat(64)
        ),
    }
}
fn task(db: &Database, principal: &CommunicationPrincipal, name: &str) -> String {
    db.create_agent_task(
        principal,
        NewAgentTask {
            title: name.into(),
            instruction: name.into(),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some(PROJECT.into()),
            idempotency_key: name.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}
fn member(task_id: &str, calls: usize) -> TeamAgentPlan {
    TeamAgentPlan {
        task_id: task_id.into(),
        cli_calls: (0..calls)
            .map(|index| TeamCliCall {
                inputs: vec![],
                provider_id: "workbuddy".into(),
                model: "observed-model".into(),
                effort: Some("high".into()),
                instruction: Some(format!("Independent assigned work {index}")),
            })
            .collect(),
    }
}
fn setup(db: &Database, principal: &CommunicationPrincipal) -> (String, String) {
    let root = task(db, principal, "root");
    let child = task(db, principal, "child");
    let policy = TeamPolicyInput {
        mode: TeamPolicyMode::Custom,
        subagents: TeamCountRange { min: 1, max: 2 },
        cli_per_agent: TeamCountRange { min: 1, max: 2 },
        providers: ["workbuddy", "antigravity"]
            .into_iter()
            .map(|id| crate::TeamProviderPreference {
                provider_id: id.into(),
                enabled: true,
                model: None,
                effort: None,
            })
            .collect(),
    };
    let policy = db.save_team_policy(principal, PROJECT, 0, policy).unwrap();
    db.register_team_plan(
        principal,
        PROJECT,
        &root,
        policy.revision,
        TeamPlanInput {
            manager: member(&root, 2),
            subagents: vec![member(&child, 1)],
        },
        "plan",
    )
    .unwrap();
    (root, child)
}
fn reserve(
    db: &Database,
    principal: &CommunicationPrincipal,
    root: &str,
    source: &str,
    index: u32,
) -> crate::TeamCliAttemptRecord {
    db.reserve_team_cli_attempt(
        principal,
        PROJECT,
        root,
        source,
        index,
        &format!("{source}:{index}"),
        "profile-high",
        Some(30),
    )
    .unwrap()
    .attempt
}
fn finish_cli(
    db: &Database,
    principal: &CommunicationPrincipal,
    attempt: &crate::TeamCliAttemptRecord,
    state: ShellCommandExecutionState,
) {
    db.claim_team_cli_dispatch(principal, &attempt.attempt_id)
        .unwrap();
    let bound = db
        .bind_team_cli_job(
            principal,
            &attempt.attempt_id,
            &format!("job-{}", attempt.attempt_id),
        )
        .unwrap();
    db.reconcile_team_cli_attempt(
        principal,
        &attempt.attempt_id,
        bound.observation_revision,
        TeamCliObservation {
            job_id: bound.job_id.clone(),
            dispatch_state: if state == ShellCommandExecutionState::OutcomeUnknown {
                TeamCliDispatchState::Unknown
            } else {
                TeamCliDispatchState::Terminal
            },
            execution_state: Some(state),
            error_code: Some("failed_test_run".into()),
            result_summary: None,
        },
    )
    .unwrap();
}

#[test]
fn team_completion_separates_plans_budget_and_confirmed_work_without_refunding_failures() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let owner = owner('a');
    let (root, child) = setup(&db, &owner);
    let progress = db.read_team_completion(&owner, PROJECT, &root).unwrap();
    assert_eq!(
        (progress.planned_subagents, progress.planned_cli_calls),
        (1, 3)
    );
    assert_eq!(
        (
            progress.confirmed_subagents,
            progress.consumed_cli_calls,
            progress.confirmed_cli_calls
        ),
        (0, 0, 0)
    );
    assert_eq!(progress.success_gate, "not_required");
    let first = reserve(&db, &owner, &root, &root, 0);
    let child_call = reserve(&db, &owner, &root, &child, 0);
    assert_eq!(
        db.read_team_completion(&owner, PROJECT, &root)
            .unwrap()
            .confirmed_cli_calls,
        0
    );
    finish_cli(&db, &owner, &first, ShellCommandExecutionState::Completed);
    finish_cli(
        &db,
        &owner,
        &child_call,
        ShellCommandExecutionState::OutcomeUnknown,
    );
    let second = reserve(&db, &owner, &root, &root, 1);
    finish_cli(&db, &owner, &second, ShellCommandExecutionState::TimedOut);
    let progress = db.read_team_completion(&owner, PROJECT, &root).unwrap();
    assert_eq!(
        (progress.consumed_cli_calls, progress.confirmed_cli_calls),
        (3, 2)
    );
    assert!(progress.members[1].minimum_met);
    // Failed executions still counted; replay cannot spend the slot again.
    reserve(&db, &owner, &root, &root, 0);
    assert_eq!(
        db.read_team_completion(&owner, PROJECT, &root)
            .unwrap()
            .consumed_cli_calls,
        3
    );
    let child_call = db
        .read_team_cli_attempt(&owner, &child_call.attempt_id)
        .unwrap();
    db.reconcile_team_cli_attempt(
        &owner,
        &child_call.attempt_id,
        child_call.observation_revision,
        TeamCliObservation {
            job_id: child_call.job_id.clone(),
            dispatch_state: TeamCliDispatchState::Terminal,
            execution_state: Some(ShellCommandExecutionState::Completed),
            error_code: Some("test_failed".into()),
            result_summary: None,
        },
    )
    .unwrap();
    let queued = db
        .reserve_team_agent_attempt(&owner, PROJECT, &root, &child, "agent", "profile-high")
        .unwrap()
        .record;
    assert_eq!(
        db.read_team_completion(&owner, PROJECT, &root)
            .unwrap()
            .confirmed_subagents,
        0
    );
    let claim = db
        .claim_team_agent_attempt(&owner, &queued.team_agent_attempt_id)
        .unwrap();
    let fence = claim.record.dispatch_attempt_id.as_deref().unwrap();
    let request_id = uuid::Uuid::new_v4().to_string();
    db.record_team_agent_observation(
        &owner,
        &queued.team_agent_attempt_id,
        fence,
        TeamAgentObservation::Dispatched {
            request_id: request_id.clone(),
            response_observed: false,
        },
    )
    .unwrap();
    assert_eq!(
        db.read_team_completion(&owner, PROJECT, &root)
            .unwrap()
            .success_gate,
        "not_required"
    );
    db.record_team_agent_observation(
        &owner,
        &queued.team_agent_attempt_id,
        fence,
        TeamAgentObservation::Dispatched {
            request_id: request_id.clone(),
            response_observed: true,
        },
    )
    .unwrap();
    db.record_team_agent_observation(
        &owner,
        &queued.team_agent_attempt_id,
        fence,
        TeamAgentObservation::Unknown {
            request_id: Some(request_id),
        },
    )
    .unwrap();
    let progress = db.read_team_completion(&owner, PROJECT, &root).unwrap();
    assert_eq!(progress.confirmed_subagents, 1);
    assert_eq!(progress.success_gate, "not_required");
    assert_eq!(
        progress.task_state, "ready",
        "quota evidence does not mark the task successful"
    );
}

#[test]
fn team_completion_unstarted_calls_consume_budget_but_do_not_satisfy_execution_minimums() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let owner = owner('a');
    let (root, _) = setup(&db, &owner);
    let attempt = reserve(&db, &owner, &root, &root, 0);
    finish_cli(
        &db,
        &owner,
        &attempt,
        ShellCommandExecutionState::NotStarted,
    );
    let progress = db.read_team_completion(&owner, PROJECT, &root).unwrap();
    assert_eq!(
        (progress.consumed_cli_calls, progress.confirmed_cli_calls),
        (1, 0)
    );
    assert!(progress.minimums_satisfied);
}

#[test]
fn team_completion_is_owner_and_project_scoped_and_auto_has_no_minimum() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let principal = owner('a');
    let root = task(&db, &principal, "auto-root");
    let policy = TeamPolicyInput {
        mode: TeamPolicyMode::Auto,
        subagents: TeamCountRange { min: 3, max: 4 },
        cli_per_agent: TeamCountRange { min: 2, max: 3 },
        providers: ["workbuddy", "antigravity"]
            .into_iter()
            .map(|id| crate::TeamProviderPreference {
                provider_id: id.into(),
                enabled: true,
                model: None,
                effort: None,
            })
            .collect(),
    };
    db.save_team_policy(&principal, PROJECT, 0, policy).unwrap();
    db.register_team_plan(
        &principal,
        PROJECT,
        &root,
        1,
        TeamPlanInput {
            manager: member(&root, 0),
            subagents: vec![],
        },
        "auto-plan",
    )
    .unwrap();
    let progress = db.read_team_completion(&principal, PROJECT, &root).unwrap();
    assert_eq!(progress.success_gate, "not_required");
    assert_eq!(progress.required_subagents, 0);
    assert_eq!(progress.members[0].required_cli_calls, 0);
    assert!(db
        .read_team_completion(&owner('b'), PROJECT, &root)
        .is_err());
    assert!(db
        .read_team_completion(&principal, "agent:other:project", &root)
        .is_err());
    db.mark_team_plan_cancelled(&principal, &root).unwrap();
    assert_eq!(
        db.read_team_completion(&principal, PROJECT, &root)
            .unwrap()
            .success_gate,
        "cancelled"
    );
}
