use super::*;
use crate::models::{ProjectAction, RegisteredProject};
use serde::Deserialize;

#[derive(Deserialize)]
struct RegistryOutput {
    client_id: String,
    projects: Vec<RegisteredProject>,
}

fn arguments(
    connection: &RuntimeConnection,
    client_id: &str,
    token: &Path,
    action: &str,
) -> Vec<String> {
    vec![
        "project".into(),
        action.into(),
        "--config".into(),
        connection.runner_config.to_string_lossy().into(),
        "--server-url".into(),
        connection.server_url.clone(),
        "--client-id".into(),
        client_id.into(),
        "--token-file".into(),
        token.to_string_lossy().into(),
        "--json".into(),
    ]
}

impl WebCodexAdapter {
    pub async fn list_registry_with_binary(
        webcodex: &Path,
        connection: &RuntimeConnection,
        client_id: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<Vec<RegisteredProject>> {
        let output: RegistryOutput = super::super::cli::run_project_management_json(
            webcodex,
            &arguments(connection, client_id, &connection.user_token_file, "list"),
            cancellation,
        )
        .await?;
        if output.client_id != client_id
            || output
                .projects
                .iter()
                .any(|p| !p.id.starts_with(&format!("agent:{client_id}:")))
        {
            return Err(invalid_contract("project list"));
        }
        Ok(output.projects)
    }

    pub async fn mutate_registry(
        &self,
        connection: &RuntimeConnection,
        client_id: &str,
        env_file: &Path,
        project: &RegisteredProject,
        action: ProjectAction,
        key: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<()> {
        // The saved local Server is the only audience for its bootstrap credential.
        if !server_url_is_loopback(&connection.server_url) {
            return Err(DesktopError::new(
                "unsupported_topology",
                "Project administration requires the local Server",
                "Use the Server's authorized administration interface.",
            ));
        }
        let credential = crate::bundled_tools::bootstrap_credential_file(env_file)?;
        let mut args = arguments(connection, client_id, credential.path(), action.as_str());
        args.extend([
            "--project".into(),
            project.id.clone(),
            "--expected-revision".into(),
            project.revision.clone(),
            "--idempotency-key".into(),
            key.into(),
        ]);
        let output: serde_json::Value = super::super::cli::run_project_management_json(
            &self.binaries()?.webcodex,
            &args,
            cancellation,
        )
        .await?;
        if output["project"] != project.id || output["operation"] != action.as_str() {
            return Err(invalid_contract("project mutation"));
        }
        Ok(())
    }
}
