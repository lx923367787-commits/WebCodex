//! Explicit additional CLI work from the current manager or authorized operator.
//! Original plans, attempts, assignments and receipts remain immutable.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamCliRound {
    pub round: u32,
    pub policy: TeamPolicy,
    pub plan: TeamPlanInput,
    pub first_call_indices: std::collections::BTreeMap<String, u32>,
    pub created_at_unix_ms: i64,
}

pub(crate) fn rounds_on(conn: &Connection, root: &str) -> Result<Vec<TeamCliRound>> {
    let mut stmt = conn
        .prepare("SELECT record_json FROM wc_team_cli_rounds WHERE root_task_id=?1 ORDER BY round")
        .map_err(store_error)?;
    let values = stmt
        .query_map([root], |r| r.get::<_, String>(0))
        .map_err(store_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_error)?;
    values.iter().map(|v| decode(v)).collect()
}

/// Public reads carry bounded lineage metadata, never repeated source bundles.
pub(super) fn public_rounds_on(conn: &Connection, root: &str) -> Result<Vec<serde_json::Value>> {
    rounds_on(conn, root)?.into_iter().map(|round| {
        let mut slots = Vec::new();
        for member in members(&round.plan) {
            let first = round.first_call_indices[&member.task_id];
            for (offset, call) in member.cli_calls.iter().enumerate() {
                slots.push(serde_json::json!({
                    "source_task_id":member.task_id,"call_index":first + offset as u32,
                    "provider_id":call.provider_id,"model":call.model,"effort":call.effort,
                    "instruction_sha256":webcodex_core::agent_cli::sha256(call.instruction.as_deref().unwrap_or("").as_bytes()),
                    "input_count":call.inputs.len(),"input_bytes":call.inputs.iter().map(|i|i.content.len()).sum::<usize>(),
                    "inputs_sha256":webcodex_core::agent_cli::sha256(encode(&call.inputs)?.as_bytes())
                }));
            }
        }
        Ok(serde_json::json!({"round":round.round,"policy_revision":round.policy.revision,
            "first_call_indices":round.first_call_indices,"slots":slots,"created_at_unix_ms":round.created_at_unix_ms}))
    }).collect()
}

pub(crate) fn execution_plan_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    root: &str,
) -> Result<TeamPlanRecord> {
    let mut plan = plan_on(conn, principal, root)?;
    for round in rounds_on(conn, root)? {
        for member in std::iter::once(&mut plan.plan.manager).chain(plan.plan.subagents.iter_mut())
        {
            let extra = members(&round.plan)
                .find(|m| m.task_id == member.task_id)
                .ok_or_else(not_found)?;
            if round.first_call_indices.get(&member.task_id)
                != Some(&(member.cli_calls.len() as u32))
            {
                return Err(error(
                    "team_store_invalid",
                    "Round slot lineage is inconsistent",
                ));
            }
            member.cli_calls.extend(extra.cli_calls.iter().cloned());
        }
    }
    Ok(plan)
}

pub(super) fn planned_call_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    root: &str,
    source: &str,
    index: u32,
) -> Result<TeamCliCall> {
    let plan = execution_plan_on(conn, principal, root)?;
    let call = members(&plan.plan)
        .find(|m| m.task_id == source)
        .and_then(|m| m.cli_calls.get(index as usize))
        .cloned()
        .ok_or_else(|| {
            error(
                "team_cli_slot_not_planned",
                "The CLI slot is absent from the frozen plan and authorized rounds",
            )
        });
    call
}

/// Count policies no longer gate execution. Validate the exact planned slot.
pub(super) fn cli_slot_budget_on(
    conn: &Connection,
    root: &str,
    source: &str,
    index: u32,
    _original_budget: u32,
) -> Result<()> {
    let (kind, digest): (String, String) = conn.query_row(
        "SELECT owner_principal_kind,owner_principal_digest FROM wc_team_plans WHERE root_task_id=?1", [root], |r| Ok((r.get(0)?,r.get(1)?)))
        .optional().map_err(store_error)?.ok_or_else(|| error("team_plan_required", "Register the requested work before dispatch"))?;
    planned_call_on(
        conn,
        &CommunicationPrincipal { kind, digest },
        root,
        source,
        index,
    )
    .map(|_| ())
}

impl Database {
    /// Append independent work without replaying or requiring earlier slots.
    pub fn register_agent_team_cli_round(
        &self,
        current: &crate::VerifiedAgentRun,
        root: &str,
        expected_round: u32,
        expected_policy_revision: i64,
        plan: TeamPlanInput,
        idempotency_key: &str,
    ) -> Result<TeamCliRound> {
        self.register_team_cli_round(
            Some(current),
            current.owner_user(),
            root,
            expected_round,
            expected_policy_revision,
            plan,
            idempotency_key,
        )
    }

    pub fn register_operator_team_cli_round(
        &self,
        owner: &str,
        root: &str,
        expected_round: u32,
        expected_policy_revision: i64,
        plan: TeamPlanInput,
        idempotency_key: &str,
    ) -> Result<TeamCliRound> {
        self.register_team_cli_round(
            None,
            owner,
            root,
            expected_round,
            expected_policy_revision,
            plan,
            idempotency_key,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn register_team_cli_round(
        &self,
        current: Option<&crate::VerifiedAgentRun>,
        owner: &str,
        root: &str,
        expected_round: u32,
        expected_policy_revision: i64,
        mut plan: TeamPlanInput,
        idempotency_key: &str,
    ) -> Result<TeamCliRound> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        if let Some(current) = current {
            crate::agent_isolation::authorize_agent_assignment_on_connection(
                &tx,
                current,
                root,
                crate::AgentAssignmentOperation::PlanManage,
            )
            .map_err(|e| error(e.code(), "Current manager assignment is unavailable"))?;
            crate::agent_isolation::agent_run_business_gate_on_connection(&tx, current)
                .map_err(|e| error(e.code(), "Current manager execution state is unavailable"))?;
        }
        let principal = crate::agent_isolation::enrollment_principal_for_execution(owner);
        let original = live_plan(&tx, &principal, root)?;
        let key = key_hash(idempotency_key)?;
        let fingerprint = hash(&(owner, root, expected_round, expected_policy_revision, &plan))?;
        let prior: Option<(String, String)> = tx.query_row("SELECT request_fingerprint,record_json FROM wc_team_cli_rounds WHERE root_task_id=?1 AND request_key_hash=?2", params![root,key], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?;
        if let Some((saved, value)) = prior {
            if saved != fingerprint {
                return Err(error(
                    "team_idempotency_conflict",
                    "Round key belongs to another intent",
                ));
            }
            return decode(&value);
        }
        let rounds = rounds_on(&tx, root)?;
        if rounds.len() != expected_round as usize || expected_round == u32::MAX {
            return Err(error(
                "team_round_revision_conflict",
                "Read the current round before authorizing another",
            ));
        }
        let binding = if let Some(current) = current {
            current.clone()
        } else {
            let (agent, run): (String, String) = tx.query_row("SELECT e.agent_id,r.run_id FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.agent_id=e.agent_id AND r.state='active' WHERE e.task_id=?1 AND e.owner_user=?2 AND e.purpose='manager' AND e.state='claimed'", params![root,owner], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?.ok_or_else(not_found)?;
            crate::agent_isolation::verify_agent_run_context_on_connection(
                &tx,
                &crate::agent_isolation::host_goal_owner_key(&original.project_id, &agent),
                &run,
            )
            .map_err(|e| error(e.code(), "Manager identity is unavailable"))?
        };
        crate::agent_isolation::authorize_agent_assignment_on_connection(
            &tx,
            &binding,
            root,
            crate::AgentAssignmentOperation::Read,
        )
        .map_err(|e| error(e.code(), "Manager assignment is unavailable"))?;
        let policy_project = crate::native_workspaces::team_target_on_connection(&tx, &binding)
            .map_err(|e| error(e.code(), "Named project is unavailable"))?
            .map_or_else(|| original.project_id.clone(), |f| f.project);
        let policy = policy_on(&tx, &principal, &policy_project)?;
        if policy.revision != expected_policy_revision {
            return Err(error(
                "team_policy_revision_conflict",
                "Project policy changed",
            ));
        }
        // Preserve the caller's selector in the fingerprint, while freezing
        // only the exact revision's resolved effort just like the initial plan.
        resolve_saved_policy_efforts(&policy, &mut plan)?;
        validate_plan(root, &policy, &plan)?;
        let effective = execution_plan_on(&tx, &principal, root)?;
        let before: BTreeSet<_> = members(&original.plan).map(|m| &m.task_id).collect();
        let after: BTreeSet<_> = members(&plan).map(|m| &m.task_id).collect();
        if before != after {
            return Err(error(
                "agent_team_budget_exceeded",
                "A round cannot replace the team",
            ));
        }
        let mut first_call_indices = std::collections::BTreeMap::new();
        for member in members(&plan) {
            let source = owned_task(&tx, &principal, &original.project_id, &member.task_id)?;
            if member.task_id == root || !member.cli_calls.is_empty() {
                require_live(&source)?;
            }
            if member.task_id != root && !member.cli_calls.is_empty() {
                // Still require one exact live delegated assignment. Its old
                // budget remains history; this operator round is a new grant.
                // An unchanged peer receiving no slots may still be waiting
                // on its original enrollment. It gains no CLI authority here.
                crate::agent_isolation::delegated_cli_budget_on_connection(
                    &tx,
                    &binding,
                    &member.task_id,
                )
                .map_err(|e| error(e.code(), "Member assignment is unavailable"))?;
            }
            let previous = members(&effective.plan)
                .find(|m| m.task_id == member.task_id)
                .ok_or_else(not_found)?;
            first_call_indices.insert(member.task_id.clone(), previous.cli_calls.len() as u32);
            // Every additional call receives a fresh index. Existing slots may
            // be unused, running, failed or unknown; none is retried or mutated.
        }
        let record = TeamCliRound {
            round: expected_round + 1,
            policy,
            plan,
            first_call_indices,
            created_at_unix_ms: now_unix_ms(),
        };
        tx.execute("INSERT INTO wc_team_cli_rounds(root_task_id,round,request_key_hash,request_fingerprint,record_json) VALUES(?1,?2,?3,?4,?5)", params![root,record.round,key,fingerprint,encode(&record)?]).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(record)
    }
}
