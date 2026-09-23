use serde_json::{json, Value};
use webcodex_core::goal::{
    GOAL_MAX_ARTIFACTS, GOAL_MAX_CRITERIA, GOAL_MAX_EVIDENCE_BYTES, GOAL_MAX_FEEDBACK_BYTES,
    GOAL_MAX_LIST_LIMIT, GOAL_MAX_OUTPUT_BYTES,
};

use super::common::wrapped_output_schema;

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

fn nullable(schema: Value) -> Value {
    json!({"anyOf":[schema,{"type":"null"}]})
}

fn list(items: Value, max: usize) -> Value {
    json!({"type":"array","maxItems":max,"items":items})
}

fn spec_schema() -> Value {
    let mut properties = crate::registry::input_schemas::goals::spec_properties();
    let provider = properties["reviewer_provider_id"].clone();
    properties["reviewer_provider_id"] = nullable(provider);
    let cwd = properties["criteria"]["items"]["oneOf"][2]["properties"]["cwd"].clone();
    properties["criteria"]["items"]["oneOf"][2]["properties"]["cwd"] = nullable(cwd);
    properties["criteria"]["items"]["oneOf"][2]["required"] = json!([
        "kind",
        "id",
        "description",
        "program",
        "args",
        "cwd",
        "timeout_secs"
    ]);
    properties["limits"]["required"] = json!([
        "max_rounds",
        "total_timeout_secs",
        "max_no_progress_rounds",
        "run_timeout_secs"
    ]);
    object(properties)
}

fn artifact_schema() -> Value {
    object(json!({
        "path":text(1024),
        "sha256":nullable(json!({"type":"string","pattern":"^[A-Fa-f0-9]{64}$"}))
    }))
}

fn check_schema() -> Value {
    object(json!({
        "criterion_id":text(64),
        "passed":{"type":"boolean"},
        "evidence":text(GOAL_MAX_EVIDENCE_BYTES),
        "error":nullable(text(GOAL_MAX_EVIDENCE_BYTES))
    }))
}

fn round_schema() -> Value {
    let dispatch = json!({"type":"string","enum":["not_started","outcome_unknown","completed"]});
    let state = nullable(json!({"type":"string","enum":[
        "starting","running","waiting_permission","completed","failed","cancelled","lost"
    ]}));
    object(json!({
        "number":{"type":"integer","minimum":1,"maximum":50},
        "execution_run_id":nullable(text(128)),
        "review_run_id":nullable(text(128)),
        "execution_dispatch_state":dispatch,
        "review_dispatch_state":dispatch,
        "execution_state":state,
        "review_state":state,
        "execution_last_sequence":{"type":"integer","minimum":0},
        "review_last_sequence":{"type":"integer","minimum":0},
        "review_has_message_phases":{"type":"boolean"},
        "execution_output":text(GOAL_MAX_OUTPUT_BYTES),
        "review_output":text(GOAL_MAX_OUTPUT_BYTES),
        "checks":list(check_schema(),GOAL_MAX_CRITERIA),
        "manifest_before_checks":list(artifact_schema(),GOAL_MAX_ARTIFACTS + GOAL_MAX_CRITERIA),
        "manifest_final":list(artifact_schema(),GOAL_MAX_ARTIFACTS + GOAL_MAX_CRITERIA),
        "verdict":nullable(json!({"type":"string","enum":["passed","needs_work","blocked"]})),
        "feedback":text(GOAL_MAX_FEEDBACK_BYTES),
        "active_check_job_id":nullable(text(128)),
        "active_check_id":nullable(text(128)),
        "started_at":{"type":"integer"},
        "finished_at":nullable(json!({"type":"integer"}))
    }))
}

fn goal_schema() -> Value {
    object(json!({
        "goal_id":text(96),
        "revision":{"type":"integer","minimum":0},
        "spec":spec_schema(),
        "state":{"type":"string","enum":["queued","running","checking","paused","blocked","cancelled","completed"]},
        "target":nullable(object(json!({
            "project_root":text(4096),
            "runner_instance_id":text(128),
            "provider_instance_id":text(128),
            "reviewer_provider_instance_id":nullable(text(128))
        }))),
        "rounds":list(round_schema(),5),
        "created_at":{"type":"integer"},
        "deadline_at":{"type":"integer"},
        "updated_at":{"type":"integer"},
        "reason":nullable(text(GOAL_MAX_FEEDBACK_BYTES)),
        "cancel_requested":{"type":"boolean"}
    }))
}

fn goal_summary_schema() -> Value {
    object(json!({
        "goal_id":text(96),
        "revision":{"type":"integer","minimum":0},
        "spec":object(json!({
            "project":text(512),"provider_id":text(64),"objective":text(65536)
        })),
        "state":{"type":"string","enum":["queued","running","checking","paused","blocked","cancelled","completed"]},
        "round_count":{"type":"integer","minimum":0,"maximum":50},
        "last_verdict":nullable(json!({"type":"string","enum":["passed","needs_work","blocked"]})),
        "created_at":{"type":"integer"},
        "deadline_at":{"type":"integer"},
        "updated_at":{"type":"integer"},
        "reason":nullable(text(GOAL_MAX_FEEDBACK_BYTES)),
        "cancel_requested":{"type":"boolean"}
    }))
}

pub(super) fn output_schema_for_tool(name: &str) -> Option<Value> {
    match name {
        "goal_start" => {
            let mut fields = goal_page_fields();
            fields.push(("replayed", json!({"type":"boolean"})));
            Some(wrapped_output_schema(fields))
        }
        "goal_read" => {
            let mut fields = goal_page_fields();
            let host_fields = super::host_goals::page_fields();
            fields.retain(|(key, _)| !host_fields.iter().any(|(host_key, _)| host_key == key));
            fields.extend(host_fields);
            let (_, goal) = fields.iter_mut().find(|(key, _)| *key == "goal").unwrap();
            *goal = json!({"anyOf":[goal_schema(),super::host_goals::goal_schema()]});
            if let Some((_, driver)) = fields.iter_mut().find(|(key, _)| *key == "driver") {
                *driver = json!({"type":"string","enum":["host","acp"]});
            }
            Some(wrapped_output_schema(fields))
        }
        "goal_cancel" | "goal_resume" => {
            Some(wrapped_output_schema(goal_page_fields()))
        }
        "goal_list" => Some(wrapped_output_schema(vec![
            ("goals", list(json!({"anyOf":[goal_summary_schema(),super::host_goals::summary_schema()]}), GOAL_MAX_LIST_LIMIT)),
            ("error_kind", text(128)),
        ])),
        _ => None,
    }
}

fn goal_page_fields() -> Vec<(&'static str, Value)> {
    vec![
        ("goal", goal_schema()),
        (
            "total_rounds",
            json!({"type":"integer","minimum":0,"maximum":50}),
        ),
        ("round_offset", json!({"type":"integer","minimum":0})),
        (
            "next_round_offset",
            nullable(json!({"type":"integer","minimum":0})),
        ),
        ("has_more", json!({"type":"boolean"})),
        (
            "delivery_freshness",
            json!({
                "type":"string","enum":["current","changed","unavailable","not_completed"],
                "description":"Fresh hash comparison of completed deliverables when goal_read can inspect the current target. Other mutation responses do not establish freshness."
            }),
        ),
        (
            "delivery_ready",
            json!({
                "type":"boolean",
                "description":"True only when the goal is completed and its deliverables are freshly confirmed unchanged. Persisted completion alone does not assert current delivery readiness."
            }),
        ),
        ("error_kind", text(128)),
    ]
}
