//! The existing Host Goal state machine behind the private Agent/run boundary.
//! Execution adapters remain unavailable; model context IDs are never trusted.
use crate::agent_private_files::{AgentPrivateFiles, PrivateFileSnapshot};
use crate::tool_runtime::{ToolCall, ToolResult};
use crate::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webcodex_core::host_goal::*;
use webcodex_store::{
    AgentIsolationError, HostGoalCheckpointAction, HostGoalWindowBinding, VerifiedAgentRun,
};

pub(crate) fn supports(name: &str) -> bool {
    matches!(
        name,
        "goal_mode_start"
            | "goal_plan_save"
            | "goal_confirm_plan"
            | "goal_bootstrap"
            | "goal_claim_node"
            | "goal_checkpoint"
            | "goal_read"
            | "goal_handoff"
    )
}

fn denied(code: &str) -> ToolResult {
    crate::agent_isolation::denial(code)
}
fn storage(error: webcodex_store::GoalStoreError) -> ToolResult {
    denied(error.code())
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn partial(code: &str, goal_id: &str) -> ToolResult {
    ToolResult::err_with_output(
        code,
        json!({"error_kind":code,"goal_id":goal_id,"state_changed":true,
        "dispatch_certainty":"outcome_unknown","recovery":"Read this exact Goal and retry the identical idempotent request; do not create another Goal or plan version."}),
    )
}

pub(crate) fn files(db: &Database) -> Result<AgentPrivateFiles, ToolResult> {
    AgentPrivateFiles::new(db.agent_private_root())
        .map_err(|_| denied("agent_private_access_denied"))
}

pub(crate) fn write_exact(
    files: &AgentPrivateFiles,
    binding: &VerifiedAgentRun,
    path: &str,
    bytes: &[u8],
) -> Result<PrivateFileSnapshot, AgentIsolationError> {
    match files.write(binding, path, bytes, None) {
        Ok(snapshot) => Ok(snapshot),
        Err(code) if code == "agent_private_hash_conflict" => {
            let snapshot = files
                .read(binding, path)
                .map_err(|_| AgentIsolationError::new("agent_private_access_denied"))?;
            if snapshot.bytes == bytes {
                Ok(snapshot)
            } else {
                Err(AgentIsolationError::new("goal_materialization_changed"))
            }
        }
        Err(_) => Err(AgentIsolationError::new("agent_private_access_denied")),
    }
}

fn publish_plan(
    db: &Database,
    binding: &VerifiedAgentRun,
    plan: &HostGoalPlanVersion,
) -> Result<HostGoalMaterialization, ToolResult> {
    let private = files(db)?;
    let mut bytes =
        serde_json::to_vec_pretty(&plan.plan).map_err(|_| denied("invalid_goal_plan"))?;
    bytes.push(b'\n');
    let plan_value = serde_json::to_value(&plan.plan).map_err(|_| denied("invalid_goal_plan"))?;
    let markdown = crate::tool_runtime::agent_goal_plan_markdown(
        &plan_value,
        &plan.goal_id,
        plan.version,
        &plan.hash,
    );
    let prefix = format!("goals/{}/plans/v{}", plan.goal_id, plan.version);
    db.with_agent_run_for_goal(binding, "goal_plan_publish", &plan.goal_id, || {
        private
            .create(binding)
            .map_err(|_| AgentIsolationError::new("agent_private_access_denied"))?;
        let json = write_exact(&private, binding, &format!("{prefix}/plan.json"), &bytes)?;
        let markdown = write_exact(
            &private,
            binding,
            &format!("{prefix}/plan.md"),
            markdown.as_bytes(),
        )?;
        Ok(HostGoalMaterialization {
            json_path: json.path,
            json_hash: json.sha256,
            markdown_path: markdown.path,
            markdown_hash: markdown.sha256,
            verified_at: now(),
        })
    })
    .map_err(|_| partial("goal_plan_publication_incomplete", &plan.goal_id))
}

fn verify_plan(
    db: &Database,
    binding: &VerifiedAgentRun,
    plan: &HostGoalPlanVersion,
) -> Result<(), ToolResult> {
    let receipt = plan
        .materialization
        .as_ref()
        .ok_or_else(|| denied("goal_plan_not_materialized"))?;
    let private = files(db)?;
    db.with_agent_run_for_goal(binding, "goal_plan_verify", &plan.goal_id, || {
        for (path, expected) in [
            (&receipt.json_path, &receipt.json_hash),
            (&receipt.markdown_path, &receipt.markdown_hash),
        ] {
            let current = private
                .read(binding, path)
                .map_err(|_| AgentIsolationError::new("goal_materialization_changed"))?;
            if &current.sha256 != expected {
                return Err(AgentIsolationError::new("goal_materialization_changed"));
            }
        }
        Ok(())
    })
    .map_err(|error| denied(error.code()))
}

fn bind_goal(
    db: &Database,
    binding: &VerifiedAgentRun,
    goal: &HostGoalRecord,
    context: &HostGoalInvocationContext,
) -> Result<(), ToolResult> {
    db.bind_host_goal_window_in_context(
        &binding.host_goal_owner_key(),
        binding.run_id(),
        &HostGoalWindowBinding {
            goal_id: goal.goal_id.clone(),
            project: goal.project.clone(),
            project_root: goal.project_root.clone(),
            context_epoch: binding.run_id().into(),
        },
        Some(context),
    )
    .map_err(|_| denied("goal_binding_unavailable"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointPatch {
    state: Option<Value>,
    reason: String,
    #[serde(default = "checkpoint_action")]
    action: String,
    #[serde(default)]
    operation_resolutions: Vec<Value>,
}
fn checkpoint_action() -> String {
    "checkpoint".into()
}

fn merge_state(
    current: &HostGoalState,
    patch: Option<&Value>,
) -> Result<HostGoalState, ToolResult> {
    let Some(patch) = patch else {
        return Ok(current.clone());
    };
    let mut state = json!(current);
    for (key, value) in patch
        .as_object()
        .ok_or_else(|| denied("invalid_goal_checkpoint"))?
    {
        if key == "facts" {
            for (key, value) in value
                .as_object()
                .ok_or_else(|| denied("invalid_goal_checkpoint"))?
            {
                if value.is_null() {
                    state["facts"].as_object_mut().unwrap().remove(key);
                } else {
                    state["facts"][key] = value.clone();
                }
            }
        } else {
            state[key] = value.clone();
        }
    }
    let state: HostGoalState =
        serde_json::from_value(state).map_err(|_| denied("invalid_goal_checkpoint"))?;
    state
        .validate()
        .map_err(|_| denied("invalid_goal_checkpoint"))?;
    Ok(state)
}

pub(crate) fn call(
    db: &Database,
    binding: &VerifiedAgentRun,
    name: &str,
    arguments: Value,
) -> ToolResult {
    if name == "goal_handoff" {
        return crate::agent_goal_handoff::call_active(db, binding, arguments);
    }
    match call_inner(db, binding, name, arguments) {
        Ok(value) => ToolResult::ok(value),
        Err(error) => error,
    }
}

fn call_inner(
    db: &Database,
    binding: &VerifiedAgentRun,
    name: &str,
    mut arguments: Value,
) -> Result<Value, ToolResult> {
    db.validate_agent_run(binding)
        .map_err(|error| denied(error.code()))?;
    if !supports(name) {
        return Err(denied("unknown_goal_tool"));
    }
    for field in [
        "agent_id",
        "owner_user",
        "run_id",
        "context_epoch",
        "identity_source",
        "project_root",
        "goal_context",
    ] {
        if arguments.get(field).is_some_and(|value| !value.is_null()) {
            return Err(denied("agent_goal_identity_server_owned"));
        }
    }
    if name == "goal_mode_start" && arguments.get("project").is_none_or(Value::is_null) {
        if let Some(object) = arguments.as_object_mut() {
            object.insert("project".into(), json!(binding.project()));
        }
    }
    let request = ToolCall::from_tool_name(name, arguments.clone())
        .map_err(|_| denied("invalid_goal_input"))?;
    let thresholds = match &request {
        ToolCall::GoalModeStart {
            context_warning_tokens,
            context_handoff_tokens,
            ..
        } if context_warning_tokens.is_some() || context_handoff_tokens.is_some() => Some((
            context_warning_tokens.unwrap_or(600_000),
            context_handoff_tokens.unwrap_or(800_000),
        )),
        _ => None,
    };
    let context = db
        .ensure_agent_host_context(binding, thresholds)
        .map_err(storage)?;
    let owner = binding.host_goal_owner_key();
    match request {
        ToolCall::GoalModeStart {
            project,
            task_directory,
            objective,
            idempotency_key,
            ..
        } => {
            if project != binding.project() {
                return Err(denied("goal_project_mismatch"));
            }
            let enrolled_directory = db
                .agent_enrollment_task_directory(binding)
                .map_err(|error| denied(error.code()))?;
            let task_directory = match (enrolled_directory, task_directory) {
                (Some(authorized), Some(requested)) if authorized != requested => {
                    return Err(denied("agent_assignment_directory_denied"));
                }
                (Some(authorized), _) => Some(authorized),
                (None, requested) => requested,
            };
            let root = format!("agent-private://{}", binding.agent_id());
            let created = match task_directory.as_deref() {
                Some(directory) => db.create_host_goal_in_task_directory_in_context(
                    &owner,
                    &project,
                    &root,
                    directory,
                    &objective,
                    &idempotency_key,
                    now(),
                    Some(&context),
                ),
                None => db.create_host_goal_at_in_context(
                    &owner,
                    &project,
                    &root,
                    &objective,
                    &idempotency_key,
                    now(),
                    Some(&context),
                ),
            }
            .map_err(storage)?;
            bind_goal(db, binding, &created.goal, &context)
                .map_err(|_| partial("goal_context_binding_incomplete", &created.goal.goal_id))?;
            Ok(json!({"goal":created.goal,"replayed":created.replayed,"mode_binding":"agent_run"}))
        }
        ToolCall::GoalPlanSave {
            goal_id,
            expected_revision,
            idempotency_key,
            plan,
            change_request,
        } => {
            db.read_host_goal_for_agent_run(binding, &goal_id)
                .map_err(storage)?;
            let plan: HostGoalPlan =
                serde_json::from_value(plan).map_err(|_| denied("invalid_goal_plan"))?;
            let saved = db
                .save_host_goal_plan_in_context(
                    &owner,
                    &goal_id,
                    expected_revision,
                    &plan,
                    change_request.as_deref(),
                    &idempotency_key,
                    now(),
                    Some(&context),
                )
                .map_err(storage)?;
            let (current, existing, _) = db
                .read_host_goal_for_agent_run(binding, &goal_id)
                .map_err(|_| partial("goal_plan_publication_incomplete", &goal_id))?;
            let existing = existing.ok_or_else(|| denied("goal_plan_not_found"))?;
            if existing.version != saved.plan.version {
                return Err(denied("goal_plan_superseded"));
            }
            if existing.materialization.is_some() {
                verify_plan(db, binding, &existing)?;
                return Ok(json!({"goal":current,"plan":existing,"replayed":saved.replayed}));
            }
            let receipt = publish_plan(db, binding, &saved.plan)?;
            let published = db
                .materialize_host_goal_plan_in_context(
                    &owner,
                    &goal_id,
                    saved.plan.version,
                    &saved.plan.hash,
                    &receipt,
                    now(),
                    Some(&context),
                )
                .map_err(|_| partial("goal_plan_publication_incomplete", &goal_id))?;
            Ok(json!({"goal":published.goal,"plan":published.plan,"replayed":saved.replayed}))
        }
        ToolCall::GoalConfirmPlan {
            goal_id,
            expected_revision,
            plan_version,
            plan_sha256,
            confirmation,
            idempotency_key,
        } => {
            let (_, plan, _) = db
                .read_host_goal_for_agent_run(binding, &goal_id)
                .map_err(storage)?;
            verify_plan(
                db,
                binding,
                &plan.ok_or_else(|| denied("goal_plan_not_found"))?,
            )?;
            let result = db
                .confirm_host_goal_plan_in_context(
                    &owner,
                    &goal_id,
                    expected_revision,
                    plan_version,
                    &plan_sha256,
                    &confirmation,
                    &idempotency_key,
                    now(),
                    Some(&context),
                )
                .map_err(storage)?;
            Ok(json!({"goal":result.goal,"replayed":result.replayed}))
        }
        ToolCall::GoalBootstrap {
            goal_id,
            project,
            activate,
            ..
        } => {
            let id = match goal_id {
                Some(id) => id,
                None => db
                    .agent_enrollment_goal(binding)
                    .map_err(|error| denied(error.code()))?
                    .ok_or_else(|| denied("explicit_goal_id_required"))?,
            };
            if project
                .as_deref()
                .is_some_and(|project| project != binding.project())
            {
                return Err(denied("goal_project_mismatch"));
            }
            let (goal, plan, attempt) = db
                .read_host_goal_for_agent_run(binding, &id)
                .map_err(storage)?;
            if activate {
                db.select_current_host_goal_in_context(&owner, &id, now(), Some(&context))
                    .map_err(storage)?;
                bind_goal(db, binding, &goal, &context)?;
            }
            Ok(
                json!({"goal":goal,"plan":plan,"attempt":attempt,"context_epoch":binding.run_id(),"mode_binding":"agent_run"}),
            )
        }
        ToolCall::GoalRead {
            goal_id,
            view,
            node_id,
            round_offset,
            round_limit,
            history_after,
            history_limit,
            history_kind,
            source_context_epoch,
            section,
            cursor,
        } => {
            if round_offset.is_some()
                || round_limit.is_some()
                || history_after.is_some()
                || history_limit.is_some()
                || history_kind.is_some()
                || source_context_epoch.is_some()
                || section.is_some()
                || cursor.is_some()
            {
                return Err(denied("agent_goal_view_unavailable"));
            }
            let (goal, plan, attempt) = db
                .read_host_goal_for_agent_run(binding, &goal_id)
                .map_err(storage)?;
            match view.as_deref().unwrap_or("summary") {
                "summary" => Ok(json!({"goal":goal,"projection":"summary"})),
                "detail" => {
                    Ok(json!({"goal":goal,"plan":plan,"attempt":attempt,"projection":"detail"}))
                }
                "node" => {
                    let node_id = node_id.ok_or_else(|| denied("goal_node_id_required"))?;
                    let plan = plan.ok_or_else(|| denied("goal_plan_not_found"))?;
                    let node = plan
                        .plan
                        .nodes
                        .iter()
                        .find(|node| node.id == node_id)
                        .ok_or_else(|| denied("goal_node_not_found"))?;
                    let response = json!({"projection":"node_context","goal":{"goal_id":goal.goal_id,"revision":goal.revision,"plan_version":goal.plan_version,"phase":goal.phase},
                        "requirements":{"objective":plan.plan.objective,"constraints":plan.plan.constraints,"deliverables":plan.plan.deliverables,
                            "final_criteria":plan.plan.final_criteria,"state_management":plan.plan.state_management,"verification_paths":plan.plan.verification_paths},
                        "node":node,"state":goal.state,"context_read":{"complete":true,"goal_id":goal.goal_id,"revision":goal.revision,"plan_version":goal.plan_version,"node_id":node_id}});
                    if response.to_string().len() > 14848 {
                        return Err(denied("goal_node_context_too_large"));
                    }
                    db.ack_agent_host_goal_context_read(
                        binding,
                        &goal_id,
                        goal.revision,
                        goal.plan_version,
                        &node_id,
                    )
                    .map_err(storage)?;
                    Ok(response)
                }
                _ => Err(denied("agent_goal_view_unavailable")),
            }
        }
        ToolCall::GoalClaimNode {
            goal_id,
            node_id,
            expected_revision,
            plan_version,
            idempotency_key,
            rework,
            takeover_attempt_id,
            ..
        } => {
            let (_, plan, _) = db
                .read_host_goal_for_agent_run(binding, &goal_id)
                .map_err(storage)?;
            verify_plan(
                db,
                binding,
                &plan.ok_or_else(|| denied("goal_plan_not_found"))?,
            )?;
            let result = db
                .claim_host_goal_node_for_plan_in_context(
                    &owner,
                    &goal_id,
                    expected_revision,
                    &node_id,
                    rework,
                    plan_version,
                    takeover_attempt_id.as_deref(),
                    &idempotency_key,
                    now(),
                    Some(&context),
                )
                .map_err(storage)?;
            Ok(
                json!({"goal":result.goal,"attempt":result.attempt,"replayed":result.replayed,
                    "workspace_write_allowed":true,"execution_adapters_available":false,
                    "execution_note":"Workspace writes are limited to this Goal's task directory and require this run and node claim to remain current. CLI, process and plugin execution remain unavailable."}),
            )
        }
        ToolCall::GoalCheckpoint {
            goal_id,
            expected_revision,
            idempotency_key,
            patch,
            ..
        } => {
            let request_hash = hash(
                &serde_json::to_vec(&(&goal_id, expected_revision, &patch))
                    .map_err(|_| denied("invalid_goal_checkpoint"))?,
            );
            let patch: CheckpointPatch =
                serde_json::from_value(patch).map_err(|_| denied("invalid_goal_checkpoint"))?;
            if !patch.operation_resolutions.is_empty() {
                return Err(denied("goal_operation_resolution_unsupported"));
            }
            let action = match patch.action.as_str() {
                "checkpoint" => HostGoalCheckpointAction::Progress,
                "pause" => HostGoalCheckpointAction::Pause,
                "exit" => HostGoalCheckpointAction::Exit,
                "block" => HostGoalCheckpointAction::Block,
                "cancel" => HostGoalCheckpointAction::Cancel,
                "handoff" => HostGoalCheckpointAction::Handoff,
                _ => return Err(denied("invalid_goal_checkpoint")),
            };
            let (goal, _, attempt) = db
                .read_host_goal_for_agent_run(binding, &goal_id)
                .map_err(storage)?;
            let state = merge_state(&goal.state, patch.state.as_ref())?;
            let result = db
                .checkpoint_host_goal_with_request_in_context(
                    &owner,
                    &goal_id,
                    expected_revision,
                    attempt.as_ref().map(|attempt| &attempt.binding),
                    &state,
                    action,
                    &patch.reason,
                    &idempotency_key,
                    &request_hash,
                    &[],
                    now(),
                    Some(&context),
                    &[],
                )
                .map_err(storage)?;
            if matches!(action, HostGoalCheckpointAction::Handoff) {
                return crate::agent_goal_handoff::publish_checkpoint(
                    db,
                    binding,
                    &result.goal,
                    result.replayed,
                );
            }
            let private = files(db)
                .map_err(|_| partial("goal_checkpoint_materialization_incomplete", &goal_id))?;
            let path = format!("checkpoints/{}/r{}.json", goal_id, result.goal.revision);
            let bytes = serde_json::to_vec_pretty(&result.goal)
                .map_err(|_| partial("goal_checkpoint_materialization_incomplete", &goal_id))?;
            db.with_agent_run_for_goal(binding, "goal_checkpoint_publish", &goal_id, || {
                private
                    .create(binding)
                    .map_err(|_| AgentIsolationError::new("agent_private_access_denied"))?;
                write_exact(&private, binding, &path, &bytes)
            })
            .map_err(|_| partial("goal_checkpoint_materialization_incomplete", &goal_id))?;
            Ok(json!({"goal":result.goal,"replayed":result.replayed,"checkpoint_path":path}))
        }
        _ => Err(denied("unknown_goal_tool")),
    }
}
