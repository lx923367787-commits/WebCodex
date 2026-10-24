//! Exact, data-free contract projection from the existing isolated catalog.
//! The caller must first authenticate the native host; this grants no run.
use serde_json::{json, Value};

pub(super) fn exact(catalog: &Value, arguments: &Value) -> Result<Value, &'static str> {
    let args = arguments
        .as_object()
        .ok_or("agent_invalid_contract_request")?;
    if args.iter().any(|(key, value)| match key.as_str() {
        "tool_name" => false,
        "include_recommended_flows" | "include_risk_summary" => !value.is_boolean(),
        _ => true,
    }) {
        return Err("agent_invalid_contract_request");
    }
    let name = args
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or("agent_invalid_contract_request")?;
    let specs = catalog
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("agent_contract_unavailable")?;
    let mut matches = specs.iter().filter(|spec| spec["name"] == name);
    let spec = matches.next().ok_or("agent_tool_not_isolated")?;
    if matches.next().is_some() {
        return Err("agent_contract_unavailable");
    }
    let description = spec
        .get("description")
        .and_then(Value::as_str)
        .ok_or("agent_contract_unavailable")?;
    let schema = spec
        .get("inputSchema")
        .filter(|schema| schema.is_object())
        .ok_or("agent_contract_unavailable")?;
    Ok(json!({
        "schema_version":1,"tool_name":name,"count":1,"returned_count":1,
        "filtered_count":1,"truncated":false,
        "contract":{"name":name,"description":description,"input_schema":schema,
            "annotations":spec.get("annotations").cloned().unwrap_or_else(||json!({})),
            "availability":"direct","gateway_tool":"call_runtime_tool"},
        "execution_granted":false,"state_changed":false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Value {
        json!({"tools":[
            {"name":"goal_handoff","description":"Current native handoff only",
             "inputSchema":{"type":"object","properties":{"action":{"enum":["status","prepare","commit"]},
                 "launch_claim":{"type":"string"}}},"outputSchema":{"private_output":"never expose"}},
            {"name":"goal_read","description":"Own Goal only","inputSchema":{"type":"object"}},
            {"name":"agent_attach","description":"Exact enrollment grant only","inputSchema":{"type":"object"}}
        ]})
    }

    #[test]
    fn native_contract_exact_projection_preserves_isolated_schema_without_private_output() {
        let catalog = catalog();
        let result = exact(
            &catalog,
            &json!({"tool_name":"goal_handoff",
            "include_recommended_flows":true,"include_risk_summary":false}),
        )
        .unwrap();
        assert_eq!(
            result["contract"]["input_schema"],
            catalog["tools"][0]["inputSchema"]
        );
        assert!(result
            .pointer("/contract/input_schema/properties/launch_claim")
            .is_some());
        assert!(result
            .pointer("/contract/input_schema/properties/context_epoch")
            .is_none());
        assert_eq!(result["execution_granted"], false);
        assert_eq!(result["state_changed"], false);
        assert!(!result.to_string().contains("private_output"));
        assert!(!result.to_string().contains("Own Goal only"));
    }

    #[test]
    fn native_contract_exact_lookup_supports_other_admitted_tools_after_handoff() {
        for name in ["goal_read", "agent_attach"] {
            let result = exact(&catalog(), &json!({"tool_name":name})).unwrap();
            assert_eq!(result["contract"]["name"], name);
            assert_eq!(result["count"], 1);
        }
    }

    #[test]
    fn native_contract_rejects_unfiltered_legacy_nested_and_authority_arguments() {
        for arguments in [
            json!({}),
            json!([]),
            json!({"tool_name":null}),
            json!({"tool_name":"runtime_status"}),
            json!({"tool_name":"call_runtime_tool"}),
            json!({"tool_name":"goal_handoff","category":"goal"}),
            json!({"tool_name":"goal_handoff","intent":"discovery"}),
            json!({"tool_name":"goal_handoff","goal_id":"other-goal"}),
            json!({"tool_name":"goal_handoff","_meta":{"openai/session":"forged"}}),
            json!({"tool_name":"goal_handoff","context_epoch":"forged"}),
            json!({"tool_name":"goal_handoff","include_risk_summary":"false"}),
        ] {
            assert!(exact(&catalog(), &arguments).is_err());
        }
    }

    #[test]
    fn native_contract_missing_or_ambiguous_catalog_fails_closed() {
        assert!(exact(&json!({}), &json!({"tool_name":"goal_handoff"})).is_err());
        let mut duplicate = catalog();
        let spec = duplicate["tools"][0].clone();
        duplicate["tools"].as_array_mut().unwrap().push(spec);
        assert!(exact(&duplicate, &json!({"tool_name":"goal_handoff"})).is_err());
    }
}
