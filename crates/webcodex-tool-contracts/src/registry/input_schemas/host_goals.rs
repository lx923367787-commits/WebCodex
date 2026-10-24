use serde_json::{json, Value};

pub const TOOL_CALL_GOAL_CONTEXT_FIELD: &str = "goal_context";
pub const TOOL_CALL_CONTEXT_EPOCH_FIELD: &str = "context_epoch";

pub fn context_epoch_input_schema() -> Value {
    json!({"type":"string","minLength":1,"maxLength":128,
        "description":"Optional caller-carried conversation key when a reliable host identity is unavailable. Reuse it for every call in the same chat, including planning reads. After enabling goal mode, the key also binds that declared conversation to its goal and restricts effects missing goal_context. This is a weak binding, not authenticated host identity: calls omitting/changing the key cannot be attributed to that chat. It grants no authority. Change it only for a new chat; when goal_context.context_epoch is also present they must match."})
}

/// Invocation metadata shared by HTTP/MCP adapters. It is intentionally absent
/// from ToolCall variants: the execution boundary consumes it before parsing.
pub fn goal_context_input_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,
        "description":"Optional goal-mode execution binding returned by goal_claim_node. Attach it to each business tool invocation for that exact confirmed plan and current attempt. Add a unique operation_id before first dispatch; reuse the identical operation_id and tool parameters after uncertain delivery. Stale or mismatched bindings are rejected. This metadata only tightens admission, never grants project, command or plugin authority, and is consumed before concrete tool parsing. Keep context_epoch unchanged within one actual conversation. With a generic gateway, supply this once at the outer wrapper or inside target arguments, never both.",
        "properties":{
            "goal_id":{"type":"string","minLength":1,"maxLength":128},
            "node_id":{"type":"string","minLength":1,"maxLength":64},
            "attempt_id":{"type":"string","minLength":1,"maxLength":128},
            "plan_version":{"type":"integer","minimum":1},
            "fence":{"type":"integer","minimum":1},
            "operation_id":{"type":"string","minLength":1,"maxLength":128},
            "context_epoch":{"type":"string","minLength":1,"maxLength":128},
            "repair":{"type":"object","additionalProperties":false,"properties":{
                "repair_id":{"type":"string","minLength":1,"maxLength":128},
                "diagnosis":{"type":"string","minLength":1,"maxLength":4096}
            },"required":["repair_id"],"description":"When recovery.business_suspended is true, normal business is refused. Copy recovery.repair_id, and provide a concrete observed cause and targeted repair as diagnosis on the first call of each repair round. Attach this to repair operations and goal_check_node. Only original acceptance can release the gate; IDs, claims and handoff never reset its budgets. Continue authorized repair autonomously, escalate only when exhausted or authority/execution uncertainty requires it."}
        },
        "required":["goal_id","node_id","attempt_id","plan_version","fence","operation_id"]
    })
}

fn text(max: usize) -> Value {
    json!({"type":"string","minLength":1,"maxLength":max})
}

fn list(items: Value, required: bool, max: usize) -> Value {
    json!({"type":"array","minItems":usize::from(required),"maxItems":max,"items":items})
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}

pub(crate) fn host_criterion_schema() -> Value {
    let mut schema = super::goals::criterion_schema();
    schema["description"] = json!("Immutable acceptance method. File and command checks are executed by the runtime; review requires source-backed model_self_check and is never described as independent or automated verification. Structured command argv is not shell source.");
    schema
}

pub(crate) fn host_plan_schema() -> Value {
    let node = object(
        json!({
            "id":text(64), "title":text(512),
            "inputs":list(text(4096),false,64), "work":text(16*1024),
            "outputs":list(text(4096),true,64), "criterion_ids":list(text(64),true,64),
            "verification_paths":{"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":1024},"default":[],"description":"Project-relative verification inputs and artifacts for this node's command checks. A nonempty list overrides the plan-level default; an empty or omitted list inherits plan.verification_paths. A command check requires a nonempty effective list. Declare files needed at this stage, not outputs that future nodes will create. Paths must be unique and remain within the plan's combined 64-path budget."},
            "acceptance":list(host_criterion_schema(),true,32), "failure_action":text(4096)
            ,"review":{"type":"integer","enum":[0,1],"description":"Review requirement, not approval. 0 keeps machine checks; 1 requires actual materials returned to the host LLM and a new source-backed verdict. Layout always corrects 0 to 1. Missing complexity is conservatively 1. New saved versions expose the effective value and reason; historical plans remain immutable."},
            "review_reason":text(1024),
            "review_max_rounds":{"type":"integer","minimum":1,"maximum":5,"default":3,"description":"Maximum submitted model verdicts for this logical node across attempts and conversations. Preparation and page reads do not consume rounds. Fixed after the first material preparation."},
            "delivery_kind":{"type":"string","enum":["lightweight","code","data","layout"],"description":"Declared logical delivery semantics. Lightweight is only for directly verifiable low-risk work. Code/data and uncertain work require quality judgment. Any actual layout delivery requires image review."},
            "managed_artifacts":list(object(json!({"path":text(1024),"kind":{"type":"string","enum":["lightweight","code","data","layout"]}}), &["path","kind"]),false,64)
        }),
        &[
            "id",
            "title",
            "work",
            "outputs",
            "criterion_ids",
            "acceptance",
            "failure_action",
        ],
    );
    let management = object(
        json!({
            "facts":text(4096), "decisions":text(4096), "issues":text(4096),
            "artifacts":text(4096), "checkpoint_triggers":list(text(4096),true,64),
            "resume_instructions":text(4096)
        }),
        &[
            "facts",
            "decisions",
            "issues",
            "artifacts",
            "checkpoint_triggers",
            "resume_instructions",
        ],
    );
    let mut schema = object(
        json!({
        "objective":text(16*1024), "constraints":list(text(4096),false,64),
        "deliverables":list(text(4096),true,64),
        "verification_paths":{"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":1024},"default":[],"description":"Project-relative files for final command acceptance and the default for nodes without their own verification_paths. Final command criteria require a nonempty plan-level list; a node command can instead declare its own nonempty list. Include files affecting the relevant check, not future-node outputs. The combined acceptance path set must fit within 64 paths. Absolute/parent-traversal paths are rejected; changed lists require a newly saved and confirmed plan."},
            "final_criteria":list(host_criterion_schema(),true,32),
            "nodes":list(node,true,64), "state_management":management
        }),
        &[
            "objective",
            "deliverables",
            "final_criteria",
            "nodes",
            "state_management",
        ],
    );
    schema["description"] = json!("Complete immutable serial plan, at most 128 KiB serialized. Shared requirements (all fields except nodes) and each node must each fit within 8 KiB, and shared requirements plus each node within 12 KiB, so a complete execution context fits one bounded read; use external document references or split large nodes. Include deliverables, final pass conditions/evidence, step inputs/work/outputs/failure handling, and state checkpoint/recovery rules. Every final criterion must be mapped by a node's criterion_ids. Saving does not confirm or start execution.");
    schema
}

pub(crate) fn host_state_schema() -> Value {
    let mut schema = object(
        json!({
            "completed_summary":{"type":"string","maxLength":16*1024},
        "facts":{"type":"object","maxProperties":128,"propertyNames":text(128),"additionalProperties":{"anyOf":[{"type":"string","maxLength":4096},{"type":"null"}]}},
            "decisions":list(text(4096),false,64), "issues":list(text(4096),false,64),
            "artifact_refs":list(text(4096),false,64),
            "next_step":{"type":"string","maxLength":4096},
            "plan_draft":{"description":"Optional incomplete planning draft; never opens the execution gate."}
        }),
        &[],
    );
    schema["description"] = json!("Partial update to task facts and progress. Omitted fields are preserved; facts merges string values by key and a null value deletes only that key. Other supplied fields replace that field, including entire arrays. Explicit plan_draft:null clears the draft. Persisted facts remain string values and prior checkpoints remain available. The resulting full state must fit within 64 KiB. These fields cannot change phase, confirm a plan, pass acceptance, or override execution identity. Never send a partial summary as a full state replacement.");
    schema
}

pub(crate) fn host_evidence_schema() -> Value {
    object(
        json!({
            "criterion_id":text(64), "passed":{"type":"boolean"},
            "source":{"type":"string","enum":["model_self_check"]},
            "evidence":text(8192), "source_refs":list(text(4096),true,64),
            "artifacts":list(object(json!({
                "path":text(1024), "sha256":{"anyOf":[{"type":"string","pattern":"^[A-Fa-f0-9]{64}$"},{"type":"null"}]}
            }), &["path","sha256"]),false,64),
            "review_packet_id":text(128),
            "reviewed_scopes":list(text(1200),false,512),
            "findings":list(object(json!({"location":text(1024),"impact":text(2048),"required_change":text(2048)}), &["location","impact","required_change"]),false,64)
        }),
        &[
            "criterion_id",
            "passed",
            "source",
            "evidence",
            "source_refs",
        ],
    )
}

fn context_epoch() -> Value {
    context_epoch_input_schema()
}

fn goal_id() -> Value {
    json!({"type":"string","minLength":1,"maxLength":96,"pattern":"^hgoal_[A-Za-z0-9_.-]+$"})
}

fn revision_properties() -> Value {
    json!({
        "goal_id":goal_id(),
        "expected_revision":{"type":"integer","minimum":0,"description":"Last observed goal revision; stale updates are rejected."},
        "idempotency_key":{"type":"string","minLength":1,"maxLength":128,"description":"Replay the exact same key and payload after a lost response. Reusing this key with changed contents conflicts."}
    })
}

pub fn goal_mode_start_input_schema() -> Value {
    object(
        json!({
            "project":text(512), "objective":text(16*1024), "idempotency_key":text(128),
            "task_directory":{"type":"string","minLength":1,"maxLength":4096,"default":".","description":"The task folder selected by the user in this chat: a project-relative directory or an absolute directory inside this exact registered project. Defaults to the project root. New goals save plans under <task>/goals and checkpoint/handoff files under <task>/handoff on that project's Runner. For a folder outside the project, first select/register that folder as the project; never infer it from a transport or another task. Fixed for this goal; a retry cannot retarget it. Use this folder as the working directory for the task's execution tools."},
            "context_epoch":context_epoch(),
        "context_warning_tokens":{"type":"integer","minimum":1000,"maximum":10000000,"default":600000,"description":"Default for a new epoch only; existing epochs retain their stored threshold. Counts partially observed tool text, not the model's full context window."},
        "context_handoff_tokens":{"type":"integer","minimum":1001,"maximum":20000000,"default":800000,"description":"Must exceed warning threshold. New epochs default to 800000; existing epochs retain their threshold. Crossing it persistently drains this epoch: save progress or finish the current attempt, but do not dispatch new business. It is not the ChatGPT context capacity."}
        }),
        &["project", "objective", "idempotency_key"],
    )
}

pub fn goal_handoff_input_schema() -> Value {
    object(
        json!({
            "goal_id":goal_id(),
            "action":{"type":"string","enum":["status","begin_attempt","fail_attempt","abort","prepare","commit"],"description":"status observes actual ownership. The local trusted scheduler alone reserves/fails attempts. A fresh host-bound candidate uses prepare, reads its verified task package, then commits the exact returned package hash."},
            "handoff_id":text(128), "attempt_id":text(128),
            "checkpoint_sequence":{"type":"integer","minimum":1},
            "package_sha256":{"type":"string","pattern":"^[a-f0-9]{64}$"},
            "reason":text(4096)
        }),
        &["goal_id", "action"],
    )
}

pub fn goal_plan_save_input_schema() -> Value {
    let mut properties = revision_properties();
    properties["plan"] = host_plan_schema();
    properties["change_request"] = json!({"type":"string","minLength":1,"maxLength":4096,"description":"Only when the user explicitly changes the goal or final acceptance contract: faithfully relay their request from the conversation. Ordinary replanning must preserve the contract. This is model-relayed user intent, not independently authenticated approval. A changed contract creates a new version, invalidates affected evidence and always requires fresh confirmation after local plan saving."});
    object(
        properties,
        &["goal_id", "expected_revision", "idempotency_key", "plan"],
    )
}

pub fn goal_confirm_plan_input_schema() -> Value {
    let mut properties = revision_properties();
    properties["plan_version"] = json!({"type":"integer","minimum":1});
    properties["plan_sha256"] = json!({"type":"string","pattern":"^[A-Fa-f0-9]{64}$","description":"Exact saved plan hash shown to the user; confirmation applies only to this immutable version."});
    properties["confirmation"] = json!({"type":"string","minLength":1,"maxLength":4096,"description":"Quote or faithfully relay the user's explicit instruction in this conversation to start the saved plan, after they reviewed it. Do not invent approval, infer it from plan saving, or claim independently authenticated human identity."});
    object(
        properties,
        &[
            "goal_id",
            "expected_revision",
            "idempotency_key",
            "plan_version",
            "plan_sha256",
            "confirmation",
        ],
    )
}

pub fn goal_bootstrap_input_schema() -> Value {
    object(
        json!({
            "goal_id":goal_id(), "project":text(512), "context_epoch":context_epoch(),
            "activate":{"type":"boolean","default":true,"description":"Enable the goal-mode admission binding for the current conversation after an explicit continue request. Does not confirm a plan or claim a node; false reads only."}
        }),
        &[],
    )
}

pub fn goal_claim_node_input_schema() -> Value {
    let mut properties = revision_properties();
    properties["node_id"] = text(64);
    properties["plan_version"] = json!({"type":"integer","minimum":1});
    properties["context_epoch"] = context_epoch();
    properties["rework"] = json!({"type":"boolean","default":false,"description":"Explicitly rework a prior node; affected later acceptance must be invalidated."});
    properties["takeover_attempt_id"] = json!({"type":"string","minLength":1,"maxLength":128,"description":"Exact current attempt to take over after checking unresolved operations. Omit for a fresh node; never guess or silently replace an active attempt."});
    object(
        properties,
        &[
            "goal_id",
            "expected_revision",
            "idempotency_key",
            "node_id",
            "plan_version",
        ],
    )
}

pub fn goal_checkpoint_input_schema() -> Value {
    let mut properties = revision_properties();
    properties["context_epoch"] = context_epoch();
    properties["patch"] = object(
        json!({
        "state":host_state_schema(), "reason":text(4096),
        "operation_resolutions":list(object(json!({
            "operation_id":text(128),
            "status":{"type":"string","enum":["succeeded","failed","cancelled"]},
            "evidence":text(4096),"source_refs":list(text(1024),true,16),
            "user_confirmation":text(4096)
        }), &["operation_id","status","evidence","source_refs","user_confirmation"]),false,8),
            "action":{"type":"string","enum":["checkpoint","handoff","pause","exit","block","cancel"],"default":"checkpoint"}
        }),
        &["reason"],
    );
    properties["patch"]["properties"]["operation_resolutions"]["description"] = json!("At most 8 explicit reconciliations. Each report's JSON must fit 8192 UTF-8 bytes, with at most 16 source references of at most 1024 bytes. Only after the user checked an unknown effect with no tracked Job or native Run: faithfully relay their confirmation and evidence. Internal check_* operations without a Job may only be cancelled, never manually passed or failed. Active requests, Jobs and native Runs cannot be overridden; observe or cancel the exact execution first. Arbitrary plugin run_id fields are not trusted native Run identities. Never invent confirmation.");
    object(
        properties,
        &["goal_id", "expected_revision", "idempotency_key", "patch"],
    )
}

pub fn goal_check_node_input_schema() -> Value {
    let mut properties = revision_properties();
    properties["node_id"] = text(64);
    properties["attempt_id"] = text(128);
    properties["plan_version"] = json!({"type":"integer","minimum":1});
    properties["evidence"] = list(host_evidence_schema(), false, 64);
    properties["evidence"]["description"] = json!("Omit or send [] to run machine checks and prepare actual review material. Then read all materials and native images before submitting a NEW model_self_check with original review_packet_id, reviewed_scopes, source_refs, and a concrete explanation. Failed judgments require findings with location, impact and required_change; passing judgments explain complete scope and basis. Use existing Review criterion IDs, or __delivery_review if review=1 has no Review criterion. File/command evidence is collected by the runtime. A current hash attached at submission cannot rebind stale review material. At the round limit unresolved findings remain blocked.");
    object(
        properties,
        &[
            "goal_id",
            "expected_revision",
            "idempotency_key",
            "node_id",
            "attempt_id",
            "plan_version",
        ],
    )
}
