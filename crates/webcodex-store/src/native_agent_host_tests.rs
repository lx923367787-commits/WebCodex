use super::*;
const TUNNEL: &str = "tunnel_0123456789abcdef0123456789abcdef";
const DIGEST: &str = "ee9d4a75bc0b42f36f345aa96231e0db1ab00488122f34ebc99d6db055b6603e";

#[test]
fn named_workspace_files_survive_real_task_handoff_without_extending_authority() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let workspace = workspace('a');
    db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![workspace.clone()])
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let source = db
        .ensure_native_host_window(&host, &"d".repeat(64), now)
        .unwrap();
    let assignment = db.read_agent_enrollment(&source).unwrap();
    assert!(db.agent_native_workspace_for_files(&source, true).is_ok());
    let context = db.ensure_agent_host_context(&source, None).unwrap();
    let owner = source.host_goal_owner_key();
    let goal = db
        .create_host_goal_in_task_directory_in_context(
            &owner,
            source.project(),
            "/private",
            &assignment.task_directory,
            "Continue same assignment",
            "native-files-goal",
            now,
            Some(&context),
        )
        .unwrap()
        .goal;
    db.bind_host_goal_window_in_context(
        &owner,
        source.run_id(),
        &crate::HostGoalWindowBinding {
            goal_id: goal.goal_id.clone(),
            project: goal.project.clone(),
            project_root: goal.project_root.clone(),
            context_epoch: source.run_id().into(),
        },
        Some(&context),
    )
    .unwrap();
    db.checkpoint_host_goal_with_request_in_context(
        &owner,
        &goal.goal_id,
        goal.revision,
        None,
        &goal.state,
        crate::HostGoalCheckpointAction::Handoff,
        "continue",
        "native-files-handoff",
        "native-files-request",
        &[],
        now,
        Some(&context),
        &[],
    )
    .unwrap();
    let waiting = db
        .read_host_goal_handoff(&owner, &goal.goal_id)
        .unwrap()
        .unwrap();
    let handoff = db
        .begin_host_goal_handoff_attempt(
            &owner,
            &goal.goal_id,
            &waiting.handoff_id,
            waiting.attempt_id.as_deref(),
            now,
            120,
        )
        .unwrap();
    let window = "e".repeat(64);
    let attempt = handoff.attempt_id.as_deref().unwrap();
    let digest = "f".repeat(64);
    db.bind_handoff_host_window(
        host.principal_key(),
        &window,
        &goal.goal_id,
        &handoff.handoff_id,
        attempt,
        now,
    )
    .unwrap();
    db.prepare_agent_handoff_for_host_window(
        host.principal_key(),
        &window,
        &goal.goal_id,
        &handoff.handoff_id,
        attempt,
        handoff.checkpoint_sequence,
        &digest,
        now,
    )
    .unwrap();
    db.commit_agent_handoff_for_host_window(
        host.principal_key(),
        &window,
        &goal.goal_id,
        &handoff.handoff_id,
        attempt,
        handoff.checkpoint_sequence,
        &digest,
        now,
    )
    .unwrap();
    let Some(crate::AgentHostWindowBinding::Active(successor)) = db
        .resolve_agent_host_window(host.principal_key(), &window, now)
        .unwrap()
    else {
        panic!("committed successor must be active")
    };
    assert_eq!(successor.agent_id(), source.agent_id());
    assert_eq!(
        db.agent_run_task_goal(&successor).unwrap(),
        Some(goal.goal_id)
    );
    for write in [false, true] {
        assert_eq!(
            db.agent_native_workspace_for_files(&successor, write)
                .unwrap(),
            workspace
        );
        assert!(db.agent_native_workspace_for_files(&source, write).is_err());
    }
    // The immutable scope also rejects attempts to substitute another Goal.
    assert!(db
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE wc_agent_runs SET task_goal_id='hgoal_unrelated' WHERE run_id=?1",
            [successor.run_id()],
        )
        .is_err());
    db.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE wc_agent_enrollment_assignments SET state='released' WHERE agent_id=?1",
            [successor.agent_id()],
        )
        .unwrap();
    assert_eq!(
        db.agent_native_workspace_for_files(&successor, true)
            .unwrap_err()
            .code(),
        "agent_assignment_released"
    );
    db.configure_native_workspaces(host.host_id(), "native-owner", 1, vec![])
        .unwrap();
    assert_eq!(
        db.agent_native_workspace_for_files(&successor, false)
            .unwrap_err()
            .code(),
        "workspace_not_authorized"
    );
}

#[test]
fn native_host_rotation_reopen_and_revocation_keep_stable_principal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let db = Database::open(&path).unwrap();
    let first = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let first_token = first.into_secret();
    let verified = db.authenticate_native_agent_host(&first_token).unwrap();
    assert_eq!(
        db.native_agent_host_principal(verified.host_id()).unwrap(),
        verified.principal_key()
    );
    let hashes: String = db
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT token_hash FROM wc_native_agent_hosts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(hashes, first_token);
    drop(db);
    let db = Database::open(&path).unwrap();
    db.validate_native_agent_host(&verified).unwrap();
    let second = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    assert_eq!(second.host_id, verified.host_id());
    assert_eq!(second.generation, verified.generation() + 1);
    let second_token = second.into_secret();
    let current = db.authenticate_native_agent_host(&second_token).unwrap();
    assert_eq!(current.principal_key(), verified.principal_key());
    assert!(db.authenticate_native_agent_host(&first_token).is_err());
    assert!(db.validate_native_agent_host(&verified).is_err());
    db.revoke_native_agent_host(current.host_id()).unwrap();
    assert!(db.authenticate_native_agent_host(&second_token).is_err());
    assert!(db.register_native_agent_host(TUNNEL, DIGEST).is_err());
}

#[test]
fn native_host_does_not_authenticate_agent_and_never_accepts_asserted_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    let grant = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let host_id = grant.host_id.clone();
    let secret = grant.into_secret();
    assert!(db.authenticate_agent_run(&secret).is_err());
    for value in ["wc_nh_fake", "wc_pat_fake", "wc_ar_fake", "bootstrap"] {
        assert!(db.authenticate_native_agent_host(value).is_err());
    }
    assert_eq!(
        db.native_agent_host_principal(&host_id).unwrap(),
        db.authenticate_native_agent_host(&secret)
            .unwrap()
            .principal_key()
    );
    assert!(db.native_agent_host_principal("asserted-host").is_err());
}

#[test]
fn native_host_concurrent_rotations_leave_one_credential_current() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let db = Database::open(&path).unwrap();
    let host_id = db
        .register_native_agent_host(TUNNEL, DIGEST)
        .unwrap()
        .host_id;
    let other = Database::open(&path).unwrap();
    let join =
        std::thread::spawn(move || other.register_native_agent_host(TUNNEL, DIGEST).unwrap());
    let a = db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    let b = join.join().unwrap();
    assert_eq!(a.host_id, host_id);
    assert_eq!(b.host_id, host_id);
    let live = [a.into_secret(), b.into_secret()]
        .iter()
        .filter(|s| db.authenticate_native_agent_host(s).is_ok())
        .count();
    assert_eq!(live, 1);
}

fn auto_enrollment_fixture() -> (tempfile::TempDir, Database, VerifiedNativeAgentHost) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("state.db")).unwrap();
    db.create_user(&crate::models::UserRecord {
        id: "native-owner".into(),
        username: "native-owner".into(),
        created_at: 1,
        disabled: 0,
        display_name: None,
        role: "user".into(),
        disabled_at: None,
        updated_at: None,
    })
    .unwrap();
    let token = db
        .register_native_agent_host(TUNNEL, DIGEST)
        .unwrap()
        .into_secret();
    let host = db.authenticate_native_agent_host(&token).unwrap();
    (dir, db, host)
}

fn configure_auto(db: &Database, host: &VerifiedNativeAgentHost) {
    db.configure_native_host_auto_enrollment(
        host.host_id(),
        "native-owner",
        "project",
        crate::AgentEnrollmentPolicy {
            max_subagents: 2,
            max_cli_calls: 1,
        },
    )
    .unwrap();
}

#[test]
fn registered_project_files_use_exact_live_host_grant_without_cli_budget() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let project = "agent:runner:a";
    db.configure_native_host_auto_enrollment(
        host.host_id(),
        "native-owner",
        project,
        crate::AgentEnrollmentPolicy {
            max_subagents: 0,
            max_cli_calls: 0,
        },
    )
    .unwrap();
    let binding = db
        .ensure_native_host_window(
            &host,
            &"d".repeat(64),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for write in [false, true] {
        assert_eq!(
            db.agent_project_files_grant(&binding, write).unwrap(),
            crate::AgentProjectFilesGrant::Registered {
                project: project.into()
            }
        );
    }
    // Adding a named catalog bridges only the already authorized business folder.
    let workspace = workspace('a');
    db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![workspace.clone()])
        .unwrap();
    let crate::AgentProjectFilesGrant::Named(grant) =
        db.agent_project_files_grant(&binding, false).unwrap()
    else {
        panic!("expected exact catalog folder");
    };
    assert_eq!(grant.folders, vec![workspace.folders[0].clone()]);
    // The legacy policy must not resurrect a folder removed from the current catalog.
    db.configure_native_workspaces(host.host_id(), "native-owner", 1, vec![])
        .unwrap();
    assert_eq!(
        db.agent_project_files_grant(&binding, false)
            .unwrap_err()
            .code(),
        "workspace_not_authorized"
    );
}

#[test]
fn registered_project_files_reject_retargeted_or_revoked_host_grants() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let policy = crate::AgentEnrollmentPolicy {
        max_subagents: 0,
        max_cli_calls: 0,
    };
    db.configure_native_host_auto_enrollment(
        host.host_id(),
        "native-owner",
        "agent:runner:a",
        policy.clone(),
    )
    .unwrap();
    let binding = db
        .ensure_native_host_window(
            &host,
            &"d".repeat(64),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    db.configure_native_host_auto_enrollment(
        host.host_id(),
        "native-owner",
        "agent:runner:b",
        policy,
    )
    .unwrap();
    assert_eq!(
        db.agent_project_files_grant(&binding, true)
            .unwrap_err()
            .code(),
        "workspace_not_authorized"
    );
    db.revoke_agent_run(
        binding.owner_user(),
        binding.project(),
        binding.agent_id(),
        binding.run_id(),
    )
    .unwrap();
    assert!(db.agent_project_files_grant(&binding, false).is_err());
}

fn workspace(n: char) -> crate::NativeWorkspace {
    crate::NativeWorkspace {
        id: format!("wc_workspace_{}", n.to_string().repeat(32)),
        name: format!("Project {n}"),
        enabled: true,
        team_project: None,
        folders: vec![
            crate::NativeWorkspaceFolder {
                project: format!("agent:runner:{n}"),
                name: "Code".into(),
                path: format!("D:/{n}"),
            },
            crate::NativeWorkspaceFolder {
                project: format!("agent:runner:{n}-docs"),
                name: "Documents".into(),
                path: format!("E:/{n}"),
            },
        ],
    }
}

#[test]
fn named_workspace_team_uses_saved_policy_without_upgrading_existing_chats() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let mut project = workspace('a');
    project.folders.truncate(1);
    db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![project.clone()])
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let old = db
        .ensure_native_host_window(&host, &"a".repeat(64), now)
        .unwrap();
    let principal = crate::agent_isolation::enrollment_principal_for_execution("native-owner");
    let mut settings = crate::TeamPolicyInput::default();
    settings.providers = ["workbuddy", "antigravity"]
        .into_iter()
        .map(|id| crate::TeamProviderPreference {
            provider_id: id.into(),
            enabled: true,
            model: None,
            effort: None,
        })
        .collect();
    settings.mode = crate::TeamPolicyMode::Custom;
    settings.subagents = crate::TeamCountRange { min: 2, max: 4 };
    settings.cli_per_agent = crate::TeamCountRange { min: 2, max: 2 };
    settings.providers[0].model = Some("explicit-model".into());
    settings.providers[0].effort = Some("high".into());
    let saved = db
        .save_team_policy(&principal, &project.folders[0].project, 0, settings)
        .unwrap();
    let old_again = db
        .ensure_native_host_window(&host, &"a".repeat(64), now)
        .unwrap();
    assert_eq!(old, old_again);
    assert_eq!(
        db.read_agent_enrollment(&old).unwrap().policy.max_subagents,
        0
    );
    assert_eq!(
        db.agent_team_execution_project(&old).unwrap_err().code(),
        "workspace_team_new_chat_required"
    );
    let manager = db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .unwrap();
    let assignment = db.read_agent_enrollment(&manager).unwrap();
    assert_eq!(assignment.policy.max_subagents, 4);
    assert_eq!(assignment.policy.max_cli_calls, 10);
    assert_eq!(
        db.agent_team_execution_project(&manager).unwrap(),
        project.folders[0].project
    );
    let observed = db.agent_team_policy(&manager, &assignment.task_id).unwrap();
    assert_eq!(observed.policy, saved);
    let mut children = vec![];
    for name in ["frontend", "backend"] {
        let child = db
            .create_agent_member(
                &manager,
                crate::AgentMemberRequest {
                    title: name.into(),
                    brief: format!("Implement the {name} assignment"),
                    task_directory: format!("workspace/{name}"),
                    inputs: vec![],
                    room_ids: vec![],
                    policy: crate::AgentEnrollmentPolicy {
                        max_subagents: 0,
                        max_cli_calls: 2,
                    },
                },
                name,
            )
            .unwrap();
        let inherited: String = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT folder_json FROM wc_native_workspace_team_targets WHERE agent_id=?1",
                [&child.agent_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<crate::NativeWorkspaceFolder>(&inherited).unwrap(),
            project.folders[0]
        );
        children.push(child.task_id);
    }
    let member = |id: &str| crate::TeamAgentPlan {
        task_id: id.into(),
        cli_calls: (0..2)
            .map(|_| crate::TeamCliCall {
                inputs: vec![],
                provider_id: "workbuddy".into(),
                model: "explicit-model".into(),
                effort: Some("high".into()),
                instruction: Some("Review this member's assigned implementation".into()),
            })
            .collect(),
    };
    let plan = crate::TeamPlanInput {
        manager: member(&assignment.task_id),
        subagents: children.iter().map(|id| member(id)).collect(),
    };
    let frozen = db
        .register_agent_team_plan(
            &manager,
            &assignment.task_id,
            saved.revision,
            plan,
            "named-plan",
        )
        .unwrap();
    assert_eq!(frozen.plan.policy, saved);
    // An operator edit may revoke the target, but must not retarget this task.
    project.folders[0].path = "D:/replacement".into();
    db.configure_native_workspaces(host.host_id(), "native-owner", 1, vec![project])
        .unwrap();
    assert_eq!(
        db.agent_team_execution_project(&manager)
            .unwrap_err()
            .code(),
        "workspace_team_target_changed"
    );
}

#[test]
fn named_workspace_team_does_not_guess_a_folder_or_borrow_another_owners_policy() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let project = workspace('a');
    let principal = crate::agent_isolation::enrollment_principal_for_execution("native-owner");
    db.save_team_policy(
        &principal,
        &project.folders[0].project,
        0,
        crate::TeamPolicyInput::default(),
    )
    .unwrap();
    db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![project.clone()])
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let ambiguous = db
        .ensure_native_host_window(&host, &"a".repeat(64), now)
        .unwrap();
    assert_eq!(
        db.read_agent_enrollment(&ambiguous)
            .unwrap()
            .policy
            .max_subagents,
        0
    );
    let mut other = workspace('b');
    other.folders.truncate(1);
    let foreign = crate::agent_isolation::enrollment_principal_for_execution("different-owner");
    db.save_team_policy(
        &foreign,
        &other.folders[0].project,
        0,
        crate::TeamPolicyInput::default(),
    )
    .unwrap();
    db.configure_native_workspaces(host.host_id(), "native-owner", 1, vec![other])
        .unwrap();
    let fresh = db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .unwrap();
    assert_eq!(
        db.read_agent_enrollment(&fresh)
            .unwrap()
            .policy
            .max_cli_calls,
        0
    );
}

#[test]
fn named_workspace_team_uses_explicit_folder_without_retargeting_existing_chats() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let mut project = workspace('a');
    let principal = crate::agent_isolation::enrollment_principal_for_execution("native-owner");
    let mut code_policy = crate::TeamPolicyInput::default();
    code_policy.providers = ["workbuddy", "antigravity"]
        .into_iter()
        .map(|id| crate::TeamProviderPreference {
            provider_id: id.into(),
            enabled: true,
            model: None,
            effort: None,
        })
        .collect();
    code_policy.mode = crate::TeamPolicyMode::Custom;
    code_policy.subagents = crate::TeamCountRange { min: 1, max: 3 };
    code_policy.cli_per_agent = crate::TeamCountRange { min: 1, max: 2 };
    let code_policy = db
        .save_team_policy(&principal, &project.folders[0].project, 0, code_policy)
        .unwrap();
    let mut docs_policy = crate::TeamPolicyInput::default();
    docs_policy.providers = ["workbuddy", "antigravity"]
        .into_iter()
        .map(|id| crate::TeamProviderPreference {
            provider_id: id.into(),
            enabled: true,
            model: None,
            effort: None,
        })
        .collect();
    docs_policy.mode = crate::TeamPolicyMode::Custom;
    docs_policy.subagents = crate::TeamCountRange { min: 1, max: 2 };
    docs_policy.cli_per_agent = crate::TeamCountRange { min: 1, max: 1 };
    let docs_policy = db
        .save_team_policy(&principal, &project.folders[1].project, 0, docs_policy)
        .unwrap();
    project.team_project = Some(project.folders[1].project.clone());
    db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![project.clone()])
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let docs = db
        .ensure_native_host_window(&host, &"a".repeat(64), now)
        .unwrap();
    let assignment = db.read_agent_enrollment(&docs).unwrap();
    assert_eq!(assignment.policy.max_subagents, 2);
    assert_eq!(assignment.policy.max_cli_calls, 3);
    assert_eq!(
        db.agent_team_policy(&docs, &assignment.task_id)
            .unwrap()
            .policy,
        docs_policy
    );
    assert_eq!(
        db.agent_team_execution_folder(&docs).unwrap(),
        Some(project.folders[1].clone())
    );
    // A changed default affects new chats only, never an existing task's target.
    project.team_project = Some(project.folders[0].project.clone());
    db.configure_native_workspaces(host.host_id(), "native-owner", 1, vec![project.clone()])
        .unwrap();
    assert_eq!(
        db.agent_team_execution_folder(&docs).unwrap(),
        Some(project.folders[1].clone())
    );
    assert_eq!(
        db.agent_team_policy(&docs, &assignment.task_id)
            .unwrap()
            .policy,
        docs_policy
    );
    let code = db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .unwrap();
    let assignment = db.read_agent_enrollment(&code).unwrap();
    assert_eq!(assignment.policy.max_subagents, 3);
    assert_eq!(assignment.policy.max_cli_calls, 8);
    assert_eq!(
        db.agent_team_policy(&code, &assignment.task_id)
            .unwrap()
            .policy,
        code_policy
    );
    project.folders.truncate(1);
    db.configure_native_workspaces(host.host_id(), "native-owner", 2, vec![project])
        .unwrap();
    assert_eq!(
        db.agent_team_execution_folder(&docs).unwrap_err().code(),
        "workspace_team_target_changed"
    );
    assert!(db.agent_team_execution_folder(&code).is_ok());
}

#[test]
fn named_workspace_team_caps_total_cli_budget_without_rejecting_feasible_policy() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let mut project = workspace('a');
    project.team_project = Some(project.folders[0].project.clone());
    let principal = crate::agent_isolation::enrollment_principal_for_execution("native-owner");
    let mut settings = crate::TeamPolicyInput::default();
    settings.providers = ["workbuddy", "antigravity"]
        .into_iter()
        .map(|id| crate::TeamProviderPreference {
            provider_id: id.into(),
            enabled: true,
            model: None,
            effort: None,
        })
        .collect();
    settings.mode = crate::TeamPolicyMode::Custom;
    settings.subagents = crate::TeamCountRange { min: 1, max: 32 };
    settings.cli_per_agent = crate::TeamCountRange { min: 1, max: 32 };
    let saved = db
        .save_team_policy(&principal, &project.folders[0].project, 0, settings)
        .unwrap();
    db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![project])
        .unwrap();
    let manager = db
        .ensure_native_host_window(
            &host,
            &"a".repeat(64),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let assignment = db.read_agent_enrollment(&manager).unwrap();
    assert_eq!(assignment.policy.max_subagents, 32);
    assert_eq!(assignment.policy.max_cli_calls, 32);
    assert_eq!(
        db.agent_team_policy(&manager, &assignment.task_id)
            .unwrap()
            .policy,
        saved
    );
}

#[test]
fn named_workspace_team_target_must_be_a_registered_member_folder() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let mut project = workspace('a');
    project.team_project = Some("agent:runner:unrelated".into());
    assert_eq!(
        db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![project])
            .unwrap_err()
            .code(),
        "workspace_team_target_invalid"
    );
    assert_eq!(
        db.read_native_workspaces(host.host_id()).unwrap().revision,
        0
    );
}

#[test]
fn named_workspaces_require_selection_keep_binding_and_revoke_folders() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let a = workspace('a');
    let b = workspace('b');
    let catalog = db
        .configure_native_workspaces(
            host.host_id(),
            "native-owner",
            0,
            vec![a.clone(), b.clone()],
        )
        .unwrap();
    assert_eq!(catalog.revision, 1);
    let now = chrono::Utc::now().timestamp_millis();
    let window = "a".repeat(64);
    assert_eq!(
        db.ensure_native_host_window(&host, &window, now)
            .unwrap_err()
            .code(),
        "workspace_selection_required"
    );
    let run = db
        .select_native_host_workspace(&host, &window, Some(&a.id), now)
        .unwrap();
    assert_eq!(db.agent_native_workspace(&run).unwrap().folders.len(), 2);
    assert_eq!(
        db.select_native_host_workspace(&host, &window, Some(&a.id), now)
            .unwrap(),
        run
    );
    assert_eq!(
        db.select_native_host_workspace(&host, &window, Some(&b.id), now)
            .unwrap_err()
            .code(),
        "workspace_new_chat_required"
    );
    let mut updated = a.clone();
    updated.folders.pop();
    db.configure_native_workspaces(
        host.host_id(),
        "native-owner",
        1,
        vec![updated.clone(), b.clone()],
    )
    .unwrap();
    assert_eq!(
        db.agent_native_workspace(&run).unwrap().folders,
        updated.folders
    );
    updated.enabled = false;
    db.configure_native_workspaces(host.host_id(), "native-owner", 2, vec![updated, b.clone()])
        .unwrap();
    assert_eq!(
        db.agent_native_workspace(&run).unwrap_err().code(),
        "workspace_not_authorized"
    );
    let second = db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .unwrap();
    assert_eq!(second.project(), b.id);
    assert_ne!(second.agent_id(), run.agent_id());
}

#[test]
fn named_workspaces_bound_revisions_names_and_exact_host() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let a = workspace('a');
    db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![a.clone()])
        .unwrap();
    assert_eq!(
        db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![a.clone()])
            .unwrap()
            .revision,
        1
    );
    assert_eq!(
        db.configure_native_workspaces(host.host_id(), "native-owner", 0, vec![])
            .unwrap_err()
            .code(),
        "workspace_revision_conflict"
    );
    let mut duplicate = workspace('b');
    duplicate.name = a.name.to_uppercase();
    assert_eq!(
        db.configure_native_workspaces(
            host.host_id(),
            "native-owner",
            1,
            vec![a.clone(), duplicate]
        )
        .unwrap_err()
        .code(),
        "workspace_invalid_input"
    );
    let now = chrono::Utc::now().timestamp_millis();
    let run = db
        .ensure_native_host_window(&host, &"a".repeat(64), now)
        .unwrap();
    let other = db
        .register_native_agent_host(&format!("tunnel_{}", "c".repeat(32)), DIGEST)
        .unwrap();
    db.configure_native_workspaces(&other.host_id, "native-owner", 0, vec![a])
        .unwrap();
    db.configure_native_workspaces(host.host_id(), "native-owner", 1, vec![])
        .unwrap();
    assert_eq!(
        db.agent_native_workspace(&run).unwrap_err().code(),
        "workspace_not_authorized"
    );
    assert_eq!(
        db.ensure_native_host_window(&host, &"b".repeat(64), now)
            .unwrap_err()
            .code(),
        "workspace_selection_required"
    );
}

#[test]
fn native_auto_enrollment_is_explicit_atomic_independent_and_idempotent() {
    let (_dir, db, host) = auto_enrollment_fixture();
    let first = "a".repeat(64);
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(
        db.ensure_native_host_window(&host, &first, now)
            .unwrap_err()
            .code(),
        "agent_host_enrollment_required"
    );
    assert_eq!(
        db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    configure_auto(&db, &host);
    let a = db.ensure_native_host_window(&host, &first, now).unwrap();
    let again = db.ensure_native_host_window(&host, &first, now).unwrap();
    let b = db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .unwrap();
    assert_eq!(a, again);
    assert_ne!(a.agent_id(), b.agent_id());
    assert_ne!(a.run_id(), b.run_id());
    assert_eq!(a.owner_user(), "native-owner");
    assert_eq!(a.project(), "project");
    let conn = db.conn.lock().unwrap();
    for table in [
        "wc_agent_tasks",
        "wc_agent_enrollments",
        "wc_agent_host_windows",
    ] {
        assert_eq!(
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(DISTINCT task_directory) FROM wc_agent_enrollments",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}

#[test]
fn native_auto_enrollment_never_reuses_stale_or_foreign_window() {
    for stale in ["revoked", "pending", "disabled", "foreign"] {
        let (_dir, db, host) = auto_enrollment_fixture();
        configure_auto(&db, &host);
        let window = "c".repeat(64);
        let now = chrono::Utc::now().timestamp_millis();
        let run = db.ensure_native_host_window(&host, &window, now).unwrap();
        {
            let conn = db.conn.lock().unwrap();
            match stale {
                "disabled" => {
                    conn.execute("UPDATE wc_agent_host_windows SET enabled=0", [])
                        .unwrap();
                }
                "foreign" => {
                    conn.execute(
                        "UPDATE wc_agent_host_windows SET principal_key='another-host'",
                        [],
                    )
                    .unwrap();
                }
                _ => {
                    conn.execute(
                        "UPDATE wc_agent_runs SET state=?1 WHERE run_id=?2",
                        params![stale, run.run_id()],
                    )
                    .unwrap();
                }
            }
        }
        assert_eq!(
            db.ensure_native_host_window(&host, &window, now)
                .unwrap_err()
                .code(),
            "agent_host_window_not_active",
            "{stale}"
        );
        assert_eq!(
            db.conn
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn native_auto_enrollment_revalidates_user_and_host_and_rolls_back_failures() {
    let (_dir, db, host) = auto_enrollment_fixture();
    configure_auto(&db, &host);
    let now = chrono::Utc::now().timestamp_millis();
    let run = db
        .ensure_native_host_window(&host, &"a".repeat(64), now)
        .unwrap();
    db.conn
        .lock()
        .unwrap()
        .execute("UPDATE users SET disabled_at=1", [])
        .unwrap();
    for window in ["a".repeat(64), "b".repeat(64)] {
        assert_eq!(
            db.ensure_native_host_window(&host, &window, now)
                .unwrap_err()
                .code(),
            "agent_enrollment_owner_unavailable"
        );
    }
    db.conn
        .lock()
        .unwrap()
        .execute("UPDATE users SET disabled_at=NULL", [])
        .unwrap();
    // A failure after the public task insert must roll the complete transaction back.
    db.conn.lock().unwrap().execute_batch("CREATE TRIGGER reject_native_claim BEFORE INSERT ON wc_agent_host_windows BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    assert!(db
        .ensure_native_host_window(&host, &"b".repeat(64), now)
        .is_err());
    assert_eq!(
        db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.ensure_native_host_window(&host, &"a".repeat(64), now)
            .unwrap(),
        run
    );
    db.register_native_agent_host(TUNNEL, DIGEST).unwrap();
    assert_eq!(
        db.ensure_native_host_window(&host, &"a".repeat(64), now)
            .unwrap_err()
            .code(),
        "native_host_invalid"
    );
}

#[test]
fn native_auto_enrollment_concurrent_first_calls_create_one_agent() {
    let (dir, db, host) = auto_enrollment_fixture();
    configure_auto(&db, &host);
    let other = Database::open(&dir.path().join("state.db")).unwrap();
    let second_host = host.clone();
    let now = chrono::Utc::now().timestamp_millis();
    let join = std::thread::spawn(move || {
        other
            .ensure_native_host_window(&second_host, &"d".repeat(64), now)
            .unwrap()
    });
    let a = db
        .ensure_native_host_window(&host, &"d".repeat(64), now)
        .unwrap();
    assert_eq!(a, join.join().unwrap());
    assert_eq!(
        db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM wc_agent_tasks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
