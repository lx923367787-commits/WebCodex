//! Checked ordinary-program admission into the existing durable Job protocol.
use crate::jobs::{ensure_dispatch_supported_locked, ensure_queue_capacity_locked, job_view};
use crate::requests::{enqueue_agent_program_request_locked, notify_runner_locked};
use crate::state::{
    JobLifecycleState, JobObservationState, JobRecoveryState, RunnerRegistryInner, ShellJobRecord,
    ShellJobVisibility,
};
use crate::{now_ts, RunnerAccess, RunnerFeature, RunnerRegistry};
use webcodex_core::agent_program::{
    AgentProgramBinding, AgentProgramStart, AGENT_PROGRAM_START_KIND,
};
use webcodex_core::runner_operation::{RunnerInvocationMetadata, RunnerOperation};
use webcodex_core::runner_protocol::{RunnerRequest, ShellJobInfo};
#[cfg(test)]
#[path = "tests/agent_program.rs"]
mod tests;

pub trait AgentProgramStoreAdmission: Send + Sync {
    /// Must atomically validate the current Agent/Task and original command,
    /// then acquire its durable one-shot dispatch fence. No model can implement it.
    fn claim_dispatch(&self, start: &AgentProgramStart) -> Result<bool, String>;
}
pub(super) struct AgentProgramEnqueuePermit(());
pub struct VerifiedAgentProgramObservation {
    binding: AgentProgramBinding,
    job: ShellJobInfo,
}
impl VerifiedAgentProgramObservation {
    pub fn binding(&self) -> &AgentProgramBinding {
        &self.binding
    }
    pub fn job(&self) -> &ShellJobInfo {
        &self.job
    }
}

pub(super) fn dispatch_fence(
    inner: &RunnerRegistryInner,
    operation: &RunnerOperation,
) -> Result<(), String> {
    if let RunnerOperation::AgentBrowser(query) = operation {
        let runner = inner
            .runners
            .get(&query.target.client_id)
            .ok_or("agent_browser_runner_unavailable")?;
        if runner.runner_instance_id != query.target.runner_instance_id {
            return Err("agent_browser_stale_target".into());
        }
        return Ok(());
    }
    if let RunnerOperation::AgentProgramInspect(query) = operation {
        let runner = inner
            .runners
            .get(&query.target.client_id)
            .ok_or("agent_program_runner_unavailable")?;
        if runner.runner_instance_id != query.target.runner_instance_id {
            return Err("agent_program_inspect_stale_target".into());
        }
        return Ok(());
    }
    let RunnerOperation::Job(job) = operation else {
        return Ok(());
    };
    let Some(binding) = job.context().and_then(|c| c.agent_program.as_ref()) else {
        return Ok(());
    };
    let runner = inner
        .runners
        .get(&binding.target.client_id)
        .ok_or("agent_program_runner_unavailable")?;
    if runner.runner_instance_id != binding.target.runner_instance_id
        || binding.expires_at_unix_ms <= chrono::Utc::now().timestamp_millis()
    {
        return Err("agent_program_stale_target".into());
    }
    if !runner
        .runner_features
        .supports(RunnerFeature::AgentProgramExecutionV1)
    {
        return Err("agent_program_backend_unavailable".into());
    }
    let project = binding
        .workspace_project
        .strip_prefix(&format!("agent:{}:", binding.target.client_id))
        .ok_or("agent_program_workspace_runner_mismatch")?;
    if !runner
        .projects
        .iter()
        .any(|p| p.id == project && !p.disabled && p.path == binding.workspace_root)
    {
        return Err("agent_program_workspace_changed".into());
    }
    if !runner.agent_execution_resources.iter().any(|r| {
        r.resource_set_id == binding.resource_set_id
            && r.manifest_hash == binding.resource_manifest_hash
            && r.provider.is_none()
            && r.reviewed_controls.is_none()
            && r.project_workspace_copy
    }) {
        return Err("agent_program_resource_changed".into());
    }
    Ok(())
}

impl RunnerRegistry {
    pub async fn inspect_agent_program(
        &self,
        access: &RunnerAccess,
        binding: &AgentProgramBinding,
    ) -> Result<webcodex_core::agent_program::AgentProgramInspection, String> {
        use webcodex_core::agent_program::{AgentProgramInspect, AgentProgramInspection};
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let request_id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut inner = tokio::time::timeout_at(deadline, self.inner.lock())
                .await
                .map_err(|_| "agent_program_inspection_timeout")?;
            let runner = inner
                .runners
                .get(&binding.target.client_id)
                .ok_or("agent_program_runner_unavailable")?;
            crate::access_control::assert_runner_access(Some(access), runner)?;
            if !runner
                .runner_features
                .supports(RunnerFeature::AgentProgramExecutionV1)
            {
                return Err("agent_program_backend_unavailable".into());
            }
            let target = webcodex_core::runner_protocol::AgentExecutionTarget {
                client_id: binding.target.client_id.clone(),
                runner_instance_id: runner.runner_instance_id.clone(),
            };
            let request = RunnerRequest::from_operation(
                RunnerInvocationMetadata {
                    request_id: request_id.clone(),
                    client_id: target.client_id.clone(),
                    requested_by: "verified-agent-program-observer".into(),
                    created_at: now_ts(),
                },
                RunnerOperation::AgentProgramInspect(AgentProgramInspect {
                    target,
                    binding: binding.clone(),
                }),
            )?;
            enqueue_agent_program_request_locked(
                self.telemetry.as_ref(),
                &mut inner,
                &binding.target.client_id,
                request_id,
                request,
                Some(sender),
                None,
                &AgentProgramEnqueuePermit(()),
            )?;
            notify_runner_locked(&inner, &binding.target.client_id);
        }
        let result = tokio::time::timeout_at(deadline, receiver)
            .await
            .map_err(|_| "agent_program_inspection_timeout")?
            .map_err(|_| "agent_program_inspection_transport_closed")?;
        if !result.success || result.exit_code != Some(0) {
            return Err("agent_program_inspection_failed".into());
        }
        let raw = result
            .stdout
            .as_deref()
            .filter(|s| s.len() <= 1024 * 1024)
            .ok_or("agent_program_inspection_invalid_response")?;
        let inspected: AgentProgramInspection =
            serde_json::from_str(raw).map_err(|_| "agent_program_inspection_invalid_response")?;
        if inspected.binding_hash != binding.identity_hash()?
            || inspected.job.as_ref().is_some_and(|j| {
                j.context.agent_program.as_ref() != Some(binding)
                    || j.job_id != binding.job_id
                    || j.request_id != binding.start_request_id
            })
            || inspected.service.as_ref().is_some_and(|s| {
                Some(s.port) != binding.local_service_port
                    || s.process_id == 0
                    || s.observed_at_unix_ms <= 0
            })
        {
            return Err("agent_program_inspection_identity_mismatch".into());
        }
        Ok(inspected)
    }
    pub async fn agent_program_observation(
        &self,
        access: &RunnerAccess,
        binding: &AgentProgramBinding,
    ) -> Result<VerifiedAgentProgramObservation, String> {
        let job = self.observe_agent_program(access, binding).await?;
        Ok(VerifiedAgentProgramObservation {
            binding: binding.clone(),
            job,
        })
    }
    pub async fn stop_agent_program(
        &self,
        access: &RunnerAccess,
        binding: &AgentProgramBinding,
    ) -> Result<ShellJobInfo, String> {
        self.stop_job_for_auth_inner(
            Some(access),
            &binding.job_id,
            "verified-agent-program-stop".into(),
            Some(binding),
        )
        .await
    }
    pub async fn dispatch_agent_program(
        &self,
        access: &RunnerAccess,
        start: AgentProgramStart,
        admission: &dyn AgentProgramStoreAdmission,
    ) -> Result<ShellJobInfo, String> {
        start.validate()?;
        let b = &start.binding;
        let mut inner = self.inner.lock().await;
        crate::agent_execution::current_target(&inner, access, &b.target)?;
        if let Some(existing) = inner.jobs_by_id.get(&b.job_id) {
            if existing.agent_program.as_ref() != Some(b) {
                return Err("agent_program_job_identity_conflict".into());
            }
            return Ok(job_view(existing));
        }
        if inner.pending_by_id.contains_key(&b.start_request_id)
            || inner.request_to_job.contains_key(&b.start_request_id)
        {
            return Err("agent_program_request_identity_conflict".into());
        }
        let operation = RunnerOperation::Job(start.clone().into_job());
        dispatch_fence(&inner, &operation)?;
        ensure_dispatch_supported_locked(&inner, &b.target.client_id).map_err(String::from)?;
        ensure_queue_capacity_locked(&inner, &b.target.client_id).map_err(String::from)?;
        let context = start.context();
        let request = RunnerRequest::from_operation(
            RunnerInvocationMetadata {
                request_id: b.start_request_id.clone(),
                client_id: b.target.client_id.clone(),
                requested_by: "verified-agent-program".into(),
                created_at: now_ts(),
            },
            operation,
        )?;
        if !admission.claim_dispatch(&start)? {
            return Err("agent_program_original_job_reconciliation_required".into());
        }
        enqueue_agent_program_request_locked(
            self.telemetry.as_ref(),
            &mut inner,
            &b.target.client_id,
            b.start_request_id.clone(),
            request,
            None,
            Some(b.job_id.clone()),
            &AgentProgramEnqueuePermit(()),
        )?;
        let job = ShellJobRecord {
            agent_program: Some(b.clone()),
            agent_execution: None,
            document: None,
            job_id: b.job_id.clone(),
            request_id: Some(b.start_request_id.clone()),
            client_id: b.target.client_id.clone(),
            auth_group: inner.runners[&b.target.client_id].auth_group.clone(),
            runner_instance_id: b.target.runner_instance_id.clone(),
            kind: AGENT_PROGRAM_START_KIND.into(),
            project_id: None,
            session_id: None,
            ssh_resource: None,
            cwd: None,
            project_cwd: None,
            purpose: context.purpose,
            shell: context.shell,
            command_preview: context.command_preview,
            detached_idempotency_intent: None,
            lifecycle: JobLifecycleState::Queued,
            created_at: now_ts(),
            started_at: None,
            ended_at: None,
            exit_code: None,
            duration_ms: None,
            stdout: Default::default(),
            stderr: Default::default(),
            error: None,
            command_execution_state: None,
            structured_execution: context.structured_execution,
            codex: None,
            validation_steps: vec![],
            validation: None,
            validation_progress: None,
            activity: None,
            visibility: ShellJobVisibility::HiddenUntilHandoff,
            last_update_seq: 0,
            recovery: JobRecoveryState::default(),
            observation: JobObservationState::new(self.observation_epoch.clone()),
        };
        inner
            .request_to_job
            .insert(b.start_request_id.clone(), b.job_id.clone());
        inner.jobs_by_id.insert(b.job_id.clone(), job);
        notify_runner_locked(&inner, &b.target.client_id);
        Ok(job_view(inner.jobs_by_id.get(&b.job_id).unwrap()))
    }
    pub async fn observe_agent_program(
        &self,
        access: &RunnerAccess,
        binding: &AgentProgramBinding,
    ) -> Result<ShellJobInfo, String> {
        let inner = self.inner.lock().await;
        let job = inner
            .jobs_by_id
            .get(&binding.job_id)
            .ok_or("agent_program_job_unavailable")?;
        if job.agent_program.as_ref() != Some(binding) {
            return Err("agent_program_job_identity_conflict".into());
        }
        if !crate::access_control::job_visible_to_access(Some(access), &inner, job) {
            return Err("agent_program_job_access_denied".into());
        }
        Ok(job_view(job))
    }
}

#[path = "agent_browser.rs"]
mod browser;
pub use browser::{AgentBrowserStoreAdmission, VerifiedAgentBrowserReceipt};
