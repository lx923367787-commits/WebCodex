//! Concrete domain dependencies and their lifetimes.
//! The Workflow Session crate already provides the Session service; do not wrap
//! its stable ledger API in a second pass-through store. These services never
//! replace canonical admission or the independent underlying state authorities.
use super::activity::{ActivityRecorder, NoopActivityRecorder};
use super::checkpoint;
use super::observations::RuntimeObservations;
use super::permissions::PermissionEvaluator;
#[cfg(test)]
use super::runtime::ValidationTerminalReconciliationTestHook;
use super::SessionShellRegistry;
use crate::runner_http::RunnerRegistry;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Clone)]
pub(crate) struct ExecutionState {
    pub(crate) team_cli_watchers: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    pub(crate) coding_agent_runs: Arc<super::coding_agent::CodingAgentServerState>,
    pub(crate) checkpoint_store: checkpoint::CheckpointStore,
    pub(crate) session_shells: SessionShellRegistry,
    /// Per-runtime secret used only to authenticate opaque committed-range
    /// git_diff_hunks continuation state. Clones share the same key; a runtime
    /// restart intentionally invalidates old committed continuations fail-closed.
    pub(crate) git_diff_hunks_continuation_mac_key: Arc<[u8; 32]>,
}
impl Default for ExecutionState {
    fn default() -> Self {
        Self {
            team_cli_watchers: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            coding_agent_runs: Arc::new(super::coding_agent::CodingAgentServerState::default()),
            checkpoint_store: checkpoint::CheckpointStore::default(),
            session_shells: SessionShellRegistry::default(),
            git_diff_hunks_continuation_mac_key:
                super::runtime::new_git_diff_hunks_continuation_mac_key(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct GoalService {
    pub(crate) goal_controller: Arc<super::goal::GoalController>,
    database: Option<Arc<crate::Database>>,
}
impl Default for GoalService {
    fn default() -> Self {
        Self {
            goal_controller: Arc::new(super::goal::GoalController::default()),
            database: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct GatewayService {
    pub(crate) mcp_gateway: Arc<crate::mcp_gateway::McpGatewayRuntime>,
    pub(crate) plugin_gateway: Arc<crate::plugin_gateway::PluginGatewayRuntime>,
    pub(crate) ssh_resource_gateway: Arc<crate::ssh_resource_gateway::SshResourceGatewayRuntime>,
}
impl Default for GatewayService {
    fn default() -> Self {
        Self {
            mcp_gateway: Arc::new(crate::mcp_gateway::McpGatewayRuntime::default()),
            plugin_gateway: Arc::new(crate::plugin_gateway::PluginGatewayRuntime::default()),
            ssh_resource_gateway: Arc::new(
                crate::ssh_resource_gateway::SshResourceGatewayRuntime::default(),
            ),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeConfig {
    pub(crate) runtime_exposure: crate::model_surface::RuntimeExposure,
    pub(crate) semantic_navigation_probe_timeout: Duration,
    pub(crate) repository_overview_probe_timeout: Duration,
    /// One deadline shared by every item in a `read_files` batch.
    pub(crate) read_files_deadline: Duration,
    /// One deadline shared by every query in a `search_project_texts` batch.
    pub(crate) search_project_texts_deadline: Duration,
    /// Internal synchronous wait window for a read-only structured validation
    /// before it promotes to a Job. Defaults to `SYNC_VALIDATION_WAIT_SECS`;
    /// tests shrink it so the handoff path can be exercised without sleeping.
    pub(crate) validation_sync_wait: Duration,
    /// Runtime cap for the model-requested synchronous grace before typed
    /// process/script Job handoff. Production permits the public maximum;
    /// each call's StructuredExecutionBudget selects the actual wait. Tests
    /// shrink this cap to exercise handoff without sleeping.
    pub(crate) structured_execution_sync_wait: Duration,
    /// Authoritative permission evaluator for this runtime instance.
    /// Resolved once at construction (`WEBCODEX_AUTHORITY_MODE`); dispatch
    /// evaluates once per tool request before mutation.
    pub(crate) permission_evaluator: PermissionEvaluator,
}
impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            runtime_exposure: crate::model_surface::RuntimeExposure::Runtime(
                crate::model_surface::ModelSurface::LocalCoding,
            ),
            semantic_navigation_probe_timeout:
                super::semantic_navigation::DEFAULT_SEMANTIC_NAVIGATION_PROBE_TIMEOUT,
            repository_overview_probe_timeout:
                super::coding_task::DEFAULT_REPOSITORY_OVERVIEW_PROBE_TIMEOUT,
            read_files_deadline: super::read_files::DEFAULT_READ_FILES_DEADLINE,
            search_project_texts_deadline:
                super::search_project_texts::DEFAULT_SEARCH_PROJECT_TEXTS_DEADLINE,
            validation_sync_wait: Duration::from_secs(super::helpers::SYNC_VALIDATION_WAIT_SECS),
            structured_execution_sync_wait: Duration::from_secs(
                super::structured_execution::STRUCTURED_EXECUTION_SYNC_WAIT_MAX_SECS,
            ),
            permission_evaluator: PermissionEvaluator::from_env(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ObservationService {
    /// Sink for the workspace activity ledger (mutating tool executions).
    /// No-op unless the host injects a durable recorder.
    pub(crate) activity: Arc<dyn ActivityRecorder>,
    /// Cross-surface connection observations (connector endpoint activity,
    /// last successful meaningful tool call). Shared with the connector
    /// runtime; never stores payloads or secrets.
    pub(crate) observations: Arc<RuntimeObservations>,
    /// Process-local payload-free view of currently in-flight MCP Window
    /// requests. It is observability only and intentionally resets on restart.
    pub(crate) window_activity: Arc<super::window_activity::WindowActivityRegistry>,
    /// Fail-open metrics projection. This consumes canonical runtime/transport
    /// facts and has no authority over execution or persistence.
    pub(crate) metrics: Arc<dyn super::runtime_metrics::RuntimeMetrics>,
    /// Durable ActionAudit-backed Window activity query handle. This shares the
    /// normal Server SQLite database and never becomes an authorization store.
    pub(crate) window_activity_db: Option<Arc<crate::Database>>,
}
impl Default for ObservationService {
    fn default() -> Self {
        Self {
            activity: Arc::new(NoopActivityRecorder),
            observations: Arc::new(RuntimeObservations::default()),
            window_activity: Arc::new(super::window_activity::WindowActivityRegistry::default()),
            metrics: Arc::new(super::runtime_metrics::TracingRuntimeMetrics),
            window_activity_db: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct MemoryService {
    /// Optional Control-owned durable project Memory store. It is injected by
    /// the server from the existing webcodex.db handle; Runner-native project
    /// filesystems never own Memory v1 persistence.
    pub(crate) memory_db: Option<Arc<crate::Database>>,
}
impl Default for MemoryService {
    fn default() -> Self {
        Self { memory_db: None }
    }
}

#[derive(Clone)]
pub(crate) struct CommunicationService {
    /// Optional Control-owned durable Agent and Conversation store. It shares
    /// the Server SQLite handle with other durable domains but owns independent tables.
    pub(crate) communication_db: Option<Arc<crate::Database>>,
    /// Optional process-local Host continuation registry/controller. It is
    /// created only when the durable communication database is injected and is
    /// intentionally empty again after process restart.
    pub(crate) agent_continuations: Option<crate::agent_wake::AgentContinuationController>,
}
impl Default for CommunicationService {
    fn default() -> Self {
        Self {
            communication_db: None,
            agent_continuations: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ValidationService {
    /// Orders authoritative terminal-Job snapshot acquisition through Session
    /// marker/evidence materialization. Marker eviction interprets absence from
    /// that snapshot as retention exit, so a later snapshot must never commit
    /// before an earlier snapshot has finished using its eviction authority.
    /// Cloned runtimes share this mutex; restart drops all in-flight snapshots.
    pub(crate) validation_terminal_reconciliation: Arc<Mutex<()>>,
    #[cfg(test)]
    pub(crate) validation_terminal_reconciliation_test_hook:
        Arc<ValidationTerminalReconciliationTestHook>,
}
impl Default for ValidationService {
    fn default() -> Self {
        Self {
            validation_terminal_reconciliation: Arc::new(Mutex::new(())),
            #[cfg(test)]
            validation_terminal_reconciliation_test_hook: Arc::new(
                ValidationTerminalReconciliationTestHook::default(),
            ),
        }
    }
}

/// A narrow execution view borrows the one public compatibility registry.
/// It never keeps a second Arc that could diverge if a caller replaces the
/// existing public ToolRuntime.runner_registry field.
pub(crate) struct ExecutionService<'a> {
    pub(super) runner_registry: &'a Arc<RunnerRegistry>,
}

impl GoalService {
    pub(super) fn store(&self) -> Option<&crate::Database> {
        self.database.as_deref()
    }

    pub(super) fn attach_database(&mut self, db: Arc<crate::Database>) {
        self.database = Some(db);
    }
}

impl CommunicationService {
    pub(super) fn attach_database(&mut self, db: Arc<crate::Database>) {
        self.agent_continuations = Some(crate::agent_wake::AgentContinuationController::new(
            db.clone(),
        ));
        self.communication_db = Some(db);
    }
}

impl ObservationService {
    pub(super) fn record_activity(&self, record: super::activity::ActivityRecord<'_>) {
        self.activity.record(record);
    }
}
