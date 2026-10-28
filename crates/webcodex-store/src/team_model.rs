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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub brief: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_execution: Option<crate::agent_task::AgentPublicTaskExecution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_lifecycle: Option<serde_json::Value>,
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
    #[serde(default)]
    pub model: webcodex_core::cli_adapter::CliModelSelection,
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
    pub decision_reviews: Vec<crate::team_decision::DecisionSummary>,
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
            task_id:r.get(0)?,title:r.get(1)?,brief:r.get(2)?,state:r.get(3)?,public_execution:None,goal_lifecycle:None,
        })).optional().map_err(store_error)?.ok_or_else(not_found)?;
    let mut task = task;
    task.public_execution = crate::agent_task::public_execution_on_connection(conn, id)?;
    // Task ownership above is authoritative. Association rows are established
    // by enrollment under the same owner/project; a caller cannot supply an Agent.
    let goal: Option<(String, String)> = conn
        .query_row(
            "SELECT g.owner_key,g.goal_id FROM wc_agent_enrollment_assignments a
         JOIN wc_agent_enrollment_goals eg ON eg.agent_id=a.agent_id
         JOIN wc_host_goals g ON g.goal_id=eg.goal_id AND g.project=a.project
         WHERE a.task_id=?1 AND a.project=?2
           AND ((?3 IS NOT NULL AND a.agent_id=?3) OR (?3 IS NULL AND a.state='active'))",
            params![
                id,
                project,
                task.public_execution
                    .as_ref()
                    .map(|execution| execution.agent_id.as_str())
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(store_error)?;
    if let Some((owner, goal_id)) = goal {
        task.goal_lifecycle = Some(
            Database::host_goal_lifecycle_metadata_on_connection(conn, &owner, &goal_id)
                .map_err(|cause| error(cause.code(), "Task lifecycle metadata is unavailable"))?,
        );
    }
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
        decision_reviews: crate::team_decision::summaries_on(conn, root)?,
        cli_rounds: cli_round::public_rounds_on(conn, root)?,
        plan,
        tasks,
        completion,
        receipts,
    })
}

// A delegated CLI instruction belongs to that CLI instance. Keep its public
// provider/model/status metadata while removing this private copy from Agent views.
fn project_agent_plan(plan: &mut TeamPlanRecord) {
    for member in std::iter::once(&mut plan.plan.manager).chain(plan.plan.subagents.iter_mut()) {
        for call in &mut member.cli_calls {
            call.instruction = None;
        }
    }
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
            max_subagents: u32::MAX,
            max_cli_calls: u32::MAX,
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
        delegated_plan_on(&tx, binding, &plan)?;
        for child in &plan.subagents {
            delegated_cli_budget_on_connection(&tx, binding, &child.task_id)
                .map_err(assignment_error)?;
        }
        let policy_project = crate::native_workspaces::team_target_on_connection(&tx, binding)
            .map_err(assignment_error)?
            .map_or_else(|| authority.project.clone(), |f| f.project);
        let mut result = register_team_plan_on_connection(
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
        project_agent_plan(&mut result.plan);
        Ok(result)
    }

    pub fn read_agent_team_task(
        &self,
        binding: &VerifiedAgentRun,
        task: &str,
    ) -> Result<AgentTeamTask> {
        let own_task = self
            .read_agent_enrollment(binding)
            .map_err(assignment_error)?
            .task_id;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            &own_task,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment_error)?;
        let mut public = public_task_on(
            &tx,
            &authority.communication_principal,
            &authority.project,
            task,
        )?;
        if task != own_task {
            public.brief.clear();
        }
        Ok(public)
    }

    pub fn read_agent_team_plan(
        &self,
        binding: &VerifiedAgentRun,
        root: &str,
        offset: usize,
        limit: usize,
    ) -> Result<AgentTeamPlanView> {
        let own_task = self
            .read_agent_enrollment(binding)
            .map_err(assignment_error)?
            .task_id;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            &own_task,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment_error)?;
        let mut view = public_plan_on(
            &tx,
            &authority.communication_principal,
            &authority.project,
            root,
            offset,
            limit,
        )?;
        project_agent_plan(&mut view.plan);
        for task in &mut view.tasks {
            if task.task_id != own_task {
                task.brief.clear();
            }
        }
        Ok(view)
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
        // Execution metadata is shared within the authenticated owner/project.
        // Purpose, memory, acceptance and raw prompt-bearing logs are not projected.
        receipts_on(
            &tx,
            &authority.communication_principal,
            &authority.project,
            None,
            None,
            offset,
            limit,
        )
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
