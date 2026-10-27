//! Explicit control-plane delegation from an authenticated OAuth host to one
//! Agent run. Host metadata alone, client display names and file-import trust
//! never establish this delegation.
use crate::auth::{AuthContext, AuthKind};
use crate::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) fn principal(client: &str, kind: &str, subject: &str) -> Option<String> {
    if client.is_empty() || client.len() > 256 || subject.is_empty() || subject.len() > 256 {
        return None;
    }
    if !matches!(kind, "managed_user" | "shared_key") {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"webcodex.agent-host-principal.v1\0");
    for component in [client, kind, subject] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component.as_bytes());
    }
    Some(format!("{:x}", digest.finalize()))
}

pub(crate) fn valid_window_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Inputs here are from the OAuth verifier's persisted token record, never tool
/// arguments or an HTTP client-name header. Refreshing a token keeps the grant.
pub(crate) fn principal_key(auth: &AuthContext) -> Option<String> {
    if auth.is_native_host() {
        return auth
            .native_host
            .as_ref()
            .map(|h| h.principal_key().to_owned());
    }
    if !auth.is_oauth_token() || auth.agent_run.is_some() {
        return None;
    }
    let client = auth.allowed_client_id.as_deref()?;
    let (kind, subject) = if auth.is_oauth_shared_key_subject() {
        ("shared_key", auth.shared_key_hash.as_deref()?)
    } else if auth.is_oauth_project_subject() {
        // Project-share credentials retain their separate exact project grant;
        // this host adapter must not turn one into a general Agent credential.
        return None;
    } else if auth.token_kind.as_deref() == Some("oauth2") {
        ("managed_user", auth.user_id.as_deref()?)
    } else {
        return None;
    };
    principal(client, kind, subject)
}

pub(crate) fn authorized(db: &Database, auth: &AuthContext) -> bool {
    if auth.is_native_host() {
        // A dedicated relay may initialize and discover this data-free broker
        // before the first enrollment. Every tools/call still needs its exact
        // active window or a valid one-time enrollment/handoff claim.
        return auth.native_host.as_ref().is_some_and(|h| {
            db.validate_native_agent_host(h).is_ok()
                && db.native_agent_host_principal(h.host_id()).ok().as_deref()
                    == Some(h.principal_key())
        });
    }
    principal_key(auth).is_some_and(|key| {
        db.agent_host_principal_authorized(&key).unwrap_or(false)
            || db
                .agent_enrollment_claim_authorized(&key, chrono::Utc::now().timestamp_millis())
                .unwrap_or(false)
    })
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum HostControl {
    BindHostWindow {
        run_token: String,
        oauth_client_id: String,
        oauth_subject_kind: String,
        oauth_subject_id: String,
        window_key: String,
        /// The operator must explicitly attest that this OAuth client owns the
        /// outer openai/session metadata. A generic OAuth app is insufficient.
        host_metadata_exclusive: bool,
    },
    RevokeHostPrincipal {
        oauth_client_id: String,
        oauth_subject_kind: String,
        oauth_subject_id: String,
    },
    BindHandoffWindow {
        oauth_client_id: String,
        oauth_subject_kind: String,
        oauth_subject_id: String,
        window_key: String,
        goal_id: String,
        handoff_id: String,
        attempt_id: String,
    },
}

pub(crate) fn try_control(
    db: &Database,
    auth: &AuthContext,
    body: &Value,
) -> Option<Result<Value, String>> {
    if !matches!(
        body.get("action").and_then(Value::as_str),
        Some("bind_host_window" | "bind_handoff_window" | "revoke_host_principal")
    ) {
        return None;
    }
    Some((|| {
        if auth.kind != AuthKind::Bootstrap || !auth.is_bootstrap() || auth.agent_run.is_some() {
            return Err("trusted_control_required".into());
        }
        let request: HostControl =
            serde_json::from_value(body.clone()).map_err(|_| "invalid_control_request")?;
        match request {
            HostControl::BindHostWindow {
                run_token,
                oauth_client_id,
                oauth_subject_kind,
                oauth_subject_id,
                window_key,
                host_metadata_exclusive,
            } => {
                if !host_metadata_exclusive {
                    return Err("agent_host_metadata_trust_required".into());
                }
                if !valid_window_key(&window_key) {
                    return Err("invalid_host_window_key".into());
                }
                if db
                    .get_oauth_client_by_client_id(&oauth_client_id)
                    .map_err(|_| "agent_host_control_unavailable")?
                    .is_none()
                {
                    return Err("agent_host_client_unavailable".into());
                }
                let key = principal(&oauth_client_id, &oauth_subject_kind, &oauth_subject_id)
                    .ok_or("invalid_host_principal")?;
                let source = db
                    .authenticate_agent_run(&run_token)
                    .map_err(|e| e.code())?;
                db.register_agent_host_window(&key, &window_key, &source)
                    .map_err(|e| e.code())?;
                Ok(json!({"registered":true,"agent_id":source.agent_id(),
                    "run_id":source.run_id(),"project":source.project(),"window_key":window_key}))
            }
            HostControl::RevokeHostPrincipal {
                oauth_client_id,
                oauth_subject_kind,
                oauth_subject_id,
            } => {
                let key = principal(&oauth_client_id, &oauth_subject_kind, &oauth_subject_id)
                    .ok_or("invalid_host_principal")?;
                db.revoke_agent_host_principal(&key).map_err(|e| e.code())?;
                Ok(json!({"revoked":true}))
            }
            HostControl::BindHandoffWindow {
                oauth_client_id,
                oauth_subject_kind,
                oauth_subject_id,
                window_key,
                goal_id,
                handoff_id,
                attempt_id,
            } => {
                if !valid_window_key(&window_key) {
                    return Err("invalid_host_window_key".into());
                }
                let key = principal(&oauth_client_id, &oauth_subject_kind, &oauth_subject_id)
                    .ok_or("invalid_host_principal")?;
                // The trusted launch callback must already know this exact
                // host window. A URL, model claim or attempt UUID alone is not
                // evidence for choosing it. Model MCP cannot call this action.
                db.bind_handoff_host_window(
                    &key,
                    &window_key,
                    &goal_id,
                    &handoff_id,
                    &attempt_id,
                    chrono::Utc::now().timestamp_millis(),
                )
                .map_err(|e| e.code())?;
                Ok(
                    json!({"registered":true,"goal_id":goal_id,"handoff_id":handoff_id,
                    "attempt_id":attempt_id,"window_key":window_key}),
                )
            }
        }
    })())
}

pub(crate) fn tool_scope(name: &str, args: &Value) -> &'static str {
    if crate::tool_runtime::named_project_files::supports(name) {
        return if crate::tool_runtime::named_project_files::is_write(name, args) {
            "project:write"
        } else {
            "project:read"
        };
    }
    match name {
        "agent_identity" | "agent_assignment" | "agent_projects" | "agent_select_project" => {
            "runtime:read"
        }
        "agent_project_files" if args.get("action").and_then(Value::as_str) != Some("write") => {
            "project:read"
        }
        "agent_private" if args.get("action").and_then(Value::as_str) != Some("write") => {
            "project:read"
        }
        "agent_meeting" if args.get("action").and_then(Value::as_str) != Some("publish") => {
            "project:read"
        }
        "goal_read" | "goal_bootstrap" => "project:read",
        "team_decision" if args.get("action").and_then(Value::as_str) == Some("read") => {
            "project:read"
        }
        "team_policy_get"
        | "team_plan_list"
        | "team_plan_read"
        | "team_task_read"
        | "team_receipt_list"
        | "team_task_execution_read"
        | "team_cli_observe"
        | "team_cli_delivery_read"
        | "team_cli_catalog" => "project:read",
        "goal_handoff" if args.get("action").and_then(Value::as_str) == Some("status") => {
            "project:read"
        }
        _ => "project:write",
    }
}
