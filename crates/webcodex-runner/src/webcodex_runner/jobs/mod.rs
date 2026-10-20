//! Runner-owned Job state and service composition.

use crate::webcodex_runner::detached_job::DetachedJobStore;
use crate::webcodex_runner::jobs::lifecycle::runner_job_is_active;
use crate::webcodex_runner::jobs::updates::{
    spawn_job_update_delivery_worker, JobUpdateDeliveryQueue, JobUpdateDeliverySignal,
};
use crate::webcodex_runner::shutdown::{lock_unpoison, ActivityTracker};
use crate::webcodex_runner::{
    PreparedShellProfileCache, RunnerPolicy, RunnerSink, ShellConfig, SshConfig, SshConnectionPool,
};
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
#[cfg(test)]
use webcodex_core::runner_operation::RunnerOperation;
use webcodex_core::runner_operation::{RunnerInvocationMetadata, RunnerJobOperation};
#[cfg(test)]
use webcodex_core::runner_protocol;
use webcodex_core::runner_protocol::{
    ShellCommandExecutionState, ShellJobActivity, ShellJobSnapshot, ShellJobValidationProgress,
};
use webcodex_process::ManagedChild;

#[derive(Debug)]
pub(crate) struct JobManagerOwnerLifetime {
    pub(crate) jobs: Weak<Mutex<HashMap<String, RunningJob>>>,
    pub(crate) detached_jobs: Weak<Mutex<HashMap<String, DetachedJobRef>>>,
    pub(crate) shutting_down: Weak<AtomicBool>,
    pub(crate) delivery_signal: Arc<JobUpdateDeliverySignal>,
}

impl Drop for JobManagerOwnerLifetime {
    fn drop(&mut self) {
        self.delivery_signal.close();
        if let Some(shutting_down) = self.shutting_down.upgrade() {
            shutting_down.store(true, Ordering::SeqCst);
        }
        let Some(jobs) = self.jobs.upgrade() else {
            return;
        };
        let detached_ids = self
            .detached_jobs
            .upgrade()
            .map(|detached| {
                lock_unpoison(&detached)
                    .keys()
                    .cloned()
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let targets = {
            let jobs = lock_unpoison(&jobs);
            jobs.iter()
                .filter(|(job_id, job)| {
                    runner_job_is_active(&job.snapshot.status)
                        && !detached_ids.contains(job_id.as_str())
                })
                .map(|(_, job)| (job.child.clone(), Arc::clone(&job.stop_requested)))
                .collect::<Vec<_>>()
        };
        for (child, stop_requested) in targets {
            stop_requested.store(true, Ordering::SeqCst);
            let Some(child) = child else {
                continue;
            };
            let mut child = match child.try_lock() {
                Ok(child) => child,
                Err(std::sync::TryLockError::WouldBlock) => continue,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            let _ = child.terminate_tree();
        }
    }
}

/// A Job start accepted by `enqueue` and waiting for a slot: the immutable
/// inputs the start pipeline consumes once capacity is available.
///
/// The sink is deliberately not part of this unit. `enqueue` installs it on
/// the manager and reports admission failures through it, but the queued
/// entry and every downstream start path communicate through the installed
/// sink (`current_sink`); a sink carried here would be dead weight for every
/// consumer of the queue.
#[derive(Debug, Clone)]
pub(crate) struct PendingJobStart {
    pub(crate) generation: u64,
    pub(crate) policy: RunnerPolicy,
    pub(crate) shell: ShellConfig,
    pub(crate) ssh: SshConfig,
    pub(crate) project_registry_dir: PathBuf,
    pub(crate) metadata: RunnerInvocationMetadata,
    pub(crate) operation: RunnerJobOperation,
}

#[cfg(test)]
impl PendingJobStart {
    pub(crate) fn from_wire(
        generation: u64,
        policy: RunnerPolicy,
        shell: ShellConfig,
        ssh: SshConfig,
        project_registry_dir: PathBuf,
        request: RunnerRequest,
    ) -> Self {
        let invocation = request
            .decode_invocation()
            .expect("test Job wire request must decode to a canonical invocation");
        let RunnerOperation::Job(operation) = invocation.operation else {
            panic!("test PendingJobStart requires a Job operation");
        };
        assert!(
            operation.is_start(),
            "test PendingJobStart requires a start operation"
        );
        Self {
            generation,
            policy,
            shell,
            ssh,
            project_registry_dir,
            metadata: invocation.metadata,
            operation,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JobManager {
    pub(crate) max_concurrent: usize,
    pub(crate) jobs: Arc<Mutex<HashMap<String, RunningJob>>>,
    pub(crate) detached_jobs: Arc<Mutex<HashMap<String, DetachedJobRef>>>,
    pub(crate) queued: Arc<Mutex<VecDeque<PendingJobStart>>>,
    pub(crate) prepared_profiles: PreparedShellProfileCache,
    pub(crate) ssh_pool: SshConnectionPool,
    pub(crate) lifecycle: Arc<Mutex<()>>,
    pub(crate) shutting_down: Arc<AtomicBool>,
    pub(crate) workers: ActivityTracker,
    pub(crate) current_sink: Arc<Mutex<Option<RunnerSink>>>,
    pub(crate) pending_job_updates: Arc<Mutex<HashMap<String, JobUpdateDeliveryQueue>>>,
    pub(crate) delivery_signal: Arc<JobUpdateDeliverySignal>,
    pub(crate) owner_lifetime: Option<Arc<JobManagerOwnerLifetime>>,
    pub(crate) detached_profile_server_url: String,
    #[cfg(test)]
    pub(crate) fail_detached_observer_spawn: Arc<AtomicBool>,
    #[cfg(test)]
    pub(crate) detached_store_root_override: Arc<Mutex<Option<PathBuf>>>,
}

impl JobManager {
    pub(crate) fn new(max_concurrent: usize) -> Self {
        let jobs = Arc::new(Mutex::new(HashMap::new()));
        let detached_jobs = Arc::new(Mutex::new(HashMap::new()));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let current_sink = Arc::new(Mutex::new(None));
        let pending_job_updates = Arc::new(Mutex::new(HashMap::new()));
        let delivery_signal = Arc::new(JobUpdateDeliverySignal::default());
        spawn_job_update_delivery_worker(
            Arc::downgrade(&jobs),
            Arc::downgrade(&current_sink),
            Arc::downgrade(&pending_job_updates),
            Arc::clone(&delivery_signal),
        );
        let owner_lifetime = Arc::new(JobManagerOwnerLifetime {
            jobs: Arc::downgrade(&jobs),
            detached_jobs: Arc::downgrade(&detached_jobs),
            shutting_down: Arc::downgrade(&shutting_down),
            delivery_signal: Arc::clone(&delivery_signal),
        });
        Self {
            max_concurrent: max_concurrent.max(1),
            jobs,
            detached_jobs,
            queued: Arc::new(Mutex::new(VecDeque::new())),
            prepared_profiles: PreparedShellProfileCache::default(),
            ssh_pool: SshConnectionPool::default(),
            lifecycle: Arc::new(Mutex::new(())),
            shutting_down,
            workers: ActivityTracker::default(),
            current_sink,
            pending_job_updates,
            delivery_signal,
            owner_lifetime: Some(owner_lifetime),
            detached_profile_server_url: String::new(),
            #[cfg(test)]
            fail_detached_observer_spawn: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            detached_store_root_override: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn with_detached_profile_identity(mut self, server_url: &str) -> Self {
        self.detached_profile_server_url = server_url.to_string();
        self
    }

    pub(crate) fn clone_for_worker(&self) -> Self {
        let mut worker = self.clone();
        worker.owner_lifetime = None;
        worker
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DetachedJobRef {
    pub(crate) store: DetachedJobStore,
    pub(crate) execution_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RunningJob {
    pub(crate) client_id: String,
    pub(crate) runner_instance_id: String,
    pub(crate) snapshot: ShellJobSnapshot,
    /// The single owner of the job's process tree. Clones are shared with the
    /// job's worker thread so it can poll the direct child and terminate the
    /// whole tree, but there is never more than one live `ManagedChild`.
    pub(crate) child: Option<Arc<Mutex<ManagedChild>>>,
    pub(crate) stop_requested: Arc<AtomicBool>,
    pub(crate) slot_reserved: bool,
}

#[cfg(test)]
pub(crate) fn test_job_snapshot(job_id: &str) -> ShellJobSnapshot {
    ShellJobSnapshot {
        job_id: job_id.to_string(),
        request_id: format!("request-{job_id}"),
        status: "running".to_string(),
        update_seq: 1,
        created_at: chrono::Utc::now().timestamp(),
        started_at: Some(chrono::Utc::now().timestamp()),
        ended_at: None,
        exit_code: None,
        duration_ms: None,
        error: None,
        command_execution_state: None,
        context: runner_protocol::ShellJobContext {
            document: None,
            runtime_project_id: None,
            workflow_session_id: None,
            ssh_resource: None,
            project_cwd: None,
            cwd: None,
            purpose: Some("other".to_string()),
            shell: Some("configured".to_string()),
            command_preview: "test job".to_string(),
            validation_steps: Vec::new(),
            validation: None,
            structured_execution: None,
        },
        stdout: ShellJobStreamSnapshot::default(),
        stderr: ShellJobStreamSnapshot::default(),
        validation_progress: None,
        activity: None,
    }
}

#[cfg(test)]
pub(crate) fn test_job_context(
    cwd: &Path,
    validation_steps: Vec<String>,
) -> runner_protocol::ShellJobContext {
    runner_protocol::ShellJobContext {
        document: None,
        runtime_project_id: None,
        workflow_session_id: None,
        ssh_resource: None,
        project_cwd: None,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        purpose: Some("other".to_string()),
        shell: Some("configured".to_string()),
        command_preview: "test command".to_string(),
        validation_steps,
        validation: None,
        structured_execution: None,
    }
}

#[derive(Debug)]
pub(crate) enum OutputChunk {
    Stdout(String),
    Stderr(String),
}

#[derive(Debug, Default)]
pub(crate) struct RunnerJobDelta {
    pub(crate) status: String,
    pub(crate) stdout_chunk: Option<String>,
    pub(crate) stderr_chunk: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) error: Option<String>,
    pub(crate) command_execution_state: Option<ShellCommandExecutionState>,
    pub(crate) stream_limit_bytes: Option<usize>,
    pub(crate) validation_progress: Option<ShellJobValidationProgress>,
    pub(crate) activity: Option<ShellJobActivity>,
    pub(crate) finished: bool,
}

pub(crate) mod context;
pub(crate) mod detached;
pub(crate) mod execution;
pub(crate) mod lifecycle;
pub(crate) mod process;
pub(crate) mod queue;
pub(crate) mod shell;
pub(crate) mod updates;

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
use crate::test_support::*;

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
use webcodex_core::runner_protocol::{RunnerRequest, ShellJobContext, ShellJobStreamSnapshot};
