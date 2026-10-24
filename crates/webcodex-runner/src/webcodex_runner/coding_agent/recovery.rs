//! Bounded execution evidence. It is provider-reported observation, never proof
//! of task acceptance, authority, or permission to replay an uncertain operation.
use super::*;

const EVIDENCE_FILE: &str = "evidence.json";
const MAX_EVIDENCE_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn prompt_error_kind(error: &Value) -> &'static str {
    // Only structured provider evidence classifies failures. Message text is
    // diagnostic data; it must not turn every provider failure into quota loss.
    let code = error
        .pointer("/data/code")
        .and_then(Value::as_str)
        .or_else(|| error.pointer("/data/error/code").and_then(Value::as_str));
    match code {
        Some("insufficient_quota" | "quota_exhausted" | "credits_exhausted") => "quota_exhausted",
        Some("rate_limit_exceeded" | "rate_limited") => "rate_limited",
        Some("authentication_error" | "invalid_api_key") => "authentication_error",
        Some("permission_denied") => "permission_denied",
        Some("service_unavailable" | "model_unavailable") => "provider_unavailable",
        _ => match error
            .pointer("/data/status")
            .and_then(Value::as_i64)
            .or_else(|| error.get("code").and_then(Value::as_i64))
        {
            Some(429) => "rate_limited",
            Some(401) => "authentication_error",
            Some(403) => "permission_denied",
            Some(502 | 503 | 504) => "provider_unavailable",
            _ => "prompt_error",
        },
    }
}

#[cfg(test)]
mod process_tests;
#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    run_id: String,
    #[serde(default)]
    observation_revision: u64,
    first_sequence: u64,
    next_sequence: u64,
    events: VecDeque<CodingAgentEvent>,
}

impl RunEntry {
    pub(super) fn restore_evidence(&self, store: &DurableRunStore, recovering: bool) {
        let mut state = self.state.lock().unwrap();
        let path = store.run_dir(&state.snapshot.run_id).join(EVIDENCE_FILE);
        *self.evidence_path.lock().unwrap() = Some(path.clone());
        if !recovering {
            return;
        }
        let read = || -> Result<Evidence, String> {
            let file = File::open(&path).map_err(|_| "missing evidence")?;
            let mut bytes = Vec::new();
            file.take((MAX_EVIDENCE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| "unreadable evidence")?;
            if bytes.len() > MAX_EVIDENCE_BYTES {
                return Err("oversized evidence".into());
            }
            let evidence: Evidence =
                serde_json::from_slice(&bytes).map_err(|_| "invalid evidence")?;
            if evidence.run_id != state.snapshot.run_id
                || evidence.events.len() > CODING_AGENT_MAX_RETAINED_EVENTS
                || evidence.first_sequence == 0
                || evidence.next_sequence
                    != evidence
                        .first_sequence
                        .saturating_add(evidence.events.len() as u64)
                || evidence
                    .events
                    .iter()
                    .enumerate()
                    .any(|(i, event)| event.sequence != evidence.first_sequence + i as u64)
            {
                return Err("evidence identity/order mismatch".into());
            }
            Ok(evidence)
        };
        match read() {
            Ok(evidence) => {
                state.first_retained_sequence = evidence.first_sequence;
                state.next_sequence = evidence.next_sequence;
                state.events = evidence.events;
                // Recovered observations must not regress below a previously
                // published event revision just because the process restarted.
                state.snapshot.observation_revision = state
                    .snapshot
                    .observation_revision
                    .max(evidence.observation_revision);
            }
            Err(_) => state.evidence_incomplete = true,
        }
    }

    pub(super) fn persist_evidence(&self, state: &LiveRunState) {
        let Some(path) = self.evidence_path.lock().unwrap().clone() else {
            return;
        };
        let mut events = state.events.clone();
        for event in &mut events {
            if event.kind == CodingAgentEventKind::Reasoning {
                // Keep sequence continuity without storing internal reasoning.
                event.text = None;
            }
        }
        let evidence = Evidence {
            run_id: state.snapshot.run_id.clone(),
            observation_revision: state.snapshot.observation_revision,
            first_sequence: state.first_retained_sequence,
            next_sequence: state.next_sequence,
            events,
        };
        let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
        let write = || -> Result<(), String> {
            let bytes = serde_json::to_vec(&evidence).map_err(|_| "invalid evidence")?;
            if bytes.len() > MAX_EVIDENCE_BYTES {
                return Err("evidence bound exceeded".into());
            }
            let mut options = File::options();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            // Windows inherits the Runner's user-local state-directory ACL.
            let mut file = options.open(&temp).map_err(|_| "cannot create evidence")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "cannot persist evidence")?;
            drop(file);
            publish_state_file(&temp, &path)?;
            sync_parent(path.parent().ok_or("missing evidence directory")?)
        };
        if write().is_err() {
            self.evidence_failed.store(true, Ordering::Release);
            let _ = fs::remove_file(&temp);
        }
    }
}
