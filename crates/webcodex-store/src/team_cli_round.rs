//! Explicit operator-authorized additional CLI work for the same team.
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

/// An operator's bounded round grant supplements, never changes, enrollment.
pub(super) fn cli_slot_budget_on(
    conn: &Connection,
    root: &str,
    source: &str,
    index: u32,
    original_budget: u32,
) -> Result<()> {
    if index >= 32 {
        return Err(not_found());
    }
    for round in rounds_on(conn, root)? {
        if let (Some(first), Some(member)) = (
            round.first_call_indices.get(source),
            members(&round.plan).find(|m| m.task_id == source),
        ) {
            if (*first..*first + member.cli_calls.len() as u32).contains(&index) {
                if member.cli_calls.len() <= original_budget as usize {
                    return Ok(());
                }
                return Err(error(
                    "agent_team_budget_exceeded",
                    "Round exceeds immutable assignment budget",
                ));
            }
        }
    }
    let local_calls = crate::local_cli::call_count(conn, source).map_err(store_error)?;
    let replay: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_cli_attempts WHERE root_task_id=?1 AND source_task_id=?2 AND call_index=?3)",
        params![root,source,index], |r|r.get(0)).map_err(store_error)?;
    let (family_native, family_legacy) =
        crate::local_cli::family_counts(conn, root).map_err(store_error)?;
    if !replay && family_native > 0 && family_native.saturating_add(family_legacy) >= 32 {
        return Err(error(
            "agent_team_budget_exceeded",
            "Native and isolated calls share the original team budget",
        ));
    }
    if replay || index.saturating_add(local_calls) < original_budget {
        Ok(())
    } else {
        Err(error(
            "agent_team_budget_exceeded",
            "No explicit round grant exists",
        ))
    }
}

impl Database {
    /// Trusted operator control only. This is never an Agent/model tool.
    /// A new request must explicitly fund one bounded round and fence its predecessor.
    pub fn register_operator_team_cli_round(
        &self,
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
        if rounds.len() != expected_round as usize || expected_round >= 15 {
            return Err(error(
                "team_round_revision_conflict",
                "Read the current round before authorizing another",
            ));
        }
        let (agent, run): (String, String) = tx.query_row("SELECT e.agent_id,r.run_id FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.agent_id=e.agent_id AND r.state='active' WHERE e.task_id=?1 AND e.owner_user=?2 AND e.purpose='manager' AND e.state='claimed'", params![root,owner], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?.ok_or_else(not_found)?;
        let binding = crate::agent_isolation::verify_agent_run_context_on_connection(
            &tx,
            &crate::agent_isolation::host_goal_owner_key(&original.project_id, &agent),
            &run,
        )
        .map_err(|e| error(e.code(), "Manager identity is unavailable"))?;
        let authority = crate::agent_isolation::authorize_agent_assignment_on_connection(
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
        if before != after
            || plan.subagents.len() > authority.policy.max_subagents as usize
            || members(&plan).map(|m| m.cli_calls.len()).sum::<usize>()
                > authority.policy.max_cli_calls as usize
        {
            return Err(error(
                "agent_team_budget_exceeded",
                "A round cannot replace the team or exceed its assignment budget",
            ));
        }
        let mut first_call_indices = std::collections::BTreeMap::new();
        for member in members(&plan) {
            require_live(&owned_task(
                &tx,
                &principal,
                &original.project_id,
                &member.task_id,
            )?)?;
            let budget = if member.task_id == root {
                authority.policy.max_cli_calls
            } else {
                crate::agent_isolation::delegated_cli_budget_on_connection(
                    &tx,
                    &binding,
                    &member.task_id,
                )
                .map_err(|e| error(e.code(), "Member assignment is unavailable"))?
            };
            let previous = members(&effective.plan)
                .find(|m| m.task_id == member.task_id)
                .ok_or_else(not_found)?;
            if member.cli_calls.is_empty()
                || member.cli_calls.len() > budget as usize
                || previous.cli_calls.len() + member.cli_calls.len() > 32
            {
                return Err(error(
                    "agent_team_budget_exceeded",
                    "Round slot limit exceeded",
                ));
            }
            for index in 0..previous.cli_calls.len() {
                let raw: Option<String> = tx.query_row("SELECT record_json FROM wc_team_cli_attempts WHERE root_task_id=?1 AND source_task_id=?2 AND call_index=?3", params![root,member.task_id,index], |r| r.get(0)).optional().map_err(store_error)?;
                let attempt: TeamCliAttemptRecord = decode(&raw.ok_or_else(|| {
                    error(
                        "team_round_previous_pending",
                        "Prior slots must be terminal before another round",
                    )
                })?)?;
                let stopped = match &attempt.execution_binding {
                    Some(b) => crate::agent_execution_close::agent_execution_binding_stop_proven_on_connection(&tx,b)
                        .map_err(|e| error(e.code(), "Prior execution stop is unproven"))?,
                    None => attempt.execution_state == Some(ShellCommandExecutionState::NotStarted),
                };
                if attempt.dispatch_state != TeamCliDispatchState::Terminal || !stopped {
                    return Err(error(
                        "team_round_previous_pending",
                        "Prior slots need terminal and stopped evidence",
                    ));
                }
            }
            first_call_indices.insert(member.task_id.clone(), previous.cli_calls.len() as u32);
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
