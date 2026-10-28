use super::common::{schema_type, wrapped_output_schema};
use crate::registry::input_schemas::team_task::{team_plan_schema, team_policy_schema};
use serde_json::{json, Value};

fn nullable(kind: &str) -> Value {
    json!({"anyOf":[{"type":kind},{"type":"null"}]})
}

fn policy() -> Value {
    let mut value = team_policy_schema();
    value["properties"]["revision"] = json!({"type":"integer","minimum":0});
    value["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("revision"));
    value
}

fn plan() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "project_id":{"type":"string"},"root_task_id":{"type":"string"},
        "policy":policy(),"plan":team_plan_schema(),
        "status":{"enum":["active","cancelled"]},"cancel_requested":{"type":"boolean"},
        "created_at_unix_ms":{"type":"integer"},"updated_at_unix_ms":{"type":"integer"}
    },"required":["project_id","root_task_id","policy","plan","status","cancel_requested","created_at_unix_ms","updated_at_unix_ms"]})
}

fn summary() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "project_id":{"type":"string"},"root_task_id":{"type":"string"},
        "policy_revision":{"type":"integer"},"status":{"enum":["active","cancelled"]},
        "cancel_requested":{"type":"boolean"},"subagent_count":{"type":"integer","minimum":0},
        "cli_call_count":{"type":"integer","minimum":0},"created_at_unix_ms":{"type":"integer"},"updated_at_unix_ms":{"type":"integer"}
    },"required":["project_id","root_task_id","policy_revision","status","cancel_requested","subagent_count","cli_call_count","created_at_unix_ms","updated_at_unix_ms"]})
}

fn attempt() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "attempt_id":{"type":"string"},"project_id":{"type":"string"},"root_task_id":{"type":"string"},
        "source_task_id":{"type":"string"},"call_index":{"type":"integer","minimum":0},
        "provider_id":{"type":"string"},"model":nullable("string"),"effort":nullable("string"),
        "instruction":schema_type("string","Frozen independent CLI task instruction; the parent Agent goal is never inherited and this text grants no execution authority."),
        "fallback_thinking_profile_id":{"type":"string"},"timeout_secs":nullable("integer"),
        "dispatch_state":{"enum":["reserved","dispatching","bound","terminal","unknown"]},
        "job_id":nullable("string"),"execution_state":{"enum":[null,"not_started","completed","timed_out","outcome_unknown"]},
        "error_code":nullable("string"),"result_summary":nullable("string"),
        "observation_revision":{"type":"integer"},"created_at_unix_ms":{"type":"integer"},"updated_at_unix_ms":{"type":"integer"}
    },"required":["attempt_id","project_id","root_task_id","source_task_id","call_index","provider_id","model","effort","instruction","fallback_thinking_profile_id","timeout_secs","dispatch_state","job_id","execution_state","error_code","result_summary","observation_revision","created_at_unix_ms","updated_at_unix_ms"]})
}

fn page(name: &str, item: Value) -> Value {
    let mut properties = json!({"total_count":{"type":"integer","minimum":0},"next_offset":{"anyOf":[{"type":"integer","minimum":0,"maximum":65536},{"type":"null"}],"description":"Next attempts_offset within the same owner/project/root scope; null when this read snapshot has no more attempts."}});
    properties[name] = json!({"type":"array","maxItems":20,"items":item});
    json!({"type":"object","description":"Bounded CLI attempt page in stable insertion order, earliest first. New attempts append and observation updates do not change order. Count and records come from one read snapshot.","properties":properties,"required":[name,"total_count","next_offset"],"additionalProperties":false})
}

fn agent_attempt() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "team_agent_attempt_id":{"type":"string"},"project":{"type":"string"},
        "root_task_id":{"type":"string"},"source_task_id":{"type":"string"},
        "thinking_profile_id":{"type":"string","maxLength":128},"role":{"const":"subagent"},
        "state":{"enum":["queued","starting","running","unknown","cancelled","confirmed_not_started","verified_terminal"]},
        "host_request_id":nullable("string"),"cancel_requested":{"type":"boolean"},
        "response_observed":{"type":"boolean"},"revision":{"type":"integer","minimum":0},
        "counts_toward_regular_min":{"const":true},"counts_toward_regular_max":{"const":true},"business_verified":{"const":false}
    },"required":["team_agent_attempt_id","project","root_task_id","source_task_id","thinking_profile_id","role","state","host_request_id","cancel_requested","response_observed","revision","counts_toward_regular_min","counts_toward_regular_max","business_verified"]})
}

fn fallback() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "fallback_id":{"type":"string"},"root_task_id":{"type":"string"},"source_task_id":{"type":"string"},"source_attempt_id":{"type":"string"},"source_job_id":{"type":"string"},"project":{"type":"string"},
        "role":{"const":"cli_fallback"},"state":{"enum":["queued","starting","running","unknown","blocked","cancelled","confirmed_not_started","verified_terminal"]},
        "reason_code":{"type":"string"},"thinking_profile_id":{"type":"string"},"host_request_id":nullable("string"),"fallback_agent_id":{"type":"null"},
        "cancel_requested":{"type":"boolean"},"response_observed":{"type":"boolean"},"counts_toward_regular_min":{"const":false},"counts_toward_regular_max":{"const":false},
        "slot_held":{"type":"boolean"},"revision":{"type":"integer"},"business_verified":{"const":false},"policy_revision":{"type":"integer"},"blocked_reason":nullable("string")
    },"required":["fallback_id","root_task_id","source_task_id","source_attempt_id","source_job_id","state","thinking_profile_id","slot_held","revision","policy_revision","business_verified"]})
}

fn completion() -> Value {
    let count = json!({"type":"integer","minimum":0});
    let member = json!({"type":"object","additionalProperties":false,"properties":{
        "task_id":{"type":"string"},"role":{"enum":["manager","subagent"]},
        "planned_cli_calls":count,"consumed_cli_calls":count,"confirmed_cli_calls":count,"required_cli_calls":count,
        "dispatch_confirmed":{"type":"boolean"},"minimum_met":{"type":"boolean"}
    },"required":["task_id","role","planned_cli_calls","consumed_cli_calls","confirmed_cli_calls","required_cli_calls","dispatch_confirmed","minimum_met"]});
    json!({"type":"object","additionalProperties":false,"description":"Authoritative frozen-policy completion gate, independent of paging. Consumed slots retain failed/unknown reservations. Confirmed CLI calls require exact bound Jobs with completed/timed-out execution; confirmed GPT dispatch requires observed response evidence. Meeting minimums is not business acceptance.","properties":{
        "root_task_id":{"type":"string"},"project_id":{"type":"string"},"mode":{"enum":["auto","custom"]},
        "task_state":{"enum":["ready","active","succeeded","failed"]},
        "planned_subagents":count,"confirmed_subagents":count,"required_subagents":count,
        "planned_cli_calls":count,"consumed_cli_calls":count,"confirmed_cli_calls":count,
        "minimums_satisfied":{"type":"boolean"},"success_gate":{"enum":["not_required","pending","satisfied","cancelled"]},
        "members":{"type":"array","maxItems":33,"items":member}
    },"required":["root_task_id","project_id","mode","task_state","planned_subagents","confirmed_subagents","required_subagents","planned_cli_calls","consumed_cli_calls","confirmed_cli_calls","minimums_satisfied","success_gate","members"]})
}

pub(super) fn output_schema_for_tool(name: &str) -> Option<Value> {
    let mut fields = vec![("error_kind", schema_type("string", "Stable failure kind."))];
    match name {
        "team_policy_get" | "team_policy_save" => fields.extend([
            ("project", json!({"type":"string"})),
            ("policy", policy()),
            ("enforcement", json!({"const":"managed_tasks"})),
        ]),
        "team_plan_register" => fields.extend([
            ("plan", plan()),
            ("created", json!({"type":"boolean"})),
            ("replayed", json!({"type":"boolean"})),
            ("state_changed", json!({"type":"boolean"})),
        ]),
        "team_plan_list" => fields.extend([
            (
                "plans",
                json!({"type":"array","maxItems":20,"items":summary()}),
            ),
            ("total_count", json!({"type":"integer","minimum":0})),
            ("next_offset", nullable("integer")),
        ]),
        "team_plan_read" => fields.extend([
            ("plan", plan()),
            ("completion", completion()),
            ("attempts", page("attempts", attempt())),
            ("agents", json!({"type":"array","maxItems":32,"items":agent_attempt()})),
            ("task_summaries", json!({"type":"array","maxItems":33,"items":{"type":"object","additionalProperties":false,"properties":{"task_id":{"type":"string"},"title":{"type":"string","maxLength":200},"state":{"enum":["ready","active","succeeded","failed"]},"activity":nullable("object")},"required":["task_id","title","state"]}})),
            ("fallbacks", json!({"type":"array","maxItems":100,"items":fallback()})),
            ("fallbacks_may_be_truncated", json!({"type":"boolean"})),
            ("enforcement", json!({"const":"managed_tasks"})),
        ]),
        "team_plan_cancel" => fields.extend([
            ("plan", plan()),
            ("stop_requests", json!({"type":"integer","minimum":0})),
            (
                "unconfirmed_stops",
                json!({"type":"array","items":{"type":"string"}}),
            ),
            ("recovery_cancel_pending", json!({"type":"boolean"})),
            ("agent_cancel_pending", json!({"type":"boolean"})),
            ("stopped_confirmed", json!({"const":false})),
        ]),
        "team_cli_start" => fields.extend([
            ("attempt", attempt()),
            ("dispatched", json!({"type":"boolean"})),
            ("replayed", json!({"type":"boolean"})),
            ("job_id", nullable("string")),
            ("job_status", nullable("string")),
            ("execution_state", json!({"type":"string"})),
            ("terminal", json!({"type":"boolean"})),
            ("command_started", json!({"type":"boolean"})),
            ("command_completed", json!({"type":"boolean"})),
            ("command_ok", json!({"type":"boolean"})),
            ("exit_code", nullable("integer")),
            ("failure_kind", nullable("string")),
            ("effective_timeout_secs", json!({"type":"integer"})),
            ("error", nullable("string")),
            ("recovery_enqueue", json!({"description":"On an exact terminal replay, the independent idempotent recovery-admission result. A failed result permits retrying the same team_cli_start intent to repair admission; it never authorizes rerunning the CLI or changing the key. Null when no repair was attempted.","anyOf":[super::common::default_output_schema(),{"type":"null"}]})),
            (
                "process_result",
                super::output_schema_for_tool("run_process"),
            ),
        ]),
        "team_cli_observe" => fields.extend([
            ("attempt", attempt()),
            ("fallbacks", json!({"type":"array","maxItems":100,"items":fallback()})),
            (
                "job",
                json!({"anyOf":[super::output_schema_for_tool("job_status"),{"type":"null"}]}),
            ),
        ]),
        "team_agent_start" => fields.extend([
            ("attempt", agent_attempt()),
            ("created", json!({"type":"boolean"})),
            ("replayed", json!({"type":"boolean"})),
            ("queued", json!({"type":"boolean"})),
            ("state_changed", json!({"type":"boolean"})),
        ]),
        "team_agent_observe" => fields.extend([
            ("attempt", agent_attempt()),
            ("state_changed", json!({"type":"boolean"})),
        ]),
        _ => return None,
    }
    Some(wrapped_output_schema(fields))
}
