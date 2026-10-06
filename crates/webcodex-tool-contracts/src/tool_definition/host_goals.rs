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
        "Start a host-driven goal in the selected project. task_directory is the user's chosen task folder (relative or absolute inside this project), defaulting to the project root. New plan files go under <task>/goals and handoff snapshots under <task>/handoff on the owning Runner; never guess a Server-local path. First inspect inputs with read/search tools or present_document, then goal_plan_save produces plan.json/plan.md. Saving does not confirm execution: await a subsequent explicit user instruction for that exact version before goal_confirm_plan. Replay the same key/payload after uncertainty; the task folder cannot be retargeted. New epochs default to 600000/800000 warning/handoff tokens; existing epochs keep their thresholds. Counts cover partially observed tool text, not actual model capacity. Draining/retired epochs cannot reset authority by bootstrapping or changing settings.",
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
        "Handle '继续本地当前任务' in a fresh conversation. Recover the exact owned task from the local current-goal pointer or explicit goal_id, recheck project access, and return bounded phase/plan/node/progress/issues, unresolved operations and next action. Follow required_read before dependent work. Planning resumes inspection; awaiting_confirmation permits read-only investigation including present_document, but execution still needs the user's explicit start instruction. Reading never confirms a plan, claims a node or proves recovery. activate defaults true for explicit continuation and tightens goal-mode admission; it cannot reactivate a draining/retired epoch. Use activate=false to read only. Preserve context_epoch within one conversation; a new conversation gets a separate estimate. Completed goals never choose replacement work.",
        goal_bootstrap_input_schema,
    ),
    state_tool(
        "goal_claim_node",
        "Claim the next serial node of a saved, user-confirmed plan, or take over/rework a prior attempt. First goal_read(view=node,node_id) in this conversation; summary/bootstrap does not acknowledge context. Requires current revision/version and valid local plan hashes. Reconcile original operations before takeover_attempt_id. Returns the binding for goal-bound calls; stale attempts cannot dispatch. If recovery.business_suspended, claim only its affected node, diagnose and repair autonomously with goal_context.repair, then goal_check_node. Claiming or handoff never resets repair budgets; normal business and later nodes stay closed until original acceptance passes. No new approval is needed to repair within the confirmed plan. Original file/command/Job permissions remain mandatory.",
        goal_claim_node_input_schema, false,
    ),
    state_tool(
        "goal_checkpoint",
        "Persist bounded progress with current revision/idempotency and goal_context during execution. State patches preserve omitted fields; facts merge (null deletes), supplied arrays replace. action=checkpoint/handoff/pause/exit/block/cancel never confirms a plan or passes acceptance. handoff commits a checkpoint and retires its source epoch, then writes immutable JSON/Markdown under the selected task folder's handoff directory. If file export fails, checkpoint_saved=true still means retirement committed; retry the identical request/key to finish export. Never substitute newer state. Active requests/unknown effects block handoff; known Jobs/Runs may continue and must be observed before a new claim. Replay exact inputs after lost responses. Keep context_epoch fixed within a chat; retirement cannot be reversed. pause/exit/cancel do not prove execution stopped.",
        goal_checkpoint_input_schema, false,
    ),
    state_tool(
        "goal_check_node",
        "Run original acceptance over verification_paths and managed_artifacts. Failure suspends business: follow recovery to diagnose/repair autonomously, then recheck with goal_context.repair. Only passed acceptance releases it. Unknown effects: reconcile original Job/Run; claims/restarts preserve budgets. review=0 needs no model verdict. review=1: omit evidence, inspect material and every page via present_document(review=1), or raster images via read_project_artifact(as_image=true). Submit fresh model_self_check: original review_packet_id, exact artifacts, reviewed_scopes, sources, evidence and findings; use saved Review IDs or __delivery_review. After edits use new keys/material; replay identical keys only for original receipts/Jobs. Stale source/preview/renderer or missing/exhausted review cannot pass. History: section=reviews/repair. Model review is not machine or user approval.",
        goal_check_node_input_schema, false,
    ),
];
