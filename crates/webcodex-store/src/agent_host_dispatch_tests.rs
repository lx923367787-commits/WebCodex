use super::*;
use crate::{
    AgentMemberRequest, TeamAgentObservation, TeamAgentPlan, TeamAgentResolution, TeamCliCall,
    TeamPlanInput,
};
use std::sync::{Arc, Barrier};
use webcodex_core::runner_protocol::*;

const PROJECT: &str = "agent:runner:queue-test";
const OWNER: &str = "synthetic-owner-id";
const HOST: &str = "synthetic-host-principal";

fn create_manager(db: &Database, key: &str) -> (VerifiedAgentRun, String) {
    let owner = enrollment_principal_for_execution(OWNER);
    let root = db
        .create_agent_task(
            &owner,
            NewAgentTask {
                title: key.into(),
                instruction: "Explicit manager task".into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(PROJECT.into()),
                idempotency_key: key.into(),
            },
        )
        .unwrap()
        .task
        .summary
        .task_id;
    let mut issued = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Manager,
                owner_user: OWNER.into(),
                project: PROJECT.into(),
                principal_key: HOST.into(),
                task_id: root.clone(),
                task_directory: format!("workspace/{key}"),
                brief: "Explicit manager scope".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 4,
                    max_cli_calls: 4,
                },
                expires_at_unix_ms: now() + 60_000,
            },
            None,
            key,
            now(),
        )
        .unwrap();
    let manager = db
        .claim_agent_enrollment(
            HOST,
            &format!("window-{key}"),
            &issued.enrollment_id,
            &issued.grant.take().unwrap().into_secret(),
            now(),
        )
        .unwrap()
        .binding;
    (manager, root)
}

struct Fixture {
    dir: tempfile::TempDir,
    db: Arc<Database>,
    manager: VerifiedAgentRun,
    root: String,
    child: crate::AgentMemberReceipt,
    owner: CommunicationPrincipal,
}
impl Fixture {
    fn new(cli_calls: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("state.db")).unwrap());
        let (manager, root) = create_manager(&db, "manager");
        let child = db
            .create_agent_member(
                &manager,
                AgentMemberRequest {
                    title: "Explicit child".into(),
                    brief: "Only the child's own assigned task".into(),
                    task_directory: "workspace/child".into(),
                    inputs: vec![],
                    room_ids: vec![],
                    policy: AgentEnrollmentPolicy {
                        max_subagents: 0,
                        max_cli_calls: 0,
                    },
                },
                "child",
            )
            .unwrap();
        db.register_agent_team_plan(
            &manager,
            &root,
            0,
            TeamPlanInput {
                manager: TeamAgentPlan {
                    task_id: root.clone(),
                    cli_calls: (0..cli_calls)
                        .map(|i| TeamCliCall {
                            provider_id: "workbuddy".into(),
                            model: "frozen-model".into(),
                            effort: Some("high".into()),
                            instruction: Some(format!("Frozen independent CLI instruction {i}")),
                        })
                        .collect(),
                },
                subagents: vec![TeamAgentPlan {
                    task_id: child.task_id.clone(),
                    cli_calls: vec![],
                }],
            },
            "plan",
        )
        .unwrap();
        Self {
            dir,
            db,
            manager,
            root,
            child,
            owner: enrollment_principal_for_execution(OWNER),
        }
    }
    fn bind_member(&self) -> AgentHostQueueBinding {
        self.db
            .reserve_agent_host_member(
                &self.manager,
                &self.root,
                &self.child.task_id,
                &self.child.enrollment_id,
                "member-queue",
                "profile-frozen",
            )
            .unwrap()
    }
    fn child_claim(&self, claim: AgentHostDispatchClaim, window: &str) -> VerifiedAgentRun {
        self.db
            .claim_agent_enrollment(
                HOST,
                window,
                &claim.status.binding.enrollment_id,
                &claim.grant.unwrap().into_secret(),
                now(),
            )
            .unwrap()
            .binding
    }
    fn failed_cli(
        &self,
        index: u32,
    ) -> (
        crate::AgentCliObservationPermit,
        ShellJobInfo,
        VerifiedAgentRun,
        AgentExecutionBinding,
    ) {
        let target = AgentExecutionTarget {
            client_id: "original-runner".into(),
            runner_instance_id: "original-instance".into(),
        };
        let job_id = format!("original-job-{index}");
        let request_id = format!("original-request-{index}");
        let preparation = self
            .db
            .prepare_agent_cli_worker(
                &self.manager,
                crate::AgentCliPreparationRequest {
                    root_task_id: self.root.clone(),
                    source_task_id: self.root.clone(),
                    call_index: index,
                    idempotency_key: format!("cli-{index}"),
                    fallback_thinking_profile_id: "profile-frozen".into(),
                    timeout_secs: Some(120),
                    target: target.clone(),
                    carrier: AgentExecutionCarrier::Job {
                        job_id: job_id.clone(),
                        start_request_id: request_id.clone(),
                    },
                },
            )
            .unwrap()
            .preparation
            .unwrap();
        let worker = preparation.worker().clone();
        let mut bound = preparation.binding_identity().clone();
        let process = AgentExecutionProcess {
            executable: "C:\\synthetic\\worker.exe".into(),
            args: vec!["run".into()],
        };
        bound.launch_intent_hash = compute_launch_intent_hash(&process, None, 120).unwrap();
        bound.resource_manifest_hash = "a".repeat(64);
        let start = AgentExecutionStart {
            binding: bound.clone(),
            resource_set_id: format!("resources-{index}"),
            process,
            stdin: None,
            timeout_secs: 120,
        };
        let permit = self
            .db
            .finalize_agent_cli_execution(&self.manager, preparation, start)
            .unwrap();
        let observation = permit.observation_permit();
        let attempt = self
            .db
            .bind_agent_cli_execution_job(&observation, &target)
            .unwrap();
        self.db
            .observe_agent_cli_execution(
                &observation,
                &target,
                attempt.observation_revision,
                crate::TeamCliObservation {
                    job_id: Some(job_id.clone()),
                    dispatch_state: crate::TeamCliDispatchState::Terminal,
                    execution_state: Some(ShellCommandExecutionState::Completed),
                    error_code: Some("quota_exhausted".into()),
                    result_summary: None,
                },
            )
            .unwrap();
        let job:ShellJobInfo=serde_json::from_value(serde_json::json!({
            "job_id":job_id,"request_id":request_id,"client_id":target.client_id,"project_id":PROJECT,"kind":"shell",
            "shell":"direct_argv","command_preview":"synthetic CLI","status":"failed","created_at":100,"started_at":101,
            "ended_at":102,"exit_code":1,"command_execution_state":"completed","last_update_seq":2,
            "structured_execution":{"execution_source":"run_process","arg_count":2,"stdin_present":false}
        })).unwrap();
        (observation, job, worker, bound)
    }
    fn fallback(
        &self,
        index: u32,
    ) -> (
        crate::CliFallbackRecord,
        VerifiedAgentRun,
        AgentExecutionBinding,
    ) {
        let (_permit, job, worker, binding) = self.failed_cli(index);
        let fallback = self
            .db
            .reserve_cli_fallback(
                &self.owner,
                &binding.requester.cli_attempt_id,
                &job,
                webcodex_core::cli_fallback::CliFailureReason::QuotaExhausted,
                webcodex_core::cli_fallback::CliCancellationOrigin::None,
            )
            .unwrap();
        (fallback, worker, binding)
    }
    fn prove_worker_stopped(&self, worker: &VerifiedAgentRun) {
        let plan = self
            .db
            .begin_agent_execution_close(
                OWNER,
                PROJECT,
                &AgentRunRef {
                    agent_id: worker.agent_id().into(),
                    agent_run_id: worker.run_id().into(),
                    generation: worker.generation(),
                },
                now() + 30_000,
            )
            .unwrap();
        assert_eq!(plan.requests().len(), 1);
        let request = &plan.requests()[0];
        let receipt = TrustedFixtureReceipt(AgentExecutionCloseReceipt {
            version: 1,
            close_ref: request.close_ref.clone(),
            target: request.target.clone(),
            deadline_at_ms: request.deadline_at_ms,
            members_sha256: request.members_sha256.clone(),
            observation_revision: 1,
            admission_closed: true,
            tombstone_persisted: true,
            receipts: request
                .members
                .iter()
                .map(|member| AgentExecutionStopReceipt {
                    binding_sha256: member.binding_hash().unwrap(),
                    carrier: member.carrier.clone(),
                    observation_revision: 1,
                    outcome: AgentExecutionStopOutcome::TreeExited {
                        leader_exit_code: Some(1),
                        leader_handle_signaled: true,
                        membership_complete: true,
                        member_handles_signaled: true,
                        worker_finished: true,
                    },
                })
                .collect(),
        });
        assert!(
            self.db
                .accept_agent_execution_close_receipt(&receipt)
                .unwrap()
                .complete
        );
    }
}

// Simulates only the already-authenticated transport receipt boundary. No
// process is launched or stopped, and no arbitrary model/wire DTO is trusted.
struct TrustedFixtureReceipt(AgentExecutionCloseReceipt);
impl crate::agent_execution_close::AuthenticatedAgentExecutionCloseReceipt
    for TrustedFixtureReceipt
{
    fn authenticated_receipt(&self) -> &AgentExecutionCloseReceipt {
        &self.0
    }
}

#[test]
fn host_dispatch_member_binding_delivery_and_reopen_are_exact_and_secret_once() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    assert_eq!(f.bind_member(), b);
    assert_eq!(b.agent_id, f.child.agent_id);
    assert!(f
        .db
        .claim_team_agent_attempt(&f.owner, &b.queue_id)
        .is_err());
    let (other, _) = create_manager(&f.db, "other-manager");
    assert!(f
        .db
        .read_agent_host_dispatch(&other, AgentHostQueueKind::Member, &b.queue_id)
        .is_err());
    let claim =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert!(claim.dispatch_permitted);
    assert_eq!(claim.status.dispatch_count, 1);
    assert_eq!(claim.status.delivery_state, "delivery_unknown");
    let cap = claim.grant.unwrap().into_secret();
    assert!(!serde_json::to_string(&claim.status).unwrap().contains(&cap));
    assert!(f.db.authenticate_agent_run(&cap).is_err());
    let reopened = Database::open(&f.dir.path().join("state.db")).unwrap();
    let replay = reopened
        .claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
        .unwrap();
    assert!(!replay.dispatch_permitted && replay.grant.is_none());
    assert_eq!(replay.status.binding, b);
    assert_eq!(replay.status.dispatch_count, 1);
    let stored: String = reopened
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT delivery_secret_hash FROM wc_agent_enrollments WHERE enrollment_id=?1",
            [&b.enrollment_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(stored, cap);
    let admitted = reopened
        .claim_agent_enrollment(HOST, "child-window", &b.enrollment_id, &cap, now())
        .unwrap();
    assert_eq!(admitted.binding.agent_id(), b.agent_id);
    assert_eq!(admitted.projection.task_id, b.task_id);
    assert_eq!(
        reopened
            .read_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap()
            .delivery_state,
        "claimed"
    );
}

#[test]
fn host_dispatch_scoped_legacy_claim_cannot_send_before_binding() {
    let f = Fixture::new(0);
    let old =
        f.db.reserve_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &f.child.task_id,
            "member-queue",
            "profile-frozen",
        )
        .unwrap();
    assert_eq!(
        f.db.claim_team_agent_attempt(&f.owner, &old.record.team_agent_attempt_id)
            .unwrap_err()
            .code(),
        "agent_host_queue_binding_required"
    );
    let b = f.bind_member();
    assert_eq!(b.queue_id, old.record.team_agent_attempt_id);
    assert_eq!(
        f.db.reserve_agent_host_member(
            &f.manager,
            &f.root,
            &f.child.task_id,
            &f.child.enrollment_id,
            "member-queue",
            "changed-profile"
        )
        .unwrap_err()
        .code(),
        "team_agent_idempotency_conflict"
    );
}

#[test]
fn host_dispatch_delivery_persistence_failure_rolls_back_original_queue_claim() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    f.db.conn.lock().unwrap().execute_batch("CREATE TRIGGER synthetic_delivery_failure BEFORE UPDATE OF delivery_secret_hash ON wc_agent_enrollments WHEN NEW.delivery_state='delivery_unknown' BEGIN SELECT RAISE(ABORT,'synthetic delivery persistence failure'); END;").unwrap();
    assert!(f
        .db
        .claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
        .is_err());
    let status =
        f.db.read_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert_eq!(status.queue_state, "queued");
    assert_eq!(status.dispatch_count, 0);
    assert_eq!(status.delivery_state, "reserved");
    f.db.conn
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER synthetic_delivery_failure;")
        .unwrap();
    assert!(
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap()
            .dispatch_permitted
    );
}

#[test]
fn host_dispatch_concurrent_connections_have_one_grant_and_one_original_claim() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    let other = Database::open(&f.dir.path().join("state.db")).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = barrier.clone();
    let manager = f.manager.clone();
    let queue = b.queue_id.clone();
    let joined = std::thread::spawn(move || {
        other_barrier.wait();
        other
            .claim_agent_host_dispatch(&manager, AgentHostQueueKind::Member, &queue)
            .unwrap()
    });
    barrier.wait();
    let first =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    let second = joined.join().unwrap();
    assert_eq!(
        usize::from(first.dispatch_permitted) + usize::from(second.dispatch_permitted),
        1
    );
    assert_eq!(
        usize::from(first.grant.is_some()) + usize::from(second.grant.is_some()),
        1
    );
    assert_eq!(
        first.status.dispatch_attempt_id,
        second.status.dispatch_attempt_id
    );
    assert_eq!(
        f.db.read_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap()
            .dispatch_count,
        1
    );
}

#[test]
fn host_dispatch_profile_and_busy_keep_original_four_send_budget_and_revoke_old_caps() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    let claim =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    let fence = claim.status.dispatch_attempt_id.unwrap();
    let first = claim.grant.unwrap().into_secret();
    f.db.record_team_agent_observation(
        &f.owner,
        &b.queue_id,
        &fence,
        TeamAgentObservation::ProfileUnavailable {
            evidence: "thinking_profile_not_discovered".into(),
        },
    )
    .unwrap();
    assert_eq!(
        f.db.read_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap()
            .dispatch_count,
        0
    );
    assert!(f
        .db
        .claim_agent_enrollment(HOST, "bad-old-window", &b.enrollment_id, &first, now())
        .is_err());
    for count in 1..=4 {
        let claim =
            f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
                .unwrap();
        assert!(claim.dispatch_permitted);
        assert_eq!(claim.status.dispatch_count, count);
        let fence = claim.status.dispatch_attempt_id.unwrap();
        let cap = claim.grant.unwrap().into_secret();
        f.db.record_team_agent_observation(
            &f.owner,
            &b.queue_id,
            &fence,
            TeamAgentObservation::Busy {
                evidence: "browser_worker_busy:launched=false".into(),
            },
        )
        .unwrap();
        assert!(f
            .db
            .claim_agent_enrollment(HOST, "stale-window", &b.enrollment_id, &cap, now())
            .is_err());
    }
    let last =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert!(!last.dispatch_permitted && last.grant.is_none());
    assert_eq!(last.status.queue_state, "confirmed_not_started");
    assert_eq!(last.status.dispatch_count, 4);
    assert_eq!(last.status.binding.agent_id, b.agent_id);
}

#[test]
fn host_dispatch_unknown_or_claimed_target_can_never_be_reset_as_busy() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    let claim =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    let fence = claim.status.dispatch_attempt_id.clone().unwrap();
    f.db.record_team_agent_observation(
        &f.owner,
        &b.queue_id,
        &fence,
        TeamAgentObservation::Unknown { request_id: None },
    )
    .unwrap();
    assert!(f
        .db
        .record_team_agent_observation(
            &f.owner,
            &b.queue_id,
            &fence,
            TeamAgentObservation::Busy {
                evidence: "browser_worker_busy:launched=false".into()
            }
        )
        .is_err());
    let replay =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert!(!replay.dispatch_permitted && replay.grant.is_none());
    assert_eq!(replay.status.queue_state, "unknown");
    let other = Fixture::new(0);
    let b = other.bind_member();
    let claim = other
        .db
        .claim_agent_host_dispatch(&other.manager, AgentHostQueueKind::Member, &b.queue_id)
        .unwrap();
    let fence = claim.status.dispatch_attempt_id.clone().unwrap();
    let _child = other.child_claim(claim, "already-claimed");
    assert!(other
        .db
        .record_team_agent_observation(
            &other.owner,
            &b.queue_id,
            &fence,
            TeamAgentObservation::Busy {
                evidence: "browser_worker_busy:launched=false".into()
            }
        )
        .is_err());
    let preserved = other
        .db
        .read_agent_host_dispatch(&other.manager, AgentHostQueueKind::Member, &b.queue_id)
        .unwrap();
    assert_eq!(preserved.queue_state, "starting");
    assert_eq!(preserved.delivery_state, "claimed");
}

#[test]
fn host_dispatch_fallback_requires_original_stop_proof_and_uses_independent_zero_budget_task() {
    let f = Fixture::new(1);
    let (fallback, worker, _binding) = f.fallback(0);
    let before: i64 =
        f.db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r.get(0))
            .unwrap();
    assert_eq!(
        f.db.bind_agent_host_fallback(&f.manager, &fallback.fallback_id)
            .unwrap_err()
            .code(),
        "agent_execution_stop_proof_required"
    );
    let after: i64 =
        f.db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r.get(0))
            .unwrap();
    assert_eq!(before, after);
    f.prove_worker_stopped(&worker);
    let b =
        f.db.bind_agent_host_fallback(&f.manager, &fallback.fallback_id)
            .unwrap();
    assert_eq!(
        f.db.bind_agent_host_fallback(&f.manager, &fallback.fallback_id)
            .unwrap(),
        b
    );
    assert_ne!(b.task_id, f.root);
    assert_ne!(b.agent_id, f.manager.agent_id());
    assert_ne!(b.agent_id, worker.agent_id());
    assert_eq!(
        f.db.read_agent_enrollment(&f.manager).unwrap().task_id,
        f.root
    );
    assert!(f
        .db
        .claim_cli_fallback(&f.owner, &fallback.fallback_id)
        .is_err());
    let claim =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Fallback, &b.queue_id)
            .unwrap();
    let recovery = f.child_claim(claim, "recovery-window");
    let assignment = f.db.read_agent_enrollment(&recovery).unwrap();
    assert_eq!(assignment.task_id, b.task_id);
    assert_eq!(assignment.purpose, AgentEnrollmentPurpose::Fallback);
    assert_eq!(
        assignment.policy,
        AgentEnrollmentPolicy {
            max_subagents: 0,
            max_cli_calls: 0
        }
    );
    assert!(assignment.room_ids.is_empty());
    assert!(f
        .db
        .authorize_agent_assignment(&recovery, &b.task_id, AgentAssignmentOperation::UseCli)
        .is_err());
    assert!(f
        .db
        .authorize_agent_assignment(&recovery, &b.task_id, AgentAssignmentOperation::Delegate)
        .is_err());
    assert_eq!(
        f.db.read_agent_enrollment(&f.manager).unwrap().task_id,
        f.root
    );
}

#[test]
fn host_dispatch_fallback_original_fifo_slot_and_unknown_occupancy_are_preserved() {
    let f = Fixture::new(2);
    let (a, worker_a, _) = f.fallback(0);
    f.prove_worker_stopped(&worker_a);
    let (b, worker_b, _) = f.fallback(1);
    f.prove_worker_stopped(&worker_b);
    let first =
        f.db.bind_agent_host_fallback(&f.manager, &a.fallback_id)
            .unwrap();
    let second =
        f.db.bind_agent_host_fallback(&f.manager, &b.fallback_id)
            .unwrap();
    let waiting =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Fallback, &second.queue_id)
            .unwrap();
    assert!(!waiting.dispatch_permitted && waiting.grant.is_none());
    assert_eq!(waiting.status.delivery_state, "reserved");
    let active =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Fallback, &first.queue_id)
            .unwrap();
    f.db.record_cli_fallback_observation(
        &f.owner,
        &first.queue_id,
        active.status.dispatch_attempt_id.as_deref().unwrap(),
        crate::CliFallbackObservation::Unknown,
    )
    .unwrap();
    let waiting =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Fallback, &second.queue_id)
            .unwrap();
    assert!(!waiting.dispatch_permitted && waiting.grant.is_none());
    assert_eq!(waiting.status.dispatch_count, 0);
}

#[test]
fn host_dispatch_quiescence_excludes_only_exact_incoming_actor_and_needs_task_terminal() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    let claim =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    let fence = claim.status.dispatch_attempt_id.clone().unwrap();
    let child = f.child_claim(claim, "child-physical");
    {
        let conn = f.db.conn.lock().unwrap();
        assert!(require_agent_host_dispatch_quiescent_on_connection(
            &conn,
            OWNER,
            PROJECT,
            &f.child.task_id,
            Some(&child)
        )
        .is_ok());
        assert!(require_agent_host_dispatch_quiescent_on_connection(
            &conn,
            OWNER,
            PROJECT,
            &f.root,
            Some(&f.manager)
        )
        .is_err());
    }
    let request = uuid::Uuid::new_v4().to_string();
    f.db.record_team_agent_observation(
        &f.owner,
        &b.queue_id,
        &fence,
        TeamAgentObservation::Dispatched {
            request_id: request,
            response_observed: true,
        },
    )
    .unwrap();
    f.db.resolve_team_agent_attempt(
        &f.owner,
        &b.queue_id,
        &fence,
        TeamAgentResolution::VerifiedTerminal,
        "trusted-host-terminal-receipt",
    )
    .unwrap();
    let conn = f.db.conn.lock().unwrap();
    assert!(require_agent_host_dispatch_quiescent_on_connection(
        &conn,
        OWNER,
        PROJECT,
        &f.root,
        Some(&f.manager)
    )
    .is_err());
    // Controlled fixture for the separate business-lifecycle input. The root
    // integration owns testing its public_task mutation/acceptance predicates.
    conn.execute(
        "UPDATE wc_agent_tasks SET state='succeeded',terminal_attempt_id='synthetic-business-terminal',terminal_at_unix_ms=1 WHERE task_id=?1",
        [&b.task_id],
    )
    .unwrap();
    assert!(require_agent_host_dispatch_quiescent_on_connection(
        &conn,
        OWNER,
        PROJECT,
        &f.root,
        Some(&f.manager)
    )
    .is_ok());
    let root_state: String = conn
        .query_row(
            "SELECT state FROM wc_agent_tasks WHERE task_id=?1",
            [&f.root],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        root_state, "ready",
        "child completion must never complete its caller"
    );
}

fn fixture_close_receipt(request: &AgentExecutionClose) -> TrustedFixtureReceipt {
    TrustedFixtureReceipt(AgentExecutionCloseReceipt {
        version: 1,
        close_ref: request.close_ref.clone(),
        target: request.target.clone(),
        deadline_at_ms: request.deadline_at_ms,
        members_sha256: request.members_sha256.clone(),
        observation_revision: 1,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: request
            .members
            .iter()
            .map(|member| AgentExecutionStopReceipt {
                binding_sha256: member.binding_hash().unwrap(),
                carrier: member.carrier.clone(),
                observation_revision: 1,
                outcome: AgentExecutionStopOutcome::TreeExited {
                    leader_exit_code: Some(1),
                    leader_handle_signaled: true,
                    membership_complete: true,
                    member_handles_signaled: true,
                    worker_finished: true,
                },
            })
            .collect(),
    })
}

#[test]
fn host_dispatch_retained_cli_permit_reserves_exact_fallback_after_revocation_without_second_cli() {
    use webcodex_core::cli_fallback::{CliCancellationOrigin, CliFailureReason};
    let f = Fixture::new(1);
    let (permit, job, worker, frozen) = f.failed_cli(0);
    let reserve = || {
        f.db.reserve_agent_cli_fallback(
            &permit,
            &frozen.target,
            &job,
            CliFailureReason::QuotaExhausted,
            CliCancellationOrigin::None,
        )
    };
    assert_eq!(
        reserve().unwrap_err().code(),
        "agent_execution_stop_proof_required"
    );
    let count: i64 =
        f.db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_cli_fallbacks", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 0);
    f.prove_worker_stopped(&worker);
    let mut wrong = frozen.target.clone();
    wrong.runner_instance_id = "replacement-instance".into();
    assert_eq!(
        f.db.reserve_agent_cli_fallback(
            &permit,
            &wrong,
            &job,
            CliFailureReason::QuotaExhausted,
            CliCancellationOrigin::None
        )
        .unwrap_err()
        .code(),
        "cli_fallback_source_job_mismatch"
    );
    let mut wrong_job = job.clone();
    wrong_job.request_id = Some("different-start-request".into());
    assert_eq!(
        f.db.reserve_agent_cli_fallback(
            &permit,
            &frozen.target,
            &wrong_job,
            CliFailureReason::QuotaExhausted,
            CliCancellationOrigin::None
        )
        .unwrap_err()
        .code(),
        "cli_fallback_source_job_mismatch"
    );
    // A retained watcher is not a new dispatch authority or a reactivated run.
    let manager_ref = AgentRunRef {
        agent_id: f.manager.agent_id().into(),
        agent_run_id: f.manager.run_id().into(),
        generation: f.manager.generation(),
    };
    let close =
        f.db.begin_agent_execution_close(OWNER, PROJECT, &manager_ref, now() + 30_000)
            .unwrap();
    for request in close.requests() {
        f.db.accept_agent_execution_close_receipt(&fixture_close_receipt(request))
            .unwrap();
    }
    assert!(
        f.db.agent_execution_close_status(OWNER, PROJECT, &manager_ref)
            .unwrap()
            .complete
    );
    f.db.revoke_agent_run(OWNER, PROJECT, worker.agent_id(), worker.run_id())
        .unwrap();
    f.db.revoke_agent_run(OWNER, PROJECT, f.manager.agent_id(), f.manager.run_id())
        .unwrap();
    let first = reserve().unwrap();
    assert!(!first.replayed);
    let second = reserve().unwrap();
    assert!(second.replayed);
    assert_eq!(second.fallback_id, first.fallback_id);
    assert_eq!(first.source_attempt_id, frozen.requester.cli_attempt_id);
    assert!(!serde_json::to_string(&first).unwrap().contains("principal"));
    assert!(f
        .db
        .bind_agent_host_fallback(&f.manager, &first.fallback_id)
        .is_err());
    let conn = f.db.conn.lock().unwrap();
    let cli_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM wc_team_cli_attempts", [], |r| {
            r.get(0)
        })
        .unwrap();
    let fallback_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM wc_cli_fallbacks", [], |r| r.get(0))
        .unwrap();
    let binding_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wc_agent_host_dispatch_bindings",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cli_count, 1);
    assert_eq!(fallback_count, 1);
    assert_eq!(binding_count, 0);
}

#[test]
fn host_dispatch_launch_material_is_frozen_authorized_and_never_contains_grant() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    let before =
        f.db.read_agent_host_launch_material(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert_eq!(before.instruction, "Only the child's own assigned task");
    assert_eq!(before.source_task_id, f.child.task_id);
    assert!(!before.cancel_requested);
    let claim =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    let material = claim.launch_material();
    assert_eq!(material.instruction, before.instruction);
    assert_eq!(
        material.status.dispatch_attempt_id,
        claim.status.dispatch_attempt_id
    );
    let secret = claim.grant.unwrap().into_secret();
    assert!(!serde_json::to_string(&material).unwrap().contains(&secret));
    let (other, _) = create_manager(&f.db, "material-foreign-manager");
    assert!(f
        .db
        .read_agent_host_launch_material(&other, AgentHostQueueKind::Member, &b.queue_id)
        .is_err());
}

#[test]
fn host_dispatch_pending_claim_gate_rechecks_original_cancel_and_dispatch_fence() {
    let f = Fixture::new(0);
    let b = f.bind_member();
    assert!(validate_host_enrollment_claim_on_connection(
        &f.db.conn.lock().unwrap(),
        &b.enrollment_id
    )
    .is_err());
    let claim =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert!(validate_host_enrollment_claim_on_connection(
        &f.db.conn.lock().unwrap(),
        &b.enrollment_id
    )
    .is_ok());
    f.db.cancel_root_team_agents(&f.owner, PROJECT, &f.root)
        .unwrap();
    assert_eq!(
        validate_host_enrollment_claim_on_connection(&f.db.conn.lock().unwrap(), &b.enrollment_id)
            .unwrap_err()
            .code(),
        "agent_host_enrollment_claim_denied"
    );
    let replay =
        f.db.claim_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert!(!replay.dispatch_permitted && replay.grant.is_none());
    assert!(replay.launch_material().cancel_requested);
    assert_eq!(
        replay.status.dispatch_attempt_id,
        claim.status.dispatch_attempt_id
    );
}

#[test]
fn host_dispatch_member_lookup_is_read_only_exact_intent_without_profile_refresh_or_new_slot() {
    let f = Fixture::new(0);
    let lookup = |key: &str, profile: &str| {
        f.db.lookup_agent_host_member(
            &f.manager,
            &f.root,
            &f.child.task_id,
            &f.child.enrollment_id,
            key,
            profile,
        )
    };
    assert!(lookup("member-queue", "profile-frozen").unwrap().is_none());
    let count: i64 =
        f.db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_team_agent_attempts", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(count, 0, "lookup must never reserve a new slot");
    let b = f.bind_member();
    assert_eq!(
        lookup("member-queue", "profile-frozen").unwrap(),
        Some(b.clone())
    );
    assert_eq!(
        lookup("different-key", "profile-frozen")
            .unwrap_err()
            .code(),
        "team_agent_already_reserved"
    );
    assert_eq!(
        lookup("member-queue", "changed-profile")
            .unwrap_err()
            .code(),
        "team_agent_idempotency_conflict"
    );
    let (foreign, foreign_root) = create_manager(&f.db, "lookup-other-manager");
    assert!(f
        .db
        .lookup_agent_host_member(
            &foreign,
            &foreign_root,
            &f.child.task_id,
            &f.child.enrollment_id,
            "member-queue",
            "profile-frozen"
        )
        .is_err());
    assert!(f
        .db
        .lookup_agent_host_member(
            &f.manager,
            &foreign_root,
            &f.child.task_id,
            &f.child.enrollment_id,
            "member-queue",
            "profile-frozen"
        )
        .is_err());
    assert!(f
        .db
        .lookup_agent_host_member(
            &f.manager,
            &f.root,
            &f.child.task_id,
            "wrong-enrollment",
            "member-queue",
            "profile-frozen"
        )
        .is_err());
    let status =
        f.db.read_agent_host_dispatch(&f.manager, AgentHostQueueKind::Member, &b.queue_id)
            .unwrap();
    assert_eq!(status.dispatch_count, 0);
    assert_eq!(status.delivery_state, "reserved");
}
