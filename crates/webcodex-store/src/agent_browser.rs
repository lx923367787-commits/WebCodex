//! Browser effects reuse the owning ordinary program's authorization and run
//! closure gate. A persistent fence survives response loss; only observation
//! may follow a fence, regardless of whether a browser receipt was delivered.
use super::*;
use webcodex_core::agent_browser::{AgentBrowserAction, AgentBrowserReceipt, BrowserStep};

pub(super) fn init(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS wc_agent_browser_actions(
        action_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, run_id TEXT NOT NULL,
        program_id TEXT NOT NULL REFERENCES wc_agent_programs(program_id),
        key_hash TEXT NOT NULL, action_json TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('prepared','dispatch_fenced','terminal','cancelled')),
        result_json TEXT, created_at INTEGER NOT NULL, UNIQUE(agent_id,key_hash));
        CREATE TRIGGER IF NOT EXISTS wc_browser_identity_immutable BEFORE UPDATE OF action_id,agent_id,run_id,program_id,key_hash,action_json ON wc_agent_browser_actions BEGIN SELECT RAISE(ABORT,'browser identity is immutable'); END;
        CREATE TRIGGER IF NOT EXISTS wc_browser_history_preserved BEFORE DELETE ON wc_agent_browser_actions BEGIN SELECT RAISE(ABORT,'browser history is preserved'); END;")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentBrowserRecord {
    pub action: AgentBrowserAction,
    pub state: String,
    pub receipt: Option<AgentBrowserReceipt>,
}
fn read(conn: &Connection, action_id: &str) -> Result<AgentBrowserRecord> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT action_json,state,result_json FROM wc_agent_browser_actions WHERE action_id=?1",
            [action_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(storage)?;
    let (raw, state, result) = row.ok_or("agent_browser_not_found")?;
    let action: AgentBrowserAction = serde_json::from_str(&raw).map_err(storage)?;
    action.validate()?;
    let receipt: Option<AgentBrowserReceipt> = result
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(storage)?;
    if let Some(receipt) = &receipt {
        receipt.validate_for(&action)?;
    }
    Ok(AgentBrowserRecord {
        action,
        state,
        receipt,
    })
}
pub trait AuthenticatedAgentBrowserReceipt {
    fn action(&self) -> &AgentBrowserAction;
    fn receipt(&self) -> &AgentBrowserReceipt;
}
impl Database {
    pub fn prepare_agent_browser(
        &self,
        run: &VerifiedAgentRun,
        program_id: &str,
        steps: Vec<BrowserStep>,
        timeout_secs: u64,
        key: &str,
    ) -> Result<AgentBrowserRecord> {
        if key.trim().is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err("agent_browser_key_invalid".into());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        validate_agent_run_on_connection(&tx, run).map_err(|e| e.code().to_string())?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT action_id FROM wc_agent_browser_actions WHERE agent_id=?1 AND key_hash=?2",
                params![run.agent_id(), hash(key)],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        if let Some(id) = existing {
            let old = read(&tx, &id)?;
            authorize(&tx, run, &old.action.service, false)?;
            if old.action.service.program_id != program_id
                || old.action.steps != steps
                || old.action.timeout_secs != timeout_secs
            {
                return Err("agent_browser_idempotency_conflict".into());
            }
            return Ok(old);
        }
        let (service, receipt) = record(&tx, program_id)?;
        authorize(&tx, run, &service.binding, true)?;
        if receipt.state != "dispatch_fenced" {
            return Err("agent_browser_service_not_running".into());
        }
        let action = AgentBrowserAction {
            action_id: format!("wc_browser_{}", uuid::Uuid::new_v4().simple()),
            service: service.binding,
            steps,
            timeout_secs,
        };
        action.validate()?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM wc_agent_browser_actions WHERE agent_id=?1",
                [run.agent_id()],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if count >= 1024 {
            return Err("agent_browser_history_capacity".into());
        }
        tx.execute("INSERT INTO wc_agent_browser_actions(action_id,agent_id,run_id,program_id,key_hash,action_json,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7)",
            params![action.action_id,run.agent_id(),run.run_id(),program_id,hash(key),serde_json::to_string(&action).map_err(storage)?,now()]).map_err(storage)?;
        let result = read(&tx, &action.action_id)?;
        tx.commit().map_err(storage)?;
        Ok(result)
    }
    pub fn read_agent_browser(
        &self,
        run: &VerifiedAgentRun,
        id: &str,
    ) -> Result<AgentBrowserRecord> {
        let conn = self.conn.lock().unwrap();
        let value = read(&conn, id)?;
        authorize(&conn, run, &value.action.service, false)?;
        Ok(value)
    }
    pub fn claim_agent_browser(
        &self,
        run: &VerifiedAgentRun,
        expected: &AgentBrowserAction,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let value = read(&tx, &expected.action_id)?;
        authorize(&tx, run, &value.action.service, false)?;
        if value.action != *expected {
            return Err("agent_browser_intent_changed".into());
        }
        if value.state != "prepared" {
            return Ok(false);
        }
        authorize(&tx, run, &expected.service, true)?;
        tx.execute(
            "UPDATE wc_agent_browser_actions SET state='dispatch_fenced' WHERE action_id=?1",
            [&expected.action_id],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(true)
    }
    pub fn record_agent_browser(
        &self,
        run: &VerifiedAgentRun,
        verified: &dyn AuthenticatedAgentBrowserReceipt,
    ) -> Result<AgentBrowserRecord> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let action = verified.action();
        let receipt = verified.receipt();
        receipt.validate_for(action)?;
        let old = read(&tx, &action.action_id)?;
        authorize(&tx, run, &old.action.service, false)?;
        if old.action != *action || old.state == "prepared" || old.state == "cancelled" {
            return Err("agent_browser_receipt_mismatch".into());
        }
        if old.state == "terminal" {
            if old.receipt.as_ref() != Some(receipt) {
                return Err("agent_browser_terminal_changed".into());
            }
            return Ok(old);
        }
        let terminal = matches!(
            receipt.state.as_str(),
            "completed" | "failed" | "not_started"
        );
        tx.execute(
            "UPDATE wc_agent_browser_actions SET state=?2,result_json=?3 WHERE action_id=?1",
            params![
                action.action_id,
                if terminal {
                    "terminal"
                } else {
                    "dispatch_fenced"
                },
                serde_json::to_string(receipt).map_err(storage)?
            ],
        )
        .map_err(storage)?;
        let result = read(&tx, &action.action_id)?;
        tx.commit().map_err(storage)?;
        Ok(result)
    }
}
