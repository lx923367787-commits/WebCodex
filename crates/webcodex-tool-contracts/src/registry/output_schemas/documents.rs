use super::common::wrapped_output_schema;
use serde_json::{json, Value};

pub fn output_schema_for_tool(name: &str) -> Option<Value> {
    if !matches!(
        name,
        "present_document" | "document_edit" | "document_convert" | "document_restore"
    ) {
        return None;
    }
    Some(wrapped_output_schema(vec![
        (
            "status",
            json!({"type":"string","enum":["pending","completed","outcome_unknown"]}),
        ),
        ("request_id", json!({"type":"string"})),
        ("project", json!({"type":"string"})),
        ("job_id", json!({"type":"string"})),
        ("source_sha256", json!({"type":"string"})),
        (
            "review_probe",
            json!({"type":"object","properties":{"status":{"type":"string","enum":["verified","unavailable"]},"source_path":{"type":"string"},"source_sha256":{"type":"string"},"render_dependencies_sha256":{"type":["string","null"]}},"required":["status","source_path","source_sha256","render_dependencies_sha256"],"additionalProperties":false}),
        ),
        ("review", json!({"type":"integer","enum":[0,1]})),
        ("review_reason", json!({"type":"string"})),
        (
            "review_status",
            json!({"type":"string","enum":["not_required","pending"]}),
        ),
        (
            "review_material",
            json!({"type":"object","description":"Current real source SHA, render provenance, preview fingerprints and full required/prepared/missing coverage. Prepared images are not proof of native image delivery or model approval.","additionalProperties":true}),
        ),
        (
            "total_pages",
            json!({"type":["integer","null"],"minimum":1}),
        ),
        (
            "native_image_receipts",
            json!({"type":"array","maxItems":2,"items":{"type":"object","additionalProperties":true},"description":"Runtime facts about actual native image blocks returned by this MCP response, not model verdicts."}),
        ),
        (
            "review_receipt",
            json!({"type":"object","additionalProperties":true,"description":"Host Goal receipt recording version-bound review material delivery in this caller context."}),
        ),
        (
            "review_transport_error",
            json!({"description":"A review delivery failure; the delivery remains pending."}),
        ),
        (
            "content",
            json!({"description":"Bounded real document structure and text; exact shape depends on the document format."}),
        ),
        (
            "previews",
            json!({"type":"array","maxItems":2,"items":{"type":"object","properties":{"path":{"type":"string"},"sha256":{"type":"string"},"source_sha256":{"type":"string"},"mime_type":{"type":"string"},"page":{"type":"integer"}},"required":["path","sha256","mime_type","source_sha256"],"additionalProperties":true}}),
        ),
        (
            "visual_delivery",
            json!({"type":"string","enum":["not_requested","ready","unavailable","mcp_images","partial"]}),
        ),
        (
            "continuation",
            json!({"type":"object","additionalProperties":true}),
        ),
    ]))
}
