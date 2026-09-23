use super::{
    def, model_spec, permission_risk, ToolApprovalPolicy, ToolAuditPolicy, ToolAuditResultField,
    ToolDefinition, ToolEffect, ToolIdempotency, ToolInputSchemaFactory, ToolSemanticContract,
    ToolSessionEvidencePolicy, ToolVisibility, PERMISSION_RISK_WRITE, TOOL_CATEGORY_GOAL,
};
use crate::metadata::{ToolPathHint, ToolRisk, PROJECT_READ, PROJECT_WRITE, TOOL_PROVIDER_CONTROL};
use crate::registry::input_schemas::*;

const AUDIT: ToolAuditPolicy = ToolAuditPolicy::typed_fields(&[
    ToolAuditResultField::pointer("goal_id", "/goal/goal_id"),
    ToolAuditResultField::pointer("phase", "/goal/phase"),
    ToolAuditResultField::pointer("revision", "/goal/revision"),
    ToolAuditResultField::pointer("plan_version", "/goal/plan_version"),
    ToolAuditResultField::value("replayed"),
    ToolAuditResultField::value("error_kind"),
]);

const fn state_tool(
    name: &'static str,
    description: &'static str,
    schema: ToolInputSchemaFactory,
    requires_project: bool,
) -> ToolDefinition {
    permission_risk(
        model_spec(
            def(
                name,
                AUDIT,
                ToolVisibility::ModelVisible,
                TOOL_CATEGORY_GOAL,
                None,
                TOOL_PROVIDER_CONTROL,
                ToolSemanticContract {
                    effect: ToolEffect::Mutate,
                    risk: ToolRisk::ProjectWrite,
                    approval: ToolApprovalPolicy::Standard,
                    idempotency: ToolIdempotency::Keyed,
                },
                Some(PROJECT_WRITE),
                requires_project,
                ToolPathHint::None,
                false,
                false,
                ToolSessionEvidencePolicy::NONE,
            ),
            description,
            schema,
        ),
        PERMISSION_RISK_WRITE,
    )
}

pub(super) const DEFINITIONS: &[ToolDefinition] = &[
    state_tool(
        "goal_mode_start",
        "Handle '开启目标模式：<目标>'. Persist/select a local planning goal; this does not start implementation. Before drafting a plan, inspect project inputs with read/search tools and present_document (text, tables, previews, pagination and read Job continuation). These reads need no saved/confirmed plan or goal_context, also while awaiting_confirmation. Discuss requirements, then present final acceptance criteria, serial steps, failure handling and state/checkpoint rules; goal_plan_save writes plan.json and plan.md. Wait for a subsequent explicit user instruction to start that exact version, then goal_confirm_plan; saving is never confirmation. Reuse the initiation key after an uncertain response. Context thresholds estimate visible tool text, not ChatGPT's true capacity.",
        goal_mode_start_input_schema, true,
    ),
    state_tool(
        "goal_plan_save",
        "Persist an immutable web-goal plan as local plan.json and plan.md. Requires deliverables, final pass conditions/methods, criterion coverage by serial nodes, failure handling and state/checkpoint/recovery rules. Success enters awaiting_confirmation, never ready/executing. Show paths, version and hash; await a subsequent explicit start instruction before implementation. Read-only investigation including present_document remains allowed. Saving or prior discussion is not confirmation. Replay identical payload/key after an uncertain result. Ordinary replanning preserves final acceptance. Use change_request only to relay an explicit user change to objective/acceptance: retain differences in a new contract version, invalidate affected evidence and require fresh confirmation. This is model-relayed intent, not independent human authentication.",
        goal_plan_save_input_schema, false,
    ),
    state_tool(
        "goal_confirm_plan",
        "After generating and saving the plan, wait for a subsequent user message explicitly agreeing to execute that exact version. The initial request to enable goal mode or complete the overall task is not this confirmation. Record the user's instruction for the exact locally saved plan version and SHA-256 after they reviewed it. Faithfully relay their words; never fabricate approval or infer it from plan persistence. This is model-relayed user confirmation, not independently authenticated human identity. The runtime checks the current revision, plan version/hash and local materialization before making the goal ready. This call does not execute a node or broaden project/command authority. A new or changed plan must be confirmed separately; next use goal_claim_node.",
        goal_confirm_plan_input_schema, false,
    ),
    model_spec(def(
        "goal_bootstrap", AUDIT, ToolVisibility::ModelVisible, TOOL_CATEGORY_GOAL, None,
        TOOL_PROVIDER_CONTROL,
        ToolSemanticContract {
            effect: ToolEffect::Observe, risk: ToolRisk::Read,
            approval: ToolApprovalPolicy::None, idempotency: ToolIdempotency::PureRead,
        },
        Some(PROJECT_READ), false, ToolPathHint::None, false, false,
        ToolSessionEvidencePolicy::NONE,
    ),
        "Handle '继续本地当前任务' (continue the local current task) in a fresh ChatGPT conversation. Recover the exact owned task from the local current-goal pointer or explicit goal_id, recheck project access, and return a bounded recovery package with phase, plan identity, current node, known progress/issues, unresolved operations and next action. Follow every required_read before dependent work. Planning resumes input inspection and planning; awaiting_confirmation allows further read-only investigation, including present_document, but implementation still requires the user's explicit instruction; reading never confirms a plan or claims a node. activate defaults true for explicit continuation and only tightens this conversation's goal-mode admission. Preserve context_epoch within one conversation; a new conversation starts a separate estimate. Completed goals are shown without choosing replacement work.",
        goal_bootstrap_input_schema,
    ),
    state_tool(
        "goal_claim_node",
        "Claim the next serial node of a locally saved and explicitly user-confirmed plan, or explicitly take over/rework a prior attempt. First call goal_read with view=node and this node_id in the current conversation; summary/bootstrap does not acknowledge execution context. Requires the current revision and plan version; local plan hashes and unresolved operations are rechecked. Use takeover_attempt_id only for the exact observed attempt after reconciling its operations. Returns the execution binding to attach to every goal-bound tool call; stale attempts cannot dispatch new effects. Claiming itself does not execute work. Normal file/command/Job permissions still apply, and node completion is established only by goal_check_node.",
        goal_claim_node_input_schema, false,
    ),
    state_tool(
        "goal_checkpoint",
        "Persist bounded progress, decisions, issues, artifacts, next action or an incomplete planning draft using revision and idempotency checks. During execution attach the current goal_context wrapper from goal_claim_node. State is a partial update: omitted fields are preserved, facts merges by key (null deletes that key), and supplied arrays replace that array. action can checkpoint, handoff, pause, exit, block or cancel; none confirms a plan or bypasses acceptance. Only after saving succeeds tell the user they can open a new conversation and say '继续本地当前任务'. Never announce saved progress after failure. Existing operations still need reconciliation; pause/exit/cancel do not prove external execution has stopped. Keep this conversation's context_epoch unchanged.",
        goal_checkpoint_input_schema, false,
    ),
    state_tool(
        "goal_check_node",
        "Verify the current node's saved acceptance criteria, then rerun final criteria on the current deliverable after all required nodes pass. Command checks use nonempty node.verification_paths or the plan default; final commands use plan.verification_paths. Declare only files needed at that stage. Later nodes may legitimately revise earlier outputs: earlier checks remain historical evidence, not frozen final artifacts. Current-check results are bound to the plan/attempt and fingerprints. Runtime collects file/command evidence under their actual scopes; supplied review evidence is only source-backed model_self_check, never an automated or independent verdict. Failure blocks completion; a progress patch cannot mark a goal complete. Replay the same key after uncertain delivery instead of blindly rerunning an unknown check.",
        goal_check_node_input_schema, false,
    ),
];
