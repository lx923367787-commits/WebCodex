//! Trusted native relay provisioning. This is not a model tool or OAuth grant.
use crate::{
    auth::{AuthContext, AuthKind},
    Database,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    NativeHostRegister {
        tunnel_id: String,
        binary_path: std::path::PathBuf,
    },
    NativeHostRevoke {
        host_id: String,
    },
}

pub(crate) fn try_control(
    db: &Database,
    auth: &AuthContext,
    body: &Value,
    local_peer: bool,
) -> Option<Result<Value, String>> {
    if !matches!(
        body["action"].as_str(),
        Some("native_host_register" | "native_host_revoke")
    ) {
        return None;
    }
    Some((|| {
        if !local_peer
            || auth.kind != AuthKind::Bootstrap
            || !auth.is_bootstrap()
            || auth.agent_run.is_some()
            || auth.native_host.is_some()
        {
            return Err("native_host_local_control_required".into());
        }
        let request: Control =
            serde_json::from_value(body.clone()).map_err(|_| "invalid_native_host_control")?;
        match request {
            Control::NativeHostRegister {
                tunnel_id,
                binary_path,
            } => {
                // The server reads and verifies the real local file; a caller's
                // version string or digest assertion does not establish trust.
                let digest = crate::project_entry::native_tunnel_binary_digest(&binary_path)
                    .map_err(|_| "native_host_binary_untrusted")?;
                let grant = db
                    .register_native_agent_host(&tunnel_id, &digest)
                    .map_err(|e| e.code())?;
                let host_id = grant.host_id.clone();
                let generation = grant.generation;
                Ok(
                    json!({"host_id":host_id,"generation":generation,"host_credential":grant.into_secret(),
                    "state":"active"}),
                )
            }
            Control::NativeHostRevoke { host_id } => {
                db.revoke_native_agent_host(&host_id)
                    .map_err(|e| e.code())?;
                Ok(json!({"revoked":true}))
            }
        }
    })())
}
