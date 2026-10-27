use crate::team_decision::*;
use crate::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

struct Fixture {
    temp: tempfile::TempDir,
    db: Database,
    manager: VerifiedAgentRun,
    member: VerifiedAgentRun,
    root: String,
    next: u32,
}
fn principal() -> CommunicationPrincipal {
    let digest =
        Sha256::digest(b"webcodex.communication.principal.v1\0managed-user\0decision-owner");
    CommunicationPrincipal {
        kind: "managed-user".into(),
        digest: format!("wc_commprincipal_{digest:x}"),
    }
}
fn task(db: &Database, name: &str) -> String {
    db.create_agent_task(
        &principal(),
        NewAgentTask {
            title: name.into(),
            instruction: "Review public evidence".into(),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some("decision-project".into()),
            idempotency_key: name.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}
fn enroll(
    db: &Database,
    task: &str,
    source: Option<&VerifiedAgentRun>,
    key: &str,
) -> VerifiedAgentRun {
    let now = chrono::Utc::now().timestamp_millis();
    let mut grant = db
        .issue_agent_enrollment(
            NewAgentEnrollment {
                purpose: if source.is_some() {
                    AgentEnrollmentPurpose::Member
                } else {
                    AgentEnrollmentPurpose::Manager
                },
                owner_user: "decision-owner".into(),
                project: "decision-project".into(),
                principal_key: "decision-host".into(),
                task_id: task.into(),
                task_directory: format!("workspace/{task}"),
                brief: "Review public evidence".into(),
                inputs: vec![],
                room_ids: vec![],
                policy: AgentEnrollmentPolicy {
                    max_subagents: if source.is_some() { 0 } else { 4 },
                    max_cli_calls: 0,
                },
                expires_at_unix_ms: now + 60_000,
            },
            source,
            key,
            now,
        )
        .unwrap();
    db.claim_agent_enrollment(
        "decision-host",
        &format!("window-{key}"),
        &grant.enrollment_id,
        &grant.grant.take().unwrap().into_secret(),
        now,
    )
    .unwrap()
    .binding
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(&temp.path().join("state.db")).unwrap();
        let root = task(&db, "root");
        let manager = enroll(&db, &root, None, "manager");
        let child = task(&db, "child");
        let member = enroll(&db, &child, Some(&manager), "member");
        Self {
            temp,
            db,
            manager,
            member,
            root,
            next: 0,
        }
    }
    fn request(&mut self, mut value: Value) -> TeamDecisionRequest {
        value["root_task_id"] = json!(self.root);
        if value["action"] != "read" {
            self.next += 1;
            value["idempotency_key"] = json!(format!("key-{}", self.next));
        }
        if value.get("issue_id").is_some()
            && value["action"] != "read"
            && value.get("expected_revision").is_none()
        {
            value["expected_revision"] =
                self.read(value["issue_id"].as_str().unwrap())["summary"]["revision"].clone();
        }
        serde_json::from_value(value).unwrap()
    }
    fn send(&mut self, value: Value) -> Value {
        let request = self.request(value);
        self.db.team_decision(&self.manager, request).unwrap()
    }
    fn reject(&mut self, value: Value, code: &str) {
        let request = self.request(value);
        assert_eq!(
            self.db
                .team_decision(&self.manager, request)
                .unwrap_err()
                .code(),
            code
        );
    }
    fn read(&self, id: &str) -> Value {
        self.db
            .team_decision(
                &self.manager,
                serde_json::from_value(
                    json!({"action":"read","root_task_id":self.root,"issue_id":id}),
                )
                .unwrap(),
            )
            .unwrap()
    }
    fn open(&mut self) -> String {
        self.send(json!({"action":"open","question":"The current implementation supports X","context":"revision abc, test environment","kind":"fact"}))["summary"]["issue_id"].as_str().unwrap().into()
    }
    fn submit(&mut self, id: &str, origin: &str, stance: &str) -> String {
        self.send(json!({"action":"submit","issue_id":id,"stance":stance,"claim":"Observed result","source":{"origin":{"locator":origin,"version":"v1"},"location":"experiment 1","excerpt":"observed result"}}))["evidence_id"].as_str().unwrap().into()
    }
    fn verify(&mut self, id: &str, e: &str, origin: &str) -> Value {
        self.send(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"verified","rationale":"Read original; applicable to this version and environment; independent origin inspected","origin":{"locator":origin,"version":"v1"}}}))
    }
    fn conclude(&mut self, id: &str, status: &str) -> Value {
        self.send(json!({"action":"conclude","issue_id":id,"status":status,"selected_stance":"support","conclusion":"Result","rationale":"Explains evidence and tradeoffs"}))
    }
}

#[test]
fn team_decision_shared_sources_and_derivatives_count_once_across_authors() {
    let mut f = Fixture::new();
    let id = f.open();
    let a = f.submit(&id, "original", "support");
    f.verify(&id, &a, "original");
    let req=f.request(json!({"action":"submit","issue_id":id,"stance":"support","claim":"Another author repeats it","source":{"origin":{"locator":"repost","version":"v2"},"upstream":{"locator":"original","version":"v1"},"location":"quote","excerpt":"observed result"}}));
    let b = f.db.team_decision(&f.member, req).unwrap()["evidence_id"]
        .as_str()
        .unwrap()
        .to_string();
    f.verify(&id, &b, "original");
    let c = f.read(&id)["summary"]["counts"].clone();
    assert_eq!(c["support_groups"], 1);
    assert_eq!(c["duplicate_references"], 1);
    f.conclude(&id, "verified");
    let body=f.db.team_decision(&f.manager,serde_json::from_value(json!({"action":"read","root_task_id":f.root,"issue_id":id,"section":"evidence","limit":1})).unwrap()).unwrap();
    assert_eq!(body["summary"]["counts"]["submitted"], 2);
    assert_eq!(body["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(body["next_offset"], 1);
}

#[test]
fn team_decision_explicit_merge_is_transitive_and_cycles_are_rejected() {
    let mut f = Fixture::new();
    let id = f.open();
    let a = f.submit(&id, "one", "support");
    f.verify(&id, &a, "one");
    let b = f.submit(&id, "two", "support");
    f.send(json!({"action":"review","issue_id":id,"evidence_id":b,"review":{"disposition":"verified","rationale":"Two quotes one","same_origin_as":a}}));
    let c = f.submit(&id, "three", "support");
    f.send(json!({"action":"review","issue_id":id,"evidence_id":c,"review":{"disposition":"verified","rationale":"Three quotes two","same_origin_as":b}}));
    assert_eq!(f.read(&id)["summary"]["counts"]["support_groups"], 1);
    f.reject(json!({"action":"review","issue_id":id,"evidence_id":a,"review":{"disposition":"verified","rationale":"Cycle","same_origin_as":c}}),"team_decision_origin_cycle");
    f.send(json!({"action":"review","issue_id":id,"evidence_id":a,"review":{"disposition":"invalid","rationale":"Original actually does not support this statement"}}));
    assert_eq!(f.read(&id)["summary"]["counts"]["support_groups"], 0);
    assert_eq!(f.read(&id)["summary"]["counts"]["pending"], 2);
}

#[test]
fn team_decision_merge_applies_to_every_citation_of_the_same_source() {
    let mut f = Fixture::new();
    let id = f.open();
    let a = f.submit(&id, "A", "support");
    f.verify(&id, &a, "A");
    let b = f.submit(&id, "B", "support");
    f.verify(&id, &b, "B");
    let b2 = f.submit(&id, "B", "support");
    f.verify(&id, &b2, "B");
    f.send(json!({"action":"review","issue_id":id,"evidence_id":b,"review":{"disposition":"verified","rationale":"B is a copy of A","same_origin_as":a}}));
    assert_eq!(f.read(&id)["summary"]["counts"]["support_groups"], 1);
    assert_eq!(f.read(&id)["summary"]["counts"]["duplicate_references"], 2);
}

#[test]
fn team_decision_old_runs_cannot_read_or_replay_but_successor_keeps_records() {
    let mut f = Fixture::new();
    let req = f.request(
        json!({"action":"open","question":"Fact","context":"Current version","kind":"fact"}),
    );
    let first = f.db.team_decision(&f.manager, req.clone()).unwrap();
    let pending = f.db.begin_agent_run_handoff(&f.manager).unwrap();
    let successor =
        f.db.activate_agent_run(
            "decision-owner",
            "decision-project",
            f.manager.agent_id(),
            &pending.run_id,
        )
        .unwrap();
    assert!(f.db.team_decision(&f.manager, req.clone()).is_err());
    let read: TeamDecisionRequest = serde_json::from_value(
        json!({"action":"read","root_task_id":f.root,"issue_id":first["summary"]["issue_id"]}),
    )
    .unwrap();
    assert!(f.db.team_decision(&f.manager, read.clone()).is_err());
    assert_eq!(
        f.db.team_decision(&successor, read).unwrap()["summary"],
        first["summary"]
    );
    assert_eq!(f.db.team_decision(&successor, req).unwrap(), first);
}

#[test]
fn team_decision_independent_experiments_require_reviewed_units() {
    let mut f = Fixture::new();
    let id = f.open();
    let a = f.submit(&id, "report", "support");
    let b = f.submit(&id, "report", "support");
    f.verify(&id, &a, "report");
    f.verify(&id, &b, "report");
    assert_eq!(f.read(&id)["summary"]["counts"]["support_groups"], 1);
    f.reject(json!({"action":"review","issue_id":id,"evidence_id":a,"review":{"disposition":"verified","rationale":"Separate experiment","origin":{"locator":"report","version":"v1"},"independent_unit":"experiment A"}}),"team_decision_independence_required");
    for (e, unit) in [(&a, "experiment A"), (&b, "experiment B")] {
        f.send(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"verified","rationale":"Read separate experimental results","origin":{"locator":"report","version":"v1"},"independent_unit":unit,"independence_rationale":"Different independent measurements, separately located in the source"}}));
    }
    assert_eq!(f.read(&id)["summary"]["counts"]["support_groups"], 2);
}

#[test]
fn team_decision_wrong_citations_opinions_and_pending_sources_cannot_verify() {
    let mut f = Fixture::new();
    let id = f.open();
    for _ in 0..3 {
        let e = f.submit(&id, "wrong-report", "support");
        f.send(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"invalid","rationale":"Original contradicts the quoted claim"}}));
    }
    let e=f.send(json!({"action":"submit","issue_id":id,"stance":"support","claim":"I agree without evidence"}))["evidence_id"].as_str().unwrap().to_string();
    f.reject(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"verified","rationale":"Agreement","origin":{"locator":"opinion","version":"v1"}}}),"team_decision_source_required");
    f.reject(json!({"action":"conclude","issue_id":id,"status":"verified","selected_stance":"support","conclusion":"Three votes","rationale":"Three authors"}),"team_decision_verification_required");
    assert_eq!(
        f.conclude(&id, "insufficient")["summary"]["current_conclusion"]["status"],
        "insufficient"
    );
}

#[test]
fn team_decision_counterevidence_and_mixed_interpretations_are_not_outvoted() {
    let mut f = Fixture::new();
    let id = f.open();
    for _ in 0..3 {
        let e = f.submit(&id, "report", "support");
        f.verify(&id, &e, "report");
    }
    let against = f.submit(&id, "runtime-observation", "oppose");
    f.verify(&id, &against, "runtime-observation");
    f.reject(json!({"action":"conclude","issue_id":id,"status":"verified","selected_stance":"support","conclusion":"Majority","rationale":"Three agree"}),"team_decision_verification_required");
    f.conclude(&id, "disputed");
    f.send(json!({"action":"review","issue_id":id,"evidence_id":against,"review":{"disposition":"verified","rationale":"Runtime observation inspected","origin":{"locator":"runtime-observation","version":"v1"},"resolution":"The observation uses a different explicitly documented mode; the scoped claim remains supported"}}));
    f.conclude(&id, "verified");
    let mixed = f.submit(&id, "report", "oppose");
    f.verify(&id, &mixed, "report");
    assert_eq!(f.read(&id)["summary"]["counts"]["conflicting_groups"], 1);
    f.reject(json!({"action":"conclude","issue_id":id,"status":"verified","selected_stance":"support","conclusion":"Still supported","rationale":"Ignoring interpretation conflict"}),"team_decision_verification_required");
}

#[test]
fn team_decision_revision_replay_and_restart_keep_history_and_invalidate_conclusions() {
    let mut f = Fixture::new();
    let id = f.open();
    let e = f.submit(&id, "report", "support");
    f.verify(&id, &e, "report");
    f.conclude(&id, "verified");
    let before = f.read(&id);
    let req = f
        .request(json!({"action":"submit","issue_id":id,"stance":"oppose","claim":"New question"}));
    let first = f.db.team_decision(&f.manager, req.clone()).unwrap();
    let again = f.db.team_decision(&f.manager, req.clone()).unwrap();
    assert_eq!(first, again);
    let mut changed = serde_json::to_value(req).unwrap();
    changed["claim"] = json!("Changed intent");
    assert_eq!(
        f.db.team_decision(&f.manager, serde_json::from_value(changed).unwrap())
            .unwrap_err()
            .code(),
        "team_decision_idempotency_conflict"
    );
    f.reject(json!({"action":"conclude","issue_id":id,"expected_revision":before["summary"]["revision"],"status":"verified","selected_stance":"support","conclusion":"Stale","rationale":"Old evidence"}),"team_decision_revision_conflict");
    let now = f.read(&id);
    assert!(now["summary"]["current_conclusion"].is_null());
    assert_eq!(now["summary"]["last_conclusion"]["status"], "verified");
    let reopened = Database::open(&f.temp.path().join("state.db")).unwrap();
    let after = reopened
        .team_decision(
            &f.manager,
            serde_json::from_value(
                json!({"action":"read","root_task_id":f.root,"issue_id":id,"section":"history"}),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(after["summary"], now["summary"]);
    assert_eq!(after["history"].as_array().unwrap().len(), 5);
    assert!(after.to_string().find("idempotency_key").is_none());
}

#[test]
fn team_decision_choices_link_facts_and_stale_facts_invalidate_choice() {
    let mut f = Fixture::new();
    let fact = f.open();
    let e = f.submit(&fact, "benchmark", "support");
    f.verify(&fact, &e, "benchmark");
    let choice=f.send(json!({"action":"open","question":"Choose implementation A","context":"Minimize latency within budget","kind":"choice"}))["summary"]["issue_id"].as_str().unwrap().to_string();
    f.reject(json!({"action":"conclude","issue_id":choice,"status":"verified","selected_stance":"support","conclusion":"A","rationale":"Preference"}),"team_decision_fact_required");
    f.reject(json!({"action":"conclude","issue_id":choice,"status":"selected","selected_stance":"support","conclusion":"A","rationale":"Measured tradeoff","factual_issue_ids":[fact]}),"team_decision_required_fact_unverified");
    f.conclude(&fact, "verified");
    f.send(json!({"action":"conclude","issue_id":choice,"status":"selected","selected_stance":"support","conclusion":"A","rationale":"Meets budget and latency goal","factual_issue_ids":[fact]}));
    f.submit(&fact, "new-benchmark", "oppose");
    assert!(f.read(&choice)["summary"]["current_conclusion"].is_null());
}

#[test]
fn team_decision_task_scope_roles_and_server_authorship_are_enforced() {
    let mut f = Fixture::new();
    let id = f.open();
    let other = task(&f.db, "other");
    let outsider = enroll(&f.db, &other, None, "outsider");
    let read = f.request(json!({"action":"read","issue_id":id}));
    assert!(f.db.team_decision(&outsider, read.clone()).is_err());
    assert!(f.db.team_decision(&f.member, read).is_ok());
    let e = f.submit(&id, "report", "support");
    let review=f.request(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"invalid","rationale":"Only manager reviews"}}));
    assert!(f.db.team_decision(&f.member, review).is_err());
    let mut forged = json!({"action":"submit","root_task_id":f.root,"issue_id":id,"expected_revision":2,"stance":"support","claim":"forged","idempotency_key":"forged","author_agent_id":outsider.agent_id()});
    assert!(serde_json::from_value::<TeamDecisionRequest>(forged.clone()).is_err());
    forged.as_object_mut().unwrap().remove("author_agent_id");
    let request: TeamDecisionRequest = serde_json::from_value(forged).unwrap();
    f.db.team_decision(&f.member, request).unwrap();
    let body = f.send(json!({"action":"read","issue_id":id,"section":"evidence"}));
    assert_eq!(body["evidence"][1]["author_agent_id"], f.member.agent_id());
}

#[test]
fn team_decision_import_checks_room_exact_version_hash_and_quote() {
    let mut f = Fixture::new();
    let id = f.open();
    let room =
        f.db.create_agent_meeting(
            "decision-project",
            &[f.manager.agent_id().into()],
            "Published evidence",
        )
        .unwrap();
    let bytes = b"observed result, published".to_vec();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let publication =
        f.db.publish_agent_meeting_entry(
            &f.manager,
            &room,
            NewAgentMeetingEntry {
                kind: AgentMeetingKind::Artifact,
                content: "Explicit publication".into(),
                artifact_id: None,
                artifact_bytes: Some(bytes),
                review_target: None,
                idempotency_key: "publication".into(),
            },
        )
        .unwrap();
    let artifact = publication.artifact.unwrap();
    let input = json!({"action":"submit","issue_id":id,"stance":"support","claim":"Artifact evidence","source":{"origin":{"locator":"report","version":"v1"},"location":"line 1","excerpt":"observed result"},"artifact":{"room_id":room,"artifact_id":artifact.artifact_id,"version":artifact.version,"sha256":digest}});
    let mut wrong = input.clone();
    wrong["artifact"]["sha256"] = json!("0".repeat(64));
    f.reject(wrong, "team_decision_artifact_mismatch");
    let mut wrong = input.clone();
    wrong["source"]["excerpt"] = json!("invented quotation");
    f.reject(wrong, "team_decision_artifact_mismatch");
    let req = f.request(input.clone());
    assert_eq!(
        f.db.team_decision(&f.member, req).unwrap_err().code(),
        "team_decision_import_manager_only"
    );
    let e = f.send(input)["evidence_id"].as_str().unwrap().to_string();
    f.verify(&id, &e, "report");
    let body = f.send(json!({"action":"read","issue_id":id,"section":"evidence"}));
    assert_eq!(body["evidence"][0]["publication_checked"], true);
    assert_eq!(
        body["evidence"][0]["review"]["input"]["disposition"],
        "verified"
    );
}

#[test]
fn team_decision_completion_is_separate_from_execution_and_freezes_summary() {
    let mut f = Fixture::new();
    let start =
        f.db.start_agent_public_task(&f.manager, &f.root, "start")
            .unwrap();
    let id=f.send(json!({"action":"open","question":"Required factual acceptance","context":"Current build","kind":"fact","required_for_success":true}))["summary"]["issue_id"].as_str().unwrap().to_string();
    f.conclude(&id, "insufficient");
    assert_eq!(
        f.db.complete_agent_public_task(
            &f.manager,
            &f.root,
            &start.execution.execution_id,
            start.execution.revision,
            AgentTaskState::Succeeded,
            Some("Done"),
            None,
            "complete"
        )
        .unwrap_err()
        .code(),
        "team_decision_required_fact_unverified"
    );
    let e = f.submit(&id, "test", "support");
    f.verify(&id, &e, "test");
    f.conclude(&id, "verified");
    let done =
        f.db.complete_agent_public_task(
            &f.manager,
            &f.root,
            &start.execution.execution_id,
            start.execution.revision,
            AgentTaskState::Succeeded,
            Some("Done"),
            None,
            "complete",
        )
        .unwrap();
    assert_eq!(done.execution.decision_reviews.len(), 1);
    let replay =
        f.db.complete_agent_public_task(
            &f.manager,
            &f.root,
            &start.execution.execution_id,
            start.execution.revision,
            AgentTaskState::Succeeded,
            Some("Done"),
            None,
            "complete",
        )
        .unwrap();
    assert_eq!(done.execution, replay.execution);
    let req =
        f.request(json!({"action":"submit","issue_id":id,"stance":"oppose","claim":"Too late"}));
    assert!(f.db.team_decision(&f.member, req).is_err());
    assert!(f.read(&id)["summary"]["current_conclusion"].is_object());
}

#[test]
fn team_decision_no_issue_and_inconclusive_investigation_can_complete() {
    for register in [false, true] {
        let mut f = Fixture::new();
        let start =
            f.db.start_agent_public_task(&f.manager, &f.root, "start")
                .unwrap();
        if register {
            let id = f.open();
            f.conclude(&id, "insufficient");
        }
        let done =
            f.db.complete_agent_public_task(
                &f.manager,
                &f.root,
                &start.execution.execution_id,
                start.execution.revision,
                AgentTaskState::Succeeded,
                Some("Investigation complete"),
                None,
                "complete",
            )
            .unwrap();
        assert_eq!(done.execution.decision_reviews.len(), usize::from(register));
    }
}

#[test]
fn team_decision_plan_projection_and_fingerprint_dedup_use_full_records() {
    let mut f = Fixture::new();
    f.db.register_agent_team_plan(
        &f.manager,
        &f.root,
        0,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: f.root.clone(),
                cli_calls: vec![],
            },
            subagents: vec![],
        },
        "plan",
    )
    .unwrap();
    assert!(f
        .db
        .read_agent_team_plan(&f.manager, &f.root, 0, 1)
        .unwrap()
        .decision_reviews
        .is_empty());
    let id = f.open();
    let digest = format!("sha256:{}", "a".repeat(64));
    for location in ["original-url", "mirror-url"] {
        let e=f.send(json!({"action":"submit","issue_id":id,"stance":"support","claim":"Same content","source":{"origin":{"locator":location,"version":digest},"location":"line 1","excerpt":"observation"}}))["evidence_id"].as_str().unwrap().to_string();
        f.send(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"verified","rationale":"Original content inspected","origin":{"locator":location,"version":digest}}}));
    }
    let plan =
        f.db.read_agent_team_plan(&f.manager, &f.root, 0, 1)
            .unwrap();
    assert_eq!(plan.decision_reviews[0].counts.support_groups, 1);
    assert_eq!(plan.decision_reviews[0].counts.duplicate_references, 1);
}

#[test]
fn team_decision_required_proposition_refuted_is_not_successful_acceptance() {
    let mut f = Fixture::new();
    let start =
        f.db.start_agent_public_task(&f.manager, &f.root, "start")
            .unwrap();
    let id=f.send(json!({"action":"open","question":"Acceptance requires X","context":"Current build","kind":"fact","required_for_success":true}))["summary"]["issue_id"].as_str().unwrap().to_string();
    let e = f.submit(&id, "test", "oppose");
    f.verify(&id, &e, "test");
    f.send(json!({"action":"conclude","issue_id":id,"status":"verified","selected_stance":"oppose","conclusion":"X is refuted","rationale":"Direct counterexample"}));
    assert_eq!(
        f.db.complete_agent_public_task(
            &f.manager,
            &f.root,
            &start.execution.execution_id,
            start.execution.revision,
            AgentTaskState::Succeeded,
            Some("Done"),
            None,
            "complete"
        )
        .unwrap_err()
        .code(),
        "team_decision_required_fact_unverified"
    );
    let failed =
        f.db.complete_agent_public_task(
            &f.manager,
            &f.root,
            &start.execution.execution_id,
            start.execution.revision,
            AgentTaskState::Failed,
            None,
            Some("Acceptance is not met"),
            "failed",
        )
        .unwrap();
    assert_eq!(
        failed.execution.decision_reviews[0]
            .current_conclusion
            .as_ref()
            .unwrap()
            .selected_stance,
        EvidenceStance::Oppose
    );
}

#[test]
fn team_decision_split_experiments_do_not_add_an_overlapping_report_vote() {
    let mut f = Fixture::new();
    let id = f.open();
    let generic = f.submit(&id, "report", "support");
    f.verify(&id, &generic, "report");
    for unit in ["page 1 experiment A", "page 2 experiment B"] {
        let e = f.submit(&id, "report", "support");
        f.send(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"verified","rationale":"Located independent experiment","origin":{"locator":"report","version":"v1"},"independent_unit":unit,"independence_rationale":"Independent measurements documented at these locations"}}));
    }
    let c = f.read(&id)["summary"]["counts"].clone();
    assert_eq!(c["support_groups"], 2);
    assert_eq!(c["pending"], 1);
    f.reject(json!({"action":"conclude","issue_id":id,"status":"verified","selected_stance":"support","conclusion":"Supported","rationale":"Overlapping report ignored"}),"team_decision_verification_required");
    f.send(json!({"action":"review","issue_id":id,"evidence_id":generic,"review":{"disposition":"excluded","rationale":"Broad citation overlaps the two located experiments"}}));
    f.conclude(&id, "verified");
}

#[test]
fn team_decision_plain_shared_revision_does_not_merge_distinct_sources() {
    let mut f = Fixture::new();
    let id = f.open();
    let revision = "b".repeat(64);
    for locator in ["file-A", "file-B"] {
        let e=f.send(json!({"action":"submit","issue_id":id,"stance":"support","claim":"Independent observation","source":{"origin":{"locator":locator,"version":revision},"location":"experiment","excerpt":"Observation"}}))["evidence_id"].as_str().unwrap().to_string();
        f.send(json!({"action":"review","issue_id":id,"evidence_id":e,"review":{"disposition":"verified","rationale":"Different independent data at this revision","origin":{"locator":locator,"version":revision}}}));
    }
    assert_eq!(f.read(&id)["summary"]["counts"]["support_groups"], 2);
}

#[test]
fn team_decision_report_alias_cannot_escape_overlapping_experiment_dedup() {
    let mut f = Fixture::new();
    let id = f.open();
    let original = f.submit(&id, "original-report", "support");
    f.verify(&id, &original, "original-report");
    let mirror = f.submit(&id, "mirror-report", "support");
    f.verify(&id, &mirror, "mirror-report");
    let mirror2 = f.submit(&id, "mirror-report", "support");
    f.send(json!({"action":"review","issue_id":id,"evidence_id":mirror2,"review":{"disposition":"verified","rationale":"Mirror copies original","same_origin_as":original}}));
    f.send(json!({"action":"review","issue_id":id,"evidence_id":original,"review":{"disposition":"verified","rationale":"Located independent experiment","origin":{"locator":"original-report","version":"v1"},"independent_unit":"page 1 experiment A","independence_rationale":"Separate independent measurement"}}));
    let c = f.read(&id)["summary"]["counts"].clone();
    assert_eq!(c["support_groups"], 1);
    assert_eq!(c["pending"], 1);
    assert_eq!(c["duplicate_references"], 1);
}
