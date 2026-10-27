//! Post-commit task access through the production Agent broker and Goal adapter.
//! Only the server-side handoff readiness observation is supplied by this
//! fixture; it makes no claim about browser credential delivery or live CLI.
use crate::auth::{AuthContext, AuthKind};
use crate::tool_runtime::ToolResult;
use crate::Database;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webcodex_core::host_goal::HostGoalInvocationContext;
use webcodex_store::{HostGoalCheckpointAction, VerifiedAgentRun};

const PROJECT: &str = "handoff-project";
const TASK: &str = "workspace/task-one";
const OTHER_TASK: &str = "workspace/task-two";

fn success(result: ToolResult) -> Value {
    assert!(result.success, "{:?}: {}", result.error, result.output);
    result.output
}

fn auth(binding: &VerifiedAgentRun) -> AuthContext {
    let mut auth = AuthContext::new(AuthKind::ModelAgent);
    auth.agent_run = Some(binding.clone());
    auth
}

fn call(db: &Database, binding: &VerifiedAgentRun, name: &str, args: Value) -> ToolResult {
    crate::agent_isolation::call(db, name, args, Some(&auth(binding)))
}

fn plan() -> Value {
    let criterion =
        json!({"kind":"review","id":"evidence","description":"Saved work cites its source"});
    json!({"objective":"Continue the approved task","deliverables":["Saved sourced result"],
        "final_criteria":[criterion],
        "nodes":[{"id":"research","title":"Research","work":"Update the task artifact using its approved evidence",
            "outputs":["workspace/task-one/artifact.txt"],"criterion_ids":["evidence"],"acceptance":[criterion],
            "failure_action":"Preserve the checkpoint and explain missing evidence"}],
        "state_management":{"facts":"Keep sources","decisions":"Keep reasons","issues":"Keep blockers",
            "artifacts":"Preserve task files","checkpoint_triggers":["Before handoff"],
            "resume_instructions":"Read this Goal's node and continue its own workspace"}})
}

struct Fixture {
    db: Database,
    temp: tempfile::TempDir,
    source: VerifiedAgentRun,
    successor: VerifiedAgentRun,
    source_token: String,
    successor_token: String,
    goal_id: String,
    other_goal_id: String,
    plan_path: String,
    plan_hash: String,
    package_path: String,
    package_hash: String,
    meeting_room: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        Self::new_with_meeting(false)
    }

    fn new_with_meeting(include_meeting: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(&temp.path().join("state.db")).unwrap();
        let grant = db
            .create_isolated_agent("handoff-owner", PROJECT, None)
            .unwrap();
        let source = db.authenticate_agent_run(&grant.token).unwrap();
        let meeting_room = include_meeting.then(|| {
            let room = db
                .create_agent_meeting(
                    PROJECT,
                    &[source.agent_id().to_owned()],
                    "Existing authorized meeting",
                )
                .unwrap();
            success(call(
                &db,
                &source,
                "agent_meeting",
                json!({"action":"publish","room_id":room,
                "kind":"message","content":"Published before the source run was replaced",
                "idempotency_key":"source-meeting-message"}),
            ));
            room
        });
        // These are pre-existing user inputs, before this run enters Goal mode.
        // A planning/ready Goal must not be used to seed business workspace writes.
        for (path, content) in [
            (format!("{TASK}/artifact.txt"), "approved original evidence"),
            (
                format!("{OTHER_TASK}/artifact.txt"),
                "unrelated-result-sentinel",
            ),
            (
                "workspace/task-one-extra/hidden.txt".into(),
                "unrelated-prefix-sentinel",
            ),
            ("memory/private.txt".into(), "private-memory-sentinel"),
        ] {
            success(call(
                &db,
                &source,
                "agent_private",
                json!({"action":"write","path":path,"content":content}),
            ));
        }
        let started = success(call(
            &db,
            &source,
            "goal_mode_start",
            json!({
                "project":PROJECT,"task_directory":TASK,"objective":"Continue the approved task","idempotency_key":"task-one"
            }),
        ));
        let goal_id = started["goal"]["goal_id"].as_str().unwrap().to_owned();
        let published = success(call(
            &db,
            &source,
            "goal_plan_save",
            json!({
                "goal_id":goal_id,"expected_revision":started["goal"]["revision"],
                "idempotency_key":"publish-one","plan":plan()
            }),
        ));
        let plan_path = published["plan"]["materialization"]["json_path"]
            .as_str()
            .unwrap()
            .to_owned();
        let plan_hash = published["plan"]["materialization"]["json_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let confirmed = success(call(
            &db,
            &source,
            "goal_confirm_plan",
            json!({
                "goal_id":goal_id,"expected_revision":published["goal"]["revision"],"plan_version":1,
                "plan_sha256":published["plan"]["hash"],"confirmation":"I read and approve this exact task plan",
                "idempotency_key":"confirm-one"
            }),
        ));
        let other = success(call(
            &db,
            &source,
            "goal_mode_start",
            json!({
                "project":PROJECT,"task_directory":OTHER_TASK,"objective":"unrelated-private-goal-sentinel",
                "idempotency_key":"task-two"
            }),
        ));
        let other_goal_id = other["goal"]["goal_id"].as_str().unwrap().to_owned();
        success(call(
            &db,
            &source,
            "agent_private",
            json!({"action":"write",
            "path":format!("goals/{other_goal_id}/notes.txt"),"content":"other-goal-private-sentinel"}),
        ));

        // A trusted server transition prepares the existing Goal. Readiness is
        // deliberately supplied here so these tests remain independent of the
        // external browser-to-pending-credential delivery integration.
        let owner = source.host_goal_owner_key();
        let source_context = HostGoalInvocationContext {
            context_epoch: "server-source-window".into(),
            identity_source: "window_bound".into(),
        };
        db.ensure_host_context_identity(&owner, &source_context)
            .unwrap();
        let goal = db.get_host_goal(&owner, &goal_id).unwrap().unwrap();
        assert_eq!(
            goal.revision,
            confirmed["goal"]["revision"].as_u64().unwrap()
        );
        let mut state = goal.state.clone();
        state.completed_summary = "Approved source artifact is saved".into();
        state.artifact_refs = vec![format!("{TASK}/artifact.txt")];
        state.next_step = "Claim this Goal's node and update its own artifact".into();
        let now = chrono::Utc::now().timestamp_millis();
        db.checkpoint_host_goal_with_request_in_context(
            &owner,
            &goal_id,
            goal.revision,
            None,
            &state,
            HostGoalCheckpointAction::Handoff,
            "Continue the same task",
            "handoff-one",
            "handoff-request-one",
            &[],
            now,
            Some(&source_context),
            &[],
        )
        .unwrap();
        let h = db
            .read_host_goal_handoff(&owner, &goal_id)
            .unwrap()
            .unwrap();
        let checkpoint = db
            .read_host_goal_handoff_snapshot(&owner, &goal_id, &source_context.context_epoch)
            .unwrap()
            .checkpoint
            .unwrap();
        let package_path = format!(
            "handoff/{goal_id}/checkpoint-{}.json",
            h.checkpoint_sequence
        );
        let package = json!({"goal_id":goal_id,"project":PROJECT,"checkpoint":checkpoint,
            "plan_version":1,"plan_sha256":published["plan"]["hash"]})
        .to_string();
        let package_hash = format!("{:x}", Sha256::digest(package.as_bytes()));
        success(call(
            &db,
            &source,
            "agent_private",
            json!({"action":"write","path":package_path,"content":package}),
        ));
        let read = success(call(
            &db,
            &source,
            "agent_private",
            json!({"action":"read","path":package_path}),
        ));
        assert_eq!(read["sha256"], package_hash);
        let plan_read = success(call(
            &db,
            &source,
            "agent_private",
            json!({"action":"read","path":plan_path}),
        ));
        assert_eq!(plan_read["sha256"], plan_hash);
        let pending = db.begin_agent_run_handoff(&source).unwrap();
        assert!(db.authenticate_agent_run(&pending.token).is_err());
        let h = db
            .begin_host_goal_handoff_attempt(&owner, &goal_id, &h.handoff_id, None, now, 300)
            .unwrap();
        let candidate = HostGoalInvocationContext {
            context_epoch: "server-successor-window".into(),
            identity_source: "window_bound".into(),
        };
        db.ensure_host_context_identity(&owner, &candidate).unwrap();
        db.bind_host_goal_handoff_candidate(
            &owner,
            &goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            &candidate,
            now + 1,
        )
        .unwrap();
        db.record_host_goal_handoff_prepared(
            &owner,
            &goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            h.checkpoint_sequence,
            &package_hash,
            &candidate,
            now + 1,
        )
        .unwrap();
        db.commit_host_goal_handoff_ready_with_run(
            &owner,
            &goal_id,
            &h.handoff_id,
            h.attempt_id.as_deref().unwrap(),
            h.checkpoint_sequence,
            &package_hash,
            &candidate,
            now + 2,
            &source,
            &pending.run_id,
        )
        .unwrap();
        let successor = db.authenticate_agent_run(&pending.token).unwrap();
        assert_eq!(successor.agent_id(), source.agent_id());
        assert_ne!(successor.run_id(), source.run_id());
        assert_eq!(
            db.agent_run_task_goal(&successor).unwrap().as_deref(),
            Some(goal_id.as_str())
        );
        assert!(db.authenticate_agent_run(&grant.token).is_err());
        Self {
            db,
            temp,
            source,
            successor,
            source_token: grant.token,
            successor_token: pending.token,
            goal_id,
            other_goal_id,
            plan_path,
            plan_hash,
            package_path,
            package_hash,
            meeting_room,
        }
    }

    fn call(&self, name: &str, args: Value) -> ToolResult {
        call(&self.db, &self.successor, name, args)
    }

    fn private(&self, action: &str, path: &str, fields: Value) -> ToolResult {
        let mut args = json!({"action":action,"path":path});
        args.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        self.call("agent_private", args)
    }

    fn claim(&self) -> Value {
        self.claim_with_key("successor-claim")
    }

    fn claim_with_key(&self, key: &str) -> Value {
        let node = success(self.call(
            "goal_read",
            json!({"goal_id":self.goal_id,"view":"node","node_id":"research"}),
        ));
        assert_eq!(node["context_read"]["complete"], true);
        success(self.call(
            "goal_claim_node",
            json!({"goal_id":self.goal_id,"node_id":"research",
            "expected_revision":node["goal"]["revision"],"plan_version":1,"idempotency_key":key}),
        ))
    }

    fn server_path(&self, relative: &str) -> std::path::PathBuf {
        self.db
            .agent_private_root()
            .join("runtime/projects")
            .join(format!("{:x}", Sha256::digest(PROJECT.as_bytes())))
            .join("agents")
            .join(self.successor.agent_id())
            .join("private")
            .join(relative)
    }
}

#[test]
fn agent_handoff_access_real_successor_can_claim_and_continue_only_task_files() {
    let f = Fixture::new();
    let early = f.private(
        "write",
        &format!("{TASK}/early.txt"),
        json!({"content":"must not execute before claim"}),
    );
    assert!(!early.success);
    assert!(!f.server_path(&format!("{TASK}/early.txt")).exists());
    let claimed = f.claim();
    assert_eq!(claimed["goal"]["goal_id"], f.goal_id);
    assert_eq!(claimed["workspace_write_allowed"], true);
    assert_eq!(claimed["execution_adapters_available"], false);
    assert!(claimed.get("execution_allowed").is_none());
    let original = success(f.private("read", &format!("{TASK}/artifact.txt"), json!({})));
    assert_eq!(original["content"], "approved original evidence");
    let updated = success(f.private(
        "write",
        &format!("{TASK}/artifact.txt"),
        json!({"content":"approved successor evidence","expected_sha256":original["sha256"]}),
    ));
    assert_ne!(updated["sha256"], original["sha256"]);
    assert!(
        !f.private(
            "write",
            &format!("{TASK}/artifact.txt"),
            json!({"content":"stale overwrite","expected_sha256":original["sha256"]})
        )
        .success
    );
    success(f.private(
        "write",
        &format!("{TASK}/new.txt"),
        json!({"content":"approved new task output"}),
    ));
    let listed = success(f.private("list", TASK, json!({"limit":20})));
    assert!(listed.as_array().unwrap().iter().all(|entry| entry["path"]
        .as_str()
        .unwrap()
        .starts_with(&format!("{TASK}/"))));
    let package = success(f.private("read", &f.package_path, json!({})));
    assert_eq!(package["sha256"], f.package_hash);
    let checkpoint = success(f.call("goal_checkpoint", json!({"goal_id":f.goal_id,
        "expected_revision":claimed["goal"]["revision"],"idempotency_key":"successor-progress",
        "patch":{"reason":"Continued the same task using its scoped files","state":{"completed_summary":"Saved successor evidence"}}})));
    assert_eq!(checkpoint["goal"]["goal_id"], f.goal_id);
    assert_eq!(
        checkpoint["goal"]["state"]["completed_summary"],
        "Saved successor evidence"
    );
    let materialized = success(f.private(
        "read",
        checkpoint["checkpoint_path"].as_str().unwrap(),
        json!({}),
    ));
    let saved: Value = serde_json::from_str(materialized["content"].as_str().unwrap()).unwrap();
    assert_eq!(saved["goal_id"], f.goal_id);
    assert_eq!(
        saved["state"]["completed_summary"],
        "Saved successor evidence"
    );
    let old = call(
        &f.db,
        &f.source,
        "agent_private",
        json!({"action":"write",
        "path":format!("{TASK}/retired-write.txt"),"content":"stale source"}),
    );
    assert!(!old.success);
    assert!(!f.server_path(&format!("{TASK}/retired-write.txt")).exists());
    assert!(f.db.authenticate_agent_run(&f.source_token).is_err());
}

#[test]
fn agent_handoff_access_unscoped_goal_binding_still_requires_confirmation_and_current_claim() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    let grant = db
        .create_isolated_agent("ordinary-owner", PROJECT, None)
        .unwrap();
    let run = db.authenticate_agent_run(&grant.token).unwrap();
    assert_eq!(db.agent_run_task_goal(&run).unwrap(), None);
    success(call(
        &db,
        &run,
        "agent_private",
        json!({"action":"write",
        "path":format!("{TASK}/input.txt"),"content":"pre-existing user input"}),
    ));
    let started = success(call(
        &db,
        &run,
        "goal_mode_start",
        json!({"project":PROJECT,
        "task_directory":TASK,"objective":"Continue the approved task","idempotency_key":"ordinary-goal"}),
    ));
    let goal_id = started["goal"]["goal_id"].as_str().unwrap();
    let path = format!("{TASK}/artifact.txt");
    let denied_write = || {
        assert!(
            !call(
                &db,
                &run,
                "agent_private",
                json!({"action":"write","path":path,
            "content":"unapproved business write"})
            )
            .success
        );
        let missing = call(
            &db,
            &run,
            "agent_private",
            json!({"action":"read","path":path}),
        );
        assert!(
            !missing.success,
            "a rejected workspace write must not leave a file"
        );
    };
    denied_write();
    let saved = success(call(
        &db,
        &run,
        "goal_plan_save",
        json!({"goal_id":goal_id,
        "expected_revision":started["goal"]["revision"],"idempotency_key":"ordinary-plan","plan":plan()}),
    ));
    denied_write();
    let confirmed = success(call(
        &db,
        &run,
        "goal_confirm_plan",
        json!({"goal_id":goal_id,
        "expected_revision":saved["goal"]["revision"],"plan_version":1,"plan_sha256":saved["plan"]["hash"],
        "confirmation":"I reviewed and authorize this saved plan","idempotency_key":"ordinary-confirm"}),
    ));
    denied_write();
    success(call(
        &db,
        &run,
        "goal_read",
        json!({"goal_id":goal_id,"view":"node","node_id":"research"}),
    ));
    success(call(
        &db,
        &run,
        "goal_claim_node",
        json!({"goal_id":goal_id,"node_id":"research",
        "expected_revision":confirmed["goal"]["revision"],"plan_version":1,"idempotency_key":"ordinary-claim"}),
    ));
    success(call(
        &db,
        &run,
        "agent_private",
        json!({"action":"write","path":path,"content":"confirmed node result"}),
    ));
    let read = success(call(
        &db,
        &run,
        "agent_private",
        json!({"action":"read","path":path}),
    ));
    assert_eq!(read["content"], "confirmed node result");
    assert_eq!(
        db.agent_run_task_goal(&run).unwrap(),
        None,
        "this must exercise ordinary Goal binding rather than the handoff-only scope branch"
    );
}

#[test]
fn agent_handoff_access_trusted_recovery_reclaims_same_goal_before_real_file_continuation() {
    let mut f = Fixture::new();
    let first = f.claim();
    let old_binding = f.successor.clone();
    let old_token = f.successor_token.clone();
    let old_attempt = first["attempt"]["binding"]["attempt_id"].clone();
    let old_fence = first["attempt"]["binding"]["fence"].as_u64().unwrap();
    let owner = old_binding.host_goal_owner_key();
    let original = success(f.private("read", &format!("{TASK}/artifact.txt"), json!({})));
    let before =
        f.db.read_host_goal_handoff(&owner, &f.goal_id)
            .unwrap()
            .unwrap();
    let generation =
        f.db.isolated_agent_generation(old_binding.owner_user(), PROJECT, old_binding.agent_id())
            .unwrap();
    // This is an explicit trusted management recovery, never an automatic
    // model callback or a retry of the browser startup loop.
    let grant =
        f.db.recover_isolated_agent(
            old_binding.owner_user(),
            PROJECT,
            old_binding.agent_id(),
            generation,
        )
        .unwrap();
    f.successor = f.db.authenticate_agent_run(&grant.token).unwrap();
    f.successor_token = grant.token;
    assert_eq!(f.successor.agent_id(), old_binding.agent_id());
    assert_ne!(f.successor.run_id(), old_binding.run_id());
    assert_eq!(
        f.db.agent_run_task_goal(&f.successor).unwrap().as_deref(),
        Some(f.goal_id.as_str())
    );
    assert!(f.db.authenticate_agent_run(&old_token).is_err());
    let handoff =
        f.db.read_host_goal_handoff(&owner, &f.goal_id)
            .unwrap()
            .unwrap();
    assert_eq!(handoff.handoff_id, before.handoff_id);
    assert_eq!(handoff.attempt_count, before.attempt_count);
    assert_eq!(
        handoff.successor_agent_run_id.as_deref(),
        Some(f.successor.run_id())
    );
    assert_eq!(handoff.execution_owner_epoch, f.successor.run_id());
    let goal = f.db.get_host_goal(&owner, &f.goal_id).unwrap().unwrap();
    assert_eq!(goal.goal_id, f.goal_id);
    assert!(goal.fence > old_fence);
    assert!(goal.current_attempt_id.is_none());
    assert!(
        !call(
            &f.db,
            &old_binding,
            "agent_private",
            json!({"action":"write",
        "path":format!("{TASK}/recovered-old-write.txt"),"content":"revoked writer"})
        )
        .success
    );
    assert!(!f
        .server_path(&format!("{TASK}/recovered-old-write.txt"))
        .exists());
    assert!(
        !f.private(
            "write",
            &format!("{TASK}/recovered-too-early.txt"),
            json!({"content":"no claim yet"})
        )
        .success
    );
    assert!(!f.call("goal_claim_node",json!({"goal_id":f.goal_id,"node_id":"research",
        "expected_revision":goal.revision,"plan_version":1,"idempotency_key":"recovery-before-node-read"})).success);
    let claimed = f.claim_with_key("recovered-successor-claim");
    assert_eq!(claimed["goal"]["goal_id"], f.goal_id);
    assert_ne!(claimed["attempt"]["binding"]["attempt_id"], old_attempt);
    success(f.private(
        "write",
        &format!("{TASK}/artifact.txt"),
        json!({
        "content":"recovered task evidence","expected_sha256":original["sha256"]}),
    ));
    success(f.private(
        "write",
        &format!("{TASK}/recovered.txt"),
        json!({"content":"new recovered output"}),
    ));
    let checkpoint = success(f.call(
        "goal_checkpoint",
        json!({"goal_id":f.goal_id,
        "expected_revision":claimed["goal"]["revision"],"idempotency_key":"recovered-checkpoint",
        "patch":{"reason":"Saved real task output after explicit recovery",
            "state":{"facts":{"recovery":"explicit trusted management"}}}}),
    ));
    let saved = success(f.private(
        "read",
        checkpoint["checkpoint_path"].as_str().unwrap(),
        json!({}),
    ));
    let saved: Value = serde_json::from_str(saved["content"].as_str().unwrap()).unwrap();
    assert_eq!(saved["goal_id"], f.goal_id);
    assert_eq!(
        saved["state"]["facts"]["recovery"],
        "explicit trusted management"
    );
    let reopened = Database::open(&f.temp.path().join("state.db")).unwrap();
    f.db = reopened;
    f.successor = f.db.authenticate_agent_run(&f.successor_token).unwrap();
    assert_eq!(
        f.db.agent_run_task_goal(&f.successor).unwrap().as_deref(),
        Some(f.goal_id.as_str())
    );
    let handoff =
        f.db.read_host_goal_handoff(&owner, &f.goal_id)
            .unwrap()
            .unwrap();
    assert_eq!(
        handoff.successor_agent_run_id.as_deref(),
        Some(f.successor.run_id())
    );
    assert_eq!(handoff.execution_owner_epoch, f.successor.run_id());
    success(f.private(
        "write",
        &format!("{TASK}/after-reopen.txt"),
        json!({"content":"same current run and claim"}),
    ));
    assert_eq!(
        success(f.private("read", &format!("{TASK}/artifact.txt"), json!({})))["content"],
        "recovered task evidence"
    );
    assert!(
        !call(
            &f.db,
            &old_binding,
            "agent_private",
            json!({"action":"write",
        "path":format!("{TASK}/reopened-old-write.txt"),"content":"revoked after reopen"})
        )
        .success
    );
    assert!(!f
        .server_path(&format!("{TASK}/reopened-old-write.txt"))
        .exists());
    assert!(!f
        .server_path(&format!("{TASK}/recovered-too-early.txt"))
        .exists());
}

#[test]
fn agent_handoff_access_other_goals_root_scans_and_package_writes_remain_denied() {
    let f = Fixture::new();
    f.claim();
    for (action, path, fields) in [
        ("read", format!("{OTHER_TASK}/artifact.txt"), json!({})),
        (
            "read",
            "workspace/task-one-extra/hidden.txt".into(),
            json!({}),
        ),
        (
            "read",
            format!("goals/{}/notes.txt", f.other_goal_id),
            json!({}),
        ),
        (
            "read",
            format!("{TASK}/../task-two/artifact.txt"),
            json!({}),
        ),
        ("list", String::new(), json!({})),
        ("list", "workspace".into(), json!({})),
        ("search", String::new(), json!({"query":"sentinel"})),
        (
            "write",
            f.plan_path.clone(),
            json!({"content":"corrupted plan","expected_sha256":f.plan_hash}),
        ),
        (
            "write",
            f.package_path.clone(),
            json!({"content":"corrupted package","expected_sha256":f.package_hash}),
        ),
    ] {
        let result = f.private(action, &path, fields);
        assert!(!result.success, "unexpected {action} access to {path}");
        assert!(!result
            .output
            .to_string()
            .contains("unrelated-result-sentinel"));
    }
    for name in ["goal_read", "goal_bootstrap"] {
        let result = f.call(name, json!({"goal_id":f.other_goal_id}));
        assert!(!result.success, "same-Agent scope escaped through {name}");
        assert!(!result
            .output
            .to_string()
            .contains("unrelated-private-goal-sentinel"));
    }
    assert!(
        !crate::agent_goal_adapter::call(
            &f.db,
            &f.successor,
            "goal_read",
            json!({"goal_id":f.other_goal_id})
        )
        .success
    );
    assert_eq!(
        success(f.call("agent_meeting", json!({"action":"list"}))),
        json!([])
    );
    assert!(!f.call("run_process", json!({})).success);
    assert_eq!(
        success(f.private("read", &f.plan_path, json!({})))["sha256"],
        f.plan_hash
    );
}

#[test]
fn agent_handoff_access_successor_retains_own_memory_while_old_run_loses_access() {
    let f = Fixture::new();
    // Memory is inherited by the same logical Agent independently of the fresh
    // business-node claim required for workspace mutations.
    let original = success(f.private("read", "memory/private.txt", json!({})));
    assert_eq!(original["content"], "private-memory-sentinel");
    let updated = success(f.private(
        "write",
        "memory/private.txt",
        json!({
        "content":"same Agent successor memory","expected_sha256":original["sha256"]}),
    ));
    success(f.private(
        "write",
        "memory/new.txt",
        json!({"content":"another successor memory"}),
    ));
    let listed = success(f.private("list", "memory", json!({"limit":20})));
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["path"].as_str().unwrap().starts_with("memory/")));
    let found = success(f.private("search", "memory", json!({"query":"successor","limit":20})));
    assert_eq!(found.as_array().unwrap().len(), 2);
    for (action, fields) in [
        ("read", json!({})),
        (
            "write",
            json!({"content":"retired writer","expected_sha256":updated["sha256"]}),
        ),
        ("list", json!({"limit":20})),
        ("search", json!({"query":"successor","limit":20})),
    ] {
        let path = if matches!(action, "read" | "write") {
            "memory/private.txt"
        } else {
            "memory"
        };
        let mut args = json!({"action":action,"path":path});
        args.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(
            !call(&f.db, &f.source, "agent_private", args).success,
            "revoked source run unexpectedly retained Memory {action}"
        );
    }
    assert_eq!(
        success(f.private("read", "memory/private.txt", json!({})))["content"],
        "same Agent successor memory"
    );
    assert_eq!(
        success(f.call("agent_meeting", json!({"action":"list"}))),
        json!([])
    );
    assert!(
        !f.private(
            "write",
            &format!("{TASK}/memory-is-not-a-claim.txt"),
            json!({"content":"no claim"})
        )
        .success
    );
    assert!(!f
        .server_path(&format!("{TASK}/memory-is-not-a-claim.txt"))
        .exists());
}

#[test]
fn agent_handoff_access_owned_memory_does_not_authorize_another_agent_private_space() {
    let f = Fixture::new();
    let b_grant =
        f.db.create_isolated_agent("handoff-owner", PROJECT, None)
            .unwrap();
    let b = f.db.authenticate_agent_run(&b_grant.token).unwrap();
    success(call(
        &f.db,
        &b,
        "agent_private",
        json!({"action":"write","path":"memory/private.txt",
        "content":"agent-b-private-sentinel"}),
    ));
    let b_path =
        f.db.agent_private_root()
            .join("runtime/projects")
            .join(format!("{:x}", Sha256::digest(PROJECT.as_bytes())))
            .join("agents")
            .join(b.agent_id())
            .join("private/memory/private.txt");
    let traversal = format!(
        "memory/../../../{}/private/memory/private.txt",
        b.agent_id()
    );
    assert_eq!(
        f.server_path(&traversal).canonicalize().unwrap(),
        b_path.canonicalize().unwrap(),
        "the traversal fixture must actually address B if path guards were absent"
    );
    for args in [
        json!({"action":"read","path":b_path.to_string_lossy()}),
        json!({"action":"read","path":traversal}),
        json!({"action":"read","path":"memory/private.txt","agent_id":b.agent_id()}),
        json!({"action":"read","path":"memory/private.txt","run_id":b.run_id()}),
    ] {
        let denied = f.call("agent_private", args);
        assert!(!denied.success);
        assert!(!denied
            .output
            .to_string()
            .contains("agent-b-private-sentinel"));
    }
    assert_eq!(
        success(f.private("read", "memory/private.txt", json!({})))["content"],
        "private-memory-sentinel"
    );
    let room =
        f.db.create_agent_meeting(
            PROJECT,
            &[b.agent_id().to_owned()],
            "B-only authorized room",
        )
        .unwrap();
    assert!(
        !f.call("agent_meeting", json!({"action":"read","room_id":room}))
            .success
    );
    assert_eq!(
        success(call(
            &f.db,
            &b,
            "agent_private",
            json!({"action":"read","path":"memory/private.txt"})
        ))["content"],
        "agent-b-private-sentinel"
    );
}

#[test]
fn agent_handoff_access_same_agent_retains_existing_authorized_meeting_membership() {
    let f = Fixture::new_with_meeting(true);
    let room = f.meeting_room.as_deref().unwrap();
    let listed = success(f.call("agent_meeting", json!({"action":"list"})));
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["room_id"] == room));
    let page = success(f.call("agent_meeting", json!({"action":"read","room_id":room})));
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["entries"][0]["author_run_id"], f.source.run_id());
    let entry = success(f.call(
        "agent_meeting",
        json!({"action":"publish","room_id":room,
        "kind":"message","content":"Published by the same Agent's successor",
        "idempotency_key":"successor-meeting-message"}),
    ));
    assert_eq!(entry["author_agent_id"], f.source.agent_id());
    assert_eq!(entry["author_run_id"], f.successor.run_id());
    assert!(
        !call(
            &f.db,
            &f.source,
            "agent_meeting",
            json!({"action":"publish","room_id":room,
        "kind":"message","content":"late old run","idempotency_key":"retired-meeting-message"})
        )
        .success
    );
    let page = success(f.call("agent_meeting", json!({"action":"read","room_id":room})));
    assert_eq!(
        page["entries"].as_array().unwrap().len(),
        2,
        "old credentials cannot add a late publication despite retained logical-Agent membership"
    );
}

#[test]
fn agent_handoff_access_exit_bootstrap_run_replacement_and_reopen_cannot_wash_task_scope() {
    let mut f = Fixture::new();
    let claimed = f.claim();
    let pending = f.db.begin_agent_run_handoff(&f.successor).unwrap();
    let error =
        f.db.activate_agent_run(
            f.successor.owner_user(),
            PROJECT,
            f.successor.agent_id(),
            &pending.run_id,
        )
        .unwrap_err();
    assert_eq!(error.code(), "agent_task_scope_denied");
    assert!(f.db.authenticate_agent_run(&pending.token).is_err());
    let exited = success(f.call(
        "goal_checkpoint",
        json!({"goal_id":f.goal_id,
        "expected_revision":claimed["goal"]["revision"],"idempotency_key":"exit-task",
        "patch":{"action":"exit","reason":"Explicitly stop this task's execution"}}),
    ));
    assert_eq!(exited["goal"]["phase"], "paused");
    assert_eq!(
        f.db.agent_run_task_goal(&f.successor).unwrap().as_deref(),
        Some(f.goal_id.as_str())
    );
    assert!(
        !f.call(
            "goal_bootstrap",
            json!({"goal_id":f.other_goal_id,"activate":true})
        )
        .success
    );
    success(f.call(
        "goal_bootstrap",
        json!({"goal_id":f.goal_id,"activate":true}),
    ));
    assert!(
        !f.call(
            "goal_mode_start",
            json!({"project":PROJECT,"objective":"escape scope","idempotency_key":"escape"})
        )
        .success
    );
    assert!(
        !f.private(
            "write",
            &format!("{TASK}/after-exit.txt"),
            json!({"content":"must remain paused"})
        )
        .success
    );
    let reopened = Database::open(&f.temp.path().join("state.db")).unwrap();
    f.db = reopened;
    f.successor = f.db.authenticate_agent_run(&f.successor_token).unwrap();
    assert_eq!(
        f.db.agent_run_task_goal(&f.successor).unwrap().as_deref(),
        Some(f.goal_id.as_str())
    );
    assert_eq!(
        success(f.private("read", "memory/private.txt", json!({})))["content"],
        "private-memory-sentinel"
    );
    success(f.private(
        "write",
        "memory/after-reopen.txt",
        json!({"content":"own memory remains available while the Goal is paused"}),
    ));
    assert!(!f.private("list", "", json!({})).success);
    assert!(
        !f.call("goal_read", json!({"goal_id":f.other_goal_id}))
            .success
    );
    assert!(!f.server_path(&format!("{TASK}/after-exit.txt")).exists());
}

#[test]
fn agent_handoff_access_hardlink_cannot_alias_memory_through_business_workspace() {
    let f = Fixture::new();
    f.claim();
    // Own Memory is authorized directly; aliases must still not bypass the
    // distinction between Memory operations and Goal workspace business writes.
    success(f.private("read", "memory/private.txt", json!({})));
    let link = format!("{TASK}/memory-hardlink.txt");
    std::fs::hard_link(f.server_path("memory/private.txt"), f.server_path(&link)).unwrap();
    assert!(!f.private("read", &link, json!({})).success);
    assert!(
        !f.private(
            "write",
            &link,
            json!({"content":"must not alter private memory"})
        )
        .success
    );
    assert_eq!(
        std::fs::read_to_string(f.server_path("memory/private.txt")).unwrap(),
        "private-memory-sentinel"
    );
}

#[test]
fn agent_handoff_access_symlink_cannot_alias_memory_through_business_workspace() {
    let f = Fixture::new();
    f.claim();
    success(f.private("read", "memory/private.txt", json!({})));
    let relative = format!("{TASK}/memory-link.txt");
    let target = f.server_path("memory/private.txt");
    let link = f.server_path(&relative);
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file(&target, &link) {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            eprintln!("SYMLINK TEST UNVERIFIED: symbolic-link privilege is unavailable; the mandatory hardlink denial is tested separately");
            return;
        }
        panic!("synthetic symlink creation failed: {error}");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[cfg(any(windows, unix))]
    {
        assert!(!f.private("read", &relative, json!({})).success);
        assert!(
            !f.private(
                "write",
                &relative,
                json!({"content":"must not follow a link"})
            )
            .success
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "private-memory-sentinel"
        );
    }
}
