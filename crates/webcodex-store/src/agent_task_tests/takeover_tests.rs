use super::*;

#[test]
fn provider_failure_handoff_survives_restart_and_freezes_replacement_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("takeover.db");
    let db = Database::open(&path).unwrap();
    let owner = principal('a');
    let first = agent(&db, &owner, "first");
    let second = agent(&db, &owner, "second");
    let task = create_assigned_task(&db, &owner, &first, "takeover");
    let now = wall_now_ms();
    let attempt = start(&db, &owner, &task, &first, "first-attempt", now);
    let intent = coding_binding_intent("takeover-first");
    prepare_coding_binding(&db, &owner, &task, &first, &attempt, &intent, now);
    db.claim_agent_task_coding_run_dispatch(
        &owner,
        &task,
        &attempt.attempt.attempt_id,
        &first,
        &attempt.attempt_fence,
        1,
        &intent.binding_intent_fingerprint,
    )
    .unwrap();
    // A completion assertion cannot release a still-running provider.
    assert_eq!(
        db.complete_agent_task_attempt_at(
            &owner,
            &task,
            &attempt.attempt.attempt_id,
            &first,
            &attempt.attempt_fence,
            1,
            AgentTaskState::Succeeded,
            None,
            None,
            "unsafe",
            now
        )
        .unwrap_err()
        .code(),
        "agent_task_execution_outcome_unknown"
    );
    let mut failed = coding_observation(&intent, "failed", "completed", 3);
    failed.terminal_error_code = Some("quota_exhausted".into());
    db.record_agent_task_coding_run_observation(
        &owner,
        &task,
        &attempt.attempt.attempt_id,
        &failed,
    )
    .unwrap();
    let partial = temp.path().join("partial.txt");
    std::fs::write(&partial, "already completed work").unwrap();
    let evidence = r#"{"events":[{"sequence":1,"kind":"file_change","status":"completed","label":"partial.txt"},{"sequence":2,"kind":"terminal_activity","status":"pending"}],"history_lost":false}"#;
    db.record_agent_task_evidence(
        &owner,
        &task,
        &attempt.attempt.attempt_id,
        &intent.run_id,
        3,
        evidence,
    )
    .unwrap();
    db.terminalize_agent_task_coding_run(
        &owner,
        &task,
        &attempt.attempt.attempt_id,
        &failed,
        Some("quota exhausted after partial work"),
        Some("quota_exhausted"),
    )
    .unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    let brief = db.agent_task_handoff_brief(&owner, &task).unwrap();
    assert!(
        brief.contains("partial.txt")
            && brief.contains("pending")
            && brief.contains("quota_exhausted")
    );
    assert!(!brief.contains(&attempt.attempt_fence));
    assert!(db.agent_task_handoff_brief(&principal('b'), &task).is_err());
    db.assign_agent_task(&owner, &task, &second).unwrap();
    let late = db
        .terminalize_agent_task_coding_run(
            &owner,
            &task,
            &attempt.attempt.attempt_id,
            &failed,
            None,
            None,
        )
        .unwrap();
    assert!(!late.state_changed);
    assert_eq!(late.task.state, AgentTaskState::Ready);
    assert_eq!(
        late.task.assignee_agent_id.as_deref(),
        Some(second.as_str())
    );
    let replacement = start(&db, &owner, &task, &second, "second-attempt", now + 20);
    let context = || {
        db.agent_task_coding_run_start_context_at(
            &owner,
            "agent:special:reference-only",
            &task,
            &replacement.attempt.attempt_id,
            &second,
            &replacement.attempt_fence,
            1,
            now + 21,
        )
        .unwrap()
    };
    let instruction = context().instruction;
    assert!(instruction.contains("partial.txt") && instruction.contains("original_instruction"));
    // Late enrichment of old evidence must not change an admitted attempt's intent.
    db.record_agent_task_evidence(
        &owner,
        &task,
        &attempt.attempt.attempt_id,
        &intent.run_id,
        3,
        "{}",
    )
    .unwrap();
    assert_eq!(context().instruction, instruction);
    assert_eq!(
        std::fs::read_to_string(partial).unwrap(),
        "already completed work"
    );
    assert!(db
        .terminalize_agent_task_coding_run(
            &owner,
            &task,
            &attempt.attempt.attempt_id,
            &failed,
            None,
            None
        )
        .is_err());
    let completed = db
        .complete_agent_task_attempt_at(
            &owner,
            &task,
            &replacement.attempt.attempt_id,
            &second,
            &replacement.attempt_fence,
            1,
            AgentTaskState::Succeeded,
            Some("Original acceptance verified against preserved files"),
            None,
            "verified",
            now + 30,
        )
        .unwrap();
    assert_eq!(completed.task.state, AgentTaskState::Succeeded);
}

#[test]
fn coding_run_history_does_not_limit_new_attempts_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("budget.db");
    let db = Database::open(&path).unwrap();
    let owner = principal('c');
    let assignee = agent(&db, &owner, "bounded");
    let task = create_assigned_task(&db, &owner, &assignee, "bounded-task");
    let now = wall_now_ms();
    for n in 0..10 {
        let attempt = start(&db, &owner, &task, &assignee, &format!("attempt-{n}"), now);
        let intent = coding_binding_intent(&format!("budget-{n}"));
        prepare_coding_binding(&db, &owner, &task, &assignee, &attempt, &intent, now);
        db.claim_agent_task_coding_run_dispatch(
            &owner,
            &task,
            &attempt.attempt.attempt_id,
            &assignee,
            &attempt.attempt_fence,
            1,
            &intent.binding_intent_fingerprint,
        )
        .unwrap();
        let failure = coding_observation(&intent, "failed", "completed", 2);
        db.terminalize_agent_task_coding_run(
            &owner,
            &task,
            &attempt.attempt.attempt_id,
            &failure,
            None,
            Some("rate_limited"),
        )
        .unwrap();
        db.assign_agent_task(&owner, &task, &assignee).unwrap();
    }
    drop(db);
    let db = Database::open(&path).unwrap();
    let attempt = start(&db, &owner, &task, &assignee, "attempt-over-budget", now);
    let prepared = db
        .prepare_agent_task_coding_run_at(
            &owner,
            "agent:special:reference-only",
            &task,
            &attempt.attempt.attempt_id,
            &assignee,
            &attempt.attempt_fence,
            1,
            &coding_binding_intent("budget-overflow"),
            now,
        )
        .unwrap();
    assert!(!prepared.replayed);
    assert_eq!(
        db.read_agent_task(&owner, &task).unwrap().summary.state,
        AgentTaskState::Active
    );
}
