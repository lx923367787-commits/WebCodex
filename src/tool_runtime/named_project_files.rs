//! No shell or owner-token impersonation: each request retains an exact granted Runner/root pair.
use super::{ToolResult, ToolRuntime};
use crate::agent_isolation::denial;
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_store::VerifiedAgentRun;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List {
        folder: String,
        path: Option<String>,
        limit: Option<usize>,
    },
    Read {
        folder: String,
        path: String,
        start_line: Option<usize>,
        limit: Option<usize>,
    },
    Write {
        folder: String,
        path: String,
        content: String,
        expected_sha256: Option<String>,
    },
}
pub(crate) fn spec() -> Value {
    json!({"name":"agent_project_files","description":"List, read or write UTF-8 files in an explicitly authorized folder of your selected named project. First read agent_assignment for exact folders; folder is the returned folder.project identifier, never a path or another project. Paths are relative to that folder. List is bounded to depth 1; reads support start_line/limit. Write creates a new file unless expected_sha256 is supplied for an existing-file replacement; read its current hash first and never blindly retry an unknown write. Revoked folders and paths outside the folder are denied. This does not grant shell/CLI execution. A successor retains the same enrolled assignment and its authorized named folders; this does not expose private Goal workspaces. Writes require the assignment to remain active and its run to pass the business gate. An admitted request retains its pinned target even if a later grant changes.","inputSchema":{"type":"object","properties":{"action":{"enum":["list","read","write"]},"folder":{"type":"string"},"path":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500},"start_line":{"type":"integer","minimum":1},"content":{"type":"string","maxLength":262144},"expected_sha256":{"type":"string"}},"required":["action","folder"],"additionalProperties":false}})
}
pub(crate) async fn call(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
    args: Value,
) -> ToolResult {
    let request: Request = match serde_json::from_value(args) {
        Ok(r) => r,
        Err(_) => return denial("workspace_invalid_input"),
    };
    let valid = match &request {
        Request::List { path, limit, .. } => {
            path.as_ref().is_none_or(|p| p.len() <= 4096)
                && limit.is_none_or(|n| (1..=500).contains(&n))
        }
        Request::Read {
            path,
            limit,
            start_line,
            ..
        } => {
            path.len() <= 4096
                && limit.is_none_or(|n| (1..=500).contains(&n))
                && start_line.is_none_or(|n| n > 0)
        }
        Request::Write { path, content, .. } => path.len() <= 4096 && content.len() <= 262144,
    };
    if !valid {
        return denial("workspace_invalid_input");
    }
    let (folder_id, write) = match &request {
        Request::List { folder, .. } | Request::Read { folder, .. } => (folder, false),
        Request::Write { folder, .. } => (folder, true),
    };
    let folder_id = folder_id.clone();
    if folder_id.len() > 256 {
        return denial("workspace_invalid_input");
    }
    let Some(db) = runtime.isolation_database() else {
        return denial("agent_isolation_unavailable");
    };
    let workspace = match db.agent_native_workspace_for_files(binding, write) {
        Ok(p) => p,
        Err(e) => return denial(e.code()),
    };
    let Some(folder) = workspace.folders.iter().find(|f| f.project == folder_id) else {
        return denial("workspace_folder_not_authorized");
    };
    let owner = match db.get_user_by_id(binding.owner_user()) {
        Ok(Some(u)) if !u.is_disabled() => u,
        _ => return denial("workspace_owner_unavailable"),
    };
    let project = match runtime.execution().resolve_project(&folder_id).await {
        Ok(p) => p,
        Err(_) => return denial("workspace_folder_unavailable"),
    };
    if project.path != folder.path {
        return denial("workspace_folder_changed");
    }
    let access = webcodex_runner_registry::RunnerAccess {
        global_visibility: false,
        owner_bypass: false,
        username: Some(owner.username),
        group: None,
    };
    if runtime
        .runner_registry
        .assert_runner_access(Some(&access), &project.client_id)
        .await
        .is_err()
    {
        return denial("workspace_folder_not_authorized");
    }
    // Recheck grant after asynchronous registry observations. The dispatched operation uses
    // this pinned config, so a later registry change cannot retarget it to another root.
    if !db
        .agent_native_workspace_for_files(binding, write)
        .is_ok_and(|p| p.folders.contains(folder))
    {
        return denial("workspace_folder_not_authorized");
    }
    let permission = if write {
        if !project.allow_patch {
            return denial("workspace_folder_read_only");
        }
        runtime
            .config
            .permission_evaluator
            .evaluate("write_project_file", Some(&folder_id))
    } else {
        None
    };
    if let Some(decision) = &permission {
        if !decision.allows_execution() {
            return super::permissions::permission_execution_denied_result(decision);
        }
    }
    let mut result = match request {
        Request::List { path, limit, .. } => {
            runtime
                .project_overview_resolved(&project, folder_id.clone(), path, Some(1), limit)
                .await
        }
        Request::Read {
            path,
            start_line,
            limit,
            ..
        } => {
            runtime
                .read_one_resolved_project_file(
                    &project,
                    path,
                    start_line,
                    limit,
                    false,
                    tokio::time::Instant::now() + std::time::Duration::from_secs(32),
                )
                .await
        }
        Request::Write {
            path,
            content,
            expected_sha256,
            ..
        } => {
            if content.len() > 262144 {
                return denial("workspace_invalid_input");
            }
            let overwrite = expected_sha256.is_some();
            runtime
                .write_resolved_project_file(
                    &project,
                    path,
                    content,
                    Some(overwrite),
                    expected_sha256,
                )
                .await
        }
    };
    if let Some(permission) = permission {
        super::permissions::add_permission_to_result(&mut result, &permission);
    }
    result
}
