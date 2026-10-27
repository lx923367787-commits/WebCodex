//! Bounded model projections. A summary is never a writable GoalState snapshot.
use super::{
    bounded, context, failure, files, store_error, HostGoalPhase, HostGoalRecord, ToolResult,
    ToolRuntime,
};
use crate::auth::AuthContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const SUMMARY_BYTES: usize = 6656;
const NODE_BYTES: usize = 14848;

pub(in crate::tool_runtime) fn repair_summary(
    repair: &webcodex_core::host_goal::HostGoalRepair,
) -> Value {
    use webcodex_core::host_goal::*;
    let exhausted = repair.exhausted(super::now());
    let (failure, failure_truncated) =
        super::super::startup_brief::bounded_json_string(&repair.failure, 512);
    json!({"repair_id":repair.repair_id,"node_id":repair.node_id,"plan_version":repair.plan_version,
        "status":repair.status,"business_suspended":repair.active(),"exhausted":exhausted,
        "rounds_used":repair.rounds.len(),"max_rounds":HOST_GOAL_REPAIR_MAX_ROUNDS,
        "operations_used":repair.operations,"max_operations":HOST_GOAL_REPAIR_MAX_OPERATIONS,
        "deadline_at":repair.deadline_at,"failure":failure,"failure_truncated":failure_truncated,
        "details":{"tool":"goal_read","arguments":{"goal_id":repair.goal_id,"view":"detail","section":"repair"}},
        "goal_context_repair":{"repair_id":repair.repair_id},
        "next_action":if !repair.active() {"Original acceptance passed; continue the original plan with the current node binding and omit repair metadata."}
        else if exhausted {"Keep affected business suspended. Report the preserved failure and exhausted budget for an operator decision; do not change IDs or weaken acceptance."}
        else {"Read/claim the affected node. Add goal_context.repair with this repair_id and diagnosis; repair within the confirmed plan, then goal_check_node with the same context. Only original acceptance passing releases business; budgets persist."}})
}

// Entry guidance only: never persist it in Goal state or operation receipts.
const WORKFLOW_OVERVIEW: &str = concat!(
    "Map only, not a restart or permission. Follow goal.phase, confirmation, continuation, node context, bindings, acceptance results and context_meter restrictions.\n",
    "New goal: read inputs -> goal_plan_save -> wait for explicit user confirmation of that version -> goal_confirm_plan -> goal_read(view=node) -> goal_claim_node -> execute with the current binding -> goal_check_node -> next node or completion.\n",
    "Resume: read the same Goal's local state, plan and recovery instructions via goal_read; reconcile unresolved operations with original evidence, then continue from the actual phase. A new chat does not require reconfirming an unchanged confirmed plan; plan changes still follow existing reconfirmation rules.\n",
    "Phase map: planning = inspect inputs/refine the plan; awaiting_confirmation = read-only investigation, no business execution; ready/executing/verifying = follow returned node-read, claim and acceptance requirements, without restarting confirmed steps. If recovery.business_suspended=true, diagnose and repair the affected node autonomously with goal_context.repair, then rerun original acceptance; do not resume other business before it passes. Recovery budgets survive claims and handoff. paused/blocked/cancelled = obey existing recovery gates; completed = report completion, never choose replacement work. context_meter lifecycle/notice takes precedence: draining/retired forbid new business/claims or reactivation.\n",
    "Boundaries: saving a plan is not execution approval; never fabricate or infer user confirmation. Goal-bound business calls use the current node, Attempt/Fence and goal_context. Unknown results: check the original Operation, Job, Run or receipt before retrying effects. Keep chat identity/context_epoch fixed; never change identifiers to bypass gates. Verify saving from tool receipts; completion requires passed acceptance, not model assertions. Handoff saves a task checkpoint and freezes source business. The scheduler starts a fresh conversation, which prepares and commits through goal_handoff. Only server-confirmed ownership permits continuation; source retirement occurs atomically with transfer. Old execution chains must have stopped. Four failed attempts stop execution and preserve manual recovery state."
);

pub(super) fn entry(
    runtime: &ToolRuntime,
    goal: &HostGoalRecord,
    replayed: Option<bool>,
) -> Result<Value, ToolResult> {
    let mut value = summary(runtime, goal, replayed)?;
    value["workflow_overview"] = json!(WORKFLOW_OVERVIEW);
    fit_summary(&mut value, SUMMARY_BYTES)?;
    Ok(value)
}

fn header(goal: &HostGoalRecord) -> Value {
    json!({"goal_id":goal.goal_id,"project":goal.project,"objective":bounded(&goal.objective,512),
        "task_directory":goal.task_directory,
        "phase":goal.phase,"revision":goal.revision,"plan_version":goal.plan_version,
        "acceptance_contract_version":goal.acceptance_contract_version,"plan_hash":goal.plan_hash,
        "plan_materialized":goal.plan_materialized,"confirmation":goal.confirmation.as_ref().map(|c|json!({"plan_version":c.plan_version,"plan_hash":c.plan_hash,"confirmed_at":c.confirmed_at})),
        "current_node_id":goal.current_node_id,"current_attempt_id":goal.current_attempt_id,
        "fence":goal.fence,"completed_node_ids":goal.completed_node_ids,"completed_node_count":goal.completed_node_ids.len(),
        "created_at":goal.created_at,"updated_at":goal.updated_at})
}

fn progress(goal: &HostGoalRecord) -> Value {
    json!({"partial":true,"completed_summary":bounded(&goal.state.completed_summary,350),
        "next_step":bounded(&goal.state.next_step,350),
        "issues":goal.state.issues.iter().take(3).map(|s|bounded(s,160)).collect::<Vec<_>>(),
        "counts":{"facts":goal.state.facts.len(),"decisions":goal.state.decisions.len(),"issues":goal.state.issues.len(),"artifacts":goal.state.artifact_refs.len()}})
}

fn size(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
}

fn fit_summary(value: &mut Value, budget: usize) -> Result<(), ToolResult> {
    if size(value) > budget {
        value["plan_artifacts"] = Value::Null;
    }
    if size(value) > budget {
        value["progress_summary"]["issues"] = json!([]);
    }
    if size(value) > budget {
        value["progress_summary"]["completed_summary"] = json!("");
        value["progress_summary"]["next_step"] =
            json!("Read the indicated node or detail section before continuing.");
    }
    while size(value) > budget {
        let Some(ids) = value["goal"]["completed_node_ids"].as_array_mut() else {
            break;
        };
        if ids.pop().is_none() {
            break;
        }
        value["goal"]["completed_node_ids_partial"] = json!(true);
    }
    if size(value) > budget {
        return Err(failure(
            "goal_projection_too_large",
            "The goal header exceeds its bounded projection; read the requested detail section.",
        ));
    }
    Ok(())
}

/// Reserve the public 16 KiB budget for the *final* node output, including the
/// kernel's repair and context-meter overlays. Never truncate requirements,
/// execution bindings, repair budgets, or acceptance. Full failure evidence
/// remains in the existing paged repair detail.
pub(super) fn fit_node_response(value: &mut Value) -> Result<(), ToolResult> {
    const RESPONSE_BYTES: usize = 16 * 1024;
    if size(value) <= RESPONSE_BYTES {
        return Ok(());
    }
    // Repeated derived paths are available from the Goal summary; the exact
    // task directory remains in the Goal header.
    if let Some(object) = value.as_object_mut() {
        object.remove("task_files");
    }
    if fit_summary(value, RESPONSE_BYTES).is_ok() {
        return Ok(());
    }
    if let Some(failure) = value["recovery"]["failure"].as_str().map(str::to_owned) {
        value["recovery"]["failure"] = json!("");
        value["recovery"]["failure_truncated"] = json!(true);
        let remaining = RESPONSE_BYTES.saturating_sub(size(value));
        let (excerpt, _) = super::super::startup_brief::bounded_json_string(&failure, remaining);
        value["recovery"]["failure"] = json!(excerpt);
    }
    fit_summary(value, RESPONSE_BYTES)
}

pub(super) fn summary(
    runtime: &ToolRuntime,
    goal: &HostGoalRecord,
    replayed: Option<bool>,
) -> Result<Value, ToolResult> {
    let db = runtime.host_goal_db()?;
    let current = if replayed == Some(true) {
        db.get_host_goal(&goal.owner_key, &goal.goal_id)
            .map_err(store_error)?
    } else {
        None
    };
    let goal = current.as_ref().unwrap_or(goal);
    let plan = db
        .get_host_goal_plan(&goal.owner_key, &goal.goal_id, None)
        .map_err(store_error)?;
    let next_node = plan.as_ref().and_then(|p| {
        if matches!(
            goal.phase,
            HostGoalPhase::Executing | HostGoalPhase::Verifying
        ) {
            p.plan
                .nodes
                .iter()
                .find(|n| Some(&n.id) == goal.current_node_id.as_ref())
        } else {
            p.plan
                .nodes
                .iter()
                .find(|n| !goal.completed_node_ids.contains(&n.id))
                .or_else(|| p.plan.nodes.last())
        }
    });
    let continuation = if let Some(node) = next_node {
        json!({"tool":"goal_read","arguments":{"goal_id":goal.goal_id,"view":"node","node_id":node.id}})
    } else {
        json!({"tool":"goal_read","arguments":{"goal_id":goal.goal_id,"view":"detail","section":"goal"}})
    };
    let operations = db
        .list_host_goal_operations(&goal.owner_key, &goal.goal_id, 0, 100, true)
        .map_err(store_error)?;
    // Runner files need authenticated asynchronous I/O. Explicit goal_read
    // checks them below; a cached publication receipt is not a current check.
    let valid = if goal.task_directory.is_none() {
        plan.as_ref()
            .and_then(|p| p.materialization.as_ref())
            .map(|materialized| {
                db.host_goal_plan_root().ok().is_some_and(|root| {
                    files::verify(
                        &root,
                        &goal.goal_id,
                        goal.plan_version,
                        &super::artifacts(materialized),
                    )
                    .is_ok()
                })
            })
    } else {
        None
    };
    let mut value = json!({"driver":"host","projection":"summary","goal":header(goal),"progress_summary":progress(goal),
        "continuation":continuation,"unresolved_operation_count":operations.len(),"plan_artifacts_valid":valid,
        "plan_artifacts":plan.as_ref().and_then(|p|p.materialization.as_ref()).map(|p|json!({"json_path":p.json_path,"markdown_path":p.markdown_path})),
        "details":{"tool":"goal_read","arguments":{"goal_id":goal.goal_id,"view":"detail"},"sections":["goal","plan","state","checkpoints","attempts","operations","repair"]}});
    if let Some(replayed) = replayed {
        value["replayed"] = json!(replayed);
    }
    if let Some(repair) = db
        .get_host_goal_repair(&goal.owner_key, &goal.goal_id)
        .map_err(store_error)?
    {
        value["recovery"] = repair_summary(&repair);
    }
    if goal.task_directory.is_some() {
        value["task_files"] = super::super::host_goal_project_files::task_paths(goal);
    }
    if let Some(node) = next_node {
        let review = db
            .read_host_goal_review(&goal.owner_key, &goal.goal_id, &node.id)
            .map_err(store_error)?;
        value["review"] = super::super::host_goal_review::review_summary(review.as_ref(), node);
    }
    fit_summary(&mut value, SUMMARY_BYTES)?;
    if let Ok(Some(meter)) = db.read_host_goal_context_meter(&goal.owner_key, &goal.goal_id) {
        value["context_meter"] = context::meter_projection(Some(meter), None);
    }
    Ok(value)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    goal_id: String,
    section: String,
    revision: u64,
    plan_version: u64,
    offset: usize,
    sha256: String,
    after: Option<u64>,
    limit: usize,
}

impl ToolRuntime {
    pub(super) fn host_goal_handoff_projection(
        &self,
        goal: &HostGoalRecord,
        source: &str,
    ) -> Result<Value, ToolResult> {
        use webcodex_core::host_goal::{HostGoalAttemptStatus, HostGoalOperationStatus};
        let db = self.host_goal_db()?;
        let snapshot = db
            .read_host_goal_handoff_snapshot(&goal.owner_key, &goal.goal_id, source)
            .map_err(store_error)?;
        let goal = &snapshot.goal;
        let state = snapshot.source;
        if state
            .as_ref()
            .and_then(|s| s.retirement_goal_id.as_deref())
            .is_some_and(|id| id != goal.goal_id)
        {
            return Err(failure(
                "goal_context_mismatch",
                "The source retirement belongs to a different Goal",
            ));
        }
        let checkpoint = snapshot.checkpoint;
        let attempt = snapshot.attempt;
        let source_fence = checkpoint.as_ref().and_then(|c| c.source_fence);
        let successor = attempt.as_ref().filter(|a| {
            a.context_epoch
                .as_deref()
                .is_some_and(|epoch| epoch != source)
                && source_fence.is_some_and(|fence| a.binding.fence > fence)
        });
        let operation = snapshot.business_operation.filter(|op| {
            successor.is_some_and(|a| {
                a.status == HostGoalAttemptStatus::Passed && op.binding == a.binding
            })
        });
        let evidence = operation.filter(|op| op.status == HostGoalOperationStatus::Succeeded)
            .map(|op| json!({"operation_id":op.operation_id,"attempt_id":op.binding.attempt_id,"status":"succeeded","check_passed":true}));
        let unresolved = snapshot.unresolved_operation_count;
        let mut value = json!({
            "driver":"host", "projection":"handoff", "goal":header(goal),
            "source_context":{
                "context_epoch":source,
                "identity_source":state.as_ref().and_then(|s|s.identity_source.as_deref()),
                "lifecycle_state":state.as_ref().map(|s|s.state.as_str()).unwrap_or("unknown"),
                "retired_at":state.as_ref().and_then(|s|s.retired_at)
            },
            "checkpoint":checkpoint.as_ref().map(|c|json!({"sequence":c.sequence,"revision":c.revision,"action":c.action,"source_context_epoch":c.source_context_epoch,"source_fence":c.source_fence,"created_at":c.created_at})),
            "current_attempt":attempt.as_ref().map(|a|json!({"binding":a.binding,"status":a.status,"context_epoch":a.context_epoch,"identity_source":a.identity_source,"started_at":a.started_at,"finished_at":a.finished_at})),
            "unresolved_operation_count":unresolved,
            "resume_started":snapshot.resume_started,
            "handoff":db.read_host_goal_handoff(&goal.owner_key,&goal.goal_id).map_err(store_error)?,
            "business_verified":evidence.is_some() && state.as_ref().is_some_and(|s|s.state=="retired") && checkpoint.as_ref().is_some_and(|c|c.action.as_deref()==Some("handoff")),
            "business_evidence":evidence
        });
        if goal.task_directory.is_some() {
            let task_files = super::super::host_goal_project_files::task_paths(goal);
            value["task_files"] = task_files.clone();
            if let Some(checkpoint) = checkpoint.as_ref() {
                let pair = super::super::host_goal_project_files::handoff_files(goal, checkpoint)?;
                value["handoff_artifacts"] = json!({"checkpoint_sequence":checkpoint.sequence,
                    "json_path":pair[0].absolute_path,"json_sha256":pair[0].sha256,
                    "markdown_path":pair[1].absolute_path,"markdown_sha256":pair[1].sha256,
                    "task_files":task_files,"publication":"not_checked"});
            }
        }
        // The handoff schema is closed and has no progress/plan_artifacts
        // fields. Trim only its own optional completed-node projection.
        while size(&value) > SUMMARY_BYTES {
            let ids = value["goal"]["completed_node_ids"].as_array_mut().unwrap();
            if ids.pop().is_none() {
                return Err(failure(
                    "goal_projection_too_large",
                    "Handoff evidence exceeds its bounded projection",
                ));
            }
            value["goal"]["completed_node_ids_partial"] = json!(true);
        }
        Ok(value)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn bounded_host_goal_read(
        &self,
        goal: &HostGoalRecord,
        after: Option<u64>,
        limit: Option<usize>,
        kind: Option<&str>,
        view: Option<&str>,
        node_id: Option<&str>,
        section: Option<&str>,
        cursor: Option<&str>,
        source_context_epoch: Option<&str>,
        auth: Option<&AuthContext>,
    ) -> Result<Value, ToolResult> {
        if limit.is_some_and(|n| !(1..=100).contains(&n)) {
            return Err(failure("invalid_goal_page", "history_limit must be 1..100"));
        }
        let selected_view = view.unwrap_or(if kind.is_some() || after.is_some() {
            "detail"
        } else {
            "summary"
        });
        if source_context_epoch.is_some() && selected_view != "handoff" {
            return Err(failure(
                "invalid_goal_view",
                "source_context_epoch is only a handoff observation filter",
            ));
        }
        match selected_view {
            "handoff" => {
                if node_id.is_some()
                    || section.is_some()
                    || cursor.is_some()
                    || after.is_some()
                    || limit.is_some()
                    || kind.is_some()
                {
                    return Err(failure(
                        "invalid_goal_view",
                        "handoff accepts only a source_context_epoch filter",
                    ));
                }
                let source = source_context_epoch
                    .filter(|v| !v.trim().is_empty() && v.len() <= 128 && !v.contains('\0'))
                    .ok_or_else(|| {
                        failure(
                            "invalid_goal_view",
                            "handoff requires a bounded source_context_epoch",
                        )
                    })?;
                self.host_goal_handoff_projection(goal, source)
            }
            "summary" => {
                if node_id.is_some() || section.is_some() || cursor.is_some() {
                    return Err(failure(
                        "invalid_goal_view",
                        "summary does not accept node_id, section or cursor",
                    ));
                }
                let mut output = summary(self, goal, None)?;
                if goal.task_directory.is_some() && goal.plan_materialized {
                    output["plan_artifacts_valid"] =
                        json!(self.verify_host_goal_plan(goal, auth).await.is_ok());
                }
                let freshness = if goal.phase == HostGoalPhase::Completed {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        self.host_goal_delivery_freshness(goal, auth),
                    )
                    .await
                    .unwrap_or("unavailable")
                } else {
                    "not_completed"
                };
                output["delivery_freshness"] = json!(freshness);
                output["delivery_ready"] =
                    json!(freshness == "current" && output["plan_artifacts_valid"] == true);
                Ok(output)
            }
            "node" => {
                if section.is_some() || cursor.is_some() || after.is_some() || kind.is_some() {
                    return Err(failure(
                        "invalid_goal_view",
                        "node does not accept detail or history pagination fields",
                    ));
                }
                let plan = self.host_goal_plan(goal)?;
                let node = if let Some(id) = node_id {
                    plan.plan.nodes.iter().find(|n| n.id == id)
                } else {
                    let current = plan
                        .plan
                        .nodes
                        .iter()
                        .find(|n| Some(&n.id) == goal.current_node_id.as_ref());
                    if matches!(
                        goal.phase,
                        HostGoalPhase::Executing | HostGoalPhase::Verifying
                    ) {
                        current
                    } else {
                        plan.plan
                            .nodes
                            .iter()
                            .find(|n| !goal.completed_node_ids.contains(&n.id))
                            .or(current)
                    }
                }
                .ok_or_else(|| failure("goal_node_missing", "Select a node from the saved plan"))?;
                let mut goal_header = header(goal);
                goal_header["completed_node_ids"] = json!([]);
                goal_header["completed_node_ids_partial"] = json!(true);
                goal_header["objective"] = json!(bounded(&goal.objective, 128));
                let mut value = json!({"driver":"host","projection":"node_context","goal":goal_header,
                    "requirements":{"objective":plan.plan.objective,"constraints":plan.plan.constraints,"deliverables":plan.plan.deliverables,"final_criteria":plan.plan.final_criteria,"state_management":plan.plan.state_management,"verification_paths":plan.plan.verification_paths},
                    "node":node,"progress_summary":progress(goal),
                    "context_read":{"goal_id":goal.goal_id,"revision":goal.revision,"plan_version":goal.plan_version,"node_id":node.id,"complete":true},
                    "details":{"tool":"goal_read","arguments":{"goal_id":goal.goal_id,"view":"detail","section":"state"}}});
                if goal.task_directory.is_some() {
                    value["task_files"] = super::super::host_goal_project_files::task_paths(goal);
                }
                let review = self
                    .host_goal_db()?
                    .read_host_goal_review(&goal.owner_key, &goal.goal_id, &node.id)
                    .map_err(store_error)?;
                value["review"] =
                    super::super::host_goal_review::review_summary(review.as_ref(), node);
                // The kernel attaches the current repair gate once, after this
                // lossless requirements projection, before acknowledging it.
                fit_summary(&mut value, NODE_BYTES)?;
                Ok(value)
            }
            "detail" => {
                if node_id.is_some() {
                    return Err(failure(
                        "invalid_goal_view",
                        "detail uses section and cursor, not node_id",
                    ));
                }
                if section.is_none() && kind.is_none() && after.is_none() {
                    return Err(failure("invalid_goal_view", "detail requires a section"));
                }
                self.host_goal_detail_page(
                    goal,
                    section.or(kind).unwrap_or("checkpoints"),
                    cursor,
                    after.or_else(|| kind.map(|_| 0)),
                    limit,
                )
            }
            _ => Err(failure(
                "invalid_goal_view",
                "view must be summary, node, detail or handoff",
            )),
        }
    }

    fn host_goal_detail_page(
        &self,
        goal: &HostGoalRecord,
        section: &str,
        cursor: Option<&str>,
        after: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Value, ToolResult> {
        let prior = cursor
            .map(|text| {
                if text.len() > 2048 {
                    return Err(failure("invalid_goal_cursor", "Cursor exceeds its bound"));
                }
                serde_json::from_str::<Cursor>(text).map_err(|_| {
                    failure(
                        "invalid_goal_cursor",
                        "Use the exact next_cursor returned by goal_read",
                    )
                })
            })
            .transpose()?;
        if prior.as_ref().is_some_and(|c| {
            c.goal_id != goal.goal_id
                || c.section != section
                || c.revision != goal.revision
                || c.plan_version != goal.plan_version
                || after.is_some_and(|a| Some(a) != c.after)
                || limit.is_some_and(|n| n != c.limit)
        }) {
            return Err(failure("goal_cursor_stale","The cursor belongs to another target, section or goal revision; restart this section"));
        }
        let after = prior.as_ref().map(|c| c.after).unwrap_or(after);
        let limit = prior.as_ref().map(|c| c.limit).or(limit).unwrap_or(10);
        if !(1..=100).contains(&limit) {
            return Err(failure(
                "invalid_goal_cursor",
                "Invalid cursor history limit",
            ));
        }
        let db = self.host_goal_db()?;
        let material = match section {
            "goal" => json!(goal),
            "state" => json!(goal.state),
            "repair" => json!(db
                .get_host_goal_repair(&goal.owner_key, &goal.goal_id)
                .map_err(store_error)?),
            "reviews" => {
                let plan = self.host_goal_plan(goal)?;
                let mut reviews = Vec::new();
                for node in &plan.plan.nodes {
                    reviews.push(json!({"node_id":node.id,"review":db.read_host_goal_review(&goal.owner_key,&goal.goal_id,&node.id).map_err(store_error)?}));
                }
                json!({"items":reviews,"notice":"Missing history is not recorded, never inferred approval. New contexts must prepare and receive images again."})
            }
            "plan" => json!(db
                .get_host_goal_plan(&goal.owner_key, &goal.goal_id, None)
                .map_err(store_error)?),
            "checkpoints" => {
                let values = if let Some(after) = after {
                    db.list_host_goal_checkpoints(&goal.owner_key, &goal.goal_id, after, limit)
                        .map_err(store_error)?
                } else {
                    db.latest_host_goal_checkpoint(&goal.owner_key, &goal.goal_id)
                        .map_err(store_error)?
                        .into_iter()
                        .collect()
                };
                let next = values.last().map(|v| v.sequence);
                let more = after.is_some()
                    && next.is_some_and(|n| {
                        db.list_host_goal_checkpoints(&goal.owner_key, &goal.goal_id, n, 1)
                            .is_ok_and(|v| !v.is_empty())
                    });
                json!({"items":values,"has_more":more,"next_sequence":if more{next}else{None}})
            }
            "attempts" => {
                let values = if let Some(after) = after {
                    db.list_host_goal_attempts(&goal.owner_key, &goal.goal_id, after, limit)
                        .map_err(store_error)?
                } else {
                    db.latest_host_goal_attempt(&goal.owner_key, &goal.goal_id)
                        .map_err(store_error)?
                        .into_iter()
                        .collect()
                };
                let next = values.last().map(|v| v.binding.fence);
                let more = after.is_some()
                    && next.is_some_and(|n| {
                        db.list_host_goal_attempts(&goal.owner_key, &goal.goal_id, n, 1)
                            .is_ok_and(|v| !v.is_empty())
                    });
                json!({"items":values,"has_more":more,"next_sequence":if more{next}else{None}})
            }
            "operations" => {
                let values = db
                    .list_host_goal_operations(
                        &goal.owner_key,
                        &goal.goal_id,
                        after.unwrap_or(0),
                        limit,
                        after.is_none(),
                    )
                    .map_err(store_error)?;
                let next = values.last().map(|v| v.sequence);
                let more = next.is_some_and(|n| {
                    db.list_host_goal_operations(
                        &goal.owner_key,
                        &goal.goal_id,
                        n,
                        1,
                        after.is_none(),
                    )
                    .is_ok_and(|v| !v.is_empty())
                });
                json!({"items":values,"has_more":more,"next_sequence":if more{next}else{None}})
            }
            _ => {
                return Err(failure(
                    "invalid_goal_section",
                    "section must be goal, plan, state, checkpoints, attempts or operations",
                ))
            }
        };
        let text = serde_json::to_string(&material)
            .map_err(|e| failure("goal_projection_failed", e.to_string()))?;
        let digest = files::sha256(text.as_bytes());
        if prior.as_ref().is_some_and(|c| c.sha256 != digest) {
            return Err(failure(
                "goal_cursor_stale",
                "The selected material changed during pagination; restart this section",
            ));
        }
        let offset = prior.as_ref().map(|c| c.offset).unwrap_or(0);
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err(failure(
                "invalid_goal_cursor",
                "Cursor does not identify a valid text boundary",
            ));
        }
        let mut goal_header = header(goal);
        goal_header["completed_node_ids"] = json!([]);
        goal_header["completed_node_ids_partial"] = json!(true);
        goal_header["objective"] = json!(bounded(&goal.objective, 128));
        let mut end = (offset + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        loop {
            let next = if end < text.len() {
                Some(
                    serde_json::to_string(&Cursor {
                        goal_id: goal.goal_id.clone(),
                        section: section.into(),
                        revision: goal.revision,
                        plan_version: goal.plan_version,
                        offset: end,
                        sha256: digest.clone(),
                        after,
                        limit,
                    })
                    .map_err(|e| failure("goal_projection_failed", e.to_string()))?,
                )
            } else {
                None
            };
            let value = json!({"driver":"host","projection":"detail","goal":goal_header,"section":section,"encoding":"json_text","text":&text[offset..end],"next_cursor":next,"has_more":end<text.len(),"material_sha256":digest});
            if size(&value) <= SUMMARY_BYTES {
                return Ok(value);
            }
            if end == offset {
                return Err(failure(
                    "goal_projection_too_large",
                    "The detail header exceeds the response budget",
                ));
            }
            end = offset + (end - offset) / 2;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
        }
    }
}
