use super::*;

#[test]
fn document_tools_expose_project_bound_inputs_and_job_continuations() {
    let specs = registered_tool_specs();
    for name in [
        "present_document",
        "document_edit",
        "document_convert",
        "document_restore",
    ] {
        let spec = spec_named(&specs, name);
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(spec.input_schema["required"], json!(["project"]));
        let properties = spec.input_schema["properties"].as_object().unwrap();
        assert!(properties.contains_key("job_id"));
        assert_eq!(properties["review"]["type"], "integer");
        assert_eq!(properties["review"]["enum"], json!([0, 1]));
        for forbidden in ["cwd", "root", "executable", "args", "manifest_path"] {
            assert!(!properties.contains_key(forbidden), "{name}: {forbidden}");
        }
        assert!(spec.description.contains("job_id"));
        assert!(crate::tool_catalog::LOCAL_CODING_TOOL_NAMES.contains(&name));
    }
    assert!(
        spec_named(&specs, "present_document").input_schema["properties"]
            .get("operations")
            .is_none()
    );
    assert!(
        spec_named(&specs, "document_edit").input_schema["properties"]
            .get("view")
            .is_none()
    );
}
