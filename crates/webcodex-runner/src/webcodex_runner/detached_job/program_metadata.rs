//! Ordinary sandbox program receipts under the existing Runner state lock.
//! Missing or corrupt dispatch fences are never recreated by an execution read.
use super::*;
use webcodex_core::agent_program::AgentProgramStart;
const FILE: &str = "agent-program-executions.json";
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentProgramJobMetadata {
    pub(crate) start: AgentProgramStart,
    pub(crate) native_create_attempted: bool,
    pub(crate) worker_finished: bool,
    pub(crate) tree_stopped: bool,
    pub(crate) snapshot: Option<ShellJobSnapshot>,
}
fn validate(rows: &[AgentProgramJobMetadata]) -> Result<(), String> {
    if rows.len() > 1024 {
        return Err("agent_program_history_capacity".into());
    }
    let mut ids = HashSet::new();
    let mut jobs = HashSet::new();
    for row in rows {
        row.start.validate()?;
        if !ids.insert(&row.start.binding.program_id)
            || !jobs.insert(&row.start.binding.job_id)
            || row.tree_stopped && !row.worker_finished
        {
            return Err("agent_program_history_invalid".into());
        }
        if let Some(snapshot) = &row.snapshot {
            if snapshot.job_id != row.start.binding.job_id
                || snapshot.request_id != row.start.binding.start_request_id
                || snapshot.context != row.start.context()
            {
                return Err("agent_program_snapshot_mismatch".into());
            }
        }
    }
    Ok(())
}
impl DetachedJobStore {
    pub(crate) fn initialize_agent_program_metadata(
        &self,
        resource_root: &Path,
    ) -> Result<(), String> {
        reject_symlink_or_non_dir(&self.root, "program state root")?;
        let _lock = exclusive_lock(&self.root.join(ROOT_LOCK_FILE), true)?;
        let path = self.root.join(FILE);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let rows: Vec<AgentProgramJobMetadata> =
                    read_json_bounded(&path, MAX_BYTES, "program metadata")?;
                validate(&rows)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if resource_root.exists()
                    && fs::read_dir(resource_root)
                        .map_err(|_| "agent_program_resource_scan_failed")?
                        .any(|e| {
                            e.map_or(true, |e| {
                                let name = e.file_name();
                                let name = name.to_string_lossy();
                                name.starts_with("program-") || name.starts_with("p-")
                            })
                        })
                {
                    return Err("agent_program_missing_dispatch_history".into());
                }
                atomic_write_json(&path, &Vec::<AgentProgramJobMetadata>::new(), MAX_BYTES)
            }
            Err(_) => Err("agent_program_history_unavailable".into()),
        }
    }
    pub(crate) fn read_agent_program_metadata(
        &self,
    ) -> Result<Vec<AgentProgramJobMetadata>, String> {
        reject_symlink_or_non_dir(&self.root, "program state root")?;
        let _lock = existing_agent_metadata_lock(&self.root.join(ROOT_LOCK_FILE))?;
        let rows: Vec<AgentProgramJobMetadata> =
            read_json_bounded(&self.root.join(FILE), MAX_BYTES, "program metadata")?;
        validate(&rows)?;
        Ok(rows)
    }
    pub(crate) fn update_agent_program_metadata<T>(
        &self,
        update: impl FnOnce(&mut Vec<AgentProgramJobMetadata>) -> Result<T, String>,
    ) -> Result<T, String> {
        reject_symlink_or_non_dir(&self.root, "program state root")?;
        let _lock = exclusive_lock(&self.root.join(ROOT_LOCK_FILE), true)?;
        let previous: Vec<AgentProgramJobMetadata> =
            read_json_bounded(&self.root.join(FILE), MAX_BYTES, "program metadata")?;
        validate(&previous)?;
        let mut current = previous.clone();
        let result = update(&mut current)?;
        validate(&current)?;
        for old in &previous {
            let new = current
                .iter()
                .find(|r| r.start.binding.program_id == old.start.binding.program_id)
                .ok_or("agent_program_history_removed")?;
            if old.start != new.start
                || old.native_create_attempted && !new.native_create_attempted
                || old.worker_finished && old != new
                || old.tree_stopped && !new.tree_stopped
            {
                return Err("agent_program_history_regressed".into());
            }
        }
        if current != previous {
            atomic_write_json(&self.root.join(FILE), &current, MAX_BYTES)?;
        }
        Ok(result)
    }
}
