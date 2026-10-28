use super::*;
use base64::Engine;

#[test]
fn authenticated_browser_image_framing_is_native_and_not_a_permission_bypass() {
    let encoded="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6rCEAAAAASUVORK5CYII=";
    let bytes = general_purpose::STANDARD.decode(encoded).unwrap();
    let output = json!({"content_base64":encoded,"mime_type":"image/png","file_bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"path":"browser/fixture/step-00.png"});
    let framed = crate::mcp::resources::mcp_isolated_browser_result(
        "agent_process",
        true,
        crate::tool_runtime::ToolResult::ok(output.clone()),
    );
    assert_eq!(framed["content"][1]["type"], "image");
    assert_eq!(framed["content"][1]["data"], encoded);
    assert!(framed["structuredContent"]["output"]
        .get("content_base64")
        .is_none());
    for (name, requested, success) in [
        ("agent_process", false, true),
        ("other", true, true),
        ("agent_process", true, false),
    ] {
        let result = if success {
            crate::tool_runtime::ToolResult::ok(output.clone())
        } else {
            crate::tool_runtime::ToolResult::err_with_output("denied", output.clone())
        };
        let framed = crate::mcp::resources::mcp_isolated_browser_result(name, requested, result);
        assert!(!framed["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "image"));
    }
}
