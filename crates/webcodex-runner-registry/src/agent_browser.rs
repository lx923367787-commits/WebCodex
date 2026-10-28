use super::*;
use webcodex_core::agent_browser::{
    AgentBrowserAction, AgentBrowserImage, AgentBrowserReceipt, AgentBrowserRequest,
    AgentBrowserResponse, MAX_BROWSER_RESULT_BYTES,
};

pub trait AgentBrowserStoreAdmission: Send + Sync {
    fn claim_browser(&self, action: &AgentBrowserAction) -> Result<bool, String>;
}
pub struct VerifiedAgentBrowserReceipt {
    action: AgentBrowserAction,
    receipt: AgentBrowserReceipt,
    image: Option<AgentBrowserImage>,
}
impl VerifiedAgentBrowserReceipt {
    pub fn action(&self) -> &AgentBrowserAction {
        &self.action
    }
    pub fn receipt(&self) -> &AgentBrowserReceipt {
        &self.receipt
    }
    pub fn image(&self) -> Option<&AgentBrowserImage> {
        self.image.as_ref()
    }
}
impl RunnerRegistry {
    pub async fn agent_browser(
        &self,
        access: &RunnerAccess,
        action: &AgentBrowserAction,
        admission: Option<&dyn AgentBrowserStoreAdmission>,
        image_step: Option<u16>,
    ) -> Result<VerifiedAgentBrowserReceipt, String> {
        action.validate()?;
        if image_step.is_some() && admission.is_some() {
            return Err("agent_browser_image_read_only".into());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut inner = tokio::time::timeout_at(deadline, self.inner.lock())
                .await
                .map_err(|_| "agent_browser_transport_timeout")?;
            let runner = inner
                .runners
                .get(&action.service.target.client_id)
                .ok_or("agent_browser_runner_unavailable")?;
            crate::access_control::assert_runner_access(Some(access), runner)?;
            if !runner
                .runner_features
                .supports(RunnerFeature::AgentProgramExecutionV1)
            {
                return Err("agent_browser_backend_unavailable".into());
            }
            let target = webcodex_core::runner_protocol::AgentExecutionTarget {
                client_id: action.service.target.client_id.clone(),
                runner_instance_id: runner.runner_instance_id.clone(),
            };
            if admission.is_some() && target != action.service.target {
                return Err("agent_browser_stale_target".into());
            }
            // Fail known queue/availability checks before crossing the durable
            // Store fence. Capacity problems are not ambiguous browser effects.
            ensure_dispatch_supported_locked(&inner, &action.service.target.client_id)
                .map_err(String::from)?;
            ensure_queue_capacity_locked(&inner, &action.service.target.client_id)
                .map_err(String::from)?;
            let observe_only = match admission {
                Some(gate) => !gate.claim_browser(action)?,
                None => true,
            };
            let request = RunnerRequest::from_operation(
                RunnerInvocationMetadata {
                    request_id: id.clone(),
                    client_id: target.client_id.clone(),
                    requested_by: "verified-local-browser".into(),
                    created_at: now_ts(),
                },
                RunnerOperation::AgentBrowser(AgentBrowserRequest {
                    target,
                    action: action.clone(),
                    observe_only,
                    image_step,
                }),
            )?;
            enqueue_agent_program_request_locked(
                self.telemetry.as_ref(),
                &mut inner,
                &action.service.target.client_id,
                id,
                request,
                Some(sender),
                None,
                &AgentProgramEnqueuePermit(()),
            )?;
            notify_runner_locked(&inner, &action.service.target.client_id);
        }
        let result = tokio::time::timeout_at(deadline, receiver)
            .await
            .map_err(|_| "agent_browser_transport_timeout")?
            .map_err(|_| "agent_browser_transport_closed")?;
        if !result.success || result.exit_code != Some(0) {
            return Err("agent_browser_transport_failed".into());
        }
        let raw = result
            .stdout
            .as_deref()
            .filter(|s| s.len() <= MAX_BROWSER_RESULT_BYTES)
            .ok_or("agent_browser_response_invalid")?;
        let response: AgentBrowserResponse =
            serde_json::from_str(raw).map_err(|_| "agent_browser_response_invalid")?;
        response.validate_for(&AgentBrowserRequest {
            target: action.service.target.clone(),
            action: action.clone(),
            observe_only: image_step.is_some() || admission.is_none(),
            image_step,
        })?;
        Ok(VerifiedAgentBrowserReceipt {
            action: action.clone(),
            receipt: response.receipt,
            image: response.image,
        })
    }
}
