//! Business-file operations retain their exact granted Runner/root and never borrow user tools.
use super::project_resolution::ResolvedProject;
use super::read_files::ReadModelProjection;
use super::tool_inputs::ApplyFileChangeInput;
use super::{ReadFilesItem, SearchProjectTextsQuery, ToolResult, ToolRuntime};
use crate::agent_isolation::denial;
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_store::{AgentProjectFilesGrant, VerifiedAgentRun};

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
        with_line_numbers: Option<bool>,
    },
    ReadFiles {
        folder: String,
        items: Vec<ReadFilesItem>,
        with_line_numbers: Option<bool>,
        max_result_bytes: Option<usize>,
    },
    Search {
        folder: String,
        queries: Vec<SearchProjectTextsQuery>,
        max_result_bytes: Option<usize>,
    },
    Edit {
        folder: String,
        changes: Vec<ApplyFileChangeInput>,
        dry_run: Option<bool>,
    },
    Write {
        folder: String,
        path: String,
        content: String,
        expected_sha256: Option<String>,
    },
}
impl Request {
    fn folder(&self) -> &str {
        match self {
            Self::List { folder, .. }
            | Self::Read { folder, .. }
            | Self::ReadFiles { folder, .. }
            | Self::Search { folder, .. }
            | Self::Edit { folder, .. }
            | Self::Write { folder, .. } => folder,
        }
    }
    fn writes(&self) -> bool {
        matches!(self, Self::Write { .. } | Self::Edit { .. })
    }
}
pub(crate) fn supports(name: &str) -> bool {
    matches!(
        name,
        "agent_project_files"
            | "read_file"
            | "read_files"
            | "search_project_texts"
            | "apply_text_edits"
    )
}
pub(crate) fn is_write(name: &str, args: &Value) -> bool {
    name == "apply_text_edits"
        || (name == "agent_project_files"
            && matches!(args["action"].as_str(), Some("write" | "edit")))
}
const DESCRIPTION: &str = "Access business files in this Agent's explicitly authorized project folders, independently of its private workspace and CLI budget. Read agent_assignment.project_access for exact folder/project ids. Paths are project-relative. Other Agents' private runtime files, secrets and escapes remain denied. Writes require a live assignment and current write permission; existing files require their latest SHA256. Unknown write outcomes must be inspected before retry. Does not grant process execution.";
pub(crate) fn spec() -> Value {
    json!({"name":"agent_project_files","description":format!("{DESCRIPTION} Use list/read/write here; prefer read_files for batches, search_project_texts for search, and apply_text_edits for transactional edits. List is depth 1."),
        "inputSchema":{"type":"object","properties":{
            "action":{"enum":["list","read","write"]},"folder":{"type":"string"},
            "path":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500},
            "start_line":{"type":"integer","minimum":1},"content":{"type":"string","maxLength":262144},
            "expected_sha256":{"type":"string"}},"required":["action","folder"],"additionalProperties":false}})
}
pub(crate) fn tool_specs() -> Vec<Value> {
    use webcodex_tool_contracts::registry::input_schemas::*;
    [
        ("read_file", read_file_input_schema()),
        ("read_files", read_files_input_schema()),
        ("search_project_texts", search_project_texts_input_schema()),
        ("apply_text_edits", apply_text_edits_input_schema()),
    ].into_iter().map(|(name, mut schema)| {
        if let Some(properties) = schema["properties"].as_object_mut() {
            properties.remove("session_id");
        }
        json!({"name":name,"description":format!("{DESCRIPTION} Use the canonical {name} arguments with the exact project id; Workflow Session selectors are unavailable on this isolated surface."),
            "inputSchema":schema,"annotations":{"readOnlyHint":name != "apply_text_edits"}})
    }).collect()
}
pub(crate) async fn call_tool(
    runtime: &ToolRuntime,
    binding: &VerifiedAgentRun,
    name: &str,
    mut args: Value,
) -> ToolResult {
    if name == "agent_project_files" {
        if !matches!(args["action"].as_str(), Some("list" | "read" | "write")) {
            return denial("workspace_invalid_input");
        }
    } else {
        let Some(object) = args.as_object_mut() else {
            return denial("workspace_invalid_input");
        };
        if object.contains_key("folder") || object.contains_key("action") {
            return denial("workspace_invalid_input");
        }
        let Some(project) = object.remove("project") else {
            return denial("workspace_invalid_input");
        };
        object.insert("folder".into(), project);
        object.insert(
            "action".into(),
            json!(match name {
                "read_file" => "read",
                "read_files" => "read_files",
                "search_project_texts" => "search",
                "apply_text_edits" => "edit",
                _ => return denial("agent_tool_not_isolated"),
            }),
        );
    }
    call(runtime, binding, args).await
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
                && limit.is_none_or(|n| (1..=2000).contains(&n))
                && start_line.is_none_or(|n| n > 0)
        }
        Request::ReadFiles {
            items,
            max_result_bytes,
            ..
        } => {
            (1..=8).contains(&items.len())
                && items.iter().all(|i| {
                    !i.path.is_empty()
                        && i.path.len() <= 4096
                        && i.start_line.is_none_or(|n| n > 0)
                        && i.limit.is_none_or(|n| (1..=2000).contains(&n))
                })
                && valid_budget(*max_result_bytes)
        }
        Request::Search {
            queries,
            max_result_bytes,
            ..
        } => (1..=8).contains(&queries.len()) && valid_budget(*max_result_bytes),
        Request::Edit { changes, .. } => !changes.is_empty(),
        Request::Write { path, content, .. } => path.len() <= 4096 && content.len() <= 262144,
    };
    if !valid || request.folder().is_empty() || request.folder().len() > 256 {
        return denial("workspace_invalid_input");
    }
    let folder_id = request.folder().to_owned();
    let write = request.writes();
    let Some(db) = runtime.isolation_database() else {
        return denial("agent_isolation_unavailable");
    };
    let grant = match db.agent_project_files_grant(binding, write) {
        Ok(grant) => grant,
        Err(e) => return denial(e.code()),
    };
    let expected_root = match &grant {
        AgentProjectFilesGrant::Named(workspace) => {
            let Some(folder) = workspace.folders.iter().find(|f| f.project == folder_id) else {
                return denial("workspace_folder_not_authorized");
            };
            Some(folder.path.as_str())
        }
        AgentProjectFilesGrant::Registered { project } if project == &folder_id => None,
        _ => return denial("workspace_folder_not_authorized"),
    };
    let owner = match db.get_user_by_id(binding.owner_user()) {
        Ok(Some(u)) if !u.is_disabled() => u,
        _ => return denial("workspace_owner_unavailable"),
    };
    let resolved = match runtime.execution().resolve_project_input(&folder_id).await {
        Ok(p) if p.resolved_id == folder_id => p,
        _ => return denial("workspace_folder_unavailable"),
    };
    let project = &resolved.config;
    if expected_root.is_some_and(|root| root != project.path) {
        return denial("workspace_folder_changed");
    }
    if webcodex_core::sensitive_paths::is_secret_path(&project.path) {
        return denial("workspace_private_root_denied");
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
    if !db
        .agent_project_files_grant(binding, write)
        .is_ok_and(|current| current == grant)
    {
        return denial("workspace_folder_not_authorized");
    }
    let permission = if write {
        if !project.allow_patch {
            return denial("workspace_folder_read_only");
        }
        runtime.config.permission_evaluator.evaluate(
            if matches!(request, Request::Edit { .. }) {
                "apply_text_edits"
            } else {
                "write_project_file"
            },
            Some(&folder_id),
        )
    } else {
        None
    };
    if let Some(decision) = &permission {
        if !decision.allows_execution() {
            return super::permissions::permission_execution_denied_result(decision);
        }
    }
    let mut result = execute(runtime, &resolved, request).await;
    if let Some(permission) = permission {
        super::permissions::add_permission_to_result(&mut result, &permission);
    }
    result
}
fn valid_budget(budget: Option<usize>) -> bool {
    budget.is_none_or(|n| (1024..=262144).contains(&n))
}
async fn execute(
    runtime: &ToolRuntime,
    resolved: &ResolvedProject,
    request: Request,
) -> ToolResult {
    let project = &resolved.config;
    let folder = resolved.resolved_id.clone();
    match request {
        Request::List { path, limit, .. } => {
            runtime
                .project_overview_resolved(project, folder, path, Some(1), limit)
                .await
        }
        Request::Read {
            path,
            start_line,
            limit,
            with_line_numbers,
            ..
        } => {
            let projection = ReadModelProjection::Single {
                project: folder,
                path: path.clone(),
                session_id: None,
                with_line_numbers,
            };
            let mut result = runtime
                .read_one_resolved_project_file(
                    project,
                    path,
                    start_line,
                    limit,
                    with_line_numbers.unwrap_or(false),
                    tokio::time::Instant::now() + std::time::Duration::from_secs(32),
                )
                .await;
            super::read_files::add_actionable_read_continuations(&projection, &mut result);
            result
        }
        Request::ReadFiles {
            items,
            with_line_numbers,
            max_result_bytes,
            ..
        } => {
            let projection = ReadModelProjection::Batch {
                project: folder,
                items: items.clone(),
                session_id: None,
                with_line_numbers,
                max_result_bytes,
            };
            let mut result = runtime
                .read_files_resolved(resolved, items, with_line_numbers)
                .await;
            super::read_files::apply_model_facing_output_budget(
                &mut result,
                max_result_bytes,
                &projection,
            );
            super::read_files::add_actionable_read_continuations(&projection, &mut result);
            super::read_files::enforce_final_model_facing_hard_cap(&mut result, &projection);
            result
        }
        Request::Search {
            queries,
            max_result_bytes,
            ..
        } => {
            // Stable read-only search command; the model cannot choose a process or shell.
            let timeouts: Vec<_> = queries.iter().map(|q| q.timeout_secs.is_none()).collect();
            let mut result = runtime
                .search_project_texts_resolved(resolved, queries)
                .await;
            super::search_project_texts::apply_model_facing_output_budget(
                &mut result,
                &timeouts,
                max_result_bytes,
            );
            super::search_project_texts::enforce_final_model_facing_hard_cap(
                &mut result,
                &timeouts,
            );
            result
        }
        Request::Edit {
            changes, dry_run, ..
        } => {
            runtime
                .apply_resolved_text_edits(project, changes, dry_run)
                .await
        }
        Request::Write {
            path,
            content,
            expected_sha256,
            ..
        } => {
            let overwrite = expected_sha256.is_some();
            runtime
                .write_resolved_project_file(
                    project,
                    path,
                    content,
                    Some(overwrite),
                    expected_sha256,
                )
                .await
        }
    }
}
