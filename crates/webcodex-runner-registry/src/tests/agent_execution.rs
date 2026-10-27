use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use webcodex_core::runner_protocol::*;

struct Admission {
    calls: AtomicUsize,
    allowed: bool,
}
impl AgentExecutionStoreAdmission for Admission {
    fn validate_persisted(&self, _: &AgentExecutionBinding) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.allowed {
            Ok(())
        } else {
            Err("test_store_claim_not_committed".into())
        }
    }
}
struct PreparationEvidence {
    original: AgentExecutionBinding,
    allowed: bool,
}
impl AgentExecutionStorePreparationEvidence for PreparationEvidence {
    fn recoverable_preparation(&self) -> Result<AgentExecutionBinding, String> {
        if self.allowed {
            Ok(self.original.clone())
        } else {
            Err("test_store_preparation_not_recoverable".into())
        }
    }
}
fn preparation_evidence(start: &AgentExecutionStart) -> PreparationEvidence {
    let mut original = start.binding.clone();
    original.launch_intent_hash.clear();
    original.resource_manifest_hash.clear();
    PreparationEvidence {
        original,
        allowed: true,
    }
}

#[tokio::test]
async fn agent_execution_reservation_recovery_after_restart_uses_original_store_proven_ids() {
    let previous = registry(true).await;
    let (lost_reservation, original) = reserved_start(&previous, 0).await;
    let job_id = lost_reservation.job_id().to_string();
    let request_id = lost_reservation.start_request_id().to_string();
    drop(lost_reservation);
    drop(previous);
    let registry = registry(true).await;
    let mut evidence = preparation_evidence(&original);
    evidence.allowed = false;
    assert!(registry
        .recover_agent_execution_target_reservation(&access(), &evidence)
        .await
        .is_err());
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    evidence.allowed = true;
    let recovered = registry
        .recover_agent_execution_target_reservation(&access(), &evidence)
        .await
        .unwrap();
    assert_eq!(recovered.job_id(), job_id);
    assert_eq!(recovered.start_request_id(), request_id);
    assert_eq!(recovered.target(), &original.binding.target);
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    assert!(poll(&registry).await.is_none());
    let prepared = registry
        .prepare_agent_execution(&access(), &recovered, original, "controller".into())
        .await
        .unwrap();
    let deny = Admission {
        calls: AtomicUsize::new(0),
        allowed: false,
    };
    assert!(registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &deny)
        .await
        .is_err());
    assert!(poll(&registry).await.is_none());
    let allow = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    assert!(
        registry
            .dispatch_prepared_agent_execution(&access(), &prepared, &allow)
            .await
            .unwrap()
            .dispatched
    );
    let request = poll(&registry).await.unwrap();
    assert_eq!(request.request_id, request_id);
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_recovered_reservation_fences_finalized_identity_retarget_and_replacement()
{
    let registry = registry(true).await;
    let (_, original) = reserved_start(&registry, 0).await;
    let finalized = PreparationEvidence {
        original: original.binding.clone(),
        allowed: true,
    };
    assert!(registry
        .recover_agent_execution_target_reservation(&access(), &finalized)
        .await
        .is_err());
    let evidence = preparation_evidence(&original);
    let recovered = registry
        .recover_agent_execution_target_reservation(&access(), &evidence)
        .await
        .unwrap();
    for field in ["requester", "executor", "slot"] {
        let mut changed = original.clone();
        match field {
            "requester" => changed.binding.requester.run.agent_id = "different-requester".into(),
            "executor" => changed.binding.executor.run.agent_id = "different-worker".into(),
            _ => changed.binding.requester.cli_attempt_id = "different-attempt".into(),
        }
        assert!(
            registry
                .prepare_agent_execution(&access(), &recovered, changed, "controller".into())
                .await
                .is_err(),
            "{field}"
        );
    }
    let mut foreign = access();
    foreign.username = Some("other-owner".into());
    assert!(registry
        .recover_agent_execution_target_reservation(&foreign, &evidence)
        .await
        .is_err());
    registry
        .inner
        .lock()
        .await
        .runners
        .get_mut("runner")
        .unwrap()
        .runner_instance_id = "replacement".into();
    assert!(registry
        .recover_agent_execution_target_reservation(&access(), &evidence)
        .await
        .is_err());
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
}

#[tokio::test]
async fn agent_execution_preparation_recovery_rejects_existing_job_pending_and_closed_scope() {
    for conflict in ["prepared", "pending", "closed"] {
        let registry = registry(true).await;
        let (reservation, original) = reserved_start(&registry, 0).await;
        let evidence = preparation_evidence(&original);
        match conflict {
            "prepared" => {
                registry
                    .prepare_agent_execution(
                        &access(),
                        &reservation,
                        original.clone(),
                        "controller".into(),
                    )
                    .await
                    .unwrap();
                assert!(registry
                    .read_prepared_agent_execution(&access(), &original.binding)
                    .await
                    .is_ok());
            }
            "pending" => {
                let request_id = reservation.start_request_id().to_string();
                let wire = RunnerRequest::from_operation(
                    RunnerInvocationMetadata {
                        request_id: request_id.clone(),
                        client_id: "runner".into(),
                        requested_by: "test".into(),
                        created_at: now_ts(),
                    },
                    RunnerOperation::RunShell(
                        webcodex_core::runner_operation::RunnerShellOperation {
                            cwd: None,
                            command: "synthetic queued request".into(),
                            stdin: None,
                            max_bytes: None,
                            timeout_secs: 30,
                            job_context: None,
                        },
                    ),
                )
                .unwrap();
                let mut inner = registry.inner.lock().await;
                crate::requests::enqueue_pending_request_locked(
                    registry.telemetry.as_ref(),
                    &mut inner,
                    "runner",
                    request_id,
                    wire,
                    None,
                    None,
                )
                .unwrap();
            }
            _ => {
                let mut closed = close(vec![original.binding.clone()]);
                closed.members.clear();
                closed.members_sha256 = compute_members_sha256(&[]).unwrap();
                registry
                    .enqueue_agent_execution_close(
                        &access(),
                        closed,
                        "close-request".into(),
                        "controller".into(),
                    )
                    .await
                    .unwrap();
            }
        }
        assert!(
            registry
                .recover_agent_execution_target_reservation(&access(), &evidence)
                .await
                .is_err(),
            "{conflict}"
        );
    }
}

pub(crate) fn access() -> RunnerAccess {
    RunnerAccess {
        global_visibility: false,
        owner_bypass: false,
        username: Some("owner".into()),
        group: None,
    }
}
pub(crate) fn start(index: u32) -> AgentExecutionStart {
    let process = AgentExecutionProcess {
        executable: format!("sha256:{}", "e".repeat(64)),
        args: vec!["--task".into(), "private synthetic input".into()],
    };
    AgentExecutionStart {
        workspace_source: None,
        binding: AgentExecutionBinding {
            version: 1,
            project: "project".into(),
            requester: AgentExecutionRequester {
                enrollment_id: "requester-enrollment".into(),
                run: AgentRunRef {
                    agent_id: "requester".into(),
                    agent_run_id: "requester-run".into(),
                    generation: 1,
                },
                root_task_id: "root".into(),
                source_task_id: "source".into(),
                call_index: index,
                cli_attempt_id: format!("attempt-{index:02}"),
            },
            executor: AgentExecutionExecutor {
                enrollment_id: format!("worker-enrollment-{index}"),
                run: AgentRunRef {
                    agent_id: format!("worker-{index}"),
                    agent_run_id: format!("worker-run-{index}"),
                    generation: 1,
                },
                task_id: format!("worker-task-{index}"),
                cli_session_scope_id: format!("worker-session-{index}"),
            },
            target: AgentExecutionTarget {
                client_id: "runner".into(),
                runner_instance_id: "instance".into(),
            },
            carrier: AgentExecutionCarrier::Job {
                job_id: format!("job-{index}"),
                start_request_id: format!("request-{index}"),
            },
            launch_intent_hash: compute_launch_intent_hash(&process, None, 30).unwrap(),
            resource_manifest_hash: "a".repeat(64),
        },
        resource_set_id: format!("resources-{index}"),
        process,
        stdin: None,
        timeout_secs: 30,
    }
}
pub(crate) async fn reserved_start(
    registry: &RunnerRegistry,
    index: u32,
) -> (AgentExecutionTargetReservation, AgentExecutionStart) {
    let reservation = registry
        .reserve_agent_execution_target(&access(), "runner")
        .await
        .unwrap();
    let mut value = start(index);
    value.binding.target = reservation.target().clone();
    value.binding.carrier = AgentExecutionCarrier::Job {
        job_id: reservation.job_id().into(),
        start_request_id: reservation.start_request_id().into(),
    };
    (reservation, value)
}
pub(crate) fn resources() -> Vec<AgentExecutionResourceDescriptor> {
    (0..AGENT_EXECUTION_RESOURCE_MAX_ENTRIES)
        .map(|index| AgentExecutionResourceDescriptor {
            project_workspace_copy: false,
            reviewed_controls: None,
            provider: None,
            resource_set_id: format!("resources-{index}"),
            manifest_hash: "a".repeat(64),
            executable_identity: format!("sha256:{}", "e".repeat(64)),
        })
        .collect()
}
pub(crate) fn registration(
    enabled: bool,
    resources: Vec<AgentExecutionResourceDescriptor>,
) -> RunnerRegisterRequest {
    let mut capabilities =
        crate::test_support::current_runner_capabilities(RunnerCapabilities::default());
    // This is a fake transport fixture, not a real executor capability claim.
    let mut encoded = serde_json::to_value(&capabilities).unwrap();
    encoded["agent_isolated_execution_v1"] = serde_json::json!(enabled);
    encoded["structured_process_argv"] = serde_json::json!(true);
    encoded["structured_execution_jobs"] = serde_json::json!(true);
    encoded["job_state_reconciliation"] = serde_json::json!(true);
    capabilities = serde_json::from_value(encoded).unwrap();
    RunnerRegisterRequest {
        agent_execution_resources: resources,
        client_id: "runner".into(),
        runner_instance_id: "instance".into(),
        runner_protocol_generation: RUNNER_PROTOCOL_GENERATION_V2,
        owner: Some("owner".into()),
        display_name: None,
        hostname: None,
        host_context: None,
        capabilities,
        policy: None,
        process_started_at: None,
        build: None,
        job_concurrency_limit: None,
        job_inventory: Some(ShellJobInventory {
            active_complete: true,
            jobs: vec![],
        }),
        coding_agent_providers: None,
        coding_agent_inventory: None,
    }
}
pub(crate) async fn registry(enabled: bool) -> RunnerRegistry {
    let registry = RunnerRegistry::default();
    registry
        .register_with_auth(
            registration(enabled, if enabled { resources() } else { Vec::new() }),
            Some(&access()),
        )
        .await
        .unwrap();
    registry
}
pub(crate) async fn poll(registry: &RunnerRegistry) -> Option<RunnerRequest> {
    registry
        .poll(RunnerPollRequest {
            client_id: "runner".into(),
            runner_instance_id: "instance".into(),
        })
        .await
        .unwrap()
}
pub(crate) fn close(members: Vec<AgentExecutionBinding>) -> AgentExecutionClose {
    AgentExecutionClose {
        version: 1,
        close_ref: AgentExecutionCloseRef {
            close_id: "close-1".into(),
            source: members[0].requester.run.clone(),
        },
        target: members[0].target.clone(),
        deadline_at_ms: now_ms() + 60_000,
        members_sha256: compute_members_sha256(&members).unwrap(),
        members,
    }
}
pub(crate) fn receipt(close: &AgentExecutionClose) -> AgentExecutionCloseReceipt {
    AgentExecutionCloseReceipt {
        version: 1,
        close_ref: close.close_ref.clone(),
        target: close.target.clone(),
        deadline_at_ms: close.deadline_at_ms,
        members_sha256: close.members_sha256.clone(),
        observation_revision: 1,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: close
            .members
            .iter()
            .map(|member| AgentExecutionStopReceipt {
                binding_sha256: member.binding_hash().unwrap(),
                carrier: member.carrier.clone(),
                observation_revision: 1,
                outcome: AgentExecutionStopOutcome::NeverCreated {
                    request_cancelled: true,
                    worker_finished: true,
                },
            })
            .collect(),
    }
}

#[tokio::test]
async fn agent_execution_prepare_never_enqueues_and_store_admission_precedes_exact_once_dispatch() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    assert!(poll(&registry).await.is_none());
    let deny = Admission {
        calls: AtomicUsize::new(0),
        allowed: false,
    };
    assert!(registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &deny)
        .await
        .is_err());
    assert!(poll(&registry).await.is_none());
    let allow = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    let dispatched = registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &allow)
        .await
        .unwrap();
    assert!(dispatched.dispatched && !dispatched.replayed);
    let replay = registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &deny)
        .await
        .unwrap();
    assert!(replay.replayed && !replay.dispatched);
    assert_eq!(allow.calls.load(Ordering::SeqCst), 1);
    assert_eq!(deny.calls.load(Ordering::SeqCst), 1);
    let request = poll(&registry).await.unwrap();
    assert_eq!(request.kind, AGENT_EXECUTION_START_KIND);
    assert_eq!(request.request_id, reservation.start_request_id());
    assert!(poll(&registry).await.is_none());
    let again = registry
        .prepare_agent_execution(&access(), &reservation, original, "controller".into())
        .await
        .unwrap();
    assert!(
        !registry
            .dispatch_prepared_agent_execution(&access(), &again, &deny)
            .await
            .unwrap()
            .dispatched
    );
}

#[tokio::test]
async fn agent_execution_capability_owner_intent_and_registry_epoch_cannot_be_bypassed() {
    let unsupported = registry(false).await;
    assert!(unsupported
        .reserve_agent_execution_target(&access(), "runner")
        .await
        .is_err());
    let registry = registry(true).await;
    let mut foreign = access();
    foreign.username = Some("other-owner".into());
    assert!(registry
        .reserve_agent_execution_target(&foreign, "runner")
        .await
        .is_err());
    let (reservation, original) = reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let mut changed = original.clone();
    changed.resource_set_id = "different-resource-set".into();
    assert!(registry
        .prepare_agent_execution(&access(), &reservation, changed, "controller".into())
        .await
        .is_err());
    let mut changed = original;
    changed.process.args.push("changed".into());
    changed.binding.launch_intent_hash =
        compute_launch_intent_hash(&changed.process, None, changed.timeout_secs).unwrap();
    assert!(registry
        .prepare_agent_execution(&access(), &reservation, changed, "controller".into())
        .await
        .is_err());
    let replacement_registry = RunnerRegistry::default();
    let admission = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    assert!(replacement_registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
        .await
        .is_err());
    assert_eq!(admission.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn agent_execution_replaced_instance_cannot_receive_prepared_or_queued_start() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(&access(), &reservation, original, "controller".into())
        .await
        .unwrap();
    let admission = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    {
        let mut inner = registry.inner.lock().await;
        inner.runners.get_mut("runner").unwrap().runner_instance_id = "replacement".into();
    }
    assert!(registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
        .await
        .is_err());
    assert_eq!(admission.calls.load(Ordering::SeqCst), 0);
    {
        let mut inner = registry.inner.lock().await;
        inner.runners.get_mut("runner").unwrap().runner_instance_id = "instance".into();
    }
    registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
        .await
        .unwrap();
    {
        let mut inner = registry.inner.lock().await;
        inner.runners.get_mut("runner").unwrap().runner_instance_id = "replacement".into();
    }
    assert!(registry
        .poll(RunnerPollRequest {
            client_id: "runner".into(),
            runner_instance_id: "replacement".into()
        })
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn agent_execution_close_fences_queued_and_new_workers_without_replaying_cli() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let admission = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
        .await
        .unwrap();
    let close = close(vec![original.binding]);
    registry
        .enqueue_agent_execution_close(
            &access(),
            close.clone(),
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    let (late_reservation, late) = reserved_start(&registry, 1).await;
    assert!(registry
        .prepare_agent_execution(&access(), &late_reservation, late, "controller".into())
        .await
        .is_err());
    let control = poll(&registry).await.unwrap();
    assert_eq!(control.kind, AGENT_EXECUTION_CLOSE_KIND);
    assert!(poll(&registry).await.is_none());
    let replay = registry
        .enqueue_agent_execution_close(
            &access(),
            close,
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    assert!(replay.enqueued);
    assert_eq!(
        poll(&registry).await.unwrap().kind,
        AGENT_EXECUTION_CLOSE_KIND
    );
    let (unrelated_reservation, mut unrelated) = reserved_start(&registry, 2).await;
    unrelated.binding.requester.run.agent_id = "other-requester".into();
    unrelated.binding.requester.run.agent_run_id = "other-requester-run".into();
    registry
        .prepare_agent_execution(
            &access(),
            &unrelated_reservation,
            unrelated,
            "controller".into(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn agent_execution_close_receipt_is_exact_and_still_requires_store_persistence() {
    let registry = registry(true).await;
    let (reservation, start) = reserved_start(&registry, 0).await;
    registry
        .prepare_agent_execution(&access(), &reservation, start.clone(), "controller".into())
        .await
        .unwrap();
    let close = close(vec![start.binding]);
    let expected = receipt(&close);
    registry
        .enqueue_agent_execution_close(
            &access(),
            close.clone(),
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    assert!(registry
        .accept_agent_execution_close_receipt(
            "runner",
            "instance",
            None,
            "close-request",
            expected.clone()
        )
        .await
        .is_err());
    poll(&registry).await.unwrap();
    let mut wrong = expected.clone();
    wrong.receipts.clear();
    assert!(registry
        .accept_agent_execution_close_receipt("runner", "instance", None, "close-request", wrong)
        .await
        .is_err());
    let mut wrong = expected.clone();
    wrong.tombstone_persisted = false;
    assert!(registry
        .accept_agent_execution_close_receipt("runner", "instance", None, "close-request", wrong)
        .await
        .is_err());
    assert!(registry
        .accept_agent_execution_close_receipt(
            "runner",
            "instance",
            Some("stale-socket"),
            "close-request",
            expected.clone()
        )
        .await
        .is_err());
    let accepted = registry
        .accept_agent_execution_close_receipt(
            "runner",
            "instance",
            None,
            "close-request",
            expected.clone(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.receipt(), &expected);
    let repeated = registry
        .accept_agent_execution_close_receipt(
            "runner",
            "instance",
            None,
            "close-request",
            expected.clone(),
        )
        .await
        .unwrap();
    assert_eq!(repeated.receipt(), &expected);
    let mut conflicting = expected;
    conflicting.observation_revision += 1;
    assert!(registry
        .accept_agent_execution_close_receipt(
            "runner",
            "instance",
            None,
            "close-request",
            conflicting
        )
        .await
        .is_err());
    let status = registry
        .enqueue_agent_execution_close(
            &access(),
            close,
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    assert!(status.awaiting_store_persistence && !status.enqueued);
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_expired_close_never_admits_a_start_or_receipt() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let mut close = close(vec![original.binding]);
    close.deadline_at_ms = 1;
    registry
        .enqueue_agent_execution_close(
            &access(),
            close.clone(),
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    let admission = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    assert!(registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
        .await
        .is_err());
    assert_eq!(admission.calls.load(Ordering::SeqCst), 0);
    let cleanup = poll(&registry).await.unwrap();
    assert_eq!(cleanup.kind, AGENT_EXECUTION_CLOSE_KIND);
    let RunnerOperation::AgentExecutionClose(sent) = cleanup.decode_operation().unwrap() else {
        panic!("wrong cleanup kind")
    };
    assert_eq!(sent.deadline_at_ms, 1);
    assert!(registry
        .accept_agent_execution_close_receipt(
            "runner",
            "instance",
            None,
            "close-request",
            receipt(&close)
        )
        .await
        .is_err());
}

#[tokio::test]
async fn agent_execution_generic_result_cannot_be_a_close_ack() {
    let registry = registry(true).await;
    let close = close(vec![start(0).binding]);
    registry
        .enqueue_agent_execution_close(
            &access(),
            close,
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    poll(&registry).await.unwrap();
    let error = registry
        .complete(RunnerResultRequest {
            client_id: "runner".into(),
            runner_instance_id: "instance".into(),
            request_id: "close-request".into(),
            exit_code: Some(0),
            stdout: None,
            stderr: None,
            duration_ms: Some(1),
            error: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error, "agent_execution_typed_observation_required");
}

#[tokio::test]
async fn agent_execution_inventory_cannot_reconstruct_missing_store_authority() {
    let original = start(0);
    let snapshot = ShellJobSnapshot {
        job_id: "job-0".into(),
        request_id: "request-0".into(),
        status: "running".into(),
        update_seq: 1,
        created_at: 1,
        started_at: Some(1),
        ended_at: None,
        exit_code: None,
        duration_ms: None,
        error: None,
        command_execution_state: None,
        context: isolated_job_context(&original),
        stdout: Default::default(),
        stderr: Default::default(),
        validation_progress: None,
        activity: None,
    };
    let registry = registry(true).await;
    let inner = registry.inner.lock().await;
    let inventory = ShellJobInventory {
        active_complete: true,
        jobs: vec![snapshot],
    };
    assert!(crate::reconciliation::preflight_inventory_locked(
        &inner, "runner", "instance", &inventory
    )
    .is_err());
}

#[tokio::test]
async fn agent_execution_reservation_is_inert_and_cannot_retarget_full_start() {
    let registry = registry(true).await;
    let (first, original) = reserved_start(&registry, 0).await;
    let second = registry
        .reserve_agent_execution_target(&access(), "runner")
        .await
        .unwrap();
    assert_ne!(first.job_id(), second.job_id());
    assert_ne!(first.start_request_id(), second.start_request_id());
    assert!(poll(&registry).await.is_none());
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    assert!(registry
        .prepare_agent_execution(&access(), &second, original.clone(), "controller".into())
        .await
        .is_err());
    let restarted = RunnerRegistry::default();
    assert!(restarted
        .prepare_agent_execution(&access(), &first, original.clone(), "controller".into())
        .await
        .is_err());
    registry
        .prepare_agent_execution(&access(), &first, original, "controller".into())
        .await
        .unwrap();
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_concurrent_dispatch_and_incomplete_close_preserve_original_set() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let admission = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    let access = access();
    let (first, second) = tokio::join!(
        registry.dispatch_prepared_agent_execution(&access, &prepared, &admission),
        registry.dispatch_prepared_agent_execution(&access, &prepared, &admission)
    );
    assert_eq!(
        usize::from(first.unwrap().dispatched) + usize::from(second.unwrap().dispatched),
        1
    );
    assert_eq!(admission.calls.load(Ordering::SeqCst), 1);
    let (other_reservation, other) = reserved_start(&registry, 1).await;
    registry
        .prepare_agent_execution(
            &access,
            &other_reservation,
            other.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let incomplete = close(vec![original.binding.clone()]);
    assert_eq!(
        registry
            .enqueue_agent_execution_close(
                &access,
                incomplete,
                "close-request".into(),
                "controller".into()
            )
            .await
            .unwrap_err(),
        "agent_execution_close_incomplete_local_set"
    );
    let complete = close(vec![original.binding, other.binding]);
    registry
        .enqueue_agent_execution_close(
            &access,
            complete,
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        poll(&registry).await.unwrap().kind,
        AGENT_EXECUTION_CLOSE_KIND
    );
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_observe_is_exact_read_only_and_allowed_after_close() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let close = close(vec![original.binding.clone()]);
    registry
        .enqueue_agent_execution_close(
            &access(),
            close,
            "close-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    let before = {
        let inner = registry.inner.lock().await;
        (
            inner.pending_by_id.len(),
            inner.queues_by_runner["runner"].len(),
            inner.jobs_by_id.len(),
        )
    };
    let observed = registry
        .observe_agent_execution(&access(), &original.binding)
        .await
        .unwrap();
    assert_eq!(observed.job_id, reservation.job_id());
    let after = {
        let inner = registry.inner.lock().await;
        (
            inner.pending_by_id.len(),
            inner.queues_by_runner["runner"].len(),
            inner.jobs_by_id.len(),
        )
    };
    assert_eq!(before, after);
    let mut wrong = original.binding.clone();
    wrong.resource_manifest_hash = "b".repeat(64);
    assert!(registry
        .observe_agent_execution(&access(), &wrong)
        .await
        .is_err());
    let mut wrong = original.binding.clone();
    wrong.target.runner_instance_id = "other-instance".into();
    assert!(registry
        .observe_agent_execution(&access(), &wrong)
        .await
        .is_err());
    let mut foreign = access();
    foreign.username = Some("other-owner".into());
    assert!(registry
        .observe_agent_execution(&foreign, &original.binding)
        .await
        .is_err());
    let restarted = RunnerRegistry::default();
    assert!(restarted
        .observe_agent_execution(&access(), &original.binding)
        .await
        .is_err());
    assert!(restarted.inner.lock().await.jobs_by_id.is_empty());
    assert_eq!(
        poll(&registry).await.unwrap().kind,
        AGENT_EXECUTION_CLOSE_KIND
    );
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_generic_queue_cannot_bypass_store_admission_or_close_checks() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    // A valid prepared Job is still insufficient: only dispatch may consume
    // the trusted Store admission and obtain the private queue permit.
    registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let operations = [
        RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(
            RunnerJobIsolatedProcessOperation {
                context: isolated_job_context(&original),
                start: original.clone(),
            },
        )),
        RunnerOperation::AgentExecutionClose(close(vec![original.binding.clone()])),
    ];
    for (index, operation) in operations.into_iter().enumerate() {
        let request_id = if index == 0 {
            reservation.start_request_id().to_string()
        } else {
            "close-request".into()
        };
        let wire = RunnerRequest::from_operation(
            RunnerInvocationMetadata {
                request_id: request_id.clone(),
                client_id: "runner".into(),
                requested_by: "generic-controller".into(),
                created_at: now_ts(),
            },
            operation,
        )
        .unwrap();
        let mut inner = registry.inner.lock().await;
        let result = crate::requests::enqueue_pending_request_locked(
            registry.telemetry.as_ref(),
            &mut inner,
            "runner",
            request_id,
            wire,
            None,
            None,
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("agent_execution_checked_admission_required"));
        assert!(inner.pending_by_id.is_empty());
        assert!(inner
            .queues_by_runner
            .values()
            .all(|queue| queue.is_empty()));
        assert!(inner.agent_execution_closes.is_empty());
    }
}

#[tokio::test]
async fn agent_execution_read_prepared_recovers_dropped_http_stack_without_new_dispatch_authority()
{
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    let binding = original.binding.clone();
    let prepared = registry
        .prepare_agent_execution(&access(), &reservation, original, "controller".into())
        .await
        .unwrap();
    drop(reservation);
    drop(prepared);
    let restored = registry
        .read_prepared_agent_execution(&access(), &binding)
        .await
        .unwrap();
    assert_eq!(restored.binding(), &binding);
    assert!(poll(&registry).await.is_none());
    let deny = Admission {
        calls: AtomicUsize::new(0),
        allowed: false,
    };
    assert!(registry
        .dispatch_prepared_agent_execution(&access(), &restored, &deny)
        .await
        .is_err());
    assert!(poll(&registry).await.is_none());
    let allow = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    assert!(
        registry
            .dispatch_prepared_agent_execution(&access(), &restored, &allow)
            .await
            .unwrap()
            .dispatched
    );
    drop(restored);
    let replay = registry
        .read_prepared_agent_execution(&access(), &binding)
        .await
        .unwrap();
    assert!(
        !registry
            .dispatch_prepared_agent_execution(&access(), &replay, &deny)
            .await
            .unwrap()
            .dispatched
    );
    assert_eq!(deny.calls.load(Ordering::SeqCst), 1);
    assert_eq!(allow.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        poll(&registry).await.unwrap().kind,
        AGENT_EXECUTION_START_KIND
    );
    assert!(poll(&registry).await.is_none());
    let mut mismatch = binding.clone();
    mismatch.resource_manifest_hash = "b".repeat(64);
    assert!(registry
        .read_prepared_agent_execution(&access(), &mismatch)
        .await
        .is_err());
    let mut foreign = access();
    foreign.username = Some("other-owner".into());
    assert!(registry
        .read_prepared_agent_execution(&foreign, &binding)
        .await
        .is_err());
    let restarted = RunnerRegistry::default();
    assert!(restarted
        .read_prepared_agent_execution(&access(), &binding)
        .await
        .is_err());
    assert!(restarted
        .dispatch_prepared_agent_execution(&access(), &replay, &allow)
        .await
        .is_err());
    assert!(restarted.inner.lock().await.jobs_by_id.is_empty());
}

#[tokio::test]
async fn agent_execution_read_prepared_cannot_reconstruct_reservation_only_or_foreign_epoch() {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    assert!(registry
        .read_prepared_agent_execution(&access(), &original.binding)
        .await
        .is_err());
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    let prepared = registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    {
        let mut inner = registry.inner.lock().await;
        inner
            .jobs_by_id
            .get_mut(reservation.job_id())
            .unwrap()
            .observation
            .epoch = Arc::from("foreign-registry-epoch");
    }
    assert!(registry
        .read_prepared_agent_execution(&access(), prepared.binding())
        .await
        .is_err());
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_resource_registration_is_bounded_unique_strict_and_atomic() {
    let registry = registry(true).await;
    let target = start(0).binding.target;
    let original = registry
        .read_agent_execution_resource(&access(), &target, "resources-0")
        .await
        .unwrap();
    let mut duplicate = resources();
    duplicate[1] = duplicate[0].clone();
    let mut excessive = resources();
    excessive.push(AgentExecutionResourceDescriptor {
        project_workspace_copy: false,
        resource_set_id: "extra".into(),
        ..original.clone()
    });
    let mut invalid_hash = resources();
    invalid_hash[0].manifest_hash = "A".repeat(64);
    let mut invalid_executable = resources();
    invalid_executable[0].executable_identity = "C:\\private\\worker.exe".into();
    for (enabled, values) in [
        (false, resources()),
        (true, duplicate),
        (true, excessive),
        (true, invalid_hash),
        (true, invalid_executable),
    ] {
        assert!(registry
            .register_with_auth(registration(enabled, values), Some(&access()))
            .await
            .is_err());
        assert_eq!(
            registry
                .read_agent_execution_resource(&access(), &target, "resources-0")
                .await
                .unwrap(),
            original
        );
    }
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_resource_lookup_requires_exact_instance_and_access_without_exposing_paths()
{
    let registry = registry(true).await;
    let target = start(0).binding.target;
    let fact = registry
        .read_agent_execution_resource(&access(), &target, "resources-0")
        .await
        .unwrap();
    let fields = serde_json::to_value(&fact).unwrap();
    assert_eq!(fields.as_object().unwrap().len(), 4);
    assert_eq!(fields["project_workspace_copy"], false);
    assert!(fact.executable_identity.starts_with("sha256:"));
    let mut foreign = access();
    foreign.username = Some("other-owner".into());
    assert!(registry
        .read_agent_execution_resource(&foreign, &target, "resources-0")
        .await
        .is_err());
    let mut wrong = target.clone();
    wrong.runner_instance_id = "other-instance".into();
    assert!(registry
        .read_agent_execution_resource(&access(), &wrong, "resources-0")
        .await
        .is_err());
    assert!(registry
        .read_agent_execution_resource(&access(), &target, "not-registered")
        .await
        .is_err());
    registry
        .register_with_auth(registration(false, vec![]), Some(&access()))
        .await
        .unwrap();
    assert!(registry
        .read_agent_execution_resource(&access(), &target, "resources-0")
        .await
        .is_err());
    assert!(registry.inner.lock().await.jobs_by_id.is_empty());
    assert!(poll(&registry).await.is_none());
}

#[tokio::test]
async fn agent_execution_resource_change_before_dispatch_preserves_binding_and_denies_admission() {
    for mutation in ["manifest", "executable", "missing"] {
        let registry = registry(true).await;
        let (reservation, original) = reserved_start(&registry, 0).await;
        let prepared = registry
            .prepare_agent_execution(
                &access(),
                &reservation,
                original.clone(),
                "controller".into(),
            )
            .await
            .unwrap();
        let mut changed = resources();
        match mutation {
            "manifest" => changed[0].manifest_hash = "b".repeat(64),
            "executable" => changed[0].executable_identity = format!("sha256:{}", "f".repeat(64)),
            _ => {
                changed.remove(0);
            }
        }
        registry
            .register_with_auth(registration(true, changed), Some(&access()))
            .await
            .unwrap();
        let admission = Admission {
            calls: AtomicUsize::new(0),
            allowed: true,
        };
        assert!(
            registry
                .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
                .await
                .is_err(),
            "{mutation}"
        );
        assert_eq!(admission.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            registry
                .read_prepared_agent_execution(&access(), &original.binding)
                .await
                .unwrap()
                .binding(),
            &original.binding
        );
        assert!(poll(&registry).await.is_none());
    }
}

#[tokio::test]
async fn agent_execution_changed_resources_fence_queued_start_but_withdrawal_does_not_block_cleanup(
) {
    let registry = registry(true).await;
    let (reservation, original) = reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(
            &access(),
            &reservation,
            original.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    let admission = Admission {
        calls: AtomicUsize::new(0),
        allowed: true,
    };
    registry
        .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
        .await
        .unwrap();
    let mut changed = resources();
    changed[0].manifest_hash = "b".repeat(64);
    registry
        .register_with_auth(registration(true, changed), Some(&access()))
        .await
        .unwrap();
    assert!(poll(&registry).await.is_none());
    registry
        .register_with_auth(registration(false, vec![]), Some(&access()))
        .await
        .unwrap();
    let mut closure = close(vec![original.binding.clone()]);
    closure.deadline_at_ms = 1;
    registry
        .enqueue_agent_execution_close(
            &access(),
            closure.clone(),
            "cleanup-request".into(),
            "controller".into(),
        )
        .await
        .unwrap();
    let cleanup = poll(&registry).await.unwrap();
    assert_eq!(cleanup.kind, AGENT_EXECUTION_CLOSE_KIND);
    let RunnerOperation::AgentExecutionClose(sent) = cleanup.decode_operation().unwrap() else {
        panic!("wrong cleanup kind")
    };
    assert_eq!(sent, closure);
    assert!(registry
        .accept_agent_execution_close_receipt(
            "runner",
            "instance",
            None,
            "cleanup-request",
            receipt(&closure)
        )
        .await
        .is_err());
    assert!(
        !registry
            .dispatch_prepared_agent_execution(&access(), &prepared, &admission)
            .await
            .unwrap()
            .dispatched
    );
    assert!(poll(&registry).await.is_none());
}
