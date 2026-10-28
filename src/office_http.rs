//! Dedicated human Office control route. Model credentials cannot use it.
use crate::action_audit::{ActionAudit, ActionAuditRecord};
use crate::auth::AuthContext;
use crate::tool_runtime::ToolRuntime;
use salvo::prelude::*;
use serde_json::json;
use std::sync::Arc;

#[handler]
pub(crate) async fn control(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(auth) = depot.obtain::<AuthContext>().ok().cloned() else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Json(json!({"error":"office_human_pat_required"})));
        return;
    };
    if !crate::tool_runtime::office_control::human_office_auth(&auth) {
        res.status_code(StatusCode::FORBIDDEN);
        res.render(Json(json!({"error":"office_human_pat_required"})));
        return;
    }
    if let Err((status, code, _)) = crate::auth::require_json_same_origin(req) {
        res.status_code(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST));
        res.render(Json(json!({"error":code})));
        return;
    }
    let body: serde_json::Value = match req.payload().await {
        Ok(bytes) if bytes.len() <= 64 * 1024 => match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(_) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(json!({"error":"office_invalid_request"})));
                return;
            }
        },
        _ => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({"error":"office_invalid_request"})));
            return;
        }
    };
    let Some(runtime) = depot.obtain::<Arc<ToolRuntime>>().ok().cloned() else {
        res.status_code(StatusCode::SERVICE_UNAVAILABLE);
        res.render(Json(json!({"error":"office_runtime_unavailable"})));
        return;
    };
    let operation = match body.get("action").and_then(serde_json::Value::as_str) {
        Some("policy-get") => "office_policy_get",
        Some("policy-save") => "office_policy_save",
        Some("plan-list") => "office_plan_list",
        Some("plan-read") => "office_plan_read",
        Some("task-create") => "office_task_create",
        Some("cli-discover") => "office_cli_discover",
        Some("cli-models") => "office_cli_models",
        _ => "office_invalid_action",
    };
    let audit = ActionAudit::start(req, depot, "/api/office/control", "officeControl");
    let result = runtime.office_control(&auth, body).await;
    let forbidden = matches!(
        result.error.as_deref(),
        Some("office_scope_denied" | "office_project_unavailable" | "office_human_pat_required")
    );
    let status = if result.success {
        StatusCode::OK
    } else if forbidden {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::BAD_REQUEST
    };
    audit.record(
        ActionAuditRecord::new(operation.to_string(), result.success, status)
            .error(result.error.clone())
            .summary(json!({"operation":operation,"success":result.success})),
    );
    res.status_code(status);
    res.headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    res.render(Json(result));
}
