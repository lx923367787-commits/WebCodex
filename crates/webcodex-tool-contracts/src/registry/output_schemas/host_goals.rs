use serde_json::{json, Value};

use super::common::wrapped_output_schema;
use crate::registry::input_schemas::host_goals::host_plan_schema;

fn object(properties: Value) -> Value {
    let required = properties
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}
fn text(max: usize) -> Value {
    json!({"type":"string","maxLength":max})
}
fn number() -> Value {
    json!({"type":"integer","minimum":0})
}
fn nullable(value: Value) -> Value {
    json!({"anyOf":[value,{"type":"null"}]})
}
fn list(items: Value, max: usize) -> Value {
    json!({"type":"array","maxItems":max,"items":items})
}
fn sha256() -> Value {
    json!({"type":"string","pattern":"^[A-Fa-f0-9]{64}$"})
}
fn phase() -> Value {
    json!({"type":"string","enum":["planning","awaiting_confirmation","ready","executing","verifying","paused","blocked","completed","cancelled"]})
}

pub(super) fn goal_schema() -> Value {
    let mut schema = object(json!({
        "goal_id":text(96), "project":text(512),
        "objective":text(512), "phase":phase(), "revision":number(),
        "plan_version":number(), "acceptance_contract_version":number(),
        "plan_hash":nullable(sha256()), "plan_materialized":{"type":"boolean"},
        "confirmation":nullable(object(json!({
            "plan_version":number(), "plan_hash":sha256(),
            "confirmed_at":{"type":"integer"}
        }))),
        "current_node_id":nullable(text(64)), "current_attempt_id":nullable(text(128)),
        "fence":number(), "completed_node_ids":list(text(64),64),
        "created_at":{"type":"integer"}, "updated_at":{"type":"integer"}
    }));
    schema["properties"]["completed_node_count"] = number();
    schema["properties"]["completed_node_ids_partial"] = json!({"type":"boolean"});
    schema["properties"]["task_directory"] = nullable(text(4096));
    schema
}

pub(super) fn summary_schema() -> Value {
    let mut schema = object(json!({
        "driver":{"type":"string","enum":["host"]}, "goal_id":text(96),
        "project":text(512), "objective":text(16*1024), "phase":phase(), "state":phase(),
        "spec":object(json!({"project":text(512),"objective":text(16*1024),"provider_id":{"type":"null"}})),
        "revision":number(), "plan_version":number(), "current_node_id":nullable(text(64)),
        "created_at":{"type":"integer"}, "updated_at":{"type":"integer"}
    }));
    schema["properties"]["plan_materialized"] = json!({"type":"boolean"});
    schema["properties"]["confirmation_required"] = json!({"type":"boolean"});
    schema["properties"]["objective_truncated"] = json!({"type":"boolean"});
    schema["properties"]["latest_handoff"] = nullable(object(json!({
        "source_context_epoch":text(128), "checkpoint_sequence":number(),
        "retired_at":nullable(json!({"type":"integer"})),
        "created_at":{"type":"integer"}, "status":text(32)
    })));
    schema
}

fn serialized_plan_schema() -> Value {
    // A serialized plan includes serde-defaulted command args/cwd; accept null cwd.
    let mut plan = host_plan_schema();
    for pointer in [
        "/properties/final_criteria/items",
        "/properties/nodes/items/properties/acceptance/items",
    ] {
        let criterion = plan.pointer_mut(pointer).unwrap();
        let cwd = criterion["oneOf"][2]["properties"]["cwd"].clone();
        criterion["oneOf"][2]["properties"]["cwd"] = nullable(cwd);
    }
    plan
}

fn requirements_schema() -> Value {
    let mut properties = serialized_plan_schema()["properties"]
        .as_object()
        .unwrap()
        .clone();
    properties.remove("nodes");
    object(Value::Object(properties))
}

fn node_schema() -> Value {
    serialized_plan_schema()["properties"]["nodes"]["items"].clone()
}

fn binding_schema() -> Value {
    object(json!({
        "goal_id":text(96), "node_id":text(64), "attempt_id":text(128),
        "plan_version":number(), "fence":number()
    }))
}

fn lifecycle_state_schema() -> Value {
    json!({"type":"string","enum":["unknown","active","warning","draining","retired","stopped"]})
}

fn task_files_schema() -> Value {
    object(json!({"storage":{"type":"string","const":"runner_project"},
        "task_directory":text(8192),"task_relative_directory":text(4096),
        "goals_directory":text(8192),"handoff_directory":text(8192)}))
}

fn handoff_artifacts_schema() -> Value {
    let mut schema = object(
        json!({"checkpoint_sequence":number(),"json_path":text(8192),"json_sha256":sha256(),
        "markdown_path":text(8192),"markdown_sha256":sha256(),"task_files":task_files_schema()}),
    );
    schema["properties"]["publication"] =
        json!({"type":"string","enum":["verified","not_checked"]});
    schema
}

pub(super) fn handoff_fields() -> Vec<(&'static str, Value)> {
    vec![
        (
            "source_context",
            object(json!({
                "context_epoch":text(128), "identity_source":nullable(mode_binding()),
                "lifecycle_state":lifecycle_state_schema(),
                "retired_at":nullable(json!({"type":"integer"}))
            })),
        ),
        (
            "checkpoint",
            nullable(object(json!({
                "sequence":number(), "revision":number(),
                "action":nullable(json!({"type":"string","enum":["checkpoint","handoff","pause","exit","block","cancel"]})),
                "source_context_epoch":nullable(text(128)), "source_fence":nullable(number()),
                "created_at":{"type":"integer"}
            }))),
        ),
        (
            "resume_started",
            nullable(object(json!({
                "context_epoch":text(128), "identity_source":mode_binding(),
                "started_at":{"type":"integer","description":"Unix milliseconds when a different conversation's activating bootstrap was accepted. This receipt grants no authority and does not prove a node claim or business completion."}
            }))),
        ),
        (
            "current_attempt",
            nullable(object(json!({
                "binding":binding_schema(),
                "status":{"type":"string","enum":["running","passed","failed","interrupted"]},
                "context_epoch":nullable(text(128)), "identity_source":nullable(mode_binding()),
                "started_at":{"type":"integer"}, "finished_at":nullable(json!({"type":"integer"}))
            }))),
        ),
        (
            "business_verified",
            json!({"type":"boolean","description":"True only with persisted successful business and passed-check evidence bound to a new epoch's attempt. Neither bootstrap nor claiming alone is recovery success."}),
        ),
        (
            "business_evidence",
            nullable(object(json!({
                "operation_id":text(128), "attempt_id":text(128),
                "status":{"type":"string","const":"succeeded"},
                "check_passed":{"type":"boolean","const":true}
            }))),
        ),
    ]
}

pub(super) fn handoff_output_schema() -> Value {
    let mut fields = handoff_fields();
    fields.extend([
        ("driver", json!({"type":"string","const":"host"})),
        ("projection", json!({"type":"string","const":"handoff"})),
        ("goal", goal_schema()),
        ("unresolved_operation_count", number()),
    ]);
    let required = fields.iter().map(|(name, _)| *name).collect::<Vec<_>>();
    let mut schema = wrapped_output_schema(fields)["properties"]["output"].clone();
    schema["additionalProperties"] = json!(false);
    schema["required"] = json!(required);
    schema["properties"]["task_files"] = task_files_schema();
    schema["properties"]["handoff_artifacts"] = handoff_artifacts_schema();
    schema["properties"]["handoff"] = nullable(json!({"type":"object"}));
    schema["description"] = json!("Read-only bounded evidence for the exact source epoch. Missing legacy provenance remains null or unknown; another epoch becoming current never changes the selected source's checkpoint or retirement evidence. It grants no execution or retry authority.");
    schema["allOf"] = json!([{
        "if":{"properties":{"business_verified":{"const":true}}},
        "then":{"properties":{"business_evidence":{"type":"object"}}},
        "else":{"properties":{"business_evidence":{"type":"null"}}}
    }]);
    schema
}

pub(super) fn context_meter_schema() -> Value {
    let mut schema = object(json!({
        "context_epoch":nullable(text(128)), "observed_tokens":nullable(number()),
        "warning_tokens":number(), "handoff_tokens":number(),
        "level":{"type":"string","enum":["normal","warning","handoff_recommended","unavailable"]},
        "newly_crossed":{"type":"boolean"}, "unmeasured_modalities":{"type":"boolean"},
        "sample_count":number(), "measurement":{"type":"string","enum":["observed_estimate"]},
        "algorithm":{"type":"string","enum":["o200k_base"]}, "coverage":{"type":"string","enum":["partial"]}
    }));
    schema["properties"]["notice"] = text(4096);
    schema["properties"]["retry_accounting"] =
        json!({"type":"string","enum":["per_server_invocation"]});
    schema["properties"]["lifecycle_state"] = lifecycle_state_schema();
    schema["properties"]["identity_source"] = nullable(mode_binding());
    schema
}

fn mode_binding() -> Value {
    json!({"type":"string","enum":["window_bound","explicit_context_only"],"description":"window_bound uses a reliable host conversation. explicit_context_only can bind calls carrying the same declared context_epoch but cannot identify calls which omit/change it; it never grants authority or authenticates a host chat."})
}

fn continuation_schema() -> Value {
    object(json!({
        "tool":{"type":"string","enum":["goal_read","goal_list","goal_bootstrap","goal_plan_save","goal_confirm_plan","goal_claim_node","goal_checkpoint","goal_check_node","observe_jobs","job_status","coding_agent_observe","coding_agent_cancel"]},
        "arguments":{"type":"object","description":"Arguments for the named canonical tool; validate against that tool's input schema before calling."}
    }))
}

pub(super) fn page_fields() -> Vec<(&'static str, Value)> {
    vec![
        ("driver", json!({"type":"string","enum":["host"]})),
        (
            "projection",
            json!({"type":"string","enum":["summary","bootstrap","node_context","detail"]}),
        ),
        ("goal", goal_schema()),
        (
            "progress_summary",
            object(json!({
                "partial":{"type":"boolean","const":true},
                "completed_summary":text(350),"next_step":text(350),"issues":list(text(160),3),
                "counts":object(json!({"facts":number(),"decisions":number(),"issues":number(),"artifacts":number()}))
            })),
        ),
        ("requirements", requirements_schema()),
        ("node", node_schema()),
        ("task_files", task_files_schema()),
        ("handoff_artifacts", handoff_artifacts_schema()),
        (
            "context_read",
            object(json!({
                "goal_id":text(96),"revision":number(),"plan_version":number(),"node_id":text(64),
                "complete":{"type":"boolean","const":true}
            })),
        ),
        (
            "section",
            json!({"type":"string","enum":["goal","plan","state","checkpoints","attempts","operations","reviews","repair"]}),
        ),
        ("encoding", json!({"type":"string","enum":["json_text"]})),
        ("text", text(8192)),
        ("next_cursor", nullable(text(2048))),
        ("has_more", json!({"type":"boolean"})),
        ("revision", number()),
        ("plan_version", number()),
        ("replayed", json!({"type":"boolean"})),
        ("binding", binding_schema()),
        ("context_meter", context_meter_schema()),
        (
            "recovery",
            json!({"type":"object","description":"Durable affected-node repair gate, including exact repair_id, failure, bounded rounds/operations/deadline and next_action. Business remains suspended until original acceptance passes. Claims and handoffs never reset this budget."}),
        ),
        ("mode_binding", mode_binding()),
        ("continuation", continuation_schema()),
        (
            "details",
            json!({"type":"object","additionalProperties":false,"properties":{
            "tool":{"type":"string","enum":["goal_read"]},"arguments":{"type":"object"},
            "sections":{"type":"array","items":{"type":"string","enum":["goal","plan","state","checkpoints","attempts","operations","reviews","repair"]},"maxItems":8}
        },"required":["tool","arguments"]}),
        ),
        ("unresolved_operation_count", number()),
        ("material_sha256", sha256()),
        (
            "review",
            json!({"type":"object","description":"Effective review requirement and reason, current packet identity, coverage, round count and next action. Missing historical records mean not_recorded, never approval."}),
        ),
        (
            "review_packet",
            json!({"type":["object","null"],"description":"Bounded current actual materials, original source manifest and automated check evidence. state_sha256 separately binds actual persisted text results (kind=state, path=goal-state) and is never a file fingerprint. Images require native image delivery in this conversation; page paths and ready flags alone never prove visual delivery. Null means material is not currently prepared. Read the reviews detail section for immutable submitted rounds."}),
        ),
        (
            "review_instructions",
            json!({"type":"object","description":"Current-version review instructions and bounded continuation entry points. Prepare, return actual materials, inspect all declared scopes, then submit a new model verdict; rendering and generation success are not approval."}),
        ),
        (
            "plan_artifacts",
            nullable(object(
                json!({"json_path":text(4096),"markdown_path":text(4096)}),
            )),
        ),
        ("confirmation_required", json!({"type":"boolean"})),
        ("plan_artifacts_valid", nullable(json!({"type":"boolean"}))),
        (
            "delivery_freshness",
            json!({"type":"string","enum":["not_completed","current","changed","unavailable"],"description":"On an explicit completed-goal read, compare current artifacts with saved acceptance fingerprints. Historical completion alone is not current delivery readiness."}),
        ),
        ("delivery_ready", json!({"type":"boolean"})),
        ("goal_id", text(96)),
        (
            "run_id",
            text(webcodex_core::coding_agent::CODING_AGENT_MAX_RUN_ID_BYTES),
        ),
        (
            "state",
            json!({"type":"string","enum":["starting","running","waiting_permission","completed","failed","cancelled","lost"]}),
        ),
        (
            "execution_state",
            json!({"type":"string","enum":["not_started","started","completed","outcome_unknown"]}),
        ),
        ("error_kind", text(128)),
        (
            "dispatch_certainty",
            json!({"type":"string","enum":["not_started","completed","unknown","outcome_unknown"]}),
        ),
    ]
}

pub(super) fn output_schema_for_tool(name: &str) -> Option<Value> {
    if name == "goal_handoff" {
        return Some(wrapped_output_schema(vec![
            ("driver", json!({"type":"string","enum":["host"]})),
            ("goal_id", text(96)),
            ("handoff", json!({"type":"object"})),
            ("execution_granted", json!({"type":"boolean"})),
            ("package", json!({"type":"object"})),
            ("package_sha256", sha256()),
            ("notice", text(4096)),
        ]));
    }
    if !matches!(
        name,
        "goal_mode_start"
            | "goal_plan_save"
            | "goal_confirm_plan"
            | "goal_claim_node"
            | "goal_checkpoint"
            | "goal_check_node"
            | "goal_bootstrap"
    ) {
        return None;
    }
    let mut schema = wrapped_output_schema(page_fields());
    schema["properties"]["output"]["additionalProperties"] = json!(false);
    if matches!(name, "goal_mode_start" | "goal_bootstrap") {
        schema["properties"]["output"]["properties"]["workflow_overview"] = json!({
            "type":"string", "minLength":1, "maxLength":3072,
            "description":"Shared entry/resume workflow map for model guidance only. Not persisted state or authority; current phase, confirmation, node context, execution bindings, acceptance and lifecycle restrictions remain authoritative."
        });
    }
    schema["description"]=json!("Bounded Host Goal projection. Summary/bootstrap/detail receipts fit within 8 KiB; complete node context fits within 16 KiB; a goal_check_node pending review packet with actual bounded material may use 32 KiB. progress_summary is explicitly partial; only context_read.complete from a node projection acknowledges execution prerequisites. Reassemble detail text pages before interpreting the selected JSON section. Images use the separate native channel and never count as delivered merely because a path appears here.");
    Some(schema)
}
