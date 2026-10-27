//! Shared batch envelope accounting for file reads and text searches.

use serde_json::{json, Value};

#[cfg(test)]
use super::ToolResult;

pub(super) fn batch_output(
    project: &str,
    requested_count: usize,
    items: Vec<Value>,
    output_truncated: bool,
    next_index: Option<usize>,
    truncation_reason: Option<&str>,
) -> Value {
    let succeeded_count = items
        .iter()
        .filter(|item| item["success"].as_bool() == Some(true))
        .count();
    let returned_count = items.len();
    let mut output = json!({
        "project": project,
        "requested_count": requested_count,
        "returned_count": returned_count,
        "succeeded_count": succeeded_count,
        "failed_count": returned_count - succeeded_count,
        "items": items,
        "output_truncated": output_truncated,
        "next_index": next_index,
    });
    if let Some(reason) = truncation_reason {
        output["truncation_reason"] = json!(reason);
    }
    output
}

#[cfg(test)]
pub(super) fn serialized_batch_len(output: &Value) -> usize {
    serde_json::to_vec(&ToolResult::ok(output.clone()))
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

pub(super) fn serialized_value_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

pub(super) fn projected_batch_len(base_len: usize, item_bytes: usize, item_count: usize) -> usize {
    base_len
        .saturating_add(item_bytes)
        .saturating_add(item_count.saturating_sub(1))
}

pub(super) fn mark_final_hard_cap_truncation(output: &mut Value, next_index: usize) {
    let Some(root) = output.as_object_mut() else {
        return;
    };
    let (returned_count, succeeded_count) = root
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            (
                items.len(),
                items
                    .iter()
                    .filter(|item| item["success"].as_bool() == Some(true))
                    .count(),
            )
        })
        .unwrap_or_default();
    root.insert("returned_count".to_string(), json!(returned_count));
    root.insert("succeeded_count".to_string(), json!(succeeded_count));
    root.insert(
        "failed_count".to_string(),
        json!(returned_count.saturating_sub(succeeded_count)),
    );
    root.insert("output_truncated".to_string(), json!(true));
    root.insert("next_index".to_string(), json!(next_index));
    root.insert("truncation_reason".to_string(), json!("hard_result_cap"));
}
