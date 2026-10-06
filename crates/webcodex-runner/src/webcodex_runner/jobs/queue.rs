//! Runner jobs queue implementation.

use crate::webcodex_runner::jobs::context::validate_runner_job_context_operation;
use crate::webcodex_runner::jobs::lifecycle::{job_prestart_lifecycle, runner_job_is_active};
use crate::webcodex_runner::jobs::{JobManager, PendingJobStart, RunnerJobDelta, RunningJob};
use crate::webcodex_runner::shutdown::lock_unpoison;
use crate::webcodex_runner::RunnerSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use webcodex_core::runner_operation::RunnerJobOperation;
use webcodex_core::runner_protocol::{
    RunnerJobUpdateRequest, ShellJobSnapshot, ShellJobStreamSnapshot, JOB_INVENTORY_MAX_ACTIVE_JOBS,
};

impl JobManager {
    pub(crate) fn shutdown_rejection(&self, operation: &RunnerJobOperation) {
        self.fail_job(operation, "runner is shutting down".to_string(), None);
    }

    pub(crate) fn enqueue(&self, sink: RunnerSink, start: PendingJobStart) {
        if !start.operation.is_start() {
            return;
        }
        let job_id = start.operation.job_id().to_string();
        let Some(context) = start.operation.context().cloned() else {
            return;
        };
        if let Err(error) =
            validate_runner_job_context_operation(&context, &start.operation, sink.client_id())
        {
            let command_execution_state = job_prestart_lifecycle(&start.operation);
            let _ = sink.send_job_update(&RunnerJobUpdateRequest {
                client_id: sink.client_id().to_string(),
                runner_instance_id: sink.runner_instance_id().to_string(),
                job_id,
                request_id: Some(start.metadata.request_id.clone()),
                update_seq: Some(1),
                status: "failed".to_string(),
                stdout_chunk: None,
                stderr_chunk: None,
                stdout_tail: None,
                stderr_tail: None,
                log_snapshot: None,
                exit_code: None,
                duration_ms: Some(0),
                error: Some(error),
                command_execution_state,
                validation_progress: None,
                activity: None,
                finished: true,
            });
            return;
        }
        self.install_sink(sink.clone());
        let client_id = sink.client_id().to_string();
        let runner_instance_id = sink.runner_instance_id().to_string();
        let (queue_locally, immediate_failure) = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let shutting_down = self.shutting_down.load(Ordering::SeqCst);
            let mut jobs = lock_unpoison(&self.jobs);
            if jobs.contains_key(&job_id) {
                return;
            }
            let active_count = jobs
                .values()
                .filter(|job| runner_job_is_active(&job.snapshot.status))
                .count();
            let reserved = jobs
                .values()
                .filter(|job| {
                    job.client_id == client_id
                        && job.slot_reserved
                        && runner_job_is_active(&job.snapshot.status)
                })
                .count();
            let inventory_full = active_count >= JOB_INVENTORY_MAX_ACTIVE_JOBS;
            let immediate_failure = if inventory_full {
                Some(format!(
                    "runner active job inventory limit reached ({})",
                    JOB_INVENTORY_MAX_ACTIVE_JOBS
                ))
            } else if shutting_down {
                Some("runner is shutting down".to_string())
            } else {
                None
            };
            let queue_locally = immediate_failure.is_none() && reserved >= self.max_concurrent;
            let slot_reserved = immediate_failure.is_none() && !queue_locally;
            let now = chrono::Utc::now().timestamp();
            let terminal = immediate_failure.is_some();
            jobs.insert(
                job_id.clone(),
                RunningJob {
                    client_id: client_id.clone(),
                    runner_instance_id,
                    snapshot: ShellJobSnapshot {
                        job_id: job_id.clone(),
                        request_id: start.metadata.request_id.clone(),
                        status: if terminal {
                            "failed".to_string()
                        } else {
                            "agent_queued".to_string()
                        },
                        update_seq: u64::from(terminal),
                        created_at: start.metadata.created_at,
                        started_at: None,
                        ended_at: terminal.then_some(now),
                        exit_code: None,
                        duration_ms: terminal.then_some(0),
                        error: immediate_failure.clone(),
                        command_execution_state: terminal
                            .then(|| job_prestart_lifecycle(&start.operation))
                            .flatten(),
                        context,
                        stdout: ShellJobStreamSnapshot::default(),
                        stderr: ShellJobStreamSnapshot::default(),
                        validation_progress: None,
                        activity: None,
                    },
                    child: None,
                    stop_requested: Arc::new(AtomicBool::new(false)),
                    slot_reserved,
                },
            );
            drop(jobs);
            if queue_locally {
                lock_unpoison(&self.queued).push_back(start.clone());
            }
            (queue_locally, immediate_failure)
        };
        if let Some(error) = immediate_failure {
            debug_assert!(!error.is_empty());
            self.resend_snapshot(&job_id);
            self.prune_terminal_records();
            return;
        }
        self.update_and_send(
            &job_id,
            RunnerJobDelta {
                status: "agent_queued".to_string(),
                ..Default::default()
            },
        );
        if queue_locally {
            return;
        }
        self.start_now(start);
    }

    pub(crate) fn start_now(&self, start: PendingJobStart) {
        if self.shutting_down.load(Ordering::SeqCst) {
            self.shutdown_rejection(&start.operation);
            return;
        }
        match &start.operation {
            RunnerJobOperation::StartDetachedProcess(_) => self.start_detached_process_job(start),
            RunnerJobOperation::StartProcess(_) | RunnerJobOperation::StartScript(_) => {
                self.start_structured_job(start)
            }
            RunnerJobOperation::StartShell(_) | RunnerJobOperation::StartValidation(_) => {
                self.start_shell_job(start)
            }
            RunnerJobOperation::Stop { .. } => {
                unreachable!("stop Job operation cannot enter the start queue")
            }
        }
    }

    pub(crate) fn start_available_queued(&self) {
        loop {
            if self.shutting_down.load(Ordering::SeqCst) {
                lock_unpoison(&self.queued).clear();
                return;
            }
            let next = {
                let _lifecycle = lock_unpoison(&self.lifecycle);
                if self.shutting_down.load(Ordering::SeqCst) {
                    lock_unpoison(&self.queued).clear();
                    return;
                }
                let mut jobs = lock_unpoison(&self.jobs);
                let mut queued = lock_unpoison(&self.queued);
                let mut selected = None;
                for (idx, queued_start) in queued.iter().enumerate() {
                    let reserved = jobs
                        .values()
                        .filter(|job| {
                            job.client_id == queued_start.metadata.client_id
                                && job.slot_reserved
                                && runner_job_is_active(&job.snapshot.status)
                        })
                        .count();
                    if reserved < self.max_concurrent {
                        selected = Some(idx);
                        break;
                    }
                }
                if let Some(idx) = selected {
                    let job_id = queued[idx].operation.job_id();
                    if let Some(job) = jobs.get_mut(job_id) {
                        job.slot_reserved = true;
                    }
                    queued.remove(idx)
                } else {
                    None
                }
            };
            let Some(start) = next else {
                return;
            };
            self.start_now(start);
        }
    }
}
