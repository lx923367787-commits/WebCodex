//! Exercise host acceptance through the real durable Job registry and Runner
//! transport fixture, including crashes between the two persistent ledgers.
use super::goals::{goal_auth, Behavior, Fixture};
use crate::tool_runtime::host_goal_checks::command_identity;
use crate::tool_runtime::host_goal_files;
use crate::tool_runtime::{ToolCall, ToolResult};
use serde_json::json;
use std::time::Duration;
use webcodex_core::goal::{GoalArtifactSnapshot, GoalCriterion};
use webcodex_core::host_goal::*;

const CONTENT: &str = "accepted final result\n";

fn command() -> GoalCriterion {
    GoalCriterion::Command {
        id: "command".into(),
        description: "Acceptance command succeeds".into(),
        program: "test-check".into(),
        args: vec!["verify".into()],
        cwd: None,
        timeout_secs: 10,
    }
}

pub(super) async fn prepare(
    behavior: Behavior,
    mutate_during_final: bool,
) -> (Fixture, HostGoalRecord, HostGoalExecutionBinding) {
    let fixture = Fixture::new(behavior).await;
    std::fs::write(fixture.temp.path().join("result.txt"), CONTENT).unwrap();
    let owner = crate::tool_runtime::coding_agent::goal_owner_key(Some(&goal_auth())).unwrap();
    let goal = fixture
        .db
        .create_host_goal_at(
            &owner,
            &fixture.project,
            &fixture.temp.path().to_string_lossy(),
            "Verify the persisted deliverable",
            "host-create",
            1,
        )
        .unwrap()
        .goal;
    let node_criteria = if mutate_during_final {
        vec![GoalCriterion::FileContains {
            id: "content".into(),
            description: "Result contains accepted text".into(),
            path: "result.txt".into(),
            text: "accepted final result".into(),
        }]
    } else {
        vec![command()]
    };
    let plan = HostGoalPlan {
        objective: goal.objective.clone(),
        constraints: vec!["Preserve evidence".into()],
        deliverables: vec!["result.txt".into()],
        verification_paths: vec!["result.txt".into()],
        final_criteria: vec![command()],
        nodes: vec![HostGoalNode {
            id: "verify".into(),
            title: "Verify result".into(),
            inputs: vec!["result.txt".into()],
            verification_paths: vec![],
            work: "Run acceptance checks".into(),
            outputs: vec!["Verified result".into()],
            criterion_ids: vec!["command".into()],
            acceptance: node_criteria,
            failure_action: "Repair the result and recheck".into(),
        }],
        state_management: HostGoalStateManagement {
            facts: "Record hashes".into(),
            decisions: "Record choices".into(),
            issues: "Preserve failures".into(),
            artifacts: "Track result.txt".into(),
            checkpoint_triggers: vec!["After verification".into()],
            resume_instructions: "Recover the original Job".into(),
        },
    };
    let prepared = fixture
        .db
        .save_host_goal_plan(&owner, &goal.goal_id, goal.revision, &plan, None, "save", 2)
        .unwrap();
    let artifact = host_goal_files::materialize(
        &fixture.db.host_goal_plan_root().unwrap(),
        &goal.goal_id,
        prepared.plan.version,
        &prepared.plan.hash,
        &json!(&plan),
    )
    .unwrap();
    let published = fixture
        .db
        .materialize_host_goal_plan(
            &owner,
            &goal.goal_id,
            1,
            &prepared.plan.hash,
            &HostGoalMaterialization {
                json_path: artifact.json_path,
                json_hash: artifact.json_sha256,
                markdown_path: artifact.markdown_path,
                markdown_hash: artifact.markdown_sha256,
                verified_at: 3,
            },
            3,
        )
        .unwrap()
        .goal;
    let ready = fixture
        .db
        .confirm_host_goal_plan(
            &owner,
            &goal.goal_id,
            published.revision,
            1,
            &prepared.plan.hash,
            "I have read the local plan; start execution",
            "confirm",
            4,
        )
        .unwrap()
        .goal;
    let claim = fixture
        .db
        .claim_host_goal_node(
            &owner,
            &goal.goal_id,
            ready.revision,
            "verify",
            false,
            "claim",
            5,
        )
        .unwrap();
    let mut state = claim.goal.state.clone();
    // Command input tracking must come from the saved plan, even if the model
    // never populated progress-state artifact refs.
    state.artifact_refs = vec![];
    let goal = fixture
        .db
        .checkpoint_host_goal(
            &owner,
            &goal.goal_id,
            claim.goal.revision,
            Some(&claim.attempt.binding),
            &state,
            webcodex_store::HostGoalCheckpointAction::Progress,
            "Track output fingerprint",
            "track",
            6,
        )
        .unwrap()
        .goal;
    (fixture, goal, claim.attempt.binding)
}

async fn check(
    fixture: &Fixture,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
) -> Result<webcodex_store::HostGoalMutation, ToolResult> {
    fixture
        .runtime
        .host_goal_check_node(
            goal.goal_id.clone(),
            binding.node_id.clone(),
            binding.attempt_id.clone(),
            goal.revision,
            goal.plan_version,
            "client-check".into(),
            None,
            Some(&goal_auth()),
        )
        .await
}

async fn wait_check(
    fixture: &Fixture,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
) -> Result<webcodex_store::HostGoalMutation, ToolResult> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match check(fixture, goal, binding).await {
            Err(error) if error.output["error_kind"] == "goal_check_pending" => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "Check Job did not finish: {:?}",
                    error.output
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            result => return result,
        }
    }
}

fn seed_intent(
    fixture: &Fixture,
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
) -> HostGoalOperation {
    let manifest = vec![GoalArtifactSnapshot {
        path: "result.txt".into(),
        sha256: Some(host_goal_files::sha256(CONTENT.as_bytes())),
    }];
    let (id, hash) = command_identity(binding, &command(), &manifest).unwrap();
    fixture
        .db
        .begin_host_goal_operation_with_manifest(
            &goal.owner_key,
            binding,
            &id,
            "run_detached_process",
            &hash,
            &manifest,
            7,
        )
        .unwrap()
        .operation
}

async fn dispatch_original_job(fixture: &Fixture, operation: &HostGoalOperation) -> String {
    let result = fixture
        .runtime
        .host_goal_internal_tool(
            ToolCall::RunDetachedProcess {
                project: fixture.project.clone(),
                idempotency_key: operation.operation_id.clone(),
                executable: "test-check".into(),
                args: vec!["verify".into()],
                stdin: None,
                session_id: None,
                timeout_secs: Some(10),
                cwd: None,
                purpose: None,
            },
            Some(&goal_auth()),
        )
        .await;
    assert!(result.success, "{:?}", result);
    let job = result.output["job_id"].as_str().unwrap().to_owned();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let result = fixture
            .runtime
            .host_goal_internal_tool(
                ToolCall::JobStatus {
                    job_id: job.clone(),
                    include_command_preview: false,
                },
                Some(&goal_auth()),
            )
            .await;
        assert!(result.success, "{:?}", result);
        if result.output["status"] == "completed" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Original Job did not finish"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    job
}

#[tokio::test]
async fn host_goal_check_recovery_committed_response_replays_without_read_or_dispatch() {
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    let completed = wait_check(&fixture, &goal, &binding).await.unwrap();
    assert_eq!(completed.goal.phase, HostGoalPhase::Completed);
    let jobs = fixture.command_jobs();
    let reads = fixture.file_read_count();
    let replay = check(&fixture, &goal, &binding).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.goal, completed.goal);
    assert_eq!(fixture.command_jobs(), jobs);
    assert_eq!(fixture.file_read_count(), reads);
    assert_eq!(jobs.len(), 1);
}

#[tokio::test]
async fn host_goal_check_recovery_intent_before_dispatch_recovers_original_identity() {
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    let intent = seed_intent(&fixture, &goal, &binding);
    assert!(intent.job_id.is_none());
    assert!(fixture.command_jobs().is_empty());
    let completed = wait_check(&fixture, &goal, &binding).await.unwrap();
    assert_eq!(completed.goal.phase, HostGoalPhase::Completed);
    let recovered = fixture
        .db
        .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &intent.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.status, HostGoalOperationStatus::Succeeded);
    assert_eq!(fixture.command_jobs(), vec![recovered.job_id.unwrap()]);
}

#[tokio::test]
async fn host_goal_check_recovery_lost_job_response_does_not_start_a_second_job() {
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    let intent = seed_intent(&fixture, &goal, &binding);
    let original_job = dispatch_original_job(&fixture, &intent).await;
    // Crash boundary: the Job ledger committed, but the host operation has no receipt.
    assert!(fixture
        .db
        .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &intent.operation_id)
        .unwrap()
        .unwrap()
        .job_id
        .is_none());
    assert_eq!(
        wait_check(&fixture, &goal, &binding)
            .await
            .unwrap()
            .goal
            .phase,
        HostGoalPhase::Completed
    );
    assert_eq!(fixture.command_jobs(), vec![original_job.clone()]);
    assert_eq!(
        fixture
            .db
            .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &intent.operation_id)
            .unwrap()
            .unwrap()
            .job_id,
        Some(original_job)
    );
}

#[tokio::test]
async fn host_goal_check_recovery_changed_inputs_require_new_command_evidence() {
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    let intent = seed_intent(&fixture, &goal, &binding);
    let original_job = dispatch_original_job(&fixture, &intent).await;
    fixture
        .db
        .finish_host_goal_operation(
            &goal.owner_key,
            &goal.goal_id,
            &intent.operation_id,
            HostGoalOperationStatus::Succeeded,
            Some(&original_job),
            Some("Original command passed against original file hash"),
            8,
        )
        .unwrap();
    let changed = "accepted final result with a later edit\n";
    std::fs::write(fixture.temp.path().join("result.txt"), changed).unwrap();
    assert_eq!(
        wait_check(&fixture, &goal, &binding)
            .await
            .unwrap()
            .goal
            .phase,
        HostGoalPhase::Completed
    );
    let jobs = fixture.command_jobs();
    assert_eq!(jobs.len(), 2);
    assert_ne!(jobs[0], jobs[1]);
    let attempt = fixture
        .db
        .latest_host_goal_attempt(&goal.owner_key, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(attempt.checks[0].source_refs, vec![jobs[1].clone()]);
    assert_eq!(
        attempt.checks[0].artifacts[0].sha256,
        Some(host_goal_files::sha256(changed.as_bytes()))
    );
}

#[tokio::test]
async fn host_goal_check_recovery_final_command_cannot_invalidate_node_evidence_and_complete() {
    let (fixture, goal, binding) = prepare(Behavior::CommandChangesFile, true).await;
    let outcome = wait_check(&fixture, &goal, &binding).await;
    match outcome {
        Err(error) => assert_eq!(
            error.output["error_kind"], "goal_artifacts_changed",
            "{:?}",
            error
        ),
        Ok(result) => assert_ne!(result.goal.phase, HostGoalPhase::Completed),
    }
    assert_ne!(
        fixture
            .db
            .get_host_goal(&goal.owner_key, &goal.goal_id)
            .unwrap()
            .unwrap()
            .phase,
        HostGoalPhase::Completed
    );
    assert_eq!(fixture.command_jobs().len(), 1);
}

#[tokio::test]
async fn host_goal_check_recovery_unreadable_declared_input_cannot_run_command() {
    let (fixture, goal, binding) = prepare(Behavior::ReadError, false).await;
    let error = check(&fixture, &goal, &binding).await.unwrap_err();
    assert_eq!(
        error.output["error_kind"],
        "goal_verification_input_unreadable"
    );
    assert!(fixture.command_jobs().is_empty());
}

#[tokio::test]
async fn host_goal_check_recovery_binary_file_exists_uses_metadata_fingerprint() {
    let (fixture, goal, _) = prepare(Behavior::Accept, false).await;
    let bytes = b"%PDF-1.7\n\xff\x00binary test fixture\n";
    std::fs::write(fixture.temp.path().join("report.pdf"), bytes).unwrap();
    let text = fixture
        .runtime
        .host_goal_internal_tool(
            ToolCall::ReadFile {
                project: goal.project.clone(),
                path: "report.pdf".into(),
                session_id: None,
                start_line: Some(1),
                limit: Some(1),
                with_line_numbers: Some(false),
            },
            Some(&goal_auth()),
        )
        .await;
    assert!(
        !text.success,
        "Binary fixture must not be accepted by UTF-8 text reading"
    );
    assert!(text.output.get("sha256").is_none());
    let mut plan = fixture
        .db
        .get_host_goal_plan(&goal.owner_key, &goal.goal_id, None)
        .unwrap()
        .unwrap()
        .plan;
    let criterion = GoalCriterion::FileExists {
        id: "pdf".into(),
        description: "The binary PDF artifact exists".into(),
        path: "report.pdf".into(),
    };
    plan.deliverables = vec!["report.pdf".into()];
    plan.verification_paths.clear();
    plan.final_criteria = vec![criterion.clone()];
    plan.nodes[0].acceptance = vec![criterion];
    plan.nodes[0].criterion_ids = vec!["pdf".into()];
    let prepared = fixture
        .db
        .save_host_goal_plan(
            &goal.owner_key,
            &goal.goal_id,
            goal.revision,
            &plan,
            Some("Verify the produced binary PDF instead of the command check"),
            "binary-plan",
            20,
        )
        .unwrap();
    let artifact = host_goal_files::materialize(
        &fixture.db.host_goal_plan_root().unwrap(),
        &goal.goal_id,
        prepared.plan.version,
        &prepared.plan.hash,
        &json!(&plan),
    )
    .unwrap();
    let published = fixture
        .db
        .materialize_host_goal_plan(
            &goal.owner_key,
            &goal.goal_id,
            prepared.plan.version,
            &prepared.plan.hash,
            &HostGoalMaterialization {
                json_path: artifact.json_path,
                json_hash: artifact.json_sha256,
                markdown_path: artifact.markdown_path,
                markdown_hash: artifact.markdown_sha256,
                verified_at: 21,
            },
            21,
        )
        .unwrap()
        .goal;
    let ready = fixture
        .db
        .confirm_host_goal_plan(
            &goal.owner_key,
            &goal.goal_id,
            published.revision,
            published.plan_version,
            &prepared.plan.hash,
            "I reviewed the binary verification plan; execute it",
            "binary-confirm",
            22,
        )
        .unwrap()
        .goal;
    let claim = fixture
        .db
        .claim_host_goal_node(
            &goal.owner_key,
            &goal.goal_id,
            ready.revision,
            "verify",
            false,
            "binary-claim",
            23,
        )
        .unwrap();
    let text_reads = fixture.text_file_read_count();
    let accepted = wait_check(&fixture, &claim.goal, &claim.attempt.binding)
        .await
        .unwrap();
    assert_eq!(accepted.goal.phase, HostGoalPhase::Completed);
    assert_eq!(
        fixture.text_file_read_count(),
        text_reads,
        "Binary verification must use metadata, not UTF-8 decoding"
    );
    assert!(fixture.command_jobs().is_empty());
    let attempt = fixture
        .db
        .latest_host_goal_attempt(&goal.owner_key, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        attempt.checks[0].artifacts,
        vec![GoalArtifactSnapshot {
            path: "report.pdf".into(),
            sha256: Some(host_goal_files::sha256(bytes))
        }]
    );
}

#[tokio::test]
async fn host_goal_check_recovery_changed_pending_inputs_require_cancel_and_new_attempt() {
    use crate::tool_runtime::host_goal_context::{self as context, RequestScope};
    let (fixture, goal, binding) = prepare(Behavior::Accept, false).await;
    let old = seed_intent(&fixture, &goal, &binding);
    std::fs::write(
        fixture.temp.path().join("result.txt"),
        "accepted final result after input changed\n",
    )
    .unwrap();
    let error = check(&fixture, &goal, &binding).await.unwrap_err();
    assert_eq!(error.output["error_kind"], "goal_check_inputs_changed");
    assert!(
        fixture.command_jobs().is_empty(),
        "Changed inputs must not dispatch the original uncertain command"
    );
    assert_eq!(
        fixture
            .db
            .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &old.operation_id)
            .unwrap()
            .unwrap()
            .status,
        HostGoalOperationStatus::Intent
    );
    let cancel = || ToolCall::GoalCheckpoint {
        goal_id: goal.goal_id.clone(),
        expected_revision: goal.revision,
        idempotency_key: "cancel-uncertain-check".into(),
        context_epoch: None,
        patch: json!({"reason":"User inspected the uncertain check before cancelling it","operation_resolutions":[{"operation_id":old.operation_id,"status":"cancelled","evidence":"Inspected the original request and changed input; no original Job is available","source_refs":["result.txt"],"user_confirmation":"Cancel this uncertain check and start a new attempt against the current input"}]}),
    };
    let scope = || {
        let mut metadata = json!(&binding);
        metadata["operation_id"] = json!("cancel-checkpoint");
        RequestScope::new(
            goal.owner_key.clone(),
            None,
            &json!({"goal_context":metadata}),
        )
        .unwrap()
    };
    let lease = context::lease_active_operation(&goal.goal_id, &old.operation_id);
    let live = context::scope(
        scope(),
        fixture
            .runtime
            .dispatch_host_goal_tool(cancel(), Some(&goal_auth())),
    )
    .await;
    assert_eq!(live.output["error_kind"], "goal_operation_still_active");
    drop(lease);
    let result = context::scope(
        scope(),
        fixture
            .runtime
            .dispatch_host_goal_tool(cancel(), Some(&goal_auth())),
    )
    .await;
    assert!(result.success, "{:?}", result);
    let current = fixture
        .db
        .get_host_goal(&goal.owner_key, &goal.goal_id)
        .unwrap()
        .unwrap();
    let next = fixture
        .db
        .claim_host_goal_node_for_plan(
            &goal.owner_key,
            &goal.goal_id,
            current.revision,
            &binding.node_id,
            true,
            current.plan_version,
            Some(&binding.attempt_id),
            "new-after-cancel",
            20,
        )
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let result = fixture
            .runtime
            .host_goal_check_node(
                goal.goal_id.clone(),
                next.attempt.binding.node_id.clone(),
                next.attempt.binding.attempt_id.clone(),
                next.goal.revision,
                next.goal.plan_version,
                "new-attempt-check".into(),
                None,
                Some(&goal_auth()),
            )
            .await;
        match result {
            Ok(result) => {
                assert_eq!(result.goal.phase, HostGoalPhase::Completed);
                break;
            }
            Err(error) if error.output["error_kind"] == "goal_check_pending" => {
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) => panic!("New check failed: {error:?}"),
        }
    }
    assert_eq!(
        fixture.command_jobs().len(),
        1,
        "Only the newly claimed attempt may dispatch a Job"
    );
    let old = fixture
        .db
        .get_host_goal_operation(&goal.owner_key, &goal.goal_id, &old.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(old.status, HostGoalOperationStatus::Cancelled);
    assert!(old.job_id.is_none());
}

#[tokio::test]
async fn host_goal_check_recovery_serial_draft_then_final_may_replace_the_same_file() {
    let (fixture, goal, _) = prepare(Behavior::Accept, false).await;
    let draft = "draft implementation\n";
    std::fs::write(fixture.temp.path().join("result.txt"), draft).unwrap();
    assert!(!fixture.temp.path().join("final.txt").exists());
    let mut plan = fixture
        .db
        .get_host_goal_plan(&goal.owner_key, &goal.goal_id, None)
        .unwrap()
        .unwrap()
        .plan;
    plan.objective =
        "Create a draft, then refine the same file and produce the final artifact".into();
    plan.verification_paths = vec!["final.txt".into()];
    let final_text = GoalCriterion::FileContains {
        id: "final-text".into(),
        description: "Final source contains accepted result".into(),
        path: "result.txt".into(),
        text: "accepted final result".into(),
    };
    let final_artifact = GoalCriterion::FileExists {
        id: "final-artifact".into(),
        description: "Final artifact exists".into(),
        path: "final.txt".into(),
    };
    plan.final_criteria = vec![final_text.clone(), final_artifact, command()];
    let mut first = plan.nodes[0].clone();
    first.id = "draft".into();
    first.title = "Build draft".into();
    first.work = "Produce the initial draft".into();
    first.verification_paths = vec!["result.txt".into()];
    first.acceptance = vec![
        GoalCriterion::FileContains {
            id: "draft-text".into(),
            description: "Draft exists".into(),
            path: "result.txt".into(),
            text: "draft implementation".into(),
        },
        command(),
    ];
    first.criterion_ids = vec!["final-text".into()];
    let mut second = first.clone();
    second.id = "final".into();
    second.title = "Refine draft".into();
    second.work = "Replace the draft with final code and generate final.txt".into();
    second.acceptance = vec![final_text, command()];
    second.criterion_ids = vec![
        "final-text".into(),
        "final-artifact".into(),
        "command".into(),
    ];
    plan.nodes = vec![first, second];
    let prepared = fixture
        .db
        .save_host_goal_plan(
            &goal.owner_key,
            &goal.goal_id,
            goal.revision,
            &plan,
            Some("Use a two-stage draft and final workflow"),
            "serial-plan",
            30,
        )
        .unwrap();
    let artifact = host_goal_files::materialize(
        &fixture.db.host_goal_plan_root().unwrap(),
        &goal.goal_id,
        prepared.plan.version,
        &prepared.plan.hash,
        &json!(&plan),
    )
    .unwrap();
    let published = fixture
        .db
        .materialize_host_goal_plan(
            &goal.owner_key,
            &goal.goal_id,
            prepared.plan.version,
            &prepared.plan.hash,
            &HostGoalMaterialization {
                json_path: artifact.json_path,
                json_hash: artifact.json_sha256,
                markdown_path: artifact.markdown_path,
                markdown_hash: artifact.markdown_sha256,
                verified_at: 31,
            },
            31,
        )
        .unwrap()
        .goal;
    let ready = fixture
        .db
        .confirm_host_goal_plan(
            &goal.owner_key,
            &goal.goal_id,
            published.revision,
            published.plan_version,
            &prepared.plan.hash,
            "I reviewed and approve the draft then final plan",
            "serial-confirm",
            32,
        )
        .unwrap()
        .goal;
    let mut current = ready;
    let mut draft_attempt = None;
    for (index, node_id) in ["draft", "final"].into_iter().enumerate() {
        if node_id == "final" {
            std::fs::write(fixture.temp.path().join("result.txt"), CONTENT).unwrap();
            std::fs::write(fixture.temp.path().join("final.txt"), "final deliverable\n").unwrap();
        }
        let claim = fixture
            .db
            .claim_host_goal_node(
                &goal.owner_key,
                &goal.goal_id,
                current.revision,
                node_id,
                false,
                &format!("claim-{node_id}"),
                33 + index as i64,
            )
            .unwrap();
        if node_id == "draft" {
            draft_attempt = Some(claim.attempt.binding.attempt_id.clone());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let result = fixture
                .runtime
                .host_goal_check_node(
                    goal.goal_id.clone(),
                    node_id.into(),
                    claim.attempt.binding.attempt_id.clone(),
                    claim.goal.revision,
                    claim.goal.plan_version,
                    format!("check-{node_id}"),
                    None,
                    Some(&goal_auth()),
                )
                .await;
            match result {
                Ok(result) => {
                    current = result.goal;
                    break;
                }
                Err(error) if error.output["error_kind"] == "goal_check_pending" => {
                    assert!(tokio::time::Instant::now() < deadline);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(error) => panic!("Serial {node_id} acceptance failed: {error:?}"),
            }
        }
        if node_id == "draft" {
            assert_eq!(current.phase, HostGoalPhase::Ready);
            assert!(
                !fixture.temp.path().join("final.txt").exists(),
                "The early node must not require its future final artifact"
            );
        }
    }
    assert_eq!(current.phase, HostGoalPhase::Completed);
    assert_eq!(current.completed_node_ids, vec!["draft", "final"]);
    assert_eq!(fixture.command_jobs().len(), 2);
    let historical = fixture
        .db
        .get_host_goal_attempt(&goal.owner_key, &goal.goal_id, &draft_attempt.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(historical.status, HostGoalAttemptStatus::Passed);
    assert_eq!(
        historical.checks[0].artifacts,
        vec![GoalArtifactSnapshot {
            path: "result.txt".into(),
            sha256: Some(host_goal_files::sha256(draft.as_bytes()))
        }]
    );
    let final_attempt = fixture
        .db
        .latest_host_goal_attempt(&goal.owner_key, &goal.goal_id)
        .unwrap()
        .unwrap();
    assert!(final_attempt.final_checks.iter().all(|check| check.passed));
    assert!(final_attempt.final_checks[0]
        .artifacts
        .iter()
        .any(|item| item.path == "result.txt"
            && item.sha256 == Some(host_goal_files::sha256(CONTENT.as_bytes()))));
}
