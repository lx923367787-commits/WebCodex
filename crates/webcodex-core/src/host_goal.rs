//! Host-driven goals: plans and progress outlive a model conversation.
//! These contracts deliberately do not confer execution or project authority.

use crate::goal::{validate_goal_relative_path, GoalArtifactSnapshot, GoalCriterion};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const HOST_GOAL_MAX_PLAN_BYTES: usize = 128 * 1024;
pub const HOST_GOAL_MAX_STATE_BYTES: usize = 64 * 1024;
pub const HOST_GOAL_MAX_NODES: usize = 64;
pub const HOST_GOAL_MAX_PAGE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGoalPhase {
    Planning,
    AwaitingConfirmation,
    Ready,
    Executing,
    Verifying,
    Paused,
    Blocked,
    Completed,
    Cancelled,
}

impl HostGoalPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Ready => "ready",
            Self::Executing => "executing",
            Self::Verifying => "verifying",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
    pub const fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalPlan {
    pub objective: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    pub deliverables: Vec<String>,
    /// Explicit inputs and artifacts that can affect command verification.
    #[serde(default)]
    pub verification_paths: Vec<String>,
    pub final_criteria: Vec<GoalCriterion>,
    pub nodes: Vec<HostGoalNode>,
    pub state_management: HostGoalStateManagement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalNode {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Command inputs for this node; empty inherits the plan default.
    #[serde(default)]
    pub verification_paths: Vec<String>,
    pub work: String,
    pub outputs: Vec<String>,
    pub criterion_ids: Vec<String>,
    pub acceptance: Vec<GoalCriterion>,
    pub failure_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalStateManagement {
    pub facts: String,
    pub decisions: String,
    pub issues: String,
    pub artifacts: String,
    pub checkpoint_triggers: Vec<String>,
    pub resume_instructions: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostGoalState {
    pub completed_summary: String,
    pub facts: BTreeMap<String, String>,
    pub decisions: Vec<String>,
    pub issues: Vec<String>,
    pub artifact_refs: Vec<String>,
    pub next_step: String,
    pub plan_draft: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalConfirmation {
    pub plan_version: u64,
    pub plan_hash: String,
    /// User's explicit instruction as relayed by the host model. The protocol
    /// cannot independently authenticate a human message; the workflow requires
    /// a new user instruction, while storage enforces its plan version and hash.
    pub user_message: String,
    pub confirmed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalRecord {
    pub goal_id: String,
    #[serde(skip)]
    pub owner_key: String,
    pub project: String,
    pub project_root: String,
    pub objective: String,
    pub phase: HostGoalPhase,
    pub revision: u64,
    pub plan_version: u64,
    pub acceptance_contract_version: u64,
    pub plan_hash: Option<String>,
    pub plan_materialized: bool,
    pub confirmation: Option<HostGoalConfirmation>,
    pub current_node_id: Option<String>,
    pub current_attempt_id: Option<String>,
    pub fence: u64,
    pub completed_node_ids: Vec<String>,
    pub state: HostGoalState,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalPlanVersion {
    pub goal_id: String,
    pub version: u64,
    pub preparation_revision: u64,
    pub acceptance_contract_version: u64,
    /// The host model relays the user's explicit change request; this is an
    /// auditable request, not independent human identity authentication.
    #[serde(default)]
    pub change_request: Option<String>,
    #[serde(default)]
    pub acceptance_changes: Vec<String>,
    pub hash: String,
    pub plan: HostGoalPlan,
    pub materialization: Option<HostGoalMaterialization>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalMaterialization {
    pub json_path: String,
    pub json_hash: String,
    pub markdown_path: String,
    pub markdown_hash: String,
    pub verified_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalExecutionBinding {
    pub goal_id: String,
    pub node_id: String,
    pub attempt_id: String,
    pub plan_version: u64,
    pub fence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGoalAttemptStatus {
    Running,
    Passed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalAttempt {
    pub binding: HostGoalExecutionBinding,
    pub status: HostGoalAttemptStatus,
    pub checks: Vec<HostGoalCheckEvidence>,
    #[serde(default)]
    pub final_checks: Vec<HostGoalCheckEvidence>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGoalEvidenceSource {
    Automated,
    ModelSelfCheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalCheckEvidence {
    pub criterion_id: String,
    pub passed: bool,
    pub source: HostGoalEvidenceSource,
    pub evidence: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<GoalArtifactSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalCheckpoint {
    pub sequence: u64,
    pub goal_id: String,
    pub revision: u64,
    pub plan_version: u64,
    pub state: HostGoalState,
    pub reason: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGoalOperationStatus {
    Intent,
    Running,
    Unknown,
    Succeeded,
    Failed,
    Cancelled,
}

impl HostGoalOperationStatus {
    pub const fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalOperation {
    pub sequence: u64,
    pub operation_id: String,
    pub binding: HostGoalExecutionBinding,
    pub payload_hash: String,
    pub tool_name: String,
    /// Check commands retain the exact input snapshot used for their identity.
    #[serde(default)]
    pub input_manifest: Vec<GoalArtifactSnapshot>,
    pub status: HostGoalOperationStatus,
    pub job_id: Option<String>,
    pub result_summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostGoalOperationResolution {
    pub operation_id: String,
    pub status: HostGoalOperationStatus,
    pub report: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentHostGoalRef {
    pub goal_id: String,
    pub project: String,
    pub project_root: String,
    pub selected_at: i64,
}

pub fn host_goal_bounded(value: &str, max: usize, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(format!(
            "{label} must be nonempty, without NUL and at most {max} bytes"
        ));
    }
    Ok(())
}

fn bounded_list(values: &[String], required: bool, label: &str) -> Result<(), String> {
    if values.len() > 64 || (required && values.is_empty()) {
        return Err(format!(
            "{label} must contain {}..64 entries",
            usize::from(required)
        ));
    }
    for value in values {
        host_goal_bounded(value, 4096, label)?;
    }
    Ok(())
}

pub fn validate_host_goal_criteria(criteria: &[GoalCriterion]) -> Result<(), String> {
    if criteria.is_empty() || criteria.len() > 32 {
        return Err("acceptance requires 1..32 criteria".into());
    }
    let mut ids = BTreeSet::new();
    for criterion in criteria {
        host_goal_bounded(criterion.id(), 64, "criterion id")?;
        host_goal_bounded(criterion.description(), 2048, "criterion description")?;
        if !ids.insert(criterion.id()) {
            return Err("criterion ids must be unique".into());
        }
        match criterion {
            GoalCriterion::FileExists { path, .. } => validate_goal_relative_path(path)?,
            GoalCriterion::FileContains { path, text, .. } => {
                validate_goal_relative_path(path)?;
                host_goal_bounded(text, 16 * 1024, "required text")?;
            }
            GoalCriterion::Command {
                program,
                args,
                cwd,
                timeout_secs,
                ..
            } => {
                host_goal_bounded(program, 1024, "program")?;
                if args.len() > 64 || args.iter().any(|a| a.len() > 4096 || a.contains('\0')) {
                    return Err("command arguments exceed bounded limits".into());
                }
                if let Some(cwd) = cwd {
                    validate_goal_relative_path(cwd)?;
                }
                if !(1..=3600).contains(timeout_secs) {
                    return Err("timeout_secs must be 1..3600".into());
                }
            }
            GoalCriterion::Review { .. } => {}
        }
    }
    Ok(())
}

impl HostGoalPlan {
    pub fn validate(&self) -> Result<(), String> {
        host_goal_bounded(&self.objective, 16 * 1024, "objective")?;
        bounded_list(&self.constraints, false, "constraints")?;
        bounded_list(&self.deliverables, true, "deliverables")?;
        validate_host_goal_criteria(&self.final_criteria)?;
        let mut artifact_paths = BTreeSet::new();
        if self.verification_paths.len() > 64 {
            return Err("verification_paths accepts at most 64 file paths".into());
        }
        for path in &self.verification_paths {
            validate_goal_relative_path(path)?;
            if !artifact_paths.insert(path.replace('\\', "/")) {
                return Err("verification_paths must be unique".into());
            }
        }
        let has_final_commands = self
            .final_criteria
            .iter()
            .any(|criterion| matches!(criterion, GoalCriterion::Command { .. }));
        if has_final_commands && self.verification_paths.is_empty() {
            return Err("Final command acceptance requires plan verification_paths covering its inputs and artifacts".into());
        }
        for node in &self.nodes {
            if node.verification_paths.len() > 64 {
                return Err("Node verification_paths accepts at most 64 paths".into());
            }
            let mut node_paths = BTreeSet::new();
            for path in &node.verification_paths {
                validate_goal_relative_path(path)?;
                let path = path.replace('\\', "/");
                if !node_paths.insert(path.clone()) {
                    return Err("Node verification_paths must be unique".into());
                }
                artifact_paths.insert(path);
            }
            if node
                .acceptance
                .iter()
                .any(|criterion| matches!(criterion, GoalCriterion::Command { .. }))
                && node.verification_paths.is_empty()
                && self.verification_paths.is_empty()
            {
                return Err(format!(
                    "Command node {} requires its own verification_paths or nonempty plan defaults",
                    node.id
                ));
            }
        }
        for criterion in self
            .final_criteria
            .iter()
            .chain(self.nodes.iter().flat_map(|node| node.acceptance.iter()))
        {
            if let GoalCriterion::FileExists { path, .. }
            | GoalCriterion::FileContains { path, .. } = criterion
            {
                artifact_paths.insert(path.replace('\\', "/"));
            }
        }
        if artifact_paths.len() > 64 {
            return Err(
                "The complete plan may reference at most 64 distinct acceptance file paths".into(),
            );
        }
        if self.nodes.is_empty() || self.nodes.len() > HOST_GOAL_MAX_NODES {
            return Err("a serial plan requires 1..64 nodes".into());
        }
        let criteria: BTreeSet<_> = self.final_criteria.iter().map(GoalCriterion::id).collect();
        let mut covered = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            host_goal_bounded(&node.id, 64, "node id")?;
            if !ids.insert(&node.id) {
                return Err("node ids must be unique".into());
            }
            host_goal_bounded(&node.title, 512, "node title")?;
            host_goal_bounded(&node.work, 16 * 1024, "node work")?;
            host_goal_bounded(&node.failure_action, 4096, "failure action")?;
            bounded_list(&node.inputs, false, "node inputs")?;
            bounded_list(&node.outputs, true, "node outputs")?;
            bounded_list(&node.criterion_ids, true, "criterion coverage")?;
            validate_host_goal_criteria(&node.acceptance)?;
            for id in &node.criterion_ids {
                if !criteria.contains(id.as_str()) {
                    return Err("node references an unknown final criterion".into());
                }
                covered.insert(id.as_str());
            }
        }
        if covered != criteria {
            return Err("every final criterion must be covered by a node".into());
        }
        let state = &self.state_management;
        for (name, value) in [
            ("facts policy", &state.facts),
            ("decisions policy", &state.decisions),
            ("issues policy", &state.issues),
            ("artifacts policy", &state.artifacts),
            ("resume instructions", &state.resume_instructions),
        ] {
            host_goal_bounded(value, 4096, name)?;
        }
        bounded_list(&state.checkpoint_triggers, true, "checkpoint triggers")?;
        let shared = serde_json::json!({
            "objective":self.objective,"constraints":self.constraints,"deliverables":self.deliverables,
            "verification_paths":self.verification_paths,"final_criteria":self.final_criteria,
            "state_management":self.state_management,
        });
        let shared_bytes = serde_json::to_vec(&shared)
            .map_err(|e| e.to_string())?
            .len();
        if shared_bytes > 8 * 1024 {
            return Err("Shared plan requirements exceed 8 KiB; reference supporting documents and retain concise acceptance and state rules".into());
        }
        for node in &self.nodes {
            let node_bytes = serde_json::to_vec(node).map_err(|e| e.to_string())?.len();
            if node_bytes > 8 * 1024 || shared_bytes + node_bytes > 12 * 1024 {
                return Err(format!("Node {} exceeds the recovery budget (node 8 KiB, shared requirements plus node 12 KiB); split the node or reference external supporting documents",node.id));
            }
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > HOST_GOAL_MAX_PLAN_BYTES {
            return Err("plan exceeds 128 KiB".into());
        }
        Ok(())
    }
}

impl HostGoalState {
    pub fn validate(&self) -> Result<(), String> {
        if self.facts.len() > 128
            || self.facts.iter().any(|(k, v)| {
                k.is_empty()
                    || k.len() > 128
                    || v.len() > 4096
                    || k.contains('\0')
                    || v.contains('\0')
            })
        {
            return Err("facts exceed bounded limits".into());
        }
        bounded_list(&self.decisions, false, "decisions")?;
        bounded_list(&self.issues, false, "issues")?;
        bounded_list(&self.artifact_refs, false, "artifact refs")?;
        if self.completed_summary.len() > 16 * 1024 || self.next_step.len() > 4096 {
            return Err("summary or next step exceeds bounded limits".into());
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > HOST_GOAL_MAX_STATE_BYTES {
            return Err("goal state exceeds 64 KiB".into());
        }
        Ok(())
    }
}

impl HostGoalRecord {
    pub fn confirmed(&self) -> bool {
        self.plan_materialized
            && self.confirmation.as_ref().is_some_and(|c| {
                c.plan_version == self.plan_version && Some(&c.plan_hash) == self.plan_hash.as_ref()
            })
    }
}
