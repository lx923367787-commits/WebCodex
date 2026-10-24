//! Product-control enrollment of a new logical Agent. This is deliberately
//! separate from same-Agent handoff: no prior private state is inherited.
use crate::auth::{AuthContext, AuthKind};
use crate::tool_runtime::ToolResult;
use crate::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_store::{
    AgentEnrollmentInput, AgentEnrollmentPolicy, AgentEnrollmentPurpose, AgentHostDispatchStatus,
    AgentHostQueueKind, AgentHostWindowBinding, NewAgentEnrollment, VerifiedAgentRun,
};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
enum HostDispatchAction {
    #[serde(rename = "agent_host_member_reserve")]
    MemberReserve,
    #[serde(rename = "agent_host_dispatch_prepare")]
    Prepare,
    #[serde(rename = "agent_host_dispatch_read")]
    Read,
    #[serde(rename = "agent_host_dispatch_claim")]
    Claim,
}

/// Bootstrap-only selectors from an exact attach/status/handoff receipt.
/// They never stand in for run authentication or select a latest generation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostDispatchRequest {
    action: HostDispatchAction,
    owner_user: String,
    project: String,
    host_id: String,
    manager_agent_id: String,
    manager_run_id: String,
    expected_generation: i64,
    queue_kind: Option<AgentHostQueueKind>,
    queue_id: Option<String>,
    root_task_id: Option<String>,
    child_task_id: Option<String>,
    enrollment_id: Option<String>,
    idempotency_key: Option<String>,
    thinking_profile_id: Option<String>,
}

fn host_control_identifier(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn host_required(value: &Option<String>) -> Result<&str, &'static str> {
    value
        .as_deref()
        .filter(|value| host_control_identifier(value, 512))
        .ok_or("invalid_agent_host_dispatch_request")
}

fn host_dispatch_manager(
    db: &Database,
    request: &HostDispatchRequest,
    kind: AgentHostQueueKind,
) -> Result<VerifiedAgentRun, String> {
    if !host_control_identifier(&request.owner_user, 256)
        || !host_control_identifier(&request.project, 512)
        || !host_control_identifier(&request.host_id, 512)
        || !host_control_identifier(&request.manager_agent_id, 128)
        || !host_control_identifier(&request.manager_run_id, 128)
        || request.expected_generation < 1
    {
        return Err("invalid_agent_host_dispatch_request".into());
    }
    db.get_user_by_id(&request.owner_user)
        .map_err(|_| "agent_host_dispatch_owner_unavailable")?
        .filter(|user| !user.is_disabled())
        .ok_or("agent_host_dispatch_owner_unavailable")?;
    let principal = db
        .native_agent_host_principal(&request.host_id)
        .map_err(|error| error.code())?;
    let purpose = match kind {
        AgentHostQueueKind::Member => AgentEnrollmentPurpose::Member,
        AgentHostQueueKind::Fallback => AgentEnrollmentPurpose::Fallback,
    };
    let manager = db
        .agent_enrollment_source_run_for_purpose(
            &principal,
            &request.owner_user,
            &request.project,
            &request.manager_agent_id,
            &request.manager_run_id,
            purpose,
        )
        .map_err(|error| error.code())?;
    if manager.generation() != request.expected_generation {
        return Err("agent_host_dispatch_generation_mismatch".into());
    }
    Ok(manager)
}

fn host_dispatch_public_reply(
    db: &Database,
    manager: &VerifiedAgentRun,
    status: &AgentHostDispatchStatus,
    instruction: &str,
    source_task_id: &str,
    cancel_requested: bool,
) -> Result<Value, String> {
    if status.binding.project != manager.project()
        || status.binding.manager_agent_id != manager.agent_id()
    {
        return Err("agent_host_dispatch_binding_mismatch".into());
    }
    let enrollment = db
        .agent_enrollment_status(
            manager.owner_user(),
            manager.project(),
            &status.binding.enrollment_id,
        )
        .map_err(|error| error.code())?;
    if enrollment.enrollment_id != status.binding.enrollment_id
        || enrollment.agent_id != status.binding.agent_id
        || enrollment.task_id != status.binding.task_id
    {
        return Err("agent_host_dispatch_binding_mismatch".into());
    }
    let mut value =
        serde_json::to_value(status).map_err(|_| "agent_host_dispatch_status_unavailable")?;
    // Only this Bootstrap response receives the exact authorized launch brief.
    // The generic public Store status remains metadata-only.
    value["instruction"] = json!(instruction);
    value["source_task_id"] = json!(source_task_id);
    value["cancel_requested"] = json!(cancel_requested);
    value["enrollment_status"] =
        serde_json::to_value(enrollment).map_err(|_| "agent_host_dispatch_status_unavailable")?;
    value["business_verified"] = json!(false);
    Ok(value)
}

/// The existing no-store Bootstrap control endpoint owns these operations.
/// Only Claim may return a transient enrollment grant to its trusted launcher.
fn host_dispatch_control(db: &Database, auth: &AuthContext, body: &Value) -> Result<Value, String> {
    if auth.kind != AuthKind::Bootstrap || !auth.is_bootstrap() || auth.agent_run.is_some() {
        return Err("trusted_control_required".into());
    }
    let request: HostDispatchRequest =
        serde_json::from_value(body.clone()).map_err(|_| "invalid_agent_host_dispatch_request")?;
    if request.action == HostDispatchAction::MemberReserve {
        if request.queue_kind.is_some() || request.queue_id.is_some() {
            return Err("invalid_agent_host_dispatch_request".into());
        }
        let manager = host_dispatch_manager(db, &request, AgentHostQueueKind::Member)?;
        let binding = db
            .reserve_agent_host_member(
                &manager,
                host_required(&request.root_task_id)?,
                host_required(&request.child_task_id)?,
                host_required(&request.enrollment_id)?,
                host_required(&request.idempotency_key)?,
                host_required(&request.thinking_profile_id)?,
            )
            .map_err(|error| error.code())?;
        let material = db
            .read_agent_host_launch_material(
                &manager,
                AgentHostQueueKind::Member,
                &binding.queue_id,
            )
            .map_err(|error| error.code())?;
        return host_dispatch_public_reply(
            db,
            &manager,
            &material.status,
            &material.instruction,
            &material.source_task_id,
            material.cancel_requested,
        );
    }
    if request.root_task_id.is_some()
        || request.child_task_id.is_some()
        || request.enrollment_id.is_some()
        || request.idempotency_key.is_some()
        || request.thinking_profile_id.is_some()
    {
        return Err("invalid_agent_host_dispatch_request".into());
    }
    let kind = request
        .queue_kind
        .ok_or("invalid_agent_host_dispatch_request")?;
    let id = request
        .queue_id
        .as_deref()
        .filter(|value| host_control_identifier(value, 128))
        .ok_or("invalid_agent_host_dispatch_request")?;
    let manager = host_dispatch_manager(db, &request, kind)?;
    if request.action == HostDispatchAction::Prepare && kind == AgentHostQueueKind::Fallback {
        db.bind_agent_host_fallback(&manager, id)
            .map_err(|error| error.code())?;
    }
    if request.action != HostDispatchAction::Claim {
        let material = db
            .read_agent_host_launch_material(&manager, kind, id)
            .map_err(|error| error.code())?;
        return host_dispatch_public_reply(
            db,
            &manager,
            &material.status,
            &material.instruction,
            &material.source_task_id,
            material.cancel_requested,
        );
    }
    let claim = db
        .claim_agent_host_dispatch(&manager, kind, id)
        .map_err(|error| error.code())?;
    let material = claim.launch_material();
    let status = host_dispatch_public_reply(
        db,
        &manager,
        &material.status,
        &material.instruction,
        &material.source_task_id,
        material.cancel_requested,
    )?;
    match (
        claim.dispatch_permitted,
        claim.grant,
        claim.grant_expires_at_unix_ms,
    ) {
        (true, Some(grant), Some(expires)) if expires > chrono::Utc::now().timestamp_millis() => {
            // Do not serialize the Claim object, log this response, or put the
            // grant into a public queue status/config. The Host API consumes it
            // as a transient EnrollmentDelivery immediately before input.
            Ok(json!({"status":status,"dispatch_permitted":true,
                "claim_grant":grant.into_secret(),"expires_at_unix_ms":expires}))
        }
        (false, None, None) => Ok(json!({"status":status,"dispatch_permitted":false,
            "claim_grant":Value::Null,"expires_at_unix_ms":Value::Null})),
        _ => Err("agent_host_dispatch_delivery_inconsistent".into()),
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    AgentHostManagerBindingRead {
        owner_user: String,
        project: String,
        root_task_id: String,
        host_id: String,
    },
    AgentEnrollmentReserve {
        owner_user: String,
        project: String,
        purpose: AgentEnrollmentPurpose,
        host_id: Option<String>,
        oauth_client_id: Option<String>,
        oauth_subject_kind: Option<String>,
        oauth_subject_id: Option<String>,
        host_metadata_exclusive: Option<bool>,
        source_window_key: Option<String>,
        source_agent_id: Option<String>,
        source_run_id: Option<String>,
        task_id: String,
        task_directory: String,
        brief: String,
        inputs: Vec<AgentEnrollmentInput>,
        room_ids: Vec<String>,
        policy: AgentEnrollmentPolicy,
        expires_at_unix_ms: i64,
        idempotency_key: String,
    },
    AgentEnrollmentRevoke {
        owner_user: String,
        project: String,
        enrollment_id: String,
    },
    AgentEnrollmentStatus {
        owner_user: String,
        project: String,
        enrollment_id: String,
    },
}

pub(crate) fn try_control(
    db: &Database,
    auth: &AuthContext,
    body: &Value,
) -> Option<Result<Value, String>> {
    if matches!(
        body.get("action").and_then(Value::as_str),
        Some(
            "agent_host_member_reserve"
                | "agent_host_dispatch_prepare"
                | "agent_host_dispatch_read"
                | "agent_host_dispatch_claim"
        )
    ) {
        return Some(host_dispatch_control(db, auth, body));
    }
    if !matches!(
        body.get("action").and_then(Value::as_str),
        Some(
            "agent_host_manager_binding_read"
                | "agent_enrollment_reserve"
                | "agent_enrollment_revoke"
                | "agent_enrollment_status"
        )
    ) {
        return None;
    }
    Some((|| {
        if auth.kind != AuthKind::Bootstrap || !auth.is_bootstrap() || auth.agent_run.is_some() {
            return Err("trusted_control_required".into());
        }
        let request: Control =
            serde_json::from_value(body.clone()).map_err(|_| "invalid_agent_enrollment_request")?;
        match request {
            Control::AgentHostManagerBindingRead {
                owner_user,
                project,
                root_task_id,
                host_id,
            } => {
                if !host_control_identifier(&owner_user, 256)
                    || [&project, &root_task_id, &host_id]
                        .iter()
                        .any(|value| !host_control_identifier(value, 512))
                {
                    return Err("invalid_agent_host_manager_binding_request".into());
                }
                db.get_user_by_id(&owner_user)
                    .map_err(|_| "agent_host_dispatch_owner_unavailable")?
                    .filter(|user| !user.is_disabled())
                    .ok_or("agent_host_dispatch_owner_unavailable")?;
                let binding = db
                    .read_agent_host_manager_binding(&owner_user, &project, &root_task_id, &host_id)
                    .map_err(|error| error.code())?;
                // Discovery is not a dispatch permit. None must not revive a cached binding.
                Ok(json!({"binding":binding}))
            }
            Control::AgentEnrollmentStatus {
                owner_user,
                project,
                enrollment_id,
            } => {
                let status = db
                    .agent_enrollment_status(&owner_user, &project, &enrollment_id)
                    .map_err(|e| e.code())?;
                serde_json::to_value(status)
                    .map_err(|_| "agent_enrollment_status_unavailable".into())
            }
            Control::AgentEnrollmentReserve {
                owner_user,
                project,
                purpose,
                host_id,
                oauth_client_id,
                oauth_subject_kind,
                oauth_subject_id,
                host_metadata_exclusive,
                source_window_key,
                source_agent_id,
                source_run_id,
                task_id,
                task_directory,
                brief,
                inputs,
                room_ids,
                policy,
                expires_at_unix_ms,
                idempotency_key,
            } => {
                let user = db
                    .get_user_by_id(&owner_user)
                    .map_err(|_| "agent_enrollment_owner_unavailable")?
                    .filter(|user| !user.is_disabled())
                    .ok_or("agent_enrollment_owner_unavailable")?;
                let principal_key = if let Some(host_id) = host_id {
                    if oauth_client_id.is_some()
                        || oauth_subject_kind.is_some()
                        || oauth_subject_id.is_some()
                        || host_metadata_exclusive.is_some()
                    {
                        return Err("agent_enrollment_host_ambiguous".into());
                    }
                    db.native_agent_host_principal(&host_id)
                        .map_err(|error| error.code())?
                } else {
                    if host_metadata_exclusive != Some(true) {
                        return Err("agent_host_metadata_trust_required".into());
                    }
                    let oauth_client_id = oauth_client_id.ok_or("invalid_host_principal")?;
                    let oauth_subject_kind = oauth_subject_kind.ok_or("invalid_host_principal")?;
                    let oauth_subject_id = oauth_subject_id.ok_or("invalid_host_principal")?;
                    if oauth_subject_kind == "managed_user" && oauth_subject_id != user.id {
                        return Err("agent_enrollment_owner_mismatch".into());
                    }
                    if db
                        .get_oauth_client_by_client_id(&oauth_client_id)
                        .map_err(|_| "agent_host_control_unavailable")?
                        .is_none()
                    {
                        return Err("agent_host_client_unavailable".into());
                    }
                    crate::agent_host_auth::principal(
                        &oauth_client_id,
                        &oauth_subject_kind,
                        &oauth_subject_id,
                    )
                    .ok_or("invalid_host_principal")?
                };
                // The current product controller can delegate within its exact
                // trusted host principal. Cross-host delegation is not inferred
                // from an account, parent label, run ID, or a model assertion.
                let source = if source_agent_id.is_some() || source_run_id.is_some() {
                    if source_window_key.is_some() {
                        return Err("agent_enrollment_source_ambiguous".into());
                    }
                    let agent_id = source_agent_id.ok_or("agent_enrollment_source_required")?;
                    let run_id = source_run_id.ok_or("agent_enrollment_source_required")?;
                    Some(
                        db.agent_enrollment_source_run_for_purpose(
                            &principal_key,
                            &owner_user,
                            &project,
                            &agent_id,
                            &run_id,
                            purpose,
                        )
                        .map_err(|error| error.code())?,
                    )
                } else if let Some(window) = source_window_key {
                    if !crate::agent_host_auth::valid_window_key(&window) {
                        return Err("invalid_host_window_key".into());
                    }
                    match db
                        .resolve_agent_host_window(
                            &principal_key,
                            &window,
                            chrono::Utc::now().timestamp_millis(),
                        )
                        .map_err(|e| e.code())?
                    {
                        Some(AgentHostWindowBinding::Active(binding)) => Some(binding),
                        _ => return Err("agent_enrollment_source_required".into()),
                    }
                } else {
                    None
                };
                let issued = db
                    .issue_agent_enrollment(
                        NewAgentEnrollment {
                            purpose,
                            owner_user,
                            project,
                            principal_key,
                            task_id,
                            task_directory,
                            brief,
                            inputs,
                            room_ids,
                            policy,
                            expires_at_unix_ms,
                        },
                        source.as_ref(),
                        &idempotency_key,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .map_err(|e| e.code())?;
                // Only this explicit no-store control response carries the
                // single-use claim. Never return a run token to the model.
                Ok(json!({
                    "enrollment_id":issued.enrollment_id,
                    "agent_id":issued.agent_id,"run_id":issued.run_id,
                    "expires_at_unix_ms":issued.expires_at_unix_ms,
                    "replayed":issued.replayed,
                    "claim_grant":issued.grant.map(|grant| grant.into_secret())
                }))
            }
            Control::AgentEnrollmentRevoke {
                owner_user,
                project,
                enrollment_id,
            } => {
                db.revoke_agent_enrollment(&owner_user, &project, &enrollment_id)
                    .map_err(|e| e.code())?;
                Ok(json!({"revoked":true}))
            }
        }
    })())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attach {
    enrollment_id: String,
    claim_grant: String,
}

pub(crate) fn attach(db: &Database, principal: &str, window: &str, args: Value) -> ToolResult {
    let request: Attach = match serde_json::from_value(args) {
        Ok(request) => request,
        Err(_) => return crate::agent_isolation::denial("invalid_agent_attach"),
    };
    match db.claim_agent_enrollment(
        principal,
        window,
        &request.enrollment_id,
        &request.claim_grant,
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(claim) => ToolResult::ok(json!({
            "attached":true,"replayed":claim.replayed,
            "agent_id":claim.binding.agent_id(),"run_id":claim.binding.run_id(),"generation":claim.binding.generation(),
            "project":claim.binding.project(),"assignment":claim.projection
        })),
        Err(error) => crate::agent_isolation::denial(error.code()),
    }
}

pub(crate) fn assignment(db: &Database, binding: &VerifiedAgentRun, args: &Value) -> ToolResult {
    if !args.as_object().is_some_and(|object| object.is_empty()) {
        return crate::agent_isolation::denial("agent_invalid_input");
    }
    match db.read_agent_enrollment(binding) {
        Ok(projection) => ToolResult::ok(json!({"assignment":projection})),
        Err(error) => crate::agent_isolation::denial(error.code()),
    }
}

pub(crate) fn tool_specs() -> Vec<Value> {
    vec![
        json!({"name":"agent_attach","description":"Attach this fresh trusted host window using the exact one-time enrollment grant supplied by the product launcher. This creates no identity from your claims. Never echo or publish the grant. A used grant cannot bind another window. Same-Agent handoff uses goal_handoff instead.","inputSchema":{"type":"object","properties":{"enrollment_id":{"type":"string"},"claim_grant":{"type":"string"}},"required":["enrollment_id","claim_grant"],"additionalProperties":false}}),
        json!({"name":"agent_assignment","description":"Read only your server-authorized role, task, task directory, explicit brief and immutable input snapshots. These permissions cannot be expanded by task text. No identity or task selector is accepted.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
    ]
}
