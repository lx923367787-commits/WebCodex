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

fn header(goal: &HostGoalRecord) -> Value {
    json!({"goal_id":goal.goal_id,"project":goal.project,"objective":bounded(&goal.objective,512),
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
    let valid = plan
        .as_ref()
        .and_then(|p| p.materialization.as_ref())
        .map(|_| runtime.verify_host_goal_plan(goal).is_ok());
    let mut value = json!({"driver":"host","projection":"summary","goal":header(goal),"progress_summary":progress(goal),
        "continuation":continuation,"unresolved_operation_count":operations.len(),"plan_artifacts_valid":valid,
        "plan_artifacts":plan.as_ref().and_then(|p|p.materialization.as_ref()).map(|p|json!({"json_path":p.json_path,"markdown_path":p.markdown_path})),
        "details":{"tool":"goal_read","arguments":{"goal_id":goal.goal_id,"view":"detail"},"sections":["goal","plan","state","checkpoints","attempts","operations"]}});
    if let Some(replayed) = replayed {
        value["replayed"] = json!(replayed);
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
        match selected_view {
            "summary" => {
                if node_id.is_some() || section.is_some() || cursor.is_some() {
                    return Err(failure(
                        "invalid_goal_view",
                        "summary does not accept node_id, section or cursor",
                    ));
                }
                let mut output = summary(self, goal, None)?;
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
                "view must be summary, node or detail",
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
