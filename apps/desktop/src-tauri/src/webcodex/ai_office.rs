//! Read-only AI office projection through the existing authenticated CLI route.
use super::*;
use crate::state::ai_office::OfficeMember;
use serde::Deserialize;

#[derive(Deserialize)]
struct WindowsOutput {
    summary: WindowsSummary,
}
#[derive(Deserialize)]
struct WindowsSummary {
    windows: Vec<Window>,
}
#[derive(Deserialize)]
struct Window {
    client_window_key: String,
    source: String,
    active_count: u64,
    last_meaningful_activity_at_ms: Option<i64>,
}

impl WebCodexAdapter {
    pub async fn ai_office_windows(
        binary: &Path,
        connection: &RuntimeConnection,
        project_id: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<Vec<OfficeMember>> {
        let mut args = vec![
            "ops".into(),
            "windows".into(),
            "--server-url".into(),
            connection.server_url.clone(),
            "--token-file".into(),
            connection.user_token_file.to_string_lossy().into_owned(),
            "--project".into(),
            project_id.into(),
            "--limit".into(),
            "64".into(),
            "--json".into(),
        ];
        if server_url_is_loopback(&connection.server_url) {
            args.push("--no-system-proxy".into());
        }
        let output: WindowsOutput = run_json(binary, &args, None, false, cancellation).await?;
        Ok(output
            .summary
            .windows
            .into_iter()
            .take(64)
            .map(|window| OfficeMember {
                id: window.client_window_key,
                role: "observed_agent",
                label: match window.source.as_str() {
                    "openai-session" | "openai-conversation" => "OpenAI session".into(),
                    _ => "Observed runtime window".into(),
                },
                state: if window.active_count > 0 {
                    "working"
                } else {
                    "idle"
                },
                active_requests: window.active_count,
                last_activity_at_ms: window.last_meaningful_activity_at_ms,
                task: None,
                parent_id: None,
                progress: None,
            })
            .collect())
    }
}
