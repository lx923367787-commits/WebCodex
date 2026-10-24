//! Native private-Agent handoff. A trusted launch claim can bind one candidate
//! window to one pending attempt; an attempt ID alone grants no authority.
//! File reads use the broker under the exact pending/source-run store lease.
use crate::agent_goal_adapter::{files, write_exact};
use crate::agent_private_files::AgentPrivateFiles;
use crate::tool_runtime::ToolResult;
use crate::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webcodex_core::host_goal::*;
use webcodex_store::{
    AgentHostWindowBinding, AgentIsolationError, HostGoalHandoff, VerifiedAgentRun,
};

#[cfg(test)]
#[path = "agent_goal_handoff_tests.rs"]
mod tests;

fn denied(code: &str) -> ToolResult {
    crate::agent_isolation::denial(code)
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_error() -> AgentIsolationError {
    AgentIsolationError::new("goal_handoff_materialization_changed")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    goal_id: String,
    action: String,
    handoff_id: Option<String>,
    attempt_id: Option<String>,
    checkpoint_sequence: Option<u64>,
    package_sha256: Option<String>,
    // The launch-only capability is deliberately never serialized or logged.
    launch_claim: Option<String>,
}
impl Request {
    fn exact(&self, h: &HostGoalHandoff) -> Result<(), ToolResult> {
        if self.goal_id != h.goal_id
            || self
                .handoff_id
                .as_deref()
                .is_some_and(|id| id != h.handoff_id)
            || self
                .attempt_id
                .as_deref()
                .is_some_and(|id| Some(id) != h.attempt_id.as_deref())
            || self
                .checkpoint_sequence
                .is_some_and(|n| n != h.checkpoint_sequence)
        {
            return Err(denied("goal_handoff_attempt_mismatch"));
        }
        Ok(())
    }
    fn readiness(&self) -> Result<(&str, &str, u64), ToolResult> {
        Ok((
            self.handoff_id
                .as_deref()
                .ok_or_else(|| denied("invalid_goal_handoff"))?,
            self.attempt_id
                .as_deref()
                .ok_or_else(|| denied("invalid_goal_handoff"))?,
            self.checkpoint_sequence
                .ok_or_else(|| denied("invalid_goal_handoff"))?,
        ))
    }
}

struct Package {
    value: Value,
    bytes: Vec<u8>,
    checkpoint_bytes: Vec<u8>,
    checkpoint_path: String,
    path: String,
    plan: Option<HostGoalPlanVersion>,
}
impl Package {
    fn build(
        h: &HostGoalHandoff,
        goal: &HostGoalRecord,
        checkpoint: &HostGoalCheckpoint,
        plan: Option<HostGoalPlanVersion>,
    ) -> Result<Self, ToolResult> {
        if h.goal_id != goal.goal_id
            || h.project != goal.project
            || h.checkpoint_sequence != checkpoint.sequence
            || checkpoint.goal_id != goal.goal_id
            || checkpoint.revision != goal.revision
            || checkpoint.plan_version != h.plan_version
            || goal.plan_version != h.plan_version
            || checkpoint.action.as_deref() != Some("handoff")
            || checkpoint.source_context_epoch.as_deref() != Some(h.source_context_epoch.as_str())
            || h.source_agent_run_id.as_deref() != Some(h.source_context_epoch.as_str())
        {
            return Err(denied("goal_handoff_checkpoint_mismatch"));
        }
        if h.plan_version > 0 {
            let p = plan.as_ref().ok_or_else(|| denied("goal_plan_not_found"))?;
            if p.goal_id != h.goal_id
                || p.version != h.plan_version
                || goal.plan_hash.as_deref() != Some(p.hash.as_str())
            {
                return Err(denied("goal_handoff_plan_mismatch"));
            }
        } else if plan.is_some() {
            return Err(denied("goal_handoff_plan_mismatch"));
        }
        let checkpoint_path = format!(
            "checkpoints/{}/handoff-{}.json",
            h.goal_id, h.checkpoint_sequence
        );
        let path = format!("goals/{}/handoffs/{}.json", h.goal_id, h.handoff_id);
        let checkpoint_bytes =
            serde_json::to_vec_pretty(checkpoint).map_err(|_| denied("invalid_goal_handoff"))?;
        // Only this Goal's saved state and plan are included. References remain
        // data, and cannot expand the successor's broker-enforced file scope.
        // Attempt IDs are deliberately absent: retries reuse the same package.
        let value = json!({"schema_version":1,"handoff_id":h.handoff_id,
            "goal":goal,"checkpoint":checkpoint,"plan":plan,
            "checkpoint_file":{"path":checkpoint_path,"sha256":sha(&checkpoint_bytes)}});
        let bytes =
            serde_json::to_vec_pretty(&value).map_err(|_| denied("invalid_goal_handoff"))?;
        Ok(Self {
            value,
            bytes,
            checkpoint_bytes,
            checkpoint_path,
            path,
            plan,
        })
    }

    fn verify(
        &self,
        private: &AgentPrivateFiles,
        binding: &VerifiedAgentRun,
    ) -> Result<(), AgentIsolationError> {
        for (path, bytes) in [
            (&self.checkpoint_path, &self.checkpoint_bytes),
            (&self.path, &self.bytes),
        ] {
            let actual = private.read(binding, path).map_err(|_| file_error())?;
            if actual.bytes != *bytes || actual.sha256 != sha(bytes) {
                return Err(file_error());
            }
        }
        if let Some(plan) = &self.plan {
            let receipt = plan.materialization.as_ref().ok_or_else(file_error)?;
            let prefix = format!("goals/{}/plans/v{}", plan.goal_id, plan.version);
            if receipt.json_path != format!("{prefix}/plan.json")
                || receipt.markdown_path != format!("{prefix}/plan.md")
            {
                return Err(file_error());
            }
            let mut bytes = serde_json::to_vec_pretty(&plan.plan).map_err(|_| file_error())?;
            bytes.push(b'\n');
            let value = serde_json::to_value(&plan.plan).map_err(|_| file_error())?;
            let markdown = crate::tool_runtime::agent_goal_plan_markdown(
                &value,
                &plan.goal_id,
                plan.version,
                &plan.hash,
            );
            for (path, expected, bytes) in [
                (&receipt.json_path, &receipt.json_hash, bytes.as_slice()),
                (
                    &receipt.markdown_path,
                    &receipt.markdown_hash,
                    markdown.as_bytes(),
                ),
            ] {
                let actual = private.read(binding, path).map_err(|_| file_error())?;
                if actual.sha256 != *expected || actual.bytes != bytes || sha(bytes) != *expected {
                    return Err(file_error());
                }
            }
        }
        Ok(())
    }
}

fn package(
    db: &Database,
    source: &VerifiedAgentRun,
    h: &HostGoalHandoff,
    saved_goal: Option<&HostGoalRecord>,
) -> Result<Package, ToolResult> {
    let snapshot = db
        .read_host_goal_handoff_snapshot(&source.host_goal_owner_key(), &h.goal_id, source.run_id())
        .map_err(|e| denied(e.code()))?;
    if snapshot.unresolved_operation_count != 0 {
        return Err(denied("goal_handoff_old_execution_unresolved"));
    }
    let checkpoint = snapshot
        .checkpoint
        .ok_or_else(|| denied("goal_handoff_checkpoint_missing"))?;
    let plan = if h.plan_version == 0 {
        None
    } else {
        db.get_host_goal_plan(
            &source.host_goal_owner_key(),
            &h.goal_id,
            Some(h.plan_version),
        )
        .map_err(|e| denied(e.code()))?
    };
    Package::build(h, saved_goal.unwrap_or(&snapshot.goal), &checkpoint, plan)
}

fn publication_error(
    goal_id: &str,
    h: Option<&HostGoalHandoff>,
    cause: Option<&str>,
) -> ToolResult {
    ToolResult::err_with_output(
        "goal_handoff_publication_incomplete",
        json!({
        "error_kind":"goal_handoff_publication_incomplete","cause":cause,
        "goal_id":goal_id,"handoff":h,"checkpoint_saved":true,"state_changed":true,
        "execution_granted":false,"publication":"incomplete",
        "recovery":"Retry the identical checkpoint request to publish the saved immutable package."}),
    )
}

pub(crate) fn publish_checkpoint(
    db: &Database,
    source: &VerifiedAgentRun,
    goal: &HostGoalRecord,
    replayed: bool,
) -> Result<Value, ToolResult> {
    let h = db
        .read_host_goal_handoff(&source.host_goal_owner_key(), &goal.goal_id)
        .map_err(|e| publication_error(&goal.goal_id, None, Some(e.code())))?
        .ok_or_else(|| publication_error(&goal.goal_id, None, Some("goal_handoff_missing")))?;
    let publish = || -> Result<Package, ToolResult> {
        let package = package(db, source, &h, Some(goal))?;
        let private = files(db)?;
        db.with_source_agent_handoff(source, &goal.goal_id, h.checkpoint_sequence, || {
            private.create(source).map_err(|_| file_error())?;
            write_exact(
                &private,
                source,
                &package.checkpoint_path,
                &package.checkpoint_bytes,
            )?;
            write_exact(&private, source, &package.path, &package.bytes)?;
            package.verify(&private, source)
        })
        .map_err(|e| denied(e.code()))?;
        Ok(package)
    };
    match publish() {
        Ok(p) => Ok(
            json!({"goal":goal,"replayed":replayed,"checkpoint_path":p.checkpoint_path,
            "handoff":h,"handoff_artifacts":{"checkpoint_path":p.checkpoint_path,"checkpoint_sha256":sha(&p.checkpoint_bytes),
            "json_path":p.path,"json_sha256":sha(&p.bytes)},"publication":"verified","execution_granted":false}),
        ),
        Err(error) => Err(publication_error(
            &goal.goal_id,
            Some(&h),
            error.error.as_deref(),
        )),
    }
}

fn status(
    db: &Database,
    binding: &VerifiedAgentRun,
    request: &Request,
) -> Result<Value, ToolResult> {
    db.read_host_goal_for_agent_run(binding, &request.goal_id)
        .map_err(|e| denied(e.code()))?;
    let h = db
        .read_host_goal_handoff(&binding.host_goal_owner_key(), &request.goal_id)
        .map_err(|e| denied(e.code()))?
        .ok_or_else(|| denied("goal_handoff_missing"))?;
    request.exact(&h)?;
    let granted = h.status == "committed"
        && h.successor_agent_run_id.as_deref() == Some(binding.run_id())
        && h.execution_owner_epoch == binding.run_id()
        && db
            .agent_run_task_goal(binding)
            .map_err(|e| denied(e.code()))?
            .as_deref()
            == Some(request.goal_id.as_str());
    Ok(
        json!({"driver":"host","goal_id":request.goal_id,"handoff":h,"execution_granted":granted,
        "notice":if h.status == "failed" {
            Some("交接失败，已停止执行，已保留可人工恢复的状态。只有用户明确要求恢复后才能重新启动。")
        } else {None}}),
    )
}

pub(crate) fn call_active(
    db: &Database,
    binding: &VerifiedAgentRun,
    arguments: Value,
) -> ToolResult {
    let result = (|| {
        let request: Request =
            serde_json::from_value(arguments).map_err(|_| denied("invalid_goal_handoff"))?;
        if request.action != "status" || request.launch_claim.is_some() {
            return Err(denied("goal_handoff_host_window_required"));
        }
        status(db, binding, &request)
    })();
    match result {
        Ok(value) => ToolResult::ok(value),
        Err(error) => error,
    }
}

fn committed_status(
    db: &Database,
    principal: &str,
    window: &str,
    request: &Request,
    committed: &HostGoalHandoff,
) -> Result<Value, ToolResult> {
    let observed = (|| {
        let Some(AgentHostWindowBinding::Active(run)) = db
            .resolve_agent_host_window(principal, window, now())
            .map_err(|e| denied(e.code()))?
        else {
            return Err(denied("goal_handoff_grant_unconfirmed"));
        };
        status(db, &run, request)
    })();
    observed.map_err(|error| ToolResult::err_with_output("goal_handoff_status_unavailable", json!({
        "error_kind":"goal_handoff_status_unavailable","cause":error.error,
        "goal_id":request.goal_id,"handoff":committed,"state_changed":true,
        "execution_granted":null,"dispatch_certainty":"outcome_unknown",
        "recovery":"Query goal_handoff status for this exact Goal and handoff before continuing or retrying; do not launch another candidate based on this response."})))
}

pub(crate) fn call_host(
    db: &Database,
    principal: &str,
    window: &str,
    arguments: Value,
) -> ToolResult {
    let result = (|| {
        let request: Request =
            serde_json::from_value(arguments).map_err(|_| denied("invalid_goal_handoff"))?;
        let binding = if let Some(claim) = request.launch_claim.as_deref() {
            if request.action != "prepare" {
                return Err(denied("goal_handoff_claim_prepare_only"));
            }
            let (handoff, attempt, _) = request.readiness()?;
            // The store verifies a server-issued, principal-bound capability,
            // current attempt and deadline. It only binds Pending; same-window
            // retries must present the identical claim and cannot retarget it.
            db.claim_agent_handoff_host_window(
                principal,
                window,
                &request.goal_id,
                handoff,
                attempt,
                claim,
                now(),
            )
            .map_err(|e| denied(e.code()))?
        } else {
            db.resolve_agent_host_window(principal, window, now())
                .map_err(|e| denied(e.code()))?
                .ok_or_else(|| denied("host_window_unregistered"))?
        };
        match binding {
            AgentHostWindowBinding::Active(run) => {
                if request.action == "status" {
                    return status(db, &run, &request);
                }
                if request.action != "commit" {
                    return Err(denied("goal_handoff_candidate_required"));
                }
                let (handoff, attempt, checkpoint) = request.readiness()?;
                let digest = request
                    .package_sha256
                    .as_deref()
                    .ok_or_else(|| denied("invalid_goal_handoff"))?;
                let committed = db
                    .commit_agent_handoff_for_host_window(
                        principal,
                        window,
                        &request.goal_id,
                        handoff,
                        attempt,
                        checkpoint,
                        digest,
                        now(),
                    )
                    .map_err(|e| denied(e.code()))?;
                committed_status(db, principal, window, &request, &committed)
            }
            AgentHostWindowBinding::Pending {
                source,
                goal_id,
                handoff_id,
                attempt_id,
                ..
            } => {
                if request.goal_id != goal_id {
                    return Err(denied("goal_handoff_attempt_mismatch"));
                }
                if !matches!(request.action.as_str(), "status" | "prepare" | "commit") {
                    return Err(denied("goal_handoff_candidate_required"));
                }
                let h = db
                    .read_host_goal_handoff(&source.host_goal_owner_key(), &goal_id)
                    .map_err(|e| denied(e.code()))?
                    .ok_or_else(|| denied("goal_handoff_missing"))?;
                request.exact(&h)?;
                if h.handoff_id != handoff_id
                    || h.attempt_id.as_deref() != Some(attempt_id.as_str())
                {
                    return Err(denied("goal_handoff_attempt_mismatch"));
                }
                if request.action == "status" {
                    return Ok(
                        json!({"driver":"host","goal_id":goal_id,"handoff":h,"execution_granted":false}),
                    );
                }
                let (handoff, attempt, checkpoint) = request.readiness()?;
                let p = package(db, &source, &h, None)?;
                let private = files(db)?;
                db.with_pending_agent_host_handoff(
                    principal,
                    window,
                    &goal_id,
                    handoff,
                    attempt,
                    now(),
                    |read_binding, current| {
                        if current.handoff_id != h.handoff_id
                            || current.checkpoint_sequence != h.checkpoint_sequence
                            || current.plan_version != h.plan_version
                            || read_binding != &source
                        {
                            return Err(file_error());
                        }
                        // Actual safe broker reads verify both the package and the
                        // required native file channel before readiness is recorded.
                        p.verify(&private, read_binding)
                    },
                )
                .map_err(|e| denied(e.code()))?;
                let digest = sha(&p.bytes);
                if request.action == "prepare" {
                    // Refresh the timestamp after potentially slow file I/O.
                    let prepared = db
                        .prepare_agent_handoff_for_host_window(
                            principal,
                            window,
                            &goal_id,
                            handoff,
                            attempt,
                            checkpoint,
                            &digest,
                            now(),
                        )
                        .map_err(|e| denied(e.code()))?;
                    Ok(
                        json!({"driver":"host","goal_id":goal_id,"handoff":prepared,"package":p.value,"package_sha256":digest,
                            "verified_channels":["agent_private.read"],"execution_adapters_available":false,"execution_granted":false}),
                    )
                } else {
                    if request.package_sha256.as_deref() != Some(digest.as_str()) {
                        return Err(denied("goal_handoff_package_mismatch"));
                    }
                    // Native CLI/background launch is unavailable. Synchronous
                    // broker writes drain under the same store lease; the store
                    // additionally rejects every unresolved historical operation.
                    let committed = db
                        .commit_agent_handoff_for_host_window(
                            principal,
                            window,
                            &goal_id,
                            handoff,
                            attempt,
                            checkpoint,
                            &digest,
                            now(),
                        )
                        .map_err(|e| denied(e.code()))?;
                    committed_status(db, principal, window, &request, &committed)
                }
            }
        }
    })();
    match result {
        Ok(value) => ToolResult::ok(value),
        Err(error) => error,
    }
}
