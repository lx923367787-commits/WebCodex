//! Desktop selection is separate from the Runner's authoritative project registry.
use super::*;
use crate::models::{ProjectAction, ProjectRegistrySnapshot, RegisteredProject};
use crate::webcodex::RuntimeConnection;

pub(super) fn project_error(code: &str) -> DesktopError {
    DesktopError::new(
        code,
        "The project operation could not be completed",
        "Refresh the project list and check the current runtime before retrying.",
    )
}

pub(super) fn connection_from_config(config: &StoredDesktopConfig) -> Option<RuntimeConnection> {
    let runtime = config.runtime.as_ref()?;
    let runner_config = runtime.runner_config.clone()?;
    let user_token_file = runtime.user_token_file.clone()?;
    if !runner_config.is_file() || !user_token_file.is_file() {
        return None;
    }
    Some(RuntimeConnection {
        runner_config,
        user_token_file,
        server_url: runtime.server_url.clone(),
    })
}

fn locally_managed(config: &StoredDesktopConfig) -> bool {
    config.topology.as_ref().is_some_and(|t| {
        t.experience == Experience::Full && matches!(t.server, ServerTopology::Local)
    })
}

fn select_observed(
    projects: Vec<RegisteredProject>,
    id: &str,
    revision: &str,
) -> DesktopResult<RegisteredProject> {
    let project = projects
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| project_error("project_revision_conflict"))?;
    if project.revision != revision {
        return Err(project_error("project_revision_conflict"));
    }
    if project.agent_status != "online" {
        return Err(project_error("project_agent_unavailable"));
    }
    Ok(project)
}

fn definitely_rejected(error: &DesktopError) -> bool {
    matches!(
        error.code.as_str(),
        "project_revision_conflict"
            | "project_active_jobs_conflict"
            | "project_unauthorized"
            | "project_forbidden"
            | "project_invalid_request"
            | "project_unsupported_runner_version"
            | "project_agent_unavailable"
            | "project_connection_changed"
    )
}

fn clear_project_selection(config: &mut StoredDesktopConfig, client_id: &str) {
    config.project = None;
    if let Some(runtime) = config.runtime.as_mut() {
        runtime.runner_client_id = Some(client_id.to_owned());
        runtime.project_id = None;
        runtime.runtime_project_id = None;
    }
}

impl AppState {
    pub async fn list_projects(&self) -> DesktopResult<ProjectRegistrySnapshot> {
        let (binary, connection, client, can_manage) = {
            let slot = self.core.lock().await;
            let core = slot
                .as_ref()
                .ok_or_else(|| project_error("desktop_operation_busy"))?;
            (
                core.adapter.binaries()?.webcodex.clone(),
                connection_from_config(&core.config)
                    .ok_or_else(|| project_error("runtime_not_ready"))?,
                stored_runner_client_id(&core.config)
                    .ok_or_else(|| project_error("runtime_not_ready"))?,
                locally_managed(&core.config),
            )
        };
        let cancellation =
            CancellationContext::new(CancellationSignal::new(), self.shutdown_signal.clone());
        let projects = WebCodexAdapter::list_registry_with_binary(
            &binary,
            &connection,
            &client,
            &cancellation,
        )
        .await?;
        Ok(ProjectRegistrySnapshot {
            projects,
            can_manage,
        })
    }

    pub async fn add_project(&self, path: &str) -> DesktopResult<DesktopStateSnapshot> {
        let (operation, cancellation, mut core, baseline) = self
            .begin_operation(DesktopOperationKind::ProjectManagement, false)
            .await?;
        let result = core.add_project(path, &cancellation).await;
        self.finish_operation(operation, cancellation, core, baseline, result)
            .await
    }

    pub async fn switch_project(
        &self,
        id: &str,
        revision: &str,
    ) -> DesktopResult<DesktopStateSnapshot> {
        let (operation, cancellation, mut core, baseline) = self
            .begin_operation(DesktopOperationKind::ProjectManagement, false)
            .await?;
        let result = core
            .switch_registered_project(id, revision, &cancellation)
            .await;
        self.finish_operation(operation, cancellation, core, baseline, result)
            .await
    }

    pub async fn mutate_project(
        &self,
        id: &str,
        revision: &str,
        action: ProjectAction,
    ) -> DesktopResult<DesktopStateSnapshot> {
        let (operation, cancellation, mut core, baseline) = self
            .begin_operation(DesktopOperationKind::ProjectManagement, false)
            .await?;
        let result = core
            .mutate_project(id, revision, action, &operation.id, &cancellation)
            .await;
        self.finish_operation(operation, cancellation, core, baseline, result)
            .await
    }
}

impl DesktopCore {
    async fn switch_registered_project(
        &mut self,
        id: &str,
        revision: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<DesktopStateSnapshot> {
        let project = select_observed(self.registry_projects(cancellation).await?, id, revision)?;
        if !project.enabled {
            return Err(project_error("project_disabled"));
        }
        self.activate_local_project(&project.path, cancellation)
            .await
    }
    pub(super) async fn registry_projects(
        &mut self,
        cancellation: &CancellationContext,
    ) -> DesktopResult<Vec<RegisteredProject>> {
        let connection = connection_from_config(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let client = stored_runner_client_id(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let binary = self
            .adapter
            .ensure_binaries(cancellation)
            .await?
            .webcodex
            .clone();
        WebCodexAdapter::list_registry_with_binary(&binary, &connection, &client, cancellation)
            .await
    }

    fn require_local_project_management(&self) -> DesktopResult<()> {
        if !locally_managed(&self.config) || self.snapshot.quick_share.is_some() {
            return Err(project_error("unsupported_topology"));
        }
        Ok(())
    }

    async fn add_project(
        &mut self,
        path: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<DesktopStateSnapshot> {
        self.require_local_project_management()?;
        let connection = connection_from_config(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let client = stored_runner_client_id(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let project = self.adapter.inspect_project(path).await?;
        let identity = self
            .adapter
            .activate_connection_project(&connection, &client, path, &project, cancellation)
            .await?;
        self.wait_for_project(&identity, cancellation).await?;
        // Registration alone does not switch the selected folder or reconfigure its plugins.
        self.get_state().await
    }

    async fn mutate_project(
        &mut self,
        id: &str,
        revision: &str,
        action: ProjectAction,
        key: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<DesktopStateSnapshot> {
        self.require_local_project_management()?;
        let connection = connection_from_config(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let client = stored_runner_client_id(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let env_file = self
            .config
            .runtime
            .as_ref()
            .and_then(|r| r.server_env_file.clone())
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let project = select_observed(self.registry_projects(cancellation).await?, id, revision)?;
        if action == ProjectAction::Unregister && project.active_jobs > 0 {
            return Err(project_error("project_active_jobs_conflict"));
        }
        let old_config = self.config.clone();
        let clear = action != ProjectAction::Enable
            && self
                .config
                .runtime
                .as_ref()
                .and_then(|r| r.runtime_project_id.as_deref())
                == Some(id);
        if clear {
            // Save before dispatch so a crash after unregister cannot re-register the folder.
            clear_project_selection(&mut self.config, &client);
            if let Err(error) = self.save_config().await {
                self.config = old_config;
                return Err(error);
            }
            self.project_selection_cleared();
        }
        if let Err(error) = self
            .adapter
            .mutate_registry(
                &connection,
                &client,
                &env_file,
                &project,
                action,
                key,
                cancellation,
            )
            .await
        {
            if clear && definitely_rejected(&error) {
                let cleared = self.config.clone();
                self.config = old_config;
                if let Err(save_error) = self.save_config().await {
                    self.config = cleared;
                    return Err(save_error);
                }
                self.snapshot.project = project_snapshot(&self.config);
                self.snapshot.readiness = aggregate_readiness(
                    self.snapshot.readiness.server.clone(),
                    self.snapshot.readiness.runner.clone(),
                    self.snapshot.readiness.exposure.clone(),
                    ProjectReadiness::Ready,
                );
            }
            // Unknown results retain no automatic project selection; re-list original evidence.
            self.publish_snapshot();
            return Err(error);
        }
        self.get_state().await
    }

    fn project_selection_cleared(&mut self) {
        self.snapshot.project = None;
        self.snapshot.chatgpt_activity = None;
        self.snapshot.readiness = aggregate_readiness(
            self.snapshot.readiness.server.clone(),
            self.snapshot.readiness.runner.clone(),
            self.snapshot.readiness.exposure.clone(),
            ProjectReadiness::None,
        );
        self.publish_snapshot();
    }

    pub(super) async fn refresh_unselected_runtime(
        &mut self,
        cancellation: &CancellationContext,
    ) -> DesktopResult<DesktopStateSnapshot> {
        let Some(connection) = connection_from_config(&self.config) else {
            return self.get_state().await;
        };
        let client = stored_runner_client_id(&self.config);
        self.snapshot.binaries = Some(self.adapter.ensure_binaries(cancellation).await?.info());
        let server = self
            .adapter
            .server_status(
                Some(&connection.server_url),
                self.config
                    .runtime
                    .as_ref()
                    .and_then(|r| r.server_env_file.as_deref()),
                Some(&connection.user_token_file),
                cancellation,
            )
            .await;
        cancellation.check()?;
        self.snapshot.readiness.server = match server {
            Ok(status) if status.http_reachable => ServerReadiness::Ready,
            Ok(_) => ServerReadiness::Error,
            Err(_) => ServerReadiness::Unknown,
        };
        let runner = self
            .adapter
            .observe_connection_until(&connection, client.as_deref(), cancellation, None)
            .await;
        cancellation.check()?;
        self.snapshot.readiness.runner = match runner {
            Ok(observation) if observation.online => RunnerReadiness::Ready,
            Ok(_) => RunnerReadiness::Connecting,
            Err(_) => RunnerReadiness::Unknown,
        };
        self.project_selection_cleared();
        self.get_state().await
    }

    pub(super) async fn resume_unselected_runtime(
        &mut self,
        cancellation: &CancellationContext,
    ) -> DesktopResult<DesktopStateSnapshot> {
        self.require_local_project_management()?;
        let connection = connection_from_config(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let client = stored_runner_client_id(&self.config)
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        let env_file = self
            .config
            .runtime
            .as_ref()
            .and_then(|r| r.server_env_file.clone())
            .ok_or_else(|| project_error("runtime_not_ready"))?;
        self.snapshot.binaries = Some(self.adapter.ensure_binaries(cancellation).await?.info());
        let deadline = Deadline::after(SERVER_READY_TIMEOUT);
        let running = self
            .adapter
            .server_status_until(
                Some(&connection.server_url),
                Some(&env_file),
                Some(&connection.user_token_file),
                cancellation,
                deadline,
            )
            .await
            .is_ok_and(|s| s.http_reachable);
        if !running {
            let command = self.adapter.local_server_command(&env_file)?;
            self.spawn_owned(ProcessKind::LocalServer, command, false, cancellation)
                .await?;
        }
        self.wait_for_server(
            &connection.server_url,
            Some(&env_file),
            Some(&connection.user_token_file),
            cancellation,
            deadline,
            !running,
        )
        .await?;
        let deadline = Deadline::after(RUNNER_READY_TIMEOUT);
        let running = self
            .adapter
            .observe_connection_until(&connection, Some(&client), cancellation, Some(deadline))
            .await?
            .online;
        if !running {
            let command = self
                .adapter
                .local_runner_command(&connection.runner_config)?;
            self.spawn_owned(ProcessKind::LocalRunner, command, false, cancellation)
                .await?;
        }
        self.wait_for_connection(&connection, Some(&client), cancellation, deadline, !running)
            .await?;
        self.snapshot.readiness.server = ServerReadiness::Ready;
        self.snapshot.readiness.runner = RunnerReadiness::Ready;
        self.config.runtime_autostart = Some(true);
        self.save_config().await?;
        self.project_selection_cleared();
        self.get_state().await
    }
}

#[cfg(test)]
#[path = "project_management_tests.rs"]
mod tests;
