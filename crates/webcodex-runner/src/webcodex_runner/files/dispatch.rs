//! Runner files dispatch implementation.

use crate::webcodex_runner::{
    handle_apply_patch_file_request, handle_apply_text_edits_file_request,
    handle_artifact_file_operation, handle_basic_file_request, handle_checkpoint_file_request,
    handle_write_project_file_request, resolve_requested_path,
    validate_structured_edit_runner_path, CommandResult, RunnerPolicy,
};
#[cfg(test)]
use crate::webcodex_runner::{
    is_artifact_request_kind, is_basic_file_request_kind, is_checkpoint_request_kind,
    is_structured_edit_request_kind,
};
use std::time::Instant;
use webcodex_core::runner_operation::RunnerFileOperation;
#[cfg(test)]
use webcodex_core::runner_operation::RunnerOperation;
#[cfg(test)]
use webcodex_core::runner_protocol::RunnerRequest;

#[cfg(test)]
pub(crate) fn is_file_request_kind(kind: &str) -> bool {
    is_basic_file_request_kind(kind)
        || is_structured_edit_request_kind(kind)
        || is_artifact_request_kind(kind)
        || is_checkpoint_request_kind(kind)
}

pub(crate) fn handle_file_operation(
    policy: &RunnerPolicy,
    operation: &RunnerFileOperation,
) -> CommandResult {
    let request = operation.payload();
    let path = request.path.as_str();
    let start = Instant::now();
    if matches!(
        operation,
        RunnerFileOperation::WriteProjectFile(_)
            | RunnerFileOperation::ApplyTextEdits(_)
            | RunnerFileOperation::ApplyPatch(_)
    ) {
        if let Err(e) = validate_structured_edit_runner_path(path) {
            return CommandResult {
                exit_code: None,
                stdout: None,
                stderr: None,
                duration_ms: Some(0),
                error: Some(e),
            };
        }
    }
    let resolved = match resolve_requested_path(policy, request.cwd.as_deref(), path) {
        Ok(path) => path,
        Err(e) => {
            return CommandResult {
                exit_code: None,
                stdout: None,
                stderr: None,
                duration_ms: Some(0),
                error: Some(e),
            }
        }
    };
    match operation {
        RunnerFileOperation::WriteProjectFile(_) => {
            handle_write_project_file_request(request, &resolved, start)
        }
        RunnerFileOperation::ApplyTextEdits(_) => {
            handle_apply_text_edits_file_request(policy, request, start)
        }
        RunnerFileOperation::ApplyPatch(_) => {
            handle_apply_patch_file_request(policy, request, start)
        }
        RunnerFileOperation::SaveProjectArtifact(_)
        | RunnerFileOperation::ReadProjectArtifactMetadata(_)
        | RunnerFileOperation::ReadProjectArtifact(_)
        | RunnerFileOperation::ReadProjectArtifactExportChunk(_)
        | RunnerFileOperation::ArtifactUploadBegin(_)
        | RunnerFileOperation::ArtifactUploadChunk(_)
        | RunnerFileOperation::ArtifactUploadFinish(_)
        | RunnerFileOperation::ArtifactUploadAbort(_) => {
            handle_artifact_file_operation(operation, &resolved, start)
        }
        RunnerFileOperation::CheckpointCreate(_) | RunnerFileOperation::CheckpointRestore(_) => {
            handle_checkpoint_file_request(operation, &resolved, start)
        }
        RunnerFileOperation::Read(_)
        | RunnerFileOperation::Write(_)
        | RunnerFileOperation::List(_)
        | RunnerFileOperation::ProjectOverview(_)
        | RunnerFileOperation::DeleteProjectFiles(_)
        | RunnerFileOperation::SkillListPackages(_)
        | RunnerFileOperation::SkillReadFile(_) => {
            handle_basic_file_request(policy, operation, &resolved, start)
        }
    }
}

#[cfg(test)]
pub(crate) fn handle_file_request(policy: &RunnerPolicy, request: &RunnerRequest) -> CommandResult {
    match request.decode_operation() {
        Ok(RunnerOperation::File(operation)) => handle_file_operation(policy, &operation),
        _ => CommandResult {
            exit_code: None,
            stdout: None,
            stderr: None,
            duration_ms: Some(0),
            error: Some("invalid file request".to_string()),
        },
    }
}
