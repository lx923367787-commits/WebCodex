//! Durable conversation bookkeeping. Window bindings can only restrict an
//! invocation; neither a binding nor a meter grants project authority.
use super::Database;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

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

fn bounded(value: &str, limit: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= limit && !value.contains('\0'),
        "Invalid context identity"
    );
    Ok(())
}

impl Database {
    pub(super) fn ensure_host_goal_context_schema(conn: &Connection) -> anyhow::Result<()> {
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
        Ok(())
    }

    pub fn bind_host_goal_window(
        &self,
        owner: &str,
        window: &str,
        binding: &HostGoalWindowBinding,
    ) -> anyhow::Result<()> {
        bounded(owner, 512)?;
        bounded(window, 128)?;
        bounded(&binding.goal_id, 128)?;
        bounded(&binding.project, 512)?;
        bounded(&binding.project_root, 4096)?;
        bounded(&binding.context_epoch, 128)?;
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM wc_host_goal_windows", [], |r| {
            r.get(0)
        })?;
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM wc_host_goal_windows WHERE owner_key=?1 AND window_key=?2)", params![owner, window], |r| r.get(0))?;
        anyhow::ensure!(
            exists || count < 32768,
            "Goal conversation binding capacity reached"
        );
        conn.execute("INSERT INTO wc_host_goal_windows(owner_key,window_key,binding_json) VALUES(?1,?2,?3) ON CONFLICT(owner_key,window_key) DO UPDATE SET binding_json=excluded.binding_json", params![owner, window, serde_json::to_string(binding)?])?;
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
        let conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        // Do not let a late exit request remove a newer explicitly selected goal.
        conn.execute("DELETE FROM wc_host_goal_windows WHERE owner_key=?1 AND window_key=?2 AND json_extract(binding_json,'$.goal_id')=?3", params![owner, window, goal_id])?;
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
        bounded(owner, 512)?;
        bounded(epoch, 128)?;
        bounded(event_id, 128)?;
        let (warning, handoff) = thresholds.unwrap_or((24_000, 32_000));
        anyhow::ensure!(
            (1000..=10_000_000).contains(&warning) && handoff > warning && handoff <= 20_000_000,
            "Invalid context warning thresholds"
        );
        let mut conn = self.conn.lock().unwrap();
        Self::ensure_host_goal_context_schema(&conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        if thresholds.is_some() {
            tx.execute("UPDATE wc_host_context_meters SET warning_tokens=?3,handoff_tokens=?4 WHERE owner_key=?1 AND context_epoch=?2",params![owner,epoch,warning as i64,handoff as i64])?;
        }
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
