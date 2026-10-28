//! Ordinary project-program execution. No model provider, CLI slot, delegated
//! worker, or model-call budget is represented by this contract.
use crate::runner_protocol::{
    AgentExecutionTarget, AgentRunRef, ShellJobContext, ShellJobStructuredExecutionMetadata,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const AGENT_PROGRAM_START_KIND: &str = "start_agent_program";
pub const AGENT_PROGRAM_INSPECT_KIND: &str = "inspect_agent_program";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProgramInspect {
    pub target: AgentExecutionTarget,
    pub binding: AgentProgramBinding,
}
impl AgentProgramInspect {
    pub fn validate(&self) -> Result<(), String> {
        self.target.validate()?;
        self.binding.validate()?;
        if self.target.client_id != self.binding.target.client_id {
            return Err("agent_program_inspect_target_mismatch".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProgramService {
    pub port: u16,
    pub process_id: u32,
    pub observed_at_unix_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProgramInspection {
    pub binding_hash: String,
    pub state: String,
    pub job: Option<crate::runner_protocol::ShellJobSnapshot>,
    pub service: Option<AgentProgramService>,
}
#[cfg(test)]
#[path = "agent_program_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProgramBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_service_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from_program: Option<String>,
    pub version: u32,
    pub program_id: String,
    pub enrollment_id: String,
    pub task_id: String,
    pub run: AgentRunRef,
    pub project: String,
    pub target: AgentExecutionTarget,
    pub job_id: String,
    pub start_request_id: String,
    /// Trusted registered source, rechecked by Runner before materialization.
    pub workspace_project: String,
    pub workspace_root: String,
    pub relative_cwd: String,
    pub resource_set_id: String,
    pub resource_manifest_hash: String,
    pub launch_intent_hash: String,
    pub expires_at_unix_ms: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProgramStart {
    pub binding: AgentProgramBinding,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub timeout_secs: u64,
}
impl std::fmt::Debug for AgentProgramStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentProgramStart")
            .field("binding", &self.binding)
            .field("arg_count", &self.args.len())
            .field("stdin_present", &self.stdin.is_some())
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn validate_relative_cwd(path: &str) -> Result<(), String> {
    if path == "." {
        return Ok(());
    }
    if path.len() > 2048
        || path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.ends_with([' ', '.']))
    {
        return Err("agent_program_invalid_cwd".into());
    }
    Ok(())
}

impl AgentProgramBinding {
    pub fn validate(&self) -> Result<(), String> {
        self.run.validate()?;
        self.target.validate()?;
        validate_relative_cwd(&self.relative_cwd)?;
        if self.version != 1
            || self.local_service_port.is_some_and(|p| p < 1024)
            || self
                .resume_from_program
                .as_deref()
                .is_some_and(|p| !id(p) || p == self.program_id)
            || self.expires_at_unix_ms <= 0
            || [
                &self.program_id,
                &self.enrollment_id,
                &self.task_id,
                &self.project,
                &self.job_id,
                &self.start_request_id,
                &self.workspace_project,
                &self.resource_set_id,
            ]
            .iter()
            .any(|s| !id(s))
            || !self.workspace_project.starts_with("agent:")
            || self.workspace_root.is_empty()
            || self.workspace_root.len() > 4096
            || self.workspace_root.chars().any(char::is_control)
            || !digest(&self.resource_manifest_hash)
            || !digest(&self.launch_intent_hash)
        {
            return Err("agent_program_invalid_binding".into());
        }
        Ok(())
    }
    /// Resource identity excludes mutable process state. A launch never selects
    /// another Agent's private namespace or changes an already frozen source.
    pub fn identity_hash(&self) -> Result<String, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| "agent_program_encoding_failed")?;
        let ordinary = format!("{:x}", Sha256::digest(bytes));
        Ok(ordinary)
    }
}
impl AgentProgramStart {
    pub fn from_job(
        operation: &crate::runner_operation::RunnerJobProcessOperation,
    ) -> Result<Self, String> {
        let binding = operation
            .context
            .agent_program
            .clone()
            .ok_or("agent_program_binding_required")?;
        let start = Self {
            binding,
            args: operation.process.args.clone(),
            stdin: operation.stdin.clone(),
            timeout_secs: operation.timeout_secs,
        };
        start.validate()?;
        if operation.job_id != start.binding.job_id
            || operation.cwd.is_some()
            || operation.process.executable != start.binding.resource_set_id
            || operation.context != start.context()
        {
            return Err("agent_program_context_mismatch".into());
        }
        Ok(start)
    }
    pub fn into_job(self) -> crate::runner_operation::RunnerJobOperation {
        let context = self.context();
        crate::runner_operation::RunnerJobOperation::StartProcess(
            crate::runner_operation::RunnerJobProcessOperation {
                job_id: self.binding.job_id.clone(),
                cwd: None,
                process: crate::runner_protocol::ShellProcessArgv {
                    executable: self.binding.resource_set_id.clone(),
                    args: self.args,
                },
                stdin: self.stdin,
                timeout_secs: self.timeout_secs,
                context,
            },
        )
    }
    pub fn compute_launch_hash(&self) -> Result<String, String> {
        if self.args.len() > 256
            || self
                .args
                .iter()
                .any(|a| a.contains('\0') || a.len() > 65536)
            || self.args.iter().map(String::len).sum::<usize>() > 262144
            || self
                .stdin
                .as_ref()
                .is_some_and(|s| s.len() > 1024 * 1024 || s.contains('\0'))
            || !(1..=86400).contains(&self.timeout_secs)
        {
            return Err("agent_program_invalid_launch".into());
        }
        let bytes = serde_json::to_vec(&(
            "webcodex.agent.program.launch.v1",
            &self.binding.resource_set_id,
            &self.binding.workspace_project,
            &self.binding.workspace_root,
            &self.binding.relative_cwd,
            &self.args,
            &self.stdin,
            self.timeout_secs,
        ))
        .map_err(|_| "agent_program_encoding_failed")?;
        let ordinary = format!("{:x}", Sha256::digest(bytes));
        if self.binding.local_service_port.is_none() && self.binding.resume_from_program.is_none() {
            return Ok(ordinary);
        }
        let bytes = serde_json::to_vec(&(
            ordinary,
            &self.binding.local_service_port,
            &self.binding.resume_from_program,
        ))
        .map_err(|_| "agent_program_encoding_failed")?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
    pub fn validate(&self) -> Result<(), String> {
        self.binding.validate()?;
        if self.compute_launch_hash() != Ok(self.binding.launch_intent_hash.clone()) {
            return Err("agent_program_launch_hash_mismatch".into());
        }
        Ok(())
    }
    pub fn context(&self) -> ShellJobContext {
        ShellJobContext {
            agent_program: Some(self.binding.clone()),
            agent_execution: None,
            document: None,
            runtime_project_id: None,
            workflow_session_id: None,
            ssh_resource: None,
            project_cwd: None,
            cwd: None,
            purpose: Some("operation".into()),
            shell: Some("isolated_program".into()),
            command_preview: "isolated project program".into(),
            validation_steps: vec![],
            validation: None,
            structured_execution: Some(ShellJobStructuredExecutionMetadata {
                execution_source: AGENT_PROGRAM_START_KIND.into(),
                language: None,
                script_bytes: None,
                arg_count: self.args.len(),
                stdin_present: self.stdin.is_some(),
                validation_identity: None,
                validation_tool: None,
                assertion_name: None,
            }),
        }
    }
}
