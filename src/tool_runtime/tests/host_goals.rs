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

struct Fixture {
    temp: tempfile::TempDir,
    db: Arc<crate::Database>,
    runtime: ToolRuntime,
    project: String,
    auth: AuthContext,
}

impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::Database::open(&temp.path().join("host-goals.db")).unwrap());
        let runtime = ToolRuntime::new_for_tests().with_communication_database(db.clone());
        let mut auth = auth_context(Some("host-goal-owner"), false);
        auth.scopes = ["runtime:read", "project:read", "project:write"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let project = register_owned_project(&runtime, temp.path(), &auth).await;
        Self {
            temp,
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
        dispatch_in_chat(&self.runtime, &self.auth, call, epoch, binding).await
    }

    async fn start(&self) -> Value {
        let result = self
            .call(ToolCall::GoalModeStart {
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
        result.output["goal"].clone()
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
            .resolve_project_input_for_auth(&project, Some(auth))
            .await
            .is_ok(),
        "fixture Runner must be visible to its actual non-admin owner"
    );
    project
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
    let outcome = fixture
        .runtime
        .call_tool_with_context(
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
    let checked = fixture
        .call(ToolCall::GoalCheckNode {
            goal_id: id(awaiting),
            node_id: "research".into(),
            attempt_id: binding["attempt_id"].as_str().unwrap().into(),
            expected_revision: revision(&checkpoint.output["goal"]),
            plan_version: awaiting["plan_version"].as_u64().unwrap(),
            idempotency_key: "check-one".into(),
            evidence: Some(review_evidence(true)),
        })
        .await;
    success(&checked);
    assert_eq!(checked.output["goal"]["phase"], "completed");
    assert_eq!(
        checked.output["goal"]["completed_node_ids"],
        json!(["research"])
    );
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
        Arc::new(crate::Database::open(&fixture.temp.path().join("host-goals.db")).unwrap());
    let runtime = ToolRuntime::new_for_tests().with_communication_database(reopened);
    register_owned_project(&runtime, fixture.temp.path(), &fixture.auth).await;
    let recovered = dispatch_in_chat(
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
    )
    .await;
    success(&recovered);
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
    assert_eq!(refused.output["error_kind"], "goal_plan_artifact_changed");
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
    let node = fixture
        .call(ToolCall::GoalRead {
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

async fn native_host_call(
    fixture: &super::goals::Fixture,
    call: ToolCall,
    binding: Option<&Value>,
) -> ToolResult {
    dispatch_in_chat(
        &fixture.runtime,
        &super::goals::goal_auth(),
        call,
        "native-chat",
        binding,
    )
    .await
}

async fn native_host_setup(
    behavior: super::goals::Behavior,
) -> (super::goals::Fixture, Value, Value, Value) {
    let fixture = super::goals::Fixture::new(behavior).await;
    let started = native_host_call(
        &fixture,
        ToolCall::GoalModeStart {
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
        let call = |call| dispatch_in_chat(&fixture.runtime, &auth, call, "native-chat", None);
        std::fs::write(
            fixture.temp.path().join("source.txt"),
            "Actual expense: 42\n",
        )
        .unwrap();
        let started = call(ToolCall::GoalModeStart {
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
    success(&checked);
    assert_eq!(checked.output["goal"]["phase"], "completed");
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
    fixture
        .db
        .begin_host_goal_operation(
            &owner,
            &binding,
            "response-lost",
            "write_project_file",
            &"a".repeat(64),
            1,
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
        .begin_host_goal_operation(
            &owner,
            &binding,
            "tracked-job",
            "run_process",
            &"b".repeat(64),
            3,
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
