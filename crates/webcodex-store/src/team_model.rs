//! Assignment-bound access to public team records. This module is a child of
//! team_task so the same transaction primitives enforce both entry points.
use super::*;
use crate::agent_isolation::{
    agent_run_business_gate_on_connection, authorize_agent_assignment_on_connection,
    authorize_agent_delegated_task_on_connection, delegated_cli_budget_on_connection,
};
use crate::{AgentAssignmentOperation, AgentEnrollmentPurpose, VerifiedAgentRun};

#[derive(Debug, Clone, Serialize)]
pub struct AgentTeamPolicy {
    pub project: String,
    pub policy: TeamPolicy,
    pub max_subagents: u32,
    pub max_cli_calls: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTeamTask {
    pub task_id: String,
    pub title: String,
    pub brief: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_execution: Option<crate::agent_task::AgentPublicTaskExecution>,
}

/// Explicit allowlist: no raw logs, CLI result text, private Goal or Memory.
#[derive(Debug, Clone, Serialize)]
pub struct AgentTeamReceipt {
    pub attempt_id: String,
    pub project_id: String,
    pub root_task_id: String,
    pub source_task_id: String,
    pub call_index: u32,
    pub provider_id: String,
    pub model: String,
    pub effort: Option<String>,
    pub fallback_thinking_profile_id: String,
    pub timeout_secs: Option<u64>,
    pub dispatch_state: TeamCliDispatchState,
    pub job_id: Option<String>,
    pub execution_state: Option<ShellCommandExecutionState>,
    pub error_code: Option<String>,
    pub observation_revision: i64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTeamReceiptPage {
    pub receipts: Vec<AgentTeamReceipt>,
    pub total_count: i64,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTeamPlanView {
    pub plan: TeamPlanRecord,
    pub cli_rounds: Vec<serde_json::Value>,
    pub tasks: Vec<AgentTeamTask>,
    pub completion: crate::TeamCompletion,
    pub receipts: AgentTeamReceiptPage,
}

fn assignment_error(error: crate::AgentIsolationError) -> CommunicationStoreError {
    CommunicationStoreError::new(
        error.code(),
        "Agent assignment is unavailable for this operation",
    )
}

fn manager(purpose: AgentEnrollmentPurpose) -> Result<()> {
    if purpose != AgentEnrollmentPurpose::Manager {
        return Err(error(
            "agent_assignment_denied",
            "Agent assignment is unavailable for this operation",
        ));
    }
    Ok(())
}

fn public_task_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    id: &str,
) -> Result<AgentTeamTask> {
    task_id(id)?;
    let task = conn.query_row("SELECT task_id,title,instruction,state FROM wc_agent_tasks WHERE task_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3 AND referenced_project_id=?4",
        params![id,principal.kind,principal.digest,project], |r|Ok(AgentTeamTask {
            task_id:r.get(0)?,title:r.get(1)?,brief:r.get(2)?,state:r.get(3)?,public_execution:None,
        })).optional().map_err(store_error)?.ok_or_else(not_found)?;
    let mut task = task;
    task.public_execution = crate::agent_task::public_execution_on_connection(conn, id)?;
    Ok(task)
}

fn receipts_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    root: Option<&str>,
    source: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<AgentTeamReceiptPage> {
    page_bounds(offset, limit)?;
    if root.is_none() && source.is_none() {
        return Err(not_found());
    }
    let total:i64=conn.query_row("SELECT COUNT(*) FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 AND (?4 IS NULL OR root_task_id=?4) AND (?5 IS NULL OR source_task_id=?5)",
        params![principal.kind,principal.digest,project,root,source],|r|r.get(0)).map_err(store_error)?;
    let ids = {
        let mut statement=conn.prepare("SELECT attempt_id FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 AND (?4 IS NULL OR root_task_id=?4) AND (?5 IS NULL OR source_task_id=?5) ORDER BY rowid ASC LIMIT ?6 OFFSET ?7").map_err(store_error)?;
        let ids = statement
            .query_map(
                params![
                    principal.kind,
                    principal.digest,
                    project,
                    root,
                    source,
                    limit as i64,
                    offset as i64
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(store_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(store_error)?;
        ids
    };
    let receipts = ids
        .into_iter()
        .map(|id| {
            let attempt = attempt_on(conn, principal, &id)?;
            Ok(AgentTeamReceipt {
                attempt_id: attempt.attempt_id,
                project_id: attempt.project_id,
                root_task_id: attempt.root_task_id,
                source_task_id: attempt.source_task_id,
                call_index: attempt.call_index,
                provider_id: attempt.provider_id,
                model: attempt.model,
                effort: attempt.effort,
                fallback_thinking_profile_id: attempt.fallback_thinking_profile_id,
                timeout_secs: attempt.timeout_secs,
                dispatch_state: attempt.dispatch_state,
                job_id: attempt.job_id,
                execution_state: attempt.execution_state,
                error_code: attempt.error_code,
                observation_revision: attempt.observation_revision,
                created_at_unix_ms: attempt.created_at_unix_ms,
                updated_at_unix_ms: attempt.updated_at_unix_ms,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let next = offset + receipts.len();
    Ok(AgentTeamReceiptPage {
        receipts,
        total_count: total,
        next_offset: ((next as i64) < total).then_some(next),
    })
}

fn public_plan_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    root: &str,
    offset: usize,
    limit: usize,
) -> Result<AgentTeamPlanView> {
    let plan = plan_on(conn, principal, root)?;
    if plan.project_id != project {
        return Err(not_found());
    }
    let tasks = members(&plan.plan)
        .map(|member| public_task_on(conn, principal, project, &member.task_id))
        .collect::<Result<Vec<_>>>()?;
    // Uses the authoritative full ledger, independent of the requested page.
    let completion = crate::team_completion::completion_on(conn, principal, &plan)?;
    let receipts = receipts_on(conn, principal, project, Some(root), None, offset, limit)?;
    Ok(AgentTeamPlanView {
        cli_rounds: cli_round::public_rounds_on(conn, root)?,
        plan,
        tasks,
        completion,
        receipts,
    })
}

fn delegated_plan_on(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    plan: &TeamPlanInput,
) -> Result<()> {
    for child in &plan.subagents {
        authorize_agent_delegated_task_on_connection(conn, binding, &child.task_id)
            .map_err(assignment_error)?;
    }
    Ok(())
}

impl Database {
    pub fn agent_team_policy(
        &self,
        binding: &VerifiedAgentRun,
        task: &str,
    ) -> Result<AgentTeamPolicy> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment_error)?;
        let policy_project = crate::native_workspaces::team_target_on_connection(&tx, binding)
            .map_err(assignment_error)?
            .map_or_else(|| authority.project.clone(), |f| f.project);
        Ok(AgentTeamPolicy {
            project: authority.project.clone(),
            policy: policy_on(&tx, &authority.communication_principal, &policy_project)?,
            max_subagents: authority.policy.max_subagents,
            max_cli_calls: authority.policy.max_cli_calls,
        })
    }

    pub fn register_agent_team_plan(
        &self,
        binding: &VerifiedAgentRun,
        root: &str,
        expected_policy_revision: i64,
        plan: TeamPlanInput,
        key: &str,
    ) -> Result<TeamPlanMutation> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            root,
            AgentAssignmentOperation::PlanManage,
        )
        .map_err(assignment_error)?;
        manager(authority.purpose)?;
        agent_run_business_gate_on_connection(&tx, binding).map_err(assignment_error)?;
        let cli_count = members(&plan)
            .try_fold(0usize, |count, member| {
                count.checked_add(member.cli_calls.len())
            })
            .ok_or_else(|| {
                error(
                    "agent_team_budget_exceeded",
                    "The plan exceeds the assignment budget",
                )
            })?;
        if plan.subagents.len() > authority.policy.max_subagents as usize
            || cli_count > authority.policy.max_cli_calls as usize
        {
            return Err(error(
                "agent_team_budget_exceeded",
                "The plan exceeds the assignment budget",
            ));
        }
        delegated_plan_on(&tx, binding, &plan)?;
        for child in &plan.subagents {
            let child_limit = delegated_cli_budget_on_connection(&tx, binding, &child.task_id)
                .map_err(assignment_error)?;
            if child.cli_calls.len() > child_limit as usize {
                return Err(error(
                    "agent_team_budget_exceeded",
                    "The member plan exceeds its own assignment budget",
                ));
            }
        }
        let policy_project = crate::native_workspaces::team_target_on_connection(&tx, binding)
            .map_err(assignment_error)?
            .map_or_else(|| authority.project.clone(), |f| f.project);
        let result = register_team_plan_on_connection(
            &tx,
            &authority.communication_principal,
            &authority.project,
            root,
            expected_policy_revision,
            plan,
            key,
            &policy_project,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(result)
    }

    pub fn read_agent_team_task(
        &self,
        binding: &VerifiedAgentRun,
        task: &str,
    ) -> Result<AgentTeamTask> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment_error)?;
        public_task_on(
            &tx,
            &authority.communication_principal,
            &authority.project,
            task,
        )
    }

    pub fn read_agent_team_plan(
        &self,
        binding: &VerifiedAgentRun,
        root: &str,
        offset: usize,
        limit: usize,
    ) -> Result<AgentTeamPlanView> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            root,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment_error)?;
        manager(authority.purpose)?;
        let plan = plan_on(&tx, &authority.communication_principal, root)?;
        delegated_plan_on(&tx, binding, &plan.plan)?;
        public_plan_on(
            &tx,
            &authority.communication_principal,
            &authority.project,
            root,
            offset,
            limit,
        )
    }

    pub fn list_agent_team_plans(
        &self,
        binding: &VerifiedAgentRun,
        task: &str,
        offset: usize,
        limit: usize,
    ) -> Result<TeamPlanPage> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment_error)?;
        manager(authority.purpose)?;
        page_bounds(offset, limit)?;
        let principal = &authority.communication_principal;
        let total:i64=tx.query_row("SELECT COUNT(*) FROM wc_team_plans WHERE root_task_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3 AND project_id=?4",
            params![task,principal.kind,principal.digest,authority.project],|r|r.get(0)).map_err(store_error)?;
        let mut plans = Vec::new();
        if total == 1 && offset == 0 {
            let plan = plan_on(&tx, principal, task)?;
            delegated_plan_on(&tx, binding, &plan.plan)?;
            plans.push(TeamPlanSummary {
                project_id: plan.project_id,
                root_task_id: plan.root_task_id,
                policy_revision: plan.policy.revision,
                status: plan.status,
                cancel_requested: plan.cancel_requested,
                subagent_count: plan.plan.subagents.len(),
                cli_call_count: members(&plan.plan)
                    .map(|member| member.cli_calls.len())
                    .sum(),
                created_at_unix_ms: plan.created_at_unix_ms,
                updated_at_unix_ms: plan.updated_at_unix_ms,
            });
        }
        let next = offset + plans.len();
        Ok(TeamPlanPage {
            plans,
            total_count: total,
            next_offset: ((next as i64) < total).then_some(next),
        })
    }

    pub fn list_agent_team_receipts(
        &self,
        binding: &VerifiedAgentRun,
        task: &str,
        offset: usize,
        limit: usize,
    ) -> Result<AgentTeamReceiptPage> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment_error)?;
        public_task_on(
            &tx,
            &authority.communication_principal,
            &authority.project,
            task,
        )?;
        if authority.purpose == AgentEnrollmentPurpose::Manager {
            let plan = plan_on(&tx, &authority.communication_principal, task)?;
            delegated_plan_on(&tx, binding, &plan.plan)?;
            receipts_on(
                &tx,
                &authority.communication_principal,
                &authority.project,
                Some(task),
                None,
                offset,
                limit,
            )
        } else {
            receipts_on(
                &tx,
                &authority.communication_principal,
                &authority.project,
                None,
                Some(task),
                offset,
                limit,
            )
        }
    }

    /// Human PAT bridge: exact project authorization is retained in the runtime.
    pub fn read_office_team_plan(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root: &str,
        offset: usize,
        limit: usize,
    ) -> Result<AgentTeamPlanView> {
        validate_communication_principal(principal)?;
        project_id(project)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        public_plan_on(&tx, principal, project, root, offset, limit)
    }
}

#[cfg(test)]
#[path = "team_model_tests.rs"]
mod tests;
