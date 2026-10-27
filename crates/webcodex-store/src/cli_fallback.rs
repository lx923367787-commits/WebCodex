//! Durable reservations for the coordinator's single CLI recovery slot.
//!
//! This is a management-only API. A provider response is an observation, not
//! business completion; uncertain dispatch and cancellation retain the slot.

use crate::communication::{digest_text, now_unix_ms, validate_communication_principal};
use crate::team_agent::{profile_unavailable_evidence, CandidateScanCursor};
use crate::team_task::load_team_cli_attempt_for_fallback;
use crate::{CommunicationPrincipal, Database};
use anyhow::ensure;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use webcodex_core::cli_fallback::{
    cli_fallback_eligibility, CliCancellationOrigin, CliFailureReason,
};
use webcodex_core::runner_protocol::ShellJobInfo;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliFallbackState {
    Queued,
    Starting,
    Running,
    Unknown,
    Blocked,
    Cancelled,
    ConfirmedNotStarted,
    VerifiedTerminal,
}

impl CliFallbackState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Unknown => "unknown",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::ConfirmedNotStarted => "confirmed_not_started",
            Self::VerifiedTerminal => "verified_terminal",
        }
    }

    pub const fn holds_slot(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Unknown)
    }

    const fn terminal(self) -> bool {
        matches!(
            self,
            Self::Blocked | Self::Cancelled | Self::ConfirmedNotStarted | Self::VerifiedTerminal
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliFallbackRecord {
    pub fallback_id: String,
    pub root_task_id: String,
    pub source_task_id: String,
    pub source_attempt_id: String,
    pub project: String,
    pub source_job_id: String,
    pub source_request_id: String,
    pub client_id: String,
    pub instruction: String,
    pub thinking_profile_id: String,
    pub failure_reason: CliFailureReason,
    pub cancellation_origin: CliCancellationOrigin,
    pub state: CliFallbackState,
    pub dispatch_attempt_id: Option<String>,
    pub dispatch_count: u32,
    pub revision: u64,
    pub request_id: Option<String>,
    pub cancel_requested: bool,
    pub response_observed: bool,
    pub terminal_evidence: Option<String>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliFallbackClaim {
    pub record: CliFallbackRecord,
    /// Only the transaction which first changes queued to starting may send.
    pub dispatch_permitted: bool,
    pub dispatch_attempt_id: Option<String>,
    pub task: String,
    pub thinking_profile_id: String,
    pub root_task_id: String,
    pub source_attempt_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliFallbackPage {
    pub records: Vec<CliFallbackRecord>,
    pub next_cursor: Option<String>,
}

/// Facts supplied by the trusted adapter, never by arbitrary model output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CliFallbackObservation {
    Dispatched {
        request_id: String,
    },
    Unknown,
    ResponseObserved,
    CancelRequested,
    /// The adapter confirms it never sent this exact dispatch. An unknown
    /// response, local worker exit, or timeout must not use this variant.
    Busy {
        evidence: String,
    },
    /// Trusted Service preflight proved no request was created for this send.
    ProfileUnavailable {
        evidence: String,
    },
}

/// These require authoritative evidence from the adapter/coordinator. A local
/// worker exit or cancellation receipt cannot establish either resolution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliFallbackResolution {
    ConfirmedNotStarted,
    VerifiedTerminal,
}

pub(crate) fn init_cli_fallback_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS wc_cli_fallbacks (
            fallback_id TEXT PRIMARY KEY,
            owner_principal_kind TEXT NOT NULL,
            owner_principal_digest TEXT NOT NULL,
            root_task_id TEXT NOT NULL REFERENCES wc_agent_tasks(task_id),
            source_task_id TEXT NOT NULL REFERENCES wc_agent_tasks(task_id),
            source_attempt_id TEXT NOT NULL UNIQUE,
            project TEXT NOT NULL,
            source_job_id TEXT NOT NULL,
            source_request_id TEXT NOT NULL,
            client_id TEXT NOT NULL,
            instruction TEXT NOT NULL,
            thinking_profile_id TEXT NOT NULL,
            failure_reason TEXT NOT NULL,
            cancellation_origin TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN
                ('queued','starting','running','unknown','blocked','cancelled',
                 'confirmed_not_started','verified_terminal')),
            dispatch_attempt_id TEXT UNIQUE,
            dispatch_count INTEGER NOT NULL DEFAULT 0 CHECK(dispatch_count BETWEEN 0 AND 4),
            revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
            request_id TEXT,
            cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
            response_observed INTEGER NOT NULL DEFAULT 0 CHECK(response_observed IN (0,1)),
            terminal_evidence TEXT,
            created_at_unix_ms INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL,
            CHECK(state NOT IN ('starting','running','unknown') OR dispatch_attempt_id IS NOT NULL),
            CHECK(state NOT IN ('confirmed_not_started','verified_terminal') OR terminal_evidence IS NOT NULL)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_cli_fallback_root_slot
            ON wc_cli_fallbacks(root_task_id)
            WHERE state IN ('starting','running','unknown');
         CREATE INDEX IF NOT EXISTS idx_cli_fallback_owner_root
            ON wc_cli_fallbacks(owner_principal_kind,owner_principal_digest,root_task_id,created_at_unix_ms,fallback_id);
         CREATE INDEX IF NOT EXISTS idx_cli_fallback_candidate_scan
            ON wc_cli_fallbacks(owner_principal_kind,owner_principal_digest,project);",
    )?;
    crate::agent_host_dispatch::init_agent_host_dispatch_tables(conn)?;
    Ok(())
}

const SELECT_RECORD: &str = "SELECT fallback_id,root_task_id,source_task_id,source_attempt_id,
    project,source_job_id,source_request_id,client_id,instruction,thinking_profile_id,
    failure_reason,cancellation_origin,state,dispatch_attempt_id,request_id,
    cancel_requested,response_observed,terminal_evidence,created_at_unix_ms,updated_at_unix_ms,dispatch_count,revision
    FROM wc_cli_fallbacks";

fn enum_column<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_value(serde_json::Value::String(value)).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CliFallbackRecord> {
    Ok(CliFallbackRecord {
        fallback_id: row.get(0)?,
        root_task_id: row.get(1)?,
        source_task_id: row.get(2)?,
        source_attempt_id: row.get(3)?,
        project: row.get(4)?,
        source_job_id: row.get(5)?,
        source_request_id: row.get(6)?,
        client_id: row.get(7)?,
        instruction: row.get(8)?,
        thinking_profile_id: row.get(9)?,
        failure_reason: enum_column(row, 10)?,
        cancellation_origin: enum_column(row, 11)?,
        state: enum_column(row, 12)?,
        dispatch_attempt_id: row.get(13)?,
        request_id: row.get(14)?,
        cancel_requested: row.get(15)?,
        response_observed: row.get(16)?,
        terminal_evidence: row.get(17)?,
        created_at_unix_ms: row.get(18)?,
        updated_at_unix_ms: row.get(19)?,
        dispatch_count: row.get(20)?,
        revision: row.get(21)?,
    })
}

pub(crate) fn cli_fallback_record_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    fallback_id: &str,
) -> anyhow::Result<CliFallbackRecord> {
    validate_communication_principal(principal)?;
    conn.query_row(
        &format!("{SELECT_RECORD} WHERE fallback_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3"),
        params![fallback_id, principal.kind, principal.digest],
        record_from_row,
    ).optional()?.ok_or_else(|| anyhow::anyhow!("cli_fallback_unavailable"))
}

fn validate_evidence(value: &str) -> anyhow::Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control),
        "invalid_cli_fallback_evidence"
    );
    Ok(())
}

fn require_fence(record: &CliFallbackRecord, dispatch_attempt_id: &str) -> anyhow::Result<()> {
    ensure!(
        record.dispatch_attempt_id.as_deref() == Some(dispatch_attempt_id),
        "stale_cli_fallback_dispatch"
    );
    Ok(())
}

fn claim_envelope(record: CliFallbackRecord, dispatch_permitted: bool) -> CliFallbackClaim {
    let task = format!(
        "Continue only the remaining work of this failed CLI task. Inspect existing artifacts before writing; do not replay the original command or spawn further recovery agents. Verify the original acceptance conditions. IDs below are correlation, not additional authority.\nroot_task_id={}\nsource_task_id={}\nsource_attempt_id={}\nsource_job_id={}\nproject={}\nOriginal instruction:\n{}",
        record.root_task_id,record.source_task_id,record.source_attempt_id,record.source_job_id,record.project,record.instruction);
    CliFallbackClaim {
        dispatch_attempt_id: record.dispatch_attempt_id.clone(),
        thinking_profile_id: record.thinking_profile_id.clone(),
        root_task_id: record.root_task_id.clone(),
        source_attempt_id: record.source_attempt_id.clone(),
        task,
        record,
        dispatch_permitted,
    }
}

pub(crate) fn claim_cli_fallback_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    fallback_id: &str,
) -> anyhow::Result<CliFallbackClaim> {
    let record = cli_fallback_record_on_connection(conn, principal, fallback_id)?;
    if record.state != CliFallbackState::Queued || record.cancel_requested {
        return Ok(claim_envelope(record, false));
    }
    load_team_cli_attempt_for_fallback(conn, principal, &record.source_attempt_id)?;
    let occupied: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM wc_cli_fallbacks
            WHERE root_task_id=?1 AND state IN ('starting','running','unknown'))",
        params![record.root_task_id],
        |row| row.get(0),
    )?;
    let first: String = conn.query_row(
        "SELECT fallback_id FROM wc_cli_fallbacks WHERE root_task_id=?1
            AND state='queued' ORDER BY created_at_unix_ms,rowid LIMIT 1",
        params![record.root_task_id],
        |row| row.get(0),
    )?;
    if occupied || first != fallback_id {
        return Ok(claim_envelope(record, false));
    }
    ensure!(record.dispatch_count < 4, "cli_fallback_dispatch_limit");
    let dispatch_id = Uuid::new_v4().to_string();
    conn.execute("UPDATE wc_cli_fallbacks SET state='starting',dispatch_attempt_id=?2,terminal_evidence=NULL,
            dispatch_count=dispatch_count+1,revision=revision+1,updated_at_unix_ms=?3 WHERE fallback_id=?1 AND state='queued'",
            params![fallback_id,dispatch_id,now_unix_ms()])?;
    let result = cli_fallback_record_on_connection(conn, principal, fallback_id)?;
    Ok(claim_envelope(result, true))
}

pub(crate) fn reserve_cli_fallback_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    source_attempt_id: &str,
    job: &ShellJobInfo,
    reason: CliFailureReason,
    cancellation_origin: CliCancellationOrigin,
) -> anyhow::Result<CliFallbackRecord> {
    validate_communication_principal(principal)?;
    let existing = conn.query_row(
            &format!("{SELECT_RECORD} WHERE source_attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3"),
            params![source_attempt_id,principal.kind,principal.digest],record_from_row).optional()?;
    if let Some(record) = existing {
        ensure!(
            record.source_job_id == job.job_id
                && Some(record.project.as_str()) == job.project_id.as_deref()
                && record.client_id == job.client_id
                && Some(record.source_request_id.as_str()) == job.request_id.as_deref(),
            "cli_fallback_source_job_mismatch"
        );
        return Ok(record);
    }
    // This loader is the sole source of team/root/task eligibility. It
    // checks task ownership, cancellation and terminal state in this conn.
    let source = load_team_cli_attempt_for_fallback(conn, principal, source_attempt_id)?;
    ensure!(
        source.job_id.as_deref() == Some(job.job_id.as_str())
            && job.project_id.as_deref() == Some(source.project_id.as_str()),
        "cli_fallback_source_job_mismatch"
    );
    ensure!(
        cli_fallback_eligibility(job, reason, cancellation_origin).is_eligible(),
        "cli_fallback_source_not_eligible"
    );
    ensure!(
        !source.fallback_thinking_profile_id.trim().is_empty(),
        "cli_fallback_profile_required"
    );
    let reason = serde_json::to_value(reason)?;
    let origin = serde_json::to_value(cancellation_origin)?;
    let fallback_id = format!("wc_cli_fallback_{}", Uuid::new_v4().simple());
    let now = now_unix_ms();
    conn.execute("INSERT INTO wc_cli_fallbacks(fallback_id,owner_principal_kind,owner_principal_digest,
            root_task_id,source_task_id,source_attempt_id,project,source_job_id,source_request_id,client_id,
            instruction,thinking_profile_id,failure_reason,cancellation_origin,state,created_at_unix_ms,updated_at_unix_ms)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'queued',?15,?15)",
            params![fallback_id,principal.kind,principal.digest,source.root_task_id,source.source_task_id,
                source_attempt_id,source.project_id,job.job_id,job.request_id,job.client_id,source.instruction,
                source.fallback_thinking_profile_id,reason.as_str(),origin.as_str(),now])?;
    let result = cli_fallback_record_on_connection(conn, principal, &fallback_id)?;
    Ok(result)
}

impl Database {
    /// The caller obtains `job` from its authorized Runner registry and supplies
    /// classified evidence, never model-selected or stdout-derived authority.
    pub fn reserve_cli_fallback(
        &self,
        principal: &CommunicationPrincipal,
        source_attempt_id: &str,
        job: &ShellJobInfo,
        reason: CliFailureReason,
        cancellation_origin: CliCancellationOrigin,
    ) -> anyhow::Result<CliFallbackRecord> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = reserve_cli_fallback_on_connection(
            &tx,
            principal,
            source_attempt_id,
            job,
            reason,
            cancellation_origin,
        )?;
        tx.commit()?;
        Ok(result)
    }

    /// Claims the oldest queued recovery only when the independent root slot is
    /// free. Repeating this call never authorizes a second uncertain send.
    pub fn claim_cli_fallback(
        &self,
        principal: &CommunicationPrincipal,
        fallback_id: &str,
    ) -> anyhow::Result<CliFallbackClaim> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
        crate::agent_host_dispatch::require_legacy_host_queue_claim_on_connection(
            &tx,
            "fallback",
            fallback_id,
            &record.source_task_id,
        )?;
        let result = claim_cli_fallback_on_connection(&tx, principal, fallback_id)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn read_cli_fallback(
        &self,
        principal: &CommunicationPrincipal,
        fallback_id: &str,
    ) -> anyhow::Result<CliFallbackRecord> {
        cli_fallback_record_on_connection(&self.conn.lock().unwrap(), principal, fallback_id)
    }

    pub fn list_cli_fallbacks(
        &self,
        principal: &CommunicationPrincipal,
        root_task_id: &str,
    ) -> anyhow::Result<Vec<CliFallbackRecord>> {
        validate_communication_principal(principal)?;
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(&format!(
            "{SELECT_RECORD} WHERE root_task_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3
             ORDER BY created_at_unix_ms,fallback_id LIMIT 100"))?;
        let records = statement.query_map(
            params![root_task_id, principal.kind, principal.digest],
            record_from_row,
        )?;
        Ok(records.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_project_cli_fallbacks(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<CliFallbackRecord>> {
        Ok(self
            .list_project_cli_fallbacks_page(principal, project, limit, None)?
            .records)
    }

    /// The opaque continuation scans a fixed insertion high-water mark. New
    /// records enter the next cycle; old unknown records cannot hide later roots.
    pub fn list_project_cli_fallbacks_page(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> anyhow::Result<CliFallbackPage> {
        validate_communication_principal(principal)?;
        ensure!(
            (1..=100).contains(&limit),
            "invalid_cli_fallback_list_limit"
        );
        let conn = self.conn.lock().unwrap();
        let scope = digest_text(
            "webcodex.cli-fallback.scan.v1",
            &serde_json::to_string(&(&principal.kind, &principal.digest, project))?,
        );
        let cursor = match cursor {
            Some(value) => CandidateScanCursor::parse(value, &scope)
                .ok_or_else(|| anyhow::anyhow!("invalid_cli_fallback_cursor"))?,
            None => CandidateScanCursor::new(scope, conn.query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM wc_cli_fallbacks WHERE project=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3 AND state IN ('queued','starting','running','unknown')",
                params![project, principal.kind, principal.digest], |row| row.get(0))?),
        };
        let mut statement = conn.prepare(&format!(
            "{SELECT_RECORD} WHERE project=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3
             AND state IN ('queued','starting','running','unknown') AND rowid>?4 AND rowid<=?5 ORDER BY rowid LIMIT ?6"))?;
        let mut records = statement
            .query_map(
                params![
                    project,
                    principal.kind,
                    principal.digest,
                    cursor.after_rowid,
                    cursor.through_rowid,
                    limit + 1
                ],
                record_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_cursor = if records.len() > limit {
            records.truncate(limit);
            let rowid: i64 = conn.query_row(
                "SELECT rowid FROM wc_cli_fallbacks WHERE fallback_id=?1",
                [&records[limit - 1].fallback_id],
                |row| row.get(0),
            )?;
            Some(cursor.next(rowid)?)
        } else {
            None
        };
        Ok(CliFallbackPage {
            records,
            next_cursor,
        })
    }

    /// Cancel every recovery for this owned root, including records beyond the
    /// bounded display page. Cancelled plans remain readable for this cleanup.
    pub fn cancel_root_cli_fallbacks(
        &self,
        principal: &CommunicationPrincipal,
        root_task_id: &str,
    ) -> anyhow::Result<usize> {
        validate_communication_principal(principal)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_team_plans p
             JOIN wc_agent_tasks t ON t.task_id=p.root_task_id
             WHERE p.root_task_id=?1 AND p.owner_principal_kind=?2 AND p.owner_principal_digest=?3
               AND t.owner_principal_kind=?2 AND t.owner_principal_digest=?3)",
            params![root_task_id, principal.kind, principal.digest],
            |row| row.get(0),
        )?;
        ensure!(owned, "cli_fallback_unavailable");
        let changed = tx.execute(
            "UPDATE wc_cli_fallbacks SET cancel_requested=1,
             state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END,
             revision=revision+1,updated_at_unix_ms=?4
             WHERE root_task_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3
               AND state IN ('queued','starting','running','unknown') AND cancel_requested=0",
            params![
                root_task_id,
                principal.kind,
                principal.digest,
                now_unix_ms()
            ],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    /// Cancellation of a queued request is conclusive; a dispatched request is
    /// retained until independent terminal or never-started evidence arrives.
    pub fn cancel_cli_fallback(
        &self,
        principal: &CommunicationPrincipal,
        fallback_id: &str,
    ) -> anyhow::Result<CliFallbackRecord> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
        if !record.state.terminal() && !record.cancel_requested {
            tx.execute(
                "UPDATE wc_cli_fallbacks SET cancel_requested=1,
                state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END,
                revision=revision+1,updated_at_unix_ms=?2 WHERE fallback_id=?1",
                params![fallback_id, now_unix_ms()],
            )?;
        }
        let result = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn record_cli_fallback_observation(
        &self,
        principal: &CommunicationPrincipal,
        fallback_id: &str,
        dispatch_attempt_id: &str,
        observation: CliFallbackObservation,
    ) -> anyhow::Result<CliFallbackRecord> {
        let no_start = match &observation {
            CliFallbackObservation::Busy { evidence }
            | CliFallbackObservation::ProfileUnavailable { evidence } => Some(evidence.clone()),
            _ => None,
        };
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
        require_fence(&record, dispatch_attempt_id)?;
        if record.state.terminal() {
            return Ok(record);
        }
        ensure!(record.state.holds_slot(), "cli_fallback_not_dispatched");
        if let CliFallbackObservation::ProfileUnavailable { evidence } = &observation {
            ensure!(
                profile_unavailable_evidence(evidence)
                    && record.state == CliFallbackState::Starting
                    && record.request_id.is_none()
                    && !record.response_observed
                    && record.dispatch_count > 0,
                "cli_fallback_not_proven_unstarted"
            );
            let state = if record.cancel_requested {
                CliFallbackState::Cancelled
            } else {
                CliFallbackState::Queued
            };
            tx.execute("UPDATE wc_cli_fallbacks SET state=?2,dispatch_attempt_id=NULL,
                dispatch_count=dispatch_count-1,terminal_evidence=?3,revision=revision+1,updated_at_unix_ms=?4
                WHERE fallback_id=?1", params![fallback_id,state.as_str(),evidence,now_unix_ms()])?;
            let result = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
            if let Some(code) = no_start.as_deref() {
                crate::agent_host_dispatch::record_host_queue_no_start_on_connection(
                    &tx,
                    "fallback",
                    fallback_id,
                    dispatch_attempt_id,
                    code,
                )?;
            }
            tx.commit()?;
            return Ok(result);
        }
        if let CliFallbackObservation::Busy { evidence } = &observation {
            validate_evidence(evidence)?;
            ensure!(
                record.state == CliFallbackState::Starting
                    && record.request_id.is_none()
                    && !record.response_observed,
                "cli_fallback_not_proven_unstarted"
            );
            let state = if record.cancel_requested {
                CliFallbackState::Cancelled
            } else if record.dispatch_count >= 4 {
                CliFallbackState::Blocked
            } else {
                CliFallbackState::Queued
            };
            tx.execute("UPDATE wc_cli_fallbacks SET state=?2,dispatch_attempt_id=NULL,
                terminal_evidence=?3,revision=revision+1,updated_at_unix_ms=?4 WHERE fallback_id=?1",
                params![fallback_id,state.as_str(),evidence,now_unix_ms()])?;
            let result = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
            if let Some(code) = no_start.as_deref() {
                crate::agent_host_dispatch::record_host_queue_no_start_on_connection(
                    &tx,
                    "fallback",
                    fallback_id,
                    dispatch_attempt_id,
                    code,
                )?;
            }
            tx.commit()?;
            return Ok(result);
        }
        let (state, request_id, cancelled, observed) = match observation {
            CliFallbackObservation::Dispatched { request_id } => {
                validate_evidence(&request_id)?;
                ensure!(
                    record
                        .request_id
                        .as_ref()
                        .is_none_or(|known| known == &request_id),
                    "cli_fallback_request_conflict"
                );
                (
                    CliFallbackState::Running,
                    Some(request_id),
                    record.cancel_requested,
                    record.response_observed,
                )
            }
            CliFallbackObservation::Unknown => (
                CliFallbackState::Unknown,
                record.request_id.clone(),
                record.cancel_requested,
                record.response_observed,
            ),
            CliFallbackObservation::ResponseObserved => (
                record.state,
                record.request_id.clone(),
                record.cancel_requested,
                true,
            ),
            CliFallbackObservation::CancelRequested => (
                record.state,
                record.request_id.clone(),
                true,
                record.response_observed,
            ),
            CliFallbackObservation::Busy { .. }
            | CliFallbackObservation::ProfileUnavailable { .. } => {
                unreachable!("handled before ordinary observations")
            }
        };
        if record.state == state
            && record.request_id == request_id
            && record.cancel_requested == cancelled
            && record.response_observed == observed
        {
            return Ok(record);
        }
        tx.execute(
            "UPDATE wc_cli_fallbacks SET state=?2,request_id=?3,cancel_requested=?4,
            response_observed=?5,revision=revision+1,updated_at_unix_ms=?6 WHERE fallback_id=?1",
            params![
                fallback_id,
                state.as_str(),
                request_id,
                cancelled,
                observed,
                now_unix_ms()
            ],
        )?;
        let result = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn resolve_cli_fallback(
        &self,
        principal: &CommunicationPrincipal,
        fallback_id: &str,
        dispatch_attempt_id: &str,
        resolution: CliFallbackResolution,
        evidence: &str,
    ) -> anyhow::Result<CliFallbackRecord> {
        validate_evidence(evidence)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
        require_fence(&record, dispatch_attempt_id)?;
        let state = match resolution {
            CliFallbackResolution::ConfirmedNotStarted => CliFallbackState::ConfirmedNotStarted,
            CliFallbackResolution::VerifiedTerminal => CliFallbackState::VerifiedTerminal,
        };
        if record.state.terminal() {
            ensure!(
                record.state == state && record.terminal_evidence.as_deref() == Some(evidence),
                "cli_fallback_resolution_conflict"
            );
            return Ok(record);
        }
        ensure!(record.state.holds_slot(), "cli_fallback_not_dispatched");
        if resolution == CliFallbackResolution::ConfirmedNotStarted {
            ensure!(
                record.request_id.is_none() && !record.response_observed,
                "cli_fallback_already_dispatched"
            );
        }
        tx.execute("UPDATE wc_cli_fallbacks SET state=?2,terminal_evidence=?3,revision=revision+1,updated_at_unix_ms=?4 WHERE fallback_id=?1",
            params![fallback_id,state.as_str(),evidence,now_unix_ms()])?;
        let result = cli_fallback_record_on_connection(&tx, principal, fallback_id)?;
        crate::agent_host_dispatch::record_host_queue_resolution_on_connection(
            &tx,
            "fallback",
            fallback_id,
            dispatch_attempt_id,
            state == CliFallbackState::VerifiedTerminal,
            evidence,
        )?;
        tx.commit()?;
        Ok(result)
    }
}
