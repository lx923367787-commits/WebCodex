use crate::tool_call::ToolCall;
use serde_json::json;

#[test]
fn tool_connection_contract_parses_and_redacts_launch_details() {
    let call = ToolCall::from_tool_name(
        "tool_connection",
        json!({
            "client_id":"local","action":"connect","id":"my-tool","entry":"C:/tools/mcp.py",
            "env":{"TOKEN":"must-not-be-logged"},"headers":{"Authorization":"must-not-be-logged"}
        }),
    )
    .unwrap();
    let ToolCall::ToolConnection { connection, .. } = &call else {
        panic!("wrong dispatch")
    };
    connection.validate().unwrap();
    assert_eq!(call.tool_name(), "tool_connection");
    let logged = call.session_log_arguments().to_string();
    assert!(!logged.contains("must-not-be-logged"));
    assert!(!logged.contains("mcp.py"));
    assert!(logged.contains("my-tool"));
}

#[test]
fn tool_connection_contract_rejects_missing_entry_and_ambiguous_mutations() {
    for args in [
        json!({"client_id":"local","action":"connect","id":"mine"}),
        json!({"client_id":"local","action":"remove","id":"mine","entry":"C:/new.exe"}),
    ] {
        let call = ToolCall::from_tool_name("tool_connection", args);
        match call {
            Ok(ToolCall::ToolConnection { connection, .. }) => {
                assert!(connection.validate().is_err())
            }
            Err(_) => {}
            _ => panic!("wrong dispatch"),
        }
    }
    let call = ToolCall::from_tool_name(
        "tool_connection",
        json!({"client_id":"local","action":"status"}),
    )
    .unwrap();
    let ToolCall::ToolConnection { connection, .. } = call else {
        panic!("wrong dispatch")
    };
    connection.validate().unwrap();
}
