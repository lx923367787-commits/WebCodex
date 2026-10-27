//! Current-assignment reads for one frozen CLI slot and its exact prior intent.
//! These projections never authorize a send or contain private launch material.
use super::*;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentCliPlannedSlot {
    pub project: String,
    pub root_task_id: String,
    pub source_task_id: String,
    pub call_index: u32,
    pub provider_id: String,
    pub model: String,
    pub effort: Option<String>,
    pub instruction: String,
    pub inputs: Vec<crate::AgentEnrollmentInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCliPreparationPhase {
    Unprepared,
    Prepared,
    Finalized,
    Cancelled,
}

/// Controller-only metadata. No serde or opaque execution permission is carried.
#[derive(Debug)]
pub struct AgentCliPreparationSnapshot {
    pub receipt: super::super::model::AgentTeamReceipt,
    pub phase: AgentCliPreparationPhase,
    pub binding: Option<AgentExecutionBinding>,
    pub resume_fence: Option<String>,
}

impl Database {
    /// Read-only validation for restoring a trusted preparation reservation.
    /// This is never dispatch authority; finalization must still pass its own
    /// atomic admission gate and produce the sole opaque dispatch permit.
    pub fn recoverable_agent_cli_preparation(
        &self,
        current: &VerifiedAgentRun,
        identity: &AgentExecutionBinding,
    ) -> Result<AgentExecutionBinding> {
        identity.identity_hash().map_err(|_| denied())?;
        if identity.requester.run != run_ref(current) || identity.project != current.project() {
            return Err(denied());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let auth = source_authority(
            &tx,
            current,
            &identity.requester.root_task_id,
            &identity.requester.source_task_id,
            true,
        )?;
        cli_slot_budget_on(
            &tx,
            &identity.requester.root_task_id,
            &identity.requester.source_task_id,
            identity.requester.call_index,
            auth.policy.max_cli_calls,
        )?;
        let requester = enrollment_projection_on_connection(&tx, current).map_err(auth_error)?;
        if requester.enrollment_id != identity.requester.enrollment_id {
            return Err(denied());
        }
        ensure_agent_run_execution_open_on_connection(&tx, &identity.requester.run)
            .map_err(auth_error)?;
        ensure_agent_run_execution_open_on_connection(&tx, &identity.executor.run)
            .map_err(auth_error)?;
        let worker = verify_agent_run_context_on_connection(
            &tx,
            &crate::agent_isolation::host_goal_owner_key(
                &identity.project,
                &identity.executor.run.agent_id,
            ),
            &identity.executor.run.agent_run_id,
        )
        .map_err(auth_error)?;
        if run_ref(&worker) != identity.executor.run || worker.owner_user() != current.owner_user()
        {
            return Err(denied());
        }
        let worker_auth = authorize_agent_assignment_on_connection(
            &tx,
            &worker,
            &identity.executor.task_id,
            AgentAssignmentOperation::TaskStart,
        )
        .map_err(auth_error)?;
        let worker_projection =
            enrollment_projection_on_connection(&tx, &worker).map_err(auth_error)?;
        if worker_projection.enrollment_id != identity.executor.enrollment_id
            || worker_auth.purpose != AgentEnrollmentPurpose::CliWorker
            || worker_auth.policy.max_subagents != 0
            || worker_auth.policy.max_cli_calls != 0
        {
            return Err(denied());
        }
        let linked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments WHERE enrollment_id=?1 AND source_agent_id=?2 AND COALESCE(delivery_source_run_id,source_run_id)=?3 AND COALESCE(delivery_source_generation,source_generation)=?4)",params![identity.executor.enrollment_id,current.agent_id(),current.run_id(),current.generation()],|r|r.get(0)).map_err(store_error)?;
        if !linked {
            return Err(denied());
        }
        let attempt = load_team_cli_attempt_for_fallback(
            &tx,
            &auth.communication_principal,
            &identity.requester.cli_attempt_id,
        )?;
        if attempt.project_id != identity.project
            || attempt.root_task_id != identity.requester.root_task_id
            || attempt.source_task_id != identity.requester.source_task_id
            || attempt.call_index != identity.requester.call_index
            || attempt.dispatch_state != TeamCliDispatchState::Reserved
            || attempt.execution_binding.is_some()
            || attempt.job_id.is_some()
            || !attempt
                .effort
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(denied());
        }
        let (raw,phase,start_present,saved_fingerprint):(Option<String>,Option<String>,bool,String)=tx.query_row("SELECT execution_preparation_json,execution_preparation_state,execution_start_json IS NOT NULL,request_fingerprint FROM wc_team_cli_attempts WHERE attempt_id=?1",[&attempt.attempt_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(store_error)?;
        if phase.as_deref() != Some("prepared") || start_present {
            return Err(denied());
        }
        let stored: AgentExecutionBinding = decode(&raw.ok_or_else(denied)?)?;
        if &stored != identity {
            return Err(denied());
        }
        let (_, fingerprint) = cli_reservation_identity(
            &attempt.project_id,
            &attempt.root_task_id,
            &attempt.source_task_id,
            attempt.call_index,
            "admission-identity",
            &attempt.fallback_thinking_profile_id,
            attempt.timeout_secs,
        )?;
        if fingerprint != saved_fingerprint {
            return Err(denied());
        }
        Ok(stored)
    }

    pub fn read_agent_cli_planned_slot(
        &self,
        current: &VerifiedAgentRun,
        root_task_id: &str,
        source_task_id: &str,
        call_index: u32,
    ) -> Result<AgentCliPlannedSlot> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let auth = source_authority(&tx, current, root_task_id, source_task_id, false)?;
        cli_slot_budget_on(
            &tx,
            root_task_id,
            source_task_id,
            call_index,
            auth.policy.max_cli_calls,
        )?;
        owned_task(
            &tx,
            &auth.communication_principal,
            current.project(),
            root_task_id,
        )?;
        let source = owned_task(
            &tx,
            &auth.communication_principal,
            current.project(),
            source_task_id,
        )?;
        // Both task ownership and the caller's exact assignment were checked
        // above. A missing plan here is an unmet prerequisite, not a missing
        // task; preserve existence hiding for callers rejected before this.
        let plan = execution_plan_on(&tx, &auth.communication_principal, root_task_id).map_err(
            |cause| {
                if cause.code() == "team_task_not_found" {
                    error(
                        "team_plan_required",
                        "Register the team's frozen plan before starting a CLI slot",
                    )
                } else {
                    cause
                }
            },
        )?;
        if plan.project_id != current.project() {
            return Err(denied());
        }
        let call = members(&plan.plan)
            .find(|member| member.task_id == source_task_id)
            .and_then(|member| member.cli_calls.get(call_index as usize))
            .ok_or_else(|| {
                error(
                    "team_cli_slot_not_planned",
                    "The exact CLI slot is not in the frozen plan",
                )
            })?;
        if call
            .effort
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(error("invalid_team_request", "Frozen CLI effort is blank"));
        }
        Ok(AgentCliPlannedSlot {
            project: current.project().into(),
            root_task_id: root_task_id.into(),
            source_task_id: source_task_id.into(),
            call_index,
            provider_id: call.provider_id.clone(),
            model: call.model.clone(),
            effort: call.effort.clone(),
            instruction: call.instruction.clone().unwrap_or(source.instruction),
            inputs: call.inputs.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn read_agent_cli_preparation(
        &self,
        current: &VerifiedAgentRun,
        root_task_id: &str,
        source_task_id: &str,
        call_index: u32,
        idempotency_key: &str,
        fallback_thinking_profile_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<AgentCliPreparationSnapshot>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let auth = source_authority(&tx, current, root_task_id, source_task_id, false)?;
        let (key, fingerprint) = cli_reservation_identity(
            current.project(),
            root_task_id,
            source_task_id,
            call_index,
            idempotency_key,
            fallback_thinking_profile_id,
            timeout_secs,
        )?;
        owned_task(
            &tx,
            &auth.communication_principal,
            current.project(),
            root_task_id,
        )?;
        owned_task(
            &tx,
            &auth.communication_principal,
            current.project(),
            source_task_id,
        )?;
        // Scope before looking up a key: an Agent cannot probe another Agent's
        // owner-wide idempotency namespace to learn whether its key exists.
        let found:Option<(String,String,String)>=tx.query_row(
            "SELECT attempt_id,request_key_hash,request_fingerprint FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 AND root_task_id=?4 AND source_task_id=?5 AND call_index=?6",
            params![auth.communication_principal.kind,auth.communication_principal.digest,current.project(),root_task_id,source_task_id,call_index],
            |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional().map_err(store_error)?;
        let Some((id, saved_key, saved_fingerprint)) = found else {
            return Ok(None);
        };
        if saved_key != key || saved_fingerprint != fingerprint {
            return Err(error(
                "team_idempotency_conflict",
                "The original CLI slot has a different request intent",
            ));
        }
        let attempt = attempt_on(&tx, &auth.communication_principal, &id)?;
        if attempt.project_id != current.project()
            || attempt.root_task_id != root_task_id
            || attempt.source_task_id != source_task_id
            || attempt.call_index != call_index
        {
            return Err(denied());
        }
        let (raw, phase, start_present):(Option<String>,Option<String>,bool)=tx.query_row(
            "SELECT execution_preparation_json,execution_preparation_state,execution_start_json IS NOT NULL FROM wc_team_cli_attempts WHERE attempt_id=?1",
            [&id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).map_err(store_error)?;
        let Some(raw) = raw else {
            if phase.is_some() || start_present || attempt.execution_binding.is_some() {
                return Err(denied());
            }
            return Ok(Some(AgentCliPreparationSnapshot {
                receipt: receipt(&attempt),
                phase: AgentCliPreparationPhase::Unprepared,
                binding: None,
                resume_fence: None,
            }));
        };
        let identity: AgentExecutionBinding = decode(&raw)?;
        let identity_hash = identity.identity_hash().map_err(|_| denied())?;
        if identity.project != current.project()
            || identity.requester.run.agent_id != current.agent_id()
            || identity.requester.root_task_id != root_task_id
            || identity.requester.source_task_id != source_task_id
            || identity.requester.call_index != call_index
            || identity.requester.cli_attempt_id != id
        {
            return Err(denied());
        }
        if let Some(frozen) = &attempt.execution_binding {
            frozen.validate().map_err(|_| denied())?;
            if phase.as_deref() != Some("finalized")
                || !start_present
                || attempt.dispatch_state == TeamCliDispatchState::Reserved
                || frozen.identity_hash().map_err(|_| denied())? != identity_hash
            {
                return Err(denied());
            }
            return Ok(Some(AgentCliPreparationSnapshot {
                receipt: receipt(&attempt),
                phase: AgentCliPreparationPhase::Finalized,
                binding: Some(frozen.clone()),
                resume_fence: None,
            }));
        }
        if start_present || attempt.job_id.is_some() {
            return Err(denied());
        }
        let (phase, resume_fence) = match phase.as_deref() {
            Some("prepared") if attempt.dispatch_state == TeamCliDispatchState::Reserved => {
                (AgentCliPreparationPhase::Prepared, None)
            }
            Some("cancelled")
                if attempt.dispatch_state == TeamCliDispatchState::Terminal
                    && attempt.execution_state == Some(ShellCommandExecutionState::NotStarted)
                    && attempt.error_code.as_deref() == Some("agent_execution_closed") =>
            {
                (AgentCliPreparationPhase::Cancelled, Some(identity_hash))
            }
            _ => return Err(denied()),
        };
        Ok(Some(AgentCliPreparationSnapshot {
            receipt: receipt(&attempt),
            phase,
            binding: Some(identity),
            resume_fence,
        }))
    }
}

#[cfg(test)]
#[path = "agent_execution_read_tests.rs"]
mod tests;
