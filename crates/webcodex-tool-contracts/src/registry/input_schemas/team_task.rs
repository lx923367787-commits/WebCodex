use serde_json::{json, Value};

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn text(max: usize) -> Value {
    json!({"type":"string","minLength":1,"maxLength":max})
}

fn task_id() -> Value {
    json!({"type":"string","pattern":"^wc_agent_task_[0-9a-f]{32}$"})
}

fn provider() -> Value {
    json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[a-z0-9][a-z0-9._-]*$","description":"CLI adapter ID returned by local discovery; selection does not authorize execution."})
}

fn optional_text(max: usize) -> Value {
    json!({"anyOf":[text(max),{"type":"null"}]})
}

pub(crate) fn team_policy_schema() -> Value {
    let range = object(
        json!({
            "min":{"type":"integer","minimum":0,"maximum":32},
            "max":{"type":"integer","minimum":0,"maximum":32}
        }),
        &["min", "max"],
    );
    object(
        json!({
            "mode":{"type":"string","enum":["auto","custom"]},
            "subagents":range,"cli_per_agent":range,
            "providers":{"type":"array","maxItems":32,"items":object(json!({
                "provider_id":provider(),"enabled":{"type":"boolean"},
                "model":optional_text(256),"effort":optional_text(64)
            }), &["provider_id","enabled"])}
        }),
        &["mode", "subagents", "cli_per_agent", "providers"],
    )
}

pub(crate) fn team_plan_schema() -> Value {
    let member = object(
        json!({
            "task_id":task_id(),
            "cli_calls":{"type":"array","maxItems":32,"items":object(json!({
                "provider_id":provider(),"model":text(256),"effort":{"anyOf":[text(64),{"type":"null"}],"description":"Explicit discovered effort, null for an effort-pinned Antigravity model slug or when unsupported, or project_policy to resolve the saved effort at expected_policy_revision. The selector requires an enabled provider and exact saved model; the frozen value is the actual effort or null."},
                "inputs":{"type":"array","maxItems":8,"description":"Explicit immutable UTF-8 task material, at most 32 KiB serialized JSON total. Names are labels, not host paths; no filesystem access is granted.","items":object(json!({"name":text(128),"content":{"type":"string"}}), &["name","content"])},
                "instruction":{"anyOf":[text(8192),{"type":"null"}],"description":"Manager-assigned CLI subtask text, not a command or authority grant. Must be nonblank, at most 8192 UTF-8 bytes and contain no NUL; newlines are allowed. Required and non-null for every call when this member has multiple calls. A sole call may omit it to inherit its AgentTask instruction."}
            }), &["provider_id","model","effort"])}
        }),
        &["task_id", "cli_calls"],
    );
    object(
        json!({"manager":member,"subagents":{"type":"array","maxItems":32,"items":member}}),
        &["manager", "subagents"],
    )
}

pub fn team_policy_get_input_schema() -> Value {
    object(json!({"project":text(512)}), &["project"])
}

pub fn team_policy_save_input_schema() -> Value {
    object(
        json!({"project":text(512),"expected_revision":{"type":"integer","minimum":0},"policy":team_policy_schema()}),
        &["project", "expected_revision", "policy"],
    )
}

pub fn team_plan_register_input_schema() -> Value {
    object(
        json!({
            "project":text(512),"root_task_id":task_id(),
            "expected_policy_revision":{"type":"integer","minimum":0},
            "plan":team_plan_schema(),"idempotency_key":text(128)
        }),
        &[
            "project",
            "root_task_id",
            "expected_policy_revision",
            "plan",
            "idempotency_key",
        ],
    )
}

pub fn team_plan_list_input_schema() -> Value {
    object(
        json!({"project":text(512),"offset":{"type":"integer","minimum":0,"default":0},"limit":{"type":"integer","minimum":1,"maximum":20,"default":20}}),
        &["project"],
    )
}

pub fn team_plan_read_input_schema() -> Value {
    object(
        json!({"project":text(512),"root_task_id":task_id(),
            "attempts_offset":{"type":"integer","minimum":0,"maximum":65536,"default":0,"description":"CLI attempt offset in stable insertion order; continue with output.attempts.next_offset."},
            "attempts_limit":{"type":"integer","minimum":1,"maximum":20,"default":20}}),
        &["project", "root_task_id"],
    )
}

pub fn team_plan_cancel_input_schema() -> Value {
    object(
        json!({"project":text(512),"root_task_id":task_id()}),
        &["project", "root_task_id"],
    )
}

pub fn team_cli_start_input_schema() -> Value {
    object(
        json!({
            "project":text(512),"root_task_id":task_id(),"source_task_id":task_id(),
            "call_index":{"type":"integer","minimum":0,"maximum":31},
            "idempotency_key":text(128),
            "fallback_thinking_profile_id":{"type":"string","minLength":1,"maxLength":128,"description":"Exact discovered ChatGPT thinking profile explicitly selected by the coordinating manager and frozen for this CLI attempt's recovery conversation. No implicit highest/default profile."},
            "timeout_secs":{"type":"integer","minimum":1,"maximum":3600,"default":900}
        }),
        &[
            "project",
            "root_task_id",
            "source_task_id",
            "call_index",
            "idempotency_key",
            "fallback_thinking_profile_id",
        ],
    )
}

pub fn team_cli_observe_input_schema() -> Value {
    object(
        json!({"project":text(512),"source_attempt_id":{"type":"string","pattern":"^wc_team_cli_attempt_[0-9a-f]{32}$"}}),
        &["project", "source_attempt_id"],
    )
}

pub fn team_agent_start_input_schema() -> Value {
    object(
        json!({
            "project":text(512), "root_task_id":task_id(), "source_task_id":task_id(),
            "idempotency_key":text(128),
            "thinking_profile_id":{"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9_.:-]+$","description":"Exact fresh Host thinking profile explicitly selected for this ordinary planned subagent. Replays retain the original choice."}
        }),
        &[
            "project",
            "root_task_id",
            "source_task_id",
            "idempotency_key",
            "thinking_profile_id",
        ],
    )
}

pub fn team_agent_observe_input_schema() -> Value {
    object(
        json!({"project":text(512),"team_agent_attempt_id":{"type":"string","pattern":"^wc_team_agent_attempt_[0-9a-f]{32}$"}}),
        &["project", "team_agent_attempt_id"],
    )
}
