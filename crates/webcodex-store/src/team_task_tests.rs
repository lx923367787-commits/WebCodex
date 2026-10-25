use super::*;
use crate::{
    AgentTaskState, NewAgentIdentity, NewAgentTask, COMMUNICATION_PRINCIPAL_DIGEST_PREFIX,
};
use std::sync::{Arc, Barrier};

const PROJECT: &str = "agent:runner:team";

#[test]
fn saved_policy_effort_resolves_null_and_replays_original_revision() {
    let (_dir, db) = database();
    let owner = principal('a');
    let root = task(&db, &owner, PROJECT, "saved-root");
    let child = task(&db, &owner, PROJECT, "saved-child");
    let mut settings = TeamPolicyInput::default();
    settings.providers[0].model = Some("discovered-model".into());
    settings.providers[0].effort = Some("high".into());
    settings.providers[1].model = Some("agy-model".into());
    db.save_team_policy(&owner, PROJECT, 0, settings.clone())
        .unwrap();
    let mut input = plan(&root, &child);
    input.manager.cli_calls[0].effort = Some("project_policy".into());
    input.manager.cli_calls[1].provider_id = "antigravity".into();
    input.manager.cli_calls[1].model = "agy-model".into();
    input.manager.cli_calls[1].effort = Some("project_policy".into());
    let mut wrong = input.clone();
    wrong.manager.cli_calls[1].model = "other-model".into();
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, wrong, "wrong")
            .unwrap_err()
            .code(),
        "team_saved_selection_mismatch"
    );
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 0, input.clone(), "stale")
            .unwrap_err()
            .code(),
        "team_policy_revision_conflict"
    );
    let frozen = db
        .register_team_plan(&owner, PROJECT, &root, 1, input.clone(), "saved-plan")
        .unwrap();
    assert_eq!(
        frozen.plan.plan.manager.cli_calls[0].effort.as_deref(),
        Some("high")
    );
    assert_eq!(frozen.plan.plan.manager.cli_calls[1].effort, None);
    settings.providers[1].effort = Some("new-effort".into());
    settings.providers[1].enabled = false;
    db.save_team_policy(&owner, PROJECT, 1, settings).unwrap();
    let replay = db
        .register_team_plan(&owner, PROJECT, &root, 1, input.clone(), "saved-plan")
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.plan.plan.manager.cli_calls[1].effort, None);
    input.manager.cli_calls[1].effort = None;
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, input, "saved-plan")
            .unwrap_err()
            .code(),
        "team_idempotency_conflict"
    );
}

fn principal(hex: char) -> CommunicationPrincipal {
    CommunicationPrincipal {
        kind: "user".into(),
        digest: format!(
            "{COMMUNICATION_PRINCIPAL_DIGEST_PREFIX}{}",
            hex.to_string().repeat(64)
        ),
    }
}

fn database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    init_team_task_tables(&db.conn.lock().unwrap()).unwrap();
    (dir, db)
}

fn task(db: &Database, owner: &CommunicationPrincipal, project: &str, key: &str) -> String {
    db.create_agent_task(
        owner,
        NewAgentTask {
            title: key.into(),
            instruction: format!("Authoritative instruction for {key}"),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some(project.into()),
            idempotency_key: key.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}

fn call() -> TeamCliCall {
    TeamCliCall {
        inputs: vec![],
        provider_id: "workbuddy".into(),
        model: "discovered-model".into(),
        effort: Some("high".into()),
        instruction: None,
    }
}
fn plan(root: &str, child: &str) -> TeamPlanInput {
    TeamPlanInput {
        manager: TeamAgentPlan {
            task_id: root.into(),
            cli_calls: vec![
                TeamCliCall {
                    instruction: Some("Inspect the current implementation".into()),
                    ..call()
                },
                TeamCliCall {
                    instruction: Some("Independently review the current implementation".into()),
                    ..call()
                },
            ],
        },
        subagents: vec![TeamAgentPlan {
            task_id: child.into(),
            cli_calls: vec![call()],
        }],
    }
}
fn setup(db: &Database, owner: &CommunicationPrincipal) -> (String, String, TeamPlanInput) {
    let root = task(db, owner, PROJECT, "root");
    let child = task(db, owner, PROJECT, "child");
    let plan = plan(&root, &child);
    db.register_team_plan(owner, PROJECT, &root, 0, plan.clone(), "plan")
        .unwrap();
    (root, child, plan)
}
fn reserve(
    db: &Database,
    owner: &CommunicationPrincipal,
    root: &str,
    source: &str,
    index: u32,
    key: &str,
) -> TeamCliAttemptRecord {
    db.reserve_team_cli_attempt(
        owner,
        PROJECT,
        root,
        source,
        index,
        key,
        "profile-high",
        Some(120),
    )
    .unwrap()
    .attempt
}
fn observation(
    job: Option<&str>,
    state: TeamCliDispatchState,
    execution: Option<ShellCommandExecutionState>,
) -> TeamCliObservation {
    TeamCliObservation {
        job_id: job.map(str::to_owned),
        dispatch_state: state,
        execution_state: execution,
        error_code: Some("cli_failed".into()),
        result_summary: Some("Bounded failed result".into()),
    }
}

#[test]
fn policy_cas_is_durable_scoped_and_wire_compatible() {
    let (dir, db) = database();
    let owner = principal('a');
    let other = principal('b');
    assert_eq!(db.get_team_policy(&owner, PROJECT).unwrap().revision, 0);
    let policy = db
        .save_team_policy(&owner, PROJECT, 0, TeamPolicyInput::default())
        .unwrap();
    assert_eq!(policy.revision, 1);
    assert_eq!(serde_json::to_value(&policy).unwrap()["mode"], "auto");
    assert_eq!(
        db.save_team_policy(&owner, PROJECT, 0, TeamPolicyInput::default())
            .unwrap_err()
            .code(),
        "team_policy_revision_conflict"
    );
    assert_eq!(db.get_team_policy(&other, PROJECT).unwrap().revision, 0);
    drop(db);
    let reopened = Database::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(reopened.get_team_policy(&owner, PROJECT).unwrap(), policy);
}

#[test]
fn frozen_plan_replay_preserves_original_policy_after_edit() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, _, plan) = setup(&db, &owner);
    db.save_team_policy(&owner, PROJECT, 0, TeamPolicyInput::default())
        .unwrap();
    let replay = db
        .register_team_plan(&owner, PROJECT, &root, 0, plan.clone(), "plan")
        .unwrap();
    assert!(replay.replayed);
    assert!(!replay.state_changed);
    assert_eq!(replay.plan.policy.revision, 0);
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, plan.clone(), "plan")
            .unwrap_err()
            .code(),
        "team_idempotency_conflict"
    );
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, plan, "another-key")
            .unwrap_err()
            .code(),
        "team_plan_frozen"
    );
}

#[test]
fn plan_requires_real_same_owner_same_project_tasks_without_existence_leak() {
    let (_dir, db) = database();
    let owner = principal('a');
    let other = principal('b');
    let root = task(&db, &owner, PROJECT, "root");
    let foreign = task(&db, &other, PROJECT, "foreign");
    let wrong_project = task(&db, &owner, "agent:runner:other", "different");
    let missing = format!("{AGENT_TASK_ID_PREFIX}{}", "0".repeat(32));
    for child in [&foreign, &wrong_project, &missing] {
        assert_eq!(
            db.register_team_plan(&owner, PROJECT, &root, 0, plan(&root, child), "attempt")
                .unwrap_err()
                .code(),
            "team_task_not_found"
        );
    }
    assert_eq!(
        db.list_team_plans(&owner, PROJECT, 0, 20)
            .unwrap()
            .total_count,
        0
    );
}

#[test]
fn custom_policy_counts_all_ordinary_members_and_pins_native_selection() {
    let (_dir, db) = database();
    let owner = principal('a');
    let root = task(&db, &owner, PROJECT, "root");
    let child = task(&db, &owner, PROJECT, "child");
    let mut settings = TeamPolicyInput::default();
    settings.mode = TeamPolicyMode::Custom;
    settings.subagents = TeamCountRange { min: 1, max: 1 };
    settings.cli_per_agent = TeamCountRange { min: 1, max: 2 };
    settings.providers[0].model = Some("discovered-model".into());
    settings.providers[0].effort = Some("high".into());
    settings.providers[1].enabled = false;
    db.save_team_policy(&owner, PROJECT, 0, settings).unwrap();
    let valid = plan(&root, &child);
    let mut changed = valid.clone();
    changed.subagents.clear();
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, changed, "missing-child")
            .unwrap_err()
            .code(),
        "team_subagent_quota"
    );
    let mut changed = valid.clone();
    changed.manager.cli_calls.clear();
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, changed, "manager-no-cli")
            .unwrap_err()
            .code(),
        "team_cli_quota"
    );
    let mut changed = valid.clone();
    changed.subagents[0].cli_calls[0].model = "other-model".into();
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, changed, "wrong-model")
            .unwrap_err()
            .code(),
        "team_model_policy_mismatch"
    );
    let mut changed = valid.clone();
    changed.subagents[0].cli_calls[0].provider_id = "antigravity".into();
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 1, changed, "disabled-provider")
            .unwrap_err()
            .code(),
        "team_provider_disabled"
    );
    assert!(
        db.register_team_plan(&owner, PROJECT, &root, 1, valid, "valid")
            .unwrap()
            .created
    );
}

#[test]
fn optional_cli_effort_preserves_none_but_rejects_blank_values() {
    let (_dir, db) = database();
    let owner = principal('a');
    let root = task(&db, &owner, PROJECT, "root");
    let child = task(&db, &owner, PROJECT, "child");
    for effort in [Some(String::new()), Some(" \t".into())] {
        let mut planned = plan(&root, &child);
        planned.manager.cli_calls[0].effort = effort;
        assert_eq!(
            db.register_team_plan(&owner, PROJECT, &root, 0, planned, "invalid-effort")
                .unwrap_err()
                .code(),
            "invalid_team_request"
        );
    }
    assert_eq!(
        db.list_team_plans(&owner, PROJECT, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    let mut no_effort = plan(&root, &child);
    no_effort.manager.cli_calls[0].effort = None;
    let valid = db
        .register_team_plan(&owner, PROJECT, &root, 0, no_effort, "valid-effort")
        .unwrap();
    assert!(valid.created);
    assert_eq!(valid.plan.plan.manager.cli_calls[0].effort.as_deref(), None);
    assert!(valid
        .plan
        .policy
        .settings
        .providers
        .iter()
        .all(|provider| provider.effort.is_none()));
}

#[test]
fn duplicate_members_and_unplanned_slots_fail_without_reservation() {
    let (_dir, db) = database();
    let owner = principal('a');
    let root = task(&db, &owner, PROJECT, "root");
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 0, plan(&root, &root), "duplicate")
            .unwrap_err()
            .code(),
        "invalid_team_request"
    );
    let child = task(&db, &owner, PROJECT, "child");
    db.register_team_plan(&owner, PROJECT, &root, 0, plan(&root, &child), "valid")
        .unwrap();
    assert_eq!(
        db.reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            1,
            "not-planned",
            "profile",
            Some(120)
        )
        .unwrap_err()
        .code(),
        "team_cli_slot_not_planned"
    );
    assert_eq!(
        db.list_team_cli_attempts(&owner, PROJECT, Some(&root), 0, 20)
            .unwrap()
            .total_count,
        0
    );
}

#[test]
fn reservation_freezes_instruction_profile_timeout_and_charges_slot_once() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let first = reserve(&db, &owner, &root, &child, 0, "call");
    assert_eq!(first.instruction, "Authoritative instruction for child");
    assert_eq!(first.timeout_secs, Some(120));
    assert_eq!(first.execution_state, None);
    let replay = db
        .reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-high",
            Some(120),
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt.attempt_id, first.attempt_id);
    assert_eq!(
        db.reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "other-profile",
            Some(120)
        )
        .unwrap_err()
        .code(),
        "team_idempotency_conflict"
    );
    assert_eq!(
        db.reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-high",
            Some(121)
        )
        .unwrap_err()
        .code(),
        "team_idempotency_conflict"
    );
    assert_eq!(
        db.reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "new-key",
            "profile-high",
            Some(120)
        )
        .unwrap_err()
        .code(),
        "team_cli_slot_reserved"
    );
}

#[test]
fn multiple_cli_calls_freeze_their_distinct_explicit_subtasks() {
    let (_dir, db) = database();
    let owner = principal('a');
    let root = task(&db, &owner, PROJECT, "root");
    let child = task(&db, &owner, PROJECT, "child");
    let mut planned = plan(&root, &child);
    planned.manager.cli_calls[0].instruction =
        Some("Inspect the API contract.\nReport only mismatches.".into());
    planned.manager.cli_calls[1].instruction =
        Some("Review the UI states and accessibility.".into());
    db.register_team_plan(&owner, PROJECT, &root, 0, planned.clone(), "plan")
        .unwrap();
    let first = reserve(&db, &owner, &root, &root, 0, "api-review");
    let second = reserve(&db, &owner, &root, &root, 1, "ui-review");
    assert_eq!(
        first.instruction,
        "Inspect the API contract.\nReport only mismatches."
    );
    assert_eq!(
        second.instruction,
        "Review the UI states and accessibility."
    );
    assert_eq!(first.source_task_id, root);
    assert_eq!(second.source_task_id, root);
    assert_eq!(
        load_team_cli_attempt_for_fallback(&db.conn.lock().unwrap(), &owner, &first.attempt_id)
            .unwrap()
            .instruction,
        first.instruction
    );
    let mut changed = planned;
    changed.manager.cli_calls[0].instruction = Some("Changed assignment".into());
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 0, changed, "plan")
            .unwrap_err()
            .code(),
        "team_idempotency_conflict"
    );
}

#[test]
fn multiple_calls_require_explicit_bounded_text_but_can_intentionally_repeat_review() {
    let (_dir, db) = database();
    let owner = principal('a');
    let root = task(&db, &owner, PROJECT, "root");
    let child = task(&db, &owner, PROJECT, "child");
    let mut planned = plan(&root, &child);
    planned.manager.cli_calls[0].instruction = None;
    assert_eq!(
        db.register_team_plan(&owner, PROJECT, &root, 0, planned.clone(), "missing")
            .unwrap_err()
            .code(),
        "team_cli_instruction_required"
    );
    for invalid in [
        String::new(),
        " \n\t".into(),
        "Review\0hidden".into(),
        "测".repeat(2731),
    ] {
        planned.manager.cli_calls[0].instruction = Some(invalid);
        assert_eq!(
            db.register_team_plan(&owner, PROJECT, &root, 0, planned.clone(), "invalid")
                .unwrap_err()
                .code(),
            "invalid_team_request"
        );
    }
    assert_eq!(
        db.list_team_plans(&owner, PROJECT, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    for call in &mut planned.manager.cli_calls {
        call.instruction = Some("Independently review the same acceptance criteria".into());
    }
    assert!(
        db.register_team_plan(&owner, PROJECT, &root, 0, planned, "intentional-review")
            .unwrap()
            .created
    );
}

#[test]
fn a_sole_cli_call_can_inherit_task_instruction_without_changing_legacy_serialization() {
    let original =
        serde_json::json!({"provider_id":"workbuddy","model":"discovered-model","effort":"high"});
    let parsed: TeamCliCall = serde_json::from_value(original.clone()).unwrap();
    assert!(parsed.instruction.is_none());
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
    let (_dir, db) = database();
    let owner = principal('a');
    let root = task(&db, &owner, PROJECT, "root");
    let child = task(&db, &owner, PROJECT, "child");
    let mut planned = plan(&root, &child);
    planned.manager.cli_calls = vec![call()];
    db.register_team_plan(&owner, PROJECT, &root, 0, planned, "single-call-plan")
        .unwrap();
    assert_eq!(
        reserve(&db, &owner, &root, &root, 0, "root-call").instruction,
        "Authoritative instruction for root"
    );
    assert_eq!(
        reserve(&db, &owner, &root, &child, 0, "child-call").instruction,
        "Authoritative instruction for child"
    );
}

#[test]
fn concurrent_dispatch_claims_cross_the_durable_fence_only_once() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let record = reserve(&db, &owner, &root, &child, 0, "call");
    let db = Arc::new(db);
    let barrier = Arc::new(Barrier::new(8));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let owner = owner.clone();
        let id = record.attempt_id.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            db.claim_team_cli_dispatch(&owner, &id).unwrap().claimed
        }));
    }
    assert_eq!(
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|claimed| *claimed)
            .count(),
        1
    );
    let stored = db
        .read_team_cli_attempt(&owner, &record.attempt_id)
        .unwrap();
    assert_eq!(stored.dispatch_state, TeamCliDispatchState::Dispatching);
    assert_eq!(stored.observation_revision, 1);
}

#[test]
fn dispatched_attempt_replay_needs_no_live_profile_and_never_reserves_or_dispatches() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    assert!(db
        .lookup_team_cli_attempt_replay(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-now-expired",
            Some(120)
        )
        .unwrap()
        .is_none());
    assert_eq!(
        db.list_team_cli_attempts(&owner, PROJECT, None, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    let reserved = db
        .reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-now-expired",
            Some(120),
        )
        .unwrap()
        .attempt;
    let dispatched = db
        .claim_team_cli_dispatch(&owner, &reserved.attempt_id)
        .unwrap()
        .attempt;
    // Profile freshness belongs to the Host adapter; this existing dispatch is
    // recoverable without asking that transient catalog or obtaining a new claim.
    assert_eq!(
        db.lookup_team_cli_attempt_replay(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-now-expired",
            Some(120)
        )
        .unwrap(),
        Some(dispatched.clone())
    );
    db.mark_team_plan_cancelled(&owner, &root).unwrap();
    assert_eq!(
        db.lookup_team_cli_attempt_replay(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-now-expired",
            Some(120)
        )
        .unwrap(),
        Some(dispatched.clone())
    );
    assert_eq!(
        db.read_team_cli_attempt(&owner, &reserved.attempt_id)
            .unwrap(),
        dispatched
    );
    assert_eq!(
        db.list_team_cli_attempts(&owner, PROJECT, None, 0, 20)
            .unwrap()
            .total_count,
        1
    );
    assert!(
        !db.claim_team_cli_dispatch(&owner, &reserved.attempt_id)
            .unwrap()
            .claimed
    );
}

#[test]
fn read_only_replay_checks_intent_owner_and_project() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let original = reserve(&db, &owner, &root, &child, 0, "call");
    assert_eq!(
        db.lookup_team_cli_attempt_replay(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-high",
            Some(121)
        )
        .unwrap_err()
        .code(),
        "team_idempotency_conflict"
    );
    assert_eq!(
        db.lookup_team_cli_attempt_replay(
            &owner,
            "agent:runner:wrong",
            &root,
            &child,
            0,
            "call",
            "profile-high",
            Some(120)
        )
        .unwrap_err()
        .code(),
        "team_task_not_found"
    );
    assert_eq!(
        db.lookup_team_cli_attempt_replay(
            &principal('b'),
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-high",
            Some(120)
        )
        .unwrap_err()
        .code(),
        "team_task_not_found"
    );
    assert!(db
        .lookup_team_cli_attempt_replay(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "unused-key",
            "profile-high",
            Some(120)
        )
        .unwrap()
        .is_none());
    assert_eq!(
        db.read_team_cli_attempt(&owner, &original.attempt_id)
            .unwrap(),
        original
    );
}

#[test]
fn unknown_dispatch_is_not_retried_and_late_job_binding_remains_exact() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let record = reserve(&db, &owner, &root, &child, 0, "call");
    let claim = db
        .claim_team_cli_dispatch(&owner, &record.attempt_id)
        .unwrap();
    assert!(claim.claimed);
    let unknown = db
        .reconcile_team_cli_attempt(
            &owner,
            &record.attempt_id,
            claim.attempt.observation_revision,
            observation(None, TeamCliDispatchState::Unknown, None),
        )
        .unwrap();
    assert!(
        !db.claim_team_cli_dispatch(&owner, &record.attempt_id)
            .unwrap()
            .claimed
    );
    let bound = db
        .bind_team_cli_job(&owner, &record.attempt_id, "job-one")
        .unwrap();
    assert!(bound.observation_revision > unknown.observation_revision);
    assert_eq!(bound.execution_state, None);
    assert_eq!(
        db.bind_team_cli_job(&owner, &record.attempt_id, "job-two")
            .unwrap_err()
            .code(),
        "team_cli_job_conflict"
    );
    assert_eq!(
        db.reconcile_team_cli_attempt(
            &owner,
            &record.attempt_id,
            bound.observation_revision,
            observation(
                Some("job-two"),
                TeamCliDispatchState::Terminal,
                Some(ShellCommandExecutionState::Completed)
            )
        )
        .unwrap_err()
        .code(),
        "team_cli_job_conflict"
    );
}

#[test]
fn failure_and_timeout_preserve_budget_and_typed_runner_execution_evidence() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let record = reserve(&db, &owner, &root, &child, 0, "call");
    db.claim_team_cli_dispatch(&owner, &record.attempt_id)
        .unwrap();
    let bound = db
        .bind_team_cli_job(&owner, &record.attempt_id, "job-timeout")
        .unwrap();
    let terminal = db
        .reconcile_team_cli_attempt(
            &owner,
            &record.attempt_id,
            bound.observation_revision,
            observation(
                Some("job-timeout"),
                TeamCliDispatchState::Terminal,
                Some(ShellCommandExecutionState::TimedOut),
            ),
        )
        .unwrap();
    assert_eq!(
        terminal.execution_state,
        Some(ShellCommandExecutionState::TimedOut)
    );
    assert_eq!(
        load_team_cli_attempt_for_fallback(&db.conn.lock().unwrap(), &owner, &record.attempt_id)
            .unwrap(),
        terminal
    );
    assert_eq!(
        db.reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "replacement",
            "profile-high",
            Some(120)
        )
        .unwrap_err()
        .code(),
        "team_cli_slot_reserved"
    );
    let replay = db
        .reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &child,
            0,
            "call",
            "profile-high",
            Some(120),
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt, terminal);
    assert!(
        !db.claim_team_cli_dispatch(&owner, &record.attempt_id)
            .unwrap()
            .claimed
    );
    assert_eq!(
        db.reconcile_team_cli_attempt(
            &owner,
            &record.attempt_id,
            terminal.observation_revision,
            observation(Some("job-timeout"), TeamCliDispatchState::Unknown, None)
        )
        .unwrap_err()
        .code(),
        "team_cli_dispatch_conflict"
    );
}

#[test]
fn cancellation_blocks_new_reservation_dispatch_and_recovery_but_keeps_evidence() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let record = reserve(&db, &owner, &root, &child, 0, "call");
    assert!(
        db.mark_team_plan_cancelled(&owner, &root)
            .unwrap()
            .state_changed
    );
    assert!(db.mark_team_plan_cancelled(&owner, &root).unwrap().replayed);
    assert_eq!(
        db.claim_team_cli_dispatch(&owner, &record.attempt_id)
            .unwrap_err()
            .code(),
        "team_plan_cancelled"
    );
    assert_eq!(
        db.reserve_team_cli_attempt(
            &owner,
            PROJECT,
            &root,
            &root,
            0,
            "next",
            "profile-high",
            Some(120)
        )
        .unwrap_err()
        .code(),
        "team_plan_cancelled"
    );
    assert_eq!(
        load_team_cli_attempt_for_fallback(&db.conn.lock().unwrap(), &owner, &record.attempt_id)
            .unwrap_err()
            .code(),
        "team_plan_cancelled"
    );
    assert_eq!(
        db.read_team_cli_attempt(&owner, &record.attempt_id)
            .unwrap(),
        record
    );
}

#[test]
fn root_task_terminal_state_blocks_recovery_inside_same_transaction() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let record = reserve(&db, &owner, &root, &child, 0, "call");
    let identity = db
        .create_agent_identity(
            &owner,
            NewAgentIdentity {
                handle: "manager".into(),
                display_name: "Manager".into(),
                description: String::new(),
                specialty_labels: vec![],
                idempotency_key: "manager".into(),
            },
        )
        .unwrap()
        .agent
        .agent_id;
    db.assign_agent_task(&owner, &root, &identity).unwrap();
    let started = db
        .start_agent_task_attempt(&owner, &root, &identity, "root-attempt")
        .unwrap();
    db.complete_agent_task_attempt(
        &owner,
        &root,
        &started.attempt.attempt_id,
        &identity,
        &started.attempt_fence,
        started.attempt.attempt_controller_generation,
        AgentTaskState::Succeeded,
        Some("Done"),
        None,
        "complete",
    )
    .unwrap();
    assert_eq!(
        load_team_cli_attempt_for_fallback(&db.conn.lock().unwrap(), &owner, &record.attempt_id)
            .unwrap_err()
            .code(),
        "team_root_terminal"
    );
}

#[test]
fn source_task_terminal_state_blocks_recovery_and_reserved_cli_dispatch() {
    let (_dir, db) = database();
    let owner = principal('a');
    let (root, child, _) = setup(&db, &owner);
    let record = reserve(&db, &owner, &root, &child, 0, "call");
    let identity = db
        .create_agent_identity(
            &owner,
            NewAgentIdentity {
                handle: "worker".into(),
                display_name: "Worker".into(),
                description: String::new(),
                specialty_labels: vec![],
                idempotency_key: "worker".into(),
            },
        )
        .unwrap()
        .agent
        .agent_id;
    db.assign_agent_task(&owner, &child, &identity).unwrap();
    let started = db
        .start_agent_task_attempt(&owner, &child, &identity, "child-attempt")
        .unwrap();
    db.complete_agent_task_attempt(
        &owner,
        &child,
        &started.attempt.attempt_id,
        &identity,
        &started.attempt_fence,
        started.attempt.attempt_controller_generation,
        AgentTaskState::Succeeded,
        Some("Done"),
        None,
        "complete-child",
    )
    .unwrap();
    assert_eq!(
        load_team_cli_attempt_for_fallback(&db.conn.lock().unwrap(), &owner, &record.attempt_id)
            .unwrap_err()
            .code(),
        "team_root_terminal"
    );
    assert_eq!(
        db.claim_team_cli_dispatch(&owner, &record.attempt_id)
            .unwrap_err()
            .code(),
        "team_root_terminal"
    );
    assert_eq!(
        db.read_team_cli_attempt(&owner, &record.attempt_id)
            .unwrap()
            .dispatch_state,
        TeamCliDispatchState::Reserved
    );
}

#[test]
fn foreign_attempts_are_hidden_and_lists_are_bounded_and_scoped() {
    let (_dir, db) = database();
    let owner = principal('a');
    let other = principal('b');
    let (root, child, _) = setup(&db, &owner);
    let record = reserve(&db, &owner, &root, &child, 0, "call");
    let missing = format!("{TEAM_CLI_ATTEMPT_ID_PREFIX}{}", "0".repeat(32));
    assert_eq!(
        db.read_team_cli_attempt(&other, &record.attempt_id)
            .unwrap_err()
            .code(),
        db.read_team_cli_attempt(&other, &missing)
            .unwrap_err()
            .code()
    );
    assert_eq!(
        db.list_team_cli_attempts(&other, PROJECT, None, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    assert_eq!(
        db.list_team_plans(&other, PROJECT, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    assert_eq!(
        db.list_team_cli_attempts(&owner, PROJECT, None, 0, 0)
            .unwrap_err()
            .code(),
        "invalid_team_request"
    );
    assert_eq!(
        db.list_team_plans(&owner, PROJECT, 0, 21)
            .unwrap_err()
            .code(),
        "invalid_team_request"
    );
    assert_eq!(
        db.list_team_cli_attempts(&owner, PROJECT, Some(&root), 0, 1)
            .unwrap()
            .attempts[0]
            .attempt_id,
        record.attempt_id
    );
}

fn cli_pagination_plan(
    db: &Database,
    owner: &CommunicationPrincipal,
    project: &str,
    key: &str,
) -> String {
    let root = task(db, owner, project, key);
    let plan = TeamPlanInput {
        manager: TeamAgentPlan {
            task_id: root.clone(),
            cli_calls: (0..32)
                .map(|index| TeamCliCall {
                    instruction: Some(format!("Complete distinct CLI subtask {index}")),
                    ..call()
                })
                .collect(),
        },
        subagents: vec![],
    };
    db.register_team_plan(owner, project, &root, 0, plan, key)
        .unwrap();
    root
}

#[test]
fn cli_attempt_pagination_reaches_twenty_first_and_preserves_insertion_order_across_appends() {
    let (_dir, db) = database();
    let owner = principal('a');
    let root = cli_pagination_plan(&db, &owner, PROJECT, "paged-root");
    let mut expected = Vec::new();
    for index in 0..25 {
        let record = reserve(&db, &owner, &root, &root, index, &format!("call-{index}"));
        // A deterministic clock fixture proves ordering does not use wall time.
        db.conn.lock().unwrap().execute("UPDATE wc_team_cli_attempts SET created_at_unix_ms=?2,record_json=json_set(record_json,'$.created_at_unix_ms',?2) WHERE attempt_id=?1", params![record.attempt_id, index as i64]).unwrap();
        expected.push(record.attempt_id);
    }
    let first = db
        .list_team_cli_attempts(&owner, PROJECT, Some(&root), 0, 20)
        .unwrap();
    assert_eq!(first.total_count, 25);
    assert_eq!(first.next_offset, Some(20));
    assert_eq!(
        first
            .attempts
            .iter()
            .map(|a| a.attempt_id.clone())
            .collect::<Vec<_>>(),
        expected[..20]
    );

    // Observation changes and additional arrivals must not move prior entries.
    db.claim_team_cli_dispatch(&owner, &expected[0]).unwrap();
    for index in 25..30 {
        expected
            .push(reserve(&db, &owner, &root, &root, index, &format!("call-{index}")).attempt_id);
    }
    let second = db
        .list_team_cli_attempts(&owner, PROJECT, Some(&root), first.next_offset.unwrap(), 20)
        .unwrap();
    assert_eq!(second.total_count, 30);
    assert_eq!(second.next_offset, None);
    assert_eq!(second.attempts.len(), 10);
    assert_eq!(second.attempts[0].call_index, 20);
    let all = first
        .attempts
        .iter()
        .chain(&second.attempts)
        .map(|a| a.attempt_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(all, expected);
    assert_eq!(all.iter().collect::<BTreeSet<_>>().len(), 30);
    let refreshed = db
        .list_team_cli_attempts(&owner, PROJECT, Some(&root), 0, 20)
        .unwrap();
    assert_eq!(
        refreshed
            .attempts
            .iter()
            .map(|a| a.attempt_id.clone())
            .collect::<Vec<_>>(),
        expected[..20]
    );
    assert_eq!(
        refreshed.attempts[0].dispatch_state,
        TeamCliDispatchState::Dispatching
    );
    assert!(db
        .list_team_cli_attempts(&owner, PROJECT, Some(&root), 30, 20)
        .unwrap()
        .attempts
        .is_empty());
}

#[test]
fn cli_attempt_pagination_rejects_foreign_roots_wrong_projects_and_unbounded_pages() {
    let (_dir, db) = database();
    let owner = principal('a');
    let other = principal('b');
    let root = cli_pagination_plan(&db, &owner, PROJECT, "owner-root");
    let foreign = cli_pagination_plan(&db, &other, PROJECT, "foreign-root");
    let other_project = "agent:runner:other";
    let outside = cli_pagination_plan(&db, &owner, other_project, "outside-root");
    reserve(&db, &other, &foreign, &foreign, 0, "foreign-call");
    db.reserve_team_cli_attempt(
        &owner,
        other_project,
        &outside,
        &outside,
        0,
        "outside-call",
        "profile-high",
        Some(120),
    )
    .unwrap();
    let own = reserve(&db, &owner, &root, &root, 0, "own-call");
    let page = db
        .list_team_cli_attempts(&owner, PROJECT, None, 0, 20)
        .unwrap();
    assert_eq!(page.total_count, 1);
    assert_eq!(page.attempts[0].attempt_id, own.attempt_id);
    assert_eq!(
        db.list_team_cli_attempts(&owner, PROJECT, Some(&foreign), 0, 20)
            .unwrap_err()
            .code(),
        "team_task_not_found"
    );
    assert_eq!(
        db.list_team_cli_attempts(&owner, other_project, Some(&root), 0, 20)
            .unwrap_err()
            .code(),
        "team_task_not_found"
    );
    assert_eq!(
        db.list_team_cli_attempts(&other, PROJECT, Some(&root), 0, 20)
            .unwrap_err()
            .code(),
        "team_task_not_found"
    );
    for (offset, limit) in [(0, 0), (0, 21), (65537, 20)] {
        assert_eq!(
            db.list_team_cli_attempts(&owner, PROJECT, Some(&root), offset, limit)
                .unwrap_err()
                .code(),
            "invalid_team_request"
        );
    }
}
