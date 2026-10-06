//! Exhaustive typed ToolCall router. Business belongs to family handlers.
//! Parsing and authority remain in kernel + pipeline + specialized governance.
mod agent_task;
mod coding_agent;
mod communication;
mod goal;
mod memory;
mod pipeline;
mod projection;
mod skill;
use crate::auth::AuthContext;
use crate::tool_runtime::project_resolution::{ProjectResolverError, ResolvedProject};
use crate::tool_runtime::{sessions, ToolCall, ToolResult, ToolRuntime};
pub(crate) use projection::{
    decorate_structured_execution_prestart_denial, sparsify_complete_file_read_output,
    sparsify_complete_read_success, sparsify_failure_model_result_metadata,
    sparsify_search_batch_success_for_model, sparsify_search_output_for_model,
    sparsify_success_model_result_metadata,
};
impl ToolRuntime {
    async fn dispatch_authorized_inner(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
        ssh_resource: Option<&str>,
        validation_assertion_name: Option<&str>,
        project_resolution: Option<Result<ResolvedProject, ProjectResolverError>>,
        trusted_recording_session_id: Option<&str>,
        trusted_recording_session_project: Option<&str>,
        protocol_capabilities: crate::tool_runtime::kernel::ToolProtocolCapabilities,
        correlation: &mut crate::tool_runtime::window_activity::ToolCallCorrelation,
    ) -> ToolResult {
        match call {
            call @ (ToolCall::GoalModeStart { .. }
            | ToolCall::GoalPlanSave { .. }
            | ToolCall::GoalConfirmPlan { .. }
            | ToolCall::GoalBootstrap { .. }
            | ToolCall::GoalClaimNode { .. }
            | ToolCall::GoalCheckpoint { .. }
            | ToolCall::GoalCheckNode { .. }
            | ToolCall::GoalStart { .. }
            | ToolCall::GoalRead { .. }
            | ToolCall::GoalList { .. }
            | ToolCall::GoalCancel { .. }
            | ToolCall::GoalResume { .. }) => self.dispatch_goal_family(call, auth).await,
            call @ (ToolCall::ListTools { .. }
            | ToolCall::ListRunners { .. }
            | ToolCall::RuntimeStatus { .. }
            | ToolCall::ReadToolTrace { .. }
            | ToolCall::ToolManifest { .. }) => {
                self.dispatch_discovery_tool(call, auth, protocol_capabilities)
                    .await
            }

            call @ (ToolCall::RunnerConfigCheck { .. } | ToolCall::RunnerConfigReload { .. }) => {
                self.dispatch_runner_config_tool(call, auth).await
            }

            ToolCall::PluginTool(_) => {
                unreachable!(
                    "plugin_tool is dispatched before generic static ToolDefinition policy"
                )
            }

            ToolCall::SshResource(_) => {
                unreachable!(
                    "ssh_resource is dispatched before generic static ToolDefinition policy"
                )
            }

            call @ (ToolCall::StartSession { .. }
            | ToolCall::SessionSummary { .. }
            | ToolCall::UpdateSessionContext { .. }
            | ToolCall::CloseSession { .. }
            | ToolCall::ValidationSummary { .. }
            | ToolCall::PostSessionMessage { .. }
            | ToolCall::ListSessionMessages { .. }
            | ToolCall::GetSessionAssignment { .. }
            | ToolCall::ObserveSessionMessages { .. }
            | ToolCall::ResolveSessionMessage { .. }
            | ToolCall::CompleteSessionMessage { .. }
            | ToolCall::SessionDiscussionSummary { .. }) => {
                self.dispatch_session_tool(call, auth, transport).await
            }

            call @ (ToolCall::WorkOnProject { .. } | ToolCall::FinishCodingTask { .. }) => {
                self.dispatch_coding_task_tool(
                    call,
                    auth,
                    transport,
                    trusted_recording_session_id,
                    trusted_recording_session_project,
                    correlation,
                )
                .await
            }

            call @ ToolCall::SessionHandoffSummary { .. } => {
                self.dispatch_handoff_tool(call, auth).await
            }

            call @ (ToolCall::WorkspaceCheckpointCreate { .. }
            | ToolCall::WorkspaceCheckpointList { .. }
            | ToolCall::WorkspaceCheckpointShow { .. }
            | ToolCall::WorkspaceCheckpointRestore { .. }
            | ToolCall::WorkspaceCheckpointDelete { .. }) => {
                self.dispatch_workspace_checkpoint_tool(call).await
            }

            call @ (ToolCall::ComputerListTargets
            | ToolCall::ComputerListWindows { .. }
            | ToolCall::ComputerListApplications { .. }
            | ToolCall::ComputerListDisplays { .. }
            | ToolCall::ComputerLaunchApplication { .. }
            | ToolCall::ComputerAccessibilityStatus { .. }
            | ToolCall::ComputerAccessibilityTree { .. }
            | ToolCall::ComputerFindElements { .. }
            | ToolCall::ComputerElementState { .. }
            | ToolCall::ComputerActivateWindow { .. }
            | ToolCall::ComputerControl { .. }
            | ToolCall::ComputerScrollToElement { .. }
            | ToolCall::ComputerKeyInput { .. }
            | ToolCall::ComputerReadClipboard { .. }
            | ToolCall::ComputerWriteClipboard { .. }
            | ToolCall::ComputerInputText { .. }
            | ToolCall::ComputerSnapshot { .. }
            | ToolCall::ComputerSnapshotDisplay { .. }
            | ToolCall::ComputerPointerMove { .. }
            | ToolCall::ComputerPointerClick { .. }
            | ToolCall::ComputerSaveSnapshot { .. }) => {
                self.dispatch_computer_tool(call, auth).await
            }

            call @ (ToolCall::ListProjects { .. }
            | ToolCall::RegisterProject { .. }
            | ToolCall::UnregisterProject { .. }
            | ToolCall::CreateProject { .. }) => self.dispatch_project_tool(call, auth).await,

            call @ (ToolCall::CodingAgentStart { .. }
            | ToolCall::CodingAgentObserve { .. }
            | ToolCall::CodingAgentCancel { .. }) => {
                self.dispatch_coding_agent_family(call, auth).await
            }
            call @ (ToolCall::PresentDocument(_)
            | ToolCall::DocumentEdit(_)
            | ToolCall::DocumentConvert(_)
            | ToolCall::DocumentRestore(_)) => self.dispatch_document_tool(call, auth).await,

            call @ (ToolCall::RunProcess { .. }
            | ToolCall::RunDetachedProcess { .. }
            | ToolCall::RunScript { .. }
            | ToolCall::RunShell { .. }) => {
                self.dispatch_shell_tool(call, ssh_resource, validation_assertion_name, auth)
                    .await
            }

            call @ (ToolCall::OpenSessionShell { .. }
            | ToolCall::SessionShellExec { .. }
            | ToolCall::SessionShellStatus { .. }
            | ToolCall::CloseSessionShell { .. }) => {
                self.dispatch_session_shell_tool(call, ssh_resource).await
            }

            call @ (ToolCall::ApplyPatch { .. } | ToolCall::ApplyUnifiedDiff { .. }) => {
                self.dispatch_patch_tool(call).await
            }

            call @ (ToolCall::SkillList { .. }
            | ToolCall::SkillReadFile { .. }
            | ToolCall::SkillVersions { .. }
            | ToolCall::SkillInstall { .. }
            | ToolCall::SkillActivate { .. }
            | ToolCall::SkillRemoveRevision { .. }) => {
                self.dispatch_skill_family(call, auth, project_resolution)
                    .await
            }
            call @ (ToolCall::CreateAgentTask { .. }
            | ToolCall::ListAgentTasks { .. }
            | ToolCall::ReadAgentTask { .. }
            | ToolCall::AssignAgentTask { .. }
            | ToolCall::StartAgentTaskAttempt { .. }
            | ToolCall::StartAgentTaskCodingRun { .. }
            | ToolCall::ReconcileAgentTaskCodingRun { .. }
            | ToolCall::HeartbeatAgentTaskAttempt { .. }
            | ToolCall::CompleteAgentTaskAttempt { .. }) => {
                self.dispatch_agent_task_family(call, auth).await
            }
            call @ (ToolCall::CreateAgentIdentity { .. }
            | ToolCall::ListAgentIdentities { .. }
            | ToolCall::UpdateAgentIdentity { .. }
            | ToolCall::AttachAgentEndpoint { .. }
            | ToolCall::DetachAgentEndpoint { .. }
            | ToolCall::CreateConversation { .. }
            | ToolCall::ListConversations { .. }
            | ToolCall::ReadConversation { .. }
            | ToolCall::PostConversationMessage { .. }
            | ToolCall::ListAgentInbox { .. }
            | ToolCall::ConsumeAgentDeliveries { .. }
            | ToolCall::BootstrapAgentConversation { .. }
            | ToolCall::ConsumeAgentWake { .. }) => {
                self.dispatch_communication_family(call, auth).await
            }
            call @ (ToolCall::MemorySearch { .. }
            | ToolCall::MemoryRead { .. }
            | ToolCall::MemorySet { .. }
            | ToolCall::MemoryDelete { .. }
            | ToolCall::MemoryScopeList { .. }
            | ToolCall::MemoryScopePurge { .. }) => {
                self.dispatch_memory_family(call, auth, project_resolution)
                    .await
            }
            call @ ToolCall::ImportConversationFilesToProject { .. } => {
                self.dispatch_conversation_import_tool(call, auth, transport)
                    .await
            }

            call @ (ToolCall::DeleteProjectFiles { .. }
            | ToolCall::ReadFile { .. }
            | ToolCall::ReadFiles { .. }
            | ToolCall::ListProjectFiles { .. }
            | ToolCall::ListProjectTrackedFiles { .. }
            | ToolCall::ProjectOverview { .. }
            | ToolCall::SearchProjectText { .. }
            | ToolCall::SearchProjectTexts { .. }
            | ToolCall::WriteProjectFile { .. }
            | ToolCall::SaveProjectArtifact { .. }
            | ToolCall::ExportProjectArtifact { .. }
            | ToolCall::ReadProjectArtifactMetadata { .. }
            | ToolCall::ReadProjectArtifact { .. }
            | ToolCall::ArtifactUploadBegin { .. }
            | ToolCall::ArtifactUploadChunk { .. }
            | ToolCall::ArtifactUploadFinish { .. }
            | ToolCall::ArtifactUploadAbort { .. }
            | ToolCall::ApplyTextEdits { .. }) => {
                self.dispatch_file_tool(call, transport, project_resolution, auth)
                    .await
            }

            call @ (ToolCall::GitRestorePaths { .. }
            | ToolCall::DiscardUntracked { .. }
            | ToolCall::GitCommitPaths { .. }
            | ToolCall::GitStatus { .. }
            | ToolCall::GitDiff { .. }
            | ToolCall::GitDiffHunks { .. }
            | ToolCall::GitReviewSummary { .. }
            | ToolCall::GitLog { .. }
            | ToolCall::GitDiffSummary { .. }
            | ToolCall::ShowChanges { .. }) => self.dispatch_git_tool(call).await,

            call @ (ToolCall::CargoFmt { .. }
            | ToolCall::CargoCheck { .. }
            | ToolCall::CargoTest { .. }
            | ToolCall::GoTest { .. }) => self.dispatch_cargo_tool(call, ssh_resource, auth).await,

            call @ (ToolCall::RunJob { .. }
            | ToolCall::StopJob { .. }
            | ToolCall::JobStatus { .. }
            | ToolCall::JobLog { .. }
            | ToolCall::ObserveJobs { .. }
            | ToolCall::ListJobs { .. }
            | ToolCall::JobTail { .. }) => self.dispatch_job_tool(call, auth, ssh_resource).await,

            call @ ToolCall::WorkspaceHygieneCheck { .. } => self.dispatch_hygiene_tool(call).await,

            call @ (ToolCall::LspStatus { .. }
            | ToolCall::DocumentSymbols { .. }
            | ToolCall::DocumentDiagnostics { .. }
            | ToolCall::Hover { .. }
            | ToolCall::WorkspaceSymbols { .. }
            | ToolCall::GotoDefinition { .. }
            | ToolCall::FindReferences { .. }
            | ToolCall::CallHierarchy { .. }) => self.dispatch_lsp_tool(call).await,
        }
    }
}
