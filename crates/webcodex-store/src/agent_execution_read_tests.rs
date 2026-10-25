use super::*;
use webcodex_core::runner_protocol::{compute_launch_intent_hash, AgentExecutionProcess};

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
struct Fixture {
    _temp: tempfile::TempDir,
    db: Database,
    manager: VerifiedAgentRun,
    member: VerifiedAgentRun,
    root: String,
    child: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_member_effort(Some("medium".into()))
    }
    fn with_member_effort(member_effort: Option<String>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(&temp.path().join("read-synthetic.db")).unwrap();
        let root = db
            .create_agent_task(
                &crate::agent_isolation::enrollment_principal_for_execution("owner"),
                crate::NewAgentTask {
                    title: "Root".into(),
                    instruction: "Public root assignment".into(),
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
                    brief: "Public root assignment".into(),
                    inputs: vec![],
                    room_ids: vec![],
                    policy: AgentEnrollmentPolicy {
                        max_subagents: 1,
                        max_cli_calls: 2,
                    },
                    expires_at_unix_ms: now() + 60_000,
                },
                None,
                "manager",
                now(),
            )
            .unwrap();
        let manager = db
            .claim_agent_enrollment(
                "principal",
                "manager-window",
                &issued.enrollment_id,
                &issued.grant.unwrap().into_secret(),
                now(),
            )
            .unwrap()
            .binding;
        let child = db
            .create_agent_member(
                &manager,
                AgentMemberRequest {
                    title: "Child".into(),
                    brief: "Public child assignment".into(),
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
        let delivery = db
            .issue_agent_enrollment_delivery(&manager, &child.enrollment_id, "deliver", now())
            .unwrap();
        let member = db
            .claim_agent_enrollment(
                "principal",
                "child-window",
                &child.enrollment_id,
                &delivery.grant.unwrap().into_secret(),
                now(),
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
                        inputs: vec![],
                        provider_id: "workbuddy".into(),
                        model: "manager-model".into(),
                        effort: Some("high".into()),
                        instruction: Some("Approved manager instruction".into()),
                    }],
                },
                subagents: vec![TeamAgentPlan {
                    task_id: child.task_id.clone(),
                    cli_calls: vec![TeamCliCall {
                        inputs: vec![],
                        provider_id: "workbuddy".into(),
                        model: "member-model".into(),
                        effort: member_effort,
                        instruction: Some("Approved member instruction".into()),
                    }],
                }],
            },
            "plan",
        )
        .unwrap();
        Self {
            _temp: temp,
            db,
            manager,
            member,
            root,
            child: child.task_id,
        }
    }
    fn request(&self) -> AgentCliPreparationRequest {
        AgentCliPreparationRequest {
            root_task_id: self.root.clone(),
            source_task_id: self.root.clone(),
            call_index: 0,
            idempotency_key: "original-cli".into(),
            fallback_thinking_profile_id: "original-profile".into(),
            timeout_secs: Some(120),
            target: AgentExecutionTarget {
                client_id: "original-client".into(),
                runner_instance_id: "original-instance".into(),
            },
            carrier: AgentExecutionCarrier::Job {
                job_id: "original-job".into(),
                start_request_id: "original-request".into(),
            },
        }
    }
    fn read(&self) -> Option<AgentCliPreparationSnapshot> {
        self.db
            .read_agent_cli_preparation(
                &self.manager,
                &self.root,
                &self.root,
                0,
                "original-cli",
                "original-profile",
                Some(120),
            )
            .unwrap()
    }
    fn counts(&self) -> (i64, i64) {
        self.db.conn_for_tests().query_row("SELECT (SELECT COUNT(*) FROM wc_team_cli_attempts),(SELECT COUNT(*) FROM wc_isolated_agents)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap()
    }
}

#[test]
fn cli_read_slot_exposes_only_own_frozen_call_and_rechecks_current_run() {
    let f = Fixture::new();
    let count = f.counts();
    let member =
        f.db.read_agent_cli_planned_slot(&f.member, &f.root, &f.child, 0)
            .unwrap();
    assert_eq!(member.model, "member-model");
    assert_eq!(member.effort.as_deref(), Some("medium"));
    assert_eq!(member.instruction, "Approved member instruction");
    assert_eq!(member.source_task_id, f.child);
    let manager =
        f.db.read_agent_cli_planned_slot(&f.manager, &f.root, &f.root, 0)
            .unwrap();
    assert_eq!(manager.model, "manager-model");
    assert_eq!(manager.effort.as_deref(), Some("high"));
    assert!(f
        .db
        .read_agent_cli_planned_slot(&f.member, &f.root, &f.root, 0)
        .is_err());
    assert!(f
        .db
        .read_agent_cli_planned_slot(&f.manager, &f.root, &f.child, 0)
        .is_err());
    assert!(f
        .db
        .read_agent_cli_planned_slot(&f.member, &f.root, &f.child, 1)
        .is_err());
    let outsider =
        f.db.create_isolated_agent("owner", "other-project", None)
            .unwrap();
    let outsider = f.db.authenticate_agent_run(&outsider.token).unwrap();
    assert!(f
        .db
        .read_agent_cli_planned_slot(&outsider, &f.root, &f.child, 0)
        .is_err());
    assert_eq!(f.counts(), (count.0, count.1 + 1));
    f.db.begin_agent_execution_close("owner", "project", &run_ref(&f.member), now() + 60_000)
        .unwrap();
    f.db.revoke_agent_run("owner", "project", f.member.agent_id(), f.member.run_id())
        .unwrap();
    assert!(f
        .db
        .read_agent_cli_planned_slot(&f.member, &f.root, &f.child, 0)
        .is_err());
}

#[test]
fn cli_read_slot_preserves_explicit_no_effort_without_defaulting() {
    let f = Fixture::with_member_effort(None);
    let slot =
        f.db.read_agent_cli_planned_slot(&f.member, &f.root, &f.child, 0)
            .unwrap();
    assert_eq!(slot.effort, None);
    assert_eq!(slot.model, "member-model");
    assert_eq!(slot.instruction, "Approved member instruction");
}

#[test]
fn cli_read_exact_retry_recovers_original_target_without_reserving_or_exposing_launch() {
    let f = Fixture::new();
    assert!(f.read().is_none());
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let p = first.preparation.unwrap();
    let count = f.counts();
    let snapshot = f.read().unwrap();
    assert_eq!(snapshot.phase, AgentCliPreparationPhase::Prepared);
    let identity = snapshot.binding.unwrap();
    assert_eq!(&identity, p.binding_identity());
    assert!(snapshot.resume_fence.is_none());
    // The controller retries with these original IDs, without a second Registry
    // target reservation. Store returns the same logical worker and original slot.
    let mut retry = f.request();
    retry.target = identity.target.clone();
    retry.carrier = identity.carrier.clone();
    let replay = f.db.prepare_agent_cli_worker(&f.manager, retry).unwrap();
    assert_eq!(replay.preparation.unwrap().binding_identity(), &identity);
    assert_eq!(f.counts(), count);
    for (key, profile, timeout) in [
        ("different-key", "original-profile", Some(120)),
        ("original-cli", "different-profile", Some(120)),
        ("original-cli", "original-profile", Some(121)),
    ] {
        assert_eq!(
            f.db.read_agent_cli_preparation(&f.manager, &f.root, &f.root, 0, key, profile, timeout)
                .unwrap_err()
                .code(),
            "team_idempotency_conflict"
        );
    }
    assert!(f
        .db
        .read_agent_cli_preparation(
            &f.member,
            &f.root,
            &f.root,
            0,
            "original-cli",
            "original-profile",
            Some(120)
        )
        .is_err());
    // An own empty slot cannot probe another Agent's owner-wide key namespace.
    assert!(f
        .db
        .read_agent_cli_preparation(
            &f.member,
            &f.root,
            &f.child,
            0,
            "original-cli",
            "original-profile",
            Some(120)
        )
        .unwrap()
        .is_none());
    let process = AgentExecutionProcess {
        executable: "C:\\synthetic\\worker.exe".into(),
        args: vec!["private-launch-argument".into()],
    };
    let stdin = Some("private-launch-input".into());
    let mut full = identity.clone();
    full.launch_intent_hash = compute_launch_intent_hash(&process, stdin.as_deref(), 120).unwrap();
    full.resource_manifest_hash = "a".repeat(64);
    let permit =
        f.db.finalize_agent_cli_execution(
            &f.manager,
            p,
            AgentExecutionStart {
                binding: full.clone(),
                resource_set_id: "resources".into(),
                process,
                stdin,
                timeout_secs: 120,
            },
        )
        .unwrap();
    let watch = permit.observation_permit();
    let revision = f.read().unwrap().receipt.observation_revision;
    f.db.observe_agent_cli_execution(
        &watch,
        &full.target,
        revision,
        TeamCliObservation {
            job_id: None,
            dispatch_state: TeamCliDispatchState::Unknown,
            execution_state: Some(ShellCommandExecutionState::OutcomeUnknown),
            error_code: None,
            result_summary: Some("private-unpublished-result".into()),
        },
    )
    .unwrap();
    let unknown = f.read().unwrap();
    assert_eq!(unknown.phase, AgentCliPreparationPhase::Finalized);
    assert_eq!(
        unknown.receipt.dispatch_state,
        TeamCliDispatchState::Unknown
    );
    assert!(unknown.resume_fence.is_none());
    assert_eq!(unknown.binding, Some(full));
    let visible = format!("{unknown:?}");
    for private in [
        "private-launch-argument",
        "private-launch-input",
        "private-unpublished-result",
    ] {
        assert!(!visible.contains(private));
    }
    assert!(f
        .db
        .prepare_agent_cli_worker(&f.manager, f.request())
        .unwrap()
        .preparation
        .is_none());
    assert_eq!(f.counts(), count);
}

#[test]
fn cli_read_cancelled_exposes_only_prior_identity_fence_and_never_recreates_worker() {
    let f = Fixture::new();
    let first =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap();
    let identity = first.preparation.unwrap().binding_identity().clone();
    let count = f.counts();
    f.db.begin_agent_execution_close("owner", "project", &run_ref(&f.manager), now() + 60_000)
        .unwrap();
    let snapshot = f.read().unwrap();
    assert_eq!(snapshot.phase, AgentCliPreparationPhase::Cancelled);
    assert_eq!(
        snapshot.resume_fence,
        Some(identity.identity_hash().unwrap())
    );
    assert_eq!(snapshot.binding, Some(identity));
    assert_eq!(snapshot.receipt.attempt_id, first.receipt.attempt_id);
    assert_eq!(
        snapshot.receipt.dispatch_state,
        TeamCliDispatchState::Terminal
    );
    let replay = f.read().unwrap();
    assert_eq!(
        replay.receipt.observation_revision,
        snapshot.receipt.observation_revision
    );
    assert_eq!(f.counts(), count);
    assert!(f
        .db
        .prepare_agent_cli_worker(&f.manager, f.request())
        .is_err());
    assert_eq!(f.counts(), count);
}

#[test]
fn cli_read_recoverable_preparation_checks_actual_store_authority_and_returns_same_identity() {
    let f = Fixture::new();
    let p =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap();
    let identity = p.binding_identity().clone();
    let count = f.counts();
    assert_eq!(
        f.db.recoverable_agent_cli_preparation(&f.manager, &identity)
            .unwrap(),
        identity
    );
    assert_eq!(
        f.db.recoverable_agent_cli_preparation(&f.manager, &identity)
            .unwrap(),
        identity
    );
    assert_eq!(f.counts(), count);
    let mut forged = identity.clone();
    forged.target.runner_instance_id = "different-instance".into();
    assert!(f
        .db
        .recoverable_agent_cli_preparation(&f.manager, &forged)
        .is_err());
    let mut forged = identity.clone();
    forged.requester.run = run_ref(&f.member);
    assert!(f
        .db
        .recoverable_agent_cli_preparation(&f.manager, &forged)
        .is_err());
    let mut forged = identity.clone();
    forged.executor.task_id = f.root.clone();
    assert!(f
        .db
        .recoverable_agent_cli_preparation(&f.manager, &forged)
        .is_err());
    assert!(f
        .db
        .recoverable_agent_cli_preparation(&f.member, &identity)
        .is_err());
    let process = AgentExecutionProcess {
        executable: "C:\\synthetic\\worker.exe".into(),
        args: vec![],
    };
    let mut full = identity.clone();
    full.launch_intent_hash = compute_launch_intent_hash(&process, None, 120).unwrap();
    full.resource_manifest_hash = "a".repeat(64);
    f.db.finalize_agent_cli_execution(
        &f.manager,
        p,
        AgentExecutionStart {
            binding: full,
            resource_set_id: "resources".into(),
            process,
            stdin: None,
            timeout_secs: 120,
        },
    )
    .unwrap();
    assert!(f
        .db
        .recoverable_agent_cli_preparation(&f.manager, &identity)
        .is_err());
    assert_eq!(f.counts(), count);
}

#[test]
fn cli_read_recoverable_preparation_rejects_worker_close_without_closing_current_caller() {
    let f = Fixture::new();
    let p =
        f.db.prepare_agent_cli_worker(&f.manager, f.request())
            .unwrap()
            .preparation
            .unwrap();
    let identity = p.binding_identity().clone();
    f.db.begin_agent_execution_close("owner", "project", &identity.executor.run, now() + 60_000)
        .unwrap();
    assert!(f.db.validate_agent_run(&f.manager).is_ok());
    ensure_agent_run_execution_open_on_connection(&f.db.conn_for_tests(), &run_ref(&f.manager))
        .unwrap();
    assert!(f
        .db
        .recoverable_agent_cli_preparation(&f.manager, &identity)
        .is_err());
}
