use super::cli_fallback::*;
use super::team_task::{init_team_task_tables, TeamAgentPlan, TeamCliCall, TeamPlanInput};
use super::{
    CommunicationPrincipal, Database, NewAgentTask, COMMUNICATION_PRINCIPAL_DIGEST_PREFIX,
};
use rusqlite::params;
use std::path::PathBuf;
use uuid::Uuid;
use webcodex_core::cli_fallback::{CliCancellationOrigin, CliFailureReason};
use webcodex_core::runner_protocol::ShellJobInfo;

fn owner(hex: char) -> CommunicationPrincipal {
    CommunicationPrincipal {
        kind: "user".into(),
        digest: format!(
            "{COMMUNICATION_PRINCIPAL_DIGEST_PREFIX}{}",
            hex.to_string().repeat(64)
        ),
    }
}

fn task(db: &Database, principal: &CommunicationPrincipal) -> String {
    db.create_agent_task(
        principal,
        NewAgentTask {
            title: "CLI recovery test".into(),
            instruction: "Inspect existing work and run the acceptance check.".into(),
            assignee_agent_id: None,
            source_conversation_id: None,
            source_message_id: None,
            referenced_project_id: Some("agent:runner-a:project-a".into()),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .unwrap()
    .task
    .summary
    .task_id
}

fn open(path: &PathBuf) -> Database {
    let db = Database::open(path).unwrap();
    init_cli_fallback_tables(&db.conn.lock().unwrap()).unwrap();
    init_team_task_tables(&db.conn.lock().unwrap()).unwrap();
    db
}

fn sources(
    db: &Database,
    principal: &CommunicationPrincipal,
) -> (String, Vec<(String, ShellJobInfo)>) {
    let root = task(db, principal);
    let source = task(db, principal);
    let project = "agent:runner-a:project-a";
    let policy = db.get_team_policy(principal, project).unwrap();
    db.register_team_plan(
        principal,
        project,
        &root,
        policy.revision,
        TeamPlanInput {
            manager: TeamAgentPlan {
                task_id: root.clone(),
                cli_calls: vec![],
            },
            subagents: vec![TeamAgentPlan {
                task_id: source.clone(),
                cli_calls: (0..2)
                    .map(|_| TeamCliCall {
                        provider_id: "workbuddy".into(),
                        model: "explicit-model".into(),
                        effort: Some("high".into()),
                        instruction: Some(
                            "Inspect existing work and run the acceptance check.".into(),
                        ),
                    })
                    .collect(),
            }],
        },
        &Uuid::new_v4().to_string(),
    )
    .unwrap();
    let attempts = (0..2).map(|index| {
        let attempt = db.reserve_team_cli_attempt(principal, project, &root, &source, index,
            &Uuid::new_v4().to_string(), "thinking-explicit", Some(300)).unwrap().attempt;
        let id = attempt.attempt_id;
        assert!(db.claim_team_cli_dispatch(principal, &id).unwrap().claimed);
        let job_id = format!("job-{}", Uuid::new_v4().simple());
        db.bind_team_cli_job(principal, &id, &job_id).unwrap();
        let job = serde_json::from_value(serde_json::json!({
            "job_id": job_id, "request_id": format!("request-{index}"),
            "client_id": "runner-a", "project_id": project, "kind": "shell",
            "shell": "direct_argv", "command_preview": "cli task", "status": "failed",
            "created_at": 100, "started_at": 101, "ended_at": 102, "exit_code": 1,
            "command_execution_state": "completed", "last_update_seq": 2,
            "structured_execution": { "execution_source": "run_process", "arg_count": 2, "stdin_present": false }
        })).unwrap();
        (id,job)
    }).collect();
    (root, attempts)
}

fn reserve(
    db: &Database,
    principal: &CommunicationPrincipal,
    source: &(String, ShellJobInfo),
) -> anyhow::Result<CliFallbackRecord> {
    db.reserve_cli_fallback(
        principal,
        &source.0,
        &source.1,
        CliFailureReason::QuotaExhausted,
        CliCancellationOrigin::None,
    )
}

#[test]
fn reservation_uses_real_source_job_and_freezes_manager_profile_without_ordinary_slot() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let (root, sources) = sources(&db, &principal);
    let mut wrong = sources[0].1.clone();
    wrong.job_id = "unbound-job".into();
    assert!(db
        .reserve_cli_fallback(
            &principal,
            &sources[0].0,
            &wrong,
            CliFailureReason::QuotaExhausted,
            CliCancellationOrigin::None
        )
        .is_err());
    assert!(reserve(&db, &owner('b'), &sources[0]).is_err());
    let record = reserve(&db, &principal, &sources[0]).unwrap();
    assert_eq!(reserve(&db, &principal, &sources[0]).unwrap(), record);
    assert_eq!(record.thinking_profile_id, "thinking-explicit");
    assert_eq!(
        record.instruction,
        "Inspect existing work and run the acceptance check."
    );
    assert_eq!(record.source_job_id, sources[0].1.job_id);
    assert_eq!(
        db.read_team_plan(&principal, &root)
            .unwrap()
            .plan
            .subagents
            .len(),
        1
    );
    wrong = sources[0].1.clone();
    wrong.request_id = Some("retargeted-request".into());
    assert!(db
        .reserve_cli_fallback(
            &principal,
            &sources[0].0,
            &wrong,
            CliFailureReason::QuotaExhausted,
            CliCancellationOrigin::None
        )
        .is_err());
    let claim = db
        .claim_cli_fallback(&principal, &record.fallback_id)
        .unwrap();
    assert!(claim.dispatch_permitted);
    assert!(claim.task.contains(&record.source_job_id));
    assert_eq!(claim.thinking_profile_id, record.thinking_profile_id);
}

#[test]
fn concurrent_claims_get_one_fence_and_unknown_keeps_other_failures_queued() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let db = open(&path);
    let principal = owner('a');
    let (_, sources) = sources(&db, &principal);
    let first = reserve(&db, &principal, &sources[0]).unwrap();
    let second = reserve(&db, &principal, &sources[1]).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let db = open(&path);
            let principal = principal.clone();
            let id = first.fallback_id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                db.claim_cli_fallback(&principal, &id).unwrap()
            })
        })
        .collect();
    let claims: Vec<_> = workers
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.dispatch_permitted)
            .count(),
        1
    );
    assert_eq!(claims[0].dispatch_attempt_id, claims[1].dispatch_attempt_id);
    let fence = claims[0].dispatch_attempt_id.as_deref().unwrap();
    db.record_cli_fallback_observation(
        &principal,
        &first.fallback_id,
        fence,
        CliFallbackObservation::Unknown,
    )
    .unwrap();
    assert!(
        !db.claim_cli_fallback(&principal, &second.fallback_id)
            .unwrap()
            .dispatch_permitted
    );
    db.resolve_cli_fallback(
        &principal,
        &first.fallback_id,
        fence,
        CliFallbackResolution::VerifiedTerminal,
        "verified-provider-ended",
    )
    .unwrap();
    assert!(
        db.claim_cli_fallback(&principal, &second.fallback_id)
            .unwrap()
            .dispatch_permitted
    );
}

#[test]
fn confirmed_busy_retries_are_bounded_and_each_retry_fences_old_delivery() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let (_, sources) = sources(&db, &principal);
    let record = reserve(&db, &principal, &sources[0]).unwrap();
    let mut fences = std::collections::BTreeSet::new();
    for count in 1..=4 {
        let claim = db
            .claim_cli_fallback(&principal, &record.fallback_id)
            .unwrap();
        assert!(claim.dispatch_permitted);
        let fence = claim.dispatch_attempt_id.unwrap();
        assert!(fences.insert(fence.clone()));
        let updated = db
            .record_cli_fallback_observation(
                &principal,
                &record.fallback_id,
                &fence,
                CliFallbackObservation::Busy {
                    evidence: format!("not-sent-{count}"),
                },
            )
            .unwrap();
        assert_eq!(updated.dispatch_count, count);
        assert_eq!(
            updated.state,
            if count == 4 {
                CliFallbackState::Blocked
            } else {
                CliFallbackState::Queued
            }
        );
        assert!(db
            .record_cli_fallback_observation(
                &principal,
                &record.fallback_id,
                &fence,
                CliFallbackObservation::Dispatched {
                    request_id: "late".into()
                }
            )
            .is_err());
    }
    assert!(
        !db.claim_cli_fallback(&principal, &record.fallback_id)
            .unwrap()
            .dispatch_permitted
    );
}

#[test]
fn root_cancellation_blocks_new_reservation_and_queued_claim_but_preserves_readback() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let (root, sources) = sources(&db, &principal);
    let record = reserve(&db, &principal, &sources[0]).unwrap();
    db.mark_team_plan_cancelled(&principal, &root).unwrap();
    assert!(db
        .claim_cli_fallback(&principal, &record.fallback_id)
        .is_err());
    assert!(reserve(&db, &principal, &sources[1]).is_err());
    assert_eq!(
        db.read_cli_fallback(&principal, &record.fallback_id)
            .unwrap(),
        record
    );
    assert_eq!(reserve(&db, &principal, &sources[0]).unwrap(), record);
}

// Lifecycle tests start after reservation. Source admission is tested through
// the real team API separately; this fixture creates no alternate source table.
fn seed_started(
    db: &Database,
    principal: &CommunicationPrincipal,
    root: &str,
) -> anyhow::Result<CliFallbackRecord> {
    let id = format!("wc_cli_fallback_{}", Uuid::new_v4().simple());
    let source = task(db, principal);
    let attempt = format!("wc_team_cli_attempt_{}", Uuid::new_v4().simple());
    db.conn.lock().unwrap().execute("INSERT INTO wc_cli_fallbacks(
        fallback_id,owner_principal_kind,owner_principal_digest,root_task_id,source_task_id,
        source_attempt_id,project,source_job_id,source_request_id,client_id,instruction,
        thinking_profile_id,failure_reason,cancellation_origin,state,dispatch_attempt_id,
        dispatch_count,created_at_unix_ms,updated_at_unix_ms)
        VALUES(?1,?2,?3,?4,?5,?6,'agent:runner-a:project-a','job-a','request-a','runner-a',
        'Continue existing work','thinking-explicit','quota_exhausted','none','starting',?7,1,1000,1000)",
        params![id,principal.kind,principal.digest,root,source,attempt,Uuid::new_v4().to_string()])?;
    db.read_cli_fallback(principal, &id)
}

#[test]
fn unknown_survives_reopen_and_owns_the_root_slot() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let principal = owner('a');
    let db = open(&path);
    let root = task(&db, &principal);
    let started = seed_started(&db, &principal, &root).unwrap();
    let unknown = db
        .record_cli_fallback_observation(
            &principal,
            &started.fallback_id,
            started.dispatch_attempt_id.as_deref().unwrap(),
            CliFallbackObservation::Unknown,
        )
        .unwrap();
    drop(db);
    let db = open(&path);
    assert_eq!(
        db.read_cli_fallback(&principal, &started.fallback_id)
            .unwrap(),
        unknown
    );
    assert!(seed_started(&db, &principal, &root).is_err());
    let repeated = db
        .claim_cli_fallback(&principal, &started.fallback_id)
        .unwrap();
    assert!(!repeated.dispatch_permitted);
    assert_eq!(repeated.dispatch_attempt_id, started.dispatch_attempt_id);
}

#[test]
fn other_owners_and_old_dispatch_fences_cannot_mutate_or_read() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let root = task(&db, &principal);
    let started = seed_started(&db, &principal, &root).unwrap();
    let fence = started.dispatch_attempt_id.as_deref().unwrap();
    assert!(db
        .read_cli_fallback(&owner('b'), &started.fallback_id)
        .is_err());
    assert!(db
        .cancel_cli_fallback(&owner('b'), &started.fallback_id)
        .is_err());
    assert!(db
        .list_cli_fallbacks(&owner('b'), &root)
        .unwrap()
        .is_empty());
    assert_eq!(
        db.list_project_cli_fallbacks(&principal, "agent:runner-a:project-a", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(db
        .list_project_cli_fallbacks(&owner('b'), "agent:runner-a:project-a", 10)
        .unwrap()
        .is_empty());
    assert!(db
        .list_project_cli_fallbacks(&principal, "agent:runner-a:other", 10)
        .unwrap()
        .is_empty());
    assert!(db
        .list_project_cli_fallbacks(&principal, "agent:runner-a:project-a", 101)
        .is_err());
    assert!(db
        .record_cli_fallback_observation(
            &owner('b'),
            &started.fallback_id,
            fence,
            CliFallbackObservation::Unknown
        )
        .is_err());
    assert!(db
        .record_cli_fallback_observation(
            &principal,
            &started.fallback_id,
            "stale",
            CliFallbackObservation::Unknown
        )
        .is_err());
    assert_eq!(
        db.read_cli_fallback(&principal, &started.fallback_id)
            .unwrap(),
        started
    );
}

#[test]
fn cancellation_and_candidate_response_do_not_release_the_slot() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let root = task(&db, &principal);
    let started = seed_started(&db, &principal, &root).unwrap();
    let fence = started.dispatch_attempt_id.as_deref().unwrap();
    let cancelled = db
        .cancel_cli_fallback(&principal, &started.fallback_id)
        .unwrap();
    assert!(cancelled.cancel_requested && cancelled.state.holds_slot());
    let candidate = db
        .record_cli_fallback_observation(
            &principal,
            &started.fallback_id,
            fence,
            CliFallbackObservation::ResponseObserved,
        )
        .unwrap();
    assert!(candidate.response_observed && candidate.state.holds_slot());
    assert!(candidate.revision > cancelled.revision);
    assert!(seed_started(&db, &principal, &root).is_err());
    let terminal = db
        .resolve_cli_fallback(
            &principal,
            &started.fallback_id,
            fence,
            CliFallbackResolution::VerifiedTerminal,
            "verified-provider-turn-ended",
        )
        .unwrap();
    assert_eq!(terminal.state, CliFallbackState::VerifiedTerminal);
    assert!(seed_started(&db, &principal, &root).is_ok());
}

#[test]
fn busy_requires_known_unstarted_dispatch_and_invalidates_its_fence() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let root = task(&db, &principal);
    let started = seed_started(&db, &principal, &root).unwrap();
    let fence = started.dispatch_attempt_id.as_deref().unwrap();
    let queued = db
        .record_cli_fallback_observation(
            &principal,
            &started.fallback_id,
            fence,
            CliFallbackObservation::Busy {
                evidence: "adapter-busy-before-send".into(),
            },
        )
        .unwrap();
    assert_eq!(queued.state, CliFallbackState::Queued);
    assert!(queued.dispatch_attempt_id.is_none());
    assert_eq!(queued.dispatch_count, 1);
    assert!(db
        .record_cli_fallback_observation(
            &principal,
            &started.fallback_id,
            fence,
            CliFallbackObservation::Dispatched {
                request_id: "late-old-result".into()
            }
        )
        .is_err());
    let other = seed_started(&db, &principal, &root).unwrap();
    let other_fence = other.dispatch_attempt_id.as_deref().unwrap();
    db.record_cli_fallback_observation(
        &principal,
        &other.fallback_id,
        other_fence,
        CliFallbackObservation::Unknown,
    )
    .unwrap();
    assert!(db
        .record_cli_fallback_observation(
            &principal,
            &other.fallback_id,
            other_fence,
            CliFallbackObservation::Busy {
                evidence: "worker-ended-without-status".into()
            }
        )
        .is_err());
}

#[test]
fn sent_turn_cannot_be_resolved_as_never_started_or_retargeted() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let root = task(&db, &principal);
    let started = seed_started(&db, &principal, &root).unwrap();
    let fence = started.dispatch_attempt_id.as_deref().unwrap();
    db.record_cli_fallback_observation(
        &principal,
        &started.fallback_id,
        fence,
        CliFallbackObservation::Dispatched {
            request_id: "host-request-one".into(),
        },
    )
    .unwrap();
    assert!(db
        .resolve_cli_fallback(
            &principal,
            &started.fallback_id,
            fence,
            CliFallbackResolution::ConfirmedNotStarted,
            "local-worker-stopped"
        )
        .is_err());
    assert!(db
        .record_cli_fallback_observation(
            &principal,
            &started.fallback_id,
            fence,
            CliFallbackObservation::Dispatched {
                request_id: "host-request-two".into()
            }
        )
        .is_err());
}

#[test]
fn cancellation_wins_before_busy_refund_and_duplicate_terminal_is_stable() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let root = task(&db, &principal);
    let started = seed_started(&db, &principal, &root).unwrap();
    let fence = started.dispatch_attempt_id.as_deref().unwrap();
    db.cancel_cli_fallback(&principal, &started.fallback_id)
        .unwrap();
    let cancelled = db
        .record_cli_fallback_observation(
            &principal,
            &started.fallback_id,
            fence,
            CliFallbackObservation::Busy {
                evidence: "never-sent".into(),
            },
        )
        .unwrap();
    assert_eq!(cancelled.state, CliFallbackState::Cancelled);
    assert!(
        !db.claim_cli_fallback(&principal, &started.fallback_id)
            .unwrap()
            .dispatch_permitted
    );
    let next = seed_started(&db, &principal, &root).unwrap();
    let next_fence = next.dispatch_attempt_id.as_deref().unwrap();
    let terminal = db
        .resolve_cli_fallback(
            &principal,
            &next.fallback_id,
            next_fence,
            CliFallbackResolution::ConfirmedNotStarted,
            "confirmed-before-send",
        )
        .unwrap();
    assert_eq!(
        db.resolve_cli_fallback(
            &principal,
            &next.fallback_id,
            next_fence,
            CliFallbackResolution::ConfirmedNotStarted,
            "confirmed-before-send"
        )
        .unwrap(),
        terminal
    );
    assert!(db
        .resolve_cli_fallback(
            &principal,
            &next.fallback_id,
            next_fence,
            CliFallbackResolution::VerifiedTerminal,
            "different-evidence"
        )
        .is_err());
}

#[test]
fn root_cancellation_reaches_live_records_after_one_hundred_historical_entries() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("state.db"));
    let principal = owner('a');
    let (root, attempts) = sources(&db, &principal);
    let mut history = Vec::new();
    for _ in 0..101 {
        let record = seed_started(&db, &principal, &root).unwrap();
        history.push(
            db.resolve_cli_fallback(
                &principal,
                &record.fallback_id,
                record.dispatch_attempt_id.as_deref().unwrap(),
                CliFallbackResolution::ConfirmedNotStarted,
                "historical-not-started",
            )
            .unwrap(),
        );
    }
    let active = reserve(&db, &principal, &attempts[0]).unwrap();
    let active = db
        .claim_cli_fallback(&principal, &active.fallback_id)
        .unwrap()
        .record;
    assert!(active.state.holds_slot());
    let queued = reserve(&db, &principal, &attempts[1]).unwrap();
    let other = owner('b');
    let (_, other_attempts) = sources(&db, &other);
    let other_record = reserve(&db, &other, &other_attempts[0]).unwrap();
    assert!(db.cancel_root_cli_fallbacks(&other, &root).is_err());
    assert_eq!(
        db.read_cli_fallback(&principal, &active.fallback_id)
            .unwrap(),
        active
    );

    db.mark_team_plan_cancelled(&principal, &root).unwrap();
    assert_eq!(db.list_cli_fallbacks(&principal, &root).unwrap().len(), 100);
    assert_eq!(db.cancel_root_cli_fallbacks(&principal, &root).unwrap(), 2);
    let cancelled_active = db
        .read_cli_fallback(&principal, &active.fallback_id)
        .unwrap();
    assert!(cancelled_active.cancel_requested && cancelled_active.state.holds_slot());
    assert_eq!(
        cancelled_active.dispatch_attempt_id,
        active.dispatch_attempt_id
    );
    let cancelled_queue = db
        .read_cli_fallback(&principal, &queued.fallback_id)
        .unwrap();
    assert_eq!(cancelled_queue.state, CliFallbackState::Cancelled);
    assert!(cancelled_queue.cancel_requested);
    assert_eq!(
        db.read_cli_fallback(&other, &other_record.fallback_id)
            .unwrap(),
        other_record
    );
    for record in history {
        assert_eq!(
            db.read_cli_fallback(&principal, &record.fallback_id)
                .unwrap(),
            record
        );
    }
    assert_eq!(db.cancel_root_cli_fallbacks(&principal, &root).unwrap(), 0);
    assert_eq!(
        db.read_cli_fallback(&principal, &active.fallback_id)
            .unwrap(),
        cancelled_active
    );
}

#[test]
fn fallback_candidate_pages_resume_beyond_unknown_roots_with_a_fixed_high_water_mark() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("candidate-pages.db");
    let db = open(&path);
    let principal = owner('a');
    let project = "agent:runner-a:project-a";
    let mut blocked_behind_unknown = None;
    for index in 0..101 {
        let (_, attempts) = sources(&db, &principal);
        let record = reserve(&db, &principal, &attempts[0]).unwrap();
        let claim = db
            .claim_cli_fallback(&principal, &record.fallback_id)
            .unwrap();
        db.record_cli_fallback_observation(
            &principal,
            &record.fallback_id,
            claim.dispatch_attempt_id.as_deref().unwrap(),
            CliFallbackObservation::Unknown,
        )
        .unwrap();
        if index == 0 {
            blocked_behind_unknown = Some(reserve(&db, &principal, &attempts[1]).unwrap());
        }
    }
    let (_, attempts) = sources(&db, &principal);
    let ready = reserve(&db, &principal, &attempts[0]).unwrap();
    let first = db
        .list_project_cli_fallbacks_page(&principal, project, 100, None)
        .unwrap();
    assert_eq!(first.records.len(), 100);
    let cursor = first.next_cursor.unwrap();
    assert!(cursor.len() <= 512);
    assert!(db
        .list_project_cli_fallbacks_page(&owner('b'), project, 100, Some(&cursor))
        .is_err());
    assert!(db
        .list_project_cli_fallbacks_page(&principal, "agent:runner-a:other", 100, Some(&cursor))
        .is_err());
    assert!(db
        .list_team_agent_attempts_page(&principal, project, None, true, 100, Some(&cursor))
        .is_err());
    assert!(db
        .list_project_cli_fallbacks_page(&principal, project, 100, Some(&"x".repeat(513)))
        .is_err());
    // An arrival during the scan belongs to the next finite cycle.
    let (_, attempts) = sources(&db, &principal);
    let later = reserve(&db, &principal, &attempts[0]).unwrap();
    drop(db);
    let db = open(&path);
    let resumed = db
        .list_project_cli_fallbacks_page(&principal, project, 100, Some(&cursor))
        .unwrap();
    assert!(resumed.next_cursor.is_none());
    assert_eq!(resumed.records.len(), 3);
    assert!(resumed
        .records
        .iter()
        .any(|record| record.fallback_id == ready.fallback_id));
    assert!(!resumed
        .records
        .iter()
        .any(|record| record.fallback_id == later.fallback_id));
    assert_eq!(
        resumed.records,
        db.list_project_cli_fallbacks_page(&principal, project, 100, Some(&cursor))
            .unwrap()
            .records
    );
    assert!(
        db.claim_cli_fallback(&principal, &ready.fallback_id)
            .unwrap()
            .dispatch_permitted
    );
    assert!(
        !db.claim_cli_fallback(&principal, &ready.fallback_id)
            .unwrap()
            .dispatch_permitted
    );
    assert!(
        !db.claim_cli_fallback(&principal, &blocked_behind_unknown.unwrap().fallback_id)
            .unwrap()
            .dispatch_permitted
    );
    let next_cycle = db
        .list_project_cli_fallbacks_page(&principal, project, 100, None)
        .unwrap();
    let tail = db
        .list_project_cli_fallbacks_page(
            &principal,
            project,
            100,
            next_cycle.next_cursor.as_deref(),
        )
        .unwrap();
    assert!(tail.next_cursor.is_none());
    assert!(tail
        .records
        .iter()
        .any(|record| record.fallback_id == later.fallback_id));
}

#[test]
fn fallback_profile_preflight_refunds_only_a_proven_uncreated_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let db = open(&temp.path().join("profile-refresh.db"));
    let principal = owner('a');
    let (_, attempts) = sources(&db, &principal);
    let record = reserve(&db, &principal, &attempts[0]).unwrap();
    let claim = db
        .claim_cli_fallback(&principal, &record.fallback_id)
        .unwrap();
    db.record_cli_fallback_observation(
        &principal,
        &record.fallback_id,
        claim.dispatch_attempt_id.as_deref().unwrap(),
        CliFallbackObservation::Busy {
            evidence: "browser_worker_busy:launched=false".into(),
        },
    )
    .unwrap();
    for reason in [
        "discover_thinking_profiles_first",
        "thinking_profiles_expired_refresh_required",
        "thinking_profile_not_discovered",
    ] {
        let claim = db
            .claim_cli_fallback(&principal, &record.fallback_id)
            .unwrap();
        assert!(claim.dispatch_permitted);
        let fence = claim.dispatch_attempt_id.unwrap();
        let unchanged = db
            .read_cli_fallback(&principal, &record.fallback_id)
            .unwrap();
        assert!(db
            .record_cli_fallback_observation(
                &principal,
                &record.fallback_id,
                &fence,
                CliFallbackObservation::ProfileUnavailable {
                    evidence: "network_error".into()
                }
            )
            .is_err());
        assert_eq!(
            db.read_cli_fallback(&principal, &record.fallback_id)
                .unwrap(),
            unchanged
        );
        let queued = db
            .record_cli_fallback_observation(
                &principal,
                &record.fallback_id,
                &fence,
                CliFallbackObservation::ProfileUnavailable {
                    evidence: reason.into(),
                },
            )
            .unwrap();
        assert_eq!(queued.state, CliFallbackState::Queued);
        assert_eq!(queued.dispatch_count, 1); // Earlier busy consumed one; profile expiry did not.
        assert_eq!(queued.thinking_profile_id, record.thinking_profile_id);
        assert_eq!(queued.fallback_id, record.fallback_id);
        assert!(queued.dispatch_attempt_id.is_none() && queued.request_id.is_none());
        assert!(db
            .record_cli_fallback_observation(
                &principal,
                &record.fallback_id,
                &fence,
                CliFallbackObservation::Unknown
            )
            .is_err());
    }
    let claim = db
        .claim_cli_fallback(&principal, &record.fallback_id)
        .unwrap();
    let fence = claim.dispatch_attempt_id.unwrap();
    let unknown = db
        .record_cli_fallback_observation(
            &principal,
            &record.fallback_id,
            &fence,
            CliFallbackObservation::Unknown,
        )
        .unwrap();
    let expired = || CliFallbackObservation::ProfileUnavailable {
        evidence: "thinking_profiles_expired_refresh_required".into(),
    };
    assert!(db
        .record_cli_fallback_observation(&principal, &record.fallback_id, &fence, expired())
        .is_err());
    assert_eq!(
        db.read_cli_fallback(&principal, &record.fallback_id)
            .unwrap(),
        unknown
    );
    let sent = db
        .record_cli_fallback_observation(
            &principal,
            &record.fallback_id,
            &fence,
            CliFallbackObservation::Dispatched {
                request_id: Uuid::new_v4().to_string(),
            },
        )
        .unwrap();
    assert!(db
        .record_cli_fallback_observation(&principal, &record.fallback_id, &fence, expired())
        .is_err());
    assert_eq!(
        db.read_cli_fallback(&principal, &record.fallback_id)
            .unwrap(),
        sent
    );
    let (_, attempts) = sources(&db, &principal);
    let cancelled = reserve(&db, &principal, &attempts[0]).unwrap();
    let claim = db
        .claim_cli_fallback(&principal, &cancelled.fallback_id)
        .unwrap();
    db.cancel_cli_fallback(&principal, &cancelled.fallback_id)
        .unwrap();
    let cancelled = db
        .record_cli_fallback_observation(
            &principal,
            &cancelled.fallback_id,
            claim.dispatch_attempt_id.as_deref().unwrap(),
            expired(),
        )
        .unwrap();
    assert_eq!(cancelled.state, CliFallbackState::Cancelled);
    assert_eq!(cancelled.dispatch_count, 0);
}
