//! Operator-authored named projects. Each folder retains its exact Runner target.
use crate::{AgentIsolationError, Database, VerifiedAgentRun, VerifiedNativeAgentHost};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeWorkspaceFolder {
    pub project: String,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeWorkspace {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub folders: Vec<NativeWorkspaceFolder>,
    /// Exact registered folder used for team policy and new team executions.
    /// Older single-folder projects have an unambiguous implicit target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_project: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeWorkspaceCatalog {
    pub revision: i64,
    pub projects: Vec<NativeWorkspace>,
}

fn error(code: &'static str) -> AgentIsolationError {
    AgentIsolationError::new(code)
}
fn unavailable(_: impl std::fmt::Display) -> AgentIsolationError {
    error("workspace_unavailable")
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

pub(crate) fn is_native_workspace_id(value: &str) -> bool {
    value.strip_prefix("wc_workspace_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

pub(crate) fn read_on_connection(
    conn: &rusqlite::Connection,
    host: &str,
) -> Result<Option<(String, NativeWorkspaceCatalog)>, AgentIsolationError> {
    let row: Option<(String, i64, String)> = conn.query_row(
        "SELECT owner_user,revision,projects_json FROM wc_native_workspace_catalogs WHERE host_id=?1",
        [host], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(unavailable)?;
    row.map(|(owner, revision, json)| {
        Ok((
            owner,
            NativeWorkspaceCatalog {
                revision,
                projects: serde_json::from_str(&json).map_err(unavailable)?,
            },
        ))
    })
    .transpose()
}

/// Only a new native chat may acquire this target. Existing assignments never
/// gain delegation merely because a folder or its policy was subsequently edited.
pub(crate) fn initial_team_on_connection(
    conn: &rusqlite::Connection,
    owner: &str,
    workspace: &NativeWorkspace,
) -> Result<Option<(NativeWorkspaceFolder, crate::AgentEnrollmentPolicy)>, AgentIsolationError> {
    let folder = match workspace.team_project.as_deref() {
        Some(project) => workspace
            .folders
            .iter()
            .find(|folder| folder.project == project)
            .ok_or_else(|| error("workspace_team_target_invalid"))?,
        None => match workspace.folders.as_slice() {
            [folder] => folder,
            _ => return Ok(None),
        },
    };
    let policy = crate::team_task::policy_on(
        conn,
        &crate::agent_isolation::enrollment_principal_for_execution(owner),
        &folder.project,
    )
    .map_err(|_| error("workspace_team_policy_unavailable"))?;
    // A default display policy is not an operator's saved execution grant.
    if policy.revision == 0 {
        return Ok(None);
    }
    let max_cli_calls = policy
        .settings
        .subagents
        .max
        .checked_add(1)
        .and_then(|members| members.checked_mul(policy.settings.cli_per_agent.max))
        .ok_or_else(|| error("workspace_team_budget_invalid"))?
        .min(32);
    Ok(Some((
        folder.clone(),
        crate::AgentEnrollmentPolicy {
            max_subagents: policy.settings.subagents.max,
            max_cli_calls,
        },
    )))
}

pub(crate) fn team_target_on_connection(
    conn: &rusqlite::Connection,
    binding: &VerifiedAgentRun,
) -> Result<Option<NativeWorkspaceFolder>, AgentIsolationError> {
    crate::agent_isolation::validate_agent_run_on_connection(conn, binding)?;
    if !binding.project().starts_with("wc_workspace_") {
        return Ok(None);
    }
    let row: Option<(String, String)> = conn.query_row(
        "SELECT t.host_id,t.folder_json FROM wc_native_workspace_team_targets t JOIN wc_native_agent_hosts h ON h.host_id=t.host_id AND h.enabled=1 WHERE t.agent_id=?1",
        [binding.agent_id()], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional().map_err(unavailable)?;
    let (host, encoded) = row.ok_or_else(|| error("workspace_team_new_chat_required"))?;
    let folder: NativeWorkspaceFolder = serde_json::from_str(&encoded).map_err(unavailable)?;
    let (owner, catalog) =
        read_on_connection(conn, &host)?.ok_or_else(|| error("workspace_not_authorized"))?;
    if owner != binding.owner_user()
        || !catalog.projects.iter().any(|p| {
            p.enabled
                && p.id == binding.project()
                && p.folders
                    .iter()
                    .any(|f| f.project == folder.project && f.path == folder.path)
        })
    {
        return Err(error("workspace_team_target_changed"));
    }
    Ok(Some(folder))
}

pub(crate) fn inherit_team_target_on_connection(
    conn: &rusqlite::Connection,
    source: &VerifiedAgentRun,
    child: &str,
) -> Result<(), AgentIsolationError> {
    if team_target_on_connection(conn, source)?.is_some() {
        conn.execute("INSERT INTO wc_native_workspace_team_targets(agent_id,host_id,folder_json) SELECT ?1,host_id,folder_json FROM wc_native_workspace_team_targets WHERE agent_id=?2", params![child,source.agent_id()]).map_err(unavailable)?;
    }
    Ok(())
}

impl Database {
    pub fn agent_team_execution_folder(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<Option<NativeWorkspaceFolder>, AgentIsolationError> {
        let conn = self.conn.lock().map_err(unavailable)?;
        team_target_on_connection(&conn, binding)
    }
    pub fn agent_team_execution_project(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<String, AgentIsolationError> {
        Ok(self
            .agent_team_execution_folder(binding)?
            .map_or_else(|| binding.project().to_owned(), |f| f.project))
    }
    pub fn read_native_workspaces(
        &self,
        host: &str,
    ) -> Result<NativeWorkspaceCatalog, AgentIsolationError> {
        let conn = self.conn.lock().map_err(unavailable)?;
        Ok(read_on_connection(&conn, host)?
            .map(|(_, c)| c)
            .unwrap_or(NativeWorkspaceCatalog {
                revision: 0,
                projects: vec![],
            }))
    }

    pub fn configure_native_workspaces(
        &self,
        host: &str,
        owner: &str,
        expected_revision: i64,
        projects: Vec<NativeWorkspace>,
    ) -> Result<NativeWorkspaceCatalog, AgentIsolationError> {
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        if projects.len() > 32 || !text(owner, 256) || expected_revision < 0 {
            return Err(error("workspace_invalid_input"));
        }
        for project in &projects {
            if !is_native_workspace_id(&project.id)
                || !ids.insert(&project.id)
                || !text(&project.name, 128)
                || !names.insert(project.name.trim().to_lowercase())
                || project.folders.is_empty()
                || project.folders.len() > 16
            {
                return Err(error("workspace_invalid_input"));
            }
            let mut folders = HashSet::new();
            for folder in &project.folders {
                if !folder.project.starts_with("agent:")
                    || !text(&folder.project, 256)
                    || !folders.insert(&folder.project)
                    || !text(&folder.name, 128)
                    || !text(&folder.path, 4096)
                {
                    return Err(error("workspace_invalid_input"));
                }
            }
            if project
                .team_project
                .as_ref()
                .is_some_and(|target| !folders.contains(target))
            {
                return Err(error("workspace_team_target_invalid"));
            }
        }
        let encoded = serde_json::to_string(&projects).map_err(unavailable)?;
        if encoded.len() > 128 * 1024 {
            return Err(error("workspace_invalid_input"));
        }
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let enabled: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_native_agent_hosts h,users u WHERE h.host_id=?1 AND h.enabled=1 AND u.id=?2 AND u.disabled=0 AND u.disabled_at IS NULL)",params![host,owner],|r|r.get(0)).map_err(unavailable)?;
        if !enabled {
            return Err(error("workspace_owner_or_host_unavailable"));
        }
        let previous = read_on_connection(&tx, host)?;
        if previous
            .as_ref()
            .is_some_and(|(old_owner, _)| old_owner != owner)
        {
            return Err(error("workspace_owner_mismatch"));
        }
        let revision = previous.as_ref().map_or(0, |(_, c)| c.revision);
        // An identical retry is an observation, including after a lost response.
        if let Some((_, catalog)) = &previous {
            if catalog.projects == projects
                && (expected_revision == revision || expected_revision == revision - 1)
            {
                return Ok(catalog.clone());
            }
        }
        if revision != expected_revision {
            return Err(error("workspace_revision_conflict"));
        }
        let revision = revision
            .checked_add(1)
            .ok_or_else(|| error("workspace_revision_conflict"))?;
        tx.execute("INSERT INTO wc_native_workspace_catalogs(host_id,owner_user,revision,projects_json) VALUES(?1,?2,?3,?4) ON CONFLICT(host_id) DO UPDATE SET revision=excluded.revision,projects_json=excluded.projects_json",params![host,owner,revision,encoded]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(NativeWorkspaceCatalog { revision, projects })
    }

    pub fn native_workspace_choices(
        &self,
        host: &VerifiedNativeAgentHost,
    ) -> Result<NativeWorkspaceCatalog, AgentIsolationError> {
        self.validate_native_agent_host(host)?;
        let conn = self.conn.lock().map_err(unavailable)?;
        let Some((owner, mut catalog)) = read_on_connection(&conn, host.host_id())? else {
            return Ok(NativeWorkspaceCatalog {
                revision: 0,
                projects: vec![],
            });
        };
        let enabled: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND disabled=0 AND disabled_at IS NULL)",[owner],|r|r.get(0)).map_err(unavailable)?;
        if !enabled {
            return Err(error("workspace_owner_or_host_unavailable"));
        }
        catalog.projects.retain(|p| p.enabled);
        Ok(catalog)
    }

    pub fn agent_native_workspace(
        &self,
        binding: &VerifiedAgentRun,
    ) -> Result<NativeWorkspace, AgentIsolationError> {
        let conn = self.conn.lock().map_err(unavailable)?;
        crate::agent_isolation::validate_agent_run_on_connection(&conn, binding)?;
        native_workspace_on_connection(&conn, binding)
    }

    /// Named project grants are separate from private Goal workspaces. A successor
    /// may retain them only for the same enrolled assignment and its unique Goal.
    pub fn agent_native_workspace_for_files(
        &self,
        binding: &VerifiedAgentRun,
        write: bool,
    ) -> Result<NativeWorkspace, AgentIsolationError> {
        use crate::agent_isolation::{
            agent_run_goal_scope_on_connection, agent_run_task_goal_on_connection,
            authorize_agent_assignment_on_connection, enrollment_projection_on_connection,
            AgentAssignmentOperation,
        };
        let mut connection = self.conn.lock().map_err(unavailable)?;
        let conn = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(unavailable)?;
        let assignment = enrollment_projection_on_connection(&conn, binding)?;
        authorize_agent_assignment_on_connection(
            &conn,
            binding,
            &assignment.task_id,
            if write {
                AgentAssignmentOperation::ProjectFiles
            } else {
                AgentAssignmentOperation::Read
            },
        )?;
        if let Some(goal) = agent_run_task_goal_on_connection(&conn, binding)? {
            agent_run_goal_scope_on_connection(&conn, binding, Some(&goal))?;
        }
        native_workspace_on_connection(&conn, binding)
    }
}

fn native_workspace_on_connection(
    conn: &rusqlite::Connection,
    binding: &VerifiedAgentRun,
) -> Result<NativeWorkspace, AgentIsolationError> {
    // Pin both the human owner and the exact authenticated host window.
    let mut stmt = conn.prepare("SELECT c.projects_json FROM wc_native_workspace_catalogs c JOIN wc_native_agent_hosts h ON h.host_id=c.host_id JOIN wc_isolated_agents a ON a.owner_user=c.owner_user JOIN wc_agent_host_windows w ON w.principal_key=h.principal_key AND w.run_id=?2 AND w.enabled=1 WHERE a.agent_id=?1 AND h.enabled=1").map_err(unavailable)?;
    let rows = stmt
        .query_map(params![binding.agent_id(), binding.run_id()], |r| {
            r.get::<_, String>(0)
        })
        .map_err(unavailable)?;
    let mut found = None;
    for row in rows {
        let projects: Vec<NativeWorkspace> =
            serde_json::from_str(&row.map_err(unavailable)?).map_err(unavailable)?;
        for project in projects
            .into_iter()
            .filter(|p| p.enabled && p.id == binding.project())
        {
            if found.is_some() {
                return Err(error("workspace_ambiguous"));
            }
            found = Some(project);
        }
    }
    found.ok_or_else(|| error("workspace_not_authorized"))
}
