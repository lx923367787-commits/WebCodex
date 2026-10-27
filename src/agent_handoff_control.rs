//! Narrow trusted scheduler operations. No Goal text, files, memory, or run
//! credentials cross normal control projections. The dedicated launch-claim
//! issuance response alone deliberately delivers one short-lived narrow grant.
use crate::auth::{AuthContext, AuthKind};
use crate::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_store::HostGoalHandoff;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    HandoffRecoverFailed {
        owner_user: String,
        project: String,
        agent_id: String,
        goal_id: String,
        handoff_id: String,
        expected_source_run_id: String,
        expected_source_generation: i64,
        expected_goal_revision: u64,
        expected_attempt_id: Option<String>,
        idempotency_key: String,
    },
    HandoffList {
        owner_user: String,
        project: String,
        agent_id: Option<String>,
        limit: Option<usize>,
    },
    HandoffStatus {
        owner_user: String,
        project: String,
        agent_id: String,
        goal_id: String,
        handoff_id: Option<String>,
        expected_attempt_id: Option<String>,
    },
    HandoffBeginAttempt {
        owner_user: String,
        project: String,
        agent_id: String,
        goal_id: String,
        handoff_id: String,
        expected_attempt_id: Option<String>,
    },
    HandoffLaunchClaim {
        owner_user: String,
        project: String,
        agent_id: String,
        goal_id: String,
        handoff_id: String,
        expected_attempt_id: String,
    },
    HandoffFailAttempt {
        owner_user: String,
        project: String,
        agent_id: String,
        goal_id: String,
        handoff_id: String,
        expected_attempt_id: String,
        reason: Option<String>,
    },
    HandoffAbort {
        owner_user: String,
        project: String,
        agent_id: String,
        goal_id: String,
        handoff_id: String,
        expected_attempt_id: Option<String>,
        reason: Option<String>,
    },
}

fn error(error: webcodex_store::GoalStoreError) -> String {
    error.code().into()
}

fn timeout() -> Result<u64, String> {
    match std::env::var("WEBCODEX_HANDOFF_TAKEOVER_TIMEOUT_SECS") {
        Err(std::env::VarError::NotPresent) => Ok(300),
        Ok(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=3600).contains(n))
            .ok_or_else(|| "goal_handoff_configuration".into()),
        Err(_) => Err("goal_handoff_configuration".into()),
    }
}

fn failure_code(value: Option<&str>) -> Option<&'static str> {
    match value {
        Some("launch_failed") => Some("launch_failed"),
        Some("thinking_policy_changed") => Some("thinking_policy_changed"),
        Some("goal_not_active") => Some("goal_not_active"),
        Some("old_execution_not_quiescent") => Some("old_execution_not_quiescent"),
        Some("All four handoff startup attempts failed or timed out") => Some("attempts_exhausted"),
        Some(_) => Some("handoff_failed"),
        None => None,
    }
}

fn reason(value: Option<&str>) -> Result<&str, String> {
    let value = value.unwrap_or("handoff_aborted");
    if matches!(
        value,
        "launch_failed"
            | "thinking_policy_changed"
            | "goal_not_active"
            | "handoff_aborted"
            | "adapter_uncertain"
    ) {
        Ok(value)
    } else {
        Err("invalid_handoff_failure_code".into())
    }
}

fn owned(
    db: &Database,
    owner_user: &str,
    project: &str,
    agent_id: &str,
    goal_id: &str,
) -> Result<(String, HostGoalHandoff), String> {
    let owner = db
        .agent_host_goal_owner_key(owner_user, project, agent_id)
        .map_err(|e| e.code().to_string())?;
    let handoff = db
        .read_host_goal_handoff(&owner, goal_id)
        .map_err(error)?
        .ok_or_else(|| "goal_handoff_missing".to_string())?;
    if handoff.project != project
        || handoff.agent_id.as_deref() != Some(agent_id)
        || handoff.source_agent_run_id.is_none()
    {
        return Err("native_agent_handoff_required".into());
    }
    Ok((owner, handoff))
}

fn projection(db: &Database, owner: &str, h: &HostGoalHandoff) -> Result<Value, String> {
    let metadata = db
        .read_host_goal_handoff_control_metadata(owner, &h.goal_id)
        .map_err(error)?;
    let source = db
        .read_host_context_state(owner, &h.source_context_epoch)
        .map_err(error)?
        .ok_or_else(|| "goal_handoff_source_missing".to_string())?;
    Ok(json!({
        "driver":"host", "projection":"handoff", "agent_id":h.agent_id,
        "goal":{"goal_id":h.goal_id,"project":h.project,"phase":metadata["goal_phase"],"fence":metadata["goal_fence"]},
        "source_context":{"context_epoch":h.source_context_epoch,"identity_source":source.identity_source,
            "lifecycle_state":source.state,"retired_at":source.retired_at},
        "checkpoint":{"sequence":h.checkpoint_sequence,"revision":metadata["checkpoint_revision"],
            "action":"handoff","source_context_epoch":h.source_context_epoch,
            "source_fence":metadata["source_fence"],"created_at":metadata["checkpoint_created_at"]},
        "handoff":{"handoff_id":h.handoff_id,"goal_id":h.goal_id,"project":h.project,
            "source_context_epoch":h.source_context_epoch,"checkpoint_sequence":h.checkpoint_sequence,
            "plan_version":h.plan_version,"status":h.status,"attempt_count":h.attempt_count,
            "attempt_id":h.attempt_id,"candidate_context_epoch":h.candidate_context_epoch,
            "deadline_at":h.deadline_at,"timeout_secs":h.timeout_secs,"prepared_at":h.prepared_at,
            "execution_owner_epoch":h.execution_owner_epoch,"failure_reason":failure_code(h.failure_reason.as_deref()),
            "created_at":h.created_at,"committed_at":h.committed_at,"agent_id":h.agent_id},
        "execution_granted":false,
        "notice":if h.status=="failed" {Some("交接失败，已停止执行，已保留可人工恢复的状态。只有用户明确要求恢复后才能重新启动。")} else {None}
    }))
}

/// Called only by the existing explicit-Bootstrap HTTP control handler. Keep
/// a second identity check here so future callers cannot repurpose this API.
pub(crate) fn try_control(
    db: &Database,
    auth: &AuthContext,
    body: &Value,
) -> Option<Result<Value, String>> {
    if !body["action"]
        .as_str()
        .is_some_and(|a| a.starts_with("handoff_"))
    {
        return None;
    }
    Some((|| {
        if auth.kind != AuthKind::Bootstrap || !auth.is_bootstrap() || auth.agent_run.is_some() {
            return Err("trusted_control_required".into());
        }
        let request: Request =
            serde_json::from_value(body.clone()).map_err(|_| "invalid_handoff_control_request")?;
        let (owner_user, project, agent_id, goal_id, handoff_id, expected, action, supplied_reason) =
            match request {
                Request::HandoffRecoverFailed {
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    handoff_id,
                    expected_source_run_id,
                    expected_source_generation,
                    expected_goal_revision,
                    expected_attempt_id,
                    idempotency_key,
                } => {
                    let (handoff, replayed, revision, source) = db
                        .recover_failed_agent_handoff(
                            &owner_user,
                            &project,
                            &agent_id,
                            &goal_id,
                            &handoff_id,
                            &expected_source_run_id,
                            expected_source_generation,
                            expected_goal_revision,
                            expected_attempt_id.as_deref(),
                            &idempotency_key,
                            chrono::Utc::now().timestamp_millis(),
                        )
                        .map_err(error)?;
                    // Store recovery commits the checkpoint first. Retry the
                    // same intent after any interrupted private publication;
                    // never expose the publisher's private payload to control.
                    let (goal, _, _) = db
                        .read_host_goal_for_agent_run(&source, &goal_id)
                        .map_err(|_| "goal_handoff_publication_incomplete")?;
                    if goal.revision != revision {
                        return Err("goal_handoff_publication_incomplete".into());
                    }
                    crate::agent_goal_handoff::publish_checkpoint(db, &source, &goal, replayed)
                        .map_err(|_| "goal_handoff_publication_incomplete")?;
                    let owner = db
                        .agent_host_goal_owner_key(&owner_user, &project, &agent_id)
                        .map_err(|e| e.code().to_string())?;
                    let mut result = projection(db, &owner, &handoff)?;
                    result["replayed"] = json!(replayed);
                    result["previous_handoff_id"] = json!(handoff_id);
                    result["goal_revision"] = json!(revision);
                    result["publication"] = json!("verified");
                    return Ok(result);
                }
                Request::HandoffList {
                    owner_user,
                    project,
                    agent_id,
                    limit,
                } => {
                    let limit = limit.unwrap_or(100);
                    if !(1..=100).contains(&limit) {
                        return Err("invalid_handoff_control_request".into());
                    }
                    let handoffs = db
                        .list_agent_handoffs(&owner_user, &project, agent_id.as_deref(), limit)
                        .map_err(|e| e.code().to_string())?;
                    let candidates: Vec<Value> = handoffs.into_iter().filter(|h| h.agent_id.is_some() && h.source_agent_run_id.is_some())
                    .map(|h| json!({"project":h.project,"goal_id":h.goal_id,"agent_id":h.agent_id,
                        "source_context_epoch":h.source_context_epoch,"checkpoint_sequence":h.checkpoint_sequence})).collect();
                    return Ok(json!({"candidates":candidates}));
                }
                Request::HandoffStatus {
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    handoff_id,
                    expected_attempt_id,
                } => {
                    let _ = expected_attempt_id; // Status reconciles the actual current attempt.
                    (
                        owner_user, project, agent_id, goal_id, handoff_id, None, "status", None,
                    )
                }
                Request::HandoffBeginAttempt {
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    handoff_id,
                    expected_attempt_id,
                } => (
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    Some(handoff_id),
                    expected_attempt_id,
                    "begin",
                    None,
                ),
                Request::HandoffLaunchClaim {
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    handoff_id,
                    expected_attempt_id,
                } => (
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    Some(handoff_id),
                    Some(expected_attempt_id),
                    "launch_claim",
                    None,
                ),
                Request::HandoffFailAttempt {
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    handoff_id,
                    expected_attempt_id,
                    reason,
                } => (
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    Some(handoff_id),
                    Some(expected_attempt_id),
                    "fail",
                    reason,
                ),
                Request::HandoffAbort {
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    handoff_id,
                    expected_attempt_id,
                    reason,
                } => (
                    owner_user,
                    project,
                    agent_id,
                    goal_id,
                    Some(handoff_id),
                    expected_attempt_id,
                    "abort",
                    reason,
                ),
            };
        let (owner, mut h) = owned(db, &owner_user, &project, &agent_id, &goal_id)?;
        if handoff_id.as_deref().is_some_and(|id| id != h.handoff_id) {
            return Err("goal_handoff_stale".into());
        }
        if action == "launch_claim" {
            let attempt = expected
                .as_deref()
                .ok_or("invalid_handoff_control_request")?;
            let grant = db
                .issue_agent_handoff_claim(
                    &owner,
                    &goal_id,
                    &h.handoff_id,
                    attempt,
                    chrono::Utc::now().timestamp_millis(),
                )
                .map_err(error)?;
            // Only this response intentionally serializes the one-time secret.
            // The outer trusted handler sets Cache-Control: no-store.
            return Ok(
                json!({"project":h.project,"goal_id":h.goal_id,"agent_id":h.agent_id,
                "handoff_id":h.handoff_id,"attempt_id":attempt,"expires_at_ms":h.deadline_at,
                "claim_grant":grant.into_secret()}),
            );
        }
        if action == "status" || matches!(h.status.as_str(), "committed" | "failed") {
            return projection(db, &owner, &h);
        }
        let now = chrono::Utc::now().timestamp_millis();
        h = match action {
            "begin" => {
                // Native Agent execution adapters are denied. If historical
                // external work exists, never pretend this synchronous API can
                // stop it: fail closed without allocating another executor.
                if db
                    .count_unresolved_host_goal_operations(&owner, &goal_id)
                    .map_err(error)?
                    != 0
                {
                    db.abort_host_goal_handoff_attempt(
                        &owner,
                        &goal_id,
                        &h.handoff_id,
                        expected.as_deref(),
                        "old_execution_not_quiescent",
                        now,
                    )
                    .map_err(error)?
                } else {
                    db.begin_host_goal_handoff_attempt(
                        &owner,
                        &goal_id,
                        &h.handoff_id,
                        expected.as_deref(),
                        now,
                        timeout()?,
                    )
                    .map_err(error)?
                }
            }
            "fail" => db
                .fail_host_goal_handoff_attempt(
                    &owner,
                    &goal_id,
                    &h.handoff_id,
                    expected
                        .as_deref()
                        .ok_or("invalid_handoff_control_request")?,
                    reason(supplied_reason.as_deref())?,
                    now,
                )
                .map_err(error)?,
            _ => db
                .abort_host_goal_handoff_attempt(
                    &owner,
                    &goal_id,
                    &h.handoff_id,
                    expected.as_deref(),
                    reason(supplied_reason.as_deref())?,
                    now,
                )
                .map_err(error)?,
        };
        projection(db, &owner, &h)
    })())
}

#[cfg(test)]
#[path = "agent_handoff_control_tests.rs"]
mod tests;
