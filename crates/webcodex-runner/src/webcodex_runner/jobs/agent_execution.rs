//! Isolated Job admission, native child custody and durable run closure.
//! This is part of the original JobManager queue, not a second task scheduler.
use super::agent_resources::{PreparedAgentResources, ValidatedResourceProfiles};
use super::child::JobChild;
use super::{JobManager, PendingJobStart, RunnerJobDelta};
use crate::webcodex_runner::detached_job::{
    AgentExecutionCloseMetadata, AgentExecutionJobMetadata, AgentExecutionMetadata,
    AgentExecutionResourceCleanupMetadata, DetachedJobStore,
};
use crate::webcodex_runner::shutdown::lock_unpoison;
use crate::webcodex_runner::RunnerSink;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use webcodex_core::agent_cli::{
    AgentCliCatalog, AgentCliCatalogQuery, AgentCliDeliveryQuery, AgentCliDeliverySnapshot,
};
use webcodex_core::runner_operation::RunnerJobOperation;
use webcodex_core::runner_protocol::{
    AgentExecutionBinding, AgentExecutionCarrier, AgentExecutionClose, AgentExecutionCloseReceipt,
    AgentExecutionStart, AgentExecutionStopOutcome, AgentExecutionStopReceipt,
    ShellCommandExecutionState,
};

const RESOURCE_CLEANUP_BUDGET: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct AgentExecutionConfiguration {
    profiles: ValidatedResourceProfiles,
    store: DetachedJobStore,
    resource_root: PathBuf,
    _root_pins: Arc<Vec<std::fs::File>>,
}

#[cfg(test)]
#[path = "agent_execution_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "agent_execution_real_tests.rs"]
mod real_tests;

/// Volatile custody only. All admission/attempt/close identities live in the
/// existing Job store. A restart never reconstructs a process handle from PID.
#[derive(Debug, Default)]
pub(crate) struct AgentExecutionRuntime {
    configuration: Option<AgentExecutionConfiguration>,
    live: HashMap<String, IsolatedJobCustody>,
}

#[derive(Debug, Default)]
struct IsolatedJobCustody {
    child: Option<Arc<Mutex<JobChild>>>,
    resources: Option<PreparedAgentResources>,
    worker: Option<JoinHandle<()>>,
    // Set only after stdin/output helpers were joined, or before any native
    // creation. The JoinHandle must independently prove this worker finished.
    helpers_finished: bool,
    worker_joined: bool,
    observation_failed: bool,
}

fn job_id(binding: &AgentExecutionBinding) -> Result<&str, String> {
    match &binding.carrier {
        AgentExecutionCarrier::Job { job_id, .. } => Ok(job_id),
        AgentExecutionCarrier::CodingAgent { .. } => {
            Err("agent_execution_carrier_unavailable".into())
        }
    }
}

fn closed_by(close: &AgentExecutionClose, binding: &AgentExecutionBinding) -> bool {
    close.close_ref.source == binding.requester.run
        || close.close_ref.source == binding.executor.run
        || close.members.iter().any(|member| {
            member.executor.run == binding.executor.run
                || execution_identity_collides(member, binding)
        })
}

fn closed_for(metadata: &AgentExecutionMetadata, binding: &AgentExecutionBinding) -> bool {
    metadata
        .closes
        .iter()
        .any(|state| closed_by(&state.close, binding))
}

fn execution_identity_collides(
    left: &AgentExecutionBinding,
    right: &AgentExecutionBinding,
) -> bool {
    if left.requester.cli_attempt_id == right.requester.cli_attempt_id {
        return true;
    }
    match (&left.carrier, &right.carrier) {
        (
            AgentExecutionCarrier::Job {
                job_id: left_job,
                start_request_id: left_request,
            },
            AgentExecutionCarrier::Job {
                job_id: right_job,
                start_request_id: right_request,
            },
        ) => left_job == right_job || left_request == right_request,
        _ => left.carrier == right.carrier,
    }
}

impl JobManager {
    pub(crate) fn agent_cli_catalog(
        &self,
        sink: &RunnerSink,
        query: &AgentCliCatalogQuery,
    ) -> Result<AgentCliCatalog, String> {
        query.validate()?;
        crate::webcodex_runner::agent_cli::validate_target(
            sink,
            &query.target.client_id,
            &query.target.runner_instance_id,
        )?;
        let deadline =
            crate::webcodex_runner::agent_cli::ControlDeadline::new(query.deadline_at_ms)?;
        deadline.require(Duration::from_secs(33))?;
        let configuration = lock_unpoison(&self.agent_execution)
            .configuration
            .clone()
            .ok_or("agent_execution_unavailable")?;
        let pinned = super::agent_resources::pin_catalog_executable(
            &configuration.profiles,
            query.provider,
        )?;
        deadline.require(Duration::from_secs(33))?;
        // `pinned` stays alive through the fixed helper and process cleanup.
        crate::webcodex_runner::agent_cli::query_catalog(
            query,
            &pinned.descriptor,
            &pinned.program,
            pinned.entrypoint.as_deref(),
            pinned.codebuddy_config_directory.as_deref(),
            &deadline,
        )
    }

    pub(crate) fn agent_cli_delivery_snapshot(
        &self,
        sink: &RunnerSink,
        query: &AgentCliDeliveryQuery,
    ) -> Result<AgentCliDeliverySnapshot, String> {
        query.validate()?;
        crate::webcodex_runner::agent_cli::validate_target(
            sink,
            &query.target.client_id,
            &query.target.runner_instance_id,
        )?;
        let deadline =
            crate::webcodex_runner::agent_cli::ControlDeadline::new(query.deadline_at_ms)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let configuration = lock_unpoison(&self.agent_execution)
            .configuration
            .clone()
            .ok_or("agent_execution_unavailable")?;
        let before = configuration
            .store
            .read_agent_execution_metadata()
            .map_err(|_| "agent_cli_delivery_metadata_unavailable")?;
        let receipt = delivery_close_receipt(&before, &query.binding)?;
        deadline.require(Duration::ZERO)?;
        let bundle = super::agent_resources::read_delivery_bundle(
            &query.binding,
            &configuration.resource_root,
        )?;
        let bytes = bundle.canonical_bytes(&query.binding)?;
        let after = configuration
            .store
            .read_agent_execution_metadata()
            .map_err(|_| "agent_cli_delivery_metadata_unavailable")?;
        if delivery_close_receipt(&after, &query.binding)? != receipt {
            return Err("agent_cli_delivery_close_proof_changed".into());
        }
        let snapshot = AgentCliDeliverySnapshot {
            version: 1,
            binding: query.binding.clone(),
            artifact_sha256: webcodex_core::agent_cli::sha256(&bytes),
            bundle,
        };
        snapshot.validate_for(&query.binding)?;
        deadline.require(Duration::ZERO)?;
        Ok(snapshot)
    }

    pub(crate) fn configure_agent_execution_resources(
        &self,
        profiles: ValidatedResourceProfiles,
        durable_root: PathBuf,
    ) -> Result<(), String> {
        if !cfg!(windows) || profiles.is_empty() || !durable_root.is_absolute() {
            return Err("agent_execution_resources_unavailable".into());
        }
        profiles.verify_sources()?;
        if profiles.requires_proxy_controller() {
            super::agent_proxy::require_controller_privilege()?;
        }
        let name = durable_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("agent_execution_state_root_invalid")?;
        let resource_root = durable_root.with_file_name(format!("{name}-agent-resources"));
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let mut runtime = lock_unpoison(&self.agent_execution);
        if runtime.configuration.is_some() || !runtime.live.is_empty() {
            return Err("agent_execution_configuration_restart_required".into());
        }
        let store = DetachedJobStore::new(durable_root);
        match std::fs::symlink_metadata(&resource_root) {
            Ok(_) => {
                store.read_agent_execution_metadata()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                store.initialize_agent_execution_metadata()?;
            }
            Err(_) => return Err("agent_execution_resource_root_unavailable".into()),
        }
        let root_pins = super::agent_resources::initialize_resource_root(&resource_root)?;
        let configuration = AgentExecutionConfiguration {
            profiles,
            store,
            resource_root,
            _root_pins: Arc::new(root_pins),
        };
        reconcile_agent_resource_cleanup(&configuration)?;
        runtime.configuration = Some(configuration);
        Ok(())
    }

    /// Reopen the same trusted durable namespace only for historical readout.
    /// It does not initialize metadata, create paths, re-enable execution, or
    /// require today's provider binary to match a completed run's old profile.
    pub(crate) fn configure_agent_delivery_history(
        &self,
        durable_root: PathBuf,
    ) -> Result<(), String> {
        if !cfg!(windows) || !durable_root.is_absolute() {
            return Err("agent_execution_history_unavailable".into());
        }
        let name = durable_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("agent_execution_state_root_invalid")?;
        let resource_root = durable_root.with_file_name(format!("{name}-agent-resources"));
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let mut runtime = lock_unpoison(&self.agent_execution);
        if runtime.configuration.is_some() || !runtime.live.is_empty() {
            return Err("agent_execution_configuration_restart_required".into());
        }
        let store = DetachedJobStore::new(durable_root);
        store.read_agent_execution_metadata()?;
        let pins = super::agent_resources::pin_existing_resource_root(&resource_root)?;
        let profiles = super::agent_resources::validate_profiles(
            &super::agent_resources::AgentExecutionResourcesConfig::default(),
        )?;
        let configuration = AgentExecutionConfiguration {
            profiles,
            store,
            resource_root,
            _root_pins: Arc::new(pins),
        };
        reconcile_agent_resource_cleanup(&configuration)?;
        runtime.configuration = Some(configuration);
        Ok(())
    }

    pub(crate) fn agent_execution_ready(&self) -> bool {
        // Component capability, not a promise that every native spawn succeeds.
        // Each spawn still verifies the real token, exact resources and Job.
        cfg!(windows)
            && lock_unpoison(&self.agent_execution)
                .configuration
                .as_ref()
                .is_some_and(|configuration| {
                    !configuration.profiles.is_empty()
                        && configuration.store.read_agent_execution_metadata().is_ok()
                })
    }

    /// Called under JobManager.lifecycle, before entering the original queue.
    pub(crate) fn admit_agent_execution_locked(
        &self,
        start: &AgentExecutionStart,
        sink: &RunnerSink,
    ) -> Result<(), String> {
        start.validate()?;
        if start.binding.target.client_id != sink.client_id()
            || start.binding.target.runner_instance_id != sink.runner_instance_id()
        {
            return Err("agent_execution_target_mismatch".into());
        }
        let runtime = lock_unpoison(&self.agent_execution);
        let configuration = runtime
            .configuration
            .as_ref()
            .ok_or("agent_execution_unavailable")?;
        if configuration.profiles.is_empty() {
            return Err("agent_execution_unavailable".into());
        }
        if configuration.profiles.manifest(&start.resource_set_id)
            != Some(start.binding.resource_manifest_hash.as_str())
        {
            return Err("agent_execution_resource_manifest_mismatch".into());
        }
        configuration.profiles.validate_execution(start)?;
        let (auth_copy_expected, proxy_requested) =
            configuration.profiles.cleanup_requirements(start)?;
        configuration
            .store
            .update_agent_execution_metadata(|metadata| {
                if closed_for(metadata, &start.binding) {
                    return Err("agent_execution_admission_closed".into());
                }
                if metadata.jobs.iter().any(|existing| {
                    existing.binding.requester.cli_attempt_id
                        == start.binding.requester.cli_attempt_id
                        || existing.binding.carrier == start.binding.carrier
                }) {
                    // Queue replay is handled by the existing live Job record.
                    // If it is absent (including restart), never replay creation.
                    return Err("agent_execution_already_recorded_reobserve".into());
                }
                metadata.jobs.push(AgentExecutionJobMetadata {
                    binding: start.binding.clone(),
                    resource_set_id: start.resource_set_id.clone(),
                    native_create_attempted: false,
                    worker_finished: false,
                    stop_receipt: None,
                    resource_cleanup: Some(AgentExecutionResourceCleanupMetadata {
                        auth_copy_expected,
                        proxy_requested,
                        ..Default::default()
                    }),
                });
                Ok(())
            })
    }

    pub(crate) fn start_isolated_process_job(&self, queued: PendingJobStart) {
        let RunnerJobOperation::StartIsolatedProcess(operation) = &queued.operation else {
            return;
        };
        let start = operation.start.clone();
        let id = match job_id(&start.binding) {
            Ok(id) => id.to_owned(),
            Err(_) => return,
        };
        let failure = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let mut runtime = lock_unpoison(&self.agent_execution);
            let Some(configuration) = runtime.configuration.clone() else {
                return;
            };
            let validation = configuration
                .store
                .read_agent_execution_metadata()
                .and_then(|metadata| {
                    if self.shutting_down.load(Ordering::SeqCst)
                        || closed_for(&metadata, &start.binding)
                    {
                        return Err("agent_execution_admission_closed".to_owned());
                    }
                    let jobs = lock_unpoison(&self.jobs);
                    let record = jobs.get(&id).ok_or("agent_execution_job_missing")?;
                    if record.stop_requested.load(Ordering::SeqCst) {
                        return Err("agent_execution_stop_requested".into());
                    }
                    if runtime.live.contains_key(&id) {
                        return Err("agent_execution_worker_already_registered".into());
                    }
                    Ok(())
                });
            match validation {
                Err(error) => Some(error),
                Ok(()) => {
                    runtime
                        .live
                        .insert(id.clone(), IsolatedJobCustody::default());
                    let manager = self.clone_for_worker();
                    let guard = self.workers.enter();
                    let worker_id = id.clone();
                    match std::thread::Builder::new()
                        .name("agent-isolated-job".into())
                        .spawn(move || {
                            let _guard = guard;
                            manager.run_isolated_job(&worker_id, start, configuration);
                            manager.start_available_queued();
                        }) {
                        Ok(worker) => {
                            runtime.live.get_mut(&id).unwrap().worker = Some(worker);
                            None
                        }
                        Err(_) => {
                            let custody = runtime.live.get_mut(&id).unwrap();
                            custody.helpers_finished = true;
                            custody.worker_joined = true;
                            Some("agent_execution_worker_start_failed".into())
                        }
                    }
                }
            }
        };
        if let Some(error) = failure {
            self.fail_job(&queued.operation, error, None);
        }
    }

    fn run_isolated_job(
        &self,
        id: &str,
        start: AgentExecutionStart,
        configuration: AgentExecutionConfiguration,
    ) {
        #[cfg(windows)]
        let result = self.run_isolated_job_windows(id, &start, &configuration);
        #[cfg(not(windows))]
        let result: Result<(), String> = Err("agent_execution_platform_unavailable".into());
        if let Err(error) = result {
            if let Some(child) = lock_unpoison(&self.agent_execution)
                .live
                .get(id)
                .and_then(|custody| custody.child.clone())
            {
                let _ = lock_unpoison(&child).terminate_tree();
            }
            // Native attempted is persisted before invoking spawn. A failure
            // after that point cannot be rewritten as NeverCreated.
            let attempted = configuration
                .store
                .read_agent_execution_metadata()
                .ok()
                .and_then(|metadata| {
                    metadata
                        .jobs
                        .into_iter()
                        .find(|job| job.binding == start.binding)
                })
                .is_none_or(|job| job.native_create_attempted);
            {
                let _lifecycle = lock_unpoison(&self.lifecycle);
                if !attempted {
                    if let Some(custody) = lock_unpoison(&self.agent_execution).live.get_mut(id) {
                        custody.helpers_finished = true;
                    }
                }
                // Spawn/helper failures may follow the durable native-attempt
                // mark. Revoke owned network authority even when stop is unknown.
                let _ = revoke_terminal_proxy(
                    &configuration,
                    &start.binding,
                    Instant::now() + RESOURCE_CLEANUP_BUDGET,
                );
            }
            self.update_and_send(
                id,
                RunnerJobDelta {
                    status: if attempted { "lost" } else { "failed" }.into(),
                    error: Some(error),
                    command_execution_state: Some(if attempted {
                        ShellCommandExecutionState::OutcomeUnknown
                    } else {
                        ShellCommandExecutionState::NotStarted
                    }),
                    finished: true,
                    ..Default::default()
                },
            );
        }
    }
}

fn require_resource_cleanup(
    metadata: &AgentExecutionMetadata,
    binding: &AgentExecutionBinding,
) -> Result<(), String> {
    if metadata
        .jobs
        .iter()
        .find(|job| &job.binding == binding)
        .is_some_and(|job| {
            job.resource_cleanup
                .as_ref()
                .is_some_and(|cleanup| !cleanup.complete())
        })
    {
        return Err("agent_execution_resource_cleanup_pending".into());
    }
    Ok(())
}

fn require_close_stop_receipts(
    metadata: &AgentExecutionMetadata,
    close: &AgentExecutionClose,
    receipt: &AgentExecutionCloseReceipt,
) -> Result<(), String> {
    for (index, binding) in close.members.iter().enumerate() {
        if let Some(job) = metadata.jobs.iter().find(|job| &job.binding == binding) {
            if job.resource_cleanup.is_some()
                && (!job.worker_finished
                    || job.stop_receipt.is_none()
                    || job.stop_receipt.as_ref() != receipt.receipts.get(index))
            {
                return Err("agent_execution_close_stop_unproven".into());
            }
        }
    }
    Ok(())
}

fn update_resource_cleanup(
    store: &DetachedJobStore,
    binding: &AgentExecutionBinding,
    update: impl FnOnce(&mut AgentExecutionResourceCleanupMetadata),
) -> Result<(), String> {
    store
        .update_agent_execution_metadata(|metadata| {
            let cleanup = metadata
                .jobs
                .iter_mut()
                .find(|job| &job.binding == binding)
                .and_then(|job| job.resource_cleanup.as_mut())
                .ok_or("agent_execution_resource_cleanup_missing")?;
            update(cleanup);
            Ok(())
        })
        .map_err(|_| "agent_execution_cleanup_metadata_unavailable".into())
}

fn resource_cleanup_failed(
    store: &DetachedJobStore,
    binding: &AgentExecutionBinding,
    code: &str,
) -> Result<(), String> {
    update_resource_cleanup(store, binding, |cleanup| {
        cleanup.last_cleanup_error = Some(code.to_owned());
    })?;
    Err(code.to_owned())
}

fn revoke_terminal_proxy_with(
    store: &DetachedJobStore,
    binding: &AgentExecutionBinding,
    deadline: Instant,
    mut revoke: impl FnMut(&AgentExecutionBinding, Instant) -> Result<(), String>,
) -> Result<(), String> {
    let metadata = store
        .read_agent_execution_metadata()
        .map_err(|_| "agent_execution_cleanup_metadata_unavailable")?;
    let owned = metadata
        .jobs
        .iter()
        .find(|job| &job.binding == binding)
        .and_then(|job| job.resource_cleanup.as_ref())
        .is_some_and(|cleanup| cleanup.grant_intent && !cleanup.proxy_revoked);
    if !owned {
        return Ok(());
    }
    if Instant::now() >= deadline {
        return resource_cleanup_failed(
            store,
            binding,
            "agent_execution_resource_cleanup_deadline_expired",
        );
    }
    if revoke(binding, deadline).is_err() {
        return resource_cleanup_failed(store, binding, "agent_execution_proxy_revoke_failed");
    }
    update_resource_cleanup(store, binding, |cleanup| {
        cleanup.proxy_revoked = true;
        cleanup.last_cleanup_error = None;
    })
}

fn revoke_terminal_proxy(
    configuration: &AgentExecutionConfiguration,
    binding: &AgentExecutionBinding,
    deadline: Instant,
) -> Result<(), String> {
    revoke_terminal_proxy_with(
        &configuration.store,
        binding,
        deadline,
        super::agent_proxy::revoke,
    )
}

// Injected effects keep recovery-policy tests independent of native APIs and
// auth files. The binding comes only from the immutable durable job record.
fn cleanup_agent_resources_with(
    store: &DetachedJobStore,
    binding: &AgentExecutionBinding,
    deadline: Instant,
    revoke_without_stop_proof: bool,
    mut revoke: impl FnMut(&AgentExecutionBinding, Instant) -> Result<(), String>,
    mut remove_auth: impl FnMut(&AgentExecutionBinding) -> Result<(), String>,
) -> Result<(), String> {
    let metadata = store
        .read_agent_execution_metadata()
        .map_err(|_| "agent_execution_cleanup_metadata_unavailable")?;
    let Some(job) = metadata.jobs.iter().find(|job| &job.binding == binding) else {
        return Ok(());
    };
    let Some(mut cleanup) = job.resource_cleanup.clone() else {
        return Ok(());
    };
    if !cleanup.auth_copy_expected && !cleanup.proxy_requested {
        return Ok(());
    }
    // read_agent_execution_metadata has validated the exact complete physical
    // stop receipt. Revocation alone can never establish this field.
    if job.stop_receipt.is_some() && !cleanup.resources_stopped {
        update_resource_cleanup(store, binding, |state| state.resources_stopped = true)?;
        cleanup.resources_stopped = true;
    }
    if cleanup.grant_intent
        && !cleanup.proxy_revoked
        && (revoke_without_stop_proof || cleanup.resources_stopped)
    {
        revoke_terminal_proxy_with(store, binding, deadline, &mut revoke)?;
    }
    if cleanup.auth_copy_expected && !cleanup.auth_removed && cleanup.resources_stopped {
        if Instant::now() >= deadline {
            return resource_cleanup_failed(
                store,
                binding,
                "agent_execution_resource_cleanup_deadline_expired",
            );
        }
        if remove_auth(binding).is_err() {
            return resource_cleanup_failed(store, binding, "agent_execution_auth_cleanup_failed");
        }
        update_resource_cleanup(store, binding, |state| {
            state.auth_removed = true;
            state.last_cleanup_error = None;
        })?;
    } else if cleanup.complete() && cleanup.last_cleanup_error.is_some() {
        update_resource_cleanup(store, binding, |state| state.last_cleanup_error = None)?;
    }
    Ok(())
}

fn cleanup_agent_resources(
    configuration: &AgentExecutionConfiguration,
    binding: &AgentExecutionBinding,
    deadline: Instant,
    revoke_without_stop_proof: bool,
) -> Result<(), String> {
    cleanup_agent_resources_with(
        &configuration.store,
        binding,
        deadline,
        revoke_without_stop_proof,
        super::agent_proxy::revoke,
        |binding| {
            super::agent_resources::cleanup_native_auth(binding, &configuration.resource_root)
        },
    )
}

fn reconcile_agent_resource_cleanup(
    configuration: &AgentExecutionConfiguration,
) -> Result<(), String> {
    let deadline = Instant::now() + RESOURCE_CLEANUP_BUDGET;
    let metadata = configuration
        .store
        .read_agent_execution_metadata()
        .map_err(|_| "agent_execution_cleanup_metadata_unavailable")?;
    for job in metadata.jobs {
        cleanup_agent_resources(configuration, &job.binding, deadline, true)?;
    }
    Ok(())
}

/// A command's Completed projection is insufficient. Only the actual durable
/// whole-tree/worker proof and matching complete close receipt authorize readout.
fn delivery_close_receipt(
    metadata: &AgentExecutionMetadata,
    binding: &AgentExecutionBinding,
) -> Result<AgentExecutionCloseReceipt, String> {
    let job = metadata
        .jobs
        .iter()
        .find(|job| &job.binding == binding)
        .ok_or("agent_cli_delivery_job_not_recorded")?;
    let stop = job
        .stop_receipt
        .as_ref()
        .ok_or("agent_cli_delivery_stop_unproven")?;
    if !job.native_create_attempted
        || !job.worker_finished
        || !closed_for(metadata, binding)
        || !matches!(
            stop.outcome,
            AgentExecutionStopOutcome::TreeExited {
                leader_handle_signaled: true,
                membership_complete: true,
                member_handles_signaled: true,
                worker_finished: true,
                ..
            }
        )
    {
        return Err("agent_cli_delivery_stop_unproven".into());
    }
    for state in &metadata.closes {
        let Some(index) = state
            .close
            .members
            .iter()
            .position(|member| member == binding)
        else {
            continue;
        };
        let Some(receipt) = state.receipt.as_ref() else {
            continue;
        };
        receipt.validate_for(&state.close)?;
        if receipt.receipts.get(index) == Some(stop) && closed_by(&state.close, binding) {
            require_resource_cleanup(metadata, binding)?;
            return Ok(receipt.clone());
        }
    }
    Err("agent_cli_delivery_close_unproven".into())
}

impl JobManager {
    /// Persist the immutable admission fence before cancellation or observation.
    /// Unknown is an error, never a partial receipt or an ACP status surrogate.
    pub(crate) fn close_agent_execution(
        &self,
        sink: &RunnerSink,
        close: &AgentExecutionClose,
    ) -> Result<AgentExecutionCloseReceipt, String> {
        close.validate()?;
        if close.target.client_id != sink.client_id()
            || close.target.runner_instance_id != sink.runner_instance_id()
        {
            return Err("agent_execution_target_mismatch".into());
        }
        let (configuration, cancelled_queue) = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let runtime = lock_unpoison(&self.agent_execution);
            let configuration = runtime
                .configuration
                .clone()
                .ok_or("agent_execution_unavailable")?;
            if let Some(receipt) =
                configuration
                    .store
                    .update_agent_execution_metadata(|metadata| {
                        if let Some(existing) = metadata.closes.iter().find(|state| {
                            state.close.close_ref.close_id == close.close_ref.close_id
                                && state.close.target == close.target
                        }) {
                            if existing.close != *close {
                                return Err("agent_execution_close_identity_conflict".into());
                            }
                            return Ok(existing.receipt.clone());
                        }
                        // A source has one immutable close budget and member set. A
                        // new close_id cannot extend the deadline or reopen the gate.
                        if metadata
                            .closes
                            .iter()
                            .any(|state| state.close.close_ref.source == close.close_ref.source)
                        {
                            return Err("agent_execution_source_already_closing".into());
                        }
                        for member in &close.members {
                            if let Some(known) = metadata
                                .jobs
                                .iter()
                                .find(|known| execution_identity_collides(&known.binding, member))
                            {
                                if known.binding != *member {
                                    return Err("agent_execution_close_member_conflict".into());
                                }
                            }
                        }
                        // The Store-authorized member list may include delegated
                        // descendants. Core/Runner do not infer its ancestry from IDs.
                        // Every known execution affected by this exact fence
                        // must be included, including another launch on a
                        // listed executor's run. A partial physical set is not
                        // a complete close even when ancestry came from Store.
                        if metadata.jobs.iter().any(|known| {
                            closed_by(close, &known.binding)
                                && !close.members.contains(&known.binding)
                        }) {
                            return Err("agent_execution_close_missing_known_member".into());
                        }
                        metadata.closes.push(AgentExecutionCloseMetadata {
                            close: close.clone(),
                            receipt: None,
                        });
                        Ok(None)
                    })?
            {
                // A lost response replays the already-persisted original proof,
                // even when its observation deadline has since elapsed.
                let metadata = configuration.store.read_agent_execution_metadata()?;
                require_close_stop_receipts(&metadata, close, &receipt)?;
                let cleanup_deadline = Instant::now() + RESOURCE_CLEANUP_BUDGET;
                for member in &close.members {
                    cleanup_agent_resources(&configuration, member, cleanup_deadline, false)?;
                }
                let metadata = configuration.store.read_agent_execution_metadata()?;
                for member in &close.members {
                    require_resource_cleanup(&metadata, member)?;
                }
                return Ok(receipt);
            }
            let metadata = configuration.store.read_agent_execution_metadata()?;
            let mut cancelled_queue = Vec::new();
            lock_unpoison(&self.queued).retain(|queued| {
                let RunnerJobOperation::StartIsolatedProcess(operation) = &queued.operation else {
                    return true;
                };
                let keep = !closed_for(&metadata, &operation.start.binding);
                if !keep {
                    cancelled_queue.push(queued.operation.job_id().to_owned());
                }
                keep
            });
            let jobs = lock_unpoison(&self.jobs);
            for job in jobs.values() {
                if job
                    .snapshot
                    .context
                    .agent_execution
                    .as_ref()
                    .is_some_and(|binding| closed_for(&metadata, binding))
                {
                    job.stop_requested.store(true, Ordering::SeqCst);
                }
            }
            for member in &close.members {
                if let Ok(id) = job_id(member) {
                    if let Some(child) = runtime
                        .live
                        .get(id)
                        .and_then(|custody| custody.child.as_ref())
                    {
                        // This requests termination. It is never itself proof.
                        let _ = lock_unpoison(child).terminate_tree();
                    }
                }
            }
            (configuration, cancelled_queue)
        };
        for id in cancelled_queue {
            self.update_and_send(
                &id,
                RunnerJobDelta {
                    status: "cancelled".into(),
                    command_execution_state: Some(ShellCommandExecutionState::NotStarted),
                    error: Some("agent_execution_admission_closed".into()),
                    duration_ms: Some(0),
                    finished: true,
                    ..Default::default()
                },
            );
        }
        let remaining_ms = close
            .deadline_at_ms
            .saturating_sub(chrono::Utc::now().timestamp_millis());
        if remaining_ms <= 0 {
            return Err("agent_execution_close_deadline_expired".into());
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(remaining_ms as u64))
            .ok_or("agent_execution_close_deadline_invalid")?;
        loop {
            let mut receipts = Vec::with_capacity(close.members.len());
            let mut unresolved = false;
            for member in &close.members {
                match self.observe_agent_execution_member(&configuration, member, deadline)? {
                    Some(receipt) => receipts.push(receipt),
                    None => unresolved = true,
                }
            }
            if !unresolved {
                let receipt = AgentExecutionCloseReceipt {
                    version: 1,
                    close_ref: close.close_ref.clone(),
                    target: close.target.clone(),
                    deadline_at_ms: close.deadline_at_ms,
                    members_sha256: close.members_sha256.clone(),
                    observation_revision: 1,
                    admission_closed: true,
                    tombstone_persisted: true,
                    receipts,
                };
                receipt.validate_for(close)?;
                let _lifecycle = lock_unpoison(&self.lifecycle);
                if Instant::now() >= deadline
                    || chrono::Utc::now().timestamp_millis() >= close.deadline_at_ms
                {
                    return Err("agent_execution_close_deadline_expired".into());
                }
                return configuration
                    .store
                    .update_agent_execution_metadata(|metadata| {
                        if Instant::now() >= deadline
                            || chrono::Utc::now().timestamp_millis() >= close.deadline_at_ms
                        {
                            return Err("agent_execution_close_deadline_expired".into());
                        }
                        for member in &close.members {
                            require_resource_cleanup(metadata, member)?;
                        }
                        require_close_stop_receipts(metadata, close, &receipt)?;
                        let stored = metadata
                            .closes
                            .iter_mut()
                            .find(|state| state.close == *close)
                            .ok_or("agent_execution_close_fence_missing")?;
                        if let Some(previous) = &stored.receipt {
                            return Ok(previous.clone());
                        }
                        stored.receipt = Some(receipt.clone());
                        Ok(receipt)
                    });
            }
            if Instant::now() >= deadline
                || chrono::Utc::now().timestamp_millis() >= close.deadline_at_ms
            {
                return Err("agent_execution_close_outcome_unknown".into());
            }
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn observe_agent_execution_member(
        &self,
        configuration: &AgentExecutionConfiguration,
        binding: &AgentExecutionBinding,
        deadline: Instant,
    ) -> Result<Option<AgentExecutionStopReceipt>, String> {
        let id = job_id(binding)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let metadata = configuration.store.read_agent_execution_metadata()?;
        if !closed_for(&metadata, binding) {
            return Err("agent_execution_close_fence_missing".into());
        }
        let stored = metadata.jobs.iter().find(|job| job.binding == *binding);
        if let Some(receipt) = stored.and_then(|job| job.stop_receipt.as_ref()) {
            // A previous close may have committed proof before cleanup failed.
            if let Some(custody) = lock_unpoison(&self.agent_execution).live.get_mut(id) {
                drop(custody.resources.take());
            }
            cleanup_agent_resources(configuration, binding, deadline, false)?;
            let current = configuration.store.read_agent_execution_metadata()?;
            require_resource_cleanup(&current, binding)?;
            return Ok(Some(receipt.clone()));
        }
        let mut runtime = lock_unpoison(&self.agent_execution);
        let custody = runtime.live.get_mut(id);
        let attempted = stored.is_some_and(|job| job.native_create_attempted);
        let outcome = match custody {
            Some(custody) => {
                if stored.is_none() {
                    return Err("agent_execution_custody_binding_mismatch".into());
                }
                if let Some(worker) = custody.worker.as_ref() {
                    if !worker.is_finished() {
                        return Ok(None);
                    }
                    let joined = custody.worker.take().unwrap().join().is_ok();
                    custody.worker_joined = joined;
                }
                if !custody.worker_joined || !custody.helpers_finished {
                    return Ok(None);
                }
                if custody.observation_failed {
                    return Err("agent_execution_member_observation_unknown".into());
                }
                if !attempted {
                    AgentExecutionStopOutcome::NeverCreated {
                        request_cancelled: true,
                        worker_finished: true,
                    }
                } else {
                    let Some(child) = custody.child.as_ref() else {
                        return Ok(None);
                    };
                    #[cfg(windows)]
                    {
                        let mut child = lock_unpoison(child);
                        let JobChild::Isolated(native) = &mut *child else {
                            return Err("agent_execution_child_carrier_mismatch".into());
                        };
                        let proof = native
                            .observe_completed_tree(deadline)
                            .map_err(|_| "agent_execution_member_observation_unknown")?;
                        let Some(proof) = proof else {
                            return Ok(None);
                        };
                        AgentExecutionStopOutcome::TreeExited {
                            leader_exit_code: Some(proof.leader_exit_code()),
                            leader_handle_signaled: true,
                            membership_complete: true,
                            member_handles_signaled: true,
                            worker_finished: true,
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        let _ = child;
                        return Err("agent_execution_platform_unavailable".into());
                    }
                }
            }
            None if attempted => {
                // A restarted Runner has no original handles or worker join.
                // Persisted PID/status/Drop cannot fill this physical proof gap.
                return Err("agent_execution_restarted_member_unknown".into());
            }
            None => {
                // No worker was ever admitted here, or durable pre-create state
                // survived a restart. The write-ahead native-attempt mark plus
                // the already-durable fence excludes any future native launch.
                AgentExecutionStopOutcome::NeverCreated {
                    request_cancelled: true,
                    worker_finished: true,
                }
            }
        };
        let receipt = AgentExecutionStopReceipt {
            binding_sha256: binding.binding_hash()?,
            carrier: binding.carrier.clone(),
            observation_revision: 1,
            outcome,
        };
        configuration
            .store
            .update_agent_execution_metadata(|metadata| {
                if let Some(stored) = metadata.jobs.iter_mut().find(|job| job.binding == *binding) {
                    stored.worker_finished = true;
                    stored.stop_receipt = Some(receipt.clone());
                    if let Some(cleanup) = stored.resource_cleanup.as_mut() {
                        cleanup.resources_stopped = true;
                    }
                }
                Ok(())
            })?;
        if let Some(custody) = runtime.live.get_mut(id) {
            drop(custody.resources.take());
        }
        drop(runtime);
        cleanup_agent_resources(configuration, binding, deadline, false)?;
        let current = configuration.store.read_agent_execution_metadata()?;
        require_resource_cleanup(&current, binding)?;
        Ok(Some(receipt))
    }
}

#[cfg(windows)]
impl JobManager {
    /// Called only after every stdin/output helper has joined. Physical cleanup
    /// does not claim that this worker's own JoinHandle has joined for close.
    fn cleanup_stopped_isolated_resources(
        &self,
        id: &str,
        binding: &AgentExecutionBinding,
        configuration: &AgentExecutionConfiguration,
        child: &Arc<Mutex<JobChild>>,
        deadline: Instant,
    ) -> Result<(), String> {
        let _lifecycle = lock_unpoison(&self.lifecycle);
        loop {
            if Instant::now() >= deadline {
                return resource_cleanup_failed(
                    &configuration.store,
                    binding,
                    "agent_execution_resource_stop_unproven",
                );
            }
            let complete = {
                let mut child = lock_unpoison(child);
                let JobChild::Isolated(native) = &mut *child else {
                    return Err("agent_execution_child_carrier_mismatch".into());
                };
                match native.observe_completed_tree(deadline) {
                    Ok(proof) => proof.is_some(),
                    Err(_) => {
                        return resource_cleanup_failed(
                            &configuration.store,
                            binding,
                            "agent_execution_resource_stop_unproven",
                        )
                    }
                }
            };
            if complete {
                break;
            }
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        update_resource_cleanup(&configuration.store, binding, |cleanup| {
            cleanup.resources_stopped = true;
        })?;
        if let Some(custody) = lock_unpoison(&self.agent_execution).live.get_mut(id) {
            drop(custody.resources.take());
        }
        cleanup_agent_resources(configuration, binding, deadline, false)
    }

    fn run_isolated_job_windows(
        &self,
        id: &str,
        start: &AgentExecutionStart,
        configuration: &AgentExecutionConfiguration,
    ) -> Result<(), String> {
        use std::io::{Read, Write};
        use std::sync::mpsc;
        use webcodex_process::windows_lpac::LpacChild;
        let began = Instant::now();
        let deadline = began + Duration::from_secs(start.timeout_secs);
        let prepared = super::agent_resources::prepare(
            &configuration.profiles,
            start,
            &configuration.resource_root,
        )?;
        let proxy_requested = prepared.proxy_requested();
        let (auth_copy_expected, _) = configuration.profiles.cleanup_requirements(start)?;
        let cleanup_required = auth_copy_expected || proxy_requested;
        let (child, stdin, stdout, stderr, stop_requested) = {
            // Final admission check, durable native-attempt mark, actual native
            // creation and child publication share one lifecycle critical section.
            let _lifecycle = lock_unpoison(&self.lifecycle);
            configuration.profiles.validate_execution(start)?;
            let mut runtime = lock_unpoison(&self.agent_execution);
            let mut jobs = lock_unpoison(&self.jobs);
            let job = jobs.get_mut(id).ok_or("agent_execution_job_missing")?;
            let custody = runtime
                .live
                .get_mut(id)
                .ok_or("agent_execution_custody_missing")?;
            custody.resources = Some(prepared);
            let validate_admission = |metadata: &AgentExecutionMetadata| -> Result<(), String> {
                if self.shutting_down.load(Ordering::SeqCst)
                    || job.stop_requested.load(Ordering::SeqCst)
                    || Instant::now() >= deadline
                    || closed_for(metadata, &start.binding)
                {
                    return Err("agent_execution_admission_closed".into());
                }
                let stored = metadata
                    .jobs
                    .iter()
                    .find(|stored| stored.binding == start.binding)
                    .ok_or("agent_execution_start_not_recorded")?;
                if stored.native_create_attempted
                    || stored.worker_finished
                    || stored.stop_receipt.is_some()
                {
                    return Err("agent_execution_creation_replay_denied".into());
                }
                let cleanup = stored
                    .resource_cleanup
                    .as_ref()
                    .ok_or("agent_execution_resource_cleanup_missing")?;
                if cleanup.proxy_requested != proxy_requested
                    || cleanup.resources_stopped
                    || cleanup.proxy_revoked
                {
                    return Err("agent_execution_resource_cleanup_conflict".into());
                }
                Ok(())
            };
            validate_admission(&configuration.store.read_agent_execution_metadata()?)?;
            if proxy_requested {
                // Never claim a pre-existing exception. Write ownership intent
                // before the native mutation so restart can revoke our SID.
                super::agent_proxy::require_absent(&start.binding, deadline)?;
                configuration
                    .store
                    .update_agent_execution_metadata(|metadata| {
                        validate_admission(metadata)?;
                        let cleanup = metadata
                            .jobs
                            .iter_mut()
                            .find(|stored| stored.binding == start.binding)
                            .and_then(|stored| stored.resource_cleanup.as_mut())
                            .ok_or("agent_execution_resource_cleanup_missing")?;
                        if cleanup.grant_intent {
                            return Err("agent_execution_proxy_grant_replay_denied".into());
                        }
                        cleanup.grant_intent = true;
                        Ok(())
                    })?;
                super::agent_proxy::grant(&start.binding, deadline)?;
                update_resource_cleanup(&configuration.store, &start.binding, |cleanup| {
                    cleanup.grant_verified = true;
                })?;
            }
            configuration
                .store
                .update_agent_execution_metadata(|metadata| {
                    validate_admission(metadata)?;
                    let stored = metadata
                        .jobs
                        .iter_mut()
                        .find(|stored| stored.binding == start.binding)
                        .ok_or("agent_execution_start_not_recorded")?;
                    stored.native_create_attempted = true;
                    Ok(())
                })?;
            let mut native = LpacChild::spawn(custody.resources.as_ref().unwrap().launch_spec())
                .map_err(|_| "agent_execution_native_create_outcome_unknown")?;
            let stdin = native.stdin.take();
            let stdout = native.stdout.take();
            let stderr = native.stderr.take();
            let child = Arc::new(Mutex::new(JobChild::Isolated(native)));
            custody.child = Some(Arc::clone(&child));
            job.child = Some(Arc::clone(&child));
            (
                child,
                stdin,
                stdout,
                stderr,
                Arc::clone(&job.stop_requested),
            )
        };
        self.update_and_send(
            id,
            RunnerJobDelta {
                status: "running".into(),
                ..Default::default()
            },
        );
        let (tx, rx) = mpsc::sync_channel::<(bool, Vec<u8>)>(16);
        let mut helpers = Vec::new();
        for (reader, is_stdout) in [(stdout, true), (stderr, false)] {
            if let Some(mut reader) = reader {
                let tx = tx.clone();
                helpers.push(
                    std::thread::Builder::new()
                        .name("agent-job-output".into())
                        .spawn(move || {
                            let mut bytes = [0; 8192];
                            loop {
                                let count = reader.read(&mut bytes).map_err(|_| ())?;
                                if count == 0 {
                                    return Ok(());
                                }
                                tx.send((is_stdout, bytes[..count].to_vec()))
                                    .map_err(|_| ())?;
                            }
                        })
                        .map_err(|_| "agent_execution_output_worker_unavailable")?,
                );
            }
        }
        drop(tx);
        if let Some(mut stdin) = stdin {
            if let Some(input) = start.stdin.clone() {
                helpers.push(
                    std::thread::Builder::new()
                        .name("agent-job-input".into())
                        .spawn(move || stdin.write_all(input.as_bytes()).map_err(|_| ()))
                        .map_err(|_| "agent_execution_input_worker_unavailable")?,
                );
            }
        }
        let flush_output = || {
            for _ in 0..16 {
                let Ok((stdout, bytes)) = rx.try_recv() else {
                    break;
                };
                let text = String::from_utf8_lossy(&bytes).into_owned();
                self.update_and_send(
                    id,
                    RunnerJobDelta {
                        stdout_chunk: stdout.then(|| text.clone()),
                        stderr_chunk: (!stdout).then_some(text),
                        ..Default::default()
                    },
                );
            }
        };
        let mut observation_failed = false;
        let mut timed_out = false;
        let mut exit_code = None;
        loop {
            flush_output();
            let now = Instant::now();
            if now >= deadline
                || stop_requested.load(Ordering::SeqCst)
                || self.shutting_down.load(Ordering::SeqCst)
            {
                timed_out = now >= deadline;
                let _ = lock_unpoison(&child).terminate_tree();
                break;
            }
            let mut child_guard = lock_unpoison(&child);
            let JobChild::Isolated(native) = &mut *child_guard else {
                unreachable!();
            };
            if native.capture_member_handles(deadline).is_err() {
                observation_failed = true;
                let _ = native.terminate_tree();
                break;
            }
            match native.try_wait() {
                Ok(Some(code)) => {
                    exit_code = Some(code);
                    // A completed leader cannot leave descendants using its
                    // resources indefinitely while the worker drains output.
                    let _ = native.terminate_tree();
                    break;
                }
                Ok(None) => {}
                Err(_) => {
                    observation_failed = true;
                    let _ = native.terminate_tree();
                    break;
                }
            }
            drop(child_guard);
            std::thread::sleep(Duration::from_millis(5));
        }
        let drain_deadline = Instant::now() + Duration::from_secs(2);
        let mut helper_error = false;
        while !helpers.is_empty() && Instant::now() < drain_deadline {
            flush_output();
            let mut index = 0;
            while index < helpers.len() {
                if helpers[index].is_finished() {
                    helper_error |= !matches!(helpers.swap_remove(index).join(), Ok(Ok(())));
                } else {
                    index += 1;
                }
            }
            if !helpers.is_empty() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        flush_output();
        // A completed write may report BrokenPipe after intentional tree
        // termination. That affects the command outcome, but a joined helper
        // is no longer an active writer and must not prevent close forever.
        let helpers_finished = helpers.is_empty();
        // Unfinished JoinHandles are deliberately not transformed into proof.
        // The bounded channels cannot keep growing if this worker must leave.
        if let Some(custody) = lock_unpoison(&self.agent_execution).live.get_mut(id) {
            custody.helpers_finished = helpers_finished;
            custody.observation_failed |= observation_failed;
        }
        let cleanup_error = if cleanup_required {
            // Revoke before waiting for proof: unknown observation must not
            // leave an owned exception until restart. All cleanup shares 10s.
            let cleanup_deadline = Instant::now() + RESOURCE_CLEANUP_BUDGET;
            let revoked = {
                let _lifecycle = lock_unpoison(&self.lifecycle);
                revoke_terminal_proxy(configuration, &start.binding, cleanup_deadline)
            };
            if revoked.is_err() {
                revoked.err()
            } else if helpers_finished && !observation_failed {
                self.cleanup_stopped_isolated_resources(
                    id,
                    &start.binding,
                    configuration,
                    &child,
                    cleanup_deadline,
                )
                .err()
            } else {
                None
            }
        } else {
            None
        };
        let uncertain =
            observation_failed || helper_error || !helpers_finished || cleanup_error.is_some();
        self.update_and_send(
            id,
            RunnerJobDelta {
                status: if uncertain {
                    "lost"
                } else if timed_out {
                    "timeout"
                } else if stop_requested.load(Ordering::SeqCst) {
                    "stopped"
                } else if exit_code == Some(0) {
                    "completed"
                } else {
                    "failed"
                }
                .into(),
                exit_code: exit_code.map(|code| code as i32),
                duration_ms: Some(began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
                error: cleanup_error
                    .or_else(|| uncertain.then(|| "agent_execution_completion_unknown".into())),
                command_execution_state: Some(if uncertain {
                    ShellCommandExecutionState::OutcomeUnknown
                } else if timed_out {
                    ShellCommandExecutionState::TimedOut
                } else {
                    ShellCommandExecutionState::Completed
                }),
                finished: true,
                ..Default::default()
            },
        );
        Ok(())
    }
}
