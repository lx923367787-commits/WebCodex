//! Ordinary team plans and fixed CLI-call slots. Execution and recovery remain
//! separate runtime domains; caller-supplied plans never supply commands.
use crate::agent_task::AGENT_TASK_ID_PREFIX;
use crate::communication::{
    digest_text, new_id, now_unix_ms, store_error, validate_communication_principal, validate_id,
    validate_idempotency_key, CommunicationPrincipal, CommunicationStoreError,
};
use crate::Database;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use webcodex_core::runner_protocol::ShellCommandExecutionState;

pub const TEAM_CLI_ATTEMPT_ID_PREFIX: &str = "wc_team_cli_attempt_";
pub const MAX_TEAM_LIST_LIMIT: usize = 20;
const MAX_POLICIES: i64 = 4096;
const MAX_PLANS: i64 = 4096;
const MAX_ATTEMPTS: i64 = 65536;
const MAX_SUBAGENTS: usize = 32;
const MAX_CLI_CALLS: usize = 32;

type Result<T> = std::result::Result<T, CommunicationStoreError>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamPolicyMode {
    Auto,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamCountRange {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamProviderPreference {
    pub provider_id: String,
    pub enabled: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamPolicyInput {
    pub mode: TeamPolicyMode,
    /// Ordinary child assignments in the complete frozen plan, not concurrency.
    pub subagents: TeamCountRange,
    pub cli_per_agent: TeamCountRange,
    pub providers: Vec<TeamProviderPreference>,
}

impl Default for TeamPolicyInput {
    fn default() -> Self {
        Self {
            mode: TeamPolicyMode::Auto,
            subagents: TeamCountRange { min: 0, max: 4 },
            cli_per_agent: TeamCountRange { min: 0, max: 2 },
            providers: ["workbuddy", "antigravity"]
                .into_iter()
                .map(|id| TeamProviderPreference {
                    provider_id: id.into(),
                    enabled: true,
                    model: None,
                    effort: None,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TeamPolicy {
    pub revision: i64,
    #[serde(flatten)]
    pub settings: TeamPolicyInput,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamCliCall {
    pub provider_id: String,
    pub model: String,
    pub effort: Option<String>,
    /// Manager-assigned subtask text, never an executable or authority grant.
    /// A member with multiple calls must explicitly assign every call's work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// Explicit immutable text material; never a host path or filesystem grant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<crate::AgentEnrollmentInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamAgentPlan {
    pub task_id: String,
    pub cli_calls: Vec<TeamCliCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamPlanInput {
    pub manager: TeamAgentPlan,
    pub subagents: Vec<TeamAgentPlan>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamPlanStatus {
    Active,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamPlanRecord {
    pub project_id: String,
    pub root_task_id: String,
    pub policy: TeamPolicy,
    pub plan: TeamPlanInput,
    pub status: TeamPlanStatus,
    pub cancel_requested: bool,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TeamPlanMutation {
    pub plan: TeamPlanRecord,
    pub created: bool,
    pub replayed: bool,
    pub state_changed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TeamPlanSummary {
    pub project_id: String,
    pub root_task_id: String,
    pub policy_revision: i64,
    pub status: TeamPlanStatus,
    pub cancel_requested: bool,
    pub subagent_count: usize,
    pub cli_call_count: usize,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamPlanPage {
    pub plans: Vec<TeamPlanSummary>,
    pub total_count: i64,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamCliDispatchState {
    Reserved,
    Dispatching,
    Bound,
    Terminal,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamCliAttemptRecord {
    pub attempt_id: String,
    pub project_id: String,
    pub root_task_id: String,
    pub source_task_id: String,
    pub call_index: u32,
    pub provider_id: String,
    pub model: String,
    pub effort: Option<String>,
    /// Frozen explicit CLI subtask, or the AgentTask instruction for a sole call.
    /// This text does not grant execution authority or supply native commands.
    pub instruction: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<crate::AgentEnrollmentInput>,
    pub fallback_thinking_profile_id: String,
    pub timeout_secs: Option<u64>,
    pub dispatch_state: TeamCliDispatchState,
    pub job_id: Option<String>,
    /// Only trusted Runner Job observations populate this field.
    pub execution_state: Option<ShellCommandExecutionState>,
    pub error_code: Option<String>,
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_binding: Option<webcodex_core::runner_protocol::AgentExecutionBinding>,
    pub observation_revision: i64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamCliAttemptReservation {
    pub attempt: TeamCliAttemptRecord,
    pub created: bool,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamCliDispatchClaim {
    pub attempt: TeamCliAttemptRecord,
    pub claimed: bool,
}

/// Runtime-only observation input. No user-facing endpoint may write this directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TeamCliObservation {
    pub job_id: Option<String>,
    pub dispatch_state: TeamCliDispatchState,
    pub execution_state: Option<ShellCommandExecutionState>,
    pub error_code: Option<String>,
    pub result_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamCliAttemptPage {
    pub attempts: Vec<TeamCliAttemptRecord>,
    pub total_count: i64,
    pub next_offset: Option<usize>,
}

pub(crate) fn init_team_task_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_team_policies (
        owner_principal_kind TEXT NOT NULL, owner_principal_digest TEXT NOT NULL,
        project_id TEXT NOT NULL, policy_json TEXT NOT NULL,
        PRIMARY KEY(owner_principal_kind,owner_principal_digest,project_id));
        CREATE TABLE IF NOT EXISTS wc_team_plans (
        root_task_id TEXT PRIMARY KEY REFERENCES wc_agent_tasks(task_id),
        owner_principal_kind TEXT NOT NULL, owner_principal_digest TEXT NOT NULL,
        project_id TEXT NOT NULL, request_key_hash TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
        record_json TEXT NOT NULL, created_at_unix_ms INTEGER NOT NULL,
        UNIQUE(owner_principal_kind,owner_principal_digest,request_key_hash));
        CREATE INDEX IF NOT EXISTS idx_wc_team_plans_scope ON wc_team_plans(owner_principal_kind,owner_principal_digest,project_id,created_at_unix_ms,root_task_id);
        CREATE TABLE IF NOT EXISTS wc_team_cli_attempts (
        attempt_id TEXT PRIMARY KEY, root_task_id TEXT NOT NULL REFERENCES wc_team_plans(root_task_id),
        source_task_id TEXT NOT NULL REFERENCES wc_agent_tasks(task_id), project_id TEXT NOT NULL,
        owner_principal_kind TEXT NOT NULL, owner_principal_digest TEXT NOT NULL,
        call_index INTEGER NOT NULL CHECK(call_index>=0 AND call_index<32),
        request_key_hash TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
        record_json TEXT NOT NULL, created_at_unix_ms INTEGER NOT NULL,
        UNIQUE(root_task_id,source_task_id,call_index),
        UNIQUE(owner_principal_kind,owner_principal_digest,request_key_hash));
        CREATE INDEX IF NOT EXISTS idx_wc_team_cli_scope ON wc_team_cli_attempts(owner_principal_kind,owner_principal_digest,project_id,created_at_unix_ms,attempt_id);")?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_team_cli_rounds (
        root_task_id TEXT NOT NULL REFERENCES wc_team_plans(root_task_id), round INTEGER NOT NULL CHECK(round>0 AND round<=15),
        request_key_hash TEXT NOT NULL, request_fingerprint TEXT NOT NULL, record_json TEXT NOT NULL,
        PRIMARY KEY(root_task_id,round), UNIQUE(root_task_id,request_key_hash));
        CREATE TRIGGER IF NOT EXISTS wc_team_round_immutable BEFORE UPDATE ON wc_team_cli_rounds BEGIN SELECT RAISE(ABORT,'CLI rounds are immutable'); END;
        CREATE TRIGGER IF NOT EXISTS wc_team_round_preserved BEFORE DELETE ON wc_team_cli_rounds BEGIN SELECT RAISE(ABORT,'CLI round history is preserved'); END;")?;
    execution_factory::ensure_factory_schema(conn)?;
    Ok(())
}

fn error(code: &'static str, message: &str) -> CommunicationStoreError {
    CommunicationStoreError::new(code, message)
}
fn not_found() -> CommunicationStoreError {
    error("team_task_not_found", "Team task resource does not exist")
}
fn invalid(message: &str) -> CommunicationStoreError {
    error("invalid_team_request", message)
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| error("team_store_invalid", "Cannot encode team state"))
}
fn decode<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    serde_json::from_str(value)
        .map_err(|_| error("team_store_invalid", "Stored team state is invalid"))
}
fn hash<T: Serialize>(value: &T) -> Result<String> {
    Ok(digest_text("webcodex.team.intent.v1", &encode(value)?))
}
fn key_hash(value: &str) -> Result<String> {
    Ok(digest_text(
        "webcodex.team.key.v1",
        &validate_idempotency_key(value)?,
    ))
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn identifier(value: &str, max: usize) -> bool {
    text(value, max)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn project_id(value: &str) -> Result<()> {
    if text(value, 512) {
        Ok(())
    } else {
        Err(invalid("Invalid project id"))
    }
}
fn provider(value: &str) -> bool {
    matches!(value, "workbuddy" | "antigravity")
}
fn task_id(value: &str) -> Result<()> {
    validate_id(value, AGENT_TASK_ID_PREFIX, "invalid_agent_task_id")
}
fn attempt_id(value: &str) -> Result<()> {
    validate_id(
        value,
        TEAM_CLI_ATTEMPT_ID_PREFIX,
        "invalid_team_cli_attempt_id",
    )
}

fn validate_policy(settings: &TeamPolicyInput) -> Result<()> {
    if settings.subagents.min > settings.subagents.max
        || settings.subagents.max > 32
        || settings.cli_per_agent.min > settings.cli_per_agent.max
        || settings.cli_per_agent.max > 32
    {
        return Err(invalid(
            "Team count ranges must be ordered and no greater than 32",
        ));
    }
    let mut ids = BTreeSet::new();
    if settings.providers.len() > 2
        || settings.providers.iter().any(|p| {
            !provider(&p.provider_id)
                || !ids.insert(&p.provider_id)
                || p.model.as_deref().is_some_and(|m| !text(m, 256))
                || p.effort
                    .as_deref()
                    .is_some_and(|e| !text(e, 64) || e == "project_policy")
        })
    {
        return Err(invalid("Invalid or duplicate team provider preferences"));
    }
    if settings.mode == TeamPolicyMode::Custom
        && settings.cli_per_agent.min > 0
        && !settings.providers.iter().any(|p| p.enabled)
    {
        return Err(invalid(
            "A positive CLI minimum requires an enabled provider",
        ));
    }
    Ok(())
}

fn validate_policy_for_admission(settings: &TeamPolicyInput) -> Result<()> {
    validate_policy(settings)?;
    // New execution constraints must not prevent reading or revising policies
    // that were valid when persisted by an earlier version.
    if settings.mode == TeamPolicyMode::Custom
        && (settings.subagents.min + 1) * settings.cli_per_agent.min > 32
    {
        return Err(error(
            "team_cli_budget_unsatisfiable",
            "The minimum team, including the manager, requires more than 32 total CLI calls",
        ));
    }
    Ok(())
}

fn members(plan: &TeamPlanInput) -> impl Iterator<Item = &TeamAgentPlan> {
    std::iter::once(&plan.manager).chain(&plan.subagents)
}

fn resolve_saved_policy_efforts(policy: &TeamPolicy, plan: &mut TeamPlanInput) -> Result<()> {
    for member in std::iter::once(&mut plan.manager).chain(plan.subagents.iter_mut()) {
        for call in &mut member.cli_calls {
            if call.effort.as_deref() == Some("project_policy") {
                let preference = policy
                    .settings
                    .providers
                    .iter()
                    .find(|p| {
                        p.provider_id == call.provider_id
                            && p.enabled
                            && p.model.as_deref() == Some(call.model.as_str())
                    })
                    .ok_or_else(|| {
                        error(
                            "team_saved_selection_mismatch",
                            "Saved effort requires an enabled provider and its exact saved model",
                        )
                    })?;
                call.effort = preference.effort.clone();
            }
        }
    }
    Ok(())
}

fn validate_plan(root: &str, policy: &TeamPolicy, plan: &TeamPlanInput) -> Result<()> {
    validate_policy_for_admission(&policy.settings)?;
    if plan.manager.task_id != root
        || plan.subagents.len() > MAX_SUBAGENTS
        || members(plan).any(|m| m.cli_calls.len() > MAX_CLI_CALLS)
    {
        return Err(invalid("Invalid manager or oversized ordinary team plan"));
    }
    let custom = policy.settings.mode == TeamPolicyMode::Custom;
    if custom
        && !(policy.settings.subagents.min..=policy.settings.subagents.max)
            .contains(&(plan.subagents.len() as u32))
    {
        return Err(error(
            "team_subagent_quota",
            "Ordinary child assignments do not satisfy the project policy",
        ));
    }
    let mut ids = BTreeSet::new();
    for member in members(plan) {
        task_id(&member.task_id)?;
        if !ids.insert(&member.task_id) {
            return Err(invalid("A task may appear only once in a team plan"));
        }
        if custom
            && !(policy.settings.cli_per_agent.min..=policy.settings.cli_per_agent.max)
                .contains(&(member.cli_calls.len() as u32))
        {
            return Err(error(
                "team_cli_quota",
                "A team member's planned CLI calls do not satisfy the policy",
            ));
        }
        for call in &member.cli_calls {
            let mut input_names = BTreeSet::new();
            if call.inputs.len() > 8
                || serde_json::to_vec(&call.inputs)
                    .map_err(|_| invalid("Invalid CLI inputs"))?
                    .len()
                    > 32 * 1024
                || call.inputs.iter().any(|input| {
                    !text(&input.name, 128)
                        || input.content.contains('\0')
                        || !input_names.insert(&input.name)
                })
            {
                return Err(invalid(
                    "CLI inputs require unique bounded names and at most 32 KiB of JSON text",
                ));
            }
            if call.instruction.as_deref().is_some_and(|instruction| {
                instruction.trim().is_empty()
                    || instruction.len() > 8192
                    || instruction.contains('\0')
            }) {
                return Err(invalid("CLI subtask instruction must be nonblank, at most 8192 UTF-8 bytes and contain no NUL"));
            }
            if member.cli_calls.len() > 1 && call.instruction.is_none() {
                return Err(error("team_cli_instruction_required", "Each CLI call needs an explicit subtask instruction when a member has multiple calls"));
            }
            if !provider(&call.provider_id)
                || !text(&call.model, 256)
                || call.effort.as_deref().is_some_and(|v| !text(v, 64))
            {
                return Err(invalid(
                    "Each planned CLI call needs a supported provider, explicit model and a nonblank effort when supplied",
                ));
            }
            if custom {
                let preference = policy
                    .settings
                    .providers
                    .iter()
                    .find(|p| p.enabled && p.provider_id == call.provider_id)
                    .ok_or_else(|| {
                        error("team_provider_disabled", "Planned CLI provider is disabled")
                    })?;
                if preference.model.as_deref().is_some_and(|m| m != call.model)
                    || preference
                        .effort
                        .as_ref()
                        .is_some_and(|e| call.effort.as_ref() != Some(e))
                {
                    return Err(error(
                        "team_model_policy_mismatch",
                        "Planned model or effort differs from the project policy",
                    ));
                }
            }
        }
    }
    Ok(())
}

struct OwnedTask {
    instruction: String,
    state: String,
}
fn owned_task(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    id: &str,
) -> Result<OwnedTask> {
    task_id(id)?;
    conn.query_row("SELECT instruction,state FROM wc_agent_tasks WHERE task_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3 AND referenced_project_id=?4",
        params![id,principal.kind,principal.digest,project], |r| Ok(OwnedTask { instruction:r.get(0)?,state:r.get(1)? })).optional().map_err(store_error)?.ok_or_else(not_found)
}
fn require_live(task: &OwnedTask) -> Result<()> {
    match task.state.as_str() {
        "ready" | "active" => Ok(()),
        "succeeded" | "failed" => Err(error("team_root_terminal", "The task is already terminal")),
        _ => Err(error(
            "team_store_invalid",
            "Stored AgentTask state is invalid",
        )),
    }
}
pub(crate) fn policy_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
) -> Result<TeamPolicy> {
    let stored: Option<String> = conn.query_row("SELECT policy_json FROM wc_team_policies WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3", params![principal.kind,principal.digest,project], |r|r.get(0)).optional().map_err(store_error)?;
    let policy: TeamPolicy = stored
        .as_deref()
        .map(decode)
        .transpose()?
        .unwrap_or_default();
    if policy.revision < 0 {
        return Err(error(
            "team_store_invalid",
            "Invalid stored policy revision",
        ));
    }
    validate_policy(&policy.settings)?;
    Ok(policy)
}
fn plan_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    root: &str,
) -> Result<TeamPlanRecord> {
    task_id(root)?;
    let row: (String,String) = conn.query_row("SELECT project_id,record_json FROM wc_team_plans WHERE root_task_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3", params![root,principal.kind,principal.digest], |r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?.ok_or_else(not_found)?;
    let plan: TeamPlanRecord = decode(&row.1)?;
    if plan.root_task_id != root
        || plan.project_id != row.0
        || plan.cancel_requested != (plan.status == TeamPlanStatus::Cancelled)
    {
        return Err(error(
            "team_store_invalid",
            "Stored team plan identity is inconsistent",
        ));
    }
    owned_task(conn, principal, &plan.project_id, root)?;
    Ok(plan)
}
fn live_plan(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    root: &str,
) -> Result<TeamPlanRecord> {
    let plan = plan_on(conn, principal, root)?;
    if plan.cancel_requested {
        return Err(error("team_plan_cancelled", "The team plan is cancelled"));
    }
    require_live(&owned_task(conn, principal, &plan.project_id, root)?)?;
    Ok(plan)
}
fn attempt_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    id: &str,
) -> Result<TeamCliAttemptRecord> {
    attempt_id(id)?;
    let row:(String,String,String,u32,String) = conn.query_row("SELECT project_id,root_task_id,source_task_id,call_index,record_json FROM wc_team_cli_attempts WHERE attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3", params![id,principal.kind,principal.digest], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(store_error)?.ok_or_else(not_found)?;
    let record: TeamCliAttemptRecord = decode(&row.4)?;
    if record.attempt_id != id
        || record.project_id != row.0
        || record.root_task_id != row.1
        || record.source_task_id != row.2
        || record.call_index != row.3
        || record.observation_revision < 0
    {
        return Err(error(
            "team_store_invalid",
            "Stored CLI attempt identity is inconsistent",
        ));
    }
    Ok(record)
}

fn cli_reservation_identity(
    project: &str,
    root_task_id: &str,
    source_task_id: &str,
    call_index: u32,
    idempotency_key: &str,
    fallback_thinking_profile_id: &str,
    timeout_secs: Option<u64>,
) -> Result<(String, String)> {
    project_id(project)?;
    task_id(root_task_id)?;
    task_id(source_task_id)?;
    if call_index >= 32
        || !identifier(fallback_thinking_profile_id, 128)
        || timeout_secs.is_some_and(|s| s == 0 || s > 3600)
    {
        return Err(invalid("Invalid CLI slot, fallback profile or timeout"));
    }
    Ok((
        key_hash(idempotency_key)?,
        hash(&(
            project,
            root_task_id,
            source_task_id,
            call_index,
            fallback_thinking_profile_id,
            timeout_secs,
        ))?,
    ))
}

fn cli_attempt_replay_on(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    key: &str,
    fingerprint: &str,
) -> Result<Option<TeamCliAttemptRecord>> {
    let replay: Option<(String, String)> = conn.query_row(
        "SELECT attempt_id,request_fingerprint FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND request_key_hash=?3",
        params![principal.kind, principal.digest, key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional().map_err(store_error)?;
    let Some((id, saved_fingerprint)) = replay else {
        return Ok(None);
    };
    if saved_fingerprint != fingerprint {
        return Err(error(
            "team_idempotency_conflict",
            "CLI reservation key belongs to a different intent",
        ));
    }
    attempt_on(conn, principal, &id).map(Some)
}
fn update_attempt(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    record: &mut TeamCliAttemptRecord,
) -> Result<()> {
    if let Some(binding) = &record.execution_binding {
        if let webcodex_core::runner_protocol::AgentExecutionCarrier::Job { job_id, .. } =
            &binding.carrier
        {
            if record
                .job_id
                .as_ref()
                .is_some_and(|actual| actual != job_id)
            {
                return Err(error(
                    "agent_execution_binding_mismatch",
                    "Job does not match the frozen execution",
                ));
            }
        }
    }
    record.observation_revision = record
        .observation_revision
        .checked_add(1)
        .ok_or_else(|| error("team_store_invalid", "Attempt revision exhausted"))?;
    record.updated_at_unix_ms = now_unix_ms();
    conn.execute("UPDATE wc_team_cli_attempts SET record_json=?4 WHERE attempt_id=?1 AND owner_principal_kind=?2 AND owner_principal_digest=?3",params![record.attempt_id,principal.kind,principal.digest,encode(record)?]).map_err(store_error)?;
    Ok(())
}
fn capacity(conn: &Connection, table: &str, limit: i64) -> Result<()> {
    let sql = match table {
        "policy" => "SELECT COUNT(*) FROM wc_team_policies",
        "plan" => "SELECT COUNT(*) FROM wc_team_plans",
        _ => "SELECT COUNT(*) FROM wc_team_cli_attempts",
    };
    let count: i64 = conn.query_row(sql, [], |r| r.get(0)).map_err(store_error)?;
    if count >= limit {
        Err(error(
            "team_capacity_exhausted",
            "Team history capacity reached; preserve existing evidence",
        ))
    } else {
        Ok(())
    }
}
fn page_bounds(offset: usize, limit: usize) -> Result<()> {
    if limit == 0 || limit > MAX_TEAM_LIST_LIMIT || offset > MAX_ATTEMPTS as usize {
        Err(invalid("Invalid team list bounds"))
    } else {
        Ok(())
    }
}

/// Recovery admission must call this inside its own immediate transaction.
/// It supplies no recovery authority; it only validates the current ordinary source.
pub(crate) fn load_team_cli_attempt_for_fallback(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    source_attempt_id: &str,
) -> Result<TeamCliAttemptRecord> {
    validate_communication_principal(principal)?;
    let record = attempt_on(conn, principal, source_attempt_id)?;
    let plan = live_plan(conn, principal, &record.root_task_id)?;
    let source = owned_task(conn, principal, &record.project_id, &record.source_task_id)?;
    require_live(&source)?;
    let call = planned_call_on(
        conn,
        principal,
        &record.root_task_id,
        &record.source_task_id,
        record.call_index,
    )?;
    if plan.project_id != record.project_id
        || call.provider_id != record.provider_id
        || call.model != record.model
        || call.effort != record.effort
        || call.inputs != record.inputs
        || call.instruction.as_deref().unwrap_or(&source.instruction) != record.instruction
    {
        return Err(error(
            "team_store_invalid",
            "CLI attempt no longer matches its frozen ordinary plan",
        ));
    }
    Ok(record)
}

fn register_team_plan_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    root_task_id: &str,
    expected_policy_revision: i64,
    mut plan: TeamPlanInput,
    idempotency_key: &str,
    policy_project: &str,
) -> Result<TeamPlanMutation> {
    validate_communication_principal(principal)?;
    project_id(project)?;
    task_id(root_task_id)?;
    let key = key_hash(idempotency_key)?;
    let fingerprint = hash(&(project, root_task_id, expected_policy_revision, &plan))?;
    let root = owned_task(conn, principal, project, root_task_id)?;
    let replay:Option<(String,String)>=conn.query_row("SELECT root_task_id,request_fingerprint FROM wc_team_plans WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND request_key_hash=?3",params![principal.kind,principal.digest,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(store_error)?;
    if let Some((saved_root, saved_fingerprint)) = replay {
        if saved_fingerprint != fingerprint || saved_root != root_task_id {
            return Err(error(
                "team_idempotency_conflict",
                "Team plan key belongs to a different intent",
            ));
        }
        let record = plan_on(conn, principal, root_task_id)?;

        return Ok(TeamPlanMutation {
            plan: record,
            created: false,
            replayed: true,
            state_changed: false,
        });
    }
    require_live(&root)?;
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_team_plans WHERE root_task_id=?1)",
            [root_task_id],
            |r| r.get(0),
        )
        .map_err(store_error)?;
    if exists {
        return Err(error(
            "team_plan_frozen",
            "This root already has a frozen ordinary plan",
        ));
    }
    let policy = policy_on(conn, principal, policy_project)?;
    if policy.revision != expected_policy_revision {
        return Err(error(
            "team_policy_revision_conflict",
            "Project team policy changed",
        ));
    }
    // Resolve an explicit saved-policy selector only after the revision check.
    // The request fingerprint above retains the original intent for replay.
    resolve_saved_policy_efforts(&policy, &mut plan)?;
    validate_plan(root_task_id, &policy, &plan)?;
    for member in members(&plan) {
        require_live(&owned_task(conn, principal, project, &member.task_id)?)?;
    }
    capacity(conn, "plan", MAX_PLANS)?;
    let now = now_unix_ms();
    let record = TeamPlanRecord {
        project_id: project.into(),
        root_task_id: root_task_id.into(),
        policy,
        plan,
        status: TeamPlanStatus::Active,
        cancel_requested: false,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
    };
    conn.execute(
        "INSERT INTO wc_team_plans VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            root_task_id,
            principal.kind,
            principal.digest,
            project,
            key,
            fingerprint,
            encode(&record)?,
            now
        ],
    )
    .map_err(store_error)?;

    Ok(TeamPlanMutation {
        plan: record,
        created: true,
        replayed: false,
        state_changed: true,
    })
}

impl Database {
    /// Runtime must authorize the exact project before reading or saving policy.
    pub fn get_team_policy(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
    ) -> Result<TeamPolicy> {
        validate_communication_principal(principal)?;
        project_id(project)?;
        policy_on(&self.conn.lock().unwrap(), principal, project)
    }

    pub fn save_team_policy(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        expected_revision: i64,
        settings: TeamPolicyInput,
    ) -> Result<TeamPolicy> {
        validate_communication_principal(principal)?;
        project_id(project)?;
        validate_policy_for_admission(&settings)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let current = policy_on(&tx, principal, project)?;
        if current.revision != expected_revision {
            return Err(error(
                "team_policy_revision_conflict",
                "Project team policy changed",
            ));
        }
        if current.revision == 0 {
            capacity(&tx, "policy", MAX_POLICIES)?;
        }
        let policy = TeamPolicy {
            revision: current
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("Policy revision exhausted"))?,
            settings,
        };
        tx.execute("INSERT INTO wc_team_policies(owner_principal_kind,owner_principal_digest,project_id,policy_json) VALUES(?1,?2,?3,?4) ON CONFLICT(owner_principal_kind,owner_principal_digest,project_id) DO UPDATE SET policy_json=excluded.policy_json",params![principal.kind,principal.digest,project,encode(&policy)?]).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(policy)
    }

    pub fn register_team_plan(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root_task_id: &str,
        expected_policy_revision: i64,
        plan: TeamPlanInput,
        idempotency_key: &str,
    ) -> Result<TeamPlanMutation> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let result = register_team_plan_on_connection(
            &tx,
            principal,
            project,
            root_task_id,
            expected_policy_revision,
            plan,
            idempotency_key,
            project,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(result)
    }

    pub fn read_team_plan(
        &self,
        principal: &CommunicationPrincipal,
        root_task_id: &str,
    ) -> Result<TeamPlanRecord> {
        validate_communication_principal(principal)?;
        plan_on(&self.conn.lock().unwrap(), principal, root_task_id)
    }

    pub fn mark_team_plan_cancelled(
        &self,
        principal: &CommunicationPrincipal,
        root_task_id: &str,
    ) -> Result<TeamPlanMutation> {
        validate_communication_principal(principal)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let mut plan = plan_on(&tx, principal, root_task_id)?;
        let changed = !plan.cancel_requested;
        if changed {
            plan.cancel_requested = true;
            plan.status = TeamPlanStatus::Cancelled;
            plan.updated_at_unix_ms = now_unix_ms();
            tx.execute(
                "UPDATE wc_team_plans SET record_json=?2 WHERE root_task_id=?1",
                params![root_task_id, encode(&plan)?],
            )
            .map_err(store_error)?;
        }
        tx.commit().map_err(store_error)?;
        Ok(TeamPlanMutation {
            plan,
            created: false,
            replayed: !changed,
            state_changed: changed,
        })
    }

    /// Recover existing evidence before refreshing transient Host profile catalogs.
    /// This never reserves a slot or grants dispatch, even after cancellation.
    pub fn lookup_team_cli_attempt_replay(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root_task_id: &str,
        source_task_id: &str,
        call_index: u32,
        idempotency_key: &str,
        fallback_thinking_profile_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<TeamCliAttemptRecord>> {
        validate_communication_principal(principal)?;
        let (key, fingerprint) = cli_reservation_identity(
            project,
            root_task_id,
            source_task_id,
            call_index,
            idempotency_key,
            fallback_thinking_profile_id,
            timeout_secs,
        )?;
        let conn = self.conn.lock().unwrap();
        owned_task(&conn, principal, project, root_task_id)?;
        owned_task(&conn, principal, project, source_task_id)?;
        cli_attempt_replay_on(&conn, principal, &key, &fingerprint)
    }

    pub fn reserve_team_cli_attempt(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root_task_id: &str,
        source_task_id: &str,
        call_index: u32,
        idempotency_key: &str,
        fallback_thinking_profile_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<TeamCliAttemptReservation> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let result = reserve_team_cli_attempt_on_connection(
            &tx,
            principal,
            project,
            root_task_id,
            source_task_id,
            call_index,
            idempotency_key,
            fallback_thinking_profile_id,
            timeout_secs,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(result)
    }

    pub fn read_team_cli_attempt(
        &self,
        principal: &CommunicationPrincipal,
        source_attempt_id: &str,
    ) -> Result<TeamCliAttemptRecord> {
        validate_communication_principal(principal)?;
        attempt_on(&self.conn.lock().unwrap(), principal, source_attempt_id)
    }

    /// `claimed=true` is the sole permission to dispatch this exact reserved call.
    /// Every replay, including dispatching/unknown, returns claimed=false.
    pub fn claim_team_cli_dispatch(
        &self,
        principal: &CommunicationPrincipal,
        attempt_id: &str,
    ) -> Result<TeamCliDispatchClaim> {
        validate_communication_principal(principal)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let mut attempt = attempt_on(&tx, principal, attempt_id)?;
        let factory_owned:bool=tx.query_row("SELECT execution_preparation_json IS NOT NULL FROM wc_team_cli_attempts WHERE attempt_id=?1",[attempt_id],|r|r.get(0)).map_err(store_error)?;
        if factory_owned {
            return Err(error(
                "agent_cli_permit_required",
                "Isolated execution requires its exact controller permit",
            ));
        }
        if attempt.dispatch_state != TeamCliDispatchState::Reserved {
            tx.commit().map_err(store_error)?;
            return Ok(TeamCliDispatchClaim {
                attempt,
                claimed: false,
            });
        }
        load_team_cli_attempt_for_fallback(&tx, principal, attempt_id)?;
        require_live(&owned_task(
            &tx,
            principal,
            &attempt.project_id,
            &attempt.source_task_id,
        )?)?;
        attempt.dispatch_state = TeamCliDispatchState::Dispatching;
        update_attempt(&tx, principal, &mut attempt)?;
        tx.commit().map_err(store_error)?;
        Ok(TeamCliDispatchClaim {
            attempt,
            claimed: true,
        })
    }

    pub fn bind_team_cli_job(
        &self,
        principal: &CommunicationPrincipal,
        attempt_id: &str,
        job_id: &str,
    ) -> Result<TeamCliAttemptRecord> {
        validate_communication_principal(principal)?;
        if !identifier(job_id, 256) {
            return Err(invalid("Invalid Job id"));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let mut attempt = attempt_on(&tx, principal, attempt_id)?;
        if let Some(existing) = &attempt.job_id {
            if existing != job_id {
                return Err(error(
                    "team_cli_job_conflict",
                    "CLI attempt is already bound to another Job",
                ));
            }
            tx.commit().map_err(store_error)?;
            return Ok(attempt);
        }
        if !matches!(
            attempt.dispatch_state,
            TeamCliDispatchState::Dispatching | TeamCliDispatchState::Unknown
        ) {
            return Err(error(
                "team_cli_dispatch_conflict",
                "This CLI attempt cannot bind a Job",
            ));
        }
        attempt.job_id = Some(job_id.into());
        attempt.dispatch_state = TeamCliDispatchState::Bound;
        attempt.execution_state = None;
        update_attempt(&tx, principal, &mut attempt)?;
        tx.commit().map_err(store_error)?;
        Ok(attempt)
    }

    pub fn reconcile_team_cli_attempt(
        &self,
        principal: &CommunicationPrincipal,
        attempt_id: &str,
        expected_observation_revision: i64,
        observation: TeamCliObservation,
    ) -> Result<TeamCliAttemptRecord> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let record = reconcile_team_cli_attempt_on_connection(
            &tx,
            principal,
            attempt_id,
            expected_observation_revision,
            observation,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(record)
    }

    pub fn list_team_plans(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        offset: usize,
        limit: usize,
    ) -> Result<TeamPlanPage> {
        validate_communication_principal(principal)?;
        project_id(project)?;
        page_bounds(offset, limit)?;
        let conn = self.conn.lock().unwrap();
        let total:i64=conn.query_row("SELECT COUNT(*) FROM wc_team_plans WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3",params![principal.kind,principal.digest,project],|r|r.get(0)).map_err(store_error)?;
        let mut statement=conn.prepare("SELECT root_task_id FROM wc_team_plans WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 ORDER BY created_at_unix_ms DESC,root_task_id LIMIT ?4 OFFSET ?5").map_err(store_error)?;
        let ids = statement
            .query_map(
                params![
                    principal.kind,
                    principal.digest,
                    project,
                    limit as i64,
                    offset as i64
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(store_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(store_error)?;
        let mut plans = Vec::new();
        for id in ids {
            let p = plan_on(&conn, principal, &id)?;
            plans.push(TeamPlanSummary {
                project_id: p.project_id,
                root_task_id: p.root_task_id,
                policy_revision: p.policy.revision,
                status: p.status,
                cancel_requested: p.cancel_requested,
                subagent_count: p.plan.subagents.len(),
                cli_call_count: members(&p.plan).map(|m| m.cli_calls.len()).sum(),
                created_at_unix_ms: p.created_at_unix_ms,
                updated_at_unix_ms: p.updated_at_unix_ms,
            });
        }
        let next = offset + plans.len();
        Ok(TeamPlanPage {
            plans,
            total_count: total,
            next_offset: ((next as i64) < total).then_some(next),
        })
    }

    pub fn list_team_cli_attempts(
        &self,
        principal: &CommunicationPrincipal,
        project: &str,
        root_task_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<TeamCliAttemptPage> {
        validate_communication_principal(principal)?;
        project_id(project)?;
        page_bounds(offset, limit)?;
        let mut conn = self.conn.lock().unwrap();
        // This ledger retains every attempt and never replaces/deletes rows.
        // Its implicit rowid therefore preserves insertion order independently
        // of clock changes and mutable observation timestamps. Keep the count,
        // selected IDs and their records in one read snapshot while other
        // connections append, so next_offset describes this exact page.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(store_error)?;
        if let Some(root) = root_task_id {
            let plan = plan_on(&tx, principal, root)?;
            if plan.project_id != project {
                return Err(not_found());
            }
        }
        let total:i64=tx.query_row("SELECT COUNT(*) FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 AND (?4 IS NULL OR root_task_id=?4)",params![principal.kind,principal.digest,project,root_task_id],|r|r.get(0)).map_err(store_error)?;
        let ids = {
            let mut statement=tx.prepare("SELECT attempt_id FROM wc_team_cli_attempts WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3 AND (?4 IS NULL OR root_task_id=?4) ORDER BY rowid ASC LIMIT ?5 OFFSET ?6").map_err(store_error)?;
            let rows = statement
                .query_map(
                    params![
                        principal.kind,
                        principal.digest,
                        project,
                        root_task_id,
                        limit as i64,
                        offset as i64
                    ],
                    |r| r.get::<_, String>(0),
                )
                .map_err(store_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(store_error)?;
            rows
        };
        let attempts = ids
            .into_iter()
            .map(|id| attempt_on(&tx, principal, &id))
            .collect::<Result<Vec<_>>>()?;
        let next = offset + attempts.len();
        tx.commit().map_err(store_error)?;
        Ok(TeamCliAttemptPage {
            attempts,
            total_count: total,
            next_offset: ((next as i64) < total).then_some(next),
        })
    }
}

#[cfg(test)]
#[path = "team_task_tests.rs"]
mod tests;

#[path = "team_model.rs"]
mod model;
pub use model::{
    AgentTeamPlanView, AgentTeamPolicy, AgentTeamReceipt, AgentTeamReceiptPage, AgentTeamTask,
};

pub(crate) fn reserve_team_cli_attempt_on_connection(
    tx: &Connection,
    principal: &CommunicationPrincipal,
    project: &str,
    root_task_id: &str,
    source_task_id: &str,
    call_index: u32,
    idempotency_key: &str,
    fallback_thinking_profile_id: &str,
    timeout_secs: Option<u64>,
) -> Result<TeamCliAttemptReservation> {
    validate_communication_principal(principal)?;
    let (key, fingerprint) = cli_reservation_identity(
        project,
        root_task_id,
        source_task_id,
        call_index,
        idempotency_key,
        fallback_thinking_profile_id,
        timeout_secs,
    )?;
    owned_task(tx, principal, project, root_task_id)?;
    let source = owned_task(tx, principal, project, source_task_id)?;
    if let Some(attempt) = cli_attempt_replay_on(tx, principal, &key, &fingerprint)? {
        return Ok(TeamCliAttemptReservation {
            attempt,
            created: false,
            replayed: true,
        });
    }
    let plan = live_plan(tx, principal, root_task_id)?;
    if plan.project_id != project {
        return Err(not_found());
    }
    require_live(&source)?;
    let call = planned_call_on(tx, principal, root_task_id, source_task_id, call_index)?;
    let occupied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_team_cli_attempts WHERE root_task_id=?1 AND source_task_id=?2 AND call_index=?3)",params![root_task_id,source_task_id,call_index],|r|r.get(0)).map_err(store_error)?;
    if occupied {
        return Err(error(
            "team_cli_slot_reserved",
            "This planned CLI slot already has its one logical call",
        ));
    }
    capacity(tx, "attempt", MAX_ATTEMPTS)?;
    let now = now_unix_ms();
    let record = TeamCliAttemptRecord {
        attempt_id: new_id(TEAM_CLI_ATTEMPT_ID_PREFIX),
        project_id: project.into(),
        root_task_id: root_task_id.into(),
        source_task_id: source_task_id.into(),
        call_index,
        provider_id: call.provider_id.clone(),
        model: call.model.clone(),
        effort: call.effort.clone(),
        instruction: call.instruction.clone().unwrap_or(source.instruction),
        inputs: call.inputs.clone(),
        fallback_thinking_profile_id: fallback_thinking_profile_id.into(),
        timeout_secs,
        dispatch_state: TeamCliDispatchState::Reserved,
        job_id: None,
        execution_state: None,
        error_code: None,
        result_summary: None,
        execution_binding: None,
        observation_revision: 0,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
    };
    tx.execute(
            "INSERT INTO wc_team_cli_attempts(attempt_id,root_task_id,source_task_id,project_id,owner_principal_kind,owner_principal_digest,call_index,request_key_hash,request_fingerprint,record_json,created_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                record.attempt_id,
                root_task_id,
                source_task_id,
                project,
                principal.kind,
                principal.digest,
                call_index,
                key,
                fingerprint,
                encode(&record)?,
                now
            ],
        )
        .map_err(store_error)?;

    Ok(TeamCliAttemptReservation {
        attempt: record,
        created: true,
        replayed: false,
    })
}

#[path = "team_cli_round.rs"]
pub(crate) mod cli_round;
pub use cli_round::TeamCliRound;
use cli_round::{cli_slot_budget_on, execution_plan_on, planned_call_on};

#[path = "agent_execution_factory.rs"]
mod execution_factory;
pub use execution_factory::*;

fn reconcile_team_cli_attempt_on_connection(
    conn: &Connection,
    principal: &CommunicationPrincipal,
    attempt_id: &str,
    expected_observation_revision: i64,
    observation: TeamCliObservation,
) -> Result<TeamCliAttemptRecord> {
    validate_communication_principal(principal)?;
    if observation
        .error_code
        .as_deref()
        .is_some_and(|v| !identifier(v, 128))
        || observation
            .result_summary
            .as_deref()
            .is_some_and(|v| v.len() > 4096 || v.contains('\0'))
    {
        return Err(invalid("Invalid bounded CLI observation"));
    }
    if !matches!(
        observation.dispatch_state,
        TeamCliDispatchState::Unknown | TeamCliDispatchState::Terminal
    ) || (observation.dispatch_state == TeamCliDispatchState::Terminal
        && !matches!(
            observation.execution_state,
            Some(
                ShellCommandExecutionState::NotStarted
                    | ShellCommandExecutionState::Completed
                    | ShellCommandExecutionState::TimedOut
            )
        ))
        || (observation.dispatch_state == TeamCliDispatchState::Unknown
            && !matches!(
                observation.execution_state,
                None | Some(ShellCommandExecutionState::OutcomeUnknown)
            ))
    {
        return Err(invalid(
            "CLI observation has inconsistent dispatch/execution states",
        ));
    }
    let mut attempt = attempt_on(conn, principal, attempt_id)?;
    if attempt.job_id != observation.job_id {
        return Err(error(
            "team_cli_job_conflict",
            "Observation does not match the bound Job",
        ));
    }
    let identical = attempt.dispatch_state == observation.dispatch_state
        && attempt.execution_state == observation.execution_state
        && attempt.error_code == observation.error_code
        && attempt.result_summary == observation.result_summary;
    if identical {
        return Ok(attempt);
    }
    if attempt.observation_revision != expected_observation_revision {
        return Err(error(
            "team_cli_observation_conflict",
            "CLI observation revision changed",
        ));
    }
    if matches!(
        attempt.dispatch_state,
        TeamCliDispatchState::Reserved | TeamCliDispatchState::Terminal
    ) {
        return Err(error(
            "team_cli_dispatch_conflict",
            "CLI attempt cannot accept this observation",
        ));
    }
    if matches!(
        observation.execution_state,
        Some(ShellCommandExecutionState::Completed | ShellCommandExecutionState::TimedOut)
    ) && attempt.job_id.is_none()
    {
        return Err(invalid("Executed CLI observations require a bound Job"));
    }
    attempt.dispatch_state = observation.dispatch_state;
    attempt.execution_state = observation.execution_state;
    attempt.error_code = observation.error_code;
    attempt.result_summary = observation.result_summary;
    update_attempt(conn, principal, &mut attempt)?;

    Ok(attempt)
}
