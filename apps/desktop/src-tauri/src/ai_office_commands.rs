use crate::error::DesktopError;
use crate::state::{ai_office::*, AppState};
use serde::Deserialize;
use tauri::State;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRequest {
    pub project_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavePolicyRequest {
    pub project_id: String,
    pub expected_revision: u64,
    pub policy: OfficePolicyInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluatePlanRequest {
    pub project_id: String,
    pub expected_revision: u64,
    pub plan: OfficePlan,
}

#[tauri::command]
pub async fn get_ai_office_snapshot(
    state: State<'_, AppState>,
    request: SnapshotRequest,
) -> Result<OfficeSnapshot, DesktopError> {
    state.ai_office_snapshot(request.project_id).await
}

#[tauri::command]
pub async fn save_ai_office_policy(
    state: State<'_, AppState>,
    request: SavePolicyRequest,
) -> Result<OfficePolicy, DesktopError> {
    state
        .save_ai_office_policy(
            request.project_id,
            request.expected_revision,
            request.policy,
        )
        .await
}

#[tauri::command]
pub async fn evaluate_ai_office_plan(
    state: State<'_, AppState>,
    request: EvaluatePlanRequest,
) -> Result<OfficePlanEvaluation, DesktopError> {
    state
        .evaluate_ai_office_plan(request.project_id, request.expected_revision, request.plan)
        .await
}

#[tauri::command]
pub async fn ai_office_team_call(
    state: State<'_, AppState>,
    action: String,
    params: serde_json::Value,
) -> Result<serde_json::Value, DesktopError> {
    state.ai_office_team_call(&action, params).await
}
