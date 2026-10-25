//! Human-authorized named projects over the saved Runner's exact folder registry.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDraft {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub folders: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSave {
    pub expected_host_id: String,
    pub expected_revision: i64,
    pub folder_revisions: std::collections::BTreeMap<String, String>,
    pub projects: Vec<WorkspaceDraft>,
}

impl AppState {
    pub async fn named_workspaces(&self, save: Option<WorkspaceSave>) -> DesktopResult<Value> {
        // Keep the saved connection stable until the finite control operation completes.
        let slot = self.core.lock().await;
        let core = slot
            .as_ref()
            .ok_or_else(|| project_management::project_error("desktop_operation_busy"))?;
        core.require_local_project_management()?;
        let runtime = core
            .config
            .runtime
            .as_ref()
            .ok_or_else(|| project_management::project_error("runtime_not_ready"))?;
        let host = runtime
            .native_host_id
            .as_deref()
            .ok_or_else(|| project_management::project_error("runtime_not_ready"))?;
        let connection = project_management::connection_from_config(&core.config)
            .ok_or_else(|| project_management::project_error("runtime_not_ready"))?;
        let client = stored_runner_client_id(&core.config)
            .ok_or_else(|| project_management::project_error("runtime_not_ready"))?;
        let env = runtime
            .server_env_file
            .as_ref()
            .ok_or_else(|| project_management::project_error("runtime_not_ready"))?;
        let token = crate::bundled_tools::bootstrap_credential_file(env)?;
        let binary = &core.adapter.binaries()?.webcodex;
        let cancellation =
            CancellationContext::new(CancellationSignal::new(), self.shutdown_signal.clone());
        let params = if let Some(save) = save {
            if save.expected_host_id != host {
                return Err(project_management::project_error(
                    "workspace_connection_changed",
                ));
            }
            if save.projects.len() > 32
                || save
                    .projects
                    .iter()
                    .any(|p| p.folders.is_empty() || p.folders.len() > 16)
            {
                return Err(project_management::project_error("workspace_invalid_input"));
            }
            let inventory = WebCodexAdapter::list_registry_inventory_with_binary(
                binary,
                &connection,
                &client,
                &cancellation,
            )
            .await?;
            inventory.require_complete()?;
            let owner = inventory
                .operator_user_id
                .ok_or_else(|| project_management::project_error("workspace_owner_unavailable"))?;
            let mut projects = vec![];
            for draft in save.projects {
                let mut folders = vec![];
                for id in draft.folders {
                    let folder = inventory
                        .projects
                        .iter()
                        .find(|p| {
                            p.id == id
                                && p.enabled
                                && save.folder_revisions.get(&id) == Some(&p.revision)
                        })
                        .ok_or_else(|| {
                            project_management::project_error("workspace_folder_unavailable")
                        })?;
                    // Frontend submits identifiers only. Paths come from the authoritative registry.
                    folders
                        .push(json!({"project":folder.id,"name":folder.name,"path":folder.path}));
                }
                projects.push(json!({"id":draft.id,"name":draft.name,"enabled":draft.enabled,"folders":folders}));
            }
            json!({"action":"native_workspaces_save","host_id":host,"owner_user":owner,"expected_revision":save.expected_revision,"projects":projects})
        } else {
            json!({"action":"native_workspaces_read","host_id":host})
        };
        let envelope = WebCodexAdapter::workspace_call(
            binary,
            &connection.server_url,
            token.path(),
            &params,
            &cancellation,
        )
        .await?;
        let mut catalog = team_bridge::successful_team_output(envelope)?;
        catalog["host_id"] = json!(host);
        Ok(catalog)
    }
}
