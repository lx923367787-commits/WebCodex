//! Exact, retained Job admission for independent Agent executors.
//! No Store dependency: a trusted C/B adapter must provide the real durable
//! claim/gate check. Neither a serialized binding nor a Runner receipt grants
//! execution or proves durable close completion by itself.
use crate::access_control::{assert_runner_access, job_visible_to_access};
use crate::jobs::{assert_active_instance_locked, ensure_dispatch_supported_locked, job_view};
use crate::requests::{
    enqueue_agent_execution_request_locked, notify_runner_locked, remove_pending_request_locked,
};
use crate::state::{
    JobLifecycleState, JobObservationState, JobRecoveryState, RunnerRecord, RunnerRegistryInner,
    ShellJobRecord, ShellJobVisibility,
};
use crate::validation::{validate_id, validate_runner_instance_id};
use crate::{now_ts, RunnerAccess, RunnerFeature, RunnerRegistry, MAX_QUEUED_REQUESTS_PER_RUNNER};
use std::sync::Arc;
use webcodex_core::runner_operation::{
    RunnerInvocationMetadata, RunnerJobIsolatedProcessOperation, RunnerJobOperation,
    RunnerOperation,
};
use webcodex_core::runner_protocol::{
    isolated_job_context, AgentExecutionBinding, AgentExecutionCarrier, AgentExecutionClose,
    AgentExecutionCloseReceipt, AgentExecutionResourceDescriptor, AgentExecutionStart,
    AgentExecutionTarget, RunnerRequest, ShellJobInfo,
};

/// Trusted, non-model adapter implemented by C/B. It must synchronously re-read
/// the committed original claim, exact binding, requester/executor generation,
/// assignment and both admission gates. A fake implementation is test-only.
/// Called under the Registry mutex; implementations must not call Registry or
/// hold a Store transaction while awaiting Registry elsewhere.
pub trait AgentExecutionStoreAdmission: Send + Sync {
    fn validate_persisted(&self, binding: &AgentExecutionBinding) -> Result<(), String>;
}

/// Trusted C/B adapter, never a model assertion. It re-reads the original
/// preparation's exact slot, requester/executor/task/current generations and
/// open gates, and proves no finalized/full binding, claim, start or possibly
/// sent execution ever existed. Unknown states must return Err. The returned
/// original D1 identity has both launch/resource hashes empty. This check runs
/// under the Registry mutex and must not call Registry from a Store transaction.
pub trait AgentExecutionStorePreparationEvidence: Send + Sync {
    fn recoverable_preparation(&self) -> Result<AgentExecutionBinding, String>;
}

/// Only the checked admission/control code in this module can construct this
/// token. The shared queue must reject isolated operations without it.
pub(super) struct AgentExecutionEnqueuePermit(());

/// Preparation coordinates only, not a durable claim or execution permit. C/B
/// must freeze the winning IDs on the original attempt before resource hashing.
#[derive(Debug, Clone)]
pub struct AgentExecutionTargetReservation {
    epoch: Arc<str>,
    target: webcodex_core::runner_protocol::AgentExecutionTarget,
    job_id: String,
    start_request_id: String,
    recovered_identity_hash: Option<String>,
}
impl AgentExecutionTargetReservation {
    pub fn target(&self) -> &webcodex_core::runner_protocol::AgentExecutionTarget {
        &self.target
    }
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
    pub fn start_request_id(&self) -> &str {
        &self.start_request_id
    }
}

#[derive(Debug, Clone)]
pub struct PreparedAgentExecution {
    epoch: Arc<str>,
    binding: AgentExecutionBinding,
    resource_set_id: String,
}
impl PreparedAgentExecution {
    pub fn binding(&self) -> &AgentExecutionBinding {
        &self.binding
    }
}

#[derive(Debug)]
pub struct AgentExecutionDispatch {
    pub job: ShellJobInfo,
    pub dispatched: bool,
    pub replayed: bool,
}

#[derive(Debug)]
pub struct AgentExecutionCloseDispatch {
    pub request_id: String,
    pub enqueued: bool,
    /// A validated response still needs the trusted Store bridge to persist it.
    pub awaiting_store_persistence: bool,
}

/// Not serializable and not a ClosedAgentExecution authorization. Root may
/// expose its receipt only to the trusted Store bridge, then acknowledge the
/// Runner after that bridge's actual transaction commits.
#[derive(Debug, Clone)]
pub struct VerifiedAgentExecutionCloseReceipt {
    receipt: AgentExecutionCloseReceipt,
}
impl VerifiedAgentExecutionCloseReceipt {
    pub fn receipt(&self) -> &AgentExecutionCloseReceipt {
        &self.receipt
    }
}

#[derive(Debug, Clone)]
pub(super) struct AgentExecutionJobState {
    pub(super) frozen_resource: AgentExecutionResourceDescriptor,
    pub(super) binding: AgentExecutionBinding,
    pub(super) resource_set_id: Option<String>,
    pub(super) prepared_start: Option<AgentExecutionStart>,
    pub(super) requested_by: String,
    pub(super) enqueued: bool,
    pub(super) transport_dispatched: bool,
}

#[derive(Debug)]
pub(super) struct AgentExecutionCloseState {
    pub(super) close: AgentExecutionClose,
    pub(super) request_id: String,
    pub(super) receipt: Option<AgentExecutionCloseReceipt>,
    pub(super) dispatched: bool,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn job_identity(binding: &AgentExecutionBinding) -> Result<(&str, &str), String> {
    match &binding.carrier {
        AgentExecutionCarrier::Job {
            job_id,
            start_request_id,
        } => Ok((job_id, start_request_id)),
        _ => Err("agent_execution_job_required".into()),
    }
}

pub(super) fn exact_job<'a>(
    inner: &'a RunnerRegistryInner,
    access: &RunnerAccess,
    binding: &AgentExecutionBinding,
) -> Result<&'a ShellJobRecord, String> {
    binding.validate()?;
    let (job_id, request_id) = job_identity(binding)?;
    let job = inner
        .jobs_by_id
        .get(job_id)
        .ok_or("agent_execution_reconciliation_required")?;
    if !job_visible_to_access(Some(access), inner, job) {
        return Err("agent_execution_observation_denied".into());
    }
    if job
        .agent_execution
        .as_ref()
        .is_none_or(|state| &state.binding != binding)
        || job.request_id.as_deref() != Some(request_id)
        || job.client_id != binding.target.client_id
        || job.runner_instance_id != binding.target.runner_instance_id
    {
        return Err("agent_execution_observation_mismatch".into());
    }
    Ok(job)
}

fn require_target(
    inner: &RunnerRegistryInner,
    access: &RunnerAccess,
    binding: &webcodex_core::runner_protocol::AgentExecutionTarget,
) -> Result<(), String> {
    let runner = current_target(inner, access, binding)?;
    require_execution_capabilities(runner)?;
    ensure_dispatch_supported_locked(inner, &binding.client_id).map_err(String::from)
}

pub(super) fn current_target<'a>(
    inner: &'a RunnerRegistryInner,
    access: &RunnerAccess,
    binding: &AgentExecutionTarget,
) -> Result<&'a RunnerRecord, String> {
    validate_id(&binding.client_id, "client_id")?;
    validate_runner_instance_id(&binding.runner_instance_id)?;
    let runner = inner
        .runners
        .get(&binding.client_id)
        .ok_or("agent_execution_runner_unavailable")?;
    assert_runner_access(Some(access), runner)?;
    if runner.runner_instance_id != binding.runner_instance_id {
        return Err("agent_execution_runner_replaced".into());
    }
    Ok(runner)
}

pub(super) fn require_execution_capabilities(runner: &RunnerRecord) -> Result<(), String> {
    for capability in [
        RunnerFeature::AgentIsolatedExecutionV1,
        RunnerFeature::StructuredExecutionJobs,
        RunnerFeature::StructuredProcessArgv,
        RunnerFeature::JobStateReconciliation,
    ] {
        if !runner.runner_features.supports(capability) {
            return Err("agent_isolated_execution_unavailable".into());
        }
    }
    Ok(())
}

fn require_resource(runner: &RunnerRecord, start: &AgentExecutionStart) -> Result<(), String> {
    let resource = runner
        .agent_execution_resources
        .iter()
        .find(|resource| resource.resource_set_id == start.resource_set_id)
        .ok_or("agent_execution_resource_unavailable")?;
    resource.validate()?;
    if resource.manifest_hash != start.binding.resource_manifest_hash
        || resource.executable_identity != start.process.executable
    {
        return Err("agent_execution_resource_changed".into());
    }
    match (&resource.provider, &resource.reviewed_controls) {
        (Some(_), None) => return Err("agent_cli_reviewed_controls_required".into()),
        (_, Some(controls)) => webcodex_core::agent_cli_controls::validate_reviewed_cli_argv(
            controls,
            &start.process.args,
        )?,
        (None, None) => {}
    }
    Ok(())
}

pub(super) fn binding_closed(inner: &RunnerRegistryInner, binding: &AgentExecutionBinding) -> bool {
    inner.agent_execution_closes.values().any(|state| {
        state.close.close_ref.source == binding.requester.run
            || state.close.close_ref.source == binding.executor.run
            || state
                .close
                .members
                .iter()
                .any(|member| member.executor.run == binding.executor.run)
    })
}

impl RunnerRegistry {
    /// Recover original, never-finalized preparation coordinates using actual
    /// Store evidence, including after Server restart. Empty Registry state is
    /// never that evidence. No new IDs, Job, queue entry or dispatch authority
    /// are created. Existing prepared Jobs use read_prepared_agent_execution.
    pub async fn recover_agent_execution_target_reservation(
        &self,
        access: &RunnerAccess,
        evidence: &dyn AgentExecutionStorePreparationEvidence,
    ) -> Result<AgentExecutionTargetReservation, String> {
        let inner = self.inner.lock().await;
        let original = evidence.recoverable_preparation()?;
        let identity_hash = original.identity_hash()?;
        if !original.launch_intent_hash.is_empty() || !original.resource_manifest_hash.is_empty() {
            return Err("agent_execution_preparation_already_finalized".into());
        }
        let (job_id, request_id) = job_identity(&original)?;
        validate_id(job_id, "job_id")?;
        validate_id(request_id, "request_id")?;
        require_target(&inner, access, &original.target)?;
        if binding_closed(&inner, &original) {
            return Err("agent_execution_closed".into());
        }
        let collision = inner.jobs_by_id.contains_key(job_id)
            || inner.request_to_job.contains_key(request_id)
            || inner.pending_by_id.contains_key(request_id)
            || inner.agent_execution_closes.contains_key(request_id)
            || inner.queues_by_runner.values().any(|queue| queue.iter().any(|id| id == request_id))
            || inner.jobs_by_id.values().any(|job| job.request_id.as_deref() == Some(request_id)
                || job.agent_execution.as_ref().is_some_and(|state| state.binding.requester.cli_attempt_id == original.requester.cli_attempt_id))
            || inner.pending_by_id.values().any(|pending| {
                pending.job_id.as_deref() == Some(job_id)
                    || matches!(&pending.operation, RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(start))
                        if start.start.binding.requester.cli_attempt_id == original.requester.cli_attempt_id)
            });
        if collision {
            return Err("agent_execution_preparation_conflict".into());
        }
        Ok(AgentExecutionTargetReservation {
            epoch: self.observation_epoch.clone(),
            target: original.target.clone(),
            job_id: job_id.into(),
            start_request_id: request_id.into(),
            recovered_identity_hash: Some(identity_hash),
        })
    }
    /// Read one exact-instance registered fact. This contains no private host
    /// path and grants no task input, output, execution, or resource ownership.
    pub async fn read_agent_execution_resource(
        &self,
        access: &RunnerAccess,
        target: &AgentExecutionTarget,
        resource_set_id: &str,
    ) -> Result<AgentExecutionResourceDescriptor, String> {
        let inner = self.inner.lock().await;
        let runner = current_target(&inner, access, target)?;
        runner
            .agent_execution_resources
            .iter()
            .find(|resource| resource.resource_set_id == resource_set_id)
            .cloned()
            .ok_or_else(|| "agent_execution_resource_unavailable".into())
    }
    pub async fn list_agent_execution_resources(
        &self, access: &RunnerAccess, target: &AgentExecutionTarget,
    ) -> Result<Vec<AgentExecutionResourceDescriptor>, String> {
        let inner = self.inner.lock().await;
        let runner = current_target(&inner, access, target)?;
        Ok(runner.agent_execution_resources.clone())
    }
    /// Exact observation only. C/B must first authorize this caller to read the
    /// original attempt. Closed execution gates do not prevent observation;
    /// missing Registry state never authorizes reconstruction or another start.
    pub async fn observe_agent_execution(
        &self,
        access: &RunnerAccess,
        binding: &AgentExecutionBinding,
    ) -> Result<ShellJobInfo, String> {
        let inner = self.inner.lock().await;
        Ok(job_view(exact_job(&inner, access, binding)?))
    }

    /// Recover only this Registry's original prepared handle after an HTTP
    /// stack was dropped. C/B must first authorize reading the original attempt.
    /// This never reconstructs a missing Job, changes IDs, or grants dispatch:
    /// dispatch still revalidates the real persisted claim and both gates.
    /// An already-enqueued original returns a handle whose dispatch is only an
    /// observation replay, preserving the response-loss case.
    pub async fn read_prepared_agent_execution(
        &self,
        access: &RunnerAccess,
        binding: &AgentExecutionBinding,
    ) -> Result<PreparedAgentExecution, String> {
        let inner = self.inner.lock().await;
        let job = exact_job(&inner, access, binding)?;
        if job.observation.epoch != self.observation_epoch {
            return Err("agent_execution_reconciliation_required".into());
        }
        let state = job.agent_execution.as_ref().expect("exact isolated Job");
        if !state.enqueued && state.prepared_start.is_none() {
            return Err("agent_execution_reconciliation_required".into());
        }
        let resource_set_id = state
            .resource_set_id
            .clone()
            .ok_or("agent_execution_reconciliation_required")?;
        Ok(PreparedAgentExecution {
            epoch: self.observation_epoch.clone(),
            binding: state.binding.clone(),
            resource_set_id,
        })
    }
    /// Allocate exact original target/IDs without a queue entry or Job. A
    /// repeated allocation is inert; it cannot replace an already frozen claim.
    /// C/B retains this handle until prepare succeeds. If lost, only the trusted
    /// recoverable-preparation callback may restore these original IDs; an empty
    /// Registry is not NeverCreated proof. Finalized/claimed/uncertain records
    /// cannot take that recovery path or replace their original IDs.
    pub async fn reserve_agent_execution_target(
        &self,
        access: &RunnerAccess,
        client_id: &str,
    ) -> Result<AgentExecutionTargetReservation, String> {
        let inner = self.inner.lock().await;
        let runner = inner
            .runners
            .get(client_id)
            .ok_or("agent_execution_runner_unavailable")?;
        let target = webcodex_core::runner_protocol::AgentExecutionTarget {
            client_id: client_id.into(),
            runner_instance_id: runner.runner_instance_id.clone(),
        };
        require_target(&inner, access, &target)?;
        Ok(AgentExecutionTargetReservation {
            epoch: self.observation_epoch.clone(),
            target,
            job_id: uuid::Uuid::new_v4().to_string(),
            start_request_id: uuid::Uuid::new_v4().to_string(),
            recovered_identity_hash: None,
        })
    }
    /// Prepare one exact server-selected Job without enqueueing anything. C/B
    /// supplies fixed IDs before resource/SID hashing; this method never retargets.
    pub async fn prepare_agent_execution(
        &self,
        access: &RunnerAccess,
        reservation: &AgentExecutionTargetReservation,
        start: AgentExecutionStart,
        requested_by: String,
    ) -> Result<PreparedAgentExecution, String> {
        start.validate()?;
        let (job_id, request_id) = job_identity(&start.binding)?;
        if reservation.epoch != self.observation_epoch
            || reservation.target != start.binding.target
            || reservation.job_id != job_id
            || reservation.start_request_id != request_id
        {
            return Err("agent_execution_reservation_mismatch".into());
        }
        if let Some(expected) = reservation.recovered_identity_hash.as_ref() {
            if &start.binding.identity_hash()? != expected {
                return Err("agent_execution_preparation_identity_mismatch".into());
            }
        }
        validate_id(job_id, "job_id")?;
        validate_id(request_id, "request_id")?;
        let mut inner = self.inner.lock().await;
        require_target(&inner, access, &start.binding.target)?;
        require_resource(&inner.runners[&start.binding.target.client_id], &start)?;
        if binding_closed(&inner, &start.binding) {
            return Err("agent_execution_closed".into());
        }
        let prepared = PreparedAgentExecution {
            epoch: self.observation_epoch.clone(),
            binding: start.binding.clone(),
            resource_set_id: start.resource_set_id.clone(),
        };
        if let Some(existing) = inner.jobs_by_id.values().find(|job| {
            job.agent_execution.as_ref().is_some_and(|state| {
                state.binding.requester.cli_attempt_id == start.binding.requester.cli_attempt_id
            })
        }) {
            let state = existing
                .agent_execution
                .as_ref()
                .expect("matched isolated Job");
            if state.binding != start.binding
                || state.resource_set_id.as_deref() != Some(start.resource_set_id.as_str())
            {
                return Err("agent_execution_intent_conflict".into());
            }
            if state.prepared_start.is_none() && !state.enqueued {
                return Err("agent_execution_reconciliation_required".into());
            }
            return Ok(prepared);
        }
        if inner.jobs_by_id.contains_key(job_id)
            || inner.request_to_job.contains_key(request_id)
            || inner.pending_by_id.contains_key(request_id)
        {
            return Err("agent_execution_identity_conflict".into());
        }
        if inner
            .jobs_by_id
            .values()
            .filter(|job| {
                job.client_id == start.binding.target.client_id && job.agent_execution.is_some()
            })
            .count()
            >= MAX_QUEUED_REQUESTS_PER_RUNNER
        {
            return Err("agent_execution_capacity_exceeded".into());
        }
        let context = isolated_job_context(&start);
        let job = ShellJobRecord {
            agent_execution: Some(AgentExecutionJobState {
                frozen_resource: inner.runners[&start.binding.target.client_id]
                    .agent_execution_resources
                    .iter()
                    .find(|resource| resource.resource_set_id == start.resource_set_id)
                    .expect("validated resource")
                    .clone(),
                binding: start.binding.clone(),
                resource_set_id: Some(start.resource_set_id.clone()),
                prepared_start: Some(start.clone()),
                requested_by,
                enqueued: false,
                transport_dispatched: false,
            }),
            document: None,
            job_id: job_id.into(),
            request_id: Some(request_id.into()),
            client_id: start.binding.target.client_id.clone(),
            auth_group: inner.runners[&start.binding.target.client_id]
                .auth_group
                .clone(),
            runner_instance_id: start.binding.target.runner_instance_id.clone(),
            kind: "agent_isolated_execution_v1".into(),
            project_id: None,
            session_id: None,
            ssh_resource: None,
            cwd: None,
            project_cwd: None,
            purpose: context.purpose,
            shell: context.shell,
            command_preview: context.command_preview,
            detached_idempotency_intent: None,
            lifecycle: JobLifecycleState::Queued,
            created_at: now_ts(),
            started_at: None,
            ended_at: None,
            exit_code: None,
            duration_ms: None,
            stdout: Default::default(),
            stderr: Default::default(),
            error: None,
            command_execution_state: None,
            structured_execution: context.structured_execution,
            codex: None,
            validation_steps: Vec::new(),
            validation: None,
            validation_progress: None,
            activity: None,
            visibility: ShellJobVisibility::HiddenUntilHandoff,
            last_update_seq: 0,
            recovery: JobRecoveryState::default(),
            observation: JobObservationState::new(self.observation_epoch.clone()),
        };
        inner
            .request_to_job
            .insert(request_id.into(), job_id.into());
        inner.jobs_by_id.insert(job_id.into(), job);
        Ok(prepared)
    }

    /// Dispatch once, only after the real Store bridge validates its committed
    /// unique claim and both gates. A replay never calls the bridge or enqueues.
    pub async fn dispatch_prepared_agent_execution(
        &self,
        access: &RunnerAccess,
        prepared: &PreparedAgentExecution,
        admission: &dyn AgentExecutionStoreAdmission,
    ) -> Result<AgentExecutionDispatch, String> {
        if prepared.epoch != self.observation_epoch {
            return Err("agent_execution_reconciliation_required".into());
        }
        let (job_id, request_id) = job_identity(&prepared.binding)?;
        let mut inner = self.inner.lock().await;
        let job = inner
            .jobs_by_id
            .get(job_id)
            .ok_or("agent_execution_reconciliation_required")?;
        let state = job
            .agent_execution
            .as_ref()
            .ok_or("agent_execution_identity_conflict")?;
        if state.binding != prepared.binding
            || state.resource_set_id.as_deref() != Some(prepared.resource_set_id.as_str())
        {
            return Err("agent_execution_intent_conflict".into());
        }
        // Access is checked on every observation, including idempotent replay.
        let runner = inner
            .runners
            .get(&prepared.binding.target.client_id)
            .ok_or("agent_execution_runner_unavailable")?;
        assert_runner_access(Some(access), runner)?;
        if state.enqueued {
            return Ok(AgentExecutionDispatch {
                job: job_view(job),
                dispatched: false,
                replayed: true,
            });
        }
        require_target(&inner, access, &prepared.binding.target)?;
        if binding_closed(&inner, &prepared.binding) {
            return Err("agent_execution_closed".into());
        }
        if job.lifecycle != JobLifecycleState::Queued {
            return Err("agent_execution_reconciliation_required".into());
        }
        let start = state
            .prepared_start
            .clone()
            .ok_or("agent_execution_reconciliation_required")?;
        require_resource(runner, &start)?;
        if !runner
            .agent_execution_resources
            .contains(&state.frozen_resource)
        {
            return Err("agent_execution_resource_changed".into());
        }
        let metadata = RunnerInvocationMetadata {
            request_id: request_id.into(),
            client_id: prepared.binding.target.client_id.clone(),
            requested_by: state.requested_by.clone(),
            created_at: job.created_at,
        };
        let request = RunnerRequest::from_operation(
            metadata,
            RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(
                RunnerJobIsolatedProcessOperation {
                    context: isolated_job_context(&start),
                    start,
                },
            )),
        )?;
        admission.validate_persisted(&prepared.binding)?;
        enqueue_agent_execution_request_locked(
            self.telemetry.as_ref(),
            &mut inner,
            &prepared.binding.target.client_id,
            request_id.into(),
            request,
            None,
            Some(job_id.into()),
            &AgentExecutionEnqueuePermit(()),
        )?;
        let job = inner
            .jobs_by_id
            .get_mut(job_id)
            .expect("prepared Job remains locked");
        let state = job.agent_execution.as_mut().expect("prepared isolated Job");
        state.enqueued = true;
        state.prepared_start = None;
        let output = AgentExecutionDispatch {
            job: job_view(job),
            dispatched: true,
            replayed: false,
        };
        notify_runner_locked(&inner, &prepared.binding.target.client_id);
        Ok(output)
    }

    /// Repeated Close may resend this control request only. It never replays an
    /// execution. A bounded transient correlation record is not durable proof.
    pub async fn enqueue_agent_execution_close(
        &self,
        access: &RunnerAccess,
        close: AgentExecutionClose,
        request_id: String,
        requested_by: String,
    ) -> Result<AgentExecutionCloseDispatch, String> {
        close.validate()?;
        validate_id(&request_id, "request_id")?;
        let mut inner = self.inner.lock().await;
        // Cleanup must remain available after admission capability/resources
        // are withdrawn. It still targets the exact authenticated old instance.
        current_target(&inner, access, &close.target)?;
        ensure_dispatch_supported_locked(&inner, &close.target.client_id).map_err(String::from)?;
        if let Some(existing) = inner.agent_execution_closes.get(&request_id) {
            if existing.close != close {
                return Err("agent_execution_close_conflict".into());
            }
            if existing.receipt.is_some() {
                return Ok(AgentExecutionCloseDispatch {
                    request_id,
                    enqueued: false,
                    awaiting_store_persistence: true,
                });
            }
        } else {
            if inner.agent_execution_closes.values().any(|state| {
                state.close.close_ref.close_id == close.close_ref.close_id
                    && state.close.target == close.target
            }) || inner.pending_by_id.contains_key(&request_id)
                || inner.request_to_job.contains_key(&request_id)
            {
                return Err("agent_execution_close_conflict".into());
            }
            if inner.agent_execution_closes.len() >= MAX_QUEUED_REQUESTS_PER_RUNNER {
                return Err("agent_execution_capacity_exceeded".into());
            }
            for member in &close.members {
                if let AgentExecutionCarrier::Job { job_id, .. } = &member.carrier {
                    if let Some(job) = inner.jobs_by_id.get(job_id) {
                        if job
                            .agent_execution
                            .as_ref()
                            .is_none_or(|state| state.binding != *member)
                        {
                            return Err("agent_execution_close_binding_mismatch".into());
                        }
                    }
                }
            }
            for state in inner
                .jobs_by_id
                .values()
                .filter_map(|job| job.agent_execution.as_ref())
            {
                let in_scope = close.close_ref.source == state.binding.requester.run
                    || close.close_ref.source == state.binding.executor.run
                    || close
                        .members
                        .iter()
                        .any(|member| member.executor.run == state.binding.executor.run);
                if in_scope
                    && state.binding.target == close.target
                    && !close.members.contains(&state.binding)
                {
                    return Err("agent_execution_close_incomplete_local_set".into());
                }
            }
            inner.agent_execution_closes.insert(
                request_id.clone(),
                AgentExecutionCloseState {
                    close: close.clone(),
                    request_id: request_id.clone(),
                    receipt: None,
                    dispatched: false,
                },
            );
        }
        // Preserve the original deadline even when sending late best-effort
        // cleanup. A newly arriving expired receipt is still rejected below.
        let cancelled: Vec<_> = inner
            .pending_by_id
            .iter()
            .filter_map(|(id, pending)| match &pending.operation {
                RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(operation))
                    if !pending.dispatched && binding_closed(&inner, &operation.start.binding) =>
                {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect();
        for id in cancelled {
            remove_pending_request_locked(&mut inner, &id);
        }
        if inner
            .queues_by_runner
            .get(&close.target.client_id)
            .is_some_and(|queue| queue.contains(&request_id))
        {
            return Ok(AgentExecutionCloseDispatch {
                request_id,
                enqueued: false,
                awaiting_store_persistence: false,
            });
        }
        remove_pending_request_locked(&mut inner, &request_id);
        let request = RunnerRequest::from_operation(
            RunnerInvocationMetadata {
                request_id: request_id.clone(),
                client_id: close.target.client_id.clone(),
                requested_by,
                created_at: now_ts(),
            },
            RunnerOperation::AgentExecutionClose(close.clone()),
        )?;
        enqueue_agent_execution_request_locked(
            self.telemetry.as_ref(),
            &mut inner,
            &close.target.client_id,
            request_id.clone(),
            request,
            None,
            None,
            &AgentExecutionEnqueuePermit(()),
        )?;
        notify_runner_locked(&inner, &close.target.client_id);
        Ok(AgentExecutionCloseDispatch {
            request_id,
            enqueued: true,
            awaiting_store_persistence: false,
        })
    }

    /// Called only after Root's transport authentication. This validates exact
    /// current connection/instance and the original request. Success returns a
    /// pending Store-persistence value, never a durable ACK or execution grant.
    pub async fn accept_agent_execution_close_receipt(
        &self,
        client_id: &str,
        runner_instance_id: &str,
        connection_id: Option<&str>,
        request_id: &str,
        receipt: AgentExecutionCloseReceipt,
    ) -> Result<VerifiedAgentExecutionCloseReceipt, String> {
        let mut inner = self.inner.lock().await;
        assert_active_instance_locked(&inner, client_id, runner_instance_id)?;
        if let Some(connection) = connection_id {
            if inner
                .runners
                .get(client_id)
                .and_then(|runner| runner.connection_id.as_deref())
                != Some(connection)
            {
                return Err("agent_execution_stale_connection".into());
            }
        }
        let state = inner
            .agent_execution_closes
            .get_mut(request_id)
            .ok_or("agent_execution_close_unknown")?;
        if state.close.target.client_id != client_id
            || state.close.target.runner_instance_id != runner_instance_id
            || state.request_id != request_id
            || !state.dispatched
        {
            return Err("agent_execution_close_target_mismatch".into());
        }
        receipt.validate_for(&state.close)?;
        if let Some(existing) = &state.receipt {
            if existing != &receipt {
                return Err("agent_execution_close_receipt_conflict".into());
            }
            return Ok(VerifiedAgentExecutionCloseReceipt { receipt });
        }
        if now_ms() >= state.close.deadline_at_ms {
            return Err("agent_execution_close_expired".into());
        }
        state.receipt = Some(receipt.clone());
        Ok(VerifiedAgentExecutionCloseReceipt { receipt })
    }
}

/// Same mutex as dequeue. Exact instance and closure checks cannot be skipped
/// by the delay between the Store commit and actual transport delivery.
pub(super) fn dispatch_fence(
    inner: &RunnerRegistryInner,
    operation: &RunnerOperation,
) -> Result<(), String> {
    let (target, binding) = match operation {
        RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(start)) => {
            (&start.start.binding.target, Some(&start.start.binding))
        }
        RunnerOperation::AgentExecutionClose(close) => (&close.target, None),
        _ => return Ok(()),
    };
    let runner = inner
        .runners
        .get(&target.client_id)
        .ok_or("agent_execution_runner_unavailable")?;
    if runner.runner_instance_id != target.runner_instance_id {
        return Err("agent_execution_runner_replaced".into());
    }
    if let RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(start)) = operation {
        require_execution_capabilities(runner)?;
        require_resource(runner, &start.start)?;
        let (job_id, _) = job_identity(&start.start.binding)?;
        let frozen = inner
            .jobs_by_id
            .get(job_id)
            .and_then(|job| job.agent_execution.as_ref())
            .ok_or("agent_execution_reconciliation_required")?;
        if !runner
            .agent_execution_resources
            .contains(&frozen.frozen_resource)
        {
            return Err("agent_execution_resource_changed".into());
        }
    }
    if binding.is_some_and(|binding| binding_closed(inner, binding)) {
        return Err("agent_execution_closed".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/agent_execution.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "tests/agent_reviewed_controls.rs"]
mod reviewed_controls_tests;
