//! Closure tests use only local metadata and an in-memory transport. They never
//! prepare resources, enqueue a runnable worker, or manufacture physical proof.
use super::*;
use crate::webcodex_runner::jobs::agent_resources::{
    validate_profiles, AgentExecutionResourceProfile, AgentExecutionResourcesConfig,
};
use crate::webcodex_runner::jobs::{test_job_snapshot, RunningJob};
use crate::webcodex_runner::{RunnerPolicy, ShellConfig, SshConfig};
use serde_json::json;
use std::sync::atomic::AtomicBool;
use webcodex_core::runner_operation::{
    RunnerInvocationMetadata, RunnerJobIsolatedProcessOperation,
};
use webcodex_core::runner_protocol::{
    compute_launch_intent_hash, compute_members_sha256, isolated_job_context,
    AgentExecutionCloseRef, AgentExecutionProcess, AgentRunRef, RunnerEnvelope,
};

struct Fixture {
    manager: JobManager,
    configuration: AgentExecutionConfiguration,
    sink: RunnerSink,
    _receiver: tokio::sync::mpsc::Receiver<RunnerEnvelope>,
    _temporary: tempfile::TempDir,
}

fn configured_manager(configuration: &AgentExecutionConfiguration) -> JobManager {
    let manager = JobManager::new(1);
    // Exercise lifecycle/metadata independently of platform readiness. The
    // fixture never calls configure/prepare/spawn or creates a native child.
    lock_unpoison(&manager.agent_execution).configuration = Some(configuration.clone());
    manager
}

fn fixture() -> Fixture {
    fixture_with_provider(None)
}

fn fixture_with_provider(provider: Option<webcodex_core::agent_cli::AgentCliProvider>) -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let profiles = validate_profiles(&AgentExecutionResourcesConfig {
        profiles: vec![AgentExecutionResourceProfile {
            id: "resource-profile".into(),
            provider,
            reviewed_controls: None,
            user_https_proxy: false,
            codebuddy_config_directory: None,
            executable: temporary.path().join("never-launched.exe"),
            executable_sha256: "a".repeat(64),
            files: Vec::new(),
        }],
    })
    .unwrap();
    let store = DetachedJobStore::new(temporary.path().join("metadata"));
    store.initialize_agent_execution_metadata().unwrap();
    let configuration = AgentExecutionConfiguration {
        profiles,
        store,
        resource_root: temporary.path().join("unused-resources"),
        _root_pins: Arc::new(Vec::new()),
    };
    let manager = configured_manager(&configuration);
    let (tx, receiver) = tokio::sync::mpsc::channel(8);
    Fixture {
        manager,
        configuration,
        sink: RunnerSink::WebSocket {
            tx,
            client_id: "runner".into(),
            runner_instance_id: "instance".into(),
        },
        _receiver: receiver,
        _temporary: temporary,
    }
}

fn start(fixture: &Fixture, index: usize) -> AgentExecutionStart {
    let process = AgentExecutionProcess {
        executable: fixture.configuration.profiles.descriptors()[0]
            .executable_identity
            .clone(),
        args: Vec::new(),
    };
    let binding = serde_json::from_value(json!({
        "version": 1,
        "project": "project",
        "requester": {
            "enrollment_id": "requester-enrollment",
            "run": {"agent_id": "requester", "agent_run_id": "requester-run", "generation": 1},
            "root_task_id": "root-task",
            "source_task_id": "source-task",
            "call_index": 0,
            "cli_attempt_id": format!("attempt-{index:03}")
        },
        "executor": {
            "enrollment_id": "worker-enrollment",
            "run": {"agent_id": "worker", "agent_run_id": "worker-run", "generation": 1},
            "task_id": "worker-task",
            "cli_session_scope_id": "worker-session"
        },
        "target": {"client_id": "runner", "runner_instance_id": "instance"},
        "carrier": {"kind": "job", "job_id": format!("job-{index}"), "start_request_id": format!("request-{index}")},
        "launch_intent_hash": compute_launch_intent_hash(&process, None, 30).unwrap(),
        "resource_manifest_hash": fixture.configuration.profiles.manifest("resource-profile").unwrap()
    }))
    .unwrap();
    let start = AgentExecutionStart {
        binding,
        resource_set_id: "resource-profile".into(),
        process,
        stdin: None,
        timeout_secs: 30,
    };
    start.validate().unwrap();
    start
}

fn unrelated(mut start: AgentExecutionStart) -> AgentExecutionStart {
    start.binding.requester.run.agent_run_id = "other-requester-run".into();
    start.binding.executor.run.agent_run_id = "other-worker-run".into();
    start.validate().unwrap();
    start
}

fn close(
    source: AgentRunRef,
    members: Vec<AgentExecutionBinding>,
    reference: &AgentExecutionBinding,
) -> AgentExecutionClose {
    let close = AgentExecutionClose {
        version: 1,
        close_ref: AgentExecutionCloseRef {
            close_id: "close-original".into(),
            source,
        },
        target: reference.target.clone(),
        deadline_at_ms: chrono::Utc::now().timestamp_millis() + 60_000,
        members_sha256: compute_members_sha256(&members).unwrap(),
        members,
    };
    close.validate().unwrap();
    close
}

fn admit(fixture: &Fixture, start: &AgentExecutionStart) -> Result<(), String> {
    let _lifecycle = lock_unpoison(&fixture.manager.lifecycle);
    fixture
        .manager
        .admit_agent_execution_locked(start, &fixture.sink)
}

// Synthetic metadata exercises cleanup decisions only; these tests never
// invoke native helpers or treat their fixtures as observed process evidence.
fn cleanup_fixture(stopped: bool) -> (Fixture, AgentExecutionStart) {
    let fixture = fixture();
    let member = start(&fixture, 0);
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs.push(AgentExecutionJobMetadata {
                binding: member.binding.clone(),
                resource_set_id: member.resource_set_id.clone(),
                native_create_attempted: !stopped,
                worker_finished: stopped,
                stop_receipt: stopped.then(|| AgentExecutionStopReceipt {
                    binding_sha256: member.binding.binding_hash().unwrap(),
                    carrier: member.binding.carrier.clone(),
                    observation_revision: 1,
                    outcome: AgentExecutionStopOutcome::NeverCreated {
                        request_cancelled: true,
                        worker_finished: true,
                    },
                }),
                resource_cleanup: Some(AgentExecutionResourceCleanupMetadata {
                    auth_copy_expected: true,
                    proxy_requested: true,
                    grant_intent: true,
                    ..Default::default()
                }),
            });
            Ok(())
        })
        .unwrap();
    (fixture, member)
}

#[test]
fn admission_persists_cleanup_intent_before_any_preparation() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    admit(&fixture, &member).unwrap();
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(metadata.jobs[0].resource_cleanup, Some(Default::default()));
    assert!(metadata.jobs[0]
        .resource_cleanup
        .as_ref()
        .unwrap()
        .complete());
    assert!(
        !metadata.jobs[0]
            .resource_cleanup
            .as_ref()
            .unwrap()
            .resources_stopped
    );
    require_resource_cleanup(&metadata, &member.binding).unwrap();
    cleanup_agent_resources_with(
        &fixture.configuration.store,
        &member.binding,
        Instant::now(),
        true,
        |_, _| panic!("no proxy requirement must not revoke"),
        |_| panic!("no auth requirement must not remove auth"),
    )
    .unwrap();
    assert_eq!(
        fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap(),
        metadata
    );
    assert_eq!(
        delivery_close_receipt(&metadata, &member.binding).unwrap_err(),
        "agent_cli_delivery_stop_unproven"
    );
    assert!(!fixture.configuration.resource_root.exists());
}

#[test]
fn restart_revokes_owned_intent_without_turning_unknown_stop_into_auth_cleanup() {
    let (fixture, member) = cleanup_fixture(false);
    let revocations = std::cell::Cell::new(0);
    for restarting in [false, true] {
        cleanup_agent_resources_with(
            &fixture.configuration.store,
            &member.binding,
            Instant::now() + RESOURCE_CLEANUP_BUDGET,
            restarting,
            |binding, _| {
                assert_eq!(binding, &member.binding);
                revocations.set(revocations.get() + 1);
                Ok(())
            },
            |_| panic!("unknown stop must retain auth"),
        )
        .unwrap();
    }
    assert_eq!(revocations.get(), 1);
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    let cleanup = metadata.jobs[0].resource_cleanup.as_ref().unwrap();
    assert!(cleanup.proxy_revoked);
    assert!(!cleanup.resources_stopped);
    assert!(!cleanup.auth_removed);
    assert!(metadata.jobs[0].stop_receipt.is_none());
    assert_eq!(
        require_resource_cleanup(&metadata, &member.binding).unwrap_err(),
        "agent_execution_resource_cleanup_pending"
    );
}

#[test]
fn native_create_failure_revokes_grant_after_attempt_mark_without_claiming_stop() {
    let (fixture, member) = cleanup_fixture(false);
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs[0]
                .resource_cleanup
                .as_mut()
                .unwrap()
                .grant_verified = true;
            Ok(())
        })
        .unwrap();
    let calls = std::cell::Cell::new(0);
    let deadline = Instant::now() + RESOURCE_CLEANUP_BUDGET;
    revoke_terminal_proxy_with(
        &fixture.configuration.store,
        &member.binding,
        deadline,
        |binding, supplied| {
            assert_eq!(binding, &member.binding);
            assert_eq!(supplied, deadline);
            calls.set(calls.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    let job = &metadata.jobs[0];
    let cleanup = job.resource_cleanup.as_ref().unwrap();
    assert!(job.native_create_attempted && cleanup.grant_verified && cleanup.proxy_revoked);
    assert!(!job.worker_finished && job.stop_receipt.is_none());
    assert!(!cleanup.resources_stopped && !cleanup.auth_removed && !cleanup.complete());
    assert!(delivery_close_receipt(&metadata, &member.binding).is_err());
    assert!(!fixture.configuration.resource_root.exists());
    revoke_terminal_proxy_with(
        &fixture.configuration.store,
        &member.binding,
        deadline,
        |_, _| panic!("already revoked terminal intent must not repeat the effect"),
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
}

#[test]
fn observation_failure_revokes_only_owned_sid_and_preserves_stop_auth_and_failure_debt() {
    let (fixture, member) = cleanup_fixture(false);
    let other = start(&fixture, 1);
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs[0]
                .resource_cleanup
                .as_mut()
                .unwrap()
                .grant_verified = true;
            let mut unrelated = metadata.jobs[0].clone();
            unrelated.binding = other.binding.clone();
            metadata.jobs.push(unrelated);
            Ok(())
        })
        .unwrap();
    let before = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    revoke_terminal_proxy_with(
        &fixture.configuration.store,
        &member.binding,
        Instant::now() + RESOURCE_CLEANUP_BUDGET,
        |binding, _| {
            assert_eq!(binding, &member.binding);
            Ok(())
        },
    )
    .unwrap();
    let after = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(after.jobs[1], before.jobs[1]);
    let cleanup = after.jobs[0].resource_cleanup.as_ref().unwrap();
    assert!(cleanup.proxy_revoked);
    assert!(!cleanup.resources_stopped && !cleanup.auth_removed);
    assert!(after.jobs[0].stop_receipt.is_none());
    assert!(require_resource_cleanup(&after, &member.binding).is_err());
    assert!(delivery_close_receipt(&after, &member.binding).is_err());

    // The same terminal path must retain ownership/debt when the bounded
    // revoke attempt fails; it cannot turn an unknown stop into cleanup proof.
    assert_eq!(
        revoke_terminal_proxy_with(
            &fixture.configuration.store,
            &other.binding,
            Instant::now() + RESOURCE_CLEANUP_BUDGET,
            |_, _| Err("synthetic failure".into())
        )
        .unwrap_err(),
        "agent_execution_proxy_revoke_failed"
    );
    let failed = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    let debt = failed.jobs[1].resource_cleanup.as_ref().unwrap();
    assert!(debt.grant_intent && debt.grant_verified && !debt.proxy_revoked);
    assert!(!debt.resources_stopped && !debt.auth_removed);
    assert_eq!(
        debt.last_cleanup_error.as_deref(),
        Some("agent_execution_proxy_revoke_failed")
    );
    assert!(!fixture.configuration.resource_root.exists());
}

#[test]
fn cleanup_failure_keeps_durable_debt_and_retries_only_unfinished_effects() {
    let (fixture, member) = cleanup_fixture(true);
    let revocations = std::cell::Cell::new(0);
    let remove_attempts = std::cell::Cell::new(0);
    let run = || {
        cleanup_agent_resources_with(
            &fixture.configuration.store,
            &member.binding,
            Instant::now() + RESOURCE_CLEANUP_BUDGET,
            false,
            |_, _| {
                revocations.set(revocations.get() + 1);
                Ok(())
            },
            |_| {
                remove_attempts.set(remove_attempts.get() + 1);
                if remove_attempts.get() == 1 {
                    Err("private detail".into())
                } else {
                    Ok(())
                }
            },
        )
    };
    assert_eq!(run().unwrap_err(), "agent_execution_auth_cleanup_failed");
    let interrupted = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    let cleanup = interrupted.jobs[0].resource_cleanup.as_ref().unwrap();
    assert!(cleanup.resources_stopped && cleanup.proxy_revoked && !cleanup.auth_removed);
    assert_eq!(
        cleanup.last_cleanup_error.as_deref(),
        Some("agent_execution_auth_cleanup_failed")
    );
    assert!(!cleanup.complete());
    run().unwrap();
    assert_eq!(revocations.get(), 1);
    assert_eq!(remove_attempts.get(), 2);
    let complete = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert!(complete.jobs[0]
        .resource_cleanup
        .as_ref()
        .unwrap()
        .last_cleanup_error
        .is_none());
    require_resource_cleanup(&complete, &member.binding).unwrap();
}

#[test]
fn revoke_failure_or_expired_budget_never_claims_success() {
    let (fixture, member) = cleanup_fixture(false);
    let mut expected = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(
        cleanup_agent_resources_with(
            &fixture.configuration.store,
            &member.binding,
            Instant::now() + RESOURCE_CLEANUP_BUDGET,
            true,
            |_, _| Err("private proxy detail".into()),
            |_| panic!("unknown stop must retain auth"),
        )
        .unwrap_err(),
        "agent_execution_proxy_revoke_failed"
    );
    expected.jobs[0]
        .resource_cleanup
        .as_mut()
        .unwrap()
        .last_cleanup_error = Some("agent_execution_proxy_revoke_failed".into());
    assert_eq!(
        fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap(),
        expected
    );
    assert_eq!(
        cleanup_agent_resources_with(
            &fixture.configuration.store,
            &member.binding,
            Instant::now(),
            true,
            |_, _| panic!("expired budget must not mutate native state"),
            |_| panic!("expired budget must not remove auth"),
        )
        .unwrap_err(),
        "agent_execution_resource_cleanup_deadline_expired"
    );
    expected.jobs[0]
        .resource_cleanup
        .as_mut()
        .unwrap()
        .last_cleanup_error = Some("agent_execution_resource_cleanup_deadline_expired".into());
    assert_eq!(
        fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap(),
        expected
    );
}

#[test]
fn legacy_records_never_claim_proxy_or_auth_during_recovery() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs.push(AgentExecutionJobMetadata {
                binding: member.binding.clone(),
                resource_set_id: member.resource_set_id.clone(),
                native_create_attempted: true,
                worker_finished: false,
                stop_receipt: None,
                resource_cleanup: None,
            });
            Ok(())
        })
        .unwrap();
    let before = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    cleanup_agent_resources_with(
        &fixture.configuration.store,
        &member.binding,
        Instant::now(),
        true,
        |_, _| panic!("legacy record owns no loopback exception"),
        |_| panic!("legacy record owns no auth cleanup intent"),
    )
    .unwrap();
    assert_eq!(
        fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap(),
        before
    );
}

#[test]
fn complete_close_replay_cannot_bypass_pending_resource_cleanup() {
    let (fixture, member) = cleanup_fixture(false);
    // A synthetic complete close cannot repair absent per-job stop evidence.
    // With no resources_stopped, this route performs no injected/native effects.
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    let receipt = AgentExecutionCloseReceipt {
        version: 1,
        close_ref: closure.close_ref.clone(),
        target: closure.target.clone(),
        deadline_at_ms: closure.deadline_at_ms,
        members_sha256: closure.members_sha256.clone(),
        observation_revision: 1,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: vec![AgentExecutionStopReceipt {
            binding_sha256: member.binding.binding_hash().unwrap(),
            carrier: member.binding.carrier.clone(),
            observation_revision: 1,
            outcome: AgentExecutionStopOutcome::NeverCreated {
                request_cancelled: true,
                worker_finished: true,
            },
        }],
    };
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.closes.push(AgentExecutionCloseMetadata {
                close: closure.clone(),
                receipt: Some(receipt),
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &closure)
            .unwrap_err(),
        "agent_execution_close_stop_unproven"
    );
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(
        delivery_close_receipt(&metadata, &member.binding).unwrap_err(),
        "agent_cli_delivery_stop_unproven"
    );
}

#[test]
fn physical_resource_cleanup_before_worker_join_is_not_formal_close_proof() {
    let (fixture, member) = cleanup_fixture(false);
    update_resource_cleanup(&fixture.configuration.store, &member.binding, |cleanup| {
        cleanup.resources_stopped = true;
    })
    .unwrap();
    cleanup_agent_resources_with(
        &fixture.configuration.store,
        &member.binding,
        Instant::now() + RESOURCE_CLEANUP_BUDGET,
        false,
        |_, _| Ok(()),
        |_| Ok(()),
    )
    .unwrap();
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert!(metadata.jobs[0]
        .resource_cleanup
        .as_ref()
        .unwrap()
        .complete());
    assert!(!metadata.jobs[0].worker_finished);
    assert!(metadata.jobs[0].stop_receipt.is_none());
    assert_eq!(
        delivery_close_receipt(&metadata, &member.binding).unwrap_err(),
        "agent_cli_delivery_stop_unproven"
    );
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &closure)
            .unwrap_err(),
        "agent_execution_restarted_member_unknown"
    );
}

#[test]
fn delivery_requires_cleanup_even_with_exact_complete_stop_and_close_receipts() {
    let (fixture, member) = cleanup_fixture(false);
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    let stop = AgentExecutionStopReceipt {
        binding_sha256: member.binding.binding_hash().unwrap(),
        carrier: member.binding.carrier.clone(),
        observation_revision: 1,
        outcome: AgentExecutionStopOutcome::TreeExited {
            leader_exit_code: Some(0),
            leader_handle_signaled: true,
            membership_complete: true,
            member_handles_signaled: true,
            worker_finished: true,
        },
    };
    let receipt = AgentExecutionCloseReceipt {
        version: 1,
        close_ref: closure.close_ref.clone(),
        target: closure.target.clone(),
        deadline_at_ms: closure.deadline_at_ms,
        members_sha256: closure.members_sha256.clone(),
        observation_revision: 1,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: vec![stop.clone()],
    };
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs[0].worker_finished = true;
            metadata.jobs[0].stop_receipt = Some(stop);
            metadata.closes.push(AgentExecutionCloseMetadata {
                close: closure.clone(),
                receipt: Some(receipt.clone()),
            });
            Ok(())
        })
        .unwrap();
    let before = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(
        delivery_close_receipt(&before, &member.binding).unwrap_err(),
        "agent_execution_resource_cleanup_pending"
    );
    cleanup_agent_resources_with(
        &fixture.configuration.store,
        &member.binding,
        Instant::now() + RESOURCE_CLEANUP_BUDGET,
        false,
        |_, _| Ok(()),
        |_| Ok(()),
    )
    .unwrap();
    let complete = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(
        delivery_close_receipt(&complete, &member.binding).unwrap(),
        receipt
    );
}

#[test]
fn agent_execution_review_missing_controls_rejects_before_admission_effects() {
    use webcodex_core::agent_cli::AgentCliProvider;

    for provider in [AgentCliProvider::WorkBuddy, AgentCliProvider::Antigravity] {
        let fixture = fixture_with_provider(Some(provider));
        let member = start(&fixture, 0);
        let before = fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap();
        assert_eq!(
            admit(&fixture, &member).unwrap_err(),
            "agent_resources_control_review_required"
        );
        assert_eq!(
            fixture
                .configuration
                .store
                .read_agent_execution_metadata()
                .unwrap(),
            before,
            "review rejection must precede durable admission"
        );
        assert!(lock_unpoison(&fixture.manager.jobs).is_empty());
        assert!(lock_unpoison(&fixture.manager.queued).is_empty());
        assert!(lock_unpoison(&fixture.manager.agent_execution)
            .live
            .is_empty());
    }
}

#[test]
fn agent_execution_review_unbound_synthetic_resource_preserves_admission() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    admit(&fixture, &member).unwrap();
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(metadata.jobs.len(), 1);
    assert_eq!(metadata.jobs[0].binding, member.binding);
    assert!(!metadata.jobs[0].native_create_attempted);
    assert!(!metadata.jobs[0].worker_finished);
    assert!(lock_unpoison(&fixture.manager.jobs).is_empty());
    assert!(lock_unpoison(&fixture.manager.agent_execution)
        .live
        .is_empty());
}

#[test]
fn delivery_rejects_missing_unknown_completed_projection_and_never_created() {
    use webcodex_core::agent_cli::AgentCliDeliveryQuery;
    let fixture = fixture();
    let member = start(&fixture, 0);
    let query = AgentCliDeliveryQuery {
        version: 1,
        target: member.binding.target.clone(),
        binding: member.binding.clone(),
        deadline_at_ms: chrono::Utc::now().timestamp_millis() + 40_000,
    };
    assert_eq!(
        fixture
            .manager
            .agent_cli_delivery_snapshot(&fixture.sink, &query)
            .unwrap_err(),
        "agent_cli_delivery_job_not_recorded"
    );
    admit(&fixture, &member).unwrap();
    insert_queued(&fixture, &member);
    lock_unpoison(&fixture.manager.jobs)
        .get_mut(job_id(&member.binding).unwrap())
        .unwrap()
        .snapshot
        .status = "completed".into();
    assert_eq!(
        fixture
            .manager
            .agent_cli_delivery_snapshot(&fixture.sink, &query)
            .unwrap_err(),
        "agent_cli_delivery_stop_unproven"
    );
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &closure)
        .unwrap();
    assert!(matches!(
        receipt.receipts[0].outcome,
        AgentExecutionStopOutcome::NeverCreated { .. }
    ));
    assert_eq!(
        fixture
            .manager
            .agent_cli_delivery_snapshot(&fixture.sink, &query)
            .unwrap_err(),
        "agent_cli_delivery_stop_unproven"
    );
}

#[test]
fn delivery_rejects_a_per_job_stop_without_complete_durable_close_receipt() {
    use webcodex_core::agent_cli::AgentCliDeliveryQuery;
    let fixture = fixture();
    let member = start(&fixture, 0);
    admit(&fixture, &member).unwrap();
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    persist_fence(&fixture, &closure);
    // Synthetic metadata exercises a negative gate only. It is never presented
    // as real process evidence, and must not permit filesystem delivery reads.
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.jobs[0].native_create_attempted = true;
            metadata.jobs[0].worker_finished = true;
            metadata.jobs[0].stop_receipt = Some(AgentExecutionStopReceipt {
                binding_sha256: member.binding.binding_hash()?,
                carrier: member.binding.carrier.clone(),
                observation_revision: 1,
                outcome: AgentExecutionStopOutcome::TreeExited {
                    leader_exit_code: Some(0),
                    leader_handle_signaled: true,
                    membership_complete: true,
                    member_handles_signaled: true,
                    worker_finished: true,
                },
            });
            Ok(())
        })
        .unwrap();
    let query = AgentCliDeliveryQuery {
        version: 1,
        target: member.binding.target.clone(),
        binding: member.binding,
        deadline_at_ms: chrono::Utc::now().timestamp_millis() + 40_000,
    };
    assert_eq!(
        fixture
            .manager
            .agent_cli_delivery_snapshot(&fixture.sink, &query)
            .unwrap_err(),
        "agent_cli_delivery_close_unproven"
    );
}

fn persist_fence(fixture: &Fixture, close: &AgentExecutionClose) {
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.closes.push(AgentExecutionCloseMetadata {
                close: close.clone(),
                receipt: None,
            });
            Ok(())
        })
        .unwrap();
}

fn insert_queued(fixture: &Fixture, start: &AgentExecutionStart) {
    let AgentExecutionCarrier::Job {
        job_id,
        start_request_id,
    } = &start.binding.carrier
    else {
        unreachable!()
    };
    let mut snapshot = test_job_snapshot(job_id);
    snapshot.request_id = start_request_id.clone();
    snapshot.status = "agent_queued".into();
    snapshot.context = isolated_job_context(start);
    lock_unpoison(&fixture.manager.jobs).insert(
        job_id.clone(),
        RunningJob {
            client_id: start.binding.target.client_id.clone(),
            runner_instance_id: start.binding.target.runner_instance_id.clone(),
            snapshot,
            child: None,
            stop_requested: Arc::new(AtomicBool::new(false)),
            slot_reserved: false,
        },
    );
    lock_unpoison(&fixture.manager.queued).push_back(PendingJobStart {
        generation: 1,
        policy: RunnerPolicy::default(),
        shell: ShellConfig::default(),
        ssh: SshConfig::default(),
        project_registry_dir: fixture.configuration.resource_root.join("unused-registry"),
        metadata: RunnerInvocationMetadata {
            request_id: start_request_id.clone(),
            client_id: start.binding.target.client_id.clone(),
            requested_by: "closure-test".into(),
            created_at: chrono::Utc::now().timestamp(),
        },
        operation: RunnerJobOperation::StartIsolatedProcess(RunnerJobIsolatedProcessOperation {
            context: isolated_job_context(start),
            start: start.clone(),
        }),
    });
}

#[test]
fn closure_fences_source_executor_and_each_explicit_member_identity() {
    for fence_kind in 0..7 {
        let fixture = fixture();
        let member = start(&fixture, 0);
        let mut late = unrelated(start(&fixture, 1));
        let mut source = AgentRunRef {
            agent_id: "ancestor".into(),
            agent_run_id: "ancestor-run".into(),
            generation: 1,
        };
        match fence_kind {
            0 => source = late.binding.requester.run.clone(),
            1 => source = late.binding.executor.run.clone(),
            2 => late.binding.executor.run = member.binding.executor.run.clone(),
            3 => {
                late.binding.requester.cli_attempt_id =
                    member.binding.requester.cli_attempt_id.clone()
            }
            4 => late.binding.carrier = member.binding.carrier.clone(),
            5 => {
                let AgentExecutionCarrier::Job {
                    job_id: late_id, ..
                } = &mut late.binding.carrier
                else {
                    unreachable!()
                };
                *late_id = job_id(&member.binding).unwrap().to_owned();
            }
            6 => {
                let AgentExecutionCarrier::Job {
                    start_request_id: member_request,
                    ..
                } = &member.binding.carrier
                else {
                    unreachable!()
                };
                let AgentExecutionCarrier::Job {
                    start_request_id: late_request,
                    ..
                } = &mut late.binding.carrier
                else {
                    unreachable!()
                };
                *late_request = member_request.clone();
            }
            _ => unreachable!(),
        }
        late.validate().unwrap();
        let closure = close(source, vec![member.binding.clone()], &member.binding);
        persist_fence(&fixture, &closure);
        let persisted = fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap();
        assert!(closed_for(&persisted, &late.binding), "fence {fence_kind}");
        assert_eq!(
            admit(&fixture, &late).unwrap_err(),
            "agent_execution_admission_closed",
            "fence {fence_kind}"
        );
        assert!(fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap()
            .jobs
            .is_empty());
        assert!(lock_unpoison(&fixture.manager.queued).is_empty());
        assert!(lock_unpoison(&fixture.manager.agent_execution)
            .live
            .is_empty());
    }
}

#[test]
fn a_different_run_and_generation_remain_admissible_after_close() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    persist_fence(&fixture, &closure);
    let different_run = unrelated(start(&fixture, 1));
    let mut next_generation = start(&fixture, 2);
    next_generation.binding.requester.run.generation += 1;
    next_generation.binding.executor.run.generation += 1;
    for candidate in [different_run, next_generation] {
        let persisted = fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap();
        assert!(!closed_for(&persisted, &candidate.binding));
        admit(&fixture, &candidate).unwrap();
    }
    assert_eq!(
        fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap()
            .jobs
            .len(),
        2
    );
}

#[test]
fn a_new_close_id_cannot_reset_the_original_source_deadline() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    let mut original = close(
        member.binding.requester.run.clone(),
        Vec::new(),
        &member.binding,
    );
    original.deadline_at_ms = chrono::Utc::now().timestamp_millis() - 1;
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &original)
            .unwrap_err(),
        "agent_execution_close_deadline_expired"
    );
    let mut replacement = original.clone();
    replacement.close_ref.close_id = "new-close-id".into();
    replacement.deadline_at_ms += 120_000;
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &replacement)
            .unwrap_err(),
        "agent_execution_source_already_closing"
    );
    replacement.close_ref.close_id = original.close_ref.close_id.clone();
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &replacement)
            .unwrap_err(),
        "agent_execution_close_identity_conflict"
    );
    let persisted = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(persisted.closes.len(), 1);
    assert_eq!(persisted.closes[0].close, original);
    assert!(persisted.closes[0].receipt.is_none());
    assert_eq!(
        admit(&fixture, &member).unwrap_err(),
        "agent_execution_admission_closed"
    );
}

#[test]
fn restart_with_a_native_attempt_and_no_original_handles_remains_unknown() {
    for worker_finished in [false, true] {
        let fixture = fixture();
        let member = start(&fixture, 0);
        admit(&fixture, &member).unwrap();
        fixture
            .configuration
            .store
            .update_agent_execution_metadata(|metadata| {
                metadata.jobs[0].native_create_attempted = true;
                metadata.jobs[0].worker_finished = worker_finished;
                Ok(())
            })
            .unwrap();
        let closure = close(
            member.binding.requester.run.clone(),
            vec![member.binding.clone()],
            &member.binding,
        );
        persist_fence(&fixture, &closure);
        let restarted = configured_manager(&fixture.configuration);
        assert_eq!(
            restarted
                .observe_agent_execution_member(
                    &fixture.configuration,
                    &member.binding,
                    Instant::now() + Duration::from_secs(1),
                )
                .unwrap_err(),
            "agent_execution_restarted_member_unknown"
        );
        assert_eq!(
            restarted
                .close_agent_execution(&fixture.sink, &closure)
                .unwrap_err(),
            "agent_execution_restarted_member_unknown"
        );
        let persisted = fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap();
        assert!(persisted.jobs[0].native_create_attempted);
        assert!(persisted.jobs[0].stop_receipt.is_none());
        assert!(persisted.closes[0].receipt.is_none());
        assert!(lock_unpoison(&restarted.agent_execution).live.is_empty());
    }
}

#[test]
fn queue_cancellation_without_a_native_attempt_produces_never_created() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    let other = unrelated(start(&fixture, 1));
    for candidate in [&member, &other] {
        admit(&fixture, candidate).unwrap();
        insert_queued(&fixture, candidate);
    }
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &closure)
        .unwrap();
    receipt.validate_for(&closure).unwrap();
    assert_eq!(receipt.receipts.len(), 1);
    assert!(matches!(
        receipt.receipts[0].outcome,
        AgentExecutionStopOutcome::NeverCreated {
            request_cancelled: true,
            worker_finished: true,
        }
    ));
    let queued = lock_unpoison(&fixture.manager.queued);
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].operation.job_id(),
        job_id(&other.binding).unwrap()
    );
    drop(queued);
    let jobs = lock_unpoison(&fixture.manager.jobs);
    assert!(jobs[job_id(&member.binding).unwrap()]
        .stop_requested
        .load(Ordering::SeqCst));
    assert!(!jobs[job_id(&other.binding).unwrap()]
        .stop_requested
        .load(Ordering::SeqCst));
    drop(jobs);
    let persisted = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert!(!persisted.jobs[0].native_create_attempted);
    assert!(persisted.jobs[0].worker_finished);
    assert_eq!(
        persisted.jobs[0].stop_receipt.as_ref(),
        Some(&receipt.receipts[0])
    );
    assert_eq!(persisted.closes[0].receipt.as_ref(), Some(&receipt));
    assert!(!persisted.jobs[1].worker_finished);
    assert!(persisted.jobs[1].stop_receipt.is_none());
    assert!(lock_unpoison(&fixture.manager.agent_execution)
        .live
        .is_empty());
}

#[test]
fn incomplete_or_conflicting_known_members_are_rejected_before_sealing() {
    let fixture = fixture();
    let first = start(&fixture, 0);
    let second = start(&fixture, 1);
    admit(&fixture, &first).unwrap();
    admit(&fixture, &second).unwrap();
    let incomplete = close(
        first.binding.requester.run.clone(),
        vec![first.binding.clone()],
        &first.binding,
    );
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &incomplete)
            .unwrap_err(),
        "agent_execution_close_missing_known_member"
    );
    let mut changed = first.binding.clone();
    changed.resource_manifest_hash = "c".repeat(64);
    let conflicting = close(
        first.binding.requester.run.clone(),
        vec![changed, second.binding.clone()],
        &first.binding,
    );
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &conflicting)
            .unwrap_err(),
        "agent_execution_close_member_conflict"
    );
    assert!(fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap()
        .closes
        .is_empty());
}

#[test]
fn a_sealed_member_set_and_its_proof_cannot_lose_or_gain_members() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    admit(&fixture, &member).unwrap();
    let closure = close(
        member.binding.requester.run.clone(),
        vec![member.binding.clone()],
        &member.binding,
    );
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &closure)
        .unwrap();
    for add_member in [false, true] {
        let mut changed = closure.clone();
        if add_member {
            changed.members.push(start(&fixture, 1).binding);
        } else {
            changed.members.clear();
        }
        changed.members_sha256 = compute_members_sha256(&changed.members).unwrap();
        changed.validate().unwrap();
        assert_eq!(
            fixture
                .manager
                .close_agent_execution(&fixture.sink, &changed)
                .unwrap_err(),
            "agent_execution_close_identity_conflict"
        );
        let mut changed_receipt = receipt.clone();
        if add_member {
            changed_receipt.receipts.push(receipt.receipts[0].clone());
        } else {
            changed_receipt.receipts.clear();
        }
        assert!(changed_receipt.validate_for(&closure).is_err());
    }
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &closure)
            .unwrap(),
        receipt
    );
}

#[test]
fn an_ancestor_close_must_include_all_known_jobs_of_an_explicit_executor() {
    let fixture = fixture();
    let first = start(&fixture, 0);
    let second = start(&fixture, 1);
    assert_eq!(first.binding.executor.run, second.binding.executor.run);
    assert_ne!(first.binding.carrier, second.binding.carrier);
    admit(&fixture, &first).unwrap();
    admit(&fixture, &second).unwrap();
    let ancestor = AgentRunRef {
        agent_id: "ancestor".into(),
        agent_run_id: "ancestor-run".into(),
        generation: 1,
    };
    let incomplete = close(
        ancestor.clone(),
        vec![first.binding.clone()],
        &first.binding,
    );
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &incomplete)
            .unwrap_err(),
        "agent_execution_close_missing_known_member"
    );
    let persisted = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert!(persisted.closes.is_empty());
    assert!(persisted.jobs.iter().all(|job| job.stop_receipt.is_none()));

    let complete = close(
        ancestor,
        vec![first.binding.clone(), second.binding.clone()],
        &first.binding,
    );
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &complete)
        .unwrap();
    receipt.validate_for(&complete).unwrap();
    assert_eq!(receipt.receipts.len(), 2);
    assert!(receipt.receipts.iter().all(|member| matches!(
        member.outcome,
        AgentExecutionStopOutcome::NeverCreated {
            request_cancelled: true,
            worker_finished: true,
        }
    )));
    let persisted = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(persisted.closes[0].receipt.as_ref(), Some(&receipt));
    assert!(persisted
        .jobs
        .iter()
        .all(|job| !job.native_create_attempted));
    assert!(lock_unpoison(&fixture.manager.agent_execution)
        .live
        .is_empty());
}

#[test]
fn close_rejects_partial_job_or_request_identity_aliases_before_observation() {
    for reuse_job_id in [false, true] {
        let fixture = fixture();
        let original = start(&fixture, 0);
        admit(&fixture, &original).unwrap();
        fixture
            .configuration
            .store
            .update_agent_execution_metadata(|metadata| {
                // A consumed native attempt survives restart. No process handle or
                // physical completion is fabricated for this rejection test.
                metadata.jobs[0].native_create_attempted = true;
                Ok(())
            })
            .unwrap();
        let mut alias = unrelated(start(&fixture, 1));
        let AgentExecutionCarrier::Job {
            job_id: original_id,
            start_request_id: original_request,
        } = &original.binding.carrier
        else {
            unreachable!()
        };
        let AgentExecutionCarrier::Job {
            job_id: alias_id,
            start_request_id: alias_request,
        } = &mut alias.binding.carrier
        else {
            unreachable!()
        };
        if reuse_job_id {
            *alias_id = original_id.clone();
        } else {
            *alias_request = original_request.clone();
        }
        alias.validate().unwrap();
        assert_ne!(alias.binding.carrier, original.binding.carrier);
        assert_ne!(
            alias.binding.requester.cli_attempt_id,
            original.binding.requester.cli_attempt_id
        );
        assert_ne!(alias.binding.requester.run, original.binding.requester.run);
        let closure = close(
            alias.binding.requester.run.clone(),
            vec![alias.binding.clone()],
            &alias.binding,
        );
        assert_eq!(
            fixture
                .manager
                .close_agent_execution(&fixture.sink, &closure)
                .unwrap_err(),
            "agent_execution_close_member_conflict"
        );
        let persisted = fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap();
        assert_eq!(persisted.jobs.len(), 1);
        assert_eq!(persisted.jobs[0].binding, original.binding);
        assert!(persisted.jobs[0].native_create_attempted);
        assert!(persisted.jobs[0].stop_receipt.is_none());
        assert!(persisted.closes.is_empty());
        assert!(lock_unpoison(&fixture.manager.agent_execution)
            .live
            .is_empty());
    }
}

#[test]
fn an_exact_durable_receipt_replays_after_its_original_deadline() {
    let fixture = fixture();
    let member = start(&fixture, 0);
    // Empty physical membership makes no invented process observation. Seed a
    // historically committed exact receipt to exercise restart/lost-response
    // replay without waiting for a wall-clock deadline or altering an identity.
    let mut closure = close(
        member.binding.requester.run.clone(),
        Vec::new(),
        &member.binding,
    );
    closure.deadline_at_ms = chrono::Utc::now().timestamp_millis() - 1;
    let receipt = AgentExecutionCloseReceipt {
        version: 1,
        close_ref: closure.close_ref.clone(),
        target: closure.target.clone(),
        deadline_at_ms: closure.deadline_at_ms,
        members_sha256: closure.members_sha256.clone(),
        observation_revision: 1,
        admission_closed: true,
        tombstone_persisted: true,
        receipts: Vec::new(),
    };
    receipt.validate_for(&closure).unwrap();
    fixture
        .configuration
        .store
        .update_agent_execution_metadata(|metadata| {
            metadata.closes.push(AgentExecutionCloseMetadata {
                close: closure.clone(),
                receipt: Some(receipt.clone()),
            });
            Ok(())
        })
        .unwrap();
    let restarted = configured_manager(&fixture.configuration);
    assert_eq!(
        restarted
            .close_agent_execution(&fixture.sink, &closure)
            .unwrap(),
        receipt
    );
    assert!(lock_unpoison(&restarted.agent_execution).live.is_empty());
    assert!(fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap()
        .jobs
        .is_empty());
}
