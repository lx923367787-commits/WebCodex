use super::activity::ActivityRecorder;
#[cfg(test)]
use super::permissions::PermissionEvaluator;
use super::runtime_info::RuntimeInfo;
use super::sessions;
use crate::runner_http::RunnerRegistry;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio::sync::Semaphore;
use uuid::Uuid;

pub(super) fn new_git_diff_hunks_continuation_mac_key() -> Arc<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"webcodex.git-diff-hunks.runtime-mac-key.v1\0");
    hasher.update(Uuid::new_v4().as_bytes());
    hasher.update(Uuid::new_v4().as_bytes());
    Arc::new(hasher.finalize().into())
}

#[cfg(test)]
pub(crate) struct ValidationTerminalReconciliationTestHook {
    reconciliation_attempted: Semaphore,
    pause_next_after_snapshot: AtomicBool,
    snapshot_acquired: Semaphore,
    resume_snapshot: Semaphore,
    snapshot_acquisition_count: AtomicUsize,
}

#[cfg(test)]
impl Default for ValidationTerminalReconciliationTestHook {
    fn default() -> Self {
        Self {
            pause_next_after_snapshot: AtomicBool::new(false),
            reconciliation_attempted: Semaphore::new(0),
            snapshot_acquired: Semaphore::new(0),
            resume_snapshot: Semaphore::new(0),
            snapshot_acquisition_count: AtomicUsize::new(0),
        }
    }
}

#[cfg(test)]
impl ValidationTerminalReconciliationTestHook {
    pub(crate) fn before_reconciliation_lock(&self) {
        self.reconciliation_attempted.add_permits(1);
    }

    pub(crate) async fn wait_for_reconciliation_attempt(&self) {
        let permit = self
            .reconciliation_attempted
            .acquire()
            .await
            .expect("validation terminal reconciliation attempt semaphore closed");
        permit.forget();
    }

    pub(crate) fn pause_next_snapshot(&self) {
        assert!(
            !self.pause_next_after_snapshot.swap(true, Ordering::SeqCst),
            "validation terminal snapshot pause already armed"
        );
    }

    pub(crate) async fn after_snapshot_acquired(&self) {
        self.snapshot_acquisition_count
            .fetch_add(1, Ordering::SeqCst);
        self.snapshot_acquired.add_permits(1);
        if self.pause_next_after_snapshot.swap(false, Ordering::SeqCst) {
            let permit = self
                .resume_snapshot
                .acquire()
                .await
                .expect("validation terminal snapshot resume semaphore closed");
            permit.forget();
        }
    }

    pub(crate) async fn wait_for_snapshot_acquired(&self) {
        let permit = self
            .snapshot_acquired
            .acquire()
            .await
            .expect("validation terminal snapshot semaphore closed");
        permit.forget();
    }

    pub(crate) fn resume_snapshot(&self) {
        self.resume_snapshot.add_permits(1);
    }

    pub(crate) fn snapshot_acquisition_count(&self) -> usize {
        self.snapshot_acquisition_count.load(Ordering::SeqCst)
    }
}

/// Composition root and compatibility facade for the canonical Tool Kernel.
/// Domain services own their dependencies; public registry/info fields retain
/// their existing API and have no duplicate authoritative copy.
#[derive(Clone)]
pub struct ToolRuntime {
    pub runner_registry: Arc<RunnerRegistry>,
    pub runtime_info: Arc<RuntimeInfo>,
    pub(crate) sessions: sessions::SessionStore,
    pub(crate) execution_state: super::services::ExecutionState,
    pub(crate) goals: super::services::GoalService,
    pub(crate) gateways: super::services::GatewayService,
    pub(crate) config: super::services::RuntimeConfig,
    pub(crate) observation: super::services::ObservationService,
    pub(crate) memory: super::services::MemoryService,
    pub(crate) communication: super::services::CommunicationService,
    pub(crate) validation: super::services::ValidationService,
}

impl ToolRuntime {
    pub fn new(runner_registry: Arc<RunnerRegistry>, runtime_info: Arc<RuntimeInfo>) -> Self {
        Self {
            runner_registry,
            runtime_info,
            sessions: sessions::SessionStore::default(),
            execution_state: super::services::ExecutionState::default(),
            goals: super::services::GoalService::default(),
            gateways: super::services::GatewayService::default(),
            config: super::services::RuntimeConfig::default(),
            observation: super::services::ObservationService::default(),
            memory: super::services::MemoryService::default(),
            communication: super::services::CommunicationService::default(),
            validation: super::services::ValidationService::default(),
        }
    }

    pub(crate) fn with_runtime_exposure(
        mut self,
        runtime_exposure: crate::model_surface::RuntimeExposure,
    ) -> Self {
        self.config.runtime_exposure = runtime_exposure;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_model_surface(
        self,
        model_surface: crate::model_surface::ModelSurface,
    ) -> Self {
        self.with_runtime_exposure(crate::model_surface::RuntimeExposure::Runtime(
            model_surface,
        ))
    }

    pub(crate) fn runtime_exposure(&self) -> crate::model_surface::RuntimeExposure {
        self.config.runtime_exposure
    }

    pub(crate) fn model_surface(&self) -> Option<crate::model_surface::ModelSurface> {
        self.config.runtime_exposure.model_surface()
    }

    /// Attach a durable workspace-activity recorder (server wiring).
    pub fn with_activity_recorder(mut self, recorder: Arc<dyn ActivityRecorder>) -> Self {
        self.observation.activity = recorder;
        self
    }

    pub(crate) fn with_window_activity_database(mut self, db: Arc<crate::Database>) -> Self {
        self.observation.window_activity_db = Some(db);
        self
    }

    pub(crate) fn window_activity_registry(
        &self,
    ) -> Arc<super::window_activity::WindowActivityRegistry> {
        self.observation.window_activity.clone()
    }

    pub(crate) fn with_memory_database(mut self, db: Arc<crate::Database>) -> Self {
        self.memory.memory_db = Some(db);
        self
    }

    pub(crate) fn with_communication_database(mut self, db: Arc<crate::Database>) -> Self {
        self.goals.attach_database(db.clone());
        self.communication.attach_database(db);
        self
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests() -> Self {
        Self::new_for_tests_with_runner_registry(Arc::new(RunnerRegistry::default()))
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests_with_runner_registry(runner_registry: Arc<RunnerRegistry>) -> Self {
        Self::new(runner_registry, Arc::new(RuntimeInfo::default()))
    }

    pub fn with_session_ledger(mut self, path: impl Into<PathBuf>) -> Self {
        self.sessions = sessions::SessionStore::with_persistence(
            path,
            sessions::DEFAULT_MAX_SESSIONS,
            sessions::DEFAULT_MAX_EVENTS_PER_SESSION,
        );
        self
    }

    pub(crate) fn workflow_sessions_console_list(
        &self,
        project: &str,
        limit: Option<usize>,
    ) -> sessions::WorkflowSessionConsoleList {
        self.sessions
            .console_list_for_project(project, limit, sessions::console_validation_hooks())
    }

    pub(crate) fn workflow_session_console_detail(
        &self,
        project: &str,
        session_id: &str,
        limit: Option<usize>,
    ) -> Option<sessions::WorkflowSessionConsoleDetail> {
        self.sessions.console_detail_for_project(
            project,
            session_id,
            limit,
            sessions::console_validation_hooks(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_semantic_navigation_probe_timeout(mut self, timeout: Duration) -> Self {
        self.config.semantic_navigation_probe_timeout = timeout;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_repository_overview_probe_timeout(mut self, timeout: Duration) -> Self {
        self.config.repository_overview_probe_timeout = timeout;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_read_files_deadline(mut self, deadline: Duration) -> Self {
        self.config.read_files_deadline = deadline;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_search_project_texts_deadline(mut self, deadline: Duration) -> Self {
        self.config.search_project_texts_deadline = deadline;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_validation_sync_wait(mut self, wait: Duration) -> Self {
        self.config.validation_sync_wait = wait;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_structured_execution_sync_wait(mut self, wait: Duration) -> Self {
        self.config.structured_execution_sync_wait = wait;
        self
    }

    /// Replace the permission evaluator (tests: mode matrix / single-eval counters).
    #[cfg(test)]
    pub(crate) fn with_permission_evaluator(mut self, evaluator: PermissionEvaluator) -> Self {
        self.config.permission_evaluator = evaluator;
        self
    }
}

impl ToolRuntime {
    pub(crate) fn execution(&self) -> super::services::ExecutionService<'_> {
        super::services::ExecutionService {
            runner_registry: &self.runner_registry,
        }
    }

    pub(crate) async fn memory_scope_list(
        &self,
        auth: Option<&crate::auth::AuthContext>,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> super::ToolResult {
        self.memory
            .memory_scope_list(&self.runner_registry, auth, offset, limit)
            .await
    }

    pub(crate) async fn memory_scope_purge(
        &self,
        auth: Option<&crate::auth::AuthContext>,
        memory_scope_id: String,
        expected_catalog_revision: String,
        confirm: bool,
    ) -> super::ToolResult {
        self.memory
            .memory_scope_purge(
                &self.runner_registry,
                auth,
                memory_scope_id,
                expected_catalog_revision,
                confirm,
            )
            .await
    }
}
