//! Durable conversation bookkeeping. Window bindings can only restrict an
//! invocation; neither a binding nor a meter grants project authority.
use super::{Database, GoalStoreError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use webcodex_core::host_goal::HostGoalInvocationContext;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostGoalWindowBinding {
    pub goal_id: String,
    pub project: String,
    pub project_root: String,
    pub context_epoch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostContextMeter {
    pub context_epoch: String,
    pub observed_tokens: u64,
    pub warning_tokens: u64,
    pub handoff_tokens: u64,
    pub level: String,
    pub newly_crossed: bool,
    pub unmeasured_modalities: bool,
    pub sample_count: u64,
}

/// Durable state is monotonic; threshold updates and bootstrap cannot reactivate it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostContextState {
    pub context_epoch: String,
    pub state: String,
    pub identity_source: Option<String>,
    pub retirement_goal_id: Option<String>,
    pub checkpoint_sequence: Option<u64>,
    pub retired_at: Option<i64>,
}

fn context_storage(error: impl std::fmt::Display) -> GoalStoreError {
    tracing::warn!(error = %error, "host context storage failed");
    GoalStoreError::new("goal_store_unavailable", "Context state is unavailable")
}

pub(super) fn context_gate(
    conn: &Connection,
    owner: &str,
    context: Option<&HostGoalInvocationContext>,
    allow_draining: bool,
) -> Result<(), GoalStoreError> {
    let Some(context) = context else {
        return Ok(());
    };
    agent_run_context_gate(conn, owner, Some(context))?;
    for value in [&context.context_epoch, &context.identity_source] {
        bounded(value, 128)
            .map_err(|_| GoalStoreError::new("goal_context_missing", "Invalid context identity"))?;
    }
    if !matches!(
        context.identity_source.as_str(),
        "window_bound" | "explicit_context_only" | "agent_run"
    ) {
        return Err(GoalStoreError::new(
            "goal_context_identity_mismatch",
            "Unsupported conversation identity source",
        ));
    }
    let stored: Option<(i64,Option<String>)> = conn.query_row(
        "SELECT s.state,s.identity_source FROM wc_host_context_states s JOIN wc_host_context_meters m USING(owner_key,context_epoch) WHERE s.owner_key=?1 AND s.context_epoch=?2",
        params![owner,context.context_epoch], |r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(context_storage)?;
    if stored
        .as_ref()
        .and_then(|(_, source)| source.as_deref())
        .is_some_and(|source| source != context.identity_source)
    {
        return Err(GoalStoreError::new("goal_context_identity_mismatch","The conversation epoch was registered with a different identity source; use a fresh conversation identity"));
    }
    match stored.map(|(state, _)| state) {
        None => {
            return Err(GoalStoreError::new(
                "goal_context_missing",
                "A measured context identity is required",
            ))
        }
        Some(3) => {
            return Err(GoalStoreError::new(
                "goal_chat_retired",
                "The source conversation is retired and read-only",
            ))
        }
        Some(4) => {
            return Err(GoalStoreError::new(
                "goal_handoff_failed",
                "Handoff failed; this execution is stopped and requires explicit user recovery",
            ))
        }
        Some(2) if !allow_draining => {
            return Err(GoalStoreError::new(
                "goal_context_draining",
                "The source conversation is draining; checkpoint or finish the current attempt",
            ))
        }
        _ => {}
    }
    conn.execute("UPDATE wc_host_context_states SET identity_source=COALESCE(identity_source,?3) WHERE owner_key=?1 AND context_epoch=?2", params![owner,context.context_epoch,context.identity_source]).map_err(context_storage)?;
    Ok(())
}

/// Run fencing precedes replay as well as ordinary mutation. Unlike the
/// complete context gate, it does not alter historical window replay semantics.
pub(super) fn agent_run_context_gate(
    conn: &Connection,
    owner: &str,
    context: Option<&HostGoalInvocationContext>,
) -> Result<(), GoalStoreError> {
    if let Some(context) = context.filter(|context| context.identity_source == "agent_run") {
        super::agent_isolation::verify_agent_run_context_on_connection(
            conn,
            owner,
            &context.context_epoch,
        )
        .map_err(|_| GoalStoreError::new("agent_run_invalid", "Agent run is unavailable"))?;
    }
    Ok(())
}

pub(super) fn agent_run_goal_context_gate(
    conn: &Connection,
    owner: &str,
    context: Option<&HostGoalInvocationContext>,
    goal_id: Option<&str>,
) -> Result<(), GoalStoreError> {
    if let Some(context) = context.filter(|context| context.identity_source == "agent_run") {
        let binding = super::agent_isolation::verify_agent_run_context_on_connection(
            conn,
            owner,
            &context.context_epoch,
        )
        .map_err(|e| GoalStoreError::new(e.code(), "Agent run is unavailable"))?;
        super::agent_isolation::agent_run_goal_scope_on_connection(conn, &binding, goal_id)
            .map_err(|e| {
                GoalStoreError::new(
                    e.code(),
                    "Agent run is limited to its authorized handoff Goal",
                )
            })?;
    }
    Ok(())
}

/// Closeout permission cannot be redirected to a second owned Goal while draining.
pub(super) fn context_goal_gate(
    conn: &Connection,
    owner: &str,
    context: Option<&HostGoalInvocationContext>,
    goal_id: &str,
    allow_draining: bool,
) -> Result<(), GoalStoreError> {
    agent_run_goal_context_gate(conn, owner, context, Some(goal_id))?;
    let handoff: Option<(String, String, Option<String>)> = conn.query_row(
        "SELECT json_extract(snapshot_json,'$.status'),json_extract(snapshot_json,'$.execution_owner_epoch'),json_extract(snapshot_json,'$.successor_agent_run_id') FROM wc_host_goal_handoffs WHERE owner_key=?1 AND goal_id=?2",
        params![owner, goal_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(context_storage)?;
    if let Some((status, execution_owner, successor_run)) = handoff {
        if status != "committed" {
            return Err(GoalStoreError::new(if status == "failed" {"goal_handoff_failed"} else {"goal_handoff_pending"}, "Goal execution is suspended during handoff; only handoff recovery reads and confirmation are allowed"));
        }
        let owns = match context {
            Some(c) if c.identity_source == "agent_run" => {
                let binding = super::agent_isolation::verify_agent_run_context_on_connection(
                    conn,
                    owner,
                    &c.context_epoch,
                )
                .map_err(|e| GoalStoreError::new(e.code(), "Agent run is unavailable"))?;
                successor_run.as_deref() == Some(binding.run_id())
                    && super::agent_isolation::agent_run_task_goal_on_connection(conn, &binding)
                        .map_err(|e| GoalStoreError::new(e.code(), "Agent run is unavailable"))?
                        .as_deref()
                        == Some(goal_id)
            }
            Some(c) => {
                successor_run.is_none()
                    && c.identity_source == "window_bound"
                    && c.context_epoch == execution_owner
            }
            None => false,
        };
        if !owns {
            return Err(GoalStoreError::new(
                "goal_execution_owner_mismatch",
                "Only the server-bound current Goal execution owner may mutate this Goal",
            ));
        }
    }
    context_gate(conn, owner, context, allow_draining)?;
    if let Some(context) = context {
        let draining: bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_context_states WHERE owner_key=?1 AND context_epoch=?2 AND state=2)",params![owner,context.context_epoch],|r|r.get(0)).map_err(context_storage)?;
        if draining {
            let bound: bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_windows WHERE owner_key=?1 AND json_extract(binding_json,'$.context_epoch')=?2 AND json_extract(binding_json,'$.goal_id')=?3)",params![owner,context.context_epoch,goal_id],|r|r.get(0)).map_err(context_storage)?;
            if !bound {
                return Err(GoalStoreError::new(
                    "goal_context_draining",
                    "A draining conversation may close out only its already bound Goal",
                ));
            }
        }
    }
    Ok(())
}

fn bounded(value: &str, limit: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= limit && !value.contains('\0'),
        "Invalid context identity"
    );
    Ok(())
}

impl Database {
    /// Construct and initialize the Agent run context without trusting model
    /// metadata. Existing thresholds and retired/draining state remain intact.
    pub fn ensure_agent_host_context(
        &self,
        binding: &super::VerifiedAgentRun,
        thresholds: Option<(u64, u64)>,
    ) -> Result<HostGoalInvocationContext, GoalStoreError> {
        let (warning, handoff) = thresholds.unwrap_or((600_000, 800_000));
        if !(1000..=10_000_000).contains(&warning) || handoff <= warning || handoff > 20_000_000 {
            return Err(GoalStoreError::new(
                "invalid_context_thresholds",
                "Invalid context thresholds",
            ));
        }
        let owner = binding.host_goal_owner_key();
        let epoch = binding.run_id();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(context_storage)?;
        super::agent_isolation::validate_agent_run_on_connection(&tx, binding)
            .map_err(|_| GoalStoreError::new("agent_run_invalid", "Agent run is unavailable"))?;
        let existing:Option<Option<String>>=tx.query_row(
            "SELECT identity_source FROM wc_host_context_states WHERE owner_key=?1 AND context_epoch=?2",
            params![owner,epoch],|row|row.get(0)).optional().map_err(context_storage)?;
        if existing
            .as_ref()
            .and_then(|source| source.as_deref())
            .is_some_and(|source| source != "agent_run")
        {
            return Err(GoalStoreError::new(
                "goal_context_identity_mismatch",
                "Context identity source differs",
            ));
        }
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM wc_host_context_meters", [], |row| {
                row.get(0)
            })
            .map_err(context_storage)?;
        if existing.is_none() && count >= 32768 {
            return Err(GoalStoreError::new(
                "goal_store_unavailable",
                "Context capacity exhausted",
            ));
        }
        tx.execute(
            "INSERT OR IGNORE INTO wc_host_context_meters VALUES(?1,?2,0,?3,?4,0,0,0)",
            params![owner, epoch, warning as i64, handoff as i64],
        )
        .map_err(context_storage)?;
        tx.execute("INSERT INTO wc_host_context_states(owner_key,context_epoch,state,identity_source) VALUES(?1,?2,0,'agent_run') ON CONFLICT(owner_key,context_epoch) DO UPDATE SET identity_source=COALESCE(identity_source,'agent_run')",
            params![owner,epoch]).map_err(context_storage)?;
        tx.commit().map_err(context_storage)?;
        Ok(HostGoalInvocationContext {
            context_epoch: epoch.into(),
            identity_source: "agent_run".into(),
        })
    }

    pub(super) fn ensure_host_goal_context_schema(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_host_goal_handoffs (owner_key TEXT NOT NULL, goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id), snapshot_json TEXT NOT NULL, PRIMARY KEY(owner_key,goal_id));")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wc_host_goal_windows (
            owner_key TEXT NOT NULL, window_key TEXT NOT NULL, binding_json TEXT NOT NULL,
            PRIMARY KEY(owner_key, window_key));
            CREATE TABLE IF NOT EXISTS wc_host_context_meters (
            owner_key TEXT NOT NULL, context_epoch TEXT NOT NULL, tokens INTEGER NOT NULL,
            warning_tokens INTEGER NOT NULL, handoff_tokens INTEGER NOT NULL,
            warned_level INTEGER NOT NULL, unmeasured INTEGER NOT NULL, samples INTEGER NOT NULL,
            PRIMARY KEY(owner_key, context_epoch));
            CREATE TABLE IF NOT EXISTS wc_host_context_samples (
            owner_key TEXT NOT NULL, context_epoch TEXT NOT NULL, event_id TEXT NOT NULL,
            PRIMARY KEY(owner_key, context_epoch, event_id));
            CREATE TABLE IF NOT EXISTS wc_host_goal_context_refs (
            owner_key TEXT NOT NULL, goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),
            context_epoch TEXT NOT NULL, PRIMARY KEY(owner_key, goal_id));
            CREATE TABLE IF NOT EXISTS wc_host_goal_context_reads (
            owner_key TEXT NOT NULL, context_epoch TEXT NOT NULL, goal_id TEXT NOT NULL REFERENCES wc_host_goals(goal_id),
            revision INTEGER NOT NULL, plan_version INTEGER NOT NULL, PRIMARY KEY(owner_key,context_epoch,goal_id));",
        )?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_host_context_states (
            owner_key TEXT NOT NULL, context_epoch TEXT NOT NULL, state INTEGER NOT NULL,
            identity_source TEXT, retirement_goal_id TEXT, checkpoint_sequence INTEGER, retired_at INTEGER,
            PRIMARY KEY(owner_key,context_epoch));
            INSERT OR IGNORE INTO wc_host_context_states(owner_key,context_epoch,state)
            SELECT owner_key,context_epoch,CASE WHEN tokens>=handoff_tokens THEN 2 WHEN tokens>=warning_tokens THEN 1 ELSE 0 END FROM wc_host_context_meters;")?;
        let has_node: bool = conn
            .prepare("PRAGMA table_info(wc_host_goal_context_reads)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "node_id");
        if !has_node {
            // Old whole-goal receipts cannot authorize any specific node.
            conn.execute("ALTER TABLE wc_host_goal_context_reads ADD COLUMN node_id TEXT NOT NULL DEFAULT ''", [])?;
        }
        let has_resume: bool = conn
            .prepare("PRAGMA table_info(wc_host_context_states)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "resume_started_json");
        if !has_resume {
            // An old binding or context read is not evidence of a successor bootstrap.
            conn.execute(
                "ALTER TABLE wc_host_context_states ADD COLUMN resume_started_json TEXT",
                [],
            )?;
        }
        Ok(())
    }

    /// Initialize only bookkeeping; an existing context's state and thresholds survive.
    pub fn ensure_host_context_identity(
        &self,
        owner: &str,
        context: &HostGoalInvocationContext,
    ) -> Result<(), GoalStoreError> {
        bounded(&context.identity_source, 128).map_err(|_| {
            GoalStoreError::new("goal_context_missing", "Invalid context identity source")
        })?;
        if !matches!(
            context.identity_source.as_str(),
            "window_bound" | "explicit_context_only"
        ) {
            return Err(GoalStoreError::new(
                "goal_context_identity_mismatch",
                "Unsupported conversation identity source",
            ));
        }
        self.observe_host_context(
            owner,
            &context.context_epoch,
            "context-identity-init",
            0,
            false,
            None,
        )
        .map_err(context_storage)?;
        let conn = self.conn.lock().unwrap();
        // Identity initialization is also valid for read-only recovery of a retired source.
        conn.execute("UPDATE wc_host_context_states SET identity_source=COALESCE(identity_source,?3) WHERE owner_key=?1 AND context_epoch=?2",params![owner,context.context_epoch,context.identity_source]).map_err(context_storage)?;
        Ok(())
    }

    pub fn read_host_context_state(
        &self,
        owner: &str,
        epoch: &str,
    ) -> Result<Option<HostContextState>, GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn).map_err(context_storage)?;
        conn.query_row("SELECT state,identity_source,retirement_goal_id,checkpoint_sequence,retired_at FROM wc_host_context_states WHERE owner_key=?1 AND context_epoch=?2",params![owner,epoch],|r| {
            let state: i64 = r.get(0)?;
            Ok(HostContextState {context_epoch:epoch.into(),state:match state {4=>"stopped",3=>"retired",2=>"draining",1=>"warning",_=>"active"}.into(),identity_source:r.get(1)?,retirement_goal_id:r.get(2)?,checkpoint_sequence:r.get(3)?,retired_at:r.get(4)?})
        }).optional().map_err(context_storage)
    }

    pub fn gate_host_context(
        &self,
        owner: &str,
        context: Option<&HostGoalInvocationContext>,
        allow_draining: bool,
    ) -> Result<(), GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn).map_err(context_storage)?;
        context_gate(&conn, owner, context, allow_draining)
    }

    pub fn gate_host_goal_execution(
        &self,
        owner: &str,
        goal_id: &str,
        context: Option<&HostGoalInvocationContext>,
    ) -> Result<(), GoalStoreError> {
        let conn = self.conn.lock().unwrap();
        context_goal_gate(&conn, owner, context, goal_id, false)
    }

    pub fn bind_host_goal_window(
        &self,
        owner: &str,
        window: &str,
        binding: &HostGoalWindowBinding,
    ) -> anyhow::Result<()> {
        self.bind_host_goal_window_in_context(owner, window, binding, None)
    }

    pub fn bind_host_goal_window_in_context(
        &self,
        owner: &str,
        window: &str,
        binding: &HostGoalWindowBinding,
        context: Option<&HostGoalInvocationContext>,
    ) -> anyhow::Result<()> {
        bounded(owner, 512)?;
        bounded(window, 128)?;
        bounded(&binding.goal_id, 128)?;
        bounded(&binding.project, 512)?;
        bounded(&binding.project_root, 4096)?;
        bounded(&binding.context_epoch, 128)?;
        anyhow::ensure!(
            context.is_none_or(|c| c.context_epoch == binding.context_epoch),
            "Binding context must match resolved invocation identity"
        );
        let mut conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        context_gate(&tx, owner, context, false)?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM wc_host_goal_windows", [], |r| {
            r.get(0)
        })?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_windows WHERE owner_key=?1 AND window_key=?2)", params![owner, window], |r| r.get(0))?;
        anyhow::ensure!(
            exists || count < 32768,
            "Goal conversation binding capacity reached"
        );
        tx.execute("INSERT INTO wc_host_goal_windows(owner_key,window_key,binding_json) VALUES(?1,?2,?3) ON CONFLICT(owner_key,window_key) DO UPDATE SET binding_json=excluded.binding_json", params![owner, window, serde_json::to_string(binding)?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn host_goal_window(
        &self,
        owner: &str,
        window: &str,
    ) -> anyhow::Result<Option<HostGoalWindowBinding>> {
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let value: Option<String> = conn.query_row("SELECT binding_json FROM wc_host_goal_windows WHERE owner_key=?1 AND window_key=?2", params![owner, window], |r| r.get(0)).optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn unbind_host_goal_window(
        &self,
        owner: &str,
        window: &str,
        goal_id: &str,
    ) -> anyhow::Result<()> {
        self.unbind_host_goal_window_in_context(owner, window, goal_id, None)
    }

    pub fn unbind_host_goal_window_in_context(
        &self,
        owner: &str,
        window: &str,
        goal_id: &str,
        context: Option<&HostGoalInvocationContext>,
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        context_gate(&tx, owner, context, false)?;
        // Do not let a late exit request remove a newer explicitly selected goal.
        tx.execute("DELETE FROM wc_host_goal_windows WHERE owner_key=?1 AND window_key=?2 AND json_extract(binding_json,'$.goal_id')=?3", params![owner, window, goal_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Record only a real ChatGPT conversation's latest activity. Console reads
    /// have no chat epoch and must never replace this pointer with their cookie.
    pub fn record_host_goal_context_ref(
        &self,
        owner: &str,
        goal_id: &str,
        epoch: &str,
    ) -> anyhow::Result<()> {
        bounded(owner, 512)?;
        bounded(goal_id, 128)?;
        bounded(epoch, 128)?;
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let owned: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wc_host_goals WHERE owner_key=?1 AND goal_id=?2)",
            params![owner, goal_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(owned, "Goal is not visible to this context owner");
        let metered: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2)",params![owner,epoch],|row|row.get(0))?;
        anyhow::ensure!(metered, "Conversation meter is unavailable");
        conn.execute("INSERT INTO wc_host_goal_context_refs(owner_key,goal_id,context_epoch) VALUES(?1,?2,?3) ON CONFLICT(owner_key,goal_id) DO UPDATE SET context_epoch=excluded.context_epoch", params![owner,goal_id,epoch])?;
        Ok(())
    }

    /// A read-only projection for the target panel: does not count a sample or
    /// consume a threshold's first-warning state in the real conversation.
    pub fn read_host_goal_context_meter(
        &self,
        owner: &str,
        goal_id: &str,
    ) -> anyhow::Result<Option<HostContextMeter>> {
        bounded(owner, 512)?;
        bounded(goal_id, 128)?;
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        conn.query_row("SELECT m.context_epoch,m.tokens,m.warning_tokens,m.handoff_tokens,m.unmeasured,m.samples FROM wc_host_goal_context_refs r JOIN wc_host_context_meters m ON m.owner_key=r.owner_key AND m.context_epoch=r.context_epoch JOIN wc_host_goals g ON g.goal_id=r.goal_id AND g.owner_key=r.owner_key WHERE r.owner_key=?1 AND r.goal_id=?2", params![owner,goal_id], |row| {
            let tokens: u64 = row.get(1)?; let warning: u64 = row.get(2)?; let handoff: u64 = row.get(3)?;
            Ok(HostContextMeter {context_epoch:row.get(0)?,observed_tokens:tokens,warning_tokens:warning,handoff_tokens:handoff,
                level:if tokens>=handoff {"handoff_recommended"} else if tokens>=warning {"warning"} else {"normal"}.into(),
                newly_crossed:false,unmeasured_modalities:row.get(4)?,sample_count:row.get(5)?})
        }).optional().map_err(Into::into)
    }

    /// Record delivery of this node's complete bounded requirements, never a
    /// summary or arbitrary detail page. Older reads cannot replace newer ones.
    pub fn ack_host_goal_context_read(
        &self,
        owner: &str,
        epoch: &str,
        goal_id: &str,
        revision: u64,
        plan_version: u64,
        node_id: &str,
    ) -> anyhow::Result<()> {
        bounded(owner, 512)?;
        bounded(epoch, 128)?;
        bounded(goal_id, 128)?;
        bounded(node_id, 64)?;
        anyhow::ensure!(
            revision > 0 && revision <= i64::MAX as u64 && plan_version <= i64::MAX as u64,
            "Invalid goal read version"
        );
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let owned:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goals WHERE owner_key=?1 AND goal_id=?2 AND revision>=?3 AND json_extract(snapshot_json,'$.plan_version')>=?4)",params![owner,goal_id,revision,plan_version],|row|row.get(0))?;
        anyhow::ensure!(owned, "Goal read snapshot is unavailable to this owner");
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM wc_host_goal_context_reads",
            [],
            |row| row.get(0),
        )?;
        let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_context_reads WHERE owner_key=?1 AND context_epoch=?2 AND goal_id=?3)",params![owner,epoch,goal_id],|row|row.get(0))?;
        anyhow::ensure!(
            exists || count < 131072,
            "Goal context read capacity reached"
        );
        conn.execute("INSERT INTO wc_host_goal_context_reads(owner_key,context_epoch,goal_id,revision,plan_version,node_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(owner_key,context_epoch,goal_id) DO UPDATE SET revision=excluded.revision,plan_version=excluded.plan_version,node_id=excluded.node_id WHERE excluded.revision>=wc_host_goal_context_reads.revision",params![owner,epoch,goal_id,revision,plan_version,node_id])?;
        Ok(())
    }

    /// A complete node package delivered to the exact active Agent run. The
    /// receipt cannot be written by a retired run or for a superseded snapshot.
    pub fn ack_agent_host_goal_context_read(
        &self,
        binding: &super::VerifiedAgentRun,
        goal_id: &str,
        revision: u64,
        plan_version: u64,
        node_id: &str,
    ) -> Result<(), GoalStoreError> {
        bounded(node_id, 64).map_err(context_storage)?;
        let owner = binding.host_goal_owner_key();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(context_storage)?;
        super::agent_isolation::validate_agent_run_on_connection(&tx, binding)
            .map_err(|_| GoalStoreError::new("agent_run_invalid", "Agent run is unavailable"))?;
        super::agent_isolation::agent_run_goal_scope_on_connection(&tx, binding, Some(goal_id))
            .map_err(|e| {
                GoalStoreError::new(
                    e.code(),
                    "Agent run is limited to its authorized handoff Goal",
                )
            })?;
        let owned:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goals WHERE owner_key=?1 AND goal_id=?2 AND revision=?3 AND json_extract(snapshot_json,'$.plan_version')=?4)",params![owner,goal_id,revision,plan_version],|row|row.get(0)).map_err(context_storage)?;
        if !owned {
            return Err(GoalStoreError::new(
                "goal_context_read_unavailable",
                "Goal snapshot changed",
            ));
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM wc_host_goal_context_reads",
                [],
                |row| row.get(0),
            )
            .map_err(context_storage)?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_context_reads WHERE owner_key=?1 AND context_epoch=?2 AND goal_id=?3)",params![owner,binding.run_id(),goal_id],|row|row.get(0)).map_err(context_storage)?;
        if !exists && count >= 131072 {
            return Err(GoalStoreError::new(
                "goal_context_read_unavailable",
                "Context read capacity reached",
            ));
        }
        tx.execute("INSERT INTO wc_host_goal_context_reads(owner_key,context_epoch,goal_id,revision,plan_version,node_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(owner_key,context_epoch,goal_id) DO UPDATE SET revision=excluded.revision,plan_version=excluded.plan_version,node_id=excluded.node_id",
            params![owner,binding.run_id(),goal_id,revision,plan_version,node_id]).map_err(context_storage)?;
        tx.commit().map_err(context_storage)?;
        Ok(())
    }

    pub fn host_goal_context_read_matches(
        &self,
        owner: &str,
        epoch: &str,
        goal_id: &str,
        revision: u64,
        plan_version: u64,
        node_id: &str,
    ) -> anyhow::Result<bool> {
        bounded(owner, 512)?;
        bounded(epoch, 128)?;
        bounded(goal_id, 128)?;
        bounded(node_id, 64)?;
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_context_reads r JOIN wc_host_goals g ON g.goal_id=r.goal_id AND g.owner_key=r.owner_key WHERE r.owner_key=?1 AND r.context_epoch=?2 AND r.goal_id=?3 AND r.revision=?4 AND r.plan_version=?5 AND r.node_id=?6 AND g.revision=?4 AND json_extract(g.snapshot_json,'$.plan_version')=?5)",params![owner,epoch,goal_id,revision,plan_version,node_id],|row|row.get(0)).map_err(Into::into)
    }

    pub fn observe_host_context(
        &self,
        owner: &str,
        epoch: &str,
        event_id: &str,
        tokens: u64,
        unmeasured: bool,
        thresholds: Option<(u64, u64)>,
    ) -> anyhow::Result<HostContextMeter> {
        self.observe_host_context_impl(owner, epoch, event_id, tokens, unmeasured, thresholds, None)
    }

    /// Count only observed input/output sizes under the exact current run. The
    /// immutable existing threshold policy is shared with the Host Goal meter.
    pub fn observe_agent_host_context(
        &self,
        binding: &super::VerifiedAgentRun,
        event_id: &str,
        tokens: u64,
        unmeasured: bool,
    ) -> Result<HostContextMeter, GoalStoreError> {
        self.observe_host_context_impl(
            &binding.host_goal_owner_key(),
            binding.run_id(),
            event_id,
            tokens,
            unmeasured,
            None,
            Some(binding),
        )
        .map_err(context_storage)
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_host_context_impl(
        &self,
        owner: &str,
        epoch: &str,
        event_id: &str,
        tokens: u64,
        unmeasured: bool,
        thresholds: Option<(u64, u64)>,
        agent_run: Option<&super::VerifiedAgentRun>,
    ) -> anyhow::Result<HostContextMeter> {
        bounded(owner, 512)?;
        bounded(epoch, 128)?;
        bounded(event_id, 128)?;
        let (warning, handoff) = thresholds.unwrap_or((600_000, 800_000));
        anyhow::ensure!(
            (1000..=10_000_000).contains(&warning) && handoff > warning && handoff <= 20_000_000,
            "Invalid context warning thresholds"
        );
        let mut conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(binding) = agent_run {
            super::agent_isolation::validate_agent_run_on_connection(&tx, binding)?;
            let existing:Option<Option<String>>=tx.query_row("SELECT identity_source FROM wc_host_context_states WHERE owner_key=?1 AND context_epoch=?2",
                params![owner,epoch],|row|row.get(0)).optional()?;
            anyhow::ensure!(
                existing
                    .flatten()
                    .is_none_or(|source| source == "agent_run"),
                "Agent context source differs"
            );
        }
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2)", params![owner,epoch], |r| r.get(0))?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM wc_host_context_meters", [], |r| {
            r.get(0)
        })?;
        anyhow::ensure!(
            exists || count < 32768,
            "Conversation meter capacity reached"
        );
        tx.execute(
            "INSERT OR IGNORE INTO wc_host_context_meters VALUES(?1,?2,0,?3,?4,0,0,0)",
            params![owner, epoch, warning as i64, handoff as i64],
        )?;
        let sample_count: i64 = tx.query_row(
            "SELECT samples FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2",
            params![owner, epoch],
            |r| r.get(0),
        )?;
        let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_context_samples WHERE owner_key=?1 AND context_epoch=?2 AND event_id=?3)",params![owner,epoch,event_id],|r|r.get(0))?;
        anyhow::ensure!(
            duplicate || sample_count < 100_000,
            "Conversation meter sample capacity reached; start a new conversation"
        );
        if !duplicate {
            tx.execute(
                "INSERT INTO wc_host_context_samples VALUES(?1,?2,?3)",
                params![owner, epoch, event_id],
            )?;
            tx.execute("UPDATE wc_host_context_meters SET tokens=MIN(1000000000000,tokens+?3),unmeasured=MAX(unmeasured,?4),samples=samples+1 WHERE owner_key=?1 AND context_epoch=?2", params![owner,epoch,tokens.min(1_000_000_000_000) as i64,unmeasured as i64])?;
        }
        let (tokens,warning,handoff,previous,unmeasured,samples):(i64,i64,i64,i64,i64,i64)=tx.query_row("SELECT tokens,warning_tokens,handoff_tokens,warned_level,unmeasured,samples FROM wc_host_context_meters WHERE owner_key=?1 AND context_epoch=?2",params![owner,epoch],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        let level = if tokens >= handoff {
            2
        } else if tokens >= warning {
            1
        } else {
            0
        };
        tx.execute("UPDATE wc_host_context_meters SET warned_level=MAX(warned_level,?3) WHERE owner_key=?1 AND context_epoch=?2",params![owner,epoch,level])?;
        tx.execute("INSERT INTO wc_host_context_states(owner_key,context_epoch,state) VALUES(?1,?2,?3) ON CONFLICT(owner_key,context_epoch) DO UPDATE SET state=MAX(state,excluded.state)",params![owner,epoch,level])?;
        if agent_run.is_some() {
            tx.execute("UPDATE wc_host_context_states SET identity_source=COALESCE(identity_source,'agent_run') WHERE owner_key=?1 AND context_epoch=?2",params![owner,epoch])?;
        }
        tx.commit()?;
        Ok(HostContextMeter {
            context_epoch: epoch.into(),
            observed_tokens: tokens as u64,
            warning_tokens: warning as u64,
            handoff_tokens: handoff as u64,
            level: match level {
                2 => "handoff_recommended",
                1 => "warning",
                _ => "normal",
            }
            .into(),
            newly_crossed: level > previous,
            unmeasured_modalities: unmeasured != 0,
            sample_count: samples as u64,
        })
    }
}
