use super::*;
use webcodex_core::agent_browser::{AgentBrowserAction, AgentBrowserReceipt};
const FILE: &str = "agent-browser-actions.json";
const MAX: usize = 32 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserMetadata {
    pub action: AgentBrowserAction,
    pub receipt: AgentBrowserReceipt,
}
fn validate(rows: &[BrowserMetadata]) -> Result<(), String> {
    if rows.len() > 1024 {
        return Err("agent_browser_history_capacity".into());
    }
    let mut ids = HashSet::new();
    for row in rows {
        row.action.validate()?;
        row.receipt.validate_for(&row.action)?;
        if !ids.insert(&row.action.action_id) {
            return Err("agent_browser_duplicate_history".into());
        }
    }
    Ok(())
}
impl DetachedJobStore {
    pub(crate) fn initialize_browser_metadata(&self) -> Result<(), String> {
        reject_symlink_or_non_dir(&self.root, "browser state root")?;
        let _lock = exclusive_lock(&self.root.join(ROOT_LOCK_FILE), true)?;
        let path = self.root.join(FILE);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let rows: Vec<BrowserMetadata> = read_json_bounded(&path, MAX, "browser metadata")?;
                validate(&rows)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if self.root.join("browser-artifacts").exists() {
                    return Err("agent_browser_history_missing".into());
                }
                atomic_write_json(&path, &Vec::<BrowserMetadata>::new(), MAX)
            }
            Err(_) => Err("agent_browser_history_unavailable".into()),
        }
    }
    pub(crate) fn browser_artifact_directory(
        &self,
        action: &AgentBrowserAction,
    ) -> Result<PathBuf, String> {
        action.validate()?;
        Ok(self.root.join("browser-artifacts").join(&action.action_id))
    }
    pub(crate) fn browser_metadata(&self, id: &str) -> Result<Option<BrowserMetadata>, String> {
        let _lock = existing_agent_metadata_lock(&self.root.join(ROOT_LOCK_FILE))?;
        let rows: Vec<BrowserMetadata> =
            read_json_bounded(&self.root.join(FILE), MAX, "browser metadata")?;
        validate(&rows)?;
        Ok(rows.into_iter().find(|r| r.action.action_id == id))
    }
    pub(crate) fn fence_browser(
        &self,
        action: &AgentBrowserAction,
        receipt: &AgentBrowserReceipt,
    ) -> Result<bool, String> {
        let _lock = exclusive_lock(&self.root.join(ROOT_LOCK_FILE), true)?;
        let mut rows: Vec<BrowserMetadata> =
            read_json_bounded(&self.root.join(FILE), MAX, "browser metadata")?;
        validate(&rows)?;
        if let Some(old) = rows.iter().find(|r| r.action.action_id == action.action_id) {
            if old.action != *action {
                return Err("agent_browser_intent_changed".into());
            }
            return Ok(false);
        }
        if receipt.state != "dispatch_fenced" {
            return Err("agent_browser_fence_invalid".into());
        }
        rows.push(BrowserMetadata {
            action: action.clone(),
            receipt: receipt.clone(),
        });
        validate(&rows)?;
        atomic_write_json(&self.root.join(FILE), &rows, MAX)?;
        Ok(true)
    }
    pub(crate) fn finish_browser(
        &self,
        action: &AgentBrowserAction,
        receipt: &AgentBrowserReceipt,
    ) -> Result<(), String> {
        let _lock = exclusive_lock(&self.root.join(ROOT_LOCK_FILE), true)?;
        let mut rows: Vec<BrowserMetadata> =
            read_json_bounded(&self.root.join(FILE), MAX, "browser metadata")?;
        validate(&rows)?;
        let old = rows
            .iter_mut()
            .find(|r| r.action.action_id == action.action_id)
            .ok_or("agent_browser_fence_missing")?;
        if old.action != *action
            || (old.receipt.state != "dispatch_fenced" && old.receipt != *receipt)
        {
            return Err("agent_browser_history_immutable".into());
        }
        let mut next = receipt.clone();
        if next.process_id.is_none() && next.state == "outcome_unknown" {
            next.process_id = old.receipt.process_id;
        }
        if old.receipt.process_id.is_some() && next.process_id != old.receipt.process_id {
            return Err("agent_browser_process_identity_changed".into());
        }
        old.receipt = next;
        validate(&rows)?;
        atomic_write_json(&self.root.join(FILE), &rows, MAX)
    }
}
