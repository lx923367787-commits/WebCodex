//! Service enrollment and recovery do not select or register a project.
use super::project_management::connection_from_config;
use super::*;

impl DesktopCore {
    pub(super) async fn configure_services(
        &mut self,
        remote_url: Option<&str>,
        pairing_code: &str,
        cancellation: &CancellationContext,
    ) -> DesktopResult<DesktopStateSnapshot> {
        cancellation.check()?;
        self.snapshot.binaries = Some(self.adapter.ensure_binaries(cancellation).await?.info());
        let env_file = if remote_url.is_none() {
            let local = self.data_dir.join("runtime").join("local");
            tokio::fs::create_dir_all(&local).await.map_err(|_| {
                DesktopError::new(
                    "desktop_state_unavailable",
                    "Cannot prepare local service storage",
                    "Check app-data permissions.",
                )
            })?;
            Some(local.join("webcodex.env"))
        } else {
            None
        };
        let server_url = match (&env_file, remote_url) {
            (Some(env), _) => {
                if !env.is_file() {
                    self.adapter
                        .init_local_server(
                            &reserve_loopback_address()?,
                            &env.parent().unwrap().join("data"),
                            env,
                            cancellation,
                        )
                        .await?;
                }
                ensure_desktop_server_defaults(env)?;
                self.adapter
                    .server_status(None, Some(env), None, cancellation)
                    .await?
                    .probe_url
            }
            (_, Some(url)) => crate::webcodex::validate_server_url(url)?,
            _ => unreachable!(),
        };
        let topology = RuntimeTopology {
            experience: Experience::Full,
            server: if env_file.is_some() {
                ServerTopology::Local
            } else {
                ServerTopology::Remote {
                    url: server_url.clone(),
                }
            },
            runner: RunnerTopology::Local,
            exposure: if env_file.is_none() && server_url.starts_with("https://") {
                Exposure::ExistingHttps {
                    url: server_url.clone(),
                }
            } else {
                Exposure::None
            },
            enrollment: Enrollment::ManagedPairing,
        };
        let saved = connection_from_config(&self.config)
            .filter(|c| same_server(&c.server_url, &server_url));
        self.snapshot.topology = Some(topology.clone());
        self.snapshot.readiness = aggregate_readiness(
            ServerReadiness::Starting,
            RunnerReadiness::Connecting,
            exposure_readiness(Some(&topology)),
            self.snapshot.readiness.project.clone(),
        );
        self.publish_snapshot();
        let deadline = Deadline::after(SERVER_READY_TIMEOUT);
        let running = self
            .adapter
            .server_status_until(
                Some(&server_url),
                env_file.as_deref(),
                saved.as_ref().map(|c| c.user_token_file.as_path()),
                cancellation,
                deadline,
            )
            .await
            .is_ok_and(|s| {
                s.http_reachable
                    || (env_file.is_some() && saved.is_none() && s.responding_for_enrollment())
            });
        let started_server = !running && env_file.is_some();
        if started_server {
            let command = self
                .adapter
                .local_server_command(env_file.as_deref().unwrap())?;
            self.spawn_owned(ProcessKind::LocalServer, command, false, cancellation)
                .await?;
        }
        if env_file.is_some() || saved.is_some() {
            self.wait_for_server(
                &server_url,
                env_file.as_deref(),
                saved.as_ref().map(|c| c.user_token_file.as_path()),
                cancellation,
                deadline,
                started_server,
            )
            .await?;
        }
        let saved_client = stored_runner_client_id(&self.config);
        let observation = match saved.as_ref() {
            Some(connection) => self
                .adapter
                .observe_connection_until(connection, saved_client.as_deref(), cancellation, None)
                .await
                .ok(),
            None => None,
        };
        let (connection, client, replaced) = match (saved, observation) {
            (Some(connection), Some(observation)) => (connection, observation.client_id, false),
            _ => {
                let code = match env_file.as_ref() {
                    Some(env) => {
                        self.adapter
                            .create_local_pairing(&server_url, env, cancellation)
                            .await?
                    }
                    None => pairing_code.to_owned(),
                };
                let connection = self
                    .adapter
                    .login_connection(
                        &server_url,
                        &code,
                        &local_enrollment_directory(&self.data_dir, &self.config),
                        cancellation,
                    )
                    .await?;
                drop(code);
                let client = self
                    .adapter
                    .observe_connection_until(&connection, None, cancellation, None)
                    .await?
                    .client_id;
                (connection, client, true)
            }
        };
        // No bootstrap response is published as ready: verify the enrolled
        // Human identity before proceeding to the Runner connection.
        self.wait_for_server(
            &server_url,
            env_file.as_deref(),
            Some(&connection.user_token_file),
            cancellation,
            Deadline::after(SERVER_READY_TIMEOUT),
            started_server,
        )
        .await?;
        self.clear_native_host_for_runtime_change(&server_url, Some(&client))
            .await?;
        // Persist a redeemed one-shot enrollment before starting its Runner. A
        // readiness failure can resume it without spending another pairing code.
        let previous = self.config.clone();
        if replaced {
            self.config.runtime = Some(StoredRuntime {
                native_host_id: None,
                server_url: server_url.clone(),
                server_env_file: env_file.clone(),
                runner_config: Some(connection.runner_config.clone()),
                user_token_file: Some(connection.user_token_file.clone()),
                runner_client_id: Some(client.clone()),
                project_id: None,
                runtime_project_id: None,
            });
            self.config.project = None;
            self.snapshot.chatgpt_activity = None;
        }
        self.config.topology = Some(topology);
        self.config.runtime_autostart = Some(true);
        if let Err(error) = self.save_config().await {
            self.config = previous;
            if started_server {
                self.stop_process(ProcessKind::LocalServer).await;
            }
            return Err(error);
        }
        if replaced {
            self.stop_process(ProcessKind::LocalRunner).await;
        }
        if let Some(env) = env_file.as_ref() {
            if self.bundled_tools.is_some() && crate::bundled_tools::needs_file_bridge(env)? {
                if !process_is_active(self.process_snapshot(ProcessKind::LocalServer).await) {
                    return Err(DesktopError::new(
                        "bundled_tools_external_server",
                        "File bridge requires Desktop's own Server",
                        "Stop the external local Server and resume Desktop.",
                    ));
                }
                let bridge = self
                    .adapter
                    .local_file_bridge_token(&connection, env, cancellation)
                    .await?;
                crate::bundled_tools::install_file_bridge(env, &bridge)?;
                drop(bridge);
                self.stop_process(ProcessKind::RegularTunnel).await;
                self.stop_process(ProcessKind::LocalRunner).await;
                self.stop_process(ProcessKind::LocalServer).await;
                let deadline = Deadline::after(SERVER_READY_TIMEOUT);
                let command = self.adapter.local_server_command(env)?;
                self.spawn_owned(ProcessKind::LocalServer, command, false, cancellation)
                    .await?;
                self.wait_for_server(
                    &server_url,
                    Some(env),
                    Some(&connection.user_token_file),
                    cancellation,
                    deadline,
                    true,
                )
                .await?;
            }
        }
        let deadline = Deadline::after(RUNNER_READY_TIMEOUT);
        let running = !replaced
            && self
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
        self.refresh_runtime_status(cancellation).await
    }
}
