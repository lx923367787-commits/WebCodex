//! Fixed metadata and stopped-worker delivery reads over the existing queue.
//! C/B must first authorize the exact original attempt/current Task read. A
//! RunnerAccess partition is not an Agent identity or Task authorization.
use crate::agent_execution::{current_target, exact_job, require_execution_capabilities};
use crate::jobs::ensure_dispatch_supported_locked;
use crate::requests::{
    enqueue_agent_cli_request_locked, next_request_id, notify_runner_locked,
    remove_pending_request_locked,
};
use crate::state::{PendingShellRequest, RunnerRegistryInner};
use crate::{RunnerAccess, RunnerRegistry};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::{timeout_at, Duration, Instant};
use webcodex_core::agent_cli::*;
use webcodex_core::runner_operation::{RunnerInvocationMetadata, RunnerOperation};
use webcodex_core::runner_protocol::{
    AgentExecutionBinding, AgentExecutionCarrier, AgentExecutionClose, AgentExecutionCloseReceipt,
    AgentExecutionResourceDescriptor, AgentExecutionStopOutcome, AgentExecutionTarget,
    RunnerRequest, ShellRunResponse,
};

pub(super) const DELIVERY_RESPONSE_MAX_BYTES: usize = AGENT_CLI_DELIVERY_RESPONSE_MAX_BYTES;
pub(super) struct AgentCliEnqueuePermit(());

/// Trusted C/B adapter backed by an actual Store transaction. It must re-read
/// current original-caller/root-manager Task read authority, original attempt,
/// exact full binding and terminal state, and the durably accepted complete
/// close containing this exact member TreeExited. No production permissive
/// implementation is provided here. A raw DTO or an empty Registry proves none
/// of these facts; the callback must not call Registry while holding Store.
pub trait AgentCliDeliveryStoreEvidence: Send + Sync {
    fn validate_closed_delivery(
        &self,
        binding: &AgentExecutionBinding,
    ) -> Result<AgentCliDeliveryStoreProof, String>;
}

/// Internal Store evidence only, deliberately not serde data. Transport-shaped
/// fields are revalidated by Registry and authorize only this bounded read.
#[derive(Debug, Clone)]
pub struct AgentCliDeliveryStoreProof {
    pub close: AgentExecutionClose,
    pub receipt: AgentExecutionCloseReceipt,
}

#[derive(Debug, Clone)]
pub(super) struct AgentCliFence {
    pub(super) deadline: Instant,
    pub(super) resource: Option<AgentExecutionResourceDescriptor>,
    pub(super) access: RunnerAccess,
    /// Per-request evidence; never installed as a Job or global execution owner.
    store_delivery_proof: Option<Arc<AgentCliDeliveryStoreProof>>,
}

/// Produced only after the original execution and accepted complete close
/// (from Registry or the trusted Store bridge), native TreeExited proof and
/// canonical artifact bytes have all been checked. This is
/// read provenance, never a durable close receipt or business-success assertion.
pub struct VerifiedAgentCliDeliverySnapshot {
    binding: AgentExecutionBinding,
    artifact_bytes: Vec<u8>,
    artifact_sha256: String,
}
impl VerifiedAgentCliDeliverySnapshot {
    pub fn binding(&self) -> &AgentExecutionBinding {
        &self.binding
    }
    pub fn artifact_bytes(&self) -> &[u8] {
        &self.artifact_bytes
    }
    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }
}
impl std::fmt::Debug for VerifiedAgentCliDeliverySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedAgentCliDeliverySnapshot")
            .field("binding", &self.binding)
            .field("artifact_sha256", &self.artifact_sha256)
            .field("artifact_bytes", &"[REDACTED]")
            .finish()
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn target(operation: &RunnerOperation) -> Result<&AgentExecutionTarget, String> {
    match operation {
        RunnerOperation::AgentCliCatalog(query) => Ok(&query.target),
        RunnerOperation::AgentCliInventory(query) => Ok(&query.target),
        RunnerOperation::AgentCliDelivery(query) => Ok(&query.target),
        _ => Err("agent_cli_fixed_query_required".into()),
    }
}
fn deadline_at_ms(operation: &RunnerOperation) -> Result<i64, String> {
    match operation {
        RunnerOperation::AgentCliCatalog(query) => Ok(query.deadline_at_ms),
        RunnerOperation::AgentCliInventory(query) => Ok(query.deadline_at_ms),
        RunnerOperation::AgentCliDelivery(query) => Ok(query.deadline_at_ms),
        _ => Err("agent_cli_fixed_query_required".into()),
    }
}
pub(super) fn is_query(operation: &RunnerOperation) -> bool {
    matches!(
        operation,
        RunnerOperation::AgentCliCatalog(_)
            | RunnerOperation::AgentCliDelivery(_)
            | RunnerOperation::AgentCliInventory(_)
    )
}

fn verify_delivery_origin(
    inner: &RunnerRegistryInner,
    access: &RunnerAccess,
    binding: &AgentExecutionBinding,
) -> Result<(), String> {
    let job = exact_job(inner, access, binding)?;
    let state = job
        .agent_execution
        .as_ref()
        .ok_or("agent_cli_original_job_required")?;
    if !state.transport_dispatched {
        return Err("agent_cli_original_job_not_dispatched".into());
    }
    for close in inner.agent_execution_closes.values() {
        let Some(receipt) = close.receipt.as_ref() else {
            continue;
        };
        let Some(index) = close
            .close
            .members
            .iter()
            .position(|member| member == binding)
        else {
            continue;
        };
        receipt.validate_for(&close.close)?;
        if matches!(
            &receipt.receipts[index].outcome,
            AgentExecutionStopOutcome::TreeExited { .. }
        ) {
            return Ok(());
        }
    }
    Err("agent_cli_delivery_tree_exit_unproven".into())
}

fn verify_recovered_delivery_origin(
    inner: &RunnerRegistryInner,
    access: &RunnerAccess,
    binding: &AgentExecutionBinding,
    proof: &AgentCliDeliveryStoreProof,
) -> Result<(), String> {
    binding.validate()?;
    proof.close.validate()?;
    proof.receipt.validate_for(&proof.close)?;
    let AgentExecutionCarrier::Job {
        job_id,
        start_request_id,
    } = &binding.carrier
    else {
        return Err("agent_cli_delivery_job_required".into());
    };
    if proof.close.target != binding.target
        || proof
            .close
            .members
            .iter()
            .any(|member| member.project != binding.project || member.target != binding.target)
    {
        return Err("agent_cli_store_close_scope_mismatch".into());
    }
    let index = proof
        .close
        .members
        .iter()
        .position(|member| member == binding)
        .ok_or("agent_cli_store_close_member_missing")?;
    if !matches!(
        &proof.receipt.receipts[index].outcome,
        AgentExecutionStopOutcome::TreeExited { .. }
    ) {
        return Err("agent_cli_delivery_tree_exit_unproven".into());
    }
    // Persisted accepted proof is replayable after its original deadline. Do
    // not change that deadline or create any new close/execute authorization.
    if inner.jobs_by_id.contains_key(job_id) {
        let job = exact_job(inner, access, binding)?;
        if job
            .agent_execution
            .as_ref()
            .is_none_or(|state| !state.transport_dispatched)
        {
            return Err("agent_cli_store_known_binding_conflict".into());
        }
    }
    if inner.request_to_job.iter().any(|(request, job)| {
        (request == start_request_id && job != job_id)
            || (job == job_id && request != start_request_id)
    }) {
        return Err("agent_cli_store_known_binding_conflict".into());
    }
    let collides = |known: &AgentExecutionBinding| {
        known.requester.cli_attempt_id == binding.requester.cli_attempt_id
            || matches!(&known.carrier, AgentExecutionCarrier::Job { job_id: known_job, start_request_id: known_request } if known_job == job_id || known_request == start_request_id)
    };
    if inner.jobs_by_id.values().any(|job| {
        (job.request_id.as_ref() == Some(start_request_id) && &job.job_id != job_id)
            || job
                .agent_execution
                .as_ref()
                .is_some_and(|state| collides(&state.binding) && &state.binding != binding)
    }) {
        return Err("agent_cli_store_known_binding_conflict".into());
    }
    for (request_id, pending) in &inner.pending_by_id {
        if request_id == start_request_id {
            if pending.request.request_id != *start_request_id
                || pending.request.client_id != binding.target.client_id
                || !matches!(&pending.operation, RunnerOperation::Job(webcodex_core::runner_operation::RunnerJobOperation::StartIsolatedProcess(start)) if &start.start.binding == binding && pending.job_id.as_ref() == Some(job_id))
            {
                return Err("agent_cli_store_known_request_conflict".into());
            }
        }
        if let RunnerOperation::Job(
            webcodex_core::runner_operation::RunnerJobOperation::StartIsolatedProcess(start),
        ) = &pending.operation
        {
            if collides(&start.start.binding)
                && (&start.start.binding != binding
                    || request_id != start_request_id
                    || pending.request.request_id != *start_request_id
                    || pending.job_id.as_ref() != Some(job_id))
            {
                return Err("agent_cli_store_known_binding_conflict".into());
            }
        }
    }
    for known in inner.agent_execution_closes.values() {
        if known
            .close
            .members
            .iter()
            .any(|member| collides(member) && member != binding)
        {
            return Err("agent_cli_store_known_binding_conflict".into());
        }
        if known.close.close_ref.close_id == proof.close.close_ref.close_id
            && known.close.target == proof.close.target
            && (known.close != proof.close
                || known
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt != &proof.receipt))
        {
            return Err("agent_cli_store_known_close_conflict".into());
        }
    }
    Ok(())
}

fn observed_resource(
    inner: &RunnerRegistryInner,
    access: &RunnerAccess,
    operation: &RunnerOperation,
    store_delivery_proof: Option<&AgentCliDeliveryStoreProof>,
) -> Result<Option<AgentExecutionResourceDescriptor>, String> {
    let runner = current_target(inner, access, target(operation)?)?;
    match operation {
        RunnerOperation::AgentCliInventory(query) => {
            query.validate()?;
            if query.native_only
                && !runner
                    .runner_features
                    .supports(crate::RunnerFeature::LocalCli)
            {
                return Err("local_cli_runner_upgrade_required".into());
            }
            if store_delivery_proof.is_some() {
                return Err("agent_cli_store_proof_not_catalog_authority".into());
            }
            Ok(None)
        }
        RunnerOperation::AgentCliCatalog(query) => {
            if store_delivery_proof.is_some() {
                return Err("agent_cli_store_proof_not_catalog_authority".into());
            }
            require_execution_capabilities(runner)?;
            query.validate()?;
            let mut matches = runner
                .agent_execution_resources
                .iter()
                .filter(|resource| resource.provider == Some(query.provider));
            let resource = matches
                .next()
                .ok_or("agent_cli_provider_resource_unavailable")?;
            if matches.next().is_some() {
                return Err("agent_cli_provider_resource_ambiguous".into());
            }
            Ok(Some(resource.clone()))
        }
        RunnerOperation::AgentCliDelivery(query) => {
            query.validate()?;
            if let Some(proof) = store_delivery_proof {
                verify_recovered_delivery_origin(inner, access, &query.binding, proof)?;
            } else {
                verify_delivery_origin(inner, access, &query.binding)?;
            }
            // A stopped Job's artifact ownership is independent of today's
            // provider catalog and executable profile availability.
            Ok(None)
        }
        _ => Err("agent_cli_fixed_query_required".into()),
    }
}

fn validate_fence(
    inner: &RunnerRegistryInner,
    operation: &RunnerOperation,
    fence: &AgentCliFence,
) -> Result<(), String> {
    if Instant::now() >= fence.deadline || now_ms() >= deadline_at_ms(operation)? {
        return Err("agent_cli_query_expired".into());
    }
    if observed_resource(
        inner,
        &fence.access,
        operation,
        fence.store_delivery_proof.as_deref(),
    )? != fence.resource
    {
        return Err("agent_cli_query_resource_changed".into());
    }
    if Instant::now() >= fence.deadline || now_ms() >= deadline_at_ms(operation)? {
        return Err("agent_cli_query_expired".into());
    }
    Ok(())
}

pub(super) fn validate_pending(
    inner: &RunnerRegistryInner,
    pending: &PendingShellRequest,
) -> Result<(), String> {
    if !is_query(&pending.operation) {
        return if pending.agent_cli.is_none() {
            Ok(())
        } else {
            Err("agent_cli_fence_mismatch".into())
        };
    }
    let fence = pending
        .agent_cli
        .as_ref()
        .ok_or("agent_cli_checked_query_required")?;
    if pending
        .waiter
        .as_ref()
        .is_none_or(oneshot::Sender::is_closed)
    {
        return Err("agent_cli_query_cancelled".into());
    }
    validate_fence(inner, &pending.operation, fence)
}

pub(super) fn reject_pending(inner: &mut RunnerRegistryInner, request_id: &str, error: String) {
    if let Some(mut pending) = remove_pending_request_locked(inner, request_id) {
        if let Some(waiter) = pending.waiter.take() {
            let _ = waiter.send(ShellRunResponse {
                success: false,
                request_id: request_id.into(),
                client_id: pending.request.client_id,
                cwd: None,
                command_preview: String::new(),
                exit_code: None,
                stdout: None,
                stderr: None,
                duration_ms: None,
                error: Some(error),
                request_dispatched: Some(pending.dispatched),
                command_execution_state: None,
            });
        }
    }
}

/// Owns receiver cancellation. No new scheduler: a contended Drop uses the
/// existing cancellation method, while poll and the existing sweep also reject
/// closed receivers. There is no await between queue insertion and arming it.
struct PendingRead {
    registry: RunnerRegistry,
    request_id: String,
    operation: RunnerOperation,
    fence: AgentCliFence,
    receiver: oneshot::Receiver<ShellRunResponse>,
}
impl Drop for PendingRead {
    fn drop(&mut self) {
        self.receiver.close();
        if let Ok(mut inner) = self.registry.inner.try_lock() {
            remove_pending_request_locked(&mut inner, &self.request_id);
        } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let registry = self.registry.clone();
            let request_id = self.request_id.clone();
            runtime.spawn(async move {
                registry.cancel_request_dispatch_state(&request_id).await;
            });
        }
    }
}
impl PendingRead {
    async fn response(&mut self, max_bytes: usize) -> Result<String, String> {
        let response = timeout_at(self.fence.deadline, &mut self.receiver)
            .await
            .map_err(|_| "agent_cli_query_timeout")?
            .map_err(|_| "agent_cli_query_response_unavailable")?;
        if response.request_id != self.request_id
            || response.client_id != target(&self.operation)?.client_id
            || response.request_dispatched != Some(true)
            || response.command_execution_state.is_some()
        {
            return Err("agent_cli_query_response_invalid".into());
        }
        // Only finite delivery codes from the exact authenticated response may
        // cross this boundary. Never expose arbitrary Runner errors/logs.
        if !response.success || response.exit_code != Some(0) || response.error.is_some() {
            if matches!(self.operation, RunnerOperation::AgentCliDelivery(_))
                && !response.success
                && response.exit_code.is_none()
                && response.stdout.is_none()
            {
                if let Some(
                    code @ ("agent_cli_delivery_not_published"
                    | "agent_cli_delivery_manifest_invalid"
                    | "agent_cli_delivery_manifest_limit"
                    | "agent_cli_delivery_text_required"
                    | "agent_cli_delivery_file_limit"
                    | "agent_cli_delivery_invalid_path"),
                ) = response.error.as_deref()
                {
                    return Err(code.into());
                }
            }
            if !response.success
                && response.exit_code.is_none()
                && response.stdout.is_none()
                && matches!(
                    self.operation,
                    RunnerOperation::AgentCliInventory(_) | RunnerOperation::AgentCliCatalog(_)
                )
            {
                if let Some(code) = response.error.as_deref().filter(|code| {
                    code.len() <= 256
                        && [
                            "cli_resource_",
                            "agent_cli_",
                            "agent_resources_",
                            "native_cli_",
                            "agent_execution_",
                        ]
                        .iter()
                        .any(|prefix| code.starts_with(prefix))
                        && code.bytes().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || b"_:-".contains(&c)
                        })
                        && !webcodex_core::sensitive_text::secret_like_value(code)
                }) {
                    return Err(code.into());
                }
            }
            return Err("agent_cli_query_response_invalid".into());
        }
        let content = response.stdout.ok_or("agent_cli_query_response_missing")?;
        if content.len() > max_bytes {
            return Err("agent_cli_query_response_too_large".into());
        }
        Ok(content)
    }
}

impl RunnerRegistry {
    async fn enqueue_agent_cli_read(
        &self,
        access: &RunnerAccess,
        operation: RunnerOperation,
        deadline: Instant,
        requested_by: String,
    ) -> Result<PendingRead, String> {
        self.enqueue_agent_cli_read_with_proof(access, operation, deadline, requested_by, None)
            .await
    }

    async fn enqueue_agent_cli_read_with_proof(
        &self,
        access: &RunnerAccess,
        operation: RunnerOperation,
        deadline: Instant,
        requested_by: String,
        store_delivery_proof: Option<Arc<AgentCliDeliveryStoreProof>>,
    ) -> Result<PendingRead, String> {
        let mut inner = timeout_at(deadline, self.inner.lock())
            .await
            .map_err(|_| "agent_cli_query_timeout")?;
        if Instant::now() >= deadline || now_ms() >= deadline_at_ms(&operation)? {
            return Err("agent_cli_query_expired".into());
        }
        let resource =
            observed_resource(&inner, access, &operation, store_delivery_proof.as_deref())?;
        let client_id = target(&operation)?.client_id.clone();
        ensure_dispatch_supported_locked(&inner, &client_id).map_err(String::from)?;
        let request_id = next_request_id();
        let wire = RunnerRequest::from_operation(
            RunnerInvocationMetadata {
                request_id: request_id.clone(),
                client_id: client_id.clone(),
                requested_by,
                created_at: crate::now_ts(),
            },
            operation.clone(),
        )?;
        let (sender, receiver) = oneshot::channel();
        if Instant::now() >= deadline || now_ms() >= deadline_at_ms(&operation)? {
            return Err("agent_cli_query_expired".into());
        }
        enqueue_agent_cli_request_locked(
            self.telemetry.as_ref(),
            &mut inner,
            &client_id,
            request_id.clone(),
            wire,
            sender,
            &AgentCliEnqueuePermit(()),
        )?;
        let fence = AgentCliFence {
            deadline,
            resource,
            access: access.clone(),
            store_delivery_proof,
        };
        inner
            .pending_by_id
            .get_mut(&request_id)
            .expect("just queued fixed read")
            .agent_cli = Some(fence.clone());
        let read = PendingRead {
            registry: self.clone(),
            request_id,
            operation,
            fence,
            receiver,
        };
        notify_runner_locked(&inner, &client_id);
        Ok(read)
    }

    pub async fn query_agent_cli_inventory(
        &self,
        access: &RunnerAccess,
        target: &AgentExecutionTarget,
        provider: Option<AgentCliProvider>,
        requested_by: String,
    ) -> Result<AgentCliInventory, String> {
        self.query_cli_inventory_mode(access, target, provider, requested_by, false)
            .await
    }

    pub async fn query_native_cli_inventory(
        &self,
        access: &RunnerAccess,
        target: &AgentExecutionTarget,
        provider: Option<AgentCliProvider>,
        requested_by: String,
    ) -> Result<AgentCliInventory, String> {
        self.query_cli_inventory_mode(access, target, provider, requested_by, true)
            .await
    }

    async fn query_cli_inventory_mode(
        &self,
        access: &RunnerAccess,
        target: &AgentExecutionTarget,
        provider: Option<AgentCliProvider>,
        requested_by: String,
        native_only: bool,
    ) -> Result<AgentCliInventory, String> {
        let deadline = Instant::now() + Duration::from_secs(AGENT_CLI_CONTROL_TIMEOUT_SECS);
        let query = AgentCliInventoryQuery {
            version: 1,
            native_only,
            target: target.clone(),
            provider,
            deadline_at_ms: now_ms() + (AGENT_CLI_CONTROL_TIMEOUT_SECS * 1000) as i64,
        };
        let mut read = self
            .enqueue_agent_cli_read(
                access,
                RunnerOperation::AgentCliInventory(query.clone()),
                deadline,
                requested_by,
            )
            .await?;
        let raw = read.response(AGENT_CLI_CATALOG_MAX_BYTES).await?;
        let inventory: AgentCliInventory =
            serde_json::from_str(&raw).map_err(|_| "agent_cli_inventory_invalid_response")?;
        let inner = timeout_at(deadline, self.inner.lock())
            .await
            .map_err(|_| "agent_cli_query_timeout")?;
        validate_fence(&inner, &read.operation, &read.fence)?;
        inventory.validate_for(&query, now_ms())?;
        if inventory.observed_at_ms
            < query.deadline_at_ms - (AGENT_CLI_CONTROL_TIMEOUT_SECS * 1000) as i64
        {
            return Err("agent_cli_inventory_stale".into());
        }
        Ok(inventory)
    }

    /// Fixed read only: no executable path, argv, prompt or caller deadline.
    /// C/B authorizes the caller first. One absolute 40s deadline covers lock,
    /// queue, response and validation. This method never retries a probe.
    pub async fn query_agent_cli_catalog(
        &self,
        access: &RunnerAccess,
        exact_target: &AgentExecutionTarget,
        provider: AgentCliProvider,
        requested_by: String,
    ) -> Result<AgentCliCatalog, String> {
        let deadline = Instant::now() + Duration::from_secs(AGENT_CLI_CONTROL_TIMEOUT_SECS);
        let query = AgentCliCatalogQuery {
            version: 1,
            target: exact_target.clone(),
            provider,
            deadline_at_ms: now_ms().saturating_add((AGENT_CLI_CONTROL_TIMEOUT_SECS * 1000) as i64),
        };
        let mut read = self
            .enqueue_agent_cli_read(
                access,
                RunnerOperation::AgentCliCatalog(query.clone()),
                deadline,
                requested_by,
            )
            .await?;
        let raw = read.response(AGENT_CLI_CATALOG_MAX_BYTES).await?;
        let catalog: AgentCliCatalog =
            serde_json::from_str(&raw).map_err(|_| "agent_cli_catalog_invalid_response")?;
        let inner = timeout_at(deadline, self.inner.lock())
            .await
            .map_err(|_| "agent_cli_query_timeout")?;
        validate_fence(&inner, &read.operation, &read.fence)?;
        catalog.validate_for(
            &query,
            read.fence
                .resource
                .as_ref()
                .ok_or("agent_cli_catalog_resource_required")?,
        )?;
        let now = now_ms();
        if catalog.observed_at_ms
            < query
                .deadline_at_ms
                .saturating_sub((AGENT_CLI_CONTROL_TIMEOUT_SECS * 1000) as i64)
            || catalog.observed_at_ms > now
            || catalog.expires_at_ms <= now
            || Instant::now() >= deadline
        {
            return Err("agent_cli_catalog_stale".into());
        }
        Ok(catalog)
    }

    /// C/B first verifies exact original-attempt/Task read authority. Registry
    /// additionally checks the original Job/owner partition/current instance,
    /// accepted whole close with this member TreeExited, and canonical bytes.
    pub async fn read_agent_cli_delivery_snapshot(
        &self,
        access: &RunnerAccess,
        binding: &AgentExecutionBinding,
        requested_by: String,
    ) -> Result<VerifiedAgentCliDeliverySnapshot, String> {
        let deadline = Instant::now() + Duration::from_secs(AGENT_CLI_CONTROL_TIMEOUT_SECS);
        let deadline_at_ms =
            now_ms().saturating_add((AGENT_CLI_CONTROL_TIMEOUT_SECS * 1000) as i64);
        let reader = {
            let inner = timeout_at(deadline, self.inner.lock())
                .await
                .map_err(|_| "agent_cli_query_timeout")?;
            verify_delivery_origin(&inner, access, binding)?;
            let runner = inner
                .runners
                .get(&binding.target.client_id)
                .ok_or("agent_cli_reader_unavailable")?;
            AgentExecutionTarget {
                client_id: binding.target.client_id.clone(),
                runner_instance_id: runner.runner_instance_id.clone(),
            }
        };
        let query = AgentCliDeliveryQuery {
            version: 1,
            target: reader,
            binding: binding.clone(),
            deadline_at_ms,
        };
        let read = self
            .enqueue_agent_cli_read(
                access,
                RunnerOperation::AgentCliDelivery(query),
                deadline,
                requested_by,
            )
            .await?;
        self.verified_delivery_response(read, binding).await
    }

    /// Narrow read-only Server-restart recovery. C/B must provide an actual
    /// current-authority Store read of the original terminal attempt and its
    /// accepted complete close; no permissive production adapter exists here.
    /// The validated proof lives only in this pending control read. Missing
    /// memory never creates a Job, preparation, dispatch permit or close ledger.
    pub async fn read_agent_cli_delivery_snapshot_recovered(
        &self,
        access: &RunnerAccess,
        binding: &AgentExecutionBinding,
        evidence: &dyn AgentCliDeliveryStoreEvidence,
        requested_by: String,
    ) -> Result<VerifiedAgentCliDeliverySnapshot, String> {
        let deadline = Instant::now() + Duration::from_secs(AGENT_CLI_CONTROL_TIMEOUT_SECS);
        let deadline_at_ms =
            now_ms().saturating_add((AGENT_CLI_CONTROL_TIMEOUT_SECS * 1000) as i64);
        let (reader, proof) = {
            let inner = timeout_at(deadline, self.inner.lock())
                .await
                .map_err(|_| "agent_cli_query_timeout")?;
            let proof = evidence.validate_closed_delivery(binding)?;
            verify_recovered_delivery_origin(&inner, access, binding, &proof)?;
            if Instant::now() >= deadline {
                return Err("agent_cli_query_expired".into());
            }
            let runner = inner
                .runners
                .get(&binding.target.client_id)
                .ok_or("agent_cli_reader_unavailable")?;
            let reader = AgentExecutionTarget {
                client_id: binding.target.client_id.clone(),
                runner_instance_id: runner.runner_instance_id.clone(),
            };
            current_target(&inner, access, &reader)?;
            (reader, Arc::new(proof))
        };
        let query = AgentCliDeliveryQuery {
            version: 1,
            target: reader,
            binding: binding.clone(),
            deadline_at_ms,
        };
        let read = self
            .enqueue_agent_cli_read_with_proof(
                access,
                RunnerOperation::AgentCliDelivery(query),
                deadline,
                requested_by,
                Some(proof),
            )
            .await?;
        self.verified_delivery_response(read, binding).await
    }

    async fn verified_delivery_response(
        &self,
        mut read: PendingRead,
        binding: &AgentExecutionBinding,
    ) -> Result<VerifiedAgentCliDeliverySnapshot, String> {
        let deadline = read.fence.deadline;
        let raw = read.response(DELIVERY_RESPONSE_MAX_BYTES).await?;
        let snapshot: AgentCliDeliverySnapshot =
            serde_json::from_str(&raw).map_err(|_| "agent_cli_delivery_invalid_response")?;
        let inner = timeout_at(deadline, self.inner.lock())
            .await
            .map_err(|_| "agent_cli_query_timeout")?;
        validate_fence(&inner, &read.operation, &read.fence)?;
        let bytes = snapshot.validate_for(binding)?;
        if Instant::now() >= deadline {
            return Err("agent_cli_query_expired".into());
        }
        Ok(VerifiedAgentCliDeliverySnapshot {
            binding: binding.clone(),
            artifact_bytes: bytes,
            artifact_sha256: snapshot.artifact_sha256,
        })
    }
}

#[cfg(test)]
#[path = "tests/agent_cli.rs"]
mod tests;
