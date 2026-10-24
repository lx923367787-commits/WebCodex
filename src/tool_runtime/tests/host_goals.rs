//! Host goal acceptance uses actual dispatch, local SQLite, and Runner project
//! resolution. The model-review fixture intentionally needs no ACP provider.
use super::support::{auth_context, registered_project};
use crate::auth::AuthContext;
use crate::runner_protocol::{RunnerCapabilities, RunnerRegisterRequest};
use crate::tool_runtime::kernel::{
    HostFileImportTrust, ToolCallContext, ToolCallRequest, ToolTransport,
};
use crate::tool_runtime::{ToolCall, ToolResult, ToolRuntime};
use serde_json::{json, Value};
use std::sync::Arc;

#[path = "host_goal_handoff_background.rs"]
mod background_handoff;
#[path = "host_goal_handoff.rs"]
mod local_handoff;
#[path = "host_goal_sync_receipts.rs"]
mod sync_receipts;

struct Fixture {
    temp: tempfile::TempDir,
    server: tempfile::TempDir,
    db: Arc<crate::Database>,
    runtime: ToolRuntime,
    project: String,
    auth: AuthContext,
}

impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let server = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::Database::open(&server.path().join("host-goals.db")).unwrap());
        let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
        let owner = format!("host-goal-owner-{}", uuid::Uuid::new_v4().simple());
        let mut auth = auth_context(Some(&owner), false);
        auth.scopes = ["runtime:read", "project:read", "project:write"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let project = register_owned_project(&runtime, temp.path(), &auth).await;
        Self {
            temp,
            server,
            db,
            runtime,
            project,
            auth,
        }
    }

    async fn call(&self, call: ToolCall) -> ToolResult {
        self.call_in_chat(call, "chat-original", None).await
    }

    async fn call_in_chat(
        &self,
        call: ToolCall,
        epoch: &str,
        binding: Option<&Value>,
    ) -> ToolResult {
        if call.tool_name().starts_with("goal_") {
            crate::test_support::runner_project_files::during(
                &self.runtime,
                "host-goal-runner",
                "inst",
                self.temp.path(),
                dispatch_in_chat(&self.runtime, &self.auth, call, epoch, binding),
            )
            .await
        } else {
            dispatch_in_chat(&self.runtime, &self.auth, call, epoch, binding).await
        }
    }

    async fn start(&self) -> Value {
        self.start_in_task(None).await
    }

    async fn start_in_task(&self, task_directory: Option<&str>) -> Value {
        let result = self
            .call(ToolCall::GoalModeStart {
                task_directory: task_directory.map(str::to_owned),
                project: self.project.clone(),
                objective: "Produce a sourced analysis".into(),
                idempotency_key: "start".into(),
                context_epoch: Some("chat-original".into()),
                context_warning_tokens: None,
                context_handoff_tokens: None,
            })
            .await;
        success(&result);
        assert_eq!(result.output["goal"]["phase"], "planning");
        assert_workflow_overview(&result.output);
        result.output["goal"].clone()
    }

    async fn bootstrap(&self, goal: &Value, epoch: &str) -> ToolResult {
        let result = self
            .call_in_chat(
                ToolCall::GoalBootstrap {
                    goal_id: Some(id(goal)),
                    project: None,
                    context_epoch: Some(epoch.into()),
                    activate: true,
                },
                epoch,
                None,
            )
            .await;
        success(&result);
        assert_workflow_overview(&result.output);
        result
    }

    async fn save_plan(&self, goal: &Value, plan: Value, key: &str) -> ToolResult {
        self.call(ToolCall::GoalPlanSave {
            goal_id: id(goal),
            expected_revision: revision(goal),
            idempotency_key: key.into(),
            plan,
            change_request: None,
        })
        .await
    }

    async fn confirm(&self, goal: &Value, key: &str) -> ToolResult {
        self.call(ToolCall::GoalConfirmPlan {
            goal_id: id(goal),
            expected_revision: revision(goal),
            plan_version: goal["plan_version"].as_u64().unwrap(),
            plan_sha256: goal["plan_hash"].as_str().unwrap().into(),
            confirmation: "我已经查看本地计划，确认此版本，现在开始执行。".into(),
            idempotency_key: key.into(),
        })
        .await
    }

    async fn claim(&self, goal: &Value, key: &str) -> ToolResult {
        let _ = self
            .call(ToolCall::GoalRead {
                source_context_epoch: None,
                goal_id: id(goal),
                round_offset: None,
                round_limit: None,
                history_after: None,
                history_limit: None,
                history_kind: None,
                view: Some("node".into()),
                node_id: Some("research".into()),
                section: None,
                cursor: None,
            })
            .await;
        self.call(ToolCall::GoalClaimNode {
            goal_id: id(goal),
            node_id: "research".into(),
            expected_revision: revision(goal),
            plan_version: goal["plan_version"].as_u64().unwrap(),
            idempotency_key: key.into(),
            context_epoch: Some("chat-original".into()),
            rework: false,
            takeover_attempt_id: None,
        })
        .await
    }

    async fn detail(&self, goal: &Value, section: &str) -> Value {
        self.detail_after(goal, section, None).await
    }

    async fn detail_after(&self, goal: &Value, section: &str, after: Option<u64>) -> Value {
        let mut text = String::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self
                .call(ToolCall::GoalRead {
                    source_context_epoch: None,
                    goal_id: id(goal),
                    round_offset: None,
                    round_limit: None,
                    history_after: after,
                    history_limit: None,
                    history_kind: None,
                    view: Some("detail".into()),
                    node_id: None,
                    section: Some(section.into()),
                    cursor: cursor.clone(),
                })
                .await;
            success(&page);
            assert_eq!(page.output["goal"]["revision"], goal["revision"]);
            assert_eq!(page.output["section"], section);
            assert!(serde_json::to_vec(&page.output).unwrap().len() <= 8192);
            text.push_str(page.output["text"].as_str().unwrap());
            cursor = page.output["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            assert!(
                seen.insert(cursor.clone()),
                "Detail pagination must advance"
            );
            assert!(seen.len() < 2048, "Detail pagination must remain bounded");
        }
        serde_json::from_str(&text).unwrap()
    }
}

async fn register_owned_project(
    runtime: &ToolRuntime,
    root: &std::path::Path,
    auth: &AuthContext,
) -> String {
    // Personal API-token visibility is owner-based. The generic shared-key
    // helper records owner=None, which makes its Runner invisible to this user.
    runtime
        .runner_registry
        .register_with_auth(
            RunnerRegisterRequest {
                agent_execution_resources: Vec::new(),
                client_id: "host-goal-runner".into(),
                runner_instance_id: "inst".into(),
                runner_protocol_generation: crate::runner_protocol::RUNNER_PROTOCOL_GENERATION_V2,
                process_started_at: None,
                build: None,
                job_concurrency_limit: None,
                job_inventory: None,
                coding_agent_providers: None,
                coding_agent_inventory: None,
                display_name: None,
                owner: auth.username.clone(),
                hostname: None,
                host_context: None,
                capabilities: crate::test_support::current_runner_capabilities(
                    RunnerCapabilities {
                        file_read: true,
                        file_write: true,
                        shell: true,
                        git: true,
                        internal_posix_script: true,
                        ..Default::default()
                    },
                ),
                policy: None,
            },
            Some(&crate::test_support::runner_access(auth)),
        )
        .await
        .unwrap();
    crate::test_support::apply_project_inventory_snapshot(
        &runtime.runner_registry,
        "host-goal-runner",
        "inst",
        vec![registered_project("project", &root.to_string_lossy())],
    )
    .await;
    let project = crate::tool_runtime::runner_project_runtime_id("host-goal-runner", "project");
    assert!(
        runtime
            .execution()
            .resolve_project_input_for_auth(&project, Some(auth))
            .await
            .is_ok(),
        "fixture Runner must be visible to its actual non-admin owner"
    );
    project
}

fn assert_workflow_overview(output: &Value) {
    let overview = output["workflow_overview"]
        .as_str()
        .expect("entry workflow map");
    for boundary in [
        "Map only, not a restart or permission",
        "read inputs -> goal_plan_save -> wait for explicit user confirmation of that version",
        "goal_read(view=node) -> goal_claim_node -> execute with the current binding -> goal_check_node",
        "same Goal's local state, plan and recovery instructions",
        "new chat does not require reconfirming an unchanged confirmed plan",
        "awaiting_confirmation = read-only investigation, no business execution",
        "completed = report completion, never choose replacement work",
        "draining/retired forbid new business/claims or reactivation",
        "original Operation, Job, Run or receipt before retrying effects",
        "current node, Attempt/Fence and goal_context",
        "Keep chat identity/context_epoch fixed",
        "Verify saving from tool receipts; completion requires passed acceptance, not model assertions",
        "Handoff saves a task checkpoint and freezes source business",
        "prepares and commits through goal_handoff",
        "Only server-confirmed ownership permits continuation",
        "source retirement occurs atomically with transfer",
        "Old execution chains must have stopped",
        "Four failed attempts stop execution and preserve manual recovery state",
    ] {
        assert!(overview.contains(boundary), "missing boundary: {boundary}");
    }
    assert!(overview.chars().count() <= 3072);
    assert!(serde_json::to_vec(output).unwrap().len() <= 8192);
    assert!(
        output.get("context_read").is_none(),
        "entry map never acknowledges node context"
    );
}

async fn dispatch_in_chat(
    runtime: &ToolRuntime,
    auth: &AuthContext,
    call: ToolCall,
    epoch: &str,
    binding: Option<&Value>,
) -> ToolResult {
    let tool_name = call.tool_name().to_owned();
    let mut arguments = serde_json::to_value(&call).unwrap()["params"].clone();
    arguments["context_epoch"] = json!(epoch);
    if let Some(binding) = binding {
        let mut metadata = binding.clone();
        metadata["operation_id"] = json!("checkpoint-operation");
        metadata["context_epoch"] = json!(epoch);
        arguments["goal_context"] = metadata;
    }
    let outcome = runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name,
                arguments,
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(auth),
                window: None,
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await;
    assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
    outcome.result.expect("kernel result")
}

async fn kernel_call(
    fixture: &Fixture,
    window: &crate::client_window::ClientWindow,
    tool: &str,
    arguments: Value,
) -> ToolResult {
    let future = fixture.runtime.call_tool_with_context(
        ToolCallRequest {
            tool_name: tool.into(),
            arguments,
        },
        ToolCallContext {
            transport: ToolTransport::Mcp,
            session_id: None,
            auth: Some(&fixture.auth),
            window: Some(window),
            record_oauth_scope_denials: true,
            host_file_import_trust: HostFileImportTrust::Untrusted,
        },
    );
    let outcome = crate::test_support::runner_project_files::during(
        &fixture.runtime,
        "host-goal-runner",
        "inst",
        fixture.temp.path(),
        future,
    )
    .await;
    assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
    outcome.result.expect("kernel model result")
}

fn success(result: &ToolResult) {
    assert!(result.success, "{:?}: {}", result.error, result.output);
}
fn id(goal: &Value) -> String {
    goal["goal_id"].as_str().unwrap().into()
}
fn revision(goal: &Value) -> u64 {
    goal["revision"].as_u64().unwrap()
}

fn plan() -> Value {
    json!({
        "objective": "Produce a sourced analysis", "constraints": ["Keep citations with conclusions"],
        "deliverables": ["A sourced conclusion recorded in state"],
        "final_criteria": [{"kind":"review","id":"final_sources","description":"Final conclusion cites its supporting source"}],
        "nodes": [{"id":"research","title":"Research and conclude","inputs":["User question"],
            "work":"Read the source and record the supported conclusion", "outputs":["Sourced analysis in state"],
            "criterion_ids":["final_sources"], "acceptance":[{"kind":"review","id":"node_sources","description":"Research result includes its source"}],
            "failure_action":"Record missing sources and return to research"}],
        "state_management": {"facts":"Store supported facts", "decisions":"Record choices with reasons", "issues":"Retain unresolved questions",
            "artifacts":"Record source references", "checkpoint_triggers":["After source review", "Before changing chat"], "resume_instructions":"Read local state and current plan, then continue the incomplete node"}
    })
}

fn review_evidence(passed: bool) -> Value {
    Value::Array(
        ["node_sources", "final_sources"]
            .map(|criterion_id| {
                json!({
                    "criterion_id":criterion_id,"passed":passed,"source":"model_self_check",
                    "evidence":"Read the saved source and checked the conclusion against it",
                    "source_refs":["https://example.test/source"],"artifacts":[]
                })
            })
            .into(),
    )
}

fn prepared_review_evidence(packet: &Value) -> Value {
    let mut evidence = review_evidence(true);
    for item in evidence.as_array_mut().unwrap() {
        item["review_packet_id"] = packet["packet_id"].clone();
        item["artifacts"] = packet["manifest"].clone();
        item["reviewed_scopes"] = packet["required_scopes"].clone();
        item["source_refs"] = json!(["persisted goal state: completed_summary and facts"]);
        item["evidence"] = json!("Read the returned persisted goal-state conclusion and supporting source facts; the recorded conclusion agrees with that evidence, and no unresolved issue remains in the reviewed state.");
    }
    evidence
}

#[tokio::test]
async fn host_goals_local_plan_requires_subsequent_confirmation_then_nodes_can_complete() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let saved = fixture.save_plan(&goal, plan(), "plan-one").await;
    success(&saved);
    let awaiting = &saved.output["goal"];
    assert_eq!(awaiting["phase"], "awaiting_confirmation");
    assert!(awaiting["confirmation"].is_null());
    let files = &saved.output["plan_artifacts"];
    let json_path = std::path::Path::new(files["json_path"].as_str().unwrap());
    let markdown_path = std::path::Path::new(files["markdown_path"].as_str().unwrap());
    assert!(std::fs::canonicalize(&json_path)
        .unwrap()
        .starts_with(std::fs::canonicalize(fixture.temp.path().join("goals")).unwrap()));
    assert!(markdown_path.is_file());
    let persisted: Value = serde_json::from_slice(&std::fs::read(json_path).unwrap()).unwrap();
    assert!(persisted.to_string().contains("final_sources"));
    let markdown = std::fs::read_to_string(markdown_path).unwrap();
    assert!(markdown.contains("final_sources"));
    assert!(markdown.contains("checkpoint_triggers"));
    assert_eq!(saved.output["plan_artifacts_valid"], true);
    assert!(!fixture.claim(awaiting, "too-early").await.success);

    let confirmed = fixture.confirm(awaiting, "confirm-one").await;
    success(&confirmed);
    assert_eq!(confirmed.output["goal"]["phase"], "ready");
    assert_eq!(
        confirmed.output["goal"]["confirmation"]["plan_hash"],
        awaiting["plan_hash"]
    );
    let claimed = fixture.claim(&confirmed.output["goal"], "claim-one").await;
    success(&claimed);
    assert_eq!(claimed.output["goal"]["phase"], "executing");
    let binding = &claimed.output["binding"];
    let checkpoint = fixture.call_in_chat(ToolCall::GoalCheckpoint {
        goal_id: id(awaiting), expected_revision: revision(&claimed.output["goal"]), idempotency_key: "checkpoint-one".into(),
        patch: json!({"reason":"Source review finished","state":{"completed_summary":"The source supports the conclusion","facts":{"source":"https://example.test/source"},"decisions":["Use the primary source"],"issues":[],"artifact_refs":[],"next_step":"Run acceptance","plan_draft":null}}),
        context_epoch: Some("chat-original".into()),
    }, "chat-original", Some(binding)).await;
    success(&checkpoint);
    assert_eq!(
        checkpoint.output["progress_summary"]["completed_summary"],
        "The source supports the conclusion"
    );
    let prepared = fixture
        .call(ToolCall::GoalCheckNode {
            goal_id: id(awaiting),
            node_id: "research".into(),
            attempt_id: binding["attempt_id"].as_str().unwrap().into(),
            expected_revision: revision(&checkpoint.output["goal"]),
            plan_version: awaiting["plan_version"].as_u64().unwrap(),
            idempotency_key: "prepare-state-review".into(),
            evidence: None,
        })
        .await;
    success(&prepared);
    assert_eq!(prepared.output["goal"]["phase"], "executing");
    let packet = &prepared.output["review_packet"];
    assert!(packet["materials"]
        .as_array()
        .unwrap()
        .iter()
        .any(|material| material["content"]
            .as_str()
            .is_some_and(|text| text.contains("The source supports the conclusion"))));
    let checked = fixture
        .call(ToolCall::GoalCheckNode {
            goal_id: id(awaiting),
            node_id: "research".into(),
            attempt_id: binding["attempt_id"].as_str().unwrap().into(),
            expected_revision: revision(&checkpoint.output["goal"]),
            plan_version: awaiting["plan_version"].as_u64().unwrap(),
            idempotency_key: "check-one".into(),
            evidence: Some(prepared_review_evidence(packet)),
        })
        .await;
    success(&checked);
    assert_eq!(checked.output["goal"]["phase"], "completed");
    assert_eq!(
        checked.output["goal"]["completed_node_ids"],
        json!(["research"])
    );
    assert!(checked.output.get("workflow_overview").is_none());
    let resumed = fixture
        .bootstrap(&checked.output["goal"], "completed-chat")
        .await;
    assert_eq!(resumed.output["goal"], checked.output["goal"]);
    assert_eq!(resumed.output["confirmation_required"], false);
    assert!(
        checked.output.get("attempt").is_none(),
        "Completion receipts stay bounded; fetch evidence explicitly"
    );
    let attempts = fixture.detail(&checked.output["goal"], "attempts").await;
    assert!(attempts["items"][0]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|check| check["source"] == "model_self_check"));
}

#[tokio::test]
async fn host_goals_serial_state_reviews_keep_current_final_delivery_ready() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let mut serial_plan = plan();
    let mut final_node = serial_plan["nodes"][0].clone();
    final_node["id"] = json!("finalize");
    final_node["title"] = json!("Refine the sourced conclusion");
    final_node["work"] = json!("Replace the preliminary state conclusion with the final analysis");
    final_node["acceptance"][0]["id"] = json!("second_sources");
    serial_plan["nodes"]
        .as_array_mut()
        .unwrap()
        .push(final_node);
    let saved = fixture
        .save_plan(&goal, serial_plan, "serial-state-plan")
        .await;
    success(&saved);
    let confirmed = fixture
        .confirm(&saved.output["goal"], "serial-state-confirm")
        .await;
    success(&confirmed);
    let mut current = confirmed.output["goal"].clone();
    let mut state_hashes = Vec::new();
    for (index, (node_id, conclusion)) in [
        (
            "research",
            "Preliminary source supports a provisional conclusion",
        ),
        (
            "finalize",
            "Final source comparison supports the revised final conclusion",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let read = fixture
            .call(ToolCall::GoalRead {
                source_context_epoch: None,
                goal_id: id(&goal),
                round_offset: None,
                round_limit: None,
                history_after: None,
                history_limit: None,
                history_kind: None,
                view: Some("node".into()),
                node_id: Some(node_id.into()),
                section: None,
                cursor: None,
            })
            .await;
        success(&read);
        let claimed = fixture
            .call(ToolCall::GoalClaimNode {
                goal_id: id(&goal),
                node_id: node_id.into(),
                expected_revision: revision(&current),
                plan_version: 1,
                idempotency_key: format!("serial-state-claim-{index}"),
                context_epoch: Some("chat-original".into()),
                rework: false,
                takeover_attempt_id: None,
            })
            .await;
        success(&claimed);
        let binding = &claimed.output["binding"];
        let recorded = fixture
            .call_in_chat(
                ToolCall::GoalCheckpoint {
                    goal_id: id(&goal),
                    expected_revision: revision(&claimed.output["goal"]),
                    idempotency_key: format!("serial-state-record-{index}"),
                    context_epoch: Some("chat-original".into()),
                    patch: json!({"reason":"Record this planned stage's actual conclusion",
                "state":{"completed_summary":conclusion,
                    "facts":{"source":format!("https://example.test/source/{node_id}")},
                    "next_step":"Review the persisted state for this stage"}}),
                },
                "chat-original",
                Some(binding),
            )
            .await;
        success(&recorded);
        let check_call = |key: &str, evidence: Option<Value>| ToolCall::GoalCheckNode {
            goal_id: id(&goal),
            node_id: node_id.into(),
            attempt_id: binding["attempt_id"].as_str().unwrap().into(),
            expected_revision: revision(&recorded.output["goal"]),
            plan_version: 1,
            idempotency_key: key.into(),
            evidence,
        };
        let prepared = fixture
            .call(check_call(&format!("serial-state-prepare-{index}"), None))
            .await;
        success(&prepared);
        assert_eq!(prepared.output["goal"]["phase"], "executing");
        let packet = &prepared.output["review_packet"];
        assert!(packet["materials"].to_string().contains(conclusion));
        state_hashes.push(packet["state_sha256"].as_str().unwrap().to_owned());
        let mut evidence = prepared_review_evidence(packet);
        if index == 0 {
            evidence.as_array_mut().unwrap().truncate(1);
        } else {
            evidence[0]["criterion_id"] = json!("second_sources");
        }
        let checked = fixture
            .call(check_call(
                &format!("serial-state-pass-{index}"),
                Some(evidence),
            ))
            .await;
        success(&checked);
        assert_eq!(
            checked.output["goal"]["phase"],
            if index == 0 { "ready" } else { "completed" }
        );
        current = checked.output["goal"].clone();
    }
    assert_ne!(
        state_hashes[0], state_hashes[1],
        "The final planned state genuinely replaces the earlier result"
    );
    let final_read = fixture
        .call(ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: id(&goal),
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("summary".into()),
            node_id: None,
            section: None,
            cursor: None,
        })
        .await;
    success(&final_read);
    assert_eq!(final_read.output["goal"]["phase"], "completed");
    assert_eq!(final_read.output["delivery_ready"], true,
        "The latest reviewed state is current; historical state hashes are archived milestones: {:?}", final_read.output);
}

#[tokio::test]
async fn host_goals_document_admission_enrolls_review_before_async_job_dispatch() {
    use crate::tool_runtime::host_goal_context::{self as context, RequestScope};
    use webcodex_core::host_goal::*;

    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    std::fs::write(
        fixture.temp.path().join("marker.txt"),
        "machine check input\n",
    )
    .unwrap();
    let criterion = json!({"kind":"file_exists","id":"exists",
        "description":"The checked marker exists","path":"marker.txt"});
    let mut lightweight = plan();
    lightweight["final_criteria"] = json!([criterion.clone()]);
    lightweight["nodes"][0]["acceptance"] = json!([criterion]);
    lightweight["nodes"][0]["criterion_ids"] = json!(["exists"]);
    lightweight["nodes"][0]["delivery_kind"] = json!("lightweight");
    lightweight["nodes"][0]["review"] = json!(0);
    let saved = fixture
        .save_plan(&goal, lightweight, "admission-plan")
        .await;
    success(&saved);
    let confirmed = fixture
        .confirm(&saved.output["goal"], "admission-confirm")
        .await;
    success(&confirmed);
    let claim = fixture
        .claim(&confirmed.output["goal"], "admission-claim")
        .await;
    success(&claim);
    let principal = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    let binding: HostGoalExecutionBinding =
        serde_json::from_value(claim.output["binding"].clone()).unwrap();
    assert!(fixture
        .db
        .read_host_goal_review(&principal, &id(&goal), "research")
        .unwrap()
        .is_none());
    let mut call_binding = claim.output["binding"].clone();
    call_binding["context_epoch"] = json!("chat-original");
    call_binding["operation_id"] = json!("async-document-admission");
    let request = json!({"context_epoch":"chat-original","goal_context":call_binding});
    let payload = json!({"params":{"project":fixture.project,"path":"source.docx","output_path":"report.pdf"}});
    let scope = RequestScope::new(principal.clone(), None, &request).unwrap();
    crate::test_support::runner_project_files::during(
        &fixture.runtime,
        "host-goal-runner",
        "inst",
        fixture.temp.path(),
        context::scope(
            scope,
            fixture.runtime.host_goal_admit_effect(
                "document_convert",
                Some(&fixture.project),
                &payload,
                Some(&fixture.auth),
                true,
            ),
        ),
    )
    .await
    .unwrap();
    // No worker or native-image transport is used: this asserts admission state,
    // not successful conversion, model review, or actual visual quality.
    let review = fixture
        .db
        .read_host_goal_review(&principal, &id(&goal), "research")
        .unwrap()
        .unwrap();
    assert_eq!(review.review, 1);
    assert!(review
        .managed_artifacts
        .iter()
        .any(|artifact| artifact.path == "report.pdf"
            && artifact.kind == HostGoalDeliveryKind::Layout));
    assert!(review.current_packet.is_none() && review.rounds.is_empty());
    let operation = fixture
        .db
        .get_host_goal_operation(&principal, &id(&goal), "async-document-admission")
        .unwrap()
        .unwrap();
    assert!(
        !operation.status.terminal(),
        "Review must be required before the async operation finishes"
    );

    // Simulate only a generic terminal observation, deliberately skipping the
    // document facade's continuation that used to be the sole enrolment path.
    fixture
        .db
        .finish_host_goal_operation(
            &principal,
            &id(&goal),
            "async-document-admission",
            HostGoalOperationStatus::Succeeded,
            None,
            Some("Simulated generic operation completion; no document review receipt"),
            100,
        )
        .unwrap();
    let marker_hash = crate::tool_runtime::host_goal_files::sha256(
        &std::fs::read(fixture.temp.path().join("marker.txt")).unwrap(),
    );
    let evidence: HostGoalCheckEvidence = serde_json::from_value(json!({
        "criterion_id":"exists","passed":true,"source":"automated",
        "evidence":"Actual marker.txt exists and its current bytes were fingerprinted",
        "source_refs":["marker.txt"],"artifacts":[{"path":"marker.txt","sha256":marker_hash}]
    }))
    .unwrap();
    let denied = fixture
        .db
        .check_host_goal_node_with_request_in_context(
            &principal,
            &binding,
            revision(&claim.output["goal"]),
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&evidence),
            "machine-only-completion",
            "machine-only-completion-request",
            101,
            Some(&HostGoalInvocationContext {
                context_epoch: "chat-original".into(),
                identity_source: "explicit_context_only".into(),
            }),
        )
        .unwrap_err();
    assert!(
        denied.message().contains("Review"),
        "The review gate must reject machine-only completion: {denied:?}"
    );
    assert_eq!(
        fixture
            .db
            .get_host_goal(&principal, &id(&goal))
            .unwrap()
            .unwrap()
            .phase,
        HostGoalPhase::Executing
    );
}

#[tokio::test]
async fn host_goals_task_directory_publishes_real_runner_files_and_absolute_receipts() {
    for absolute_input in [false, true] {
        let fixture = Fixture::new().await;
        let task_root = fixture.temp.path().join("selected/task");
        let absolute = task_root.to_string_lossy().into_owned();
        let goal = fixture
            .start_in_task(Some(if absolute_input {
                &absolute
            } else {
                "selected/task"
            }))
            .await;
        assert_eq!(goal["task_directory"], "selected/task");
        let saved = fixture.save_plan(&goal, plan(), "task-plan").await;
        success(&saved);
        for (key, name) in [("json_path", "plan.json"), ("markdown_path", "plan.md")] {
            let shown = std::path::Path::new(saved.output["plan_artifacts"][key].as_str().unwrap());
            let expected = task_root
                .join("goals")
                .join(id(&goal))
                .join("plans/v1")
                .join(name);
            assert!(
                shown.is_absolute(),
                "Users must see the actual Runner filesystem path"
            );
            assert_eq!(
                shown.canonicalize().unwrap(),
                expected.canonicalize().unwrap()
            );
            assert!(std::fs::metadata(shown).unwrap().len() > 0);
        }
        assert!(
            !fixture.server.path().join("goals").exists(),
            "New task plans must not be written beside the Server database"
        );
        let before = std::fs::read(
            saved.output["plan_artifacts"]["json_path"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let replay = fixture.save_plan(&goal, plan(), "task-plan").await;
        success(&replay);
        assert_eq!(replay.output["replayed"], true);
        assert_eq!(
            std::fs::read(
                replay.output["plan_artifacts"]["json_path"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn host_goals_task_directory_rejects_escape_before_creating_goal_files() {
    let fixture = Fixture::new().await;
    let outside = fixture
        .server
        .path()
        .join("must-not-create")
        .to_string_lossy()
        .into_owned();
    for (index, directory) in ["../escape", "selected/../../escape", "", outside.as_str()]
        .into_iter()
        .enumerate()
    {
        let result = fixture
            .call(ToolCall::GoalModeStart {
                project: fixture.project.clone(),
                task_directory: Some(directory.into()),
                objective: "Reject an out-of-project task folder".into(),
                idempotency_key: format!("invalid-folder-{index}"),
                context_epoch: Some("chat-original".into()),
                context_warning_tokens: None,
                context_handoff_tokens: None,
            })
            .await;
        assert!(
            !result.success,
            "Accepted invalid task directory {directory}: {:?}",
            result.output
        );
        assert_eq!(result.output["error_kind"], "invalid_goal_task_directory");
    }
    assert!(!fixture.temp.path().join("goals").exists());
    assert!(!fixture.server.path().join("must-not-create").exists());
}

#[tokio::test]
async fn host_goals_legacy_plan_restores_without_moving_or_rehashing_its_local_files() {
    let fixture = Fixture::new().await;
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    let legacy = fixture
        .db
        .create_host_goal_at(
            &owner,
            &fixture.project,
            &fixture.temp.path().to_string_lossy(),
            "Produce a sourced analysis",
            "legacy-create",
            1,
        )
        .unwrap()
        .goal;
    assert!(legacy.task_directory.is_none());
    let bootstrap = fixture
        .call(ToolCall::GoalBootstrap {
            goal_id: Some(legacy.goal_id.clone()),
            project: None,
            context_epoch: Some("chat-original".into()),
            activate: true,
        })
        .await;
    success(&bootstrap);
    let saved = fixture
        .save_plan(&bootstrap.output["goal"], plan(), "legacy-plan")
        .await;
    success(&saved);
    let path = std::path::PathBuf::from(
        saved.output["plan_artifacts"]["json_path"]
            .as_str()
            .unwrap(),
    );
    assert!(path
        .canonicalize()
        .unwrap()
        .starts_with(fixture.server.path().canonicalize().unwrap()));
    assert!(!fixture.temp.path().join("goals").exists());
    let before = std::fs::read(&path).unwrap();
    let hash = saved.output["goal"]["plan_hash"].clone();
    let restored_db =
        Arc::new(crate::Database::open(&fixture.server.path().join("host-goals.db")).unwrap());
    let runtime = ToolRuntime::new_for_tests().with_communication_database(restored_db);
    register_owned_project(&runtime, fixture.temp.path(), &fixture.auth).await;
    let recovered = crate::test_support::runner_project_files::during(
        &runtime,
        "host-goal-runner",
        "inst",
        fixture.temp.path(),
        dispatch_in_chat(
            &runtime,
            &fixture.auth,
            ToolCall::GoalBootstrap {
                goal_id: Some(legacy.goal_id),
                project: None,
                context_epoch: Some("legacy-restored-chat".into()),
                activate: true,
            },
            "legacy-restored-chat",
            None,
        ),
    )
    .await;
    success(&recovered);
    assert!(recovered.output["goal"]["task_directory"].is_null());
    assert_eq!(recovered.output["goal"]["plan_hash"], hash);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!fixture.temp.path().join("goals").exists());
}

#[tokio::test]
async fn host_goals_fresh_chat_reads_local_current_task_after_database_reopen() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let checkpoint = fixture.call(ToolCall::GoalCheckpoint {
        goal_id: id(&goal), expected_revision: revision(&goal), idempotency_key: "planning-draft".into(),
        patch: json!({"reason":"Need one requirement clarified","state":{"completed_summary":"Read requirements","issues":["Unclear delivery format"],"next_step":"Clarify output format","plan_draft":{"nodes":["research"]}}}),
        context_epoch: Some("chat-original".into()),
    }).await;
    success(&checkpoint);
    let reopened =
        Arc::new(crate::Database::open(&fixture.server.path().join("host-goals.db")).unwrap());
    let runtime = ToolRuntime::new_for_tests().with_communication_database(reopened);
    register_owned_project(&runtime, fixture.temp.path(), &fixture.auth).await;
    let recovered = crate::test_support::runner_project_files::during(
        &runtime,
        "host-goal-runner",
        "inst",
        fixture.temp.path(),
        dispatch_in_chat(
            &runtime,
            &fixture.auth,
            ToolCall::GoalBootstrap {
                goal_id: None,
                project: None,
                context_epoch: Some("fresh-chat".into()),
                activate: true,
            },
            "fresh-chat",
            None,
        ),
    )
    .await;
    success(&recovered);
    assert_workflow_overview(&recovered.output);
    assert_eq!(recovered.output["goal"]["goal_id"], goal["goal_id"]);
    assert_eq!(recovered.output["goal"]["phase"], "planning");
    assert_eq!(
        recovered.output["goal"]["revision"],
        checkpoint.output["goal"]["revision"]
    );
    assert_eq!(
        recovered.output["progress_summary"]["issues"][0],
        "Unclear delivery format"
    );
    assert_eq!(
        recovered.output["progress_summary"]["next_step"],
        "Clarify output format"
    );
    assert!(serde_json::to_vec(&recovered.output).unwrap().len() <= 8192);
    assert_eq!(
        recovered.output["continuation"]["arguments"]["section"],
        "goal"
    );
    assert_eq!(recovered.output["progress_summary"]["partial"], true);
    assert!(recovered.output["goal"].get("state").is_none());
}

#[tokio::test]
async fn host_goals_plan_changes_clear_confirmation_and_changed_local_files_block_claim() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let first = fixture.save_plan(&goal, plan(), "plan-one").await;
    success(&first);
    let confirmed = fixture.confirm(&first.output["goal"], "confirm-one").await;
    success(&confirmed);
    let mut revised_plan = plan();
    revised_plan["nodes"][0]["work"] = json!("Review both source context and conclusion");
    let revised = fixture
        .save_plan(&confirmed.output["goal"], revised_plan, "plan-two")
        .await;
    success(&revised);
    assert_eq!(revised.output["goal"]["plan_version"], 2);
    assert_eq!(revised.output["goal"]["phase"], "awaiting_confirmation");
    assert!(revised.output["goal"]["confirmation"].is_null());
    assert!(
        !fixture
            .claim(&revised.output["goal"], "old-confirmation")
            .await
            .success
    );
    let old_hash = fixture
        .call(ToolCall::GoalConfirmPlan {
            goal_id: id(&goal),
            expected_revision: revision(&revised.output["goal"]),
            plan_version: 1,
            plan_sha256: first.output["goal"]["plan_hash"].as_str().unwrap().into(),
            confirmation: "Start version one".into(),
            idempotency_key: "wrong-version".into(),
        })
        .await;
    assert!(!old_hash.success);
    let current = fixture
        .confirm(&revised.output["goal"], "confirm-two")
        .await;
    success(&current);
    let path = revised.output["plan_artifacts"]["markdown_path"]
        .as_str()
        .unwrap();
    std::fs::write(path, "Edited outside goal_plan_save").unwrap();
    let refused = fixture
        .claim(&current.output["goal"], "tampered-file")
        .await;
    assert!(!refused.success);
    assert_eq!(refused.output["error_kind"], "goal_task_file_changed");
}

#[tokio::test]
async fn host_goals_reject_incomplete_acceptance_and_model_claimed_automated_evidence() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let mut incomplete = plan();
    incomplete["final_criteria"] = json!([]);
    assert!(
        !fixture
            .save_plan(&goal, incomplete, "incomplete")
            .await
            .success
    );
    let saved = fixture.save_plan(&goal, plan(), "complete").await;
    success(&saved);
    let confirmed = fixture.confirm(&saved.output["goal"], "confirm").await;
    success(&confirmed);
    let claimed = fixture.claim(&confirmed.output["goal"], "claim").await;
    success(&claimed);
    let mut evidence = review_evidence(true);
    evidence[0]["source"] = json!("automated");
    let checked = fixture
        .call(ToolCall::GoalCheckNode {
            goal_id: id(&goal),
            node_id: "research".into(),
            attempt_id: claimed.output["binding"]["attempt_id"]
                .as_str()
                .unwrap()
                .into(),
            expected_revision: revision(&claimed.output["goal"]),
            plan_version: 1,
            idempotency_key: "forged-evidence".into(),
            evidence: Some(evidence),
        })
        .await;
    assert!(!checked.success);
    assert_eq!(checked.output["error_kind"], "invalid_goal_evidence");
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    assert_eq!(
        fixture
            .db
            .get_host_goal(&owner, &id(&goal))
            .unwrap()
            .unwrap()
            .phase
            .as_str(),
        "executing"
    );
}

#[tokio::test]
async fn goal_service_store_injection_and_clones_keep_consistent_handles() {
    let temp = tempfile::tempdir().unwrap();
    let first = Arc::new(crate::Database::open(&temp.path().join("first.db")).unwrap());
    let second = Arc::new(crate::Database::open(&temp.path().join("second.db")).unwrap());
    let runtime = ToolRuntime::new_for_tests();
    assert!(runtime.goals.store().is_none());
    let auth = super::goals::goal_auth();
    let unavailable = runtime.goal_list(None, None, Some(&auth)).await;
    assert_eq!(unavailable.output["error_kind"], "goal_store_unavailable");
    assert_eq!(
        unavailable.error.as_deref(),
        Some("Goal mode requires the durable Server database")
    );
    let unavailable = runtime.host_goal_db().err().unwrap();
    assert_eq!(unavailable.output["error_kind"], "goal_store_unavailable");
    assert_eq!(
        unavailable.error.as_deref(),
        Some("Host goal mode requires the persistent local Server database")
    );

    let assert_handles = |runtime: &ToolRuntime, expected: &Arc<crate::Database>| {
        assert!(std::ptr::eq(
            runtime.goals.store().unwrap(),
            expected.as_ref()
        ));
        assert!(Arc::ptr_eq(
            runtime.communication.communication_db.as_ref().unwrap(),
            expected
        ));
    };
    let runtime = runtime.with_communication_database(first.clone());
    let original = runtime.clone();
    assert_handles(&runtime, &first);
    assert_handles(&original, &first);
    let replaced = runtime.with_communication_database(second.clone());
    assert_handles(&replaced, &second);
    assert_handles(&original, &first);
}

#[tokio::test]
async fn goal_service_store_keeps_admission_and_observation_independent_of_communication() {
    let mut fixture = Fixture::new().await;
    fixture.runtime.communication = Default::default();
    let window =
        crate::client_window::ClientWindow::from_opaque("openai-session", "goal-store-chat")
            .unwrap();
    let started = kernel_call(
        &fixture,
        &window,
        "goal_mode_start",
        json!({"project":fixture.project,"objective":"Produce a sourced analysis","idempotency_key":"store-start"}),
    )
    .await;
    success(&started);
    // Final request observation must use the Goal handle even without a
    // Communication service. Initializing the epoch alone records no tokens.
    assert!(started.output["context_meter"]["observed_tokens"]
        .as_u64()
        .is_some_and(|count| count > 0));
    let gateway = fixture
        .runtime
        .independent_gateway_goal_denial(&json!({}), Some(&fixture.auth), Some(&window))
        .expect("the selected Goal still fences the independent gateway");
    assert_eq!(gateway.output["error_kind"], "goal_gateway_effect_unknown");
    let denied = kernel_call(
        &fixture,
        &window,
        "write_project_file",
        json!({"project":fixture.project,"path":"must-not-exist.txt","content":"No confirmation yet"}),
    )
    .await;
    assert_eq!(denied.output["error_kind"], "goal_context_required");
    assert!(!fixture.temp.path().join("must-not-exist.txt").exists());
}

#[tokio::test]
async fn host_goals_kernel_fences_missing_and_stale_bindings_before_runner_mutation() {
    let fixture = Fixture::new().await;
    let window =
        crate::client_window::ClientWindow::from_opaque("openai-session", "goal-chat").unwrap();
    let started = kernel_call(&fixture, &window, "goal_mode_start", json!({
        "project":fixture.project,"objective":"Produce a sourced analysis","idempotency_key":"kernel-start"
    })).await;
    success(&started);
    assert_eq!(started.output["mode_binding"], "window_bound");
    assert!(started.output["context_meter"]["observed_tokens"]
        .as_u64()
        .is_some_and(|count| count > 0));
    let write = json!({"project":fixture.project,"path":"must-not-exist.txt","content":"No confirmation yet"});
    let missing = kernel_call(&fixture, &window, "write_project_file", write.clone()).await;
    assert!(!missing.success);
    assert_eq!(missing.output["error_kind"], "goal_context_required");
    let saved = fixture
        .save_plan(&started.output["goal"], plan(), "kernel-plan")
        .await;
    success(&saved);
    let mut forged = write.clone();
    forged["goal_context"] = json!({"goal_id":id(&started.output["goal"]),"node_id":"research","attempt_id":"fake-attempt","plan_version":1,"fence":1,"operation_id":"early-write"});
    let early = kernel_call(&fixture, &window, "write_project_file", forged).await;
    assert!(!early.success);
    assert_eq!(early.output["error_kind"], "goal_conflict");
    let confirmed = fixture
        .confirm(&saved.output["goal"], "kernel-confirm")
        .await;
    success(&confirmed);
    let first = fixture
        .claim(&confirmed.output["goal"], "kernel-claim")
        .await;
    success(&first);
    // Claiming changed the revision. An explicit takeover must read that
    // revision's complete node package, not rely on the prior claim receipt.
    let current_node = fixture
        .call(ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: id(&first.output["goal"]),
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("node".into()),
            node_id: Some("research".into()),
            section: None,
            cursor: None,
        })
        .await;
    success(&current_node);
    let replacement = fixture
        .call(ToolCall::GoalClaimNode {
            goal_id: id(&first.output["goal"]),
            node_id: "research".into(),
            expected_revision: revision(&first.output["goal"]),
            plan_version: 1,
            idempotency_key: "takeover".into(),
            context_epoch: Some("chat-original".into()),
            rework: false,
            takeover_attempt_id: Some(
                first.output["binding"]["attempt_id"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
        })
        .await;
    success(&replacement);
    let mut stale = write;
    stale["goal_context"] = first.output["binding"].clone();
    stale["goal_context"]["operation_id"] = json!("old-attempt-write");
    let refused = kernel_call(&fixture, &window, "write_project_file", stale).await;
    assert!(!refused.success);
    assert_eq!(refused.output["error_kind"], "goal_conflict");
    assert!(!fixture.temp.path().join("must-not-exist.txt").exists());
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    assert!(fixture
        .db
        .list_host_goal_operations(&owner, &id(&started.output["goal"]), 0, 100, false)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn host_goals_console_reads_real_chat_meter_without_resetting_or_counting_refreshes() {
    let fixture = Fixture::new().await;
    let chat =
        crate::client_window::ClientWindow::from_opaque("openai-session", "active-chat").unwrap();
    let console =
        crate::client_window::ClientWindow::from_opaque("http-cookie", "console-cookie").unwrap();
    let started=kernel_call(&fixture,&chat,"goal_mode_start",json!({"project":fixture.project,"objective":"Produce a sourced analysis","idempotency_key":"meter-start"})).await;
    success(&started);
    let first = &started.output["context_meter"];
    let args = json!({"goal_id":id(&started.output["goal"])});
    for _ in 0..2 {
        let panel = kernel_call(&fixture, &console, "goal_read", args.clone()).await;
        success(&panel);
        assert_eq!(
            panel.output["context_meter"]["context_epoch"],
            first["context_epoch"]
        );
        assert_eq!(
            panel.output["context_meter"]["observed_tokens"],
            first["observed_tokens"]
        );
        assert_eq!(
            panel.output["context_meter"]["sample_count"],
            first["sample_count"]
        );
        assert_eq!(panel.output["context_meter"]["newly_crossed"], false);
    }
    let chat_read = kernel_call(&fixture, &chat, "goal_read", args.clone()).await;
    success(&chat_read);
    assert!(
        chat_read.output["context_meter"]["observed_tokens"]
            .as_u64()
            .unwrap()
            > first["observed_tokens"].as_u64().unwrap()
    );
    let panel = kernel_call(&fixture, &console, "goal_read", args).await;
    success(&panel);
    assert_eq!(
        panel.output["context_meter"]["observed_tokens"],
        chat_read.output["context_meter"]["observed_tokens"]
    );
}

#[tokio::test]
async fn host_goals_new_chat_must_read_node_requirements_after_summary_before_claim() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let saved = fixture.save_plan(&goal, plan(), "plan").await;
    success(&saved);
    let confirmed = fixture.confirm(&saved.output["goal"], "confirm").await;
    success(&confirmed);
    let restored = fixture
        .call_in_chat(
            ToolCall::GoalBootstrap {
                goal_id: None,
                project: None,
                context_epoch: Some("new-chat".into()),
                activate: true,
            },
            "new-chat",
            None,
        )
        .await;
    success(&restored);
    assert_eq!(restored.output["goal"]["phase"], "ready");
    let claim = || ToolCall::GoalClaimNode {
        goal_id: id(&goal),
        node_id: "research".into(),
        expected_revision: revision(&confirmed.output["goal"]),
        plan_version: 1,
        idempotency_key: "fresh-claim".into(),
        context_epoch: Some("new-chat".into()),
        rework: false,
        takeover_attempt_id: None,
    };
    let before_read = fixture.call_in_chat(claim(), "new-chat", None).await;
    assert!(!before_read.success);
    assert_eq!(
        before_read.output["error_kind"],
        "goal_context_read_required"
    );
    assert_eq!(before_read.output["continuation"]["tool"], "goal_read");
    let summary = fixture
        .call_in_chat(
            ToolCall::GoalRead {
                source_context_epoch: None,
                goal_id: id(&goal),
                round_offset: None,
                round_limit: None,
                history_after: None,
                history_limit: None,
                history_kind: None,
                view: None,
                node_id: None,
                section: None,
                cursor: None,
            },
            "new-chat",
            None,
        )
        .await;
    success(&summary);
    assert_eq!(summary.output["projection"], "summary");
    assert_eq!(
        fixture.call_in_chat(claim(), "new-chat", None).await.output["error_kind"],
        "goal_context_read_required"
    );
    let full = fixture
        .call_in_chat(
            ToolCall::GoalRead {
                source_context_epoch: None,
                goal_id: id(&goal),
                round_offset: None,
                round_limit: None,
                history_after: None,
                history_limit: None,
                history_kind: None,
                view: Some("node".into()),
                node_id: Some("research".into()),
                section: None,
                cursor: None,
            },
            "new-chat",
            None,
        )
        .await;
    success(&full);
    assert_eq!(full.output["node"]["id"], "research");
    assert_eq!(full.output["context_read"]["complete"], true);
    assert!(serde_json::to_vec(&full.output).unwrap().len() <= 16384);
    let after_read = fixture.call_in_chat(claim(), "new-chat", None).await;
    success(&after_read);
    assert_eq!(after_read.output["goal"]["phase"], "executing");
}

#[tokio::test]
async fn host_goals_near_limit_repair_context_remains_lossless_and_claimable() {
    use webcodex_core::host_goal::{
        HostGoalExecutionBinding, HostGoalInvocationContext, HostGoalPlan,
    };
    let fixture = Fixture::new().await;
    let task_path = format!("tasks/{}", "bounded-repair-".repeat(4));
    let goal = fixture.start_in_task(Some(&task_path)).await;
    let node_id = format!("node-{}", "n".repeat(59));
    let new_epoch = "r".repeat(128);
    let mut large = plan();
    large["nodes"][0]["id"] = json!(node_id);
    large["constraints"] = json!(["C".repeat(4096)]);
    large["nodes"][0]["review"] = json!(1);
    large["nodes"][0]["delivery_kind"] = json!("lightweight");
    let mut typed: HostGoalPlan = serde_json::from_value(large).unwrap();
    typed.resolve_review_requirements();
    let requirements = json!({
        "objective":typed.objective,"constraints":typed.constraints,"deliverables":typed.deliverables,
        "final_criteria":typed.final_criteria,"state_management":typed.state_management,
        "verification_paths":typed.verification_paths,
    });
    let combined = serde_json::to_vec(&requirements).unwrap().len()
        + serde_json::to_vec(&typed.nodes[0]).unwrap().len();
    let padding = 12 * 1024 - 32 - combined;
    typed.nodes[0].work.push_str(&"W".repeat(padding));
    typed.validate().unwrap();
    let expected_node = serde_json::to_value(&typed.nodes[0]).unwrap();
    assert!(
        serde_json::to_vec(&requirements).unwrap().len()
            + serde_json::to_vec(&typed.nodes[0]).unwrap().len()
            >= 12 * 1024 - 32
    );
    let saved = fixture
        .save_plan(
            &goal,
            serde_json::to_value(&typed).unwrap(),
            "large-repair-plan",
        )
        .await;
    success(&saved);
    let confirmed = fixture
        .confirm(&saved.output["goal"], "large-repair-confirm")
        .await;
    success(&confirmed);
    let first_read = fixture
        .call(ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: id(&goal),
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("node".into()),
            node_id: Some(node_id.clone()),
            section: None,
            cursor: None,
        })
        .await;
    success(&first_read);
    let claimed = fixture
        .call(ToolCall::GoalClaimNode {
            goal_id: id(&goal),
            node_id,
            expected_revision: revision(&confirmed.output["goal"]),
            plan_version: 1,
            idempotency_key: "large-repair-first".into(),
            context_epoch: Some("chat-original".into()),
            rework: false,
            takeover_attempt_id: None,
        })
        .await;
    success(&claimed);
    let binding: HostGoalExecutionBinding =
        serde_json::from_value(claimed.output["binding"].clone()).unwrap();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    // Use the authoritative failure transition, with escape-heavy evidence that
    // cannot be bounded by raw string length alone. No database row is patched.
    let evidence = format!("Acceptance failed: {}", "\n\u{0001}".repeat(500));
    fixture
        .db
        .record_host_goal_repair_failure(
            &owner,
            &binding,
            &evidence,
            crate::tool_runtime::host_goal::now(),
            Some(&HostGoalInvocationContext {
                context_epoch: "chat-original".into(),
                identity_source: "explicit_context_only".into(),
            }),
        )
        .unwrap();
    let repair = fixture
        .db
        .get_host_goal_repair(&owner, &binding.goal_id)
        .unwrap()
        .unwrap();
    fixture.bootstrap(&claimed.output["goal"], &new_epoch).await;
    let node = fixture
        .call_in_chat(
            ToolCall::GoalRead {
                source_context_epoch: None,
                goal_id: binding.goal_id.clone(),
                round_offset: None,
                round_limit: None,
                history_after: None,
                history_limit: None,
                history_kind: None,
                view: Some("node".into()),
                node_id: Some(binding.node_id.clone()),
                section: None,
                cursor: None,
            },
            &new_epoch,
            None,
        )
        .await;
    success(&node);
    assert_eq!(node.output["requirements"], requirements);
    assert_eq!(node.output["node"], expected_node);
    assert_eq!(node.output["recovery"]["business_suspended"], true);
    assert_eq!(node.output["recovery"]["repair_id"], repair.repair_id);
    assert_eq!(node.output["recovery"]["deadline_at"], repair.deadline_at);
    assert_eq!(node.output["recovery"]["failure_truncated"], true);
    assert!(serde_json::to_vec(&node.output).unwrap().len() <= 16384);
    let resumed = fixture
        .call_in_chat(
            ToolCall::GoalClaimNode {
                goal_id: binding.goal_id.clone(),
                node_id: binding.node_id.clone(),
                expected_revision: revision(&node.output["goal"]),
                plan_version: binding.plan_version,
                idempotency_key: "large-repair-takeover".into(),
                context_epoch: Some(new_epoch.clone()),
                rework: false,
                takeover_attempt_id: Some(binding.attempt_id),
            },
            &new_epoch,
            None,
        )
        .await;
    success(&resumed);
    assert!(resumed.output["binding"]["fence"].as_u64().unwrap() > binding.fence);
    assert_eq!(
        resumed.output["recovery"]["deadline_at"],
        repair.deadline_at
    );
    assert_eq!(
        fixture.detail(&resumed.output["goal"], "repair").await["failure"],
        evidence
    );
}

#[tokio::test]
async fn host_goals_large_state_and_plan_use_bounded_lossless_projections() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let mut facts = serde_json::Map::new();
    for index in 0..20 {
        facts.insert(
            format!("fact-{index}"),
            json!("source evidence ".repeat(160)),
        );
    }
    let state = json!({"facts":facts,"completed_summary":"Saved findings","next_step":"Read the selected node","issues":["Keep exact citations"],"decisions":[],"artifact_refs":[],"plan_draft":null});
    let checkpoint = fixture
        .call(ToolCall::GoalCheckpoint {
            goal_id: id(&goal),
            expected_revision: revision(&goal),
            idempotency_key: "large-state".into(),
            patch: json!({"state":state,"reason":"Preserve long-running research"}),
            context_epoch: Some("chat-original".into()),
        })
        .await;
    success(&checkpoint);
    assert!(serde_json::to_vec(&checkpoint.output).unwrap().len() <= 8192);
    assert!(checkpoint.output["goal"].get("state").is_none());
    assert_eq!(checkpoint.output["progress_summary"]["partial"], true);
    let summary_as_state=fixture.call(ToolCall::GoalCheckpoint {
        goal_id:id(&goal),expected_revision:revision(&checkpoint.output["goal"]),idempotency_key:"reject-summary-as-state".into(),
        patch:json!({"reason":"A projection must not replace saved state","state":checkpoint.output["progress_summary"]}),context_epoch:Some("chat-original".into()),
    }).await;
    assert_eq!(
        summary_as_state.output["error_kind"],
        "invalid_goal_checkpoint"
    );
    let mut cursor = None;
    let mut text = String::new();
    let mut first_cursor = None;
    loop {
        let page = fixture
            .call(ToolCall::GoalRead {
                source_context_epoch: None,
                goal_id: id(&goal),
                round_offset: None,
                round_limit: None,
                history_after: None,
                history_limit: None,
                history_kind: None,
                view: Some("detail".into()),
                node_id: None,
                section: Some("state".into()),
                cursor: cursor.clone(),
            })
            .await;
        success(&page);
        assert!(serde_json::to_vec(&page.output).unwrap().len() <= 8192);
        text.push_str(page.output["text"].as_str().unwrap());
        cursor = page.output["next_cursor"].as_str().map(str::to_owned);
        if first_cursor.is_none() {
            first_cursor = cursor.clone();
        }
        if cursor.is_none() {
            break;
        }
    }
    assert!(first_cursor.is_some());
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), state);
    let wrong_section = fixture
        .call(ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: id(&goal),
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("detail".into()),
            node_id: None,
            section: Some("plan".into()),
            cursor: first_cursor.clone(),
        })
        .await;
    assert_eq!(wrong_section.output["error_kind"], "goal_cursor_stale");
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    let other = fixture
        .db
        .create_host_goal_at(
            &owner,
            &fixture.project,
            &fixture.temp.path().to_string_lossy(),
            "Another target",
            "another-target",
            10,
        )
        .unwrap()
        .goal;
    let wrong_goal = fixture
        .call(ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: other.goal_id,
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("detail".into()),
            node_id: None,
            section: Some("state".into()),
            cursor: first_cursor.clone(),
        })
        .await;
    assert_eq!(wrong_goal.output["error_kind"], "goal_cursor_stale");
    let mut large_plan = plan();
    let prototype = large_plan["nodes"][0].clone();
    large_plan["nodes"] = json!((0..24)
        .map(|index| {
            let mut node = prototype.clone();
            node["id"] = json!(format!("step-{index}"));
            node["work"] = json!("Read the referenced evidence. ".repeat(120));
            node
        })
        .collect::<Vec<_>>());
    let saved = fixture
        .save_plan(&checkpoint.output["goal"], large_plan, "large-plan")
        .await;
    success(&saved);
    assert!(serde_json::to_vec(&saved.output).unwrap().len() <= 8192);
    assert!(saved.output.get("plan").is_none());
    let resumed = fixture
        .bootstrap(&saved.output["goal"], "large-plan-chat")
        .await;
    assert_eq!(resumed.output["goal"], saved.output["goal"]);
    assert_eq!(resumed.output["confirmation_required"], true);
    let node = fixture
        .call(ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: id(&goal),
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("node".into()),
            node_id: Some("step-0".into()),
            section: None,
            cursor: None,
        })
        .await;
    success(&node);
    assert!(serde_json::to_vec(&node.output).unwrap().len() <= 16384);
    assert_eq!(
        node.output["node"]["work"],
        "Read the referenced evidence. ".repeat(120)
    );
    let stale = fixture
        .call(ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: id(&goal),
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("detail".into()),
            node_id: None,
            section: Some("state".into()),
            cursor: first_cursor,
        })
        .await;
    assert_eq!(stale.output["error_kind"], "goal_cursor_stale");
}

#[tokio::test]
async fn host_goals_planning_recovery_keeps_full_objective_outside_bounded_summary() {
    let fixture = Fixture::new().await;
    let objective = "Preserve every requirement: ".to_owned() + &"完整需求".repeat(200);
    let started = fixture
        .call(ToolCall::GoalModeStart {
            task_directory: None,
            project: fixture.project.clone(),
            objective: objective.clone(),
            idempotency_key: "long-objective".into(),
            context_epoch: Some("chat-original".into()),
            context_warning_tokens: None,
            context_handoff_tokens: None,
        })
        .await;
    success(&started);
    assert_ne!(started.output["goal"]["objective"], objective);
    assert_eq!(
        started.output["continuation"]["arguments"]["section"],
        "goal"
    );
    assert!(serde_json::to_vec(&started.output).unwrap().len() <= 8192);
    let complete = fixture.detail(&started.output["goal"], "goal").await;
    assert_eq!(complete["objective"], objective);
    assert!(complete.get("owner_key").is_none());
}

#[tokio::test]
async fn host_goals_partial_fact_deletion_preserves_other_state_and_checkpoint_history() {
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let seeded=fixture.call(ToolCall::GoalCheckpoint{
        goal_id:id(&goal),expected_revision:revision(&goal),idempotency_key:"seed-facts".into(),context_epoch:Some("chat-original".into()),
        patch:json!({"reason":"Save sourced facts","state":{"facts":{"obsolete":"Original supported detail","current":"Still relevant"},"decisions":["Keep the chosen method"],"issues":["Outstanding question"]}}),
    }).await;
    success(&seeded);
    let compacted=fixture.call(ToolCall::GoalCheckpoint{
        goal_id:id(&goal),expected_revision:revision(&seeded.output["goal"]),idempotency_key:"compact-facts".into(),context_epoch:Some("chat-original".into()),
        patch:json!({"reason":"Remove one superseded fact","state":{"facts":{"obsolete":null,"fresh":"New supported detail"}}}),
    }).await;
    success(&compacted);
    let state = fixture.detail(&compacted.output["goal"], "state").await;
    assert!(state["facts"].get("obsolete").is_none());
    assert_eq!(state["facts"]["current"], "Still relevant");
    assert_eq!(state["facts"]["fresh"], "New supported detail");
    assert_eq!(state["decisions"], json!(["Keep the chosen method"]));
    assert_eq!(state["issues"], json!(["Outstanding question"]));
    let history = fixture
        .detail_after(&compacted.output["goal"], "checkpoints", Some(0))
        .await;
    let original = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["revision"] == seeded.output["goal"]["revision"])
        .expect("original checkpoint remains readable");
    assert_eq!(
        original["state"]["facts"]["obsolete"],
        "Original supported detail"
    );
    assert!(original["state"]["facts"].get("fresh").is_none());
}

fn native_host_window(fixture: &super::goals::Fixture) -> crate::client_window::ClientWindow {
    let hash = crate::tool_runtime::host_goal_files::sha256(
        fixture.temp.path().to_string_lossy().as_bytes(),
    );
    crate::client_window::ClientWindow::from_opaque(
        "openai-session",
        &format!("native-chat-{}", &hash[..16]),
    )
    .unwrap()
}

fn native_host_epoch(fixture: &super::goals::Fixture) -> String {
    format!("chat_{}", native_host_window(fixture).key())
}

async fn native_host_call(
    fixture: &super::goals::Fixture,
    call: ToolCall,
    binding: Option<&Value>,
) -> ToolResult {
    let epoch = native_host_epoch(fixture);
    let window = native_host_window(fixture);
    let auth = super::goals::goal_auth();
    let tool_name = call.tool_name().to_owned();
    let mut arguments = serde_json::to_value(&call).unwrap()["params"].clone();
    arguments["context_epoch"] = json!(epoch);
    if let Some(binding) = binding {
        let mut metadata = binding.clone();
        metadata["operation_id"] = json!("checkpoint-operation");
        metadata["context_epoch"] = json!(epoch);
        arguments["goal_context"] = metadata;
    }
    let outcome = fixture
        .runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name,
                arguments,
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(&auth),
                window: Some(&window),
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await;
    assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
    outcome.result.expect("native host kernel result")
}

async fn native_host_setup(
    behavior: super::goals::Behavior,
) -> (super::goals::Fixture, Value, Value, Value) {
    let fixture = super::goals::Fixture::new(behavior).await;
    let started = native_host_call(
        &fixture,
        ToolCall::GoalModeStart {
            task_directory: None,
            project: fixture.project.clone(),
            objective: "Produce a sourced analysis".into(),
            idempotency_key: "host-native".into(),
            context_epoch: Some("native-chat".into()),
            context_warning_tokens: None,
            context_handoff_tokens: None,
        },
        None,
    )
    .await;
    success(&started);
    let saved = native_host_call(
        &fixture,
        ToolCall::GoalPlanSave {
            goal_id: id(&started.output["goal"]),
            expected_revision: revision(&started.output["goal"]),
            idempotency_key: "host-native-plan".into(),
            plan: plan(),
            change_request: None,
        },
        None,
    )
    .await;
    success(&saved);
    let published = &saved.output["goal"];
    let confirmed = native_host_call(
        &fixture,
        ToolCall::GoalConfirmPlan {
            goal_id: id(published),
            expected_revision: revision(published),
            plan_version: 1,
            plan_sha256: published["plan_hash"].as_str().unwrap().into(),
            confirmation: "I reviewed this exact saved plan and now authorize execution.".into(),
            idempotency_key: "host-native-confirm".into(),
        },
        None,
    )
    .await;
    success(&confirmed);
    let read = native_host_call(
        &fixture,
        ToolCall::GoalRead {
            source_context_epoch: None,
            goal_id: id(published),
            round_offset: None,
            round_limit: None,
            history_after: None,
            history_limit: None,
            history_kind: None,
            view: Some("node".into()),
            node_id: Some("research".into()),
            section: None,
            cursor: None,
        },
        None,
    )
    .await;
    success(&read);
    let claimed = native_host_call(
        &fixture,
        ToolCall::GoalClaimNode {
            goal_id: id(published),
            node_id: "research".into(),
            expected_revision: revision(&confirmed.output["goal"]),
            plan_version: 1,
            idempotency_key: "host-native-claim".into(),
            context_epoch: Some("native-chat".into()),
            rework: false,
            takeover_attempt_id: None,
        },
        None,
    )
    .await;
    success(&claimed);
    (
        fixture,
        claimed.output["goal"].clone(),
        claimed.output["binding"].clone(),
        saved.output["plan_artifacts"].clone(),
    )
}

fn native_start(project: &str, key: &str) -> ToolCall {
    ToolCall::CodingAgentStart {
        project: project.into(),
        provider_id: "executor".into(),
        idempotency_key: key.into(),
        instruction: "Inspect the requested result and produce the sourced analysis.".into(),
        config: None,
        timeout_secs: Some(10),
        recording_session_id: None,
    }
}

#[tokio::test]
async fn host_goals_unconfirmed_plans_allow_inspection_but_fence_implementation() {
    for phase in ["planning", "awaiting_confirmation"] {
        let fixture = super::goals::Fixture::new(super::goals::Behavior::Accept).await;
        let mut auth = super::goals::goal_auth();
        auth.scopes.push("plugin:invoke".into());
        let epoch = native_host_epoch(&fixture);
        let call = |call| dispatch_in_chat(&fixture.runtime, &auth, call, &epoch, None);
        std::fs::write(
            fixture.temp.path().join("source.txt"),
            "Actual expense: 42\n",
        )
        .unwrap();
        let started = call(ToolCall::GoalModeStart {
            task_directory: None,
            project: fixture.project.clone(),
            objective: "Produce a sourced analysis".into(),
            idempotency_key: "inspection-start".into(),
            context_epoch: Some("native-chat".into()),
            context_warning_tokens: None,
            context_handoff_tokens: None,
        })
        .await;
        success(&started);
        let mut goal = started.output["goal"].clone();
        if phase == "awaiting_confirmation" {
            let saved = call(ToolCall::GoalPlanSave {
                goal_id: id(&goal),
                expected_revision: revision(&goal),
                idempotency_key: "inspection-plan".into(),
                plan: plan(),
                change_request: None,
            })
            .await;
            success(&saved);
            goal = saved.output["goal"].clone();
        }
        assert_eq!(goal["phase"], phase);

        let read = call(
            ToolCall::from_tool_name(
                "read_files",
                json!({"project":fixture.project,"items":[{"path":"source.txt"}],"with_line_numbers":false}),
            )
            .unwrap(),
        )
        .await;
        success(&read);
        assert_eq!(read.output["items"][0]["success"], true);
        assert_eq!(
            read.output["items"][0]["output"]["text"],
            "Actual expense: 42"
        );
        assert_eq!(fixture.text_file_read_count(), 1);

        // This fixture has no document provider. Its downstream error proves
        // that goal admission permits initial reads, pagination and backup lists.
        for (tool, mut arguments) in [
            (
                "present_document",
                json!({"path":"report.xlsx","view":"text"}),
            ),
            (
                "present_document",
                json!({"path":"report.pdf","view":"pages","cursor":"next-page"}),
            ),
            (
                "document_restore",
                json!({"path":"report.xlsx","offset":10}),
            ),
        ] {
            arguments["project"] = json!(fixture.project);
            let result = call(ToolCall::from_tool_name(tool, arguments).unwrap()).await;
            assert!(!result.success);
            assert_eq!(
                result.output["error_kind"], "document_runner_unavailable",
                "{phase} {tool}: {:?}",
                result.error
            );
        }

        for tool in [
            "present_document",
            "document_edit",
            "document_convert",
            "document_restore",
        ] {
            let resume = call(
                ToolCall::from_tool_name(
                    tool,
                    json!({"project":fixture.project,"job_id":"missing-document-job"}),
                )
                .unwrap(),
            )
            .await;
            assert!(!resume.success);
            assert_eq!(
                resume.output["error_kind"], "document_job_unavailable",
                "{phase} {tool}: {:?}",
                resume.error
            );
            let mixed = call(
                ToolCall::from_tool_name(tool, json!({"project":fixture.project,"job_id":"missing-document-job","path":"report.xlsx"})).unwrap(),
            )
            .await;
            assert!(!mixed.success);
            assert_eq!(
                mixed.output["error_kind"], "invalid_document_arguments",
                "{phase} {tool}: {:?}",
                mixed.error
            );
        }

        for (tool, mut arguments) in [
            (
                "document_edit",
                json!({"path":"report.xlsx","expected_sha256":"a".repeat(64),"operations":[]}),
            ),
            (
                "document_convert",
                json!({"path":"report.xlsx","output_path":"report.pdf"}),
            ),
            (
                "document_restore",
                json!({"path":"report.xlsx","backup_id":"backup-one","expected_sha256":"a".repeat(64)}),
            ),
            (
                "run_process",
                json!({"executable":"reader","args":["report.xlsx"]}),
            ),
        ] {
            arguments["project"] = json!(fixture.project);
            let denied = call(ToolCall::from_tool_name(tool, arguments).unwrap()).await;
            assert!(!denied.success);
            assert_eq!(
                denied.output["error_kind"], "goal_context_required",
                "{phase} {tool}: {:?}",
                denied.error
            );
        }
        let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
        let persisted = fixture
            .db
            .get_host_goal(&owner, &id(&goal))
            .unwrap()
            .unwrap();
        assert_eq!(persisted.phase.as_str(), phase);
        assert!(fixture
            .db
            .list_host_goal_operations(&owner, &id(&goal), 0, 100, false)
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn host_goals_explicit_epoch_restricts_unconfirmed_mutation_without_binding_but_not_identityless_calls(
) {
    let fixture = super::goals::Fixture::new(super::goals::Behavior::Accept).await;
    let auth = super::goals::goal_auth();
    let started = native_host_call(
        &fixture,
        ToolCall::GoalModeStart {
            task_directory: None,
            project: fixture.project.clone(),
            objective: "Plan before dispatch".into(),
            idempotency_key: "epoch-mode".into(),
            context_epoch: Some("native-chat".into()),
            context_warning_tokens: None,
            context_handoff_tokens: None,
        },
        None,
    )
    .await;
    success(&started);
    assert_eq!(started.output["goal"]["phase"], "planning");
    let denied = native_host_call(
        &fixture,
        native_start(&fixture.project, "same-epoch-denied"),
        None,
    )
    .await;
    assert_eq!(denied.output["error_kind"], "goal_context_required");
    assert!(
        !fixture.temp.path().join("result.txt").exists(),
        "Unconfirmed epoch-bound call must not reach the Runner"
    );
    let call = native_start(&fixture.project, "ordinary-no-identity");
    let outcome = fixture
        .runtime
        .call_tool_with_context(
            ToolCallRequest {
                tool_name: call.tool_name().into(),
                arguments: serde_json::to_value(call).unwrap()["params"].clone(),
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(&auth),
                window: None,
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        )
        .await;
    assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
    let ordinary = outcome.result.unwrap();
    success(&ordinary);
    assert!(fixture.temp.path().join("result.txt").exists());
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    assert!(
        fixture
            .db
            .list_host_goal_operations(&owner, &id(&started.output["goal"]), 0, 100, false)
            .unwrap()
            .is_empty(),
        "A caller without chat identity is not implicitly adopted by the owner's selected goal"
    );
    let cleanup = fixture
        .runtime
        .dispatch_with_auth(
            ToolCall::CodingAgentCancel {
                run_id: ordinary.output["run_id"].as_str().unwrap().into(),
            },
            Some(&auth),
        )
        .await;
    success(&cleanup);
}

#[tokio::test]
async fn host_goals_native_run_reconciles_without_completing_goal_or_manufacturing_acceptance() {
    use webcodex_core::host_goal::{HostGoalOperationStatus, HostGoalPhase};
    let (fixture, goal, binding, _) = native_host_setup(super::goals::Behavior::Accept).await;
    let auth = super::goals::goal_auth();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    let nested_acp = native_host_call(
        &fixture,
        ToolCall::GoalStart {
            project: fixture.project.clone(),
            provider_id: "executor".into(),
            objective: "Do not nest a second goal controller".into(),
            criteria: vec![webcodex_core::goal::GoalCriterion::FileExists {
                id: "file".into(),
                description: "An output exists".into(),
                path: "result.txt".into(),
            }],
            artifact_paths: vec!["result.txt".into()],
            reviewer_provider_id: None,
            limits: None,
            config: None,
            idempotency_key: "nested-acp-denied".into(),
        },
        Some(&binding),
    )
    .await;
    assert_eq!(nested_acp.output["error_kind"], "goal_mode_conflict");
    let started = native_host_call(
        &fixture,
        native_start(&fixture.project, "native-start"),
        Some(&binding),
    )
    .await;
    success(&started);
    let run_id = started.output["run_id"].as_str().unwrap();
    let operation = fixture
        .db
        .get_host_goal_operation(&owner, &id(&goal), "checkpoint-operation")
        .unwrap()
        .unwrap();
    assert_eq!(operation.status, HostGoalOperationStatus::Running);
    let reference: Value =
        serde_json::from_str(operation.result_summary.as_ref().unwrap()).unwrap();
    assert_eq!(reference["source"], "runtime_coding_agent");
    assert_eq!(reference["run_id"], run_id);
    let manual=native_host_call(&fixture,ToolCall::GoalCheckpoint{goal_id:id(&goal),expected_revision:revision(&goal),idempotency_key:"native-manual-denied".into(),context_epoch:Some("native-chat".into()),patch:json!({"reason":"Native status must remain authoritative","operation_resolutions":[{"operation_id":"checkpoint-operation","status":"succeeded","evidence":"A human report is not native terminal status","source_refs":[run_id],"user_confirmation":"I believe it is finished"}]})},Some(&binding)).await;
    assert_eq!(
        manual.output["error_kind"],
        "goal_run_reconciliation_required"
    );
    let persisted = fixture
        .db
        .get_host_goal(&owner, &id(&goal))
        .unwrap()
        .unwrap();
    fixture
        .runtime
        .reconcile_host_goal_operations(&persisted, Some(&auth))
        .await
        .unwrap();
    let operation = fixture
        .db
        .get_host_goal_operation(&owner, &id(&goal), "checkpoint-operation")
        .unwrap()
        .unwrap();
    assert_eq!(operation.status, HostGoalOperationStatus::Succeeded);
    assert_eq!(
        fixture
            .db
            .get_host_goal(&owner, &id(&goal))
            .unwrap()
            .unwrap()
            .phase,
        HostGoalPhase::Executing
    );
    assert!(fixture
        .db
        .latest_host_goal_attempt(&owner, &id(&goal))
        .unwrap()
        .unwrap()
        .checks
        .is_empty());
    let checked = native_host_call(
        &fixture,
        ToolCall::GoalCheckNode {
            goal_id: id(&goal),
            node_id: "research".into(),
            attempt_id: binding["attempt_id"].as_str().unwrap().into(),
            expected_revision: revision(&goal),
            plan_version: 1,
            idempotency_key: "native-node-check".into(),
            evidence: Some(review_evidence(true)),
        },
        None,
    )
    .await;
    assert!(
        !checked.success,
        "Successful native execution cannot manufacture model acceptance"
    );
    assert_eq!(
        checked.output["error_kind"],
        "goal_review_material_required"
    );
    assert_eq!(
        fixture
            .db
            .get_host_goal(&owner, &id(&goal))
            .unwrap()
            .unwrap()
            .phase,
        HostGoalPhase::Executing
    );
    let recorded = native_host_call(
        &fixture,
        ToolCall::GoalCheckpoint {
            goal_id: id(&goal),
            expected_revision: revision(&goal),
            idempotency_key: "native-record-conclusion".into(),
            context_epoch: Some("native-chat".into()),
            patch: json!({"reason":"Record actual native-run result for a subsequent model review",
                "state":{"completed_summary":"The native run finished; its output supports the recorded conclusion",
                    "facts":{"source_run_id":run_id,"source_operation":"checkpoint-operation"},
                    "next_step":"Prepare state review and submit a fresh source-backed verdict"}}),
        },
        Some(&binding),
    ).await;
    success(&recorded);
    let prepared = native_host_call(
        &fixture,
        ToolCall::GoalCheckNode {
            goal_id: id(&goal),
            node_id: "research".into(),
            attempt_id: binding["attempt_id"].as_str().unwrap().into(),
            expected_revision: revision(&recorded.output["goal"]),
            plan_version: 1,
            idempotency_key: "native-prepare-state-review".into(),
            evidence: None,
        },
        None,
    )
    .await;
    success(&prepared);
    assert_eq!(prepared.output["goal"]["phase"], "executing");
    assert!(prepared.output["review_packet"]["materials"]
        .to_string()
        .contains(run_id));
    let reviewed = native_host_call(
        &fixture,
        ToolCall::GoalCheckNode {
            goal_id: id(&goal),
            node_id: "research".into(),
            attempt_id: binding["attempt_id"].as_str().unwrap().into(),
            expected_revision: revision(&recorded.output["goal"]),
            plan_version: 1,
            idempotency_key: "native-current-review".into(),
            evidence: Some(prepared_review_evidence(&prepared.output["review_packet"])),
        },
        None,
    )
    .await;
    success(&reviewed);
    assert_eq!(reviewed.output["goal"]["phase"], "completed");
}

#[tokio::test]
async fn host_goals_native_cancel_recovers_only_recorded_run_while_paused_and_plan_damaged() {
    use webcodex_core::host_goal::{HostGoalOperationStatus, HostGoalPhase};
    let (fixture, goal, binding, artifacts) =
        native_host_setup(super::goals::Behavior::RunningUntilCancelled).await;
    let auth = super::goals::goal_auth();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&auth)).unwrap();
    let started = native_host_call(
        &fixture,
        native_start(&fixture.project, "recorded-run"),
        Some(&binding),
    )
    .await;
    success(&started);
    let foreign = fixture
        .runtime
        .dispatch_with_auth(
            native_start(&fixture.project, "independent-run"),
            Some(&auth),
        )
        .await;
    success(&foreign);
    let denied = native_host_call(
        &fixture,
        ToolCall::CodingAgentCancel {
            run_id: foreign.output["run_id"].as_str().unwrap().into(),
        },
        None,
    )
    .await;
    assert_eq!(denied.output["error_kind"], "goal_run_mismatch");
    let paused = native_host_call(
        &fixture,
        ToolCall::GoalCheckpoint {
            goal_id: id(&goal),
            expected_revision: revision(&goal),
            idempotency_key: "pause-native".into(),
            context_epoch: Some("native-chat".into()),
            patch: json!({"reason":"Pause before cancelling the original Run","action":"pause"}),
        },
        Some(&binding),
    )
    .await;
    success(&paused);
    std::fs::write(
        artifacts["markdown_path"].as_str().unwrap(),
        "External edit requiring plan repair",
    )
    .unwrap();
    let cancelled = native_host_call(
        &fixture,
        ToolCall::CodingAgentCancel {
            run_id: started.output["run_id"].as_str().unwrap().into(),
        },
        None,
    )
    .await;
    success(&cancelled);
    assert_eq!(cancelled.output["state"], "cancelled");
    let persisted = fixture
        .db
        .get_host_goal(&owner, &id(&goal))
        .unwrap()
        .unwrap();
    fixture
        .runtime
        .reconcile_host_goal_operations(&persisted, Some(&auth))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .db
            .get_host_goal_operation(&owner, &id(&goal), "checkpoint-operation")
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Cancelled
    );
    assert_eq!(
        fixture
            .db
            .get_host_goal(&owner, &id(&goal))
            .unwrap()
            .unwrap()
            .phase,
        HostGoalPhase::Paused
    );
    // The independent Run remains a separate authority/lifecycle, cleaned up by
    // its ordinary native cancellation path rather than the Host exception.
    let cleanup = fixture
        .runtime
        .dispatch_with_auth(
            ToolCall::CodingAgentCancel {
                run_id: foreign.output["run_id"].as_str().unwrap().into(),
            },
            Some(&auth),
        )
        .await;
    success(&cleanup);
}

#[tokio::test]
async fn host_goals_unknown_operation_requires_user_reconciliation_and_never_overrides_a_job() {
    use webcodex_core::host_goal::{HostGoalExecutionBinding, HostGoalOperationStatus};
    let fixture = Fixture::new().await;
    let goal = fixture.start().await;
    let saved = fixture.save_plan(&goal, plan(), "resolution-plan").await;
    success(&saved);
    let confirmed = fixture
        .confirm(&saved.output["goal"], "resolution-confirm")
        .await;
    success(&confirmed);
    let claimed = fixture
        .claim(&confirmed.output["goal"], "resolution-claim")
        .await;
    success(&claimed);
    let binding: HostGoalExecutionBinding =
        serde_json::from_value(claimed.output["binding"].clone()).unwrap();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&fixture.auth)).unwrap();
    let invocation = webcodex_core::host_goal::HostGoalInvocationContext {
        context_epoch: "chat-original".into(),
        identity_source: "explicit_context_only".into(),
    };
    fixture
        .db
        .begin_host_goal_operation_with_manifest_in_context(
            &owner,
            &binding,
            "response-lost",
            "write_project_file",
            &"a".repeat(64),
            &[],
            1,
            Some(&invocation),
            false,
        )
        .unwrap();
    fixture
        .db
        .finish_host_goal_operation(
            &owner,
            &id(&goal),
            "response-lost",
            HostGoalOperationStatus::Unknown,
            None,
            Some("Response lost after dispatch"),
            2,
        )
        .unwrap();
    let operation_before = fixture
        .db
        .get_host_goal_operation(&owner, &id(&goal), "response-lost")
        .unwrap();
    let resumed = fixture
        .bootstrap(&claimed.output["goal"], "unknown-result-chat")
        .await;
    assert_eq!(resumed.output["goal"], claimed.output["goal"]);
    assert_eq!(resumed.output["confirmation_required"], false);
    assert_eq!(resumed.output["unresolved_operation_count"], 1);
    assert_eq!(
        serde_json::to_value(
            fixture
                .db
                .get_host_goal_operation(&owner, &id(&goal), "response-lost")
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(operation_before).unwrap(),
        "guidance must not reconcile or replay the original operation"
    );
    let resolution = |operation: &str, confirmation: &str| {
        json!({"reason":"Reconciled original operation","operation_resolutions":[{
            "operation_id":operation,"status":"succeeded","evidence":"User inspected the produced file and verified its expected contents and fingerprint",
            "source_refs":["result.txt sha256: verified by user"],"user_confirmation":confirmation
        }]})
    };
    let no_confirmation = fixture
        .call_in_chat(
            ToolCall::GoalCheckpoint {
                goal_id: id(&goal),
                expected_revision: revision(&claimed.output["goal"]),
                idempotency_key: "resolution-missing-confirmation".into(),
                patch: resolution("response-lost", ""),
                context_epoch: Some("chat-original".into()),
            },
            "chat-original",
            Some(&claimed.output["binding"]),
        )
        .await;
    assert!(!no_confirmation.success);
    assert_eq!(
        no_confirmation.output["error_kind"],
        "invalid_goal_resolution"
    );
    assert_eq!(
        fixture
            .db
            .get_host_goal_operation(&owner, &id(&goal), "response-lost")
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Unknown
    );
    for field in ["evidence", "source_refs"] {
        let mut missing = resolution("response-lost", "I checked the original operation");
        missing["operation_resolutions"][0][field] = if field == "evidence" {
            json!("")
        } else {
            json!([])
        };
        let refused = fixture
            .call_in_chat(
                ToolCall::GoalCheckpoint {
                    goal_id: id(&goal),
                    expected_revision: revision(&claimed.output["goal"]),
                    idempotency_key: format!("missing-{field}"),
                    patch: missing,
                    context_epoch: Some("chat-original".into()),
                },
                "chat-original",
                Some(&claimed.output["binding"]),
            )
            .await;
        assert!(!refused.success);
        assert_eq!(refused.output["error_kind"], "invalid_goal_resolution");
    }
    let reconciled = fixture
        .call_in_chat(
            ToolCall::GoalCheckpoint {
                goal_id: id(&goal),
                expected_revision: revision(&claimed.output["goal"]),
                idempotency_key: "resolution-confirmed".into(),
                patch: resolution(
                    "response-lost",
                    "我已核对原操作产物，确认该操作成功，请记录核对结果并继续。",
                ),
                context_epoch: Some("chat-original".into()),
            },
            "chat-original",
            Some(&claimed.output["binding"]),
        )
        .await;
    success(&reconciled);
    let resolved = fixture
        .db
        .get_host_goal_operation(&owner, &id(&goal), "response-lost")
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, HostGoalOperationStatus::Succeeded);
    assert!(resolved
        .result_summary
        .unwrap()
        .contains("user_reconciliation_report"));
    assert_eq!(reconciled.output["goal"]["phase"], "executing");
    fixture
        .db
        .begin_host_goal_operation_with_manifest_in_context(
            &owner,
            &binding,
            "tracked-job",
            "run_process",
            &"b".repeat(64),
            &[],
            3,
            Some(&invocation),
            false,
        )
        .unwrap();
    fixture
        .db
        .finish_host_goal_operation(
            &owner,
            &id(&goal),
            "tracked-job",
            HostGoalOperationStatus::Running,
            Some("job-still-running"),
            Some("Job still running"),
            4,
        )
        .unwrap();
    let refused = fixture
        .call_in_chat(
            ToolCall::GoalCheckpoint {
                goal_id: id(&goal),
                expected_revision: revision(&reconciled.output["goal"]),
                idempotency_key: "do-not-override-job".into(),
                patch: resolution("tracked-job", "I approve this resolution"),
                context_epoch: Some("chat-original".into()),
            },
            "chat-original",
            Some(&claimed.output["binding"]),
        )
        .await;
    assert!(!refused.success);
    assert_eq!(
        refused.output["error_kind"],
        "goal_job_reconciliation_required"
    );
    let still_running = fixture
        .db
        .get_host_goal_operation(&owner, &id(&goal), "tracked-job")
        .unwrap()
        .unwrap();
    assert_eq!(still_running.status, HostGoalOperationStatus::Running);
    assert_eq!(still_running.job_id.as_deref(), Some("job-still-running"));
}
#[path = "host_goal_takeover.rs"]
mod takeover;
