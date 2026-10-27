use super::*;
use crate::agent_goal_adapter;

const PRINCIPAL: &str = "trusted-oauth-principal";
const SOURCE: &str = "registered-source-window";
const CANDIDATE: &str = "registered-candidate-window";

struct Fixture {
    _dir: tempfile::TempDir,
    db: Database,
    source: VerifiedAgentRun,
    checkpoint_args: Value,
    published: Value,
    handoff: HostGoalHandoff,
}
fn success(result: ToolResult) -> Value {
    assert!(result.success, "{:?}: {}", result.error, result.output);
    result.output
}
impl Fixture {
    fn new() -> Self {
        Self::with_plan(false)
    }
    fn with_plan(save_plan: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("state.db")).unwrap();
        let grant = db.create_isolated_agent("owner", "project", None).unwrap();
        let source = db.authenticate_agent_run(&grant.token).unwrap();
        db.register_agent_host_window(PRINCIPAL, SOURCE, &source)
            .unwrap();
        let mut start = success(agent_goal_adapter::call(
            &db,
            &source,
            "goal_mode_start",
            json!({
                "project":"project","objective":"Continue authorized task","idempotency_key":"start"
            }),
        ));
        if save_plan {
            let criterion = json!({"kind":"review","id":"evidence","description":"Sources support the conclusion"});
            let plan = json!({"objective":"Continue authorized task","deliverables":["Comparison"],"final_criteria":[criterion],
                "nodes":[{"id":"research","title":"Research","work":"Compare primary sources","outputs":["Comparison"],
                    "criterion_ids":["evidence"],"acceptance":[criterion],"failure_action":"Record missing evidence"}],
                "state_management":{"facts":"Cite sources","decisions":"Explain choices","issues":"Track gaps","artifacts":"Save references",
                    "checkpoint_triggers":["After research"],"resume_instructions":"Read the exact current state"}});
            start = success(agent_goal_adapter::call(
                &db,
                &source,
                "goal_plan_save",
                json!({
                    "goal_id":start["goal"]["goal_id"],"expected_revision":start["goal"]["revision"],
                    "idempotency_key":"save-plan","plan":plan
                }),
            ));
        }
        let checkpoint_args = json!({"goal_id":start["goal"]["goal_id"],"expected_revision":start["goal"]["revision"],
            "idempotency_key":"handoff","patch":{"action":"handoff","reason":"Continue planning in a fresh window",
            "state":{"facts":{"progress":"Saved task progress"}}}});
        let published = success(agent_goal_adapter::call(
            &db,
            &source,
            "goal_checkpoint",
            checkpoint_args.clone(),
        ));
        let handoff = serde_json::from_value(published["handoff"].clone()).unwrap();
        Self {
            _dir: dir,
            db,
            source,
            checkpoint_args,
            published,
            handoff,
        }
    }
    fn begin(&mut self) {
        self.handoff = self
            .db
            .begin_host_goal_handoff_attempt(
                &self.source.host_goal_owner_key(),
                &self.handoff.goal_id,
                &self.handoff.handoff_id,
                self.handoff.attempt_id.as_deref(),
                now(),
                120,
            )
            .unwrap();
    }
    fn bind(&self) {
        self.db
            .bind_handoff_host_window(
                PRINCIPAL,
                CANDIDATE,
                &self.handoff.goal_id,
                &self.handoff.handoff_id,
                self.handoff.attempt_id.as_deref().unwrap(),
                now(),
            )
            .unwrap();
    }
    fn args(&self, action: &str) -> Value {
        json!({"action":action,"goal_id":self.handoff.goal_id,"handoff_id":self.handoff.handoff_id,
            "attempt_id":self.handoff.attempt_id,"checkpoint_sequence":self.handoff.checkpoint_sequence})
    }
    fn candidate(&self, args: Value) -> ToolResult {
        call_host(&self.db, PRINCIPAL, CANDIDATE, args)
    }
    fn issue_claim(&self, issued_at: i64) -> String {
        self.db
            .issue_agent_handoff_claim(
                &self.source.host_goal_owner_key(),
                &self.handoff.goal_id,
                &self.handoff.handoff_id,
                self.handoff.attempt_id.as_deref().unwrap(),
                issued_at,
            )
            .unwrap()
            .into_secret()
    }
    fn claim_args(&self, claim: &str) -> Value {
        let mut args = self.args("prepare");
        args["launch_claim"] = json!(claim);
        args
    }
}

#[test]
fn native_agent_handoff_saved_package_real_reads_and_commit_are_idempotent() {
    let mut f = Fixture::new();
    assert_eq!(f.handoff.status, "waiting");
    assert_eq!(f.published["publication"], "verified");
    assert_eq!(f.published["execution_granted"], false);
    assert!(f.db.validate_agent_run(&f.source).is_ok());
    let replay = success(agent_goal_adapter::call(
        &f.db,
        &f.source,
        "goal_checkpoint",
        f.checkpoint_args.clone(),
    ));
    assert_eq!(
        replay["handoff_artifacts"],
        f.published["handoff_artifacts"]
    );
    f.begin();
    assert_eq!(
        f.candidate(f.args("prepare")).error.as_deref(),
        Some("host_window_unregistered")
    );
    f.bind();
    let ready = success(f.candidate(f.args("prepare")));
    assert_eq!(ready["execution_granted"], false);
    assert_eq!(ready["verified_channels"], json!(["agent_private.read"]));
    assert_eq!(ready["execution_adapters_available"], false);
    assert_eq!(ready["package"]["goal"]["goal_id"], f.handoff.goal_id);
    assert_eq!(
        ready["package"]["checkpoint"]["state"]["facts"]["progress"],
        "Saved task progress"
    );
    assert!(ready["package"].get("attempt_id").is_none());
    assert!(f.db.validate_agent_run(&f.source).is_ok());
    let mut args = f.args("commit");
    args["package_sha256"] = ready["package_sha256"].clone();
    let committed = success(f.candidate(args.clone()));
    assert_eq!(committed["execution_granted"], true);
    assert!(f.db.validate_agent_run(&f.source).is_err());
    assert_eq!(success(f.candidate(args))["handoff"], committed["handoff"]);
    assert_eq!(
        success(f.candidate(f.args("status")))["execution_granted"],
        true
    );
    // Simulate an unavailable post-commit observation without rolling back the
    // real committed transfer or claiming that the mutation never happened.
    let receipt: HostGoalHandoff = serde_json::from_value(committed["handoff"].clone()).unwrap();
    let request: Request = serde_json::from_value(f.args("status")).unwrap();
    let uncertain =
        committed_status(&f.db, PRINCIPAL, "unavailable-window", &request, &receipt).unwrap_err();
    assert_eq!(uncertain.output["state_changed"], true);
    assert_eq!(uncertain.output["dispatch_certainty"], "outcome_unknown");
    assert!(uncertain.output["execution_granted"].is_null());
    assert_eq!(
        success(f.candidate(f.args("status")))["execution_granted"],
        true
    );
    let reopened = Database::open(&f._dir.path().join("state.db")).unwrap();
    assert_eq!(
        success(call_host(&reopened, PRINCIPAL, CANDIDATE, f.args("status")))["execution_granted"],
        true
    );
}

#[test]
fn native_agent_handoff_altered_package_neither_prepares_nor_overwrites_on_replay() {
    let mut f = Fixture::new();
    let private = files(&f.db).unwrap();
    let path = f.published["handoff_artifacts"]["json_path"]
        .as_str()
        .unwrap();
    // Trusted test corruption models disk tampering; normal broker writes to
    // package paths are unavailable to models.
    let original = private.read(&f.source, path).unwrap();
    private
        .write(&f.source, path, b"changed package", Some(&original.sha256))
        .unwrap();
    let replay = agent_goal_adapter::call(
        &f.db,
        &f.source,
        "goal_checkpoint",
        f.checkpoint_args.clone(),
    );
    assert!(!replay.success);
    assert_eq!(replay.output["checkpoint_saved"], true);
    assert_eq!(replay.output["state_changed"], true);
    assert_eq!(
        private.read(&f.source, path).unwrap().bytes,
        b"changed package"
    );
    f.begin();
    f.bind();
    assert_eq!(
        f.candidate(f.args("prepare")).error.as_deref(),
        Some("goal_handoff_materialization_changed")
    );
    assert_eq!(
        success(f.candidate(f.args("status")))["execution_granted"],
        false
    );
    assert!(f.db.validate_agent_run(&f.source).is_ok());
}

#[test]
fn native_agent_handoff_revoked_attempt_and_wrong_principal_cannot_prepare() {
    let mut f = Fixture::new();
    f.begin();
    f.bind();
    assert!(!call_host(&f.db, "other-principal", CANDIDATE, f.args("prepare")).success);
    let old = f.args("prepare");
    f.handoff =
        f.db.fail_host_goal_handoff_attempt(
            &f.source.host_goal_owner_key(),
            &f.handoff.goal_id,
            &f.handoff.handoff_id,
            f.handoff.attempt_id.as_deref().unwrap(),
            "simulated timeout",
            now(),
        )
        .unwrap();
    f.begin();
    assert!(!f.candidate(old).success);
    assert_eq!(f.handoff.attempt_count, 2);
    assert!(f.db.validate_agent_run(&f.source).is_ok());
}

#[test]
fn native_agent_handoff_rejects_identity_fields_and_direct_token_candidate_claims() {
    let mut f = Fixture::new();
    f.begin();
    f.bind();
    let mut spoofed = f.args("prepare");
    spoofed["run_id"] = json!(f.source.run_id());
    assert_eq!(
        f.candidate(spoofed).error.as_deref(),
        Some("invalid_goal_handoff")
    );
    assert_eq!(
        call_active(&f.db, &f.source, f.args("prepare"))
            .error
            .as_deref(),
        Some("goal_handoff_host_window_required")
    );
    assert_eq!(
        success(call_active(&f.db, &f.source, f.args("status")))["execution_granted"],
        false
    );
    let mut other = f.args("prepare");
    other["goal_id"] = json!("hgoal_other");
    assert!(!f.candidate(other).success);
}

#[test]
fn native_agent_handoff_reads_and_verifies_both_immutable_plan_files() {
    for receipt_field in ["json_path", "markdown_path"] {
        let mut f = Fixture::with_plan(true);
        let plan =
            f.db.get_host_goal_plan(&f.source.host_goal_owner_key(), &f.handoff.goal_id, Some(1))
                .unwrap()
                .unwrap();
        let receipt = serde_json::to_value(plan.materialization.unwrap()).unwrap();
        let path = receipt[receipt_field].as_str().unwrap();
        let private = files(&f.db).unwrap();
        let original = private.read(&f.source, path).unwrap();
        private
            .write(
                &f.source,
                path,
                b"tampered immutable plan",
                Some(&original.sha256),
            )
            .unwrap();
        f.begin();
        f.bind();
        assert_eq!(
            f.candidate(f.args("prepare")).error.as_deref(),
            Some("goal_handoff_materialization_changed"),
            "{receipt_field}"
        );
        assert!(f.db.validate_agent_run(&f.source).is_ok());
        assert_eq!(
            success(f.candidate(f.args("status")))["handoff"]["status"],
            "awaiting_ready"
        );
    }
}

#[test]
fn native_agent_handoff_commit_rechecks_prepared_files_before_granting() {
    let mut f = Fixture::with_plan(true);
    f.begin();
    f.bind();
    let ready = success(f.candidate(f.args("prepare")));
    let path = ready["package"]["plan"]["materialization"]["markdown_path"]
        .as_str()
        .unwrap();
    let private = files(&f.db).unwrap();
    let original = private.read(&f.source, path).unwrap();
    private
        .write(
            &f.source,
            path,
            b"changed after readiness",
            Some(&original.sha256),
        )
        .unwrap();
    let mut args = f.args("commit");
    args["package_sha256"] = ready["package_sha256"].clone();
    assert_eq!(
        f.candidate(args).error.as_deref(),
        Some("goal_handoff_materialization_changed")
    );
    assert!(f.db.validate_agent_run(&f.source).is_ok());
    assert_eq!(
        success(f.candidate(f.args("status")))["execution_granted"],
        false
    );
}

#[test]
fn native_agent_handoff_launch_claim_binds_only_pending_and_never_appears_in_results() {
    let mut f = Fixture::with_plan(true);
    f.begin();
    let claim = f.issue_claim(now());
    let args = f.claim_args(&claim);
    let ready = success(f.candidate(args.clone()));
    assert_eq!(ready["execution_granted"], false);
    assert!(!ready.to_string().contains(&claim));
    assert!(f.db.validate_agent_run(&f.source).is_ok());
    let replay = success(f.candidate(args.clone()));
    assert_eq!(ready, replay);
    assert!(!call_host(&f.db, PRINCIPAL, "other-candidate", args).success);
    let mut wrong_cap = f.claim_args("wc_hc_invalid");
    assert!(!f.candidate(wrong_cap.clone()).success);
    wrong_cap["launch_claim"] = json!(claim);
    for action in ["status", "commit"] {
        wrong_cap["action"] = json!(action);
        let result = f.candidate(wrong_cap.clone());
        assert_eq!(
            result.error.as_deref(),
            Some("goal_handoff_claim_prepare_only")
        );
        assert!(!result.output.to_string().contains(&claim));
    }
    let mut commit = f.args("commit");
    commit["package_sha256"] = ready["package_sha256"].clone();
    assert_eq!(success(f.candidate(commit))["execution_granted"], true);
    assert_eq!(
        success(f.candidate(f.args("status")))["execution_granted"],
        true
    );
    assert!(!f.candidate(f.claim_args(&claim)).success);
}

#[test]
fn native_agent_handoff_claim_rejects_fake_source_active_and_other_principal_without_consuming() {
    let mut f = Fixture::new();
    f.begin();
    let claim = f.issue_claim(now());
    assert!(
        !f.candidate(
            f.claim_args("wc_hc_0000000000000000000000000000000000000000000000000000000000000000")
        )
        .success
    );
    let other =
        f.db.create_isolated_agent("owner", "project", None)
            .unwrap();
    let other = f.db.authenticate_agent_run(&other.token).unwrap();
    f.db.register_agent_host_window(PRINCIPAL, "other-active", &other)
        .unwrap();
    for window in [SOURCE, "other-active"] {
        let result = call_host(&f.db, PRINCIPAL, window, f.claim_args(&claim));
        assert!(!result.success);
        assert!(!result.output.to_string().contains(&claim));
    }
    let wrong_principal = call_host(&f.db, "other-principal", CANDIDATE, f.claim_args(&claim));
    assert!(!wrong_principal.success);
    assert!(!wrong_principal.output.to_string().contains(&claim));
    assert_eq!(
        success(f.candidate(f.claim_args(&claim)))["handoff"]["status"],
        "prepared"
    );
}

#[test]
fn native_agent_handoff_expired_and_old_attempt_claims_cannot_bind() {
    let mut expired = Fixture::new();
    let past = now() - 10_000;
    expired.handoff = expired
        .db
        .begin_host_goal_handoff_attempt(
            &expired.source.host_goal_owner_key(),
            &expired.handoff.goal_id,
            &expired.handoff.handoff_id,
            None,
            past,
            1,
        )
        .unwrap();
    let claim = expired.issue_claim(past);
    assert!(!expired.candidate(expired.claim_args(&claim)).success);

    let mut f = Fixture::new();
    f.begin();
    let old_claim = f.issue_claim(now());
    let old_args = f.claim_args(&old_claim);
    f.handoff =
        f.db.fail_host_goal_handoff_attempt(
            &f.source.host_goal_owner_key(),
            &f.handoff.goal_id,
            &f.handoff.handoff_id,
            f.handoff.attempt_id.as_deref().unwrap(),
            "simulated failed launch",
            now(),
        )
        .unwrap();
    f.begin();
    assert!(!f.candidate(old_args).success);
    assert!(!f.candidate(f.claim_args(&old_claim)).success);
    let new_claim = f.issue_claim(now());
    assert_eq!(
        success(f.candidate(f.claim_args(&new_claim)))["handoff"]["attempt_count"],
        2
    );
}

#[test]
fn native_agent_handoff_concurrent_claim_windows_have_exactly_one_prepared_candidate() {
    let mut f = Fixture::new();
    f.begin();
    let claim = f.issue_claim(now());
    let db_path = f._dir.path().join("state.db");
    let second = Database::open(&db_path).unwrap();
    let args = f.claim_args(&claim);
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            call_host(&f.db, PRINCIPAL, "candidate-one", args.clone())
        });
        let other = scope.spawn(|| {
            barrier.wait();
            call_host(&second, PRINCIPAL, "candidate-two", args.clone())
        });
        [first.join().unwrap(), other.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.success).count(), 1);
    for result in results {
        assert!(!result.output.to_string().contains(&claim));
        if result.success {
            assert_eq!(result.output["execution_granted"], false);
        }
    }
    assert!(f.db.validate_agent_run(&f.source).is_ok());
}

#[test]
fn native_agent_handoff_failed_status_explicitly_reports_stop_and_preserved_recovery_state() {
    let mut f = Fixture::new();
    for _ in 0..4 {
        f.begin();
        f.handoff =
            f.db.fail_host_goal_handoff_attempt(
                &f.source.host_goal_owner_key(),
                &f.handoff.goal_id,
                &f.handoff.handoff_id,
                f.handoff.attempt_id.as_deref().unwrap(),
                "simulated failed launch",
                now(),
            )
            .unwrap();
    }
    assert_eq!(f.handoff.status, "failed");
    assert!(f.db.validate_agent_run(&f.source).is_ok());
    let status = success(call_active(&f.db, &f.source, f.args("status")));
    assert_eq!(status["execution_granted"], false);
    assert_eq!(status["handoff"]["attempt_count"], 4);
    assert_eq!(
        status["notice"],
        "交接失败，已停止执行，已保留可人工恢复的状态。只有用户明确要求恢复后才能重新启动。"
    );
    f.begin();
    assert_eq!(f.handoff.attempt_count, 4);
    assert_eq!(f.handoff.status, "failed");
}
