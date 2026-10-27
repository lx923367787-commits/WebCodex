use super::*;
use crate::{AgentEnrollmentPolicy, NewAgentEnrollment, NewAgentTask, TeamCliObservation};
use sha2::{Digest, Sha256};

const PROJECT: &str = "agent:runner:team";
const OWNER: &str = "synthetic-managed-user-id";
fn owner() -> CommunicationPrincipal {
    let mut hash = Sha256::new();
    hash.update(b"webcodex.communication.principal.v1\0managed-user\0");
    hash.update(OWNER.as_bytes());
    CommunicationPrincipal {
        kind: "managed-user".into(),
        digest: format!("wc_commprincipal_{:x}", hash.finalize()),
    }
}
fn task(db: &Database, name: &str) -> String {
    db.create_agent_task(
        &owner(),
        NewAgentTask {
            title: name.into(),
            instruction: format!("Public assigned brief {name}"),
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
fn enroll(
    db: &Database,
    task: &str,
    purpose: AgentEnrollmentPurpose,
    source: Option<&VerifiedAgentRun>,
    key: &str,
) -> VerifiedAgentRun {
    let now = chrono::Utc::now().timestamp_millis();
    let mut issued = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose,
                owner_user: OWNER.into(),
                project: PROJECT.into(),
                principal_key: "office-principal".into(),
                task_id: task.into(),
                task_directory: format!("workspace/{task}"),
                brief: "Explicit public assignment".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: if purpose == AgentEnrollmentPurpose::Manager {
                        1
                    } else {
                        0
                    },
                    max_cli_calls: if purpose == AgentEnrollmentPurpose::Manager {
                        3
                    } else {
                        1
                    },
                },
                expires_at_unix_ms: now + 60_000,
            },
            source,
            key,
            now,
        )
        .unwrap();
    let secret = issued.grant.take().unwrap().into_secret();
    db.claim_agent_enrollment(
        "office-principal",
        &format!("window-{key}"),
        &issued.enrollment_id,
        &secret,
        now,
    )
    .unwrap()
    .binding
}
fn member(id: &str, calls: usize) -> TeamAgentPlan {
    TeamAgentPlan {
        task_id: id.into(),
        cli_calls: (0..calls)
            .map(|index| TeamCliCall {
                inputs: vec![],
                provider_id: "workbuddy".into(),
                model: "observed-model".into(),
                effort: Some("high".into()),
                instruction: Some(format!("Public subtask {index}")),
            })
            .collect(),
    }
}
fn plan(root: &str, child: &str) -> TeamPlanInput {
    TeamPlanInput {
        manager: member(root, 2),
        subagents: vec![member(child, 1)],
    }
}

#[test]
fn assigned_cli_without_plan_reports_prerequisite_only_to_its_owner() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let root = task(&db, "planless-cli-root");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let unrelated = task(&db, "unrelated-cli-root");
    let other = enroll(
        &db,
        &unrelated,
        AgentEnrollmentPurpose::Manager,
        None,
        "other",
    );
    assert!(db
        .read_agent_cli_preparation(&manager, &root, &root, 0, "probe", "profile", Some(900))
        .unwrap()
        .is_none());
    assert_eq!(
        db.read_agent_cli_planned_slot(&manager, &root, &root, 0)
            .unwrap_err()
            .code(),
        "team_plan_required"
    );
    for (caller, source) in [(&other, root.as_str()), (&manager, unrelated.as_str())] {
        assert_ne!(
            db.read_agent_cli_planned_slot(caller, source, source, 0)
                .unwrap_err()
                .code(),
            "team_plan_required"
        );
    }
    assert_eq!(
        db.read_agent_team_task(&manager, &root).unwrap().state,
        "ready"
    );
    assert_eq!(
        db.list_agent_team_receipts(&manager, &root, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    assert_eq!(
        db.list_agent_team_plans(&manager, &root, 0, 20)
            .unwrap()
            .total_count,
        0
    );
}

#[test]
fn assigned_manager_can_read_own_receipts_before_registering_a_team_plan() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let root = task(&db, "planless-root");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let unrelated = task(&db, "unrelated-root");
    let other = enroll(
        &db,
        &unrelated,
        AgentEnrollmentPurpose::Manager,
        None,
        "other",
    );
    let receipts = db.list_agent_team_receipts(&manager, &root, 0, 20).unwrap();
    assert_eq!(receipts.total_count, 0);
    assert!(receipts.receipts.is_empty());
    assert!(db
        .list_agent_team_receipts(&manager, &root, 0, MAX_TEAM_LIST_LIMIT + 1)
        .is_err());
    assert!(db
        .list_agent_team_receipts(&manager, &unrelated, 0, 20)
        .is_err());
    assert!(db.list_agent_team_receipts(&other, &root, 0, 20).is_err());
    assert_eq!(
        db.list_agent_team_plans(&manager, &root, 0, 20)
            .unwrap()
            .total_count,
        0
    );
}

#[test]
fn assigned_team_registration_checks_child_budget_when_aggregate_and_project_allow() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let root = task(&db, "root");
    let child = task(&db, "child");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    enroll(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "child",
    );
    let excess = TeamPlanInput {
        manager: member(&root, 1),
        subagents: vec![member(&child, 2)],
    };
    // Project permits up to two per member and the manager permits three in
    // total; this must still fail because this child's frozen ceiling is one.
    assert_eq!(
        db.register_agent_team_plan(&manager, &root, 0, excess, "child-over-limit")
            .unwrap_err()
            .code(),
        "agent_team_budget_exceeded"
    );
    assert_eq!(
        db.list_agent_team_plans(&manager, &root, 0, 20)
            .unwrap()
            .total_count,
        0
    );
    db.register_agent_team_plan(
        &manager,
        &root,
        0,
        plan(&root, &child),
        "valid-after-denial",
    )
    .unwrap();
    assert_eq!(
        db.list_agent_team_receipts(&manager, &root, 0, 20)
            .unwrap()
            .total_count,
        0
    );
}

#[test]
fn assigned_team_registration_enforces_exact_role_delegation_and_total_budget() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let root = task(&db, "root");
    let child = task(&db, "child");
    let unrelated = task(&db, "unrelated");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let child_run = enroll(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "child",
    );
    assert!(db
        .register_agent_team_plan(&child_run, &root, 0, plan(&root, &child), "forged-manager")
        .is_err());
    assert!(db
        .register_agent_team_plan(
            &manager,
            &unrelated,
            0,
            TeamPlanInput {
                manager: member(&unrelated, 0),
                subagents: vec![]
            },
            "wrong-root"
        )
        .is_err());
    assert!(db
        .register_agent_team_plan(&manager, &root, 0, plan(&root, &unrelated), "not-delegated")
        .is_err());
    let mut excess = plan(&root, &child);
    let extra = excess.subagents[0].cli_calls[0].clone();
    excess.subagents[0].cli_calls.push(extra);
    assert_eq!(
        db.register_agent_team_plan(&manager, &root, 0, excess, "too-many-cli")
            .unwrap_err()
            .code(),
        "agent_team_budget_exceeded"
    );
    let registered = db
        .register_agent_team_plan(&manager, &root, 0, plan(&root, &child), "valid")
        .unwrap();
    assert!(registered.created && registered.state_changed);
    assert!(
        db.register_agent_team_plan(&manager, &root, 0, plan(&root, &child), "valid")
            .unwrap()
            .replayed
    );
    assert_eq!(
        db.list_agent_team_plans(&manager, &root, 0, 1)
            .unwrap()
            .total_count,
        1
    );
    assert!(db
        .list_agent_team_plans(&manager, &root, 1, 1)
        .unwrap()
        .plans
        .is_empty());
    assert!(db.read_agent_team_plan(&child_run, &root, 0, 20).is_err());
    assert!(db.read_agent_team_task(&manager, &child).is_err());
    assert_eq!(
        db.read_agent_team_task(&child_run, &child).unwrap().title,
        "child"
    );
    let legacy = db.create_isolated_agent(OWNER, PROJECT, None).unwrap();
    let legacy = db.authenticate_agent_run(&legacy.token).unwrap();
    assert!(db.agent_team_policy(&legacy, &root).is_err());
    db.revoke_agent_run(
        manager.owner_user(),
        manager.project(),
        manager.agent_id(),
        manager.run_id(),
    )
    .unwrap();
    assert!(db
        .register_agent_team_plan(&manager, &root, 0, plan(&root, &child), "valid")
        .is_err());
}

#[test]
fn assigned_team_receipts_are_scoped_paginated_and_never_replace_completion() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let root = task(&db, "root");
    let child = task(&db, "child");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let child_run = enroll(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "child",
    );
    let policy = db
        .save_team_policy(
            &owner(),
            PROJECT,
            0,
            TeamPolicyInput {
                mode: TeamPolicyMode::Custom,
                subagents: TeamCountRange { min: 1, max: 1 },
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
            },
        )
        .unwrap();
    db.register_agent_team_plan(
        &manager,
        &root,
        policy.revision,
        plan(&root, &child),
        "plan",
    )
    .unwrap();
    let mut attempts = Vec::new();
    for (source, index) in [(&root, 0), (&root, 1), (&child, 0)] {
        attempts.push(
            db.reserve_team_cli_attempt(
                &owner(),
                PROJECT,
                &root,
                source,
                index,
                &format!("{source}-{index}"),
                "profile-high",
                Some(60),
            )
            .unwrap()
            .attempt,
        );
    }
    db.claim_team_cli_dispatch(&owner(), &attempts[0].attempt_id)
        .unwrap();
    let bound = db
        .bind_team_cli_job(&owner(), &attempts[0].attempt_id, "job-synthetic")
        .unwrap();
    db.reconcile_team_cli_attempt(
        &owner(),
        &bound.attempt_id,
        bound.observation_revision,
        TeamCliObservation {
            job_id: bound.job_id,
            dispatch_state: TeamCliDispatchState::Terminal,
            execution_state: Some(ShellCommandExecutionState::Completed),
            error_code: None,
            result_summary: Some("PRIVATE CLI RESULT MUST NOT BE PROJECTED".into()),
        },
    )
    .unwrap();
    let first = db.read_agent_team_plan(&manager, &root, 0, 1).unwrap();
    assert_eq!(first.receipts.total_count, 3);
    assert_eq!(first.receipts.receipts.len(), 1);
    assert_eq!(first.receipts.next_offset, Some(1));
    assert!(!first.completion.minimums_satisfied);
    assert_eq!(first.completion.confirmed_cli_calls, 1);
    let empty = db.read_agent_team_plan(&manager, &root, 3, 1).unwrap();
    assert!(empty.receipts.receipts.is_empty());
    assert_eq!(empty.receipts.total_count, 3);
    assert!(!empty.completion.minimums_satisfied);
    assert_eq!(empty.completion.confirmed_cli_calls, 1);
    let own = db
        .list_agent_team_receipts(&child_run, &child, 0, 20)
        .unwrap();
    assert_eq!(own.total_count, 1);
    assert_eq!(own.receipts[0].source_task_id, child);
    assert!(db
        .list_agent_team_receipts(&child_run, &root, 0, 20)
        .is_err());
    for value in [
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&own).unwrap(),
    ] {
        assert!(!value.contains("PRIVATE CLI RESULT"));
        assert!(!value.contains("result_summary"));
    }
    assert!(db
        .list_agent_team_receipts(&manager, &root, 0, MAX_TEAM_LIST_LIMIT + 1)
        .is_err());
    assert!(db
        .read_office_team_plan(&owner(), "agent:other:team", &root, 0, 20)
        .is_err());
}

#[test]
fn assigned_team_expired_member_delivery_preserves_reads_without_new_admission() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let root = task(&db, "root");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let child = db
        .create_agent_member(
            &manager,
            crate::AgentMemberRequest {
                title: "Reserved child".into(),
                brief: "Public work".into(),
                task_directory: "workspace/child".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 1,
                },
            },
            "child",
        )
        .unwrap();
    let frozen = plan(&root, &child.task_id);
    db.register_agent_team_plan(&manager, &root, 0, frozen.clone(), "plan")
        .unwrap();
    let issued = db
        .issue_agent_enrollment_delivery(
            &manager,
            &child.enrollment_id,
            "delivery",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let secret = issued.grant.unwrap().into_secret();
    // A deterministic expiry of this synthetic delivery, not production state.
    db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_enrollments SET delivery_expires_at_unix_ms=1 WHERE enrollment_id=?1",
            [&child.enrollment_id],
        )
        .unwrap();
    assert!(db.read_agent_team_plan(&manager, &root, 0, 20).is_ok());
    assert_eq!(
        db.list_agent_team_plans(&manager, &root, 0, 20)
            .unwrap()
            .total_count,
        1
    );
    assert!(db.list_agent_team_receipts(&manager, &root, 0, 20).is_ok());
    assert!(db
        .claim_agent_enrollment(
            "office-principal",
            "late-window",
            &child.enrollment_id,
            &secret,
            chrono::Utc::now().timestamp_millis()
        )
        .is_err());
    assert!(db
        .register_agent_team_plan(&manager, &root, 0, frozen, "plan")
        .is_err());
    let other_root = task(&db, "other-root");
    let other = enroll(
        &db,
        &other_root,
        AgentEnrollmentPurpose::Manager,
        None,
        "other-manager",
    );
    assert!(db.read_agent_team_plan(&other, &root, 0, 20).is_err());
    assert!(db.list_agent_team_receipts(&other, &root, 0, 20).is_err());
}

#[test]
fn assigned_team_reads_continue_while_draining_but_plan_mutations_are_fenced() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let root = task(&db, "root");
    let child = task(&db, "child");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    enroll(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "child",
    );
    db.register_agent_team_plan(&manager, &root, 0, plan(&root, &child), "plan")
        .unwrap();
    let context = db
        .ensure_agent_host_context(&manager, Some((1000, 2000)))
        .unwrap();
    db.observe_host_context(
        &manager.host_goal_owner_key(),
        &context.context_epoch,
        "drain",
        2000,
        false,
        None,
    )
    .unwrap();
    assert!(db.read_agent_team_plan(&manager, &root, 0, 20).is_ok());
    assert_eq!(
        db.register_agent_team_plan(&manager, &root, 0, plan(&root, &child), "plan")
            .unwrap_err()
            .code(),
        "goal_context_draining"
    );
}

#[test]
fn operator_cli_round_preserves_history_and_requires_stopped_slots_and_exact_team() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("rounds.db")).unwrap();
    let root = task(&db, "root");
    let child = task(&db, "child");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let child_run = enroll(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "child",
    );
    let base = plan(&root, &child);
    db.register_agent_team_plan(&manager, &root, 0, base.clone(), "base")
        .unwrap();
    assert!(db
        .register_operator_team_cli_round("other-owner", &root, 0, 0, base.clone(), "round")
        .is_err());
    assert_eq!(
        db.register_operator_team_cli_round(OWNER, &root, 0, 0, base.clone(), "round")
            .unwrap_err()
            .code(),
        "team_round_previous_pending"
    );
    for (source, index) in [(&root, 0), (&root, 1), (&child, 0)] {
        let a = db
            .reserve_team_cli_attempt(
                &owner(),
                PROJECT,
                &root,
                source,
                index,
                &format!("{source}-{index}"),
                "profile",
                Some(60),
            )
            .unwrap()
            .attempt;
        let a = db
            .claim_team_cli_dispatch(&owner(), &a.attempt_id)
            .unwrap()
            .attempt;
        db.reconcile_team_cli_attempt(
            &owner(),
            &a.attempt_id,
            a.observation_revision,
            TeamCliObservation {
                job_id: None,
                dispatch_state: TeamCliDispatchState::Terminal,
                execution_state: Some(ShellCommandExecutionState::NotStarted),
                error_code: Some("synthetic_preflight_failure".into()),
                result_summary: None,
            },
        )
        .unwrap();
    }
    let mut extra = base.clone();
    let duplicate = extra.subagents[0].cli_calls[0].clone();
    extra.subagents[0].cli_calls.push(duplicate);
    assert!(db
        .register_operator_team_cli_round(OWNER, &root, 0, 0, extra, "excess")
        .is_err());
    let mut next = base.clone();
    next.subagents[0].cli_calls[0].inputs = vec![crate::AgentEnrollmentInput {
        name: "material".into(),
        content: "immutable input".into(),
    }];
    let r = db
        .register_operator_team_cli_round(OWNER, &root, 0, 0, next.clone(), "round")
        .unwrap();
    assert_eq!(r.round, 1);
    assert_eq!(r.first_call_indices[&root], 2);
    assert_eq!(r.first_call_indices[&child], 1);
    assert_eq!(
        db.register_operator_team_cli_round(OWNER, &root, 0, 0, next.clone(), "round")
            .unwrap()
            .round,
        1
    );
    assert!(db
        .register_operator_team_cli_round(OWNER, &root, 0, 0, base.clone(), "round")
        .is_err());
    assert!(db
        .register_operator_team_cli_round(OWNER, &root, 0, 0, next.clone(), "new-key")
        .is_err());
    let view = db.read_agent_team_plan(&manager, &root, 0, 20).unwrap();
    assert_eq!(view.plan.plan, base);
    assert_eq!(view.cli_rounds.len(), 1);
    assert_eq!(view.receipts.total_count, 3);
    assert_eq!(view.completion.planned_cli_calls, 6);
    assert_eq!(
        db.read_agent_cli_planned_slot(&child_run, &root, &child, 1)
            .unwrap()
            .instruction,
        "Public subtask 0"
    );
    assert!(db
        .read_agent_cli_planned_slot(&child_run, &root, &child, 2)
        .is_err());
    let conn = db.conn_for_tests();
    assert!(conn
        .execute("UPDATE wc_team_cli_rounds SET record_json='{}'", [])
        .is_err());
    assert!(conn.execute("DELETE FROM wc_team_cli_rounds", []).is_err());
}

#[test]
fn operator_cli_round_resolves_saved_efforts_and_replays_original_policy_revision() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("round-policy.db")).unwrap();
    let root = task(&db, "root");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let base = TeamPlanInput {
        manager: member(&root, 1),
        subagents: vec![],
    };
    let original = db
        .register_agent_team_plan(&manager, &root, 0, base.clone(), "base")
        .unwrap();
    let attempt = db
        .reserve_team_cli_attempt(
            &owner(),
            PROJECT,
            &root,
            &root,
            0,
            "prior",
            "profile",
            Some(60),
        )
        .unwrap()
        .attempt;
    let attempt = db
        .claim_team_cli_dispatch(&owner(), &attempt.attempt_id)
        .unwrap()
        .attempt;
    let stopped = db
        .reconcile_team_cli_attempt(
            &owner(),
            &attempt.attempt_id,
            attempt.observation_revision,
            TeamCliObservation {
                job_id: None,
                dispatch_state: TeamCliDispatchState::Terminal,
                execution_state: Some(ShellCommandExecutionState::NotStarted),
                error_code: Some("synthetic_preflight_failure".into()),
                result_summary: None,
            },
        )
        .unwrap();
    let mut settings = TeamPolicyInput::default();
    settings.providers = ["workbuddy", "antigravity"]
        .into_iter()
        .map(|id| TeamProviderPreference {
            provider_id: id.into(),
            enabled: true,
            model: None,
            effort: None,
        })
        .collect();
    settings.mode = TeamPolicyMode::Custom;
    settings.cli_per_agent = TeamCountRange { min: 2, max: 2 };
    settings.providers[0].model = Some("observed-model".into());
    settings.providers[0].effort = Some("high".into());
    settings.providers[1].model = Some("observed-agy-model".into());
    db.save_team_policy(&owner(), PROJECT, 0, settings.clone())
        .unwrap();
    let mut next = TeamPlanInput {
        manager: member(&root, 2),
        subagents: vec![],
    };
    next.manager.cli_calls[0].effort = Some("project_policy".into());
    next.manager.cli_calls[1].provider_id = "antigravity".into();
    next.manager.cli_calls[1].model = "observed-agy-model".into();
    next.manager.cli_calls[1].effort = Some("project_policy".into());
    assert_eq!(
        db.register_operator_team_cli_round(OWNER, &root, 0, 0, next.clone(), "stale")
            .unwrap_err()
            .code(),
        "team_policy_revision_conflict"
    );
    let mut wrong = next.clone();
    wrong.manager.cli_calls[1].model = "different-model".into();
    assert_eq!(
        db.register_operator_team_cli_round(OWNER, &root, 0, 1, wrong, "wrong-model")
            .unwrap_err()
            .code(),
        "team_saved_selection_mismatch"
    );
    let round = db
        .register_operator_team_cli_round(OWNER, &root, 0, 1, next.clone(), "round")
        .unwrap();
    assert_eq!(round.first_call_indices[&root], 1);
    assert_eq!(
        round.plan.manager.cli_calls[0].effort.as_deref(),
        Some("high")
    );
    assert_eq!(round.plan.manager.cli_calls[1].effort, None);
    settings.providers[0].effort = Some("low".into());
    settings.providers[1].enabled = false;
    db.save_team_policy(&owner(), PROJECT, 1, settings).unwrap();
    let replay = db
        .register_operator_team_cli_round(OWNER, &root, 0, 1, next.clone(), "round")
        .unwrap();
    assert_eq!(
        serde_json::to_value(replay).unwrap(),
        serde_json::to_value(round).unwrap()
    );
    next.manager.cli_calls[1].effort = None;
    assert_eq!(
        db.register_operator_team_cli_round(OWNER, &root, 0, 1, next, "round")
            .unwrap_err()
            .code(),
        "team_idempotency_conflict"
    );
    assert_eq!(
        serde_json::to_value(db.read_team_plan(&owner(), &root).unwrap()).unwrap(),
        serde_json::to_value(original.plan).unwrap()
    );
    assert_eq!(
        serde_json::to_value(
            db.read_team_cli_attempt(&owner(), &attempt.attempt_id)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(stopped).unwrap()
    );
}
