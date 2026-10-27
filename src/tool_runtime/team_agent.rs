//! Ordinary subagent queue and trusted host transport control.
use super::{ToolResult, ToolRuntime};
use crate::auth::{AuthContext, AuthKind};
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_store::{TeamAgentObservation, TeamAgentRecord, TeamAgentResolution, TeamAgentState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    project: String,
    root_task_id: String,
    source_task_id: String,
    idempotency_key: String,
    thinking_profile_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observe {
    project: String,
    team_agent_attempt_id: String,
}

fn error(code: &str) -> ToolResult {
    ToolResult::err_with_output(code, json!({"error_kind":code,"state_changed":false}))
}
fn store_error(value: webcodex_store::CommunicationStoreError) -> ToolResult {
    error(value.code())
}

pub(crate) fn team_agent_projection(record: &TeamAgentRecord) -> Value {
    json!({"team_agent_attempt_id":record.team_agent_attempt_id,"project":record.project,
        "root_task_id":record.root_task_id,"source_task_id":record.source_task_id,
        "thinking_profile_id":record.thinking_profile_id,"role":"subagent","state":record.state,
        "host_request_id":record.request_id,"cancel_requested":record.cancel_requested,
        "response_observed":record.response_observed,"revision":record.revision,
        "counts_toward_regular_min":true,"counts_toward_regular_max":true,"business_verified":false})
}

impl ToolRuntime {
    pub(crate) async fn call_team_agent_tool(
        &self,
        tool: &str,
        params: Value,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        let Some(project) = params
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return error("team_agent_project_required");
        };
        let resolved = match self
            .execution()
            .resolve_project_input_for_auth(&project, auth)
            .await
        {
            Ok(value) => value,
            Err(_) => return error("team_agent_project_unavailable"),
        };
        if resolved.resolved_id != project {
            return error("team_agent_exact_project_required");
        }
        let principal = match super::communication::communication_principal(auth) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let Some(db) = self.communication.communication_db.as_ref() else {
            return error("team_agent_store_unavailable");
        };
        match tool {
            "team_agent_start" => {
                let request: Start = match serde_json::from_value(params) {
                    Ok(value) => value,
                    Err(_) => return error("team_agent_invalid_request"),
                };
                // Recover the original intent without depending on whether its
                // once-validated host catalog observation has since expired.
                match db.lookup_team_agent_attempt(
                    &principal,
                    &request.project,
                    &request.root_task_id,
                    &request.source_task_id,
                    &request.idempotency_key,
                    &request.thinking_profile_id,
                ) {
                    Ok(Some(record)) => {
                        return ToolResult::ok(
                            json!({"attempt":team_agent_projection(&record),"created":false,"replayed":true,"queued":record.state==TeamAgentState::Queued,"state_changed":false}),
                        )
                    }
                    Ok(None) => {}
                    Err(value) => return store_error(value),
                }
                if let Err(value) = self
                    .validate_team_fallback_profile(
                        auth,
                        &request.project,
                        &request.thinking_profile_id,
                    )
                    .await
                {
                    return value;
                }
                match db.reserve_team_agent_attempt(
                    &principal,
                    &request.project,
                    &request.root_task_id,
                    &request.source_task_id,
                    &request.idempotency_key,
                    &request.thinking_profile_id,
                ) {
                    Ok(value) => ToolResult::ok(
                        json!({"attempt":team_agent_projection(&value.record),"created":value.created,"replayed":value.replayed,"queued":value.record.state==TeamAgentState::Queued,"state_changed":value.created}),
                    ),
                    Err(value) => store_error(value),
                }
            }
            "team_agent_observe" => {
                let request: Observe = match serde_json::from_value(params) {
                    Ok(value) => value,
                    Err(_) => return error("team_agent_invalid_request"),
                };
                match db.read_team_agent_attempt(&principal, &request.team_agent_attempt_id) {
                    Ok(record) if record.project == request.project => ToolResult::ok(
                        json!({"attempt":team_agent_projection(&record),"state_changed":false}),
                    ),
                    Ok(_) => error("team_agent_unavailable"),
                    Err(value) => store_error(value),
                }
            }
            _ => error("team_agent_invalid_tool"),
        }
    }

    pub(crate) fn try_team_agent_control(
        &self,
        auth: &AuthContext,
        body: &Value,
    ) -> Option<Result<Value, String>> {
        if !body["action"]
            .as_str()
            .is_some_and(|action| action.starts_with("team_agent_"))
        {
            return None;
        }
        Some(self.team_agent_control(auth, body))
    }
    fn team_agent_control(&self, auth: &AuthContext, body: &Value) -> Result<Value, String> {
        if auth.kind != AuthKind::Bootstrap || !auth.is_bootstrap() || auth.agent_run.is_some() {
            return Err("trusted_control_required".into());
        }
        let request: ControlRequest = serde_json::from_value(body.clone())
            .map_err(|_| "invalid_team_agent_control_request")?;
        let (owner, project) = request.scope();
        if owner.is_empty()
            || owner.len() > 256
            || project.is_empty()
            || project.len() > 256
            || owner.chars().any(char::is_control)
            || project.chars().any(char::is_control)
        {
            return Err("invalid_team_agent_control_request".into());
        }
        let db = self
            .communication
            .communication_db
            .as_ref()
            .ok_or("team_agent_store_unavailable")?;
        let user = db
            .get_user_by_id(owner)
            .map_err(|_| "team_agent_owner_unavailable")?
            .filter(|user| !user.is_disabled())
            .ok_or("team_agent_owner_unavailable")?;
        let mut owner_auth = AuthContext::new(AuthKind::ApiToken);
        owner_auth.user_id = Some(user.id);
        let principal = super::communication::communication_principal(Some(&owner_auth))
            .map_err(|_| "team_agent_owner_unavailable")?;
        if let ControlRequest::List { limit, cursor, .. } = &request {
            let page = db
                .list_team_agent_attempts_page(
                    &principal,
                    project,
                    None,
                    true,
                    limit.unwrap_or(100),
                    cursor.as_deref(),
                )
                .map_err(|_| "team_agent_list_unavailable")?;
            return Ok(
                json!({"candidates":page.records.iter().map(team_agent_projection).collect::<Vec<_>>(),
                    "next_cursor":page.next_cursor}),
            );
        }
        let id = request
            .attempt_id()
            .ok_or("invalid_team_agent_control_request")?
            .to_owned();
        let record = db
            .read_team_agent_attempt(&principal, &id)
            .map_err(|_| "team_agent_unavailable")?;
        if record.project != project {
            return Err("team_agent_unavailable".into());
        }
        match request {
            ControlRequest::Acquire { .. } => {
                let claim = db
                    .claim_team_agent_attempt(&principal, &id)
                    .map_err(|_| "team_agent_claim_denied")?;
                if !claim.record.state.active() {
                    return Ok(
                        json!({"state":claim.record.state,"team_agent_attempt_id":id,"dispatch_permitted":false}),
                    );
                }
                Ok(
                    json!({"state":"claimed","team_agent_attempt_id":id,"project":claim.record.project,
                    "root_task_id":claim.record.root_task_id,"source_task_id":claim.record.source_task_id,
                    "dispatch_attempt_id":claim.record.dispatch_attempt_id,"task":claim.task,
                    "thinking_profile_id":claim.record.thinking_profile_id,"request_id":claim.record.request_id,
                    "dispatch_permitted":claim.dispatch_permitted,"cancel_requested":claim.record.cancel_requested}),
                )
            }
            ControlRequest::Read { .. } => Ok(team_agent_projection(&record)),
            ControlRequest::Record {
                dispatch_attempt_id,
                request_id,
                state,
                reason,
                response_observed,
                ..
            } => {
                if reason.len() > 128
                    || reason.chars().any(char::is_control)
                    || (response_observed && state != "dispatched")
                {
                    return Err("invalid_team_agent_observation".into());
                }
                let observation = match state.as_str() {
                    "dispatched" => TeamAgentObservation::Dispatched {
                        request_id: request_id.ok_or("invalid_chat_request_id")?,
                        response_observed,
                    },
                    "unknown" => TeamAgentObservation::Unknown { request_id },
                    "cancel_requested" => TeamAgentObservation::CancelRequested { request_id },
                    "busy" if request_id.is_none() && reason == "browser_worker_busy" => {
                        TeamAgentObservation::Busy {
                            evidence: "browser_worker_busy:launched=false".into(),
                        }
                    }
                    "profile_unavailable"
                        if request_id.is_none()
                            && matches!(
                                reason.as_str(),
                                "discover_thinking_profiles_first"
                                    | "thinking_profiles_expired_refresh_required"
                                    | "thinking_profile_not_discovered"
                            ) =>
                    {
                        TeamAgentObservation::ProfileUnavailable { evidence: reason }
                    }
                    _ => return Err("invalid_team_agent_observation".into()),
                };
                let updated = db
                    .record_team_agent_observation(
                        &principal,
                        &id,
                        &dispatch_attempt_id,
                        observation,
                    )
                    .map_err(|_| "team_agent_record_rejected")?;
                Ok(json!({"recorded":true,"attempt":team_agent_projection(&updated)}))
            }
            ControlRequest::Resolve {
                dispatch_attempt_id,
                resolution,
                evidence,
                ..
            } => {
                let updated = db
                    .resolve_team_agent_attempt(
                        &principal,
                        &id,
                        &dispatch_attempt_id,
                        resolution,
                        &evidence,
                    )
                    .map_err(|_| "team_agent_resolution_rejected")?;
                Ok(team_agent_projection(&updated))
            }
            ControlRequest::List { .. } => unreachable!("handled above"),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
enum ControlRequest {
    #[serde(rename = "team_agent_list")]
    List {
        owner_user: String,
        project: String,
        limit: Option<usize>,
        cursor: Option<String>,
    },
    #[serde(rename = "team_agent_acquire")]
    Acquire {
        owner_user: String,
        project: String,
        team_agent_attempt_id: String,
    },
    #[serde(rename = "team_agent_read")]
    Read {
        owner_user: String,
        project: String,
        team_agent_attempt_id: String,
    },
    #[serde(rename = "team_agent_record")]
    Record {
        owner_user: String,
        project: String,
        team_agent_attempt_id: String,
        dispatch_attempt_id: String,
        request_id: Option<String>,
        state: String,
        reason: String,
        #[serde(default)]
        response_observed: bool,
    },
    #[serde(rename = "team_agent_resolve")]
    Resolve {
        owner_user: String,
        project: String,
        team_agent_attempt_id: String,
        dispatch_attempt_id: String,
        resolution: TeamAgentResolution,
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
    fn attempt_id(&self) -> Option<&str> {
        match self {
            Self::List { .. } => None,
            Self::Acquire {
                team_agent_attempt_id,
                ..
            }
            | Self::Read {
                team_agent_attempt_id,
                ..
            }
            | Self::Record {
                team_agent_attempt_id,
                ..
            }
            | Self::Resolve {
                team_agent_attempt_id,
                ..
            } => Some(team_agent_attempt_id),
        }
    }
}

#[cfg(test)]
#[path = "tests/team_agent.rs"]
mod tests;
