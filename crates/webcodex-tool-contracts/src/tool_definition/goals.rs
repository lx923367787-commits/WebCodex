use super::{
    def, model_spec, permission_risk, require_all_scopes, require_any_scopes, RunnerCapabilityRequirement,
    ToolApprovalPolicy, ToolAuditPolicy, ToolAuditResultField, ToolDefinition, ToolEffect,
    ToolIdempotency, ToolSemanticContract, ToolSessionEvidencePolicy, ToolVisibility,
    PERMISSION_RISK_JOB, PERMISSION_RISK_WRITE, TOOL_CATEGORY_GOAL,
};
use crate::metadata::{ToolPathHint, ToolRisk, CODING_AGENT_RUN, TOOL_PROVIDER_CONTROL};
use crate::registry::input_schemas::{
    goal_cancel_input_schema, goal_list_input_schema, goal_read_input_schema,
    goal_resume_input_schema, goal_start_input_schema,
};

const EXECUTION_SCOPES: &[&str] = &[
    CODING_AGENT_RUN,
    webcodex_core::authority::SCOPE_PROJECT_WRITE,
    webcodex_core::authority::SCOPE_PROJECT_READ,
    webcodex_core::authority::SCOPE_JOB_RUN,
    webcodex_core::authority::SCOPE_JOB_DETACH,
];

const GOAL_AUDIT: ToolAuditPolicy = ToolAuditPolicy::typed_fields(&[
    ToolAuditResultField::pointer("goal_id", "/goal/goal_id"),
    ToolAuditResultField::pointer("state", "/goal/state"),
    ToolAuditResultField::pointer_array_len("returned_round_count", "/goal/rounds"),
    ToolAuditResultField::value("total_rounds"),
    ToolAuditResultField::value("delivery_freshness"),
    ToolAuditResultField::value("delivery_ready"),
    ToolAuditResultField::value("replayed"),
    ToolAuditResultField::value("error_kind"),
]);

// Host goal.state is private task content rather than the ACP phase string.
// A shared read projection must never copy it into the audit ledger.
const GOAL_READ_AUDIT: ToolAuditPolicy = ToolAuditPolicy::typed_fields(&[
    ToolAuditResultField::pointer("goal_id", "/goal/goal_id"),
    ToolAuditResultField::pointer("phase", "/goal/phase"),
    ToolAuditResultField::pointer_array_len("returned_round_count", "/goal/rounds"),
    ToolAuditResultField::value("total_rounds"),
    ToolAuditResultField::value("delivery_freshness"),
    ToolAuditResultField::value("delivery_ready"),
    ToolAuditResultField::value("error_kind"),
]);

pub(super) const DEFINITIONS: &[ToolDefinition] = &[
    permission_risk(
        model_spec(
            require_all_scopes(
                def(
                    "goal_start",
                    GOAL_AUDIT,
                    ToolVisibility::ModelVisible,
                    TOOL_CATEGORY_GOAL,
                    Some(RunnerCapabilityRequirement::CodingAgentRuns),
                    TOOL_PROVIDER_CONTROL,
                    ToolSemanticContract {
                        effect: ToolEffect::Execute,
                        risk: ToolRisk::JobRun,
                        approval: ToolApprovalPolicy::Standard,
                        idempotency: ToolIdempotency::Keyed,
                    },
                    Some(CODING_AGENT_RUN),
                    true,
                    ToolPathHint::None,
                    true,
                    false,
                    ToolSessionEvidencePolicy::NONE,
                ),
                EXECUTION_SCOPES,
            ),
            "Start an explicitly authorized bounded goal loop on one exact Project. Each round executes the objective through the configured ACP provider, collects artifact evidence, and checks immutable acceptance criteria; a provider end_turn alone never completes the goal. The Server persists rounds and only advances after verification. Use goal_read to inspect progress. Reuse the same idempotency_key after uncertain admission. Runner/provider permissions remain authoritative; budget exhaustion, approval needs, lost execution, or unavailable evidence stop continuation for inspection rather than silently retrying uncertain effects.",
            goal_start_input_schema,
        ),
        PERMISSION_RISK_JOB,
    ),
    model_spec(
        require_any_scopes(def(
            "goal_read",
            GOAL_READ_AUDIT,
            ToolVisibility::ModelVisible,
            TOOL_CATEGORY_GOAL,
            None,
            TOOL_PROVIDER_CONTROL,
            ToolSemanticContract {
                effect: ToolEffect::Observe,
                risk: ToolRisk::Read,
                approval: ToolApprovalPolicy::None,
                idempotency: ToolIdempotency::PureRead,
            },
            Some(CODING_AGENT_RUN),
            false,
            ToolPathHint::None,
            false,
            false,
            ToolSessionEvidencePolicy::NONE,
        ), &[CODING_AGENT_RUN, webcodex_core::authority::SCOPE_PROJECT_READ]),
        "Read an owned ACP or web-hosted goal after driver-specific project checks. ACP retains bounded rounds and delivery freshness, paged with round_offset/round_limit. Host defaults to an 8 KiB partial summary. Before claiming a Host node, request view=node with its node_id for the complete requirements/node context (at most 16 KiB); only that view can return context_read.complete. Read full plan/state or checkpoint/attempt/operation history with view=detail and section, concatenate JSON text fragments and follow next_cursor until complete; each detail page is at most 8 KiB. Summary or bootstrap is not a complete state snapshot. Reading never confirms a plan or starts execution. Use goal_bootstrap to find the local current task in a fresh conversation.",
        goal_read_input_schema,
    ),
    model_spec(
        require_any_scopes(def(
            "goal_list",
            ToolAuditPolicy::typed_fields(&[
                ToolAuditResultField::array_len("goal_count", "goals"),
                ToolAuditResultField::value("error_kind"),
            ]),
            ToolVisibility::ModelVisible,
            TOOL_CATEGORY_GOAL,
            None,
            TOOL_PROVIDER_CONTROL,
            ToolSemanticContract {
                effect: ToolEffect::Observe,
                risk: ToolRisk::Read,
                approval: ToolApprovalPolicy::None,
                idempotency: ToolIdempotency::PureRead,
            },
            Some(CODING_AGENT_RUN),
            false,
            ToolPathHint::None,
            false,
            false,
            ToolSessionEvidencePolicy::NONE,
        ), &[CODING_AGENT_RUN, webcodex_core::authority::SCOPE_PROJECT_READ]),
        "List a bounded number of owned ACP and web-hosted goals, optionally filtered by exact Project. Driver-specific project scopes and ownership are checked for each result; this admission does not grant access to the other driver's records. Observation only: never selects a task, confirms a plan or dispatches work. Use goal_bootstrap for the explicitly selected local current web task, rather than guessing the newest list entry.",
        goal_list_input_schema,
    ),
    permission_risk(
        model_spec(
            def(
                "goal_cancel",
                GOAL_AUDIT,
                ToolVisibility::ModelVisible,
                TOOL_CATEGORY_GOAL,
                None,
                TOOL_PROVIDER_CONTROL,
                ToolSemanticContract {
                    effect: ToolEffect::Mutate,
                    risk: ToolRisk::RunControl,
                    approval: ToolApprovalPolicy::InheritFromStart,
                    idempotency: ToolIdempotency::DesiredState,
                },
                Some(CODING_AGENT_RUN),
                false,
                ToolPathHint::None,
                true,
                false,
                ToolSessionEvidencePolicy::NONE,
            ),
            "Request cancellation of an owned goal and its current execution. This inherits admitted lifecycle-control authority and never grants permission or starts replacement work. Inspect goal_read for authoritative stopping state and any unresolved execution outcome.",
            goal_cancel_input_schema,
        ),
        PERMISSION_RISK_WRITE,
    ),
    permission_risk(
        model_spec(
            require_all_scopes(
                def(
                    "goal_resume",
                    GOAL_AUDIT,
                    ToolVisibility::ModelVisible,
                    TOOL_CATEGORY_GOAL,
                    None,
                    TOOL_PROVIDER_CONTROL,
                    ToolSemanticContract {
                        effect: ToolEffect::Execute,
                        risk: ToolRisk::JobRun,
                        approval: ToolApprovalPolicy::Standard,
                        idempotency: ToolIdempotency::DesiredState,
                    },
                    Some(CODING_AGENT_RUN),
                    false,
                    ToolPathHint::None,
                    true,
                    false,
                    ToolSessionEvidencePolicy::NONE,
                ),
                EXECUTION_SCOPES,
            ),
            "Explicitly resume a stopped owned goal using its existing immutable Project, providers, objective, criteria, and remaining budgets. Re-authorizes execution under the current caller; it cannot retarget work, reset consumed limits, revive success/cancellation, or redispatch an execution with unknown outcome. Repeated active resume does not create another worker.",
            goal_resume_input_schema,
        ),
        PERMISSION_RISK_JOB,
    ),
];
