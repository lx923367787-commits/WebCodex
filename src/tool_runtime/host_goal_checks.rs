//! Durable host-goal acceptance. Client retries replay receipts; command retries
//! recover the original logical Job against its saved input fingerprint.
use super::host_goal::{bounded, failure, now, store_error};
use super::host_goal_context as context;
use super::host_goal_files::sha256;
use super::{ToolCall, ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use webcodex_core::goal::{GoalArtifactSnapshot, GoalCriterion};
use webcodex_core::host_goal::*;

pub(super) fn command_identity(
    binding: &HostGoalExecutionBinding,
    criterion: &GoalCriterion,
    manifest: &[GoalArtifactSnapshot],
) -> Result<(String, String), ToolResult> {
    let GoalCriterion::Command {
        id,
        program,
        args,
        cwd,
        timeout_secs,
        ..
    } = criterion
    else {
        return Err(failure(
            "invalid_goal_check",
            "Expected a command criterion",
        ));
    };
    let bytes = serde_json::to_vec(&(binding, id, program, args, cwd, timeout_secs, manifest))
        .map_err(|e| failure("goal_check_identity_failed", e.to_string()))?;
    let fingerprint = sha256(&bytes);
    Ok((format!("check_{fingerprint}"), fingerprint))
}

fn terminal_check(operation: &HostGoalOperation, criterion_id: &str) -> HostGoalCheckEvidence {
    let canonical = operation
        .result_summary
        .as_deref()
        .and_then(|summary| serde_json::from_str::<Value>(summary).ok());
    let trusted_pass = operation.status == HostGoalOperationStatus::Succeeded
        && operation.job_id.as_ref().is_some_and(|job| {
            !job.is_empty()
                && canonical.as_ref().is_some_and(|value| {
                    value["source"] == "job_status"
                        && value["job_id"].as_str() == Some(job.as_str())
                        && value["status"] == "completed"
                        && value["exit_code"] == 0
                        && value["error"].is_null()
                })
        });
    HostGoalCheckEvidence {
        criterion_id: criterion_id.into(),
        passed: trusted_pass,
        source: HostGoalEvidenceSource::Automated,
        evidence: format!(
            "Original operation {}: {:?}; {}",
            operation.operation_id,
            operation.status,
            operation
                .result_summary
                .as_deref()
                .unwrap_or("No terminal detail")
        ),
        source_refs: vec![operation
            .job_id
            .clone()
            .unwrap_or_else(|| operation.operation_id.clone())],
        artifacts: operation.input_manifest.clone(),
        review_packet_id: None,
        reviewed_scopes: vec![],
        findings: vec![],
    }
}

fn stable_job_result(output: &Value) -> String {
    json!({"source":"job_status","job_id":output["job_id"],"status":output["status"],"exit_code":output["exit_code"],"error":output["error"].as_str().map(|error|bounded(error,2000))}).to_string()
}

impl ToolRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn host_goal_check_node(
        &self,
        goal_id: String,
        node_id: String,
        attempt_id: String,
        expected_revision: u64,
        plan_version: u64,
        idempotency_key: String,
        evidence: Option<Value>,
        auth: Option<&AuthContext>,
    ) -> Result<webcodex_store::HostGoalMutation, ToolResult> {
        let mut result = Box::pin(self.host_goal_check_node_inner(
            goal_id.clone(),
            node_id,
            attempt_id.clone(),
            expected_revision,
            plan_version,
            idempotency_key,
            evidence,
            auth,
        ))
        .await;
        if let Err(error) = &mut result {
            if matches!(
                error.output["error_kind"].as_str(),
                Some(
                    "goal_verification_input_unreadable"
                        | "goal_artifacts_changed"
                        | "goal_evidence_unavailable"
                        | "goal_review_preview_changed"
                )
            ) {
                let goal = self.host_goal_access(&goal_id, auth, true).await?;
                let db = self.host_goal_db()?;
                if db
                    .count_unresolved_host_goal_operations(&goal.owner_key, &goal_id)
                    .map_err(store_error)?
                    == 0
                {
                    if let Some(attempt) = db
                        .get_host_goal_attempt(&goal.owner_key, &goal_id, &attempt_id)
                        .map_err(store_error)?
                    {
                        db.record_host_goal_repair_failure(
                            &goal.owner_key,
                            &attempt.binding,
                            error
                                .error
                                .as_deref()
                                .unwrap_or("Verification evidence is unavailable"),
                            now(),
                            context::invocation_context().as_ref(),
                        )
                        .map_err(store_error)?;
                        if let Some(repair) = db
                            .get_host_goal_repair(&goal.owner_key, &goal_id)
                            .map_err(store_error)?
                        {
                            error.output["recovery"] =
                                super::host_goal::projection::repair_summary(&repair);
                        }
                    }
                }
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn host_goal_check_node_inner(
        &self,
        goal_id: String,
        node_id: String,
        attempt_id: String,
        expected_revision: u64,
        plan_version: u64,
        idempotency_key: String,
        evidence: Option<Value>,
        auth: Option<&AuthContext>,
    ) -> Result<webcodex_store::HostGoalMutation, ToolResult> {
        let goal = self.host_goal_access(&goal_id, auth, true).await?;
        let supplied: Vec<HostGoalCheckEvidence> =
            serde_json::from_value(evidence.unwrap_or_else(|| json!([])))
                .map_err(|e| failure("invalid_goal_evidence", e.to_string()))?;
        if supplied
            .iter()
            .any(|item| item.source != HostGoalEvidenceSource::ModelSelfCheck)
        {
            return Err(failure(
                "invalid_goal_evidence",
                "Automated evidence must be produced by the runtime",
            ));
        }
        let request_hash = sha256(
            &serde_json::to_vec(&(
                &goal_id,
                &node_id,
                &attempt_id,
                expected_revision,
                plan_version,
                &supplied,
            ))
            .map_err(|e| failure("invalid_goal_evidence", e.to_string()))?,
        );
        let db = self.host_goal_db()?;
        // The original client request is checked before lifecycle admission:
        // successful acceptance deliberately clears the running attempt.
        if let Some(replay) = db
            .lookup_host_goal_check_replay(
                &goal.owner_key,
                &goal_id,
                &idempotency_key,
                &request_hash,
            )
            .map_err(store_error)?
        {
            return Ok(replay);
        }
        self.host_goal_context_gate(&goal.owner_key, true)?;
        // Prevent retirement between acceptance admission and its last write.
        let _acceptance_lease =
            context::lease_active_operation(&goal_id, &format!("acceptance_{idempotency_key}"));
        if goal.revision != expected_revision {
            return Err(failure(
                "goal_conflict",
                "Read the current goal revision before running acceptance",
            ));
        }
        let attempt = db
            .get_host_goal_attempt(&goal.owner_key, &goal_id, &attempt_id)
            .map_err(store_error)?
            .ok_or_else(|| {
                failure(
                    "goal_attempt_missing",
                    "The saved node attempt does not exist",
                )
            })?;
        let binding = attempt.binding;
        if binding.node_id != node_id || binding.plan_version != plan_version {
            return Err(failure(
                "goal_conflict",
                "Check request does not identify the saved node attempt",
            ));
        }
        db.validate_host_goal_binding_in_context(
            &goal.owner_key,
            &binding,
            context::invocation_context().as_ref(),
        )
        .map_err(store_error)?;
        let plan = self.verify_host_goal_plan(&goal, auth).await?;
        let node = plan
            .plan
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .ok_or_else(|| failure("goal_node_missing", "Unknown node"))?;
        if let Some(replay) = db
            .reserve_host_goal_check_request_in_context(
                &goal.owner_key,
                &goal_id,
                &idempotency_key,
                &request_hash,
                context::invocation_context().as_ref(),
            )
            .map_err(store_error)?
        {
            return Ok(replay);
        }

        let last = plan
            .plan
            .nodes
            .last()
            .is_some_and(|node| node.id == node_id);
        if last {
            // Final delivery covers all explicitly managed layout outputs,
            // including those created by earlier document operations.
            let records = db
                .list_host_goal_reviews(&goal.owner_key, &goal_id)
                .map_err(store_error)?;
            let mut layouts = BTreeSet::new();
            for prior in &plan.plan.nodes {
                for artifact in prior.managed_artifacts.as_deref().unwrap_or_default() {
                    if artifact.kind == HostGoalDeliveryKind::Layout
                        || host_goal_layout_path(&artifact.path)
                    {
                        layouts.insert(artifact.path.clone());
                    }
                }
            }
            for record in &records {
                for artifact in &record.managed_artifacts {
                    if artifact.kind == HostGoalDeliveryKind::Layout
                        || host_goal_layout_path(&artifact.path)
                    {
                        layouts.insert(artifact.path.clone());
                    }
                }
            }
            let enrolled = records
                .iter()
                .find(|r| r.node_id == node_id)
                .map(|r| {
                    r.managed_artifacts
                        .iter()
                        .map(|a| a.path.clone())
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            for path in layouts.difference(&enrolled) {
                db.enforce_host_goal_layout_review(&goal.owner_key, &binding, path, now())
                    .map_err(store_error)?;
            }
        }
        let mut criteria = node.acceptance.clone();
        let node_has_command = node
            .acceptance
            .iter()
            .any(|criterion| matches!(criterion, GoalCriterion::Command { .. }));
        let mut verification_paths = if !node.verification_paths.is_empty() {
            node.verification_paths.clone()
        } else if node_has_command {
            plan.plan.verification_paths.clone()
        } else {
            Vec::new()
        };
        if last {
            criteria.extend(plan.plan.final_criteria.clone());
            verification_paths.extend(plan.plan.verification_paths.clone());
        }
        // Generic command outputs are managed only when explicitly enrolled.
        verification_paths.extend(
            node.managed_artifacts
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|a| a.path.clone()),
        );
        let forced_review = db
            .read_host_goal_review(&goal.owner_key, &goal_id, &node_id)
            .map_err(store_error)?;
        if let Some(record) = &forced_review {
            verification_paths.extend(record.managed_artifacts.iter().map(|a| a.path.clone()));
        }
        verification_paths.sort();
        verification_paths.dedup();
        // Earlier nodes describe milestones accepted at their execution time.
        // Later planned steps may legitimately replace the same intermediate
        // file; final criteria, rerun at the end, own current delivery acceptance.
        let manifest = self
            .host_goal_manifest(&goal, &criteria, &verification_paths, auth)
            .await?;
        self.reconcile_host_goal_operations_inner(&goal, auth, true)
            .await?;
        db.admit_host_goal_repair_check(
            &goal.owner_key,
            &binding,
            context::current_binding()
                .as_ref()
                .and_then(|value| value.repair.as_ref()),
            now(),
            context::invocation_context().as_ref(),
        )
        .map_err(store_error)?;
        let checks = self
            .host_goal_checks(
                &goal,
                &binding,
                &node.acceptance,
                &supplied,
                &manifest,
                auth,
            )
            .await?;
        let final_checks = if last
            && checks
                .iter()
                .filter(|c| c.source == HostGoalEvidenceSource::Automated)
                .all(|check| check.passed)
        {
            self.host_goal_checks(
                &goal,
                &binding,
                &plan.plan.final_criteria,
                &supplied,
                &manifest,
                auth,
            )
            .await?
        } else {
            Vec::new()
        };
        if self
            .host_goal_manifest(&goal, &criteria, &verification_paths, auth)
            .await?
            != manifest
        {
            return Err(failure("goal_artifacts_changed","Artifacts changed during the combined node and final acceptance checks; no acceptance was committed"));
        }
        // Recheck local plan files immediately before committing acceptance.
        self.verify_host_goal_plan(&goal, auth).await?;
        if checks
            .iter()
            .chain(final_checks.iter())
            .any(|check| check.source == HostGoalEvidenceSource::Automated && !check.passed)
        {
            if checks.iter().chain(final_checks.iter()).any(|check| {
                check.source == HostGoalEvidenceSource::ModelSelfCheck
                    && check.source_refs.is_empty()
            }) {
                let failures = checks
                    .iter()
                    .chain(final_checks.iter())
                    .filter(|check| {
                        check.source == HostGoalEvidenceSource::Automated && !check.passed
                    })
                    .map(|check| format!("{}: {}", check.criterion_id, check.evidence))
                    .collect::<Vec<_>>()
                    .join("\n");
                db.record_host_goal_repair_failure(
                    &goal.owner_key,
                    &binding,
                    &failures,
                    now(),
                    context::invocation_context().as_ref(),
                )
                .map_err(store_error)?;
                return Ok(webcodex_store::HostGoalMutation {
                    goal,
                    replayed: false,
                });
            }
            // A model review cannot override machine failure. Persist it now
            // so normal business stays closed while the host repairs the node.
            return db
                .check_host_goal_node_with_request_in_context(
                    &goal.owner_key,
                    &binding,
                    expected_revision,
                    &checks,
                    &final_checks,
                    &idempotency_key,
                    &request_hash,
                    now(),
                    context::invocation_context().as_ref(),
                )
                .map_err(store_error);
        }
        let previous_review = db
            .read_host_goal_review(&goal.owner_key, &goal_id, &node_id)
            .map_err(store_error)?;
        if host_goal_review_policy(node).review == 1
            || previous_review.as_ref().is_some_and(|r| r.review == 1)
        {
            let all_checks = checks
                .iter()
                .chain(final_checks.iter())
                .cloned()
                .collect::<Vec<_>>();
            if supplied.is_empty() {
                self.prepare_review_packet(
                    &goal,
                    node,
                    &binding,
                    &manifest,
                    &all_checks,
                    &idempotency_key,
                    auth,
                )
                .await?;
                return Ok(webcodex_store::HostGoalMutation {
                    goal,
                    replayed: false,
                });
            }
            let packet = previous_review.as_ref().and_then(|r|r.current_packet.as_ref()).ok_or_else(||failure("goal_review_material_required","Run goal_check_node without evidence first; judge the returned material in a subsequent model turn."))?;
            if packet.manifest != manifest
                || packet.binding != binding
                || packet.context_epoch != context::current_epoch()
            {
                return Err(failure("goal_review_stale","The verdict does not match current artifacts, attempt and conversation; prepare new material instead of relabeling old evidence."));
            }
            for material in packet.materials.iter().filter(|m| m.kind == "image") {
                if let (Some(path), Some(expected)) =
                    (&material.preview_path, &material.preview_sha256)
                {
                    if self
                        .host_goal_file_fingerprint(&goal.project, path, auth)
                        .await
                        .as_ref()
                        != Some(expected)
                    {
                        return Err(failure("goal_review_preview_changed","A delivered preview changed or disappeared; render and review current images."));
                    }
                }
            }
            self.verify_review_render_dependencies(&goal, packet, &idempotency_key, auth)
                .await?;
            let review_ids = node
                .acceptance
                .iter()
                .chain(if last {
                    plan.plan.final_criteria.iter()
                } else {
                    node.acceptance[..0].iter()
                })
                .filter(|c| matches!(c, GoalCriterion::Review { .. }))
                .map(|c| c.id())
                .collect::<BTreeSet<_>>();
            if (review_ids.is_empty()
                && (supplied.len() != 1 || supplied[0].criterion_id != "__delivery_review"))
                || (!review_ids.is_empty()
                    && supplied
                        .iter()
                        .map(|c| c.criterion_id.as_str())
                        .collect::<BTreeSet<_>>()
                        != review_ids)
            {
                return Err(failure("invalid_goal_evidence","Submit each saved review criterion, or exactly __delivery_review when the node has no explicit review criterion."));
            }
            let reviewed = db
                .submit_host_goal_review(
                    &goal.owner_key,
                    &binding,
                    &packet.packet_id,
                    context::current_epoch().as_deref(),
                    &manifest,
                    &supplied,
                    now(),
                )
                .map_err(store_error)?;
            if !reviewed.rounds.last().is_some_and(|r| r.passed) {
                return Ok(webcodex_store::HostGoalMutation {
                    goal,
                    replayed: false,
                });
            }
        }
        if self
            .host_goal_manifest(&goal, &criteria, &verification_paths, auth)
            .await?
            != manifest
        {
            return Err(failure("goal_artifacts_changed","Artifacts changed during review validation; the saved verdict cannot complete this delivery."));
        }
        db.check_host_goal_node_with_request_in_context(
            &goal.owner_key,
            &binding,
            expected_revision,
            &checks,
            &final_checks,
            &idempotency_key,
            &request_hash,
            now(),
            context::invocation_context().as_ref(),
        )
        .map_err(store_error)
    }

    pub(crate) fn host_goal_internal_tool(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        self.host_goal_internal_tool_with_lease(call, auth, None)
    }

    fn host_goal_internal_tool_with_lease(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
        lease: Option<Arc<context::ActiveOperationLease>>,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        let runtime = self.clone();
        let auth = auth.cloned();
        Box::pin(async move {
            match tokio::spawn(async move {
                let _dispatch_lease = lease;
                Box::pin(runtime.dispatch_with_auth(call, auth.as_ref())).await
            })
            .await
            {
                Ok(result) => result,
                Err(_) => failure(
                    "goal_verification_interrupted",
                    "Verification outcome is unknown; reconcile its original operation",
                ),
            }
        })
    }

    pub(crate) async fn reconcile_host_goal_operations(
        &self,
        goal: &HostGoalRecord,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        self.reconcile_host_goal_operations_inner(goal, auth, false)
            .await
    }

    async fn reconcile_host_goal_operations_inner(
        &self,
        goal: &HostGoalRecord,
        auth: Option<&AuthContext>,
        recover_checks: bool,
    ) -> Result<(), ToolResult> {
        let db = self.host_goal_db()?;
        let operations = db
            .list_host_goal_operations(&goal.owner_key, &goal.goal_id, 0, 100, true)
            .map_err(store_error)?;
        for operation in operations {
            if operation.tool_name == "present_document"
                && operation.operation_id.starts_with("review_probe_")
            {
                if operation.result_summary.is_none() && recover_checks {
                    continue;
                }
                self.resume_host_review_probe(goal, operation, auth).await?;
                continue;
            }
            if self
                .reconcile_host_goal_native_run(goal, &operation, auth)
                .await?
            {
                continue;
            }
            if operation.job_id.is_none() {
                if !recover_checks {
                    return Err(failure("goal_operation_outcome_unknown","The unresolved operation has no tracked Job. Resume its original check explicitly or supply an explicit cancellation resolution; this recovery action will not dispatch a new command."));
                }
                // Recovery is permitted only when the saved immutable plan and
                // saved input snapshot reconstruct this exact command identity.
                let version = db
                    .get_host_goal_plan(
                        &goal.owner_key,
                        &goal.goal_id,
                        Some(operation.binding.plan_version),
                    )
                    .map_err(store_error)?
                    .ok_or_else(|| {
                        failure(
                            "goal_operation_outcome_unknown",
                            "Pending operation has no saved plan",
                        )
                    })?;
                let known = version
                    .plan
                    .nodes
                    .iter()
                    .flat_map(|node| node.acceptance.iter())
                    .chain(version.plan.final_criteria.iter())
                    .find(|criterion| {
                        operation.tool_name == "run_detached_process"
                            && command_identity(
                                &operation.binding,
                                criterion,
                                &operation.input_manifest,
                            )
                            .is_ok_and(|(id, hash)| {
                                id == operation.operation_id && hash == operation.payload_hash
                            })
                    });
                if let Some(criterion) = known {
                    self.resume_host_goal_command(goal, operation, criterion, auth)
                        .await?;
                    continue;
                }
                return Err(failure("goal_operation_outcome_unknown",format!("Operation {} has an unknown outcome; reconcile the original operation before continuing",operation.operation_id)));
            }
            self.observe_host_goal_operation(goal, operation, auth)
                .await?;
        }
        Ok(())
    }

    pub(crate) fn quarantine_host_goal_job_binding(
        &self,
        goal: &HostGoalRecord,
        operation: &HostGoalOperation,
    ) -> Result<(), ToolResult> {
        let db = self.host_goal_db()?;
        context::guarded_operation_reconciliation(
            &goal.goal_id,
            &[operation.operation_id.clone()],
            || db.clear_untrusted_host_goal_job_binding(&goal.owner_key, operation, now()),
        )
        .map_err(|error| failure("goal_operation_still_active", error))?
        .map_err(store_error)?;
        Ok(())
    }

    async fn observe_host_goal_operation(
        &self,
        goal: &HostGoalRecord,
        operation: HostGoalOperation,
        auth: Option<&AuthContext>,
    ) -> Result<HostGoalOperation, ToolResult> {
        if operation.status.terminal() {
            return Ok(operation);
        }
        let job = operation.job_id.as_ref().ok_or_else(|| {
            failure(
                "goal_operation_outcome_unknown",
                "The operation has no confirmed Job",
            )
        })?;
        if !super::host_goal::execution_refs::has_native_job_receipt(&operation.tool_name) {
            self.quarantine_host_goal_job_binding(goal, &operation)?;
            return Err(failure(
                "goal_operation_outcome_unknown",
                "An untrusted historical Job binding was removed. Inspect the original provider operation and use an explicit checkpoint resolution; no Job was observed or stopped",
            ));
        }
        let result = self
            .host_goal_internal_tool(
                ToolCall::JobStatus {
                    job_id: job.clone(),
                    include_command_preview: false,
                },
                auth,
            )
            .await;
        if !result.success {
            return Err(result);
        }
        if result.output["job_id"].as_str() != Some(job.as_str())
            || result.output["project"] != goal.project
        {
            return Err(failure(
                "goal_job_identity_conflict",
                "The original Job does not match the saved goal project and execution",
            ));
        }
        let status = result.output["status"].as_str().unwrap_or("");
        let state = super::host_goal::execution_refs::job_receipt_status(&result.output);
        if state == HostGoalOperationStatus::Unknown {
            return Err(failure(
                "goal_operation_outcome_unknown",
                format!("Original Job {job} has no trustworthy terminal outcome; do not dispatch a replacement"),
            ));
        }
        if !state.terminal() {
            return Err(failure("goal_check_pending",format!("Original Job {job} is {status}; observe it, then retry the same goal check request")));
        }
        self.host_goal_db()?
            .finish_host_goal_operation(
                &goal.owner_key,
                &goal.goal_id,
                &operation.operation_id,
                state,
                Some(job),
                Some(&stable_job_result(&result.output)),
                now(),
            )
            .map_err(store_error)
    }

    async fn resume_host_goal_command(
        &self,
        goal: &HostGoalRecord,
        mut operation: HostGoalOperation,
        criterion: &GoalCriterion,
        auth: Option<&AuthContext>,
    ) -> Result<HostGoalOperation, ToolResult> {
        let lease = context::lease_active_operation(&goal.goal_id, &operation.operation_id);
        // Cancellation and dispatch admission share a process lock. Re-read
        // after leasing so a just-cancelled intent cannot be resurrected.
        operation = self
            .host_goal_db()?
            .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &operation.operation_id)
            .map_err(store_error)?
            .ok_or_else(|| {
                failure(
                    "goal_operation_missing",
                    "The saved check operation is missing",
                )
            })?;
        if operation.status.terminal() {
            return Ok(operation);
        }
        let GoalCriterion::Command {
            program,
            args,
            cwd,
            timeout_secs,
            ..
        } = criterion
        else {
            return Err(failure(
                "invalid_goal_check",
                "Expected a saved command criterion",
            ));
        };
        if operation.job_id.is_none() {
            let mut current = Vec::new();
            for saved in &operation.input_manifest {
                current.push(GoalArtifactSnapshot {
                    path: saved.path.clone(),
                    sha256: self
                        .host_goal_file_fingerprint(&goal.project, &saved.path, auth)
                        .await,
                });
            }
            if current != operation.input_manifest {
                return Err(failure("goal_check_inputs_changed","The pending check's input fingerprints changed before Job recovery. Explicitly reconcile/cancel this uncertain check, then claim a new attempt; do not redispatch its old identity."));
            }
            // run_detached_process owns durable Job idempotency; repeating its
            // original identity recovers dispatch intent rather than inventing
            // a replacement command after a lost response.
            let result = self
                .host_goal_internal_tool_with_lease(
                    ToolCall::RunDetachedProcess {
                        project: goal.project.clone(),
                        idempotency_key: operation.operation_id.clone(),
                        executable: program.clone(),
                        args: args.clone(),
                        stdin: None,
                        session_id: None,
                        timeout_secs: Some(*timeout_secs),
                        cwd: cwd.clone(),
                        purpose: None,
                    },
                    auth,
                    Some(lease.clone()),
                )
                .await;
            let job = result.output["job_id"].as_str();
            let state = if job.is_some() {
                HostGoalOperationStatus::Running
            } else if result.output["execution_state"] == "not_started"
                || result.output["dispatch_certainty"] == "not_started"
            {
                HostGoalOperationStatus::Failed
            } else {
                HostGoalOperationStatus::Unknown
            };
            let summary=json!({"job_id":job,"dispatch_certainty":if state==HostGoalOperationStatus::Failed {"not_started"}else{"unknown"},"error_kind":result.output["error_kind"]}).to_string();
            operation = self
                .host_goal_db()?
                .finish_host_goal_operation(
                    &goal.owner_key,
                    &goal.goal_id,
                    &operation.operation_id,
                    state,
                    job,
                    Some(&summary),
                    now(),
                )
                .map_err(store_error)?;
            if operation.status.terminal() {
                return Ok(operation);
            }
            if job.is_none() {
                return Err(failure("goal_check_outcome_unknown","The original check dispatch is unresolved; retry this same request to recover its idempotent Job"));
            }
        }
        self.observe_host_goal_operation(goal, operation, auth)
            .await
    }

    /// Read an ordinary file's full fingerprint without decoding binary data.
    /// A missing, denied, oversized, or malformed response supplies no proof.
    pub(crate) async fn host_goal_file_fingerprint(
        &self,
        project: &str,
        path: &str,
        auth: Option<&AuthContext>,
    ) -> Option<String> {
        if webcodex_core::goal::validate_goal_relative_path(path).is_err() {
            return None;
        }
        let result = self
            .host_goal_internal_tool(
                ToolCall::ReadProjectArtifactMetadata {
                    project: project.into(),
                    path: path.into(),
                    session_id: None,
                    allow_missing: Some(true),
                },
                auth,
            )
            .await;
        if !result.success || result.output["exists"] == false || result.output["missing"] == true {
            return None;
        }
        result.output["sha256"]
            .as_str()
            .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .map(str::to_owned)
    }

    async fn host_goal_manifest(
        &self,
        goal: &HostGoalRecord,
        criteria: &[GoalCriterion],
        verification_paths: &[String],
        auth: Option<&AuthContext>,
    ) -> Result<Vec<GoalArtifactSnapshot>, ToolResult> {
        let mut paths = BTreeSet::new();
        paths.extend(verification_paths.iter().cloned());
        for criterion in criteria {
            match criterion {
                GoalCriterion::FileExists { path, .. }
                | GoalCriterion::FileContains { path, .. } => {
                    paths.insert(path.clone());
                }
                _ => {}
            }
        }
        if paths.len() > 64 {
            return Err(failure("goal_artifact_bound_exceeded","Acceptance and state references together exceed 64 files; narrow the tracked artifacts before verification"));
        }
        let mut manifest = Vec::new();
        for path in paths {
            let sha256 = self
                .host_goal_file_fingerprint(&goal.project, &path, auth)
                .await;
            manifest.push(GoalArtifactSnapshot { path, sha256 });
        }
        if let Some(missing) = manifest
            .iter()
            .find(|item| verification_paths.contains(&item.path) && item.sha256.is_none())
        {
            return Err(failure("goal_verification_input_unreadable",format!("Declared verification input {} has no readable fingerprint; command verification cannot safely run or reuse prior evidence",missing.path)));
        }
        Ok(manifest)
    }

    async fn host_goal_checks(
        &self,
        goal: &HostGoalRecord,
        binding: &HostGoalExecutionBinding,
        criteria: &[GoalCriterion],
        supplied: &[HostGoalCheckEvidence],
        manifest: &[GoalArtifactSnapshot],
        auth: Option<&AuthContext>,
    ) -> Result<Vec<HostGoalCheckEvidence>, ToolResult> {
        let mut checks = Vec::new();
        for criterion in criteria {
            let mut check = match criterion {
                GoalCriterion::Review { id, .. } => {
                    let items: Vec<_> = supplied.iter().filter(|e| &e.criterion_id == id).collect();
                    if supplied.is_empty() {
                        HostGoalCheckEvidence {criterion_id:id.clone(),passed:false,source:HostGoalEvidenceSource::ModelSelfCheck,
                            evidence:"Pending current-version model review; automatic checks cannot approve this criterion.".into(),
                            source_refs:vec![],artifacts:manifest.to_vec(),review_packet_id:None,reviewed_scopes:vec![],findings:vec![]}
                    } else {
                        if items.len() != 1
                            || items[0].source_refs.is_empty()
                            || items[0].evidence.trim().is_empty()
                            || items[0].artifacts != manifest
                        {
                            return Err(failure("goal_review_evidence_required",format!("Supply exactly one model_self_check with sources for criterion {id}")));
                        }
                        items[0].clone()
                    }
                }
                GoalCriterion::FileExists { id, path, .. } => {
                    let hash = manifest
                        .iter()
                        .find(|a| &a.path == path)
                        .and_then(|a| a.sha256.as_ref());
                    HostGoalCheckEvidence {
                        criterion_id: id.clone(),
                        passed: hash.is_some(),
                        source: HostGoalEvidenceSource::Automated,
                        evidence: format!(
                            "File {path}; sha256={}",
                            hash.map(String::as_str).unwrap_or("unavailable")
                        ),
                        source_refs: vec![path.clone()],
                        artifacts: Vec::new(),
                        review_packet_id: None,
                        reviewed_scopes: vec![],
                        findings: vec![],
                    }
                }
                GoalCriterion::FileContains { id, path, text, .. } => {
                    let expected = manifest
                        .iter()
                        .find(|a| &a.path == path)
                        .and_then(|a| a.sha256.as_ref());
                    let mut start = 1;
                    let mut tail = String::new();
                    let mut found = false;
                    let mut complete = false;
                    for _ in 0..128 {
                        let read = self
                            .host_goal_internal_tool(
                                ToolCall::ReadFile {
                                    project: goal.project.clone(),
                                    path: path.clone(),
                                    session_id: None,
                                    start_line: Some(start),
                                    limit: Some(300),
                                    with_line_numbers: Some(false),
                                },
                                auth,
                            )
                            .await;
                        if !read.success {
                            return Err(read);
                        }
                        if expected.map(String::as_str) != read.output["sha256"].as_str() {
                            return Err(failure(
                                "goal_artifacts_changed",
                                "File changed during verification",
                            ));
                        }
                        let content = read.output["text"].as_str().ok_or_else(|| {
                            failure("goal_evidence_unavailable", "File check returned no text")
                        })?;
                        tail.push_str(content);
                        found = tail.contains(text);
                        if found || read.output["has_more"] != true {
                            complete = true;
                            break;
                        }
                        let next = read.output["next_start_line"]
                            .as_u64()
                            .filter(|next| *next > start as u64)
                            .ok_or_else(|| {
                                failure(
                                    "goal_evidence_unavailable",
                                    "File check returned no forward cursor",
                                )
                            })?;
                        start = next as usize;
                        let keep = tail
                            .char_indices()
                            .map(|(index, _)| index)
                            .find(|index| *index >= tail.len().saturating_sub(text.len()))
                            .unwrap_or(0);
                        tail = tail[keep..].into();
                        tail.push('\n');
                    }
                    if !complete {
                        return Err(failure(
                            "goal_check_bound_reached",
                            "File check exceeded its bounded read budget",
                        ));
                    }
                    HostGoalCheckEvidence {
                        criterion_id: id.clone(),
                        passed: found,
                        source: HostGoalEvidenceSource::Automated,
                        evidence: format!(
                            "{path}: required text {}",
                            if found { "found" } else { "not found" }
                        ),
                        source_refs: vec![path.clone()],
                        artifacts: Vec::new(),
                        review_packet_id: None,
                        reviewed_scopes: vec![],
                        findings: vec![],
                    }
                }
                GoalCriterion::Command { id, .. } => {
                    let (operation_id, fingerprint) =
                        command_identity(binding, criterion, manifest)?;
                    let _check_lease =
                        context::lease_active_operation(&goal.goal_id, &operation_id);
                    let db = self.host_goal_db()?;
                    let operation = match db
                        .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &operation_id)
                        .map_err(store_error)?
                    {
                        Some(operation) => operation,
                        None => {
                            db.begin_host_goal_operation_with_manifest_in_context(
                                &goal.owner_key,
                                binding,
                                &operation_id,
                                "run_detached_process",
                                &fingerprint,
                                manifest,
                                now(),
                                context::invocation_context().as_ref(),
                                true,
                            )
                            .map_err(store_error)?
                            .operation
                        }
                    };
                    if operation.input_manifest != manifest
                        || operation.payload_hash != fingerprint
                        || operation.binding != *binding
                    {
                        return Err(failure(
                            "goal_check_identity_conflict",
                            "Saved command evidence does not match the current input snapshot",
                        ));
                    }
                    terminal_check(
                        &self
                            .resume_host_goal_command(goal, operation, criterion, auth)
                            .await?,
                        id,
                    )
                }
            };
            if check.source == HostGoalEvidenceSource::Automated {
                check.artifacts = manifest.to_vec();
            }
            checks.push(check);
        }
        Ok(checks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_goal_command_identity_changes_with_input_fingerprint() {
        let binding = HostGoalExecutionBinding {
            goal_id: "goal".into(),
            node_id: "node".into(),
            attempt_id: "attempt".into(),
            plan_version: 1,
            fence: 1,
        };
        let criterion = GoalCriterion::Command {
            id: "test".into(),
            description: "Run tests".into(),
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: None,
            timeout_secs: 60,
        };
        let old = vec![GoalArtifactSnapshot {
            path: "src/main.rs".into(),
            sha256: Some("a".repeat(64)),
        }];
        let mut new = old.clone();
        new[0].sha256 = Some("b".repeat(64));
        assert_eq!(
            command_identity(&binding, &criterion, &old).unwrap(),
            command_identity(&binding, &criterion, &old).unwrap()
        );
        assert_ne!(
            command_identity(&binding, &criterion, &old).unwrap(),
            command_identity(&binding, &criterion, &new).unwrap()
        );
    }
    #[test]
    fn host_goal_terminal_projection_ignores_volatile_job_observation_fields() {
        let first = json!({"job_id":"job","status":"completed","exit_code":0,"observation_token":"a","elapsed_secs":1});
        let second = json!({"job_id":"job","status":"completed","exit_code":0,"observation_token":"b","elapsed_secs":2});
        assert_eq!(stable_job_result(&first), stable_job_result(&second));
    }

    #[test]
    fn host_goal_terminal_check_requires_canonical_job_evidence() {
        let mut operation = HostGoalOperation {
            sequence: 1,
            operation_id: "check_example".into(),
            binding: HostGoalExecutionBinding {
                goal_id: "goal".into(),
                node_id: "node".into(),
                attempt_id: "attempt".into(),
                plan_version: 1,
                fence: 1,
            },
            payload_hash: "hash".into(),
            tool_name: "run_detached_process".into(),
            purpose: HostGoalOperationPurpose::Verification,
            input_manifest: vec![],
            status: HostGoalOperationStatus::Succeeded,
            job_id: None,
            result_summary: Some(
                json!({"source":"user_reconciliation_report","evidence":"I inspected it"})
                    .to_string(),
            ),
            created_at: 1,
            updated_at: 2,
        };
        assert!(!terminal_check(&operation, "test").passed);
        operation.job_id = Some("job-real".into());
        assert!(!terminal_check(&operation, "test").passed);
        operation.result_summary = Some(stable_job_result(
            &json!({"job_id":"job-other","status":"completed","exit_code":0}),
        ));
        assert!(!terminal_check(&operation, "test").passed);
        operation.result_summary = Some(stable_job_result(
            &json!({"job_id":"job-real","status":"completed","exit_code":0}),
        ));
        assert!(terminal_check(&operation, "test").passed);
        operation.result_summary = Some(stable_job_result(
            &json!({"job_id":"job-real","status":"completed","exit_code":0,"error":"cleanup failed"}),
        ));
        assert!(!terminal_check(&operation, "test").passed);
        operation.status = HostGoalOperationStatus::Failed;
        assert!(!terminal_check(&operation, "test").passed);
    }
}
