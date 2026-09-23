//! Project-bound office facade. Slow work uses the existing process Job owner.
use super::tool_call::DocumentToolCall;
use super::{ToolCall, ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webcodex_core::plugin::{PluginGatewayRequest, PluginGatewayResponsePayload};
use webcodex_core::runner_protocol::DocumentJobMetadata;

pub(crate) fn is_document_tool(name: &str) -> bool {
    matches!(
        name,
        "present_document" | "document_edit" | "document_convert" | "document_restore"
    )
}

fn failure(code: &str, message: impl Into<String>) -> ToolResult {
    ToolResult::err_with_output(message, json!({"error_kind":code,"state_changed":false}))
}

pub(crate) fn valid_document_request_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains([':', '\0'])
        && path
            .replace('\\', "/")
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !path.starts_with(['/', '\\'])
        && !webcodex_core::sensitive_paths::is_secret_path(path)
        && !path
            .replace('\\', "/")
            .to_ascii_lowercase()
            .starts_with(".webcodex-office/")
}

fn validate_arguments(operation: &str, args: &DocumentToolCall) -> Result<(), &'static str> {
    if args.job_id.is_some() {
        if args.path.is_some()
            || args.options.is_some()
            || args.expected_sha256.is_some()
            || args.view.is_some()
            || args.cursor.is_some()
            || args.export_file.is_some()
            || args.output_path.is_some()
            || args.save_mode.is_some()
            || args.operations.is_some()
            || args.backup_id.is_some()
            || args.offset.is_some()
        {
            return Err("Resume with project and job_id only; do not replay document operations");
        }
        return Ok(());
    }
    if !args.path.as_deref().is_some_and(valid_relative_path) {
        return Err("A safe project-relative source path is required");
    }
    if args
        .output_path
        .as_deref()
        .is_some_and(|path| !valid_relative_path(path))
    {
        return Err("output_path must be a safe project-relative document path");
    }
    if args.expected_sha256.as_deref().is_some_and(|sha| {
        sha.len() != 64
            || !sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err("expected_sha256 must be the 64-character lowercase SHA from a current read");
    }
    if (operation == "document_edit"
        || (operation == "document_restore" && args.backup_id.is_some()))
        && args.expected_sha256.is_none()
    {
        return Err("An exact current expected_sha256 is required before modification");
    }
    if args
        .options
        .as_ref()
        .is_some_and(|value| !value.is_object())
    {
        return Err("options must be an object");
    }
    if args
        .operations
        .as_ref()
        .is_some_and(|value| !value.is_array() || value.as_array().unwrap().len() > 100)
    {
        return Err("operations must be an array with at most 100 entries");
    }
    if operation != "document_restore" && (args.backup_id.is_some() || args.offset.is_some()) {
        return Err("Only document_restore accepts backup_id and offset");
    }
    if operation != "document_edit" && (args.operations.is_some() || args.save_mode.is_some()) {
        return Err("Only document_edit accepts operations and save_mode");
    }
    if operation == "document_restore" && (args.output_path.is_some() || args.options.is_some()) {
        return Err("document_restore does not accept output_path or options");
    }
    if operation == "present_document"
        && (args.operations.is_some()
            || args.save_mode.is_some()
            || args.backup_id.is_some()
            || args.output_path.is_some())
    {
        return Err("present_document does not accept mutation fields");
    }
    if operation != "present_document"
        && (args.view.is_some() || args.cursor.is_some() || args.export_file.is_some())
    {
        return Err("Only present_document accepts view, cursor and export_file");
    }
    if args
        .view
        .as_deref()
        .is_some_and(|view| !matches!(view, "auto" | "text" | "pages"))
    {
        return Err("view must be auto, text or pages");
    }
    if serde_json::to_vec(args).map_or(true, |bytes| bytes.len() > 64 * 1024) {
        return Err("Document arguments exceed 64 KiB");
    }
    Ok(())
}

impl ToolRuntime {
    pub(crate) async fn dispatch_document_tool(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        let operation = call.tool_name();
        let args = match call {
            ToolCall::PresentDocument(args)
            | ToolCall::DocumentEdit(args)
            | ToolCall::DocumentConvert(args)
            | ToolCall::DocumentRestore(args) => args,
            _ => unreachable!("document dispatcher"),
        };
        if let Err(message) = validate_arguments(operation, &args) {
            return failure("invalid_document_arguments", message);
        }
        let resolved = match self
            .resolve_project_input_for_auth(&args.project, auth)
            .await
        {
            Ok(value) => value,
            Err(error) => return error.into_tool_result(),
        };
        if let Some(job_id) = args.job_id.as_deref() {
            let access = crate::runner_http::runner_access_from_auth(auth);
            let job = match self
                .runner_registry
                .get_job_for_auth(access.as_ref(), job_id)
                .await
            {
                Ok(job) => job,
                Err(_) => {
                    return failure(
                        "document_job_unavailable",
                        "Document Job is unavailable to the current caller",
                    )
                }
            };
            let Some(metadata) = job.document else {
                return failure("document_job_mismatch", "This is not a document Job");
            };
            if job.project_id.as_deref() != Some(resolved.resolved_id.as_str())
                || metadata.project_root != resolved.config.path
                || metadata.operation != operation
            {
                return failure(
                    "document_job_mismatch",
                    "Job operation and exact project root must match the original request",
                );
            }
            if !super::jobs::is_terminal_job_status(&job.status) {
                return ToolResult::ok(
                    json!({"status":"pending","project":resolved.resolved_id,"job_id":job_id,"request_id":metadata.request_id,"operation":operation,"continuation":{"tool":operation,"arguments":{"project":resolved.resolved_id,"job_id":job_id}}}),
                );
            }
            return self
                .read_document_manifest(&resolved.resolved_id, &metadata, Some(job_id), auth)
                .await;
        }
        let runner =
            match crate::plugin_gateway::resolve_runner(self, &resolved.config.client_id, auth)
                .await
            {
                Ok(value) => value,
                Err(_) => {
                    return failure(
                        "document_runner_unavailable",
                        "Document Runner is unavailable to the current caller",
                    )
                }
            };
        let response = match crate::plugin_gateway::execute_exact(
            self,
            &runner,
            PluginGatewayRequest::DocumentWorkerSpec {
                project_id: resolved.resolved_id.clone(),
                project_root: resolved.config.path.clone(),
            },
            auth,
        )
        .await
        {
            Ok(response) => response,
            Err(_) => {
                return failure(
                    "document_provider_unavailable",
                    "Document provider resolution failed before worker dispatch",
                )
            }
        };
        if let Some(error) = response.error {
            return failure(&error.code, error.message);
        }
        let Some(PluginGatewayResponsePayload::DocumentWorker { spec }) = response.payload else {
            return failure(
                "invalid_document_provider",
                "Runner returned an invalid document worker specification",
            );
        };
        let request_id = uuid::Uuid::new_v4().simple().to_string();
        let metadata = DocumentJobMetadata {
            request_id: request_id.clone(),
            operation: operation.into(),
            project_root: resolved.config.path.clone(),
            source_path: args.path.clone().expect("validated path"),
            provider_id: spec.provider_id.clone(),
            provider_instance_id: spec.provider_instance_id.clone(),
            export_file: args.export_file.unwrap_or(false),
        };
        let mut arguments = serde_json::to_value(&args).expect("document arguments serialize");
        for key in ["project", "session_id", "job_id", "export_file"] {
            arguments.as_object_mut().unwrap().remove(key);
        }
        let stdin = json!({"request_id":request_id,"operation":operation,"arguments":arguments})
            .to_string();
        let result = self
            .run_document_worker(
                resolved.resolved_id.clone(),
                spec,
                stdin,
                metadata.clone(),
                args.session_id,
                auth,
            )
            .await;
        if let Some(job_id) = result.output.get("job_id").and_then(Value::as_str) {
            return ToolResult {
                success: result.success,
                error: result.error,
                output: json!({"status":if result.success {"pending"} else {"outcome_unknown"},"project":resolved.resolved_id,"job_id":job_id,"request_id":request_id,"operation":operation,"continuation":{"tool":operation,"arguments":{"project":resolved.resolved_id,"job_id":job_id}}}),
            };
        }
        if !result.success
            && result
                .output
                .get("command_completed")
                .and_then(Value::as_bool)
                != Some(true)
        {
            return result;
        }
        self.read_document_manifest(&resolved.resolved_id, &metadata, None, auth)
            .await
    }

    async fn read_document_manifest(
        &self,
        project: &str,
        metadata: &DocumentJobMetadata,
        job_id: Option<&str>,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        let result_failure = |code: &str, message: &str| {
            ToolResult::err_with_output(
                message,
                json!({"error_kind":code,"request_id":metadata.request_id,"job_id":job_id,"failure_kind":"outcome_unknown","recovery_kind":"reconcile"}),
            )
        };
        if !valid_document_request_id(&metadata.request_id) {
            return result_failure(
                "invalid_document_continuation",
                "Document request identity is invalid",
            );
        }
        let manifest_path = format!(".webcodex-office/cache/{}/result.json", metadata.request_id);
        let mut bytes = Vec::new();
        let mut sha = None;
        for _ in 0..4 {
            let read = self
                .read_project_artifact(
                    project.into(),
                    manifest_path.clone(),
                    None,
                    Some(bytes.len()),
                    Some(64 * 1024),
                    None,
                )
                .await;
            if !read.success {
                return ToolResult::err_with_output(
                    "Document result is unavailable; inspect the existing Job before retrying",
                    json!({"error_kind":"document_result_unavailable","request_id":metadata.request_id,"job_id":job_id,"recovery_kind":"reconcile"}),
                );
            }
            let current = read
                .output
                .get("sha256")
                .and_then(Value::as_str)
                .unwrap_or("");
            if read
                .output
                .get("file_bytes")
                .and_then(Value::as_u64)
                .is_none_or(|size| size > 256 * 1024)
            {
                return result_failure(
                    "document_result_too_large",
                    "Document manifest exceeds 256 KiB",
                );
            }
            if current.len() != 64 || sha.as_deref().is_some_and(|previous| previous != current) {
                return result_failure(
                    "document_result_changed",
                    "Document result changed during bounded reading",
                );
            }
            sha = Some(current.to_string());
            let chunk = read
                .output
                .get("content_base64")
                .and_then(Value::as_str)
                .and_then(|text| STANDARD.decode(text).ok());
            let Some(chunk) = chunk else {
                return result_failure(
                    "invalid_document_result",
                    "Document manifest content is invalid",
                );
            };
            if chunk.is_empty() && read.output.get("eof").and_then(Value::as_bool) != Some(true) {
                return result_failure(
                    "invalid_document_result",
                    "Document manifest read made no progress",
                );
            }
            bytes.extend(chunk);
            if read.output.get("eof").and_then(Value::as_bool) == Some(true) {
                break;
            }
        }
        if sha.as_deref() != Some(format!("{:x}", Sha256::digest(&bytes)).as_str()) {
            return result_failure(
                "document_result_changed",
                "Document manifest bytes do not match their observed SHA",
            );
        }
        let manifest: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return result_failure(
                    "invalid_document_result",
                    "Document manifest is invalid or exceeds 256 KiB",
                )
            }
        };
        if manifest.get("request_id").and_then(Value::as_str) != Some(metadata.request_id.as_str())
        {
            return result_failure(
                "document_result_mismatch",
                "Document manifest belongs to another request",
            );
        }
        let Some(success) = manifest.get("success").and_then(Value::as_bool) else {
            return result_failure(
                "invalid_document_result",
                "Document result must declare boolean success",
            );
        };
        if success && !manifest.get("output").is_some_and(Value::is_object) {
            return result_failure(
                "invalid_document_result",
                "Successful document results require an output object",
            );
        }
        let mut output = manifest
            .get("output")
            .filter(|output| output.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if success && metadata.operation == "present_document" {
            let source_sha = output
                .get("source_sha256")
                .and_then(Value::as_str)
                .unwrap_or("");
            if output.get("path").and_then(Value::as_str) != Some(metadata.source_path.as_str())
                || source_sha.len() != 64
                || !source_sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return result_failure(
                    "invalid_document_source",
                    "Presentation must identify the exact requested source and SHA",
                );
            }
            let source = self
                .read_project_artifact_export_metadata_internal(
                    project,
                    &metadata.source_path,
                    auth,
                )
                .await;
            if !source.success
                || source.output.get("sha256").and_then(Value::as_str) != Some(source_sha)
            {
                return ToolResult::err_with_output("Document changed after presentation; call present_document again to read its saved current version",json!({"error_kind":"document_source_changed","project":project,"path":metadata.source_path,"request_id":metadata.request_id,"recovery_kind":"reobserve"}));
            }
        }
        let prefix = format!(".webcodex-office/cache/{}/", metadata.request_id);
        if let Some(previews) = output.get("previews") {
            let Some(previews) = previews.as_array() else {
                return result_failure(
                    "invalid_document_result",
                    "Document previews must be an array",
                );
            };
            if previews.len() > 2
                || previews.iter().any(|preview| {
                    let path = preview.get("path").and_then(Value::as_str).unwrap_or("");
                    !path.starts_with(&prefix)
                        || path.contains(['\\', ':'])
                        || path
                            .split('/')
                            .any(|component| matches!(component, ".." | "." | ""))
                        || preview
                            .get("sha256")
                            .and_then(Value::as_str)
                            .is_none_or(|hash| hash.len() != 64)
                        || preview.get("source_sha256") != output.get("source_sha256")
                })
            {
                return result_failure(
                    "invalid_document_preview",
                    "Document preview references must belong to this bounded request",
                );
            }
        }
        output["project"] = json!(project);
        output["request_id"] = json!(metadata.request_id);
        output["operation"] = json!(metadata.operation);
        output["source_path"] = json!(metadata.source_path);
        output["export_file"] = json!(metadata.export_file);
        output["status"] = json!("completed");
        if let Some(job_id) = job_id {
            output["job_id"] = json!(job_id);
        }
        if success {
            ToolResult::ok(output)
        } else {
            output["error_kind"] = manifest
                .pointer("/error/code")
                .cloned()
                .unwrap_or_else(|| json!("document_worker_failed"));
            ToolResult::err_with_output(
                manifest
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Document worker failed"),
                output,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_paths_and_continuation_are_bounded() {
        for path in [
            "C:/x.docx",
            "../x.docx",
            "/x.docx",
            ".webcodex-office/backups/x.docx",
            "a//x.docx",
        ] {
            assert!(!valid_relative_path(path), "{path}");
        }
        assert!(valid_relative_path("资料/报告.docx"));
        assert!(valid_document_request_id(&"a".repeat(32)));
        assert!(!valid_document_request_id("../result"));
        let args: DocumentToolCall =
            serde_json::from_value(json!({"project":"p","job_id":"job","path":"x.docx"})).unwrap();
        assert!(validate_arguments("present_document", &args).is_err());
    }
}
