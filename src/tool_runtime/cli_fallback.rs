//! Recovery admission from authorized team attempts and trusted host control.
//!
//! The model-facing team dispatcher owns ordinary budgets. Only a typed,
//! bound Job can create a separate recovery reservation; discovery and UI
//! assertions cannot. The dedicated Bootstrap host control transports the
//! admitted task, never a user/Server credential, to the existing chat worker.

use super::{ToolResult, ToolRuntime};
use crate::auth::{AuthContext, AuthKind};
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_core::cli_fallback::{
    cli_fallback_eligibility, CliCancellationOrigin, CliFailureReason,
};
use webcodex_core::runner_protocol::{ShellCommandExecutionState, ShellJobInfo};
use webcodex_store::{
    CliFallbackObservation, CliFallbackRecord, CliFallbackResolution, CliFallbackState,
    TeamCliDispatchState,
};

pub(crate) fn failure_reason(
    job: &ShellJobInfo,
    verified_code: Option<&str>,
) -> Option<CliFailureReason> {
    use CliFailureReason as Reason;
    match job.command_execution_state? {
        ShellCommandExecutionState::TimedOut => Some(Reason::DeadlineExceeded),
        ShellCommandExecutionState::NotStarted => {
            // These are Runner/tool-layer error facts, not model stdout. A
            // permission, isolation or malformed-input denial is not recovery.
            (verified_code == Some("spawn_failed")).then_some(Reason::StartupFailed)
        }
        ShellCommandExecutionState::Completed => match verified_code? {
            "system_timeout_cancel" if job.status == "stopped" => Some(Reason::DeadlineExceeded),
            "quota_exhausted" | "insufficient_quota" => Some(Reason::QuotaExhausted),
            "rate_limited" | "rate_limit_exceeded" => Some(Reason::RateLimited),
            "network_unavailable" | "connection_failed" => Some(Reason::NetworkUnavailable),
            "provider_unavailable" | "authentication_failed" => Some(Reason::ProviderUnavailable),
            _ => None,
        },
        ShellCommandExecutionState::OutcomeUnknown => None,
    }
}

pub(crate) fn fallback_projection(record: &CliFallbackRecord) -> Value {
    json!({
        "fallback_id": record.fallback_id,
        "root_task_id": record.root_task_id,
        "source_task_id": record.source_task_id,
        "source_attempt_id": record.source_attempt_id,
        "source_job_id": record.source_job_id,
        "project": record.project,
        "role": "cli_fallback",
        "state": record.state,
        "reason_code": record.failure_reason,
        "thinking_profile_id": record.thinking_profile_id,
        "host_request_id": record.request_id,
        "fallback_agent_id": Value::Null,
        "cancel_requested": record.cancel_requested,
        "response_observed": record.response_observed,
        "counts_toward_regular_min": false,
        "counts_toward_regular_max": false,
        "slot_held": record.state.holds_slot(),
        "revision": record.revision,
        "business_verified": false,
    })
}

impl ToolRuntime {
    /// Validate the manager's selection against the host's existing catalog.
    /// This calls only the cache-read tool: it cannot discover, select a model,
    /// open a chat or refresh an expired observation on the manager's behalf.
    pub(crate) async fn validate_team_fallback_profile(
        &self,
        auth: Option<&AuthContext>,
        project: &str,
        profile_id: &str,
    ) -> Result<(), ToolResult> {
        if profile_id.is_empty()
            || profile_id.len() > 128
            || !profile_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        {
            return Err(ToolResult::err("cli_fallback_profile_required"));
        }
        let project = self
            .execution()
            .resolve_project_for_auth(project, auth)
            .await
            .map_err(|_| ToolResult::err("cli_fallback_project_unavailable"))?;
        let describe = super::PluginToolCall {
            action: "describe".into(),
            runner: Some(project.client_id),
            plugin: Some("chat-agents".into()),
            tool: Some("read_chat_profiles".into()),
            binding: None,
            arguments: None,
        };
        let result = crate::plugin_gateway::invoke(
            self,
            describe,
            None,
            auth,
            super::sessions::SessionTransport::Api,
        )
        .await
        .map_err(|_| ToolResult::err("cli_fallback_profile_access_denied"))?
        .to_tool_result();
        if !result.success {
            return Err(result);
        }
        let binding = result.output["binding"]
            .as_str()
            .ok_or_else(|| ToolResult::err("cli_fallback_profile_unavailable"))?
            .to_owned();
        let call = super::PluginToolCall {
            action: "call".into(),
            runner: None,
            plugin: None,
            tool: None,
            binding: Some(binding),
            arguments: Some(json!({})),
        };
        let result = crate::plugin_gateway::invoke(
            self,
            call,
            None,
            auth,
            super::sessions::SessionTransport::Api,
        )
        .await
        .map_err(|_| ToolResult::err("cli_fallback_profile_access_denied"))?
        .to_tool_result();
        if !result.success {
            return Err(result);
        }
        let catalog = &result.output["structuredContent"];
        if !catalog_has_profile(catalog, profile_id) {
            return Err(ToolResult::err_with_output(
                "cli_fallback_profile_not_ready",
                json!({
                    "state_changed":false,"dispatched":false,
                    "recovery":"The manager must discover current chat profiles and explicitly choose an available thinking_profile_id. No default is selected."
                }),
            ));
        }
        Ok(())
    }

    /// Called after team attempt reconciliation, never with caller-supplied
    /// failure facts. Exact replay returns the original logical reservation.
    pub(crate) async fn enqueue_cli_fallback_from_attempt(
        &self,
        auth: Option<&AuthContext>,
        source_attempt_id: &str,
    ) -> ToolResult {
        let principal = match super::communication::communication_principal(auth) {
            Ok(principal) => principal,
            Err(error) => return error,
        };
        let Some(db) = self.communication.communication_db.as_ref() else {
            return ToolResult::err("cli_fallback_store_unavailable");
        };
        let source = match db.read_team_cli_attempt(&principal, source_attempt_id) {
            Ok(source) => source,
            Err(_) => return ToolResult::err("team_cli_attempt_unavailable"),
        };
        if source.dispatch_state != TeamCliDispatchState::Terminal {
            return ToolResult::ok(
                json!({"state":"not_eligible","reason":"source_execution_unresolved"}),
            );
        }
        let Some(job_id) = source.job_id.as_ref() else {
            return ToolResult::ok(json!({"state":"not_eligible","reason":"missing_job_evidence"}));
        };
        let access = crate::runner_http::runner_access_from_auth(auth);
        let job = match self
            .runner_registry
            .get_job_for_auth(access.as_ref(), job_id)
            .await
        {
            Ok(job) => job,
            Err(_) => return ToolResult::err("cli_fallback_job_unavailable"),
        };
        let Some(reason) = failure_reason(&job, source.error_code.as_deref()) else {
            return ToolResult::ok(
                json!({"state":"not_eligible","reason":"failure_not_classified"}),
            );
        };
        // User cancellation of the root is checked again inside the store's
        // reservation transaction. Native timeout cleanup is not user cancel.
        let cancellation = if source.error_code.as_deref() == Some("system_timeout_cancel")
            && job.status == "stopped"
        {
            CliCancellationOrigin::Coordinator
        } else if source.error_code.as_deref() == Some("user_cancelled") {
            CliCancellationOrigin::User
        } else {
            CliCancellationOrigin::None
        };
        let eligibility = cli_fallback_eligibility(&job, reason, cancellation);
        if !eligibility.is_eligible() {
            return ToolResult::ok(json!({"state":"not_eligible","eligibility":eligibility}));
        }
        match db.reserve_cli_fallback(&principal, source_attempt_id, &job, reason, cancellation) {
            Ok(record) => ToolResult::ok(fallback_projection(&record)),
            Err(_) => ToolResult::err("cli_fallback_admission_denied"),
        }
    }

    /// Management-only dispatch control, called after the existing explicit
    /// Bootstrap bearer gate. No normal model tool can claim this capability.
    pub(crate) fn try_cli_fallback_control(
        &self,
        auth: &AuthContext,
        body: &Value,
    ) -> Option<Result<Value, String>> {
        if !body["action"]
            .as_str()
            .is_some_and(|action| action.starts_with("cli_fallback_"))
        {
            return None;
        }
        Some(self.cli_fallback_control(auth, body))
    }

    fn cli_fallback_control(&self, auth: &AuthContext, body: &Value) -> Result<Value, String> {
        if auth.kind != AuthKind::Bootstrap || !auth.is_bootstrap() || auth.agent_run.is_some() {
            return Err("trusted_control_required".into());
        }
        let request: ControlRequest = serde_json::from_value(body.clone())
            .map_err(|_| "invalid_cli_fallback_control_request")?;
        let (owner, project) = request.scope();
        if owner.is_empty() || owner.len() > 256 || project.is_empty() || project.len() > 512 {
            return Err("invalid_cli_fallback_control_request".into());
        }
        let db = self
            .communication
            .communication_db
            .as_ref()
            .ok_or("cli_fallback_store_unavailable")?;
        let user = db
            .get_user_by_id(owner)
            .map_err(|_| "cli_fallback_owner_unavailable")?
            .filter(|user| !user.is_disabled())
            .ok_or("cli_fallback_owner_unavailable")?;
        let mut owner_auth = AuthContext::new(AuthKind::ApiToken);
        owner_auth.user_id = Some(user.id);
        let principal = super::communication::communication_principal(Some(&owner_auth))
            .map_err(|_| "cli_fallback_owner_unavailable")?;
        if let ControlRequest::List { limit, cursor, .. } = &request {
            let page = db
                .list_project_cli_fallbacks_page(
                    &principal,
                    project,
                    limit.unwrap_or(100),
                    cursor.as_deref(),
                )
                .map_err(|_| "cli_fallback_list_unavailable")?;
            return Ok(
                json!({"candidates":page.records.iter().map(fallback_projection).collect::<Vec<_>>(),
                    "next_cursor":page.next_cursor}),
            );
        }
        let fallback_id_owned = request
            .fallback_id()
            .ok_or("invalid_cli_fallback_control_request")?
            .to_owned();
        let fallback_id = fallback_id_owned.as_str();
        let record = db
            .read_cli_fallback(&principal, fallback_id)
            .map_err(|_| "cli_fallback_unavailable")?;
        if record.project != project {
            return Err("cli_fallback_unavailable".into());
        }
        match request {
            ControlRequest::Acquire { .. } => {
                let claim = db
                    .claim_cli_fallback(&principal, fallback_id)
                    .map_err(|_| "cli_fallback_claim_denied")?;
                if !claim.record.state.holds_slot() {
                    return Ok(json!({"fallback_id":fallback_id,
                        "state":if claim.record.state == CliFallbackState::Queued {"busy"} else {"blocked"},
                        "dispatch_permitted":false}));
                }
                Ok(json!({"state":"claimed","fallback_id":fallback_id,
                    "root_task_id":claim.root_task_id,"source_attempt_id":claim.source_attempt_id,
                    "dispatch_attempt_id":claim.dispatch_attempt_id,"task":claim.task,
                    "thinking_profile_id":claim.thinking_profile_id,
                    "request_id":claim.record.request_id,"dispatch_permitted":claim.dispatch_permitted,
                    "cancel_requested":claim.record.cancel_requested}))
            }
            ControlRequest::Read { .. } => Ok(fallback_projection(&record)),
            ControlRequest::Record {
                dispatch_attempt_id,
                request_id,
                state,
                reason,
                response_observed,
                ..
            } => {
                if !matches!(
                    state.as_str(),
                    "dispatched" | "unknown" | "cancel_requested" | "busy" | "profile_unavailable"
                ) || (state == "dispatched" && request_id.is_none())
                    || (response_observed && state != "dispatched")
                {
                    return Err("invalid_cli_fallback_observation".into());
                }
                if let Some(request_id) = request_id.as_ref() {
                    let parsed =
                        uuid::Uuid::parse_str(request_id).map_err(|_| "invalid_chat_request_id")?;
                    if parsed.to_string() != *request_id {
                        return Err("invalid_chat_request_id".into());
                    }
                }
                if state == "busy" && (request_id.is_some() || reason != "browser_worker_busy") {
                    return Err("cli_fallback_not_proven_unstarted".into());
                }
                if state == "profile_unavailable"
                    && (request_id.is_some()
                        || !matches!(
                            reason.as_str(),
                            "discover_thinking_profiles_first"
                                | "thinking_profiles_expired_refresh_required"
                                | "thinking_profile_not_discovered"
                        ))
                {
                    return Err("cli_fallback_not_proven_unstarted".into());
                }
                // Persist an identity even if its launch result is unknown.
                // Binding it does not release the slot or prove completion.
                if let Some(id) = request_id.as_ref() {
                    db.record_cli_fallback_observation(
                        &principal,
                        fallback_id,
                        &dispatch_attempt_id,
                        CliFallbackObservation::Dispatched {
                            request_id: id.clone(),
                        },
                    )
                    .map_err(|_| "cli_fallback_record_rejected")?;
                }
                let observation = match state.as_str() {
                    "dispatched" => CliFallbackObservation::Dispatched {
                        request_id: request_id.ok_or("invalid_chat_request_id")?,
                    },
                    "unknown" => CliFallbackObservation::Unknown,
                    "cancel_requested" => CliFallbackObservation::CancelRequested,
                    "busy" => CliFallbackObservation::Busy {
                        evidence: "browser_worker_busy:launched=false".into(),
                    },
                    "profile_unavailable" => {
                        CliFallbackObservation::ProfileUnavailable { evidence: reason }
                    }
                    _ => return Err("invalid_cli_fallback_observation".into()),
                };
                let mut updated = db
                    .record_cli_fallback_observation(
                        &principal,
                        fallback_id,
                        &dispatch_attempt_id,
                        observation,
                    )
                    .map_err(|_| "cli_fallback_record_rejected")?;
                if response_observed {
                    updated = db
                        .record_cli_fallback_observation(
                            &principal,
                            fallback_id,
                            &dispatch_attempt_id,
                            CliFallbackObservation::ResponseObserved,
                        )
                        .map_err(|_| "cli_fallback_record_rejected")?;
                }
                Ok(json!({"recorded":true,"fallback":fallback_projection(&updated)}))
            }
            ControlRequest::Resolve {
                dispatch_attempt_id,
                resolution,
                evidence,
                ..
            } => {
                let updated = db
                    .resolve_cli_fallback(
                        &principal,
                        fallback_id,
                        &dispatch_attempt_id,
                        resolution,
                        &evidence,
                    )
                    .map_err(|_| "cli_fallback_resolution_rejected")?;
                Ok(fallback_projection(&updated))
            }
            ControlRequest::List { .. } => unreachable!("handled above"),
        }
    }
}

fn catalog_has_profile(catalog: &Value, profile_id: &str) -> bool {
    catalog["state"] == "ready"
        && catalog["refresh_required"] == false
        && catalog["profiles"].as_array().is_some_and(|profiles| {
            !profiles.is_empty()
                && profiles.len() <= 33
                && profiles
                    .iter()
                    .filter(|profile| profile["profile_id"].as_str() == Some(profile_id))
                    .count()
                    == 1
        })
}

#[derive(Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
enum ControlRequest {
    #[serde(rename = "cli_fallback_list")]
    List {
        owner_user: String,
        project: String,
        limit: Option<usize>,
        cursor: Option<String>,
    },
    #[serde(rename = "cli_fallback_acquire")]
    Acquire {
        owner_user: String,
        project: String,
        fallback_id: String,
    },
    #[serde(rename = "cli_fallback_read")]
    Read {
        owner_user: String,
        project: String,
        fallback_id: String,
    },
    #[serde(rename = "cli_fallback_record")]
    Record {
        owner_user: String,
        project: String,
        fallback_id: String,
        dispatch_attempt_id: String,
        request_id: Option<String>,
        state: String,
        reason: String,
        #[serde(default)]
        response_observed: bool,
    },
    #[serde(rename = "cli_fallback_resolve")]
    Resolve {
        owner_user: String,
        project: String,
        fallback_id: String,
        dispatch_attempt_id: String,
        resolution: CliFallbackResolution,
        evidence: String,
    },
}

impl ControlRequest {
    fn scope(&self) -> (&str, &str) {
        match self {
            Self::List {
                owner_user,
                project,
                ..
            }
            | Self::Acquire {
                owner_user,
                project,
                ..
            }
            | Self::Read {
                owner_user,
                project,
                ..
            }
            | Self::Record {
                owner_user,
                project,
                ..
            }
            | Self::Resolve {
                owner_user,
                project,
                ..
            } => (owner_user, project),
        }
    }
    fn fallback_id(&self) -> Option<&str> {
        match self {
            Self::List { .. } => None,
            Self::Acquire { fallback_id, .. }
            | Self::Read { fallback_id, .. }
            | Self::Record { fallback_id, .. }
            | Self::Resolve { fallback_id, .. } => Some(fallback_id),
        }
    }
}

#[cfg(test)]
#[path = "tests/cli_fallback.rs"]
mod tests;
