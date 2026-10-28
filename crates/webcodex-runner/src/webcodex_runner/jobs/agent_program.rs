//! Ordinary programs in the existing Job queue and Windows LPAC custody.
//! No ambient execution fallback and no external-model or CLI budget path.
use super::*;
use crate::webcodex_runner::detached_job::AgentProgramJobMetadata;
use webcodex_core::agent_program::AgentProgramStart;

impl JobManager {
    pub(crate) fn inspect_agent_program(
        &self,
        sink: &RunnerSink,
        query: &webcodex_core::agent_program::AgentProgramInspect,
    ) -> Result<webcodex_core::agent_program::AgentProgramInspection, String> {
        use webcodex_core::agent_program::{AgentProgramInspection, AgentProgramService};
        query.validate()?;
        if query.target.client_id != sink.client_id()
            || query.target.runner_instance_id != sink.runner_instance_id()
        {
            return Err("agent_program_inspect_stale_target".into());
        }
        let (configuration, child) = {
            let runtime = lock_unpoison(&self.agent_execution);
            (
                runtime
                    .configuration
                    .clone()
                    .ok_or("agent_program_history_unavailable")?,
                runtime
                    .live
                    .get(&query.binding.job_id)
                    .and_then(|c| c.child.clone()),
            )
        };
        let stored = configuration
            .store
            .read_agent_program_metadata()?
            .into_iter()
            .find(|r| r.start.binding == query.binding)
            .ok_or("agent_program_history_missing")?;
        let current = lock_unpoison(&self.jobs)
            .get(&query.binding.job_id)
            .map(|j| j.snapshot.clone());
        if current
            .as_ref()
            .is_some_and(|j| j.context.agent_program.as_ref() != Some(&query.binding))
        {
            return Err("agent_program_inspection_identity_mismatch".into());
        }
        let job = current.or(stored.snapshot);
        let state = job
            .as_ref()
            .map_or("outcome_unknown", |j| j.status.as_str())
            .to_owned();
        let mut service = None;
        #[cfg(windows)]
        if state == "running" {
            if let (Some(child), Some(port)) = (child, query.binding.local_service_port) {
                let mut child = lock_unpoison(&child);
                if let JobChild::Isolated(native) = &mut *child {
                    if native
                        .try_wait()
                        .map_err(|_| "agent_program_process_observation_failed")?
                        .is_none()
                    {
                        if let Some((pid, _)) = native
                            .local_listeners()
                            .map_err(|_| "agent_program_port_observation_failed")?
                            .into_iter()
                            .find(|(_, p)| *p == port)
                        {
                            service = Some(AgentProgramService {
                                port,
                                process_id: pid,
                                observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                            });
                        }
                    }
                }
            }
        }
        Ok(AgentProgramInspection {
            binding_hash: query.binding.identity_hash()?,
            state,
            job,
            service,
        })
    }
    pub(crate) fn admit_agent_program_locked(
        &self,
        start: &AgentProgramStart,
        sink: &RunnerSink,
    ) -> Result<(), String> {
        start.validate()?;
        if start.binding.target.client_id != sink.client_id()
            || start.binding.target.runner_instance_id != sink.runner_instance_id()
            || start.binding.expires_at_unix_ms <= chrono::Utc::now().timestamp_millis()
        {
            return Err("agent_program_target_stale".into());
        }
        let configuration = lock_unpoison(&self.agent_execution)
            .configuration
            .clone()
            .ok_or("agent_program_resources_unavailable")?;
        configuration.profiles.validate_program(start)?;
        configuration.store.update_agent_program_metadata(|rows| {
            if rows.iter().any(|r| {
                r.start.binding.program_id == start.binding.program_id
                    || r.start.binding.job_id == start.binding.job_id
                    || r.start.binding.start_request_id == start.binding.start_request_id
            }) {
                return Err("agent_program_original_execution_must_be_observed".into());
            }
            rows.push(AgentProgramJobMetadata {
                start: start.clone(),
                native_create_attempted: false,
                worker_finished: false,
                tree_stopped: false,
                snapshot: None,
            });
            Ok(())
        })
    }

    pub(crate) fn start_agent_program_job(&self, queued: PendingJobStart) {
        let RunnerJobOperation::StartProcess(operation) = &queued.operation else {
            return;
        };
        let start = match AgentProgramStart::from_job(operation) {
            Ok(s) => s,
            Err(e) => {
                self.fail_job(&queued.operation, e, None);
                return;
            }
        };
        let configuration = match lock_unpoison(&self.agent_execution).configuration.clone() {
            Some(c) => c,
            None => {
                self.fail_job(
                    &queued.operation,
                    "agent_program_resources_unavailable".into(),
                    None,
                );
                return;
            }
        };
        let id = start.binding.job_id.clone();
        {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let mut runtime = lock_unpoison(&self.agent_execution);
            if runtime.live.contains_key(&id) {
                return;
            }
            runtime
                .live
                .insert(id.clone(), IsolatedJobCustody::default());
        }
        let manager = self.clone_for_worker();
        let guard = self.workers.enter();
        let worker_id = id.clone();
        let worker = std::thread::Builder::new()
            .name("agent-program-job".into())
            .spawn(move || {
                let _guard = guard;
                let result = manager.run_agent_program(
                    &start,
                    &configuration,
                    &queued.policy,
                    &queued.project_registry_dir,
                );
            if let Err(error) = result {
                    let attempted = configuration
                        .store
                        .read_agent_program_metadata()
                        .ok()
                        .and_then(|rows| rows.into_iter().find(|r| r.start == start))
                        .is_none_or(|r| r.native_create_attempted);
                let child = {lock_unpoison(&manager.agent_execution)
                        .live
                        .get(&worker_id)
                        .and_then(|c| c.child.clone())};
                if let Some(child)=child {
                    let stopped={let mut child=lock_unpoison(&child);let _=child.terminate_tree();child.wait_tree_exit(Duration::from_secs(5)).unwrap_or(false)};
                    if stopped {if let Some(c)=lock_unpoison(&manager.agent_execution).live.get_mut(&worker_id){if let Some(resources)=c.resources.as_mut(){let _=resources.revoke_local_network_after_stop();}}}
                }
                #[cfg(windows)]
                if !attempted && start.binding.local_service_port.is_some(){
                    if let Ok(identity)=start.binding.identity_hash(){let _=webcodex_process::windows_local_service::LocalServiceNetwork::remove_stopped(&identity);}
                }
                    manager.update_and_send(
                        &worker_id,
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
                manager.start_available_queued();
            });
        match worker {
            Ok(worker) => {
                if let Some(c) = lock_unpoison(&self.agent_execution).live.get_mut(&id) {
                    c.worker = Some(worker);
                }
            }
            Err(_) => self.update_and_send(
                &id,
                RunnerJobDelta {
                    status: "failed".into(),
                    error: Some("agent_program_worker_start_failed".into()),
                    command_execution_state: Some(ShellCommandExecutionState::NotStarted),
                    finished: true,
                    ..Default::default()
                },
            ),
        }
    }

    fn run_agent_program(
        &self,
        start: &AgentProgramStart,
        configuration: &AgentExecutionConfiguration,
        policy: &super::super::RunnerPolicy,
        registry: &std::path::Path,
    ) -> Result<(), String> {
        let local = start
            .binding
            .workspace_project
            .strip_prefix(&format!("agent:{}:", start.binding.target.client_id))
            .ok_or("agent_program_workspace_runner_mismatch")?;
        let registered = crate::webcodex_runner::projects::registered_project_root_for_execution(
            registry, local,
        )?;
        if registered.to_str() != Some(start.binding.workspace_root.as_str()) {
            return Err("agent_program_workspace_changed".into());
        }
        crate::webcodex_runner::shell::cwd_allowed(policy, &registered)
            .map_err(|_| "agent_program_workspace_denied")?;
        #[cfg(windows)]
        {
            self.run_agent_program_windows(start, configuration)
        }
        #[cfg(not(windows))]
        {
            let _ = configuration;
            Err("agent_program_platform_unavailable".into())
        }
    }

    #[cfg(windows)]
    fn run_agent_program_windows(
        &self,
        start: &AgentProgramStart,
        configuration: &AgentExecutionConfiguration,
    ) -> Result<(), String> {
        use crate::webcodex_runner::jobs::process::{join_reader_threads_until, spawn_reader};
        use crate::webcodex_runner::jobs::OutputChunk;
        use crate::webcodex_runner::output_text::OutputTextSource;
        use std::io::Write;
        let id = &start.binding.job_id;
        let began = Instant::now();
        let deadline = began + Duration::from_secs(start.timeout_secs);
        let resume_source = if let Some(previous) = &start.binding.resume_from_program {
            let old = configuration
                .store
                .read_agent_program_metadata()?
                .into_iter()
                .find(|r| &r.start.binding.program_id == previous)
                .ok_or("agent_program_resume_history_missing")?;
            if !old.worker_finished
                || !old.tree_stopped
                || old.snapshot.is_none()
                || old.start.binding.task_id != start.binding.task_id
                || old.start.binding.run.agent_id != start.binding.run.agent_id
                || old.start.binding.workspace_project != start.binding.workspace_project
                || old.start.binding.workspace_root != start.binding.workspace_root
                || old.start.binding.relative_cwd != start.binding.relative_cwd
            {
                return Err("agent_program_resume_source_not_stopped_or_owned".into());
            }
            Some(
                super::super::agent_resources::program_resource_directory(
                    &configuration.resource_root,
                    &old.start.binding,
                )?
                .join("workspace"),
            )
        } else {
            None
        };
        let resources = super::super::agent_resources::prepare_program(
            &configuration.profiles,
            start,
            &configuration.resource_root,
            resume_source.as_deref(),
        )?;
        let (child, stdin, stdout, stderr, stop) = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let mut runtime = lock_unpoison(&self.agent_execution);
            let mut jobs = lock_unpoison(&self.jobs);
            let job = jobs.get_mut(id).ok_or("agent_program_job_missing")?;
            if self.shutting_down.load(Ordering::SeqCst)
                || job.stop_requested.load(Ordering::SeqCst)
                || Instant::now() >= deadline
                || start.binding.expires_at_unix_ms <= chrono::Utc::now().timestamp_millis()
            {
                return Err("agent_program_admission_closed".into());
            }
            configuration.profiles.validate_program(start)?;
            configuration.store.update_agent_program_metadata(|rows| {
                let row = rows
                    .iter_mut()
                    .find(|r| r.start == *start)
                    .ok_or("agent_program_admission_missing")?;
                if row.native_create_attempted || row.worker_finished {
                    return Err("agent_program_creation_replay_denied".into());
                }
                row.native_create_attempted = true;
                Ok(())
            })?;
            let mut native =
                webcodex_process::windows_lpac::LpacChild::spawn_host(resources.launch_spec())
                    .map_err(|_| "agent_program_native_create_unknown")?;
            let (stdin, stdout, stderr) = (
                native.stdin.take(),
                native.stdout.take(),
                native.stderr.take(),
            );
            let child = Arc::new(Mutex::new(JobChild::Isolated(native)));
            let custody = runtime
                .live
                .get_mut(id)
                .ok_or("agent_program_custody_missing")?;
            custody.child = Some(child.clone());
            custody.resources = Some(resources);
            job.child = Some(child.clone());
            (child, stdin, stdout, stderr, job.stop_requested.clone())
        };
        self.update_and_send(
            id,
            RunnerJobDelta {
                status: "running".into(),
                ..Default::default()
            },
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let mut readers = Vec::new();
        for (reader, is_stdout) in [(stdout, true), (stderr, false)] {
            if let Some(reader) = reader {
                readers.push(spawn_reader(
                    reader,
                    tx.clone(),
                    is_stdout,
                    OutputTextSource::LocalProcess,
                ));
            }
        }
        drop(tx);
        let input = stdin
            .zip(start.stdin.clone())
            .map(|(mut pipe, text)| std::thread::spawn(move || pipe.write_all(text.as_bytes())));
        let flush = || {
            for _ in 0..16 {
                match rx.try_recv() {
                    Ok(OutputChunk::Stdout(text)) => self.update_and_send(
                        id,
                        RunnerJobDelta {
                            stdout_chunk: Some(text),
                            ..Default::default()
                        },
                    ),
                    Ok(OutputChunk::Stderr(text)) => self.update_and_send(
                        id,
                        RunnerJobDelta {
                            stderr_chunk: Some(text),
                            ..Default::default()
                        },
                    ),
                    Err(_) => break,
                }
            }
        };
        let (exit, timed_out) = loop {
            flush();
            let expired = Instant::now() >= deadline
                || start.binding.expires_at_unix_ms <= chrono::Utc::now().timestamp_millis();
            let mut native = lock_unpoison(&child);
            if expired || stop.load(Ordering::SeqCst) {
                native
                    .terminate_tree()
                    .map_err(|_| "agent_program_stop_unknown")?;
            }
            if let Some(exit) = native
                .try_wait()
                .map_err(|_| "agent_program_exit_unknown")?
            {
                if !native
                    .wait_tree_exit(Duration::from_secs(5))
                    .map_err(|_| "agent_program_tree_unknown")?
                {
                    return Err("agent_program_tree_stop_unproven".into());
                }
                break (exit, expired);
            }
            drop(native);
            std::thread::sleep(Duration::from_millis(20));
        };
        let settle = Instant::now() + Duration::from_secs(5);
        while readers.iter().any(|h| !h.is_finished()) && Instant::now() < settle {
            flush();
            std::thread::sleep(Duration::from_millis(5));
        }
        flush();
        if join_reader_threads_until(readers, settle) != 0 {
            return Err("agent_program_output_unsettled".into());
        }
        if let Some(input) = input {
            while !input.is_finished() && Instant::now() < settle {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !input.is_finished() {
                return Err("agent_program_input_unsettled".into());
            }
            let _ = input.join();
        }
        let stopped = stop.load(Ordering::SeqCst);
        self.update_and_send(
            id,
            RunnerJobDelta {
                status: if timed_out {
                    "timeout"
                } else if stopped {
                    "stopped"
                } else if exit.success() {
                    "completed"
                } else {
                    "failed"
                }
                .into(),
                exit_code: exit.code(),
                duration_ms: Some(began.elapsed().as_millis() as u64),
                command_execution_state: Some(if timed_out {
                    ShellCommandExecutionState::TimedOut
                } else {
                    ShellCommandExecutionState::Completed
                }),
                finished: true,
                ..Default::default()
            },
        );
        let snapshot = lock_unpoison(&self.jobs)
            .get(id)
            .map(|j| j.snapshot.clone())
            .ok_or("agent_program_snapshot_missing")?;
        configuration.store.update_agent_program_metadata(|rows| {
            let row = rows
                .iter_mut()
                .find(|r| r.start == *start)
                .ok_or("agent_program_admission_missing")?;
            row.worker_finished = true;
            row.tree_stopped = true;
            row.snapshot = Some(snapshot);
            Ok(())
        })?;
        if let Some(c) = lock_unpoison(&self.agent_execution).live.get_mut(id) {
            c.helpers_finished = true;
            if let Some(resources) = c.resources.as_mut() {
                resources.revoke_local_network_after_stop()?;
            }
            c.resources.take();
        }
        Ok(())
    }
}
