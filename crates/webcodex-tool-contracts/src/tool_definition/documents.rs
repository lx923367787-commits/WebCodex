use super::*;
use crate::metadata::{
    ToolPathHint::SinglePath, ToolRisk::RunControl, JOB_RUN, PLUGIN_INVOKE, PROJECT_READ,
    PROJECT_WRITE, TOOL_PROVIDER_RUNNER,
};
use crate::registry::input_schemas::*;

const DOCUMENT_AUDIT_FIELDS: &[ToolAuditResultField] = &[
    ToolAuditResultField::value("project"),
    ToolAuditResultField::value("path"),
    ToolAuditResultField::value("request_id"),
    ToolAuditResultField::value("job_id"),
    ToolAuditResultField::value("status"),
    ToolAuditResultField::value("source_sha256"),
    ToolAuditResultField::value("review"),
    ToolAuditResultField::value("review_status"),
    ToolAuditResultField::value("backup_id"),
    ToolAuditResultField::value("error_kind"),
];

const fn document(
    name: &'static str,
    description: &'static str,
    schema: fn() -> serde_json::Value,
    write: bool,
    rank: u16,
) -> ToolDefinition {
    adaptive_runtime_direct(
        require_all_scopes(
            permission_risk(
                model_spec(
                    def(
                        name,
                        ToolAuditPolicy::typed_fields(DOCUMENT_AUDIT_FIELDS).session_input(
                            ToolAuditSessionInputPolicy::OmitTopLevel(&["operations", "options"]),
                        ),
                        ToolVisibility::ModelVisible,
                        TOOL_CATEGORY_FILE,
                        Some(RunnerCapabilityRequirement::Shell),
                        TOOL_PROVIDER_RUNNER,
                        ToolSemanticContract {
                            effect: if write {
                                ToolEffect::Mutate
                            } else {
                                ToolEffect::Execute
                            },
                            risk: RunControl,
                            approval: ToolApprovalPolicy::Standard,
                            idempotency: ToolIdempotency::NonIdempotent,
                        },
                        Some(if write { PROJECT_WRITE } else { PROJECT_READ }),
                        true,
                        SinglePath,
                        write,
                        true,
                        ToolSessionEvidencePolicy::NONE,
                    ),
                    description,
                    schema,
                ),
                if write {
                    PERMISSION_RISK_WRITE
                } else {
                    PERMISSION_RISK_SHELL
                },
            ),
            if write {
                &[PROJECT_READ, PROJECT_WRITE, JOB_RUN, PLUGIN_INVOKE]
            } else {
                &[PROJECT_READ, JOB_RUN, PLUGIN_INVOKE]
            },
        ),
        rank,
    )
}

pub(super) const DEFINITIONS: &[ToolDefinition] = &[
    document("present_document", "Inspect a registered project's document as bounded real text/tables and native MCP page images. Read-only inspection is allowed before goal plan confirmation without goal_context and preserves the source. Uses the configured worker; no arbitrary commands or roots. auto gives a short overview and at most two previews; follow source-bound cursors for every page. review defaults to 0; review=1 stays pending until the host judges all current image scopes. review_material gives coverage, source/render fingerprints and continuation; paths are not approval. options.review_probe=true reads current source/render dependencies without new images. Resume slow work with project+job_id only to observe the same Job. export_file returns the original through an authenticated MCP ResourceLink; it does not guarantee native chat attachment ingestion.", present_document_input_schema, false, 57),
    document("document_edit", "Edit an office document using structured operations and its current expected_sha256. copy writes a new output_path; in_place creates a recoverable backup and refuses stale source bytes. Defaults to copy with output_path, otherwise in_place. Layout edits force review=1, correcting explicit 0 with a reason. Machine success leaves review_status=pending: follow review_material.continuation to render the actual saved output and obtain a version-bound model judgment before managed delivery. A slow operation returns job_id: resume with project+job_id only; never blindly repeat an uncertain mutation.", document_edit_input_schema, true, 58),
    document("document_convert", "Convert a local document with the operator-configured worker and explicit output_path. Preserves the source. Format-specific options are validated by the worker. Conversion forces review=1, correcting explicit 0; machine success leaves the new source-bound delivery pending actual image review via review_material.continuation. Slow work returns job_id; resume with project+job_id, never repeat an uncertain operation.", document_convert_input_schema, true, 59),
    document("document_restore", "List bounded backup metadata for path when backup_id is omitted, or restore an exact backup with the current target expected_sha256. Refuses conflicting local changes. Listing defaults to review=0; restoring a layout forces review=1, including explicit 0, and remains pending actual image review. Slow work returns job_id; resume with project+job_id only instead of replaying the restoration. Goal mode allows metadata inspection and existing Job continuation before plan confirmation; starting a backup restoration still requires a confirmed plan.", document_restore_input_schema, true, 61),
];
