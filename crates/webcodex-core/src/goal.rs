//! Durable goal contracts. Finishing an executor turn is not goal acceptance.

use crate::coding_agent::{CodingAgentConfigValue, CodingAgentDispatchState, CodingAgentRunState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const GOAL_MAX_CRITERIA: usize = 32;
pub const GOAL_MAX_ARTIFACTS: usize = 64;
pub const GOAL_MAX_OUTPUT_BYTES: usize = 32 * 1024;
pub const GOAL_MAX_EVIDENCE_BYTES: usize = 8 * 1024;
pub const GOAL_MAX_FEEDBACK_BYTES: usize = 16 * 1024;
pub const GOAL_MAX_RECORD_BYTES: usize = 24 * 1024 * 1024;
pub const GOAL_MAX_LIST_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalSpec {
    pub project: String,
    pub provider_id: String,
    pub objective: String,
    pub criteria: Vec<GoalCriterion>,
    pub artifact_paths: Vec<String>,
    #[serde(default)]
    pub reviewer_provider_id: Option<String>,
    #[serde(default)]
    pub limits: GoalLimits,
    #[serde(default)]
    pub config: BTreeMap<String, CodingAgentConfigValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GoalCriterion {
    FileExists {
        id: String,
        description: String,
        path: String,
    },
    FileContains {
        id: String,
        description: String,
        path: String,
        text: String,
    },
    Command {
        id: String,
        description: String,
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        timeout_secs: u64,
    },
    Review {
        id: String,
        description: String,
    },
}

impl GoalCriterion {
    pub fn id(&self) -> &str {
        match self {
            Self::FileExists { id, .. }
            | Self::FileContains { id, .. }
            | Self::Command { id, .. }
            | Self::Review { id, .. } => id,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::FileExists { description, .. }
            | Self::FileContains { description, .. }
            | Self::Command { description, .. }
            | Self::Review { description, .. } => description,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoalLimits {
    pub max_rounds: u32,
    pub total_timeout_secs: u64,
    pub max_no_progress_rounds: u32,
    pub run_timeout_secs: u64,
}

impl Default for GoalLimits {
    fn default() -> Self {
        Self {
            max_rounds: 5,
            total_timeout_secs: 1800,
            max_no_progress_rounds: 3,
            run_timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    Queued,
    Running,
    Checking,
    Paused,
    Blocked,
    Cancelled,
    Completed,
}

impl GoalState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Checking => "checking",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
        }
    }

    pub const fn terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed)
    }

    pub const fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Checking)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalVerdict {
    Passed,
    NeedsWork,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalCheckEvidence {
    pub criterion_id: String,
    pub passed: bool,
    pub evidence: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalArtifactSnapshot {
    pub path: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalExecutionTarget {
    pub project_root: String,
    pub runner_instance_id: String,
    pub provider_instance_id: String,
    pub reviewer_provider_instance_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalRound {
    pub number: u32,
    /// Known failures enter a new bounded repair round; unknown effects retain
    /// their original execution identity until reconciliation succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_error: Option<String>,
    #[serde(default)]
    pub observation_failures: u32,
    pub execution_run_id: Option<String>,
    pub review_run_id: Option<String>,
    pub execution_dispatch_state: CodingAgentDispatchState,
    pub review_dispatch_state: CodingAgentDispatchState,
    pub execution_state: Option<CodingAgentRunState>,
    pub review_state: Option<CodingAgentRunState>,
    pub execution_last_sequence: u64,
    pub review_last_sequence: u64,
    #[serde(default)]
    pub review_has_message_phases: bool,
    pub execution_output: String,
    pub review_output: String,
    pub checks: Vec<GoalCheckEvidence>,
    pub manifest_before_checks: Vec<GoalArtifactSnapshot>,
    pub manifest_final: Vec<GoalArtifactSnapshot>,
    pub verdict: Option<GoalVerdict>,
    pub feedback: String,
    pub active_check_job_id: Option<String>,
    pub active_check_id: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl GoalRound {
    pub fn new(number: u32, now_ms: i64) -> Self {
        Self {
            number,
            repair_error: None,
            observation_failures: 0,
            execution_run_id: None,
            review_run_id: None,
            execution_dispatch_state: CodingAgentDispatchState::NotStarted,
            review_dispatch_state: CodingAgentDispatchState::NotStarted,
            execution_state: None,
            review_state: None,
            execution_last_sequence: 0,
            review_last_sequence: 0,
            review_has_message_phases: false,
            execution_output: String::new(),
            review_output: String::new(),
            checks: Vec::new(),
            manifest_before_checks: Vec::new(),
            manifest_final: Vec::new(),
            verdict: None,
            feedback: String::new(),
            active_check_job_id: None,
            active_check_id: None,
            started_at: now_ms,
            finished_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalRecord {
    pub goal_id: String,
    pub revision: u64,
    pub spec: GoalSpec,
    pub state: GoalState,
    pub target: Option<GoalExecutionTarget>,
    pub rounds: Vec<GoalRound>,
    pub created_at: i64,
    pub deadline_at: i64,
    pub updated_at: i64,
    pub reason: Option<String>,
    pub cancel_requested: bool,
    /// Persisted separately by the store; never sent to a model or client.
    #[serde(skip)]
    pub owner_key: String,
    /// Persisted separately by the store; identifies the controlling process.
    #[serde(skip)]
    pub worker_epoch: Option<String>,
}

fn bounded(value: &str, max: usize, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(format!(
            "{label} must be nonempty, contain no NUL, and be at most {max} bytes"
        ));
    }
    Ok(())
}

/// Lexical validation is OS-neutral. Runtime must also confine canonical paths
/// to the pinned project root, including symlinks and other filesystem aliases.
pub fn validate_goal_relative_path(path: &str) -> Result<(), String> {
    bounded(path, 1024, "relative path")?;
    if path.starts_with(['/', '\\'])
        || path.contains(':')
        || path
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return Err(
            "goal paths must be relative project paths without empty, '.' or '..' components"
                .into(),
        );
    }
    Ok(())
}

impl GoalSpec {
    pub fn validate(&self) -> Result<(), String> {
        bounded(&self.project, 512, "project")?;
        bounded(&self.provider_id, 64, "provider_id")?;
        bounded(&self.objective, 64 * 1024, "objective")?;
        if let Some(value) = &self.reviewer_provider_id {
            bounded(value, 64, "reviewer_provider_id")?;
        }
        if !(1..=GOAL_MAX_CRITERIA).contains(&self.criteria.len()) {
            return Err("criteria must contain 1..32 acceptance requirements".into());
        }
        if !(1..=GOAL_MAX_ARTIFACTS).contains(&self.artifact_paths.len()) {
            return Err("artifact_paths must contain 1..64 final deliverables".into());
        }
        let mut ids = BTreeSet::new();
        for criterion in &self.criteria {
            bounded(criterion.id(), 64, "criterion id")?;
            bounded(criterion.description(), 2048, "criterion description")?;
            if !ids.insert(criterion.id()) {
                return Err("criterion ids must be unique".into());
            }
            match criterion {
                GoalCriterion::FileExists { path, .. } => validate_goal_relative_path(path)?,
                GoalCriterion::FileContains { path, text, .. } => {
                    validate_goal_relative_path(path)?;
                    bounded(text, 16 * 1024, "required file text")?;
                }
                GoalCriterion::Command {
                    program,
                    args,
                    cwd,
                    timeout_secs,
                    ..
                } => {
                    bounded(program, 1024, "check program")?;
                    if args.len() > 64
                        || args
                            .iter()
                            .any(|arg| arg.len() > 4096 || arg.contains('\0'))
                    {
                        return Err(
                            "check command accepts at most 64 arguments of 4096 bytes without NUL"
                                .into(),
                        );
                    }
                    if let Some(cwd) = cwd {
                        validate_goal_relative_path(cwd)?;
                    }
                    if !(1..=3600).contains(timeout_secs) {
                        return Err("check timeout_secs must be 1..3600".into());
                    }
                }
                GoalCriterion::Review { .. } if self.reviewer_provider_id.is_none() => {
                    return Err("review criteria require reviewer_provider_id".into());
                }
                GoalCriterion::Review { .. } => {}
            }
        }
        let mut paths = BTreeSet::new();
        for path in &self.artifact_paths {
            validate_goal_relative_path(path)?;
            if !paths.insert(path.replace('\\', "/")) {
                return Err("artifact_paths must be unique".into());
            }
        }
        let limits = &self.limits;
        if !(1..=50).contains(&limits.max_rounds)
            || !(1..=50).contains(&limits.max_no_progress_rounds)
            || !(1..=86400).contains(&limits.total_timeout_secs)
            || !(1..=3600).contains(&limits.run_timeout_secs)
        {
            return Err("limits must use max_rounds/max_no_progress_rounds 1..50, total_timeout_secs 1..86400, run_timeout_secs 1..3600".into());
        }
        if self.config.len() > 32 {
            return Err("config accepts at most 32 options".into());
        }
        for (key, value) in &self.config {
            bounded(key, 128, "config key")?;
            if value.serialized_len() > 4096 {
                return Err("config values must be at most 4096 bytes".into());
            }
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 24 * 1024 {
            return Err("The complete goal specification must be at most 24 KiB so execution and review prompts retain space for evidence".into());
        }
        Ok(())
    }
}

impl GoalRecord {
    /// Structural completion gate. Evidence is produced by the trusted runtime;
    /// this rejects missing/failed evidence and stale manifest snapshots.
    pub fn validate(&self) -> Result<(), String> {
        self.spec.validate()?;
        bounded(&self.goal_id, 96, "goal_id")?;
        if self.rounds.len() > self.spec.limits.max_rounds as usize {
            return Err("goal round limit exceeded".into());
        }
        if self
            .reason
            .as_ref()
            .is_some_and(|v| v.len() > GOAL_MAX_FEEDBACK_BYTES)
        {
            return Err("goal reason is too large".into());
        }
        if let Some(target) = &self.target {
            bounded(&target.project_root, 4096, "target project_root")?;
            bounded(&target.runner_instance_id, 128, "target runner_instance_id")?;
            bounded(
                &target.provider_instance_id,
                128,
                "target provider_instance_id",
            )?;
            if let Some(id) = &target.reviewer_provider_instance_id {
                bounded(id, 128, "target reviewer_provider_instance_id")?;
            }
        }
        let ids: BTreeSet<_> = self.spec.criteria.iter().map(GoalCriterion::id).collect();
        for (index, round) in self.rounds.iter().enumerate() {
            if round.number as usize != index + 1 {
                return Err("goal rounds must be consecutive".into());
            }
            if round.execution_output.len() > GOAL_MAX_OUTPUT_BYTES
                || round.review_output.len() > GOAL_MAX_OUTPUT_BYTES
                || round.feedback.len() > GOAL_MAX_FEEDBACK_BYTES
            {
                return Err("goal round output exceeds its bound".into());
            }
            if round.checks.len() > ids.len() {
                return Err("too many check results".into());
            }
            for id in [
                &round.execution_run_id,
                &round.review_run_id,
                &round.active_check_job_id,
                &round.active_check_id,
            ]
            .into_iter()
            .flatten()
            {
                bounded(id, 128, "run/check identifier")?;
            }
            let mut checked = BTreeSet::new();
            for check in &round.checks {
                if !ids.contains(check.criterion_id.as_str())
                    || !checked.insert(check.criterion_id.as_str())
                {
                    return Err("checks must identify distinct acceptance criteria".into());
                }
                if check.evidence.len() > GOAL_MAX_EVIDENCE_BYTES
                    || check
                        .error
                        .as_ref()
                        .is_some_and(|v| v.len() > GOAL_MAX_EVIDENCE_BYTES)
                {
                    return Err("check evidence exceeds its bound".into());
                }
            }
            for manifest in [&round.manifest_before_checks, &round.manifest_final] {
                if manifest.len() > GOAL_MAX_ARTIFACTS + GOAL_MAX_CRITERIA {
                    return Err("artifact manifest exceeds its bound".into());
                }
                let mut paths = BTreeSet::new();
                for artifact in manifest {
                    validate_goal_relative_path(&artifact.path)?;
                    if !paths.insert(&artifact.path) {
                        return Err("duplicate manifest path".into());
                    }
                    if artifact.sha256.as_ref().is_some_and(|hash| {
                        hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                    }) {
                        return Err("artifact sha256 must contain 64 hexadecimal characters".into());
                    }
                }
            }
        }
        if self.state == GoalState::Completed {
            if self.cancel_requested || self.target.is_none() {
                return Err("cancelled or unbound goals cannot complete".into());
            }
            let last = self
                .rounds
                .last()
                .ok_or("completed goal requires a checked round")?;
            if last.verdict != Some(GoalVerdict::Passed)
                || last.finished_at.is_none()
                || last.checks.len() != ids.len()
                || last.checks.iter().any(|check| {
                    !check.passed || check.error.is_some() || check.evidence.trim().is_empty()
                })
            {
                return Err(
                    "completed goal requires passing evidence for every acceptance criterion"
                        .into(),
                );
            }
            let paths: BTreeSet<_> = last
                .manifest_final
                .iter()
                .map(|artifact| artifact.path.as_str())
                .collect();
            let mut expected: BTreeSet<_> = self
                .spec
                .artifact_paths
                .iter()
                .map(String::as_str)
                .collect();
            for criterion in &self.spec.criteria {
                if let GoalCriterion::FileExists { path, .. }
                | GoalCriterion::FileContains { path, .. } = criterion
                {
                    expected.insert(path.as_str());
                }
            }
            if paths != expected
                || last
                    .manifest_final
                    .iter()
                    .any(|artifact| artifact.sha256.is_none())
                || last.manifest_before_checks != last.manifest_final
            {
                return Err(
                    "completed goal requires existing deliverables unchanged since checks".into(),
                );
            }
            if last.active_check_job_id.is_some() || last.active_check_id.is_some() {
                return Err("completed goal cannot have an active check".into());
            }
        }
        Ok(())
    }
}
