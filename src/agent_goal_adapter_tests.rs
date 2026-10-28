use crate::agent_goal_adapter::{call, supports};
use crate::agent_private_files::AgentPrivateFiles;
use crate::Database;
use serde_json::{json, Value};
use webcodex_core::host_goal::*;
use webcodex_store::{HostGoalCheckpointAction, VerifiedAgentRun};

fn fixture() -> (tempfile::TempDir, Database, VerifiedAgentRun) {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let grant = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let binding = db.authenticate_agent_run(&grant.token).unwrap();
    (temp, db, binding)
}
fn output(db: &Database, binding: &VerifiedAgentRun, name: &str, args: Value) -> Value {
    let result = call(db, binding, name, args);
    assert!(result.success, "{}", result.error.unwrap_or_default());
    result.output
}
fn plan() -> Value {
    let criterion =
        json!({"kind":"review","id":"evidence","description":"Sources support the conclusion"});
    json!({"objective":"Compare sources","deliverables":["Comparison"],"final_criteria":[criterion],
        "nodes":[{"id":"research","title":"Research","work":"Compare primary sources","outputs":["Comparison"],"criterion_ids":["evidence"],"acceptance":[criterion],"failure_action":"Record missing evidence"}],
        "state_management":{"facts":"Cite sources","decisions":"Explain choices","issues":"Track gaps","artifacts":"Save references","checkpoint_triggers":["After research"],"resume_instructions":"Read the exact current state"}})
}
fn start(db: &Database, binding: &VerifiedAgentRun) -> Value {
    output(
        db,
        binding,
        "goal_mode_start",
        json!({"objective":"Compare sources","idempotency_key":"start"}),
    )
}
fn publish(db: &Database, binding: &VerifiedAgentRun) -> Value {
    let started = start(db, binding);
    output(
        db,
        binding,
        "goal_plan_save",
        json!({"goal_id":started["goal"]["goal_id"],"expected_revision":1,"idempotency_key":"save","plan":plan()}),
    )
}
fn confirm(db: &Database, binding: &VerifiedAgentRun, published: &Value) -> Value {
    output(
        db,
        binding,
        "goal_confirm_plan",
        json!({"goal_id":published["goal"]["goal_id"],"expected_revision":published["goal"]["revision"],
        "plan_version":1,"plan_sha256":published["plan"]["hash"],"confirmation":"I read this plan; begin execution","idempotency_key":"confirm"}),
    )
}

#[test]
fn agent_goal_lifecycle_uses_private_publication_and_requires_exact_goal() {
    let (_temp, db, binding) = fixture();
    let published = publish(&db, &binding);
    assert_eq!(published["goal"]["phase"], "awaiting_confirmation");
    assert!(published["goal"]["project_root"]
        .as_str()
        .unwrap()
        .starts_with("agent-private://"));
    let private = AgentPrivateFiles::new(db.agent_private_root()).unwrap();
    let path = published["plan"]["materialization"]["json_path"]
        .as_str()
        .unwrap();
    assert!(path.starts_with("goals/"));
    let read = db
        .with_agent_run(&binding, "test_read", || {
            Ok(private.read(&binding, path).unwrap())
        })
        .unwrap();
    let saved: HostGoalPlan = serde_json::from_slice(&read.bytes).unwrap();
    assert_eq!(saved.objective, "Compare sources");
    let confirmed = confirm(&db, &binding, &published);
    assert_eq!(confirmed["goal"]["phase"], "ready");
    assert_eq!(call(&db,&binding,"goal_claim_node",json!({"goal_id":confirmed["goal"]["goal_id"],"expected_revision":confirmed["goal"]["revision"],
        "plan_version":1,"node_id":"research","idempotency_key":"claim"})).error.as_deref(),Some("goal_context_read_required"));
    let node = output(
        &db,
        &binding,
        "goal_read",
        json!({"goal_id":confirmed["goal"]["goal_id"],"view":"node","node_id":"research"}),
    );
    assert_eq!(node["context_read"]["complete"], true);
    let claimed = output(
        &db,
        &binding,
        "goal_claim_node",
        json!({"goal_id":confirmed["goal"]["goal_id"],"expected_revision":confirmed["goal"]["revision"],
        "plan_version":1,"node_id":"research","idempotency_key":"claim"}),
    );
    assert_eq!(claimed["workspace_write_allowed"], true);
    assert_eq!(claimed["execution_adapters_available"], false);
    assert!(claimed.get("execution_allowed").is_none());
    let checkpoint = output(
        &db,
        &binding,
        "goal_checkpoint",
        json!({"goal_id":claimed["goal"]["goal_id"],"expected_revision":claimed["goal"]["revision"],
        "idempotency_key":"checkpoint","patch":{"reason":"Saved sourced fact","state":{"facts":{"source":"primary"}}}}),
    );
    assert_eq!(checkpoint["goal"]["state"]["facts"]["source"], "primary");
    let bootstrap = output(
        &db,
        &binding,
        "goal_bootstrap",
        json!({"goal_id":checkpoint["goal"]["goal_id"]}),
    );
    assert_eq!(
        bootstrap["goal"]["revision"],
        checkpoint["goal"]["revision"]
    );
    assert_eq!(
        call(&db, &binding, "goal_bootstrap", json!({}))
            .error
            .as_deref(),
        Some("explicit_goal_id_required")
    );
    assert!(!supports("goal_check_node"));
    assert_eq!(
        call(&db, &binding, "goal_check_node", json!({}))
            .error
            .as_deref(),
        Some("execution_isolation_unavailable")
    );
}

#[test]
fn agent_goal_foreign_goal_project_context_and_modified_plan_fail_closed() {
    let (_temp, db, binding) = fixture();
    let published = publish(&db, &binding);
    let other = db
        .create_isolated_agent("owner-a", "project-a", Some(binding.agent_id()))
        .unwrap();
    let other = db.authenticate_agent_run(&other.token).unwrap();
    let id = published["goal"]["goal_id"].as_str().unwrap();
    assert_eq!(
        call(&db, &other, "goal_bootstrap", json!({"goal_id":id}))
            .error
            .as_deref(),
        Some("goal_not_found")
    );
    assert_eq!(
        call(
            &db,
            &binding,
            "goal_mode_start",
            json!({"project":"other","objective":"x","idempotency_key":"other"})
        )
        .error
        .as_deref(),
        Some("goal_project_mismatch")
    );
    assert_eq!(
        call(
            &db,
            &binding,
            "goal_bootstrap",
            json!({"goal_id":id,"context_epoch":"forged"})
        )
        .error
        .as_deref(),
        Some("agent_goal_identity_server_owned")
    );
    let private = AgentPrivateFiles::new(db.agent_private_root()).unwrap();
    let receipt = &published["plan"]["materialization"];
    db.with_agent_run(&binding, "test_modify", || {
        Ok(private
            .write(
                &binding,
                receipt["json_path"].as_str().unwrap(),
                b"modified",
                receipt["json_hash"].as_str(),
            )
            .unwrap())
    })
    .unwrap();
    let result = call(
        &db,
        &binding,
        "goal_confirm_plan",
        json!({"goal_id":id,"expected_revision":published["goal"]["revision"],"plan_version":1,
        "plan_sha256":published["plan"]["hash"],"confirmation":"begin","idempotency_key":"confirm"}),
    );
    assert_eq!(
        result.error.as_deref(),
        Some("goal_materialization_changed")
    );
}

#[test]
fn agent_goal_store_fences_revoked_run_before_exact_replay_and_every_state_write() {
    let (_temp, db, binding) = fixture();
    let published = publish(&db, &binding);
    let confirmed = confirm(&db, &binding, &published);
    let id = published["goal"]["goal_id"].as_str().unwrap();
    let owner = binding.host_goal_owner_key();
    let context = db.ensure_agent_host_context(&binding, None).unwrap();
    let parsed: HostGoalPlan = serde_json::from_value(plan()).unwrap();
    let (_, version, _) = db.read_host_goal_for_agent_run(&binding, id).unwrap();
    let version = version.unwrap();
    db.revoke_agent_run("owner-a", "project-a", binding.agent_id(), binding.run_id())
        .unwrap();
    let check = |result: Result<(), webcodex_store::GoalStoreError>| {
        assert_eq!(result.unwrap_err().code(), "agent_run_invalid")
    };
    check(
        db.create_host_goal_in_task_directory_in_context(
            &owner,
            "project-a",
            &format!("agent-private://{}", binding.agent_id()),
            "workspace",
            "Compare sources",
            "start",
            0,
            Some(&context),
        )
        .map(|_| ()),
    );
    check(
        db.save_host_goal_plan_in_context(&owner, id, 1, &parsed, None, "save", 0, Some(&context))
            .map(|_| ()),
    );
    check(
        db.materialize_host_goal_plan_in_context(
            &owner,
            id,
            1,
            &version.hash,
            version.materialization.as_ref().unwrap(),
            0,
            Some(&context),
        )
        .map(|_| ()),
    );
    check(
        db.confirm_host_goal_plan_in_context(
            &owner,
            id,
            published["goal"]["revision"].as_u64().unwrap(),
            1,
            &version.hash,
            "I read this plan; begin execution",
            "confirm",
            0,
            Some(&context),
        )
        .map(|_| ()),
    );
    check(
        db.claim_host_goal_node_for_plan_in_context(
            &owner,
            id,
            confirmed["goal"]["revision"].as_u64().unwrap(),
            "research",
            false,
            1,
            None,
            "claim",
            0,
            Some(&context),
        )
        .map(|_| ()),
    );
    check(
        db.checkpoint_host_goal_with_request_in_context(
            &owner,
            id,
            4,
            None,
            &HostGoalState::default(),
            HostGoalCheckpointAction::Progress,
            "checkpoint",
            "checkpoint",
            "request",
            &[],
            0,
            Some(&context),
            &[],
        )
        .map(|_| ()),
    );
    check(
        db.select_current_host_goal_in_context(&owner, id, 0, Some(&context))
            .map(|_| ()),
    );
    check(db.read_host_goal_for_agent_run(&binding, id).map(|_| ()));
    assert!(db.ensure_agent_host_context(&binding, None).is_err());
    assert_eq!(
        db.get_host_goal(&owner, id).unwrap().unwrap().revision,
        confirmed["goal"]["revision"].as_u64().unwrap()
    );
}

#[test]
fn agent_goal_recovery_preserves_logical_owner_and_existing_thresholds() {
    let (_temp, db, binding) = fixture();
    let started = output(
        &db,
        &binding,
        "goal_mode_start",
        json!({"project":"project-a","objective":"Compare sources","idempotency_key":"start",
        "context_warning_tokens":1000,"context_handoff_tokens":2000}),
    );
    db.ensure_agent_host_context(&binding, Some((9000, 12000)))
        .unwrap();
    let thresholds:(u64,u64)=db.conn_for_tests().query_row("SELECT warning_tokens,handoff_tokens FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2",
        [&binding.host_goal_owner_key(),binding.run_id()],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(thresholds, (1000, 2000));
    let warning = db
        .observe_agent_host_context(&binding, "event-one", 1500, false)
        .unwrap();
    assert_eq!(warning.level, "warning");
    let repeated = db
        .observe_agent_host_context(&binding, "event-one", 1500, false)
        .unwrap();
    assert_eq!(repeated.observed_tokens, 1500);
    let draining = db
        .observe_agent_host_context(&binding, "event-two", 600, false)
        .unwrap();
    assert_eq!(draining.level, "handoff_recommended");
    assert_eq!(
        call(
            &db,
            &binding,
            "goal_mode_start",
            json!({"project":"project-a","objective":"Another goal","idempotency_key":"other"})
        )
        .error
        .as_deref(),
        Some("goal_context_draining")
    );
    let grant = db
        .recover_isolated_agent("owner-a", "project-a", binding.agent_id(), 1)
        .unwrap();
    let next = db.authenticate_agent_run(&grant.token).unwrap();
    assert!(db
        .observe_agent_host_context(&binding, "late", 1, false)
        .is_err());
    let bootstrap = output(
        &db,
        &next,
        "goal_bootstrap",
        json!({"goal_id":started["goal"]["goal_id"]}),
    );
    assert_eq!(bootstrap["goal"]["goal_id"], started["goal"]["goal_id"]);
    assert_eq!(next.host_goal_owner_key(), binding.host_goal_owner_key());
}

#[test]
fn agent_goal_exit_pauses_and_unbinds_atomically_and_old_run_replay_is_denied() {
    use sha2::{Digest, Sha256};
    let (_temp, db, binding) = fixture();
    let started = start(&db, &binding);
    let id = started["goal"]["goal_id"].as_str().unwrap();
    let owner = binding.host_goal_owner_key();
    let before = db.get_host_goal(&owner, id).unwrap().unwrap();
    let bound = db.host_goal_window(&owner, binding.run_id()).unwrap();
    assert!(bound.is_some());
    assert_eq!(
        db.checkpoint_host_goal(
            &owner,
            id,
            before.revision,
            None,
            &before.state,
            HostGoalCheckpointAction::Exit,
            "Stop work",
            "no-context",
            0
        )
        .unwrap_err()
        .code(),
        "goal_context_missing"
    );
    let patch = json!({"action":"exit","reason":"Stop work"});
    let arguments = json!({"goal_id":id,"expected_revision":before.revision,"idempotency_key":"exit","patch":patch});
    db.conn_for_tests()
        .execute_batch(
            "CREATE TRIGGER fail_exit_unbind BEFORE DELETE ON wc_host_goal_windows
        BEGIN SELECT RAISE(ABORT,'synthetic exit unbind failure'); END;",
        )
        .unwrap();
    let failed = call(&db, &binding, "goal_checkpoint", arguments.clone());
    assert_eq!(failed.error.as_deref(), Some("goal_store_unavailable"));
    assert_eq!(db.get_host_goal(&owner, id).unwrap().unwrap(), before);
    assert_eq!(
        db.host_goal_window(&owner, binding.run_id()).unwrap(),
        bound
    );
    db.conn_for_tests()
        .execute_batch("DROP TRIGGER fail_exit_unbind;")
        .unwrap();
    let exited = output(&db, &binding, "goal_checkpoint", arguments.clone());
    assert_eq!(exited["goal"]["phase"], "paused");
    assert_eq!(
        exited["goal"]["revision"].as_u64(),
        Some(before.revision + 1)
    );
    assert_eq!(exited["goal"]["fence"].as_u64(), Some(before.fence + 1));
    assert!(db
        .host_goal_window(&owner, binding.run_id())
        .unwrap()
        .is_none());
    assert_eq!(
        db.latest_host_goal_checkpoint(&owner, id)
            .unwrap()
            .unwrap()
            .action
            .as_deref(),
        Some("exit")
    );
    let after_exit = db.get_host_goal(&owner, id).unwrap().unwrap();
    let receipts_before: i64 = db
        .conn_for_tests()
        .query_row(
            "SELECT COUNT(*) FROM wc_host_goal_checkpoint_requests WHERE goal_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    db.observe_agent_host_context(&binding, "drain-after-exit", 800_001, false)
        .unwrap();
    let drained_replay = call(&db, &binding, "goal_checkpoint", arguments.clone());
    assert_eq!(
        drained_replay.error.as_deref(),
        Some("goal_context_draining")
    );
    assert_eq!(drained_replay.output["state_changed"], false);
    assert_eq!(db.get_host_goal(&owner, id).unwrap().unwrap(), after_exit);
    assert!(db
        .host_goal_window(&owner, binding.run_id())
        .unwrap()
        .is_none());
    let receipts_after: i64 = db
        .conn_for_tests()
        .query_row(
            "SELECT COUNT(*) FROM wc_host_goal_checkpoint_requests WHERE goal_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts_before, receipts_after);
    let context = db.ensure_agent_host_context(&binding, None).unwrap();
    db.revoke_agent_run("owner-a", "project-a", binding.agent_id(), binding.run_id())
        .unwrap();
    let request_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(id, before.revision, &patch)).unwrap())
    );
    let replay = db
        .checkpoint_host_goal_with_request_in_context(
            &owner,
            id,
            before.revision,
            None,
            &before.state,
            HostGoalCheckpointAction::Exit,
            "Stop work",
            "exit",
            &request_hash,
            &[],
            0,
            Some(&context),
            &[],
        )
        .unwrap_err();
    assert_eq!(replay.code(), "agent_run_invalid");
    assert_eq!(
        call(&db, &binding, "goal_checkpoint", arguments)
            .error
            .as_deref(),
        Some("agent_run_invalid")
    );
}

#[test]
fn agent_goal_draining_exit_preserves_revision_fence_state_and_binding() {
    let (_temp, db, binding) = fixture();
    let started = start(&db, &binding);
    let id = started["goal"]["goal_id"].as_str().unwrap();
    let owner = binding.host_goal_owner_key();
    let before = db.get_host_goal(&owner, id).unwrap().unwrap();
    let bound = db.host_goal_window(&owner, binding.run_id()).unwrap();
    db.observe_agent_host_context(&binding, "drain-before-exit", 800_001, false)
        .unwrap();
    let result = call(
        &db,
        &binding,
        "goal_checkpoint",
        json!({"goal_id":id,"expected_revision":before.revision,"idempotency_key":"exit",
        "patch":{"action":"exit","reason":"Stop work","state":{"facts":{"must_not_commit":"true"}}}}),
    );
    assert_eq!(result.error.as_deref(), Some("goal_context_draining"));
    assert_eq!(result.output["state_changed"], false);
    assert_eq!(db.get_host_goal(&owner, id).unwrap().unwrap(), before);
    assert_eq!(
        db.host_goal_window(&owner, binding.run_id()).unwrap(),
        bound
    );
    assert!(db
        .latest_host_goal_checkpoint(&owner, id)
        .unwrap()
        .is_none());
    let receipts: i64 = db
        .conn_for_tests()
        .query_row(
            "SELECT COUNT(*) FROM wc_host_goal_checkpoint_requests WHERE goal_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 0);
}
