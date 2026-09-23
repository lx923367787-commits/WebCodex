use serde_json::{json, Value};

fn bounded_string(max: usize) -> Value {
    json!({"type":"string","minLength":1,"maxLength":max})
}

fn path_schema() -> Value {
    json!({
        "type":"string","minLength":1,"maxLength":1024,
        "description":"Project-relative path; absolute paths and parent traversal are rejected."
    })
}

pub(crate) fn criterion_schema() -> Value {
    let common = json!({
        "id": bounded_string(64),
        "description": bounded_string(2048)
    });
    let mut variants = Vec::new();
    for (kind, fields, required) in [
        ("file_exists", json!({"path":path_schema()}), vec!["path"]),
        (
            "file_contains",
            json!({"path":path_schema(),"text":bounded_string(16 * 1024)}),
            vec!["path", "text"],
        ),
        (
            "command",
            json!({
                "program":bounded_string(1024),
                "args":{"type":"array","maxItems":64,"items":{"type":"string","maxLength":4096},"default":[]},
                "cwd":path_schema(),
                "timeout_secs":{"type":"integer","minimum":1,"maximum":3600}
            }),
            vec!["program", "timeout_secs"],
        ),
        ("review", json!({}), vec![]),
    ] {
        let mut properties = common.as_object().unwrap().clone();
        properties.insert("kind".to_string(), json!({"type":"string","enum":[kind]}));
        properties.extend(fields.as_object().unwrap().clone());
        let mut all_required = vec!["kind", "id", "description"];
        all_required.extend(required);
        variants.push(json!({
            "type":"object","additionalProperties":false,
            "properties":properties,"required":all_required
        }));
    }
    json!({
        "oneOf":variants,
        "description":"Immutable acceptance criterion. File/command checks use independent runtime evidence; review criteria require a separate reviewer verdict. Command arguments are structured argv, not shell source."
    })
}

pub(crate) fn limits_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,
        "properties":{
            "max_rounds":{"type":"integer","minimum":1,"maximum":50,"default":5},
            "total_timeout_secs":{"type":"integer","minimum":1,"maximum":86400,"default":1800},
            "max_no_progress_rounds":{"type":"integer","minimum":1,"maximum":50,"default":3},
            "run_timeout_secs":{"type":"integer","minimum":1,"maximum":3600,"default":300}
        },
        "description":"Finite total limits. Consumed rounds and elapsed budget survive pause/resume; a single ACP Run has its own bounded timeout."
    })
}

pub(crate) fn spec_properties() -> Value {
    json!({
        "project":{
            "type":"string","minLength":1,"maxLength":512,
            "description":"Exact registered Project whose Runner owns execution and verification. Goal creation fixes this execution target."
        },
        "provider_id":{
            "type":"string","minLength":1,"maxLength":64,
            "description":"Logical operator-configured ACP provider advertised by that Runner; executable, credentials and environment are not caller supplied."
        },
        "objective":{
            "type":"string","minLength":1,"maxLength":65536,
            "description":"Concrete user-authorized goal and scope. Execution output alone cannot establish completion; criteria are evaluated separately."
        },
        "criteria":{"type":"array","minItems":1,"maxItems":32,"items":criterion_schema()},
        "artifact_paths":{
            "type":"array","minItems":1,"maxItems":64,"uniqueItems":true,"items":path_schema(),
            "description":"Project-relative files to inspect and retain as bounded evidence each round. Declare the actual deliverables, not a directory or speculative output."
        },
        "reviewer_provider_id":{
            "type":"string","minLength":1,"maxLength":64,
            "description":"Optional logical ACP provider for independent review. Review uses a fresh Run and does not inherit execution success as a verdict."
        },
        "limits":limits_schema(),
        "config":super::coding_agents::coding_agent_start_input_schema()["properties"]["config"].clone()
    })
}

pub fn goal_start_input_schema() -> Value {
    let mut properties = spec_properties();
    properties["idempotency_key"] = json!({
        "type":"string","minLength":1,"maxLength":128,
        "description":"Caller-chosen exact initiation replay key. Reuse it unchanged with the same specification after an uncertain start."
    });
    json!({
        "type":"object","additionalProperties":false,"properties":properties,
        "description":"The combined serialized goal specification, excluding idempotency_key, must fit within 24 KiB. Individual field bounds do not override this aggregate limit.",
        "required":["project","provider_id","objective","criteria","artifact_paths","idempotency_key"]
    })
}

fn exact_goal_schema(description: &str) -> Value {
    json!({
        "type":"object","additionalProperties":false,
        "properties":{"goal_id":{
            "type":"string","minLength":1,"maxLength":96,
            "pattern":"^wc_goal_[A-Za-z0-9_.-]+$","description":description
        }},
        "required":["goal_id"]
    })
}

pub fn goal_read_input_schema() -> Value {
    let mut schema = exact_goal_schema(
        "Exact opaque owned Goal id. A guessed id grants no execution or observation authority.",
    );
    schema["properties"]["goal_id"]["pattern"] = json!("^(wc_goal_|hgoal_)[A-Za-z0-9_.-]+$");
    schema["properties"]["view"] = json!({"type":"string","enum":["summary","node","detail"],"default":"summary","description":"Host goals: summary is a bounded progress index; node returns complete context required to claim one node; detail pages a selected persisted section as JSON text. ACP observation retains its original rounds contract."});
    schema["properties"]["node_id"] = json!({"type":"string","minLength":1,"maxLength":64,"description":"Host view=node: exact planned node. Omit for the current or first incomplete node. Read this context before claiming that node."});
    schema["properties"]["section"] = json!({"type":"string","enum":["goal","plan","state","checkpoints","attempts","operations"],"description":"Required for Host view=detail. goal includes the full original objective, confirmation and state even before a plan exists; state reads just task state. Concatenate returned text fragments and follow next_cursor; a partial section is not a complete snapshot."});
    schema["properties"]["cursor"] = json!({"type":"string","minLength":1,"maxLength":2048,"description":"Opaque continuation cursor from the same Host detail section and unchanged goal revision. Never invent, decode or reuse it for another goal/section."});
    schema["allOf"] = json!([{"if":{"properties":{"view":{"const":"detail"}},"required":["view"]},"then":{"required":["section"]}}]);
    schema["properties"]["round_offset"] = json!({
        "type":"integer","minimum":0,
        "description":"Zero-based round offset. Omit to start at the newest round. Supply 0 to read the earliest round and follow next_round_offset for later pages."
    });
    schema["properties"]["round_limit"] = json!({
        "type":"integer","minimum":1,"maximum":5,"default":1,
        "description":"Maximum persisted rounds returned on this page. The full immutable specification and current goal state remain visible."
    });
    schema["properties"]["history_after"] = json!({
        "type":"integer","minimum":0,
        "description":"Host goals only: history after this persisted checkpoint/operation sequence or attempt fence. With view=detail, omit for the latest checkpoint/attempt or unresolved operations; pass 0 to read history from the beginning, including terminal records."
    });
    schema["properties"]["history_kind"] = json!({
        "type":"string","enum":["checkpoints","attempts","operations"],
        "description":"Legacy Host history selector; prefer view=detail with section. Do not combine with view=node. Follow history_after using checkpoint/operation sequence or attempt fence; terminal operations and past attempts remain readable across conversations."
    });
    schema["properties"]["history_limit"] = json!({
        "type":"integer","minimum":1,"maximum":100,"default":10,
        "description":"Host goals only: maximum history records on this page. Follow next_sequence while has_more is true."
    });
    schema
}

pub fn goal_cancel_input_schema() -> Value {
    exact_goal_schema("Exact Goal whose continuation and current execution should be cancelled. Cancellation never creates replacement work.")
}

pub fn goal_resume_input_schema() -> Value {
    exact_goal_schema("Exact stopped Goal to resume under current authorization and its original immutable specification and remaining limits.")
}

pub fn goal_list_input_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,
        "properties":{
            "project":{"type":"string","minLength":1,"maxLength":512,"description":"Optional exact registered Project filter."},
            "limit":{"type":"integer","minimum":1,"maximum":100,"default":20}
        },
        "required":[]
    })
}
