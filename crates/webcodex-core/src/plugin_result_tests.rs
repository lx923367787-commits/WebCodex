use super::*;
use serde_json::json;

#[test]
fn large_evidence_result_passes_gateway_and_output_schema_validation() {
    let structured = json!({
        "body": "证据".repeat(24_000),
        "locators": vec![0; 8_192],
    });
    let output_schema = json!({
        "type": "object",
        "properties": {
            "body": {"type": "string"},
            "locators": {"type": "array", "items": {"type": "integer"}},
        },
        "required": ["body", "locators"],
        "additionalProperties": false,
    });
    validate_plugin_structured_output(&output_schema, &structured).unwrap();
    let result = PluginToolResult {
        content: vec![PluginContent::Text {
            text: serde_json::to_string(&structured).unwrap(),
        }],
        structured_content: Some(structured),
        is_error: false,
    };
    validate_response(&PluginGatewayResponse::success(
        PluginGatewayResponsePayload::ToolResult { result },
    ))
    .unwrap();
}

#[test]
fn result_node_budget_is_larger_but_remains_bounded() {
    let at_limit = json!({"locators": vec![0; PLUGIN_MAX_RESULT_JSON_NODES - 2]});
    validate_result_json_value(&at_limit, PLUGIN_MAX_MESSAGE_BYTES, "result").unwrap();
    let over_limit = json!({"locators": vec![0; PLUGIN_MAX_RESULT_JSON_NODES - 1]});
    assert!(
        validate_result_json_value(&over_limit, PLUGIN_MAX_MESSAGE_BYTES, "result")
            .unwrap_err()
            .contains("node")
    );
}

#[test]
fn result_strings_and_aggregate_remain_bounded() {
    let at_limit = json!({"body": "x".repeat(PLUGIN_MAX_RESULT_JSON_STRING_BYTES)});
    validate_result_json_value(&at_limit, PLUGIN_MAX_MESSAGE_BYTES, "result").unwrap();
    let over_limit = json!({"body": "x".repeat(PLUGIN_MAX_RESULT_JSON_STRING_BYTES + 1)});
    assert!(
        validate_result_json_value(&over_limit, PLUGIN_MAX_MESSAGE_BYTES, "result")
            .unwrap_err()
            .contains("string")
    );
    let result = PluginToolResult {
        content: vec![
            PluginContent::Text {
                text: "x".repeat(PLUGIN_MAX_TEXT_CONTENT_BYTES),
            };
            4
        ],
        structured_content: None,
        is_error: false,
    };
    assert!(validate_tool_result(&result)
        .unwrap_err()
        .contains("aggregate"));
}

#[test]
fn result_budget_does_not_expand_arguments_schemas_or_depth() {
    let many_nodes = json!({"values": vec![0; PLUGIN_MAX_JSON_NODES]});
    assert!(
        validate_json_value(&many_nodes, PLUGIN_MAX_ARGUMENT_BYTES, "arguments")
            .unwrap_err()
            .contains("node")
    );
    assert!(
        validate_json_value(&many_nodes, PLUGIN_MAX_SCHEMA_BYTES, "schema")
            .unwrap_err()
            .contains("node")
    );
    let long_string = json!("x".repeat(PLUGIN_MAX_JSON_STRING_BYTES + 1));
    assert!(
        validate_json_value(&long_string, PLUGIN_MAX_MESSAGE_BYTES, "input")
            .unwrap_err()
            .contains("string")
    );
    let mut deep = json!({});
    for _ in 0..=PLUGIN_MAX_JSON_DEPTH {
        deep = json!({"next": deep});
    }
    assert!(
        validate_result_json_value(&deep, PLUGIN_MAX_MESSAGE_BYTES, "result")
            .unwrap_err()
            .contains("depth")
    );
}
