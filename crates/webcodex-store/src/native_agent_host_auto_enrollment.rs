//! Explicit local policy admits a fresh native chat into its own Agent/task.
//! Existing window history is never reused to choose another identity.
use super::*;
use crate::{
    AgentEnrollmentPolicy, AgentEnrollmentPurpose, NewAgentEnrollment, NewAgentTask,
    VerifiedAgentRun,
};

fn owner_enabled(conn: &rusqlite::Connection, owner: &str) -> Result<(), AgentIsolationError> {
    let enabled: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND disabled=0 AND disabled_at IS NULL)",
            [owner],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if enabled {
        Ok(())
    } else {
        Err(AgentIsolationError::new(
            "agent_enrollment_owner_unavailable",
        ))
    }
}

impl Database {
    /// Bootstrap control selects the exact owner/project and delegation budget.
    /// This policy grants no access to an existing Agent or conversation.
    pub fn configure_native_host_auto_enrollment(
        &self,
        host_id: &str,
        owner_user: &str,
        project: &str,
        policy: AgentEnrollmentPolicy,
    ) -> Result<(), AgentIsolationError> {
        if [owner_user, project]
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(AgentIsolationError::new(
                "native_host_enrollment_policy_invalid",
            ));
        }
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let enabled: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM wc_native_agent_hosts WHERE host_id=?1 AND enabled=1)",
                [host_id],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if !enabled {
            return Err(invalid());
        }
        owner_enabled(&tx, owner_user)?;
        tx.execute(
            "INSERT INTO wc_native_host_auto_enrollment(host_id,owner_user,project,max_subagents,max_cli_calls)
             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(host_id) DO UPDATE SET
             owner_user=excluded.owner_user,project=excluded.project,
             max_subagents=excluded.max_subagents,max_cli_calls=excluded.max_cli_calls",
            params![host_id, owner_user, project, policy.max_subagents, policy.max_cli_calls],
        ).map_err(unavailable)?;
        tx.commit().map_err(unavailable)
    }

    /// Call only after admitting a supported native tool and its input schema.
    /// The authenticated host and outer hashed window own the whole transaction.
    pub fn ensure_native_host_window(
        &self,
        host: &VerifiedNativeAgentHost,
        window_key: &str,
        now: i64,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        self.select_native_host_workspace(host, window_key, None, now)
    }

    /// Canonical coding entry has already validated its arguments. A fresh
    /// window uses the exact authorized folder; an active window keeps its Agent.
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_native_host_coding_window(
        &self,
        host: &VerifiedNativeAgentHost,
        window_key: &str,
        project: &str,
        client_id: Option<&str>,
        path: Option<&str>,
        now: i64,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        self.select_native_host_window_inner(
            host,
            window_key,
            None,
            Some((
                project.trim(),
                client_id.map(str::trim),
                path.map(str::trim),
            )),
            now,
        )
    }

    pub fn select_native_host_workspace(
        &self,
        host: &VerifiedNativeAgentHost,
        window_key: &str,
        selected_project: Option<&str>,
        now: i64,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        self.select_native_host_window_inner(host, window_key, selected_project, None, now)
    }

    fn select_native_host_window_inner(
        &self,
        host: &VerifiedNativeAgentHost,
        window_key: &str,
        selected_project: Option<&str>,
        coding_source: Option<(&str, Option<&str>, Option<&str>)>,
        now: i64,
    ) -> Result<VerifiedAgentRun, AgentIsolationError> {
        if !hex(window_key, 64) {
            return Err(AgentIsolationError::new("invalid_host_window_key"));
        }
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_native_agent_hosts WHERE host_id=?1 AND principal_key=?2 AND generation=?3 AND enabled=1)",
            params![host.host_id, host.principal_key, host.generation], |row| row.get(0),
        ).map_err(unavailable)?;
        if !valid {
            return Err(invalid());
        }
        // Globally occupied windows include disabled and revoked history. They
        // must never become a fresh Agent, even under a different host principal.
        let occupied: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_host_windows WHERE window_key=?1",
                [window_key],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if occupied != 0 {
            let existing: Option<(String, String, String, String)> = tx.query_row(
                "SELECT r.run_id,a.project,a.agent_id,a.owner_user FROM wc_agent_host_windows w
                 JOIN wc_agent_runs r ON r.run_id=w.run_id JOIN wc_isolated_agents a ON a.agent_id=r.agent_id
                 WHERE w.principal_key=?1 AND w.window_key=?2 AND w.enabled=1 AND r.state='active'",
                params![host.principal_key, window_key], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).optional().map_err(unavailable)?;
            if occupied != 1 {
                return Err(AgentIsolationError::new("agent_host_window_not_active"));
            }
            let (run_id, project, agent_id, owner) =
                existing.ok_or_else(|| AgentIsolationError::new("agent_host_window_not_active"))?;
            if selected_project.is_some_and(|selected| selected != project) {
                return Err(AgentIsolationError::new("workspace_new_chat_required"));
            }
            owner_enabled(&tx, &owner)?;
            return crate::agent_isolation::verify_agent_run_context_on_connection(
                &tx,
                &crate::agent_isolation::host_goal_owner_key(&project, &agent_id),
                &run_id,
            );
        }
        let legacy: Option<(String,String,u32,u32)> = tx.query_row(
            "SELECT owner_user,project,max_subagents,max_cli_calls FROM wc_native_host_auto_enrollment WHERE host_id=?1",
            [&host.host_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional().map_err(unavailable)?;
        let mut team_target = None;
        let (owner, project, max_subagents, max_cli_calls) = if let Some((owner, catalog)) =
            crate::native_workspaces::read_on_connection(&tx, &host.host_id)?
        {
            let choices: Vec<_> = catalog.projects.iter().filter(|p| p.enabled).collect();
            let selected = if let Some((project, client, path)) = coding_source {
                crate::native_workspaces::workspace_for_coding_source(
                    &catalog, project, client, path,
                )?
            } else {
                match selected_project {
                    Some(id) => choices
                        .iter()
                        .find(|p| p.id == id)
                        .copied()
                        .ok_or_else(|| AgentIsolationError::new("workspace_not_authorized"))?,
                    None if choices.len() == 1 => choices[0],
                    None => return Err(AgentIsolationError::new("workspace_selection_required")),
                }
            };
            let policy = match crate::native_workspaces::initial_team_on_connection(
                &tx, &owner, selected,
            )? {
                Some((folder, policy)) => {
                    team_target = Some(folder);
                    policy
                }
                None => AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 0,
                },
            };
            (
                owner,
                selected.id.clone(),
                policy.max_subagents,
                policy.max_cli_calls,
            )
        } else {
            if selected_project.is_some() {
                return Err(AgentIsolationError::new("workspace_not_authorized"));
            }
            let legacy =
                legacy.ok_or_else(|| AgentIsolationError::new("agent_host_enrollment_required"))?;
            if coding_source.is_some_and(|(project, client, path)| {
                project != legacy.1 || client.is_some() || path.is_some()
            }) {
                return Err(AgentIsolationError::new("workspace_not_authorized"));
            }
            legacy
        };
        owner_enabled(&tx, &owner)?;
        let now = now.max(chrono::Utc::now().timestamp_millis());
        let key = format!(
            "native-chat:{}",
            hash(&format!("{}\0{window_key}", host.principal_key))
        );
        let brief = "Independent work for this native chat. Follow the user's requests within this assigned project; other Agents' private state is not shared.";
        let task = crate::agent_task::create_agent_task_on_connection(
            &tx,
            &crate::agent_isolation::enrollment_principal_for_execution(&owner),
            NewAgentTask {
                title: "Native chat".into(),
                instruction: brief.into(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(project.clone()),
                idempotency_key: key.clone(),
            },
            now,
        )
        .map_err(|_| AgentIsolationError::new("agent_enrollment_task_unavailable"))?;
        let task_id = task.task.summary.task_id;
        let mut issued = crate::agent_isolation::issue_agent_enrollment_on_connection(
            &tx,
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Manager,
                owner_user: owner,
                project,
                principal_key: host.principal_key.clone(),
                task_directory: format!("workspace/{task_id}"),
                task_id,
                brief: brief.into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents,
                    max_cli_calls,
                },
                expires_at_unix_ms: now.saturating_add(60_000),
            },
            None,
            &key,
            now,
            false,
        )?;
        if let Some(folder) = team_target {
            tx.execute("INSERT INTO wc_native_workspace_team_targets(agent_id,host_id,folder_json) VALUES(?1,?2,?3)", params![issued.agent_id,host.host_id,serde_json::to_string(&folder).map_err(unavailable)?]).map_err(unavailable)?;
        }
        let secret = issued
            .grant
            .take()
            .ok_or_else(|| AgentIsolationError::new("agent_host_window_not_active"))?
            .into_secret();
        let claimed = crate::agent_isolation::claim_agent_enrollment_on_connection(
            &tx,
            &host.principal_key,
            window_key,
            &issued.enrollment_id,
            &secret,
            now,
        )?;
        tx.commit().map_err(unavailable)?;
        Ok(claimed.binding)
    }
}
