//! Frozen plan quotas are admission bounds; successful completion additionally
//! requires trusted execution evidence. Reservations are not performed work.
use crate::communication::{store_error, validate_communication_principal};
use crate::{
    CommunicationPrincipal, CommunicationStoreError, Database, TeamPlanRecord, TeamPolicyMode,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

type Result<T> = std::result::Result<T, CommunicationStoreError>;

#[derive(Debug, Clone, Serialize)]
pub struct TeamMemberCompletion {
    pub task_id: String,
    pub role: &'static str,
    pub planned_cli_calls: u32,
    /// All consumed slots, including reserved, failed and unknown attempts.
    pub consumed_cli_calls: u32,
    /// Exact bound Jobs with confirmed completed/timed-out execution.
    pub confirmed_cli_calls: u32,
    pub required_cli_calls: u32,
    /// A candidate response proves a send occurred, never business success.
    pub dispatch_confirmed: bool,
    pub minimum_met: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamCompletion {
    pub root_task_id: String,
    pub project_id: String,
    pub mode: TeamPolicyMode,
    pub task_state: String,
    pub planned_subagents: u32,
    pub confirmed_subagents: u32,
    pub required_subagents: u32,
    pub planned_cli_calls: u32,
    pub consumed_cli_calls: u32,
    pub confirmed_cli_calls: u32,
    pub minimums_satisfied: bool,
    /// Quota gate only; satisfying it does not verify business acceptance.
    pub success_gate: &'static str,
    pub members: Vec<TeamMemberCompletion>,
}

fn unavailable() -> CommunicationStoreError {
    CommunicationStoreError::new("team_plan_not_found", "The team plan is unavailable")
}
fn invariant() -> CommunicationStoreError {
    CommunicationStoreError::new(
        "team_store_invalid",
        "Stored team completion evidence is inconsistent",
    )
}

pub(crate) fn completion_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    plan: &TeamPlanRecord,
) -> Result<TeamCompletion> {
    let task_state: String = conn.query_row(
        "SELECT state FROM wc_agent_tasks WHERE task_id=?1 AND referenced_project_id=?2 AND owner_principal_kind=?3 AND owner_principal_digest=?4",
        params![plan.root_task_id, plan.project_id, principal.kind, principal.digest], |row| row.get(0),
    ).optional().map_err(store_error)?.ok_or_else(unavailable)?;
    if plan.plan.manager.task_id != plan.root_task_id || plan.plan.subagents.len() > 32 {
        return Err(invariant());
    }
    let original_plan = &plan.plan;
    let rounds = crate::team_task::cli_round::rounds_on(conn, &plan.root_task_id)?;
    let effective =
        crate::team_task::cli_round::execution_plan_on(conn, principal, &plan.root_task_id)?;
    let plan = &effective;
    let custom = plan.policy.settings.mode == TeamPolicyMode::Custom;
    let required_cli_calls = if custom {
        plan.policy.settings.cli_per_agent.min
    } else {
        0
    };
    let required_cli_calls = required_cli_calls
        + rounds
            .iter()
            .map(|r| {
                if r.policy.settings.mode == TeamPolicyMode::Custom {
                    r.policy.settings.cli_per_agent.min
                } else {
                    0
                }
            })
            .sum::<u32>();
    let required_subagents = if custom {
        plan.policy.settings.subagents.min
    } else {
        0
    };
    let mut members = Vec::with_capacity(plan.plan.subagents.len() + 1);
    for (index, member) in std::iter::once(&plan.plan.manager)
        .chain(&plan.plan.subagents)
        .enumerate()
    {
        // These immutable owner/project/root/source columns bound the entire
        // evidence set; pagination or hidden records cannot weaken the gate.
        let (consumed, confirmed): (u32, u32) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN
                json_extract(record_json,'$.dispatch_state')='terminal'
                AND json_type(record_json,'$.job_id')='text'
                AND length(json_extract(record_json,'$.job_id'))>0
                AND json_extract(record_json,'$.execution_state') IN ('completed','timed_out')
                THEN 1 ELSE 0 END),0)
             FROM wc_team_cli_attempts WHERE root_task_id=?1 AND project_id=?2
                AND source_task_id=?3 AND owner_principal_kind=?4 AND owner_principal_digest=?5",
                params![
                    plan.root_task_id,
                    plan.project_id,
                    member.task_id,
                    principal.kind,
                    principal.digest
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(store_error)?;
        if member.cli_calls.len() > 32
            || consumed > member.cli_calls.len() as u32
            || confirmed > consumed
        {
            return Err(invariant());
        }
        let dispatch_confirmed = index != 0 && conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_team_agent_attempts WHERE root_task_id=?1
                AND project=?2 AND source_task_id=?3 AND owner_principal_kind=?4 AND owner_principal_digest=?5
                AND json_extract(record_json,'$.response_observed')=1
                AND json_type(record_json,'$.request_id')='text')",
            params![plan.root_task_id, plan.project_id, member.task_id, principal.kind, principal.digest],
            |row| row.get::<_, bool>(0),
        ).map_err(store_error)?;
        let original_calls = std::iter::once(&original_plan.manager)
            .chain(&original_plan.subagents)
            .find(|m| m.task_id == member.task_id)
            .ok_or_else(invariant)?
            .cli_calls
            .len() as u32;
        let confirmed_in = |first: u32, end: u32| -> Result<u32> {
            conn.query_row("SELECT COUNT(*) FROM wc_team_cli_attempts WHERE root_task_id=?1 AND project_id=?2
                AND source_task_id=?3 AND owner_principal_kind=?4 AND owner_principal_digest=?5
                AND call_index>=?6 AND call_index<?7
                AND json_extract(record_json,'$.dispatch_state')='terminal'
                AND json_type(record_json,'$.job_id')='text' AND length(json_extract(record_json,'$.job_id'))>0
                AND json_extract(record_json,'$.execution_state') IN ('completed','timed_out')",
                params![plan.root_task_id,plan.project_id,member.task_id,principal.kind,principal.digest,first,end], |r| r.get(0)).map_err(store_error)
        };
        let mut minimum_met =
            !custom || confirmed_in(0, original_calls)? >= plan.policy.settings.cli_per_agent.min;
        for round in &rounds {
            let first = *round
                .first_call_indices
                .get(&member.task_id)
                .ok_or_else(invariant)?;
            let count = std::iter::once(&round.plan.manager)
                .chain(&round.plan.subagents)
                .find(|m| m.task_id == member.task_id)
                .ok_or_else(invariant)?
                .cli_calls
                .len() as u32;
            minimum_met &= round.policy.settings.mode != TeamPolicyMode::Custom
                || confirmed_in(first, first + count)? >= round.policy.settings.cli_per_agent.min;
        }
        members.push(TeamMemberCompletion {
            task_id: member.task_id.clone(),
            role: if index == 0 { "manager" } else { "subagent" },
            planned_cli_calls: member.cli_calls.len() as u32,
            consumed_cli_calls: consumed,
            confirmed_cli_calls: confirmed,
            required_cli_calls,
            dispatch_confirmed,
            minimum_met,
        });
    }
    let confirmed_subagents = members
        .iter()
        .filter(|member| member.dispatch_confirmed)
        .count() as u32;
    let minimums_satisfied = confirmed_subagents >= required_subagents
        && members.iter().all(|member| member.minimum_met);
    Ok(TeamCompletion {
        root_task_id: plan.root_task_id.clone(),
        project_id: plan.project_id.clone(),
        mode: plan.policy.settings.mode,
        task_state,
        planned_subagents: plan.plan.subagents.len() as u32,
        confirmed_subagents,
        required_subagents,
        planned_cli_calls: members.iter().map(|member| member.planned_cli_calls).sum(),
        consumed_cli_calls: members.iter().map(|member| member.consumed_cli_calls).sum(),
        confirmed_cli_calls: members
            .iter()
            .map(|member| member.confirmed_cli_calls)
            .sum(),
        minimums_satisfied,
        success_gate: if plan.cancel_requested {
            "cancelled"
        } else if !custom {
            "not_required"
        } else if minimums_satisfied {
            "satisfied"
        } else {
            "pending"
        },
        members,
    })
}

impl Database {
    pub fn read_team_completion(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root: &str,
    ) -> Result<TeamCompletion> {
        validate_communication_principal(principal)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let encoded: String = tx.query_row(
            "SELECT record_json FROM wc_team_plans WHERE root_task_id=?1 AND project_id=?2 AND owner_principal_kind=?3 AND owner_principal_digest=?4",
            params![root, project, principal.kind, principal.digest], |row| row.get(0),
        ).optional().map_err(store_error)?.ok_or_else(unavailable)?;
        let plan: TeamPlanRecord = serde_json::from_str(&encoded).map_err(|_| invariant())?;
        if plan.root_task_id != root || plan.project_id != project {
            return Err(invariant());
        }
        let result = completion_on(&tx, principal, &plan)?;
        tx.commit().map_err(store_error)?;
        Ok(result)
    }
}

/// Called inside the existing successful AgentTask terminalization transaction,
/// after identity/fence/replay checks and before any success state is written.
pub(crate) fn require_team_completion(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    task_id: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT record_json FROM wc_team_plans WHERE owner_principal_kind=?1 AND owner_principal_digest=?2
           AND (root_task_id=?3 OR EXISTS(SELECT 1 FROM json_each(record_json,'$.plan.subagents') AS member
               WHERE json_extract(member.value,'$.task_id')=?3)) LIMIT 4097",
    ).map_err(store_error)?;
    let plans = stmt
        .query_map(params![principal.kind, principal.digest, task_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(store_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(store_error)?;
    if plans.len() > 4096 {
        return Err(invariant());
    }
    for encoded in plans {
        let plan: TeamPlanRecord = serde_json::from_str(&encoded).map_err(|_| invariant())?;
        if plan.cancel_requested {
            return Err(CommunicationStoreError::new(
                "team_completion_cancelled",
                "A cancelled team plan cannot be completed successfully",
            ));
        }
        if plan.policy.settings.mode == TeamPolicyMode::Auto {
            continue;
        }
        let progress = completion_on(conn, principal, &plan)?;
        let satisfied = if task_id == plan.root_task_id {
            progress.minimums_satisfied
        } else {
            progress
                .members
                .iter()
                .find(|member| member.task_id == task_id)
                .is_some_and(|member| member.minimum_met)
        };
        if !satisfied {
            return Err(CommunicationStoreError::new("team_minimum_execution_unmet",
                "Successful completion requires the frozen custom policy's actual dispatch/call minimums; planned slots, reservations and unknown outcomes do not satisfy them. Read team_plan_read.completion and reconcile original work; do not fabricate or duplicate calls."));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "team_completion_tests.rs"]
mod tests;
