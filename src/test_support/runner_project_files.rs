//! Narrow Runner file protocol fixture backed by an isolated project directory.
use crate::runner_protocol::{
    RunnerPollRequest, RunnerRequest, RunnerResultPayload, RunnerResultRequest,
};
use crate::tool_runtime::ToolRuntime;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{future::Future, path::Path, time::Duration};

fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn is_goal_file(request: &RunnerRequest) -> bool {
    request.path.as_deref().is_some_and(|path| {
        path.split(['/', '\\'])
            .any(|part| matches!(part, "goals" | "handoff"))
    })
}

fn output(root: &Path, request: &RunnerRequest) -> Result<Value, String> {
    let path = request.path.as_deref().ok_or("missing fixture file path")?;
    webcodex_core::goal::validate_goal_relative_path(path)?;
    let cwd = Path::new(
        request
            .cwd
            .as_deref()
            .ok_or("missing fixture project root")?,
    );
    if cwd.canonicalize().map_err(|e| e.to_string())?
        != root.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("fixture request targeted a different project root".into());
    }
    let target = root.join(path);
    let payload: Value = request
        .content
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| json!({}));
    match request.kind.as_str() {
        "file_write_project_file" | "file_write" => {
            if target.exists() && payload["overwrite"] != true {
                return Ok(
                    json!({"path":path,"error":"File already exists; overwrite was not requested","applied":false}),
                );
            }
            if let Some(expected) = payload["expected_sha256"].as_str() {
                let bytes = std::fs::read(&target).map_err(|e| e.to_string())?;
                if fingerprint(&bytes) != expected {
                    return Ok(
                        json!({"path":path,"error":"SHA256 precondition failed","applied":false}),
                    );
                }
            }
            let content = payload["content"]
                .as_str()
                .or_else(|| {
                    (request.kind == "file_write")
                        .then_some(request.content.as_deref())
                        .flatten()
                })
                .ok_or("fixture write has no content")?;
            std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            let created = !target.exists();
            std::fs::write(&target, content).map_err(|e| e.to_string())?;
            Ok(
                json!({"path":path,"created":created,"bytes_written":content.len(),
                "sha256":fingerprint(content.as_bytes()),
                "applied":true,"changed":true,"execution_state":"completed","changed_paths":[path]}),
            )
        }
        "file_read_project_artifact_metadata" => match std::fs::read(&target) {
            Ok(bytes) => Ok(
                json!({"path":path,"exists":true,"missing":false,"bytes":bytes.len(),
                "sha256":fingerprint(&bytes),"mime_type":"application/octet-stream"}),
            ),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && payload["allow_missing"] == true =>
            {
                Ok(json!({"path":path,"exists":false,"missing":true}))
            }
            Err(error) => Err(error.to_string()),
        },
        "file_read_project_artifact" => {
            let bytes = std::fs::read(&target).map_err(|e| e.to_string())?;
            let offset = payload["offset"].as_u64().unwrap_or(0) as usize;
            let end = offset
                .saturating_add(payload["max_bytes"].as_u64().unwrap_or(65536) as usize)
                .min(bytes.len());
            if offset > end {
                return Err("fixture read offset exceeds file size".into());
            }
            Ok(
                json!({"path":path,"mime_type":"application/json","file_bytes":bytes.len(),
                "sha256":fingerprint(&bytes),"offset":offset,
                "bytes_returned":end-offset,"content_base64":base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end]),
                "next_offset":end,"truncated":end<bytes.len(),"eof":end==bytes.len()}),
            )
        }
        "file_read" => {
            use webcodex_workspace::file_read_range::{read_range_from, EffectiveRange};
            let bytes = std::fs::read(&target).map_err(|e| e.to_string())?;
            let start = request.start_line.unwrap_or(1);
            let limit = request
                .end_line
                .map(|end| end.saturating_sub(start) + 1)
                .unwrap_or(300);
            let result = read_range_from(
                bytes.as_slice(),
                EffectiveRange::new(Some(start), Some(limit)),
            )
            .map_err(|error| format!("{error:?}"))?;
            Ok(
                json!({"format":"webcodex.file_read_range.v1","content":result.content,
                "sha256":result.sha256,"total_lines":result.total_lines,"start_line":result.start_line,"limit":result.limit}),
            )
        }
        kind => Err(format!(
            "Unexpected Runner operation in goal file fixture: {kind}"
        )),
    }
}

pub(crate) async fn serve(
    runtime: &ToolRuntime,
    instance: &str,
    root: &Path,
    request: &RunnerRequest,
) {
    let result = output(root, request);
    runtime
        .runner_registry
        .complete(RunnerResultPayload {
            result: RunnerResultRequest {
                client_id: request.client_id.clone(),
                runner_instance_id: instance.into(),
                request_id: request.request_id.clone(),
                exit_code: Some(if result.is_ok() { 0 } else { 1 }),
                stdout: result.as_ref().ok().map(Value::to_string),
                stderr: result.err(),
                duration_ms: Some(1),
                error: None,
            },
            command_execution_state: None,
            mcp_gateway: None,
            plugin_gateway: None,
            coding_agent: None,
        })
        .await
        .unwrap();
}

/// Service only Goal artifact I/O, leaving the first actual business request
/// uncompleted for its caller's existing assertion or manual Runner fixture.
pub(crate) async fn next_business_request(
    runtime: &ToolRuntime,
    client: &str,
    instance: &str,
    root: &Path,
    deadline: tokio::time::Instant,
) -> RunnerRequest {
    loop {
        let request = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => panic!("Goal fixture did not finish within its 10-second file protocol budget"),
            request = runtime.runner_registry.poll(RunnerPollRequest {client_id:client.into(),runner_instance_id:instance.into()}) => request.unwrap(),
        };
        if let Some(request) = request {
            if !is_goal_file(&request) {
                return request;
            }
            serve(runtime, instance, root, &request).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
}

/// Goal controls and rejected business admissions may verify plan files. Any
/// actual business dispatch remains an assertion failure, never auto-executed.
pub(crate) async fn during<F: Future>(
    runtime: &ToolRuntime,
    client: &str,
    instance: &str,
    root: &Path,
    future: F,
) -> F::Output {
    tokio::pin!(future);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    tokio::select! {
        biased;
        result = &mut future => result,
        request = next_business_request(runtime, client, instance, root, deadline) => {
            panic!("Goal/admission fixture unexpectedly dispatched business work: {} {:?}",request.kind,request.path);
        }
    }
}
