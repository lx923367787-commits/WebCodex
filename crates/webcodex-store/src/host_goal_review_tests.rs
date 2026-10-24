//! Protocol tests use synthetic materials, not claims of actual visual review.
use super::*;
use webcodex_core::goal::GoalArtifactSnapshot;

fn fixture(
    layout: bool,
    max_rounds: u8,
) -> (
    tempfile::TempDir,
    Database,
    HostGoalRecord,
    HostGoalExecutionBinding,
    HostGoalInvocationContext,
) {
    let mut value = plan();
    value.nodes[0].review_max_rounds = Some(max_rounds);
    if layout {
        value.nodes[0].delivery_kind = Some(HostGoalDeliveryKind::Layout);
        value.nodes[0].review = Some(0);
        value.nodes[0].managed_artifacts = Some(vec![HostGoalManagedArtifact {
            path: "report.pdf".into(),
            kind: HostGoalDeliveryKind::Layout,
        }]);
    }
    fixture_plan(value)
}

fn fixture_plan(
    value: HostGoalPlan,
) -> (
    tempfile::TempDir,
    Database,
    HostGoalRecord,
    HostGoalExecutionBinding,
    HostGoalInvocationContext,
) {
    let (dir, db) = database();
    let goal = new_goal(&db);
    let saved = db
        .save_host_goal_plan(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &value,
            None,
            "save",
            NOW,
        )
        .unwrap();
    assert!(saved.plan.plan.nodes[0].review.is_some());
    let materialized = db
        .materialize_host_goal_plan(
            OWNER,
            &goal.goal_id,
            saved.plan.version,
            &saved.plan.hash,
            &HostGoalMaterialization {
                json_path: "/state/plan.json".into(),
                json_hash: "a".repeat(64),
                markdown_path: "/state/plan.md".into(),
                markdown_hash: "b".repeat(64),
                verified_at: NOW,
            },
            NOW,
        )
        .unwrap();
    let ready = db
        .confirm_host_goal_plan(
            OWNER,
            &goal.goal_id,
            materialized.goal.revision,
            saved.plan.version,
            &saved.plan.hash,
            "Synthetic explicit user confirmation",
            "confirm",
            NOW,
        )
        .unwrap()
        .goal;
    let context = HostGoalInvocationContext {
        context_epoch: "review-chat-a".into(),
        identity_source: "window_bound".into(),
    };
    db.observe_host_context(
        OWNER,
        &context.context_epoch,
        "initial",
        0,
        false,
        Some((3000, 4000)),
    )
    .unwrap();
    let claim = managed_claim(&db, &ready, &context, "claim");
    (dir, db, claim.goal, claim.attempt.binding, context)
}

fn packet(
    binding: &HostGoalExecutionBinding,
    context: &HostGoalInvocationContext,
    id: &str,
    hash: char,
    layout: bool,
) -> HostGoalReviewPacket {
    let path = if layout { "report.pdf" } else { "source.txt" };
    let fingerprint = hash.to_string().repeat(64);
    HostGoalReviewPacket {
        packet_id: id.into(),
        binding: binding.clone(),
        context_epoch: Some(context.context_epoch.clone()),
        manifest: vec![GoalArtifactSnapshot {
            path: path.into(),
            sha256: Some(fingerprint.clone()),
        }],
        state_sha256: None,
        machine_checks: vec![],
        materials: if layout {
            vec![]
        } else {
            vec![HostGoalReviewMaterial{id:"source".into(),path:path.into(),sha256:fingerprint,kind:"text".into(),preview_path:None,preview_sha256:None,render_source_sha256:None,render_dependencies_sha256:None,content:Some("The actual bounded comparison, together with its documented calculations and limitations.".into())}]
        },
        required_scopes: vec![if layout {
            "report.pdf#visual_pending".into()
        } else {
            "source".into()
        }],
        delivered_scopes: if layout {
            vec![]
        } else {
            vec!["source".into()]
        },
        created_at: NOW,
    }
}

fn page(packet: &HostGoalReviewPacket, number: u32) -> HostGoalReviewMaterial {
    HostGoalReviewMaterial {
        id: format!("report.pdf#page:{number}"),
        path: "report.pdf".into(),
        sha256: packet.manifest[0].sha256.clone().unwrap(),
        kind: "image".into(),
        preview_path: Some(format!("previews/{number}.png")),
        preview_sha256: Some(format!("{number:x}").repeat(64)),
        render_source_sha256: packet.manifest[0].sha256.clone(),
        render_dependencies_sha256: Some("d".repeat(64)),
        content: None,
    }
}

fn verdict(
    packet: &HostGoalReviewPacket,
    scopes: &[String],
    passed: bool,
) -> Vec<HostGoalCheckEvidence> {
    vec![HostGoalCheckEvidence {
        criterion_id: "analysis".into(),
        passed,
        source: HostGoalEvidenceSource::ModelSelfCheck,
        evidence: format!(
            "{}: {}",
            packet.manifest[0].path,
            if passed {
                "Inspected the complete returned source and all listed pages; labels are readable and the prior overlap has been removed."
            } else {
                "Page 2 has a title overlapping the numeric table, hiding the second column and making the totals ambiguous."
            }
        ),
        source_refs: vec![packet.packet_id.clone()],
        artifacts: packet.manifest.clone(),
        review_packet_id: Some(packet.packet_id.clone()),
        reviewed_scopes: scopes.to_vec(),
        findings: if passed {
            vec![]
        } else {
            vec![HostGoalReviewFinding {
                location: "report.pdf page 2 upper table".into(),
                impact: "Title obscures column values".into(),
                required_change: "Move title above table and return the rerendered full pages"
                    .into(),
            }]
        },
    }]
}

fn deliver_all(db: &Database, packet: &HostGoalReviewPacket) -> Vec<String> {
    let materials = [page(packet, 1), page(packet, 2)];
    let scopes = materials
        .iter()
        .map(|material| material.id.clone())
        .collect::<Vec<_>>();
    db.record_host_goal_review_delivery(
        OWNER,
        &packet.binding,
        &packet.packet_id,
        packet.context_epoch.as_deref(),
        &materials,
        &scopes,
        &scopes,
        NOW,
    )
    .unwrap();
    scopes
}

#[test]
fn host_goal_review_pending_partial_delivery_then_two_round_repair_cannot_complete_early() {
    let (_dir, db, goal, binding, context) = fixture(true, 3);
    let first = packet(&binding, &context, "first", 'a', true);
    db.prepare_host_goal_review(OWNER, &binding, &first, NOW)
        .unwrap();
    let scopes = vec!["report.pdf#page:1".into(), "report.pdf#page:2".into()];
    let good = verdict(&first, &scopes, true);
    assert!(db
        .check_host_goal_node_with_request_in_context(
            OWNER,
            &binding,
            goal.revision,
            &good,
            &good,
            "early",
            "early",
            NOW,
            Some(&context)
        )
        .is_err());
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &first.manifest,
            &good,
            NOW
        )
        .is_err());
    let materials = [page(&first, 1), page(&first, 2)];
    db.record_host_goal_review_delivery(
        OWNER,
        &binding,
        "first",
        Some(&context.context_epoch),
        &materials,
        &scopes,
        &scopes[..1],
        NOW,
    )
    .unwrap();
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &first.manifest,
            &good,
            NOW
        )
        .is_err());
    assert!(db
        .read_host_goal_review(OWNER, &goal.goal_id, &binding.node_id)
        .unwrap()
        .unwrap()
        .rounds
        .is_empty());
    db.record_host_goal_review_delivery(
        OWNER,
        &binding,
        "first",
        Some(&context.context_epoch),
        &[],
        &scopes,
        &scopes[1..],
        NOW,
    )
    .unwrap();
    let bad = verdict(&first, &scopes, false);
    let rejected = db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &first.manifest,
            &bad,
            NOW,
        )
        .unwrap();
    assert_eq!(rejected.rounds.len(), 1);
    assert!(!rejected.rounds[0].passed);
    let second = packet(&binding, &context, "second", 'b', true);
    db.prepare_host_goal_review(OWNER, &binding, &second, NOW)
        .unwrap();
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &first.manifest,
            &good,
            NOW
        )
        .is_err());
    let scopes = deliver_all(&db, &second);
    let good = verdict(&second, &scopes, true);
    let accepted = db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "second",
            Some(&context.context_epoch),
            &second.manifest,
            &good,
            NOW,
        )
        .unwrap();
    assert_eq!(accepted.rounds.len(), 2);
    assert_eq!(accepted.rounds[0], rejected.rounds[0]);
    let duplicate = db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "second",
            Some(&context.context_epoch),
            &second.manifest,
            &good,
            NOW,
        )
        .unwrap();
    assert_eq!(duplicate.rounds.len(), 2);
    let completed = db
        .check_host_goal_node_with_request_in_context(
            OWNER,
            &binding,
            goal.revision,
            &good,
            &good,
            "finish",
            "finish",
            NOW,
            Some(&context),
        )
        .unwrap();
    assert_eq!(completed.goal.phase, HostGoalPhase::Completed);
}

#[test]
fn host_goal_review_first_pass_finishes_once_and_wrong_source_or_restatement_is_rejected() {
    let (_dir, db, _goal, binding, context) = fixture(true, 3);
    let value = packet(&binding, &context, "first", 'a', true);
    db.prepare_host_goal_review(OWNER, &binding, &value, NOW)
        .unwrap();
    let mut impostor = page(&value, 1);
    impostor.render_source_sha256 = Some("f".repeat(64));
    assert!(db
        .record_host_goal_review_delivery(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &[impostor],
            &["report.pdf#page:1".into()],
            &["report.pdf#page:1".into()],
            NOW
        )
        .is_err());
    let scopes = deliver_all(&db, &value);
    let mut good = verdict(&value, &scopes, true);
    good[0].evidence = "passed=true".into();
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &value.manifest,
            &good,
            NOW
        )
        .is_err());
    good[0].evidence = "已检查".repeat(30);
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &value.manifest,
            &good,
            NOW
        )
        .is_err());
    good = verdict(&value, &scopes, true);
    let changed = vec![GoalArtifactSnapshot {
        path: "report.pdf".into(),
        sha256: Some("b".repeat(64)),
    }];
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &changed,
            &good,
            NOW
        )
        .is_err());
    let mut wrong_evidence = good.clone();
    wrong_evidence[0].artifacts.clear();
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &value.manifest,
            &wrong_evidence,
            NOW
        )
        .is_err());
    let record = db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &value.manifest,
            &good,
            NOW,
        )
        .unwrap();
    assert_eq!(record.rounds.len(), 1);
    assert!(record.rounds[0].passed);
    let mut changed_verdict = good.clone();
    changed_verdict[0].evidence.push_str(" altered");
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &value.manifest,
            &changed_verdict,
            NOW
        )
        .is_err());
}

#[test]
fn host_goal_review_round_limit_and_restart_preserve_all_unresolved_findings() {
    let (dir, db, goal, binding, context) = fixture(true, 2);
    for (id, hash) in [("first", 'a'), ("second", 'b')] {
        let value = packet(&binding, &context, id, hash, true);
        db.prepare_host_goal_review(OWNER, &binding, &value, NOW)
            .unwrap();
        let scopes = deliver_all(&db, &value);
        db.submit_host_goal_review(
            OWNER,
            &binding,
            id,
            Some(&context.context_epoch),
            &value.manifest,
            &verdict(&value, &scopes, false),
            NOW,
        )
        .unwrap();
    }
    let third = packet(&binding, &context, "third", 'c', true);
    assert!(db
        .prepare_host_goal_review(OWNER, &binding, &third, NOW)
        .is_err());
    drop(db);
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let record = db
        .read_host_goal_review(OWNER, &goal.goal_id, &binding.node_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.rounds.len(), 2);
    assert_eq!(record.max_rounds, 2);
    assert!(record
        .rounds
        .iter()
        .all(|round| !round.passed && !round.evidence[0].findings.is_empty()));
    assert!(db
        .prepare_host_goal_review(OWNER, &binding, &third, NOW)
        .is_err());
    assert!(db
        .read_host_goal_review(OTHER, &goal.goal_id, &binding.node_id)
        .is_err());
}

#[test]
fn host_goal_review_new_attempt_and_epoch_preserve_rounds_but_require_new_image_delivery() {
    let (_dir, db, goal, binding, context) = fixture(true, 3);
    let first = packet(&binding, &context, "first", 'a', true);
    db.prepare_host_goal_review(OWNER, &binding, &first, NOW)
        .unwrap();
    let scopes = deliver_all(&db, &first);
    db.submit_host_goal_review(
        OWNER,
        &binding,
        "first",
        Some(&context.context_epoch),
        &first.manifest,
        &verdict(&first, &scopes, false),
        NOW,
    )
    .unwrap();
    let claim = super::super::HostGoalClaimMutation {
        goal,
        attempt: db
            .get_host_goal_attempt(OWNER, &binding.goal_id, &binding.attempt_id)
            .unwrap()
            .unwrap(),
        replayed: false,
    };
    let handed = managed_handoff(&db, &claim, &context, "handoff-review", &[]).unwrap();
    let next_context = HostGoalInvocationContext {
        context_epoch: "review-chat-b".into(),
        identity_source: "window_bound".into(),
    };
    db.observe_host_context(
        OWNER,
        &next_context.context_epoch,
        "initial",
        0,
        false,
        Some((3000, 4000)),
    )
    .unwrap();
    db.ensure_host_context_identity(OWNER, &next_context)
        .unwrap();
    let committed = managed_takeover(&db, &handed.goal.goal_id, &next_context);
    let next = managed_claim(&db, &committed, &next_context, "claim-b");
    let second = packet(&next.attempt.binding, &next_context, "second", 'a', true);
    let record = db
        .prepare_host_goal_review(OWNER, &next.attempt.binding, &second, NOW)
        .unwrap();
    assert_eq!(record.rounds.len(), 1);
    assert!(record
        .current_packet
        .as_ref()
        .unwrap()
        .delivered_scopes
        .is_empty());
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &next.attempt.binding,
            "second",
            Some(&next_context.context_epoch),
            &second.manifest,
            &verdict(&second, &scopes, true),
            NOW
        )
        .is_err());
    assert!(db
        .record_host_goal_review_delivery(
            OWNER,
            &next.attempt.binding,
            "second",
            Some(&context.context_epoch),
            &[page(&second, 1)],
            &scopes,
            &scopes[..1],
            NOW
        )
        .is_err());
    deliver_all(&db, &second);
    let record = db
        .submit_host_goal_review(
            OWNER,
            &next.attempt.binding,
            "second",
            Some(&next_context.context_epoch),
            &second.manifest,
            &verdict(&second, &scopes, true),
            NOW,
        )
        .unwrap();
    assert_eq!(record.rounds.len(), 2);
}

#[test]
fn host_goal_review_actual_layout_edit_invalidates_pass_without_mutating_plan() {
    let (_dir, db, goal, binding, context) = fixture(false, 3);
    let first = packet(&binding, &context, "first", 'a', false);
    db.prepare_host_goal_review(OWNER, &binding, &first, NOW)
        .unwrap();
    let good = verdict(&first, &first.required_scopes, true);
    db.submit_host_goal_review(
        OWNER,
        &binding,
        "first",
        Some(&context.context_epoch),
        &first.manifest,
        &good,
        NOW,
    )
    .unwrap();
    let record = db
        .enforce_host_goal_layout_review(OWNER, &binding, "new-report.pdf", NOW)
        .unwrap();
    assert_eq!(record.review, 1);
    assert!(record.current_packet.is_none());
    assert_eq!(record.rounds.len(), 1);
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .plan_hash,
        goal.plan_hash
    );
    assert!(db
        .check_host_goal_node_with_request_in_context(
            OWNER,
            &binding,
            goal.revision,
            &good,
            &good,
            "finish",
            "finish",
            NOW,
            Some(&context)
        )
        .is_err());
    let second = packet(&binding, &context, "second", 'b', false);
    assert!(
        db.prepare_host_goal_review(OWNER, &binding, &second, NOW)
            .is_err(),
        "actual managed delivery cannot be omitted from new packet"
    );
}

fn lightweight_plan() -> HostGoalPlan {
    let mut value = plan();
    let criterion = GoalCriterion::FileContains {
        id: "analysis".into(),
        description: "Exact replacement is present".into(),
        path: "source.txt".into(),
        text: "replacement".into(),
    };
    value.nodes[0].title = "Replace exact text".into();
    value.nodes[0].work = "Replace one known low-risk phrase".into();
    value.nodes[0].delivery_kind = Some(HostGoalDeliveryKind::Lightweight);
    value.nodes[0].review = Some(0);
    value.nodes[0].acceptance = vec![criterion.clone()];
    value.final_criteria = vec![criterion];
    value
}

#[test]
fn host_goal_review_zero_keeps_machine_checks_without_a_model_round() {
    for passed in [false, true] {
        let (_dir, db, goal, binding, context) = fixture_plan(lightweight_plan());
        let mut machine = evidence(passed);
        machine[0].source = HostGoalEvidenceSource::Automated;
        machine[0].review_packet_id = None;
        machine[0].reviewed_scopes.clear();
        let result = db
            .check_host_goal_node_with_request_in_context(
                OWNER,
                &binding,
                goal.revision,
                &machine,
                if passed { &machine } else { &[] },
                "machine",
                "machine",
                NOW,
                Some(&context),
            )
            .unwrap();
        assert_eq!(
            result.goal.phase,
            if passed {
                HostGoalPhase::Completed
            } else {
                HostGoalPhase::Blocked
            }
        );
        assert!(db
            .read_host_goal_review(OWNER, &goal.goal_id, &binding.node_id)
            .unwrap()
            .is_none());
    }
}

#[test]
fn host_goal_review_actual_layout_cannot_bypass_a_lightweight_plan() {
    let (_dir, db, goal, binding, context) = fixture_plan(lightweight_plan());
    let forced = db
        .enforce_host_goal_layout_review(OWNER, &binding, "chart.webp", NOW)
        .unwrap();
    assert_eq!(forced.review, 1);
    let mut machine = evidence(true);
    machine[0].source = HostGoalEvidenceSource::Automated;
    assert!(db
        .check_host_goal_node_with_request_in_context(
            OWNER,
            &binding,
            goal.revision,
            &machine,
            &machine,
            "bypass",
            "bypass",
            NOW,
            Some(&context)
        )
        .is_err());
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .plan_hash,
        goal.plan_hash
    );
}

#[test]
fn host_goal_review_superseded_packets_are_archived_and_new_render_dependencies_need_preparation() {
    let (_dir, db, goal, binding, context) = fixture(true, 3);
    let first = packet(&binding, &context, "first", 'a', true);
    db.prepare_host_goal_review(OWNER, &binding, &first, NOW)
        .unwrap();
    deliver_all(&db, &first);
    let mut changed = page(&first, 1);
    changed.render_dependencies_sha256 = Some("e".repeat(64));
    assert!(db
        .record_host_goal_review_delivery(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &[changed],
            &["report.pdf#page:1".into()],
            &["report.pdf#page:1".into()],
            NOW
        )
        .is_err());
    let second = packet(&binding, &context, "second", 'a', true);
    let record = db
        .prepare_host_goal_review(OWNER, &binding, &second, NOW)
        .unwrap();
    assert!(record.rounds.is_empty());
    assert_eq!(record.superseded_packets.len(), 1);
    assert_eq!(record.superseded_packets[0].delivered_scopes.len(), 2);
    assert!(db
        .record_host_goal_review_delivery(
            OWNER,
            &binding,
            "first",
            Some(&context.context_epoch),
            &[],
            &[],
            &[],
            NOW
        )
        .is_err());
    assert!(db
        .prepare_host_goal_review(OWNER, &binding, &first, NOW)
        .is_err());
    assert_eq!(
        db.read_host_goal_review(OWNER, &goal.goal_id, &binding.node_id)
            .unwrap()
            .unwrap()
            .superseded_packets,
        record.superseded_packets
    );
}

#[test]
fn host_goal_review_requirement_or_recorded_round_limit_cannot_be_downgraded_by_replanning() {
    let (_dir, db, goal, binding, context) = fixture(false, 3);
    let first = packet(&binding, &context, "first", 'a', false);
    db.prepare_host_goal_review(OWNER, &binding, &first, NOW)
        .unwrap();
    let reduced = lightweight_plan();
    assert!(db
        .save_host_goal_plan_in_context(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &reduced,
            Some("Change plan"),
            "downgrade",
            NOW,
            Some(&context)
        )
        .is_err());
    let mut changed_limit = plan();
    changed_limit.nodes[0].review_max_rounds = Some(5);
    assert!(db
        .save_host_goal_plan_in_context(
            OWNER,
            &goal.goal_id,
            goal.revision,
            &changed_limit,
            None,
            "reset-limit",
            NOW,
            Some(&context)
        )
        .is_err());
    assert_eq!(
        db.read_host_goal_review(OWNER, &goal.goal_id, &binding.node_id)
            .unwrap()
            .unwrap()
            .max_rounds,
        3
    );
    assert_eq!(
        db.get_host_goal(OWNER, &goal.goal_id)
            .unwrap()
            .unwrap()
            .plan_hash,
        goal.plan_hash
    );
}

fn state_packet(
    goal: &HostGoalRecord,
    binding: &HostGoalExecutionBinding,
    context: &HostGoalInvocationContext,
    id: &str,
) -> HostGoalReviewPacket {
    use sha2::{Digest, Sha256};
    let content = serde_json::to_string(&host_goal_review_state_material(&goal.state)).unwrap();
    let fingerprint = format!("{:x}", Sha256::digest(content.as_bytes()));
    HostGoalReviewPacket {
        packet_id: id.into(),
        binding: binding.clone(),
        context_epoch: Some(context.context_epoch.clone()),
        manifest: vec![],
        state_sha256: Some(fingerprint.clone()),
        machine_checks: vec![],
        materials: vec![HostGoalReviewMaterial {
            id: "goal-state".into(),
            path: "goal-state".into(),
            sha256: fingerprint,
            kind: "state".into(),
            preview_path: None,
            preview_sha256: None,
            render_source_sha256: None,
            render_dependencies_sha256: None,
            content: Some(content),
        }],
        required_scopes: vec!["goal-state".into()],
        delivered_scopes: vec!["goal-state".into()],
        created_at: NOW,
    }
}

#[test]
fn host_goal_review_persisted_text_uses_state_fingerprint_without_fabricating_file_artifacts() {
    let (_dir, db, goal, binding, context) = fixture(false, 3);
    let empty = state_packet(&goal, &binding, &context, "empty");
    assert!(db
        .prepare_host_goal_review(OWNER, &binding, &empty, NOW)
        .is_err());
    let mut state = goal.state.clone();
    state.completed_summary = "The documented synchronous mode returns a complete result; streaming instead reports incremental progress. The cited primary documentation supports this comparison.".into();
    let saved = db
        .checkpoint_host_goal_with_request_in_context(
            OWNER,
            &goal.goal_id,
            goal.revision,
            Some(&binding),
            &state,
            HostGoalCheckpointAction::Progress,
            "Persist actual research result",
            "state-a",
            "state-a",
            &[],
            NOW,
            Some(&context),
            &[],
        )
        .unwrap()
        .goal;
    let first = state_packet(&saved, &binding, &context, "state-first");
    db.prepare_host_goal_review(OWNER, &binding, &first, NOW)
        .unwrap();
    let mut good = evidence(true);
    good[0].artifacts.clear();
    good[0].review_packet_id = Some(first.packet_id.clone());
    good[0].reviewed_scopes = vec!["goal-state".into()];
    good[0].evidence="goal-state: The persisted comparison distinguishes complete synchronous results from incremental streaming, and its scope agrees with the cited documentation.".into();
    let mut copied = good.clone();
    copied[0].evidence = first.materials[0].content.clone().unwrap();
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            &first.packet_id,
            Some(&context.context_epoch),
            &[],
            &copied,
            NOW
        )
        .unwrap_err()
        .message()
        .contains("copied prepared material"));
    let mut revised_state = saved.state.clone();
    revised_state.facts.insert(
        "limitation".into(),
        "Streaming reports progress; it does not independently verify result quality.".into(),
    );
    let changed = db
        .checkpoint_host_goal_with_request_in_context(
            OWNER,
            &goal.goal_id,
            saved.revision,
            Some(&binding),
            &revised_state,
            HostGoalCheckpointAction::Progress,
            "Persist a newly discovered limitation",
            "state-b",
            "state-b",
            &[],
            NOW,
            Some(&context),
            &[],
        )
        .unwrap()
        .goal;
    assert!(db
        .submit_host_goal_review(
            OWNER,
            &binding,
            &first.packet_id,
            Some(&context.context_epoch),
            &[],
            &good,
            NOW
        )
        .is_err());
    let second = state_packet(&changed, &binding, &context, "state-second");
    db.prepare_host_goal_review(OWNER, &binding, &second, NOW)
        .unwrap();
    good[0].review_packet_id = Some(second.packet_id.clone());
    good[0].evidence.push_str(
        " The new limitation correctly separates progress reporting from result validation.",
    );
    let accepted = db
        .submit_host_goal_review(
            OWNER,
            &binding,
            &second.packet_id,
            Some(&context.context_epoch),
            &[],
            &good,
            NOW,
        )
        .unwrap();
    assert_eq!(accepted.rounds.len(), 1);
    assert_eq!(accepted.superseded_packets.len(), 1);
    assert!(accepted.rounds[0].packet.manifest.is_empty());
    let completed = db
        .check_host_goal_node_with_request_in_context(
            OWNER,
            &binding,
            changed.revision,
            &good,
            &good,
            "state-finish",
            "state-finish",
            NOW,
            Some(&context),
        )
        .unwrap()
        .goal;
    assert_eq!(completed.phase, HostGoalPhase::Completed);
    assert_eq!(
        host_goal_review_state_material(&completed.state),
        host_goal_review_state_material(&changed.state)
    );
    assert_ne!(completed.state.next_step, changed.state.next_step);
}
