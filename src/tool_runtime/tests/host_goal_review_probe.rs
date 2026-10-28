//! Real SQLite/Job recovery with a simulated document Runner. This verifies
//! dependency transport and fencing, not a model's visual quality judgment.
use crate::auth::{AuthContext, AuthKind};
use crate::runner_protocol::{
    RunnerCapabilities, RunnerJobUpdateRequest, RunnerPollRequest, RunnerProjectSummary,
    RunnerRegisterRequest, RunnerRequest, RunnerResultPayload, RunnerResultRequest,
    ShellCommandExecutionState,
};
use crate::tool_runtime::{ToolResult, ToolRuntime};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use webcodex_core::host_goal::*;
use webcodex_core::plugin::{
    DocumentWorkerSpec, PluginGatewayResponse, PluginGatewayResponsePayload,
};

const CLIENT: &str = "review-probe-runner";
const INSTANCE: &str = "review-probe-instance";

async fn next_request(runtime: &ToolRuntime) -> RunnerRequest {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(request) = runtime
            .runner_registry
            .poll(RunnerPollRequest {
                client_id: CLIENT.into(),
                runner_instance_id: INSTANCE.into(),
            })
            .await
            .unwrap()
        {
            return request;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Review probe did not dispatch before deadline"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn before_result(
    runtime: &ToolRuntime,
    task: &mut tokio::task::JoinHandle<Result<(), ToolResult>>,
) -> RunnerRequest {
    tokio::select! {
        request=next_request(runtime)=>request,
        result=task=>panic!("Probe returned before required current observation: {result:?}"),
    }
}

async fn complete(
    runtime: &ToolRuntime,
    request: RunnerRequest,
    output: Option<Value>,
    plugin: Option<PluginGatewayResponse>,
    failed: bool,
) {
    runtime
        .runner_registry
        .complete(RunnerResultPayload {
            result: RunnerResultRequest {
                client_id: CLIENT.into(),
                runner_instance_id: INSTANCE.into(),
                request_id: request.request_id,
                exit_code: plugin.is_none().then_some(if failed { 1 } else { 0 }),
                stdout: output.map(|value| value.to_string()),
                stderr: failed.then(|| "Simulated interrupted manifest observation".into()),
                duration_ms: plugin.is_none().then_some(1),
                error: None,
            },
            command_execution_state: None,
            mcp_gateway: None,
            plugin_gateway: plugin,
            coding_agent: None,
        })
        .await
        .unwrap();
}

async fn start_and_finish_worker(
    runtime: &ToolRuntime,
    root: &str,
    task: &mut tokio::task::JoinHandle<Result<(), ToolResult>>,
) -> (String, String) {
    let spec = before_result(runtime, task).await;
    assert!(
        spec.plugin_gateway.is_some(),
        "Expected a fresh provider dispatch after old Job recovery: {spec:?}"
    );
    complete(
        runtime,
        spec,
        None,
        Some(PluginGatewayResponse::success(
            PluginGatewayResponsePayload::DocumentWorker {
                spec: DocumentWorkerSpec {
                    provider_id: "office".into(),
                    provider_instance_id: "office-probe".into(),
                    executable: "python.exe".into(),
                    args: vec!["document_worker.py".into()],
                    cwd: root.into(),
                },
            },
        )),
        false,
    )
    .await;
    let process = before_result(runtime, task).await;
    assert_eq!(process.kind, "start_process_job");
    let metadata = process
        .job_context
        .as_ref()
        .unwrap()
        .document
        .as_ref()
        .unwrap();
    let request_id = metadata.request_id.clone();
    let job_id = process.job_id.clone().unwrap();
    runtime
        .runner_registry
        .update_job(RunnerJobUpdateRequest {
            client_id: CLIENT.into(),
            runner_instance_id: INSTANCE.into(),
            job_id: job_id.clone(),
            request_id: Some(process.request_id),
            update_seq: None,
            status: "completed".into(),
            stdout_chunk: None,
            stderr_chunk: None,
            stdout_tail: None,
            stderr_tail: None,
            log_snapshot: None,
            exit_code: Some(0),
            duration_ms: Some(1),
            error: None,
            command_execution_state: Some(ShellCommandExecutionState::Completed),
            validation_progress: None,
            activity: None,
            finished: true,
        })
        .await
        .unwrap();
    (request_id, job_id)
}

async fn return_probe(
    runtime: &ToolRuntime,
    task: &mut tokio::task::JoinHandle<Result<(), ToolResult>>,
    request_id: &str,
    dependencies: &str,
) {
    let request = before_result(runtime, task).await;
    assert_eq!(request.kind, "file_read_project_artifact");
    let source_sha = "a".repeat(64);
    let manifest=json!({"request_id":request_id,"success":true,"error":null,"output":{
        "path":"report.pdf","source_sha256":source_sha,"review":1,"review_status":"pending","previews":[],
        "review_probe":{"status":"verified","source_path":"report.pdf","source_sha256":source_sha,"render_dependencies_sha256":dependencies},
        "review_material":{"source_path":"report.pdf","source_sha256":source_sha,"render_dependencies_sha256":dependencies}
    }}).to_string();
    let path = format!(".webcodex-office/cache/{request_id}/result.json");
    let arguments: Value = serde_json::from_str(request.content.as_deref().unwrap()).unwrap();
    assert_eq!(
        arguments["path"], path,
        "Recovery must read the original Job's manifest"
    );
    complete(runtime,request,Some(json!({"path":path,"mime_type":"application/json","file_bytes":manifest.len(),
        "sha256":format!("{:x}",Sha256::digest(manifest.as_bytes())),"offset":0,"bytes_returned":manifest.len(),
        "content_base64":STANDARD.encode(manifest.as_bytes()),"next_offset":manifest.len(),"truncated":false,"eof":true})),None,false).await;
    let metadata = before_result(runtime, task).await;
    assert_eq!(metadata.kind, "file_read_project_artifact_metadata");
    complete(runtime,metadata,Some(json!({"path":"report.pdf","bytes":10,"mime_type":"application/pdf","sha256":source_sha})),None,false).await;
}

fn spawn_probe(
    runtime: &ToolRuntime,
    goal: &HostGoalRecord,
    packet: &HostGoalReviewPacket,
    auth: &AuthContext,
) -> tokio::task::JoinHandle<Result<(), ToolResult>> {
    let (runtime, goal, packet, auth) =
        (runtime.clone(), goal.clone(), packet.clone(), auth.clone());
    tokio::spawn(async move {
        runtime
            .verify_review_render_dependencies(&goal, &packet, "same-submission-key", Some(&auth))
            .await
    })
}

#[tokio::test]
async fn host_goal_review_probe_unknown_recovers_original_job_then_requires_current_generation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_string_lossy().to_string();
    let db = Arc::new(crate::Database::open(&temp.path().join("probe.db")).unwrap());
    let runtime = ToolRuntime::new_for_tests()
        .with_communication_database(db.clone())
        .with_structured_execution_sync_wait(Duration::from_millis(5));
    runtime
        .runner_registry
        .register(crate::test_support::current_runner_registration(
            RunnerRegisterRequest {
                agent_execution_resources: Vec::new(),
                client_id: CLIENT.into(),
                runner_instance_id: INSTANCE.into(),
                runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
                display_name: None,
                owner: None,
                hostname: None,
                host_context: None,
                capabilities: RunnerCapabilities {
                    shell: true,
                    file_read: true,
                    native_tool_plugins: true,
                    async_jobs: true,
                    async_shell_jobs: true,
                    structured_process_argv: true,
                    structured_validation_argv: true,
                    structured_script_payload: true,
                    structured_execution_jobs: true,
                    agent_isolated_execution_v1: false,
                    agent_program_execution_v1: false,
                    ..Default::default()
                },
                policy: None,
                process_started_at: None,
                build: None,
                job_concurrency_limit: None,
                job_inventory: None,
                coding_agent_providers: None,
                coding_agent_inventory: None,
            },
        ))
        .await
        .unwrap();
    crate::test_support::apply_project_inventory_snapshot(
        &runtime.runner_registry,
        CLIENT,
        INSTANCE,
        vec![RunnerProjectSummary {
            id: "office".into(),
            name: None,
            path: root.clone(),
            allow_patch: true,
            kind: None,
            registration_source: None,
            description: None,
            hooks: vec![],
            disabled: false,
            revision: None,
            git_branch: None,
            git_head: None,
            git_dirty: None,
            updated_at: 1,
            shell_profile: None,
        }],
    )
    .await;
    let project = format!("agent:{CLIENT}:office");
    let mut auth = AuthContext::new(AuthKind::Bootstrap);
    auth.is_bootstrap = true;
    let owner_key = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    let owner = owner_key.as_str();
    let goal = db
        .create_host_goal_at(
            owner,
            &project,
            &root,
            "Review current report rendering dependencies",
            "start",
            1,
        )
        .unwrap()
        .goal;
    let plan:HostGoalPlan=serde_json::from_value(json!({
        "objective":goal.objective,"constraints":[],"deliverables":["report.pdf"],"verification_paths":["report.pdf"],
        "final_criteria":[{"kind":"review","id":"layout","description":"Every actual page is readable"}],
        "nodes":[{"id":"render","title":"Review report","inputs":["report.pdf"],"verification_paths":[],"work":"Inspect all report images",
            "outputs":["report.pdf"],"criterion_ids":["layout"],"acceptance":[{"kind":"review","id":"layout","description":"Every actual page is readable"}],
            "failure_action":"Keep pending and rerender","review":1,"delivery_kind":"layout","managed_artifacts":[{"path":"report.pdf","kind":"layout"}]}],
        "state_management":{"facts":"Retain source hashes","decisions":"Keep decisions","issues":"Keep findings","artifacts":"Retain image receipts",
            "checkpoint_triggers":["After render"],"resume_instructions":"Resume the original probe Job before a fresh observation"}
    })).unwrap();
    let saved = db
        .save_host_goal_plan(owner, &goal.goal_id, goal.revision, &plan, None, "save", 2)
        .unwrap();
    let materialized = db
        .materialize_host_goal_plan(
            owner,
            &goal.goal_id,
            saved.plan.version,
            &saved.plan.hash,
            &HostGoalMaterialization {
                json_path: "fixture-plan.json".into(),
                json_hash: "b".repeat(64),
                markdown_path: "fixture-plan.md".into(),
                markdown_hash: "c".repeat(64),
                verified_at: 3,
            },
            3,
        )
        .unwrap();
    let ready = db
        .confirm_host_goal_plan(
            owner,
            &goal.goal_id,
            materialized.goal.revision,
            saved.plan.version,
            &saved.plan.hash,
            "Synthetic test fixture explicitly confirms this plan version",
            "confirm",
            4,
        )
        .unwrap()
        .goal;
    let claimed = db
        .claim_host_goal_node(
            owner,
            &goal.goal_id,
            ready.revision,
            "render",
            false,
            "claim",
            5,
        )
        .unwrap();
    let goal = claimed.goal;
    let packet = HostGoalReviewPacket {
        packet_id: "probe-recovery-packet".into(),
        binding: claimed.attempt.binding,
        context_epoch: None,
        manifest: vec![webcodex_core::goal::GoalArtifactSnapshot {
            path: "report.pdf".into(),
            sha256: Some("a".repeat(64)),
        }],
        state_sha256: None,
        machine_checks: vec![],
        required_scopes: vec!["report.pdf#page:1".into()],
        delivered_scopes: vec![],
        created_at: 6,
        materials: vec![HostGoalReviewMaterial {
            id: "report.pdf#page:1".into(),
            path: "report.pdf".into(),
            sha256: "a".repeat(64),
            kind: "image".into(),
            preview_path: Some("preview.png".into()),
            preview_sha256: Some("b".repeat(64)),
            render_source_sha256: Some("a".repeat(64)),
            render_dependencies_sha256: Some("d".repeat(64)),
            content: Some(json!({"dpi":150,"revision_view":"final"}).to_string()),
        }],
    };
    let mut first = spawn_probe(&runtime, &goal, &packet, &auth);
    let (old_request, old_job) = start_and_finish_worker(&runtime, &root, &mut first).await;
    let unreadable = before_result(&runtime, &mut first).await;
    assert_eq!(unreadable.kind, "file_read_project_artifact");
    complete(&runtime, unreadable, None, None, true).await;
    let pending = first.await.unwrap().unwrap_err();
    assert_eq!(pending.output["error_kind"], "goal_review_probe_pending");
    let operations = db
        .list_host_goal_operations(owner, &goal.goal_id, 0, 100, false)
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(
        operations[0].status,
        HostGoalOperationStatus::Unknown,
        "Lost observation is not a terminal failure"
    );
    assert_eq!(operations[0].job_id.as_deref(), Some(old_job.as_str()));
    assert!(!operations[0].status.terminal());

    let mut retry = spawn_probe(&runtime, &goal, &packet, &auth);
    return_probe(&runtime, &mut retry, &old_request, &"d".repeat(64)).await;
    // The original Job's success is reconciled, then a genuinely fresh read
    // must start. A stale-proof implementation returns early and fails here.
    let (new_request, new_job) = start_and_finish_worker(&runtime, &root, &mut retry).await;
    assert_ne!(new_job, old_job);
    assert_ne!(new_request, old_request);
    return_probe(&runtime, &mut retry, &new_request, &"e".repeat(64)).await;
    let rejected = retry.await.unwrap().unwrap_err();
    assert_eq!(
        rejected.output["error_kind"],
        "goal_review_render_dependencies_changed"
    );
    let operations = db
        .list_host_goal_operations(owner, &goal.goal_id, 0, 100, false)
        .unwrap();
    assert_eq!(
        operations.len(),
        2,
        "Recovery retains exactly one old Job and one new observation"
    );
    assert!(operations
        .iter()
        .all(|operation| operation.status == HostGoalOperationStatus::Succeeded));
    assert!(operations
        .iter()
        .any(|operation| serde_json::from_str::<Value>(
            operation.result_summary.as_deref().unwrap()
        )
        .unwrap()["proof"]["render_dependencies_sha256"]
            == "e".repeat(64)));
}
