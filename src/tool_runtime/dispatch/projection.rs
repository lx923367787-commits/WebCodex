//! Model-facing projection after canonical evidence recording.
use crate::tool_runtime::project_resolution::ResolvedProject;
use crate::tool_runtime::{ToolCall, ToolResult};
use serde_json::{json, Value};

/// Add the Phase A lifecycle tuple to a definite pre-execution structured
/// execution denial without changing generic denial helpers used by unrelated
/// tools.
pub(crate) fn decorate_structured_execution_prestart_denial(
    tool_name: &str,
    result: &mut ToolResult,
    fallback_failure_kind: &'static str,
) {
    if !matches!(
        tool_name,
        "run_process" | "run_detached_process" | "run_script"
    ) {
        return;
    }
    let mut output = match std::mem::take(&mut result.output) {
        Value::Object(output) => output,
        other => {
            let mut output = serde_json::Map::new();
            output.insert("value".to_string(), other);
            output
        }
    };
    let failure_kind = output
        .get("failure_kind")
        .and_then(Value::as_str)
        .or_else(|| output.get("error_kind").and_then(Value::as_str))
        .or_else(|| output.get("code").and_then(Value::as_str))
        .unwrap_or(fallback_failure_kind)
        .to_string();
    output.insert(
        "execution_state".to_string(),
        Value::String("not_started".to_string()),
    );
    output.insert("command_started".to_string(), Value::Bool(false));
    output.insert("command_completed".to_string(), Value::Bool(false));
    output.insert("command_ok".to_string(), Value::Bool(false));
    output.insert("exit_code".to_string(), Value::Null);
    output.insert("failure_kind".to_string(), Value::String(failure_kind));
    output.insert("tool_failure".to_string(), Value::Bool(true));
    result.output = Value::Object(output);
}

/// Strip successful wrapper/audit facts only after every authority and Session
/// recorder that needs them has consumed the canonical ToolResult.
/// Failure projection is handled separately and preserves every fact required
/// for retry, escalation, uncertainty, Job handoff, and reconciliation.
pub(crate) fn sparsify_success_model_result_metadata(tool_name: &str, result: &mut ToolResult) {
    if !result.success {
        return;
    }
    crate::tool_runtime::git::sparsify_complete_git_review_success(tool_name, result);
    let Some(output) = result.output.as_object_mut() else {
        return;
    };
    output.remove("permission");
    output.remove("session_recorded");
    output.remove("session_event_id");
}

/// Strip model-irrelevant audit/wrapper facts from failures only after the
/// canonical ToolResult has been consumed by permission and Session recorders.
/// Retry/recovery, uncertainty, process output, exit state, Job handoff, and
/// authorization-denial evidence remain explicit.
pub(crate) fn sparsify_failure_model_result_metadata(_tool_name: &str, result: &mut ToolResult) {
    if result.success {
        return;
    }
    let Some(output) = result.output.as_object_mut() else {
        return;
    };
    if output
        .get("permission")
        .and_then(|permission| permission.get("status"))
        .and_then(Value::as_str)
        == Some("auto_approved")
    {
        output.remove("permission");
    }
    output.remove("session_recorded");
    output.remove("session_event_id");
}

enum SearchModelProjection {
    None,
    Single {
        default_timeout: bool,
    },
    Batch {
        default_timeouts: Vec<bool>,
        max_result_bytes: Option<usize>,
    },
}

impl SearchModelProjection {
    fn capture(call: &ToolCall) -> Self {
        match call {
            ToolCall::SearchProjectText { timeout_secs, .. } => Self::Single {
                default_timeout: caller_uses_default_search_timeout(timeout_secs),
            },
            ToolCall::SearchProjectTexts {
                queries,
                max_result_bytes,
                ..
            } => Self::Batch {
                default_timeouts: queries
                    .iter()
                    .map(|query| caller_uses_default_search_timeout(&query.timeout_secs))
                    .collect(),
                max_result_bytes: *max_result_bytes,
            },
            _ => Self::None,
        }
    }
}

enum ModelFacingProjection {
    None,
    Read(crate::tool_runtime::read_files::ReadModelProjection),
    Search(SearchModelProjection),
}

/// Request facts needed only after canonical execution/recording has finished.
/// Captured once before the ToolCall is moved, then consumed exactly once by the
/// terminal model-facing projection stage. The concrete projection stays private
/// so callers cannot branch on tool-specific result policy.
pub(crate) struct ModelFacingProjectionPlan {
    projection: ModelFacingProjection,
}

impl ModelFacingProjectionPlan {
    pub(crate) fn capture(call: &ToolCall) -> Self {
        let projection = match call {
            ToolCall::ReadFile { .. } | ToolCall::ReadFiles { .. } => ModelFacingProjection::Read(
                crate::tool_runtime::read_files::ReadModelProjection::capture(call),
            ),
            ToolCall::SearchProjectText { .. } | ToolCall::SearchProjectTexts { .. } => {
                ModelFacingProjection::Search(SearchModelProjection::capture(call))
            }
            _ => ModelFacingProjection::None,
        };
        Self { projection }
    }

    pub(crate) fn bind_resolved_project(&mut self, resolved: Option<&ResolvedProject>) {
        if let ModelFacingProjection::Read(projection) = &mut self.projection {
            projection.bind_resolved_project(resolved);
        }
    }

    /// Consume the plan at the only stage allowed to turn canonical execution
    /// output into the final model-facing read/search shape. Session/audit
    /// recorders must run before this method.
    pub(crate) fn project(self, result: &mut ToolResult) {
        match self.projection {
            ModelFacingProjection::None => {}
            ModelFacingProjection::Read(projection) => {
                let tool_name = match &projection {
                    crate::tool_runtime::read_files::ReadModelProjection::Single { .. } => {
                        "read_file"
                    }
                    crate::tool_runtime::read_files::ReadModelProjection::Batch {
                        max_result_bytes,
                        ..
                    } => {
                        crate::tool_runtime::read_files::apply_model_facing_output_budget(
                            result,
                            *max_result_bytes,
                            &projection,
                        );
                        crate::tool_runtime::read_files::enforce_final_model_facing_hard_cap(
                            result,
                            &projection,
                        );
                        "read_files"
                    }
                    crate::tool_runtime::read_files::ReadModelProjection::None => return,
                };
                crate::tool_runtime::read_files::add_actionable_read_continuations(
                    &projection,
                    result,
                );
                sparsify_complete_read_success(tool_name, result);
            }
            ModelFacingProjection::Search(projection) => {
                if let SearchModelProjection::Batch {
                    default_timeouts,
                    max_result_bytes,
                } = &projection
                {
                    crate::tool_runtime::search_project_texts::apply_model_facing_output_budget(
                        result,
                        default_timeouts,
                        *max_result_bytes,
                    );
                    crate::tool_runtime::search_project_texts::enforce_final_model_facing_hard_cap(
                        result,
                        default_timeouts,
                    );
                }
                sparsify_search_success_for_model(&projection, result);
            }
        }
    }
}

fn caller_uses_default_search_timeout(timeout_secs: &Option<i64>) -> bool {
    timeout_secs
        .as_ref()
        .copied()
        .unwrap_or(crate::tool_runtime::files::DEFAULT_SEARCH_TIMEOUT_SECS as i64)
        == crate::tool_runtime::files::DEFAULT_SEARCH_TIMEOUT_SECS as i64
}

fn sparsify_search_match_items(output: &mut serde_json::Map<String, Value>) {
    let Some(matches) = output.get_mut("matches").and_then(Value::as_array_mut) else {
        return;
    };
    for item in matches {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        for key in ["context_before", "context_after"] {
            if item
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                item.remove(key);
            }
        }
        let path = item.get("path").and_then(Value::as_str).map(str::to_string);
        let Some(read_hint) = item.get_mut("read_hint").and_then(Value::as_object_mut) else {
            continue;
        };
        if path
            .as_deref()
            .is_some_and(|path| read_hint.get("path").and_then(Value::as_str) == Some(path))
        {
            read_hint.remove("path");
        }
    }
}

fn add_search_refinement_continuation(output: &mut serde_json::Map<String, Value>) {
    if output.get("truncated").and_then(Value::as_bool) != Some(true)
        || !matches!(
            output.get("truncation_reason").and_then(Value::as_str),
            Some("limit" | "output_bytes")
        )
    {
        return;
    }
    output.entry("continuation".to_string()).or_insert_with(|| {
        json!({
            "kind": "refine_query",
            "safe_cursor": false,
            "refine_with": ["path", "include_globs", "pattern", "result_mode", "limit"]
        })
    });
}

/// Project successful text-search presentation after Session/event consumers
/// have seen canonical evidence. Complete rg results keep only mode-relevant
/// records and explicit non-default controls. Fallback/truncated successes retain
/// diagnostic metadata; match items still drop only empty context and duplicate
/// read-hint paths. A default batch timeout may shrink under the shared absolute
/// deadline without making that derived value model-relevant.
pub(crate) fn sparsify_search_output_for_model(
    output: &mut serde_json::Map<String, Value>,
    default_timeout: bool,
    allow_batch_deadline_reduction: bool,
) -> bool {
    sparsify_search_match_items(output);
    add_search_refinement_continuation(output);

    let exit_code = output.get("exit_code").and_then(Value::as_i64);
    let result_mode = output
        .get("result_mode")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mode_consistent = match result_mode.as_deref() {
        Some("matches") => output
            .get("matches")
            .and_then(Value::as_array)
            .is_some_and(|matches| {
                output.get("count").and_then(Value::as_u64) == Some(matches.len() as u64)
            }),
        Some("files_with_matches") => {
            output
                .get("files")
                .and_then(Value::as_array)
                .is_some_and(|files| {
                    output.get("returned_file_count").and_then(Value::as_u64)
                        == Some(files.len() as u64)
                })
        }
        Some("count") => output
            .get("files")
            .and_then(Value::as_array)
            .is_some_and(|files| {
                output.get("returned_file_count").and_then(Value::as_u64)
                    == Some(files.len() as u64)
                    && output.get("count_complete").and_then(Value::as_bool) == Some(true)
                    && output.get("returned_match_count").and_then(Value::as_u64)
                        == output.get("total_matches").and_then(Value::as_u64)
            }),
        _ => false,
    };
    let ordinary_complete = output.get("backend").and_then(Value::as_str) == Some("rg")
        && matches!(
            output.get("pattern_mode").and_then(Value::as_str),
            Some("regex" | "literal")
        )
        && mode_consistent
        && output.get("truncated").and_then(Value::as_bool) == Some(false)
        && output.get("truncation_reason").is_some_and(Value::is_null)
        && matches!(exit_code, Some(0 | 1));
    if !ordinary_complete {
        return false;
    }

    for key in [
        "project",
        "pattern",
        "backend",
        "exit_code",
        "truncated",
        "truncation_reason",
    ] {
        output.remove(key);
    }
    if output.get("pattern_mode").and_then(Value::as_str) == Some("regex") {
        output.remove("pattern_mode");
    }
    match result_mode.as_deref() {
        Some("matches") => {
            output.remove("result_mode");
            output.remove("count");
        }
        Some("files_with_matches") => {
            output.remove("returned_file_count");
        }
        Some("count") => {
            output.remove("returned_file_count");
            output.remove("returned_match_count");
            output.remove("count_complete");
            if output
                .get("files")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                output.remove("files");
            }
        }
        _ => {}
    }
    for key in ["context_before", "context_after"] {
        if output.get(key).and_then(Value::as_u64) == Some(0) {
            output.remove(key);
        }
    }
    let effective_timeout = output.get("effective_timeout_secs").and_then(Value::as_u64);
    let timeout_is_boring_default = default_timeout
        && if allow_batch_deadline_reduction {
            effective_timeout.is_some_and(|timeout| {
                (1..=crate::tool_runtime::files::DEFAULT_SEARCH_TIMEOUT_SECS).contains(&timeout)
            })
        } else {
            effective_timeout == Some(crate::tool_runtime::files::DEFAULT_SEARCH_TIMEOUT_SECS)
        };
    if timeout_is_boring_default {
        output.remove("effective_timeout_secs");
    }
    if output.get("path").and_then(Value::as_str) == Some(".") {
        output.remove("path");
    }
    true
}

fn sparsify_search_success_for_model(projection: &SearchModelProjection, result: &mut ToolResult) {
    if !result.success {
        return;
    }
    let Some(output) = result.output.as_object_mut() else {
        return;
    };
    match projection {
        SearchModelProjection::Single { default_timeout } => {
            sparsify_search_output_for_model(output, *default_timeout, false);
        }
        SearchModelProjection::Batch {
            default_timeouts, ..
        } => {
            let complete_batch =
                output
                    .get("items")
                    .and_then(Value::as_array)
                    .is_some_and(|items| {
                        let item_count = items.len() as u64;
                        item_count > 0
                            && items.iter().all(|item| {
                                item.get("success").and_then(Value::as_bool) == Some(true)
                                    && item.get("error").is_some_and(Value::is_null)
                            })
                            && output.get("requested_count").and_then(Value::as_u64)
                                == Some(item_count)
                            && output.get("returned_count").and_then(Value::as_u64)
                                == Some(item_count)
                            && output.get("succeeded_count").and_then(Value::as_u64)
                                == Some(item_count)
                            && output.get("failed_count").and_then(Value::as_u64) == Some(0)
                            && output.get("output_truncated").and_then(Value::as_bool)
                                == Some(false)
                            && output.get("next_index").is_some_and(Value::is_null)
                    });
            let Some(items) = output.get_mut("items").and_then(Value::as_array_mut) else {
                return;
            };
            for item in items {
                let Some(item) = item.as_object_mut() else {
                    continue;
                };
                if item.get("success").and_then(Value::as_bool) != Some(true)
                    || !item.get("error").is_some_and(Value::is_null)
                {
                    continue;
                }
                let Some(index) = item.get("index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(search_output) = item.get_mut("output").and_then(Value::as_object_mut)
                else {
                    continue;
                };
                sparsify_search_output_for_model(
                    search_output,
                    default_timeouts
                        .get(index as usize)
                        .copied()
                        .unwrap_or(false),
                    true,
                );
            }
            if complete_batch {
                for key in [
                    "project",
                    "requested_count",
                    "returned_count",
                    "succeeded_count",
                    "failed_count",
                    "output_truncated",
                    "next_index",
                ] {
                    output.remove(key);
                }
            }
        }
        SearchModelProjection::None => {}
    }
}

pub(crate) fn sparsify_search_batch_success_for_model(
    default_timeouts: &[bool],
    result: &mut ToolResult,
) {
    sparsify_search_success_for_model(
        &SearchModelProjection::Batch {
            default_timeouts: default_timeouts.to_vec(),
            max_result_bytes: None,
        },
        result,
    );
}

/// Remove range bookkeeping only when the returned text is provably the complete
/// file. `sha256` and `total_lines` remain explicit freshness/content-shape
/// evidence. Partial reads and every real continuation keep the canonical full
/// range tuple. In a batch, the outer item path remains the navigation identity,
/// so an identical inner path is redundant.
pub(crate) fn sparsify_complete_file_read_output(
    output: &mut serde_json::Map<String, Value>,
    duplicate_outer_path: Option<&str>,
) -> bool {
    let format = output.get("format").and_then(Value::as_str);
    let valid_format = matches!(format, Some("plain" | "numbered"));
    let plain_format = format == Some("plain");
    let Some(inner_path) = output.get("path").and_then(Value::as_str) else {
        return false;
    };
    if duplicate_outer_path.is_some_and(|path| path != inner_path) {
        return false;
    }
    let Some(total_lines) = output.get("total_lines").and_then(Value::as_u64) else {
        return false;
    };
    let Some(returned_lines) = output.get("returned_lines").and_then(Value::as_u64) else {
        return false;
    };
    let default_limit =
        webcodex_workspace::file_read_range::EffectiveRange::new(None, None).limit as u64;
    let end_line_matches = if total_lines == 0 {
        output.get("end_line").is_some_and(Value::is_null)
    } else {
        output.get("end_line").and_then(Value::as_u64) == Some(total_lines)
    };
    let complete_file = output.get("text").and_then(Value::as_str).is_some()
        && valid_format
        && output
            .get("sha256")
            .and_then(Value::as_str)
            .is_some_and(|sha| {
                sha.len() == 64
                    && sha
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        && output.get("start_line").and_then(Value::as_u64) == Some(1)
        && output.get("limit").and_then(Value::as_u64) == Some(default_limit)
        && returned_lines == total_lines
        && returned_lines <= default_limit
        && end_line_matches
        && output.get("has_more").and_then(Value::as_bool) == Some(false)
        && output.get("next_start_line").is_some_and(Value::is_null);
    if !complete_file {
        return false;
    }

    for key in [
        "start_line",
        "limit",
        "returned_lines",
        "end_line",
        "has_more",
        "next_start_line",
    ] {
        output.remove(key);
    }
    if plain_format {
        output.remove("format");
    }
    if duplicate_outer_path.is_some() {
        output.remove("path");
    }
    true
}

pub(crate) fn sparsify_complete_read_success(tool_name: &str, result: &mut ToolResult) {
    if !result.success || !matches!(tool_name, "read_file" | "read_files") {
        return;
    }
    let Some(output) = result.output.as_object_mut() else {
        return;
    };
    if tool_name == "read_file" {
        sparsify_complete_file_read_output(output, None);
        return;
    }

    let complete_batch = output
        .get("items")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            let item_count = items.len() as u64;
            item_count > 0
                && items.iter().all(|item| {
                    item.get("success").and_then(Value::as_bool) == Some(true)
                        && item.get("error").is_some_and(Value::is_null)
                })
                && output.get("requested_count").and_then(Value::as_u64) == Some(item_count)
                && output.get("returned_count").and_then(Value::as_u64) == Some(item_count)
                && output.get("succeeded_count").and_then(Value::as_u64) == Some(item_count)
                && output.get("failed_count").and_then(Value::as_u64) == Some(0)
                && output.get("output_truncated").and_then(Value::as_bool) == Some(false)
                && output.get("next_index").is_some_and(Value::is_null)
        });
    let Some(items) = output.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    let mut every_item_complete = complete_batch;
    for item in items {
        let Some(item) = item.as_object_mut() else {
            every_item_complete = false;
            continue;
        };
        if item.get("success").and_then(Value::as_bool) != Some(true)
            || !item.get("error").is_some_and(Value::is_null)
        {
            every_item_complete = false;
            continue;
        }
        let Some(outer_path) = item.get("path").and_then(Value::as_str).map(str::to_string) else {
            every_item_complete = false;
            continue;
        };
        let Some(read_output) = item.get_mut("output").and_then(Value::as_object_mut) else {
            every_item_complete = false;
            continue;
        };
        if !sparsify_complete_file_read_output(read_output, Some(&outer_path)) {
            every_item_complete = false;
        }
    }
    if every_item_complete {
        for key in [
            "project",
            "requested_count",
            "returned_count",
            "succeeded_count",
            "failed_count",
            "output_truncated",
            "next_index",
        ] {
            output.remove(key);
        }
    }
}

#[cfg(test)]
mod structured_execution_sparse_projection_tests {
    use super::*;
    use serde_json::json;

    fn terminal_process_result(execution_source: &str) -> ToolResult {
        ToolResult::ok(json!({
            "duration_ms": 1,
            "exit_code": 0,
            "stdout_tail": "",
            "stderr_tail": "",
            "stdout_lines": 0,
            "stderr_lines": 0,
            "stdout_truncated": false,
            "stderr_truncated": false,
            "command_started": true,
            "command_completed": true,
            "command_ok": true,
            "failure_kind": null,
            "tool_failure": false,
            "purpose": "diagnostic",
            "process_summary": "tool --arg",
            "cwd": ".",
            "executor": "agent",
            "execution_source": execution_source,
            "execution_state": "completed",
            "promoted_to_job": false,
            "terminal": true,
            "job_id": null,
            "job_status": null,
            "observation_token": null,
            "effective_timeout_secs": 60,
            "sync_wait_secs": 10,
            "async_handoff_available": true
        }))
    }

    #[test]
    fn terminal_execution_preserves_real_result_metadata_and_streams() {
        for source in ["run_process", "alternate_process_source"] {
            let mut result = terminal_process_result(source);
            let expected = result.output.clone();
            sparsify_success_model_result_metadata("run_process", &mut result);
            assert_eq!(result.output, expected);
            assert_eq!(result.output["exit_code"], 0);
            assert_eq!(result.output["execution_state"], "completed");
        }
    }

    #[test]
    fn failure_projection_removes_audit_noise_but_preserves_decision_relevant_facts() {
        let mut result = ToolResult::err_with_output(
            "process exited 17",
            json!({
                "permission": {"status": "auto_approved", "request_id": "wc_perm_private"},
                "session_recorded": true,
                "session_event_id": "evt_private",
                "executor": "agent",
                "duration_ms": 123,
                "purpose": "diagnostic",
                "cwd": "src/private",
                "execution_source": "run_process",
                "process_summary": "private-command --secret value",
                "failure_kind": "command_exit_nonzero",
                "execution_state": "completed",
                "command_started": true,
                "command_completed": true,
                "command_ok": false,
                "exit_code": 17,
                "stderr_tail": "compiler error",
                "stderr_truncated": false,
                "job_id": "job_keep",
                "observation_token": "token_keep",
                "recovery": {"kind": "inspect_output"}
            }),
        );
        sparsify_failure_model_result_metadata("run_process", &mut result);

        for omitted in ["permission", "session_recorded", "session_event_id"] {
            assert!(
                result.output.get(omitted).is_none(),
                "{omitted}: {}",
                result.output
            );
        }
        for retained in [
            "executor",
            "duration_ms",
            "purpose",
            "cwd",
            "execution_source",
            "process_summary",
            "failure_kind",
            "execution_state",
            "command_started",
            "command_completed",
            "command_ok",
            "exit_code",
            "stderr_tail",
            "stderr_truncated",
            "job_id",
            "observation_token",
            "recovery",
        ] {
            assert!(
                result.output.get(retained).is_some(),
                "{retained}: {}",
                result.output
            );
        }
    }

    #[test]
    fn failure_projection_keeps_permission_denials_and_unknown_outcomes() {
        let mut result = ToolResult::err_with_output(
            "permission denied",
            json!({
                "permission": {
                    "status": "denied",
                    "reason": "restricted_requires_human_authorization"
                },
                "failure_kind": "outcome_unknown",
                "execution_state": "outcome_unknown",
                "command_started": true,
                "command_completed": false
            }),
        );
        sparsify_failure_model_result_metadata("run_process", &mut result);
        assert_eq!(result.output["permission"]["status"], "denied");
        assert_eq!(result.output["failure_kind"], "outcome_unknown");
        assert_eq!(result.output["execution_state"], "outcome_unknown");
        assert_eq!(result.output["command_started"], true);
        assert_eq!(result.output["command_completed"], false);
    }
}

#[cfg(test)]
mod sparse_read_projection_tests {
    use super::*;
    use serde_json::json;

    fn complete_batch_item(outer_path: Option<&str>, inner_path: &str) -> Value {
        let default_limit =
            webcodex_workspace::file_read_range::EffectiveRange::new(None, None).limit;
        let mut item = json!({
            "index": 0,
            "success": true,
            "output": {
                "text": "one",
                "format": "plain",
                "path": inner_path,
                "sha256": "a".repeat(64),
                "start_line": 1,
                "limit": default_limit,
                "total_lines": 1,
                "returned_lines": 1,
                "end_line": 1,
                "has_more": false,
                "next_start_line": null
            },
            "error": null
        });
        if let Some(path) = outer_path {
            item["path"] = json!(path);
        }
        item
    }

    #[test]
    fn sparse_read_batch_requires_exact_outer_path_identity() {
        for item in [
            complete_batch_item(None, "a.rs"),
            complete_batch_item(Some("other.rs"), "a.rs"),
        ] {
            let mut result = ToolResult::ok(json!({
                "project": "demo",
                "requested_count": 1,
                "returned_count": 1,
                "succeeded_count": 1,
                "failed_count": 0,
                "items": [item],
                "output_truncated": false,
                "next_index": null
            }));

            sparsify_complete_read_success("read_files", &mut result);

            assert_eq!(result.output["requested_count"], 1);
            assert_eq!(result.output["items"][0]["output"]["path"], "a.rs");
            assert_eq!(result.output["items"][0]["output"]["start_line"], 1);
        }
    }
}
