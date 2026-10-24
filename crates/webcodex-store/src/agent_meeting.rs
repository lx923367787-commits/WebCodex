//! Explicitly published, append-only meeting records. Membership grants access
//! only to these snapshots, never to an Agent's private filesystem or process.

use crate::agent_isolation::{
    agent_run_business_gate_on_connection, validate_agent_run_on_connection, VerifiedAgentRun,
};
use crate::Database;
use anyhow::{bail, ensure};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const MAX_AGENT_MEETING_CONTENT_BYTES: usize = 32 * 1024;
pub const MAX_AGENT_MEETING_ARTIFACT_BYTES: usize = 1024 * 1024;
pub const MAX_AGENT_MEETING_PAGE_SIZE: usize = 50;
const MAX_ROOM_BYTES: i64 = 64 * 1024 * 1024;
const MAX_ROOM_ENTRIES: i64 = 10_000;
const UNAVAILABLE: &str = "agent_meeting_unavailable";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMeetingKind {
    Report,
    Message,
    Review,
    Artifact,
}

impl AgentMeetingKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Message => "message",
            Self::Review => "review",
            Self::Artifact => "artifact",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentMeetingArtifactTarget {
    pub artifact_id: String,
    pub version: i64,
}

/// Publication accepts inline bytes only. There is deliberately no source path,
/// URL, author, timestamp, or caller-selected version in this input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewAgentMeetingEntry {
    pub kind: AgentMeetingKind,
    pub content: String,
    pub artifact_id: Option<String>,
    pub artifact_bytes: Option<Vec<u8>>,
    pub review_target: Option<AgentMeetingArtifactTarget>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentMeetingEntry {
    pub entry_id: String,
    pub room_id: String,
    pub sequence: i64,
    pub author_agent_id: String,
    pub author_run_id: String,
    pub author_generation: i64,
    pub kind: AgentMeetingKind,
    pub content: String,
    pub artifact: Option<AgentMeetingArtifactTarget>,
    pub review_target: Option<AgentMeetingArtifactTarget>,
    pub created_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentMeetingRoom {
    pub room_id: String,
    pub project: String,
    pub brief: String,
    pub created_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentMeetingPage {
    pub room: AgentMeetingRoom,
    pub entries: Vec<AgentMeetingEntry>,
    pub next_after_sequence: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentMeetingArtifact {
    pub room_id: String,
    pub artifact_id: String,
    pub version: i64,
    pub author_agent_id: String,
    pub bytes: Vec<u8>,
    pub created_at_unix_ms: i64,
}

pub(crate) fn init_agent_meeting_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS wc_agent_meeting_rooms (
            room_id TEXT PRIMARY KEY,
            project TEXT NOT NULL,
            brief TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS wc_agent_meeting_members (
            room_id TEXT NOT NULL REFERENCES wc_agent_meeting_rooms(room_id),
            agent_id TEXT NOT NULL REFERENCES wc_isolated_agents(agent_id),
            PRIMARY KEY(room_id, agent_id)
         );
         CREATE INDEX IF NOT EXISTS idx_agent_meeting_member
            ON wc_agent_meeting_members(agent_id, room_id);
         CREATE TABLE IF NOT EXISTS wc_agent_meeting_entries (
            entry_id TEXT PRIMARY KEY,
            room_id TEXT NOT NULL REFERENCES wc_agent_meeting_rooms(room_id),
            sequence INTEGER NOT NULL CHECK(sequence > 0),
            author_agent_id TEXT NOT NULL,
            author_run_id TEXT NOT NULL,
            author_generation INTEGER NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('report', 'message', 'review', 'artifact')),
            content TEXT NOT NULL,
            artifact_id TEXT,
            artifact_version INTEGER,
            review_artifact_id TEXT,
            review_artifact_version INTEGER,
            created_at_unix_ms INTEGER NOT NULL,
            idempotency_key TEXT NOT NULL,
            request_hash TEXT NOT NULL,
            UNIQUE(room_id, sequence),
            UNIQUE(room_id, author_agent_id, idempotency_key),
            FOREIGN KEY(room_id, author_agent_id)
                REFERENCES wc_agent_meeting_members(room_id, agent_id),
            CHECK((kind = 'artifact') = (artifact_id IS NOT NULL)),
            CHECK((artifact_id IS NULL) = (artifact_version IS NULL)),
            CHECK((kind = 'review') = (review_artifact_id IS NOT NULL)),
            CHECK((review_artifact_id IS NULL) = (review_artifact_version IS NULL))
         );
         CREATE TABLE IF NOT EXISTS wc_agent_meeting_artifacts (
            room_id TEXT NOT NULL REFERENCES wc_agent_meeting_rooms(room_id),
            artifact_id TEXT NOT NULL,
            version INTEGER NOT NULL CHECK(version > 0),
            author_agent_id TEXT NOT NULL,
            bytes BLOB NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            entry_id TEXT NOT NULL UNIQUE REFERENCES wc_agent_meeting_entries(entry_id),
            PRIMARY KEY(room_id, artifact_id, version),
            FOREIGN KEY(room_id, author_agent_id)
                REFERENCES wc_agent_meeting_members(room_id, agent_id)
         );
         CREATE TABLE IF NOT EXISTS wc_agent_meeting_audit (
            audit_id TEXT PRIMARY KEY,
            room_id TEXT NOT NULL REFERENCES wc_agent_meeting_rooms(room_id),
            entry_id TEXT,
            agent_id TEXT,
            run_id TEXT,
            generation INTEGER,
            event_kind TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TRIGGER IF NOT EXISTS agent_meeting_entries_no_update
         BEFORE UPDATE ON wc_agent_meeting_entries BEGIN
            SELECT RAISE(ABORT, 'agent_meeting_append_only'); END;
         CREATE TRIGGER IF NOT EXISTS agent_meeting_entries_no_delete
         BEFORE DELETE ON wc_agent_meeting_entries BEGIN
            SELECT RAISE(ABORT, 'agent_meeting_append_only'); END;
         CREATE TRIGGER IF NOT EXISTS agent_meeting_artifacts_no_update
         BEFORE UPDATE ON wc_agent_meeting_artifacts BEGIN
            SELECT RAISE(ABORT, 'agent_meeting_append_only'); END;
         CREATE TRIGGER IF NOT EXISTS agent_meeting_artifacts_no_delete
         BEFORE DELETE ON wc_agent_meeting_artifacts BEGIN
            SELECT RAISE(ABORT, 'agent_meeting_append_only'); END;",
    )?;
    Ok(())
}

impl Database {
    /// Trusted management operation. The runtime must not expose this operation
    /// through an ordinary Agent's tool dispatch.
    pub fn create_agent_meeting(
        &self,
        project: &str,
        participant_agent_ids: &[String],
        brief: &str,
    ) -> anyhow::Result<String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let room = create_agent_meeting_on_connection(&tx, project, participant_agent_ids, brief)?;
        tx.commit()?;
        Ok(room)
    }

    pub fn publish_agent_meeting_entry(
        &self,
        binding: &VerifiedAgentRun,
        room_id: &str,
        input: NewAgentMeetingEntry,
    ) -> anyhow::Result<AgentMeetingEntry> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry = publish_agent_meeting_entry_on_connection(&tx, binding, room_id, input)?;
        tx.commit()?;
        Ok(entry)
    }

    pub fn list_agent_meetings(
        &self,
        binding: &VerifiedAgentRun,
        limit: usize,
    ) -> anyhow::Result<Vec<AgentMeetingRoom>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        validate_agent_run_on_connection(&tx, binding).map_err(|_| anyhow::anyhow!(UNAVAILABLE))?;
        let mut statement = tx.prepare(
            "SELECT r.room_id, r.project, r.brief, r.created_at_unix_ms FROM wc_agent_meeting_rooms r
             JOIN wc_agent_meeting_members m ON m.room_id = r.room_id
             WHERE m.agent_id = ?1 AND r.project = ?2 ORDER BY r.created_at_unix_ms, r.room_id LIMIT ?3",
        )?;
        let rooms = statement
            .query_map(
                params![
                    binding.agent_id(),
                    binding.project(),
                    limit.clamp(1, MAX_AGENT_MEETING_PAGE_SIZE) as i64
                ],
                row_to_room,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rooms)
    }

    pub fn read_agent_meeting(
        &self,
        binding: &VerifiedAgentRun,
        room_id: &str,
        after_sequence: i64,
        limit: usize,
    ) -> anyhow::Result<AgentMeetingPage> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let room = authorize_room(&tx, binding, room_id)?;
        ensure!(after_sequence >= 0, "invalid_agent_meeting_cursor");
        let limit = limit.clamp(1, MAX_AGENT_MEETING_PAGE_SIZE);
        let mut statement = tx.prepare(&format!(
            "{ENTRY_SELECT} WHERE room_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3"
        ))?;
        let mut entries = statement
            .query_map(
                params![room_id, after_sequence, (limit + 1) as i64],
                row_to_entry,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let next_after_sequence = if has_more {
            entries.last().map(|entry| entry.sequence)
        } else {
            None
        };
        Ok(AgentMeetingPage {
            room,
            entries,
            next_after_sequence,
        })
    }

    pub fn read_agent_meeting_artifact(
        &self,
        binding: &VerifiedAgentRun,
        room_id: &str,
        artifact_id: &str,
        version: i64,
    ) -> anyhow::Result<AgentMeetingArtifact> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        authorize_room(&tx, binding, room_id)?;
        load_agent_meeting_artifact_on_connection(&tx, room_id, artifact_id, version)
    }
}

pub(crate) fn authorize_room(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    room_id: &str,
) -> anyhow::Result<AgentMeetingRoom> {
    // Meeting membership belongs to the logical Agent and survives run/task
    // handoff. Every access still requires its current active run and project.
    validate_agent_run_on_connection(conn, binding).map_err(|_| anyhow::anyhow!(UNAVAILABLE))?;
    let room = conn.query_row(
        "SELECT r.room_id, r.project, r.brief, r.created_at_unix_ms FROM wc_agent_meeting_rooms r
         JOIN wc_agent_meeting_members m ON m.room_id = r.room_id
         WHERE r.room_id = ?1 AND r.project = ?2 AND m.agent_id = ?3",
        params![room_id, binding.project(), binding.agent_id()], row_to_room,
    ).optional()?;
    room.ok_or_else(|| anyhow::anyhow!(UNAVAILABLE))
}

fn validate_publication(input: &NewAgentMeetingEntry) -> anyhow::Result<()> {
    ensure!(
        input.content.len() <= MAX_AGENT_MEETING_CONTENT_BYTES
            && !input.idempotency_key.is_empty()
            && input.idempotency_key.len() <= 128,
        "invalid_agent_meeting_input"
    );
    match input.kind {
        AgentMeetingKind::Artifact => {
            let Some(bytes) = &input.artifact_bytes else {
                bail!("agent_meeting_artifact_snapshot_required")
            };
            ensure!(
                !bytes.is_empty() && bytes.len() <= MAX_AGENT_MEETING_ARTIFACT_BYTES,
                "invalid_agent_meeting_artifact_size"
            );
            ensure!(
                !is_reference_only(bytes),
                "agent_meeting_artifact_reference_not_allowed"
            );
            ensure!(input.review_target.is_none(), "invalid_agent_meeting_input");
        }
        AgentMeetingKind::Review => {
            let Some(target) = &input.review_target else {
                bail!("agent_meeting_review_target_required")
            };
            ensure!(
                target.version > 0
                    && !target.artifact_id.is_empty()
                    && target.artifact_id.len() <= 128,
                "invalid_agent_meeting_review_target"
            );
            ensure!(
                input.artifact_id.is_none() && input.artifact_bytes.is_none(),
                "invalid_agent_meeting_input"
            );
            ensure!(
                !input.content.trim().is_empty(),
                "invalid_agent_meeting_input"
            );
        }
        AgentMeetingKind::Message | AgentMeetingKind::Report => {
            ensure!(
                input.artifact_id.is_none()
                    && input.artifact_bytes.is_none()
                    && input.review_target.is_none(),
                "invalid_agent_meeting_input"
            );
            ensure!(
                !input.content.trim().is_empty(),
                "invalid_agent_meeting_input"
            );
        }
    }
    ensure!(
        input
            .artifact_id
            .as_ref()
            .is_none_or(|id| !id.is_empty() && id.len() <= 128),
        "invalid_agent_meeting_artifact_id"
    );
    Ok(())
}

// These strings are data, and are never opened or resolved. Reject a publication
// that is merely a filesystem/URL pointer instead of a self-contained snapshot.
fn is_reference_only(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let text = text.trim();
    if text.contains('\n') || text.contains('\r') {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    text.starts_with('/')
        || text.starts_with('\\')
        || text.starts_with("~/")
        || text.starts_with("./")
        || text.starts_with("../")
        || (text.as_bytes().get(1) == Some(&b':')
            && text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
        || lower.starts_with("file:")
        || lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("smb:")
        || lower.starts_with("agent-private:")
}

fn row_to_room(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMeetingRoom> {
    Ok(AgentMeetingRoom {
        room_id: row.get(0)?,
        project: row.get(1)?,
        brief: row.get(2)?,
        created_at_unix_ms: row.get(3)?,
    })
}

const ENTRY_SELECT: &str = "SELECT entry_id, room_id, sequence, author_agent_id, author_run_id,
    author_generation, kind, content, artifact_id, artifact_version, review_artifact_id,
    review_artifact_version, created_at_unix_ms FROM wc_agent_meeting_entries";

pub(crate) fn load_entry(conn: &Connection, entry_id: &str) -> anyhow::Result<AgentMeetingEntry> {
    Ok(conn.query_row(
        &format!("{ENTRY_SELECT} WHERE entry_id = ?1"),
        [entry_id],
        row_to_entry,
    )?)
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMeetingEntry> {
    let kind: String = row.get(6)?;
    let kind = match kind.as_str() {
        "report" => AgentMeetingKind::Report,
        "message" => AgentMeetingKind::Message,
        "review" => AgentMeetingKind::Review,
        "artifact" => AgentMeetingKind::Artifact,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let artifact_id: Option<String> = row.get(8)?;
    let review_id: Option<String> = row.get(10)?;
    Ok(AgentMeetingEntry {
        entry_id: row.get(0)?,
        room_id: row.get(1)?,
        sequence: row.get(2)?,
        author_agent_id: row.get(3)?,
        author_run_id: row.get(4)?,
        author_generation: row.get(5)?,
        kind,
        content: row.get(7)?,
        artifact: artifact_id
            .map(|artifact_id| -> rusqlite::Result<_> {
                Ok(AgentMeetingArtifactTarget {
                    artifact_id,
                    version: row.get(9)?,
                })
            })
            .transpose()?,
        review_target: review_id
            .map(|artifact_id| -> rusqlite::Result<_> {
                Ok(AgentMeetingArtifactTarget {
                    artifact_id,
                    version: row.get(11)?,
                })
            })
            .transpose()?,
        created_at_unix_ms: row.get(12)?,
    })
}

pub(crate) fn load_agent_meeting_artifact_on_connection(
    conn: &Connection,
    room_id: &str,
    artifact_id: &str,
    version: i64,
) -> anyhow::Result<AgentMeetingArtifact> {
    let artifact = conn.query_row(
            "SELECT room_id, artifact_id, version, author_agent_id, bytes, created_at_unix_ms
             FROM wc_agent_meeting_artifacts WHERE room_id = ?1 AND artifact_id = ?2 AND version = ?3",
            params![room_id, artifact_id, version],
            |row| Ok(AgentMeetingArtifact {
                room_id: row.get(0)?, artifact_id: row.get(1)?, version: row.get(2)?,
                author_agent_id: row.get(3)?, bytes: row.get(4)?, created_at_unix_ms: row.get(5)?,
            }),
        ).optional()?;
    artifact.ok_or_else(|| anyhow::anyhow!(UNAVAILABLE))
}

pub(crate) fn create_agent_meeting_on_connection(
    conn: &Connection,
    project: &str,
    participant_agent_ids: &[String],
    brief: &str,
) -> anyhow::Result<String> {
    ensure!(
        !project.is_empty()
            && project.len() <= 1024
            && brief.len() <= MAX_AGENT_MEETING_CONTENT_BYTES,
        "invalid_agent_meeting_input"
    );
    ensure!(
        !participant_agent_ids.is_empty() && participant_agent_ids.len() <= 64,
        "invalid_agent_meeting_participants"
    );
    let participants: BTreeSet<_> = participant_agent_ids.iter().collect();
    for agent_id in participants.iter() {
        let allowed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_isolated_agents WHERE agent_id = ?1 AND project = ?2)",
            params![agent_id, project],
            |row| row.get(0),
        )?;
        ensure!(allowed, UNAVAILABLE);
    }
    let room_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM wc_agent_meeting_rooms", [], |row| {
            row.get(0)
        })?;
    ensure!(room_count < 1024, "agent_meeting_capacity_reached");
    let room_id = format!("wc_meeting_{}", Uuid::new_v4().simple());
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
            "INSERT INTO wc_agent_meeting_rooms(room_id, project, brief, created_at_unix_ms) VALUES (?1, ?2, ?3, ?4)",
            params![room_id, project, brief, now],
        )?;
    for agent_id in participants {
        conn.execute(
            "INSERT INTO wc_agent_meeting_members(room_id, agent_id) VALUES (?1, ?2)",
            params![room_id, agent_id],
        )?;
    }
    conn.execute(
        "INSERT INTO wc_agent_meeting_audit(audit_id, room_id, event_kind, created_at_unix_ms)
             VALUES (?1, ?2, 'create_room', ?3)",
        params![Uuid::new_v4().to_string(), room_id, now],
    )?;

    Ok(room_id)
}

/// Ordinary model publication retains the existing current-run/business checks.
pub(crate) fn publish_agent_meeting_entry_on_connection(
    conn: &Connection,
    binding: &VerifiedAgentRun,
    room_id: &str,
    input: NewAgentMeetingEntry,
) -> anyhow::Result<AgentMeetingEntry> {
    authorize_room(conn, binding, room_id)?;
    agent_run_business_gate_on_connection(conn, binding)?;
    publish_agent_meeting_entry_as_run_on_connection(
        conn,
        binding.project(),
        &webcodex_core::runner_protocol::AgentRunRef {
            agent_id: binding.agent_id().into(),
            agent_run_id: binding.run_id().into(),
            generation: binding.generation(),
        },
        room_id,
        input,
    )
}

/// Crate-only trusted terminal delivery. Caller must establish its authenticated
/// exact execution proof before selecting this historical author. No model input
/// may invoke this helper or select an author; revoked terminal runs are allowed.
pub(crate) fn publish_agent_meeting_entry_as_run_on_connection(
    conn: &Connection,
    project: &str,
    author: &webcodex_core::runner_protocol::AgentRunRef,
    room_id: &str,
    input: NewAgentMeetingEntry,
) -> anyhow::Result<AgentMeetingEntry> {
    let allowed:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_agent_runs run JOIN wc_isolated_agents agent ON agent.agent_id=run.agent_id JOIN wc_agent_meeting_members m ON m.agent_id=agent.agent_id JOIN wc_agent_meeting_rooms room ON room.room_id=m.room_id WHERE run.run_id=?1 AND run.agent_id=?2 AND run.generation=?3 AND agent.project=?4 AND room.project=?4 AND room.room_id=?5)",params![author.agent_run_id,author.agent_id,author.generation,project,room_id],|r|r.get(0))?;
    ensure!(allowed, UNAVAILABLE);
    validate_publication(&input)?;
    let request_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&input)?));
    let replay: Option<(String, String)> = conn
        .query_row(
            "SELECT entry_id, request_hash FROM wc_agent_meeting_entries
             WHERE room_id = ?1 AND author_agent_id = ?2 AND idempotency_key = ?3",
            params![room_id, author.agent_id.as_str(), input.idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((entry_id, previous_hash)) = replay {
        ensure!(
            previous_hash == request_hash,
            "agent_meeting_idempotency_conflict"
        );
        return load_entry(conn, &entry_id);
    }

    let artifact = if input.kind == AgentMeetingKind::Artifact {
        if let Some(artifact_id) = &input.artifact_id {
            let previous: Option<(String, i64)> = conn
                .query_row(
                    "SELECT author_agent_id, version FROM wc_agent_meeting_artifacts
                     WHERE room_id = ?1 AND artifact_id = ?2 ORDER BY version DESC LIMIT 1",
                    params![room_id, artifact_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((previous_author, version)) = previous else {
                bail!(UNAVAILABLE)
            };
            ensure!(previous_author == author.agent_id.as_str(), UNAVAILABLE);
            Some(AgentMeetingArtifactTarget {
                artifact_id: artifact_id.clone(),
                version: version + 1,
            })
        } else {
            Some(AgentMeetingArtifactTarget {
                artifact_id: format!("wc_martifact_{}", Uuid::new_v4().simple()),
                version: 1,
            })
        }
    } else {
        None
    };
    if let Some(target) = &input.review_target {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_meeting_artifacts
                 WHERE room_id = ?1 AND artifact_id = ?2 AND version = ?3)",
            params![room_id, target.artifact_id, target.version],
            |row| row.get(0),
        )?;
        ensure!(exists, UNAVAILABLE);
    }
    let (entry_count, content_bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(CAST(content AS BLOB))), 0)
             FROM wc_agent_meeting_entries WHERE room_id = ?1",
        [room_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let artifact_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(length(bytes)), 0) FROM wc_agent_meeting_artifacts WHERE room_id = ?1",
        [room_id],
        |row| row.get(0),
    )?;
    let added_bytes = input.content.len() + input.artifact_bytes.as_ref().map_or(0, Vec::len);
    ensure!(
        entry_count < MAX_ROOM_ENTRIES
            && content_bytes + artifact_bytes + added_bytes as i64 <= MAX_ROOM_BYTES,
        "agent_meeting_capacity_reached"
    );
    let entry = AgentMeetingEntry {
        entry_id: format!("wc_mentry_{}", Uuid::new_v4().simple()),
        room_id: room_id.to_string(),
        sequence: entry_count + 1,
        author_agent_id: author.agent_id.as_str().to_string(),
        author_run_id: author.agent_run_id.as_str().to_string(),
        author_generation: author.generation,
        kind: input.kind,
        content: input.content,
        artifact,
        review_target: input.review_target,
        created_at_unix_ms: chrono::Utc::now().timestamp_millis(),
    };
    conn.execute(
            "INSERT INTO wc_agent_meeting_entries(entry_id, room_id, sequence, author_agent_id,
             author_run_id, author_generation, kind, content, artifact_id, artifact_version,
             review_artifact_id, review_artifact_version, created_at_unix_ms, idempotency_key, request_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![entry.entry_id, room_id, entry.sequence, entry.author_agent_id, entry.author_run_id,
                entry.author_generation, entry.kind.as_str(), entry.content,
                entry.artifact.as_ref().map(|a| &a.artifact_id), entry.artifact.as_ref().map(|a| a.version),
                entry.review_target.as_ref().map(|a| &a.artifact_id), entry.review_target.as_ref().map(|a| a.version),
                entry.created_at_unix_ms, input.idempotency_key, request_hash],
        )?;
    if let Some(target) = &entry.artifact {
        conn.execute(
            "INSERT INTO wc_agent_meeting_artifacts(room_id, artifact_id, version, author_agent_id,
                 bytes, created_at_unix_ms, entry_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                room_id,
                target.artifact_id,
                target.version,
                author.agent_id.as_str(),
                input.artifact_bytes.as_deref().unwrap(),
                entry.created_at_unix_ms,
                entry.entry_id
            ],
        )?;
    }
    conn.execute(
        "INSERT INTO wc_agent_meeting_audit(audit_id, room_id, entry_id, agent_id, run_id,
             generation, event_kind, created_at_unix_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            Uuid::new_v4().to_string(),
            room_id,
            entry.entry_id,
            author.agent_id.as_str(),
            author.agent_run_id.as_str(),
            author.generation,
            entry.kind.as_str(),
            entry.created_at_unix_ms
        ],
    )?;

    Ok(entry)
}
