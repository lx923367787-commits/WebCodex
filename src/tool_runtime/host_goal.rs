//! Goal orchestration for an explicitly participating ChatGPT conversation.
//! Plan publication and the user's subsequent instruction are separate steps.
use super::coding_agent::goal_owner_key;
use super::host_goal_context as context;
use super::host_goal_files::{self as files, PlanArtifacts};
use super::{ToolCall, ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_core::host_goal::*;
use webcodex_store::{HostGoalCheckpointAction, HostGoalWindowBinding};

#[path = "host_goal_execution_refs.rs"]
pub(super) mod execution_refs;
#[path = "host_goal_handoff.rs"]
mod handoff;
#[path = "host_goal_projection.rs"]
pub(super) mod projection;

pub(crate) fn failure(code: &str, message: impl Into<String>) -> ToolResult {
    ToolResult::err_with_output(message, json!({"error_kind":code}))
}
pub(crate) fn store_error(error: webcodex_store::GoalStoreError) -> ToolResult {
    failure(error.code(), error.message())
}
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub(crate) fn bounded(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].into()
}
pub(crate) fn owner(auth: Option<&AuthContext>) -> Result<String, ToolResult> {
    goal_owner_key(auth).map_err(|e| failure("goal_identity_unavailable", e))
}
fn scope(auth: Option<&AuthContext>, name: &str) -> Result<(), ToolResult> {
    if auth.is_some_and(|auth| auth.has_scope(name)) {
        Ok(())
    } else {
        Err(failure(
            "insufficient_scope",
            format!("Missing required scope: {name}"),
        ))
    }
}
fn artifacts(value: &HostGoalMaterialization) -> PlanArtifacts {
    PlanArtifacts {
        json_path: value.json_path.clone(),
        json_sha256: value.json_hash.clone(),
        markdown_path: value.markdown_path.clone(),
        markdown_sha256: value.markdown_hash.clone(),
    }
}
pub(crate) fn is_host_goal_control(tool: &str) -> bool {
    matches!(
        tool,
        "goal_mode_start"
            | "goal_plan_save"
            | "goal_confirm_plan"
            | "goal_bootstrap"
            | "goal_claim_node"
            | "goal_checkpoint"
            | "goal_check_node"
            | "goal_read"
            | "goal_list"
    )
}

/// Goal planning gates implementation, not inspection. Document reads use a
/// managed worker/Job, so their execution permissions remain consequential even
/// though they do not require an approved implementation plan.
pub(crate) fn requires_confirmed_plan(call: &ToolCall) -> bool {
    match call {
        ToolCall::PresentDocument(_) => false,
        // Resume observes an existing Job; document argument validation rejects
        // mixing job_id with fields that could start another operation.
        ToolCall::DocumentEdit(args)
        | ToolCall::DocumentConvert(args)
        | ToolCall::DocumentRestore(args)
            if args.job_id.is_some() =>
        {
            false
        }
        ToolCall::DocumentRestore(args) if args.backup_id.is_none() => false,
        _ => !super::metadata::tool_metadata(call.tool_name())
            .effect
            .read_only_hint(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointPatch {
    state: Option<Value>,
    reason: String,
    #[serde(default = "checkpoint_action")]
    action: String,
    #[serde(default)]
    operation_resolutions: Vec<OperationResolution>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationResolution {
    operation_id: String,
    status: HostGoalOperationStatus,
    evidence: String,
    source_refs: Vec<String>,
    user_confirmation: String,
}
fn checkpoint_action() -> String {
    "checkpoint".into()
}

fn merge_checkpoint_state(
    current: &HostGoalState,
    patch: Option<&Value>,
) -> Result<HostGoalState, ToolResult> {
    let Some(patch) = patch else {
        return Ok(current.clone());
    };
    let fields = patch.as_object().ok_or_else(|| {
        failure(
            "invalid_goal_checkpoint",
            "state must be a field-level object patch",
        )
    })?;
    let mut state = json!(current);
    for (key, value) in fields {
        if key == "facts" {
            let facts = value.as_object().ok_or_else(|| {
                failure(
                    "invalid_goal_checkpoint",
                    "facts must be an object of string values",
                )
            })?;
            for (fact, value) in facts {
                if value.is_null() {
                    if let Some(facts) = state["facts"].as_object_mut() {
                        facts.remove(fact);
                    }
                    continue;
                }
                if !value.is_string() {
                    return Err(failure(
                        "invalid_goal_checkpoint",
                        "Each fact value must be a string",
                    ));
                }
                state["facts"][fact] = value.clone();
            }
        } else {
            state[key] = value.clone();
        }
    }
    let state: HostGoalState = serde_json::from_value(state)
        .map_err(|e| failure("invalid_goal_checkpoint", e.to_string()))?;
    state
        .validate()
        .map_err(|e| failure("invalid_goal_checkpoint", e))?;
    Ok(state)
}

impl ToolRuntime {
    /// The independent MCP forwarding gateway cannot supply a verified effect
    /// classification to the goal controller. Never let it bypass a selected mode.
    pub(crate) fn independent_gateway_goal_denial(
        &self,
        arguments: &Value,
        auth: Option<&AuthContext>,
        window: Option<&crate::client_window::ClientWindow>,
    ) -> Option<ToolResult> {
        let deny = || {
            failure("goal_gateway_effect_unknown","This gateway cannot verify a goal-bound effect. Use an admitted runtime tool with goal_context, or explicitly exit goal mode.")
        };
        if arguments.get("goal_context").is_some() {
            return Some(deny());
        }
        let mode_key = window
            .filter(|w| matches!(w.source(), "openai-session" | "openai-conversation"))
            .map(|window| window.key().to_string())
            .or_else(|| {
                arguments["context_epoch"]
                    .as_str()
                    .map(context::explicit_mode_key)
            })?;
        let principal = owner(auth).ok()?;
        let db = self.goals.store()?;
        match db.host_goal_window(&principal, &mode_key) {
            Ok(None) => None,
            Ok(Some(_)) | Err(_) => Some(deny()),
        }
    }
    /// Shared by static dispatch and effect-resolved plugin/SSH governance.
    /// Called only after their ordinary authority checks have succeeded.
    pub(crate) async fn host_goal_admit_effect(
        &self,
        tool: &str,
        project: Option<&str>,
        payload: &Value,
        auth: Option<&AuthContext>,
        consequential: bool,
    ) -> Result<(), ToolResult> {
        if !consequential || is_host_goal_control(tool) {
            return Ok(());
        }
        let principal = match owner(auth) {
            Ok(owner) => owner,
            Err(_) if context::current_binding().is_none() => return Ok(()),
            Err(e) => return Err(e),
        };
        let Some(db) = self.goals.store() else {
            return if context::current_binding().is_some() {
                Err(failure(
                    "goal_store_unavailable",
                    "Goal execution requires durable storage",
                ))
            } else {
                Ok(())
            };
        };
        let window_binding = if let Some(window) = context::current_mode_key() {
            db.host_goal_window(&principal, &window)
                .map_err(|e| failure("goal_binding_unavailable", e.to_string()))?
        } else {
            None
        };
        let call_context = context::current_binding();
        // A retired host identity remains retired even after its old mode was
        // cleared. Stop/cancel is an effect too, so it must not return early.
        let retired = if let Some(epoch) = context::current_epoch() {
            db.read_host_context_state(&principal, &epoch)
                .map_err(store_error)?
                .is_some_and(|state| state.state == "retired")
        } else {
            false
        };
        if window_binding.is_some() || call_context.is_some() || retired {
            self.host_goal_context_gate(
                &principal,
                matches!(tool, "stop_job" | "coding_agent_cancel"),
            )?;
        }
        if window_binding.is_none() && call_context.is_none() {
            return Ok(());
        }
        if matches!(tool, "goal_start" | "goal_resume") {
            return Err(failure("goal_mode_conflict", "A ChatGPT-controlled goal cannot start or resume a separate ACP goal. Continue the current node or explicitly exit goal mode first."));
        }
        if tool == "coding_agent_cancel" {
            let id = window_binding
                .as_ref()
                .map(|binding| binding.goal_id.as_str())
                .or_else(|| {
                    call_context
                        .as_ref()
                        .map(|binding| binding.goal_id.as_str())
                })
                .ok_or_else(|| {
                    failure(
                        "goal_context_required",
                        "Select the goal whose native Run is being cancelled",
                    )
                })?;
            if call_context
                .as_ref()
                .is_some_and(|binding| binding.goal_id != id)
            {
                return Err(failure(
                    "goal_context_mismatch",
                    "The cancel request names another goal",
                ));
            }
            let params = payload.get("params").unwrap_or(payload);
            let run_id = params["run_id"].as_str().ok_or_else(|| {
                failure(
                    "invalid_goal_run",
                    "coding_agent_cancel requires its exact recorded Run ID",
                )
            })?;
            context::track_active_operation(id, &format!("cancel_{run_id}"))
                .map_err(|e| failure("goal_operation_tracking_unavailable", e))?;
            self.host_goal_context_gate(&principal, true)?;
            return self
                .host_goal_admit_native_cancel(id, run_id, project, auth)
                .await;
        }
        // Stopping an already tracked Job is recovery, not a new node effect.
        // It must remain possible while a goal is paused or its plan is damaged.
        if tool == "stop_job" {
            let id = window_binding
                .as_ref()
                .map(|binding| binding.goal_id.as_str())
                .or_else(|| {
                    call_context
                        .as_ref()
                        .map(|binding| binding.goal_id.as_str())
                })
                .ok_or_else(|| {
                    failure(
                        "goal_context_required",
                        "Select the goal whose Job is being stopped",
                    )
                })?;
            if call_context
                .as_ref()
                .is_some_and(|binding| binding.goal_id != id)
            {
                return Err(failure(
                    "goal_context_mismatch",
                    "The stop request names another goal",
                ));
            }
            let goal = self.host_goal_access(id, auth, false).await?;
            if project.is_some_and(|project| project != goal.project) {
                return Err(failure(
                    "goal_project_mismatch",
                    "Stop the Job through its exact goal project",
                ));
            }
            let params = payload.get("params").unwrap_or(payload);
            let job_id = params["job_id"].as_str().ok_or_else(|| {
                failure(
                    "invalid_goal_job",
                    "stop_job requires its exact recorded Job ID",
                )
            })?;
            if db
                .find_host_goal_operation_by_job(&principal, id, job_id)
                .map_err(store_error)?
                .is_none()
            {
                return Err(failure(
                    "goal_job_mismatch",
                    "This Job is not recorded in the selected goal",
                ));
            }
            // The concrete stop_job implementation still enforces its original
            // user confirmation, Job ownership, target and process-tree policy.
            context::track_active_operation(id, &format!("stop_{job_id}"))
                .map_err(|e| failure("goal_operation_tracking_unavailable", e))?;
            self.host_goal_context_gate(&principal, true)?;
            return Ok(());
        }
        let call_context=call_context.ok_or_else(||failure("goal_context_required","This implementation operation requires a saved, user-confirmed plan and a claimed node's goal_context. Before preparing or confirming the plan, inspect project inputs with read/search tools or present_document; document parsing, previews and read Job continuation do not require plan confirmation."))?;
        let goal = self
            .host_goal_access(&call_context.goal_id, auth, true)
            .await?;
        if let Some(window) = window_binding {
            if window.goal_id != goal.goal_id
                || window.project != goal.project
                || window.project_root != goal.project_root
            {
                return Err(failure(
                    "goal_context_mismatch",
                    "This conversation is bound to a different explicitly selected goal",
                ));
            }
        }
        if let Some(project) = project {
            if project != goal.project {
                return Err(failure(
                    "goal_project_mismatch",
                    "Goal operations must target their fixed project",
                ));
            }
        }
        self.verify_host_goal_plan(&goal, auth).await?;
        db.validate_host_goal_binding_in_context(
            &principal,
            &call_context.binding(),
            context::invocation_context().as_ref(),
        )
        .map_err(store_error)?;
        let hash = context::current_payload_hash(tool).unwrap_or_else(|| {
            files::sha256(&serde_json::to_vec(&(tool, payload)).unwrap_or_default())
        });
        context::track_active_operation(&goal.goal_id, &call_context.operation_id)
            .map_err(|e| failure("goal_operation_tracking_unavailable", e))?;
        // Enrol layout at authorized mutation admission, before any worker can
        // return a Job. Observing that Job through a generic status tool must
        // not bypass review by skipping the document facade's continuation.
        if matches!(
            tool,
            "document_edit" | "document_convert" | "document_restore"
        ) {
            let arguments = &payload["params"];
            let path = if arguments["save_mode"] == "in_place" {
                arguments["path"].as_str()
            } else {
                arguments["output_path"]
                    .as_str()
                    .or(arguments["path"].as_str())
            };
            if let Some(path) = path {
                let known = db
                    .read_host_goal_review(&principal, &goal.goal_id, &call_context.node_id)
                    .map_err(store_error)?;
                if !known.as_ref().is_some_and(|record| {
                    record
                        .managed_artifacts
                        .iter()
                        .any(|artifact| artifact.path == path)
                }) {
                    db.enforce_host_goal_layout_review(
                        &principal,
                        &call_context.binding(),
                        path,
                        now(),
                    )
                    .map_err(store_error)?;
                }
            }
        }
        let admitted = db
            .begin_host_goal_operation_with_repair_in_context(
                &principal,
                &call_context.binding(),
                &call_context.operation_id,
                tool,
                &hash,
                &[],
                now(),
                context::invocation_context().as_ref(),
                false,
                call_context.repair.as_ref(),
            )
            .map_err(store_error)?;
        if admitted.replayed {
            return Err(ToolResult::err_with_output("This exact operation is already recorded. Read its evidence or original Job instead of dispatching the effect again.",json!({"error_kind":"goal_operation_replayed","dispatch_certainty":"not_started","goal_id":goal.goal_id,"operation_id":admitted.operation.operation_id,"operation_status":admitted.operation.status,"job_id":admitted.operation.job_id,"replayed":true})));
        }
        context::mark_operation_admitted(&goal.goal_id, &call_context.operation_id);
        Ok(())
    }

    pub(crate) fn preserve_host_goal_operation_receipt(&self, tool: &str, result: &ToolResult) {
        if matches!(tool, "run_process" | "run_script") && context::admitted_operation().is_some() {
            context::preserve_operation_receipt(
                tool,
                Self::host_goal_operation_receipt(tool, result),
            );
        }
    }

    fn host_goal_operation_receipt(tool: &str, result: &ToolResult) -> context::OperationReceipt {
        let (status, job) = execution_refs::operation_receipt(tool, result);
        let (status, summary) = execution_refs::native_run_receipt(tool, result)
            .unwrap_or_else(|| (status, bounded(&result.output.to_string(), 4000)));
        context::OperationReceipt {
            status,
            job_id: job.map(str::to_owned),
            summary,
        }
    }

    pub(crate) async fn finish_host_goal_request(
        &self,
        tool: &str,
        arguments: &Value,
        auth: Option<&AuthContext>,
        outcome: &mut super::kernel::ToolCallOutcome,
    ) {
        let Ok(principal) = owner(auth) else {
            return;
        };
        let Some(db) = self.goals.store() else {
            return;
        };
        if matches!(
            tool,
            "document_edit" | "document_convert" | "document_restore"
        ) {
            if let Some(result) = outcome.result.as_mut().filter(|r| {
                r.success && r.output["status"] == "completed" && r.output["review"] == 1
            }) {
                if let Some(path) = result.output["review_material"]["source_path"]
                    .as_str()
                    .map(str::to_owned)
                {
                    let binding = if let Some(call) = context::current_binding() {
                        Some(call.binding())
                    } else {
                        context::current_mode_key()
                            .and_then(|window| {
                                db.host_goal_window(&principal, &window).ok().flatten()
                            })
                            .and_then(|mode| {
                                db.get_host_goal(&principal, &mode.goal_id).ok().flatten()
                            })
                            .and_then(|goal| {
                                goal.current_attempt_id.and_then(|id| {
                                    db.get_host_goal_attempt(&principal, &goal.goal_id, &id)
                                        .ok()
                                        .flatten()
                                })
                            })
                            .map(|attempt| attempt.binding)
                    };
                    if let Some(binding) = binding {
                        if let Err(error) =
                            db.enforce_host_goal_layout_review(&principal, &binding, &path, now())
                        {
                            result.success = false;
                            result.error=Some(format!("Document effect completed, but mandatory review registration failed: {}",error.message()));
                            result.output["review_status"] = json!("pending");
                            outcome.success = false;
                        }
                    }
                }
            }
        }
        if let Some((goal_id, operation_id)) = context::admitted_operation() {
            if let Some(result) = outcome.result.as_mut() {
                let receipt = context::take_operation_receipt(tool)
                    .unwrap_or_else(|| Self::host_goal_operation_receipt(tool, result));
                if let Err(error) = db.finish_host_goal_operation(
                    &principal,
                    &goal_id,
                    &operation_id,
                    receipt.status,
                    receipt.job_id.as_deref(),
                    Some(&receipt.summary),
                    now(),
                ) {
                    *result=ToolResult::err_with_output("The tool may have executed, but its goal receipt could not be saved. Reconcile the existing operation before retrying.",json!({"error_kind":"goal_receipt_unavailable","operation_id":operation_id,"goal_id":goal_id,"outcome_unknown":true,"detail":error.code()}));
                    outcome.success = false;
                }
            }
        }
        let window_binding = context::current_mode_key()
            .and_then(|window| db.host_goal_window(&principal, &window).ok().flatten());
        let bound = window_binding.is_some();
        let host_control = is_host_goal_control(tool) && !matches!(tool, "goal_read" | "goal_list");
        if !bound
            && context::current_binding().is_none()
            && arguments.get("context_epoch").is_none()
            && !host_control
            && !outcome
                .result
                .as_ref()
                .is_some_and(|r| r.output["driver"] == "host")
        {
            return;
        }
        if let Some(result) = outcome.result.as_mut() {
            if !result.output.is_object() {
                return;
            }
            let bound_goal_id = context::current_binding()
                .map(|binding| binding.goal_id)
                .or_else(|| {
                    window_binding
                        .as_ref()
                        .map(|binding| binding.goal_id.clone())
                })
                .or_else(|| result.output["goal"]["goal_id"].as_str().map(str::to_owned));
            if let Some(id) = bound_goal_id {
                if self.host_goal_access(&id, auth, false).await.is_ok() {
                    if let Ok(Some(repair)) = db.get_host_goal_repair(&principal, &id) {
                        result.output["recovery"] = projection::repair_summary(&repair);
                    }
                }
            }
            // A console read has no ChatGPT identity. Keep the saved latest-chat
            // projection instead of replacing it with an unavailable new meter.
            if context::current_epoch().is_none() && result.output.get("context_meter").is_some() {
                return;
            }
            let goal_id = window_binding
                .as_ref()
                .map(|binding| binding.goal_id.clone())
                .or_else(|| context::current_binding().map(|binding| binding.goal_id))
                .or_else(|| result.output["goal"]["goal_id"].as_str().map(str::to_owned))
                .or_else(|| {
                    (result.output["driver"] == "host")
                        .then(|| result.output["goal_id"].as_str().map(str::to_owned))
                        .flatten()
                });
            let mut visible = serde_json::to_value(&*result).unwrap_or_default();
            if let Some(output) = visible["output"].as_object_mut() {
                output.remove("context_meter");
            }
            match context::observe_response(db, arguments, &visible, None) {
                Ok(meter) => {
                    if let (Some(goal_id), Some(epoch)) =
                        (goal_id.as_deref(), meter["context_epoch"].as_str())
                    {
                        if let Err(error) =
                            db.record_host_goal_context_ref(&principal, goal_id, epoch)
                        {
                            tracing::warn!(error=%error,"cannot update goal conversation meter reference");
                        }
                    }
                    result.output["context_meter"] = meter;
                }
                Err(_) => {
                    result.output["context_meter"] = context::meter_projection(None, None);
                }
            }
            if result.success && result.output["projection"] == "node_context" {
                if let Err(error) = projection::fit_node_response(&mut result.output) {
                    *result = error;
                    outcome.success = false;
                    return;
                }
            }
            if result.success
                && tool == "goal_read"
                && result.output["projection"] == "node_context"
                && result.output["context_read"]["complete"] == true
            {
                if let (
                    Some(epoch),
                    Some(goal_id),
                    Some(revision),
                    Some(plan_version),
                    Some(node_id),
                ) = (
                    context::current_epoch(),
                    result.output["context_read"]["goal_id"].as_str(),
                    result.output["context_read"]["revision"].as_u64(),
                    result.output["context_read"]["plan_version"].as_u64(),
                    result.output["context_read"]["node_id"].as_str(),
                ) {
                    if result.output["node"]["id"] == node_id
                        && result.output["requirements"].is_object()
                    {
                        if let Err(error) = db.ack_host_goal_context_read(
                            &principal,
                            &epoch,
                            goal_id,
                            revision,
                            plan_version,
                            node_id,
                        ) {
                            tracing::warn!(error=%error,"cannot save goal node context read receipt");
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn host_goal_db(&self) -> Result<&crate::Database, ToolResult> {
        self.goals.store().ok_or_else(|| {
            failure(
                "goal_store_unavailable",
                "Host goal mode requires the persistent local Server database",
            )
        })
    }

    pub(crate) async fn host_goal_access(
        &self,
        id: &str,
        auth: Option<&AuthContext>,
        write: bool,
    ) -> Result<HostGoalRecord, ToolResult> {
        let principal = owner(auth)?;
        scope(
            auth,
            if write {
                "project:write"
            } else {
                "project:read"
            },
        )?;
        let goal = self
            .host_goal_db()?
            .get_host_goal(&principal, id)
            .map_err(store_error)?
            .ok_or_else(|| failure("goal_not_found", "Goal is not visible to this caller"))?;
        let project = self
            .execution()
            .resolve_project_input_for_auth(&goal.project, auth)
            .await
            .map_err(|e| e.into_tool_result())?;
        if project.resolved_id != goal.project || project.config.path != goal.project_root {
            return Err(failure(
                "goal_target_changed",
                "The saved goal belongs to a different project root; do not retarget it implicitly",
            ));
        }
        Ok(goal)
    }

    fn host_goal_plan(&self, goal: &HostGoalRecord) -> Result<HostGoalPlanVersion, ToolResult> {
        self.host_goal_db()?
            .get_host_goal_plan(&goal.owner_key, &goal.goal_id, None)
            .map_err(store_error)?
            .ok_or_else(|| {
                failure(
                    "goal_plan_missing",
                    "Save a complete local plan before requesting execution",
                )
            })
    }

    pub(crate) async fn verify_host_goal_plan(
        &self,
        goal: &HostGoalRecord,
        auth: Option<&AuthContext>,
    ) -> Result<HostGoalPlanVersion, ToolResult> {
        let plan = self.host_goal_plan(goal)?;
        let materialized = plan.materialization.as_ref().ok_or_else(|| {
            failure(
                "goal_plan_not_materialized",
                "The plan has not been published locally",
            )
        })?;
        if !goal.plan_materialized
            || goal.plan_version != plan.version
            || goal.plan_hash.as_deref() != Some(plan.hash.as_str())
        {
            return Err(failure(
                "goal_plan_version_conflict",
                "Current plan publication is incomplete",
            ));
        }
        if goal.task_directory.is_some() {
            self.verify_project_goal_plan(goal, &plan, materialized, auth)
                .await?;
        } else {
            files::verify(
                &self
                    .host_goal_db()?
                    .host_goal_plan_root()
                    .map_err(store_error)?,
                &goal.goal_id,
                plan.version,
                &artifacts(materialized),
            )
            .map_err(|e| failure("goal_plan_artifact_changed", e))?;
        }
        Ok(plan)
    }

    fn bind_host_goal_context(
        &self,
        goal: &HostGoalRecord,
        epoch: Option<&str>,
    ) -> Result<&'static str, ToolResult> {
        if context::current_window().is_none() && context::current_owner().is_some() {
            if let Some(epoch) = epoch {
                context::set_epoch(epoch).map_err(|e| failure("invalid_context_epoch", e))?;
            }
        }
        let Some(window) = context::current_mode_key() else {
            return Ok("explicit_context_only");
        };
        let epoch = context::current_epoch().ok_or_else(|| {
            failure(
                "context_identity_unavailable",
                "The conversation has no meter identity",
            )
        })?;
        self.host_goal_db()?
            .bind_host_goal_window_in_context(
                &goal.owner_key,
                &window,
                &HostGoalWindowBinding {
                    goal_id: goal.goal_id.clone(),
                    project: goal.project.clone(),
                    project_root: goal.project_root.clone(),
                    context_epoch: epoch,
                },
                context::invocation_context().as_ref(),
            )
            .map_err(|e| failure("goal_binding_unavailable", e.to_string()))?;
        Ok(if context::current_window().is_some() {
            "window_bound"
        } else {
            "explicit_context_only"
        })
    }

    pub(crate) async fn host_goal_read(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<usize>,
        kind: Option<&str>,
        view: Option<&str>,
        node_id: Option<&str>,
        section: Option<&str>,
        cursor: Option<&str>,
        source_context_epoch: Option<&str>,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        let goal = match self.host_goal_access(id, auth, false).await {
            Ok(goal) => goal,
            Err(e) => return e,
        };
        match self
            .bounded_host_goal_read(
                &goal,
                after,
                limit,
                kind,
                view,
                node_id,
                section,
                cursor,
                source_context_epoch,
                auth,
            )
            .await
        {
            Ok(output) => ToolResult::ok(output),
            Err(error) => error,
        }
    }

    async fn host_goal_delivery_freshness(
        &self,
        goal: &HostGoalRecord,
        auth: Option<&AuthContext>,
    ) -> &'static str {
        let Ok(db) = self.host_goal_db() else {
            return "unavailable";
        };
        let Ok(Some(attempt)) = db.latest_host_goal_attempt(&goal.owner_key, &goal.goal_id) else {
            return "unavailable";
        };
        let mut artifacts = std::collections::BTreeMap::new();
        let mut state_fingerprint = None;
        let mut image_sources = std::collections::BTreeMap::new();
        let Ok(plan) = self.host_goal_plan(goal) else {
            return "unavailable";
        };
        for node in &plan.plan.nodes {
            let Ok(record) = db.read_host_goal_review(&goal.owner_key, &goal.goal_id, &node.id)
            else {
                return "unavailable";
            };
            if webcodex_core::host_goal::host_goal_review_policy(node).review == 1
                || record.as_ref().is_some_and(|r| r.review == 1)
            {
                let Some(record) = record else {
                    return "unavailable";
                };
                let Some(round) = record
                    .rounds
                    .last()
                    .filter(|r| r.passed && record.current_packet.as_ref() == Some(&r.packet))
                else {
                    return "unavailable";
                };
                if let Some(hash) = &round.packet.state_sha256 {
                    state_fingerprint = Some(hash.clone());
                }
                for artifact in &round.packet.manifest {
                    if let Some(hash) = &artifact.sha256 {
                        artifacts.insert(artifact.path.clone(), hash.clone());
                    }
                }
                let mut current_images =
                    std::collections::BTreeMap::<String, Vec<(String, String)>>::new();
                for material in &round.packet.materials {
                    if let (Some(path), Some(hash)) =
                        (&material.preview_path, &material.preview_sha256)
                    {
                        current_images
                            .entry(material.path.clone())
                            .or_default()
                            .push((path.clone(), hash.clone()));
                    }
                }
                image_sources.extend(current_images);
            }
        }
        // Later planned stages can replace earlier state/results. Check the
        // latest reviewed version of each source, retaining earlier history.
        if state_fingerprint.is_some_and(|expected| {
            expected
                != files::sha256(
                    &serde_json::to_vec(
                        &webcodex_core::host_goal::host_goal_review_state_material(&goal.state),
                    )
                    .unwrap(),
                )
        }) {
            return "changed";
        }
        for snapshots in image_sources.into_values() {
            artifacts.extend(snapshots);
        }
        for evidence in attempt.checks.iter().chain(attempt.final_checks.iter()) {
            for artifact in &evidence.artifacts {
                if let Some(hash) = &artifact.sha256 {
                    artifacts.insert(artifact.path.clone(), hash.clone());
                }
            }
        }
        for (path, expected) in artifacts {
            let Some(hash) = self
                .host_goal_file_fingerprint(&goal.project, &path, auth)
                .await
            else {
                return "unavailable";
            };
            if hash != expected {
                return "changed";
            }
        }
        "current"
    }

    fn require_host_goal_context_read(
        &self,
        goal: &HostGoalRecord,
        revision: u64,
        node_id: &str,
    ) -> Result<(), ToolResult> {
        if context::current_owner().is_none() {
            return Ok(());
        }
        let Some(epoch) = context::current_epoch() else {
            return Err(failure("goal_context_identity_required","Node execution needs a reliable chat identity or a fixed explicit context_epoch. Supply the same epoch on goal_read and goal_claim_node."));
        };
        if !self
            .host_goal_db()?
            .host_goal_context_read_matches(
                &goal.owner_key,
                &epoch,
                &goal.goal_id,
                revision,
                goal.plan_version,
                node_id,
            )
            .map_err(|e| failure("goal_context_read_unavailable", e.to_string()))?
        {
            return Err(ToolResult::err_with_output("Read this node's bounded requirements package in this conversation before claiming it; summaries and bootstrap do not acknowledge node context.",json!({"error_kind":"goal_context_read_required","dispatch_certainty":"not_started","goal_id":goal.goal_id,"continuation":{"tool":"goal_read","arguments":{"goal_id":goal.goal_id,"view":"node","node_id":node_id,"context_epoch":epoch}}})));
        }
        Ok(())
    }

    fn host_goal_page(
        &self,
        goal: &HostGoalRecord,
        _after: Option<u64>,
        _limit: Option<usize>,
        replayed: Option<bool>,
    ) -> ToolResult {
        match projection::summary(self, goal, replayed) {
            Ok(output) => ToolResult::ok(output),
            Err(error) => error,
        }
    }

    pub(crate) async fn dispatch_host_goal_tool(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        match self.host_goal_tool_inner(call, auth).await {
            Ok(result) => result,
            Err(error) => error,
        }
    }

    async fn host_goal_tool_inner(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> Result<ToolResult, ToolResult> {
        let principal = owner(auth)?;
        let invocation = context::invocation_context();
        if let Some(invocation) = &invocation {
            if let ToolCall::GoalModeStart {
                context_warning_tokens,
                context_handoff_tokens,
                ..
            } = &call
            {
                if context_warning_tokens.is_some() || context_handoff_tokens.is_some() {
                    let warning = context_warning_tokens.unwrap_or(600_000);
                    let handoff = context_handoff_tokens.unwrap_or(800_000);
                    if !(1000..=10_000_000).contains(&warning)
                        || handoff <= warning
                        || handoff > 20_000_000
                    {
                        return Err(failure(
                            "invalid_context_thresholds",
                            "Require 1000 <= warning <= 10000000 and warning < handoff <= 20000000",
                        ));
                    }
                    // Set the new epoch's policy before identity initialization;
                    // existing meters deliberately retain their original policy.
                    self.host_goal_db()?
                        .observe_host_context(
                            &principal,
                            &invocation.context_epoch,
                            "context-identity-init",
                            0,
                            false,
                            Some((warning, handoff)),
                        )
                        .map_err(|e| failure("invalid_context_thresholds", e.to_string()))?;
                }
            }
            self.host_goal_db()?
                .ensure_host_context_identity(&principal, invocation)
                .map_err(store_error)?;
        }
        match call {
            ToolCall::GoalModeStart {
                project,
                task_directory,
                objective,
                idempotency_key,
                context_epoch,
                context_warning_tokens,
                context_handoff_tokens,
            } => {
                scope(auth, "project:write")?;
                scope(auth, "project:read")?;
                if context_warning_tokens.is_some() || context_handoff_tokens.is_some() {
                    let warning = context_warning_tokens.unwrap_or(600_000);
                    let handoff = context_handoff_tokens.unwrap_or(800_000);
                    if !(1000..=10_000_000).contains(&warning)
                        || handoff <= warning
                        || handoff > 20_000_000
                    {
                        return Err(failure(
                            "invalid_context_thresholds",
                            "Require 1000 <= warning <= 10000000 and warning < handoff <= 20000000",
                        ));
                    }
                    if context::current_epoch().is_none() {
                        return Err(failure("context_identity_unavailable","Provide a fixed context_epoch before configuring a conversation without a reliable host identity"));
                    }
                }
                let project = self
                    .execution()
                    .resolve_project_input_for_auth(&project, auth)
                    .await
                    .map_err(|e| e.into_tool_result())?;
                if task_directory
                    .as_deref()
                    .is_some_and(|path| path.trim().is_empty())
                {
                    return Err(failure("invalid_goal_task_directory","task_directory must name the user's task folder, or be omitted for the project root"));
                }
                let task_root =
                    super::helpers::resolve_runner_cwd(&project.config, task_directory.as_deref())
                        .map_err(|e| failure("invalid_goal_task_directory", e))?;
                let task_directory =
                    super::helpers::project_relative_runner_cwd(&project.config, &task_root)
                        .map_err(|e| failure("invalid_goal_task_directory", e))?;
                let result = self
                    .host_goal_db()?
                    .create_host_goal_in_task_directory_in_context(
                        &principal,
                        &project.resolved_id,
                        &project.config.path,
                        &task_directory,
                        &objective,
                        &idempotency_key,
                        now(),
                        invocation.as_ref(),
                    )
                    .map_err(store_error)?;
                if result.replayed && self.host_goal_context_gate(&principal, false).is_err() {
                    let mut response =
                        ToolResult::ok(projection::entry(self, &result.goal, Some(true))?);
                    response.output["mode_binding"] =
                        json!(if context::current_window().is_some() {
                            "window_bound"
                        } else {
                            "explicit_context_only"
                        });
                    return Ok(response);
                }
                let binding =
                    self.bind_host_goal_context(&result.goal, context_epoch.as_deref())?;
                if context_warning_tokens.is_some() || context_handoff_tokens.is_some() {
                    let epoch = context::current_epoch().ok_or_else(|| {
                        failure(
                            "context_identity_unavailable",
                            "Start a conversation meter before changing its thresholds",
                        )
                    })?;
                    self.host_goal_db()?
                        .observe_host_context(
                            &principal,
                            &epoch,
                            &format!("config_{}", files::sha256(idempotency_key.as_bytes())),
                            0,
                            false,
                            Some((
                                context_warning_tokens.unwrap_or(600_000),
                                context_handoff_tokens.unwrap_or(800_000),
                            )),
                        )
                        .map_err(|e| failure("invalid_context_thresholds", e.to_string()))?;
                }
                let mut response = ToolResult::ok(projection::entry(
                    self,
                    &result.goal,
                    Some(result.replayed),
                )?);
                response.output["mode_binding"] = json!(binding);
                Ok(response)
            }
            ToolCall::GoalPlanSave {
                goal_id,
                expected_revision,
                idempotency_key,
                plan,
                change_request,
            } => {
                let _goal = self.host_goal_access(&goal_id, auth, true).await?;
                let plan: HostGoalPlan = serde_json::from_value(plan)
                    .map_err(|e| failure("invalid_goal_plan", e.to_string()))?;
                let saved = self
                    .host_goal_db()?
                    .save_host_goal_plan_in_context(
                        &principal,
                        &goal_id,
                        expected_revision,
                        &plan,
                        change_request.as_deref(),
                        &idempotency_key,
                        now(),
                        invocation.as_ref(),
                    )
                    .map_err(store_error)?;
                if saved.replayed {
                    // The save receipt precedes publication. Read the actual
                    // version's publication receipt instead of rewriting files
                    // on an already completed request (especially after retire).
                    let published = self
                        .host_goal_db()?
                        .get_host_goal_plan(&principal, &goal_id, Some(saved.plan.version))
                        .map_err(store_error)?;
                    if published
                        .is_some_and(|p| p.hash == saved.plan.hash && p.materialization.is_some())
                    {
                        return Ok(self.host_goal_page(&saved.goal, None, None, Some(true)));
                    }
                }
                self.host_goal_context_gate(&principal, false)?;
                let _plan_lease =
                    context::lease_active_operation(&goal_id, &format!("plan_{idempotency_key}"));
                self.host_goal_context_gate(&principal, false)?;
                let artifact = if saved.goal.task_directory.is_some() {
                    self.materialize_project_goal_plan(&saved.goal, &saved.plan, auth)
                        .await?
                } else {
                    files::materialize(
                        &self
                            .host_goal_db()?
                            .host_goal_plan_root()
                            .map_err(store_error)?,
                        &goal_id,
                        saved.plan.version,
                        &saved.plan.hash,
                        &json!(&saved.plan.plan),
                    )
                    .map_err(|e| failure("goal_plan_materialization_failed", e))?
                };
                let receipt = HostGoalMaterialization {
                    json_path: artifact.json_path,
                    json_hash: artifact.json_sha256,
                    markdown_path: artifact.markdown_path,
                    markdown_hash: artifact.markdown_sha256,
                    verified_at: now(),
                };
                let published = self
                    .host_goal_db()?
                    .materialize_host_goal_plan_in_context(
                        &principal,
                        &goal_id,
                        saved.plan.version,
                        &saved.plan.hash,
                        &receipt,
                        now(),
                        invocation.as_ref(),
                    )
                    .map_err(store_error)?;
                let mut response =
                    self.host_goal_page(&published.goal, None, None, Some(saved.replayed));
                response.output["plan_artifacts_valid"] = json!(true);
                Ok(response)
            }
            ToolCall::GoalConfirmPlan {
                goal_id,
                expected_revision,
                plan_version,
                plan_sha256,
                confirmation,
                idempotency_key,
            } => {
                let goal = self.host_goal_access(&goal_id, auth, true).await?;
                self.verify_host_goal_plan(&goal, auth).await?;
                let result = self
                    .host_goal_db()?
                    .confirm_host_goal_plan_in_context(
                        &principal,
                        &goal_id,
                        expected_revision,
                        plan_version,
                        &plan_sha256,
                        &confirmation,
                        &idempotency_key,
                        now(),
                        invocation.as_ref(),
                    )
                    .map_err(store_error)?;
                Ok(self.host_goal_page(&result.goal, None, None, Some(result.replayed)))
            }
            ToolCall::GoalBootstrap {
                goal_id,
                project,
                context_epoch,
                activate,
            } => {
                scope(auth, "project:read")?;
                let id=match goal_id {Some(id)=>id,None=>self.host_goal_db()?.current_host_goal(&principal).map_err(store_error)?.ok_or_else(||failure("goal_current_missing","No explicitly selected local goal. Use goal_list and select a goal."))?.goal_id};
                let goal = self.host_goal_access(&id, auth, false).await?;
                if let Some(project) = project {
                    let resolved = self
                        .execution()
                        .resolve_project_input_for_auth(&project, auth)
                        .await
                        .map_err(|e| e.into_tool_result())?;
                    if resolved.resolved_id != goal.project {
                        return Err(failure(
                            "goal_project_mismatch",
                            "Current goal belongs to another project; select an explicit goal",
                        ));
                    }
                }
                let mode = if activate {
                    self.host_goal_context_gate(&principal, false)?;
                    self.bind_host_goal_context(&goal, context_epoch.as_deref())?
                } else {
                    "explicit_context_only"
                };
                Ok(self.host_goal_bootstrap_result(&goal, mode)?)
            }
            ToolCall::GoalClaimNode {
                goal_id,
                node_id,
                expected_revision,
                plan_version,
                idempotency_key,
                context_epoch,
                rework,
                takeover_attempt_id,
            } => {
                let goal = self.host_goal_access(&goal_id, auth, true).await?;
                if let Some(previous) = self
                    .host_goal_db()?
                    .lookup_host_goal_claim_request(
                        &principal,
                        &goal_id,
                        expected_revision,
                        &node_id,
                        rework,
                        plan_version,
                        takeover_attempt_id.as_deref(),
                        &idempotency_key,
                    )
                    .map_err(store_error)?
                {
                    let mut response = self.host_goal_page(&previous.goal, None, None, Some(true));
                    if self
                        .host_goal_db()?
                        .validate_host_goal_binding_in_context(
                            &principal,
                            &previous.attempt.binding,
                            invocation.as_ref(),
                        )
                        .is_ok()
                    {
                        response.output["binding"] = json!(previous.attempt.binding);
                    }
                    return Ok(response);
                }
                self.host_goal_context_gate(&principal, false)?;
                self.require_host_goal_context_read(&goal, expected_revision, &node_id)?;
                self.verify_host_goal_plan(&goal, auth).await?;
                if goal.plan_version != plan_version {
                    return Err(failure(
                        "goal_plan_version_conflict",
                        "Read and confirm the current plan version",
                    ));
                }
                self.reconcile_host_goal_operations(&goal, auth).await?;
                let result = self
                    .host_goal_db()?
                    .claim_host_goal_node_for_plan_in_context(
                        &principal,
                        &goal_id,
                        expected_revision,
                        &node_id,
                        rework,
                        plan_version,
                        takeover_attempt_id.as_deref(),
                        &idempotency_key,
                        now(),
                        invocation.as_ref(),
                    )
                    .map_err(store_error)?;
                let mode = self.bind_host_goal_context(&result.goal, context_epoch.as_deref())?;
                let mut response =
                    self.host_goal_page(&result.goal, None, None, Some(result.replayed));
                response.output["binding"] = json!(result.attempt.binding);
                response.output["mode_binding"] = json!(mode);
                Ok(response)
            }
            ToolCall::GoalCheckpoint {
                goal_id,
                expected_revision,
                idempotency_key,
                patch,
                context_epoch,
            } => {
                let goal = self.host_goal_access(&goal_id, auth, true).await?;
                let exit_window = if patch["action"] == "exit" {
                    context::current_mode_key()
                } else {
                    None
                };
                let request_hash = files::sha256(
                    &serde_json::to_vec(&(&goal_id, expected_revision, &patch, exit_window))
                        .map_err(|e| failure("invalid_goal_checkpoint", e.to_string()))?,
                );
                if let Some(previous) = self
                    .host_goal_db()?
                    .lookup_host_goal_checkpoint_replay(
                        &principal,
                        &goal_id,
                        &idempotency_key,
                        &request_hash,
                    )
                    .map_err(store_error)?
                {
                    if patch["action"] == "exit"
                        && self.host_goal_context_gate(&principal, false).is_ok()
                    {
                        if let Some(window) = context::current_mode_key() {
                            self.host_goal_db()?
                                .unbind_host_goal_window_in_context(
                                    &principal,
                                    &window,
                                    &goal_id,
                                    invocation.as_ref(),
                                )
                                .map_err(|e| failure("goal_binding_unavailable", e.to_string()))?;
                        }
                    }
                    let mut response = self.host_goal_page(&previous.goal, None, None, Some(true));
                    if patch["action"] == "handoff" {
                        self.attach_host_goal_handoff_files(&previous.goal, &mut response, auth)
                            .await?;
                    }
                    return Ok(response);
                }
                self.host_goal_context_gate(&principal, true)?;
                if patch["action"] == "exit" {
                    // Removing the mode while draining would turn later calls
                    // into unbound work. Reject before committing any pause.
                    self.host_goal_context_gate(&principal, false)?;
                }
                if patch.get("state").is_some_and(|state| !state.is_object()) {
                    return Err(failure(
                        "invalid_goal_checkpoint",
                        "state must be an object; omit it to preserve state",
                    ));
                }
                let patch: CheckpointPatch = serde_json::from_value(patch)
                    .map_err(|e| failure("invalid_goal_checkpoint", e.to_string()))?;
                let updated_state = merge_checkpoint_state(&goal.state, patch.state.as_ref())?;
                if expected_revision != goal.revision {
                    return Err(failure(
                        "goal_conflict",
                        "Read the current goal before recording a checkpoint",
                    ));
                }
                self.host_goal_db()?
                    .reserve_host_goal_checkpoint_request_in_context(
                        &principal,
                        &goal_id,
                        &idempotency_key,
                        &request_hash,
                        invocation.as_ref(),
                    )
                    .map_err(store_error)?;
                if patch.operation_resolutions.len() > 8 {
                    return Err(failure(
                        "invalid_goal_resolution",
                        "At most eight explicit operator resolutions per checkpoint",
                    ));
                }
                let mut resolutions = Vec::new();
                for resolution in &patch.operation_resolutions {
                    if context::operation_is_active(&goal_id, &resolution.operation_id) {
                        return Err(failure("goal_operation_still_active","The original tool request is still running. Wait for its result before manual reconciliation."));
                    }
                    if !resolution.status.terminal()
                        || resolution.evidence.trim().is_empty()
                        || resolution.evidence.len() > 4096
                        || resolution.user_confirmation.trim().is_empty()
                        || resolution.user_confirmation.len() > 4096
                        || resolution.source_refs.is_empty()
                        || resolution.source_refs.len() > 16
                        || resolution
                            .source_refs
                            .iter()
                            .any(|s| s.trim().is_empty() || s.len() > 1024)
                    {
                        return Err(failure("invalid_goal_resolution","Manual reconciliation requires a terminal outcome, concrete evidence/sources, and the user's explicit instruction; it is not automated acceptance"));
                    }
                    let summary=json!({"source":"user_reconciliation_report","evidence":resolution.evidence,"source_refs":resolution.source_refs,"user_confirmation":resolution.user_confirmation}).to_string();
                    if summary.len() > 8192 {
                        return Err(failure("invalid_goal_resolution","The combined reconciliation report must fit in 8192 bytes; use concise evidence and source references"));
                    }
                    resolutions.push(HostGoalOperationResolution {
                        operation_id: resolution.operation_id.clone(),
                        status: resolution.status,
                        report: summary,
                    });
                }
                let action = match patch.action.as_str() {
                    "checkpoint" => HostGoalCheckpointAction::Progress,
                    "handoff" => HostGoalCheckpointAction::Handoff,
                    "pause" | "exit" => HostGoalCheckpointAction::Pause,
                    "block" => HostGoalCheckpointAction::Block,
                    "cancel" => HostGoalCheckpointAction::Cancel,
                    _ => {
                        return Err(failure(
                            "invalid_goal_checkpoint",
                            "Unsupported checkpoint action",
                        ))
                    }
                };
                let binding = context::current_binding().map(|c| c.binding());
                if patch.action == "cancel" && resolutions.is_empty() {
                    self.reconcile_host_goal_operations(&goal, auth).await?;
                }
                let db = self.host_goal_db()?;
                let verified_background = if action == HostGoalCheckpointAction::Handoff {
                    if !resolutions.is_empty() {
                        return Err(failure("goal_handoff_reconciliation_required", "Commit operation resolutions in an ordinary checkpoint before handing off"));
                    }
                    self.prepare_host_goal_handoff(&goal, auth).await?
                } else {
                    Vec::new()
                };
                let resolution_ids: Vec<String> =
                    resolutions.iter().map(|r| r.operation_id.clone()).collect();
                let commit = || {
                    db.checkpoint_host_goal_with_request_in_context(
                        &principal,
                        &goal_id,
                        expected_revision,
                        binding.as_ref(),
                        &updated_state,
                        action,
                        &patch.reason,
                        &idempotency_key,
                        &request_hash,
                        &resolutions,
                        now(),
                        invocation.as_ref(),
                        &verified_background,
                    )
                };
                let result = if action == HostGoalCheckpointAction::Handoff {
                    context::guarded_goal_handoff(&goal_id, commit)
                } else {
                    context::guarded_operation_reconciliation(&goal_id, &resolution_ids, commit)
                }
                .map_err(|e| failure("goal_operation_still_active", e))?
                .map_err(store_error)?;
                if patch.action == "exit" {
                    if let Some(window) = context::current_mode_key() {
                        self.host_goal_db()?
                            .unbind_host_goal_window_in_context(
                                &principal,
                                &window,
                                &goal_id,
                                invocation.as_ref(),
                            )
                            .map_err(|e| failure("goal_binding_unavailable", e.to_string()))?;
                    }
                }
                if context::current_window().is_none() && context::current_owner().is_some() {
                    if let Some(epoch) = context_epoch {
                        context::set_epoch(&epoch)
                            .map_err(|e| failure("invalid_context_epoch", e))?;
                    }
                }
                let mut response =
                    self.host_goal_page(&result.goal, None, None, Some(result.replayed));
                if patch.action == "handoff" {
                    self.attach_host_goal_handoff_files(&result.goal, &mut response, auth)
                        .await?;
                }
                Ok(response)
            }
            ToolCall::GoalCheckNode {
                goal_id,
                node_id,
                attempt_id,
                expected_revision,
                plan_version,
                idempotency_key,
                evidence,
            } => {
                let review_node_id = node_id.clone();
                let preparing_review = evidence
                    .as_ref()
                    .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty));
                let result = self
                    .host_goal_check_node(
                        goal_id,
                        node_id,
                        attempt_id,
                        expected_revision,
                        plan_version,
                        idempotency_key,
                        evidence,
                        auth,
                    )
                    .await?;
                let mut response =
                    self.host_goal_page(&result.goal, None, None, Some(result.replayed));
                if let Some(mut record) = self
                    .host_goal_db()?
                    .read_host_goal_review(
                        &result.goal.owner_key,
                        &result.goal.goal_id,
                        &review_node_id,
                    )
                    .map_err(store_error)?
                    .filter(|_| result.goal.current_attempt_id.is_some())
                {
                    if let Some(packet) = record.current_packet.as_ref() {
                        let text_materials = packet
                            .materials
                            .iter()
                            .filter(|m| m.kind == "text" || m.kind == "state")
                            .cloned()
                            .collect::<Vec<_>>();
                        let text_scopes = text_materials
                            .iter()
                            .map(|m| m.id.clone())
                            .collect::<Vec<_>>();
                        if text_scopes
                            .iter()
                            .any(|s| !packet.delivered_scopes.contains(s))
                            && result.goal.current_attempt_id.as_deref()
                                == Some(packet.binding.attempt_id.as_str())
                        {
                            record = self
                                .host_goal_db()?
                                .record_host_goal_review_delivery(
                                    &result.goal.owner_key,
                                    &packet.binding,
                                    &packet.packet_id,
                                    context::current_epoch().as_deref(),
                                    &text_materials,
                                    &[],
                                    &text_scopes,
                                    now(),
                                )
                                .map_err(store_error)?;
                        }
                    }
                    if preparing_review {
                        response.output["review_packet"] = json!(record.current_packet);
                    }
                    response.output["review_instructions"] = json!({"source":"model_self_check","checks":["semantic correctness and actual machine results","all declared pages/regions; overlap, bounds, clipping, font/readability, axes/legends, color and numeric consistency","previous findings resolved and no new regression"],"submit_tool":"goal_check_node","criterion_id_when_no_review_criterion":"__delivery_review","required_evidence_fields":["review_packet_id","artifacts","reviewed_scopes","evidence","findings"],"read_images":{"tool":"present_document","arguments":{"project":result.goal.project,"view":"pages","review":1},"path":"Use each #visual_pending source, then follow all cursors. Raster drawing sources use read_project_artifact as_image=true."},"history":{"tool":"goal_read","arguments":{"goal_id":result.goal.goal_id,"view":"detail","section":"reviews"}}});
                }
                Ok(response)
            }
            _ => Err(failure("invalid_goal_tool", "Not a host goal operation")),
        }
    }

    fn host_goal_bootstrap_result(
        &self,
        goal: &HostGoalRecord,
        mode: &str,
    ) -> Result<ToolResult, ToolResult> {
        let mut output = projection::entry(self, goal, None)?;
        output["projection"] = json!("bootstrap");
        output["mode_binding"] = json!(mode);
        output["confirmation_required"] = json!(!goal.confirmed());
        Ok(ToolResult::ok(output))
    }

    async fn attach_host_goal_handoff_files(
        &self,
        goal: &HostGoalRecord,
        response: &mut ToolResult,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        if goal.task_directory.is_none() {
            return Ok(());
        }
        let checkpoint=self.host_goal_db()?.host_goal_checkpoint_for_revision(&goal.owner_key,&goal.goal_id,goal.revision)
            .map_err(store_error)?.ok_or_else(||failure("goal_handoff_checkpoint_missing","The committed handoff checkpoint could not be read; its state has not been rewritten"))?;
        match self.export_project_goal_handoff(goal,&checkpoint,auth).await {
            Ok(mut files)=> {files["publication"]=json!("verified");response.output["handoff_artifacts"]=files;Ok(())}
            Err(error)=>Err(ToolResult::err_with_output("The handoff checkpoint is committed, but its task-folder files are not verified. Retry the identical goal_checkpoint request/key to finish publication; do not restart work or reinterpret this as an uncommitted handoff.",
                json!({"error_kind":"goal_handoff_files_pending","checkpoint_saved":true,"state_changed":true,
                    "goal_id":goal.goal_id,"committed_revision":goal.revision,"checkpoint_sequence":checkpoint.sequence,
                    "task_files":super::host_goal_project_files::task_paths(goal),"cause":error.output,"detail":error.error})))
        }
    }
}
