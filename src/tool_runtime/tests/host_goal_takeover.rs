//! Synthetic host identities exercise the real kernel, SQLite and Runner file
//! protocol. This suite does not claim a real browser conversation was launched.
use super::*;
use crate::client_window::ClientWindow;
use crate::runner_protocol::{RunnerPollRequest, RunnerRequest};
use std::time::Duration;

pub(super) struct TakeoverFixture {
    pub(super) inner: Fixture,
    pub(super) source: ClientWindow,
    pub(super) candidate: ClientWindow,
    pub(super) goal: Value,
    pub(super) binding: Value,
    plan_path: std::path::PathBuf,
}

impl TakeoverFixture {
    pub(super) async fn new() -> Self {
        let inner = Fixture::new().await;
        let source = ClientWindow::from_opaque("openai-session", "takeover-source").unwrap();
        let candidate = ClientWindow::from_opaque("openai-session", "takeover-candidate").unwrap();
        let start = kernel_call(
            &inner,
            &source,
            "goal_mode_start",
            json!({"project":inner.project,"objective":"Produce a sourced analysis",
                "idempotency_key":"takeover-start","task_directory":"task-output"}),
        )
        .await;
        success(&start);
        let goal = &start.output["goal"];
        let saved = kernel_call(
            &inner,
            &source,
            "goal_plan_save",
            json!({"goal_id":id(goal),"expected_revision":revision(goal),
                "idempotency_key":"takeover-plan","plan":plan()}),
        )
        .await;
        success(&saved);
        let plan_path = saved.output["plan_artifacts"]["json_path"]
            .as_str()
            .unwrap()
            .into();
        let goal = &saved.output["goal"];
        let confirmed = kernel_call(
            &inner,
            &source,
            "goal_confirm_plan",
            json!({"goal_id":id(goal),"expected_revision":revision(goal),
                "plan_version":goal["plan_version"],"plan_sha256":goal["plan_hash"],
                "confirmation":"I reviewed this exact saved plan and authorize execution.",
                "idempotency_key":"takeover-confirm"}),
        )
        .await;
        success(&confirmed);
        let goal = &confirmed.output["goal"];
        let read = kernel_call(
            &inner,
            &source,
            "goal_read",
            json!({"goal_id":id(goal),"view":"node","node_id":"research"}),
        )
        .await;
        success(&read);
        let claim = kernel_call(
            &inner,
            &source,
            "goal_claim_node",
            json!({"goal_id":id(goal),"node_id":"research","expected_revision":revision(goal),
                "plan_version":goal["plan_version"],"idempotency_key":"takeover-claim"}),
        )
        .await;
        success(&claim);
        Self {
            inner,
            source,
            candidate,
            goal: claim.output["goal"].clone(),
            binding: claim.output["binding"].clone(),
            plan_path,
        }
    }

    pub(super) fn operation(&self, operation_id: &str) -> Value {
        let mut binding = self.binding.clone();
        binding["operation_id"] = json!(operation_id);
        binding
    }

    pub(super) async fn checkpoint(&self) -> ToolResult {
        kernel_call(
            &self.inner,
            &self.source,
            "goal_checkpoint",
            json!({"goal_id":id(&self.goal),"expected_revision":revision(&self.goal),
                "idempotency_key":"takeover-handoff","goal_context":self.operation("handoff"),
                "patch":{"action":"handoff","reason":"Continue this exact task in a fresh conversation",
                    "state":{"completed_summary":"Saved evidence remains available",
                        "next_step":"Continue research against the original requirements"}}}),
        )
        .await
    }

    pub(super) async fn handoff(
        &self,
        window: &ClientWindow,
        action: &str,
        fields: Value,
    ) -> ToolResult {
        let mut arguments = json!({"goal_id":id(&self.goal),"action":action});
        arguments
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        kernel_call(&self.inner, window, "goal_handoff", arguments).await
    }

    async fn denied_write(&self, window: &ClientWindow, operation: &str) -> ToolResult {
        kernel_call(
            &self.inner,
            window,
            "write_project_file",
            json!({"project":self.inner.project,"path":"must-not-write.txt","content":"forbidden",
                "goal_context":self.operation(operation)}),
        )
        .await
    }

    /// Retain each actual Runner request so prepare assertions distinguish a
    /// durable readiness receipt from a merely accepted request or local claim.
    async fn call_recording_runner(
        &self,
        window: &ClientWindow,
        tool: &str,
        arguments: Value,
    ) -> (ToolResult, Vec<RunnerRequest>) {
        let future = self.inner.runtime.call_tool_with_context(
            ToolCallRequest {
                tool_name: tool.into(),
                arguments,
            },
            ToolCallContext {
                transport: ToolTransport::Mcp,
                session_id: None,
                auth: Some(&self.inner.auth),
                window: Some(window),
                record_oauth_scope_denials: true,
                host_file_import_trust: HostFileImportTrust::Untrusted,
            },
        );
        tokio::pin!(future);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut requests = Vec::new();
        loop {
            tokio::select! {
                biased;
                outcome = &mut future => {
                    assert!(outcome.error_status.is_none(), "{:?}", outcome.error_status);
                    return (outcome.result.expect("kernel model result"), requests);
                }
                _ = tokio::time::sleep_until(deadline) => panic!("takeover request exceeded fixture deadline"),
                request = self.inner.runtime.runner_registry.poll(RunnerPollRequest {
                    client_id:"host-goal-runner".into(),runner_instance_id:"inst".into(),
                }) => {
                    if let Some(request) = request.unwrap() {
                        assert!(crate::test_support::runner_project_files::is_goal_file(&request),
                            "takeover preparation dispatched business work: {} {:?}", request.kind, request.path);
                        crate::test_support::runner_project_files::serve(
                            &self.inner.runtime, "inst", self.inner.temp.path(), &request,
                        ).await;
                        requests.push(request);
                    } else {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }

    pub(super) fn owner(&self) -> String {
        crate::tool_runtime::coding_agent::goal_owner_key(Some(&self.inner.auth)).unwrap()
    }

    pub(super) fn stored(&self) -> webcodex_store::HostGoalHandoff {
        self.inner
            .db
            .read_host_goal_handoff(&self.owner(), &id(&self.goal))
            .unwrap()
            .unwrap()
    }

    pub(super) async fn reserve(&self) -> Value {
        let checkpoint = self.checkpoint().await;
        success(&checkpoint);
        let handoff = self.stored();
        let reserved = self
            .inner
            .db
            .begin_host_goal_handoff_attempt(
                &self.owner(),
                &id(&self.goal),
                &handoff.handoff_id,
                None,
                crate::tool_runtime::host_goal::now(),
                300,
            )
            .unwrap();
        json!({"handoff_id":reserved.handoff_id,"attempt_id":reserved.attempt_id,
            "checkpoint_sequence":reserved.checkpoint_sequence})
    }

    pub(super) async fn prepare(&self, fields: &Value) -> ToolResult {
        self.handoff(&self.candidate, "prepare", fields.clone())
            .await
    }

    pub(super) async fn commit(&self, fields: &Value, prepared: &ToolResult) -> ToolResult {
        let mut fields = fields.clone();
        fields["package_sha256"] = prepared.output["package_sha256"].clone();
        self.handoff(&self.candidate, "commit", fields).await
    }
}

#[tokio::test]
async fn host_goal_takeover_prepares_from_runner_and_commits_one_owner_before_retirement() {
    let f = TakeoverFixture::new().await;
    let fields = f.reserve().await;
    let before = f.stored();
    assert_eq!(before.status, "awaiting_ready");
    let replayed = f.checkpoint().await;
    success(&replayed);
    assert_eq!(f.stored().handoff_id, before.handoff_id);
    assert_eq!(
        f.stored().attempt_count,
        1,
        "checkpoint replay must not launch or reserve another attempt"
    );
    assert_eq!(
        f.inner
            .db
            .read_host_context_state(&f.owner(), &before.source_context_epoch)
            .unwrap()
            .unwrap()
            .state,
        "draining"
    );
    let source_denied = f.denied_write(&f.source, "waiting-source-write").await;
    assert!(!source_denied.success);
    let candidate_denied = f
        .denied_write(&f.candidate, "unprepared-candidate-write")
        .await;
    assert!(!candidate_denied.success);

    let mut arguments = fields.clone();
    arguments["goal_id"] = json!(id(&f.goal));
    arguments["action"] = json!("prepare");
    let (prepared, requests) = f
        .call_recording_runner(&f.candidate, "goal_handoff", arguments)
        .await;
    success(&prepared);
    assert_eq!(prepared.output["execution_granted"], false);
    assert_eq!(prepared.output["handoff"]["status"], "prepared");
    assert_eq!(prepared.output["package"]["goal_id"], f.goal["goal_id"]);
    assert_eq!(prepared.output["package"]["project"], f.inner.project);
    assert_eq!(
        prepared.output["package"]["checkpoint_sequence"],
        fields["checkpoint_sequence"]
    );
    assert_eq!(
        prepared.output["package"]["progress"]["completed_summary"],
        "Saved evidence remains available"
    );
    assert!(
        requests.len() >= 4,
        "prepare must actually verify the plan and handoff files through the Runner"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.kind.starts_with("file_read")),
        "readiness probing must have no business or publication writes"
    );
    assert_eq!(
        f.stored().execution_owner_epoch,
        before.source_context_epoch
    );
    assert!(
        !f.denied_write(&f.candidate, "prepared-candidate-write")
            .await
            .success
    );

    let committed = f.commit(&fields, &prepared).await;
    success(&committed);
    assert_eq!(committed.output["execution_granted"], true);
    assert_eq!(committed.output["handoff"]["status"], "committed");
    let goal_after = f
        .inner
        .db
        .get_host_goal(&f.owner(), &id(&f.goal))
        .unwrap()
        .unwrap();
    assert_eq!(goal_after.goal_id, id(&f.goal));
    assert!(goal_after.fence > f.binding["fence"].as_u64().unwrap());
    assert_eq!(
        f.inner
            .db
            .read_host_context_state(&f.owner(), &before.source_context_epoch)
            .unwrap()
            .unwrap()
            .state,
        "retired"
    );
    assert!(
        !f.denied_write(&f.source, "retired-source-write")
            .await
            .success
    );
    assert!(!f.inner.temp.path().join("must-not-write.txt").exists());

    // A missing client response is reconciled from the durable ownership fact.
    let recovered = f.handoff(&f.candidate, "status", json!({})).await;
    success(&recovered);
    assert_eq!(recovered.output["execution_granted"], true);
    let repeated = f.commit(&fields, &prepared).await;
    success(&repeated);
    assert_eq!(repeated.output["handoff"], recovered.output["handoff"]);
    assert_eq!(f.stored().attempt_count, 1);
    assert_eq!(
        f.inner
            .db
            .get_host_goal(&f.owner(), &id(&f.goal))
            .unwrap()
            .unwrap()
            .fence,
        goal_after.fence,
        "duplicate confirmation cannot acquire a second execution fence"
    );
}

#[tokio::test]
async fn host_goal_takeover_rejects_modified_plan_and_handoff_before_recording_readiness() {
    for corrupt_plan in [true, false] {
        let f = TakeoverFixture::new().await;
        let fields = f.reserve().await;
        let path = if corrupt_plan {
            f.plan_path.clone()
        } else {
            f.inner.temp.path().join(format!(
                "task-output/handoff/{}/checkpoint-{}/handoff.json",
                id(&f.goal),
                fields["checkpoint_sequence"].as_u64().unwrap()
            ))
        };
        assert!(path.is_file(), "the tested artifact must actually exist");
        std::fs::write(&path, "{\"tampered\":true}\n").unwrap();
        let prepared = f.prepare(&fields).await;
        assert!(
            !prepared.success,
            "modified Runner artifact must reject readiness"
        );
        let stored = f.stored();
        assert_eq!(stored.status, "awaiting_ready");
        assert!(stored.prepared_at.is_none());
        assert!(stored.prepared_package_sha256.is_none());
        assert_eq!(stored.execution_owner_epoch, stored.source_context_epoch);
    }
}

#[tokio::test]
async fn host_goal_takeover_requires_preparation_digest_and_exact_candidate() {
    let f = TakeoverFixture::new().await;
    let fields = f.reserve().await;
    let mut unprepared = fields.clone();
    unprepared["package_sha256"] = json!("a".repeat(64));
    let denied = f.handoff(&f.candidate, "commit", unprepared.clone()).await;
    assert!(!denied.success);
    assert_ne!(f.stored().status, "committed");
    let prepared = f.prepare(&fields).await;
    success(&prepared);
    let wrong_digest = f.handoff(&f.candidate, "commit", unprepared).await;
    assert!(!wrong_digest.success);
    let competitor = ClientWindow::from_opaque("openai-session", "takeover-competitor").unwrap();
    let other_prepare = f.handoff(&competitor, "prepare", fields.clone()).await;
    assert!(
        !other_prepare.success,
        "one attempt cannot bind two conversations"
    );
    let mut other_commit = fields.clone();
    other_commit["package_sha256"] = prepared.output["package_sha256"].clone();
    assert!(!f.handoff(&competitor, "commit", other_commit).await.success);
    success(&f.commit(&fields, &prepared).await);
}

#[tokio::test]
async fn host_goal_takeover_retries_revoke_old_candidate_and_keep_private_surfaces_closed() {
    let f = TakeoverFixture::new().await;
    let fields = f.reserve().await;
    let prepared = f.prepare(&fields).await;
    success(&prepared);
    let first = f.stored();
    f.inner
        .db
        .fail_host_goal_handoff_attempt(
            &f.owner(),
            &id(&f.goal),
            &first.handoff_id,
            first.attempt_id.as_deref().unwrap(),
            "simulated candidate timeout",
            crate::tool_runtime::host_goal::now(),
        )
        .unwrap();
    let second = f
        .inner
        .db
        .begin_host_goal_handoff_attempt(
            &f.owner(),
            &id(&f.goal),
            &first.handoff_id,
            first.attempt_id.as_deref(),
            crate::tool_runtime::host_goal::now(),
            300,
        )
        .unwrap();
    assert_eq!(second.attempt_count, 2);
    assert_ne!(second.attempt_id, first.attempt_id);
    assert!(
        !f.commit(&fields, &prepared).await.success,
        "a revoked candidate cannot confirm a late successful takeover"
    );
    for (tool, args) in [
        (
            "read_file",
            json!({"project":f.inner.project,"path":"other-agent/private/memory.txt"}),
        ),
        (
            "memory_read",
            json!({"project":f.inner.project,"memory_key":"other-agent-memory"}),
        ),
        (
            "run_shell",
            json!({"project":f.inner.project,"command":"read-private-memory"}),
        ),
        (
            "goal_read",
            json!({"goal_id":"some-other-goal","view":"summary"}),
        ),
    ] {
        let denied = kernel_call(&f.inner, &f.candidate, tool, args).await;
        assert!(
            !denied.success,
            "revoked candidate unexpectedly reached {tool}"
        );
        assert_eq!(denied.output["error_kind"], "goal_handoff_context_revoked");
    }
}

#[tokio::test]
async fn host_goal_takeover_committed_owner_keeps_file_cli_and_memory_access_fail_closed() {
    let f = TakeoverFixture::new().await;
    let fields = f.reserve().await;
    let prepared = f.prepare(&fields).await;
    success(&prepared);
    success(&f.commit(&fields, &prepared).await);
    for (tool, args) in [
        (
            "read_file",
            json!({"project":f.inner.project,"path":"other-agent/private/memory.txt"}),
        ),
        (
            "write_project_file",
            json!({"project":f.inner.project,"path":"result.txt","content":"x"}),
        ),
        (
            "run_shell",
            json!({"project":f.inner.project,"command":"read-private-memory"}),
        ),
        (
            "memory_read",
            json!({"project":f.inner.project,"memory_key":"other-agent-memory"}),
        ),
        (
            "goal_read",
            json!({"goal_id":"another-goal","view":"summary"}),
        ),
    ] {
        let denied = kernel_call(&f.inner, &f.candidate, tool, args).await;
        assert!(
            !denied.success,
            "unadapted task surface unexpectedly reached {tool}"
        );
        assert_eq!(
            denied.output["error_kind"],
            "goal_handoff_isolation_unavailable"
        );
    }
    let own = kernel_call(
        &f.inner,
        &f.candidate,
        "goal_read",
        json!({"goal_id":id(&f.goal),"view":"node","node_id":"research"}),
    )
    .await;
    success(&own);
    assert_eq!(own.output["goal"]["goal_id"], f.goal["goal_id"]);
}
