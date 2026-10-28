//! Store-only boundary tests. These exercise real factory, Task, observation,
//! close-ledger and meeting persistence against temporary SQLite databases.
//! Synthetic authenticated receipts stand only at the documented transport
//! boundary; no Runner process, model, GUI or production state is involved.

use crate::agent_cli_delivery::{AgentCliReviewDecision, AuthenticatedAgentCliDeliverySnapshot};
use crate::*;
use sha2::{Digest, Sha256};
use webcodex_core::runner_protocol::{
    compute_launch_intent_hash, AgentExecutionBinding, AgentExecutionCarrier, AgentExecutionClose,
    AgentExecutionCloseReceipt, AgentExecutionProcess, AgentExecutionStart,
    AgentExecutionStopOutcome, AgentExecutionStopReceipt, AgentExecutionTarget, AgentRunRef,
    ShellCommandExecutionState,
};

const OWNER: &str = "delivery-test-owner";
const PROJECT: &str = "delivery-test-project";
const HOST: &str = "delivery-test-host";
const ARTIFACT: &[u8] = b"Public worker deliverable; checked result: 42";

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn reference(run: &VerifiedAgentRun) -> AgentRunRef {
    AgentRunRef {
        agent_id: run.agent_id().into(),
        agent_run_id: run.run_id().into(),
        generation: run.generation(),
    }
}

fn principal(owner: &str) -> CommunicationPrincipal {
    crate::agent_isolation::enrollment_principal_for_execution(owner)
}

fn new_manager(db: &Database, owner: &str, label: &str) -> (VerifiedAgentRun, String) {
    let task = db
        .create_agent_task(
            &principal(owner),
            NewAgentTask {
                title: label.into(),
                instruction: format!("Public assignment {label}"),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(PROJECT.into()),
                idempotency_key: label.into(),
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
                owner_user: owner.into(),
                project: PROJECT.into(),
                principal_key: HOST.into(),
                task_id: task.clone(),
                task_directory: format!("workspace/{label}"),
                brief: format!("Public assignment {label}"),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 2,
                    max_cli_calls: 2,
                },
                expires_at_unix_ms: now() + 60_000,
            },
            None,
            label,
            now(),
        )
        .unwrap();
    let run = db
        .claim_agent_enrollment(
            HOST,
            &format!("{label}-window"),
            &issued.enrollment_id,
            &issued.grant.unwrap().into_secret(),
            now(),
        )
        .unwrap()
        .binding;
    db.start_agent_public_task(&run, &task, "start").unwrap();
    (run, task)
}

fn new_member(
    db: &Database,
    manager: &VerifiedAgentRun,
    label: &str,
) -> (VerifiedAgentRun, String) {
    let created = db
        .create_agent_member(
            manager,
            AgentMemberRequest {
                title: label.into(),
                brief: format!("Public assignment {label}"),
                task_directory: format!("workspace/{label}"),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 1,
                },
            },
            label,
        )
        .unwrap();
    let issued = db
        .issue_agent_enrollment_delivery(manager, &created.enrollment_id, label, now())
        .unwrap();
    let run = db
        .claim_agent_enrollment(
            HOST,
            &format!("{label}-window"),
            &created.enrollment_id,
            &issued.grant.unwrap().into_secret(),
            now(),
        )
        .unwrap()
        .binding;
    db.start_agent_public_task(&run, &created.task_id, "start")
        .unwrap();
    (run, created.task_id)
}

struct SyntheticStop(AgentExecutionCloseReceipt);
impl crate::agent_execution_close::AuthenticatedAgentExecutionCloseReceipt for SyntheticStop {
    fn authenticated_receipt(&self) -> &AgentExecutionCloseReceipt {
        &self.0
    }
}

fn stopped(close: &AgentExecutionClose) -> SyntheticStop {
    SyntheticStop(AgentExecutionCloseReceipt {
        version: 1,
        close_ref: close.close_ref.clone(),
        target: close.target.clone(),
        deadline_at_ms: close.deadline_at_ms,
        members_sha256: close.members_sha256.clone(),
        observation_revision: 2,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: close
            .members
            .iter()
            .map(|binding| AgentExecutionStopReceipt {
                binding_sha256: binding.binding_hash().unwrap(),
                carrier: binding.carrier.clone(),
                observation_revision: 1,
                outcome: AgentExecutionStopOutcome::TreeExited {
                    leader_exit_code: Some(0),
                    leader_handle_signaled: true,
                    membership_complete: true,
                    member_handles_signaled: true,
                    worker_finished: true,
                },
            })
            .collect(),
    })
}

struct SyntheticDelivery {
    binding: AgentExecutionBinding,
    bytes: Vec<u8>,
    digest: String,
}

impl SyntheticDelivery {
    fn new(binding: &AgentExecutionBinding, bytes: &[u8]) -> Self {
        let text = std::str::from_utf8(bytes).unwrap();
        let bundle = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "binding_sha256": binding.binding_hash().unwrap(),
            "summary": "Public explicit worker delivery for independent review",
            "files": [{
                "path": "result.txt", "sha256": format!("{:x}", Sha256::digest(bytes)),
                "bytes": bytes.len(), "text": text,
            }],
        }))
        .unwrap();
        Self {
            binding: binding.clone(),
            digest: format!("{:x}", Sha256::digest(&bundle)),
            bytes: bundle,
        }
    }
}

impl AuthenticatedAgentCliDeliverySnapshot for SyntheticDelivery {
    fn binding(&self) -> &AgentExecutionBinding {
        &self.binding
    }
    fn artifact_bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn artifact_sha256(&self) -> &str {
        &self.digest
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    db: Database,
    manager: VerifiedAgentRun,
    caller: VerifiedAgentRun,
    sibling: VerifiedAgentRun,
    worker: VerifiedAgentRun,
    root: String,
    source: String,
    sibling_task: String,
    binding: AgentExecutionBinding,
    permit: AgentCliObservationPermit,
    started: AgentPublicTaskMutation,
    attempt_revision: i64,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(&temp.path().join("delivery.db")).unwrap();
        let (manager, root) = new_manager(&db, OWNER, "root");
        let (caller, source) = new_member(&db, &manager, "caller");
        let (sibling, sibling_task) = new_member(&db, &manager, "sibling");
        let planned = |task_id: String| TeamAgentPlan {
            task_id,
            cli_calls: vec![TeamCliCall {
                inputs: vec![],
                provider_id: "workbuddy".into(),
                model: "synthetic-model".into(),
                effort: Some("high".into()),
                instruction: Some("Public independent worker assignment".into()),
            }],
        };
        db.register_agent_team_plan(
            &manager,
            &root,
            0,
            TeamPlanInput {
                manager: TeamAgentPlan {
                    task_id: root.clone(),
                    cli_calls: vec![],
                },
                subagents: vec![planned(source.clone()), planned(sibling_task.clone())],
            },
            "plan",
        )
        .unwrap();
        let preparation = db
            .prepare_agent_cli_worker(
                &caller,
                AgentCliPreparationRequest {
                    root_task_id: root.clone(),
                    source_task_id: source.clone(),
                    call_index: 0,
                    idempotency_key: "original-cli".into(),
                    fallback_thinking_profile_id: "original-profile".into(),
                    timeout_secs: Some(120),
                    target: AgentExecutionTarget {
                        client_id: "original-client".into(),
                        runner_instance_id: "original-runner".into(),
                    },
                    carrier: AgentExecutionCarrier::Job {
                        job_id: "original-job".into(),
                        start_request_id: "original-request".into(),
                    },
                },
            )
            .unwrap()
            .preparation
            .unwrap();
        let worker = preparation.worker().clone();
        let mut binding = preparation.binding_identity().clone();
        let started = db
            .start_agent_public_task(&worker, &binding.executor.task_id, "worker-start")
            .unwrap();
        let process = AgentExecutionProcess {
            executable: "C:\\synthetic\\worker.exe".into(),
            args: vec!["synthetic-only".into()],
        };
        binding.launch_intent_hash = compute_launch_intent_hash(&process, None, 120).unwrap();
        binding.resource_manifest_hash = "a".repeat(64);
        let dispatch = db
            .finalize_agent_cli_execution(
                &caller,
                preparation,
                AgentExecutionStart {
                    workspace_source: None,
                    binding: binding.clone(),
                    resource_set_id: "synthetic-resources".into(),
                    process,
                    stdin: None,
                    timeout_secs: 120,
                },
            )
            .unwrap();
        let permit = dispatch.observation_permit();
        let bound = db
            .bind_agent_cli_execution_job(&permit, &binding.target)
            .unwrap();
        Self {
            temp,
            db,
            manager,
            caller,
            sibling,
            worker,
            root,
            source,
            sibling_task,
            binding,
            permit,
            started,
            attempt_revision: bound.observation_revision,
        }
    }

    fn attempt(&self) -> &str {
        &self.binding.requester.cli_attempt_id
    }

    fn reopen(&self) -> Database {
        Database::open(&self.temp.path().join("delivery.db")).unwrap()
    }

    fn observe(&self, state: ShellCommandExecutionState, error: Option<&str>) {
        let AgentExecutionCarrier::Job { job_id, .. } = &self.binding.carrier else {
            panic!("Job fixture required")
        };
        self.db
            .observe_agent_cli_execution(
                &self.permit,
                &self.binding.target,
                self.attempt_revision,
                TeamCliObservation {
                    job_id: Some(job_id.clone()),
                    dispatch_state: TeamCliDispatchState::Terminal,
                    execution_state: Some(state),
                    error_code: error.map(str::to_owned),
                    result_summary: Some("Bounded public observation".into()),
                },
            )
            .unwrap();
    }

    fn prove_stop(&self) {
        // Stop only the exact executor. The caller remains able to review the
        // delivery; a broader caller close is not needed for this fixture.
        let plan = self
            .db
            .begin_agent_execution_close(OWNER, PROJECT, &reference(&self.worker), now() + 60_000)
            .unwrap();
        assert_eq!(plan.requests().len(), 1);
        assert_eq!(plan.requests()[0].members, vec![self.binding.clone()]);
        assert!(
            self.db
                .accept_agent_execution_close_receipt(&stopped(&plan.requests()[0]))
                .unwrap()
                .complete
        );
    }

    fn close_run(&self, run: &VerifiedAgentRun) {
        let run_ref = reference(run);
        let plan = self
            .db
            .begin_agent_execution_close(run.owner_user(), run.project(), &run_ref, now() + 60_000)
            .unwrap();
        for request in plan.requests() {
            self.db
                .accept_agent_execution_close_receipt(&stopped(request))
                .unwrap();
        }
        assert!(
            self.db
                .agent_execution_close_status(run.owner_user(), run.project(), &run_ref)
                .unwrap()
                .complete
        );
    }

    fn completed(&self) -> SyntheticDelivery {
        self.observe(ShellCommandExecutionState::Completed, None);
        self.prove_stop();
        SyntheticDelivery::new(&self.binding, ARTIFACT)
    }

    fn execution(&self) -> AgentPublicTaskExecution {
        self.db
            .read_agent_cli_worker_task(&self.permit)
            .unwrap()
            .unwrap()
            .execution
    }

    fn counts(&self) -> (i64, i64, i64, i64) {
        self.db
            .conn_for_tests()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM wc_agent_meeting_rooms),
                    (SELECT COUNT(*) FROM wc_agent_meeting_artifacts),
                    (SELECT COUNT(*) FROM wc_agent_meeting_entries WHERE kind='review'),
                    (SELECT COUNT(*) FROM wc_cli_fallbacks)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    }

    fn attempt_json(&self) -> String {
        self.db
            .conn_for_tests()
            .query_row(
                "SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1",
                [self.attempt()],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn execution_facts(&self) -> serde_json::Value {
        let record: serde_json::Value = serde_json::from_str(&self.attempt_json()).unwrap();
        serde_json::json!({
            "binding": record["execution_binding"],
            "job_id": record["job_id"],
            "dispatch_state": record["dispatch_state"],
            "execution_state": record["execution_state"],
            "error_code": record["error_code"],
            "result_summary": record["result_summary"],
            "observation_revision": record["observation_revision"],
        })
    }

    fn assert_other_tasks_active(&self) {
        for task in [&self.root, &self.source, &self.sibling_task] {
            assert_eq!(
                self.db
                    .read_agent_task(&principal(OWNER), task)
                    .unwrap()
                    .summary
                    .state,
                AgentTaskState::Active,
            );
        }
    }
}

#[test]
fn cli_delivery_rejects_unstopped_or_forged_proof_without_publication() {
    let f = Fixture::new();
    f.observe(ShellCommandExecutionState::Completed, None);
    let counts = f.counts();
    let facts = f.execution_facts();
    let proof = SyntheticDelivery::new(&f.binding, ARTIFACT);
    assert!(f.db.publish_agent_cli_delivery(&proof).is_err());
    assert_eq!(f.counts(), counts);
    assert_eq!(f.execution(), f.started.execution);

    f.prove_stop();
    let mut bad_digest = SyntheticDelivery::new(&f.binding, ARTIFACT);
    bad_digest.digest = "0".repeat(64);
    assert!(f.db.publish_agent_cli_delivery(&bad_digest).is_err());
    let mut wrong_payload = SyntheticDelivery::new(&f.binding, ARTIFACT);
    let mut payload: serde_json::Value = serde_json::from_slice(&wrong_payload.bytes).unwrap();
    payload["binding_sha256"] = "0".repeat(64).into();
    wrong_payload.bytes = serde_json::to_vec(&payload).unwrap();
    wrong_payload.digest = format!("{:x}", Sha256::digest(&wrong_payload.bytes));
    assert!(f.db.publish_agent_cli_delivery(&wrong_payload).is_err());
    for field in 0..4 {
        let mut forged = SyntheticDelivery::new(&f.binding, ARTIFACT);
        match field {
            0 => forged.binding.project = "other-project".into(),
            1 => forged.binding.target.runner_instance_id = "replacement-runner".into(),
            2 => forged.binding.executor.run.agent_id = f.sibling.agent_id().into(),
            _ => forged.binding.requester.source_task_id = f.sibling_task.clone(),
        }
        // Keep the outer digest and bundle binding self-consistent: rejection
        // must still compare the claimed binding with the persisted attempt.
        let forged = SyntheticDelivery::new(&forged.binding, ARTIFACT);
        assert!(f.db.publish_agent_cli_delivery(&forged).is_err());
        assert_eq!(f.counts(), counts);
        assert_eq!(f.execution(), f.started.execution);
        assert_eq!(f.execution_facts(), facts);
    }
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_publication_is_immutable_and_survives_reopen_after_worker_revoke() {
    let f = Fixture::new();
    let proof = f.completed();
    f.db.revoke_agent_run(OWNER, PROJECT, f.worker.agent_id(), f.worker.run_id())
        .unwrap();
    let first = f.db.publish_agent_cli_delivery(&proof).unwrap();
    assert_eq!(first.source_attempt_id, f.attempt());
    assert_eq!(first.worker_agent_id, f.worker.agent_id());
    assert_eq!(first.worker_run_id, f.worker.run_id());
    assert_eq!(first.worker_generation, f.worker.generation());
    assert_eq!(first.artifact_sha256, proof.digest);
    assert_eq!(f.counts(), (1, 1, 0, 0));
    let reopened = f.reopen();
    assert_eq!(reopened.publish_agent_cli_delivery(&proof).unwrap(), first);
    let view = reopened
        .read_agent_cli_delivery(&f.caller, f.attempt())
        .unwrap();
    assert_eq!(view.delivery, first);
    assert_eq!(view.artifact_content.as_bytes(), proof.bytes);
    assert_eq!(view.worker_task_id, f.binding.executor.task_id);
    assert_eq!(view.worker_revision, f.started.execution.revision);
    assert_eq!(view.worker_state, AgentTaskState::Active);
    assert!(view.review.is_none());
    let changed = SyntheticDelivery::new(&f.binding, br#"{"version":1,"files":[]}"#);
    assert!(reopened.publish_agent_cli_delivery(&changed).is_err());
    assert_eq!(f.counts(), (1, 1, 0, 0));
    let persisted: (String, String, i64, Vec<u8>) =
        f.db.conn_for_tests()
            .query_row(
                "SELECT e.author_agent_id,e.author_run_id,e.author_generation,a.bytes
         FROM wc_agent_meeting_entries e JOIN wc_agent_meeting_artifacts a ON a.entry_id=e.entry_id
         WHERE e.entry_id=?1",
                [&first.entry_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
    assert_eq!(
        persisted,
        (
            f.worker.agent_id().into(),
            f.worker.run_id().into(),
            f.worker.generation(),
            proof.bytes
        )
    );
    assert_eq!(f.execution(), f.started.execution);
}

#[test]
fn cli_delivery_review_requires_current_caller_or_exact_root_manager_never_worker() {
    let f = Fixture::new();
    let self_complete =
        f.db.complete_agent_public_task(
            &f.worker,
            &f.binding.executor.task_id,
            &f.started.execution.execution_id,
            f.started.execution.revision,
            AgentTaskState::Succeeded,
            Some("Worker cannot accept its own result"),
            None,
            "self-complete",
        )
        .unwrap_err();
    assert_eq!(self_complete.code(), "public_task_business_review_required");
    assert_eq!(f.execution(), f.started.execution);
    let proof = f.completed();
    let published = f.db.publish_agent_cli_delivery(&proof).unwrap();
    assert_eq!(
        f.db.read_agent_cli_delivery(&f.caller, f.attempt())
            .unwrap()
            .delivery,
        published
    );
    assert_eq!(
        f.db.read_agent_cli_delivery(&f.manager, f.attempt())
            .unwrap()
            .delivery,
        published
    );
    let (unrelated_manager, _) = new_manager(&f.db, OWNER, "unrelated-root");
    let (foreign, _) = new_manager(&f.db, "foreign-owner", "foreign-root");
    for actor in [&f.worker, &f.sibling, &unrelated_manager, &foreign] {
        assert!(f.db.read_agent_cli_delivery(actor, f.attempt()).is_err());
        assert!(f
            .db
            .review_agent_cli_delivery(
                actor,
                f.attempt(),
                f.started.execution.revision,
                &published.artifact,
                AgentCliReviewDecision::Accept,
                "Cannot review another task",
                "foreign-review",
            )
            .is_err());
        assert_eq!(f.counts(), (1, 1, 0, 0));
        assert_eq!(f.execution(), f.started.execution);
    }
    f.close_run(&f.caller);
    f.db.revoke_agent_run(OWNER, PROJECT, f.caller.agent_id(), f.caller.run_id())
        .unwrap();
    assert!(f
        .db
        .read_agent_cli_delivery(&f.caller, f.attempt())
        .is_err());
    assert!(f
        .db
        .review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Revoked caller",
            "revoked-review",
        )
        .is_err());
    let accepted =
        f.db.review_agent_cli_delivery(
            &f.manager,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Root manager reviewed the published bytes",
            "manager-review",
        )
        .unwrap();
    assert_eq!(accepted.review.reviewer_agent_id, f.manager.agent_id());
    assert_eq!(accepted.worker_state, AgentTaskState::Succeeded);
    assert_eq!(f.counts(), (1, 1, 1, 0));
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_review_version_revision_and_idempotency_are_atomic() {
    let f = Fixture::new();
    let published = f.db.publish_agent_cli_delivery(&f.completed()).unwrap();
    let counts = f.counts();
    let facts = f.execution_facts();
    for target in [
        AgentMeetingArtifactTarget {
            artifact_id: "other-artifact".into(),
            version: published.artifact.version,
        },
        AgentMeetingArtifactTarget {
            artifact_id: published.artifact.artifact_id.clone(),
            version: published.artifact.version + 1,
        },
    ] {
        assert!(f
            .db
            .review_agent_cli_delivery(
                &f.caller,
                f.attempt(),
                f.started.execution.revision,
                &target,
                AgentCliReviewDecision::Accept,
                "Reviewed",
                "accept",
            )
            .is_err());
        assert_eq!(f.counts(), counts);
        assert_eq!(f.execution(), f.started.execution);
    }
    assert!(f
        .db
        .review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision + 1,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Reviewed",
            "accept",
        )
        .is_err());
    assert_eq!(f.counts(), counts);
    assert_eq!(f.execution(), f.started.execution);
    let accepted =
        f.db.review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Reviewed",
            "accept",
        )
        .unwrap();
    assert!(!accepted.replayed);
    assert_eq!(accepted.worker_revision, f.started.execution.revision + 1);
    assert_eq!(accepted.worker_state, AgentTaskState::Succeeded);
    assert_eq!(accepted.review.artifact, published.artifact);
    assert_eq!(accepted.review.artifact_sha256, published.artifact_sha256);
    let execution = f.execution();
    let replay =
        f.db.review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Reviewed",
            "accept",
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.review, accepted.review);
    for (content, key) in [("Changed review", "accept"), ("Reviewed", "other-key")] {
        assert!(f
            .db
            .review_agent_cli_delivery(
                &f.caller,
                f.attempt(),
                f.started.execution.revision,
                &published.artifact,
                AgentCliReviewDecision::Accept,
                content,
                key,
            )
            .is_err());
        assert_eq!(f.execution(), execution);
        assert_eq!(f.counts(), (1, 1, 1, 0));
    }
    assert_eq!(f.execution_facts(), facts);
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_accept_requires_publication_and_completed_error_free_execution() {
    let missing = Fixture::new();
    let _proof = missing.completed();
    assert!(missing
        .db
        .review_agent_cli_delivery(
            &missing.caller,
            missing.attempt(),
            missing.started.execution.revision,
            &AgentMeetingArtifactTarget {
                artifact_id: "unpublished".into(),
                version: 1
            },
            AgentCliReviewDecision::Accept,
            "No artifact exists",
            "accept",
        )
        .is_err());
    assert_eq!(missing.counts(), (0, 0, 0, 0));
    assert_eq!(missing.execution(), missing.started.execution);
    for (state, error) in [
        (ShellCommandExecutionState::TimedOut, None),
        (
            ShellCommandExecutionState::Completed,
            Some("quota_exhausted"),
        ),
    ] {
        let f = Fixture::new();
        f.observe(state, error);
        f.prove_stop();
        let published =
            f.db.publish_agent_cli_delivery(&SyntheticDelivery::new(&f.binding, ARTIFACT))
                .unwrap();
        assert!(f
            .db
            .review_agent_cli_delivery(
                &f.caller,
                f.attempt(),
                f.started.execution.revision,
                &published.artifact,
                AgentCliReviewDecision::Accept,
                "Cannot promote execution failure to success",
                "accept",
            )
            .is_err());
        assert_eq!(f.counts(), (1, 1, 0, 0));
        assert_eq!(f.execution(), f.started.execution);
        f.assert_other_tasks_active();
    }
}

#[test]
fn cli_delivery_reject_changes_only_worker_business_result_not_attempt_or_fallback() {
    use webcodex_core::cli_fallback::{CliCancellationOrigin, CliFailureReason};
    let f = Fixture::new();
    f.observe(
        ShellCommandExecutionState::Completed,
        Some("quota_exhausted"),
    );
    f.prove_stop();
    let job: webcodex_core::runner_protocol::ShellJobInfo = serde_json::from_value(serde_json::json!({
        "job_id":"original-job","request_id":"original-request","client_id":"original-client",
        "project_id":PROJECT,"kind":"shell","shell":"direct_argv","command_preview":"synthetic CLI",
        "status":"failed","created_at":100,"started_at":101,"ended_at":102,"exit_code":1,
        "command_execution_state":"completed","last_update_seq":2,
        "structured_execution":{"execution_source":"run_process","arg_count":1,"stdin_present":false}
    })).unwrap();
    let fallback =
        f.db.reserve_agent_cli_fallback(
            &f.permit,
            &f.binding.target,
            &job,
            CliFailureReason::QuotaExhausted,
            CliCancellationOrigin::None,
        )
        .unwrap();
    let fallback_before =
        f.db.read_cli_fallback(&principal(OWNER), &fallback.fallback_id)
            .unwrap();
    let published =
        f.db.publish_agent_cli_delivery(&SyntheticDelivery::new(&f.binding, ARTIFACT))
            .unwrap();
    let facts = f.execution_facts();
    let rejected =
        f.db.review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Reject,
            "Published result does not satisfy the assigned work",
            "reject",
        )
        .unwrap();
    assert_eq!(rejected.worker_state, AgentTaskState::Failed);
    assert_eq!(rejected.worker_revision, f.started.execution.revision + 1);
    assert_eq!(rejected.review.decision, AgentCliReviewDecision::Reject);
    assert_eq!(f.execution_facts(), facts);
    let fallback_after =
        f.db.read_cli_fallback(&principal(OWNER), &fallback.fallback_id)
            .unwrap();
    assert_eq!(
        serde_json::to_value(fallback_before).unwrap(),
        serde_json::to_value(fallback_after).unwrap()
    );
    assert_eq!(f.counts(), (1, 1, 1, 1));
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_same_agent_handoff_preserves_review_scope_and_denies_old_run() {
    let f = Fixture::new();
    let published = f.db.publish_agent_cli_delivery(&f.completed()).unwrap();
    f.close_run(&f.caller);
    let pending = f.db.begin_agent_run_handoff(&f.caller).unwrap();
    let current =
        f.db.activate_agent_run(OWNER, PROJECT, f.caller.agent_id(), &pending.run_id)
            .unwrap();
    assert_eq!(current.agent_id(), f.caller.agent_id());
    assert_ne!(current.run_id(), f.caller.run_id());
    let reopened = f.reopen();
    assert!(reopened
        .read_agent_cli_delivery(&f.caller, f.attempt())
        .is_err());
    assert!(reopened
        .review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Old run",
            "review",
        )
        .is_err());
    assert_eq!(f.counts(), (1, 1, 0, 0));
    assert_eq!(
        reopened
            .read_agent_cli_delivery(&current, f.attempt())
            .unwrap()
            .delivery,
        published
    );
    let accepted = reopened
        .review_agent_cli_delivery(
            &current,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Current caller reviewed the original delivery",
            "review",
        )
        .unwrap();
    assert_eq!(accepted.review.reviewer_agent_id, current.agent_id());
    assert_eq!(accepted.review.reviewer_run_id, current.run_id());
    assert_eq!(accepted.review.reviewer_generation, current.generation());
    assert!(reopened
        .review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Current caller reviewed the original delivery",
            "review",
        )
        .is_err());
    assert_eq!(f.counts(), (1, 1, 1, 0));
    assert_eq!(f.execution().state, AgentTaskState::Succeeded);
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_concurrent_connections_commit_one_review_and_worker_terminal() {
    let f = Fixture::new();
    let published = f.db.publish_agent_cli_delivery(&f.completed()).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let db = f.reopen();
        let caller = f.caller.clone();
        let attempt = f.attempt().to_owned();
        let target = published.artifact.clone();
        let expected = f.started.execution.revision;
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            db.review_agent_cli_delivery(
                &caller,
                &attempt,
                expected,
                &target,
                AgentCliReviewDecision::Accept,
                "Both connections reviewed identical published bytes",
                "same-review",
            )
            .unwrap()
        }));
    }
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| !r.replayed).count(), 1);
    assert_eq!(results[0].review, results[1].review);
    assert_eq!(results[0].worker_revision, f.started.execution.revision + 1);
    assert_eq!(results[1].worker_revision, results[0].worker_revision);
    assert_eq!(f.counts(), (1, 1, 1, 0));
    assert_eq!(f.execution().state, AgentTaskState::Succeeded);
    assert_eq!(f.execution().revision, f.started.execution.revision + 1);
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_reviews_published_output_after_failure_without_rewriting_execution() {
    for decision in [
        AgentCliReviewDecision::Accept,
        AgentCliReviewDecision::Reject,
    ] {
        let f = Fixture::new();
        f.observe(
            ShellCommandExecutionState::TimedOut,
            Some("provider_failed"),
        );
        f.prove_stop();
        let failed =
            f.db.complete_agent_cli_worker_task(
                &f.permit,
                f.started.execution.revision,
                AgentTaskState::Failed,
                None,
                Some("Original timeout"),
                "observer-timeout",
            )
            .unwrap();
        let published =
            f.db.publish_agent_cli_delivery(&SyntheticDelivery::new(&f.binding, ARTIFACT))
                .unwrap();
        let original_execution = f.execution();
        let original_attempt = f.attempt_json();
        let counts = f.counts();
        assert!(f
            .db
            .review_agent_cli_delivery(
                &f.caller,
                f.attempt(),
                failed.execution.revision - 1,
                &published.artifact,
                decision,
                "Independently inspected candidate",
                "review"
            )
            .is_err());
        assert_eq!(f.counts(), counts);
        let reviewed =
            f.db.review_agent_cli_delivery(
                &f.caller,
                f.attempt(),
                failed.execution.revision,
                &published.artifact,
                decision,
                "Independently inspected candidate",
                "review",
            )
            .unwrap();
        assert_eq!(reviewed.worker_state, AgentTaskState::Failed);
        assert_eq!(reviewed.worker_revision, failed.execution.revision);
        assert!(!reviewed.replayed);
        let reopened = f.reopen();
        let replay = reopened
            .review_agent_cli_delivery(
                &f.caller,
                f.attempt(),
                failed.execution.revision,
                &published.artifact,
                decision,
                "Independently inspected candidate",
                "review",
            )
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(f.execution(), original_execution);
        assert_eq!(f.attempt_json(), original_attempt);
        assert_eq!(f.counts(), (counts.0, counts.1, counts.2 + 1, counts.3));
        assert!(reopened
            .review_agent_cli_delivery(
                &f.caller,
                f.attempt(),
                failed.execution.revision,
                &published.artifact,
                decision,
                "Changed decision grounds",
                "review"
            )
            .is_err());
        // Original failure completion still replays with its original key.
        assert!(
            f.db.complete_agent_cli_worker_task(
                &f.permit,
                f.started.execution.revision,
                AgentTaskState::Failed,
                None,
                Some("Original timeout"),
                "observer-timeout"
            )
            .unwrap()
            .replayed
        );
        f.assert_other_tasks_active();
    }
}

#[test]
fn cli_delivery_pending_review_blocks_caller_and_root_completion_then_caller_can_finish() {
    let f = Fixture::new();
    let published = f.db.publish_agent_cli_delivery(&f.completed()).unwrap();
    let caller_execution =
        f.db.read_agent_public_task_execution(&f.caller, &f.source)
            .unwrap()
            .unwrap();
    let manager_execution =
        f.db.read_agent_public_task_execution(&f.manager, &f.root)
            .unwrap()
            .unwrap();
    let facts = f.execution_facts();
    for (actor, task, before) in [
        (&f.caller, &f.source, &caller_execution),
        (&f.manager, &f.root, &manager_execution),
    ] {
        for outcome in [AgentTaskState::Succeeded, AgentTaskState::Failed] {
            let error =
                f.db.complete_agent_public_task(
                    actor,
                    task,
                    &before.execution_id,
                    before.revision,
                    outcome,
                    Some("Upper task must wait for independent worker review"),
                    None,
                    "upper-complete",
                )
                .unwrap_err();
            assert_eq!(error.code(), "public_task_worker_delivery_pending");
            assert_eq!(
                f.db.read_agent_public_task_execution(actor, task)
                    .unwrap()
                    .unwrap(),
                *before,
            );
            assert_eq!(f.execution(), f.started.execution);
            assert_eq!(f.execution_facts(), facts);
            assert_eq!(f.counts(), (1, 1, 0, 0));
            f.assert_other_tasks_active();
        }
    }

    let accepted =
        f.db.review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Accept,
            "Caller independently accepted the published delivery",
            "accept-worker",
        )
        .unwrap();
    assert_eq!(accepted.worker_state, AgentTaskState::Succeeded);
    assert_eq!(accepted.worker_revision, f.started.execution.revision + 1);
    f.assert_other_tasks_active();
    assert_eq!(
        f.db.read_agent_public_task_execution(&f.caller, &f.source)
            .unwrap()
            .unwrap(),
        caller_execution,
    );
    let completed =
        f.db.complete_agent_public_task(
            &f.caller,
            &f.source,
            &caller_execution.execution_id,
            caller_execution.revision,
            AgentTaskState::Succeeded,
            Some("Caller completed its own assignment after accepting delivery"),
            None,
            "upper-complete",
        )
        .unwrap();
    assert!(completed.state_changed);
    assert!(!completed.replayed);
    assert_eq!(completed.task.task_id, f.source);
    assert_eq!(
        completed.execution.execution_id,
        caller_execution.execution_id
    );
    assert_eq!(completed.execution.state, AgentTaskState::Succeeded);
    assert_eq!(completed.execution.revision, caller_execution.revision + 1);
    assert_eq!(f.execution().revision, accepted.worker_revision);
    assert_eq!(f.execution_facts(), facts);
    assert_eq!(f.counts(), (1, 1, 1, 0));
    assert_eq!(
        f.db.read_agent_public_task_execution(&f.manager, &f.root)
            .unwrap()
            .unwrap(),
        manager_execution,
    );
    assert_eq!(
        f.db.read_agent_task(&principal(OWNER), &f.sibling_task)
            .unwrap()
            .summary
            .state,
        AgentTaskState::Active,
    );
}

// Append to agent_cli_delivery_tests.rs. Reuses that module's real B3 Fixture,
// SyntheticStop, stopped(), reference(), OWNER and PROJECT; no fixture copy.

fn delivery_recovery_db_snapshot(db: &Database) -> std::collections::BTreeMap<String, Vec<String>> {
    let conn = db.conn_for_tests();
    let schema: Vec<String> = conn
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
        .unwrap()
        .query_map([], |row| {
            Ok(format!(
                "{:?}",
                (
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                )
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut snapshot = std::collections::BTreeMap::new();
    snapshot.insert("<schema>".into(), schema);
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let mut statement = conn
            .prepare(&format!("SELECT * FROM \"{quoted}\""))
            .unwrap();
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                let cells = (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(format!("{cells:?}"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        snapshot.insert(table, rows);
    }
    snapshot
}

fn delivery_recovery_assert_read_only<T>(db: &Database, read: impl FnOnce() -> T) -> T {
    let before = delivery_recovery_db_snapshot(db);
    let changes = || {
        db.conn_for_tests()
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    let before_changes = changes();
    let result = read();
    assert_eq!(delivery_recovery_db_snapshot(db), before);
    assert_eq!(
        changes(),
        before_changes,
        "read path must not even attempt writes"
    );
    result
}

fn delivery_recovery_worker_close(f: &Fixture) -> AgentExecutionClose {
    let plan =
        f.db.begin_agent_execution_close(OWNER, PROJECT, &reference(&f.worker), now() + 60_000)
            .unwrap();
    assert_eq!(plan.requests().len(), 1);
    assert_eq!(plan.requests()[0].members, vec![f.binding.clone()]);
    plan.requests()[0].clone()
}

#[test]
fn cli_delivery_recovery_reopen_returns_exact_original_tuple_without_first_publication() {
    let f = Fixture::new();
    f.observe(ShellCommandExecutionState::Completed, None);
    let close = delivery_recovery_worker_close(&f);
    let receipt = stopped(&close);
    assert!(
        f.db.accept_agent_execution_close_receipt(&receipt)
            .unwrap()
            .complete
    );
    let before_reopen = delivery_recovery_db_snapshot(&f.db);
    let reopened = f.reopen();
    assert_eq!(delivery_recovery_db_snapshot(&reopened), before_reopen);
    for actor in [&f.caller, &f.manager] {
        let evidence = delivery_recovery_assert_read_only(&reopened, || {
            reopened.read_agent_cli_delivery_stop_evidence(actor, &f.binding)
        })
        .unwrap();
        assert_eq!(evidence.binding(), &f.binding);
        assert_eq!(evidence.close(), &close);
        assert_eq!(evidence.receipt(), &receipt.0);
        evidence.receipt().validate_for(evidence.close()).unwrap();
    }
    assert_eq!(f.counts(), (0, 0, 0, 0));
    assert_eq!(f.execution(), f.started.execution);
    assert_eq!(delivery_recovery_db_snapshot(&f.db), before_reopen);
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_recovery_rejects_wrong_binding_other_roles_and_revoked_reviewer() {
    let f = Fixture::new();
    let _unused_bundle = f.completed();
    let (foreign, _) = new_manager(&f.db, "recovery-foreign-owner", "recovery-foreign");
    let (unrelated_manager, _) = new_manager(&f.db, OWNER, "recovery-unrelated");
    for actor in [&f.worker, &f.sibling, &foreign, &unrelated_manager] {
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.read_agent_cli_delivery_stop_evidence(actor, &f.binding)
        })
        .is_err());
    }
    for field in 0..6 {
        let mut wrong = f.binding.clone();
        match field {
            0 => wrong.project = "another-project".into(),
            1 => wrong.target.runner_instance_id = "replacement-runner".into(),
            2 => wrong.requester.source_task_id = f.sibling_task.clone(),
            3 => wrong.executor.run.agent_id = f.sibling.agent_id().into(),
            4 => wrong.executor.run.generation += 1,
            _ => wrong.launch_intent_hash = "b".repeat(64),
        }
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &wrong)
        })
        .is_err());
    }
    f.close_run(&f.caller);
    f.db.revoke_agent_run(OWNER, PROJECT, f.caller.agent_id(), f.caller.run_id())
        .unwrap();
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    assert_eq!(f.counts(), (0, 0, 0, 0));
    assert_eq!(f.execution(), f.started.execution);
}

#[test]
fn cli_delivery_recovery_requires_accepted_tree_exit_not_plan_or_never_created() {
    let f = Fixture::new();
    f.observe(ShellCommandExecutionState::Completed, None);
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    let close = delivery_recovery_worker_close(&f);
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    assert!(
        !f.db
            .agent_execution_close_status(OWNER, PROJECT, &reference(&f.worker))
            .unwrap()
            .complete
    );
    let mut never_created = stopped(&close);
    for member in &mut never_created.0.receipts {
        member.outcome = AgentExecutionStopOutcome::NeverCreated {
            request_cancelled: true,
            worker_finished: true,
        };
    }
    // NeverCreated is valid stop evidence in the close protocol, but it cannot
    // authenticate a deliverable recovered from a process that ran to exit.
    assert!(
        f.db.accept_agent_execution_close_receipt(&never_created)
            .unwrap()
            .complete
    );
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    assert_eq!(f.counts(), (0, 0, 0, 0));
    assert_eq!(f.execution(), f.started.execution);
}

#[test]
fn cli_delivery_recovery_rejects_missing_members_and_incomplete_persisted_close() {
    let f = Fixture::new();
    f.observe(ShellCommandExecutionState::Completed, None);
    let close = delivery_recovery_worker_close(&f);
    for incomplete in 0..2 {
        let mut receipt = stopped(&close);
        if incomplete == 0 {
            receipt.0.receipts.clear();
        } else {
            receipt.0.receipts[0].outcome = AgentExecutionStopOutcome::TreeExited {
                leader_exit_code: Some(0),
                leader_handle_signaled: true,
                membership_complete: false,
                member_handles_signaled: true,
                worker_finished: true,
            };
        }
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.accept_agent_execution_close_receipt(&receipt)
        })
        .is_err());
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
        })
        .is_err());
    }
    let accepted = stopped(&close);
    assert!(
        f.db.accept_agent_execution_close_receipt(&accepted)
            .unwrap()
            .complete
    );
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_ok());

    // Deliberate corruption of this disposable fixture only. No trigger is
    // removed and no production validation/acceptance path is weakened.
    let mut missing_member = accepted.0.clone();
    missing_member.receipts.clear();
    f.db.conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET execution_close_receipts_json=?2 WHERE run_id=?1",
            rusqlite::params![
                f.worker.run_id(),
                serde_json::to_string(&vec![missing_member]).unwrap()
            ],
        )
        .unwrap();
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    assert!(f.db.conn_for_tests().execute(
        "UPDATE wc_agent_runs SET execution_close_receipts_json=?2,execution_closed_at_unix_ms=NULL WHERE run_id=?1",
        rusqlite::params![f.worker.run_id(), serde_json::to_string(&vec![accepted.0]).unwrap()],
    ).is_err());
    // The monotonic tombstone must not be removed even by this synthetic SQL.
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    // A separate unaccepted plan can contain syntactically valid injected JSON
    // without the atomic accepted/closed marker. It must still be rejected.
    let pending = Fixture::new();
    pending.observe(ShellCommandExecutionState::Completed, None);
    let pending_close = delivery_recovery_worker_close(&pending);
    pending
        .db
        .conn_for_tests()
        .execute(
            "UPDATE wc_agent_runs SET execution_close_receipts_json=?2 WHERE run_id=?1",
            rusqlite::params![
                pending.worker.run_id(),
                serde_json::to_string(&vec![stopped(&pending_close).0]).unwrap()
            ],
        )
        .unwrap();
    assert!(delivery_recovery_assert_read_only(&pending.db, || {
        pending
            .db
            .read_agent_cli_delivery_stop_evidence(&pending.caller, &pending.binding)
    })
    .is_err());
    assert_eq!(pending.counts(), (0, 0, 0, 0));
    assert_eq!(f.counts(), (0, 0, 0, 0));
    assert_eq!(f.execution(), f.started.execution);
}

#[test]
fn cli_delivery_recovery_rechecks_attempt_terminal_and_active_assignment() {
    let f = Fixture::new();
    f.prove_stop();
    // The physical receipt does not manufacture the original observation.
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    f.observe(ShellCommandExecutionState::Completed, None);
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_ok());
    for actor in [&f.caller, &f.manager] {
        // Synthetic state injection isolates the assignment check from run
        // revocation; public revoke would change both and hide this boundary.
        assert_eq!(f.db.conn_for_tests().execute(
            "UPDATE wc_agent_enrollment_assignments SET state='released' WHERE agent_id=?1 AND state='active'",
            [actor.agent_id()],
        ).unwrap(), 1);
        f.db.validate_agent_run(actor).unwrap();
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.read_agent_cli_delivery_stop_evidence(actor, &f.binding)
        })
        .is_err());
    }
    assert_eq!(f.counts(), (0, 0, 0, 0));
    assert_eq!(f.execution(), f.started.execution);
}

#[test]
fn cli_delivery_recovery_same_agent_handoff_uses_current_reviewer_without_rebinding_proof() {
    let f = Fixture::new();
    let _unused_bundle = f.completed();
    let original = delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .unwrap();
    let original_close = original.close().clone();
    let original_receipt = original.receipt().clone();
    f.close_run(&f.caller);
    let pending = f.db.begin_agent_run_handoff(&f.caller).unwrap();
    let current =
        f.db.activate_agent_run(OWNER, PROJECT, f.caller.agent_id(), &pending.run_id)
            .unwrap();
    assert_eq!(current.agent_id(), f.caller.agent_id());
    assert_ne!(current.run_id(), f.caller.run_id());
    let reopened = f.reopen();
    assert!(delivery_recovery_assert_read_only(&reopened, || {
        reopened.read_agent_cli_delivery_stop_evidence(&f.caller, &f.binding)
    })
    .is_err());
    let recovered = delivery_recovery_assert_read_only(&reopened, || {
        reopened.read_agent_cli_delivery_stop_evidence(&current, &f.binding)
    })
    .unwrap();
    assert_eq!(recovered.binding(), &f.binding);
    assert_eq!(recovered.close(), &original_close);
    assert_eq!(recovered.receipt(), &original_receipt);
    assert_eq!(
        recovered.binding().requester.run.agent_run_id,
        f.caller.run_id()
    );
    assert_ne!(
        recovered.binding().requester.run.agent_run_id,
        current.run_id()
    );
    assert_eq!(f.counts(), (0, 0, 0, 0));
    assert_eq!(f.execution(), f.started.execution);
    f.assert_other_tasks_active();
}

// Append to agent_cli_delivery_tests.rs; reuse the real B3 Fixture and B4
// delivery_recovery_assert_read_only / delivery_recovery_db_snapshot helpers.

fn unpublished_delivery_state(db: &Database, attempt: &str) -> (Option<String>, Option<String>) {
    db.conn_for_tests()
        .query_row(
            "SELECT delivery_json,review_json FROM wc_team_cli_attempts WHERE attempt_id=?1",
            [attempt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn cli_delivery_unpublished_reject_checks_authority_stop_reason_cas_and_logical_replay() {
    let f = Fixture::new();
    let reason = "No usable delivery was published for the assigned work";
    let reject = || {
        f.db.reject_agent_cli_delivery_unpublished(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            reason,
            "reject-unpublished",
        )
    };
    assert!(delivery_recovery_assert_read_only(&f.db, reject).is_err());
    f.observe(ShellCommandExecutionState::Completed, None);
    assert!(delivery_recovery_assert_read_only(&f.db, reject).is_err());
    f.prove_stop();
    for invalid_reason in [
        String::new(),
        " \t\n".into(),
        "x".repeat(4097),
        "Bearer synthetic-only".into(),
    ] {
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.reject_agent_cli_delivery_unpublished(
                &f.caller,
                f.attempt(),
                f.started.execution.revision,
                &invalid_reason,
                "reject-unpublished",
            )
        })
        .is_err());
    }
    for revision in [0, f.started.execution.revision + 1] {
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.reject_agent_cli_delivery_unpublished(
                &f.caller,
                f.attempt(),
                revision,
                reason,
                "reject-unpublished",
            )
        })
        .is_err());
    }
    let (unrelated, _) = new_manager(&f.db, OWNER, "unpublished-unrelated");
    let (foreign, _) = new_manager(&f.db, "unpublished-foreign-owner", "unpublished-foreign");
    for actor in [&f.worker, &f.sibling, &unrelated, &foreign] {
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.reject_agent_cli_delivery_unpublished(
                actor,
                f.attempt(),
                f.started.execution.revision,
                reason,
                "reject-unpublished",
            )
        })
        .is_err());
    }
    assert_eq!(unpublished_delivery_state(&f.db, f.attempt()), (None, None));
    assert_eq!(f.execution(), f.started.execution);
    let rejected = reject().unwrap();
    assert_eq!(rejected.source_attempt_id, f.attempt());
    assert_eq!(rejected.worker_task_id, f.binding.executor.task_id);
    assert_eq!(rejected.worker_state, AgentTaskState::Failed);
    assert_eq!(rejected.worker_revision, f.started.execution.revision + 1);
    assert!(!rejected.replayed);
    let stored = unpublished_delivery_state(&f.db, f.attempt());
    assert!(stored.0.is_none());
    assert!(stored.1.is_some());
    let marker: serde_json::Value = serde_json::from_str(stored.1.as_ref().unwrap()).unwrap();
    assert_eq!(marker["version"], 1);
    assert_eq!(marker["kind"], "unpublished_rejection");
    assert_eq!(marker["binding_sha256"], f.binding.binding_hash().unwrap());
    assert_eq!(marker["reviewer_agent_id"], f.caller.agent_id());
    assert_eq!(marker["reviewer_run_id"], f.caller.run_id());
    assert_eq!(marker["reviewer_generation"], f.caller.generation());
    assert_eq!(
        marker["expected_worker_revision"],
        f.started.execution.revision
    );
    assert_eq!(
        marker["reason_sha256"],
        format!("{:x}", Sha256::digest(reason.as_bytes()))
    );
    assert!(marker.get("artifact").is_none());
    assert!(marker.get("reason").is_none());
    let reopened = f.reopen();
    let replay = delivery_recovery_assert_read_only(&reopened, || {
        reopened.reject_agent_cli_delivery_unpublished(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            reason,
            "reject-unpublished",
        )
    })
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.worker_revision, rejected.worker_revision);
    assert_eq!(unpublished_delivery_state(&reopened, f.attempt()), stored);
    for (changed_reason, key, revision) in [
        (
            "Different assessment",
            "reject-unpublished",
            f.started.execution.revision,
        ),
        (reason, "different-key", f.started.execution.revision),
        (reason, "reject-unpublished", rejected.worker_revision),
    ] {
        assert!(delivery_recovery_assert_read_only(&f.db, || {
            f.db.reject_agent_cli_delivery_unpublished(
                &f.caller,
                f.attempt(),
                revision,
                changed_reason,
                key,
            )
        })
        .is_err());
    }
    // The root Manager is normally an authorized reviewer, but cannot replay
    // another logical reviewer's already committed completion intent.
    let error = delivery_recovery_assert_read_only(&f.db, || {
        f.db.reject_agent_cli_delivery_unpublished(
            &f.manager,
            f.attempt(),
            f.started.execution.revision,
            reason,
            "reject-unpublished",
        )
    })
    .unwrap_err();
    assert_eq!(error.code(), "agent_cli_delivery_idempotency_conflict");
    f.close_run(&f.caller);
    let pending = f.db.begin_agent_run_handoff(&f.caller).unwrap();
    let current =
        f.db.activate_agent_run(OWNER, PROJECT, f.caller.agent_id(), &pending.run_id)
            .unwrap();
    assert!(delivery_recovery_assert_read_only(&f.db, reject).is_err());
    let replay = delivery_recovery_assert_read_only(&f.db, || {
        f.db.reject_agent_cli_delivery_unpublished(
            &current,
            f.attempt(),
            f.started.execution.revision,
            reason,
            "reject-unpublished",
        )
    })
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.worker_state, AgentTaskState::Failed);
    assert_eq!(replay.worker_revision, rejected.worker_revision);
    assert_eq!(unpublished_delivery_state(&f.db, f.attempt()), stored);
    assert_eq!(f.counts(), (0, 0, 0, 0));
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_unpublished_reject_cannot_replace_review_of_a_published_empty_files_bundle() {
    let f = Fixture::new();
    let mut proof = f.completed();
    let mut bundle: serde_json::Value = serde_json::from_slice(&proof.bytes).unwrap();
    bundle["files"] = serde_json::json!([]);
    bundle["summary"] = "Public result is entirely expressed in this summary".into();
    proof.bytes = serde_json::to_vec(&bundle).unwrap();
    proof.digest = format!("{:x}", Sha256::digest(&proof.bytes));
    let published = f.db.publish_agent_cli_delivery(&proof).unwrap();
    let error = delivery_recovery_assert_read_only(&f.db, || {
        f.db.reject_agent_cli_delivery_unpublished(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            "This is published and requires an artifact review",
            "reject-unpublished",
        )
    })
    .unwrap_err();
    assert_eq!(error.code(), "agent_cli_delivery_artifact_review_required");
    let view =
        f.db.read_agent_cli_delivery(&f.caller, f.attempt())
            .unwrap();
    assert_eq!(view.artifact_content.as_bytes(), proof.bytes);
    assert_eq!(view.delivery, published);
    assert_eq!(view.worker_state, AgentTaskState::Active);
    assert!(view.review.is_none());
    let reviewed =
        f.db.review_agent_cli_delivery(
            &f.caller,
            f.attempt(),
            f.started.execution.revision,
            &published.artifact,
            AgentCliReviewDecision::Reject,
            "Reviewed the published summary and rejected its result",
            "artifact-review",
        )
        .unwrap();
    assert_eq!(reviewed.worker_state, AgentTaskState::Failed);
    assert_eq!(reviewed.review.artifact, published.artifact);
    assert_eq!(f.counts(), (1, 1, 1, 0));
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_unpublished_reject_fences_late_publish_and_allows_explicit_parent_completion() {
    let f = Fixture::new();
    let proof = f.completed();
    let record = f.attempt_json();
    let before = delivery_recovery_db_snapshot(&f.db);
    let caller_execution =
        f.db.read_agent_public_task_execution(&f.caller, &f.source)
            .unwrap()
            .unwrap();
    let manager_execution =
        f.db.read_agent_public_task_execution(&f.manager, &f.root)
            .unwrap()
            .unwrap();
    let rejected =
        f.db.reject_agent_cli_delivery_unpublished(
            &f.manager,
            f.attempt(),
            f.started.execution.revision,
            "The worker stopped without an acceptable published deliverable",
            "manager-reject",
        )
        .unwrap();
    assert_eq!(rejected.worker_state, AgentTaskState::Failed);
    assert_eq!(rejected.worker_revision, f.started.execution.revision + 1);
    assert_eq!(f.attempt_json(), record);
    assert_eq!(f.counts(), (0, 0, 0, 0));
    let after = delivery_recovery_db_snapshot(&f.db);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (table, rows) in &before {
        if ![
            "wc_team_cli_attempts",
            "wc_agent_tasks",
            "wc_agent_public_executions",
            "wc_agent_isolation_audit",
        ]
        .contains(&table.as_str())
        {
            assert_eq!(
                after.get(table),
                Some(rows),
                "unrelated state changed: {table}"
            );
        }
    }
    // In particular no Artifact/Review/room, queue, budget or new identity was
    // manufactured by rejecting an unpublished deliverable.
    let error =
        delivery_recovery_assert_read_only(&f.db, || f.db.publish_agent_cli_delivery(&proof))
            .unwrap_err();
    assert_eq!(error.code(), "agent_cli_delivery_unpublished_rejected");
    assert!(delivery_recovery_assert_read_only(&f.db, || {
        f.db.read_agent_cli_delivery(&f.caller, f.attempt())
    })
    .is_err());
    assert_eq!(f.attempt_json(), record);
    f.assert_other_tasks_active();
    assert_eq!(
        f.db.read_agent_public_task_execution(&f.caller, &f.source)
            .unwrap()
            .unwrap(),
        caller_execution
    );
    let completed =
        f.db.complete_agent_public_task(
            &f.caller,
            &f.source,
            &caller_execution.execution_id,
            caller_execution.revision,
            AgentTaskState::Failed,
            None,
            Some("Caller explicitly closed its assignment after worker rejection"),
            "caller-complete",
        )
        .unwrap();
    assert_eq!(completed.execution.state, AgentTaskState::Failed);
    assert_eq!(completed.execution.revision, caller_execution.revision + 1);
    assert_eq!(f.execution().revision, rejected.worker_revision);
    assert_eq!(
        f.db.read_agent_public_task_execution(&f.manager, &f.root)
            .unwrap()
            .unwrap(),
        manager_execution
    );
    assert_eq!(f.attempt_json(), record);
    assert_eq!(f.counts(), (0, 0, 0, 0));
}

#[test]
fn cli_delivery_unpublished_reject_and_publish_race_has_one_committed_winner() {
    let f = Fixture::new();
    let proof = f.completed();
    let record = f.attempt_json();
    let publishing_db = f.reopen();
    let rejecting_db = f.reopen();
    let barrier = std::sync::Barrier::new(3);
    let (publication, rejection) = std::thread::scope(|scope| {
        let publishing = scope.spawn(|| {
            barrier.wait();
            publishing_db.publish_agent_cli_delivery(&proof)
        });
        let rejecting = scope.spawn(|| {
            barrier.wait();
            rejecting_db.reject_agent_cli_delivery_unpublished(
                &f.caller,
                f.attempt(),
                f.started.execution.revision,
                "No delivery existed when this decision was admitted",
                "race-reject",
            )
        });
        barrier.wait();
        (publishing.join().unwrap(), rejecting.join().unwrap())
    });
    assert_eq!(
        usize::from(publication.is_ok()) + usize::from(rejection.is_ok()),
        1
    );
    let stored = unpublished_delivery_state(&f.db, f.attempt());
    if let Ok(published) = publication {
        assert!(rejection.is_err());
        assert!(stored.0.is_some());
        assert!(stored.1.is_none());
        assert_eq!(f.execution(), f.started.execution);
        assert_eq!(f.counts(), (1, 1, 0, 0));
        assert_eq!(
            f.db.read_agent_cli_delivery(&f.caller, f.attempt())
                .unwrap()
                .delivery,
            published
        );
    } else {
        let rejected = rejection.unwrap();
        assert!(stored.0.is_none());
        assert!(stored.1.is_some());
        assert_eq!(rejected.worker_state, AgentTaskState::Failed);
        assert_eq!(f.execution().state, AgentTaskState::Failed);
        assert_eq!(f.execution().revision, f.started.execution.revision + 1);
        assert_eq!(f.counts(), (0, 0, 0, 0));
    }
    assert_eq!(f.attempt_json(), record);
    f.assert_other_tasks_active();
}

#[test]
fn cli_delivery_observer_failed_without_unpublished_marker_can_publish_phase_result() {
    use webcodex_core::cli_fallback::{CliCancellationOrigin, CliFailureReason};
    let f = Fixture::new();
    f.observe(
        ShellCommandExecutionState::Completed,
        Some("quota_exhausted"),
    );
    f.prove_stop();
    let job: webcodex_core::runner_protocol::ShellJobInfo = serde_json::from_value(serde_json::json!({
        "job_id":"original-job","request_id":"original-request","client_id":"original-client",
        "project_id":PROJECT,"kind":"shell","shell":"direct_argv","command_preview":"synthetic CLI",
        "status":"failed","created_at":100,"started_at":101,"ended_at":102,"exit_code":1,
        "command_execution_state":"completed","last_update_seq":2,
        "structured_execution":{"execution_source":"run_process","arg_count":1,"stdin_present":false}
    })).unwrap();
    let fallback =
        f.db.reserve_agent_cli_fallback(
            &f.permit,
            &f.binding.target,
            &job,
            CliFailureReason::QuotaExhausted,
            CliCancellationOrigin::None,
        )
        .unwrap();
    let fallback_before =
        f.db.read_cli_fallback(&principal(OWNER), &fallback.fallback_id)
            .unwrap();
    let record = f.attempt_json();
    let failed =
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Failed,
            Some("Partial public work remains available for review"),
            Some("Execution stopped after quota exhaustion"),
            "observer-failed",
        )
        .unwrap();
    assert_eq!(failed.execution.state, AgentTaskState::Failed);
    assert_eq!(unpublished_delivery_state(&f.db, f.attempt()), (None, None));
    let proof = SyntheticDelivery::new(
        &f.binding,
        b"Public partial output from an unsuccessful execution",
    );
    let published = f.db.publish_agent_cli_delivery(&proof).unwrap();
    let view =
        f.db.read_agent_cli_delivery(&f.caller, f.attempt())
            .unwrap();
    assert_eq!(view.delivery, published);
    assert_eq!(view.artifact_content.as_bytes(), proof.bytes);
    assert_eq!(view.worker_state, AgentTaskState::Failed);
    assert_eq!(view.worker_revision, failed.execution.revision);
    assert!(view.review.is_none());
    let persisted = unpublished_delivery_state(&f.db, f.attempt());
    assert!(persisted.0.is_some());
    assert!(persisted.1.is_none());
    let fallback_after =
        f.db.read_cli_fallback(&principal(OWNER), &fallback.fallback_id)
            .unwrap();
    assert_eq!(
        serde_json::to_value(fallback_before).unwrap(),
        serde_json::to_value(fallback_after).unwrap()
    );
    assert_eq!(f.attempt_json(), record);
    assert_eq!(f.counts(), (1, 1, 0, 1));
    f.assert_other_tasks_active();
}

#[test]
fn cli_observer_success_requires_stopped_completed_execution_without_publication() {
    let f = Fixture::new();
    f.observe(ShellCommandExecutionState::Completed, None);
    let complete = || {
        f.db.complete_agent_cli_worker_task(
            &f.permit,
            f.started.execution.revision,
            AgentTaskState::Succeeded,
            Some("CLI execution completed; parent Goal acceptance remains independent"),
            None,
            "observer-success",
        )
    };
    assert_eq!(
        complete().unwrap_err().code(),
        "public_task_execution_stop_proof_required"
    );
    f.prove_stop();
    let completed = complete().unwrap();
    assert_eq!(completed.execution.state, AgentTaskState::Succeeded);
    assert_eq!(f.counts(), (0, 0, 0, 0));
    assert!(complete().unwrap().replayed);
    f.assert_other_tasks_active();
}

#[test]
fn cli_observer_success_rejects_failed_or_uncompleted_execution() {
    for (state, code) in [
        (
            ShellCommandExecutionState::Completed,
            Some("provider_failed"),
        ),
        (ShellCommandExecutionState::TimedOut, None),
    ] {
        let f = Fixture::new();
        f.observe(state, code);
        f.prove_stop();
        let error =
            f.db.complete_agent_cli_worker_task(
                &f.permit,
                f.started.execution.revision,
                AgentTaskState::Succeeded,
                None,
                None,
                "unproven-success",
            )
            .unwrap_err();
        assert_eq!(error.code(), "public_task_success_not_proven");
        assert_eq!(f.execution().state, AgentTaskState::Active);
        assert_eq!(f.counts(), (0, 0, 0, 0));
        f.assert_other_tasks_active();
    }
}
