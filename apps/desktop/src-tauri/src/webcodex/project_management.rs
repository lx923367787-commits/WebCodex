use super::*;
use crate::models::{ProjectAction, RegisteredProject};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct RegistryInventory {
    client_id: String,
    pub(crate) projects: Vec<RegisteredProject>,
    #[serde(default)]
    pub(crate) matched_count: Option<usize>,
    #[serde(default)]
    pub(crate) truncated: Option<bool>,
    #[serde(default)]
    pub(crate) operator_user_id: Option<String>,
}

impl RegistryInventory {
    fn for_client(self, client_id: &str) -> DesktopResult<Self> {
        if self.client_id != client_id
            || self.operator_user_id.as_deref().is_some_and(|id| {
                id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control)
            })
            || self
                .projects
                .iter()
                .any(|project| !project.id.starts_with(&format!("agent:{client_id}:")))
        {
            return Err(invalid_contract("project list"));
        }
        Ok(self)
    }

    /// UI callers may display a legacy or partial inventory. Automatic scope
    /// consumers must explicitly require authoritative completeness first.
    pub(crate) fn require_complete(&self) -> DesktopResult<()> {
        if self.truncated != Some(false) || self.matched_count != Some(self.projects.len()) {
            return Err(DesktopError::new(
                "project_inventory_incomplete",
                "The Server did not confirm a complete project inventory",
                "Read a complete inventory before starting automatic project work.",
            ));
        }
        Ok(())
    }
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
        Ok(
            Self::list_registry_inventory_with_binary(
                webcodex,
                connection,
                client_id,
                cancellation,
            )
            .await?
            .projects,
        )
    }

    pub(crate) async fn list_registry_inventory_with_binary(
        webcodex: &Path,
        connection: &RuntimeConnection,
        client_id: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<RegistryInventory> {
        let output: RegistryInventory = super::super::cli::run_project_management_json(
            webcodex,
            &arguments(connection, client_id, &connection.user_token_file, "list"),
            cancellation,
        )
        .await?;
        output.for_client(client_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn projects() -> Value {
        json!({
            "client_id":"runner",
            "projects":[
                {"id":"agent:runner:enabled","name":"Enabled","path":"/enabled","revision":"1",
                 "enabled":true,"connected":false,"agent_status":"idle","active_jobs":0},
                {"id":"agent:runner:disabled","name":"Disabled","path":"/disabled","revision":"2",
                 "enabled":false,"connected":false,"agent_status":"idle","active_jobs":0}
            ]
        })
    }

    fn decode(value: Value) -> DesktopResult<RegistryInventory> {
        serde_json::from_value::<RegistryInventory>(value)
            .unwrap()
            .for_client("runner")
    }

    #[test]
    fn complete_inventory_preserves_disconnected_and_disabled_rows() {
        let mut value = projects();
        value["matched_count"] = json!(2);
        value["truncated"] = json!(false);
        let inventory = decode(value).unwrap();
        inventory.require_complete().unwrap();
        assert_eq!(inventory.projects.len(), 2);
        assert!(inventory.projects[0].enabled);
        assert!(!inventory.projects[1].enabled);
        assert!(inventory.projects.iter().all(|project| !project.connected));
    }

    #[test]
    fn legacy_unknown_truncated_and_inconsistent_inventories_are_not_complete() {
        let legacy = decode(projects()).unwrap();
        assert_eq!(legacy.projects.len(), 2);
        assert_eq!(legacy.matched_count, None);
        assert_eq!(legacy.truncated, None);
        assert_eq!(
            legacy.require_complete().unwrap_err().code,
            "project_inventory_incomplete"
        );
        for (matched, truncated) in [
            (json!(null), json!(null)),
            (json!(null), json!(false)),
            (json!(2), json!(null)),
            (json!(2), json!(true)),
            (json!(3), json!(false)),
            (json!(1), json!(false)),
        ] {
            let mut value = projects();
            value["matched_count"] = matched;
            value["truncated"] = truncated;
            let inventory = decode(value).unwrap();
            assert_eq!(inventory.projects.len(), 2);
            assert_eq!(
                inventory.require_complete().unwrap_err().code,
                "project_inventory_incomplete"
            );
        }
    }

    #[test]
    fn explicitly_complete_empty_inventory_is_valid() {
        let inventory =
            decode(json!({"client_id":"runner","projects":[],"matched_count":0,"truncated":false}))
                .unwrap();
        inventory.require_complete().unwrap();
        assert!(inventory.projects.is_empty());
    }

    #[test]
    fn operator_identity_is_preserved_from_the_same_inventory_response() {
        let mut value = projects();
        value["matched_count"] = json!(2);
        value["truncated"] = json!(false);
        value["operator_user_id"] = json!("authenticated-human");
        let inventory = decode(value.clone()).unwrap();
        inventory.require_complete().unwrap();
        assert_eq!(
            inventory.operator_user_id.as_deref(),
            Some("authenticated-human")
        );
        for invalid in [
            "".to_string(),
            " \t".to_string(),
            "x".repeat(257),
            "user\nother".into(),
        ] {
            value["operator_user_id"] = json!(invalid);
            assert!(decode(value.clone()).is_err());
        }
        value.as_object_mut().unwrap().remove("operator_user_id");
        assert!(decode(value).unwrap().operator_user_id.is_none());
    }

    #[test]
    fn inventory_keeps_exact_client_and_project_prefix_checks() {
        let mut wrong_client = projects();
        wrong_client["client_id"] = json!("another-runner");
        assert_eq!(
            decode(wrong_client).unwrap_err().code,
            "webcodex_contract_invalid"
        );
        for id in [
            "agent:another-runner:one",
            "agent:runner-other:one",
            "agent:runner",
        ] {
            let mut wrong_project = projects();
            wrong_project["projects"][0]["id"] = json!(id);
            assert_eq!(
                decode(wrong_project).unwrap_err().code,
                "webcodex_contract_invalid"
            );
        }
    }

    #[test]
    fn inventory_metadata_is_typed_without_coercing_completeness() {
        for (field, value) in [
            ("matched_count", json!(-1)),
            ("matched_count", json!("2")),
            ("truncated", json!("false")),
        ] {
            let mut inventory = projects();
            inventory[field] = value;
            assert!(serde_json::from_value::<RegistryInventory>(inventory).is_err());
        }
    }
}
