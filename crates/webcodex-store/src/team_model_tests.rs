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
                ..TeamPolicyInput::default()
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
