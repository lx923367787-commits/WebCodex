//! Atomic bridge from the existing ordinary/fallback queues to independent
//! Agent enrollment. No parallel scheduling queue or plaintext delivery store.
use crate::agent_isolation::{
    agent_enrollment_source_principal_on_connection, authorize_agent_assignment_on_connection,
    enrollment_principal_for_execution, issue_delivery_grant_on_connection,
    reserve_agent_enrollment_on_connection,
    reset_unstarted_agent_enrollment_delivery_on_connection,
    validate_agent_enrollment_dispatch_on_connection, validate_agent_run_on_connection,
};
use crate::{
    AgentAssignmentOperation, AgentEnrollmentGrant, AgentEnrollmentInput, AgentEnrollmentPolicy,
    AgentEnrollmentPurpose, AgentIsolationError, CommunicationPrincipal, Database,
    NewAgentEnrollment, NewAgentTask, VerifiedAgentRun,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use webcodex_core::runner_protocol::{AgentExecutionBinding, AgentExecutionCarrier};

type Result<T> = std::result::Result<T, AgentIsolationError>;
const MAX_REFERENCES: usize = 4096;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AgentHostQueueKind {
    Member,
    Fallback,
}
impl AgentHostQueueKind {
    fn text(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Fallback => "fallback",
        }
    }
    fn purpose(self) -> AgentEnrollmentPurpose {
        match self {
            Self::Member => AgentEnrollmentPurpose::Member,
            Self::Fallback => AgentEnrollmentPurpose::Fallback,
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "member" => Ok(Self::Member),
            "fallback" => Ok(Self::Fallback),
            _ => Err(denied()),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentHostQueueBinding {
    pub queue_kind: AgentHostQueueKind,
    pub queue_id: String,
    pub enrollment_id: String,
    pub agent_id: String,
    pub task_id: String,
    pub root_task_id: String,
    pub manager_agent_id: String,
    pub project: String,
    pub thinking_profile_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentHostDispatchStatus {
    pub binding: AgentHostQueueBinding,
    pub queue_state: String,
    pub dispatch_attempt_id: Option<String>,
    pub dispatch_count: u32,
    pub delivery_state: String,
    pub request_id: Option<String>,
    pub response_observed: bool,
    pub target_task_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentCliFallbackReservation {
    pub fallback_id: String,
    pub project: String,
    pub root_task_id: String,
    pub source_task_id: String,
    pub source_attempt_id: String,
    pub state: crate::CliFallbackState,
    pub replayed: bool,
}

/// Bootstrap Host control only: the original frozen PUBLIC task instruction.
/// No private Goal/Memory/log reads and no delivery capability are included.
#[derive(Debug, Clone, Serialize)]
pub struct AgentHostLaunchMaterial {
    pub status: AgentHostDispatchStatus,
    pub instruction: String,
    pub source_task_id: String,
    pub cancel_requested: bool,
}

/// Only the trusted exact-window launch adapter may consume this grant. It is
/// deliberately neither Debug nor Serialize and is never a normal receipt.
pub struct AgentHostDispatchClaim {
    pub status: AgentHostDispatchStatus,
    pub dispatch_permitted: bool,
    pub grant: Option<AgentEnrollmentGrant>,
    pub grant_expires_at_unix_ms: Option<i64>,
    launch_instruction: String,
    launch_source_task_id: String,
    launch_cancel_requested: bool,
}
impl AgentHostDispatchClaim {
    pub fn launch_material(&self) -> AgentHostLaunchMaterial {
        AgentHostLaunchMaterial {
            status: self.status.clone(),
            instruction: self.launch_instruction.clone(),
            source_task_id: self.launch_source_task_id.clone(),
            cancel_requested: self.launch_cancel_requested,
        }
    }
}

struct BindingRow {
    binding: AgentHostQueueBinding,
    owner_user: String,
    source_task_id: String,
    delivery_attempt_id: Option<String>,
    no_start_attempt_id: Option<String>,
    no_start_code: Option<String>,
    terminal_request_id: Option<String>,
    terminal_receipt_hash: Option<String>,
}

struct QueueView {
    instruction: String,
    root: String,
    source: String,
    project: String,
    profile: String,
    state: String,
    dispatch: Option<String>,
    count: u32,
    request: Option<String>,
    observed: bool,
    cancelled: bool,
    evidence: Option<String>,
}

fn denied() -> AgentIsolationError {
    AgentIsolationError::new("agent_host_dispatch_denied")
}
fn unavailable(_: impl std::fmt::Display) -> AgentIsolationError {
    AgentIsolationError::new("agent_host_dispatch_unavailable")
}
fn queue_error(error: anyhow::Error) -> AgentIsolationError {
    if let Some(e) = error.downcast_ref::<AgentIsolationError>() {
        return *e;
    }
    if let Some(e) = error.downcast_ref::<crate::CommunicationStoreError>() {
        return AgentIsolationError::new(e.code());
    }
    let code = match error.to_string().as_str() {
        "cli_fallback_source_job_mismatch" => "cli_fallback_source_job_mismatch",
        "cli_fallback_source_not_eligible" => "cli_fallback_source_not_eligible",
        "cli_fallback_profile_required" => "cli_fallback_profile_required",
        "cli_fallback_unavailable" => "cli_fallback_unavailable",
        "cli_fallback_dispatch_limit" => "cli_fallback_dispatch_limit",
        _ => "agent_host_dispatch_unavailable",
    };
    AgentIsolationError::new(code)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn digest(value: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"webcodex.agent-host-dispatch.v1\0");
    h.update(value.as_bytes());
    format!("{:x}", h.finalize())
}
fn no_start_code(code: &str) -> bool {
    code == "browser_worker_busy:launched=false"
        || crate::team_agent::profile_unavailable_evidence(code)
}

pub(crate) fn init_agent_host_dispatch_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_agent_enrollment_renewals(
        renewal_id TEXT PRIMARY KEY,enrollment_id TEXT NOT NULL,original_request_id TEXT NOT NULL,
        key_hash TEXT NOT NULL,previous_expiry INTEGER NOT NULL,expires_at INTEGER NOT NULL,
        source_run_id TEXT NOT NULL,source_generation INTEGER NOT NULL,created_at INTEGER NOT NULL,
        UNIQUE(enrollment_id,key_hash));
        CREATE TRIGGER IF NOT EXISTS wc_enrollment_renewal_immutable BEFORE UPDATE ON wc_agent_enrollment_renewals BEGIN SELECT RAISE(ABORT,'renewal history is immutable'); END;
        CREATE TRIGGER IF NOT EXISTS wc_enrollment_renewal_preserved BEFORE DELETE ON wc_agent_enrollment_renewals BEGIN SELECT RAISE(ABORT,'renewal history is preserved'); END;")?;

    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_agent_host_dispatch_bindings (
      queue_kind TEXT NOT NULL CHECK(queue_kind IN ('member','fallback')), queue_id TEXT NOT NULL,
      owner_user TEXT NOT NULL, project TEXT NOT NULL, root_task_id TEXT NOT NULL, source_task_id TEXT NOT NULL,
      task_id TEXT NOT NULL REFERENCES wc_agent_tasks(task_id), manager_agent_id TEXT NOT NULL,
      enrollment_id TEXT NOT NULL UNIQUE REFERENCES wc_agent_enrollments(enrollment_id),
      agent_id TEXT NOT NULL UNIQUE, thinking_profile_id TEXT NOT NULL,
      delivery_attempt_id TEXT, no_start_attempt_id TEXT, no_start_code TEXT,
      terminal_request_id TEXT, terminal_receipt_hash TEXT, created_at_unix_ms INTEGER NOT NULL,
      PRIMARY KEY(queue_kind,queue_id));
      CREATE INDEX IF NOT EXISTS idx_agent_host_dispatch_tasks
      ON wc_agent_host_dispatch_bindings(owner_user,project,root_task_id,source_task_id,task_id);")?;
    Ok(())
}

fn binding(conn: &Connection, kind: AgentHostQueueKind, id: &str) -> Result<Option<BindingRow>> {
    conn.query_row("SELECT owner_user,project,root_task_id,source_task_id,task_id,manager_agent_id,
      enrollment_id,agent_id,thinking_profile_id,delivery_attempt_id,no_start_attempt_id,no_start_code,
      terminal_request_id,terminal_receipt_hash FROM wc_agent_host_dispatch_bindings WHERE queue_kind=?1 AND queue_id=?2",
      params![kind.text(),id], |r| Ok(BindingRow {
        binding:AgentHostQueueBinding {queue_kind:kind,queue_id:id.into(),project:r.get(1)?,root_task_id:r.get(2)?,
          task_id:r.get(4)?,manager_agent_id:r.get(5)?,enrollment_id:r.get(6)?,agent_id:r.get(7)?,thinking_profile_id:r.get(8)?},
        owner_user:r.get(0)?,source_task_id:r.get(3)?,delivery_attempt_id:r.get(9)?,no_start_attempt_id:r.get(10)?,
        no_start_code:r.get(11)?,terminal_request_id:r.get(12)?,terminal_receipt_hash:r.get(13)?,
      })).optional().map_err(unavailable)
}

fn manager_authority(
    conn: &Connection,
    manager: &VerifiedAgentRun,
    root: &str,
    write: bool,
) -> Result<crate::AgentEnrollmentAuthority> {
    let authority = authorize_agent_assignment_on_connection(
        conn,
        manager,
        root,
        if write {
            AgentAssignmentOperation::PlanManage
        } else {
            AgentAssignmentOperation::Read
        },
    )?;
    if authority.purpose != AgentEnrollmentPurpose::Manager {
        return Err(denied());
    }
    Ok(authority)
}
fn authorize_binding(
    conn: &Connection,
    manager: &VerifiedAgentRun,
    row: &BindingRow,
    write: bool,
) -> Result<CommunicationPrincipal> {
    if row.owner_user != manager.owner_user()
        || row.binding.project != manager.project()
        || row.binding.manager_agent_id != manager.agent_id()
    {
        return Err(denied());
    }
    Ok(manager_authority(conn, manager, &row.binding.root_task_id, write)?.communication_principal)
}

fn queue_view(
    conn: &Connection,
    owner: &CommunicationPrincipal,
    kind: AgentHostQueueKind,
    id: &str,
) -> Result<QueueView> {
    match kind {
        AgentHostQueueKind::Member => {
            let r = crate::team_agent::team_agent_record_on_connection(conn, owner, id)
                .map_err(|e| AgentIsolationError::new(e.code()))?;
            Ok(QueueView {
                instruction: r.instruction,
                root: r.root_task_id,
                source: r.source_task_id,
                project: r.project,
                profile: r.thinking_profile_id,
                state: r.state.as_str().into(),
                dispatch: r.dispatch_attempt_id,
                count: r.dispatch_count,
                request: r.request_id,
                observed: r.response_observed,
                cancelled: r.cancel_requested,
                evidence: r.terminal_evidence,
            })
        }
        AgentHostQueueKind::Fallback => {
            let r = crate::cli_fallback::cli_fallback_record_on_connection(conn, owner, id)
                .map_err(queue_error)?;
            Ok(QueueView {
                instruction: r.instruction,
                root: r.root_task_id,
                source: r.source_task_id,
                project: r.project,
                profile: r.thinking_profile_id,
                state: r.state.as_str().into(),
                dispatch: r.dispatch_attempt_id,
                count: r.dispatch_count,
                request: r.request_id,
                observed: r.response_observed,
                cancelled: r.cancel_requested,
                evidence: r.terminal_evidence,
            })
        }
    }
}
fn matches_queue(row: &BindingRow, queue: &QueueView) -> Result<()> {
    if row.binding.project != queue.project
        || row.binding.root_task_id != queue.root
        || row.source_task_id != queue.source
        || row.binding.thinking_profile_id != queue.profile
    {
        return Err(denied());
    }
    Ok(())
}
fn delivery_state(conn: &Connection, row: &BindingRow) -> Result<String> {
    conn.query_row("SELECT CASE WHEN state='claimed' THEN 'claimed' WHEN state='revoked' THEN 'revoked' ELSE delivery_state END
        FROM wc_agent_enrollments WHERE enrollment_id=?1 AND agent_id=?2 AND task_id=?3 AND owner_user=?4 AND project=?5 AND source_agent_id=?6",
        params![row.binding.enrollment_id,row.binding.agent_id,row.binding.task_id,row.owner_user,row.binding.project,row.binding.manager_agent_id],|r|r.get(0))
        .optional().map_err(unavailable)?.ok_or_else(denied)
}
fn task_state(conn: &Connection, row: &BindingRow) -> Result<String> {
    let principal = enrollment_principal_for_execution(&row.owner_user);
    conn.query_row("SELECT state FROM wc_agent_tasks WHERE task_id=?1 AND referenced_project_id=?2 AND owner_principal_kind=?3 AND owner_principal_digest=?4",
        params![row.binding.task_id,row.binding.project,principal.kind,principal.digest],|r|r.get(0))
        .optional().map_err(unavailable)?.ok_or_else(denied)
}
fn status(
    conn: &Connection,
    row: BindingRow,
    owner: &CommunicationPrincipal,
) -> Result<AgentHostDispatchStatus> {
    let q = queue_view(conn, owner, row.binding.queue_kind, &row.binding.queue_id)?;
    matches_queue(&row, &q)?;
    let delivery_state = delivery_state(conn, &row)?;
    let target_task_state = task_state(conn, &row)?;
    Ok(AgentHostDispatchStatus {
        binding: row.binding,
        queue_state: q.state,
        dispatch_attempt_id: q.dispatch,
        dispatch_count: q.count,
        delivery_state,
        request_id: q.request,
        response_observed: q.observed,
        target_task_state,
    })
}
fn insert_binding(
    conn: &Connection,
    manager: &VerifiedAgentRun,
    kind: AgentHostQueueKind,
    id: &str,
    q: &QueueView,
    enrollment: &crate::AgentEnrollmentProjection,
) -> Result<AgentHostQueueBinding> {
    conn.execute("INSERT INTO wc_agent_host_dispatch_bindings(queue_kind,queue_id,owner_user,project,root_task_id,source_task_id,
        task_id,manager_agent_id,enrollment_id,agent_id,thinking_profile_id,created_at_unix_ms)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![kind.text(),id,manager.owner_user(),q.project,q.root,q.source,enrollment.task_id,manager.agent_id(),
            enrollment.enrollment_id,enrollment.agent_id,q.profile,now()]).map_err(unavailable)?;
    Ok(binding(conn, kind, id)?.ok_or_else(denied)?.binding)
}

/// Legacy queue APIs remain available for genuinely legacy tasks. A scoped
/// Agent task must use atomic enrollment delivery, including before binding.
pub(crate) fn require_legacy_host_queue_claim_on_connection(
    conn: &Connection,
    kind: &str,
    id: &str,
    source_task: &str,
) -> Result<()> {
    let bound: bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_host_dispatch_bindings WHERE queue_kind=?1 AND queue_id=?2)",params![kind,id],|r|r.get(0)).map_err(unavailable)?;
    let scoped: bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments WHERE task_id=?1 AND state!='revoked')",[source_task],|r|r.get(0)).map_err(unavailable)?;
    if bound || scoped {
        Err(AgentIsolationError::new(
            "agent_host_queue_binding_required",
        ))
    } else {
        Ok(())
    }
}

/// Invoked only after the ORIGINAL queue observer validated a no-send receipt
/// and wrote its transition in this same transaction. Reset independently joins
/// this fence back to that exact queue record before revoking the old grant.
pub(crate) fn record_host_queue_no_start_on_connection(
    conn: &Connection,
    kind: &str,
    id: &str,
    fence: &str,
    code: &str,
) -> Result<()> {
    let kind = AgentHostQueueKind::parse(kind)?;
    let Some(row) = binding(conn, kind, id)? else {
        return Ok(());
    };
    if !no_start_code(code) || row.delivery_attempt_id.as_deref() != Some(fence) {
        return Err(denied());
    }
    conn.execute(
        "UPDATE wc_agent_host_dispatch_bindings SET no_start_attempt_id=?3,no_start_code=?4
        WHERE queue_kind=?1 AND queue_id=?2",
        params![kind.text(), id, fence, code],
    )
    .map_err(unavailable)?;
    reset_unstarted_agent_enrollment_delivery_on_connection(
        conn,
        &row.binding.enrollment_id,
        fence,
        now(),
    )
}

/// The existing resolution API is trusted adapter/control only. A terminal
/// receipt is recorded only after its original exact dispatch fence succeeds.
/// Business completion additionally requires the bound public Task terminal.
pub(crate) fn record_host_queue_resolution_on_connection(
    conn: &Connection,
    kind: &str,
    id: &str,
    fence: &str,
    terminal: bool,
    proof: &str,
) -> Result<()> {
    let kind = AgentHostQueueKind::parse(kind)?;
    let Some(row) = binding(conn, kind, id)? else {
        return Ok(());
    };
    if row.delivery_attempt_id.as_deref() != Some(fence) {
        return Err(denied());
    }
    if !terminal {
        return record_host_queue_no_start_on_connection(conn, kind.text(), id, fence, proof);
    }
    let owner = enrollment_principal_for_execution(&row.owner_user);
    let q = queue_view(conn, &owner, kind, id)?;
    matches_queue(&row, &q)?;
    let request = q.request.as_deref().ok_or_else(denied)?;
    let parsed = uuid::Uuid::parse_str(request).map_err(unavailable)?;
    if parsed.to_string() != request
        || q.state != "verified_terminal"
        || q.evidence.as_deref() != Some(proof)
    {
        return Err(denied());
    }
    conn.execute(
        "UPDATE wc_agent_host_dispatch_bindings SET terminal_request_id=?3,terminal_receipt_hash=?4
        WHERE queue_kind=?1 AND queue_id=?2",
        params![kind.text(), id, request, digest(proof)],
    )
    .map_err(unavailable)?;
    Ok(())
}

fn source_stop_proven(
    conn: &Connection,
    owner: &CommunicationPrincipal,
    record: &crate::CliFallbackRecord,
) -> Result<()> {
    let source = crate::team_task::load_team_cli_attempt_for_fallback(
        conn,
        owner,
        &record.source_attempt_id,
    )
    .map_err(|e| AgentIsolationError::new(e.code()))?;
    if source.project_id != record.project
        || source.root_task_id != record.root_task_id
        || source.source_task_id != record.source_task_id
        || source.job_id.as_deref() != Some(record.source_job_id.as_str())
        || source.dispatch_state != crate::TeamCliDispatchState::Terminal
    {
        return Err(AgentIsolationError::new(
            "agent_host_fallback_failure_required",
        ));
    }
    let encoded:String=conn.query_row("SELECT record_json FROM wc_team_cli_attempts WHERE attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3",
        params![record.source_attempt_id,owner.kind,owner.digest],|r|r.get(0)).map_err(unavailable)?;
    let value: serde_json::Value = serde_json::from_str(&encoded).map_err(unavailable)?;
    let binding: AgentExecutionBinding = serde_json::from_value(value["execution_binding"].clone())
        .map_err(|_| AgentIsolationError::new("agent_execution_stop_proof_required"))?;
    if binding.project != record.project
        || binding.requester.root_task_id != record.root_task_id
        || binding.requester.source_task_id != record.source_task_id
        || binding.requester.cli_attempt_id != record.source_attempt_id
        || binding.target.client_id != record.client_id
    {
        return Err(denied());
    }
    match &binding.carrier {
        AgentExecutionCarrier::Job {
            job_id,
            start_request_id,
        } if job_id == &record.source_job_id && start_request_id == &record.source_request_id => {}
        _ => {
            return Err(AgentIsolationError::new(
                "agent_host_fallback_job_binding_required",
            ))
        }
    }
    if !crate::agent_execution_close::agent_execution_binding_stop_proven_on_connection(
        conn, &binding,
    )? {
        return Err(AgentIsolationError::new(
            "agent_execution_stop_proof_required",
        ));
    }
    Ok(())
}

/// Called only in the first-claim branch of the existing enrollment transaction,
/// after its principal/secret/source checks and before activating the pending run.
/// Existing manual first-use grants with no queue binding retain their contract.
pub(crate) fn validate_host_enrollment_claim_on_connection(
    conn: &Connection,
    enrollment_id: &str,
) -> Result<()> {
    let link:Option<(String,String)>=conn.query_row("SELECT queue_kind,queue_id FROM wc_agent_host_dispatch_bindings WHERE enrollment_id=?1",
        [enrollment_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(unavailable)?;
    let Some((kind, id)) = link else {
        return Ok(());
    };
    let kind = AgentHostQueueKind::parse(&kind)?;
    let row = binding(conn, kind, &id)?.ok_or_else(denied)?;
    let owner = enrollment_principal_for_execution(&row.owner_user);
    let q = queue_view(conn, &owner, kind, &id)?;
    matches_queue(&row, &q)?;
    if q.cancelled
        || !matches!(q.state.as_str(), "starting" | "running" | "unknown")
        || q.dispatch.is_none()
        || row.delivery_attempt_id != q.dispatch
    {
        return Err(AgentIsolationError::new(
            "agent_host_enrollment_claim_denied",
        ));
    }
    match kind {
        AgentHostQueueKind::Member => {
            if !crate::team_agent::team_agent_can_deliver_on_connection(conn, &owner, &id)
                .map_err(|e| AgentIsolationError::new(e.code()))?
            {
                return Err(AgentIsolationError::new(
                    "agent_host_enrollment_claim_denied",
                ));
            }
        }
        AgentHostQueueKind::Fallback => {
            let record = crate::cli_fallback::cli_fallback_record_on_connection(conn, &owner, &id)
                .map_err(queue_error)?;
            source_stop_proven(conn, &owner, &record)?;
        }
    }
    Ok(())
}

impl Database {
    /// Trusted retained CLI watcher/F04 reconciliation only. The opaque permit
    /// binds one original attempt even after its requester/worker is revoked.
    /// This creates no Agent or delivery grant; a current Manager must later
    /// bind a RecoveryTask and pass the ordinary live-source admission checks.
    pub fn reserve_agent_cli_fallback(
        &self,
        permit: &crate::AgentCliObservationPermit,
        target: &webcodex_core::runner_protocol::AgentExecutionTarget,
        job: &webcodex_core::runner_protocol::ShellJobInfo,
        reason: webcodex_core::cli_fallback::CliFailureReason,
        cancellation_origin: webcodex_core::cli_fallback::CliCancellationOrigin,
    ) -> Result<AgentCliFallbackReservation> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let frozen = crate::team_task::validate_agent_cli_observation_on_connection(&tx, permit)
            .map_err(|e| AgentIsolationError::new(e.code()))?;
        if &frozen.target != target
            || job.client_id != target.client_id
            || job.project_id.as_deref() != Some(frozen.project.as_str())
        {
            return Err(AgentIsolationError::new("cli_fallback_source_job_mismatch"));
        }
        match &frozen.carrier {
            AgentExecutionCarrier::Job {
                job_id,
                start_request_id,
            } if job_id == &job.job_id
                && job.request_id.as_deref() == Some(start_request_id.as_str()) => {}
            _ => return Err(AgentIsolationError::new("cli_fallback_source_job_mismatch")),
        }
        if !crate::agent_execution_close::agent_execution_binding_stop_proven_on_connection(
            &tx, &frozen,
        )? {
            return Err(AgentIsolationError::new(
                "agent_execution_stop_proof_required",
            ));
        }
        let owner:CommunicationPrincipal=tx.query_row("SELECT owner_principal_kind,owner_principal_digest FROM wc_team_cli_attempts WHERE attempt_id=?1",
            [&frozen.requester.cli_attempt_id],|r|Ok(CommunicationPrincipal{kind:r.get(0)?,digest:r.get(1)?})).map_err(unavailable)?;
        let replayed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_cli_fallbacks WHERE source_attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3)",
            params![frozen.requester.cli_attempt_id,owner.kind,owner.digest],|r|r.get(0)).map_err(unavailable)?;
        let record = crate::cli_fallback::reserve_cli_fallback_on_connection(
            &tx,
            &owner,
            &frozen.requester.cli_attempt_id,
            job,
            reason,
            cancellation_origin,
        )
        .map_err(queue_error)?;
        tx.commit().map_err(unavailable)?;
        Ok(AgentCliFallbackReservation {
            fallback_id: record.fallback_id,
            project: record.project,
            root_task_id: record.root_task_id,
            source_task_id: record.source_task_id,
            source_attempt_id: record.source_attempt_id,
            state: record.state,
            replayed,
        })
    }
    pub fn lookup_agent_host_member(
        &self,
        manager: &VerifiedAgentRun,
        root_task_id: &str,
        child_task_id: &str,
        enrollment_id: &str,
        idempotency_key: &str,
        thinking_profile_id: &str,
    ) -> Result<Option<AgentHostQueueBinding>> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let authority = manager_authority(&tx, manager, root_task_id, false)?;
        let target:Option<(String,String)>=tx.query_row("SELECT state,delivery_state FROM wc_agent_enrollments WHERE enrollment_id=?1 AND owner_user=?2 AND project=?3 AND task_id=?4 AND source_agent_id=?5 AND purpose='member'",
            params![enrollment_id,manager.owner_user(),manager.project(),child_task_id,manager.agent_id()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(unavailable)?;
        let target = target.ok_or_else(denied)?;
        let queued = crate::team_agent::lookup_team_agent_attempt_on_connection(
            &tx,
            &authority.communication_principal,
            manager.project(),
            root_task_id,
            child_task_id,
            idempotency_key,
            thinking_profile_id,
        )
        .map_err(|e| AgentIsolationError::new(e.code()))?;
        if let Some(queued) = queued {
            if let Some(row) = binding(
                &tx,
                AgentHostQueueKind::Member,
                &queued.team_agent_attempt_id,
            )? {
                authorize_binding(&tx, manager, &row, false)?;
                if row.binding.enrollment_id != enrollment_id
                    || row.binding.task_id != child_task_id
                {
                    return Err(denied());
                }
                tx.commit().map_err(unavailable)?;
                return Ok(Some(row.binding));
            }
        } else {
            let occupied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_agent_attempts WHERE root_task_id=?1 AND source_task_id=?2)",params![root_task_id,child_task_id],|r|r.get(0)).map_err(unavailable)?;
            if occupied {
                return Err(AgentIsolationError::new("team_agent_already_reserved"));
            }
        }
        if target.0 != "issued" || target.1 != "reserved" {
            return Err(denied());
        }
        tx.commit().map_err(unavailable)?;
        Ok(None)
    }

    pub fn reserve_agent_host_member(
        &self,
        manager: &VerifiedAgentRun,
        root_task_id: &str,
        child_task_id: &str,
        enrollment_id: &str,
        idempotency_key: &str,
        thinking_profile_id: &str,
    ) -> Result<AgentHostQueueBinding> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let authority = manager_authority(&tx, manager, root_task_id, true)?;
        let target = validate_agent_enrollment_dispatch_on_connection(
            &tx,
            manager,
            enrollment_id,
            child_task_id,
            AgentEnrollmentPurpose::Member,
        )?;
        let reservation = crate::team_agent::reserve_team_agent_attempt_on_connection(
            &tx,
            &authority.communication_principal,
            manager.project(),
            root_task_id,
            child_task_id,
            idempotency_key,
            thinking_profile_id,
        )
        .map_err(|e| AgentIsolationError::new(e.code()))?;
        let id = &reservation.record.team_agent_attempt_id;
        if let Some(existing) = binding(&tx, AgentHostQueueKind::Member, id)? {
            authorize_binding(&tx, manager, &existing, false)?;
            if existing.binding.enrollment_id != enrollment_id {
                return Err(denied());
            }
            return Ok(existing.binding);
        }
        if target.state != "reserved"
            || reservation.record.state != crate::TeamAgentState::Queued
            || reservation.record.dispatch_count != 0
        {
            return Err(denied());
        }
        let q = queue_view(
            &tx,
            &authority.communication_principal,
            AgentHostQueueKind::Member,
            id,
        )?;
        let result = insert_binding(
            &tx,
            manager,
            AgentHostQueueKind::Member,
            id,
            &q,
            &target.projection,
        )?;
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn bind_agent_host_fallback(
        &self,
        manager: &VerifiedAgentRun,
        fallback_id: &str,
    ) -> Result<AgentHostQueueBinding> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        if let Some(existing) = binding(&tx, AgentHostQueueKind::Fallback, fallback_id)? {
            authorize_binding(&tx, manager, &existing, false)?;
            return Ok(existing.binding);
        }
        let root: String = tx
            .query_row(
                "SELECT root_task_id FROM wc_cli_fallbacks WHERE fallback_id=?1",
                [fallback_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable)?
            .ok_or_else(denied)?;
        let authority = manager_authority(&tx, manager, &root, true)?;
        let record = crate::cli_fallback::cli_fallback_record_on_connection(
            &tx,
            &authority.communication_principal,
            fallback_id,
        )
        .map_err(queue_error)?;
        if record.state != crate::CliFallbackState::Queued
            || record.dispatch_count != 0
            || record.cancel_requested
        {
            return Err(denied());
        }
        source_stop_proven(&tx, &authority.communication_principal, &record)?;
        let key = digest(&format!("recovery-task:{fallback_id}"));
        let task = crate::agent_task::create_agent_task_on_connection(
            &tx,
            &authority.communication_principal,
            NewAgentTask {
                title: "Recover the remaining failed CLI work".into(),
                instruction: record.instruction.clone(),
                assignee_agent_id: None,
                source_conversation_id: None,
                source_message_id: None,
                referenced_project_id: Some(record.project.clone()),
                idempotency_key: key.clone(),
            },
            now(),
        )
        .map_err(|e| AgentIsolationError::new(e.code()))?
        .task
        .summary
        .task_id;
        let principal = agent_enrollment_source_principal_on_connection(&tx, manager)?;
        let snapshot = serde_json::json!({"source_task_id":record.source_task_id,"source_attempt_id":record.source_attempt_id,
            "source_job_id":record.source_job_id,"source_request_id":record.source_request_id,
            "failure_reason":record.failure_reason,"cancellation_origin":record.cancellation_origin});
        let issued=reserve_agent_enrollment_on_connection(&tx,NewAgentEnrollment {
            purpose:AgentEnrollmentPurpose::Fallback,owner_user:manager.owner_user().into(),project:record.project.clone(),
            principal_key:principal,task_id:task.clone(),task_directory:format!("workspace/recovery-{}",&key[..24]),
            brief:"Recover only the remaining work using the explicit original task and failure inputs. No caller-private files, CLI calls or delegation are authorized.".into(),
            inputs:vec![AgentEnrollmentInput{name:"source-cli.json".into(),content:snapshot.to_string()},AgentEnrollmentInput{name:"source-task.txt".into(),content:record.instruction.clone()}],room_ids:vec![],
            policy:AgentEnrollmentPolicy{max_subagents:0,max_cli_calls:0},expires_at_unix_ms:0,
        },Some(manager),&digest(&format!("recovery-enrollment:{fallback_id}")),now())?;
        if issued.grant.is_some() {
            return Err(denied());
        }
        let target = validate_agent_enrollment_dispatch_on_connection(
            &tx,
            manager,
            &issued.enrollment_id,
            &task,
            AgentEnrollmentPurpose::Fallback,
        )?;
        if target.state != "reserved" {
            return Err(denied());
        }
        let q = queue_view(
            &tx,
            &authority.communication_principal,
            AgentHostQueueKind::Fallback,
            fallback_id,
        )?;
        let result = insert_binding(
            &tx,
            manager,
            AgentHostQueueKind::Fallback,
            fallback_id,
            &q,
            &target.projection,
        )?;
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn read_agent_host_dispatch(
        &self,
        manager: &VerifiedAgentRun,
        kind: AgentHostQueueKind,
        id: &str,
    ) -> Result<AgentHostDispatchStatus> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let row = binding(&tx, kind, id)?.ok_or_else(denied)?;
        let owner = authorize_binding(&tx, manager, &row, false)?;
        let result = status(&tx, row, &owner)?;
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn read_agent_host_launch_material(
        &self,
        manager: &VerifiedAgentRun,
        kind: AgentHostQueueKind,
        id: &str,
    ) -> Result<AgentHostLaunchMaterial> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let row = binding(&tx, kind, id)?.ok_or_else(denied)?;
        let owner = authorize_binding(&tx, manager, &row, false)?;
        let q = queue_view(&tx, &owner, kind, id)?;
        matches_queue(&row, &q)?;
        let result = AgentHostLaunchMaterial {
            status: status(&tx, row, &owner)?,
            instruction: q.instruction,
            source_task_id: q.source,
            cancel_requested: q.cancelled,
        };
        tx.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn claim_agent_host_dispatch(
        &self,
        manager: &VerifiedAgentRun,
        kind: AgentHostQueueKind,
        id: &str,
    ) -> Result<AgentHostDispatchClaim> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let row = binding(&tx, kind, id)?.ok_or_else(denied)?;
        let owner = authorize_binding(&tx, manager, &row, false)?;
        let q = queue_view(&tx, &owner, kind, id)?;
        matches_queue(&row, &q)?;
        if q.state != "queued" || q.cancelled {
            return Ok(AgentHostDispatchClaim {
                status: status(&tx, row, &owner)?,
                dispatch_permitted: false,
                grant: None,
                grant_expires_at_unix_ms: None,
                launch_instruction: q.instruction,
                launch_source_task_id: q.source,
                launch_cancel_requested: q.cancelled,
            });
        }
        if q.count >= 4 {
            return Err(AgentIsolationError::new("agent_host_dispatch_limit"));
        }
        authorize_binding(&tx, manager, &row, true)?;
        let target = validate_agent_enrollment_dispatch_on_connection(
            &tx,
            manager,
            &row.binding.enrollment_id,
            &row.binding.task_id,
            kind.purpose(),
        )?;
        if target.state != "reserved" {
            return Err(AgentIsolationError::new(
                "agent_host_delivery_outcome_unknown",
            ));
        }
        if kind == AgentHostQueueKind::Fallback {
            let record = crate::cli_fallback::cli_fallback_record_on_connection(&tx, &owner, id)
                .map_err(queue_error)?;
            source_stop_proven(&tx, &owner, &record)?;
        }
        let permitted = match kind {
            AgentHostQueueKind::Member => {
                crate::team_agent::claim_team_agent_attempt_on_connection(&tx, &owner, id)
                    .map_err(|e| AgentIsolationError::new(e.code()))?
                    .dispatch_permitted
            }
            AgentHostQueueKind::Fallback => {
                crate::cli_fallback::claim_cli_fallback_on_connection(&tx, &owner, id)
                    .map_err(queue_error)?
                    .dispatch_permitted
            }
        };
        if !permitted {
            let refreshed = queue_view(&tx, &owner, kind, id)?;
            let status = status(&tx, row, &owner)?;
            tx.commit().map_err(unavailable)?;
            return Ok(AgentHostDispatchClaim {
                status,
                dispatch_permitted: false,
                grant: None,
                grant_expires_at_unix_ms: None,
                launch_instruction: refreshed.instruction,
                launch_source_task_id: refreshed.source,
                launch_cancel_requested: refreshed.cancelled,
            });
        }
        let claimed = queue_view(&tx, &owner, kind, id)?;
        let fence = claimed.dispatch.as_deref().ok_or_else(denied)?;
        let issued = issue_delivery_grant_on_connection(
            &tx,
            manager,
            &row.binding.enrollment_id,
            fence,
            now(),
        )?;
        if issued.replayed || issued.grant.is_none() {
            return Err(AgentIsolationError::new(
                "agent_host_delivery_outcome_unknown",
            ));
        }
        tx.execute("UPDATE wc_agent_host_dispatch_bindings SET delivery_attempt_id=?3,no_start_attempt_id=NULL,no_start_code=NULL,
            terminal_request_id=NULL,terminal_receipt_hash=NULL WHERE queue_kind=?1 AND queue_id=?2",params![kind.text(),id,fence]).map_err(unavailable)?;
        let status = status(&tx, binding(&tx, kind, id)?.ok_or_else(denied)?, &owner)?;
        tx.commit().map_err(unavailable)?;
        Ok(AgentHostDispatchClaim {
            status,
            dispatch_permitted: true,
            grant: issued.grant,
            grant_expires_at_unix_ms: Some(issued.expires_at_unix_ms),
            launch_instruction: claimed.instruction,
            launch_source_task_id: claimed.source,
            launch_cancel_requested: claimed.cancelled,
        })
    }
}

/// Public Task finalization calls this in its own transaction. Only the exact
/// actor's REAL incoming binding may be excluded, since its Host cannot finish
/// before this actor finishes the Task. Other descendants remain fenced.
pub(crate) fn require_agent_host_dispatch_quiescent_on_connection(
    conn: &Connection,
    owner_user: &str,
    project: &str,
    task_id: &str,
    completing_actor: Option<&VerifiedAgentRun>,
) -> Result<()> {
    if let Some(actor) = completing_actor {
        validate_agent_run_on_connection(conn, actor)?;
        if actor.owner_user() != owner_user || actor.project() != project {
            return Err(denied());
        }
    }
    let owner = enrollment_principal_for_execution(owner_user);
    let mut tasks = VecDeque::from([task_id.to_owned()]);
    let mut seen_tasks = HashSet::new();
    let mut seen_queues = HashSet::new();
    while let Some(task) = tasks.pop_front() {
        if !seen_tasks.insert(task.clone()) {
            continue;
        }
        if seen_tasks.len() > MAX_REFERENCES {
            return Err(AgentIsolationError::new(
                "agent_host_dispatch_reference_limit",
            ));
        }
        let mut statement=conn.prepare("SELECT 'member',q.team_agent_attempt_id,q.source_task_id FROM wc_team_agent_attempts q
            LEFT JOIN wc_agent_host_dispatch_bindings b ON b.queue_kind='member' AND b.queue_id=q.team_agent_attempt_id
            WHERE q.owner_principal_kind=?1 AND q.owner_principal_digest=?2 AND q.project=?3
              AND (q.root_task_id=?4 OR q.source_task_id=?4 OR b.task_id=?4)
            UNION ALL SELECT 'fallback',q.fallback_id,q.source_task_id FROM wc_cli_fallbacks q
            LEFT JOIN wc_agent_host_dispatch_bindings b ON b.queue_kind='fallback' AND b.queue_id=q.fallback_id
            WHERE q.owner_principal_kind=?1 AND q.owner_principal_digest=?2 AND q.project=?3
              AND (q.root_task_id=?4 OR q.source_task_id=?4 OR b.task_id=?4) LIMIT 4097").map_err(unavailable)?;
        let rows = statement
            .query_map(params![owner.kind, owner.digest, project, task], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(unavailable)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(unavailable)?;
        if rows.len() > MAX_REFERENCES {
            return Err(AgentIsolationError::new(
                "agent_host_dispatch_reference_limit",
            ));
        }
        for (kind, id, source) in rows {
            let kind = AgentHostQueueKind::parse(&kind)?;
            if !seen_queues.insert((kind, id.clone())) {
                continue;
            }
            if seen_queues.len() > MAX_REFERENCES {
                return Err(AgentIsolationError::new(
                    "agent_host_dispatch_reference_limit",
                ));
            }
            let q = queue_view(conn, &owner, kind, &id)?;
            let Some(row) = binding(conn, kind, &id)? else {
                if q.state != "confirmed_not_started"
                    || q.request.is_some()
                    || q.observed
                    || q.evidence.is_none()
                {
                    return Err(AgentIsolationError::new("agent_host_stop_proof_required"));
                }
                if kind == AgentHostQueueKind::Member {
                    tasks.push_back(source);
                }
                continue;
            };
            if row.owner_user != owner_user || row.binding.project != project {
                return Err(denied());
            }
            matches_queue(&row, &q)?;
            tasks.push_back(row.binding.task_id.clone());
            if completing_actor.is_some_and(|actor| {
                row.binding.agent_id == actor.agent_id() && row.binding.task_id == task_id
            }) {
                continue;
            }
            let delivery = delivery_state(conn, &row)?;
            let no_start = row.delivery_attempt_id.is_some()
                && row.no_start_attempt_id == row.delivery_attempt_id
                && row.no_start_code.as_deref().is_some_and(no_start_code)
                && row.no_start_code == q.evidence
                && q.request.is_none()
                && !q.observed
                && delivery == "reserved"
                && matches!(
                    q.state.as_str(),
                    "queued" | "cancelled" | "confirmed_not_started" | "blocked"
                );
            if no_start && q.state != "queued" {
                continue;
            }
            let terminal = q.state == "verified_terminal"
                && q.request.is_some()
                && row.terminal_request_id == q.request
                && q.evidence.as_ref().is_some_and(|proof| {
                    row.terminal_receipt_hash.as_deref() == Some(digest(proof).as_str())
                })
                && delivery == "claimed"
                && matches!(task_state(conn, &row)?.as_str(), "succeeded" | "failed");
            if !terminal {
                return Err(AgentIsolationError::new("agent_host_stop_proof_required"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "agent_host_dispatch_tests.rs"]
mod tests;

/// Trusted Host has verified the original message in its original chat. Renew
/// only that expired, still-unclaimed one-time capability; never mint another
/// identity, re-send the business task, or turn Unknown into NotSent.
impl Database {
    pub fn renew_original_member_delivery(
        &self,
        manager: &VerifiedAgentRun,
        queue_id: &str,
        original_request: &str,
        recovery_key: &str,
    ) -> Result<serde_json::Value> {
        if uuid::Uuid::parse_str(original_request)
            .map(|u| u.to_string())
            .ok()
            .as_deref()
            != Some(original_request)
            || recovery_key.trim().is_empty()
            || recovery_key.len() > 128
            || recovery_key.chars().any(char::is_control)
        {
            return Err(denied());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let row = binding(&tx, AgentHostQueueKind::Member, queue_id)?.ok_or_else(denied)?;
        let owner = authorize_binding(&tx, manager, &row, true)?;
        crate::agent_isolation::ensure_agent_run_execution_open_on_connection(
            &tx,
            &webcodex_core::runner_protocol::AgentRunRef {
                agent_id: manager.agent_id().into(),
                agent_run_id: manager.run_id().into(),
                generation: manager.generation(),
            },
        )?;
        let queue = queue_view(&tx, &owner, AgentHostQueueKind::Member, queue_id)?;
        matches_queue(&row, &queue)?;
        if queue.cancelled
            || !matches!(queue.state.as_str(), "unknown" | "running")
            || queue.request.as_deref() != Some(original_request)
            || queue.dispatch != row.delivery_attempt_id
            || !matches!(task_state(&tx, &row)?.as_str(), "ready" | "active")
        {
            return Err(denied());
        }
        let key = digest(recovery_key);
        let prior:Option<(String,i64)>=tx.query_row("SELECT renewal_id,expires_at FROM wc_agent_enrollment_renewals WHERE enrollment_id=?1 AND key_hash=?2 AND original_request_id=?3",params![row.binding.enrollment_id,key,original_request],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(unavailable)?;
        if let Some((id, expiry)) = prior {
            return Ok(
                serde_json::json!({"renewal_id":id,"renewed":false,"replayed":true,"expires_at_unix_ms":expiry,"original_request_id":original_request,"enrollment_id":row.binding.enrollment_id,"task_id":row.binding.task_id,"business_task_resent":false}),
            );
        }
        let prior_expiry:Option<i64>=tx.query_row("SELECT e.delivery_expires_at_unix_ms FROM wc_agent_enrollments e JOIN wc_agent_runs r ON r.run_id=e.run_id WHERE e.enrollment_id=?1 AND e.agent_id=?2 AND e.task_id=?3 AND e.source_agent_id=?4 AND e.state='issued' AND e.delivery_state='delivery_unknown' AND e.delivery_secret_hash IS NOT NULL AND e.delivery_expires_at_unix_ms<=?5 AND r.state='pending' AND r.previous_run_id IS NULL AND r.execution_close_id IS NULL AND NOT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments a WHERE a.agent_id=e.agent_id) AND NOT EXISTS(SELECT 1 FROM wc_agent_host_windows w WHERE w.run_id=e.run_id)",params![row.binding.enrollment_id,row.binding.agent_id,row.binding.task_id,manager.agent_id(),now()],|r|r.get(0)).optional().map_err(unavailable)?;
        let prior_expiry = prior_expiry
            .ok_or_else(|| AgentIsolationError::new("enrollment_original_renewal_ineligible"))?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_enrollment_renewals WHERE enrollment_id=?1",
                [&row.binding.enrollment_id],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
        if count >= 3 {
            return Err(AgentIsolationError::new(
                "enrollment_original_renewal_limit",
            ));
        }
        let expiry = now() + 5 * 60 * 1000;
        let id = format!("wc_enrollment_renewal_{}", uuid::Uuid::new_v4().simple());
        tx.execute("INSERT INTO wc_agent_enrollment_renewals(renewal_id,enrollment_id,original_request_id,key_hash,previous_expiry,expires_at,source_run_id,source_generation,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,row.binding.enrollment_id,original_request,key,prior_expiry,expiry,manager.run_id(),manager.generation(),now()]).map_err(unavailable)?;
        // The original secret hash, delivery key and Unknown record are retained.
        tx.execute("UPDATE wc_agent_enrollments SET delivery_expires_at_unix_ms=?2,delivery_source_run_id=?3,delivery_source_generation=?4 WHERE enrollment_id=?1",params![row.binding.enrollment_id,expiry,manager.run_id(),manager.generation()]).map_err(unavailable)?;
        tx.execute("UPDATE wc_agent_runs SET expires_at_unix_ms=MAX(expires_at_unix_ms,?2) WHERE run_id=(SELECT run_id FROM wc_agent_enrollments WHERE enrollment_id=?1) AND state='pending'",params![row.binding.enrollment_id,now()+crate::agent_isolation::AGENT_RUN_LIFETIME_MS]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        Ok(
            serde_json::json!({"renewal_id":id,"renewed":true,"replayed":false,"expires_at_unix_ms":expiry,"original_request_id":original_request,"enrollment_id":row.binding.enrollment_id,"task_id":row.binding.task_id,"business_task_resent":false}),
        )
    }
}
