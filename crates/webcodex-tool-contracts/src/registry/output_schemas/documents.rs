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
