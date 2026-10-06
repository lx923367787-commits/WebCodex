//! Invocation-local goal metadata and partial, durable conversation metering.
//! This module does not authorize tools or infer a chat from transport sessions.

use crate::client_window::ClientWindow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::future::Future;
use webcodex_core::host_goal::HostGoalExecutionBinding;
use webcodex_store::{Database, HostContextMeter};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GoalCallContext {
    pub(crate) goal_id: String,
    pub(crate) node_id: String,
    pub(crate) attempt_id: String,
    pub(crate) plan_version: u64,
    pub(crate) fence: u64,
    pub(crate) operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) repair: Option<webcodex_core::host_goal::HostGoalRepairContext>,
}

fn identity(value: &str, limit: usize, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > limit || value.contains('\0') {
        return Err(format!(
            "{label} must be nonempty, without NUL, and at most {limit} bytes"
        ));
    }
    Ok(())
}

impl GoalCallContext {
    pub(crate) fn binding(&self) -> HostGoalExecutionBinding {
        HostGoalExecutionBinding {
            goal_id: self.goal_id.clone(),
            node_id: self.node_id.clone(),
            attempt_id: self.attempt_id.clone(),
            plan_version: self.plan_version,
            fence: self.fence,
        }
    }

    fn validate(&self) -> Result<(), String> {
        identity(&self.goal_id, 128, "goal_context.goal_id")?;
        identity(&self.node_id, 64, "goal_context.node_id")?;
        identity(&self.attempt_id, 128, "goal_context.attempt_id")?;
        identity(&self.operation_id, 128, "goal_context.operation_id")?;
        if self.plan_version == 0 || self.fence == 0 {
            return Err("goal_context requires positive plan_version and fence".into());
        }
        if let Some(epoch) = &self.context_epoch {
            identity(epoch, 128, "context_epoch")?;
        }
        if let Some(repair) = &self.repair {
            identity(&repair.repair_id, 128, "repair_id")?;
            if let Some(diagnosis) = &repair.diagnosis {
                identity(diagnosis, 4096, "repair diagnosis")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RequestScope {
    owner_key: String,
    window_key: Option<String>,
    context_epoch: Option<String>,
    goal_context: Option<GoalCallContext>,
    event_id: String,
    request_arguments: Value,
    admitted_operation: Option<(String, String)>,
    active_operation: Option<std::sync::Arc<ActiveOperationLease>>,
}

impl RequestScope {
    pub(crate) fn new(
        owner_key: String,
        window: Option<&ClientWindow>,
        arguments: &Value,
    ) -> Result<Self, String> {
        identity(&owner_key, 512, "goal owner")?;
        let goal_context: Option<GoalCallContext> = arguments
            .get("goal_context")
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid goal_context: {e}"))
            })
            .transpose()?;
        if let Some(context) = &goal_context {
            context.validate()?;
        }
        let explicit_epoch = arguments
            .get("context_epoch")
            .map(|value| {
                let epoch = value
                    .as_str()
                    .ok_or_else(|| "context_epoch must be a string".to_owned())?;
                identity(epoch, 128, "context_epoch")?;
                Ok::<_, String>(epoch.to_owned())
            })
            .transpose()?;
        let bound_epoch = goal_context
            .as_ref()
            .and_then(|context| context.context_epoch.as_ref());
        if explicit_epoch
            .as_ref()
            .zip(bound_epoch)
            .is_some_and(|(a, b)| a != b)
        {
            return Err("context_epoch conflicts with goal_context.context_epoch".into());
        }
        let window_key = window
            .filter(|window| matches!(window.source(), "openai-session" | "openai-conversation"))
            .map(|window| window.key().to_owned());
        // Host identity takes precedence: changing a supplied epoch cannot reset
        // the same host conversation, nor merge two distinct host conversations.
        let context_epoch = window_key
            .as_ref()
            .map(|key| format!("chat_{key}"))
            .or(explicit_epoch)
            .or_else(|| bound_epoch.cloned());
        let mut request_arguments = arguments.clone();
        if let Some(fields) = request_arguments.as_object_mut() {
            for key in [
                "goal_context",
                "context_epoch",
                "recording_session_id",
                "ack_session_context_revision",
                "ack_session_message_ids",
                "session_message_resolution",
                "context_request",
            ] {
                fields.remove(key);
            }
        }
        Ok(Self {
            owner_key,
            window_key,
            context_epoch,
            goal_context,
            event_id: uuid::Uuid::new_v4().to_string(),
            request_arguments,
            admitted_operation: None,
            active_operation: None,
        })
    }
}

tokio::task_local! {
    static CURRENT: RefCell<RequestScope>;
}

type OperationKey = (String, String, Option<(String, String)>);

fn operation_key(goal_id: &str, operation_id: &str) -> OperationKey {
    (
        goal_id.to_string(),
        operation_id.to_string(),
        current_owner().zip(current_epoch()),
    )
}
static ACTIVE_OPERATIONS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<OperationKey, usize>>,
> = std::sync::OnceLock::new();

fn active_operations() -> &'static std::sync::Mutex<std::collections::HashMap<OperationKey, usize>>
{
    ACTIVE_OPERATIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[derive(Debug)]
pub(crate) struct ActiveOperationLease {
    key: OperationKey,
}

impl Drop for ActiveOperationLease {
    fn drop(&mut self) {
        let mut active = active_operations()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(count) = active.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.key);
            }
        }
    }
}

pub(crate) fn lease_active_operation(
    goal_id: &str,
    operation_id: &str,
) -> std::sync::Arc<ActiveOperationLease> {
    let key = operation_key(goal_id, operation_id);
    lease_operation_key(key)
}

fn lease_operation_key(key: OperationKey) -> std::sync::Arc<ActiveOperationLease> {
    *active_operations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.clone())
        .or_default() += 1;
    std::sync::Arc::new(ActiveOperationLease { key })
}

/// Keep the process-local liveness check and the durable reconciliation commit
/// in one critical section. Recovery cannot begin a dispatch between them.
pub(crate) fn guarded_operation_reconciliation<T>(
    goal_id: &str,
    operations: &[String],
    commit: impl FnOnce() -> T,
) -> Result<T, String> {
    let active = active_operations()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if operations.iter().any(|id| {
        active
            .keys()
            .any(|(goal, operation, _)| goal == goal_id && operation == id)
    }) {
        return Err(
            "An original tool request is still active; wait for its authoritative result".into(),
        );
    }
    let result = commit();
    drop(active);
    Ok(result)
}

/// Hold the same liveness lock used when registering new dispatches until the
/// handoff transaction commits. Checking just the current operation list would
/// miss a request which registered its lease before its durable intent.
pub(crate) fn guarded_goal_handoff<T>(
    goal_id: &str,
    commit: impl FnOnce() -> T,
) -> Result<T, String> {
    let context_key = current_owner().zip(current_epoch());
    let active = active_operations()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if active
        .keys()
        .any(|(goal, _, epoch)| goal == goal_id || (context_key.is_some() && epoch == &context_key))
    {
        return Err("An original request is still active; save ordinary progress and wait before handing off".into());
    }
    let result = commit();
    drop(active);
    Ok(result)
}

/// Register before committing dispatch intent. The invocation owns this lease,
/// including while it awaits a Runner. Cancellation/unwind releases it; after
/// restart there is no live request and a stranded intent remains reconcilable.
pub(crate) fn track_active_operation(goal_id: &str, operation_id: &str) -> Result<(), String> {
    CURRENT
        .try_with(|state| {
            let mut state = state.borrow_mut();
            let key = (
                goal_id.to_string(),
                operation_id.to_string(),
                Some(state.owner_key.clone()).zip(state.context_epoch.clone()),
            );
            if let Some(lease) = &state.active_operation {
                return if lease.key == key {
                    Ok(())
                } else {
                    Err("One invocation cannot dispatch multiple goal operation identities".into())
                };
            }
            state.active_operation = Some(lease_operation_key(key));
            Ok(())
        })
        .map_err(|_| "Goal operation requires an invocation scope".to_string())?
}

pub(crate) fn operation_is_active(goal_id: &str, operation_id: &str) -> bool {
    active_operations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .any(|(goal, operation, _)| goal == goal_id && operation == operation_id)
}

pub(crate) async fn scope<F: Future>(request: RequestScope, future: F) -> F::Output {
    CURRENT.scope(RefCell::new(request), future).await
}

pub(crate) fn current_binding() -> Option<GoalCallContext> {
    CURRENT
        .try_with(|state| state.borrow().goal_context.clone())
        .ok()
        .flatten()
}

pub(crate) fn current_owner() -> Option<String> {
    CURRENT
        .try_with(|state| state.borrow().owner_key.clone())
        .ok()
}

pub(crate) fn current_window() -> Option<String> {
    CURRENT
        .try_with(|state| state.borrow().window_key.clone())
        .ok()
        .flatten()
}

/// Explicit epochs are a weaker, caller-carried coordination key. They can
/// restrict calls which carry that epoch; they are never host-authenticated
/// window identity and do not authorize calls which omit it.
pub(crate) fn explicit_mode_key(epoch: &str) -> String {
    use sha2::Digest;
    format!("explicit_{:x}", sha2::Sha256::digest(epoch.as_bytes()))
}

pub(crate) fn current_mode_key() -> Option<String> {
    current_window().or_else(|| current_epoch().map(|epoch| explicit_mode_key(&epoch)))
}

pub(crate) fn current_epoch() -> Option<String> {
    CURRENT
        .try_with(|state| state.borrow().context_epoch.clone())
        .ok()
        .flatten()
}

pub(crate) fn invocation_context() -> Option<webcodex_core::host_goal::HostGoalInvocationContext> {
    current_epoch().map(
        |context_epoch| webcodex_core::host_goal::HostGoalInvocationContext {
            context_epoch,
            identity_source: if current_window().is_some() {
                "window_bound"
            } else {
                "explicit_context_only"
            }
            .into(),
        },
    )
}

pub(crate) fn set_epoch(epoch: &str) -> Result<(), String> {
    identity(epoch, 128, "context_epoch")?;
    CURRENT
        .try_with(|state| {
            let mut state = state.borrow_mut();
            if state
                .context_epoch
                .as_deref()
                .is_some_and(|current| current != epoch)
            {
                return Err(
                    "Cannot change context_epoch within the same invocation or host conversation"
                        .into(),
                );
            }
            state.context_epoch = Some(epoch.to_owned());
            Ok(())
        })
        .map_err(|_| "No goal invocation scope".to_owned())?
}

pub(crate) fn mark_operation_admitted(goal_id: &str, operation_id: &str) {
    let _ = CURRENT.try_with(|state| {
        state.borrow_mut().admitted_operation = Some((goal_id.to_owned(), operation_id.to_owned()));
    });
}

pub(crate) fn admitted_operation() -> Option<(String, String)> {
    CURRENT
        .try_with(|state| state.borrow().admitted_operation.clone())
        .ok()
        .flatten()
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let sorted: std::collections::BTreeMap<_, _> = fields
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        value => value.clone(),
    }
}

/// Hash original request parameters, not a specialized handler's lossy identity
/// projection. Metadata about context retention does not change the operation.
pub(crate) fn current_payload_hash(tool_name: &str) -> Option<String> {
    CURRENT
        .try_with(|state| {
            let state = state.borrow();
            let request = canonical(&json!({"tool":tool_name,"arguments":state.request_arguments}));
            format!("{:x}", Sha256::digest(request.to_string().as_bytes()))
        })
        .ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleTextEstimate {
    pub(crate) tokens: u64,
    pub(crate) unmeasured_modalities: bool,
}

fn binary_key(key: &str) -> bool {
    matches!(
        key,
        "base64"
            | "data_base64"
            | "content_base64"
            | "image_base64"
            | "audio_base64"
            | "b64_json"
            | "base64_data"
            | "bytes"
            | "binary"
            | "blob"
    ) || key.ends_with("_base64")
}

fn duplicate_structured_text(block: &Value, structured: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("text")
        && block
            .get("text")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .as_ref()
            == Some(structured)
}

/// Retain visible text, keys, scalar fields and resource URLs, but never turn
/// image/audio/file byte payloads into a fictitious number of text tokens.
fn visible_value(value: &Value, unmeasured: &mut bool) -> Value {
    match value {
        Value::Object(object) => {
            let is_media = object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "image" | "audio" | "image_url" | "input_image" | "input_audio" | "video"
                    )
                });
            if is_media {
                *unmeasured = true;
            }
            let binary_data = is_media
                || object.get("encoding").and_then(Value::as_str) == Some("base64")
                || object
                    .get("mimeType")
                    .or_else(|| object.get("mime_type"))
                    .and_then(Value::as_str)
                    .is_some_and(|mime| !mime.starts_with("text/") && mime != "application/json");
            let structured = object
                .get("structuredContent")
                .or_else(|| object.get("structured_content"));
            let mut visible = Map::new();
            for (key, item) in object {
                // MCP _meta is host/application data, not model-visible text.
                if key == "_meta" {
                    continue;
                }
                if binary_key(key)
                    || (binary_data && matches!(key.as_str(), "data" | "image" | "audio"))
                {
                    *unmeasured = true;
                    continue;
                }
                if key == "content" {
                    if let (Some(structured), Some(blocks)) = (structured, item.as_array()) {
                        let unique: Vec<Value> = blocks
                            .iter()
                            .filter(|block| !duplicate_structured_text(block, structured))
                            .map(|block| visible_value(block, unmeasured))
                            .collect();
                        if !unique.is_empty() {
                            visible.insert(key.clone(), Value::Array(unique));
                        }
                        continue;
                    }
                }
                visible.insert(key.clone(), visible_value(item, unmeasured));
            }
            Value::Object(visible)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| visible_value(item, unmeasured))
                .collect(),
        ),
        Value::String(text) if text.starts_with("data:") && text.contains(";base64,") => {
            *unmeasured = true;
            Value::String("[unmeasured binary content]".into())
        }
        other => other.clone(),
    }
}

pub(crate) fn estimate_visible(arguments: &Value, final_output: &Value) -> VisibleTextEstimate {
    let mut unmeasured_modalities = false;
    let parameters = visible_value(arguments, &mut unmeasured_modalities).to_string();
    let result = visible_value(final_output, &mut unmeasured_modalities).to_string();
    let encoder = tiktoken_rs::o200k_base_singleton();
    let tokens = encoder
        .encode_ordinary(&parameters)
        .len()
        .saturating_add(encoder.encode_ordinary(&result).len()) as u64;
    VisibleTextEstimate {
        tokens,
        unmeasured_modalities,
    }
}

pub(crate) fn meter_projection(
    meter: Option<HostContextMeter>,
    thresholds: Option<(u64, u64)>,
) -> Value {
    let (warning, handoff) = thresholds.unwrap_or((600_000, 800_000));
    let mut value = meter.as_ref().map(|meter| json!(meter)).unwrap_or_else(|| {
        json!({
            "context_epoch": null, "observed_tokens": null, "warning_tokens": warning,
            "handoff_tokens": handoff, "level": "unavailable", "newly_crossed": false,
            "unmeasured_modalities": false, "sample_count": 0,
        })
    });
    value["measurement"] = json!("observed_estimate");
    value["algorithm"] = json!("o200k_base");
    value["coverage"] = json!("partial");
    value["retry_accounting"] = json!("per_server_invocation");
    let notice = match meter.as_ref() {
        None => Some("Context metering is unavailable: no reliable host conversation identity or explicit context_epoch. Reuse one explicit epoch for this chat. Ordinary chat and other apps are not measured.".to_owned()),
        Some(meter) if meter.level == "handoff_recommended" && meter.newly_crossed => Some(format!("Visible tool text has reached {} estimated tokens (handoff threshold {}). Save a handoff checkpoint, verify that saving succeeded, then suggest opening a new ChatGPT conversation and continuing the local current task. This is partial observation, not actual context capacity; do not cancel active Jobs.", meter.observed_tokens, meter.handoff_tokens)),
        Some(meter) if meter.level == "warning" && meter.newly_crossed => Some(format!("Visible tool text has reached {} estimated tokens (warning threshold {}). Save progress, issues and next steps. This is partial observation, not actual context capacity.", meter.observed_tokens, meter.warning_tokens)),
        Some(meter) if meter.level == "handoff_recommended" => Some("Handoff recommended; confirm checkpoint saving succeeded before changing chat.".into()),
        Some(meter) if meter.level == "warning" => Some("Context warning: save progress.".into()),
        _ => None,
    };
    if let Some(notice) = notice {
        value["notice"] = json!(notice);
    }
    value
}

/// Call once on the final model-facing projection while the invocation scope
/// is alive. The UUID deduplicates this invocation only; an unseen transport
/// retry is another sample, never falsely identified as a replay.
pub(crate) fn observe_response(
    db: &Database,
    arguments: &Value,
    final_output: &Value,
    thresholds: Option<(u64, u64)>,
) -> anyhow::Result<Value> {
    if let Some((warning, handoff)) = thresholds {
        anyhow::ensure!(
            (1000..=10_000_000).contains(&warning) && handoff > warning && handoff <= 20_000_000,
            "Invalid context warning thresholds"
        );
    }
    let state = CURRENT.try_with(|state| state.borrow().clone()).ok();
    let Some((state, epoch)) =
        state.and_then(|state| state.context_epoch.clone().map(|epoch| (state, epoch)))
    else {
        return Ok(meter_projection(None, thresholds));
    };
    let estimate = estimate_visible(arguments, final_output);
    let meter = db.observe_host_context(
        &state.owner_key,
        &epoch,
        &state.event_id,
        estimate.tokens,
        estimate.unmeasured_modalities,
        thresholds,
    )?;
    let mut projection = meter_projection(Some(meter), thresholds);
    if let Some(lifecycle) = db
        .read_host_context_state(&state.owner_key, &epoch)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
    {
        projection["lifecycle_state"] = json!(lifecycle.state);
        projection["identity_source"] = json!(lifecycle.identity_source);
        if lifecycle.state == "retired" {
            projection["notice"] = json!("This conversation is retired and read-only. The user must open a new chat and complete required login/tool approvals; recover the same Goal there. Do not repeat business operations or reactivate this epoch.");
        } else if lifecycle.state == "draining" {
            projection["notice"] = if projection["newly_crossed"] == true {
                json!("This conversation is durably draining: no new business or claims. Finish bounded current acceptance or save a handoff; existing tracked Jobs may continue. This is partial observation, not actual model capacity.")
            } else {
                json!("Draining: checkpoint current work; no new business or claims. This is partial observation.")
            };
        }
    }
    Ok(projection)
}

#[cfg(test)]
#[path = "tests/host_goal_context.rs"]
mod tests;
