//! Runner jobs updates implementation.

use crate::webcodex_runner::jobs::lifecycle::{
    job_prestart_lifecycle, runner_job_is_active, runner_job_is_terminal,
};
use crate::webcodex_runner::jobs::{JobManager, RunnerJobDelta, RunningJob};
use crate::webcodex_runner::shutdown::lock_unpoison;
use crate::webcodex_runner::RunnerSink;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;
use webcodex_core::runner_job_lifecycle::RunnerJobLifecycle;
use webcodex_core::runner_operation::RunnerJobOperation;
use webcodex_core::runner_protocol::{
    RunnerJobUpdateRequest, ShellCommandExecutionState, ShellJobActivity, ShellJobActivityPhase,
    ShellJobActivitySource, ShellJobActivityState, ShellJobInventory, ShellJobLogSnapshot,
    ShellJobSnapshot, ShellJobStreamSnapshot, ShellJobValidationProgress, ShellJobValidationStep,
    JOB_INVENTORY_MAX_SERIALIZED_BYTES, JOB_INVENTORY_MAX_TERMINAL_JOBS,
    JOB_SNAPSHOT_STREAM_MAX_BYTES, JOB_TERMINAL_RETENTION_SECS,
};

pub(crate) const JOB_UPDATE_INTERVAL_MS: u64 = 250;

/// At most the validated Job state machine's required semantic transitions are
/// retained for live delivery. Output and advisory current-activity-only
/// updates are coalesced separately below.
pub(crate) const JOB_UPDATE_REQUIRED_PENDING_MAX: usize = 8;

pub(crate) const JOB_UPDATE_DELIVERY_RETRY: Duration =
    Duration::from_millis(JOB_UPDATE_INTERVAL_MS);

#[derive(Debug, Default)]
pub(crate) struct JobUpdateDeliverySignalState {
    pub(crate) generation: u64,
    pub(crate) closed: bool,
}

#[derive(Debug, Default)]
pub(crate) struct JobUpdateDeliverySignal {
    pub(crate) state: Mutex<JobUpdateDeliverySignalState>,
    pub(crate) wake: Condvar,
}

impl JobUpdateDeliverySignal {
    pub(crate) fn notify(&self) {
        let mut state = lock_unpoison(&self.state);
        state.generation = state.generation.saturating_add(1);
        self.wake.notify_one();
    }

    pub(crate) fn close(&self) {
        let mut state = lock_unpoison(&self.state);
        state.closed = true;
        state.generation = state.generation.saturating_add(1);
        self.wake.notify_all();
    }

    pub(crate) fn generation(&self) -> u64 {
        lock_unpoison(&self.state).generation
    }

    pub(crate) fn wait_for_change(&self, observed: u64, timeout: Duration) -> Option<u64> {
        let mut state = lock_unpoison(&self.state);
        if state.closed {
            return None;
        }
        if state.generation == observed {
            let (next, _) = self
                .wake
                .wait_timeout(state, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
        (!state.closed).then_some(state.generation)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingJobUpdateDelivery {
    pub(crate) update_seq: u64,
    pub(crate) status: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) error: Option<String>,
    pub(crate) command_execution_state: Option<ShellCommandExecutionState>,
    pub(crate) validation_progress: Option<ShellJobValidationProgress>,
    pub(crate) activity: Option<ShellJobActivity>,
    pub(crate) finished: bool,
}

impl PendingJobUpdateDelivery {
    pub(crate) fn from_update(update: &RunnerJobUpdateRequest) -> Self {
        Self {
            update_seq: update.update_seq.unwrap_or_default(),
            status: update.status.clone(),
            exit_code: update.exit_code,
            duration_ms: update.duration_ms,
            error: update.error.clone(),
            command_execution_state: update.command_execution_state.clone(),
            validation_progress: update.validation_progress.clone(),
            activity: update.activity,
            finished: update.finished,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JobUpdateDeliveryQueue {
    pub(crate) required: VecDeque<PendingJobUpdateDelivery>,
    pub(crate) output_only: Option<PendingJobUpdateDelivery>,
    pub(crate) suspended_until_reconciliation: bool,
}

impl JobUpdateDeliveryQueue {
    pub(crate) fn enqueue(&mut self, update: PendingJobUpdateDelivery, semantic: bool) -> bool {
        if self.suspended_until_reconciliation {
            return true;
        }
        if semantic {
            self.output_only = None;
            if let Some(last) = self.required.back_mut() {
                if last.update_seq == update.update_seq {
                    *last = update;
                    return true;
                }
            }
            if self.required.len() >= JOB_UPDATE_REQUIRED_PENDING_MAX {
                self.required.clear();
                self.suspended_until_reconciliation = true;
                return false;
            }
            self.required.push_back(update);
        } else {
            self.output_only = Some(update);
        }
        true
    }

    pub(crate) fn next(&self) -> Option<&PendingJobUpdateDelivery> {
        if self.suspended_until_reconciliation {
            None
        } else {
            self.required.front().or(self.output_only.as_ref())
        }
    }

    pub(crate) fn acknowledge(&mut self, update_seq: u64) {
        if self
            .required
            .front()
            .is_some_and(|update| update.update_seq == update_seq)
        {
            self.required.pop_front();
        } else if self
            .output_only
            .as_ref()
            .is_some_and(|update| update.update_seq == update_seq)
        {
            self.output_only = None;
        }
    }

    pub(crate) fn discard_through(&mut self, update_seq: u64) {
        while self
            .required
            .front()
            .is_some_and(|update| update.update_seq <= update_seq)
        {
            self.required.pop_front();
        }
        if self
            .output_only
            .as_ref()
            .is_some_and(|update| update.update_seq <= update_seq)
        {
            self.output_only = None;
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.required.is_empty() && self.output_only.is_none()
    }
}

pub(crate) fn job_update_from_delivery(
    job: &RunningJob,
    pending: &PendingJobUpdateDelivery,
) -> RunnerJobUpdateRequest {
    let mut update =
        job_update_from_snapshot(&job.client_id, &job.runner_instance_id, &job.snapshot);
    update.update_seq = Some(pending.update_seq);
    update.status = pending.status.clone();
    update.exit_code = pending.exit_code;
    update.duration_ms = pending.duration_ms;
    update.error = pending.error.clone();
    update.command_execution_state = pending.command_execution_state.clone();
    update.validation_progress = pending.validation_progress.clone();
    update.activity = pending.activity;
    update.finished = pending.finished;
    update
}

pub(crate) fn spawn_job_update_delivery_worker(
    jobs: Weak<Mutex<HashMap<String, RunningJob>>>,
    current_sink: Weak<Mutex<Option<RunnerSink>>>,
    pending_job_updates: Weak<Mutex<HashMap<String, JobUpdateDeliveryQueue>>>,
    signal: Arc<JobUpdateDeliverySignal>,
) {
    std::thread::spawn(move || {
        let mut observed_generation = signal.generation();
        loop {
            let Some(pending_map) = pending_job_updates.upgrade() else {
                break;
            };
            let candidate = {
                let pending = lock_unpoison(&pending_map);
                pending.iter().find_map(|(job_id, queue)| {
                    queue.next().cloned().map(|update| (job_id.clone(), update))
                })
            };
            let Some((job_id, pending_update)) = candidate else {
                let Some(next) =
                    signal.wait_for_change(observed_generation, Duration::from_secs(60))
                else {
                    break;
                };
                observed_generation = next;
                continue;
            };

            let Some(jobs_map) = jobs.upgrade() else {
                break;
            };
            let update = {
                let jobs = lock_unpoison(&jobs_map);
                jobs.get(&job_id)
                    .map(|job| job_update_from_delivery(job, &pending_update))
            };
            let Some(update) = update else {
                lock_unpoison(&pending_map).remove(&job_id);
                continue;
            };

            let Some(sink_slot) = current_sink.upgrade() else {
                break;
            };
            let sink = lock_unpoison(&sink_slot).clone();
            let Some(sink) = sink else {
                let Some(next) =
                    signal.wait_for_change(observed_generation, Duration::from_secs(60))
                else {
                    break;
                };
                observed_generation = next;
                continue;
            };

            let send_result = if matches!(
                &sink,
                RunnerSink::WebSocket { .. } | RunnerSink::Quic { .. }
            ) {
                // Stream delivery is a non-blocking try_send. Keep candidate
                // validation and enqueue atomic with respect to coalescing: a
                // semantic update may supersede an output/activity-only item
                // after the worker clones it but before channel capacity returns.
                // HTTP remains outside this lock because it performs a bounded
                // synchronous request and must never block update producers.
                let pending = lock_unpoison(&pending_map);
                let still_pending = pending
                    .get(&job_id)
                    .and_then(JobUpdateDeliveryQueue::next)
                    .is_some_and(|current| current.update_seq == pending_update.update_seq);
                if !still_pending {
                    continue;
                }
                sink.try_send_job_update(&update)
            } else {
                sink.try_send_job_update(&update)
            };

            match send_result {
                Ok(true) => {
                    let still_current = lock_unpoison(&sink_slot)
                        .as_ref()
                        .is_some_and(|current| current.same_job_update_target(&sink));
                    if still_current {
                        let mut pending = lock_unpoison(&pending_map);
                        let remove = if let Some(queue) = pending.get_mut(&job_id) {
                            queue.acknowledge(pending_update.update_seq);
                            queue.is_empty() && !queue.suspended_until_reconciliation
                        } else {
                            false
                        };
                        if remove {
                            pending.remove(&job_id);
                        }
                    }
                }
                Ok(false) | Err(_) => {
                    let Some(next) =
                        signal.wait_for_change(observed_generation, JOB_UPDATE_DELIVERY_RETRY)
                    else {
                        break;
                    };
                    observed_generation = next;
                }
            }
        }
    });
}

pub(crate) fn process_running_activity() -> ShellJobActivity {
    ShellJobActivity {
        state: ShellJobActivityState::Working,
        phase: ShellJobActivityPhase::ProcessRunning,
        source: ShellJobActivitySource::RunnerExecution,
    }
}

pub(crate) fn validation_step_activity(step: &ShellJobValidationStep) -> ShellJobActivity {
    let phase = match step.name.as_str() {
        "format" => ShellJobActivityPhase::ValidationFormat,
        "check" => ShellJobActivityPhase::ValidationCheck,
        "test" => ShellJobActivityPhase::ValidationTest,
        _ => unreachable!("canonical validation step name"),
    };
    ShellJobActivity {
        state: ShellJobActivityState::Working,
        phase,
        source: ShellJobActivitySource::ValidationPlan,
    }
}

/// Recognize only a tiny bounded subset of Cargo's own stderr progress while a
/// canonical structured Cargo validation step is running. Clear phase-boundary
/// lines return to the step's canonical validation-plan activity so transient
/// Cargo detail cannot remain sticky after that detail has ended. This is
/// advisory activity provenance, not validation/completion evidence.
pub(crate) fn cargo_activity_from_stderr(
    step: &ShellJobValidationStep,
    stderr: &str,
) -> Option<ShellJobActivity> {
    if step.program != "cargo" || !step.is_canonical() {
        return None;
    }
    let validation_activity = validation_step_activity(step);
    let mut observed = None;
    for line in stderr.lines() {
        let line = line.trim_start();
        let activity = if line.contains("Blocking waiting for file lock on build directory") {
            ShellJobActivity {
                state: ShellJobActivityState::Waiting,
                phase: ShellJobActivityPhase::CargoWaitingForBuildLock,
                source: ShellJobActivitySource::CargoOutput,
            }
        } else if line.starts_with("Compiling ") {
            ShellJobActivity {
                state: ShellJobActivityState::Working,
                phase: ShellJobActivityPhase::CargoCompiling,
                source: ShellJobActivitySource::CargoOutput,
            }
        } else if line.starts_with("Checking ") {
            ShellJobActivity {
                state: ShellJobActivityState::Working,
                phase: ShellJobActivityPhase::CargoChecking,
                source: ShellJobActivitySource::CargoOutput,
            }
        } else if line.starts_with("Finished ")
            || (step.name == "test"
                && (line.starts_with("Running unittests ")
                    || line.starts_with("Running tests/")
                    || line.starts_with("Running benches/")
                    || line.starts_with("Doc-tests ")))
        {
            validation_activity
        } else {
            continue;
        };
        observed = Some(activity);
    }
    observed
}

pub(crate) fn job_update_from_snapshot(
    client_id: &str,
    runner_instance_id: &str,
    snapshot: &ShellJobSnapshot,
) -> RunnerJobUpdateRequest {
    RunnerJobUpdateRequest {
        client_id: client_id.to_string(),
        runner_instance_id: runner_instance_id.to_string(),
        job_id: snapshot.job_id.clone(),
        request_id: Some(snapshot.request_id.clone()),
        update_seq: Some(snapshot.update_seq),
        status: snapshot.status.clone(),
        stdout_chunk: None,
        stderr_chunk: None,
        stdout_tail: None,
        stderr_tail: None,
        log_snapshot: Some(ShellJobLogSnapshot {
            stdout: snapshot.stdout.clone(),
            stderr: snapshot.stderr.clone(),
        }),
        exit_code: snapshot.exit_code,
        duration_ms: snapshot.duration_ms,
        error: snapshot.error.clone(),
        command_execution_state: snapshot.command_execution_state,
        validation_progress: snapshot.validation_progress.clone(),
        activity: snapshot.activity,
        finished: runner_job_is_terminal(&snapshot.status),
    }
}

pub(crate) fn runner_retained_line_count(value: &str) -> usize {
    value.lines().count()
}

pub(crate) fn append_runner_stream(stream: &mut ShellJobStreamSnapshot, chunk: Option<&str>) {
    let Some(chunk) = chunk else {
        return;
    };
    stream.tail.push_str(chunk);
    if stream.tail.len() > JOB_SNAPSHOT_STREAM_MAX_BYTES {
        let observed_next = stream
            .first_retained_line
            .saturating_add(runner_retained_line_count(&stream.tail));
        let mut minimum_start = stream.tail.len() - JOB_SNAPSHOT_STREAM_MAX_BYTES;
        while minimum_start < stream.tail.len() && !stream.tail.is_char_boundary(minimum_start) {
            minimum_start += 1;
        }
        if let Some(relative_newline) = stream.tail[minimum_start..].find('\n') {
            let drop_end = minimum_start + relative_newline + 1;
            let dropped_lines = stream.tail[..drop_end]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            stream.tail.drain(..drop_end);
            stream.first_retained_line = stream.first_retained_line.saturating_add(dropped_lines);
        } else {
            let dropped_lines = stream.tail[..minimum_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            stream.tail.drain(..minimum_start);
            stream.first_retained_line = stream.first_retained_line.saturating_add(dropped_lines);
        }
        if stream.tail.is_empty() {
            // The last retained partial line was dropped too. Preserve the
            // absolute next cursor by advancing the empty range to the
            // observed end rather than resetting it backwards.
            stream.first_retained_line = observed_next;
        }
        stream.truncated = true;
    }
    stream.next_line = stream
        .first_retained_line
        .saturating_add(runner_retained_line_count(&stream.tail));
}

pub(crate) fn trim_runner_stream_to(stream: &mut ShellJobStreamSnapshot, max_bytes: usize) {
    if stream.tail.len() <= max_bytes {
        return;
    }
    let observed_next = stream
        .first_retained_line
        .saturating_add(runner_retained_line_count(&stream.tail));
    let mut minimum_start = stream.tail.len().saturating_sub(max_bytes);
    while minimum_start < stream.tail.len() && !stream.tail.is_char_boundary(minimum_start) {
        minimum_start += 1;
    }
    if let Some(relative_newline) = stream.tail[minimum_start..].find('\n') {
        let drop_end = minimum_start + relative_newline + 1;
        let dropped_lines = stream.tail[..drop_end]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        stream.tail.drain(..drop_end);
        stream.first_retained_line = stream.first_retained_line.saturating_add(dropped_lines);
    } else {
        let dropped_lines = stream.tail[..minimum_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        stream.tail.drain(..minimum_start);
        stream.first_retained_line = stream.first_retained_line.saturating_add(dropped_lines);
    }
    if stream.tail.is_empty() {
        stream.first_retained_line = observed_next;
    }
    stream.truncated = true;
    stream.next_line = stream
        .first_retained_line
        .saturating_add(runner_retained_line_count(&stream.tail));
}

pub(crate) fn bounded_runner_error(error: Option<String>) -> Option<String> {
    error.map(|error| error.chars().take(4_096).collect())
}

/// Retain enough SSH diagnostic output to classify an uncertain transport
/// failure without allowing a chatty remote command to grow worker memory.
pub(crate) fn append_bounded_tail(target: &mut String, next: &str, max_bytes: usize) {
    target.push_str(next);
    if target.len() <= max_bytes {
        return;
    }
    let mut start = target.len().saturating_sub(max_bytes);
    while start < target.len() && !target.is_char_boundary(start) {
        start += 1;
    }
    target.drain(..start);
}

impl JobManager {
    pub(crate) fn install_sink(&self, sink: RunnerSink) {
        *lock_unpoison(&self.current_sink) = Some(sink);
        self.delivery_signal.notify();
    }

    pub(crate) fn current_sink(&self) -> Option<RunnerSink> {
        lock_unpoison(&self.current_sink).clone()
    }

    pub(crate) fn queue_recorded_update(&self, update: RunnerJobUpdateRequest, semantic: bool) {
        let job_id = update.job_id.clone();
        let accepted = lock_unpoison(&self.pending_job_updates)
            .entry(job_id.clone())
            .or_default()
            .enqueue(PendingJobUpdateDelivery::from_update(&update), semantic);
        if !accepted {
            tracing::warn!(target: "webcodex_runner",
                job_id = %job_id,
                limit = JOB_UPDATE_REQUIRED_PENDING_MAX,
                "runner job live delivery backlog exceeded its semantic bound; waiting for reconciliation"
            );
        }
        self.delivery_signal.notify();
    }

    pub(crate) fn record_update(
        &self,
        job_id: &str,
        mut delta: RunnerJobDelta,
    ) -> Option<(RunnerJobUpdateRequest, bool)> {
        let (update, semantic) = {
            let mut jobs = lock_unpoison(&self.jobs);
            let job = jobs.get_mut(job_id)?;
            if runner_job_is_terminal(&job.snapshot.status) {
                // The first locally observed terminal outcome is immutable.
                // In particular, a racing stop request or late output poll
                // must not revive a handle-free retained record.
                return None;
            }
            let previous_status = job.snapshot.status.clone();
            let previous_progress = job.snapshot.validation_progress.clone();
            let explicit_semantic =
                delta.finished || delta.command_execution_state.is_some() || delta.error.is_some();
            let now = chrono::Utc::now().timestamp();
            append_runner_stream(&mut job.snapshot.stdout, delta.stdout_chunk.as_deref());
            append_runner_stream(&mut job.snapshot.stderr, delta.stderr_chunk.as_deref());
            if let Some(max_bytes) = delta.stream_limit_bytes {
                let max_bytes = max_bytes.min(JOB_SNAPSHOT_STREAM_MAX_BYTES);
                trim_runner_stream_to(&mut job.snapshot.stdout, max_bytes);
                trim_runner_stream_to(&mut job.snapshot.stderr, max_bytes);
            }
            job.snapshot.update_seq = job.snapshot.update_seq.saturating_add(1);
            if !delta.status.trim().is_empty() {
                let incoming_status = delta.status.trim();
                let current_lifecycle = RunnerJobLifecycle::from_wire(&job.snapshot.status).ok();
                let incoming_lifecycle = RunnerJobLifecycle::from_wire(incoming_status).ok();
                let would_regress_stop = current_lifecycle
                    == Some(RunnerJobLifecycle::StopRequested)
                    && matches!(
                        incoming_lifecycle,
                        Some(RunnerJobLifecycle::RunnerQueued | RunnerJobLifecycle::Running)
                    );
                let would_regress_running = current_lifecycle == Some(RunnerJobLifecycle::Running)
                    && incoming_lifecycle == Some(RunnerJobLifecycle::RunnerQueued);
                if !would_regress_stop && !would_regress_running {
                    job.snapshot.status = incoming_status.to_string();
                }
            }
            if delta.command_execution_state.is_some() {
                job.snapshot.command_execution_state = delta.command_execution_state;
            }
            if job.snapshot.started_at.is_none()
                && job.snapshot.command_execution_state
                    != Some(ShellCommandExecutionState::NotStarted)
                && RunnerJobLifecycle::from_wire(&job.snapshot.status).is_ok_and(|lifecycle| {
                    matches!(
                        lifecycle,
                        RunnerJobLifecycle::Running
                            | RunnerJobLifecycle::Completed
                            | RunnerJobLifecycle::Failed
                            | RunnerJobLifecycle::Stopped
                            | RunnerJobLifecycle::Timeout
                            | RunnerJobLifecycle::TimedOut
                            | RunnerJobLifecycle::Cancelled
                    )
                })
            {
                job.snapshot.started_at = Some(now);
            }
            if delta.validation_progress.is_some() {
                job.snapshot.validation_progress = delta.validation_progress.clone();
            }
            if let Some(activity) = delta.activity {
                debug_assert!(activity.is_canonical());
                job.snapshot.activity = Some(activity);
            }
            if runner_job_is_terminal(&job.snapshot.status) || delta.finished {
                job.snapshot.activity = None;
                job.snapshot.ended_at.get_or_insert(now);
                job.snapshot.exit_code = delta.exit_code;
                job.snapshot.duration_ms = delta.duration_ms;
                job.snapshot.error = bounded_runner_error(delta.error.take());
                job.child = None;
                job.slot_reserved = false;
            } else if delta.error.is_some() {
                job.snapshot.error = bounded_runner_error(delta.error.take());
            }
            let semantic = explicit_semantic
                || job.snapshot.status != previous_status
                || job.snapshot.validation_progress != previous_progress;
            // Each sequenced update carries the current authoritative bounded
            // tails and activity. Delivery may coalesce output/activity-only
            // attempts, but required semantic markers preserve their sequence
            // while using the latest retained authoritative snapshot at send
            // time.
            (
                job_update_from_snapshot(&job.client_id, &job.runner_instance_id, &job.snapshot),
                semantic,
            )
        };
        self.prune_terminal_records();
        Some((update, semantic))
    }

    pub(crate) fn update_and_send(&self, job_id: &str, delta: RunnerJobDelta) {
        if let Some((update, semantic)) = self.record_update(job_id, delta) {
            self.queue_recorded_update(update, semantic);
        }
    }

    pub(crate) fn replay_snapshots_since(&self, registered: &ShellJobInventory) {
        if self.current_sink().is_none() {
            return;
        }
        let registered_by_job = registered
            .jobs
            .iter()
            .map(|snapshot| (snapshot.job_id.as_str(), snapshot))
            .collect::<HashMap<_, _>>();
        let snapshots = self.inventory().jobs;
        let mut pending = lock_unpoison(&self.pending_job_updates);
        let mut remove = Vec::new();
        for snapshot in snapshots {
            let registered_snapshot = registered_by_job.get(snapshot.job_id.as_str()).copied();
            let registered_seq = registered_snapshot.map(|item| item.update_seq).unwrap_or(0);
            let queue = pending.entry(snapshot.job_id.clone()).or_default();
            queue.discard_through(registered_seq);

            if queue.suspended_until_reconciliation {
                if registered_seq >= snapshot.update_seq {
                    queue.suspended_until_reconciliation = false;
                } else {
                    continue;
                }
            }

            if snapshot.update_seq > registered_seq && queue.is_empty() {
                let replay_safe = if snapshot.context.validation_steps.is_empty() {
                    true
                } else {
                    let previous_completed = registered_snapshot
                        .and_then(|item| item.validation_progress.as_ref())
                        .map(|progress| progress.completed)
                        .unwrap_or(0);
                    let current_completed = snapshot
                        .validation_progress
                        .as_ref()
                        .map(|progress| progress.completed)
                        .unwrap_or(0);
                    current_completed <= previous_completed.saturating_add(1)
                };
                if replay_safe {
                    let marker = PendingJobUpdateDelivery {
                        update_seq: snapshot.update_seq,
                        status: snapshot.status.clone(),
                        exit_code: snapshot.exit_code,
                        duration_ms: snapshot.duration_ms,
                        error: snapshot.error.clone(),
                        command_execution_state: snapshot.command_execution_state.clone(),
                        validation_progress: snapshot.validation_progress.clone(),
                        activity: snapshot.activity,
                        finished: runner_job_is_terminal(&snapshot.status),
                    };
                    let _ = queue.enqueue(marker, true);
                } else {
                    queue.suspended_until_reconciliation = true;
                }
            }
            if queue.is_empty() && !queue.suspended_until_reconciliation {
                remove.push(snapshot.job_id);
            }
        }
        for job_id in remove {
            pending.remove(&job_id);
        }
        drop(pending);
        self.delivery_signal.notify();
    }

    pub(crate) fn resend_snapshot(&self, job_id: &str) {
        let update = lock_unpoison(&self.jobs).get(job_id).map(|job| {
            job_update_from_snapshot(&job.client_id, &job.runner_instance_id, &job.snapshot)
        });
        if let Some(update) = update {
            self.queue_recorded_update(update, true);
        }
    }

    pub(crate) fn fail_job(
        &self,
        operation: &RunnerJobOperation,
        error: String,
        validation_progress: Option<ShellJobValidationProgress>,
    ) {
        let job_id = operation.job_id();
        self.update_and_send(
            job_id,
            RunnerJobDelta {
                status: "failed".to_string(),
                duration_ms: Some(0),
                error: Some(error),
                command_execution_state: job_prestart_lifecycle(operation),
                validation_progress,
                finished: true,
                ..Default::default()
            },
        );
        self.start_available_queued();
    }

    pub(crate) fn prune_terminal_records(&self) {
        let now = chrono::Utc::now().timestamp();
        let removed = {
            let mut jobs = lock_unpoison(&self.jobs);
            let mut removed = jobs
                .iter()
                .filter(|(_, job)| {
                    runner_job_is_terminal(&job.snapshot.status)
                        && job.snapshot.ended_at.is_some_and(|ended| {
                            now.saturating_sub(ended) >= JOB_TERMINAL_RETENTION_SECS
                        })
                })
                .map(|(job_id, _)| job_id.clone())
                .collect::<Vec<_>>();
            for job_id in &removed {
                jobs.remove(job_id);
            }
            let mut terminal = jobs
                .iter()
                .filter(|(_, job)| runner_job_is_terminal(&job.snapshot.status))
                .map(|(job_id, job)| {
                    (
                        job_id.clone(),
                        job.snapshot.ended_at.unwrap_or(job.snapshot.created_at),
                    )
                })
                .collect::<Vec<_>>();
            terminal.sort_by_key(|(_, ended_at)| *ended_at);
            let excess = terminal
                .len()
                .saturating_sub(JOB_INVENTORY_MAX_TERMINAL_JOBS);
            for (job_id, _) in terminal.into_iter().take(excess) {
                jobs.remove(&job_id);
                removed.push(job_id);
            }
            removed
        };
        if !removed.is_empty() {
            let mut pending = lock_unpoison(&self.pending_job_updates);
            let mut detached = lock_unpoison(&self.detached_jobs);
            for job_id in removed {
                pending.remove(&job_id);
                detached.remove(&job_id);
            }
        }
    }

    pub(crate) fn inventory(&self) -> ShellJobInventory {
        self.prune_terminal_records();
        let jobs = lock_unpoison(&self.jobs);
        let mut active = jobs
            .values()
            .filter(|job| runner_job_is_active(&job.snapshot.status))
            .map(|job| job.snapshot.clone())
            .collect::<Vec<_>>();
        let mut terminal = jobs
            .values()
            .filter(|job| runner_job_is_terminal(&job.snapshot.status))
            .map(|job| job.snapshot.clone())
            .collect::<Vec<_>>();
        drop(jobs);
        active.sort_by_key(|snapshot| snapshot.created_at);
        terminal.sort_by(|left, right| {
            right
                .ended_at
                .unwrap_or(right.created_at)
                .cmp(&left.ended_at.unwrap_or(left.created_at))
        });
        terminal.truncate(JOB_INVENTORY_MAX_TERMINAL_JOBS);
        let mut inventory = ShellJobInventory {
            active_complete: true,
            jobs: active,
        };

        // Active records are never omitted. Only when active records alone
        // exceed the frame budget do their authoritative tails shrink.
        let mut tail_limit = JOB_SNAPSHOT_STREAM_MAX_BYTES;
        while serde_json::to_vec(&inventory)
            .map(|bytes| bytes.len() > JOB_INVENTORY_MAX_SERIALIZED_BYTES)
            .unwrap_or(true)
            && tail_limit > 0
        {
            tail_limit /= 2;
            for snapshot in &mut inventory.jobs {
                trim_runner_stream_to(&mut snapshot.stdout, tail_limit);
                trim_runner_stream_to(&mut snapshot.stderr, tail_limit);
            }
        }

        // Add newest terminal history only while it fits. Serializing each
        // record once avoids repeatedly encoding a multi-megabyte inventory
        // while preserving the newest-first eviction rule.
        let mut serialized_len = serde_json::to_vec(&inventory)
            .map(|bytes| bytes.len())
            .unwrap_or(JOB_INVENTORY_MAX_SERIALIZED_BYTES.saturating_add(1));
        for snapshot in terminal {
            let Ok(encoded) = serde_json::to_vec(&snapshot) else {
                continue;
            };
            let separator = usize::from(!inventory.jobs.is_empty());
            let added = encoded.len().saturating_add(separator);
            if serialized_len.saturating_add(added) > JOB_INVENTORY_MAX_SERIALIZED_BYTES {
                break;
            }
            serialized_len = serialized_len.saturating_add(added);
            inventory.jobs.push(snapshot);
        }
        inventory
    }
}
