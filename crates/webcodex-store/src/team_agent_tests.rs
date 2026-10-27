use super::*;
use crate::{NewAgentTask, TeamAgentPlan, TeamPlanInput, COMMUNICATION_PRINCIPAL_DIGEST_PREFIX};
use std::sync::{Arc, Barrier};

const PROJECT: &str = "agent:runner:ordinary";
fn principal(character: char) -> CommunicationPrincipal {
    CommunicationPrincipal {
        kind: "user".into(),
        digest: format!(
            "{COMMUNICATION_PRINCIPAL_DIGEST_PREFIX}{}",
            character.to_string().repeat(64)
        ),
    }
}
fn task(db: &Database, owner: &CommunicationPrincipal, project: &str, key: &str) -> String {
    db.create_agent_task(
        owner,
        NewAgentTask {
            title: key.into(),
            instruction: format!("Authoritative instruction {key}"),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some(project.into()),
            idempotency_key: key.into(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}
struct Fixture {
    _dir: tempfile::TempDir,
    db: Arc<Database>,
    owner: CommunicationPrincipal,
    root: String,
    child: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_project(PROJECT)
    }
    fn with_project(project: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("state.db")).unwrap());
        crate::team_task::init_team_task_tables(&db.conn.lock().unwrap()).unwrap();
        init_team_agent_tables(&db.conn.lock().unwrap()).unwrap();
        let owner = principal('a');
        let root = task(&db, &owner, project, "root");
        let child = task(&db, &owner, project, "child");
        let plan = TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: root.clone(),
                cli_calls: vec![],
            },
            subagents: vec![TeamAgentPlan {
                task_id: child.clone(),
                cli_calls: vec![],
            }],
        };
        db.register_team_plan(&owner, project, &root, 0, plan, "plan")
            .unwrap();
        Self {
            _dir: dir,
            db,
            owner,
            root,
            child,
        }
    }
    fn reserve(&self) -> TeamAgentRecord {
        self.db
            .reserve_team_agent_attempt(
                &self.owner,
                PROJECT,
                &self.root,
                &self.child,
                "spawn-once",
                "profile-manager-selected",
            )
            .unwrap()
            .record
    }
}

#[test]
fn team_agent_named_workspace_preserves_frozen_scope_and_owner() {
    let project = "wc_workspace_0123456789abcdef0123456789abcdef";
    let f = Fixture::with_project(project);
    let reserve = |owner: &CommunicationPrincipal, scope: &str| {
        f.db.reserve_team_agent_attempt(
            owner,
            scope,
            &f.root,
            &f.child,
            "named-member",
            "explicit-profile",
        )
    };
    assert_eq!(
        reserve(&principal('b'), project).unwrap_err().code(),
        "team_agent_source_unavailable"
    );
    assert_eq!(
        reserve(&f.owner, "wc_workspace_ffffffffffffffffffffffffffffffff")
            .unwrap_err()
            .code(),
        "team_agent_source_unavailable"
    );
    for invalid in [
        "wc_workspace_",
        "wc_workspace_0123456789ABCDEF0123456789abcdef",
        "wc_workspace_0123456789abcdef0123456789abcdef/other",
    ] {
        assert_eq!(
            reserve(&f.owner, invalid).unwrap_err().code(),
            "team_agent_invalid_source"
        );
    }
    let first = reserve(&f.owner, project).unwrap();
    assert!(first.created);
    assert_eq!(first.record.project, project);
    let replay = reserve(&f.owner, project).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.record, first.record);
    let claimed =
        f.db.claim_team_agent_attempt(&f.owner, &first.record.team_agent_attempt_id)
            .unwrap();
    assert!(claimed.dispatch_permitted);
    assert_eq!(claimed.record.project, project);
}

#[test]
fn team_agent_reservation_uses_frozen_subagent_and_one_immutable_intent() {
    let f = Fixture::new();
    let first = f.reserve();
    assert_eq!(first.instruction, "Authoritative instruction child");
    assert_eq!(first.state, TeamAgentState::Queued);
    let replay =
        f.db.reserve_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &f.child,
            "spawn-once",
            "profile-manager-selected",
        )
        .unwrap();
    assert!(replay.replayed);
    assert!(!replay.created);
    assert_eq!(replay.record, first);
    assert_eq!(
        f.db.reserve_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &f.child,
            "new-key",
            "profile-manager-selected"
        )
        .unwrap_err()
        .code(),
        "team_agent_already_reserved"
    );
    assert_eq!(
        f.db.reserve_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &f.child,
            "spawn-once",
            "different-profile"
        )
        .unwrap_err()
        .code(),
        "team_agent_idempotency_conflict"
    );
    assert!(f
        .db
        .reserve_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &f.root,
            "manager",
            "profile-manager-selected"
        )
        .is_err());
    let unrelated = task(&f.db, &f.owner, PROJECT, "unplanned");
    assert!(f
        .db
        .reserve_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &unrelated,
            "other",
            "profile-manager-selected"
        )
        .is_err());
    assert!(f
        .db
        .reserve_team_agent_attempt(&f.owner, PROJECT, &f.root, &f.child, "key", "")
        .is_err());
}

#[test]
fn team_agent_scope_is_revalidated_for_creation_lookup_reads_and_claims() {
    let f = Fixture::new();
    let record = f.reserve();
    assert!(f
        .db
        .read_team_agent_attempt(&principal('b'), &record.team_agent_attempt_id)
        .is_err());
    assert!(f
        .db
        .lookup_team_agent_attempt(
            &f.owner,
            "agent:runner:other",
            &f.root,
            &f.child,
            "spawn-once",
            "profile-manager-selected"
        )
        .is_err());
    f.db.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE wc_agent_tasks SET referenced_project_id='agent:runner:other' WHERE task_id=?1",
            [&f.child],
        )
        .unwrap();
    assert!(f
        .db
        .claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
        .is_err());
    assert!(f
        .db
        .read_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
        .is_err());
}

#[test]
fn team_agent_only_one_concurrent_claim_can_send() {
    let f = Fixture::new();
    let record = f.reserve();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let db = f.db.clone();
        let owner = f.owner.clone();
        let id = record.team_agent_attempt_id.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            db.claim_team_agent_attempt(&owner, &id).unwrap()
        }));
    }
    barrier.wait();
    let a = threads.remove(0).join().unwrap();
    let b = threads.remove(0).join().unwrap();
    assert_eq!(
        usize::from(a.dispatch_permitted) + usize::from(b.dispatch_permitted),
        1
    );
    assert_eq!(a.record.dispatch_attempt_id, b.record.dispatch_attempt_id);
    assert_eq!(a.record.dispatch_count, 1);
    assert!(a.task.contains("team_cli_start"));
    assert!(a.task.contains("Authoritative instruction child"));
}

#[test]
fn team_agent_unknown_is_not_requeued_and_replay_needs_no_new_profile_observation() {
    let f = Fixture::new();
    let record = f.reserve();
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    let fence = claim.record.dispatch_attempt_id.unwrap();
    f.db.record_team_agent_observation(
        &f.owner,
        &record.team_agent_attempt_id,
        &fence,
        TeamAgentObservation::Unknown { request_id: None },
    )
    .unwrap();
    let next =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    assert!(!next.dispatch_permitted);
    assert_eq!(next.record.state, TeamAgentState::Unknown);
    assert!(f
        .db
        .record_team_agent_observation(
            &f.owner,
            &record.team_agent_attempt_id,
            &fence,
            TeamAgentObservation::Busy {
                evidence: "browser_worker_busy:launched=false".into()
            }
        )
        .is_err());
    let replay =
        f.db.lookup_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &f.child,
            "spawn-once",
            "profile-manager-selected",
        )
        .unwrap()
        .unwrap();
    assert_eq!(replay.state, TeamAgentState::Unknown);
    assert_eq!(replay.dispatch_count, 1);
}

#[test]
fn team_agent_busy_is_bounded_and_old_dispatch_fences_are_rejected() {
    let f = Fixture::new();
    let record = f.reserve();
    let mut previous: Option<String> = None;
    for index in 1..=MAX_BUSY_DISPATCHES {
        let claim =
            f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
                .unwrap();
        assert!(claim.dispatch_permitted);
        let fence = claim.record.dispatch_attempt_id.unwrap();
        if let Some(old) = previous {
            assert!(f
                .db
                .record_team_agent_observation(
                    &f.owner,
                    &record.team_agent_attempt_id,
                    &old,
                    TeamAgentObservation::Unknown { request_id: None }
                )
                .is_err());
        }
        let updated =
            f.db.record_team_agent_observation(
                &f.owner,
                &record.team_agent_attempt_id,
                &fence,
                TeamAgentObservation::Busy {
                    evidence: "browser_worker_busy:launched=false".into(),
                },
            )
            .unwrap();
        assert_eq!(
            updated.state,
            if index == MAX_BUSY_DISPATCHES {
                TeamAgentState::ConfirmedNotStarted
            } else {
                TeamAgentState::Queued
            }
        );
        previous = Some(fence);
    }
    assert!(
        !f.db
            .claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap()
            .dispatch_permitted
    );
}

#[test]
fn team_agent_candidate_response_is_not_task_success_and_request_binding_is_immutable() {
    let f = Fixture::new();
    let record = f.reserve();
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    let fence = claim.record.dispatch_attempt_id.unwrap();
    let request = Uuid::new_v4().to_string();
    let candidate =
        f.db.record_team_agent_observation(
            &f.owner,
            &record.team_agent_attempt_id,
            &fence,
            TeamAgentObservation::Dispatched {
                request_id: request.clone(),
                response_observed: true,
            },
        )
        .unwrap();
    assert_eq!(candidate.state, TeamAgentState::Running);
    assert!(candidate.response_observed);
    assert!(f
        .db
        .record_team_agent_observation(
            &f.owner,
            &record.team_agent_attempt_id,
            &fence,
            TeamAgentObservation::Dispatched {
                request_id: Uuid::new_v4().to_string(),
                response_observed: false
            }
        )
        .is_err());
    assert!(f
        .db
        .resolve_team_agent_attempt(
            &f.owner,
            &record.team_agent_attempt_id,
            &fence,
            TeamAgentResolution::ConfirmedNotStarted,
            "not valid after binding"
        )
        .is_err());
    let done =
        f.db.resolve_team_agent_attempt(
            &f.owner,
            &record.team_agent_attempt_id,
            &fence,
            TeamAgentResolution::VerifiedTerminal,
            "trusted terminal observation",
        )
        .unwrap();
    assert_eq!(done.state, TeamAgentState::VerifiedTerminal);
    let state: String =
        f.db.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM wc_agent_tasks WHERE task_id=?1",
                [&f.child],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(state, "ready");
}

#[test]
fn team_agent_cancel_before_dispatch_is_terminal_but_active_cancel_is_only_a_request() {
    let f = Fixture::new();
    let record = f.reserve();
    f.db.mark_team_plan_cancelled(&f.owner, &f.root).unwrap();
    f.db.cancel_root_team_agents(&f.owner, PROJECT, &f.root)
        .unwrap();
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    assert!(!claim.dispatch_permitted);
    assert_eq!(claim.record.state, TeamAgentState::Cancelled);
    assert!(f
        .db
        .lookup_team_agent_attempt(
            &f.owner,
            PROJECT,
            &f.root,
            &f.child,
            "spawn-once",
            "profile-manager-selected"
        )
        .unwrap()
        .is_some());
    let f = Fixture::new();
    let record = f.reserve();
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    let fence = claim.record.dispatch_attempt_id.unwrap();
    f.db.mark_team_plan_cancelled(&f.owner, &f.root).unwrap();
    let rows =
        f.db.cancel_root_team_agents(&f.owner, PROJECT, &f.root)
            .unwrap();
    assert_eq!(rows[0].state, TeamAgentState::Starting);
    assert!(rows[0].cancel_requested);
    let late =
        f.db.record_team_agent_observation(
            &f.owner,
            &record.team_agent_attempt_id,
            &fence,
            TeamAgentObservation::Dispatched {
                request_id: Uuid::new_v4().to_string(),
                response_observed: false,
            },
        )
        .unwrap();
    assert_eq!(late.state, TeamAgentState::Running);
    assert!(late.cancel_requested);
}

#[test]
fn team_agent_claim_rechecks_terminal_tasks_and_listing_is_scoped() {
    let f = Fixture::new();
    let record = f.reserve();
    let identity =
        f.db.create_agent_identity(
            &f.owner,
            crate::NewAgentIdentity {
                handle: "manager".into(),
                display_name: "Manager".into(),
                description: String::new(),
                specialty_labels: vec![],
                idempotency_key: "manager".into(),
            },
        )
        .unwrap()
        .agent
        .agent_id;
    f.db.assign_agent_task(&f.owner, &f.root, &identity)
        .unwrap();
    let started =
        f.db.start_agent_task_attempt(&f.owner, &f.root, &identity, "root-attempt")
            .unwrap();
    f.db.complete_agent_task_attempt(
        &f.owner,
        &f.root,
        &started.attempt.attempt_id,
        &identity,
        &started.attempt_fence,
        started.attempt.attempt_controller_generation,
        crate::AgentTaskState::Succeeded,
        Some("Verified done"),
        None,
        "complete-root",
    )
    .unwrap();
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    assert!(!claim.dispatch_permitted);
    assert_eq!(claim.record.state, TeamAgentState::Cancelled);
    assert!(f
        .db
        .list_team_agent_attempts(&f.owner, PROJECT, None, true, 100)
        .unwrap()
        .is_empty());
    assert_eq!(
        f.db.list_team_agent_attempts(&f.owner, PROJECT, Some(&f.root), false, 100)
            .unwrap()
            .len(),
        1
    );
    assert!(f
        .db
        .list_team_agent_attempts(&principal('b'), PROJECT, None, false, 100)
        .unwrap()
        .is_empty());
}

fn reserve_new_root(db: &Database, owner: &CommunicationPrincipal, label: &str) -> TeamAgentRecord {
    let root = task(db, owner, PROJECT, &format!("root-{label}"));
    let child = task(db, owner, PROJECT, &format!("child-{label}"));
    db.register_team_plan(
        owner,
        PROJECT,
        &root,
        0,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: root.clone(),
                cli_calls: vec![],
            },
            subagents: vec![TeamAgentPlan {
                task_id: child.clone(),
                cli_calls: vec![],
            }],
        },
        &format!("plan-{label}"),
    )
    .unwrap();
    db.reserve_team_agent_attempt(
        owner,
        PROJECT,
        &root,
        &child,
        &format!("spawn-{label}"),
        "profile-manager-selected",
    )
    .unwrap()
    .record
}

#[test]
fn team_agent_candidate_pages_do_not_starve_new_roots_behind_unknown_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("candidate-pages.db");
    let db = Database::open(&path).unwrap();
    let owner = principal('a');
    for index in 0..101 {
        let record = reserve_new_root(&db, &owner, &format!("old-{index}"));
        let claim = db
            .claim_team_agent_attempt(&owner, &record.team_agent_attempt_id)
            .unwrap();
        db.record_team_agent_observation(
            &owner,
            &record.team_agent_attempt_id,
            claim.record.dispatch_attempt_id.as_deref().unwrap(),
            TeamAgentObservation::Unknown { request_id: None },
        )
        .unwrap();
    }
    let ready = reserve_new_root(&db, &owner, "ready");
    let first = db
        .list_team_agent_attempts_page(&owner, PROJECT, None, true, 100, None)
        .unwrap();
    assert_eq!(first.records.len(), 100);
    let cursor = first.next_cursor.unwrap();
    assert!(cursor.len() <= 512);
    assert!(db
        .list_team_agent_attempts_page(&principal('b'), PROJECT, None, true, 100, Some(&cursor))
        .is_err());
    assert!(db
        .list_team_agent_attempts_page(&owner, "agent:runner:other", None, true, 100, Some(&cursor))
        .is_err());
    assert!(db
        .list_team_agent_attempts_page(
            &owner,
            PROJECT,
            Some(&ready.root_task_id),
            true,
            100,
            Some(&cursor)
        )
        .is_err());
    assert!(db
        .list_team_agent_attempts_page(&owner, PROJECT, None, false, 100, Some(&cursor))
        .is_err());
    assert!(db
        .list_project_cli_fallbacks_page(&owner, PROJECT, 100, Some(&cursor))
        .is_err());
    let later = reserve_new_root(&db, &owner, "later");
    drop(db);
    let db = Database::open(&path).unwrap();
    let second = db
        .list_team_agent_attempts_page(&owner, PROJECT, None, true, 100, Some(&cursor))
        .unwrap();
    assert!(second.next_cursor.is_none());
    assert_eq!(second.records.len(), 2);
    assert!(second
        .records
        .iter()
        .any(|record| record.team_agent_attempt_id == ready.team_agent_attempt_id));
    assert!(!second
        .records
        .iter()
        .any(|record| record.team_agent_attempt_id == later.team_agent_attempt_id));
    assert_eq!(
        second.records,
        db.list_team_agent_attempts_page(&owner, PROJECT, None, true, 100, Some(&cursor))
            .unwrap()
            .records
    );
    assert!(
        db.claim_team_agent_attempt(&owner, &ready.team_agent_attempt_id)
            .unwrap()
            .dispatch_permitted
    );
    assert!(
        !db.claim_team_agent_attempt(&owner, &ready.team_agent_attempt_id)
            .unwrap()
            .dispatch_permitted
    );
    let next = db
        .list_team_agent_attempts_page(&owner, PROJECT, None, true, 100, None)
        .unwrap();
    let tail = db
        .list_team_agent_attempts_page(
            &owner,
            PROJECT,
            None,
            true,
            100,
            next.next_cursor.as_deref(),
        )
        .unwrap();
    assert!(tail.next_cursor.is_none());
    assert!(tail
        .records
        .iter()
        .any(|record| record.team_agent_attempt_id == later.team_agent_attempt_id));
}

#[test]
fn team_agent_profile_preflight_preserves_selection_and_busy_budget_but_never_requeues_unknown() {
    let f = Fixture::new();
    let record = f.reserve();
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    f.db.record_team_agent_observation(
        &f.owner,
        &record.team_agent_attempt_id,
        claim.record.dispatch_attempt_id.as_deref().unwrap(),
        TeamAgentObservation::Busy {
            evidence: "browser_worker_busy:launched=false".into(),
        },
    )
    .unwrap();
    for reason in [
        "discover_thinking_profiles_first",
        "thinking_profiles_expired_refresh_required",
        "thinking_profile_not_discovered",
    ] {
        let claim =
            f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
                .unwrap();
        let fence = claim.record.dispatch_attempt_id.as_deref().unwrap();
        assert!(f
            .db
            .record_team_agent_observation(
                &f.owner,
                &record.team_agent_attempt_id,
                fence,
                TeamAgentObservation::ProfileUnavailable {
                    evidence: "chat_operation_failed".into()
                }
            )
            .is_err());
        assert_eq!(
            f.db.read_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
                .unwrap(),
            claim.record
        );
        let queued =
            f.db.record_team_agent_observation(
                &f.owner,
                &record.team_agent_attempt_id,
                fence,
                TeamAgentObservation::ProfileUnavailable {
                    evidence: reason.into(),
                },
            )
            .unwrap();
        assert_eq!(queued.state, TeamAgentState::Queued);
        assert_eq!(queued.dispatch_count, 1);
        assert_eq!(queued.thinking_profile_id, record.thinking_profile_id);
        assert_eq!(queued.team_agent_attempt_id, record.team_agent_attempt_id);
        assert!(queued.dispatch_attempt_id.is_none() && queued.request_id.is_none());
        assert!(f
            .db
            .record_team_agent_observation(
                &f.owner,
                &record.team_agent_attempt_id,
                fence,
                TeamAgentObservation::Unknown { request_id: None }
            )
            .is_err());
    }
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap();
    let fence = claim.record.dispatch_attempt_id.as_deref().unwrap();
    let unknown =
        f.db.record_team_agent_observation(
            &f.owner,
            &record.team_agent_attempt_id,
            fence,
            TeamAgentObservation::Unknown { request_id: None },
        )
        .unwrap();
    let expired = || TeamAgentObservation::ProfileUnavailable {
        evidence: "thinking_profiles_expired_refresh_required".into(),
    };
    assert!(f
        .db
        .record_team_agent_observation(&f.owner, &record.team_agent_attempt_id, fence, expired())
        .is_err());
    assert_eq!(
        f.db.read_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap(),
        unknown
    );
    let bound =
        f.db.record_team_agent_observation(
            &f.owner,
            &record.team_agent_attempt_id,
            fence,
            TeamAgentObservation::Unknown {
                request_id: Some(Uuid::new_v4().to_string()),
            },
        )
        .unwrap();
    assert!(f
        .db
        .record_team_agent_observation(&f.owner, &record.team_agent_attempt_id, fence, expired())
        .is_err());
    assert_eq!(
        f.db.read_team_agent_attempt(&f.owner, &record.team_agent_attempt_id)
            .unwrap(),
        bound
    );
    let cancelled = reserve_new_root(&f.db, &f.owner, "cancelled-profile");
    let claim =
        f.db.claim_team_agent_attempt(&f.owner, &cancelled.team_agent_attempt_id)
            .unwrap();
    f.db.mark_team_plan_cancelled(&f.owner, &cancelled.root_task_id)
        .unwrap();
    let cancelled =
        f.db.record_team_agent_observation(
            &f.owner,
            &cancelled.team_agent_attempt_id,
            claim.record.dispatch_attempt_id.as_deref().unwrap(),
            expired(),
        )
        .unwrap();
    assert_eq!(cancelled.state, TeamAgentState::Cancelled);
    assert_eq!(cancelled.dispatch_count, 0);
}
