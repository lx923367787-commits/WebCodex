//! Never-dispatched recovery only. Included as a child of execution_factory.
use super::*;

pub(super) fn ensure_resume_schema(conn: &Connection) -> anyhow::Result<()> {
    let columns = conn
        .prepare("PRAGMA table_info(wc_team_cli_attempts)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns
        .iter()
        .any(|c| c == "execution_preparation_history_json")
    {
        conn.execute_batch("ALTER TABLE wc_team_cli_attempts ADD COLUMN execution_preparation_history_json TEXT NOT NULL DEFAULT '[]'")?;
    }
    conn.execute_batch("DROP TRIGGER IF EXISTS wc_cli_preparation_immutable;
      CREATE TRIGGER wc_cli_preparation_immutable BEFORE UPDATE OF execution_preparation_json ON wc_team_cli_attempts
      WHEN OLD.execution_preparation_json IS NOT NULL AND NEW.execution_preparation_json IS NOT OLD.execution_preparation_json
      AND NOT (OLD.execution_preparation_state='cancelled' AND NEW.execution_preparation_state='prepared'
        AND OLD.execution_start_json IS NULL AND json_extract(OLD.record_json,'$.execution_binding') IS NULL
        AND json_array_length(NEW.execution_preparation_history_json)=json_array_length(OLD.execution_preparation_history_json)+1
        AND json_extract(NEW.execution_preparation_history_json,'$[#-1]')=json(OLD.execution_preparation_json)
        AND NOT EXISTS(SELECT 1 FROM json_each(OLD.execution_preparation_history_json) old_history WHERE json_extract(NEW.execution_preparation_history_json,'$['||old_history.key||']') IS NOT json(old_history.value)))
      BEGIN SELECT RAISE(ABORT,'CLI preparation is immutable'); END;")?;
    Ok(())
}

impl Database {
    /// Read-only restart discovery for the exact original logical requester.
    /// This digest is only a compare-and-swap fence, never a send permission.
    pub fn cancelled_agent_cli_preparation_hash(
        &self,
        current: &VerifiedAgentRun,
        attempt_id: &str,
    ) -> Result<String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(store_error)?;
        let raw:Option<String>=tx.query_row("SELECT execution_preparation_json FROM wc_team_cli_attempts WHERE attempt_id=?1 AND execution_preparation_state='cancelled' AND execution_start_json IS NULL AND json_extract(record_json,'$.execution_binding') IS NULL AND json_extract(record_json,'$.job_id') IS NULL AND json_extract(record_json,'$.dispatch_state')='terminal' AND json_extract(record_json,'$.execution_state')='not_started' AND json_extract(record_json,'$.error_code')='agent_execution_closed'",[attempt_id],|r|r.get(0)).optional().map_err(store_error)?;
        let old: AgentExecutionBinding = decode(&raw.ok_or_else(denied)?)?;
        if old.project != current.project() || old.requester.run.agent_id != current.agent_id() {
            return Err(denied());
        }
        source_authority(
            &tx,
            current,
            &old.requester.root_task_id,
            &old.requester.source_task_id,
            false,
        )?;
        old.identity_hash().map_err(|_| denied())
    }

    /// Trusted controller entry, never a fresh logical CLI call. The explicit
    /// prior identity hash is a CAS fence, not a new idempotency or budget key.
    pub fn resume_cancelled_agent_cli_preparation(
        &self,
        requester: &VerifiedAgentRun,
        attempt_id: &str,
        prior_identity_hash: &str,
    ) -> Result<AgentCliPreparationResult> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let (raw,phase,history,start):(String,String,String,Option<String>)=tx.query_row("SELECT execution_preparation_json,execution_preparation_state,execution_preparation_history_json,execution_start_json FROM wc_team_cli_attempts WHERE attempt_id=?1",[attempt_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(store_error)?.ok_or_else(denied)?;
        let old: AgentExecutionBinding = decode(&raw)?;
        let mut history: Vec<AgentExecutionBinding> = decode(&history)?;
        if old.project != requester.project() || old.requester.run.agent_id != requester.agent_id()
        {
            return Err(denied());
        }
        let auth = source_authority(
            &tx,
            requester,
            &old.requester.root_task_id,
            &old.requester.source_task_id,
            true,
        )?;
        ensure_agent_run_execution_open_on_connection(&tx, &run_ref(requester))
            .map_err(auth_error)?;
        let mut attempt = attempt_on(&tx, &auth.communication_principal, attempt_id)?;
        if phase == "prepared"
            && old.requester.run == run_ref(requester)
            && attempt.dispatch_state == TeamCliDispatchState::Reserved
            && history
                .last()
                .is_some_and(|h| h.identity_hash().ok().as_deref() == Some(prior_identity_hash))
        {
            ensure_agent_run_execution_open_on_connection(&tx, &old.executor.run)
                .map_err(auth_error)?;
            let worker = verify_agent_run_context_on_connection(
                &tx,
                &crate::agent_isolation::host_goal_owner_key(
                    requester.project(),
                    &old.executor.run.agent_id,
                ),
                &old.executor.run.agent_run_id,
            )
            .map_err(auth_error)?;
            return Ok(AgentCliPreparationResult {
                receipt: receipt(&attempt),
                phase,
                preparation: Some(AgentCliPreparation {
                    identity: old,
                    worker,
                }),
            });
        }
        if phase != "cancelled"
            || start.is_some()
            || attempt.execution_binding.is_some()
            || attempt.job_id.is_some()
            || attempt.dispatch_state != TeamCliDispatchState::Terminal
            || attempt.execution_state != Some(ShellCommandExecutionState::NotStarted)
            || attempt.error_code.as_deref() != Some("agent_execution_closed")
            || old.identity_hash().map_err(|_| denied())? != prior_identity_hash
            || old.requester.run == run_ref(requester)
            || history.len() >= 32
        {
            return Err(denied());
        }
        // Reuse the original frozen plan and slot. No reserve, counter, key,
        // attempt ID, WorkerTask, Agent, or enrollment is created here.
        load_team_cli_attempt_for_fallback(&tx, &auth.communication_principal, attempt_id)?;
        if old.requester.call_index >= auth.policy.max_cli_calls {
            return Err(denied());
        }
        let worker = crate::agent_isolation::recover_cancelled_cli_worker_on_connection(
            &tx, requester, &old,
        )
        .map_err(auth_error)?;
        let mut next = old.clone();
        next.requester.run = run_ref(requester);
        next.executor.run = run_ref(&worker);
        next.executor.cli_session_scope_id =
            format!("wc_cli_scope_{}", uuid::Uuid::new_v4().simple());
        next.identity_hash().map_err(|_| denied())?;
        history.push(old);
        attempt.dispatch_state = TeamCliDispatchState::Reserved;
        attempt.execution_state = None;
        attempt.error_code = None;
        attempt.result_summary = None;
        update_attempt(&tx, &auth.communication_principal, &mut attempt)?;
        let changed=tx.execute("UPDATE wc_team_cli_attempts SET execution_preparation_json=?2,execution_preparation_state='prepared',execution_preparation_history_json=?3 WHERE attempt_id=?1 AND execution_preparation_state='cancelled' AND execution_preparation_json=?4 AND execution_start_json IS NULL",params![attempt_id,encode(&next)?,encode(&history)?,raw]).map_err(store_error)?;
        if changed != 1 {
            return Err(denied());
        }
        tx.commit().map_err(store_error)?;
        Ok(AgentCliPreparationResult {
            receipt: receipt(&attempt),
            phase: "prepared".into(),
            preparation: Some(AgentCliPreparation {
                identity: next,
                worker,
            }),
        })
    }
}
