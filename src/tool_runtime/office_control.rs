//! Human PAT Office operations: finite configuration/read operations only.
use super::{ToolResult, ToolRuntime};
use crate::auth::{AuthContext, AuthKind};
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_store::{NewAgentTask, TeamPolicyInput};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
enum OfficeRequest {
    PolicyGet {
        project: String,
    },
    PolicySave {
        project: String,
        expected_revision: i64,
        policy: TeamPolicyInput,
    },
    TaskCreate {
        project: String,
        title: String,
        instruction: String,
        idempotency_key: String,
    },
    PlanList {
        project: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "limit")]
        limit: usize,
    },
    PlanRead {
        project: String,
        root_task_id: String,
        #[serde(default)]
        attempts_offset: usize,
        #[serde(default = "limit")]
        attempts_limit: usize,
    },
}
fn limit() -> usize {
    20
}
fn denied(code: &str) -> ToolResult {
    ToolResult::err_with_output(
        code,
        json!({"error_kind":code,"state_changed":false,"dispatch_certainty":"not_started"}),
    )
}
fn serialized<T: serde::Serialize>(
    result: Result<T, webcodex_store::CommunicationStoreError>,
) -> ToolResult {
    match result {
        Err(error) => denied(error.code()),
        Ok(value) => match serde_json::to_value(value) {
            Ok(value) => ToolResult::ok(value),
            Err(_) => ToolResult::err_with_output(
                "office_result_unavailable",
                json!({"error_kind":"office_result_unavailable","state_changed":null,"dispatch_certainty":"unknown"}),
            ),
        },
    }
}

pub(crate) fn human_office_auth(auth: &AuthContext) -> bool {
    auth.kind == AuthKind::ApiToken
        && auth.token_kind.as_deref() == Some("user")
        && auth.agent_run.is_none()
        && !auth.is_bootstrap
        && auth.user_id.is_some()
}

impl ToolRuntime {
    pub(crate) async fn office_control(&self, auth: &AuthContext, body: Value) -> ToolResult {
        if !human_office_auth(auth) {
            return denied("office_human_pat_required");
        }
        let request: OfficeRequest = match serde_json::from_value(body) {
            Ok(value) => value,
            Err(_) => return denied("office_invalid_request"),
        };
        let (project, write) = match &request {
            OfficeRequest::PolicyGet { project }
            | OfficeRequest::PlanList { project, .. }
            | OfficeRequest::PlanRead { project, .. } => (project, false),
            OfficeRequest::PolicySave { project, .. }
            | OfficeRequest::TaskCreate { project, .. } => (project, true),
        };
        let required = if write {
            "project:write"
        } else {
            "project:read"
        };
        if !auth.has_scope(required) {
            return denied("office_scope_denied");
        }
        let resolved = match self
            .execution()
            .resolve_project_input_for_auth(project, Some(auth))
            .await
        {
            Ok(value) if &value.resolved_id == project => value,
            _ => return denied("office_project_unavailable"),
        };
        let principal = match super::communication::communication_principal(Some(auth)) {
            Ok(value) => value,
            Err(_) => return denied("office_human_pat_required"),
        };
        let Some(db) = self.communication.communication_db.as_ref() else {
            return denied("office_store_unavailable");
        };
        match request {
            OfficeRequest::PolicyGet { .. } => {
                serialized(db.get_team_policy(&principal, &resolved.resolved_id))
            }
            OfficeRequest::PolicySave {
                expected_revision,
                policy,
                ..
            } => serialized(db.save_team_policy(
                &principal,
                &resolved.resolved_id,
                expected_revision,
                policy,
            )),
            OfficeRequest::TaskCreate {
                title,
                instruction,
                idempotency_key,
                ..
            } => serialized(db.create_agent_task(
                &principal,
                NewAgentTask {
                    title,
                    instruction,
                    idempotency_key,
                    referenced_project_id: Some(resolved.resolved_id),
                    assignee_agent_id: None,
                    source_conversation_id: None,
                    source_message_id: None,
                },
            )),
            OfficeRequest::PlanList { offset, limit, .. } => {
                serialized(db.list_team_plans(&principal, &resolved.resolved_id, offset, limit))
            }
            OfficeRequest::PlanRead {
                root_task_id,
                attempts_offset,
                attempts_limit,
                ..
            } => match db.read_office_team_plan(
                &principal,
                &resolved.resolved_id,
                &root_task_id,
                attempts_offset,
                attempts_limit,
            ) {
                Ok(view) => {
                    let agents = match db.list_team_agent_attempts(
                        &principal,
                        &resolved.resolved_id,
                        Some(&root_task_id),
                        false,
                        100,
                    ) {
                        Ok(rows) => rows
                            .iter()
                            .map(super::team_agent::team_agent_projection)
                            .collect::<Vec<_>>(),
                        Err(error) => return denied(error.code()),
                    };
                    let fallbacks = match db.list_cli_fallbacks(&principal, &root_task_id) {
                        Ok(rows) => {
                            if rows.iter().any(|row| row.project != resolved.resolved_id) {
                                return denied("office_project_unavailable");
                            }
                            rows.iter()
                                .map(|row| {
                                    let mut value = super::cli_fallback::fallback_projection(row);
                                    value["policy_revision"] = json!(view.plan.policy.revision);
                                    value["blocked_reason"] = Value::Null;
                                    value
                                })
                                .collect::<Vec<_>>()
                        }
                        Err(_) => return denied("team_recovery_unavailable"),
                    };
                    ToolResult::ok(json!({"plan":view.plan,"completion":view.completion,
                        "task_summaries":view.tasks.into_iter().map(|task|json!({"task_id":task.task_id,"title":task.title,"state":task.state})).collect::<Vec<_>>(),
                        "attempts":{"attempts":view.receipts.receipts,"total_count":view.receipts.total_count,"next_offset":view.receipts.next_offset},
                        "agents":agents,"agents_may_be_truncated":agents.len()==100,"fallbacks":fallbacks,
                        "fallbacks_may_be_truncated":fallbacks.len()==100,"enforcement":"managed_tasks"}))
                }
                Err(error) => denied(error.code()),
            },
        }
    }
}
