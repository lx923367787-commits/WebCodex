use super::*;

fn expand_local_references(schema: &Value) -> Value {
    fn expand(value: &Value, root: &Value) -> Value {
        if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
            let pointer = reference.strip_prefix('#').expect("tool-local reference");
            assert_eq!(value.as_object().unwrap().len(), 1);
            return expand(root.pointer(pointer).expect("resolved definition"), root);
        }
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), expand(value, root)))
                    .collect(),
            ),
            Value::Array(array) => {
                Value::Array(array.iter().map(|value| expand(value, root)).collect())
            }
            _ => value.clone(),
        }
    }
    let mut inline = schema.clone();
    inline.as_object_mut().unwrap().remove("$defs");
    expand(&inline, schema)
}

#[test]
fn adaptive_output_schema_projection_preserves_every_canonical_constraint() {
    let mut payload = mcp_tools_list_payload_with_features_for_auth(
        ModelSurface::AdaptiveRuntime,
        false,
        false,
        true,
        true,
        None,
    );
    add_stateless_workflow_recorder_metadata(&mut payload, ModelSurface::AdaptiveRuntime);
    let original = payload.clone();
    crate::mcp::schema_projection::deduplicate_output_schemas(&mut payload);
    let original_tools = original["tools"].as_array().unwrap();
    for (projected, canonical) in payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .zip(original_tools)
    {
        let mut expanded = projected.clone();
        expanded["outputSchema"] = expand_local_references(&projected["outputSchema"]);
        assert_eq!(
            &expanded, canonical,
            "lossy projection for {}",
            canonical["name"]
        );
    }
    assert!(
        serde_json::to_vec(&payload).unwrap().len() < serde_json::to_vec(&original).unwrap().len()
    );
    let once = payload.clone();
    crate::mcp::schema_projection::deduplicate_output_schemas(&mut payload);
    assert_eq!(payload, once, "projection must be idempotent");
}

#[test]
fn adaptive_schema_deduplication_preserves_one_of_and_strict_object_validation() {
    let detail = json!({
        "type": "object",
        "description": "A sufficiently detailed repeated output schema with strict fields and bounded values. Factoring this schema into a local definition must preserve both positive and negative validation, including when a duplicate branch makes a oneOf match ambiguous.",
        "properties": {"text": {"type": "string", "minLength": 3, "maxLength": 8}},
        "required": ["text"], "additionalProperties": false
    });
    let branch = |kind: &str| {
        json!({
            "type": "object",
            "properties": {"kind": {"const": kind}, "detail": detail},
            "required": ["kind", "detail"], "additionalProperties": false
        })
    };
    let original = json!({"type": "object", "oneOf": [branch("ambiguous"), branch("ambiguous"), branch("unique")]});
    let mut projected = original.clone();
    crate::mcp::schema_projection::deduplicate_schema(&mut projected);
    assert!(projected["$defs"].is_object());
    assert_eq!(projected["oneOf"][0], projected["oneOf"][1]);
    let expanded = expand_local_references(&projected);
    assert_eq!(expanded, original);
    for (instance, valid) in [
        (json!({"kind": "unique", "detail": {"text": "valid"}}), true),
        (
            json!({"kind": "ambiguous", "detail": {"text": "valid"}}),
            false,
        ),
        (
            json!({"kind": "unknown", "detail": {"text": "valid"}}),
            false,
        ),
        (json!({"kind": "unique"}), false),
        (json!({"kind": "unique", "detail": {"text": "x"}}), false),
        (
            json!({"kind": "unique", "detail": {"text": "too long text"}}),
            false,
        ),
        (
            json!({"kind": "unique", "detail": {"text": "valid", "extra": true}}),
            false,
        ),
    ] {
        for schema in [&original, &expanded] {
            assert_eq!(
                webcodex_tool_contracts::test_support::validate_schema_instance(&instance, schema)
                    .is_ok(),
                valid,
                "{instance}"
            );
        }
    }
}

#[test]
fn adaptive_schema_deduplication_keeps_schema_data_and_existing_reference_scopes() {
    let repeated = json!({"type": "string", "description": "annotation ".repeat(30)});
    let original = json!({
        "type": "object",
        "properties": {"first": repeated, "second": repeated},
        "required": ["first", "second"],
        "dependencies": {"first": ["second"]},
        "examples": [repeated, repeated],
        "default": repeated,
        "enum": [repeated],
        "additionalProperties": false
    });
    let mut projected = original.clone();
    crate::mcp::schema_projection::deduplicate_schema(&mut projected);
    for key in [
        "required",
        "dependencies",
        "examples",
        "default",
        "enum",
        "additionalProperties",
    ] {
        assert_eq!(projected[key], original[key], "{key} is not a subschema");
    }
    assert_eq!(expand_local_references(&projected), original);

    for scoped in [
        json!({"$ref": "#/$defs/existing", "$defs": {"existing": repeated}}),
        json!({"$id": "https://example.test/nested", "properties": {"ref": {"$ref": "#local"}}}),
        json!({"$anchor": "local", "type": "string"}),
        json!({"definitions": {"existing": repeated}}),
        json!({"dependencies": {"first": {"$ref": "#local"}, "second": ["first"]}}),
    ] {
        let mut schema = original.clone();
        schema["properties"]["scoped"] = scoped;
        let before = schema.clone();
        crate::mcp::schema_projection::deduplicate_schema(&mut schema);
        assert_eq!(schema, before, "existing scopes must remain untouched");
    }
}
