//! Authenticated terminal snapshots and independent business review.
use crate::agent_isolation::{
    agent_run_business_gate_on_connection, authorize_agent_assignment_on_connection,
    enrollment_principal_for_execution,
};
use crate::agent_meeting::{
    authorize_room, create_agent_meeting_on_connection, load_agent_meeting_artifact_on_connection,
    load_entry, publish_agent_meeting_entry_as_run_on_connection,
    publish_agent_meeting_entry_on_connection, MAX_AGENT_MEETING_ARTIFACT_BYTES,
};
use crate::{
    AgentAssignmentOperation, AgentEnrollmentPurpose, AgentMeetingKind, NewAgentMeetingEntry,
};
use crate::{
    AgentMeetingArtifactTarget, AgentTaskState, CommunicationStoreError, Database, VerifiedAgentRun,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use webcodex_core::runner_protocol::AgentExecutionBinding;

type Result<T> = std::result::Result<T, CommunicationStoreError>;
fn denied() -> CommunicationStoreError {
    CommunicationStoreError::new(
        "agent_cli_delivery_unavailable",
        "CLI delivery is unavailable for this operation",
    )
}
fn storage(_: impl std::fmt::Display) -> CommunicationStoreError {
    denied()
}
fn invalid() -> CommunicationStoreError {
    CommunicationStoreError::new(
        "invalid_agent_cli_delivery",
        "CLI delivery input is invalid",
    )
}
fn conflict() -> CommunicationStoreError {
    CommunicationStoreError::new(
        "agent_cli_delivery_idempotency_conflict",
        "CLI delivery intent conflicts with the original publication",
    )
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(storage)
}
fn decode<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(storage)
}

/// Trusted Root adapter only: implement for the real D verified delivery proof,
/// never for raw DTO/JSON, arbitrary bytes, model input, or a run credential.
pub trait AuthenticatedAgentCliDeliverySnapshot {
    fn binding(&self) -> &AgentExecutionBinding;
    fn artifact_bytes(&self) -> &[u8];
    fn artifact_sha256(&self) -> &str;
}

/// A fresh read of accepted Store evidence for the trusted Root-to-D recovery
/// callback. Private construction and no Serde: this is never model input or a
/// dispatch/activation capability, and it must not be cached as current authority.
pub struct AgentCliDeliveryStopEvidence {
    binding: AgentExecutionBinding,
    close: webcodex_core::runner_protocol::AgentExecutionClose,
    receipt: webcodex_core::runner_protocol::AgentExecutionCloseReceipt,
}
impl AgentCliDeliveryStopEvidence {
    pub fn binding(&self) -> &AgentExecutionBinding {
        &self.binding
    }
    pub fn close(&self) -> &webcodex_core::runner_protocol::AgentExecutionClose {
        &self.close
    }
    pub fn receipt(&self) -> &webcodex_core::runner_protocol::AgentExecutionCloseReceipt {
        &self.receipt
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCliDeliveryReference {
    pub source_attempt_id: String,
    pub room_id: String,
    pub entry_id: String,
    pub artifact: AgentMeetingArtifactTarget,
    pub artifact_sha256: String,
    pub worker_agent_id: String,
    pub worker_run_id: String,
    pub worker_generation: i64,
    pub published_at_unix_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCliReviewDecision {
    Accept,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCliDeliveryReviewReference {
    pub entry_id: String,
    pub reviewer_agent_id: String,
    pub reviewer_run_id: String,
    pub reviewer_generation: i64,
    pub decision: AgentCliReviewDecision,
    pub artifact: AgentMeetingArtifactTarget,
    pub artifact_sha256: String,
    pub created_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentCliDeliveryView {
    pub delivery: AgentCliDeliveryReference,
    pub artifact_content: String,
    pub worker_task_id: String,
    pub worker_revision: i64,
    pub worker_state: AgentTaskState,
    pub review: Option<AgentCliDeliveryReviewReference>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentCliDeliveryReviewResult {
    pub delivery: AgentCliDeliveryReference,
    pub review: AgentCliDeliveryReviewReference,
    pub worker_task_id: String,
    pub worker_revision: i64,
    pub worker_state: AgentTaskState,
    pub replayed: bool,
}

/// Explicit rejection without claiming that an artifact was published/reviewed.
#[derive(Debug, Clone, Serialize)]
pub struct AgentCliDeliveryRejectionResult {
    pub source_attempt_id: String,
    pub worker_task_id: String,
    pub worker_revision: i64,
    pub worker_state: AgentTaskState,
    pub replayed: bool,
}

#[derive(Serialize, Deserialize)]
struct StoredDelivery {
    version: u32,
    binding: AgentExecutionBinding,
    reference: AgentCliDeliveryReference,
    root_manager_agent_id: String,
}
#[derive(Serialize, Deserialize)]
struct StoredReview {
    version: u32,
    binding_sha256: String,
    room_id: String,
    reference: AgentCliDeliveryReviewReference,
    content_sha256: String,
    completion_key_sha256: String,
    expected_worker_revision: i64,
    #[serde(default)]
    worker_terminal_preserved: bool,
    request_hash: String,
}
/// Uses the original attempt's existing immutable review_json slot. There is no
/// fake meeting entry/artifact and no second completion or quota ledger.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredUnpublishedRejection {
    version: u32,
    kind: String,
    binding_sha256: String,
    reviewer_agent_id: String,
    reviewer_run_id: String,
    reviewer_generation: i64,
    expected_worker_revision: i64,
    reason_sha256: String,
    completion_key_sha256: String,
    request_hash: String,
}
struct OriginalAttempt {
    binding: AgentExecutionBinding,
    owner: String,
    root_manager_agent_id: String,
    terminal: bool,
    delivery_json: Option<String>,
    review_json: Option<String>,
}

pub(crate) fn init_agent_cli_delivery_tables(conn: &Connection) -> anyhow::Result<()> {
    let names = conn
        .prepare("PRAGMA table_info(wc_team_cli_attempts)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for name in ["delivery_json", "review_json"] {
        if !names.iter().any(|v| v == name) {
            conn.execute_batch(&format!(
                "ALTER TABLE wc_team_cli_attempts ADD COLUMN {name} TEXT"
            ))?;
        }
    }
    conn.execute_batch("CREATE TRIGGER IF NOT EXISTS wc_cli_delivery_immutable BEFORE UPDATE OF delivery_json ON wc_team_cli_attempts WHEN OLD.delivery_json IS NOT NULL AND NEW.delivery_json IS NOT OLD.delivery_json BEGIN SELECT RAISE(ABORT,'CLI delivery is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS wc_cli_review_immutable BEFORE UPDATE OF review_json ON wc_team_cli_attempts WHEN OLD.review_json IS NOT NULL AND NEW.review_json IS NOT OLD.review_json BEGIN SELECT RAISE(ABORT,'CLI review is immutable'); END;")?;
    Ok(())
}

fn original_attempt(conn: &Connection, attempt_id: &str) -> Result<OriginalAttempt> {
    // Read only binding/terminal metadata, never the attempt's raw result text.
    let raw:Option<(Option<String>,String,Option<String>,Option<String>,String,String,String,String,String,i64)>=conn.query_row(
        "SELECT json_extract(record_json,'$.execution_binding'),json_extract(record_json,'$.dispatch_state'),delivery_json,review_json,owner_principal_kind,owner_principal_digest,project_id,root_task_id,source_task_id,call_index FROM wc_team_cli_attempts WHERE attempt_id=?1",
        [attempt_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)),
    ).optional().map_err(storage)?;
    let (
        binding,
        state,
        delivery_json,
        review_json,
        kind,
        owner_digest,
        project,
        root,
        source,
        index,
    ) = raw.ok_or_else(denied)?;
    let binding: AgentExecutionBinding = decode(&binding.ok_or_else(denied)?)?;
    binding.validate().map_err(storage)?;
    if binding.requester.cli_attempt_id != attempt_id
        || binding.project != project
        || binding.requester.root_task_id != root
        || binding.requester.source_task_id != source
        || i64::from(binding.requester.call_index) != index
    {
        return Err(denied());
    }
    let worker:Option<(String,String)>=conn.query_row("SELECT a.owner_user,e.source_agent_id FROM wc_isolated_agents a JOIN wc_agent_enrollments e ON e.agent_id=a.agent_id WHERE a.agent_id=?1 AND a.project=?2 AND e.enrollment_id=?3 AND e.task_id=?4 AND e.purpose='cli_worker' AND e.owner_user=a.owner_user AND e.project=a.project AND json_extract(e.policy_json,'$.max_subagents')=0 AND json_extract(e.policy_json,'$.max_cli_calls')=0",params![binding.executor.run.agent_id,binding.project,binding.executor.enrollment_id,binding.executor.task_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage)?;
    let (owner, parent) = worker.ok_or_else(denied)?;
    if parent != binding.requester.run.agent_id {
        return Err(denied());
    }
    let principal = enrollment_principal_for_execution(&owner);
    if principal.kind != kind || principal.digest != owner_digest {
        return Err(denied());
    }
    let requester:Option<(String,Option<String>)>=conn.query_row("SELECT purpose,source_agent_id FROM wc_agent_enrollments WHERE enrollment_id=?1 AND agent_id=?2 AND owner_user=?3 AND project=?4 AND task_id=?5",params![binding.requester.enrollment_id,binding.requester.run.agent_id,owner,binding.project,binding.requester.source_task_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage)?;
    let (purpose, manager) = requester.ok_or_else(denied)?;
    let root_manager_agent_id = match purpose.as_str() {
        "manager" if binding.requester.root_task_id == binding.requester.source_task_id => {
            binding.requester.run.agent_id.clone()
        }
        "member" => {
            let manager = manager.ok_or_else(denied)?;
            let exact:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_enrollments WHERE agent_id=?1 AND owner_user=?2 AND project=?3 AND purpose='manager' AND task_id=?4)",params![manager,owner,binding.project,binding.requester.root_task_id],|r|r.get(0)).map_err(storage)?;
            if !exact {
                return Err(denied());
            }
            manager
        }
        _ => return Err(denied()),
    };
    for run in [&binding.requester.run, &binding.executor.run] {
        let exact:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id WHERE r.run_id=?1 AND r.agent_id=?2 AND r.generation=?3 AND a.owner_user=?4 AND a.project=?5)",params![run.agent_run_id,run.agent_id,run.generation,owner,binding.project],|r|r.get(0)).map_err(storage)?;
        if !exact {
            return Err(denied());
        }
    }
    Ok(OriginalAttempt {
        binding,
        owner,
        root_manager_agent_id,
        terminal: state == "terminal",
        delivery_json,
        review_json,
    })
}

fn published(
    conn: &Connection,
    original: &OriginalAttempt,
) -> Result<(StoredDelivery, crate::AgentMeetingArtifact)> {
    let delivery: StoredDelivery = decode(original.delivery_json.as_deref().ok_or_else(denied)?)?;
    let reference = &delivery.reference;
    if delivery.version != 1
        || delivery.binding != original.binding
        || delivery.root_manager_agent_id != original.root_manager_agent_id
        || reference.source_attempt_id != original.binding.requester.cli_attempt_id
        || reference.worker_agent_id != original.binding.executor.run.agent_id
        || reference.worker_run_id != original.binding.executor.run.agent_run_id
        || reference.worker_generation != original.binding.executor.run.generation
    {
        return Err(denied());
    }
    let room_matches: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_meeting_rooms WHERE room_id=?1 AND project=?2)",
            params![reference.room_id, original.binding.project],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if !room_matches {
        return Err(denied());
    }
    let artifact = load_agent_meeting_artifact_on_connection(
        conn,
        &reference.room_id,
        &reference.artifact.artifact_id,
        reference.artifact.version,
    )
    .map_err(storage)?;
    let entry = load_entry(conn, &reference.entry_id).map_err(storage)?;
    if artifact.author_agent_id != reference.worker_agent_id
        || digest(&artifact.bytes) != reference.artifact_sha256
        || entry.room_id != reference.room_id
        || entry.kind != AgentMeetingKind::Artifact
        || entry.artifact.as_ref() != Some(&reference.artifact)
        || entry.author_agent_id != reference.worker_agent_id
        || entry.author_run_id != reference.worker_run_id
        || entry.author_generation != reference.worker_generation
    {
        return Err(denied());
    }
    let members = conn
        .prepare("SELECT agent_id FROM wc_agent_meeting_members WHERE room_id=?1 ORDER BY agent_id")
        .map_err(storage)?
        .query_map([&reference.room_id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(storage)?;
    let expected: BTreeSet<_> = [
        original.binding.executor.run.agent_id.clone(),
        original.binding.requester.run.agent_id.clone(),
        original.root_manager_agent_id.clone(),
    ]
    .into_iter()
    .collect();
    if members != expected {
        return Err(denied());
    }
    Ok((delivery, artifact))
}

fn reviewer_authority(
    conn: &Connection,
    current: &VerifiedAgentRun,
    original: &OriginalAttempt,
    write: bool,
) -> Result<()> {
    if current.owner_user() != original.owner
        || current.project() != original.binding.project
        || current.agent_id() == original.binding.executor.run.agent_id
    {
        return Err(denied());
    }
    let task = if current.agent_id() == original.binding.requester.run.agent_id {
        &original.binding.requester.source_task_id
    } else if current.agent_id() == original.root_manager_agent_id {
        &original.binding.requester.root_task_id
    } else {
        return Err(denied());
    };
    let authority = authorize_agent_assignment_on_connection(
        conn,
        current,
        task,
        if write {
            AgentAssignmentOperation::TaskComplete
        } else {
            AgentAssignmentOperation::Read
        },
    )
    .map_err(storage)?;
    if (current.agent_id() == original.root_manager_agent_id
        && authority.purpose != AgentEnrollmentPurpose::Manager)
        || !matches!(
            authority.purpose,
            AgentEnrollmentPurpose::Manager | AgentEnrollmentPurpose::Member
        )
    {
        return Err(denied());
    }
    Ok(())
}

fn worker_execution(
    conn: &Connection,
    original: &OriginalAttempt,
) -> Result<crate::AgentPublicTaskExecution> {
    let execution = crate::agent_task::public_execution_on_connection(
        conn,
        &original.binding.executor.task_id,
    )?
    .ok_or_else(denied)?;
    if execution.agent_id != original.binding.executor.run.agent_id {
        return Err(denied());
    }
    Ok(execution)
}

fn stored_review(
    conn: &Connection,
    original: &OriginalAttempt,
    delivery: &StoredDelivery,
) -> Result<Option<StoredReview>> {
    let Some(raw) = &original.review_json else {
        return Ok(None);
    };
    let review: StoredReview = decode(raw)?;
    let reference = &review.reference;
    let entry = load_entry(conn, &reference.entry_id).map_err(storage)?;
    if review.version != 1
        || review.binding_sha256 != original.binding.binding_hash().map_err(storage)?
        || review.room_id != delivery.reference.room_id
        || reference.artifact != delivery.reference.artifact
        || reference.artifact_sha256 != delivery.reference.artifact_sha256
        || reference.reviewer_agent_id == original.binding.executor.run.agent_id
        || !(reference.reviewer_agent_id == original.binding.requester.run.agent_id
            || reference.reviewer_agent_id == original.root_manager_agent_id)
        || entry.kind != AgentMeetingKind::Review
        || entry.room_id != review.room_id
        || entry.review_target.as_ref() != Some(&reference.artifact)
        || entry.author_agent_id != reference.reviewer_agent_id
        || entry.author_run_id != reference.reviewer_run_id
        || entry.author_generation != reference.reviewer_generation
        || digest(entry.content.as_bytes()) != review.content_sha256
    {
        return Err(denied());
    }
    Ok(Some(review))
}

impl Database {
    /// Current caller/root-manager decision only. Missing delivery never
    /// implies automatic failure, and this entry point cannot approve success.
    pub fn reject_agent_cli_delivery_unpublished(
        &self,
        current_reviewer: &VerifiedAgentRun,
        source_attempt_id: &str,
        expected_worker_revision: i64,
        reason: &str,
        completion_key: &str,
    ) -> Result<AgentCliDeliveryRejectionResult> {
        let reason = reason.trim();
        if expected_worker_revision < 1
            || reason.is_empty()
            || reason.len() > crate::MAX_AGENT_TASK_TERMINAL_TEXT_BYTES
            || reason.contains('\0')
            || webcodex_core::sensitive_text::secret_like_value(reason)
        {
            return Err(invalid());
        }
        let key = crate::communication::validate_idempotency_key(completion_key)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let original = original_attempt(&tx, source_attempt_id)?;
        reviewer_authority(&tx, current_reviewer, &original, true)?;
        if !original.terminal {
            return Err(denied());
        }
        if original.delivery_json.is_some() {
            return Err(CommunicationStoreError::new(
                "agent_cli_delivery_artifact_review_required",
                "A published delivery requires its exact artifact/version review",
            ));
        }
        if !crate::agent_execution_close::agent_execution_binding_stop_proven_on_connection(
            &tx,
            &original.binding,
        )
        .map_err(storage)?
        {
            return Err(denied());
        }
        let binding_sha256 = original.binding.binding_hash().map_err(storage)?;
        let reason_sha256 = digest(reason.as_bytes());
        let completion_key_sha256 = digest(key.as_bytes());
        // Bind the existing Task completion key to the original execution and
        // logical reviewer. A successor run of that Agent can retry; a different
        // authorized reviewer cannot borrow the first reviewer's completion.
        let scoped_key = format!("cli-no-delivery:{}", digest(encode(&serde_json::json!({
            "domain":"webcodex.cli.unpublished-rejection.v1", "binding_sha256":binding_sha256,
            "reviewer_agent_id":current_reviewer.agent_id(), "key_sha256":completion_key_sha256,
        }))?.as_bytes()));
        let request_hash = digest(
            encode(&serde_json::json!({
                "binding_sha256":binding_sha256,"reviewer_agent_id":current_reviewer.agent_id(),
                "expected_worker_revision":expected_worker_revision,"reason_sha256":reason_sha256,
                "completion_key_sha256":completion_key_sha256,
            }))?
            .as_bytes(),
        );
        let prior = original
            .review_json
            .as_deref()
            .map(decode::<StoredUnpublishedRejection>)
            .transpose()?;
        if let Some(saved) = &prior {
            if saved.version != 1
                || saved.kind != "unpublished_rejection"
                || saved.binding_sha256 != binding_sha256
                || saved.reviewer_agent_id != current_reviewer.agent_id()
                || saved.expected_worker_revision != expected_worker_revision
                || saved.reason_sha256 != reason_sha256
                || saved.completion_key_sha256 != completion_key_sha256
                || saved.request_hash != request_hash
            {
                return Err(conflict());
            }
            let author: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM wc_agent_runs r JOIN wc_isolated_agents a ON a.agent_id=r.agent_id
                 WHERE r.run_id=?1 AND r.agent_id=?2 AND r.generation=?3 AND a.owner_user=?4 AND a.project=?5)",
                params![saved.reviewer_run_id,saved.reviewer_agent_id,saved.reviewer_generation,
                    original.owner,original.binding.project], |r| r.get(0),
            ).map_err(storage)?;
            let execution = worker_execution(&tx, &original)?;
            if !author
                || execution.state != AgentTaskState::Failed
                || Some(execution.revision) != expected_worker_revision.checked_add(1)
            {
                return Err(denied());
            }
        }
        // This shared primitive retains the real stopped-tree/terminal/Task CAS
        // gates and the original completion receipt. It never completes a parent.
        let completed = crate::agent_task::complete_cli_worker_task_on_connection(
            &tx,
            &original.binding,
            expected_worker_revision,
            AgentTaskState::Failed,
            None,
            Some(reason),
            &scoped_key,
        )?;
        if completed.replayed != prior.is_some() {
            return Err(conflict());
        }
        if prior.is_none() {
            let saved = StoredUnpublishedRejection {
                version: 1,
                kind: "unpublished_rejection".into(),
                binding_sha256,
                reviewer_agent_id: current_reviewer.agent_id().into(),
                reviewer_run_id: current_reviewer.run_id().into(),
                reviewer_generation: current_reviewer.generation(),
                expected_worker_revision,
                reason_sha256,
                completion_key_sha256,
                request_hash,
            };
            let changed = tx
                .execute(
                    "UPDATE wc_team_cli_attempts SET review_json=?2 WHERE attempt_id=?1
                 AND review_json IS NULL AND delivery_json IS NULL",
                    params![source_attempt_id, encode(&saved)?],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(conflict());
            }
            tx.execute(
                "INSERT INTO wc_agent_isolation_audit(event_id,agent_id,run_id,action,code,created_at_unix_ms)
                 VALUES(?1,?2,?3,'reject_cli_delivery_unpublished','ok',?4)",
                params![uuid::Uuid::new_v4().to_string(),current_reviewer.agent_id(),
                    current_reviewer.run_id(),chrono::Utc::now().timestamp_millis()],
            ).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(AgentCliDeliveryRejectionResult {
            source_attempt_id: original.binding.requester.cli_attempt_id,
            worker_task_id: completed.execution.task_id,
            worker_revision: completed.execution.revision,
            worker_state: completed.execution.state,
            replayed: completed.replayed,
        })
    }

    /// Recover only the original stopped delivery's read proof after a server
    /// restart. Published artifacts should be read directly from Store instead.
    /// No writes, new identity, close plan, Job, or dispatch permission is created.
    pub fn read_agent_cli_delivery_stop_evidence(
        &self,
        current_reviewer: &VerifiedAgentRun,
        expected_binding: &AgentExecutionBinding,
    ) -> Result<AgentCliDeliveryStopEvidence> {
        expected_binding.validate().map_err(storage)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(storage)?;
        let original = original_attempt(&tx, &expected_binding.requester.cli_attempt_id)?;
        if original.binding != *expected_binding || !original.terminal {
            return Err(denied());
        }
        reviewer_authority(&tx, current_reviewer, &original, false)?;
        let task = if current_reviewer.agent_id() == original.binding.requester.run.agent_id {
            &original.binding.requester.source_task_id
        } else {
            &original.binding.requester.root_task_id
        };
        let assigned: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments WHERE agent_id=?1
             AND owner_user=?2 AND project=?3 AND task_id=?4 AND state='active')",
                params![
                    current_reviewer.agent_id(),
                    original.owner,
                    original.binding.project,
                    task
                ],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if !assigned {
            return Err(denied());
        }
        let (close, receipt) =
            crate::agent_execution_close::read_agent_execution_tree_exited_proof_on_connection(
                &tx,
                &original.binding,
            )
            .map_err(|_| {
                CommunicationStoreError::new(
                    "agent_cli_delivery_stop_evidence_unavailable",
                    "Original accepted physical-exit evidence is unavailable",
                )
            })?;
        Ok(AgentCliDeliveryStopEvidence {
            binding: original.binding,
            close,
            receipt,
        })
    }

    pub fn publish_agent_cli_delivery(
        &self,
        proof: &impl AuthenticatedAgentCliDeliverySnapshot,
    ) -> Result<AgentCliDeliveryReference> {
        let binding = proof.binding().clone();
        binding.validate().map_err(storage)?;
        let bytes = proof.artifact_bytes();
        if bytes.is_empty() || bytes.len() > MAX_AGENT_MEETING_ARTIFACT_BYTES {
            return Err(invalid());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let sha = digest(bytes);
        if sha != proof.artifact_sha256() {
            return Err(invalid());
        }
        let bundle: serde_json::Value = serde_json::from_str(text).map_err(|_| invalid())?;
        if bundle.get("version").and_then(|v| v.as_u64()) != Some(1)
            || bundle.get("binding_sha256").and_then(|v| v.as_str())
                != Some(binding.binding_hash().map_err(storage)?.as_str())
        {
            return Err(invalid());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let original = original_attempt(&tx, &binding.requester.cli_attempt_id)?;
        if original.binding != binding || !original.terminal {
            return Err(denied());
        }
        if !crate::agent_execution_close::agent_execution_binding_stop_proven_on_connection(
            &tx, &binding,
        )
        .map_err(storage)?
        {
            return Err(denied());
        }
        let execution = worker_execution(&tx, &original)?;
        if original.delivery_json.is_some() {
            let (saved, artifact) = published(&tx, &original)?;
            if saved.reference.artifact_sha256 != sha || artifact.bytes != bytes {
                return Err(conflict());
            }
            return Ok(saved.reference);
        }
        // The publisher and explicit unpublished rejection serialize on the
        // same Immediate transaction. A rejection winner closes first publish;
        // ordinary observer failures without this decision may still publish
        // explicit stage results. Existing immutable publication replay is above.
        if let Some(raw) = &original.review_json {
            let rejection: StoredUnpublishedRejection = decode(raw)?;
            if rejection.version != 1
                || rejection.kind != "unpublished_rejection"
                || rejection.binding_sha256 != binding.binding_hash().map_err(storage)?
                || ![
                    original.binding.requester.run.agent_id.as_str(),
                    original.root_manager_agent_id.as_str(),
                ]
                .contains(&rejection.reviewer_agent_id.as_str())
                || rejection.expected_worker_revision < 1
                || execution.state != AgentTaskState::Failed
                || Some(execution.revision) != rejection.expected_worker_revision.checked_add(1)
                || execution
                    .terminal_reason
                    .as_deref()
                    .map(|text| digest(text.as_bytes()))
                    != Some(rejection.reason_sha256)
            {
                return Err(denied());
            }
            return Err(CommunicationStoreError::new(
                "agent_cli_delivery_unpublished_rejected",
                "This unpublished delivery was explicitly rejected",
            ));
        }
        let participants: BTreeSet<_> = [
            binding.executor.run.agent_id.clone(),
            binding.requester.run.agent_id.clone(),
            original.root_manager_agent_id.clone(),
        ]
        .into_iter()
        .collect();
        let room = create_agent_meeting_on_connection(
            &tx,
            &binding.project,
            &participants.into_iter().collect::<Vec<_>>(),
            "Public CLI delivery for independent review",
        )
        .map_err(storage)?;
        let entry = publish_agent_meeting_entry_as_run_on_connection(
            &tx,
            &binding.project,
            &binding.executor.run,
            &room,
            NewAgentMeetingEntry {
                kind: AgentMeetingKind::Artifact,
                content: "Authenticated CLI delivery snapshot".into(),
                artifact_id: None,
                artifact_bytes: Some(bytes.to_vec()),
                review_target: None,
                idempotency_key: format!("cli-delivery:{}", binding.requester.cli_attempt_id),
            },
        )
        .map_err(storage)?;
        let reference = AgentCliDeliveryReference {
            source_attempt_id: binding.requester.cli_attempt_id.clone(),
            room_id: room,
            entry_id: entry.entry_id,
            artifact: entry.artifact.ok_or_else(denied)?,
            artifact_sha256: sha,
            worker_agent_id: binding.executor.run.agent_id.clone(),
            worker_run_id: binding.executor.run.agent_run_id.clone(),
            worker_generation: binding.executor.run.generation,
            published_at_unix_ms: entry.created_at_unix_ms,
        };
        let saved = StoredDelivery {
            version: 1,
            binding,
            reference: reference.clone(),
            root_manager_agent_id: original.root_manager_agent_id,
        };
        let changed=tx.execute("UPDATE wc_team_cli_attempts SET delivery_json=?2 WHERE attempt_id=?1 AND delivery_json IS NULL",params![reference.source_attempt_id,encode(&saved)?]).map_err(storage)?;
        if changed != 1 {
            return Err(conflict());
        }
        tx.commit().map_err(storage)?;
        Ok(reference)
    }

    pub fn read_agent_cli_delivery(
        &self,
        current_reviewer: &VerifiedAgentRun,
        source_attempt_id: &str,
    ) -> Result<AgentCliDeliveryView> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(storage)?;
        let original = original_attempt(&tx, source_attempt_id)?;
        reviewer_authority(&tx, current_reviewer, &original, false)?;
        let (delivery, artifact) = published(&tx, &original)?;
        authorize_room(&tx, current_reviewer, &delivery.reference.room_id).map_err(storage)?;
        let execution = worker_execution(&tx, &original)?;
        let review = stored_review(&tx, &original, &delivery)?.map(|r| r.reference);
        let artifact_content = String::from_utf8(artifact.bytes).map_err(storage)?;
        Ok(AgentCliDeliveryView {
            delivery: delivery.reference,
            artifact_content,
            worker_task_id: execution.task_id,
            worker_revision: execution.revision,
            worker_state: execution.state,
            review,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn review_agent_cli_delivery(
        &self,
        current_reviewer: &VerifiedAgentRun,
        source_attempt_id: &str,
        expected_worker_revision: i64,
        artifact_target: &AgentMeetingArtifactTarget,
        decision: AgentCliReviewDecision,
        review_content: &str,
        completion_key: &str,
    ) -> Result<AgentCliDeliveryReviewResult> {
        if expected_worker_revision < 1
            || review_content.trim().is_empty()
            || review_content.len() > 8192
            || review_content.contains('\0')
            || webcodex_core::sensitive_text::secret_like_value(review_content)
        {
            return Err(invalid());
        }
        let key = crate::communication::validate_idempotency_key(completion_key)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let original = original_attempt(&tx, source_attempt_id)?;
        reviewer_authority(&tx, current_reviewer, &original, false)?;
        // Historical exact replay needs current identity and an open business
        // context, but it does not require reactivating a completed caller Task.
        agent_run_business_gate_on_connection(&tx, current_reviewer).map_err(storage)?;
        let (delivery, _) = published(&tx, &original)?;
        authorize_room(&tx, current_reviewer, &delivery.reference.room_id).map_err(storage)?;
        if artifact_target != &delivery.reference.artifact {
            return Err(denied());
        }
        let binding_sha = original.binding.binding_hash().map_err(storage)?;
        let content_sha = digest(review_content.as_bytes());
        let key_sha = digest(key.as_bytes());
        let request_hash=digest(encode(&serde_json::json!({"binding_sha256":binding_sha,"room_id":delivery.reference.room_id,"artifact":artifact_target,"artifact_sha256":delivery.reference.artifact_sha256,"reviewer_agent_id":current_reviewer.agent_id(),"expected_worker_revision":expected_worker_revision,"decision":decision,"content_sha256":content_sha,"completion_key_sha256":key_sha}))?.as_bytes());
        if let Some(review) = stored_review(&tx, &original, &delivery)? {
            if review.request_hash != request_hash
                || review.reference.reviewer_agent_id != current_reviewer.agent_id()
            {
                return Err(conflict());
            }
            let execution = worker_execution(&tx, &original)?;
            let expected_state = if review.worker_terminal_preserved {
                AgentTaskState::Failed
            } else {
                match review.reference.decision {
                    AgentCliReviewDecision::Accept => AgentTaskState::Succeeded,
                    AgentCliReviewDecision::Reject => AgentTaskState::Failed,
                }
            };
            let expected_revision = review.expected_worker_revision.saturating_add(
                if review.worker_terminal_preserved {
                    0
                } else {
                    1
                },
            );
            if execution.revision != expected_revision || execution.state != expected_state {
                return Err(denied());
            }
            return Ok(AgentCliDeliveryReviewResult {
                delivery: delivery.reference,
                review: review.reference,
                worker_task_id: execution.task_id,
                worker_revision: execution.revision,
                worker_state: execution.state,
                replayed: true,
            });
        }
        reviewer_authority(&tx, current_reviewer, &original, true)?;
        let entry = publish_agent_meeting_entry_on_connection(
            &tx,
            current_reviewer,
            &delivery.reference.room_id,
            NewAgentMeetingEntry {
                kind: AgentMeetingKind::Review,
                content: review_content.into(),
                artifact_id: None,
                artifact_bytes: None,
                review_target: Some(artifact_target.clone()),
                idempotency_key: format!("cli-review:{}", uuid::Uuid::new_v4().simple()),
            },
        )
        .map_err(storage)?;
        let (outcome, result, reason) = match decision {
            AgentCliReviewDecision::Accept => (
                AgentTaskState::Succeeded,
                Some("CLI delivery accepted after independent review"),
                None,
            ),
            AgentCliReviewDecision::Reject => (
                AgentTaskState::Failed,
                None,
                Some("CLI delivery rejected by an authorized reviewer"),
            ),
        };
        let execution = worker_execution(&tx, &original)?;
        let worker_terminal_preserved = execution.state == AgentTaskState::Failed;
        let execution = if worker_terminal_preserved {
            // A stopped failed process may already have explicitly published
            // useful files. Review that immutable artifact without rewriting
            // the worker's failed completion, revision, reason or replay key.
            if execution.revision != expected_worker_revision
                || !original.terminal
                || !crate::agent_execution_close::agent_execution_binding_stop_proven_on_connection(
                    &tx,
                    &original.binding,
                )
                .map_err(storage)?
            {
                return Err(denied());
            }
            execution
        } else {
            crate::agent_task::complete_cli_worker_task_on_connection(
                &tx,
                &original.binding,
                expected_worker_revision,
                outcome,
                result,
                reason,
                &key,
            )?
            .execution
        };
        let reference = AgentCliDeliveryReviewReference {
            entry_id: entry.entry_id,
            reviewer_agent_id: entry.author_agent_id,
            reviewer_run_id: entry.author_run_id,
            reviewer_generation: entry.author_generation,
            decision,
            artifact: artifact_target.clone(),
            artifact_sha256: delivery.reference.artifact_sha256.clone(),
            created_at_unix_ms: entry.created_at_unix_ms,
        };
        let saved = StoredReview {
            version: 1,
            binding_sha256: binding_sha,
            room_id: delivery.reference.room_id.clone(),
            reference: reference.clone(),
            content_sha256: content_sha,
            completion_key_sha256: key_sha,
            expected_worker_revision,
            worker_terminal_preserved,
            request_hash,
        };
        let changed=tx.execute("UPDATE wc_team_cli_attempts SET review_json=?2 WHERE attempt_id=?1 AND review_json IS NULL",params![source_attempt_id,encode(&saved)?]).map_err(storage)?;
        if changed != 1 {
            return Err(conflict());
        }
        tx.commit().map_err(storage)?;
        Ok(AgentCliDeliveryReviewResult {
            delivery: delivery.reference,
            review: reference,
            worker_task_id: execution.task_id,
            worker_revision: execution.revision,
            worker_state: execution.state,
            replayed: false,
        })
    }
}
