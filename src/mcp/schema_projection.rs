//! Lossless, tool-local compression for the adaptive discovery surface.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Factor repeated output subschemas into local definitions. Inputs, tool
/// descriptions, canonical contracts and invocation validation stay unchanged.
pub(super) fn deduplicate_output_schemas(payload: &mut Value) {
    if let Some(tools) = payload.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(schema) = tool.get_mut("outputSchema") {
                deduplicate_schema(schema);
            }
        }
    }
}

fn visit_children(schema: &mut Value, visit: &mut impl FnMut(&mut Value)) {
    // Visit schema locations only. In particular, examples, defaults, enums,
    // property names and required arrays are data, not replaceable schemas.
    for keyword in [
        "properties",
        "patternProperties",
        "dependencies",
        "dependentSchemas",
        "$defs",
        "definitions",
    ] {
        if let Some(children) = schema.get_mut(keyword).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                visit(child);
            }
        }
    }
    for keyword in [
        "items",
        "additionalItems",
        "additionalProperties",
        "unevaluatedItems",
        "unevaluatedProperties",
        "contains",
        "propertyNames",
        "not",
        "if",
        "then",
        "else",
        "contentSchema",
    ] {
        if let Some(child) = schema.get_mut(keyword) {
            if let Some(children) = child.as_array_mut() {
                for child in children {
                    visit(child);
                }
            } else {
                visit(child);
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = schema.get_mut(keyword).and_then(Value::as_array_mut) {
            for child in children {
                visit(child);
            }
        }
    }
}

fn count_subschemas(schema: &mut Value, counts: &mut BTreeMap<String, usize>) -> bool {
    // Moving a schema with an existing identifier/reference could change its
    // resolution scope. Leave those contracts entirely intact rather than
    // attempting to rewrite provider-defined references or definition names.
    let mut scoped = [
        "$id",
        "id",
        "$ref",
        "$dynamicRef",
        "$recursiveRef",
        "$anchor",
        "$dynamicAnchor",
        "$recursiveAnchor",
        "$defs",
        "definitions",
    ]
    .iter()
    .any(|key| schema.get(key).is_some());
    let serialized = serde_json::to_string(schema).expect("JSON schema serializes");
    if schema.is_object() && serialized.len() >= 256 {
        *counts.entry(serialized).or_default() += 1;
    }
    visit_children(schema, &mut |child| {
        scoped |= count_subschemas(child, counts);
    });
    scoped
}

fn intern_subschema(
    schema: &mut Value,
    counts: &BTreeMap<String, usize>,
    names: &mut BTreeMap<String, String>,
    definitions: &mut Map<String, Value>,
) {
    let serialized = serde_json::to_string(schema).expect("JSON schema serializes");
    if counts.get(&serialized).is_some_and(|count| *count > 1) {
        let name = if let Some(name) = names.get(&serialized) {
            name.clone()
        } else {
            let name = format!("s{}", names.len());
            names.insert(serialized, name.clone());
            let mut definition = schema.take();
            visit_children(&mut definition, &mut |child| {
                intern_subschema(child, counts, names, definitions);
            });
            definitions.insert(name.clone(), definition);
            name
        };
        *schema = json!({"$ref": format!("#/$defs/{name}")});
    } else {
        visit_children(schema, &mut |child| {
            intern_subschema(child, counts, names, definitions);
        });
    }
}

pub(super) fn deduplicate_schema(schema: &mut Value) {
    let mut counts = BTreeMap::new();
    if count_subschemas(schema, &mut counts) {
        return;
    }
    let mut names = BTreeMap::new();
    let mut definitions = Map::new();
    visit_children(schema, &mut |child| {
        intern_subschema(child, &counts, &mut names, &mut definitions);
    });
    if !definitions.is_empty() {
        schema["$defs"] = Value::Object(definitions);
    }
}
