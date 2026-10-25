use super::*;
use crate::agent_execution::tests as execution;
use crate::AgentExecutionStoreAdmission;
use webcodex_core::runner_protocol::*;

struct Admission;
impl AgentExecutionStoreAdmission for Admission {
    fn validate_persisted(&self, _: &AgentExecutionBinding) -> Result<(), String> {
        Ok(())
    }
}

/// Simulated Store only. Production C/B must call the real current-authority DB
/// validator; no permissive implementation is exported by Registry.
#[derive(Clone)]
struct StoreEvidence {
    expected: AgentExecutionBinding,
    proof: AgentCliDeliveryStoreProof,
    deny: bool,
}
impl AgentCliDeliveryStoreEvidence for StoreEvidence {
    fn validate_closed_delivery(
        &self,
        binding: &AgentExecutionBinding,
    ) -> Result<AgentCliDeliveryStoreProof, String> {
        if self.deny || binding != &self.expected {
            return Err("simulated_store_delivery_denied".into());
        }
        Ok(self.proof.clone())
    }
}
async fn store_evidence_fixture() -> (RunnerRegistry, AgentExecutionBinding, StoreEvidence) {
    let (registry, binding) = stopped_fixture(Some(true)).await;
    let proof = {
        let inner = registry.inner.lock().await;
        let original = &inner.agent_execution_closes["close-request"];
        AgentCliDeliveryStoreProof {
            close: original.close.clone(),
            receipt: original.receipt.clone().unwrap(),
        }
    };
    let evidence = StoreEvidence {
        expected: binding.clone(),
        proof,
        deny: false,
    };
    (registry, binding, evidence)
}

#[tokio::test]
async fn agent_cli_recovered_delivery_replays_store_proof_without_rebuilding_execution() {
    let (previous, binding, mut store) = store_evidence_fixture().await;
    drop(previous);
    // Simulate a historical accepted DB row whose original close deadline has
    // elapsed. The recovered read must preserve that exact proof and use only
    // its own fresh read deadline.
    store.proof.close.deadline_at_ms = 1;
    store.proof.receipt.deadline_at_ms = 1;
    let expected_receipt = store.proof.receipt.clone();
    let registry = fixture().await;
    assert!(registry
        .read_agent_cli_delivery_snapshot(&execution::access(), &binding, "controller".into())
        .await
        .is_err());
    let reader = registry.clone();
    let original = binding.clone();
    let task = tokio::spawn(async move {
        reader
            .read_agent_cli_delivery_snapshot_recovered(
                &execution::access(),
                &original,
                &store,
                "controller".into(),
            )
            .await
    });
    let id = queued(&registry).await;
    {
        let inner = registry.inner.lock().await;
        assert!(inner.jobs_by_id.is_empty());
        assert!(inner.request_to_job.is_empty());
        assert!(inner.agent_execution_closes.is_empty());
        let proof = inner.pending_by_id[&id]
            .agent_cli
            .as_ref()
            .unwrap()
            .store_delivery_proof
            .as_ref()
            .unwrap();
        assert_eq!(proof.close.deadline_at_ms, 1);
        assert_eq!(proof.receipt, expected_receipt);
    }
    assert!(registry
        .read_prepared_agent_execution(&execution::access(), &binding)
        .await
        .is_err());
    assert!(registry
        .observe_agent_execution(&execution::access(), &binding)
        .await
        .is_err());
    let request = next(&registry).await;
    let RunnerOperation::AgentCliDelivery(query) = request.decode_operation().unwrap() else {
        panic!("not a read")
    };
    assert_eq!(query.binding, binding);
    assert!(query.deadline_at_ms > now_ms());
    respond(
        &registry,
        &request,
        serde_json::to_value(snapshot(&binding, false)).unwrap(),
    )
    .await
    .unwrap();
    let verified = task.await.unwrap().unwrap();
    assert_eq!(verified.binding(), &binding);
    assert_eq!(
        verified.artifact_sha256(),
        sha256(verified.artifact_bytes())
    );
    let inner = registry.inner.lock().await;
    assert!(
        inner.pending_by_id.is_empty()
            && inner.jobs_by_id.is_empty()
            && inner.agent_execution_closes.is_empty()
    );
    drop(inner);
    assert!(registry
        .read_agent_cli_delivery_snapshot(&execution::access(), &binding, "controller".into())
        .await
        .is_err());
}

#[tokio::test]
async fn agent_cli_recovered_delivery_rejects_denied_wrong_member_incomplete_or_nevercreated_proof()
{
    for mutation in ["deny", "member", "scope", "nevercreated", "incomplete"] {
        let (previous, binding, mut store) = store_evidence_fixture().await;
        drop(previous);
        match mutation {
            "deny" => store.deny = true,
            "member" | "scope" => {
                if mutation == "member" {
                    store.proof.close.members[0] = execution::start(1).binding;
                } else {
                    store.proof.close.members[0].project = "other-project".into();
                }
                store.proof.close.members_sha256 =
                    compute_members_sha256(&store.proof.close.members).unwrap();
                store.proof.receipt = execution::receipt(&store.proof.close);
                store.proof.receipt.receipts[0].outcome = AgentExecutionStopOutcome::TreeExited {
                    leader_exit_code: Some(0),
                    leader_handle_signaled: true,
                    membership_complete: true,
                    member_handles_signaled: true,
                    worker_finished: true,
                };
            }
            "nevercreated" => {
                store.proof.receipt.receipts[0].outcome = AgentExecutionStopOutcome::NeverCreated {
                    request_cancelled: true,
                    worker_finished: true,
                }
            }
            _ => store.proof.receipt.receipts.clear(),
        }
        let registry = fixture().await;
        assert!(
            registry
                .read_agent_cli_delivery_snapshot_recovered(
                    &execution::access(),
                    &binding,
                    &store,
                    "controller".into()
                )
                .await
                .is_err(),
            "{mutation}"
        );
        let inner = registry.inner.lock().await;
        assert!(
            inner.pending_by_id.is_empty()
                && inner.jobs_by_id.is_empty()
                && inner.agent_execution_closes.is_empty()
        );
    }
}

#[tokio::test]
async fn agent_cli_recovered_delivery_rejects_known_job_and_request_conflicts() {
    for mutation in ["job", "request"] {
        let (registry, binding, store) = store_evidence_fixture().await;
        let AgentExecutionCarrier::Job {
            job_id,
            start_request_id,
        } = &binding.carrier
        else {
            panic!()
        };
        {
            let mut inner = registry.inner.lock().await;
            if mutation == "job" {
                inner
                    .jobs_by_id
                    .get_mut(job_id)
                    .unwrap()
                    .agent_execution
                    .as_mut()
                    .unwrap()
                    .binding
                    .resource_manifest_hash = "b".repeat(64);
            } else {
                inner
                    .pending_by_id
                    .get_mut(start_request_id)
                    .unwrap()
                    .request
                    .client_id = "different-client".into();
            }
        }
        assert!(
            registry
                .read_agent_cli_delivery_snapshot_recovered(
                    &execution::access(),
                    &binding,
                    &store,
                    "controller".into()
                )
                .await
                .is_err(),
            "{mutation}"
        );
        assert!(registry
            .inner
            .lock()
            .await
            .pending_by_id
            .values()
            .all(|pending| pending.agent_cli.is_none()));
    }
}

#[tokio::test]
async fn agent_cli_recovered_delivery_keeps_current_reader_fence_and_foreign_access_denial() {
    let (previous, binding, store) = store_evidence_fixture().await;
    drop(previous);
    let registry = fixture().await;
    let mut foreign = execution::access();
    foreign.username = Some("other-owner".into());
    assert!(registry
        .read_agent_cli_delivery_snapshot_recovered(&foreign, &binding, &store, "controller".into())
        .await
        .is_err());
    let reader = registry.clone();
    let original = binding.clone();
    let task = tokio::spawn(async move {
        reader
            .read_agent_cli_delivery_snapshot_recovered(
                &execution::access(),
                &original,
                &store,
                "controller".into(),
            )
            .await
    });
    let request = next(&registry).await;
    registry
        .inner
        .lock()
        .await
        .runners
        .get_mut("runner")
        .unwrap()
        .runner_instance_id = "replacement".into();
    assert!(respond(
        &registry,
        &request,
        serde_json::to_value(snapshot(&binding, false)).unwrap()
    )
    .await
    .is_err());
    assert!(task.await.unwrap().is_err());
    let inner = registry.inner.lock().await;
    assert!(
        inner.pending_by_id.is_empty()
            && inner.jobs_by_id.is_empty()
            && inner.agent_execution_closes.is_empty()
    );
}
async fn fixture() -> RunnerRegistry {
    let registry = execution::registry(true).await;
    let mut resources = execution::resources();
    resources[0].provider = Some(AgentCliProvider::WorkBuddy);
    resources[1].provider = Some(AgentCliProvider::Antigravity);
    registry
        .register_with_auth(
            execution::registration(true, resources),
            Some(&execution::access()),
        )
        .await
        .unwrap();
    registry
}
async fn queued(registry: &RunnerRegistry) -> String {
    timeout_at(Instant::now() + Duration::from_secs(3), async {
        loop {
            if let Some(id) = registry
                .inner
                .lock()
                .await
                .pending_by_id
                .iter()
                .find_map(|(id, pending)| pending.agent_cli.is_some().then(|| id.clone()))
            {
                return id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixed read enqueued")
}
async fn next(registry: &RunnerRegistry) -> RunnerRequest {
    timeout_at(Instant::now() + Duration::from_secs(3), async {
        loop {
            let instance = registry.inner.lock().await.runners["runner"]
                .runner_instance_id
                .clone();
            if let Some(request) = registry
                .poll(RunnerPollRequest {
                    client_id: "runner".into(),
                    runner_instance_id: instance,
                })
                .await
                .unwrap()
            {
                return request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixed read dispatched")
}
fn catalog_task(
    registry: &RunnerRegistry,
) -> tokio::task::JoinHandle<Result<AgentCliCatalog, String>> {
    let registry = registry.clone();
    tokio::spawn(async move {
        registry
            .query_agent_cli_catalog(
                &execution::access(),
                &execution::start(0).binding.target,
                AgentCliProvider::WorkBuddy,
                "controller".into(),
            )
            .await
    })
}
fn catalog(query: &AgentCliCatalogQuery) -> AgentCliCatalog {
    let now = now_ms();
    AgentCliCatalog {
        version: 1,
        target: query.target.clone(),
        resource_set_id: "resources-0".into(),
        resource_manifest_hash: "a".repeat(64),
        executable_identity: format!("sha256:{}", "e".repeat(64)),
        provider: query.provider,
        cli_version: "1.2.3".into(),
        models: vec!["model-a".into()],
        efforts: vec!["low".into()],
        source: "installed_help".into(),
        status: "ready".into(),
        message: "Installed metadata only".into(),
        observed_at_ms: now,
        expires_at_ms: now + AGENT_CLI_CATALOG_TTL_MS,
        account_availability_verified: false,
        nested_delegation_verified_disabled: false,
    }
}
async fn respond(
    registry: &RunnerRegistry,
    request: &RunnerRequest,
    value: serde_json::Value,
) -> Result<(), String> {
    let instance = registry.inner.lock().await.runners["runner"]
        .runner_instance_id
        .clone();
    registry
        .complete(RunnerResultRequest {
            client_id: "runner".into(),
            runner_instance_id: instance,
            request_id: request.request_id.clone(),
            exit_code: Some(0),
            stdout: Some(value.to_string()),
            stderr: None,
            duration_ms: Some(1),
            error: None,
        })
        .await
}
async fn stopped_fixture(tree: Option<bool>) -> (RunnerRegistry, AgentExecutionBinding) {
    // Synthetic lifecycle fixture, not an unreviewed autonomous provider run.
    let registry = execution::registry(true).await;
    let (reservation, start) = execution::reserved_start(&registry, 0).await;
    let prepared = registry
        .prepare_agent_execution(
            &execution::access(),
            &reservation,
            start.clone(),
            "controller".into(),
        )
        .await
        .unwrap();
    registry
        .dispatch_prepared_agent_execution(&execution::access(), &prepared, &Admission)
        .await
        .unwrap();
    assert_eq!(next(&registry).await.kind, AGENT_EXECUTION_START_KIND);
    if let Some(tree) = tree {
        let close = execution::close(vec![start.binding.clone()]);
        registry
            .enqueue_agent_execution_close(
                &execution::access(),
                close.clone(),
                "close-request".into(),
                "controller".into(),
            )
            .await
            .unwrap();
        assert_eq!(next(&registry).await.kind, AGENT_EXECUTION_CLOSE_KIND);
        let mut receipt = execution::receipt(&close);
        if tree {
            receipt.receipts[0].outcome = AgentExecutionStopOutcome::TreeExited {
                leader_exit_code: Some(0),
                leader_handle_signaled: true,
                membership_complete: true,
                member_handles_signaled: true,
                worker_finished: true,
            };
        }
        registry
            .accept_agent_execution_close_receipt(
                "runner",
                "instance",
                None,
                "close-request",
                receipt,
            )
            .await
            .unwrap();
    }
    (registry, start.binding)
}
fn snapshot(binding: &AgentExecutionBinding, large: bool) -> AgentCliDeliverySnapshot {
    let files = if large {
        ["files/a.txt", "files/b.txt"]
            .into_iter()
            .map(|path| {
                let text = "z".repeat(192 * 1024);
                AgentCliDeliveryFile {
                    path: path.into(),
                    sha256: sha256(text.as_bytes()),
                    bytes: text.len() as u64,
                    text,
                }
            })
            .collect()
    } else {
        vec![]
    };
    let bundle = AgentCliDeliveryBundle {
        version: 1,
        binding_sha256: binding.binding_hash().unwrap(),
        summary: "authorized task delivery".into(),
        files,
    };
    let bytes = bundle.canonical_bytes(binding).unwrap();
    AgentCliDeliverySnapshot {
        version: 1,
        binding: binding.clone(),
        artifact_sha256: sha256(&bytes),
        bundle,
    }
}

#[tokio::test]
async fn agent_cli_catalog_fixed_query_returns_exact_non_authorizing_metadata() {
    let registry = fixture().await;
    let task = catalog_task(&registry);
    let request = next(&registry).await;
    assert_eq!(request.kind, AGENT_CLI_CATALOG_KIND);
    assert_eq!(request.timeout_secs, AGENT_CLI_CONTROL_TIMEOUT_SECS);
    assert!(request.command.is_empty() && request.process.is_none() && request.path.is_none());
    let RunnerOperation::AgentCliCatalog(query) = request.decode_operation().unwrap() else {
        panic!("wrong operation")
    };
    assert!(query.deadline_at_ms <= now_ms() + 40_000);
    respond(
        &registry,
        &request,
        serde_json::to_value(catalog(&query)).unwrap(),
    )
    .await
    .unwrap();
    let result = task.await.unwrap().unwrap();
    assert_eq!(result.models, vec!["model-a"]);
    assert!(!result.account_availability_verified && !result.nested_delegation_verified_disabled);
    assert!(registry.inner.lock().await.pending_by_id.is_empty());
}

#[tokio::test]
async fn agent_cli_catalog_mapping_is_explicit_unique_and_access_scoped() {
    let registry = execution::registry(true).await;
    let target = execution::start(0).binding.target;
    assert!(registry
        .query_agent_cli_catalog(
            &execution::access(),
            &target,
            AgentCliProvider::WorkBuddy,
            "controller".into()
        )
        .await
        .is_err());
    let mut resources = execution::resources();
    resources[0].provider = Some(AgentCliProvider::WorkBuddy);
    resources[1].provider = Some(AgentCliProvider::WorkBuddy);
    assert!(registry
        .register_with_auth(
            execution::registration(true, resources),
            Some(&execution::access())
        )
        .await
        .is_err());
    let mut foreign = execution::access();
    foreign.username = Some("other-owner".into());
    assert!(registry
        .query_agent_cli_catalog(
            &foreign,
            &target,
            AgentCliProvider::WorkBuddy,
            "controller".into()
        )
        .await
        .is_err());
    assert!(registry.inner.lock().await.pending_by_id.is_empty());
}

#[tokio::test]
async fn agent_cli_catalog_rejects_wrong_resource_and_execution_claims() {
    for mutation in ["resource", "account", "nested", "future"] {
        let registry = fixture().await;
        let task = catalog_task(&registry);
        let request = next(&registry).await;
        let RunnerOperation::AgentCliCatalog(query) = request.decode_operation().unwrap() else {
            panic!()
        };
        let mut result = catalog(&query);
        match mutation {
            "resource" => result.resource_manifest_hash = "b".repeat(64),
            "account" => result.account_availability_verified = true,
            "nested" => result.nested_delegation_verified_disabled = true,
            _ => {
                result.observed_at_ms += 120_000;
                result.expires_at_ms += 120_000;
            }
        }
        respond(&registry, &request, serde_json::to_value(result).unwrap())
            .await
            .unwrap();
        assert!(task.await.unwrap().is_err(), "{mutation}");
        assert!(registry.inner.lock().await.pending_by_id.is_empty());
    }
}

#[tokio::test]
async fn agent_cli_catalog_resource_change_is_fenced_before_dispatch_and_response() {
    for dispatched in [false, true] {
        let registry = fixture().await;
        let task = catalog_task(&registry);
        queued(&registry).await;
        let request = if dispatched {
            Some(next(&registry).await)
        } else {
            None
        };
        registry
            .inner
            .lock()
            .await
            .runners
            .get_mut("runner")
            .unwrap()
            .agent_execution_resources[0]
            .manifest_hash = "b".repeat(64);
        if let Some(request) = request {
            let RunnerOperation::AgentCliCatalog(query) = request.decode_operation().unwrap()
            else {
                panic!()
            };
            assert!(respond(
                &registry,
                &request,
                serde_json::to_value(catalog(&query)).unwrap()
            )
            .await
            .is_err());
        } else {
            assert!(execution::poll(&registry).await.is_none());
        }
        assert!(task.await.unwrap().is_err());
        assert!(registry.inner.lock().await.pending_by_id.is_empty());
    }
}

#[tokio::test]
async fn agent_cli_drop_while_registry_locked_closes_receiver_and_cleans_original_pending() {
    let registry = fixture().await;
    let deadline = Instant::now() + Duration::from_secs(40);
    let operation = RunnerOperation::AgentCliCatalog(AgentCliCatalogQuery {
        version: 1,
        target: execution::start(0).binding.target,
        provider: AgentCliProvider::WorkBuddy,
        deadline_at_ms: now_ms() + 40_000,
    });
    let read = registry
        .enqueue_agent_cli_read(
            &execution::access(),
            operation,
            deadline,
            "controller".into(),
        )
        .await
        .unwrap();
    let id = read.request_id.clone();
    let inner = registry.inner.lock().await;
    drop(read);
    assert!(inner.pending_by_id[&id]
        .waiter
        .as_ref()
        .unwrap()
        .is_closed());
    drop(inner);
    assert!(execution::poll(&registry).await.is_none());
    crate::recovery_timeout_sweep(&registry).await;
    assert!(registry.inner.lock().await.pending_by_id.is_empty());
}

#[tokio::test]
async fn agent_cli_deadline_is_not_reset_by_queue_and_expiry_prevents_late_dispatch() {
    let registry = fixture().await;
    let task = catalog_task(&registry);
    let id = queued(&registry).await;
    registry
        .inner
        .lock()
        .await
        .pending_by_id
        .get_mut(&id)
        .unwrap()
        .agent_cli
        .as_mut()
        .unwrap()
        .deadline = Instant::now();
    assert!(execution::poll(&registry).await.is_none());
    assert!(task.await.unwrap().is_err());
    let query = RunnerOperation::AgentCliCatalog(AgentCliCatalogQuery {
        version: 1,
        target: execution::start(0).binding.target,
        provider: AgentCliProvider::WorkBuddy,
        deadline_at_ms: now_ms() + 40_000,
    });
    assert!(registry
        .enqueue_agent_cli_read(
            &execution::access(),
            query,
            Instant::now(),
            "controller".into()
        )
        .await
        .is_err());
    assert!(registry.inner.lock().await.pending_by_id.is_empty());
}

#[tokio::test]
async fn agent_cli_cancelled_dispatched_query_is_not_retried_and_late_response_is_rejected() {
    let registry = fixture().await;
    let task = catalog_task(&registry);
    let request = next(&registry).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    registry.cancel_abandoned_sync_requests().await;
    let RunnerOperation::AgentCliCatalog(query) = request.decode_operation().unwrap() else {
        panic!()
    };
    assert!(respond(
        &registry,
        &request,
        serde_json::to_value(catalog(&query)).unwrap()
    )
    .await
    .is_err());
    assert!(execution::poll(&registry).await.is_none());
    assert!(registry.inner.lock().await.pending_by_id.is_empty());
}

#[tokio::test]
async fn agent_cli_delivery_rejects_unknown_and_nevercreated_even_with_a_valid_raw_snapshot() {
    for tree in [None, Some(false)] {
        let (registry, binding) = stopped_fixture(tree).await;
        assert!(snapshot(&binding, false).validate_for(&binding).is_ok());
        assert!(registry
            .read_agent_cli_delivery_snapshot(&execution::access(), &binding, "controller".into())
            .await
            .is_err());
        assert!(execution::poll(&registry).await.is_none());
    }
}

#[tokio::test]
async fn agent_cli_delivery_native_proof_returns_canonical_bytes_above_generic_output_limit() {
    let (registry, binding) = stopped_fixture(Some(true)).await;
    let expected = snapshot(&binding, true);
    let bytes = expected.validate_for(&binding).unwrap();
    assert!(bytes.len() > crate::MAX_OUTPUT_BYTES);
    let reader = registry.clone();
    let original = binding.clone();
    let task = tokio::spawn(async move {
        reader
            .read_agent_cli_delivery_snapshot(&execution::access(), &original, "controller".into())
            .await
    });
    let request = next(&registry).await;
    assert_eq!(request.kind, AGENT_CLI_DELIVERY_KIND);
    respond(
        &registry,
        &request,
        serde_json::to_value(&expected).unwrap(),
    )
    .await
    .unwrap();
    let verified = task.await.unwrap().unwrap();
    assert_eq!(verified.binding(), &binding);
    assert_eq!(verified.artifact_bytes(), bytes);
    assert_eq!(verified.artifact_sha256(), expected.artifact_sha256);
}

#[tokio::test]
async fn agent_cli_delivery_reader_can_restart_without_retargeting_original_execution() {
    let (registry, binding) = stopped_fixture(Some(true)).await;
    let mut registration = execution::registration(false, vec![]);
    registration.runner_instance_id = "new-reader".into();
    registry
        .register_with_auth(registration, Some(&execution::access()))
        .await
        .unwrap();
    let reader = registry.clone();
    let original = binding.clone();
    let task = tokio::spawn(async move {
        reader
            .read_agent_cli_delivery_snapshot(&execution::access(), &original, "controller".into())
            .await
    });
    let request = next(&registry).await;
    let RunnerOperation::AgentCliDelivery(query) = request.decode_operation().unwrap() else {
        panic!()
    };
    assert_eq!(query.target.runner_instance_id, "new-reader");
    assert_eq!(query.binding, binding);
    assert_eq!(query.binding.target.runner_instance_id, "instance");
    respond(
        &registry,
        &request,
        serde_json::to_value(snapshot(&binding, false)).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(task.await.unwrap().unwrap().binding(), &binding);
    let restarted_server = fixture().await;
    assert!(restarted_server
        .read_agent_cli_delivery_snapshot(&execution::access(), &binding, "controller".into())
        .await
        .is_err());
}

#[tokio::test]
async fn agent_cli_delivery_rejects_reader_replacement_digest_and_foreign_visibility() {
    for mutation in ["reader", "digest"] {
        let (registry, binding) = stopped_fixture(Some(true)).await;
        let mut foreign = execution::access();
        foreign.username = Some("other-owner".into());
        assert!(registry
            .read_agent_cli_delivery_snapshot(&foreign, &binding, "controller".into())
            .await
            .is_err());
        let reader = registry.clone();
        let original = binding.clone();
        let task = tokio::spawn(async move {
            reader
                .read_agent_cli_delivery_snapshot(
                    &execution::access(),
                    &original,
                    "controller".into(),
                )
                .await
        });
        let request = next(&registry).await;
        let mut output = snapshot(&binding, false);
        if mutation == "reader" {
            registry
                .inner
                .lock()
                .await
                .runners
                .get_mut("runner")
                .unwrap()
                .runner_instance_id = "replacement".into();
        } else {
            output.artifact_sha256 = "b".repeat(64);
        }
        let result = respond(&registry, &request, serde_json::to_value(output).unwrap()).await;
        if mutation == "reader" {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        assert!(task.await.unwrap().is_err());
    }
}

#[tokio::test]
async fn agent_cli_generic_queue_cannot_bypass_fixed_read_fence() {
    let registry = fixture().await;
    let operation = RunnerOperation::AgentCliCatalog(AgentCliCatalogQuery {
        version: 1,
        target: execution::start(0).binding.target,
        provider: AgentCliProvider::WorkBuddy,
        deadline_at_ms: now_ms() + 40_000,
    });
    let wire = RunnerRequest::from_operation(
        RunnerInvocationMetadata {
            request_id: "generic-query".into(),
            client_id: "runner".into(),
            requested_by: "test".into(),
            created_at: crate::now_ts(),
        },
        operation,
    )
    .unwrap();
    let mut inner = registry.inner.lock().await;
    assert!(crate::requests::enqueue_pending_request_locked(
        registry.telemetry.as_ref(),
        &mut inner,
        "runner",
        "generic-query".into(),
        wire,
        None,
        None
    )
    .is_err());
    assert!(inner.pending_by_id.is_empty());
}

#[tokio::test]
async fn agent_cli_delivery_returns_only_allowlisted_errors_from_exact_runner() {
    for (error, expected) in [
        (
            "agent_cli_delivery_not_published",
            "agent_cli_delivery_not_published",
        ),
        ("private-path-or-secret", "agent_cli_query_response_invalid"),
    ] {
        let (registry, binding) = stopped_fixture(Some(true)).await;
        let reader = registry.clone();
        let task = tokio::spawn(async move {
            reader
                .read_agent_cli_delivery_snapshot(
                    &execution::access(),
                    &binding,
                    "controller".into(),
                )
                .await
        });
        let request = next(&registry).await;
        let instance = registry.inner.lock().await.runners["runner"]
            .runner_instance_id
            .clone();
        registry
            .complete(RunnerResultRequest {
                client_id: "runner".into(),
                runner_instance_id: instance,
                request_id: request.request_id,
                exit_code: None,
                stdout: None,
                stderr: None,
                duration_ms: Some(0),
                error: Some(error.into()),
            })
            .await
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap_err(), expected);
    }
}
