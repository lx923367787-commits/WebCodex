use super::{
    def, model_spec, permission_risk, require_all_scopes, ToolApprovalPolicy, ToolAuditPolicy,
    ToolAuditResultField, ToolDefinition, ToolEffect, ToolIdempotency, ToolInputSchemaFactory,
    ToolSemanticContract, ToolSessionEvidencePolicy, ToolVisibility, PERMISSION_RISK_JOB,
    PERMISSION_RISK_WRITE, TOOL_CATEGORY_AGENT_TASK,
};
use crate::metadata::{
    ToolPathHint, ToolRisk, COMMUNICATION_MANAGE, COMMUNICATION_READ, JOB_RUN, PLUGIN_INSPECT,
    PLUGIN_INVOKE, PROJECT_READ, PROJECT_WRITE, TOOL_PROVIDER_CONTROL,
};
use crate::registry::input_schemas::*;

const READ_SCOPES: &[&str] = &[PROJECT_READ, COMMUNICATION_READ];
const WRITE_SCOPES: &[&str] = &[PROJECT_WRITE, COMMUNICATION_MANAGE];
const START_SCOPES: &[&str] = &[
    PROJECT_WRITE,
    COMMUNICATION_MANAGE,
    JOB_RUN,
    PLUGIN_INSPECT,
    PLUGIN_INVOKE,
];
const AGENT_START_SCOPES: &[&str] = &[
    PROJECT_WRITE,
    COMMUNICATION_MANAGE,
    PLUGIN_INSPECT,
    PLUGIN_INVOKE,
];
const AGENT_AUDIT: ToolAuditPolicy = ToolAuditPolicy::typed_fields(&[
    ToolAuditResultField::pointer("root_task_id", "/attempt/root_task_id"),
    ToolAuditResultField::pointer("source_task_id", "/attempt/source_task_id"),
    ToolAuditResultField::pointer("team_agent_attempt_id", "/attempt/team_agent_attempt_id"),
    ToolAuditResultField::pointer("state", "/attempt/state"),
    ToolAuditResultField::value("created"),
    ToolAuditResultField::value("replayed"),
    ToolAuditResultField::value("queued"),
    ToolAuditResultField::value("error_kind"),
]);
const AUDIT: ToolAuditPolicy = ToolAuditPolicy::typed_fields(&[
    ToolAuditResultField::pointer("root_task_id", "/plan/root_task_id"),
    ToolAuditResultField::pointer("source_attempt_id", "/attempt/attempt_id"),
    ToolAuditResultField::pointer("job_id", "/attempt/job_id"),
    ToolAuditResultField::value("state_changed"),
    ToolAuditResultField::value("replayed"),
    ToolAuditResultField::value("error_kind"),
]);

const fn read(
    name: &'static str,
    description: &'static str,
    schema: ToolInputSchemaFactory,
) -> ToolDefinition {
    require_all_scopes(
        model_spec(
            def(
                name,
                AUDIT,
                ToolVisibility::ModelVisible,
                TOOL_CATEGORY_AGENT_TASK,
                None,
                TOOL_PROVIDER_CONTROL,
                ToolSemanticContract {
                    effect: ToolEffect::Observe,
                    risk: ToolRisk::Read,
                    approval: ToolApprovalPolicy::None,
                    idempotency: ToolIdempotency::PureRead,
                },
                Some(PROJECT_READ),
                true,
                ToolPathHint::None,
                false,
                false,
                ToolSessionEvidencePolicy::NONE,
            ),
            description,
            schema,
        ),
        READ_SCOPES,
    )
}

const fn write(
    name: &'static str,
    description: &'static str,
    schema: ToolInputSchemaFactory,
    idempotency: ToolIdempotency,
) -> ToolDefinition {
    permission_risk(
        require_all_scopes(
            model_spec(
                def(
                    name,
                    AUDIT,
                    ToolVisibility::ModelVisible,
                    TOOL_CATEGORY_AGENT_TASK,
                    None,
                    TOOL_PROVIDER_CONTROL,
                    ToolSemanticContract {
                        effect: ToolEffect::Mutate,
                        risk: ToolRisk::WorkflowManage,
                        approval: ToolApprovalPolicy::Standard,
                        idempotency,
                    },
                    Some(PROJECT_WRITE),
                    true,
                    ToolPathHint::None,
                    false,
                    false,
                    ToolSessionEvidencePolicy::NONE,
                ),
                description,
                schema,
            ),
            WRITE_SCOPES,
        ),
        PERMISSION_RISK_WRITE,
    )
}

pub(super) const DEFINITIONS: &[ToolDefinition] = &[
    read("team_policy_get", "Read this caller's project team policy and revision. Subagents and CLI calls are optional. Historical count ranges are compatibility data, not quotas or success requirements; model and effort choices are optional preferences. This read starts no work.", team_policy_get_input_schema),
    write("team_policy_save", "Save project team policy with expected_revision compare-and-swap. Count ranges and enabled provider preferences do not impose dispatch or completion quotas, fixed models or thinking strengths. Existing frozen plans retain their recorded policy; explicit project_policy effort selection resolves the exact saved preference. Saving policy neither creates Agents nor invokes a CLI.", team_policy_save_input_schema, ToolIdempotency::NonIdempotent),
    write("team_plan_register", "Freeze a plan for an owned root AgentTask (manager=root) at the exact policy revision; account/project authorization applies to every child and CLI. Model/effort are optional: omit/null uses adapter-supported CLI defaults with no model flag; explicit values must be discovered. project_policy effort requires the exact saved model/revision. Give each CLI a nonblank independent instruction; never inherit the parent goal. Duplicate review instructions are allowed. Optional input snapshots: up to 8, 32 KiB total JSON; names are labels, not existing project files. Only memory, objectives and acceptance criteria follow personal-directory conventions; other authorized files/outputs are shared. Text/snapshots are not shell arguments and grant no permissions. Freezes assignments/slots without starting work. After response loss reuse the same key and payload.", team_plan_register_input_schema, ToolIdempotency::Keyed),
    read("team_plan_list", "List up to 20 owned team-plan summaries in this exact project. Pagination is read-only; listing never creates workers, starts CLI calls, or triggers recovery.", team_plan_list_input_schema),
    read("team_plan_read", "Read one owned frozen team plan by exact project and root_task_id. CLI attempts use attempts_offset/attempts_limit, default 0/20 and maximum 20 per page, in stable insertion order with earliest attempts first. Continue with output.attempts.next_offset; new attempts append and status updates never reorder existing rows. Refresh earlier pages for their latest states. Other collections retain their separate bounds. Counts record ordinary assignments and executions; they are not success requirements. Reading grants no execution authority and starts no work.", team_plan_read_input_schema),
    require_all_scopes(write("team_plan_cancel", "Cancel the exact owned ordinary team plan, prevent new planned CLI dispatch and request stops for its bound Jobs. Requires job:run in addition to project and communication management. Cancellation is not proof that an already-started process stopped; observe its original attempt/Job and preserve uncertain execution. This action never starts a replacement conversation.", team_plan_cancel_input_schema, ToolIdempotency::DesiredState), &[PROJECT_WRITE, COMMUNICATION_MANAGE, JOB_RUN]),
    permission_risk(require_all_scopes(model_spec(def("team_cli_start", AUDIT,
        ToolVisibility::ModelVisible, TOOL_CATEGORY_AGENT_TASK,
        Some(super::RunnerCapabilityRequirement::StructuredProcess), TOOL_PROVIDER_CONTROL,
        ToolSemanticContract { effect: ToolEffect::Execute, risk: ToolRisk::JobRun,
            approval: ToolApprovalPolicy::Standard, idempotency: ToolIdempotency::Keyed },
        Some(JOB_RUN), true, ToolPathHint::None, true, true, ToolSessionEvidencePolicy::NONE),
        "Start an exact frozen team CLI slot using its stored subtask instruction, provider, model and effort. Every call requires an independent explicit instruction; the source Agent goal is never inherited. Start accepts no executable, shell command or replacement prompt; task text grants no additional permissions. Omit fallback_thinking_profile_id or use default to retain the recovery chat setting, or explicitly choose a discovered profile ID; the selection is frozen on this attempt. Eligible CLI unavailability may use the separate single recovery slot for a new ChatGPT conversation invoking @webcodx, not a Codex CLI fallback. Start authorization covers the bounded recovery hook. Unknown dispatch or unconfirmed termination never authorizes replacement. Retry only identical key/payload and observe the original source_attempt_id after response loss. timeout_secs defaults to 900, maximum 3600.",
        team_cli_start_input_schema), START_SCOPES), PERMISSION_RISK_JOB),
    read("team_cli_observe", "Read one exact owned CLI attempt and its recorded execution/recovery status. Pure observation: this call does not dispatch a CLI, reserve a recovery slot, cancel execution, invoke a plugin, or create/send to a ChatGPT conversation. Unknown or lost outcomes require reconciliation of the original attempt, never a fresh start.", team_cli_observe_input_schema),
    permission_risk(require_all_scopes(model_spec(def("team_agent_start", AGENT_AUDIT,
        ToolVisibility::ModelVisible, TOOL_CATEGORY_AGENT_TASK, None, TOOL_PROVIDER_CONTROL,
        ToolSemanticContract { effect: ToolEffect::Execute, risk: ToolRisk::WorkflowManage,
            approval: ToolApprovalPolicy::Standard, idempotency: ToolIdempotency::Keyed },
        Some(PROJECT_WRITE), true, ToolPathHint::None, false, false, ToolSessionEvidencePolicy::NONE),
        "Queue one ordinary ChatGPT subagent already present in this owned frozen team plan. root_task_id and source_task_id must be real authorized AgentTasks; instructions come from the stored task. Omit thinking_profile_id to keep page settings without discovery or readback; legacy default also leaves settings unchanged. An explicit ID requests an optional UI adjustment, whose failure is reported separately and never cancels dispatch. This starts the specified independent task; it does not impose a minimum or maximum number of Agents or CLI calls. A trusted Host dispatcher starts the queued conversation; queued/response_observed is not proof of business completion. Retry only the identical key and arguments; unknown dispatch never authorizes a new send. Caller-supplied recovery flags, commands and instructions are not accepted.",
        team_agent_start_input_schema), AGENT_START_SCOPES), PERMISSION_RISK_WRITE),
    require_all_scopes(model_spec(def("team_agent_observe", AGENT_AUDIT,
        ToolVisibility::ModelVisible, TOOL_CATEGORY_AGENT_TASK, None, TOOL_PROVIDER_CONTROL,
        ToolSemanticContract { effect: ToolEffect::Observe, risk: ToolRisk::Read,
            approval: ToolApprovalPolicy::None, idempotency: ToolIdempotency::PureRead },
        Some(PROJECT_READ), true, ToolPathHint::None, false, false, ToolSessionEvidencePolicy::NONE),
        "Read one exact owned ordinary team-agent attempt in its project. This read never spawns, resumes, cancels, invokes a plugin, or reserves recovery. State and response_observed are transport observations, not verified task completion; inspect original evidence for an unknown dispatch instead of starting a replacement.",
        team_agent_observe_input_schema), READ_SCOPES),
];
