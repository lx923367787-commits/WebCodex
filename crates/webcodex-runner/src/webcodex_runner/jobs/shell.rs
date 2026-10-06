//! Runner jobs shell implementation.

use crate::webcodex_runner::jobs::lifecycle::{
    post_spawn_interruption_delta, post_spawn_interruption_reason, raw_shell_job_terminal_lifecycle,
};
use crate::webcodex_runner::jobs::process::{
    cleanup_managed_tree, join_reader_threads_until, managed_tree_running, spawn_reader,
    terminate_managed_tree, validation_failed_step, validation_module_available,
    wait_failure_error,
};
use crate::webcodex_runner::jobs::updates::{
    append_bounded_tail, cargo_activity_from_stderr, process_running_activity,
    validation_step_activity, JOB_UPDATE_INTERVAL_MS,
};
use crate::webcodex_runner::jobs::{JobManager, OutputChunk, PendingJobStart, RunnerJobDelta};
use crate::webcodex_runner::output_text::OutputTextSource;
use crate::webcodex_runner::shutdown::lock_unpoison;
use crate::webcodex_runner::{
    configured_prepared_shell_job_command, configured_shell_job_command,
    configured_validation_job_command, cwd_allowed, is_transport_failure,
    resolve_prepared_shell_profile, RunnerPolicy, SshConfig,
};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use webcodex_core::runner_operation::RunnerJobOperation;
use webcodex_core::runner_protocol::{
    ShellCommandExecutionState, ShellJobValidationProgress, VALIDATION_STEP_SPAWN_FAILED_CODE,
    VALIDATION_TOOL_UNAVAILABLE_CODE,
};
use webcodex_process::ManagedChild;

impl JobManager {
    pub(crate) fn start_shell_job(&self, start: PendingJobStart) {
        let PendingJobStart {
            generation,
            policy,
            shell,
            ssh,
            project_registry_dir,
            metadata: _,
            operation,
        } = start;
        let (job_id, cwd, raw_command, steps, timeout_secs, context, validation) = match &operation
        {
            RunnerJobOperation::StartShell(request) => (
                request.job_id.clone(),
                request.cwd.clone(),
                Some(request.command.clone()),
                Vec::new(),
                request.timeout_secs,
                request.context.clone(),
                false,
            ),
            RunnerJobOperation::StartValidation(request) => (
                request.job_id.clone(),
                request.cwd.clone(),
                None,
                request.steps.clone(),
                request.timeout_secs,
                request.context.clone(),
                true,
            ),
            _ => unreachable!("shell Job starter received non shell/validation operation"),
        };
        if !policy.allow_raw_shell {
            self.fail_job(
                &operation,
                "raw shell is disabled by local Runner policy".to_string(),
                None,
            );
            return;
        }
        if context.ssh_resource.is_some() {
            self.start_ssh_shell_job(generation, policy, ssh, operation);
            return;
        }
        let cwd_path = cwd
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
        if let Err(e) = cwd_allowed(&policy, &cwd_path) {
            self.fail_job(&operation, e, None);
            return;
        }
        let prepared_profile = match resolve_prepared_shell_profile(
            generation,
            &shell,
            &project_registry_dir,
            &cwd_path,
            cwd.is_some(),
            &self.prepared_profiles,
            Some(self.shutting_down.as_ref()),
        ) {
            Ok(profile) => profile,
            Err(e) => {
                self.fail_job(&operation, e, None);
                return;
            }
        };
        if validation
            && steps.iter().any(|step| {
                !validation_module_available(
                    &shell,
                    prepared_profile.as_deref(),
                    &cwd_path,
                    step,
                    Some(self.shutting_down.as_ref()),
                )
            })
        {
            self.fail_job(
                &operation,
                VALIDATION_TOOL_UNAVAILABLE_CODE.to_string(),
                Some(ShellJobValidationProgress {
                    completed: 0,
                    current_step: None,
                    failed_step: None,
                }),
            );
            return;
        }
        let step_count = if validation { steps.len() } else { 1 };
        let mut commands = VecDeque::with_capacity(step_count);
        for index in 0..step_count {
            let configured = if validation {
                configured_validation_job_command(
                    &shell,
                    prepared_profile.as_deref(),
                    &steps[index].program,
                    &steps[index].args,
                    &cwd_path,
                )
            } else {
                let raw_command = raw_command
                    .as_deref()
                    .expect("typed raw shell Job carries command text");
                match prepared_profile.as_deref() {
                    Some(profile) => configured_prepared_shell_job_command(profile, raw_command),
                    None => configured_shell_job_command(&shell, raw_command),
                }
            };
            let mut command = match configured {
                Ok(command) => command,
                Err(error) => {
                    self.fail_job(&operation, error, None);
                    return;
                }
            };
            if validation {
                command.envs(
                    steps[index]
                        .env
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                );
            }
            command
                .current_dir(&cwd_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            commands.push_back(command);
        }
        let stop_requested = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            if self.shutting_down.load(Ordering::SeqCst) {
                None
            } else {
                let mut jobs = lock_unpoison(&self.jobs);
                let Some(job) = jobs.get_mut(&job_id) else {
                    return;
                };
                job.slot_reserved = true;
                Some(Arc::clone(&job.stop_requested))
            }
        };
        let Some(stop_requested) = stop_requested else {
            self.shutdown_rejection(&operation);
            return;
        };
        // Preserve the pre-start proof boundary explicitly. A stop/shutdown
        // observed here is still known to precede ManagedChild::spawn; the
        // fence below is intentionally repeated after spawn because that later
        // race can no longer claim NotStarted.
        if stop_requested.load(Ordering::SeqCst) {
            self.fail_job(&operation, "job stopped before start".to_string(), None);
            return;
        }
        if self.shutting_down.load(Ordering::SeqCst) {
            self.shutdown_rejection(&operation);
            return;
        }
        let start = Instant::now();
        let mut command = commands.pop_front().expect("validated non-empty plan");
        let spawn = ManagedChild::spawn(&mut command);
        let mut child = match spawn {
            Ok(child) => child,
            Err(e) => {
                if validation {
                    self.fail_job(
                        &operation,
                        VALIDATION_STEP_SPAWN_FAILED_CODE.to_string(),
                        Some(ShellJobValidationProgress {
                            completed: 0,
                            current_step: None,
                            failed_step: None,
                        }),
                    );
                } else {
                    let error = prepared_profile
                        .as_ref()
                        .map(|profile_name| {
                            format!(
                                "failed to spawn shell profile '{}': {}",
                                profile_name.profile_name, e
                            )
                        })
                        .unwrap_or_else(|| format!("failed to spawn command: {}", e));
                    self.fail_job(&operation, error, None);
                }
                return;
            }
        };
        let mut stdout = child.child_mut().stdout.take();
        let mut stderr = child.child_mut().stderr.take();
        let mut child = Arc::new(Mutex::new(child));
        let post_spawn_rejection = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let mut jobs = lock_unpoison(&self.jobs);
            let mut job = jobs.get_mut(&job_id);
            let rejection = post_spawn_interruption_reason(
                self.shutting_down.load(Ordering::SeqCst),
                stop_requested.load(Ordering::SeqCst),
                job.is_some(),
            );
            if rejection.is_none() {
                if let Some(job) = job.as_mut() {
                    job.child = Some(child.clone());
                }
            }
            rejection
        };
        if let Some(error) = post_spawn_rejection {
            let _ = terminate_managed_tree(&child);
            // ManagedChild::spawn succeeded before this fence. Even if the
            // termination request succeeds, the user command may already have
            // executed and we do not wait here for a trustworthy exit status.
            // Raw shell must therefore remain conservatively started/unknown;
            // never recycle the pre-start NotStarted evidence across this
            // spawn boundary.
            self.update_and_send(
                &job_id,
                post_spawn_interruption_delta(
                    &operation,
                    start.elapsed().as_millis() as u64,
                    error,
                ),
            );
            self.start_available_queued();
            return;
        }
        self.update_and_send(
            &job_id,
            RunnerJobDelta {
                status: "running".to_string(),
                validation_progress: validation.then(|| ShellJobValidationProgress {
                    completed: 0,
                    current_step: Some(steps[0].name.clone()),
                    failed_step: None,
                }),
                activity: Some(if validation {
                    validation_step_activity(&steps[0])
                } else {
                    process_running_activity()
                }),
                ..Default::default()
            },
        );
        let jobs = self.jobs.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let shutting_down = Arc::clone(&self.shutting_down);
        let manager = self.clone_for_worker();
        let worker_guard = self.workers.enter();
        std::thread::spawn(move || {
            let _worker_guard = worker_guard;
            let timeout_secs = timeout_secs.min(policy.max_timeout_secs).max(1);
            let mut step_index = 0;
            let (final_status, out, err, final_progress) = loop {
                const OUTPUT_CHANNEL_CAPACITY: usize = 64;
                let (tx, rx) = mpsc::sync_channel::<OutputChunk>(OUTPUT_CHANNEL_CAPACITY);
                let mut readers = Vec::new();
                if let Some(stdout) = stdout {
                    readers.push(spawn_reader(
                        stdout,
                        tx.clone(),
                        true,
                        OutputTextSource::LocalProcess,
                    ));
                }
                if let Some(stderr) = stderr {
                    readers.push(spawn_reader(
                        stderr,
                        tx.clone(),
                        false,
                        OutputTextSource::LocalProcess,
                    ));
                }
                drop(tx);
                let step_status = loop {
                    let mut out = String::new();
                    let mut err = String::new();
                    while let Ok(chunk) = rx.try_recv() {
                        match chunk {
                            OutputChunk::Stdout(text) => out.push_str(&text),
                            OutputChunk::Stderr(text) => err.push_str(&text),
                        }
                    }
                    if !out.is_empty() || !err.is_empty() {
                        let activity = validation
                            .then(|| cargo_activity_from_stderr(&steps[step_index], &err))
                            .flatten();
                        manager.update_and_send(
                            &job_id,
                            RunnerJobDelta {
                                status: "running".to_string(),
                                stdout_chunk: (!out.is_empty()).then_some(out),
                                stderr_chunk: (!err.is_empty()).then_some(err),
                                validation_progress: validation.then(|| {
                                    ShellJobValidationProgress {
                                        completed: step_index,
                                        current_step: Some(steps[step_index].name.clone()),
                                        failed_step: None,
                                    }
                                }),
                                activity,
                                ..Default::default()
                            },
                        );
                    }
                    let wait_result = {
                        let mut child = lock_unpoison(&child);
                        child.try_wait()
                    };
                    match wait_result {
                        Ok(Some(status)) => {
                            let stopped = stop_requested.load(Ordering::SeqCst);
                            break (
                                if stopped {
                                    "stopped"
                                } else if status.success() {
                                    "completed"
                                } else {
                                    "failed"
                                }
                                .to_string(),
                                Some(status.code().unwrap_or(-1)),
                                if stopped {
                                    Some("job stopped by request".to_string())
                                } else {
                                    None
                                },
                            );
                        }
                        Ok(None) => {
                            if stop_requested.load(Ordering::SeqCst) {
                                let _ = terminate_managed_tree(&child);
                                break (
                                    "stopped".to_string(),
                                    Some(-1),
                                    Some("job stopped by request".to_string()),
                                );
                            }
                            if start.elapsed() >= Duration::from_secs(timeout_secs) {
                                stop_requested.store(true, Ordering::SeqCst);
                                let _ = terminate_managed_tree(&child);
                                break (
                                    "timeout".to_string(),
                                    Some(-1),
                                    Some(format!("job timed out after {} seconds", timeout_secs)),
                                );
                            }
                        }
                        Err(e) => {
                            // The host lost track of a process it started.
                            // For a validation job that must arrive as a
                            // machine-readable infrastructure code: the step
                            // did not fail, its outcome is simply unknown,
                            // and saying "check failed" would blame the
                            // project for the executor's problem.
                            eprintln!("webcodex-runner failed to wait job {job_id}: {e}");
                            break (
                                "failed".to_string(),
                                None,
                                Some(wait_failure_error(validation, &e)),
                            );
                        }
                    }
                    std::thread::sleep(Duration::from_millis(JOB_UPDATE_INTERVAL_MS));
                };
                // A direct child can exit while a background descendant keeps
                // stdout/stderr open. Give the tree a short window to exit on
                // its own, then force-terminate whatever remains before the
                // bounded reader join, so cleanup cannot wait forever on EOF.
                cleanup_managed_tree(&child);
                join_reader_threads_until(readers, Instant::now() + Duration::from_secs(1));
                let mut out = String::new();
                let mut err = String::new();
                while let Ok(chunk) = rx.try_recv() {
                    match chunk {
                        OutputChunk::Stdout(text) => out.push_str(&text),
                        OutputChunk::Stderr(text) => err.push_str(&text),
                    }
                }
                if step_status.0 == "completed" && step_index + 1 < step_count {
                    step_index += 1;
                    if stop_requested.load(Ordering::SeqCst) {
                        break (
                            (
                                "stopped".to_string(),
                                Some(-1),
                                Some("job stopped by request".to_string()),
                            ),
                            out,
                            err,
                            validation.then_some(ShellJobValidationProgress {
                                completed: step_index,
                                current_step: None,
                                failed_step: None,
                            }),
                        );
                    }
                    {
                        let _lifecycle_guard = lock_unpoison(&lifecycle);
                        if shutting_down.load(Ordering::SeqCst)
                            || stop_requested.load(Ordering::SeqCst)
                        {
                            break (
                                (
                                    "stopped".to_string(),
                                    Some(-1),
                                    Some("job stopped by request".to_string()),
                                ),
                                out,
                                err,
                                validation.then_some(ShellJobValidationProgress {
                                    completed: step_index,
                                    current_step: None,
                                    failed_step: None,
                                }),
                            );
                        }
                    }
                    let mut next_command = commands
                        .pop_front()
                        .expect("one command per validation step");
                    let spawn = ManagedChild::spawn(&mut next_command);
                    let mut next = match spawn {
                        Ok(child) => child,
                        Err(_error) => {
                            break (
                                (
                                    "failed".to_string(),
                                    None,
                                    Some(VALIDATION_STEP_SPAWN_FAILED_CODE.to_string()),
                                ),
                                out,
                                err,
                                validation.then_some(ShellJobValidationProgress {
                                    completed: step_index,
                                    current_step: None,
                                    failed_step: None,
                                }),
                            )
                        }
                    };
                    let next_stdout = next.child_mut().stdout.take();
                    let next_stderr = next.child_mut().stderr.take();
                    let next = Arc::new(Mutex::new(next));
                    let reject_for_shutdown = {
                        let _lifecycle_guard = lock_unpoison(&lifecycle);
                        if shutting_down.load(Ordering::SeqCst)
                            || stop_requested.load(Ordering::SeqCst)
                        {
                            true
                        } else if let Some(job) = lock_unpoison(&jobs).get_mut(&job_id) {
                            job.child = Some(Arc::clone(&next));
                            false
                        } else {
                            true
                        }
                    };
                    if reject_for_shutdown {
                        let _ = terminate_managed_tree(&next);
                        break (
                            (
                                "stopped".to_string(),
                                Some(-1),
                                Some("job stopped by request".to_string()),
                            ),
                            out,
                            err,
                            validation.then_some(ShellJobValidationProgress {
                                completed: step_index,
                                current_step: None,
                                failed_step: None,
                            }),
                        );
                    }
                    child = next;
                    manager.update_and_send(
                        &job_id,
                        RunnerJobDelta {
                            status: "running".to_string(),
                            stdout_chunk: (!out.is_empty()).then_some(out),
                            stderr_chunk: (!err.is_empty()).then_some(err),
                            validation_progress: validation.then(|| ShellJobValidationProgress {
                                completed: step_index,
                                current_step: Some(steps[step_index].name.clone()),
                                failed_step: None,
                            }),
                            activity: validation
                                .then(|| validation_step_activity(&steps[step_index])),
                            ..Default::default()
                        },
                    );
                    stdout = next_stdout;
                    stderr = next_stderr;
                    continue;
                }
                let progress = validation.then(|| ShellJobValidationProgress {
                    completed: if step_status.0 == "completed" {
                        steps.len()
                    } else {
                        step_index
                    },
                    current_step: None,
                    // An infrastructure code names no failed step: the
                    // connector reads `failed_step` as "this check rejected
                    // the work", which is exactly what did not happen.
                    failed_step: validation_failed_step(
                        &step_status.0,
                        step_status.2.as_deref(),
                        &steps[step_index].name,
                    ),
                });
                break (step_status, out, err, progress);
            };
            let command_execution_state = (!validation)
                .then(|| raw_shell_job_terminal_lifecycle(&final_status.0, final_status.1));
            manager.update_and_send(
                &job_id,
                RunnerJobDelta {
                    status: final_status.0,
                    stdout_chunk: (!out.is_empty()).then_some(out),
                    stderr_chunk: (!err.is_empty()).then_some(err),
                    exit_code: final_status.1,
                    duration_ms: Some(start.elapsed().as_millis() as u64),
                    error: final_status.2,
                    command_execution_state,
                    stream_limit_bytes: None,
                    validation_progress: final_progress,
                    activity: None,
                    finished: true,
                },
            );
            manager.start_available_queued();
        });
    }

    /// Start one remote SSH command as a normal runner job. Resource/session
    /// validation and local command preparation happen before spawn, so those
    /// failures are unambiguously command-not-started. Once `ssh` is running,
    /// never retry because remote delivery may already have happened.
    pub(crate) fn start_ssh_shell_job(
        &self,
        generation: u64,
        policy: RunnerPolicy,
        ssh: SshConfig,
        operation: RunnerJobOperation,
    ) {
        let request = match &operation {
            RunnerJobOperation::StartShell(request) => request,
            RunnerJobOperation::StartValidation(_) => {
                self.fail_job(
                    &operation,
                    "ssh_resource_unsupported_for_request: SSH resources do not support structured validation jobs; command was not started".to_string(),
                    None,
                );
                return;
            }
            _ => unreachable!("SSH Job starter received non shell/validation operation"),
        };
        let job_id = request.job_id.clone();
        let Some(resource_name) = request.context.ssh_resource.as_deref() else {
            return;
        };
        let Some(session_id) = request.context.workflow_session_id.as_deref() else {
            self.fail_job(
                &operation,
                "ssh_session_required: an SSH resource requires a Workflow Session id; command was not started".to_string(),
                None,
            );
            return;
        };
        let prepared = match self.ssh_pool.prepare_job_command(
            generation,
            &ssh,
            resource_name,
            session_id,
            request.cwd.as_deref(),
            &request.command,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_job(&operation, error, None);
                return;
            }
        };
        let transport = prepared.transport.clone();
        let program_delivery = prepared.program_delivery;
        let mut command = prepared.command;
        if program_delivery.requires_stdin() {
            command.stdin(Stdio::piped());
        } else {
            command.stdin(Stdio::null());
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let stop_requested = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            if self.shutting_down.load(Ordering::SeqCst) {
                None
            } else {
                let mut jobs = lock_unpoison(&self.jobs);
                let Some(job) = jobs.get_mut(&job_id) else {
                    return;
                };
                job.slot_reserved = true;
                Some(Arc::clone(&job.stop_requested))
            }
        };
        let Some(stop_requested) = stop_requested else {
            self.shutdown_rejection(&operation);
            return;
        };
        let start = Instant::now();
        let spawn = ManagedChild::spawn(&mut command);
        let mut child = match spawn {
            Ok(child) => child,
            Err(error) => {
                self.fail_job(
                    &operation,
                    format!(
                        "ssh_command_spawn_failed: could not start local ssh client: {error}; command was not started"
                    ),
                    None,
                );
                return;
            }
        };
        let mut child_stdin = child.child_mut().stdin.take();
        let mut stdout = child.child_mut().stdout.take();
        let mut stderr = child.child_mut().stderr.take();
        let child = Arc::new(Mutex::new(child));
        let post_spawn_rejection = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let mut jobs = lock_unpoison(&self.jobs);
            let mut job = jobs.get_mut(&job_id);
            let rejection = post_spawn_interruption_reason(
                self.shutting_down.load(Ordering::SeqCst),
                stop_requested.load(Ordering::SeqCst),
                job.is_some(),
            );
            if rejection.is_none() {
                if let Some(job) = job.as_mut() {
                    job.child = Some(Arc::clone(&child));
                }
            }
            rejection
        };
        if let Some(error) = post_spawn_rejection {
            let _ = terminate_managed_tree(&child);
            // ManagedChild::spawn has already resumed ssh.exe. A successful
            // local tree termination cannot prove that the remote command was
            // never dispatched, so do not recycle pre-start NotStarted here.
            self.update_and_send(
                &job_id,
                post_spawn_interruption_delta(
                    &operation,
                    start.elapsed().as_millis() as u64,
                    error,
                ),
            );
            self.start_available_queued();
            return;
        }
        self.update_and_send(
            &job_id,
            RunnerJobDelta {
                status: "running".to_string(),
                ..Default::default()
            },
        );
        let manager = self.clone_for_worker();
        let ssh_pool = self.ssh_pool.clone();
        let output_limit_bytes = policy.max_output_bytes;
        let worker_guard = self.workers.enter();
        let timeout_secs = request.timeout_secs.min(policy.max_timeout_secs).max(1);
        std::thread::spawn(move || {
            let _worker_guard = worker_guard;
            const OUTPUT_CHANNEL_CAPACITY: usize = 64;
            let (tx, rx) = mpsc::sync_channel::<OutputChunk>(OUTPUT_CHANNEL_CAPACITY);
            let mut readers = Vec::new();
            if let Some(stdout) = stdout.take() {
                readers.push(spawn_reader(
                    stdout,
                    tx.clone(),
                    true,
                    OutputTextSource::RemoteSsh,
                ));
            }
            if let Some(stderr) = stderr.take() {
                readers.push(spawn_reader(
                    stderr,
                    tx.clone(),
                    false,
                    OutputTextSource::RemoteSsh,
                ));
            }
            drop(tx);
            // Readers must already be draining before program/caller stdin can
            // block. The writer is tracked and polled by this same Job worker.
            let mut writer_start_error = None;
            let mut stdin_writer = match program_delivery.spawn_writer(child_stdin.take(), None) {
                Ok(writer) => writer,
                Err(error) => {
                    writer_start_error = Some(error);
                    let _ = terminate_managed_tree(&child);
                    None
                }
            };
            let mut transport_stderr = String::new();
            let (mut status, mut exit_code, mut error, interrupted_after_dispatch) = loop {
                let mut out = String::new();
                let mut err = String::new();
                while let Ok(chunk) = rx.try_recv() {
                    match chunk {
                        OutputChunk::Stdout(text) => out.push_str(&text),
                        OutputChunk::Stderr(text) => err.push_str(&text),
                    }
                }
                if !err.is_empty() {
                    append_bounded_tail(&mut transport_stderr, &err, 16 * 1024);
                }
                if !out.is_empty() || !err.is_empty() {
                    manager.update_and_send(
                        &job_id,
                        RunnerJobDelta {
                            status: "running".to_string(),
                            stdout_chunk: (!out.is_empty()).then_some(out),
                            stderr_chunk: (!err.is_empty()).then_some(err),
                            stream_limit_bytes: Some(output_limit_bytes),
                            ..Default::default()
                        },
                    );
                }
                if let Some(writer_error) = writer_start_error.take().or_else(|| {
                    stdin_writer
                        .as_mut()
                        .and_then(|writer| writer.poll_failure())
                }) {
                    let _ = terminate_managed_tree(&child);
                    break ("failed".to_string(), None, Some(writer_error), true);
                }
                let wait_result = {
                    let mut child = lock_unpoison(&child);
                    child.try_wait()
                };
                match wait_result {
                    Ok(Some(status)) => {
                        if stop_requested.load(Ordering::SeqCst) {
                            break (
                                "failed".to_string(),
                                None,
                                Some(
                                    "ssh_command_stopped_after_dispatch: local SSH tree was terminated, remote command outcome is unknown; do not blindly retry"
                                        .to_string(),
                                ),
                                true,
                            );
                        }
                        if status.success() {
                            break ("completed".to_string(), Some(0), None, false);
                        }
                        break ("failed".to_string(), status.code(), None, false);
                    }
                    Ok(None) => {
                        if stop_requested.load(Ordering::SeqCst) {
                            let _ = terminate_managed_tree(&child);
                            break (
                                "failed".to_string(),
                                None,
                                Some(
                                    "ssh_command_stopped_after_dispatch: local SSH tree was terminated, remote command outcome is unknown; do not blindly retry"
                                        .to_string(),
                                ),
                                true,
                            );
                        }
                        if start.elapsed() >= Duration::from_secs(timeout_secs) {
                            stop_requested.store(true, Ordering::SeqCst);
                            let _ = terminate_managed_tree(&child);
                            break (
                                "timeout".to_string(),
                                Some(-1),
                                Some(format!("job timed out after {timeout_secs} seconds")),
                                false,
                            );
                        }
                    }
                    Err(wait_error) => {
                        break (
                            "failed".to_string(),
                            None,
                            Some(format!(
                                "ssh_command_wait_failed: command may have started and was not retried: {wait_error}"
                            )),
                            false,
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(JOB_UPDATE_INTERVAL_MS));
            };
            // The SSH client is the root of a private process tree. Ensure a
            // background child holding either pipe cannot delay the terminal
            // update indefinitely.
            cleanup_managed_tree(&child);
            let tree_cleanup_uncertain = managed_tree_running(&child);
            let writer_finish_error = stdin_writer.as_mut().and_then(|writer| {
                let interrupted =
                    interrupted_after_dispatch || status == "timeout" || tree_cleanup_uncertain;
                let result = if interrupted {
                    writer.finish_after_tree_cleanup()
                } else {
                    writer.finish_bounded()
                };
                result.err()
            });
            join_reader_threads_until(readers, Instant::now() + Duration::from_secs(1));
            let mut final_out = String::new();
            let mut final_err = String::new();
            while let Ok(chunk) = rx.try_recv() {
                match chunk {
                    OutputChunk::Stdout(text) => final_out.push_str(&text),
                    OutputChunk::Stderr(text) => final_err.push_str(&text),
                }
            }
            if !final_err.is_empty() {
                append_bounded_tail(&mut transport_stderr, &final_err, 16 * 1024);
            }
            let mut command_execution_state = if interrupted_after_dispatch {
                ShellCommandExecutionState::OutcomeUnknown
            } else {
                raw_shell_job_terminal_lifecycle(&status, exit_code)
            };
            if let Some(writer_error) = writer_finish_error {
                status = "failed".to_string();
                exit_code = None;
                error = Some(writer_error);
                command_execution_state = ShellCommandExecutionState::OutcomeUnknown;
            }
            if tree_cleanup_uncertain {
                status = "failed".to_string();
                error = Some(
                    "ssh_command_cleanup_failed: local SSH process tree exit could not be proven; command may have started and was not retried"
                        .to_string(),
                );
                command_execution_state = ShellCommandExecutionState::OutcomeUnknown;
            }
            if matches!(status.as_str(), "completed" | "failed")
                && matches!(
                    command_execution_state,
                    ShellCommandExecutionState::Completed
                )
                && is_transport_failure(&transport, exit_code, Some(&transport_stderr))
            {
                ssh_pool.invalidate_after_transport_failure(&transport);
                status = "failed".to_string();
                error = Some(
                    "ssh_transport_failed: command may have started and was not retried"
                        .to_string(),
                );
                command_execution_state = ShellCommandExecutionState::OutcomeUnknown;
                if !final_err.is_empty() && !final_err.ends_with('\n') {
                    final_err.push('\n');
                }
                final_err.push_str(
                    "webcodex: SSH transport ended after dispatch; the command may have started and was not retried\n",
                );
            }
            manager.update_and_send(
                &job_id,
                RunnerJobDelta {
                    status,
                    stdout_chunk: (!final_out.is_empty()).then_some(final_out),
                    stderr_chunk: (!final_err.is_empty()).then_some(final_err),
                    exit_code,
                    duration_ms: Some(start.elapsed().as_millis() as u64),
                    error,
                    command_execution_state: Some(command_execution_state),
                    stream_limit_bytes: Some(output_limit_bytes),
                    finished: true,
                    ..Default::default()
                },
            );
            manager.start_available_queued();
        });
    }
}
