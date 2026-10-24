//! Finite human Office bridge for configuration, reads and public task creation.
//! Saved credentials never move into the frontend or browser preview.
use super::*;
use crate::models::RegisteredProject;
use crate::webcodex::RuntimeConnection;

const MAX_TEAM_INPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TeamOfficeAction {
    PolicyGet,
    PolicySave,
    PlanList,
    PlanRead,
    TaskCreate,
}

impl TeamOfficeAction {
    pub(crate) fn parse(value: &str) -> DesktopResult<Self> {
        Ok(match value {
            "policy-get" => Self::PolicyGet,
            "policy-save" => Self::PolicySave,
            "plan-list" => Self::PlanList,
            "plan-read" => Self::PlanRead,
            "task-create" => Self::TaskCreate,
            "plan-register" | "plan-cancel" | "cli-start" | "cli-observe" | "agent-start"
            | "agent-observe" => {
                return Err(bridge_error(
                    "ai_office_action_unavailable",
                    "此人类入口仅支持策略、计划查询及公开任务创建；取消和执行入口尚未接入。",
                ))
            }
            _ => {
                return Err(bridge_error(
                    "ai_office_invalid_action",
                    "不支持的团队操作。",
                ))
            }
        })
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PolicyGet => "policy-get",
            Self::PolicySave => "policy-save",
            Self::PlanList => "plan-list",
            Self::PlanRead => "plan-read",
            Self::TaskCreate => "task-create",
        }
    }

    fn parameter_names(self) -> &'static [&'static str] {
        match self {
            Self::PolicyGet => &["project"],
            Self::PolicySave => &["project", "expected_revision", "policy"],
            Self::PlanList => &["project", "offset", "limit"],
            Self::PlanRead => &[
                "project",
                "root_task_id",
                "attempts_offset",
                "attempts_limit",
            ],
            Self::TaskCreate => &["project", "title", "instruction", "idempotency_key"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TeamConnection {
    binary: PathBuf,
    connection: RuntimeConnection,
    client_id: String,
}

pub(super) fn bridge_error(code: &str, message: &str) -> DesktopError {
    DesktopError::new(
        code,
        message,
        "刷新项目和任务状态后再操作；结果未知时不要自动重复派发。",
    )
}

fn connection(core: &DesktopCore) -> DesktopResult<TeamConnection> {
    let connection = project_management::connection_from_config(&core.config).ok_or_else(|| {
        bridge_error(
            "runtime_not_ready",
            "连接不可用，无法读取或保存 Server 团队策略。",
        )
    })?;
    let client_id = stored_runner_client_id(&core.config)
        .ok_or_else(|| bridge_error("runtime_not_ready", "保存的 Runner 身份不可用。"))?;
    Ok(TeamConnection {
        binary: core.adapter.binaries()?.webcodex.clone(),
        connection,
        client_id,
    })
}

fn validate_params(
    action: TeamOfficeAction,
    client_id: &str,
    params: &Value,
) -> DesktopResult<String> {
    let object = params
        .as_object()
        .ok_or_else(|| bridge_error("ai_office_invalid_params", "团队操作需要 JSON 参数对象。"))?;
    if object
        .keys()
        .any(|key| !action.parameter_names().contains(&key.as_str()))
    {
        return Err(bridge_error(
            "ai_office_invalid_params",
            "参数不属于当前人类操作；不能覆盖工具、身份、项目引用或连接凭据。",
        ));
    }
    let project = object
        .get("project")
        .and_then(Value::as_str)
        .ok_or_else(|| bridge_error("ai_office_invalid_project", "团队操作缺少项目身份。"))?;
    let prefix = format!("agent:{client_id}:");
    if !project.starts_with(&prefix)
        || project.len() <= prefix.len()
        || project.len() > 256
        || project.trim() != project
        || project.chars().any(char::is_control)
    {
        return Err(bridge_error(
            "project_connection_changed",
            "项目不属于当前保存的 Runner 连接。",
        ));
    }
    if serde_json::to_vec(params).map_or(true, |bytes| bytes.len() > MAX_TEAM_INPUT_BYTES) {
        return Err(bridge_error(
            "ai_office_invalid_params",
            "团队操作参数超过大小限制。",
        ));
    }
    Ok(project.to_string())
}

fn require_registered(project: &str, projects: &[RegisteredProject]) -> DesktopResult<()> {
    if projects.iter().any(|row| row.id == project) {
        Ok(())
    } else {
        Err(bridge_error(
            "ai_office_unknown_project",
            "项目未出现在当前 Runner 的真实登记目录中，请刷新项目列表。",
        ))
    }
}

pub(super) fn successful_team_output(envelope: Value) -> DesktopResult<Value> {
    match envelope.get("success").and_then(Value::as_bool) {
        Some(true) => envelope
            .get("output")
            .cloned()
            .ok_or_else(|| bridge_error("webcodex_contract_invalid", "Server 团队响应缺少结果。")),
        Some(false) => {
            let code = envelope
                .pointer("/output/error_kind")
                .and_then(Value::as_str)
                .filter(|code| {
                    code.len() <= 128
                        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
                .unwrap_or("ai_office_team_rejected");
            let message = match code {
                "team_policy_revision_conflict" => {
                    "Server 团队策略已更新，请载入最新版本后再编辑。"
                }
                "team_task_not_found" => "Server 未找到当前账户可以访问的团队任务。",
                _ => "Server 未接受这次团队操作，请检查项目权限和最新任务状态。",
            };
            Err(bridge_error(code, message))
        }
        None => Err(bridge_error(
            "webcodex_contract_invalid",
            "Server 团队响应格式无效。",
        )),
    }
}

impl AppState {
    pub(super) async fn team_connection(&self) -> DesktopResult<TeamConnection> {
        let slot = self.core.lock().await;
        connection(slot.as_ref().ok_or_else(|| {
            bridge_error("desktop_operation_busy", "桌面端正在处理另一个连接操作。")
        })?)
    }

    pub(super) async fn verify_team_connection(
        &self,
        expected: &TeamConnection,
    ) -> DesktopResult<()> {
        if self
            .team_connection()
            .await
            .as_ref()
            .is_ok_and(|current| current == expected)
        {
            Ok(())
        } else {
            Err(bridge_error(
                "project_connection_changed",
                "团队请求期间连接发生变化；请重新观察原项目，确认结果后再继续。",
            ))
        }
    }

    pub async fn ai_office_team_call(&self, action: &str, params: Value) -> DesktopResult<Value> {
        let action = TeamOfficeAction::parse(action)?;
        let captured = self.team_connection().await?;
        let project = validate_params(action, &captured.client_id, &params)?;
        let cancellation =
            CancellationContext::new(CancellationSignal::new(), self.shutdown_signal.clone());
        let projects = WebCodexAdapter::list_registry_with_binary(
            &captured.binary,
            &captured.connection,
            &captured.client_id,
            &cancellation,
        )
        .await?;
        require_registered(&project, &projects)?;
        self.verify_team_connection(&captured).await?;
        let result = WebCodexAdapter::team_office_call(
            &captured.binary,
            &captured.connection,
            &captured.client_id,
            action,
            &params,
            &cancellation,
        )
        .await;
        // Even errors may follow a committed mutation; never retarget a response
        // to the connection selected while the request was in flight.
        self.verify_team_connection(&captured).await?;
        result
    }
}

#[cfg(test)]
#[path = "team_bridge_tests.rs"]
mod tests;
