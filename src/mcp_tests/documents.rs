use super::*;
use crate::runner_protocol::{
    RunnerJobUpdateRequest, RunnerRequest, RunnerResultPayload, ShellCommandExecutionState,
};
use std::time::Duration;
use webcodex_core::plugin::{
    DocumentWorkerSpec, PluginGatewayRequest, PluginGatewayResponse, PluginGatewayResponsePayload,
};

async fn next_request(runtime: &ToolRuntime) -> RunnerRequest {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(request) = runtime
            .runner_registry
            .poll(RunnerPollRequest {
                client_id: "document-runner".into(),
                runner_instance_id: "document-instance".into(),
            })
            .await
            .unwrap()
        {
            return request;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "document request did not dispatch"
        );
        tokio::task::yield_now().await;
    }
}

async fn request_before_result(
    runtime: &ToolRuntime,
    task: &mut tokio::task::JoinHandle<McpOutcome>,
    stage: &str,
) -> RunnerRequest {
    tokio::select! {
        biased;
        request = next_request(runtime) => request,
        result = task => panic!("document call completed before {stage}: {result:?}"),
    }
}

async fn complete(
    runtime: &ToolRuntime,
    request: RunnerRequest,
    output: Option<Value>,
    plugin: Option<PluginGatewayResponse>,
) {
    runtime
        .runner_registry
        .complete(RunnerResultPayload {
            result: RunnerResultRequest {
                client_id: request.client_id,
                runner_instance_id: "document-instance".into(),
                request_id: request.request_id,
                exit_code: plugin.is_none().then_some(0),
                stdout: output.map(|value| value.to_string()),
                stderr: None,
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

fn call(
    runtime: &ToolRuntime,
    auth: &crate::auth::AuthContext,
    surface: ModelSurface,
    tool: &str,
    mut arguments: Value,
) -> tokio::task::JoinHandle<McpOutcome> {
    let runtime = runtime.clone();
    let auth = auth.clone();
    arguments["context_epoch"] = json!("document-inspection-chat");
    let params = if surface == ModelSurface::AdaptiveRuntime {
        mcp_2026_params(
            json!({"name":"call_runtime_tool","arguments":{"tool":tool,"arguments":arguments}}),
        )
    } else {
        mcp_2026_params(json!({"name":tool,"arguments":arguments}))
    };
    tokio::spawn(async move {
        handle_mcp_request(
            &runtime,
            rpc("tools/call", Some(json!(9)), params),
            Some(&auth),
        )
        .await
    })
}

#[tokio::test]
async fn document_present_job_resume_delivers_native_images_direct_and_gateway() {
    for surface in [
        ModelSurface::AdaptiveRuntime,
        ModelSurface::FullOperatorRuntime,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_string_lossy().into_owned();
        let db = std::sync::Arc::new(
            crate::Database::open(&temp.path().join("document-goal.db")).unwrap(),
        );
        let runtime = test_runtime_with_surface(surface)
            .with_communication_database(db.clone())
            .with_structured_execution_sync_wait(Duration::from_millis(30));
        runtime
            .runner_registry
            .register(crate::test_support::current_runner_registration(
                RunnerRegisterRequest {
                    agent_execution_resources: Vec::new(),
                    client_id: "document-runner".into(),
                    runner_instance_id: "document-instance".into(),
                    runner_protocol_generation:
                        crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
                    display_name: None,
                    owner: None,
                    hostname: None,
                    host_context: None,
                    capabilities: RunnerCapabilities {
                        shell: true,
                        file_read: true,
                        file_write: true,
                        native_tool_plugins: true,
                        async_jobs: true,
                        async_shell_jobs: true,
                        structured_process_argv: true,
                        structured_validation_argv: true,
                        structured_script_payload: true,
                        structured_execution_jobs: true,
                        agent_isolated_execution_v1: false,
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
            "document-runner",
            "document-instance",
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
        let project = "agent:document-runner:office";
        let mut auth = crate::auth::AuthContext::new(crate::auth::AuthKind::Bootstrap);
        auth.is_bootstrap = true;
        let goal = call(
            &runtime,
            &auth,
            surface,
            "goal_mode_start",
            json!({"project":project,"objective":"Read the source before planning implementation","idempotency_key":"document-inspection-goal"}),
        )
        .await
        .unwrap();
        let McpOutcome::Ok(goal) = goal else {
            panic!("{goal:?}")
        };
        assert_eq!(goal["result"]["isError"], false, "{goal}");
        let goal = &goal["result"]["structuredContent"]["output"]["goal"];
        assert_eq!(goal["phase"], "planning");
        let mut start = call(
            &runtime,
            &auth,
            surface,
            "present_document",
            json!({"project":project,"path":"report.pdf","view":"pages"}),
        );
        let spec_request = request_before_result(&runtime, &mut start, "provider binding").await;
        assert!(
            matches!(spec_request.plugin_gateway,Some(PluginGatewayRequest::DocumentWorkerSpec {ref project_id,ref project_root}) if project_id==project && project_root==&root)
        );
        complete(
            &runtime,
            spec_request,
            None,
            Some(PluginGatewayResponse::success(
                PluginGatewayResponsePayload::DocumentWorker {
                    spec: DocumentWorkerSpec {
                        provider_id: "office".into(),
                        provider_instance_id: "provider-office".into(),
                        executable: "python.exe".into(),
                        args: vec!["worker.py".into()],
                        cwd: root.clone(),
                    },
                },
            )),
        )
        .await;
        let process = request_before_result(&runtime, &mut start, "worker Job").await;
        assert_eq!(process.kind, "start_process_job");
        let document = process
            .job_context
            .as_ref()
            .unwrap()
            .document
            .as_ref()
            .unwrap();
        let request_id = document.request_id.clone();
        assert_eq!(document.source_path, "report.pdf");
        assert_eq!(document.project_root, root);
        assert_eq!(
            process.process.as_ref().unwrap().args.last().unwrap(),
            &format!(".webcodex-office/cache/{request_id}/result.json")
        );
        let started = start.await.unwrap();
        let McpOutcome::Ok(started) = started else {
            panic!("{started:?}")
        };
        assert_eq!(started["result"]["isError"], false, "{started}");
        let job_id = started["result"]["structuredContent"]["output"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(process.job_id.as_deref(), Some(job_id.as_str()));
        runtime
            .runner_registry
            .update_job(RunnerJobUpdateRequest {
                client_id: "document-runner".into(),
                runner_instance_id: "document-instance".into(),
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
        let saved = crate::test_support::runner_project_files::during(&runtime, "document-runner", "document-instance", temp.path(), call(
            &runtime,
            &auth,
            surface,
            "goal_plan_save",
            json!({
                "goal_id":goal["goal_id"],"expected_revision":goal["revision"],"idempotency_key":"document-inspection-plan",
                "plan":{
                    "objective":"Read the source before planning implementation","constraints":[],
                    "deliverables":["A sourced conclusion"],
                    "final_criteria":[{"kind":"review","id":"sources","description":"Conclusion cites the source"}],
                    "nodes":[{"id":"research","title":"Read the source","inputs":["report.pdf"],
                        "delivery_kind":"layout","review":1,
                        "managed_artifacts":[{"path":"page-1.png","kind":"layout"},{"path":"page-2.png","kind":"layout"}],
                        "work":"Read and record the supported conclusion","outputs":["Sourced conclusion"],
                        "criterion_ids":["sources"],"acceptance":[{"kind":"review","id":"node_sources","description":"Source supports the conclusion"}],
                        "failure_action":"Record missing evidence"}],
                    "state_management":{"facts":"Store source facts","decisions":"Record reasons","issues":"Retain missing evidence",
                        "artifacts":"Record sources","checkpoint_triggers":["After reading"],"resume_instructions":"Read current plan and state"}
                }
            }),
        ))
        .await
        .unwrap();
        let McpOutcome::Ok(saved) = saved else {
            panic!("{saved:?}")
        };
        assert_eq!(saved["result"]["isError"], false, "{saved}");
        assert_eq!(
            saved["result"]["structuredContent"]["output"]["goal"]["phase"],
            "awaiting_confirmation"
        );
        let mut resume = call(
            &runtime,
            &auth,
            surface,
            "present_document",
            json!({"project":project,"job_id":job_id}),
        );
        let image_bytes = general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGP8z8DAwMDAxMDAwMDAAAANHQEDasKb6QAAAABJRU5ErkJggg==").unwrap();
        let image = image_bytes.as_slice();
        let image_sha = format!("{:x}", Sha256::digest(image));
        let preview = format!(".webcodex-office/cache/{request_id}/page-1.png");
        let manifest=json!({"request_id":request_id,"success":true,"output":{"path":"report.pdf","source_sha256":"a".repeat(64),"review":0,"review_reason":"Read-only inspection does not start a delivery review","review_status":"not_required","total_pages":1,"content":{"text":"真实中文正文"},"review_material":{"source_path":"report.pdf","source_sha256":"a".repeat(64),"render_source_path":"report.pdf","render_source_sha256":"a".repeat(64),"render_dependencies_sha256":"d".repeat(64),"coverage":{"required":["page:1"],"prepared":["page:1"],"missing":[],"complete":true}},"previews":[{"path":preview,"sha256":image_sha,"source_sha256":"a".repeat(64),"render_source_path":"report.pdf","render_source_sha256":"a".repeat(64),"render_dependencies_sha256":"d".repeat(64),"coverage_id":"page:1","mime_type":"image/png","page":1}]},"error":null}).to_string();
        let manifest_request =
            request_before_result(&runtime, &mut resume, "result manifest").await;
        assert_eq!(manifest_request.kind, "file_read_project_artifact");
        let payload: Value =
            serde_json::from_str(manifest_request.content.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload["path"],
            format!(".webcodex-office/cache/{request_id}/result.json")
        );
        complete(&runtime,manifest_request,Some(json!({"path":payload["path"],"mime_type":"application/json","file_bytes":manifest.len(),"sha256":format!("{:x}",Sha256::digest(manifest.as_bytes())),"offset":0,"bytes_returned":manifest.len(),"content_base64":general_purpose::STANDARD.encode(manifest.as_bytes()),"next_offset":manifest.len(),"truncated":false,"eof":true})),None).await;
        let source_request =
            request_before_result(&runtime, &mut resume, "source SHA verification").await;
        assert_eq!(source_request.kind, "file_read_project_artifact_metadata");
        complete(&runtime,source_request,Some(json!({"path":"report.pdf","bytes":20,"mime_type":"application/pdf","sha256":"a".repeat(64)})),None).await;
        let image_request = request_before_result(&runtime, &mut resume, "native page image").await;
        let payload: Value =
            serde_json::from_str(image_request.content.as_deref().unwrap()).unwrap();
        assert_eq!(payload["path"], preview);
        assert_eq!(payload["mcp_image"], true);
        complete(&runtime,image_request,Some(json!({"path":preview,"mime_type":"image/png","file_bytes":image.len(),"sha256":image_sha,"offset":0,"bytes_returned":image.len(),"content_base64":general_purpose::STANDARD.encode(image),"next_offset":image.len(),"truncated":false,"eof":true})),None).await;
        let finished = resume.await.unwrap();
        let McpOutcome::Ok(finished) = finished else {
            panic!("{finished:?}")
        };
        assert_eq!(finished["result"]["isError"], false, "{finished}");
        assert_eq!(
            finished["result"]["structuredContent"]["output"]["content"]["text"],
            "真实中文正文"
        );
        assert!(finished["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "image"));
        let native = finished["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "image")
            .collect::<Vec<_>>();
        let receipts = finished["result"]["structuredContent"]["output"]["native_image_receipts"]
            .as_array()
            .unwrap();
        assert_eq!(native.len(), receipts.len());
        assert_eq!(receipts[0]["sha256"], image_sha);
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(
                    general_purpose::STANDARD
                        .decode(native[0]["data"].as_str().unwrap())
                        .unwrap()
                )
            ),
            image_sha
        );
        assert!(!finished["result"]["structuredContent"]
            .to_string()
            .contains("content_base64"));
        let mut stale = call(
            &runtime,
            &auth,
            surface,
            "present_document",
            json!({"project":project,"job_id":job_id}),
        );
        let request = request_before_result(&runtime, &mut stale, "stale result manifest").await;
        complete(&runtime,request,Some(json!({"path":format!(".webcodex-office/cache/{request_id}/result.json"),"mime_type":"application/json","file_bytes":manifest.len(),"sha256":format!("{:x}",Sha256::digest(manifest.as_bytes())),"offset":0,"bytes_returned":manifest.len(),"content_base64":general_purpose::STANDARD.encode(manifest.as_bytes()),"next_offset":manifest.len(),"truncated":false,"eof":true})),None).await;
        let request = request_before_result(&runtime, &mut stale, "changed source SHA").await;
        complete(&runtime,request,Some(json!({"path":"report.pdf","bytes":21,"mime_type":"application/pdf","sha256":"b".repeat(64)})),None).await;
        let stale = stale.await.unwrap();
        let McpOutcome::Ok(stale) = stale else {
            panic!("{stale:?}")
        };
        assert_eq!(stale["result"]["isError"], true);
        assert_eq!(
            stale["result"]["structuredContent"]["output"]["error_kind"],
            "document_source_changed"
        );
        assert!(!stale["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "image"));
        let wrong = call(
            &runtime,
            &auth,
            surface,
            "document_convert",
            json!({"project":project,"job_id":job_id}),
        )
        .await
        .unwrap();
        let McpOutcome::Ok(wrong) = wrong else {
            panic!("{wrong:?}")
        };
        assert_eq!(wrong["result"]["isError"], true);
        assert_eq!(
            wrong["result"]["structuredContent"]["output"]["error_kind"],
            "document_job_mismatch"
        );
        // A transport protocol test with valid PNG bytes. The synthetic model
        // verdict below is not evidence of a real visual quality assessment.
        let awaiting = &saved["result"]["structuredContent"]["output"]["goal"];
        let confirmed = crate::test_support::runner_project_files::during(&runtime, "document-runner", "document-instance", temp.path(), call(&runtime, &auth, surface, "goal_confirm_plan", json!({
            "goal_id":awaiting["goal_id"],"expected_revision":awaiting["revision"],
            "plan_version":awaiting["plan_version"],"plan_sha256":awaiting["plan_hash"],
            "confirmation":"Synthetic fixture confirmation of this exact plan; no real user approval is implied.",
            "idempotency_key":"transport-review-confirm"}))).await.unwrap();
        let McpOutcome::Ok(confirmed) = confirmed else {
            panic!("{confirmed:?}")
        };
        assert_eq!(confirmed["result"]["isError"], false, "{confirmed}");
        let ready = &confirmed["result"]["structuredContent"]["output"]["goal"];
        let read = crate::test_support::runner_project_files::during(
            &runtime,
            "document-runner",
            "document-instance",
            temp.path(),
            call(
                &runtime,
                &auth,
                surface,
                "goal_read",
                json!({"goal_id":ready["goal_id"],"view":"node","node_id":"research"}),
            ),
        )
        .await
        .unwrap();
        let McpOutcome::Ok(read) = read else {
            panic!("{read:?}")
        };
        assert_eq!(read["result"]["isError"], false, "{read}");
        let claimed=crate::test_support::runner_project_files::during(&runtime, "document-runner", "document-instance", temp.path(), call(&runtime,&auth,surface,"goal_claim_node",json!({"goal_id":ready["goal_id"],"node_id":"research",
            "expected_revision":ready["revision"],"plan_version":ready["plan_version"],"idempotency_key":"transport-review-claim"}))).await.unwrap();
        let McpOutcome::Ok(claimed) = claimed else {
            panic!("{claimed:?}")
        };
        assert_eq!(claimed["result"]["isError"], false, "{claimed}");
        use webcodex_core::goal::GoalArtifactSnapshot;
        use webcodex_core::host_goal::{
            HostGoalCheckEvidence, HostGoalExecutionBinding, HostGoalReviewPacket,
        };
        let binding: HostGoalExecutionBinding = serde_json::from_value(
            claimed["result"]["structuredContent"]["output"]["binding"].clone(),
        )
        .unwrap();
        let stored_goal = runtime
            .host_goal_access(&binding.goal_id, Some(&auth), false)
            .await
            .unwrap();
        let owner = stored_goal.owner_key.as_str();
        let images=[
            general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGP8z8DAwMDAxMDAwMDAAAANHQEDasKb6QAAAABJRU5ErkJggg==").unwrap(),
            general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGNkYPjPwMDAxMDAwMDAAAALHwEDmIWXfgAAAABJRU5ErkJggg==").unwrap()];
        let fingerprints = images
            .iter()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .collect::<Vec<_>>();
        let manifest = (0..2)
            .map(|index| GoalArtifactSnapshot {
                path: format!("page-{}.png", index + 1),
                sha256: Some(fingerprints[index].clone()),
            })
            .collect::<Vec<_>>();
        let packet = HostGoalReviewPacket {
            packet_id: "native-transport-fixture".into(),
            binding: binding.clone(),
            context_epoch: Some("document-inspection-chat".into()),
            manifest: manifest.clone(),
            state_sha256: None,
            machine_checks: vec![],
            materials: vec![],
            required_scopes: manifest
                .iter()
                .map(|file| format!("{}#visual_pending", file.path))
                .collect(),
            delivered_scopes: vec![],
            created_at: 1,
        };
        db.prepare_host_goal_review(owner, &binding, &packet, 1)
            .unwrap();
        let verdict:HostGoalCheckEvidence=serde_json::from_value(json!({"criterion_id":"node_sources","passed":true,"source":"model_self_check",
            "evidence":"Synthetic transport assessment of page-1.png and page-2.png; both full image scopes arrived with the expected current hashes. This test asserts protocol gates only.",
            "source_refs":["native-transport-fixture"],"artifacts":manifest,"review_packet_id":packet.packet_id,
            "reviewed_scopes":["page-1.png#page:1","page-2.png#page:1"],"findings":[]})).unwrap();
        assert!(db
            .submit_host_goal_review(
                owner,
                &binding,
                &packet.packet_id,
                Some("document-inspection-chat"),
                &manifest,
                &[verdict.clone()],
                2
            )
            .is_err());
        // A path/base64-only response must not register any visual coverage.
        for (index, as_image) in [(0, false), (0, true), (1, true)] {
            let path = &manifest[index].path;
            let bytes = &images[index];
            let mut request = call(
                &runtime,
                &auth,
                surface,
                "read_project_artifact",
                json!({"project":project,"path":path,"as_image":as_image}),
            );
            let artifact =
                request_before_result(&runtime, &mut request, "transport fixture artifact").await;
            assert_eq!(artifact.kind, "file_read_project_artifact");
            complete(&runtime,artifact,Some(json!({"path":path,"mime_type":"image/png","file_bytes":bytes.len(),"sha256":fingerprints[index],"offset":0,
                "bytes_returned":bytes.len(),"content_base64":general_purpose::STANDARD.encode(bytes),"next_offset":bytes.len(),"truncated":false,"eof":true})),None).await;
            let shown = request.await.unwrap();
            let McpOutcome::Ok(shown) = shown else {
                panic!("{shown:?}")
            };
            assert_eq!(shown["result"]["isError"], false, "{shown}");
            let image_blocks = shown["result"]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["type"] == "image")
                .collect::<Vec<_>>();
            assert_eq!(image_blocks.len(), usize::from(as_image));
            let record = db
                .read_host_goal_review(owner, &binding.goal_id, &binding.node_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                record.rounds.len(),
                0,
                "Pagination is not a model review round"
            );
            let coverage = &record.current_packet.as_ref().unwrap().delivered_scopes;
            assert_eq!(coverage.len(), if !as_image { 0 } else { index + 1 });
            if as_image {
                let actual = general_purpose::STANDARD
                    .decode(image_blocks[0]["data"].as_str().unwrap())
                    .unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(actual)), fingerprints[index]);
                assert!(
                    shown["result"]["structuredContent"]["output"]["review_receipt"].is_object(),
                    "{shown}"
                );
            }
            if !as_image || index == 0 {
                assert!(db
                    .submit_host_goal_review(
                        owner,
                        &binding,
                        &packet.packet_id,
                        Some("document-inspection-chat"),
                        &manifest,
                        &[verdict.clone()],
                        3
                    )
                    .is_err());
            }
        }
        let reviewed = db
            .submit_host_goal_review(
                owner,
                &binding,
                &packet.packet_id,
                Some("document-inspection-chat"),
                &manifest,
                &[verdict],
                4,
            )
            .unwrap();
        assert_eq!(reviewed.rounds.len(), 1);
        assert!(reviewed.rounds[0].passed);
    }
}
