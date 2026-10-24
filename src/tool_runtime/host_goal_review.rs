//! Review material is prepared from real artifacts, then bound to a model
//! verdict. Transport receipts describe returned content, not model cognition.
use super::host_goal::{bounded, failure, now, owner, store_error};
use super::host_goal_context as context;
use super::host_goal_files::sha256;
use super::tool_call::DocumentToolCall;
use super::{ToolCall, ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use crate::client_window::ClientWindow;
use serde_json::{json, Value};
use webcodex_core::goal::GoalArtifactSnapshot;
use webcodex_core::host_goal::*;

pub(super) fn visual_paths(node: &HostGoalNode, manifest: &[GoalArtifactSnapshot]) -> Vec<String> {
    manifest
        .iter()
        .filter(|entry| {
            (node.outputs.contains(&entry.path) && host_goal_layout_path(&entry.path))
                || node
                    .managed_artifacts
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|a| {
                        a.path == entry.path
                            && (a.kind == HostGoalDeliveryKind::Layout
                                || host_goal_layout_path(&a.path))
                    })
        })
        .map(|entry| entry.path.clone())
        .collect()
}

pub(super) fn review_summary(record: Option<&HostGoalReviewRecord>, node: &HostGoalNode) -> Value {
    let policy = host_goal_review_policy(node);
    let packet = record.and_then(|r| r.current_packet.as_ref());
    let last = record.and_then(|r| r.rounds.last());
    let status = if record.map(|r| r.review).unwrap_or(policy.review) == 0 {
        "not_required"
    } else if last
        .is_some_and(|r| r.passed && packet.is_some_and(|p| p.packet_id == r.packet.packet_id))
    {
        "passed"
    } else if record.is_some_and(|r| r.rounds.len() >= r.max_rounds as usize) {
        "exhausted"
    } else if record.is_none() {
        "not_recorded"
    } else {
        "pending"
    };
    json!({"review":record.map(|r|r.review).unwrap_or(policy.review),
        "reason":record.map(|r|r.reason.as_str()).unwrap_or(&policy.reason),
        "status":status,"rounds":record.map(|r|r.rounds.len()).unwrap_or(0),
        "max_rounds":record.map(|r|r.max_rounds).unwrap_or(policy.max_rounds),
        "packet_id":packet.map(|p|&p.packet_id),
        "context_epoch":packet.and_then(|p|p.context_epoch.as_ref()),
        "required_scope_count":packet.map(|p|p.required_scopes.len()).unwrap_or(0),
        "delivered_scope_count":packet.map(|p|p.delivered_scopes.len()).unwrap_or(0),
        "next_action":if status=="exhausted" {"Resolve the retained findings; the round limit does not authorize completion or downgrading review."} else {"Use goal_check_node without evidence to prepare current material. Read all required images in this context, then submit specific model_self_check evidence with the packet id, exact manifest, reviewed scopes and findings. Tool success is not review approval."}})
}

impl ToolRuntime {
    pub(super) async fn verify_review_render_dependencies(
        &self,
        goal: &HostGoalRecord,
        packet: &HostGoalReviewPacket,
        key: &str,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        let mut seen = std::collections::BTreeSet::new();
        for image in packet.materials.iter().filter(|m| m.kind == "image") {
            if !seen.insert(&image.path)
                || image.preview_path.as_deref() == Some(image.path.as_str())
            {
                continue;
            }
            let render_options = image
                .content
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or_else(|| json!({}));
            let recipe = json!({"source":"runtime_review_probe","path":image.path,"source_sha256":image.sha256,
                "render_dependencies_sha256":image.render_dependencies_sha256,"options":render_options,"packet_id":packet.packet_id});
            let mut proof = None;
            for generation in 0..32u8 {
                let identity = sha256(
                    &serde_json::to_vec(&(&packet.binding, key, &recipe, generation)).unwrap(),
                );
                let operation_id = format!("review_probe_{identity}");
                let db = self.host_goal_db()?;
                let prior = db
                    .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &operation_id)
                    .map_err(store_error)?;
                // A resumed Job proves its historical execution, never today's
                // renderer state. Finish it, then launch a fresh read generation.
                let fresh = prior
                    .as_ref()
                    .is_none_or(|op| !op.status.terminal() && op.job_id.is_none());
                let operation = match prior {
                    Some(operation) => operation,
                    None => {
                        db.begin_host_goal_operation_with_manifest_in_context(
                            &goal.owner_key,
                            &packet.binding,
                            &operation_id,
                            "present_document",
                            &identity,
                            &[GoalArtifactSnapshot {
                                path: image.path.clone(),
                                sha256: Some(image.sha256.clone()),
                            }],
                            now(),
                            context::invocation_context().as_ref(),
                            true,
                        )
                        .map_err(store_error)?
                        .operation
                    }
                };
                let operation = if operation.result_summary.is_none() {
                    db.finish_host_goal_operation(
                        &goal.owner_key,
                        &goal.goal_id,
                        &operation_id,
                        HostGoalOperationStatus::Unknown,
                        None,
                        Some(&recipe.to_string()),
                        now(),
                    )
                    .map_err(store_error)?
                } else {
                    operation
                };
                let current = self.resume_host_review_probe(goal, operation, auth).await?;
                if fresh {
                    proof = Some(current);
                    break;
                }
            }
            let proof=proof.ok_or_else(||failure("goal_review_probe_refresh_bound","Dependency read recovery exceeded 32 generations; retain the original Jobs and use a new review request key after inspecting them."))?;
            if proof["source_sha256"].as_str() != Some(image.sha256.as_str())
                || proof["render_dependencies_sha256"].as_str()
                    != image.render_dependencies_sha256.as_deref()
            {
                return Err(failure("goal_review_render_dependencies_changed","Actual source or rendering dependencies changed. Prepare with a new idempotency key, render all affected pages and review again; prior image approval is stale."));
            }
        }
        Ok(())
    }

    /// Resume the original read-only document Job. A lost pre-Job dispatch may
    /// repeat only this dependency read; it cannot replay a document mutation.
    pub(super) async fn resume_host_review_probe(
        &self,
        goal: &HostGoalRecord,
        operation: HostGoalOperation,
        auth: Option<&AuthContext>,
    ) -> Result<Value, ToolResult> {
        let _lease = context::lease_active_operation(&goal.goal_id, &operation.operation_id);
        let recipe:Value=operation.result_summary.as_deref().and_then(|s|serde_json::from_str(s).ok()).ok_or_else(||failure("goal_review_probe_recovery","Retry the original review submission key to reconstruct this read-only probe intent."))?;
        if recipe["source"] != "runtime_review_probe" {
            return Err(failure(
                "goal_review_probe_invalid",
                "Stored operation is not a runtime dependency probe",
            ));
        }
        if operation.status.terminal() {
            if operation.status == HostGoalOperationStatus::Succeeded {
                return Ok(recipe["proof"].clone());
            }
            return Err(failure("goal_review_probe_failed","The original dependency probe failed; inspect its retained Job and prepare new material after repair."));
        }
        let mut options = recipe["options"].clone();
        if !options.is_object() {
            options = json!({})
        }
        options["review_probe"] = json!(true);
        let args:DocumentToolCall=serde_json::from_value(if let Some(job_id)=&operation.job_id {
            json!({"project":goal.project,"job_id":job_id})
        }else{
            json!({"project":goal.project,"path":recipe["path"],"expected_sha256":recipe["source_sha256"],"review":1,"view":"pages","options":options})
        }).map_err(|e|failure("goal_review_probe_invalid",e.to_string()))?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
        let mut result = self
            .host_goal_internal_tool(ToolCall::PresentDocument(args), auth)
            .await;
        let mut job = result.output["job_id"]
            .as_str()
            .map(str::to_owned)
            .or(operation.job_id.clone());
        while result.success && result.output["status"] == "pending" && job.is_some() {
            self.host_goal_db()?
                .finish_host_goal_operation(
                    &goal.owner_key,
                    &goal.goal_id,
                    &operation.operation_id,
                    HostGoalOperationStatus::Running,
                    job.as_deref(),
                    Some(&recipe.to_string()),
                    now(),
                )
                .map_err(store_error)?;
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let args: DocumentToolCall =
                serde_json::from_value(json!({"project":goal.project,"job_id":job})).unwrap();
            let resumed = tokio::time::timeout_at(
                deadline,
                self.host_goal_internal_tool(ToolCall::PresentDocument(args), auth),
            )
            .await;
            let Ok(next) = resumed else { break };
            job = next.output["job_id"].as_str().map(str::to_owned).or(job);
            result = next;
        }
        let complete = result.success
            && result.output["status"] == "completed"
            && result.output["review_probe"]["status"] == "verified";
        let mut saved = recipe;
        if complete {
            saved["proof"] = json!({"source_sha256":result.output["review_material"]["source_sha256"],"render_dependencies_sha256":result.output["review_material"]["render_dependencies_sha256"],"checked_at":now()});
        }
        let status = if complete {
            HostGoalOperationStatus::Succeeded
        } else if result.success && result.output["status"] == "pending" && job.is_some() {
            HostGoalOperationStatus::Running
        } else if result.output["failure_kind"] == "outcome_unknown"
            || result.output["recovery_kind"] == "reconcile"
            || result.output["execution_state"] == "outcome_unknown"
            || result.output["dispatch_certainty"] == "unknown"
        {
            HostGoalOperationStatus::Unknown
        } else {
            HostGoalOperationStatus::Failed
        };
        self.host_goal_db()?
            .finish_host_goal_operation(
                &goal.owner_key,
                &goal.goal_id,
                &operation.operation_id,
                status,
                job.as_deref(),
                Some(&saved.to_string()),
                now(),
            )
            .map_err(store_error)?;
        if complete {
            return Ok(saved["proof"].clone());
        }
        Err(ToolResult::err_with_output("Rendering dependency verification is not complete; retry the same review submission to continue its original read Job.",
            json!({"error_kind":if !status.terminal(){"goal_review_probe_pending"}else{"goal_review_probe_failed"},"job_id":job,"details":result.output})))
    }

    pub(super) async fn prepare_review_packet(
        &self,
        goal: &HostGoalRecord,
        node: &HostGoalNode,
        binding: &HostGoalExecutionBinding,
        manifest: &[GoalArtifactSnapshot],
        checks: &[HostGoalCheckEvidence],
        preparation_key: &str,
        auth: Option<&AuthContext>,
    ) -> Result<HostGoalReviewRecord, ToolResult> {
        let db = self.host_goal_db()?;
        let previous = db
            .read_host_goal_review(&goal.owner_key, &goal.goal_id, &node.id)
            .map_err(store_error)?;
        let epoch = context::current_epoch();
        // An explicit preparation refreshes render dependencies and context.
        // Pagination uses present_document and never enters this path.
        if manifest.iter().any(|a| a.sha256.is_none()) {
            return Err(failure("goal_review_material_required", "Declare readable managed_artifacts or verification_paths for the actual delivery before review; arbitrary command files are not automatically supervised."));
        }
        let mut visuals = visual_paths(node, manifest);
        if let Some(record) = &previous {
            for artifact in &record.managed_artifacts {
                if artifact.kind == HostGoalDeliveryKind::Layout
                    && !visuals.contains(&artifact.path)
                {
                    visuals.push(artifact.path.clone());
                }
            }
        }
        if host_goal_review_policy(node).visual && visuals.is_empty() {
            return Err(failure("goal_review_visual_source_required", "Declare each actual layout source in managed_artifacts with kind=layout. A separate PDF cannot prove a PPTX layout."));
        }
        let mut materials = Vec::new();
        let mut required = visuals
            .iter()
            .map(|p| format!("{p}#visual_pending"))
            .collect::<Vec<_>>();
        let mut state_sha256 = None;
        if visuals.is_empty()
            && manifest
                .iter()
                .all(|entry| host_goal_layout_path(&entry.path))
        {
            if goal.state.completed_summary.trim().is_empty() && goal.state.facts.is_empty() {
                return Err(failure("goal_review_material_required","Persist the actual conclusion and supporting facts in goal state or declare real managed artifacts before review."));
            }
            let state =
                serde_json::to_string(&host_goal_review_state_material(&goal.state)).unwrap();
            let hash = sha256(state.as_bytes());
            state_sha256 = Some(hash.clone());
            required.push("goal-state".into());
            materials.push(HostGoalReviewMaterial{id:"goal-state".into(),path:"goal-state".into(),sha256:hash,kind:"state".into(),
                preview_path:None,preview_sha256:None,render_source_sha256:None,render_dependencies_sha256:None,
                content:Some(json!({"text":bounded(&state,5000),"partial":state.len()>5000,"source":"persisted_goal_state",
                    "continuation":{"tool":"goal_read","arguments":{"goal_id":goal.goal_id,"view":"detail","section":"state"}}}).to_string())});
        }
        let text_count = manifest.len().saturating_sub(visuals.len()).max(1);
        let per_file = (5000 / text_count).max(64);
        for entry in manifest
            .iter()
            .filter(|a| !visuals.contains(&a.path) && !host_goal_layout_path(&a.path))
        {
            let read = self
                .host_goal_internal_tool(
                    ToolCall::ReadFile {
                        project: goal.project.clone(),
                        path: entry.path.clone(),
                        session_id: None,
                        start_line: Some(1),
                        limit: Some(80),
                        with_line_numbers: Some(true),
                    },
                    auth,
                )
                .await;
            if !read.success || read.output["sha256"].as_str() != entry.sha256.as_deref() {
                return Err(failure(
                    "goal_review_material_unavailable",
                    format!("Cannot read current review context for {}", entry.path),
                ));
            }
            let text = read.output["text"].as_str().ok_or_else(|| {
                failure("goal_review_material_unavailable", "Read returned no text")
            })?;
            let id = format!("{}#text", entry.path);
            required.push(id.clone());
            materials.push(HostGoalReviewMaterial { id, path: entry.path.clone(), sha256: entry.sha256.clone().unwrap(),
                kind: "text".into(), preview_path: None, preview_sha256: None, render_source_sha256: None,
                render_dependencies_sha256: None,
                content: Some(json!({"text":bounded(text,per_file),"partial":text.len()>per_file || read.output["has_more"]==true,
                    "continuation":{"tool":"read_file","arguments":{"project":goal.project,"path":entry.path,"start_line":1,"limit":300}}}).to_string()),
            });
        }
        if node.delivery_kind == Some(HostGoalDeliveryKind::Code) && !manifest.is_empty() {
            let diff = self
                .host_goal_internal_tool(
                    ToolCall::GitDiffHunks {
                        project: goal.project.clone(),
                        session_id: None,
                        paths: Some(manifest.iter().map(|a| a.path.clone()).collect()),
                        max_hunks: Some(8),
                        max_hunk_lines: Some(24),
                        cached: Some(false),
                        base_commit: None,
                        head_commit: None,
                        continuation: None,
                    },
                    auth,
                )
                .await;
            let entry = &manifest[0];
            let id = format!("{}#diff", entry.path);
            required.push(id.clone());
            materials.push(HostGoalReviewMaterial {id,path:entry.path.clone(),sha256:entry.sha256.clone().unwrap(),kind:"text".into(),
                preview_path:None,preview_sha256:None,render_source_sha256:None,render_dependencies_sha256:None,
                content:Some(bounded(&json!({"success":diff.success,"actual_diff":diff.output,"error":diff.error,
                    "notice":"Bounded diff; use returned continuation for remaining hunks. Untracked/new files are represented by real source context."}).to_string(),6000))});
        }
        let machine_checks = checks
            .iter()
            .filter(|c| c.source == HostGoalEvidenceSource::Automated)
            .cloned()
            .collect::<Vec<_>>();
        let packet_id = sha256(
            &serde_json::to_vec(&(
                binding,
                &epoch,
                manifest,
                &machine_checks,
                &materials,
                preparation_key,
            ))
            .unwrap(),
        );
        if previous
            .as_ref()
            .and_then(|r| r.current_packet.as_ref())
            .is_some_and(|p| p.packet_id == packet_id)
        {
            return Ok(previous.unwrap());
        }
        let packet = HostGoalReviewPacket {
            packet_id,
            binding: binding.clone(),
            context_epoch: epoch,
            manifest: manifest.to_vec(),
            state_sha256,
            machine_checks,
            required_scopes: required,
            delivered_scopes: vec![],
            materials,
            created_at: now(),
        };
        if serde_json::to_vec(&packet).unwrap().len() > 28 * 1024 {
            return Err(failure("goal_review_material_bound", "Review packet exceeds 28 KiB. Split this delivery into bounded nodes; no review was recorded."));
        }
        db.prepare_host_goal_review(&goal.owner_key, binding, &packet, now())
            .map_err(store_error)
    }

    /// The MCP adapter calls this only after constructing native image blocks.
    /// Recreate the original request's identity; never infer it from credentials.
    pub(crate) async fn record_host_review_transport(
        &self,
        framed: &mut Value,
        tool: &str,
        args: &Value,
        auth: Option<&AuthContext>,
        window: Option<&ClientWindow>,
    ) {
        let Ok(principal) = owner(auth) else { return };
        let Ok(scope) = context::RequestScope::new(principal, window, args) else {
            return;
        };
        let outcome = context::scope(
            scope,
            self.record_host_review_images(framed, tool, args["project"].as_str(), auth),
        )
        .await;
        if let Err(error) = outcome {
            if let Some(output) = framed.pointer_mut("/structuredContent/output") {
                output["review_transport_error"] =
                    json!({"error":error.error,"details":error.output,"review_status":"pending"});
            }
        }
    }

    async fn record_host_review_images(
        &self,
        framed: &mut Value,
        tool: &str,
        requested_project: Option<&str>,
        auth: Option<&AuthContext>,
    ) -> Result<(), ToolResult> {
        let Some(output) = framed.pointer("/structuredContent/output").cloned() else {
            return Ok(());
        };
        let receipts = output["native_image_receipts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if receipts.is_empty()
            && !(tool == "read_project_artifact" && output["content_delivery"] == "mcp_image")
        {
            return Ok(());
        }
        let owner = context::current_owner().unwrap_or_default();
        let db = self.host_goal_db()?;
        let goal_id = if let Some(binding) = context::current_binding() {
            Some(binding.goal_id)
        } else {
            context::current_mode_key()
                .and_then(|window| db.host_goal_window(&owner, &window).ok().flatten())
                .map(|b| b.goal_id)
        };
        let Some(goal_id) = goal_id else {
            return Ok(());
        };
        let goal = self.host_goal_access(&goal_id, auth, false).await?;
        // Raw artifact reads intentionally omit project from their result.
        // Resolve the original authorized arguments instead of inventing a
        // project from the ambient goal or a caller-supplied result field.
        let Some(project) = output["project"].as_str().or(requested_project) else {
            return Ok(());
        };
        let resolved = self
            .execution()
            .resolve_project_input_for_auth(project, auth)
            .await
            .map_err(|e| e.into_tool_result())?;
        if resolved.resolved_id != goal.project || resolved.config.path != goal.project_root {
            return Ok(());
        }
        let Some(node_id) = goal.current_node_id.as_deref() else {
            return Ok(());
        };
        let Some(record) = db
            .read_host_goal_review(&owner, &goal_id, node_id)
            .map_err(store_error)?
        else {
            return Ok(());
        };
        let Some(packet) = record.current_packet else {
            return Ok(());
        };
        if packet.context_epoch != context::current_epoch()
            || goal.current_attempt_id.as_deref() != Some(packet.binding.attempt_id.as_str())
        {
            return Err(failure("goal_review_context_changed","Prepare review material in this actual conversation before reading images; historical delivery is not current-context coverage."));
        }
        let mut materials = Vec::new();
        let mut required = Vec::new();
        let mut delivered = Vec::new();
        if tool == "read_project_artifact" {
            let path = output["path"].as_str().unwrap_or("");
            let hash = output["sha256"].as_str().unwrap_or("");
            if !packet
                .manifest
                .iter()
                .any(|a| a.path == path && a.sha256.as_deref() == Some(hash))
            {
                return Ok(());
            }
            if ![".png", ".jpg", ".jpeg", ".webp"]
                .iter()
                .any(|ext| path.to_lowercase().ends_with(ext))
            {
                return Ok(());
            }
            let id = format!("{path}#page:1");
            required.push(id.clone());
            delivered.push(id.clone());
            materials.push(HostGoalReviewMaterial {
                id,
                path: path.into(),
                sha256: hash.into(),
                kind: "image".into(),
                preview_path: Some(path.into()),
                preview_sha256: Some(hash.into()),
                render_source_sha256: Some(hash.into()),
                render_dependencies_sha256: Some(sha256(b"native-raster-v1")),
                content: None,
            });
        } else {
            let material = &output["review_material"];
            let path = material["source_path"].as_str().unwrap_or("");
            let hash = material["source_sha256"].as_str().unwrap_or("");
            if !packet
                .manifest
                .iter()
                .any(|a| a.path == path && a.sha256.as_deref() == Some(hash))
            {
                return Ok(());
            }
            if material["render_source_path"].as_str() != Some(path)
                || material["render_source_sha256"].as_str() != Some(hash)
            {
                return Err(failure("goal_review_render_source_mismatch","Preview must originate from the exact managed source, not a separately laid-out document."));
            }
            if self
                .host_goal_file_fingerprint(&goal.project, path, auth)
                .await
                .as_deref()
                != Some(hash)
            {
                return Err(failure(
                    "goal_review_source_changed",
                    "Source changed since rendering; prepare and render its current version.",
                ));
            }
            let scopes = material["coverage"]["required"].as_array().ok_or_else(|| {
                failure(
                    "goal_review_coverage_missing",
                    "Renderer returned no full-document coverage declaration",
                )
            })?;
            if scopes.is_empty() {
                return Err(failure(
                    "goal_review_coverage_missing",
                    "No full-document coverage was declared",
                ));
            }
            for scope in scopes {
                if let Some(scope) = scope.as_str() {
                    required.push(format!("{path}#{scope}"));
                }
            }
            for receipt in &receipts {
                let scope = receipt["coverage_id"].as_str().unwrap_or("");
                if !scopes.iter().any(|s| s.as_str() == Some(scope)) {
                    continue;
                }
                if receipt["render_source_path"].as_str() != Some(path)
                    || receipt["render_source_sha256"].as_str() != Some(hash)
                {
                    continue;
                }
                let id = format!("{path}#{scope}");
                delivered.push(id.clone());
                materials.push(HostGoalReviewMaterial {
                    id,
                    path: path.into(),
                    sha256: hash.into(),
                    kind: "image".into(),
                    preview_path: receipt["path"].as_str().map(str::to_owned),
                    preview_sha256: receipt["sha256"].as_str().map(str::to_owned),
                    render_source_sha256: Some(hash.into()),
                    render_dependencies_sha256: receipt["render_dependencies_sha256"]
                        .as_str()
                        .map(str::to_owned),
                    content: Some(material["render_options"].to_string()),
                });
            }
        }
        if materials.is_empty() {
            return Ok(());
        }
        let updated = db
            .record_host_goal_review_delivery(
                &owner,
                &packet.binding,
                &packet.packet_id,
                context::current_epoch().as_deref(),
                &materials,
                &required,
                &delivered,
                now(),
            )
            .map_err(store_error)?;
        if let Some(out) = framed.pointer_mut("/structuredContent/output") {
            let p = updated.current_packet.as_ref().unwrap();
            out["review_receipt"] = json!({"packet_id":p.packet_id,"source":"runtime_mcp_image","context_epoch":p.context_epoch,
                "delivered_scopes":p.delivered_scopes,"missing_scopes":p.required_scopes.iter().filter(|s|!p.delivered_scopes.contains(s)).collect::<Vec<_>>(),
                "notice":"Native image blocks were included in this response. This does not prove internal model understanding or approval."});
        }
        // Keep the model's text and structured representations consistent.
        if framed["content"][0]["type"] == "text" {
            framed["content"][0]["text"] = json!(framed["structuredContent"].to_string());
        }
        Ok(())
    }
}
