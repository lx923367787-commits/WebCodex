use super::*;
use crate::agent_execution::tests::{access, poll, registration, resources};
use std::sync::atomic::{AtomicUsize, Ordering};
use webcodex_core::runner_protocol::{AgentExecutionTarget, AgentRunRef};
struct Admission {
    calls: AtomicUsize,
    allow: bool,
}
impl AgentProgramStoreAdmission for Admission {
    fn claim_dispatch(&self, _: &AgentProgramStart) -> Result<bool, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.allow)
    }
}
async fn fixture(enabled: bool) -> RunnerRegistry {
    let registry = RunnerRegistry::default();
    let mut resource = resources().remove(0);
    resource.project_workspace_copy = true;
    let mut request = registration(true, vec![resource]);
    request.capabilities.agent_program_execution_v1 = enabled;
    registry
        .register_with_auth(request, Some(&access()))
        .await
        .unwrap();
    let project=serde_json::from_value(serde_json::json!({"id":"repo","name":"project","path":"E:\\public-project","allow_patch":true,"disabled":false,"hooks":[],"updated_at":0})).unwrap();
    registry
        .upsert_runner_project_for_instance("runner", "instance", project)
        .await
        .unwrap();
    registry
}
fn start() -> AgentProgramStart {
    let mut start = AgentProgramStart {
        binding: AgentProgramBinding {
            local_service_port: None,
            resume_from_program: None,
            version: 1,
            program_id: "program-fixture".into(),
            enrollment_id: "own-enrollment".into(),
            task_id: "own-task".into(),
            run: AgentRunRef {
                agent_id: "own-agent".into(),
                agent_run_id: "own-run".into(),
                generation: 1,
            },
            project: "logical-project".into(),
            target: AgentExecutionTarget {
                client_id: "runner".into(),
                runner_instance_id: "instance".into(),
            },
            job_id: "program-job".into(),
            start_request_id: "program-request".into(),
            workspace_project: "agent:runner:repo".into(),
            workspace_root: "E:\\public-project".into(),
            relative_cwd: "app".into(),
            resource_set_id: "resources-0".into(),
            resource_manifest_hash: "a".repeat(64),
            launch_intent_hash: "b".repeat(64),
            expires_at_unix_ms: chrono::Utc::now().timestamp_millis() + 60000,
        },
        args: vec!["test.py".into()],
        stdin: None,
        timeout_secs: 60,
    };
    start.binding.launch_intent_hash = start.compute_launch_hash().unwrap();
    start
}
#[tokio::test]
async fn ordinary_program_requires_store_fence_and_dispatches_original_job_once() {
    let registry = fixture(true).await;
    let start = start();
    let deny = Admission {
        calls: AtomicUsize::new(0),
        allow: false,
    };
    assert!(registry
        .dispatch_agent_program(&access(), start.clone(), &deny)
        .await
        .is_err());
    assert!(poll(&registry).await.is_none());
    let allow = Admission {
        calls: AtomicUsize::new(0),
        allow: true,
    };
    registry
        .dispatch_agent_program(&access(), start.clone(), &allow)
        .await
        .unwrap();
    registry
        .dispatch_agent_program(&access(), start.clone(), &allow)
        .await
        .unwrap();
    assert_eq!(allow.calls.load(Ordering::SeqCst), 1);
    let wire = poll(&registry).await.unwrap();
    assert_eq!(wire.kind, AGENT_PROGRAM_START_KIND);
    assert_eq!(wire.request_id, start.binding.start_request_id);
    assert!(poll(&registry).await.is_none());
    assert!(
        registry.list_jobs(None).await.is_empty(),
        "program output is not a generic public job"
    );
    registry
        .observe_agent_program(&access(), &start.binding)
        .await
        .unwrap();
}
#[tokio::test]
async fn ordinary_program_stop_preserves_original_job_and_exact_scope() {
    let registry = fixture(true).await;
    let start = start();
    let allow = Admission {
        calls: AtomicUsize::new(0),
        allow: true,
    };
    registry
        .dispatch_agent_program(&access(), start.clone(), &allow)
        .await
        .unwrap();
    let mut foreign = start.binding.clone();
    foreign.run.agent_id = "different-agent".into();
    assert!(registry
        .stop_agent_program(&access(), &foreign)
        .await
        .is_err());
    let stopped = registry
        .stop_agent_program(&access(), &start.binding)
        .await
        .unwrap();
    assert_eq!(stopped.status, "stopped");
    assert_eq!(
        stopped.command_execution_state,
        Some(webcodex_core::runner_protocol::ShellCommandExecutionState::NotStarted)
    );
    assert!(poll(&registry).await.is_none());
    let observed = registry
        .observe_agent_program(&access(), &start.binding)
        .await
        .unwrap();
    assert_eq!(observed.job_id, start.binding.job_id);
    registry
        .hidden_job_log_for_auth(Some(&access()), &start.binding.job_id, Some(100))
        .await
        .unwrap();
    assert_eq!(
        registry
            .dispatch_agent_program(&access(), start, &allow)
            .await
            .unwrap()
            .status,
        "stopped"
    );
    assert_eq!(allow.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ordinary_program_capability_source_and_generic_queue_cannot_bypass_admission() {
    let registry = fixture(false).await;
    let start = start();
    let allow = Admission {
        calls: AtomicUsize::new(0),
        allow: true,
    };
    assert!(registry
        .dispatch_agent_program(&access(), start.clone(), &allow)
        .await
        .is_err());
    assert_eq!(allow.calls.load(Ordering::SeqCst), 0);
    let registry = fixture(true).await;
    let mut wrong = start.clone();
    wrong.binding.workspace_root = "E:\\foreign".into();
    wrong.binding.launch_intent_hash = wrong.compute_launch_hash().unwrap();
    assert!(registry
        .dispatch_agent_program(&access(), wrong, &allow)
        .await
        .is_err());
    assert_eq!(allow.calls.load(Ordering::SeqCst), 0);
    let wire = RunnerRequest::from_operation(
        RunnerInvocationMetadata {
            request_id: start.binding.start_request_id.clone(),
            client_id: "runner".into(),
            requested_by: "generic".into(),
            created_at: now_ts(),
        },
        RunnerOperation::Job(start.into_job()),
    )
    .unwrap();
    let mut inner = registry.inner.lock().await;
    assert!(crate::requests::enqueue_pending_request_locked(
        registry.telemetry.as_ref(),
        &mut inner,
        "runner",
        "program-request".into(),
        wire,
        None,
        None
    )
    .is_err());
    assert!(inner.pending_by_id.is_empty());
}

impl AgentBrowserStoreAdmission for Admission {
    fn claim_browser(
        &self,
        _: &webcodex_core::agent_browser::AgentBrowserAction,
    ) -> Result<bool, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.allow)
    }
}
#[tokio::test]
async fn browser_wire_requires_private_admission_and_checks_retained_action_identity() {
    use webcodex_core::agent_browser::{
        AgentBrowserAction, AgentBrowserReceipt, AgentBrowserRequest, BrowserStep,
    };
    let registry = fixture(true).await;
    let mut service = start().binding;
    service.local_service_port = Some(8080);
    let action = AgentBrowserAction {
        action_id: format!("wc_browser_{}", "a".repeat(32)),
        service,
        steps: vec![BrowserStep::Navigate { path: "/ui".into() }],
        timeout_secs: 10,
    };
    let query = AgentBrowserRequest {
        target: action.service.target.clone(),
        action: action.clone(),
        observe_only: false,
        image_step: None,
    };
    let wire = RunnerRequest::from_operation(
        RunnerInvocationMetadata {
            request_id: "browser-generic".into(),
            client_id: "runner".into(),
            requested_by: "untrusted".into(),
            created_at: now_ts(),
        },
        RunnerOperation::AgentBrowser(query),
    )
    .unwrap();
    {
        let mut inner = registry.inner.lock().await;
        assert!(crate::requests::enqueue_pending_request_locked(
            registry.telemetry.as_ref(),
            &mut inner,
            "runner",
            "browser-generic".into(),
            wire,
            None,
            None
        )
        .is_err());
    }
    for corrupt in [false, true] {
        let worker_registry = registry.clone();
        let original = action.clone();
        let worker = tokio::spawn(async move {
            let gate = Admission {
                calls: AtomicUsize::new(0),
                allow: false,
            };
            worker_registry
                .agent_browser(&access(), &original, Some(&gate), None)
                .await
        });
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let request = loop {
            if let Some(request) = poll(&registry).await {
                break request;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "browser was not queued"
            );
            tokio::task::yield_now().await;
        };
        let RunnerOperation::AgentBrowser(decoded) = request.decode_operation().unwrap() else {
            panic!("wrong operation")
        };
        assert!(
            decoded.observe_only,
            "a refused/replayed fence must only observe"
        );
        assert_eq!(decoded.action, action);
        let receipt = AgentBrowserReceipt {
            action_id: action.action_id.clone(),
            identity_hash: if corrupt {
                "0".repeat(64)
            } else {
                action.identity_hash().unwrap()
            },
            state: "outcome_unknown".into(),
            process_id: None,
            exit_code: None,
            result: None,
            error: Some("agent_browser_original_execution_unsettled".into()),
        };
        let response=serde_json::from_value(serde_json::json!({"success":true,"request_id":request.request_id,"client_id":"runner","command_preview":"browser fixture","exit_code":0,"stdout":serde_json::to_string(&serde_json::json!({"receipt":receipt,"image":null})).unwrap()})).unwrap();
        registry
            .inner
            .lock()
            .await
            .pending_by_id
            .get_mut(&request.request_id)
            .unwrap()
            .waiter
            .take()
            .unwrap()
            .send(response)
            .unwrap();
        assert_eq!(worker.await.unwrap().is_err(), corrupt);
    }
}
