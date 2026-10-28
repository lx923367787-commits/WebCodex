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
    enroll_with_budget(
        db,
        task,
        purpose,
        source,
        key,
        if purpose == AgentEnrollmentPurpose::Manager {
            3
        } else {
            1
        },
    )
}

fn enroll_with_budget(
    db: &Database,
    task: &str,
    purpose: AgentEnrollmentPurpose,
    source: Option<&VerifiedAgentRun>,
    key: &str,
    budget: u32,
) -> VerifiedAgentRun {
    enroll_with_limits(db, task, purpose, source, key, budget, 1)
}

fn enroll_with_limits(
    db: &Database,
    task: &str,
    purpose: AgentEnrollmentPurpose,
    source: Option<&VerifiedAgentRun>,
    key: &str,
    budget: u32,
    max_subagents: u32,
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
                        max_subagents
                    } else {
                        0
                    },
                    max_cli_calls: budget,
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
    db.register_agent_team_plan(&manager, &root, 0, excess, "child-over-old-limit")
        .unwrap();
    assert_eq!(
        db.list_agent_team_plans(&manager, &root, 0, 20)
            .unwrap()
            .total_count,
        1
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
    let child_view = db.read_agent_team_plan(&child_run, &root, 0, 20).unwrap();
    assert!(child_view
        .tasks
        .iter()
        .find(|task| task.task_id == root)
        .unwrap()
        .brief
        .is_empty());
    assert!(!child_view
        .tasks
        .iter()
        .find(|task| task.task_id == child)
        .unwrap()
        .brief
        .is_empty());
    let manager_view = db.read_agent_team_plan(&manager, &root, 0, 20).unwrap();
    assert!(!manager_view
        .tasks
        .iter()
        .find(|task| task.task_id == root)
        .unwrap()
        .brief
        .is_empty());
    assert!(manager_view
        .tasks
        .iter()
        .find(|task| task.task_id == child)
        .unwrap()
        .brief
        .is_empty());
    assert!(members(&manager_view.plan.plan).all(|member| member
        .cli_calls
        .iter()
        .all(|call| call.instruction.is_none())));
    assert!(db
        .read_agent_team_task(&manager, &child)
        .unwrap()
        .brief
        .is_empty());
    let human_view = db
        .read_office_team_plan(&owner(), PROJECT, &root, 0, 20)
        .unwrap();
    assert!(human_view.tasks.iter().all(|task| !task.brief.is_empty()));
    assert!(members(&human_view.plan.plan).all(|member| member
        .cli_calls
        .iter()
        .all(|call| call.instruction.is_some())));
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
    assert!(first.completion.minimums_satisfied);
    assert_eq!(first.completion.confirmed_cli_calls, 1);
    let empty = db.read_agent_team_plan(&manager, &root, 3, 1).unwrap();
    assert!(empty.receipts.receipts.is_empty());
    assert_eq!(empty.receipts.total_count, 3);
    assert!(empty.completion.minimums_satisfied);
    assert_eq!(empty.completion.confirmed_cli_calls, 1);
    let own = db
        .list_agent_team_receipts(&child_run, &child, 0, 20)
        .unwrap();
    assert_eq!(own.total_count, 3);
    assert!(own
        .receipts
        .iter()
        .any(|receipt| receipt.source_task_id == root));
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
    assert!(db
        .read_agent_team_plan(&other, &root, 0, 20)
        .unwrap()
        .tasks
        .iter()
        .all(|task| task.brief.is_empty()));
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
fn operator_cli_round_unfunded_expired_peer_does_not_block_enrolled_member() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("pending-peer.db")).unwrap();
    let root = task(&db, "root");
    let child = task(&db, "funded-member");
    let pending = task(&db, "pending-peer");
    let manager = enroll_with_limits(
        &db,
        &root,
        AgentEnrollmentPurpose::Manager,
        None,
        "manager",
        3,
        2,
    );
    let worker = enroll_with_budget(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "member",
        0,
    );
    let now = chrono::Utc::now().timestamp_millis();
    let issued = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Member,
                owner_user: OWNER.into(),
                project: PROJECT.into(),
                principal_key: "office-principal".into(),
                task_id: pending.clone(),
                task_directory: format!("workspace/{pending}"),
                brief: "Pending public assignment".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 0,
                },
                expires_at_unix_ms: now + 60_000,
            },
            Some(&manager),
            "pending",
            now,
        )
        .unwrap();
    let base = TeamPlanInput {
        manager: member(&root, 0),
        subagents: vec![member(&child, 0), member(&pending, 0)],
    };
    db.register_agent_team_plan(&manager, &root, 0, base.clone(), "base")
        .unwrap();
    // Expiry is time, not revoked ownership. Preserve the original unknown
    // delivery and pending run while granting only the other member's slot.
    db.conn.lock().unwrap().execute("UPDATE wc_agent_enrollments SET delivery_expires_at_unix_ms=0,delivery_state='delivery_unknown' WHERE enrollment_id=?1", [&issued.enrollment_id]).unwrap();
    let mut round = base.clone();
    round.subagents[0] = member(&child, 1);
    let mut invalid = round.clone();
    invalid.subagents[1] = member(&pending, 1);
    assert_eq!(
        db.register_operator_team_cli_round(OWNER, &root, 0, 0, invalid, "invalid")
            .unwrap_err()
            .code(),
        "agent_delegated_assignment_ambiguous"
    );
    let grant = db
        .register_operator_team_cli_round(OWNER, &root, 0, 0, round, "grant")
        .unwrap();
    assert_eq!(grant.first_call_indices[&pending], 0);
    assert!(grant.plan.subagents[1].cli_calls.is_empty());
    db.read_agent_cli_planned_slot(&worker, &root, &child, 0)
        .unwrap();
    let conn = db.conn.lock().unwrap();
    let retained: (String, String, String) = conn.query_row("SELECT e.state,e.delivery_state,r.state FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.run_id=e.run_id WHERE e.enrollment_id=?1", [&issued.enrollment_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        retained,
        ("issued".into(), "delivery_unknown".into(), "pending".into())
    );
    assert_eq!(plan_on(&conn, &owner(), &root).unwrap().plan, base);
}

#[test]
fn operator_cli_round_funds_only_selected_zero_budget_member_and_preserves_original() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("zero-budget-round.db")).unwrap();
    let root = task(&db, "root");
    let child = task(&db, "child");
    let manager = enroll(&db, &root, AgentEnrollmentPurpose::Manager, None, "manager");
    let worker = enroll_with_budget(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "child",
        0,
    );
    let base = TeamPlanInput {
        manager: member(&root, 0),
        subagents: vec![member(&child, 0)],
    };
    db.register_agent_team_plan(&manager, &root, 0, base.clone(), "base")
        .unwrap();
    assert!(db
        .authorize_agent_assignment(&worker, &child, crate::AgentAssignmentOperation::UseCli)
        .is_ok());
    assert!(db
        .read_agent_cli_planned_slot(&worker, &root, &child, 0)
        .is_err());
    let round_plan = TeamPlanInput {
        manager: member(&root, 0),
        subagents: vec![member(&child, 2)],
    };
    assert!(db
        .register_agent_team_plan(&manager, &root, 0, round_plan.clone(), "agent-expand")
        .is_err());
    assert!(db
        .register_operator_team_cli_round("another-owner", &root, 0, 0, round_plan.clone(), "grant")
        .is_err());
    let grant = db
        .register_operator_team_cli_round(OWNER, &root, 0, 0, round_plan.clone(), "grant")
        .unwrap();
    assert_eq!(grant.first_call_indices[&child], 0);
    assert_eq!(
        db.register_operator_team_cli_round(OWNER, &root, 0, 0, round_plan, "grant")
            .unwrap()
            .round,
        1
    );
    db.authorize_agent_assignment(&worker, &child, crate::AgentAssignmentOperation::UseCli)
        .unwrap();
    for index in 0..2 {
        db.read_agent_cli_planned_slot(&worker, &root, &child, index)
            .unwrap();
    }
    assert!(db
        .read_agent_cli_planned_slot(&worker, &root, &child, 2)
        .is_err());
    assert!(db
        .read_agent_cli_planned_slot(&manager, &root, &root, 0)
        .is_err());
    assert!(db
        .read_agent_cli_planned_slot(&manager, &root, &child, 0)
        .is_err());
    assert_eq!(
        db.read_agent_enrollment(&worker)
            .unwrap()
            .policy
            .max_cli_calls,
        0
    );
    let view = db.read_agent_team_plan(&manager, &root, 0, 20).unwrap();
    let mut projected = view.plan.clone();
    projected.plan = base;
    project_agent_plan(&mut projected);
    assert_eq!(view.plan.plan, projected.plan);
    assert_eq!(view.completion.planned_cli_calls, 2);
    drop(db);
    let db = Database::open(&temp.path().join("zero-budget-round.db")).unwrap();
    db.authorize_agent_assignment(&worker, &child, crate::AgentAssignmentOperation::UseCli)
        .unwrap();
    db.read_agent_cli_planned_slot(&worker, &root, &child, 1)
        .unwrap();
    assert!(db
        .read_agent_cli_planned_slot(&worker, &root, &child, 2)
        .is_err());
}

#[test]
fn operator_cli_round_preserves_history_and_exact_team() {
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
    let mut projected = view.plan.clone();
    projected.plan = base;
    project_agent_plan(&mut projected);
    assert_eq!(view.plan.plan, projected.plan);
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
    let mut projected = db.read_team_plan(&owner(), &root).unwrap();
    project_agent_plan(&mut projected);
    assert_eq!(
        serde_json::to_value(projected).unwrap(),
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

#[test]
fn shared_team_lifecycle_exposes_peer_status_without_goal_memory_or_acceptance() {
    use serde_json::json;
    use webcodex_core::host_goal::HostGoalPlan;
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("shared-lifecycle.db")).unwrap();
    let root = task(&db, "shared-manager");
    let child = db
        .create_agent_task(
            &owner(),
            NewAgentTask {
                title: "Shared child title".into(),
                instruction: "private-brief-sentinel".into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(PROJECT.into()),
                idempotency_key: "lifecycle-child".into(),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let manager = enroll(
        &db,
        &root,
        AgentEnrollmentPurpose::Manager,
        None,
        "lifecycle-manager",
    );
    let worker = enroll(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "lifecycle-worker",
    );
    db.register_agent_team_plan(&manager, &root, 0, plan(&root, &child), "lifecycle-plan")
        .unwrap();
    db.start_agent_public_task(&worker, &child, "lifecycle-start")
        .unwrap();
    let context = db.ensure_agent_host_context(&worker, None).unwrap();
    let goal_owner = worker.host_goal_owner_key();
    let now = chrono::Utc::now().timestamp_millis();
    let created = db
        .create_host_goal_in_task_directory_in_context(
            &goal_owner,
            PROJECT,
            "agent-private://private-path-sentinel",
            &format!("workspace/{child}"),
            "private-objective-sentinel",
            "lifecycle-goal",
            now,
            Some(&context),
        )
        .unwrap()
        .goal;
    let planning = db.read_agent_team_task(&manager, &child).unwrap();
    assert_eq!(
        planning.goal_lifecycle.as_ref().unwrap()["goal_phase"],
        "planning"
    );
    assert!(planning.brief.is_empty());
    assert_eq!(planning.title, "Shared child title");
    assert!(db
        .read_host_goal_for_agent_run(&manager, &created.goal_id)
        .is_err());
    let criterion =
        json!({"kind":"review","id":"check","description":"private-acceptance-sentinel"});
    let private_plan: HostGoalPlan = serde_json::from_value(json!({
        "objective":"private-plan-objective-sentinel","constraints":["private-constraint-sentinel"],
        "deliverables":["private-plan-deliverable-sentinel"],"final_criteria":[criterion.clone()],
        "nodes":[{"id":"node","title":"private-node-title-sentinel","work":"private-work-sentinel",
            "outputs":["private-plan-output-sentinel"],"criterion_ids":["check"],"acceptance":[criterion],
            "failure_action":"private-recovery-sentinel"}],
        "state_management":{"facts":"private-facts-rule-sentinel","decisions":"private-decisions-rule-sentinel",
            "issues":"private-issues-rule-sentinel","artifacts":"private-artifacts-rule-sentinel",
            "checkpoint_triggers":["private-trigger-sentinel"],"resume_instructions":"private-resume-sentinel"}
    })).unwrap();
    let saved = db
        .save_host_goal_plan_in_context(
            &goal_owner,
            &created.goal_id,
            created.revision,
            &private_plan,
            None,
            "save-private-plan",
            now + 1,
            Some(&context),
        )
        .unwrap();
    let mut state = saved.goal.state.clone();
    state
        .facts
        .insert("memory".into(), "private-memory-sentinel".into());
    state.next_step = "private-next-step-sentinel".into();
    db.bind_host_goal_window_in_context(
        &goal_owner,
        worker.run_id(),
        &crate::HostGoalWindowBinding {
            goal_id: created.goal_id.clone(),
            project: PROJECT.into(),
            project_root: created.project_root.clone(),
            context_epoch: worker.run_id().into(),
        },
        Some(&context),
    )
    .unwrap();
    db.checkpoint_host_goal_with_request_in_context(
        &goal_owner,
        &created.goal_id,
        saved.goal.revision,
        None,
        &state,
        crate::HostGoalCheckpointAction::Handoff,
        "private-checkpoint-reason-sentinel",
        "checkpoint",
        "checkpoint-hash",
        &[],
        now + 2,
        Some(&context),
        &[],
    )
    .unwrap();
    let handoff = db
        .read_host_goal_handoff(&goal_owner, &created.goal_id)
        .unwrap()
        .unwrap();
    let begun = db
        .begin_host_goal_handoff_attempt(
            &goal_owner,
            &created.goal_id,
            &handoff.handoff_id,
            None,
            now + 3,
            60,
        )
        .unwrap();
    db.fail_host_goal_handoff_attempt(
        &goal_owner,
        &created.goal_id,
        &handoff.handoff_id,
        begun.attempt_id.as_deref().unwrap(),
        "private-failure-reason-sentinel",
        now + 4,
    )
    .unwrap();
    let shared = db.read_agent_team_task(&manager, &child).unwrap();
    let lifecycle = shared.goal_lifecycle.as_ref().unwrap();
    assert_eq!(lifecycle["goal_id"], created.goal_id);
    assert_eq!(lifecycle["goal_phase"], saved.goal.phase.as_str());
    assert_eq!(lifecycle["plan_version"], 1);
    assert_eq!(
        lifecycle["checkpoint_sequence"],
        handoff.checkpoint_sequence
    );
    assert_eq!(lifecycle["handoff_attempt_count"], 1);
    assert_eq!(lifecycle["handoff_failure_code"], "handoff_failed");
    assert!(lifecycle["checkpoint_created_at"].is_i64());
    let control = db
        .read_host_goal_handoff_control_metadata(&goal_owner, &created.goal_id)
        .unwrap();
    assert_eq!(lifecycle, &control);
    let view = db.read_agent_team_plan(&manager, &root, 0, 20).unwrap();
    assert_eq!(
        view.tasks
            .iter()
            .find(|task| task.task_id == child)
            .unwrap()
            .goal_lifecycle
            .as_ref(),
        Some(lifecycle)
    );
    for output in [
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(&view).unwrap(),
        control,
    ] {
        assert!(!output.to_string().contains("private-"));
    }
    let own = db
        .read_host_goal_for_agent_run(&worker, &created.goal_id)
        .unwrap();
    assert_eq!(own.0.state.facts["memory"], "private-memory-sentinel");
    assert_eq!(own.1.unwrap().plan.final_criteria[0].id(), "check");
    let mut foreign = owner();
    foreign.digest = format!("wc_commprincipal_{}", "0".repeat(64));
    assert!(db
        .read_office_team_plan(&foreign, PROJECT, &root, 0, 20)
        .is_err());
    assert!(db
        .read_office_team_plan(&owner(), "agent:runner:other", &root, 0, 20)
        .is_err());
}

#[test]
fn manager_cli_append_keeps_unknown_and_unused_slots_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("append.db")).unwrap();
    let root = task(&db, "append-root");
    let child = task(&db, "append-child");
    let manager = enroll(
        &db,
        &root,
        AgentEnrollmentPurpose::Manager,
        None,
        "append-manager",
    );
    let member_run = enroll(
        &db,
        &child,
        AgentEnrollmentPurpose::Member,
        Some(&manager),
        "append-member",
    );
    db.register_agent_team_plan(&manager, &root, 0, plan(&root, &child), "base")
        .unwrap();
    let old = db
        .reserve_team_cli_attempt(
            &owner(),
            PROJECT,
            &root,
            &root,
            0,
            "original",
            "default",
            Some(60),
        )
        .unwrap()
        .attempt;
    let old = db
        .claim_team_cli_dispatch(&owner(), &old.attempt_id)
        .unwrap()
        .attempt;
    let old = db
        .bind_team_cli_job(&owner(), &old.attempt_id, "original-job")
        .unwrap();
    let old = db
        .reconcile_team_cli_attempt(
            &owner(),
            &old.attempt_id,
            old.observation_revision,
            TeamCliObservation {
                job_id: Some("original-job".into()),
                dispatch_state: TeamCliDispatchState::Unknown,
                execution_state: Some(ShellCommandExecutionState::OutcomeUnknown),
                error_code: Some("original-uncertain".into()),
                result_summary: None,
            },
        )
        .unwrap();
    let mut extra = TeamPlanInput {
        manager: member(&root, 1),
        subagents: vec![member(&child, 0)],
    };
    extra.manager.cli_calls[0].model = Default::default();
    extra.manager.cli_calls[0].effort = None;
    extra.manager.cli_calls[0].instruction =
        Some("Independent three-directory CRUD validation".into());
    assert!(db
        .register_agent_team_cli_round(&member_run, &root, 0, 0, extra.clone(), "not-manager")
        .is_err());
    let added = db
        .register_agent_team_cli_round(&manager, &root, 0, 0, extra.clone(), "append")
        .unwrap();
    assert_eq!(added.round, 1);
    assert_eq!(added.first_call_indices[&root], 2);
    assert_eq!(added.first_call_indices[&child], 1);
    assert_eq!(
        serde_json::to_value(db.read_team_cli_attempt(&owner(), &old.attempt_id).unwrap()).unwrap(),
        serde_json::to_value(&old).unwrap()
    );
    assert_eq!(
        db.list_team_cli_attempts(&owner(), PROJECT, Some(&root), 0, 20)
            .unwrap()
            .total_count,
        1
    );
    let slot = db
        .read_agent_cli_planned_slot(&manager, &root, &root, 2)
        .unwrap();
    assert!(slot.model.is_cli_default() && slot.effort.is_none());
    assert_eq!(
        slot.instruction,
        "Independent three-directory CRUD validation"
    );
    let next = db
        .reserve_team_cli_attempt(
            &owner(),
            PROJECT,
            &root,
            &root,
            2,
            "new-independent-call",
            "default",
            Some(60),
        )
        .unwrap()
        .attempt;
    assert_ne!(next.attempt_id, old.attempt_id);
    assert_eq!(
        db.register_agent_team_cli_round(&manager, &root, 0, 0, extra.clone(), "append")
            .unwrap()
            .round,
        1
    );
    assert_eq!(
        db.register_agent_team_cli_round(&manager, &root, 0, 0, extra.clone(), "different-key")
            .unwrap_err()
            .code(),
        "team_round_revision_conflict"
    );
    assert_eq!(
        db.revoke_agent_run(
            manager.owner_user(),
            manager.project(),
            manager.agent_id(),
            manager.run_id()
        )
        .unwrap_err()
        .code(),
        "agent_execution_close_required"
    );
    assert_eq!(
        serde_json::to_value(db.read_team_cli_attempt(&owner(), &old.attempt_id).unwrap()).unwrap(),
        serde_json::to_value(&old).unwrap()
    );
}
