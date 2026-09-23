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
    document("present_document", "Read a local office document as bounded real text/tables and, over MCP, native page images. Goal mode allows this inspection before plan creation or confirmation, without goal_context; parsing and previews preserve the source. Uses the exact registered project's configured document worker; no arbitrary commands or roots. auto returns a short overview with at most two page previews; follow the source-bound cursor for more. A slow worker returns an existing durable job_id: resume this tool with project+job_id only. export_file optionally delivers the original by authenticated MCP ResourceLink, not a promise of native chat attachment ingestion.", present_document_input_schema, false, 57),
    document("document_edit", "Edit an office document using structured operations and its current expected_sha256. copy writes a new output_path; in_place creates a recoverable backup and refuses stale source bytes. Defaults to copy with output_path, otherwise in_place. A slow operation returns job_id: resume with project+job_id only; never blindly repeat an uncertain mutation. Read and inspect the completed artifact before claiming success.", document_edit_input_schema, true, 58),
    document("document_convert", "Convert a local document with the operator-configured worker and explicit output_path. Preserves the source. Format-specific options are validated by the worker. Slow work returns job_id; resume with project+job_id, never repeat an uncertain operation.", document_convert_input_schema, true, 59),
    document("document_restore", "List bounded backup metadata for path when backup_id is omitted, or restore an exact backup with the current target expected_sha256. Refuses conflicting local changes. Slow work returns job_id; resume with project+job_id only instead of replaying the restoration. Goal mode allows metadata inspection and existing Job continuation before plan confirmation; starting a backup restoration still requires a confirmed plan.", document_restore_input_schema, true, 61),
];
