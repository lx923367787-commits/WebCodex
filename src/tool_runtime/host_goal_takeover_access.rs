//! Handoff narrows authority to the transferred Goal. Until the Runner exposes
//! a canonical task-scoped file/process capability, unrelated tool surfaces
//! fail closed rather than inheriting the source conversation's access.
use super::host_goal::{failure, owner, store_error};
use super::host_goal_context as context;
use super::{ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use serde_json::Value;

fn isolation_unavailable() -> ToolResult {
    failure(
        "goal_handoff_isolation_unavailable",
        "This handoff grants access only to its saved Goal package and controls. A verified task-scoped Runner file/process capability is unavailable, so ordinary files, CLI, plugins, memory and other task data are denied.",
    )
}

fn status_request(tool: &str, args: &Value) -> bool {
    tool == "goal_handoff" && args["action"] == "status"
}

impl ToolRuntime {
    pub(crate) fn host_goal_takeover_access(
        &self,
        tool: &str,
        args: &Value,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        let Some(db) = self.goals.store() else {
            return Ok(());
        };
        let principal = match owner(auth) {
            Ok(principal) => principal,
            Err(_) if context::current_binding().is_none() && tool != "goal_handoff" => {
                return Ok(())
            }
            Err(error) => return Err(error),
        };
        let epoch = context::current_epoch();
        let related = epoch
            .as_deref()
            .map(|epoch| db.host_goal_handoff_for_context(&principal, epoch))
            .transpose()
            .map_err(store_error)?
            .flatten();

        // Failed candidates are revoked even after their attempt is replaced
        // and its candidate field no longer points to their conversation.
        if let Some(epoch) = epoch.as_deref() {
            if let Some(state) = db
                .read_host_context_state(&principal, epoch)
                .map_err(store_error)?
            {
                if matches!(state.state.as_str(), "retired" | "stopped") {
                    if status_request(tool, args)
                        && related
                            .as_ref()
                            .is_none_or(|h| args["goal_id"] == h.goal_id)
                    {
                        return Ok(());
                    }
                    return Err(failure(
                        "goal_handoff_context_revoked",
                        "This conversation has no execution or task-data authority after handoff retirement or failure. Only handoff status remains available.",
                    ));
                }
            }
        }

        let explicit_goal = args["goal_id"]
            .as_str()
            .or_else(|| args["goal_context"]["goal_id"].as_str());
        let targeted = if related.is_none() {
            if let Some(goal) = explicit_goal {
                if db
                    .get_host_goal(&principal, goal)
                    .map_err(store_error)?
                    .is_some()
                {
                    db.read_host_goal_handoff(&principal, goal)
                        .map_err(store_error)?
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let Some(handoff) = related.or(targeted) else {
            return Ok(());
        };
        if explicit_goal != Some(handoff.goal_id.as_str()) {
            return Err(isolation_unavailable());
        }
        if status_request(tool, args) {
            return Ok(());
        }
        if context::current_window().is_none() {
            // The scheduler's bounded attempt controls are separately checked
            // by the handoff handler; explicit model epochs are never owners.
            if tool == "goal_handoff"
                && matches!(
                    args["action"].as_str(),
                    Some("begin_attempt" | "fail_attempt" | "abort")
                )
            {
                return Ok(());
            }
            return Err(failure(
                "goal_handoff_identity_required",
                "Handoff task access requires the server-bound new conversation identity; a supplied context_epoch is not execution authority.",
            ));
        }
        let epoch = epoch.as_deref().ok_or_else(isolation_unavailable)?;
        if handoff.status == "failed" {
            return Err(failure(
                "goal_handoff_failed",
                "Handoff failed; execution stopped and requires explicit user recovery. Checkpoint and artifacts remain saved.",
            ));
        }
        if handoff.status != "committed" {
            if epoch == handoff.source_context_epoch {
                // Only a previously committed, byte-identical handoff request
                // may finish its immutable package publication during waiting.
                if tool == "goal_checkpoint" && args["patch"]["action"] == "handoff" {
                    let revision = args["expected_revision"].as_u64();
                    let key = args["idempotency_key"].as_str();
                    if let (Some(revision), Some(key)) = (revision, key) {
                        let hash = super::host_goal_files::sha256(
                            &serde_json::to_vec(&(
                                &handoff.goal_id,
                                revision,
                                &args["patch"],
                                Option::<String>::None,
                            ))
                            .map_err(|error| {
                                failure("invalid_goal_checkpoint", error.to_string())
                            })?,
                        );
                        if db
                            .lookup_host_goal_checkpoint_replay(
                                &principal,
                                &handoff.goal_id,
                                key,
                                &hash,
                            )
                            .map_err(store_error)?
                            .is_some()
                        {
                            return Ok(());
                        }
                    }
                }
                return Err(failure(
                    "goal_handoff_pending",
                    "The source conversation is waiting for takeover and may only query status or replay its exact saved handoff checkpoint.",
                ));
            }
            if tool == "goal_handoff"
                && matches!(args["action"].as_str(), Some("prepare" | "commit"))
            {
                // Store transactions validate the exact live attempt, deadline,
                // source inequality and binding to this trusted conversation.
                return Ok(());
            }
            return Err(failure(
                "goal_handoff_not_ready",
                "The candidate may only prepare and confirm the current handoff. Business tools remain disabled until server ownership commits.",
            ));
        }
        if epoch != handoff.execution_owner_epoch {
            return Err(failure(
                "goal_execution_owner_mismatch",
                "Only the server-bound current Goal owner may access its task controls.",
            ));
        }
        if tool == "goal_handoff" && matches!(args["action"].as_str(), Some("prepare" | "commit")) {
            return Ok(());
        }
        if tool == "goal_read" {
            return if matches!(
                args["view"].as_str(),
                None | Some("summary" | "node" | "handoff")
            ) {
                Ok(())
            } else {
                Err(isolation_unavailable())
            };
        }
        if matches!(
            tool,
            "goal_bootstrap"
                | "goal_plan_save"
                | "goal_confirm_plan"
                | "goal_claim_node"
                | "goal_checkpoint"
        ) {
            return Ok(());
        }
        // goal_check_node can launch command acceptance and is deliberately not
        // admitted until it carries a task-scoped Runner execution capability.
        Err(isolation_unavailable())
    }
}
