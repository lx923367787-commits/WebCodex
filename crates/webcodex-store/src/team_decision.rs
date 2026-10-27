//! Task-scoped evidence review. People are authors, never units of support.
//! Source semantics are model-reviewed; immutable publication checks are separate.
use crate::agent_isolation::{
    authorize_agent_assignment_on_connection, enrollment_projection_on_connection,
};
use crate::communication::{now_unix_ms, store_error};
use crate::{
    AgentAssignmentOperation, AgentEnrollmentPurpose, CommunicationStoreError, Database,
    VerifiedAgentRun,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, CommunicationStoreError>;
const MAX_ISSUES: usize = 32;
const MAX_EVIDENCE: usize = 128;
const MAX_EVENTS: i64 = 512;
const MAX_RECORD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Fact,
    Choice,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStance {
    Support,
    Oppose,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceDisposition {
    Pending,
    Verified,
    Invalid,
    Excluded,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Verified,
    Insufficient,
    Disputed,
    Selected,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceOrigin {
    pub locator: String,
    pub version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSource {
    pub origin: EvidenceOrigin,
    pub location: String,
    pub excerpt: String,
    pub upstream: Option<EvidenceOrigin>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceArtifact {
    pub room_id: String,
    pub artifact_id: String,
    pub version: i64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReviewInput {
    pub disposition: EvidenceDisposition,
    /// Records directness, reproducibility, applicability and citation checks.
    pub rationale: String,
    /// Reviewed origin; differing labels require an explicit merge below.
    pub origin: Option<EvidenceOrigin>,
    pub same_origin_as: Option<String>,
    /// Only an independently reviewed experimental unit can split one source.
    pub independent_unit: Option<String>,
    pub independence_rationale: Option<String>,
    /// Why a valid contrary observation does not defeat the scoped conclusion.
    pub resolution: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TeamDecisionRequest {
    Open {
        root_task_id: String,
        question: String,
        context: String,
        kind: DecisionKind,
        #[serde(default)]
        required_for_success: bool,
        idempotency_key: String,
    },
    Submit {
        root_task_id: String,
        issue_id: String,
        expected_revision: i64,
        stance: EvidenceStance,
        claim: String,
        source: Option<EvidenceSource>,
        artifact: Option<EvidenceArtifact>,
        idempotency_key: String,
    },
    Review {
        root_task_id: String,
        issue_id: String,
        expected_revision: i64,
        evidence_id: String,
        review: EvidenceReviewInput,
        idempotency_key: String,
    },
    Read {
        root_task_id: String,
        issue_id: Option<String>,
        #[serde(default)]
        section: DecisionSection,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_limit")]
        limit: usize,
    },
    Conclude {
        root_task_id: String,
        issue_id: String,
        expected_revision: i64,
        status: DecisionStatus,
        selected_stance: EvidenceStance,
        conclusion: String,
        rationale: String,
        #[serde(default)]
        factual_issue_ids: Vec<String>,
        idempotency_key: String,
    },
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSection {
    #[default]
    Summary,
    Evidence,
    History,
}
fn page_limit() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewedEvidence {
    input: EvidenceReviewInput,
    reviewer_agent_id: String,
    reviewer_run_id: String,
    revision: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Evidence {
    evidence_id: String,
    author_agent_id: String,
    author_run_id: String,
    stance: EvidenceStance,
    claim: String,
    source: Option<EvidenceSource>,
    artifact: Option<EvidenceArtifact>,
    /// This is only a byte/version/quotation check, not semantic verification.
    publication_checked: bool,
    review: Option<ReviewedEvidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionConclusion {
    pub revision: i64,
    pub status: DecisionStatus,
    pub selected_stance: EvidenceStance,
    pub conclusion: String,
    pub rationale: String,
    pub reviewer_agent_id: String,
    pub reviewer_run_id: String,
    pub factual_revisions: BTreeMap<String, i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Issue {
    issue_id: String,
    root_task_id: String,
    question: String,
    context: String,
    kind: DecisionKind,
    required_for_success: bool,
    revision: i64,
    evidence: Vec<Evidence>,
    conclusion: Option<DecisionConclusion>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceCounts {
    pub submitted: usize,
    pub independent_groups: usize,
    pub support_groups: usize,
    pub oppose_groups: usize,
    pub conflicting_groups: usize,
    pub duplicate_references: usize,
    pub pending: usize,
    pub excluded_or_invalid: usize,
    pub unresolved_support_groups: usize,
    pub unresolved_oppose_groups: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionSummary {
    pub issue_id: String,
    pub question: String,
    pub context: String,
    pub kind: DecisionKind,
    pub revision: i64,
    pub required_for_success: bool,
    pub counts: EvidenceCounts,
    /// Null after any evidence/review change, or when a dependent fact changes.
    pub current_conclusion: Option<DecisionConclusion>,
    pub last_conclusion: Option<DecisionConclusion>,
}

fn error(code: &'static str) -> CommunicationStoreError {
    CommunicationStoreError::new(
        code,
        "Team evidence review could not complete this operation",
    )
}
fn check(ok: bool, code: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(error(code))
    }
}
fn text(value: &str, max: usize) -> Result<()> {
    check(
        !value.trim().is_empty()
            && value.len() <= max
            && !value
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
        "team_decision_invalid_input",
    )
}
fn origin_valid(origin: &EvidenceOrigin) -> Result<()> {
    text(&origin.locator, 2048)?;
    text(&origin.version, 256)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|_| error("team_decision_store_invalid"))
}
fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|_| error("team_decision_store_invalid"))
}

pub(crate) fn init_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_team_decisions (
      issue_id TEXT PRIMARY KEY,root_task_id TEXT NOT NULL REFERENCES wc_agent_tasks(task_id),
      manager_agent_id TEXT NOT NULL,project TEXT NOT NULL,record_json TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS idx_team_decisions_root ON wc_team_decisions(root_task_id);
      CREATE TABLE IF NOT EXISTS wc_team_decision_events (
      sequence INTEGER PRIMARY KEY AUTOINCREMENT,issue_id TEXT NOT NULL REFERENCES wc_team_decisions(issue_id),
      root_task_id TEXT NOT NULL,author_agent_id TEXT NOT NULL,author_run_id TEXT NOT NULL,
      revision INTEGER NOT NULL,key_hash TEXT NOT NULL,request_hash TEXT NOT NULL,
      request_json TEXT NOT NULL,response_json TEXT NOT NULL,created_at_unix_ms INTEGER NOT NULL,
      UNIQUE(root_task_id,author_agent_id,key_hash),UNIQUE(issue_id,revision));
      CREATE TABLE IF NOT EXISTS wc_team_decision_completions (
      task_id TEXT PRIMARY KEY REFERENCES wc_agent_tasks(task_id),summary_json TEXT NOT NULL);
      CREATE TRIGGER IF NOT EXISTS team_decision_events_no_update BEFORE UPDATE ON wc_team_decision_events
      BEGIN SELECT RAISE(ABORT,'team decision history is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS team_decision_events_no_delete BEFORE DELETE ON wc_team_decision_events
      BEGIN SELECT RAISE(ABORT,'team decision history is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS team_decision_completion_no_update BEFORE UPDATE ON wc_team_decision_completions
      BEGIN SELECT RAISE(ABORT,'team decision completion is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS team_decision_completion_no_delete BEFORE DELETE ON wc_team_decision_completions
      BEGIN SELECT RAISE(ABORT,'team decision completion is immutable'); END;")?;
    Ok(())
}

impl TeamDecisionRequest {
    fn root(&self) -> &str {
        match self {
            Self::Open { root_task_id, .. }
            | Self::Submit { root_task_id, .. }
            | Self::Review { root_task_id, .. }
            | Self::Read { root_task_id, .. }
            | Self::Conclude { root_task_id, .. } => root_task_id,
        }
    }
    pub fn is_read(&self) -> bool {
        matches!(self, Self::Read { .. })
    }
    fn issue_revision_key(&self) -> (Option<&str>, i64, &str) {
        match self {
            Self::Open {
                idempotency_key, ..
            } => (None, 0, idempotency_key),
            Self::Submit {
                issue_id,
                expected_revision,
                idempotency_key,
                ..
            }
            | Self::Review {
                issue_id,
                expected_revision,
                idempotency_key,
                ..
            }
            | Self::Conclude {
                issue_id,
                expected_revision,
                idempotency_key,
                ..
            } => (Some(issue_id), *expected_revision, idempotency_key),
            Self::Read { .. } => (None, 0, ""),
        }
    }
}

/// Reuses the exact enrolled task and explicit parent enrollment, never account-wide access.
fn authorize(
    conn: &Connection,
    run: &VerifiedAgentRun,
    request: &TeamDecisionRequest,
) -> Result<String> {
    let own = enrollment_projection_on_connection(conn, run).map_err(|e| error(e.code()))?;
    let auth = authorize_agent_assignment_on_connection(
        conn,
        run,
        &own.task_id,
        if request.is_read() {
            AgentAssignmentOperation::Read
        } else {
            AgentAssignmentOperation::TaskStart
        },
    )
    .map_err(|e| error(e.code()))?;
    let manager_only = !matches!(
        request,
        TeamDecisionRequest::Read { .. } | TeamDecisionRequest::Submit { .. }
    );
    if auth.purpose == AgentEnrollmentPurpose::Manager && auth.task_id == request.root() {
        return Ok(run.agent_id().into());
    }
    check(
        !manager_only && auth.purpose == AgentEnrollmentPurpose::Member,
        "team_decision_denied",
    )?;
    let parent: Option<String> = conn
        .query_row(
            "SELECT parent.agent_id FROM wc_agent_enrollments child
      JOIN wc_agent_enrollments parent ON parent.agent_id=child.source_agent_id
      WHERE child.enrollment_id=?1 AND child.state='claimed' AND parent.task_id=?2
      AND parent.purpose='manager' AND parent.state='claimed'
      AND parent.owner_user=child.owner_user AND parent.project=child.project",
            params![own.enrollment_id, request.root()],
            |r| r.get(0),
        )
        .optional()
        .map_err(store_error)?;
    let parent = parent.ok_or_else(|| error("team_decision_denied"))?;
    if !request.is_read() {
        crate::agent_isolation::require_enrollment_public_task_on_connection(
            conn,
            run.owner_user(),
            run.project(),
            request.root(),
        )
        .map_err(|e| error(e.code()))?;
    }
    Ok(parent)
}

fn load(conn: &Connection, root: &str, id: &str) -> Result<Issue> {
    let raw: String = conn
        .query_row(
            "SELECT record_json FROM wc_team_decisions WHERE issue_id=?1 AND root_task_id=?2",
            params![id, root],
            |r| r.get(0),
        )
        .optional()
        .map_err(store_error)?
        .ok_or_else(|| error("team_decision_not_found"))?;
    decode(&raw)
}

/// Resolve reviewed provenance recursively. Cycles are rejected on review, not truncated.
fn group(
    issue: &Issue,
    evidence: &Evidence,
    seen: &mut BTreeSet<String>,
) -> Result<Option<String>> {
    check(
        seen.insert(evidence.evidence_id.clone()),
        "team_decision_origin_cycle",
    )?;
    let Some(review) = &evidence.review else {
        return Ok(None);
    };
    if review.input.disposition != EvidenceDisposition::Verified {
        return Ok(None);
    }
    if let Some(id) = &review.input.same_origin_as {
        let target = issue
            .evidence
            .iter()
            .find(|e| &e.evidence_id == id)
            .ok_or_else(|| error("team_decision_invalid_origin"))?;
        return group(issue, target, seen);
    }
    let Some(origin) = &review.input.origin else {
        return Ok(None);
    };
    Ok(Some(source_key(
        origin,
        review.input.independent_unit.as_deref(),
    )?))
}

fn source_key(origin: &EvidenceOrigin, unit: Option<&str>) -> Result<String> {
    let version = origin.version.trim();
    let digest = version.strip_prefix("sha256:").unwrap_or(version);
    let fingerprint = version.starts_with("sha256:")
        && digest.len() == 64
        && digest.bytes().all(|b| b.is_ascii_hexdigit());
    encode(&(
        if fingerprint {
            "sha256"
        } else {
            origin.locator.trim()
        },
        if fingerprint {
            digest.to_ascii_lowercase()
        } else {
            version.to_string()
        },
        unit.unwrap_or("").trim(),
    ))
}
fn representative(parents: &BTreeMap<String, String>, key: &str) -> String {
    let mut current = key;
    while let Some(parent) = parents.get(current) {
        if parent == current {
            break;
        }
        current = parent;
    }
    current.to_string()
}
fn group_parts(key: &str) -> Result<(String, String)> {
    let (locator, version, unit): (String, String, String) = decode(key)?;
    Ok((encode(&(locator, version, ""))?, unit))
}
/// Merging a source must merge *all* its citations, not just the selected author.
fn group_keys(issue: &Issue) -> Result<BTreeMap<String, Option<String>>> {
    let mut parents = BTreeMap::new();
    let mut keys = BTreeMap::new();
    for e in &issue.evidence {
        let key = group(issue, e, &mut BTreeSet::new())?;
        if let (Some(key), Some(review), Some(source)) = (&key, &e.review, &e.source) {
            let raw = source_key(
                review
                    .input
                    .origin
                    .as_ref()
                    .unwrap_or(source.upstream.as_ref().unwrap_or(&source.origin)),
                None,
            )?;
            let a = representative(&parents, &raw);
            let b = representative(&parents, &group_parts(key)?.0);
            if a != b {
                let (low, high) = if a < b { (a, b) } else { (b, a) };
                parents.insert(high, low);
            }
        }
        keys.insert(e.evidence_id.clone(), key);
    }
    let mut split_sources = BTreeSet::new();
    for key in keys.values().flatten() {
        let (base, unit) = group_parts(key)?;
        if !unit.is_empty() {
            split_sources.insert(representative(&parents, &base));
        }
    }
    for key in keys.values_mut() {
        if let Some(current) = key {
            let (base, unit) = group_parts(current)?;
            let base = representative(&parents, &base);
            // Broad citations overlap reviewed units, including through aliases.
            // Locate/merge them into a unit or exclude them before verification.
            *key = if unit.is_empty() && split_sources.contains(&base) {
                None
            } else {
                Some(encode(&(base, unit))?)
            };
        }
    }
    Ok(keys)
}

fn counts(issue: &Issue) -> Result<EvidenceCounts> {
    let mut result = EvidenceCounts {
        submitted: issue.evidence.len(),
        ..Default::default()
    };
    let mut groups: BTreeMap<String, (usize, bool, bool, bool, bool)> = BTreeMap::new();
    let keys = group_keys(issue)?;
    for e in &issue.evidence {
        match e.review.as_ref().map(|r| r.input.disposition) {
            Some(EvidenceDisposition::Invalid | EvidenceDisposition::Excluded) => {
                result.excluded_or_invalid += 1;
                continue;
            }
            _ => (),
        }
        let Some(key) = keys.get(&e.evidence_id).cloned().flatten() else {
            result.pending += 1;
            continue;
        };
        let g = groups.entry(key).or_default();
        g.0 += 1;
        let resolved = e
            .review
            .as_ref()
            .is_some_and(|r| r.input.resolution.is_some());
        match e.stance {
            EvidenceStance::Support => {
                g.1 = true;
                g.3 |= !resolved;
            }
            EvidenceStance::Oppose => {
                g.2 = true;
                g.4 |= !resolved;
            }
        }
    }
    result.independent_groups = groups.len();
    for (_, (n, s, o, us, uo)) in groups {
        result.duplicate_references += n.saturating_sub(1);
        if s && o {
            result.conflicting_groups += 1;
        } else {
            result.support_groups += usize::from(s);
            result.oppose_groups += usize::from(o);
        }
        result.unresolved_support_groups += usize::from(us);
        result.unresolved_oppose_groups += usize::from(uo);
    }
    Ok(result)
}

fn summary(conn: &Connection, issue: &Issue) -> Result<DecisionSummary> {
    let mut current = issue
        .conclusion
        .clone()
        .filter(|c| c.revision == issue.revision);
    if let Some(c) = &current {
        for (id, revision) in &c.factual_revisions {
            let fact = load(conn, &issue.root_task_id, id)?;
            if fact.revision != *revision
                || !fact.conclusion.as_ref().is_some_and(|f| {
                    f.revision == fact.revision && f.status == DecisionStatus::Verified
                })
            {
                current = None;
                break;
            }
        }
    }
    Ok(DecisionSummary {
        issue_id: issue.issue_id.clone(),
        question: issue.question.clone(),
        context: issue.context.clone(),
        kind: issue.kind,
        revision: issue.revision,
        required_for_success: issue.required_for_success,
        counts: counts(issue)?,
        current_conclusion: current,
        last_conclusion: issue.conclusion.clone(),
    })
}

pub(crate) fn summaries_on(conn: &Connection, root: &str) -> Result<Vec<DecisionSummary>> {
    let mut stmt = conn
        .prepare("SELECT record_json FROM wc_team_decisions WHERE root_task_id=?1 ORDER BY rowid")
        .map_err(store_error)?;
    let records = stmt
        .query_map([root], |r| r.get::<_, String>(0))
        .map_err(store_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_error)?;
    records
        .iter()
        .map(|r| summary(conn, &decode::<Issue>(r)?))
        .collect()
}

pub(crate) fn completion_on(conn: &Connection, task: &str) -> Result<Vec<DecisionSummary>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT summary_json FROM wc_team_decision_completions WHERE task_id=?1",
            [task],
            |r| r.get(0),
        )
        .optional()
        .map_err(store_error)?;
    raw.map(|r| decode(&r))
        .transpose()
        .map(|v| v.unwrap_or_default())
}

pub(crate) fn freeze_completion(conn: &Connection, task: &str, success: bool) -> Result<()> {
    let summaries = summaries_on(conn, task)?;
    if success {
        check(
            summaries.iter().all(|s| {
                !s.required_for_success
                    || s.current_conclusion.as_ref().is_some_and(|c| {
                        c.status == DecisionStatus::Verified
                            && c.selected_stance == EvidenceStance::Support
                    })
            }),
            "team_decision_required_fact_unverified",
        )?;
    }
    if !summaries.is_empty() {
        conn.execute(
            "INSERT INTO wc_team_decision_completions(task_id,summary_json) VALUES(?1,?2)",
            params![task, encode(&summaries)?],
        )
        .map_err(store_error)?;
    }
    Ok(())
}

fn check_publication(
    conn: &Connection,
    run: &VerifiedAgentRun,
    artifact: &EvidenceArtifact,
    source: &EvidenceSource,
) -> Result<()> {
    crate::agent_meeting::authorize_room(conn, run, &artifact.room_id)
        .map_err(|_| error("team_decision_artifact_denied"))?;
    let published = crate::agent_meeting::load_agent_meeting_artifact_on_connection(
        conn,
        &artifact.room_id,
        &artifact.artifact_id,
        artifact.version,
    )
    .map_err(|_| error("team_decision_artifact_unavailable"))?;
    let content = std::str::from_utf8(&published.bytes)
        .map_err(|_| error("team_decision_artifact_not_utf8"))?;
    check(
        hash(&published.bytes) == artifact.sha256 && content.contains(&source.excerpt),
        "team_decision_artifact_mismatch",
    )
}

impl Database {
    pub fn team_decision(
        &self,
        run: &VerifiedAgentRun,
        request: TeamDecisionRequest,
    ) -> Result<Value> {
        let encoded = encode(&request)?;
        check(encoded.len() <= 32 * 1024, "team_decision_input_too_large")?;
        text(request.root(), 128)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let manager = authorize(&tx, run, &request)?;
        // A shared human/project does not permit another manager to adopt an issue.
        let foreign: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_decisions WHERE root_task_id=?1 AND (manager_agent_id!=?2 OR project!=?3))",params![request.root(),manager,run.project()],|r|r.get(0)).map_err(store_error)?;
        check(!foreign, "team_decision_denied")?;
        if let TeamDecisionRequest::Read {
            root_task_id,
            issue_id,
            section,
            offset,
            limit,
        } = &request
        {
            check(
                (1..=50).contains(limit) && *offset <= MAX_EVENTS as usize,
                "team_decision_invalid_page",
            )?;
            let response = if let Some(id) = issue_id {
                let issue = load(&tx, root_task_id, id)?;
                let mut value = json!({"summary":summary(&tx,&issue)?});
                match section {
                    DecisionSection::Summary => (),
                    DecisionSection::Evidence => {
                        let keys = group_keys(&issue)?;
                        let records = issue
                            .evidence
                            .iter()
                            .skip(*offset)
                            .take(*limit)
                            .map(|e| {
                                let mut item = json!(e);
                                item["source_group_id"] = json!(keys
                                    .get(&e.evidence_id)
                                    .and_then(|k| k.as_ref())
                                    .map(|k| hash(k.as_bytes())));
                                item
                            })
                            .collect::<Vec<_>>();
                        value["evidence"] = json!(records);
                        value["next_offset"] = json!(
                            (offset + limit < issue.evidence.len()).then_some(offset + limit)
                        );
                    }
                    DecisionSection::History => {
                        let mut stmt=tx.prepare("SELECT request_json,author_agent_id,author_run_id,revision,created_at_unix_ms FROM wc_team_decision_events WHERE issue_id=?1 ORDER BY revision LIMIT ?2 OFFSET ?3").map_err(store_error)?;
                        let rows = stmt
                            .query_map(params![id, (*limit + 1) as i64, *offset as i64], |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, String>(2)?,
                                    r.get::<_, i64>(3)?,
                                    r.get::<_, i64>(4)?,
                                ))
                            })
                            .map_err(store_error)?;
                        let mut history = Vec::new();
                        for row in rows {
                            let (raw, agent, run, revision, time) = row.map_err(store_error)?;
                            let mut input: Value = decode(&raw)?;
                            if let Some(obj) = input.as_object_mut() {
                                obj.remove("idempotency_key");
                            }
                            history.push(json!({"request":input,"author_agent_id":agent,"author_run_id":run,"revision":revision,"created_at_unix_ms":time}));
                        }
                        value["next_offset"] =
                            json!((history.len() > *limit).then_some(offset + limit));
                        history.truncate(*limit);
                        value["history"] = json!(history);
                    }
                }
                value
            } else {
                check(
                    matches!(section, DecisionSection::Summary),
                    "team_decision_issue_required",
                )?;
                let all = summaries_on(&tx, root_task_id)?;
                json!({"issues":all.iter().skip(*offset).take(*limit).collect::<Vec<_>>(),"total_count":all.len(),"next_offset":(offset+limit<all.len()).then_some(offset+limit)})
            };
            tx.commit().map_err(store_error)?;
            return Ok(response);
        }
        let (id, expected, key) = request.issue_revision_key();
        text(key, 128)?;
        let request_hash = hash(encoded.as_bytes());
        let key_hash = hash(key.as_bytes());
        let replay:Option<(String,String)>=tx.query_row("SELECT request_hash,response_json FROM wc_team_decision_events WHERE root_task_id=?1 AND author_agent_id=?2 AND key_hash=?3",params![request.root(),run.agent_id(),key_hash],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?;
        if let Some((old, response)) = replay {
            check(old == request_hash, "team_decision_idempotency_conflict")?;
            return decode(&response);
        }
        let mut issue = if let Some(id) = id {
            let issue = load(&tx, request.root(), id)?;
            check(
                expected == issue.revision,
                "team_decision_revision_conflict",
            )?;
            check(
                issue.revision < MAX_EVENTS,
                "team_decision_capacity_reached",
            )?;
            issue
        } else {
            let TeamDecisionRequest::Open {
                question,
                context,
                kind,
                required_for_success,
                ..
            } = &request
            else {
                unreachable!()
            };
            text(question, 2048)?;
            text(context, 4096)?;
            check(
                !required_for_success || *kind == DecisionKind::Fact,
                "team_decision_required_fact_only",
            )?;
            let count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM wc_team_decisions WHERE root_task_id=?1",
                    [request.root()],
                    |r| r.get(0),
                )
                .map_err(store_error)?;
            check(count < MAX_ISSUES as i64, "team_decision_capacity_reached")?;
            let total: i64 = tx
                .query_row("SELECT COUNT(*) FROM wc_team_decisions", [], |r| r.get(0))
                .map_err(store_error)?;
            check(total < 4096, "team_decision_capacity_reached")?;
            Issue {
                issue_id: format!("wc_issue_{}", uuid::Uuid::new_v4().simple()),
                root_task_id: request.root().into(),
                question: question.clone(),
                context: context.clone(),
                kind: *kind,
                required_for_success: *required_for_success,
                revision: 0,
                evidence: vec![],
                conclusion: None,
            }
        };
        issue.revision += 1;
        let mut evidence_id = None;
        match &request {
            TeamDecisionRequest::Submit {
                stance,
                claim,
                source,
                artifact,
                ..
            } => {
                text(claim, 4096)?;
                check(
                    issue.evidence.len() < MAX_EVIDENCE,
                    "team_decision_capacity_reached",
                )?;
                if let Some(s) = source {
                    origin_valid(&s.origin)?;
                    text(&s.location, 1024)?;
                    text(&s.excerpt, 8192)?;
                    if let Some(o) = &s.upstream {
                        origin_valid(o)?;
                    }
                }
                if let Some(a) = artifact {
                    check(
                        run.agent_id() == manager,
                        "team_decision_import_manager_only",
                    )?;
                    let s = source
                        .as_ref()
                        .ok_or_else(|| error("team_decision_source_required"))?;
                    check_publication(&tx, run, a, s)?;
                }
                let id = format!("wc_evidence_{}", uuid::Uuid::new_v4().simple());
                evidence_id = Some(id.clone());
                issue.evidence.push(Evidence {
                    evidence_id: id,
                    author_agent_id: run.agent_id().into(),
                    author_run_id: run.run_id().into(),
                    stance: *stance,
                    claim: claim.clone(),
                    source: source.clone(),
                    artifact: artifact.clone(),
                    publication_checked: artifact.is_some(),
                    review: None,
                });
            }
            TeamDecisionRequest::Review {
                evidence_id: id,
                review,
                ..
            } => {
                text(&review.rationale, 4096)?;
                for s in [
                    &review.independent_unit,
                    &review.independence_rationale,
                    &review.resolution,
                ]
                .into_iter()
                .flatten()
                {
                    text(s, 2048)?;
                }
                check(
                    review.independent_unit.is_some() == review.independence_rationale.is_some(),
                    "team_decision_independence_required",
                )?;
                check(
                    review.independent_unit.is_none() || review.same_origin_as.is_none(),
                    "team_decision_invalid_origin",
                )?;
                let index = issue
                    .evidence
                    .iter()
                    .position(|e| &e.evidence_id == id)
                    .ok_or_else(|| error("team_decision_evidence_not_found"))?;
                if review.disposition == EvidenceDisposition::Verified {
                    let s = issue.evidence[index]
                        .source
                        .as_ref()
                        .ok_or_else(|| error("team_decision_source_required"))?;
                    if let Some(a) = &issue.evidence[index].artifact {
                        check_publication(&tx, run, a, s)?;
                    }
                    check(
                        review.origin.is_some() != review.same_origin_as.is_some(),
                        "team_decision_origin_required",
                    )?;
                    if let Some(origin) = &review.origin {
                        origin_valid(origin)?;
                        check(
                            origin == s.upstream.as_ref().unwrap_or(&s.origin),
                            "team_decision_origin_mismatch",
                        )?;
                    }
                    if let Some(target) = &review.same_origin_as {
                        check(
                            target != id && issue.evidence.iter().any(|e| &e.evidence_id == target),
                            "team_decision_invalid_origin",
                        )?;
                    }
                } else {
                    check(
                        review.origin.is_none()
                            && review.same_origin_as.is_none()
                            && review.independent_unit.is_none()
                            && review.resolution.is_none(),
                        "team_decision_invalid_review",
                    )?;
                }
                issue.evidence[index].review = Some(ReviewedEvidence {
                    input: review.clone(),
                    reviewer_agent_id: run.agent_id().into(),
                    reviewer_run_id: run.run_id().into(),
                    revision: issue.revision,
                });
                if review.disposition == EvidenceDisposition::Verified {
                    check(
                        group(&issue, &issue.evidence[index], &mut BTreeSet::new())?.is_some(),
                        "team_decision_origin_unverified",
                    )?;
                }
                // Validate dependants too; source reclassification must never create cycles.
                counts(&issue)?;
            }
            TeamDecisionRequest::Conclude {
                status,
                selected_stance,
                conclusion,
                rationale,
                factual_issue_ids,
                ..
            } => {
                text(conclusion, 4096)?;
                text(rationale, 4096)?;
                check(
                    factual_issue_ids.len() <= MAX_ISSUES,
                    "team_decision_invalid_input",
                )?;
                let c = counts(&issue)?;
                if *status == DecisionStatus::Verified {
                    check(
                        issue.kind == DecisionKind::Fact,
                        "team_decision_fact_required",
                    )?;
                    let (support, contrary) = match selected_stance {
                        EvidenceStance::Support => (c.support_groups, c.unresolved_oppose_groups),
                        EvidenceStance::Oppose => (c.oppose_groups, c.unresolved_support_groups),
                    };
                    check(
                        support > 0 && contrary == 0 && c.pending == 0 && c.conflicting_groups == 0,
                        "team_decision_verification_required",
                    )?;
                }
                let mut facts = BTreeMap::new();
                if *status == DecisionStatus::Selected {
                    check(
                        issue.kind == DecisionKind::Choice,
                        "team_decision_choice_required",
                    )?;
                    let contrary = match selected_stance {
                        EvidenceStance::Support => c.unresolved_oppose_groups,
                        EvidenceStance::Oppose => c.unresolved_support_groups,
                    };
                    check(
                        c.pending == 0 && c.conflicting_groups == 0 && contrary == 0,
                        "team_decision_verification_required",
                    )?;
                    for id in factual_issue_ids {
                        let fact = load(&tx, &issue.root_task_id, id)?;
                        check(
                            fact.kind == DecisionKind::Fact
                                && fact.conclusion.as_ref().is_some_and(|c| {
                                    c.revision == fact.revision
                                        && c.status == DecisionStatus::Verified
                                }),
                            "team_decision_required_fact_unverified",
                        )?;
                        facts.insert(id.clone(), fact.revision);
                    }
                } else {
                    check(factual_issue_ids.is_empty(), "team_decision_invalid_input")?;
                }
                issue.conclusion = Some(DecisionConclusion {
                    revision: issue.revision,
                    status: *status,
                    selected_stance: *selected_stance,
                    conclusion: conclusion.clone(),
                    rationale: rationale.clone(),
                    reviewer_agent_id: run.agent_id().into(),
                    reviewer_run_id: run.run_id().into(),
                    factual_revisions: facts,
                });
            }
            TeamDecisionRequest::Open { .. } => (),
            TeamDecisionRequest::Read { .. } => unreachable!(),
        }
        let record = encode(&issue)?;
        check(
            record.len() <= MAX_RECORD_BYTES,
            "team_decision_capacity_reached",
        )?;
        tx.execute("INSERT INTO wc_team_decisions(issue_id,root_task_id,manager_agent_id,project,record_json) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(issue_id) DO UPDATE SET record_json=excluded.record_json",
            params![issue.issue_id,issue.root_task_id,manager,run.project(),record]).map_err(store_error)?;
        let response =
            json!({"summary":summary(&tx,&issue)?,"evidence_id":evidence_id,"state_changed":true});
        tx.execute("INSERT INTO wc_team_decision_events(issue_id,root_task_id,author_agent_id,author_run_id,revision,key_hash,request_hash,request_json,response_json,created_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![issue.issue_id,issue.root_task_id,run.agent_id(),run.run_id(),issue.revision,key_hash,request_hash,encoded,encode(&response)?,now_unix_ms()]).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(response)
    }
}
