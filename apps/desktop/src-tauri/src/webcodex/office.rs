//! Fixed CLI transport to the finite human Office API. The Server authorizes
//! the project and policy revision; this bridge cannot dispatch or cancel work.
use super::*;
use crate::state::team_bridge::TeamOfficeAction;
use serde_json::Value;

fn arguments(
    connection: &RuntimeConnection,
    client_id: &str,
    action: TeamOfficeAction,
) -> Vec<String> {
    vec![
        "office".into(),
        action.as_str().into(),
        "--config".into(),
        connection.runner_config.to_string_lossy().into_owned(),
        "--server-url".into(),
        connection.server_url.clone(),
        "--client-id".into(),
        client_id.into(),
        "--token-file".into(),
        connection.user_token_file.to_string_lossy().into_owned(),
        "--json".into(),
    ]
}

impl WebCodexAdapter {
    pub(crate) async fn team_office_call(
        binary: &Path,
        connection: &RuntimeConnection,
        client_id: &str,
        action: TeamOfficeAction,
        params: &Value,
        cancellation: &CancellationContext,
    ) -> DesktopResult<Value> {
        let stdin = serde_json::to_vec(params).map_err(|_| invalid_contract("team request"))?;
        let envelope: Value = run_json_until(
            binary,
            &arguments(connection, client_id, action),
            Some(&stdin),
            true,
            cancellation,
            Deadline::after(std::time::Duration::from_secs(
                if matches!(
                    action,
                    TeamOfficeAction::CliDiscover | TeamOfficeAction::CliModels
                ) {
                    50
                } else {
                    30
                },
            )),
        )
        .await?;
        if envelope.get("success").and_then(Value::as_bool).is_none()
            || envelope.get("output").is_none()
            || envelope
                .get("error")
                .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(invalid_contract("team response"));
        }
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_uses_only_saved_user_connection_and_fixed_action_arguments() {
        let connection = RuntimeConnection {
            runner_config: "runner.toml".into(),
            user_token_file: "user-token".into(),
            server_url: "https://server.example".into(),
        };
        let args = arguments(&connection, "runner", TeamOfficeAction::PlanRead);
        assert_eq!(
            args,
            [
                "office",
                "plan-read",
                "--config",
                "runner.toml",
                "--server-url",
                "https://server.example",
                "--client-id",
                "runner",
                "--token-file",
                "user-token",
                "--json"
            ]
        );
        assert!(!args
            .iter()
            .any(|value| value == "--token" || value == "--env-file" || value == "--tool"));
    }
}
