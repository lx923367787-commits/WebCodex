use super::*;
use webcodex_store::HostGoalCheckpointAction;

const OWNER: &str = "handoff-control-owner";
const PROJECT: &str = "handoff-control-project";
const PRIVATE: &str = "synthetic-private-content-must-not-leak";

fn bootstrap() -> AuthContext {
    let mut auth = AuthContext::new(AuthKind::Bootstrap);
    auth.is_bootstrap = true;
    auth
}

struct Fixture {
    _directory: tempfile::TempDir,
    db: Database,
    agent_id: String,
    goal_id: String,
    handoff_id: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db = Database::open(&directory.path().join("state.db")).unwrap();
        let grant = db.create_isolated_agent(OWNER, PROJECT, None).unwrap();
        let run = db.authenticate_agent_run(&grant.token).unwrap();
        db.register_agent_host_window(&"a".repeat(64), &"b".repeat(64), &run)
            .unwrap();
        let context = db.ensure_agent_host_context(&run, None).unwrap();
        let owner = run.host_goal_owner_key();
        let now = chrono::Utc::now().timestamp_millis();
        let goal = db
            .create_host_goal_at_in_context(
                &owner,
                PROJECT,
                "agent-private://test",
                PRIVATE,
                "control-create",
                now,
                Some(&context),
            )
            .unwrap()
            .goal;
        let mut state = goal.state.clone();
        state.next_step = PRIVATE.into();
        let goal = db
            .checkpoint_host_goal_with_request_in_context(
                &owner,
                &goal.goal_id,
                goal.revision,
                None,
                &state,
                HostGoalCheckpointAction::Handoff,
                "checkpoint",
                "control-handoff",
                "request-hash",
                &[],
                now,
                Some(&context),
                &[],
            )
            .unwrap()
            .goal;
        let h = db
            .read_host_goal_handoff(&owner, &goal.goal_id)
            .unwrap()
            .unwrap();
        Self {
            _directory: directory,
            db,
            agent_id: run.agent_id().into(),
            goal_id: goal.goal_id,
            handoff_id: h.handoff_id,
        }
    }

    fn body(&self, action: &str, attempt: Option<&str>) -> Value {
        json!({"action":action,"owner_user":OWNER,"project":PROJECT,"agent_id":self.agent_id,
            "goal_id":self.goal_id,"handoff_id":self.handoff_id,"expected_attempt_id":attempt})
    }

    fn call(&self, body: Value) -> Result<Value, String> {
        try_control(&self.db, &bootstrap(), &body).unwrap()
    }
}

#[test]
fn agent_handoff_control_failed_recovery_is_operator_only_and_metadata_only() {
    let f = Fixture::new();
    let begun = f.call(f.body("handoff_begin_attempt", None)).unwrap();
    let attempt = begun["handoff"]["attempt_id"].as_str().unwrap();
    f.call(f.body("handoff_abort", Some(attempt))).unwrap();
    let source = match f
        .db
        .resolve_agent_host_window(
            &"a".repeat(64),
            &"b".repeat(64),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
        .unwrap()
    {
        webcodex_store::AgentHostWindowBinding::Active(source) => source,
        _ => panic!("source must remain the same current run"),
    };
    let revision =
        f.db.read_host_goal_for_agent_run(&source, &f.goal_id)
            .unwrap()
            .0
            .revision;
    let body = json!({"action":"handoff_recover_failed","owner_user":OWNER,"project":PROJECT,
        "agent_id":f.agent_id,"goal_id":f.goal_id,"handoff_id":f.handoff_id,
        "expected_source_run_id":source.run_id(),"expected_source_generation":source.generation(),
        "expected_goal_revision":revision,"expected_attempt_id":attempt,"idempotency_key":"explicit-recovery"});
    for kind in [AuthKind::ApiToken, AuthKind::ModelAgent] {
        let mut auth = AuthContext::new(kind);
        auth.scopes = vec!["admin".into()];
        assert_eq!(
            try_control(&f.db, &auth, &body).unwrap().unwrap_err(),
            "trusted_control_required"
        );
    }
    // Fail actual broker publication after the Store transaction has committed.
    // This blocker is a file we create in this fixture's own temporary root.
    let blocker = f.db.agent_private_root();
    assert!(blocker.starts_with(f._directory.path()));
    assert!(!blocker.exists());
    std::fs::write(&blocker, b"synthetic publication blocker").unwrap();
    assert_eq!(
        f.call(body.clone()).unwrap_err(),
        "goal_handoff_publication_incomplete"
    );
    let saved =
        f.db.read_host_goal_handoff(&source.host_goal_owner_key(), &f.goal_id)
            .unwrap()
            .unwrap();
    let saved_revision =
        f.db.read_host_goal_for_agent_run(&source, &f.goal_id)
            .unwrap()
            .0
            .revision;
    assert_ne!(saved.handoff_id, f.handoff_id);
    std::fs::remove_file(&blocker).unwrap();
    let result = f.call(body.clone()).unwrap();
    assert_eq!(result["execution_granted"], false);
    assert_eq!(result["replayed"], true);
    assert_eq!(result["publication"], "verified");
    assert_eq!(result["handoff"]["handoff_id"], saved.handoff_id);
    assert_eq!(
        result["handoff"]["checkpoint_sequence"],
        saved.checkpoint_sequence
    );
    assert_eq!(result["goal_revision"], saved_revision);
    assert_eq!(result["goal"]["goal_id"], f.goal_id);
    assert_eq!(result["source_context"]["lifecycle_state"], "draining");
    assert_eq!(result["handoff"]["attempt_count"], 1);
    assert_eq!(result["previous_handoff_id"], f.handoff_id);
    assert_ne!(result["handoff"]["handoff_id"], f.handoff_id);
    let replay = f.call(body).unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["publication"], "verified");
    assert_eq!(
        replay["handoff"]["handoff_id"],
        result["handoff"]["handoff_id"]
    );
    // Real pending prepare must verify the newly published private files.
    let begun = f.call(json!({"action":"handoff_begin_attempt","owner_user":OWNER,"project":PROJECT,
        "agent_id":f.agent_id,"goal_id":f.goal_id,"handoff_id":saved.handoff_id,"expected_attempt_id":null})).unwrap();
    let attempt = begun["handoff"]["attempt_id"].as_str().unwrap();
    f.db.bind_handoff_host_window(
        &"a".repeat(64),
        &"c".repeat(64),
        &f.goal_id,
        &saved.handoff_id,
        attempt,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    let prepared = crate::agent_goal_handoff::call_host(
        &f.db,
        &"a".repeat(64),
        &"c".repeat(64),
        json!({"action":"prepare","goal_id":f.goal_id,"handoff_id":saved.handoff_id,
            "attempt_id":attempt,"checkpoint_sequence":saved.checkpoint_sequence}),
    );
    assert!(prepared.success, "prepare rejected: {:?}", prepared.error);
    for output in [result, replay] {
        let text = output.to_string();
        assert!(!text.contains(PRIVATE));
        assert!(!text.contains("run_token"));
        assert!(!text.contains("claim_grant"));
        assert!(!text.contains("checkpoint_path"));
        assert!(!text.contains("handoff_artifacts"));
    }
}

#[test]
fn agent_handoff_control_requires_bootstrap_and_does_not_intercept_other_actions() {
    let f = Fixture::new();
    let mut ordinary = AuthContext::new(AuthKind::ApiToken);
    ordinary.scopes = vec!["admin".into()];
    assert_eq!(
        try_control(&f.db, &ordinary, &f.body("handoff_status", None))
            .unwrap()
            .unwrap_err(),
        "trusted_control_required"
    );
    assert!(try_control(&f.db, &ordinary, &json!({"action":"generation"})).is_none());
    assert!(
        try_control(&f.db, &bootstrap(), &json!({"action":"handoff_unknown"}))
            .unwrap()
            .is_err()
    );
}

#[test]
fn agent_handoff_control_list_and_status_are_metadata_only_and_exactly_owned() {
    let f = Fixture::new();
    let listed = f
        .call(json!({"action":"handoff_list","owner_user":OWNER,"project":PROJECT,"limit":100}))
        .unwrap();
    assert_eq!(listed["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(listed["candidates"][0]["agent_id"], f.agent_id);
    let state = f.call(f.body("handoff_status", None)).unwrap();
    assert_eq!(state["goal"]["goal_id"], f.goal_id);
    assert_eq!(state["source_context"]["lifecycle_state"], "draining");
    assert_eq!(state["handoff"]["status"], "waiting");
    assert_eq!(state["execution_granted"], false);
    for value in [listed, state] {
        let rendered = value.to_string();
        assert!(!rendered.contains(PRIVATE));
        assert!(!rendered.contains("run_token"));
        assert!(!rendered.contains("agent-private://"));
        assert!(!rendered.contains("pending_agent_run_id"));
    }
    let mut wrong = f.body("handoff_status", None);
    wrong["owner_user"] = json!("different-owner");
    assert!(f.call(wrong).is_err());
    let mut wrong = f.body("handoff_status", None);
    wrong["project"] = json!("different-project");
    assert!(f.call(wrong).is_err());
}

#[test]
fn agent_handoff_control_repeated_reservation_and_stale_abort_preserve_current_attempt() {
    let f = Fixture::new();
    let first = f.call(f.body("handoff_begin_attempt", None)).unwrap();
    let id = first["handoff"]["attempt_id"].as_str().unwrap();
    assert_eq!(first["handoff"]["attempt_count"], 1);
    let duplicate = f.call(f.body("handoff_begin_attempt", None)).unwrap();
    assert_eq!(duplicate["handoff"]["attempt_id"], id);
    let mut failed = f.body("handoff_fail_attempt", Some(id));
    failed["reason"] = json!("launch_failed");
    f.call(failed).unwrap();
    let second = f.call(f.body("handoff_begin_attempt", Some(id))).unwrap();
    assert_eq!(second["handoff"]["attempt_count"], 2);
    let late = f.call(f.body("handoff_abort", Some(id))).unwrap();
    assert_eq!(late["handoff"]["status"], "awaiting_ready");
    assert_eq!(
        late["handoff"]["attempt_id"],
        second["handoff"]["attempt_id"]
    );
}

#[test]
fn agent_handoff_control_four_failures_stop_without_a_fifth_reservation() {
    let f = Fixture::new();
    let mut previous = None;
    for number in 1..=4 {
        let begun = f
            .call(f.body("handoff_begin_attempt", previous.as_deref()))
            .unwrap();
        assert_eq!(begun["handoff"]["attempt_count"], number);
        previous = Some(begun["handoff"]["attempt_id"].as_str().unwrap().to_owned());
        let mut failure = f.body("handoff_fail_attempt", previous.as_deref());
        failure["reason"] = json!("launch_failed");
        f.call(failure).unwrap();
    }
    let terminal = f
        .call(f.body("handoff_begin_attempt", previous.as_deref()))
        .unwrap();
    assert_eq!(terminal["handoff"]["status"], "failed");
    assert_eq!(terminal["handoff"]["attempt_count"], 4);
    assert_eq!(terminal["source_context"]["lifecycle_state"], "stopped");
    assert_eq!(terminal["goal"]["phase"], "blocked");
    assert!(terminal["notice"].as_str().unwrap().contains("已保留"));
}

#[test]
fn agent_handoff_control_rejects_unbounded_list_and_payload_or_reason_injection() {
    let f = Fixture::new();
    assert!(f
        .call(json!({"action":"handoff_list","owner_user":OWNER,"project":PROJECT,"limit":101}))
        .is_err());
    let mut request = f.body("handoff_status", None);
    request["run_token"] = json!("synthetic-no-access");
    assert!(f.call(request).is_err());
    let first = f.call(f.body("handoff_begin_attempt", None)).unwrap();
    let mut request = f.body("handoff_abort", first["handoff"]["attempt_id"].as_str());
    request["reason"] = json!(PRIVATE);
    assert_eq!(f.call(request).unwrap_err(), "invalid_handoff_failure_code");
    assert_eq!(
        f.call(f.body("handoff_status", None)).unwrap()["handoff"]["status"],
        "awaiting_ready"
    );
}

#[test]
fn agent_handoff_control_launch_claim_is_one_time_and_absent_from_all_metadata() {
    let f = Fixture::new();
    let begun = f.call(f.body("handoff_begin_attempt", None)).unwrap();
    let attempt = begun["handoff"]["attempt_id"].as_str().unwrap();
    assert!(!begun.to_string().contains("claim_grant"));
    let issued = f
        .call(f.body("handoff_launch_claim", Some(attempt)))
        .unwrap();
    let claim = issued["claim_grant"].as_str().unwrap();
    assert!(claim.starts_with("wc_hc_"));
    assert_eq!(issued["attempt_id"], attempt);
    assert_eq!(issued["expires_at_ms"], begun["handoff"]["deadline_at"]);
    assert_eq!(
        f.call(f.body("handoff_launch_claim", Some(attempt)))
            .unwrap_err(),
        "goal_handoff_claim_already_issued"
    );
    let status = f.call(f.body("handoff_status", Some(attempt))).unwrap();
    let listed = f
        .call(json!({"action":"handoff_list","owner_user":OWNER,"project":PROJECT}))
        .unwrap();
    for value in [begun, status, listed] {
        assert!(!value.to_string().contains(claim));
        assert!(!value.to_string().contains("claim_grant"));
        assert!(!value.to_string().contains("claim_hash"));
    }
    for entry in std::fs::read_dir(f._directory.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes
                .windows(claim.len())
                .any(|window| window == claim.as_bytes()));
        }
    }
}
