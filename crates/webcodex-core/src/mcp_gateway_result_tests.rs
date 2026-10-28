use super::*;
use serde_json::json;

#[test]
fn large_evidence_result_passes_provider_and_gateway_validation() {
    let structured = json!({
        "body": "证据".repeat(24_000),
        "locators": vec![0; 8_192],
    });
    let result = McpGatewayToolResult {
        content: vec![McpGatewayContent::Text {
            text: serde_json::to_string(&structured).unwrap(),
        }],
        structured_content: Some(structured),
        is_error: false,
    };
    validate_result_json_value(
        &serde_json::to_value(&result).unwrap(),
        MCP_GATEWAY_MAX_MESSAGE_BYTES,
        "provider tool result",
    )
    .unwrap();
    validate_response(&McpGatewayResponse::success(
        McpGatewayResponsePayload::ToolResult { result },
    ))
    .unwrap();
}

#[test]
fn result_node_budget_is_larger_but_remains_bounded() {
    let at_limit = json!({"locators": vec![0; MCP_GATEWAY_MAX_RESULT_JSON_NODES - 2]});
    validate_result_json_value(&at_limit, MCP_GATEWAY_MAX_MESSAGE_BYTES, "result").unwrap();
    let over_limit = json!({"locators": vec![0; MCP_GATEWAY_MAX_RESULT_JSON_NODES - 1]});
    assert!(
        validate_result_json_value(&over_limit, MCP_GATEWAY_MAX_MESSAGE_BYTES, "result")
            .unwrap_err()
            .contains("node count")
    );
}

#[test]
fn result_strings_and_aggregate_remain_bounded() {
    let at_limit = json!({"body": "x".repeat(MCP_GATEWAY_MAX_RESULT_JSON_STRING_BYTES)});
    validate_result_json_value(&at_limit, MCP_GATEWAY_MAX_MESSAGE_BYTES, "result").unwrap();
    let over_limit = json!({"body": "x".repeat(MCP_GATEWAY_MAX_RESULT_JSON_STRING_BYTES + 1)});
    assert!(
        validate_result_json_value(&over_limit, MCP_GATEWAY_MAX_MESSAGE_BYTES, "result")
            .unwrap_err()
            .contains("string")
    );
    let result = McpGatewayToolResult {
        content: vec![
            McpGatewayContent::Text {
                text: "x".repeat(MCP_GATEWAY_MAX_TEXT_CONTENT_BYTES),
            };
            4
        ],
        structured_content: None,
        is_error: false,
    };
    assert!(validate_tool_result(&result)
        .unwrap_err()
        .contains("tool result exceeds"));
}

#[test]
fn result_budget_does_not_expand_arguments_schemas_or_depth() {
    let many_nodes = json!({"values": vec![0; MCP_GATEWAY_MAX_JSON_NODES]});
    assert!(
        validate_json_value(&many_nodes, MCP_GATEWAY_MAX_ARGUMENT_BYTES, "arguments")
            .unwrap_err()
            .contains("node count")
    );
    let schema = McpGatewaySchemaObservation {
        input_schema: many_nodes,
        output_schema: None,
        annotations: None,
    };
    assert!(validate_schema_observation(&schema)
        .unwrap_err()
        .contains("node count"));
    let long_string = json!("x".repeat(MCP_GATEWAY_MAX_JSON_STRING_BYTES + 1));
    assert!(
        validate_json_value(&long_string, MCP_GATEWAY_MAX_MESSAGE_BYTES, "input")
            .unwrap_err()
            .contains("string")
    );
    let mut deep = json!({});
    for _ in 0..=MCP_GATEWAY_MAX_JSON_DEPTH {
        deep = json!({"next": deep});
    }
    assert!(
        validate_result_json_value(&deep, MCP_GATEWAY_MAX_MESSAGE_BYTES, "result")
            .unwrap_err()
            .contains("depth")
    );
}
