use super::*;

const USER: &str = "failed-recovery-user";
const PROJECT: &str = "failed-recovery-project";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

struct Fixture {
    directory: tempfile::TempDir,
    db: Database,
    source: crate::VerifiedAgentRun,
    failed: HostGoalHandoff,
    revision: u64,
    principal: String,
    task_id: String,
}

impl Fixture {
    fn new(attempts: u32) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db = Database::open(&directory.path().join("state.db")).unwrap();
        let native = db
            .register_native_agent_host(&format!("tunnel_{}", "a".repeat(32)), DIGEST)
            .unwrap();
        let principal = db.native_agent_host_principal(&native.host_id).unwrap();
        let task_id = db
            .create_agent_task(
                &crate::agent_isolation::enrollment_principal_for_execution(USER),
                crate::NewAgentTask {
                    title: "Recovery task".into(),
                    instruction: "Explicit test input".into(),
                    assignee_agent_id: None,
                    source_conversation_id: None,
                    source_message_id: None,
                    referenced_project_id: Some(PROJECT.into()),
                    idempotency_key: "task".into(),
                },
            )
            .unwrap()
            .task
            .summary
            .task_id;
        let mut issued = db
            .issue_agent_enrollment(
                crate::NewAgentEnrollment {
                    purpose: crate::AgentEnrollmentPurpose::Manager,
                    owner_user: USER.into(),
                    project: PROJECT.into(),
                    principal_key: principal.clone(),
                    task_id: task_id.clone(),
                    task_directory: "workspace/recovery-task".into(),
                    brief: "Only this task".into(),
                    inputs: vec![],
                    room_ids: vec![],
                    policy: crate::AgentEnrollmentPolicy {
                        max_subagents: 0,
                        max_cli_calls: 0,
                    },
                    expires_at_unix_ms: now() + 60_000,
                },
                None,
                "enrollment",
                now(),
            )
            .unwrap();
        let source = db
            .claim_agent_enrollment(
                &principal,
                "source-window",
                &issued.enrollment_id,
                &issued.grant.take().unwrap().into_secret(),
                now(),
            )
            .unwrap()
            .binding;
        db.register_agent_team_plan(
            &source,
            &task_id,
            0,
            crate::TeamPlanInput {
                manager: crate::TeamAgentPlan {
                    task_id: task_id.clone(),
                    cli_calls: vec![],
                },
                subagents: vec![],
            },
            "plan",
        )
        .unwrap();
        let context = db.ensure_agent_host_context(&source, None).unwrap();
        let owner = source.host_goal_owner_key();
        let goal = db
            .create_host_goal_in_task_directory_in_context(
                &owner,
                PROJECT,
                "/private",
                "workspace/recovery-task",
                "Retain the exact task and work",
                "create",
                now(),
                Some(&context),
            )
            .unwrap()
            .goal;
        db.bind_host_goal_window_in_context(
            &owner,
            source.run_id(),
            &crate::HostGoalWindowBinding {
                goal_id: goal.goal_id.clone(),
                project: PROJECT.into(),
                project_root: goal.project_root.clone(),
                context_epoch: source.run_id().into(),
            },
            Some(&context),
        )
        .unwrap();
        let mut state = goal.state.clone();
        state.next_step = "Continue the preserved task".into();
        db.checkpoint_host_goal_with_request_in_context(
            &owner,
            &goal.goal_id,
            goal.revision,
            None,
            &state,
            HostGoalCheckpointAction::Handoff,
            "handoff",
            "checkpoint",
            "checkpoint-request",
            &[],
            now(),
            Some(&context),
            &[],
        )
        .unwrap();
        let mut h = db
            .read_host_goal_handoff(&owner, &goal.goal_id)
            .unwrap()
            .unwrap();
        for index in 0..attempts {
            h = db
                .begin_host_goal_handoff_attempt(
                    &owner,
                    &goal.goal_id,
                    &h.handoff_id,
                    h.attempt_id.as_deref(),
                    now(),
                    300,
                )
                .unwrap();
            h = if index + 1 == attempts {
                db.abort_host_goal_handoff_attempt(
                    &owner,
                    &goal.goal_id,
                    &h.handoff_id,
                    h.attempt_id.as_deref(),
                    "handoff_aborted",
                    now(),
                )
                .unwrap()
            } else {
                db.fail_host_goal_handoff_attempt(
                    &owner,
                    &goal.goal_id,
                    &h.handoff_id,
                    h.attempt_id.as_deref().unwrap(),
                    "launch_failed",
                    now(),
                )
                .unwrap()
            };
        }
        let revision = read(&db.conn_for_tests(), &owner, &goal.goal_id)
            .unwrap()
            .unwrap()
            .revision;
        assert_eq!(h.status, "failed");
        assert_eq!(h.attempt_count, attempts);
        Self {
            directory,
            db,
            source,
            failed: h,
            revision,
            principal,
            task_id,
        }
    }

    fn recover_on(
        &self,
        db: &Database,
        key: &str,
    ) -> Result<(HostGoalHandoff, bool, u64, crate::VerifiedAgentRun), GoalStoreError> {
        db.recover_failed_agent_handoff(
            USER,
            PROJECT,
            self.source.agent_id(),
            &self.failed.goal_id,
            &self.failed.handoff_id,
            self.source.run_id(),
            self.source.generation(),
            self.revision,
            self.failed.attempt_id.as_deref().unwrap(),
            key,
            now(),
        )
    }

    fn begin(&self, h: &HostGoalHandoff) -> HostGoalHandoff {
        self.db
            .begin_host_goal_handoff_attempt(
                &self.source.host_goal_owner_key(),
                &h.goal_id,
                &h.handoff_id,
                h.attempt_id.as_deref(),
                now(),
                300,
            )
            .unwrap()
    }
}

fn snapshot(db: &Database) -> Vec<(String, Vec<String>)> {
    let conn = db.conn_for_tests();
    let names = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut statement = conn
                .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
                .unwrap();
            let columns = statement.column_count();
            let mut rows = statement
                .query_map([], |r| {
                    let values = (0..columns)
                        .map(|index| r.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(format!("{values:?}"))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows.sort();
            (name, rows)
        })
        .collect()
}

#[test]
fn failed_handoff_recovery_same_actor_goal_replays_once_and_commits_natively() {
    let f = Fixture::new(2);
    let owner = f.source.host_goal_owner_key();
    let (next, replayed, revision, _) = f.recover_on(&f.db, "explicit-recovery").unwrap();
    assert!(!replayed);
    assert_eq!(revision, f.revision + 1);
    assert_ne!(next.handoff_id, f.failed.handoff_id);
    assert_eq!(next.goal_id, f.failed.goal_id);
    assert_eq!(next.agent_id, f.failed.agent_id);
    assert_eq!(next.source_agent_run_id, f.failed.source_agent_run_id);
    assert_eq!(next.attempt_count, 2);
    assert_eq!(next.timeout_secs, f.failed.timeout_secs);
    assert!(next.checkpoint_sequence > f.failed.checkpoint_sequence);
    assert_eq!(
        f.db.read_host_context_state(&owner, f.source.run_id())
            .unwrap()
            .unwrap()
            .state,
        "draining"
    );
    let goal = read(&f.db.conn_for_tests(), &owner, &next.goal_id)
        .unwrap()
        .unwrap();
    assert_eq!(goal.phase, HostGoalPhase::Planning);
    assert_eq!(goal.state.next_step, "Continue the preserved task");
    // Recovery still grants no source business execution or Goal checkpoint.
    assert!(f
        .db
        .with_agent_run_for_path(&f.source, "private_write", "memory/notes.txt", true, || Ok(
            ()
        ))
        .is_err());
    let context = HostGoalInvocationContext {
        context_epoch: f.source.run_id().into(),
        identity_source: "agent_run".into(),
    };
    assert!(f
        .db
        .checkpoint_host_goal_with_request_in_context(
            &owner,
            &next.goal_id,
            revision,
            None,
            &goal.state,
            HostGoalCheckpointAction::Progress,
            "progress",
            "denied-progress",
            "denied-progress",
            &[],
            now(),
            Some(&context),
            &[]
        )
        .is_err());
    let reopened = Database::open(&f.directory.path().join("state.db")).unwrap();
    let before = snapshot(&reopened);
    let (again, replayed, _, _) = f.recover_on(&reopened, "explicit-recovery").unwrap();
    assert!(replayed);
    assert_eq!(again.handoff_id, next.handoff_id);
    assert_eq!(snapshot(&reopened), before);
    assert!(f.recover_on(&reopened, "different-key").is_err());
    let begun = f.begin(&next);
    assert_eq!(begun.attempt_count, 3);
    f.db.bind_handoff_host_window(
        &f.principal,
        "successor-window",
        &begun.goal_id,
        &begun.handoff_id,
        begun.attempt_id.as_deref().unwrap(),
        now(),
    )
    .unwrap();
    f.db.prepare_agent_handoff_for_host_window(
        &f.principal,
        "successor-window",
        &begun.goal_id,
        &begun.handoff_id,
        begun.attempt_id.as_deref().unwrap(),
        begun.checkpoint_sequence,
        DIGEST,
        now(),
    )
    .unwrap();
    let committed =
        f.db.commit_agent_handoff_for_host_window(
            &f.principal,
            "successor-window",
            &begun.goal_id,
            &begun.handoff_id,
            begun.attempt_id.as_deref().unwrap(),
            begun.checkpoint_sequence,
            DIGEST,
            now(),
        )
        .unwrap();
    assert_eq!(committed.status, "committed");
    assert_eq!(committed.agent_id, f.failed.agent_id);
    assert_eq!(committed.goal_id, f.failed.goal_id);
    assert!(f.db.validate_agent_run(&f.source).is_err());
    assert!(f.recover_on(&f.db, "explicit-recovery").is_err());
}

#[test]
fn failed_handoff_recovery_rejects_stale_cancelled_and_live_candidates_without_writes() {
    for mode in [
        "revision",
        "generation",
        "source",
        "attempt",
        "pending",
        "cancelled",
        "goal-cancelled",
        "source-revoked",
        "committed",
        "uncertain-step",
    ] {
        let f = Fixture::new(2);
        let mut revision = f.revision;
        let mut generation = f.source.generation();
        let mut source = f.source.run_id().to_string();
        let mut attempt = f.failed.attempt_id.clone().unwrap();
        match mode {
            "revision" => revision += 1,
            "generation" => generation += 1,
            "source" => source = "unrelated-run".into(),
            "attempt" => attempt = "stale-attempt".into(),
            "pending" => {
                // Corruption fixture retains production triggers: an old
                // candidate not revoked must never be silently replaced.
                f.db.conn_for_tests().execute("UPDATE wc_agent_runs SET state='pending' WHERE agent_id=?1 AND previous_run_id=?2 AND generation=2",
                    params![f.source.agent_id(),f.source.run_id()]).unwrap();
            }
            "cancelled" => {
                f.db.mark_team_plan_cancelled(
                    &crate::agent_isolation::enrollment_principal_for_execution(USER),
                    &f.task_id,
                )
                .unwrap();
            }
            "goal-cancelled" => {
                f.db.conn_for_tests().execute("UPDATE wc_host_goals SET phase='cancelled',snapshot_json=json_set(snapshot_json,'$.phase','cancelled') WHERE goal_id=?1",[&f.failed.goal_id]).unwrap();
            }
            "source-revoked" => {
                f.db.revoke_agent_run(USER, PROJECT, f.source.agent_id(), f.source.run_id())
                    .unwrap();
            }
            "committed" => {
                f.db.conn_for_tests().execute("UPDATE wc_host_goal_handoffs SET snapshot_json=json_set(snapshot_json,'$.status','committed','$.committed_at',1) WHERE goal_id=?1",[&f.failed.goal_id]).unwrap();
            }
            "uncertain-step" => {
                f.db.conn_for_tests().execute("UPDATE wc_host_goal_replays SET result_json=json_set(result_json,'$.goal.phase','executing') WHERE owner_key=?1 AND scope=?2",params![f.source.host_goal_owner_key(),format!("{}:checkpoint",f.failed.goal_id)]).unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(&f.db);
        assert!(
            f.db.recover_failed_agent_handoff(
                USER,
                PROJECT,
                f.source.agent_id(),
                &f.failed.goal_id,
                &f.failed.handoff_id,
                &source,
                generation,
                revision,
                &attempt,
                "recovery",
                now()
            )
            .is_err(),
            "{mode}"
        );
        assert_eq!(snapshot(&f.db), before, "{mode}");
    }
}

#[test]
fn failed_handoff_recovery_rolls_back_atomically_and_never_refills_retry_budget() {
    let f = Fixture::new(2);
    f.db.conn_for_tests().execute_batch("CREATE TRIGGER fail_operator_recovery BEFORE INSERT ON wc_host_goal_replays
        WHEN NEW.scope LIKE '%:failed-handoff-recovery' BEGIN SELECT RAISE(ABORT,'synthetic recovery failure'); END;").unwrap();
    let before = snapshot(&f.db);
    assert!(f.recover_on(&f.db, "recovery").is_err());
    let observer = Database::open(&f.directory.path().join("state.db")).unwrap();
    assert_eq!(snapshot(&observer), before);
    let f = Fixture::new(4);
    let before = snapshot(&f.db);
    assert!(f.recover_on(&f.db, "cannot-refill-budget").is_err());
    assert_eq!(snapshot(&f.db), before);
}
