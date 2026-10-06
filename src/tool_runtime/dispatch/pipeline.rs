//! Canonical typed dispatch admission and evidence pipeline.
//! All families pass through the same existing gates in the same order.
use crate::auth::AuthContext;
use crate::tool_runtime::edit_tool_telemetry;
use crate::tool_runtime::session_context::{
    add_session_hint, session_guard_denied_result, session_lifecycle_denied_result,
    session_project_mismatch_result, SessionProjectMismatch,
};
use crate::tool_runtime::{
    permissions, session_context, sessions, ToolCall, ToolResult, ToolRuntime,
};
use serde_json::Value;

use super::projection::*;
/// Snapshot of the activity-relevant request facts, captured before the
/// `ToolCall` is moved into execution.
struct WorkspaceActivityContext {
    tool: &'static str,
    project: Option<String>,
    client: Option<String>,
    command: Option<String>,
    paths: Vec<String>,
}

impl ToolRuntime {
    /// Main dispatch — call from MCP handler or GPT Actions handler.
    ///
    /// This no-auth convenience defaults the caller context to `None`, which
    /// means Runner-backed tools are rejected (no owner can be proven). HTTP
    /// wrappers should prefer `dispatch_with_auth` so the depot `AuthContext`
    /// is forwarded. Tests use this wrapper for local-executor projects.
    #[cfg(test)]
    pub async fn dispatch(&self, call: ToolCall) -> ToolResult {
        self.dispatch_with_auth(call, None).await
    }

    /// Dispatch carrying the caller's auth context. Runner-backed tools enforce
    /// the owner boundary and capability requirements through
    /// `authorize_runner_tool`; local-executor tools are unaffected. Wrappers
    /// stay thin: they only forward the depot `AuthContext` here.
    pub async fn dispatch_with_auth(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        self.dispatch_with_auth_transport(call, auth, sessions::SessionTransport::Api)
            .await
    }

    pub(crate) async fn dispatch_with_auth_transport(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
    ) -> ToolResult {
        self.dispatch_with_auth_transport_options(call, auth, transport)
            .await
    }

    pub(crate) async fn dispatch_with_auth_transport_options(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
    ) -> ToolResult {
        self.dispatch_with_auth_transport_options_and_metadata(
            call,
            auth,
            transport,
            sessions::ToolCallRecorderMetadata::default(),
        )
        .await
    }

    pub(crate) async fn dispatch_with_auth_transport_options_and_metadata(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
        recorder_metadata: sessions::ToolCallRecorderMetadata,
    ) -> ToolResult {
        self.dispatch_with_auth_transport_options_and_metadata_with_window(
            call,
            auth,
            transport,
            recorder_metadata,
            None,
        )
        .await
    }

    pub(crate) async fn dispatch_with_auth_transport_options_and_metadata_with_window(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
        recorder_metadata: sessions::ToolCallRecorderMetadata,
        window: Option<&crate::client_window::ClientWindow>,
    ) -> ToolResult {
        self.dispatch_with_auth_transport_options_and_metadata_with_recording_mode(
            call,
            auth,
            transport,
            recorder_metadata,
            window,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_with_auth_transport_options_and_metadata_with_recording_mode(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
        recorder_metadata: sessions::ToolCallRecorderMetadata,
        window: Option<&crate::client_window::ClientWindow>,
        inner_model_facing_recording: bool,
    ) -> ToolResult {
        self.dispatch_with_auth_transport_options_and_metadata_with_recording_mode_and_context(
            call,
            auth,
            transport,
            recorder_metadata,
            window,
            inner_model_facing_recording,
            Vec::new(),
            crate::tool_runtime::context_projection::ContextMaterialCapabilities::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_with_auth_transport_options_and_metadata_with_recording_mode_and_context(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
        recorder_metadata: sessions::ToolCallRecorderMetadata,
        window: Option<&crate::client_window::ClientWindow>,
        inner_model_facing_recording: bool,
        context_request: Vec<String>,
        material_capabilities: crate::tool_runtime::context_projection::ContextMaterialCapabilities,
    ) -> ToolResult {
        let (mut result, projection, _) = self
            .dispatch_with_auth_transport_options_and_metadata_with_recording_mode_and_context_with_result_projection(
            call,
            auth,
            transport,
            recorder_metadata,
            window,
            inner_model_facing_recording,
            context_request,
            material_capabilities,
            crate::tool_runtime::kernel::ToolProtocolCapabilities::default(),
        )
        .await;
        projection.project(&mut result);
        result
    }

    /// Kernel-only companion that returns the terminal model-facing projection
    /// plan after the same authoritative Project resolution used for execution.
    /// The returned ToolResult is still canonical with respect to read/search
    /// budgeting and sparse projection so an outer recorder can consume it first.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_with_auth_transport_options_and_metadata_with_recording_mode_and_context_with_result_projection(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
        recorder_metadata: sessions::ToolCallRecorderMetadata,
        window: Option<&crate::client_window::ClientWindow>,
        inner_model_facing_recording: bool,
        context_request: Vec<String>,
        material_capabilities: crate::tool_runtime::context_projection::ContextMaterialCapabilities,
        protocol_capabilities: crate::tool_runtime::kernel::ToolProtocolCapabilities,
    ) -> (
        ToolResult,
        ModelFacingProjectionPlan,
        crate::tool_runtime::window_activity::ToolCallCorrelation,
    ) {
        let mut result_projection = ModelFacingProjectionPlan::capture(&call);
        let mut correlation = crate::tool_runtime::window_activity::ToolCallCorrelation::default();
        // Edit usage telemetry retains only fixed safe classifications. For
        // apply_patch it captures the requested matching enum before the call is
        // moved, never the patch/path/content arguments.
        let mut edit_usage = edit_tool_telemetry::start_edit_tool_usage_for_call(&call);
        let mut result = self
            .dispatch_with_auth_transport_options_and_metadata_inner(
                call,
                auth,
                transport,
                recorder_metadata.clone(),
                window,
                inner_model_facing_recording,
                context_request.clone(),
                material_capabilities,
                protocol_capabilities,
                &mut correlation,
                &mut result_projection,
            )
            .await;
        // Early project/session/auth failures can return before the normal
        // resolved-project sidecar hook. Preserve the main ToolResult while still
        // answering the explicit sidecar request conservatively: static material
        // remains available, but project-scoped material must not guess a target.
        if !context_request.is_empty() && result.output.get("context_projection").is_none() {
            self.add_requested_context_projection(
                &mut result,
                &context_request,
                None,
                auth,
                material_capabilities,
            )
            .await;
        }
        if let Some(guard) = edit_usage.as_mut() {
            guard.finish_with_result(&result);
        }
        (result, result_projection, correlation)
    }

    /// Everything the activity ledger needs from a call, captured before the
    /// call value is moved into execution. `None` for non-mutating tools.
    fn capture_workspace_activity_context(
        call: &ToolCall,
        resolved_project: Option<&str>,
    ) -> Option<WorkspaceActivityContext> {
        let tool = call.tool_name();
        let mutating = crate::tool_runtime::tool_definition::runtime_tool_is_write_like(tool)
            || crate::tool_runtime::tool_definition::runtime_tool_is_shell_like(tool);
        if !mutating {
            return None;
        }
        let sanitized = call.session_log_arguments();
        let project = resolved_project.or_else(|| call.project());
        Some(WorkspaceActivityContext {
            tool,
            project: project.map(str::to_string),
            client: project
                .and_then(crate::tool_runtime::activity::runner_client_from_project)
                .map(str::to_string),
            command: match call {
                ToolCall::RunProcess {
                    executable, args, ..
                } => Some(crate::runner_http::process_preview(
                    executable,
                    args.iter().map(String::as_str),
                )),
                ToolCall::RunDetachedProcess { args, .. } => {
                    Some(format!("detached process ({} args)", args.len()))
                }
                ToolCall::RunScript {
                    language,
                    script,
                    args,
                    ..
                } => Some(crate::runner_http::script_preview(
                    language.as_str(),
                    script.len(),
                    args.len(),
                )),
                _ => call.command_text().map(str::to_string),
            },
            paths: crate::tool_runtime::activity::paths_from_sanitized_arguments(&sanitized, 16),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_dispatch_session_result(
        &self,
        result: &mut ToolResult,
        session_id: &str,
        start: Option<sessions::ToolCallStart>,
        tool_name: &str,
        error_kind: Option<&str>,
        auth: Option<&AuthContext>,
        model_facing: bool,
        ack_observation: Option<&sessions::SessionAckObservation>,
        ack_requested: bool,
    ) {
        let success = result.success;
        let error = result.error.clone();
        if model_facing {
            let session_output = crate::tool_runtime::tool_audit::session_log_result_for_tool(
                tool_name,
                &result.output,
            );
            let recorded = self.sessions.record_model_facing_tool_call_finished(
                start,
                success,
                &session_output,
                error.as_deref(),
                error_kind,
            );
            add_session_hint(result, &self.sessions, session_id);
            if let Some(recorded) = recorded.as_ref() {
                if session_context::add_session_context_continuity(result, recorded) {
                    self.add_session_history_recovery(result, recorded, auth)
                        .await;
                }
            }
            if let Some(ack) = ack_observation {
                session_context::add_session_attention_projection(
                    result,
                    &self.sessions,
                    session_id,
                    ack,
                    ack_requested,
                );
            }
        } else {
            self.sessions.record_tool_call_finished(
                start,
                success,
                &result.output,
                error.as_deref(),
                error_kind,
            );
            add_session_hint(result, &self.sessions, session_id);
        }
    }

    async fn dispatch_with_auth_transport_options_and_metadata_inner(
        &self,
        mut call: ToolCall,
        auth: Option<&AuthContext>,
        transport: sessions::SessionTransport,
        mut recorder_metadata: sessions::ToolCallRecorderMetadata,
        _window: Option<&crate::client_window::ClientWindow>,
        inner_model_facing_recording: bool,
        context_request: Vec<String>,
        material_capabilities: crate::tool_runtime::context_projection::ContextMaterialCapabilities,
        protocol_capabilities: crate::tool_runtime::kernel::ToolProtocolCapabilities,
        correlation: &mut crate::tool_runtime::window_activity::ToolCallCorrelation,
        result_projection: &mut ModelFacingProjectionPlan,
    ) -> ToolResult {
        call = call
            .with_coding_agent_recording_session_id(recorder_metadata.recording_session_id.clone());
        if let ToolCall::PluginTool(plugin) = call {
            return match crate::plugin_gateway::invoke(
                self,
                plugin,
                recorder_metadata.recording_session_id.as_deref(),
                auth,
                transport,
            )
            .await
            {
                Ok(invocation) => invocation.to_tool_result(),
                Err(crate::tool_runtime::specialized::SpecializedGovernanceDenial::Scope {
                    required_scope,
                    description,
                }) => ToolResult::err_with_output(
                    description,
                    serde_json::json!({
                        "failure_kind": "insufficient_scope",
                        "required_scope": required_scope,
                        "dispatch_certainty": "not_started",
                    }),
                ),
                Err(crate::tool_runtime::specialized::SpecializedGovernanceDenial::Tool(
                    result,
                )) => result,
            };
        }
        if let ToolCall::SshResource(ssh_resource) = call {
            return match crate::ssh_resource_gateway::invoke(
                self,
                ssh_resource,
                recorder_metadata.recording_session_id.as_deref(),
                auth,
                transport,
            )
            .await
            {
                Ok(invocation) => invocation.to_tool_result(),
                Err(crate::tool_runtime::specialized::SpecializedGovernanceDenial::Scope {
                    required_scope,
                    description,
                }) => ToolResult::err_with_output(
                    description,
                    serde_json::json!({
                        "failure_kind": "insufficient_scope",
                        "required_scope": required_scope,
                        "dispatch_certainty": "not_started",
                    }),
                ),
                Err(crate::tool_runtime::specialized::SpecializedGovernanceDenial::Tool(
                    result,
                )) => result,
            };
        }
        // Kernel requests arrive with the same trusted logical identity already
        // used by the outer recorder. Mark only this concrete ledger path as the
        // authoritative business role; direct/internal dispatch without a kernel
        // identity remains uncorrelated legacy-style evidence.
        recorder_metadata.mark_business_execution();
        let project_resolution = match call.project() {
            Some(project) => Some(
                self.execution()
                    .resolve_project_input_for_auth(project, auth)
                    .await,
            ),
            None => None,
        };
        let resolved_project = project_resolution
            .as_ref()
            .and_then(|resolution| resolution.as_ref().ok());
        result_projection.bind_resolved_project(resolved_project);
        // Preserve the canonical project for activity attribution before the
        // session recorder consumes the resolved value below. Short aliases
        // must not turn a real Runner execution into a client-less row.
        let activity_project = resolved_project
            .as_ref()
            .map(|resolved| resolved.resolved_id.clone());
        correlation.resolved_project = activity_project.clone();
        if let (Some(project), Some(trace_id)) = (
            activity_project.as_deref(),
            crate::tool_request_trace::current_active_trace_id(),
        ) {
            self.observation
                .window_activity
                .update(&trace_id, None, Some(project));
        }
        let context_projection_project = if context_request.is_empty() {
            None
        } else {
            resolved_project.cloned()
        };
        // work_on_project.session_id is explicit coding-resume business input,
        // never a generic tool recorder. Its implementation delegates exact
        // Session/project/lifecycle/authority handling to the coding workflow
        // engine.
        let defer_work_session = matches!(&call, ToolCall::WorkOnProject { .. });
        // session_handoff_summary.session_id remains business input for direct
        // internal dispatch. When the kernel already has an explicit outer
        // recording Session, suppress this inner recorder so worker W reading
        // coordinator C records the tool execution only in W.
        let suppress_handoff_business_recorder = !inner_model_facing_recording
            && matches!(&call, ToolCall::SessionHandoffSummary { .. });
        let session_id = if defer_work_session || suppress_handoff_business_recorder {
            None
        } else {
            call.session_id().map(str::to_string)
        };
        if let Some(session_id) = session_id.as_deref() {
            // Direct/internal dispatch may derive a recorder from explicit
            // business session_id (notably the handoff compatibility path).
            // Fence that exact Session before lifecycle/guard inheritance,
            // project mismatch logic, or any ledger mutation.
            if let Err(mut result) = self
                .authorize_session_target(session_id, call.tool_name(), auth)
                .await
            {
                decorate_structured_execution_prestart_denial(
                    call.tool_name(),
                    &mut result,
                    "session_authority_denied",
                );
                return result;
            }
        }
        let inner_ack_requested =
            inner_model_facing_recording && !recorder_metadata.ack_session_message_ids.is_empty();
        let inner_ack_observation = if inner_model_facing_recording {
            session_id.as_deref().map(|session_id| {
                session_context::observe_session_attention_acks(
                    &self.sessions,
                    session_id,
                    &recorder_metadata.ack_session_message_ids,
                )
            })
        } else {
            None
        };
        let session_contract =
            crate::tool_runtime::sessions::session_tool_contract(call.tool_name());
        let session_project_mismatch = session_id.as_deref().and_then(|session_id| {
            match (
                self.sessions.session_project(session_id),
                resolved_project.as_ref(),
            ) {
                (Some(Some(session_project)), Some(resolved))
                    if session_project != resolved.resolved_id =>
                {
                    Some(SessionProjectMismatch {
                        session_project,
                        request_project: resolved.resolved_id.clone(),
                    })
                }
                _ => None,
            }
        });
        if let (Some(session_id), Some(mismatch)) =
            (session_id.as_deref(), session_project_mismatch.as_ref())
        {
            let session_start = self.sessions.record_tool_call_started_with_metadata(
                Some(session_id),
                transport,
                call.tool_name(),
                &call.session_log_arguments(),
                Some(mismatch.request_project.clone()),
                recorder_metadata.clone(),
                session_contract,
            );
            let mut result =
                session_project_mismatch_result(session_id, call.tool_name(), mismatch);
            decorate_structured_execution_prestart_denial(
                call.tool_name(),
                &mut result,
                session_context::SESSION_PROJECT_MISMATCH_KIND,
            );
            self.record_dispatch_session_result(
                &mut result,
                session_id,
                session_start,
                call.tool_name(),
                Some(session_context::SESSION_PROJECT_MISMATCH_KIND),
                auth,
                inner_model_facing_recording,
                inner_ack_observation.as_ref(),
                inner_ack_requested,
            )
            .await;
            return result;
        }
        // Inherit execution defaults only after exact project matching has
        // been established. Explicit per-call cwd/shell fields remain authoritative.
        let mut ssh_resource = None;
        if session_project_mismatch.is_none() {
            if let (Some(session_id), Some(resolved)) =
                (session_id.as_deref(), resolved_project.as_ref())
            {
                if let Some(execution_context) = self
                    .sessions
                    .execution_context_for_project(session_id, &resolved.resolved_id)
                {
                    if matches!(
                        &call,
                        ToolCall::RunProcess { .. }
                            | ToolCall::RunDetachedProcess { .. }
                            | ToolCall::RunScript { .. }
                            | ToolCall::RunShell { .. }
                            | ToolCall::RunJob { .. }
                            | ToolCall::OpenSessionShell { .. }
                            | ToolCall::CargoFmt { .. }
                            | ToolCall::CargoCheck { .. }
                            | ToolCall::CargoTest { .. }
                            | ToolCall::GoTest { .. }
                    ) {
                        ssh_resource = execution_context.resource.clone();
                    }
                    call = call.with_session_execution_context(&execution_context);
                }
            }
        }
        if let Some(session_id) = session_id.as_deref() {
            // Lifecycle denial is orthogonal to mode/guards and wins first.
            if let Some(denial) =
                self.sessions
                    .lifecycle_denial(session_id, call.tool_name(), session_contract)
            {
                let session_start = self.sessions.record_tool_call_started_with_metadata(
                    Some(session_id),
                    transport,
                    call.tool_name(),
                    &call.session_log_arguments(),
                    None,
                    recorder_metadata.clone(),
                    session_contract,
                );
                let mut result =
                    session_lifecycle_denied_result(session_id, call.tool_name(), denial);
                decorate_structured_execution_prestart_denial(
                    call.tool_name(),
                    &mut result,
                    "session_lifecycle_denied",
                );
                let error_kind = result
                    .output
                    .get("error_kind")
                    .and_then(Value::as_str)
                    .unwrap_or("session_closed")
                    .to_string();
                self.record_dispatch_session_result(
                    &mut result,
                    session_id,
                    session_start,
                    call.tool_name(),
                    Some(error_kind.as_str()),
                    auth,
                    inner_model_facing_recording,
                    inner_ack_observation.as_ref(),
                    inner_ack_requested,
                )
                .await;
                return result;
            }
            if let Some(denial) = self.sessions.guard_denial(session_id, session_contract) {
                let session_start = self.sessions.record_tool_call_started_with_metadata(
                    Some(session_id),
                    transport,
                    call.tool_name(),
                    &call.session_log_arguments(),
                    None,
                    recorder_metadata.clone(),
                    session_contract,
                );
                let mut result = session_guard_denied_result(session_id, call.tool_name(), denial);
                decorate_structured_execution_prestart_denial(
                    call.tool_name(),
                    &mut result,
                    "session_guard_denied",
                );
                self.record_dispatch_session_result(
                    &mut result,
                    session_id,
                    session_start,
                    call.tool_name(),
                    Some("session_guard_denied"),
                    auth,
                    inner_model_facing_recording,
                    inner_ack_observation.as_ref(),
                    inner_ack_requested,
                )
                .await;
                return result;
            }
        }
        let mut session_start = if session_id.is_some() {
            let resolved_project = resolved_project.map(|resolved| resolved.resolved_id.clone());
            self.sessions.record_tool_call_started_with_metadata(
                session_id.as_deref(),
                transport,
                call.tool_name(),
                &call.session_log_arguments(),
                resolved_project,
                recorder_metadata.clone(),
                session_contract,
            )
        } else {
            None
        };
        if let Err(err) = self
            .authorize_runner_tool(
                &call,
                ssh_resource.as_deref(),
                auth,
                project_resolution.as_ref(),
            )
            .await
        {
            let mut err = err;
            let failure_kind = crate::tool_runtime::process::classify_process_failure(
                err.error.as_deref().unwrap_or_default(),
            );
            decorate_structured_execution_prestart_denial(call.tool_name(), &mut err, failure_kind);
            if let Some(session_id) = session_id.as_deref() {
                self.record_dispatch_session_result(
                    &mut err,
                    session_id,
                    session_start,
                    call.tool_name(),
                    None,
                    auth,
                    inner_model_facing_recording,
                    inner_ack_observation.as_ref(),
                    inner_ack_requested,
                )
                .await;
            }
            return err;
        }
        // Authoritative single evaluation (kernel must not re-evaluate).
        // Order: session/auth guards above → permission gate → mutation below.
        // Path/sensitive hard checks still run inside tools; hard-deny filter
        // suppresses permission attach so soft policy never overrides them.
        let permission = crate::tool_runtime::permissions::evaluate_permission_for_tool(
            &self.config.permission_evaluator,
            call.tool_name(),
            call.project(),
        );
        if let Some(decision) = permission.as_ref() {
            if !decision.allows_execution() {
                let mut result = permissions::permission_execution_denied_result(decision);
                decorate_structured_execution_prestart_denial(
                    call.tool_name(),
                    &mut result,
                    "permission_denied",
                );
                if let Some(start) = session_start.as_mut() {
                    self.sessions
                        .record_permission_decision(start, decision.clone());
                }
                permissions::add_permission_to_result(&mut result, decision);
                if let Some(session_id) = session_id.as_deref() {
                    self.record_dispatch_session_result(
                        &mut result,
                        session_id,
                        session_start,
                        call.tool_name(),
                        None,
                        auth,
                        inner_model_facing_recording,
                        inner_ack_observation.as_ref(),
                        inner_ack_requested,
                    )
                    .await;
                }
                return result;
            }
        }
        let activity_context =
            Self::capture_workspace_activity_context(&call, activity_project.as_deref());
        let validation_assertion_name = recorder_metadata.expectation.assertion_name.as_deref();
        let tool_name = call.tool_name();
        let trusted_recording_session_id = recorder_metadata
            .recording_session_authorized
            .then(|| recorder_metadata.recording_session_id.as_deref())
            .flatten();
        let trusted_recording_session_project = recorder_metadata
            .recording_session_authorized
            .then(|| recorder_metadata.recording_session_project.as_deref())
            .flatten();
        if let Err(result) = self
            .host_goal_admit_effect(
                tool_name,
                activity_project.as_deref(),
                &serde_json::to_value(&call).unwrap_or_default(),
                auth,
                crate::tool_runtime::host_goal::requires_confirmed_plan(&call),
            )
            .await
        {
            return result;
        }
        let mut result = self
            .dispatch_authorized_inner(
                call,
                auth,
                transport,
                ssh_resource.as_deref(),
                validation_assertion_name,
                project_resolution,
                trusted_recording_session_id,
                trusted_recording_session_project,
                protocol_capabilities,
                correlation,
            )
            .await;
        let permission = permission.filter(|_| {
            !permissions::is_hard_denied_output(&result.output, result.error.as_deref())
        });
        if let Some(permission) = permission.as_ref() {
            if let Some(start) = session_start.as_mut() {
                self.sessions
                    .record_permission_decision(start, permission.clone());
            }
            permissions::add_permission_to_result(&mut result, permission);
        }
        if let Some(session_id) = session_id.as_deref() {
            self.record_dispatch_session_result(
                &mut result,
                session_id,
                session_start,
                tool_name,
                None,
                auth,
                inner_model_facing_recording,
                inner_ack_observation.as_ref(),
                inner_ack_requested,
            )
            .await;
        }
        if let Some(context) = activity_context {
            self.observation
                .record_activity(crate::tool_runtime::activity::ActivityRecord {
                    tool: context.tool,
                    project: context.project.as_deref(),
                    surface: transport.as_str(),
                    client: context.client.as_deref(),
                    success: result.success,
                    session_id: session_id.as_deref(),
                    command: context.command.as_deref(),
                    paths: context.paths,
                    error_summary: result.error.as_deref(),
                    // Derived from the verified caller here, not looked up later
                    // from whoever holds this client id at read time.
                    scope: crate::tool_runtime::activity::activity_scope_from_auth(auth),
                });
        }
        if result.success
            && crate::tool_runtime::observations::is_meaningful_activity_tool(tool_name)
        {
            if let Ok((principal_kind, principal_id)) =
                crate::tool_runtime::session_context::runtime_observation_principal(auth)
            {
                self.observation.observations.record_successful_tool_call(
                    crate::tool_runtime::observations::ToolCallObservation {
                        principal_kind,
                        principal_id,
                        project: activity_project.clone(),
                        surface: transport.as_str().to_string(),
                        session_id: session_id.clone(),
                        tool: tool_name.to_string(),
                        observed_at: chrono::Utc::now().timestamp(),
                    },
                );
            }
        }
        self.add_requested_context_projection(
            &mut result,
            &context_request,
            context_projection_project.as_ref(),
            auth,
            material_capabilities,
        )
        .await;
        sparsify_terminal_structured_execution_success(tool_name, &mut result);
        result
    }
}
