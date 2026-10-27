//! Isolated Agent-only schema. Never exposed through the shared legacy dispatcher.
use serde_json::{json, Value};

pub(crate) const GUIDANCE: &str = "When multiple Agents/CLIs assess the same proposition, or independent tasks produce conflicting conclusions, use team_decision to open that specific issue before aggregation. Do not open issues for ordinary separate implementation tasks. State the proposition and applicable version/environment. Submit source excerpts with original provenance; import CLI evidence only from authorized, exact published artifacts. Review the original evidence, its applicability and independence, merge derivative sources, and record concrete reasons. Authors and execution counts are never votes. Unknown provenance contributes no verified support. One reviewed origin contributes once; splitting independent experiments requires located units and an independence rationale. Use required_for_success only for facts that existing acceptance explicitly requires. Read the latest revision, then conclude verified, insufficient, disputed, or selected. For choices, record goals/constraints/tradeoffs and link all decisive factual issues. Report current conclusion status, independent support/opposition, duplicate references and unresolved questions. A review is a model judgment, not an automated proof. New evidence invalidates the current conclusion. Do not increase Agent/CLI budgets or access private logs to perform this review.";

fn string(max: usize) -> Value {
    json!({"type":"string","minLength":1,"maxLength":max})
}
fn nullable(value: Value) -> Value {
    json!({"anyOf":[value,{"type":"null"}]})
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub(crate) fn tool_spec() -> Value {
    let origin = object(
        json!({"locator":string(2048),"version":string(256)}),
        &["locator", "version"],
    );
    let source = object(
        json!({"origin":origin,"location":string(1024),"excerpt":string(8192),"upstream":nullable(origin.clone())}),
        &["origin", "location", "excerpt"],
    );
    let artifact = object(
        json!({"room_id":string(128),"artifact_id":string(128),"version":{"type":"integer","minimum":1},"sha256":{"type":"string","pattern":"^[a-f0-9]{64}$"}}),
        &["room_id", "artifact_id", "version", "sha256"],
    );
    let stance = json!({"type":"string","enum":["support","oppose"]});
    let review = object(
        json!({
            "disposition":{"type":"string","enum":["pending","verified","invalid","excluded"]},
            "rationale":string(4096),"origin":nullable(origin),"same_origin_as":nullable(string(128)),
            "independent_unit":nullable(string(2048)),"independence_rationale":nullable(string(2048)),"resolution":nullable(string(2048))
        }),
        &["disposition", "rationale"],
    );
    let mut variants = Vec::new();
    for (action, fields, required) in [
        (
            "open",
            json!({"question":string(2048),"context":string(4096),"kind":{"type":"string","enum":["fact","choice"]},"required_for_success":{"type":"boolean","default":false}}),
            vec!["question", "context", "kind"],
        ),
        (
            "submit",
            json!({"stance":stance,"claim":string(4096),"source":nullable(source),"artifact":nullable(artifact)}),
            vec!["stance", "claim"],
        ),
        (
            "review",
            json!({"evidence_id":string(128),"review":review}),
            vec!["evidence_id", "review"],
        ),
        (
            "read",
            json!({"issue_id":nullable(string(128)),"section":{"type":"string","enum":["summary","evidence","history"],"default":"summary"},"offset":{"type":"integer","minimum":0,"maximum":512},"limit":{"type":"integer","minimum":1,"maximum":50,"default":20}}),
            vec![],
        ),
        (
            "conclude",
            json!({"status":{"type":"string","enum":["verified","insufficient","disputed","selected"]},"selected_stance":stance,"conclusion":string(4096),"rationale":string(4096),"factual_issue_ids":{"type":"array","maxItems":32,"items":string(128)}}),
            vec!["status", "selected_stance", "conclusion", "rationale"],
        ),
    ] {
        let mut properties = fields;
        properties["action"] = json!({"type":"string","const":action});
        properties["root_task_id"] = string(128);
        let mut required = required;
        required.extend(["action", "root_task_id"]);
        if action != "read" {
            properties["idempotency_key"] = string(128);
            required.push("idempotency_key");
        }
        if !matches!(action, "read" | "open") {
            properties["issue_id"] = string(128);
            properties["expected_revision"] = json!({"type":"integer","minimum":1});
            required.extend(["issue_id", "expected_revision"]);
        }
        variants.push(object(properties, &required));
    }
    json!({"name":"team_decision","description":"Review evidence for one explicitly registered team issue. Manager opens, reviews and concludes; enrolled members submit evidence. Read returns bounded evidence/history pages and full-issue counts. Same reviewed original source counts once, regardless of author count. Verified requires source-backed semantic review and resolved counterevidence; it is not automated truth certification. Use insufficient/disputed when unresolved. Ordinary task division has no review requirement. Writes require the current issue revision and a stable idempotency key; identical replay returns the original response, whose revision may now be old. New evidence/review invalidates prior conclusions. Read current summary before relying on a result. Import only authorized exact published artifact versions; no private paths or raw logs. Current task/run authority and budgets remain unchanged.","inputSchema":{"type":"object","oneOf":variants}})
}
