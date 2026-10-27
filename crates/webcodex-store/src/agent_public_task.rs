//! Logical-Agent ownership of an existing public Task. This is a business
//! lifecycle receipt, not a Runner start/stop proof or a second task ledger.
use super::*;
use crate::agent_isolation::{
    authorize_agent_assignment_on_connection, require_enrollment_public_task_on_connection,
};
use crate::{AgentAssignmentOperation, VerifiedAgentRun};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentPublicTaskExecution {
    pub execution_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub started_run_id: String,
    pub started_generation: i64,
    pub state: AgentTaskState,
    pub revision: i64,
    pub started_at_unix_ms: i64,
    pub terminal_at_unix_ms: Option<i64>,
    pub completed_run_id: Option<String>,
    pub completed_generation: Option<i64>,
    pub terminal_result: Option<String>,
    pub terminal_reason: Option<String>,
    pub decision_reviews: Vec<crate::team_decision::DecisionSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentPublicTaskMutation {
    pub task: AgentTaskSummary,
    pub execution: AgentPublicTaskExecution,
    pub replayed: bool,
    pub state_changed: bool,
}

/// Trusted observer read of exactly its WorkerTask. The revision for replay is
/// computed here from the committed lifecycle transition, never guessed by C.
#[derive(Debug, Clone)]
pub struct AgentCliWorkerTaskSnapshot {
    pub execution: AgentPublicTaskExecution,
    pub completion_expected_revision: i64,
}

fn denied(code: &'static str) -> CommunicationStoreError {
    CommunicationStoreError::new(
        code,
        "Public task execution is unavailable for this operation",
    )
}
fn assignment(error: crate::AgentIsolationError) -> CommunicationStoreError {
    denied(error.code())
}
fn key_hash(key: &str) -> Result<String, CommunicationStoreError> {
    Ok(digest_text(
        "webcodex.public-task.key.v1",
        &validate_idempotency_key(key)?,
    ))
}

pub(super) fn ensure_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_agent_public_executions (
        task_id TEXT PRIMARY KEY REFERENCES wc_agent_tasks(task_id), execution_id TEXT NOT NULL UNIQUE,
        agent_id TEXT NOT NULL REFERENCES wc_isolated_agents(agent_id),
        started_run_id TEXT NOT NULL REFERENCES wc_agent_runs(run_id), started_generation INTEGER NOT NULL CHECK(started_generation>0),
        state TEXT NOT NULL CHECK(state IN ('active','succeeded','failed')), revision INTEGER NOT NULL CHECK(revision>0),
        started_at_unix_ms INTEGER NOT NULL, terminal_at_unix_ms INTEGER,
        completed_run_id TEXT REFERENCES wc_agent_runs(run_id), completed_generation INTEGER,
        terminal_result TEXT, terminal_reason TEXT,start_key_hash TEXT NOT NULL,start_request_hash TEXT NOT NULL,
        completion_key_hash TEXT,completion_request_hash TEXT,
        CHECK((state='active' AND terminal_at_unix_ms IS NULL AND completed_run_id IS NULL AND completion_key_hash IS NULL)
           OR (state IN ('succeeded','failed') AND terminal_at_unix_ms IS NOT NULL AND completed_run_id IS NOT NULL AND completed_generation>0 AND completion_key_hash IS NOT NULL AND completion_request_hash IS NOT NULL)),
        UNIQUE(agent_id,start_key_hash),UNIQUE(agent_id,completion_key_hash));
        CREATE TRIGGER IF NOT EXISTS public_task_no_legacy_attempt BEFORE INSERT ON wc_agent_task_attempts
        WHEN EXISTS(SELECT 1 FROM wc_agent_public_executions WHERE task_id=NEW.task_id)
        BEGIN SELECT RAISE(ABORT,'public task execution conflicts with legacy attempt'); END;
        CREATE TRIGGER IF NOT EXISTS public_task_no_legacy_owner BEFORE UPDATE OF assignee_agent_id,latest_attempt_id ON wc_agent_tasks
        WHEN EXISTS(SELECT 1 FROM wc_agent_public_executions WHERE task_id=NEW.task_id)
             AND (NEW.assignee_agent_id IS NOT NULL OR NEW.latest_attempt_id IS NOT NULL)
        BEGIN SELECT RAISE(ABORT,'public task execution conflicts with legacy owner'); END;
        CREATE TRIGGER IF NOT EXISTS public_task_no_existing_legacy BEFORE INSERT ON wc_agent_public_executions
        WHEN EXISTS(SELECT 1 FROM wc_agent_task_attempts WHERE task_id=NEW.task_id)
          OR EXISTS(SELECT 1 FROM wc_agent_tasks WHERE task_id=NEW.task_id AND (assignee_agent_id IS NOT NULL OR latest_attempt_id IS NOT NULL))
        BEGIN SELECT RAISE(ABORT,'legacy task execution already owns task'); END;")?;
    Ok(())
}

pub(super) fn legacy_gate(conn: &Connection, task: &str) -> Result<(), CommunicationStoreError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_public_executions WHERE task_id=?1)",
            [task],
            |r| r.get(0),
        )
        .map_err(store_error)?;
    if exists {
        Err(denied("agent_task_execution_conflict"))
    } else {
        Ok(())
    }
}

pub(crate) fn public_execution_on_connection(
    conn: &Connection,
    task: &str,
) -> Result<Option<AgentPublicTaskExecution>, CommunicationStoreError> {
    let decision_reviews = crate::team_decision::completion_on(conn, task)?;
    conn.query_row("SELECT execution_id,task_id,agent_id,started_run_id,started_generation,state,revision,started_at_unix_ms,terminal_at_unix_ms,completed_run_id,completed_generation,terminal_result,terminal_reason FROM wc_agent_public_executions WHERE task_id=?1",[task],|r|Ok(AgentPublicTaskExecution {
        execution_id:r.get(0)?,task_id:r.get(1)?,agent_id:r.get(2)?,started_run_id:r.get(3)?,started_generation:r.get(4)?,
        state:AgentTaskState::from_db(&r.get::<_,String>(5)?,5)?,revision:r.get(6)?,started_at_unix_ms:r.get(7)?,terminal_at_unix_ms:r.get(8)?,
        completed_run_id:r.get(9)?,completed_generation:r.get(10)?,terminal_result:r.get(11)?,terminal_reason:r.get(12)?,decision_reviews:decision_reviews.clone(),
    })).optional().map_err(store_error)
}

/// The original CLI and Host ledgers remain authoritative. A terminal enum is
/// not a process-stop proof; exclude only this actor's own incoming Host carrier.
pub(crate) fn require_public_task_quiescent_on_connection(
    conn: &Connection,
    owner_user: &str,
    project: &str,
    task: &str,
    completing_actor: Option<&VerifiedAgentRun>,
) -> Result<(), CommunicationStoreError> {
    crate::agent_execution_close::require_agent_task_execution_quiescent_on_connection(
        conn, owner_user, project, task,
    ).map_err(|error| {
        if error.code() == "agent_execution_stop_proof_required" {
            denied("public_task_execution_stop_proof_required")
        } else {
            assignment(error)
        }
    })?;
    crate::agent_host_dispatch::require_agent_host_dispatch_quiescent_on_connection(
        conn, owner_user, project, task, completing_actor,
    ).map_err(assignment)?;
    let principal = crate::agent_isolation::enrollment_principal_for_execution(owner_user);
    // A stopped CLI process still owes an independent WorkerTask disposition.
    // Exclude only the task being completed, so its own review can close it.
    let pending_worker: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM wc_team_cli_attempts a
         LEFT JOIN wc_agent_tasks worker ON worker.task_id=json_extract(a.record_json,'$.execution_binding.executor.task_id')
         LEFT JOIN wc_agent_public_executions execution ON execution.task_id=worker.task_id
         WHERE a.owner_principal_kind=?1 AND a.owner_principal_digest=?2 AND a.project_id=?3
         AND (a.root_task_id=?4 OR a.source_task_id=?4)
         AND json_type(a.record_json,'$.execution_binding')='object'
         AND json_extract(a.record_json,'$.execution_binding.executor.task_id')<>?4
         AND (worker.task_id IS NULL OR worker.state NOT IN ('succeeded','failed')
              OR execution.task_id IS NULL OR execution.state NOT IN ('succeeded','failed')
              OR execution.state<>worker.state
              OR execution.agent_id<>json_extract(a.record_json,'$.execution_binding.executor.run.agent_id')))",
        params![principal.kind,principal.digest,project,task], |r| r.get(0),
    ).map_err(store_error)?;
    if pending_worker { return Err(denied("public_task_worker_delivery_pending")); }
    let children:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_plans p JOIN json_each(p.record_json,'$.plan.subagents') m JOIN wc_agent_tasks t ON t.task_id=json_extract(m.value,'$.task_id') WHERE p.root_task_id=?4 AND p.owner_principal_kind=?1 AND p.owner_principal_digest=?2 AND p.project_id=?3 AND t.state='active')",params![principal.kind,principal.digest,project,task],|r|r.get(0)).map_err(store_error)?;
    if children { Err(denied("public_task_execution_tree_active")) } else { Ok(()) }
}

fn audit(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    action: &str,
) -> Result<(), CommunicationStoreError> {
    conn.execute("INSERT INTO wc_agent_isolation_audit(event_id,agent_id,run_id,action,code,created_at_unix_ms) VALUES(?1,?2,?3,?4,'ok',?5)",
        params![uuid::Uuid::new_v4().to_string(),binding.agent_id(),binding.run_id(),action,now_unix_ms()]).map_err(store_error)?;
    Ok(())
}

/// Shared transaction primitive. Only the independently authorized delivery
/// review path may request success; the retained-observer API allows failure.
#[allow(clippy::too_many_arguments)]
pub(crate) fn complete_cli_worker_task_on_connection(
    conn: &Connection,
    binding: &webcodex_core::runner_protocol::AgentExecutionBinding,
    expected_revision: i64,
    outcome: AgentTaskState,
    terminal_result: Option<&str>,
    terminal_reason: Option<&str>,
    completion_key: &str,
) -> Result<AgentPublicTaskMutation, CommunicationStoreError> {
    if !outcome.terminal() || expected_revision < 1 {
        return Err(denied("invalid_public_task_completion"));
    }
    let result = validate_optional_terminal_text(terminal_result, "terminal_result")?;
    let reason = validate_optional_terminal_text(terminal_reason, "terminal_reason")?;
    if result.as_deref().into_iter().chain(reason.as_deref())
        .any(webcodex_core::sensitive_text::secret_like_value) {
        return Err(denied("public_task_sensitive_terminal_text"));
    }
    let key = key_hash(completion_key)?;
    let original: String = conn.query_row("SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1", [&binding.requester.cli_attempt_id], |r| r.get(0)).map_err(store_error)?;
    let original: crate::TeamCliAttemptRecord = serde_json::from_str(&original)
        .map_err(|_| denied("agent_task_storage_invariant"))?;
    if original.execution_binding.as_ref() != Some(binding) {
        return Err(denied("public_task_execution_conflict"));
    }
    let executor = &binding.executor;
    let task_id = &executor.task_id;
    // Scope is derived solely from the frozen worker identity, not input IDs.
    let owner: Option<String> = conn.query_row(
        "SELECT a.owner_user FROM wc_isolated_agents a JOIN wc_agent_enrollments e ON e.agent_id=a.agent_id WHERE a.agent_id=?1 AND a.project=?2 AND e.enrollment_id=?3 AND e.task_id=?4 AND e.purpose='cli_worker'",
        params![executor.run.agent_id,binding.project,executor.enrollment_id,task_id], |r|r.get(0),
    ).optional().map_err(store_error)?;
    let owner = owner.ok_or_else(||denied("public_task_execution_conflict"))?;
    let principal = crate::agent_isolation::enrollment_principal_for_execution(&owner);
    let now = now_unix_ms();
    let task = load_owned_task(conn,&principal,task_id,now)?;
    let execution = public_execution_on_connection(conn,task_id)?.ok_or_else(||denied("public_task_execution_missing"))?;
    if task.referenced_project_id.as_deref()!=Some(binding.project.as_str())
        || execution.agent_id!=executor.run.agent_id {
        return Err(denied("public_task_execution_conflict"));
    }
    let hash=task_request_hash(&json!({"task_id":task_id,"execution_id":execution.execution_id,"expected_revision":expected_revision,"outcome":outcome,"result":result,"reason":reason}));
    if execution.state.terminal() {
        let saved:(Option<String>,Option<String>)=conn.query_row("SELECT completion_key_hash,completion_request_hash FROM wc_agent_public_executions WHERE task_id=?1",[task_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store_error)?;
        if saved.0.as_deref()!=Some(key.as_str()) || saved.1.as_deref()!=Some(hash.as_str()) {
            return Err(denied("public_task_idempotency_conflict"));
        }
        return Ok(AgentPublicTaskMutation {task:task.summary(now),execution,replayed:true,state_changed:false});
    }
    if task.stored_state!=AgentTaskState::Active || execution.revision!=expected_revision {
        return Err(denied("public_task_revision_conflict"));
    }
    let conflicting: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments WHERE owner_user=?1 AND project=?2 AND task_id=?3 AND state='active' AND agent_id<>?4)",params![owner,binding.project,task_id,executor.run.agent_id],|r|r.get(0)).map_err(store_error)?;
    if conflicting { return Err(denied("public_task_execution_conflict")); }
    if !crate::agent_execution_close::agent_execution_binding_stop_proven_on_connection(conn,&binding).map_err(assignment)? {
        return Err(denied("public_task_execution_stop_proof_required"));
    }
    require_public_task_quiescent_on_connection(conn,&owner,&binding.project,task_id,None)?;
    let record_json: String = conn.query_row("SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1",[&binding.requester.cli_attempt_id],|r|r.get(0)).map_err(store_error)?;
    let attempt: crate::TeamCliAttemptRecord = serde_json::from_str(&record_json)
        .map_err(|_| denied("agent_task_storage_invariant"))?;
    if attempt.dispatch_state!=crate::TeamCliDispatchState::Terminal {
        return Err(denied("public_task_execution_observation_required"));
    }
    if outcome==AgentTaskState::Succeeded
        && (attempt.execution_state!=Some(webcodex_core::runner_protocol::ShellCommandExecutionState::Completed)
            || attempt.error_code.is_some()) {
        return Err(denied("public_task_success_not_proven"));
    }
    // CliWorker is not a plan member and has zero delegation budget. Do not
    // impose the requester's CLI minimum recursively on this worker Task.
    conn.execute("UPDATE wc_agent_public_executions SET state=?2,revision=revision+1,terminal_at_unix_ms=?3,completed_run_id=?4,completed_generation=?5,terminal_result=?6,terminal_reason=?7,completion_key_hash=?8,completion_request_hash=?9 WHERE task_id=?1",params![task_id,outcome.as_str(),now,executor.run.agent_run_id,executor.run.generation,result,reason,key,hash]).map_err(store_error)?;
    conn.execute("UPDATE wc_agent_tasks SET state=?2,terminal_attempt_id=?3,terminal_at_unix_ms=?4,updated_at_unix_ms=MAX(updated_at_unix_ms,?4) WHERE task_id=?1",params![task_id,outcome.as_str(),execution.execution_id,now]).map_err(store_error)?;
    conn.execute("INSERT INTO wc_agent_isolation_audit(event_id,agent_id,run_id,action,code,created_at_unix_ms) VALUES(?1,?2,?3,'complete_cli_worker_task','ok',?4)",params![uuid::Uuid::new_v4().to_string(),executor.run.agent_id,executor.run.agent_run_id,now]).map_err(store_error)?;
    let task=load_owned_task(conn,&principal,task_id,now)?.summary(now);
    let execution=public_execution_on_connection(conn,task_id)?.ok_or_else(||denied("public_task_execution_missing"))?;
    Ok(AgentPublicTaskMutation{task,execution,replayed:false,state_changed:true})
}

impl Database {
    pub fn read_agent_cli_worker_task(
        &self,
        permit: &crate::AgentCliObservationPermit,
    ) -> Result<Option<AgentCliWorkerTaskSnapshot>, CommunicationStoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let binding = crate::team_task::validate_agent_cli_observation_on_connection(&tx, permit)?;
        let executor = &binding.executor;
        let owner: Option<String> = tx.query_row(
            "SELECT a.owner_user FROM wc_isolated_agents a JOIN wc_agent_enrollments e ON e.agent_id=a.agent_id WHERE a.agent_id=?1 AND a.project=?2 AND e.enrollment_id=?3 AND e.task_id=?4 AND e.purpose='cli_worker'",
            params![executor.run.agent_id,binding.project,executor.enrollment_id,executor.task_id], |r|r.get(0),
        ).optional().map_err(store_error)?;
        let owner = owner.ok_or_else(||denied("public_task_execution_conflict"))?;
        let principal = crate::agent_isolation::enrollment_principal_for_execution(&owner);
        let task = load_owned_task(&tx, &principal, &executor.task_id, now_unix_ms())?;
        if task.referenced_project_id.as_deref() != Some(binding.project.as_str()) {
            return Err(denied("public_task_not_found"));
        }
        let Some(execution) = public_execution_on_connection(&tx,&executor.task_id)? else { return Ok(None); };
        if execution.agent_id != executor.run.agent_id {
            return Err(denied("public_task_execution_conflict"));
        }
        let completion_expected_revision = execution.revision - i64::from(execution.state.terminal());
        if completion_expected_revision < 1 { return Err(denied("agent_task_storage_invariant")); }
        Ok(Some(AgentCliWorkerTaskSnapshot { execution, completion_expected_revision }))
    }

    pub fn start_agent_public_task(
        &self,
        binding: &VerifiedAgentRun,
        task_id: &str,
        idempotency_key: &str,
    ) -> Result<AgentPublicTaskMutation, CommunicationStoreError> {
        let key = key_hash(idempotency_key)?;
        let hash = task_request_hash(&json!({"task_id":task_id,"agent_id":binding.agent_id()}));
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task_id,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment)?;
        let now = now_unix_ms();
        let task = load_owned_task(&tx, &authority.communication_principal, task_id, now)?;
        if task.referenced_project_id.as_deref() != Some(binding.project()) {
            return Err(denied("public_task_not_found"));
        }
        if let Some(execution) = public_execution_on_connection(&tx, task_id)? {
            let (saved_key,saved_hash):(String,String)=tx.query_row("SELECT start_key_hash,start_request_hash FROM wc_agent_public_executions WHERE task_id=?1",[task_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store_error)?;
            if execution.agent_id != binding.agent_id() {
                return Err(denied("public_task_execution_conflict"));
            }
            if saved_key != key || saved_hash != hash {
                return Err(denied("public_task_idempotency_conflict"));
            }
            // Historical replay never reactivates a terminal public Task.
            return Ok(AgentPublicTaskMutation {
                task: task.summary(now),
                execution,
                replayed: true,
                state_changed: false,
            });
        }
        authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task_id,
            AgentAssignmentOperation::TaskStart,
        )
        .map_err(assignment)?;
        require_enrollment_public_task_on_connection(
            &tx,
            binding.owner_user(),
            binding.project(),
            task_id,
        )
        .map_err(assignment)?;
        if task.stored_state != AgentTaskState::Ready {
            return Err(denied("public_task_execution_conflict"));
        }
        let legacy: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM wc_agent_task_attempts WHERE task_id=?1)",
                [task_id],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        if legacy {
            return Err(denied("agent_task_execution_conflict"));
        }
        let execution_id = format!("wc_public_execution_{}", uuid::Uuid::new_v4().simple());
        tx.execute("INSERT INTO wc_agent_public_executions(task_id,execution_id,agent_id,started_run_id,started_generation,state,revision,started_at_unix_ms,start_key_hash,start_request_hash) VALUES(?1,?2,?3,?4,?5,'active',1,?6,?7,?8)",params![task_id,execution_id,binding.agent_id(),binding.run_id(),binding.generation(),now,key,hash]).map_err(store_error)?;
        tx.execute("UPDATE wc_agent_tasks SET state='active',updated_at_unix_ms=MAX(updated_at_unix_ms,?2) WHERE task_id=?1",params![task_id,now]).map_err(store_error)?;
        audit(&tx, binding, "start_public_task")?;
        let task =
            load_owned_task(&tx, &authority.communication_principal, task_id, now)?.summary(now);
        let execution = public_execution_on_connection(&tx, task_id)?
            .ok_or_else(|| denied("public_task_execution_missing"))?;
        tx.commit().map_err(store_error)?;
        Ok(AgentPublicTaskMutation {
            task,
            execution,
            replayed: false,
            state_changed: true,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_agent_public_task(
        &self,
        binding: &VerifiedAgentRun,
        task_id: &str,
        execution_id: &str,
        expected_revision: i64,
        outcome: AgentTaskState,
        terminal_result: Option<&str>,
        terminal_reason: Option<&str>,
        completion_key: &str,
    ) -> Result<AgentPublicTaskMutation, CommunicationStoreError> {
        if !outcome.terminal() || expected_revision < 1 {
            return Err(denied("invalid_public_task_completion"));
        }
        let result = validate_optional_terminal_text(terminal_result, "terminal_result")?;
        let reason = validate_optional_terminal_text(terminal_reason, "terminal_reason")?;
        if result
            .as_deref()
            .into_iter()
            .chain(reason.as_deref())
            .any(webcodex_core::sensitive_text::secret_like_value)
        {
            return Err(denied("public_task_sensitive_terminal_text"));
        }
        let key = key_hash(completion_key)?;
        let hash = task_request_hash(
            &json!({"task_id":task_id,"execution_id":execution_id,"expected_revision":expected_revision,"outcome":outcome,"result":result,"reason":reason}),
        );
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task_id,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment)?;
        // A CLI Worker cannot approve its own deliverable through the generic
        // Task endpoint. Success belongs to the independent delivery reviewer.
        if authority.purpose == crate::AgentEnrollmentPurpose::CliWorker
            && outcome == AgentTaskState::Succeeded
        {
            return Err(denied("public_task_business_review_required"));
        }
        let now = now_unix_ms();
        let task = load_owned_task(&tx, &authority.communication_principal, task_id, now)?;
        if task.referenced_project_id.as_deref() != Some(binding.project()) {
            return Err(denied("public_task_not_found"));
        }
        let execution = public_execution_on_connection(&tx, task_id)?
            .ok_or_else(|| denied("public_task_execution_missing"))?;
        if execution.execution_id != execution_id || execution.agent_id != binding.agent_id() {
            return Err(denied("public_task_execution_conflict"));
        }
        if execution.state.terminal() {
            let (saved_key,saved_hash):(Option<String>,Option<String>)=tx.query_row("SELECT completion_key_hash,completion_request_hash FROM wc_agent_public_executions WHERE task_id=?1",[task_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store_error)?;
            if saved_key.as_deref() != Some(key.as_str())
                || saved_hash.as_deref() != Some(hash.as_str())
            {
                return Err(denied("public_task_idempotency_conflict"));
            }
            return Ok(AgentPublicTaskMutation {
                task: task.summary(now),
                execution,
                replayed: true,
                state_changed: false,
            });
        }
        authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task_id,
            AgentAssignmentOperation::TaskComplete,
        )
        .map_err(assignment)?;
        require_enrollment_public_task_on_connection(
            &tx,
            binding.owner_user(),
            binding.project(),
            task_id,
        )
        .map_err(assignment)?;
        if task.stored_state != AgentTaskState::Active || execution.revision != expected_revision {
            return Err(denied("public_task_revision_conflict"));
        }
        require_public_task_quiescent_on_connection(
            &tx,
            binding.owner_user(),
            binding.project(),
            task_id,
            Some(binding),
        )?;
        if outcome == AgentTaskState::Succeeded {
            crate::team_completion::require_team_completion(
                &tx,
                &authority.communication_principal,
                task_id,
            )?;
        }
        crate::team_decision::freeze_completion(&tx, task_id, outcome == AgentTaskState::Succeeded)?;
        tx.execute("UPDATE wc_agent_public_executions SET state=?2,revision=revision+1,terminal_at_unix_ms=?3,completed_run_id=?4,completed_generation=?5,terminal_result=?6,terminal_reason=?7,completion_key_hash=?8,completion_request_hash=?9 WHERE task_id=?1",params![task_id,outcome.as_str(),now,binding.run_id(),binding.generation(),result,reason,key,hash]).map_err(store_error)?;
        // The canonical Task's terminal marker is intentionally not a legacy
        // attempt: latest_attempt_id remains NULL, so no legacy lookup occurs.
        tx.execute("UPDATE wc_agent_tasks SET state=?2,terminal_attempt_id=?3,terminal_at_unix_ms=?4,updated_at_unix_ms=MAX(updated_at_unix_ms,?4) WHERE task_id=?1",params![task_id,outcome.as_str(),execution_id,now]).map_err(store_error)?;
        audit(&tx, binding, "complete_public_task")?;
        let task =
            load_owned_task(&tx, &authority.communication_principal, task_id, now)?.summary(now);
        let execution = public_execution_on_connection(&tx, task_id)?
            .ok_or_else(|| denied("public_task_execution_missing"))?;
        tx.commit().map_err(store_error)?;
        Ok(AgentPublicTaskMutation {
            task,
            execution,
            replayed: false,
            state_changed: true,
        })
    }

    /// A retained observer can reconcile failure after revocation. It cannot
    /// approve business success; that requires the current delivery reviewer.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_agent_cli_worker_task(
        &self,
        permit: &crate::AgentCliObservationPermit,
        expected_revision: i64,
        outcome: AgentTaskState,
        terminal_result: Option<&str>,
        terminal_reason: Option<&str>,
        completion_key: &str,
    ) -> Result<AgentPublicTaskMutation, CommunicationStoreError> {
        if outcome == AgentTaskState::Succeeded {
            return Err(denied("public_task_business_review_required"));
        }
        if outcome != AgentTaskState::Failed {
            return Err(denied("invalid_public_task_completion"));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let binding = crate::team_task::validate_agent_cli_observation_on_connection(&tx, permit)?;
        let result = complete_cli_worker_task_on_connection(
            &tx, &binding, expected_revision, outcome, terminal_result, terminal_reason, completion_key,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(result)
    }

    pub fn read_agent_public_task_execution(
        &self,
        binding: &VerifiedAgentRun,
        task_id: &str,
    ) -> Result<Option<AgentPublicTaskExecution>, CommunicationStoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let authority = authorize_agent_assignment_on_connection(
            &tx,
            binding,
            task_id,
            AgentAssignmentOperation::Read,
        )
        .map_err(assignment)?;
        let task = load_owned_task(
            &tx,
            &authority.communication_principal,
            task_id,
            now_unix_ms(),
        )?;
        if task.referenced_project_id.as_deref() != Some(binding.project()) {
            return Err(denied("public_task_not_found"));
        }
        public_execution_on_connection(&tx, task_id)
    }
}
