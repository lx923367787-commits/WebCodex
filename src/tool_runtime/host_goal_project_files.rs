//! Immutable Goal files on the owning Runner. Absolute paths are presentation
//! metadata only: every I/O request retains the authorized Runner/root pair and
//! a project-relative path. Never interpret a Runner path on the Server disk.
use super::host_goal::failure;
use super::host_goal_files::{markdown, sha256, PlanArtifacts};
use super::project_resolution::ResolvedProject;
use super::{ToolCall, ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use serde_json::{json, Value};
use webcodex_core::host_goal::{
    normalize_host_goal_task_directory, HostGoalCheckpoint, HostGoalMaterialization,
    HostGoalPlanVersion, HostGoalRecord,
};

const MAX_TASK_FILE_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub(super) struct TaskFile {
    pub(super) relative_path: String,
    pub(super) absolute_path: String,
    pub(super) content: String,
    pub(super) sha256: String,
}

fn task_relative(goal: &HostGoalRecord) -> Result<String, ToolResult> {
    let value = goal.task_directory.as_deref().ok_or_else(|| {
        failure(
            "goal_task_file_legacy_location",
            "This historical Goal retains Server-side plan storage",
        )
    })?;
    let normalized = normalize_host_goal_task_directory(value)
        .map_err(|error| failure("goal_task_file_invalid_path", error))?;
    if goal.goal_id.is_empty()
        || goal.goal_id.len() > 128
        || !goal
            .goal_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err(failure(
            "goal_task_file_invalid_identity",
            "Goal file identity is invalid",
        ));
    }
    if goal.project_root.is_empty() || goal.project_root.contains('\0') {
        return Err(failure(
            "goal_task_file_invalid_path",
            "Pinned Runner root is invalid",
        ));
    }
    Ok(normalized)
}

fn under_task(task: &str, suffix: &str) -> String {
    if task == "." {
        suffix.into()
    } else {
        format!("{task}/{suffix}")
    }
}

fn runner_absolute(root: &str, relative: &str) -> String {
    let normalized = root.replace('\\', "/");
    let root = normalized.trim_end_matches('/');
    if relative.is_empty() || relative == "." {
        if root.is_empty() {
            "/".into()
        } else {
            root.into()
        }
    } else {
        format!("{root}/{relative}")
    }
}

/// Null preserves the historical location contract; callers expose this only
/// for Goals whose task directory has been explicitly persisted.
pub(crate) fn task_paths(goal: &HostGoalRecord) -> Value {
    let Ok(task) = task_relative(goal) else {
        return Value::Null;
    };
    json!({"storage":"runner_project",
        "task_directory":runner_absolute(&goal.project_root,&task),
        "task_relative_directory":task,
        "goals_directory":runner_absolute(&goal.project_root,&under_task(&task,"goals")),
        "handoff_directory":runner_absolute(&goal.project_root,&under_task(&task,"handoff"))})
}

fn task_file(goal: &HostGoalRecord, suffix: &str, content: String) -> Result<TaskFile, ToolResult> {
    let relative_path = under_task(&task_relative(goal)?, suffix);
    webcodex_core::goal::validate_goal_relative_path(&relative_path)
        .and_then(|_| super::files::validate_artifact_file_path(&relative_path))
        .map_err(|error| failure("goal_task_file_invalid_path", error))?;
    if content.len() > MAX_TASK_FILE_BYTES || content.contains('\0') {
        return Err(failure(
            "goal_task_file_too_large",
            "Each Goal file must be NUL-free UTF-8 within 256 KiB",
        ));
    }
    Ok(TaskFile {
        absolute_path: runner_absolute(&goal.project_root, &relative_path),
        relative_path,
        sha256: sha256(content.as_bytes()),
        content,
    })
}

pub(super) fn plan_files(
    goal: &HostGoalRecord,
    plan: &HostGoalPlanVersion,
) -> Result<[TaskFile; 2], ToolResult> {
    if plan.goal_id != goal.goal_id || plan.version == 0 {
        return Err(failure(
            "goal_task_file_invalid_identity",
            "Saved plan belongs to another Goal or version",
        ));
    }
    let value = json!(&plan.plan);
    let mut json_text = serde_json::to_string_pretty(&value)
        .map_err(|error| failure("goal_task_file_serialization_failed", error.to_string()))?;
    json_text.push('\n');
    let directory = format!("goals/{}/plans/v{}", goal.goal_id, plan.version);
    Ok([
        task_file(goal, &format!("{directory}/plan.json"), json_text)?,
        task_file(
            goal,
            &format!("{directory}/plan.md"),
            markdown(&value, &goal.goal_id, plan.version, &plan.hash),
        )?,
    ])
}

pub(super) fn handoff_files(
    goal: &HostGoalRecord,
    checkpoint: &HostGoalCheckpoint,
) -> Result<[TaskFile; 2], ToolResult> {
    if checkpoint.goal_id != goal.goal_id
        || checkpoint.sequence == 0
        || checkpoint.action.as_deref() != Some("handoff")
    {
        return Err(failure(
            "goal_task_file_invalid_checkpoint",
            "Export requires the exact committed handoff checkpoint for this Goal",
        ));
    }
    let paths = task_paths(goal);
    let value = json!({"schema_version":1,"goal_id":checkpoint.goal_id,"project":goal.project,
        "task_directory":paths["task_directory"],"task_relative_directory":paths["task_relative_directory"],
        "checkpoint":checkpoint});
    let mut json_text = serde_json::to_string_pretty(&value)
        .map_err(|error| failure("goal_task_file_serialization_failed", error.to_string()))?;
    json_text.push('\n');
    let state_text = serde_json::to_string_pretty(&checkpoint.state)
        .map_err(|error| failure("goal_task_file_serialization_failed", error.to_string()))?;
    let markdown_text = format!("# Goal 交接快照\n\nGoal：{}\n\n项目：{}\n\n任务目录：{}\n\n检查点：{}；修订：{}；计划版本：{}\n\n源上下文：{}；来源：{}；源 fence：{}\n\n原因：{}\n\n此文件只记录已提交的检查点，不授予旧上下文继续执行的权限。恢复时核验同一 Goal 的当前状态，保留原 Job 和审核证据，再认领新 attempt。\n\n## 检查点保存的完整状态\n\n```json\n{}\n```\n",
        checkpoint.goal_id,goal.project,paths["task_directory"].as_str().unwrap_or(""),checkpoint.sequence,
        checkpoint.revision,checkpoint.plan_version,checkpoint.source_context_epoch.as_deref().unwrap_or("未记录"),
        checkpoint.identity_source.as_deref().unwrap_or("未记录"),checkpoint.source_fence.map(|value|value.to_string()).unwrap_or_else(||"未记录".into()),
        checkpoint.reason,state_text);
    let directory = format!(
        "handoff/{}/checkpoint-{}",
        goal.goal_id, checkpoint.sequence
    );
    Ok([
        task_file(goal, &format!("{directory}/handoff.json"), json_text)?,
        task_file(goal, &format!("{directory}/handoff.md"), markdown_text)?,
    ])
}

pub(super) fn metadata_fingerprint(
    path: &str,
    value: &Value,
) -> Result<Option<String>, ToolResult> {
    if value.get("error").is_some_and(|error| !error.is_null()) {
        return Err(failure(
            "goal_task_file_read_failed",
            "Runner could not inspect this Goal file",
        ));
    }
    if value["path"].as_str() != Some(path) {
        return Err(failure(
            "goal_task_file_invalid_metadata",
            "Runner metadata belongs to another file",
        ));
    }
    if value["exists"] == false && value["missing"] == true {
        return Ok(None);
    }
    let hash = value["sha256"]
        .as_str()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if value["exists"] != true
        || value["missing"] != false
        || hash.is_none()
        || value["bytes"]
            .as_u64()
            .is_none_or(|bytes| bytes > MAX_TASK_FILE_BYTES as u64)
    {
        return Err(failure(
            "goal_task_file_invalid_metadata",
            "Runner must return an actual bounded file SHA or explicit verified absence",
        ));
    }
    Ok(hash.map(str::to_owned))
}

impl ToolRuntime {
    async fn resolve_goal_file_target(
        &self,
        goal: &HostGoalRecord,
        files: &[TaskFile],
        write: bool,
        auth: Option<&AuthContext>,
    ) -> Result<ResolvedProject, ToolResult> {
        for scope in if write {
            &["project:read", "project:write"][..]
        } else {
            &["project:read"][..]
        } {
            if auth.is_some_and(|auth| !auth.has_scope(scope)) {
                return Err(failure(
                    "goal_task_file_scope_required",
                    format!("Goal file access requires {scope}"),
                ));
            }
        }
        let resolved = self
            .execution()
            .resolve_project_input_for_auth(&goal.project, auth)
            .await
            .map_err(|error| error.into_tool_result())?;
        if resolved.resolved_id != goal.project || resolved.config.path != goal.project_root {
            return Err(failure(
                "goal_task_file_project_changed",
                "The current authorized project no longer has the Goal's exact pinned Runner root",
            ));
        }
        // Authorize both the verification reads and each eventual write using
        // its own actual path, without resolving the project again.
        let pinned = Ok(resolved.clone());
        for file in files {
            let read = ToolCall::ReadProjectArtifactMetadata {
                project: goal.project.clone(),
                path: file.relative_path.clone(),
                session_id: None,
                allow_missing: Some(true),
            };
            self.authorize_runner_tool(&read, None, auth, Some(&pinned))
                .await?;
            if write {
                let write_call = ToolCall::WriteProjectFile {
                    project: goal.project.clone(),
                    path: file.relative_path.clone(),
                    content: String::new(),
                    session_id: None,
                    overwrite: Some(false),
                    expected_sha256: None,
                };
                self.authorize_runner_tool(&write_call, None, auth, Some(&pinned))
                    .await?;
            }
        }
        Ok(resolved)
    }

    async fn goal_file_fingerprint_at(
        &self,
        target: &ResolvedProject,
        path: &str,
    ) -> Result<Option<String>, ToolResult> {
        let value = self
            .run_runner_json_file_op(
                target.config.client_id.clone(),
                target.config.path.clone(),
                path.into(),
                "read_project_artifact_metadata",
                json!({"path":path,"allow_missing":true,"max_bytes":MAX_TASK_FILE_BYTES}),
                "goal_task_file_metadata",
            )
            .await
            .map_err(|error| failure("goal_task_file_read_failed", error))?;
        metadata_fingerprint(path, &value)
    }

    async fn publish_goal_file_pair(
        &self,
        target: &ResolvedProject,
        files: &[TaskFile; 2],
    ) -> Result<(), ToolResult> {
        // Detect either conflicting historical file before creating its peer.
        let mut absent = Vec::new();
        for file in files {
            match self
                .goal_file_fingerprint_at(target, &file.relative_path)
                .await?
            {
                Some(hash) if hash == file.sha256 => {}
                Some(_) => {
                    return Err(failure(
                        "goal_task_file_conflict",
                        format!(
                            "{} already contains different bytes; preserved without overwrite",
                            file.relative_path
                        ),
                    ))
                }
                None => absent.push(file),
            }
        }
        for file in absent {
            let write = self
                .run_runner_json_file_op(
                    target.config.client_id.clone(),
                    target.config.path.clone(),
                    file.relative_path.clone(),
                    "write_project_file",
                    json!({"path":file.relative_path,"content":file.content,"overwrite":false}),
                    "goal_task_file_publish",
                )
                .await;
            // A lost reply is reconciled by the actual file, never by issuing
            // an overwrite or treating a producer's success flag as proof.
            match self
                .goal_file_fingerprint_at(target, &file.relative_path)
                .await
            {
                Ok(Some(hash)) if hash == file.sha256 => {}
                Ok(Some(_)) => {
                    return Err(failure(
                        "goal_task_file_conflict",
                        format!(
                            "{} differs after publication; preserved without overwrite",
                            file.relative_path
                        ),
                    ))
                }
                Ok(None) => {
                    return Err(failure(
                        "goal_task_file_publish_failed",
                        format!(
                            "{} was not published; retry this exact saved plan or checkpoint",
                            file.relative_path
                        ),
                    ))
                }
                Err(mut error) => {
                    error.output["recovery_kind"] = json!("reconcile");
                    error.output["failure_kind"] = json!("outcome_unknown");
                    return Err(error);
                }
            }
            // The actual SHA proves publication even if its transport reply
            // was lost. Never trust or replay an uncertain write response.
            drop(write);
        }
        self.verify_goal_file_pair(target, files).await
    }

    async fn verify_goal_file_pair(
        &self,
        target: &ResolvedProject,
        files: &[TaskFile; 2],
    ) -> Result<(), ToolResult> {
        for file in files {
            if self
                .goal_file_fingerprint_at(target, &file.relative_path)
                .await?
                .as_deref()
                != Some(file.sha256.as_str())
            {
                return Err(failure(
                    "goal_task_file_changed",
                    format!(
                        "{} is missing or changed; the immutable file has not been verified",
                        file.relative_path
                    ),
                ));
            }
        }
        Ok(())
    }

    pub(crate) async fn materialize_project_goal_plan(
        &self,
        goal: &HostGoalRecord,
        plan: &HostGoalPlanVersion,
        auth: Option<&AuthContext>,
    ) -> Result<PlanArtifacts, ToolResult> {
        let files = plan_files(goal, plan)?;
        let target = self
            .resolve_goal_file_target(goal, &files, true, auth)
            .await?;
        self.publish_goal_file_pair(&target, &files).await?;
        Ok(PlanArtifacts {
            json_path: files[0].absolute_path.clone(),
            json_sha256: files[0].sha256.clone(),
            markdown_path: files[1].absolute_path.clone(),
            markdown_sha256: files[1].sha256.clone(),
        })
    }

    pub(crate) async fn verify_project_goal_plan(
        &self,
        goal: &HostGoalRecord,
        plan: &HostGoalPlanVersion,
        receipt: &HostGoalMaterialization,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        let files = plan_files(goal, plan)?;
        if receipt.json_path != files[0].absolute_path
            || receipt.json_hash != files[0].sha256
            || receipt.markdown_path != files[1].absolute_path
            || receipt.markdown_hash != files[1].sha256
        {
            return Err(failure(
                "goal_task_file_receipt_mismatch",
                "Plan receipt belongs to another exact task path, content or immutable version",
            ));
        }
        let target = self
            .resolve_goal_file_target(goal, &files, false, auth)
            .await?;
        self.verify_goal_file_pair(&target, &files).await
    }

    pub(crate) async fn export_project_goal_handoff(
        &self,
        goal: &HostGoalRecord,
        checkpoint: &HostGoalCheckpoint,
        auth: Option<&AuthContext>,
    ) -> Result<Value, ToolResult> {
        let files = handoff_files(goal, checkpoint)?;
        let target = self
            .resolve_goal_file_target(goal, &files, true, auth)
            .await?;
        self.publish_goal_file_pair(&target, &files).await?;
        Ok(
            json!({"checkpoint_sequence":checkpoint.sequence,"json_path":files[0].absolute_path,"json_sha256":files[0].sha256,
            "markdown_path":files[1].absolute_path,"markdown_sha256":files[1].sha256,"task_files":task_paths(goal)}),
        )
    }

    /// A read-only readiness probe on the exact immutable handoff files. Never
    /// publish or repair files on behalf of an uncommitted successor.
    pub(crate) async fn verify_project_goal_handoff(
        &self,
        goal: &HostGoalRecord,
        checkpoint: &HostGoalCheckpoint,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        let files = handoff_files(goal, checkpoint)?;
        let target = self
            .resolve_goal_file_target(goal, &files, false, auth)
            .await?;
        self.verify_goal_file_pair(&target, &files).await
    }
}
