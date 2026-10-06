//! Runner jobs lifecycle implementation.

use crate::webcodex_runner::jobs::process::{
    managed_tree_running, reap_managed_direct_child, request_terminate_managed_tree,
    terminate_managed_tree, wait_managed_tree_exit,
};
use crate::webcodex_runner::jobs::{JobManager, RunnerJobDelta};
use crate::webcodex_runner::shutdown::lock_unpoison;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use webcodex_core::runner_job_lifecycle::RunnerJobLifecycle;
use webcodex_core::runner_operation::RunnerJobOperation;
use webcodex_core::runner_protocol::{RunnerRequest, ShellCommandExecutionState};
use webcodex_process::ManagedChild;

#[derive(Clone)]
pub(crate) struct JobShutdownTarget {
    pub(crate) child: Arc<Mutex<ManagedChild>>,
}

pub(crate) struct JobShutdownBatch {
    pub(crate) targets: Vec<JobShutdownTarget>,
    pub(crate) running: usize,
    pub(crate) failures: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct JobShutdownOutcome {
    pub(crate) resources: usize,
    pub(crate) timed_out: usize,
    pub(crate) failures: usize,
}

pub(crate) fn shutdown_target_running(target: &mut JobShutdownTarget) -> bool {
    if managed_tree_running(&target.child) {
        return true;
    }
    // Tree liveness and direct-child reaping are distinct on Unix. Darwin can
    // prove a zombie-only process group non-executable just before waitpid makes
    // the direct child's status observable. Keep the shutdown target pending
    // within the existing global deadline until that direct child is reaped;
    // do not re-signal the already-confirmed-empty process group.
    !reap_managed_direct_child(&target.child, Instant::now()).unwrap_or(false)
}

pub(crate) fn runner_job_is_terminal(status: &str) -> bool {
    RunnerJobLifecycle::from_wire(status).is_ok_and(RunnerJobLifecycle::is_terminal)
}

pub(crate) fn runner_job_is_active(status: &str) -> bool {
    RunnerJobLifecycle::from_wire(status).is_ok_and(RunnerJobLifecycle::is_runner_active)
}

pub(crate) fn job_prestart_lifecycle(
    operation: &RunnerJobOperation,
) -> Option<ShellCommandExecutionState> {
    match operation {
        RunnerJobOperation::StartShell(_)
        | RunnerJobOperation::StartProcess(_)
        | RunnerJobOperation::StartDetachedProcess(_)
        | RunnerJobOperation::StartScript(_) => Some(ShellCommandExecutionState::NotStarted),
        RunnerJobOperation::StartValidation(_) | RunnerJobOperation::Stop { .. } => None,
    }
}

/// Compatibility-only lifecycle projection for malformed V2 Job requests that
/// fail before a canonical operation can be constructed. Production Job
/// execution never uses this string registry.
pub(crate) fn decode_failure_prestart_lifecycle(
    request: &RunnerRequest,
) -> Option<ShellCommandExecutionState> {
    matches!(
        request.kind.as_str(),
        "start_job" | "start_process_job" | "start_detached_process_job" | "start_script_job"
    )
    .then_some(ShellCommandExecutionState::NotStarted)
}

pub(crate) fn post_spawn_interruption_lifecycle(
    operation: &RunnerJobOperation,
) -> Option<ShellCommandExecutionState> {
    matches!(operation, RunnerJobOperation::StartShell(_))
        .then_some(ShellCommandExecutionState::OutcomeUnknown)
}

pub(crate) fn post_spawn_interruption_reason(
    shutting_down: bool,
    stop_requested: bool,
    job_record_present: bool,
) -> Option<&'static str> {
    if shutting_down {
        Some("runner began shutdown after command start")
    } else if stop_requested {
        Some("job stop requested after command start")
    } else if !job_record_present {
        Some("runner lost the Job record after command start")
    } else {
        None
    }
}

pub(crate) fn post_spawn_interruption_delta(
    operation: &RunnerJobOperation,
    duration_ms: u64,
    error: &str,
) -> RunnerJobDelta {
    RunnerJobDelta {
        status: "failed".to_string(),
        exit_code: None,
        duration_ms: Some(duration_ms),
        error: Some(error.to_string()),
        command_execution_state: post_spawn_interruption_lifecycle(operation),
        finished: true,
        ..Default::default()
    }
}

pub(crate) fn raw_shell_job_terminal_lifecycle(
    status: &str,
    exit_code: Option<i32>,
) -> ShellCommandExecutionState {
    match RunnerJobLifecycle::from_wire(status).ok() {
        Some(lifecycle) if lifecycle.is_timed_out() => ShellCommandExecutionState::TimedOut,
        Some(
            RunnerJobLifecycle::Completed
            | RunnerJobLifecycle::Stopped
            | RunnerJobLifecycle::Cancelled,
        ) => ShellCommandExecutionState::Completed,
        Some(RunnerJobLifecycle::Failed) if exit_code.is_some() => {
            ShellCommandExecutionState::Completed
        }
        _ => ShellCommandExecutionState::OutcomeUnknown,
    }
}

impl JobManager {
    pub(crate) fn has_work(&self) -> bool {
        let detached_ids = lock_unpoison(&self.detached_jobs)
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        lock_unpoison(&self.jobs).iter().any(|(job_id, job)| {
            runner_job_is_active(&job.snapshot.status) && !detached_ids.contains(job_id.as_str())
        }) || !lock_unpoison(&self.queued).is_empty()
    }

    pub(crate) fn stop_accepting_work(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    pub(crate) fn cancel_queued_for_shutdown(&self) -> usize {
        let _lifecycle = lock_unpoison(&self.lifecycle);
        self.shutting_down.store(true, Ordering::SeqCst);
        let mut queued = lock_unpoison(&self.queued);
        let cancelled = queued.len();
        queued.clear();
        cancelled
    }

    pub(crate) fn signal_all_for_shutdown(&self) -> JobShutdownBatch {
        let detached_ids = lock_unpoison(&self.detached_jobs)
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let running = {
            let jobs = lock_unpoison(&self.jobs);
            jobs.iter()
                .filter(|(job_id, job)| {
                    runner_job_is_active(&job.snapshot.status)
                        && !detached_ids.contains(job_id.as_str())
                })
                .map(|(_, job)| (job.child.clone(), Arc::clone(&job.stop_requested)))
                .collect::<Vec<_>>()
        };
        let running_count = running.len();
        let mut targets = Vec::with_capacity(running.len());
        let mut failures = 0;
        for (child, stop_requested) in running {
            stop_requested.store(true, Ordering::SeqCst);
            let Some(child) = child else {
                continue;
            };
            // Graceful tree termination where supported (SIGTERM on Unix);
            // Windows has no graceful Job Object signal and escalates to a
            // force terminate immediately.
            if request_terminate_managed_tree(&child).is_err() {
                failures += 1;
            }
            targets.push(JobShutdownTarget { child });
        }
        JobShutdownBatch {
            running: running_count,
            targets,
            failures,
        }
    }

    pub(crate) fn drain_shutdown(
        &self,
        mut batch: JobShutdownBatch,
        deadline: Instant,
    ) -> JobShutdownOutcome {
        const TERM_GRACE: Duration = Duration::from_millis(500);
        let resources = batch.targets.len();
        let grace_deadline = deadline.min(Instant::now() + TERM_GRACE);
        // Phase 1: after the graceful request, wait up to the grace window for
        // each managed tree to empty on its own.
        while Instant::now() < grace_deadline {
            if batch
                .targets
                .iter_mut()
                .all(|target| !shutdown_target_running(target))
            {
                break;
            }
            let remaining = grace_deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(Duration::from_millis(10).min(remaining));
        }

        // Phase 2: force-terminate every tree that is still alive.
        for target in &mut batch.targets {
            if managed_tree_running(&target.child) && terminate_managed_tree(&target.child).is_err()
            {
                batch.failures += 1;
            }
        }

        // Phase 3: wait out the remaining budget for all trees to empty.
        while Instant::now() < deadline {
            if batch
                .targets
                .iter_mut()
                .all(|target| !shutdown_target_running(target))
            {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(Duration::from_millis(10).min(remaining));
        }
        let mut timed_out = 0;
        for target in &mut batch.targets {
            timed_out += usize::from(shutdown_target_running(target));
        }
        JobShutdownOutcome {
            resources,
            timed_out,
            failures: batch.failures,
        }
    }

    #[cfg(test)]
    pub(crate) fn stop_all(&self) {
        self.stop_accepting_work();
        self.cancel_queued_for_shutdown();
        let batch = self.signal_all_for_shutdown();
        let outcome = self.drain_shutdown(batch, Instant::now() + Duration::from_secs(2));
        if outcome.timed_out > 0 || outcome.failures > 0 {
            eprintln!(
                "webcodex-runner shutdown job cleanup incomplete resources={} timed_out={} failures={}",
                outcome.resources, outcome.timed_out, outcome.failures
            );
        }
    }

    pub(crate) fn wait_for_workers(&self, deadline: Instant) -> bool {
        self.workers.wait_until(deadline)
    }

    pub(crate) fn worker_count(&self) -> usize {
        self.workers.active()
    }

    pub(crate) fn stop(&self, job_id: &str) -> Result<(), String> {
        let queued_job = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let mut queued = lock_unpoison(&self.queued);
            if let Some(pos) = queued
                .iter()
                .position(|queued_start| queued_start.operation.job_id() == job_id)
            {
                queued.remove(pos)
            } else {
                None
            }
        };
        if let Some(queued_start) = queued_job {
            let operation = queued_start.operation;
            self.update_and_send(
                job_id,
                RunnerJobDelta {
                    status: "stopped".to_string(),
                    stderr_chunk: Some("job stopped before start".to_string()),
                    exit_code: Some(-1),
                    duration_ms: Some(0),
                    error: Some("job stopped before start".to_string()),
                    command_execution_state: job_prestart_lifecycle(&operation),
                    finished: true,
                    ..Default::default()
                },
            );
            self.start_available_queued();
            return Ok(());
        }
        {
            let jobs = lock_unpoison(&self.jobs);
            let Some(job) = jobs.get(job_id) else {
                return Err(format!("unknown local job: {}", job_id));
            };
            if runner_job_is_terminal(&job.snapshot.status) {
                drop(jobs);
                // A stop can race a terminal update that failed in transport.
                // Replay the retained terminal snapshot with its original
                // sequence so the server converges instead of remaining
                // `stop_requested`.
                self.resend_snapshot(job_id);
                return Ok(());
            }
        }
        let detached = {
            let detached_jobs = lock_unpoison(&self.detached_jobs);
            detached_jobs.get(job_id).cloned()
        };
        if let Some(detached) = detached {
            let record = detached
                .store
                .request_stop(job_id, &detached.execution_id)?;
            self.sync_detached_record(job_id, &record)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let record = detached.store.read(job_id)?;
                let record = detached
                    .store
                    .reconcile_after_runner_restart(record)?
                    .ok_or_else(|| {
                        format!("detached Job {job_id} regressed before ownership acceptance")
                    })?;
                let terminal = self.sync_detached_record(job_id, &record)?;
                if terminal {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "detached Job {job_id} stop was durably requested but terminal state was not observed within the bounded deadline"
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let (child, stop_requested) = {
            let jobs = lock_unpoison(&self.jobs);
            let job = jobs
                .get(job_id)
                .ok_or_else(|| format!("unknown local job: {job_id}"))?;
            (job.child.clone(), job.stop_requested.clone())
        };
        stop_requested.store(true, Ordering::SeqCst);
        self.update_and_send(
            job_id,
            RunnerJobDelta {
                status: "stop_requested".to_string(),
                error: Some("stop requested".to_string()),
                ..Default::default()
            },
        );
        if let Some(child) = child {
            let deadline = Instant::now() + Duration::from_secs(1);
            if let Err(e) = terminate_managed_tree(&child) {
                return Err(format!("failed to kill job {}: {}", job_id, e));
            }
            match wait_managed_tree_exit(&child, deadline) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(format!(
                        "failed to kill job {}: job tree did not exit within the bounded stop deadline",
                        job_id
                    ));
                }
                Err(e) => {
                    return Err(format!("failed to wait for job {} tree: {}", job_id, e));
                }
            }
            // Best-effort reap of the direct child so a Unix parent killed by
            // termination is not left as a zombie for the worker to discover
            // late.
            let _ = reap_managed_direct_child(&child, deadline);
            Ok(())
        } else {
            Ok(())
        }
    }
}
