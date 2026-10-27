use super::client_handoff_service::{copy_text_to_clipboard, mcp_url, ClipboardCopyOutcome};
use super::openai_tunnel_service::{prepare_openai_tunnel, start_openai_tunnel};
use super::setup_service::{create_private_dir, write_new_private};
use super::ProductError;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REGULAR_TUNNEL_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RegularServerTunnelOptions {
    pub(crate) local_server_url: String,
    pub(crate) bootstrap_token: String,
    pub(crate) runtime_parent: PathBuf,
}

struct RegularTunnelSession {
    directory: PathBuf,
}

impl RegularTunnelSession {
    fn create(runtime_parent: &Path) -> Result<Self, ProductError> {
        let root = runtime_parent.join("regular-tunnel-runtime");
        create_private_dir(&root)?;
        let directory = root.join(format!("openai-{}", uuid::Uuid::new_v4().simple()));
        create_private_dir(&directory)?;
        Ok(Self { directory })
    }

    fn write_authorization_file(&self, mcp_token: &str) -> Result<PathBuf, ProductError> {
        let token = mcp_token.trim();
        if !token
            .strip_prefix(webcodex_store::NATIVE_AGENT_HOST_TOKEN_PREFIX)
            .is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(tunnel_auth_error(
                "the local Server MCP credential is unavailable",
            ));
        }
        let path = self.directory.join("openai-mcp-authorization");
        write_new_private(&path, format!("Bearer {token}").as_bytes())?;
        Ok(path)
    }
}

impl Drop for RegularTunnelSession {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

pub(crate) async fn run_regular_server_tunnel(
    options: &RegularServerTunnelOptions,
) -> Result<(), ProductError> {
    let local_server_url = validate_local_server_url(&options.local_server_url)?;
    let session = RegularTunnelSession::create(&options.runtime_parent)?;
    let prerequisites = prepare_openai_tunnel().await?;
    super::openai_tunnel_service::native_tunnel_binary_digest(&prerequisites.binary)?;
    let registration =
        provision_native_host(&local_server_url, &options.bootstrap_token, &prerequisites).await?;
    let authorization_file = session.write_authorization_file(&registration.credential)?;
    let deadline = Instant::now() + REGULAR_TUNNEL_STARTUP_TIMEOUT;
    let mut tunnel = start_openai_tunnel(
        &prerequisites,
        &mcp_url(&local_server_url),
        &authorization_file,
        &session.directory,
        deadline,
    )
    .await?;

    let clipboard = copy_text_to_clipboard(&prerequisites.tunnel_id, true).await;
    emit_regular_tunnel_lifecycle("ready", clipboard, &registration.host_id)?;

    let outcome = tokio::select! {
        _ = wait_for_regular_tunnel_stop_signal() => Ok(()),
        result = tunnel.wait_with_recovery(|status| emit_regular_tunnel_lifecycle(status, clipboard, &registration.host_id)) => result,
    };
    tunnel.stop().await;
    if outcome.is_ok() {
        emit_regular_tunnel_lifecycle("stopped", clipboard, &registration.host_id)?;
    }
    outcome
}

struct NativeHostRegistration {
    host_id: String,
    credential: String,
}

async fn provision_native_host(
    local_server_url: &str,
    bootstrap_token: &str,
    prerequisites: &super::openai_tunnel_service::OpenAiTunnelPrerequisites,
) -> Result<NativeHostRegistration, ProductError> {
    // Never follow a redirect or inherit a proxy with the local control secret.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| tunnel_auth_error("native host provisioning unavailable"))?;
    let response = client
        .post(format!("{local_server_url}/api/admin/agent-isolation"))
        .bearer_auth(bootstrap_token)
        .json(
            &json!({"action":"native_host_register","tunnel_id":prerequisites.tunnel_id,
            "binary_path":prerequisites.binary}),
        )
        .send()
        .await
        .map_err(|_| tunnel_auth_error("native host provisioning unavailable"))?;
    if !response.status().is_success() {
        return Err(tunnel_auth_error("native host provisioning denied"));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| tunnel_auth_error("native host provisioning invalid"))?;
    native_registration_from_response(&body)
}

fn native_registration_from_response(body: &Value) -> Result<NativeHostRegistration, ProductError> {
    if body["state"].as_str() != Some("active") {
        return Err(tunnel_auth_error("native host registration is not active"));
    }
    let host_id = body["host_id"]
        .as_str()
        .filter(|id| {
            id.strip_prefix("wc_host_").is_some_and(|v| {
                v.len() == 32
                    && v.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        })
        .ok_or_else(|| tunnel_auth_error("native host identity unavailable"))?;
    let credential = body["host_credential"]
        .as_str()
        .ok_or_else(|| tunnel_auth_error("native host credential unavailable"))?;
    Ok(NativeHostRegistration {
        host_id: host_id.into(),
        credential: credential.into(),
    })
}

fn emit_regular_tunnel_lifecycle(
    status: &str,
    clipboard: ClipboardCopyOutcome,
    host_id: &str,
) -> Result<(), ProductError> {
    let event = machine_regular_tunnel_lifecycle_event(status, clipboard, host_id);
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{event}")
        .and_then(|_| stdout.flush())
        .map_err(|_| {
            ProductError::new(
                "machine_output_failed",
                "WebCodex could not publish Tunnel lifecycle state",
                Some("Check the Tunnel owner output channel."),
            )
        })
}

fn machine_regular_tunnel_lifecycle_event(
    status: &str,
    clipboard: ClipboardCopyOutcome,
    host_id: &str,
) -> Value {
    let mut event = machine_regular_tunnel_ready_event(clipboard);
    event["event"] = json!(status);
    event["host_id"] = json!(host_id);
    if status != "ready" {
        event["ready_for_chatgpt"] = json!(false);
    }
    event
}

fn machine_regular_tunnel_ready_event(clipboard: ClipboardCopyOutcome) -> Value {
    let clipboard_state = match clipboard {
        ClipboardCopyOutcome::Copied => "copied",
        ClipboardCopyOutcome::Unavailable => "unavailable",
        ClipboardCopyOutcome::Disabled => "disabled",
    };
    json!({
        "event": "ready",
        "schema_version": 1,
        "provider": "openai",
        "ready_for_chatgpt": clipboard == ClipboardCopyOutcome::Copied,
        "connection": {
            "kind": "openai_tunnel",
            "clipboard_state": clipboard_state,
            "clipboard_contains": "tunnel_id",
        }
    })
}

fn validate_local_server_url(value: &str) -> Result<String, ProductError> {
    let value = value.trim().trim_end_matches('/');
    let parsed = url::Url::parse(value).map_err(|_| {
        ProductError::new(
            "unsupported_topology",
            "Regular OpenAI Tunnel requires a valid local WebCodex Server URL",
            Some("Start the local WebCodex runtime before starting the Tunnel."),
        )
    })?;
    let host = parsed.host_str().unwrap_or("");
    if parsed.scheme() != "http"
        || !matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err(ProductError::new(
            "unsupported_topology",
            "Regular OpenAI Tunnel only exposes a loopback local WebCodex Server",
            Some("Use this command with Local Full Runtime; remote Server exposure is managed remotely."),
        ));
    }
    Ok(value.to_string())
}

async fn wait_for_regular_tunnel_stop_signal() {
    tokio::select! {
        _ = wait_for_platform_stop_signal() => {},
        _ = wait_for_stdin_eof() => {},
    }
}

async fn wait_for_stdin_eof() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = std::thread::Builder::new()
        .name("webcodex-server-tunnel-stdin".to_string())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buffer = [0_u8; 256];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = tx.send(());
        });
    let _ = rx.await;
}

#[cfg(not(windows))]
async fn wait_for_platform_stop_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(windows)]
async fn wait_for_platform_stop_signal() {
    let mut ctrl_break = match tokio::signal::windows::ctrl_break() {
        Ok(signal) => signal,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = ctrl_break.recv() => {},
    }
}

fn tunnel_auth_error(message: &str) -> ProductError {
    ProductError::new(
        "tunnel_auth_invalid",
        message,
        Some("Restore the local Server bootstrap configuration, then retry."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_ready_event_contains_only_safe_handoff_metadata() {
        let event = machine_regular_tunnel_ready_event(ClipboardCopyOutcome::Copied);
        let encoded = serde_json::to_string(&event).unwrap();
        assert!(encoded.contains("\"provider\":\"openai\""));
        assert!(encoded.contains("\"clipboard_contains\":\"tunnel_id\""));
        assert!(!encoded.contains("CONTROL_PLANE_API_KEY"));
        assert!(!encoded.contains("Authorization"));
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("wc_pat_"));
        assert!(!encoded.contains("wc_boot_"));
    }

    #[test]
    fn lifecycle_events_invalidate_ready_until_verified_recovery() {
        for status in ["ready", "reconnecting", "ready", "failed", "stopped"] {
            let event = machine_regular_tunnel_lifecycle_event(
                status,
                ClipboardCopyOutcome::Copied,
                "wc_host_00000000000000000000000000000000",
            );
            assert_eq!(event["event"], status);
            assert_eq!(event["ready_for_chatgpt"], status == "ready");
            assert_eq!(event["schema_version"], 1);
            assert_eq!(event["connection"]["kind"], "openai_tunnel");
        }
    }

    #[test]
    fn native_registration_lifecycle_exposes_only_non_secret_host_identity() {
        let token = format!("wc_nh_{}", "a".repeat(64));
        let id = "wc_host_00000000000000000000000000000000";
        let body = json!({"state":"active","host_id":id,"host_credential":token});
        let registration = native_registration_from_response(&body).unwrap();
        let event = machine_regular_tunnel_lifecycle_event(
            "ready",
            ClipboardCopyOutcome::Copied,
            &registration.host_id,
        );
        assert_eq!(event["host_id"], id);
        assert!(!event.to_string().contains(&token));
        assert!(native_registration_from_response(
            &json!({"state":"active","host_id":token,"host_credential":token})
        )
        .is_err());
        assert!(native_registration_from_response(
            &json!({"state":"revoked","host_id":id,"host_credential":token})
        )
        .is_err());
    }

    #[test]
    fn regular_tunnel_rejects_non_loopback_server_origins() {
        assert!(validate_local_server_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_local_server_url("http://localhost:8080").is_ok());
        assert!(validate_local_server_url("https://example.test").is_err());
        assert!(validate_local_server_url("http://0.0.0.0:8080").is_err());
    }

    #[test]
    fn authorization_file_is_private_distinct_and_cleaned_up() {
        let temp = tempfile::tempdir().unwrap();
        let bootstrap_token = format!("wc_nh_{}", "a".repeat(64));
        let session = RegularTunnelSession::create(temp.path()).unwrap();
        let session_dir = session.directory.clone();
        let authorization_file = session.write_authorization_file(&bootstrap_token).unwrap();
        assert_eq!(
            std::fs::read_to_string(&authorization_file).unwrap(),
            format!("Bearer {bootstrap_token}")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&authorization_file)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
        #[cfg(windows)]
        {
            let sddl =
                super::super::windows_private_state::dacl_sddl(&authorization_file, false).unwrap();
            assert!(sddl.starts_with("D:P"));
            for broad in [";;;WD)", ";;;BU)", ";;;BA)"] {
                assert!(!sddl.contains(broad));
            }
        }
        drop(session);
        assert!(!session_dir.exists());
        assert!(!authorization_file.exists());
    }

    #[test]
    fn authorization_file_rejects_empty_mcp_credentials() {
        let temp = tempfile::tempdir().unwrap();
        for value in [
            "",
            "   ",
            "wc_pat_secret",
            "wc_ar_secret",
            "bootstrap-secret",
        ] {
            let session = RegularTunnelSession::create(temp.path()).unwrap();
            let error = session.write_authorization_file(value).unwrap_err();
            assert_eq!(error.code, "tunnel_auth_invalid");
            assert_eq!(
                error.message,
                "the local Server MCP credential is unavailable"
            );
            drop(session);
        }
    }
}
