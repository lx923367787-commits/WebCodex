//! Runner jobs detached implementation.

use crate::webcodex_runner;
use crate::webcodex_runner::detached_job::{
    handoff_detached_job, snapshot_from_detached_record, DetachedHandoffOutcome, DetachedJobStore,
    DetachedLaunchSpec, DetachedStartRequest,
};
use crate::webcodex_runner::jobs::context::validate_detached_recovery_context;
use crate::webcodex_runner::jobs::lifecycle::{runner_job_is_active, runner_job_is_terminal};
use crate::webcodex_runner::jobs::updates::job_update_from_snapshot;
use crate::webcodex_runner::jobs::{
    DetachedJobRef, JobManager, PendingJobStart, RunnerJobDelta, RunningJob,
};
use crate::webcodex_runner::prepare_detached_process_launch;
use crate::webcodex_runner::shutdown::lock_unpoison;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use webcodex_core::runner_operation::RunnerJobOperation;
use webcodex_core::runner_protocol::{
    ShellCommandExecutionState, JOB_INVENTORY_MAX_ACTIVE_JOBS, JOB_TERMINAL_RETENTION_SECS,
};

impl JobManager {
    pub(crate) fn detached_store_for_start(
        &self,
        client_id: &str,
    ) -> Result<DetachedJobStore, String> {
        #[cfg(test)]
        if let Some(root) = lock_unpoison(&self.detached_store_root_override).clone() {
            return Ok(DetachedJobStore::new(root));
        }
        DetachedJobStore::default_root_for_runner(client_id, &self.detached_profile_server_url)
            .map(DetachedJobStore::new)
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub(crate) fn recover_detached_jobs(
        &self,
        store: DetachedJobStore,
        client_id: &str,
        runner_instance_id: &str,
    ) -> Result<usize, String> {
        let records = store.scan_for_client(client_id)?;
        let mut recoverable = Vec::new();
        for record in records {
            let Some(record) = store.reconcile_after_runner_restart(record)? else {
                continue;
            };
            validate_detached_recovery_context(&record.context, client_id)?;
            let snapshot = snapshot_from_detached_record(&record)?;
            if runner_job_is_terminal(&snapshot.status)
                && snapshot.ended_at.is_some_and(|ended| {
                    chrono::Utc::now().timestamp().saturating_sub(ended)
                        >= JOB_TERMINAL_RETENTION_SECS
                })
            {
                continue;
            }
            recoverable.push((record, snapshot));
        }
        let active_count = recoverable
            .iter()
            .filter(|(_, snapshot)| runner_job_is_active(&snapshot.status))
            .count();
        if active_count > JOB_INVENTORY_MAX_ACTIVE_JOBS {
            return Err(format!(
                "detached Job recovery found {active_count} active records; maximum is {JOB_INVENTORY_MAX_ACTIVE_JOBS}"
            ));
        }

        let mut observers = Vec::new();
        {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let mut detached_jobs = lock_unpoison(&self.detached_jobs);
            let mut jobs = lock_unpoison(&self.jobs);
            for (record, snapshot) in recoverable {
                if jobs.contains_key(&record.job_id) || detached_jobs.contains_key(&record.job_id) {
                    return Err(format!(
                        "detached Job recovery conflicts with existing local job {}",
                        record.job_id
                    ));
                }
                let active = runner_job_is_active(&snapshot.status);
                let detached = DetachedJobRef {
                    store: store.clone(),
                    execution_id: record.execution_id.clone(),
                };
                let job_id = record.job_id.clone();
                jobs.insert(
                    job_id.clone(),
                    RunningJob {
                        client_id: client_id.to_string(),
                        runner_instance_id: runner_instance_id.to_string(),
                        snapshot,
                        child: None,
                        stop_requested: Arc::new(AtomicBool::new(record.stop_requested)),
                        slot_reserved: active,
                    },
                );
                if active {
                    detached_jobs.insert(job_id.clone(), detached.clone());
                    observers.push((job_id, detached));
                }
            }
        }
        let recovered = observers.len();
        for (job_id, detached) in observers {
            if let Err(error) = self.spawn_detached_observer(job_id.clone(), detached) {
                // Observation is best-effort after exact durable ownership has
                // been recovered. Keep the durable control reference and Job
                // projection so shutdown exclusion and normal stop routing
                // remain correct even when live observation is degraded.
                tracing::error!(target: "webcodex_runner",job_id = %job_id, error = %error, "detached Job observer startup failed; durable control retained");
            }
        }
        Ok(recovered)
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub(crate) fn spawn_detached_observer(
        &self,
        job_id: String,
        detached: DetachedJobRef,
    ) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_detached_observer_spawn.load(Ordering::SeqCst) {
            return Err("test-injected detached observer startup failure".to_string());
        }
        let manager = self.clone_for_worker();
        let shutting_down = Arc::clone(&self.shutting_down);
        let worker_guard = self.workers.enter();
        let observer_job_id = job_id.clone();
        std::thread::Builder::new()
            .name("webcodex-detached-job-observer".to_string())
            .spawn(move || {
                let _worker_guard = worker_guard;
                loop {
                    if shutting_down.load(Ordering::SeqCst) {
                        return;
                    }
                    let record = match detached.store.read(&observer_job_id) {
                        Ok(record) => record,
                        Err(error) => {
                            tracing::error!(target: "webcodex_runner",job_id = %observer_job_id, error = %error, "detached Job durable observer failed closed");
                            return;
                        }
                    };
                    let record = match detached.store.reconcile_after_runner_restart(record) {
                        Ok(Some(record)) => record,
                        Ok(None) => {
                            tracing::error!(target: "webcodex_runner",job_id = %observer_job_id, "detached Job observer found a pre-accept record after recovery");
                            return;
                        }
                        Err(error) => {
                            tracing::error!(target: "webcodex_runner",job_id = %observer_job_id, error = %error, "detached Job liveness reconciliation failed closed");
                            return;
                        }
                    };
                    if record.execution_id != detached.execution_id {
                        tracing::error!(target: "webcodex_runner",job_id = %observer_job_id, "detached Job durable observer saw execution identity replacement");
                        return;
                    }
                    let terminal = record.phase == webcodex_runner::detached_job::DetachedJobPhase::Terminal;
                    match manager.sync_detached_record(&observer_job_id, &record) {
                        Ok(_) => {}
                        Err(error) => {
                            tracing::error!(target: "webcodex_runner",job_id = %observer_job_id, error = %error, "detached Job inventory sync failed closed");
                            return;
                        }
                    }
                    if terminal {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .map(|_| ())
            .map_err(|error| format!("failed to start detached Job observer: {error}"))
    }

    pub(crate) fn sync_detached_record(
        &self,
        job_id: &str,
        record: &webcodex_runner::detached_job::DetachedJobRecord,
    ) -> Result<bool, String> {
        let snapshot = snapshot_from_detached_record(record)?;
        let (update, terminal, semantic) = {
            let mut jobs = lock_unpoison(&self.jobs);
            let job = jobs
                .get_mut(job_id)
                .ok_or_else(|| format!("unknown recovered detached Job: {job_id}"))?;
            if snapshot.request_id != job.snapshot.request_id
                || snapshot.context != job.snapshot.context
            {
                return Err("detached Job durable ownership context changed".to_string());
            }
            if snapshot.update_seq < job.snapshot.update_seq {
                return Err("detached Job durable update sequence regressed".to_string());
            }
            if snapshot.update_seq == job.snapshot.update_seq {
                if snapshot != job.snapshot {
                    return Err(
                        "detached Job state changed without advancing update sequence".to_string(),
                    );
                }
                return Ok(runner_job_is_terminal(&snapshot.status));
            }
            let semantic = snapshot.status != job.snapshot.status
                || snapshot.started_at != job.snapshot.started_at
                || snapshot.ended_at != job.snapshot.ended_at
                || snapshot.exit_code != job.snapshot.exit_code
                || snapshot.duration_ms != job.snapshot.duration_ms
                || snapshot.error != job.snapshot.error
                || snapshot.command_execution_state != job.snapshot.command_execution_state
                || snapshot.validation_progress != job.snapshot.validation_progress;
            job.snapshot = snapshot;
            job.stop_requested
                .store(record.stop_requested, Ordering::SeqCst);
            let terminal = runner_job_is_terminal(&job.snapshot.status);
            if terminal {
                job.slot_reserved = false;
                job.child = None;
            }
            (
                job_update_from_snapshot(&job.client_id, &job.runner_instance_id, &job.snapshot),
                terminal,
                semantic || terminal,
            )
        };
        self.queue_recorded_update(update, terminal || semantic);
        if terminal {
            lock_unpoison(&self.detached_jobs).remove(job_id);
            self.start_available_queued();
        }
        Ok(terminal)
    }

    pub(crate) fn start_detached_process_job(&self, start: PendingJobStart) {
        let PendingJobStart {
            generation,
            policy,
            shell,
            project_registry_dir,
            metadata,
            operation,
            ..
        } = start;
        let job_id = operation.job_id().to_string();
        if !matches!(operation, RunnerJobOperation::StartDetachedProcess(_)) {
            unreachable!("detached Job starter received non-detached operation");
        }
        let (stop_requested, runner_instance_id) = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            if self.shutting_down.load(Ordering::SeqCst) {
                (None, None)
            } else {
                let mut jobs = lock_unpoison(&self.jobs);
                let Some(job) = jobs.get_mut(&job_id) else {
                    return;
                };
                job.slot_reserved = true;
                (
                    Some(Arc::clone(&job.stop_requested)),
                    Some(job.runner_instance_id.clone()),
                )
            }
        };
        let (Some(stop_requested), Some(runner_instance_id)) = (stop_requested, runner_instance_id)
        else {
            self.shutdown_rejection(&operation);
            return;
        };
        let manager = self.clone_for_worker();
        let worker_guard = self.workers.enter();
        std::thread::spawn(move || {
            let _worker_guard = worker_guard;
            let RunnerJobOperation::StartDetachedProcess(request) = &operation else {
                unreachable!("detached Job starter received non-detached operation");
            };
            let prepared = match prepare_detached_process_launch(
                generation,
                &policy,
                &shell,
                &project_registry_dir,
                &manager.prepared_profiles,
                request.cwd.as_deref(),
                &request.process.executable,
                &request.process.args,
                request.timeout_secs,
                Some(stop_requested.as_ref()),
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    manager.fail_job(&operation, error, None);
                    manager.start_available_queued();
                    return;
                }
            };
            if stop_requested.load(Ordering::SeqCst) || manager.shutting_down.load(Ordering::SeqCst)
            {
                manager.update_and_send(
                    &job_id,
                    RunnerJobDelta {
                        status: "stopped".to_string(),
                        duration_ms: Some(0),
                        error: Some(
                            "detached process Job stopped before ownership acceptance".to_string(),
                        ),
                        command_execution_state: Some(ShellCommandExecutionState::NotStarted),
                        finished: true,
                        ..Default::default()
                    },
                );
                manager.start_available_queued();
                return;
            }
            let store = match manager.detached_store_for_start(&metadata.client_id) {
                Ok(store) => store,
                Err(error) => {
                    manager.fail_job(&operation, error, None);
                    manager.start_available_queued();
                    return;
                }
            };
            let detached_request = DetachedStartRequest {
                job_id: job_id.clone(),
                request_id: metadata.request_id.clone(),
                client_id: metadata.client_id.clone(),
                runner_instance_id,
                context: request.context.clone(),
                launch: DetachedLaunchSpec {
                    process: prepared.process,
                    cwd: Some(prepared.cwd),
                    stdin: request.stdin.clone(),
                    env: prepared.env,
                    timeout_secs: prepared.timeout_secs,
                },
            };
            let outcome = match handoff_detached_job(&store, detached_request) {
                Ok(outcome) => outcome,
                Err(error) => {
                    match store.read(&job_id) {
                        Ok(record) => {
                            if let Err(sync_error) = manager.sync_detached_record(&job_id, &record)
                            {
                                tracing::error!(target: "webcodex_runner",job_id = %job_id, error = %sync_error, "detached Job failed-start durable sync failed closed");
                            }
                        }
                        Err(_) => manager.fail_job(&operation, error, None),
                    }
                    manager.start_available_queued();
                    return;
                }
            };
            let (execution_id, record, observe) = match outcome {
                DetachedHandoffOutcome::Accepted {
                    execution_id,
                    record,
                    ..
                }
                | DetachedHandoffOutcome::Existing {
                    execution_id,
                    record,
                }
                | DetachedHandoffOutcome::OutcomeUnknown {
                    execution_id,
                    record,
                } => (execution_id, record, true),
                DetachedHandoffOutcome::PreAcceptFailed {
                    execution_id,
                    record,
                } => (execution_id, record, false),
            };
            let detached = DetachedJobRef {
                store: store.clone(),
                execution_id,
            };
            if observe {
                lock_unpoison(&manager.detached_jobs).insert(job_id.clone(), detached.clone());
            }
            match manager.sync_detached_record(&job_id, &record) {
                Ok(terminal) if !terminal && observe => {
                    if let Err(error) = manager.spawn_detached_observer(job_id.clone(), detached) {
                        tracing::error!(target: "webcodex_runner",job_id = %job_id, error = %error, "detached Job observer startup failed after ownership handoff; durable control retained");
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(target: "webcodex_runner",job_id = %job_id, error = %error, "detached Job durable sync failed after ownership handoff; durable control retained");
                }
            }
            manager.start_available_queued();
        });
    }
}
