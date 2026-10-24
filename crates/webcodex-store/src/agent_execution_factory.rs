//! Atomic construction around the canonical public Task and original CLI slot.
//! Opaque controller values are not model input and cannot be deserialized.
use super::*;
#[path = "agent_execution_resume.rs"]
mod resume;
#[path = "agent_execution_read.rs"]
mod read;
pub use read::{AgentCliPlannedSlot, AgentCliPreparationPhase, AgentCliPreparationSnapshot};
use crate::agent_isolation::{
    activate_cli_worker_on_connection, authorize_agent_assignment_on_connection,
    enrollment_projection_on_connection, ensure_agent_run_execution_open_on_connection,
    reserve_agent_enrollment_on_connection, validate_agent_execution_admission_on_connection,
    verify_agent_run_context_on_connection,
};
use crate::{
    AgentAssignmentOperation, AgentEnrollmentInput, AgentEnrollmentPolicy, AgentEnrollmentPurpose,
    NewAgentEnrollment, VerifiedAgentRun,
};
use webcodex_core::runner_protocol::{
    AgentExecutionBinding, AgentExecutionCarrier, AgentExecutionExecutor, AgentExecutionRequester,
    AgentExecutionStart, AgentExecutionTarget, AgentRunRef,
};

fn auth_error(e: crate::AgentIsolationError) -> CommunicationStoreError {
    error(e.code(), "Agent execution authority is unavailable")
}
fn denied() -> CommunicationStoreError {
    error(
        "agent_execution_factory_denied",
        "Agent execution authority is unavailable",
    )
}
fn run_ref(run: &VerifiedAgentRun) -> AgentRunRef {
    AgentRunRef {
        agent_id: run.agent_id().into(),
        agent_run_id: run.run_id().into(),
        generation: run.generation(),
    }
}
fn derived_key(domain: &str, source: &str, key: &str) -> Result<String> {
    validate_idempotency_key(key)?;
    Ok(digest_text(
        domain,
        &format!("{}:{source}{key}", source.len()),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMemberRequest {
    pub title: String,
    pub brief: String,
    pub task_directory: String,
    pub inputs: Vec<AgentEnrollmentInput>,
    pub room_ids: Vec<String>,
    pub policy: AgentEnrollmentPolicy,
}
#[derive(Debug, Clone, Serialize)]
pub struct AgentMemberReceipt {
    pub task_id: String,
    pub enrollment_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub replayed: bool,
}

#[derive(Debug, Clone)]
pub struct AgentCliPreparationRequest {
    pub root_task_id: String,
    pub source_task_id: String,
    pub call_index: u32,
    pub idempotency_key: String,
    pub fallback_thinking_profile_id: String,
    pub timeout_secs: Option<u64>,
    pub target: AgentExecutionTarget,
    pub carrier: AgentExecutionCarrier,
}

pub struct AgentCliPreparation {
    identity: AgentExecutionBinding,
    worker: VerifiedAgentRun,
}
impl std::fmt::Debug for AgentCliPreparation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCliPreparation")
            .field("attempt_id", &self.identity.requester.cli_attempt_id)
            .finish_non_exhaustive()
    }
}
impl AgentCliPreparation {
    pub fn binding_identity(&self) -> &AgentExecutionBinding {
        &self.identity
    }
    pub fn worker(&self) -> &VerifiedAgentRun {
        &self.worker
    }
}
#[derive(Debug)]
pub struct AgentCliPreparationResult {
    pub receipt: super::model::AgentTeamReceipt,
    pub phase: String,
    pub preparation: Option<AgentCliPreparation>,
}

pub struct AgentCliDispatchPermit {
    start: AgentExecutionStart,
}

/// Retained only by a trusted observer. It authorizes reconciliation of this
/// exact immutable attempt after revocation, never another dispatch.
#[derive(Clone)]
pub struct AgentCliObservationPermit {
    binding: AgentExecutionBinding,
}
impl std::fmt::Debug for AgentCliObservationPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCliObservationPermit")
            .field("attempt_id", &self.binding.requester.cli_attempt_id)
            .finish_non_exhaustive()
    }
}
impl AgentCliObservationPermit {
    pub fn binding(&self) -> &AgentExecutionBinding {
        &self.binding
    }
}

pub(crate) fn validate_agent_cli_observation_on_connection(
    conn: &Connection,
    permit: &AgentCliObservationPermit,
) -> Result<AgentExecutionBinding> {
    let b = &permit.binding;
    let raw:Option<String>=conn.query_row("SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1 AND project_id=?2 AND root_task_id=?3 AND source_task_id=?4 AND call_index=?5",params![b.requester.cli_attempt_id,b.project,b.requester.root_task_id,b.requester.source_task_id,b.requester.call_index],|r|r.get(0)).optional().map_err(store_error)?;
    let record: TeamCliAttemptRecord = decode(&raw.ok_or_else(denied)?)?;
    if record.execution_binding.as_ref() != Some(b) {
        return Err(denied());
    }
    Ok(b.clone())
}
impl std::fmt::Debug for AgentCliDispatchPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCliDispatchPermit")
            .field("attempt_id", &self.start.binding.requester.cli_attempt_id)
            .finish_non_exhaustive()
    }
}
impl AgentCliDispatchPermit {
    pub fn observation_permit(&self) -> AgentCliObservationPermit {
        AgentCliObservationPermit {
            binding: self.start.binding.clone(),
        }
    }
    pub fn binding(&self) -> &AgentExecutionBinding {
        &self.start.binding
    }
    pub fn into_start(self) -> AgentExecutionStart {
        self.start
    }
}

fn receipt(a: &TeamCliAttemptRecord) -> super::model::AgentTeamReceipt {
    super::model::AgentTeamReceipt {
        attempt_id: a.attempt_id.clone(),
        project_id: a.project_id.clone(),
        root_task_id: a.root_task_id.clone(),
        source_task_id: a.source_task_id.clone(),
        call_index: a.call_index,
        provider_id: a.provider_id.clone(),
        model: a.model.clone(),
        effort: a.effort.clone(),
        fallback_thinking_profile_id: a.fallback_thinking_profile_id.clone(),
        timeout_secs: a.timeout_secs,
        dispatch_state: a.dispatch_state,
        job_id: a.job_id.clone(),
        execution_state: a.execution_state.clone(),
        error_code: a.error_code.clone(),
        observation_revision: a.observation_revision,
        created_at_unix_ms: a.created_at_unix_ms,
        updated_at_unix_ms: a.updated_at_unix_ms,
    }
}

fn source_authority(
    conn: &Connection,
    requester: &VerifiedAgentRun,
    root: &str,
    source: &str,
    write: bool,
) -> Result<crate::AgentEnrollmentAuthority> {
    let auth = authorize_agent_assignment_on_connection(
        conn,
        requester,
        source,
        if write {
            AgentAssignmentOperation::UseCli
        } else {
            AgentAssignmentOperation::Read
        },
    )
    .map_err(auth_error)?;
    let assigned = enrollment_projection_on_connection(conn, requester).map_err(auth_error)?;
    let allowed=match auth.purpose {
        AgentEnrollmentPurpose::Manager=>root==source,
        AgentEnrollmentPurpose::Member=>conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments child JOIN wc_agent_enrollments parent ON parent.agent_id=child.source_agent_id WHERE child.enrollment_id=?1 AND parent.task_id=?2 AND parent.purpose='manager' AND parent.owner_user=child.owner_user AND parent.project=child.project)",params![assigned.enrollment_id,root],|r|r.get::<_,bool>(0)).map_err(store_error)?,
        _=>false,
    };
    if !allowed {
        return Err(denied());
    }
    Ok(auth)
}

impl Database {
    pub fn agent_cli_observation_permit(
        &self,
        current: &VerifiedAgentRun,
        attempt_id: &str,
    ) -> Result<AgentCliObservationPermit> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let own = enrollment_projection_on_connection(&tx, current).map_err(auth_error)?;
        let principal =
            crate::agent_isolation::enrollment_principal_for_execution(current.owner_user());
        let attempt = attempt_on(&tx, &principal, attempt_id)?;
        let b = attempt.execution_binding.as_ref().ok_or_else(denied)?;
        if attempt.project_id != current.project()
            || !(own.task_id == attempt.source_task_id
                || (own.purpose == AgentEnrollmentPurpose::Manager
                    && own.task_id == attempt.root_task_id)
                || b.executor.run.agent_id == current.agent_id())
        {
            return Err(denied());
        }
        Ok(AgentCliObservationPermit { binding: b.clone() })
    }

    pub fn bind_agent_cli_execution_job(
        &self,
        permit: &AgentCliObservationPermit,
        target: &AgentExecutionTarget,
    ) -> Result<TeamCliAttemptRecord> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let b = validate_agent_cli_observation_on_connection(&tx, permit)?;
        if &b.target != target {
            return Err(denied());
        }
        let AgentExecutionCarrier::Job { job_id, .. } = &b.carrier else {
            return Err(denied());
        };
        let (kind,digest):(String,String)=tx.query_row("SELECT owner_principal_kind,owner_principal_digest FROM wc_team_cli_attempts WHERE attempt_id=?1",[&b.requester.cli_attempt_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store_error)?;
        let principal = CommunicationPrincipal { kind, digest };
        let mut attempt = attempt_on(&tx, &principal, &b.requester.cli_attempt_id)?;
        if let Some(existing) = &attempt.job_id {
            if existing != job_id {
                return Err(denied());
            }
            return Ok(attempt);
        }
        if !matches!(
            attempt.dispatch_state,
            TeamCliDispatchState::Dispatching | TeamCliDispatchState::Unknown
        ) {
            return Err(denied());
        }
        attempt.job_id = Some(job_id.clone());
        attempt.dispatch_state = TeamCliDispatchState::Bound;
        attempt.execution_state = None;
        update_attempt(&tx, &principal, &mut attempt)?;
        tx.commit().map_err(store_error)?;
        Ok(attempt)
    }

    pub fn observe_agent_cli_execution(
        &self,
        permit: &AgentCliObservationPermit,
        target: &AgentExecutionTarget,
        expected_revision: i64,
        observation: TeamCliObservation,
    ) -> Result<TeamCliAttemptRecord> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let b = validate_agent_cli_observation_on_connection(&tx, permit)?;
        if &b.target != target {
            return Err(denied());
        }
        let (kind,digest):(String,String)=tx.query_row("SELECT owner_principal_kind,owner_principal_digest FROM wc_team_cli_attempts WHERE attempt_id=?1",[&b.requester.cli_attempt_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store_error)?;
        let record = reconcile_team_cli_attempt_on_connection(
            &tx,
            &CommunicationPrincipal { kind, digest },
            &b.requester.cli_attempt_id,
            expected_revision,
            observation,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(record)
    }
    pub fn create_agent_member(
        &self,
        source: &VerifiedAgentRun,
        request: AgentMemberRequest,
        idempotency_key: &str,
    ) -> Result<AgentMemberReceipt> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let parent = enrollment_projection_on_connection(&tx, source).map_err(auth_error)?;
        let auth = authorize_agent_assignment_on_connection(
            &tx,
            source,
            &parent.task_id,
            AgentAssignmentOperation::Delegate,
        )
        .map_err(auth_error)?;
        ensure_agent_run_execution_open_on_connection(&tx, &run_ref(source)).map_err(auth_error)?;
        for room in &request.room_ids {
            if !parent.room_ids.contains(room) {
                return Err(denied());
            }
        }
        let task_key = derived_key(
            "webcodex.agent.member.task.v1",
            source.agent_id(),
            idempotency_key,
        )?;
        let task = crate::agent_task::create_agent_task_on_connection(
            &tx,
            &auth.communication_principal,
            crate::NewAgentTask {
                title: request.title,
                instruction: request.brief.clone(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(source.project().into()),
                idempotency_key: task_key.clone(),
            },
            now_unix_ms(),
        )?;
        let principal_key: String = tx
            .query_row(
                "SELECT principal_key FROM wc_agent_enrollments WHERE enrollment_id=?1",
                [parent.enrollment_id],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        let enrolled = reserve_agent_enrollment_on_connection(
            &tx,
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::Member,
                owner_user: source.owner_user().into(),
                project: source.project().into(),
                principal_key,
                task_id: task.task.summary.task_id.clone(),
                task_directory: request.task_directory,
                brief: request.brief,
                inputs: request.inputs,
                room_ids: request.room_ids,
                policy: request.policy,
                expires_at_unix_ms: 0,
            },
            Some(source),
            &task_key,
            now_unix_ms(),
        )
        .map_err(auth_error)?;
        let result = AgentMemberReceipt {
            task_id: task.task.summary.task_id,
            enrollment_id: enrolled.enrollment_id,
            agent_id: enrolled.agent_id,
            run_id: enrolled.run_id,
            replayed: enrolled.replayed,
        };
        tx.commit().map_err(store_error)?;
        Ok(result)
    }

    pub fn prepare_agent_cli_worker(
        &self,
        requester: &VerifiedAgentRun,
        request: AgentCliPreparationRequest,
    ) -> Result<AgentCliPreparationResult> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let auth = source_authority(
            &tx,
            requester,
            &request.root_task_id,
            &request.source_task_id,
            true,
        )?;
        ensure_agent_run_execution_open_on_connection(&tx, &run_ref(requester))
            .map_err(auth_error)?;
        if request.call_index >= auth.policy.max_cli_calls
            || !matches!(request.carrier, AgentExecutionCarrier::Job { .. })
        {
            return Err(denied());
        }
        let reservation = reserve_team_cli_attempt_on_connection(
            &tx,
            &auth.communication_principal,
            requester.project(),
            &request.root_task_id,
            &request.source_task_id,
            request.call_index,
            &request.idempotency_key,
            &request.fallback_thinking_profile_id,
            request.timeout_secs,
        )?;
        let attempt = reservation.attempt;
        let old:Option<(String,String)>=tx.query_row("SELECT execution_preparation_json,execution_preparation_state FROM wc_team_cli_attempts WHERE attempt_id=?1 AND execution_preparation_json IS NOT NULL",[&attempt.attempt_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?;
        if let Some((json, phase)) = old {
            let identity: AgentExecutionBinding = decode(&json)?;
            if identity.target != request.target || identity.carrier != request.carrier {
                return Err(error(
                    "agent_cli_preparation_conflict",
                    "CLI preparation is immutable",
                ));
            }
            let preparation = if phase == "prepared"
                && identity.requester.run == run_ref(requester)
                && attempt.dispatch_state == TeamCliDispatchState::Reserved
            {
                ensure_agent_run_execution_open_on_connection(&tx, &identity.executor.run)
                    .map_err(auth_error)?;
                let worker = verify_agent_run_context_on_connection(
                    &tx,
                    &crate::agent_isolation::host_goal_owner_key(
                        requester.project(),
                        &identity.executor.run.agent_id,
                    ),
                    &identity.executor.run.agent_run_id,
                )
                .map_err(auth_error)?;
                Some(AgentCliPreparation { identity, worker })
            } else {
                None
            };
            let result = AgentCliPreparationResult {
                receipt: receipt(&attempt),
                phase,
                preparation,
            };
            tx.commit().map_err(store_error)?;
            return Ok(result);
        }
        if !reservation.created || attempt.dispatch_state != TeamCliDispatchState::Reserved {
            return Err(denied());
        }
        let parent = enrollment_projection_on_connection(&tx, requester).map_err(auth_error)?;
        let principal_key: String = tx
            .query_row(
                "SELECT principal_key FROM wc_agent_enrollments WHERE enrollment_id=?1",
                [&parent.enrollment_id],
                |r| r.get(0),
            )
            .map_err(store_error)?;
        let worker_key = derived_key(
            "webcodex.agent.cli-worker.task.v1",
            requester.agent_id(),
            &attempt.attempt_id,
        )?;
        let worker_task = crate::agent_task::create_agent_task_on_connection(
            &tx,
            &auth.communication_principal,
            crate::NewAgentTask {
                title: "CLI execution".into(),
                instruction: attempt.instruction.clone(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(requester.project().into()),
                idempotency_key: worker_key.clone(),
            },
            now_unix_ms(),
        )?;
        let enrolled = reserve_agent_enrollment_on_connection(
            &tx,
            NewAgentEnrollment {
                purpose: AgentEnrollmentPurpose::CliWorker,
                owner_user: requester.owner_user().into(),
                project: requester.project().into(),
                principal_key,
                task_id: worker_task.task.summary.task_id.clone(),
                task_directory: format!("workspace/cli-{}", uuid::Uuid::new_v4().simple()),
                brief: attempt.instruction.clone(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: 0,
                    max_cli_calls: 0,
                },
                expires_at_unix_ms: 0,
            },
            Some(requester),
            &worker_key,
            now_unix_ms(),
        )
        .map_err(auth_error)?;
        let worker = activate_cli_worker_on_connection(&tx, &enrolled.enrollment_id, now_unix_ms())
            .map_err(auth_error)?;
        ensure_agent_run_execution_open_on_connection(&tx, &run_ref(&worker))
            .map_err(auth_error)?;
        let identity = AgentExecutionBinding {
            version: 1,
            project: requester.project().into(),
            requester: AgentExecutionRequester {
                enrollment_id: parent.enrollment_id,
                run: run_ref(requester),
                root_task_id: request.root_task_id,
                source_task_id: request.source_task_id,
                call_index: request.call_index,
                cli_attempt_id: attempt.attempt_id.clone(),
            },
            executor: AgentExecutionExecutor {
                enrollment_id: enrolled.enrollment_id,
                run: run_ref(&worker),
                task_id: worker_task.task.summary.task_id,
                cli_session_scope_id: format!("wc_cli_scope_{}", uuid::Uuid::new_v4().simple()),
            },
            target: request.target,
            carrier: request.carrier,
            launch_intent_hash: String::new(),
            resource_manifest_hash: String::new(),
        };
        identity.identity_hash().map_err(|_| denied())?;
        tx.execute("UPDATE wc_team_cli_attempts SET execution_preparation_json=?2,execution_preparation_state='prepared' WHERE attempt_id=?1 AND execution_preparation_json IS NULL",params![attempt.attempt_id,encode(&identity)?]).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(AgentCliPreparationResult {
            receipt: receipt(&attempt),
            phase: "prepared".into(),
            preparation: Some(AgentCliPreparation { identity, worker }),
        })
    }

    pub fn finalize_agent_cli_execution(
        &self,
        requester: &VerifiedAgentRun,
        preparation: AgentCliPreparation,
        start: AgentExecutionStart,
    ) -> Result<AgentCliDispatchPermit> {
        start.validate().map_err(|_| denied())?;
        if start.binding.identity_hash().map_err(|_| denied())?
            != preparation.identity.identity_hash().map_err(|_| denied())?
            || start.binding.requester.run != run_ref(requester)
            || start.binding.executor.run != run_ref(&preparation.worker)
        {
            return Err(denied());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let b = &start.binding;
        let auth = source_authority(
            &tx,
            requester,
            &b.requester.root_task_id,
            &b.requester.source_task_id,
            true,
        )?;
        ensure_agent_run_execution_open_on_connection(&tx, &b.requester.run).map_err(auth_error)?;
        ensure_agent_run_execution_open_on_connection(&tx, &b.executor.run).map_err(auth_error)?;
        let (stored,phase):(String,String)=tx.query_row("SELECT execution_preparation_json,execution_preparation_state FROM wc_team_cli_attempts WHERE attempt_id=?1",[&b.requester.cli_attempt_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store_error)?;
        let frozen: AgentExecutionBinding = decode(&stored)?;
        if phase != "prepared" || frozen != preparation.identity {
            return Err(error(
                "agent_cli_dispatch_already_claimed",
                "No second dispatch permission is available",
            ));
        }
        let mut attempt = attempt_on(
            &tx,
            &auth.communication_principal,
            &b.requester.cli_attempt_id,
        )?;
        if attempt.dispatch_state != TeamCliDispatchState::Reserved
            || attempt.execution_binding.is_some()
            || attempt
                .timeout_secs
                .is_some_and(|timeout| start.timeout_secs != timeout)
        {
            return Err(denied());
        }
        load_team_cli_attempt_for_fallback(
            &tx,
            &auth.communication_principal,
            &attempt.attempt_id,
        )?;
        attempt.execution_binding = Some(b.clone());
        attempt.dispatch_state = TeamCliDispatchState::Dispatching;
        update_attempt(&tx, &auth.communication_principal, &mut attempt)?;
        tx.execute("UPDATE wc_team_cli_attempts SET execution_preparation_state='finalized',execution_start_json=?2 WHERE attempt_id=?1 AND execution_preparation_state='prepared'",params![attempt.attempt_id,encode(&start)?]).map_err(store_error)?;
        validate_agent_execution_admission_on_connection(&tx, b).map_err(auth_error)?;
        tx.commit().map_err(store_error)?;
        Ok(AgentCliDispatchPermit { start })
    }

    pub fn read_agent_cli_attempt(
        &self,
        binding: &VerifiedAgentRun,
        attempt_id: &str,
    ) -> Result<super::model::AgentTeamReceipt> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let own = enrollment_projection_on_connection(&tx, binding).map_err(auth_error)?;
        let principal =
            crate::agent_isolation::enrollment_principal_for_execution(binding.owner_user());
        let attempt = attempt_on(&tx, &principal, attempt_id)?;
        let allowed = own.task_id == attempt.source_task_id
            || (own.purpose == AgentEnrollmentPurpose::Manager
                && own.task_id == attempt.root_task_id)
            || attempt
                .execution_binding
                .as_ref()
                .is_some_and(|b| b.executor.run.agent_id == binding.agent_id());
        if !allowed || attempt.project_id != binding.project() {
            return Err(denied());
        }
        Ok(receipt(&attempt))
    }
}

pub(super) fn ensure_factory_schema(conn: &Connection) -> anyhow::Result<()> {
    for (name, definition) in [
        ("execution_preparation_json", "TEXT"),
        ("execution_preparation_state", "TEXT"),
        ("execution_start_json", "TEXT"),
    ] {
        let mut stmt = conn.prepare("PRAGMA table_info(wc_team_cli_attempts)")?;
        let found = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|n| n == name);
        if !found {
            conn.execute_batch(&format!(
                "ALTER TABLE wc_team_cli_attempts ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    conn.execute_batch("CREATE TRIGGER IF NOT EXISTS wc_cli_preparation_immutable BEFORE UPDATE OF execution_preparation_json ON wc_team_cli_attempts WHEN OLD.execution_preparation_json IS NOT NULL AND NEW.execution_preparation_json IS NOT OLD.execution_preparation_json BEGIN SELECT RAISE(ABORT,'CLI preparation is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS wc_cli_start_immutable BEFORE UPDATE OF execution_start_json ON wc_team_cli_attempts WHEN OLD.execution_start_json IS NOT NULL AND NEW.execution_start_json IS NOT OLD.execution_start_json BEGIN SELECT RAISE(ABORT,'CLI launch is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS wc_cli_execution_binding_immutable BEFORE UPDATE OF record_json ON wc_team_cli_attempts WHEN json_extract(OLD.record_json,'$.execution_binding') IS NOT NULL AND json_extract(NEW.record_json,'$.execution_binding') IS NOT json_extract(OLD.record_json,'$.execution_binding') BEGIN SELECT RAISE(ABORT,'CLI execution binding is immutable'); END;")?;
    resume::ensure_resume_schema(conn)?;
    Ok(())
}

#[cfg(test)]
#[path = "agent_execution_factory_tests.rs"]
mod tests;
