//! Explicit operator recovery of a failed, never-committed native handoff.
//! This re-arms transfer only; the source remains unable to do business work.
use super::*;

#[derive(Serialize, Deserialize)]
struct RecoveryReceipt {
    previous: HostGoalHandoff,
    handoff_id: String,
}

fn denied() -> GoalStoreError {
    GoalStoreError::new(
        "goal_failed_handoff_recovery_denied",
        "The exact failed handoff is not eligible for operator recovery",
    )
}

fn live_assignment(
    conn: &Connection,
    source: &crate::VerifiedAgentRun,
) -> Result<(), GoalStoreError> {
    let task: Option<String> = conn.query_row(
        "SELECT task_id FROM wc_agent_enrollments WHERE agent_id=?1 AND owner_user=?2 AND project=?3",
        params![source.agent_id(), source.owner_user(), source.project()], |r| r.get(0),
    ).optional().map_err(unavailable)?;
    if let Some(task) = task {
        crate::agent_isolation::require_enrollment_public_task_on_connection(
            conn,
            source.owner_user(),
            source.project(),
            &task,
        )
        .map_err(|e| GoalStoreError::new(e.code(), "The assigned Task is not live"))?;
        let active: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM wc_agent_enrollment_assignments
             WHERE agent_id=?1 AND owner_user=?2 AND project=?3 AND task_id=?4 AND state='active')",
                params![
                    source.agent_id(),
                    source.owner_user(),
                    source.project(),
                    task
                ],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
        let principal =
            crate::agent_isolation::enrollment_principal_for_execution(source.owner_user());
        let cancelled: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_team_plans p
             WHERE owner_principal_kind=?1 AND owner_principal_digest=?2 AND project_id=?3
               AND (root_task_id=?4 OR EXISTS(SELECT 1 FROM json_each(p.record_json,'$.plan.subagents')
                                            WHERE json_extract(value,'$.task_id')=?4))
               AND NOT COALESCE(json_type(record_json,'$.cancel_requested')='false'
                                AND json_extract(record_json,'$.status')='active',0))",
            params![principal.kind,principal.digest,source.project(),task], |r| r.get(0),
        ).map_err(unavailable)?;
        if !active || cancelled {
            return Err(denied());
        }
    }
    Ok(())
}

impl Database {
    /// Trusted operator only, never a model tool. Returns current transfer
    /// metadata, replay status and Goal revision; no run credential is issued.
    #[allow(clippy::too_many_arguments)]
    pub fn recover_failed_agent_handoff(
        &self,
        owner_user: &str,
        project: &str,
        agent_id: &str,
        goal_id: &str,
        handoff_id: &str,
        expected_source_run_id: &str,
        expected_source_generation: i64,
        expected_goal_revision: u64,
        expected_attempt_id: Option<&str>,
        idempotency_key: &str,
        now: i64,
    ) -> Result<(HostGoalHandoff, bool, u64, crate::VerifiedAgentRun), GoalStoreError> {
        let owner = crate::agent_isolation::host_goal_owner_key(project, agent_id);
        let fingerprint = request_hash(&(
            owner_user,
            project,
            agent_id,
            goal_id,
            handoff_id,
            expected_source_run_id,
            expected_source_generation,
            expected_goal_revision,
            expected_attempt_id,
        ))?;
        let scope = format!("{goal_id}:failed-handoff-recovery");
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let source = crate::agent_isolation::verify_agent_run_context_on_connection(
            &tx,
            &owner,
            expected_source_run_id,
        )
        .map_err(|e| GoalStoreError::new(e.code(), "The source run is no longer current"))?;
        if source.owner_user() != owner_user
            || source.project() != project
            || source.agent_id() != agent_id
            || source.generation() != expected_source_generation
        {
            return Err(denied());
        }
        crate::agent_isolation::agent_run_goal_scope_on_connection(&tx, &source, Some(goal_id))
            .map_err(|e| {
                GoalStoreError::new(e.code(), "Recovery must preserve the source Goal scope")
            })?;
        live_assignment(&tx, &source)?;
        let mut goal = read(&tx, &owner, goal_id)?
            .filter(|g| g.project == project)
            .ok_or_else(not_found)?;
        if goal.phase.terminal() {
            return Err(denied());
        }
        if let Some(receipt) =
            replay::<RecoveryReceipt>(&tx, &owner, &scope, idempotency_key, &fingerprint)?
        {
            let current = current(&tx, &owner, goal_id, &receipt.handoff_id)?;
            if current.status == "committed"
                || current.committed_at.is_some()
                || current.successor_agent_run_id.is_some()
                || current.source_agent_run_id.as_deref() != Some(source.run_id())
            {
                return Err(denied());
            }
            // A repeat may observe a subsequently reserved attempt, but never
            // allocates another handoff or resets its consumed retry budget.
            return Ok((current, true, goal.revision, source));
        }
        let h = current(&tx, &owner, goal_id, handoff_id)?;
        // The scheduler used to abort a valid blocked native checkpoint before
        // allocating its first candidate. Recover only that exact prelaunch
        // case; absence of an attempt is not proof for any other failure.
        let prelaunch_blocked = h.attempt_count == 0
            && h.attempt_id.is_none()
            && expected_attempt_id.is_none()
            && h.failure_reason.as_deref() == Some("goal_not_active");
        if goal.revision != expected_goal_revision
            || goal.phase != HostGoalPhase::Blocked
            || h.status != "failed"
            || h.agent_id.as_deref() != Some(agent_id)
            || h.project != project
            || h.source_agent_run_id.as_deref() != Some(source.run_id())
            || h.source_context_epoch != source.run_id()
            || h.execution_owner_epoch != source.run_id()
            || h.attempt_id.as_deref() != expected_attempt_id
            || h.committed_at.is_some()
            || h.successor_agent_run_id.is_some()
            || h.pending_agent_run_id.is_some()
            || h.candidate_context_epoch.is_some()
            || h.prepared_at.is_some()
            || h.prepared_package_sha256.is_some()
            || h.deadline_at.is_some()
            || (h.attempt_count == 0 && !prelaunch_blocked)
            || h.attempt_count >= 4
            || goal.current_attempt_id.is_some()
            || unresolved(&tx, goal_id)?
        {
            return Err(denied());
        }
        let candidate_live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_agent_runs WHERE agent_id=?1 AND previous_run_id=?2 AND state<>'revoked')",
            params![agent_id,source.run_id()], |r| r.get(0),
        ).map_err(unavailable)?;
        let stopped: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_host_context_states s JOIN wc_host_context_meters m USING(owner_key,context_epoch)
             WHERE owner_key=?1 AND context_epoch=?2 AND state=4 AND identity_source='agent_run'
               AND retirement_goal_id=?3 AND checkpoint_sequence=?4 AND retired_at IS NULL AND resume_started_json IS NULL)",
            params![owner,source.run_id(),goal_id,h.checkpoint_sequence], |r| r.get(0),
        ).map_err(unavailable)?;
        if candidate_live || !stopped {
            return Err(denied());
        }
        crate::agent_execution_close::require_agent_run_execution_closed_on_connection(
            &tx,
            owner_user,
            project,
            &webcodex_core::runner_protocol::AgentRunRef {
                agent_id: agent_id.into(),
                agent_run_id: source.run_id().into(),
                generation: source.generation(),
            },
        )
        .map_err(|e| GoalStoreError::new(e.code(), "Original execution has not closed"))?;

        let checkpoint: HostGoalCheckpoint = decode(tx.query_row(
            "SELECT snapshot_json FROM wc_host_goal_checkpoints WHERE goal_id=?1 AND sequence=?2",
            params![goal_id,h.checkpoint_sequence], |r| r.get(0),
        ).map_err(unavailable)?)?;
        if checkpoint.goal_id != goal_id
            || checkpoint.plan_version != goal.plan_version
            || h.plan_version != goal.plan_version
            || checkpoint.action.as_deref() != Some("handoff")
            || checkpoint.identity_source.as_deref() != Some("agent_run")
            || checkpoint.source_context_epoch.as_deref() != Some(source.run_id())
            || checkpoint.revision.checked_add(1) != Some(goal.revision)
            || checkpoint.source_fence.and_then(|f| f.checked_add(1)) != Some(goal.fence)
        {
            return Err(denied());
        }
        // Recover the exact phase saved by the original checkpoint operation;
        // do not infer it from plan flags or a latest/global Goal pointer.
        let saved = tx
            .prepare(
                "SELECT result_json FROM wc_host_goal_replays WHERE owner_key=?1 AND scope=?2
             AND json_extract(result_json,'$.goal.goal_id')=?3
             AND json_extract(result_json,'$.goal.revision')=?4 LIMIT 2",
            )
            .map_err(unavailable)?
            .query_map(
                params![
                    owner,
                    format!("{goal_id}:checkpoint"),
                    goal_id,
                    checkpoint.revision
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        if saved.len() != 1 {
            return Err(denied());
        }
        let prior: HostGoalMutation = decode(saved[0].clone())?;
        if prior.goal.project != project
            || prior.goal.plan_version != goal.plan_version
            || prior.goal.state != checkpoint.state
            || Some(prior.goal.fence) != checkpoint.source_fence
        {
            return Err(denied());
        }
        goal.phase = match prior.goal.phase {
            HostGoalPhase::Planning
            | HostGoalPhase::AwaitingConfirmation
            | HostGoalPhase::Ready => prior.goal.phase,
            HostGoalPhase::Blocked if prelaunch_blocked => HostGoalPhase::Blocked,
            // A fenced node may already have caused effects. This narrow
            // recovery cannot infer whether repeating that step is safe.
            _ => return Err(denied()),
        };
        goal.state.next_step = checkpoint.state.next_step.clone();
        save(&tx, &mut goal, now)?;
        let mut next_checkpoint = add_checkpoint(
            &tx,
            &goal,
            "Explicit operator recovery of failed handoff",
            now,
        )?;
        next_checkpoint.action = Some("handoff".into());
        next_checkpoint.source_context_epoch = Some(source.run_id().into());
        next_checkpoint.identity_source = Some("agent_run".into());
        next_checkpoint.source_fence = Some(goal.fence);
        tx.execute(
            "UPDATE wc_host_goal_checkpoints SET snapshot_json=?2 WHERE sequence=?1",
            params![next_checkpoint.sequence, encode(&next_checkpoint)?],
        )
        .map_err(unavailable)?;
        let mut next = h.clone();
        next.handoff_id = format!("hhandoff_{}", uuid::Uuid::new_v4().simple());
        next.checkpoint_sequence = next_checkpoint.sequence;
        next.status = "waiting".into();
        next.attempt_id = None;
        next.failure_reason = None;
        next.created_at = now;
        // Preserve attempt_count and timeout_secs: recovery cannot refill the
        // four-attempt budget. Source remains draining, never business-active.
        let changed=tx.execute(
            "UPDATE wc_host_context_states SET state=2,checkpoint_sequence=?5
             WHERE owner_key=?1 AND context_epoch=?2 AND state=4 AND retirement_goal_id=?3 AND checkpoint_sequence=?4",
            params![owner,source.run_id(),goal_id,h.checkpoint_sequence,next_checkpoint.sequence],
        ).map_err(unavailable)?;
        if changed != 1 {
            return Err(denied());
        }
        persist(&tx, &owner, &next)?;
        remember(
            &tx,
            &owner,
            &format!("{goal_id}:checkpoint"),
            &format!("recovery:{}", hash(idempotency_key)),
            &fingerprint,
            &HostGoalMutation {
                goal: goal.clone(),
                replayed: false,
            },
        )?;
        remember(
            &tx,
            &owner,
            &scope,
            idempotency_key,
            &fingerprint,
            &RecoveryReceipt {
                previous: h,
                handoff_id: next.handoff_id.clone(),
            },
        )?;
        tx.commit().map_err(unavailable)?;
        Ok((next, false, goal.revision, source))
    }
}

#[cfg(test)]
#[path = "host_goal_handoff_recovery_tests.rs"]
mod tests;
