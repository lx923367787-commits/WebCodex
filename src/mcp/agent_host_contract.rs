//! Data-free contract projection from the current Agent runtime catalog.
//! The caller must first authenticate the native host; this grants no run.
use serde_json::{json, Value};

pub(super) fn native_catalog() -> Value {
    let mut catalog = crate::agent_isolation::tool_specs();
    // These host-aware contracts replace the corresponding runtime projection.
    // A duplicate name makes exact discovery ambiguous even when it is callable.
    catalog["tools"].as_array_mut().unwrap().retain(|spec| {
        !matches!(
            spec["name"].as_str(),
            Some("agent_projects" | "agent_select_project" | "runtime_status" | "tool_manifest")
        )
    });
    catalog["tools"].as_array_mut().unwrap().extend([
        json!({"name":"agent_projects","description":"List only operator-authorized named projects and their folders on this connected computer. Creates no Agent. Match the user's project name to this list. If several projects are available and the user has not chosen one, ask before selecting; never infer the target from the Desktop's current folder.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
        json!({"name":"agent_select_project","description":"Select an exact project id returned by agent_projects for this chat. Use only the project the user chose. Creates the chat's independent Agent automatically. Repeating the same selection preserves identity. An already-bound chat cannot silently move to another project; start a new chat for a different project. The selected project uses the account-authorized runtime capabilities; private state stays bound to this Agent.","inputSchema":{"type":"object","properties":{"project":{"type":"string"}},"required":["project"],"additionalProperties":false}}),
        json!({"name":"runtime_status","description":"Check the actual Runner, build and Job status together with this chat's Agent connection. Privacy is a three-folder soft convention, not an OS access guarantee. First use selects the sole enabled named project, or reports project_selection_required when a choice is needed. A canonical work_on_project call with an explicit authorized runtime project or client_id + path initializes a fresh chat directly; a separate identity/select call is optional. Never guess among projects.","inputSchema":{"type":"object","properties":{"client_id":{"type":"string"},"compact":{"type":"boolean"},"summary_only":{"type":"boolean"}},"additionalProperties":false}}),
        json!({"name":"tool_manifest","description":"Read current Agent and shared-runtime tool contracts. Omit tool_name to discover this connection's tools, or supply one exact name. Discovery creates no Agent and grants no execution. work_on_project can initialize a fresh chat directly from an explicit authorized runtime project or client_id + path. agent_projects/agent_select_project remain optional explicit selection tools; agent_assignment shows authorized folders.","inputSchema":{"type":"object","properties":{"tool_name":{"type":"string"},"include_recommended_flows":{"type":"boolean"},"include_risk_summary":{"type":"boolean"}},"additionalProperties":false}}),
    ]);
    catalog
}

pub(super) fn discover(catalog: &Value, arguments: &Value) -> Result<Value, &'static str> {
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
    let name = match args.get("tool_name") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|name| !name.is_empty() && name.len() <= 128)
                .ok_or("agent_invalid_contract_request")?,
        ),
    };
    let specs = catalog
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("agent_contract_unavailable")?;
    if let Some(name) = name {
        let mut matches = specs.iter().filter(|spec| spec["name"] == name);
        let spec = matches.next().ok_or("unknown_tool")?;
        if matches.next().is_some() {
            return Err("agent_contract_unavailable");
        }
        return Ok(json!({
            "schema_version":1,"tool_name":name,"count":1,"returned_count":1,
            "filtered_count":1,"truncated":false,"contract":project(spec)?,
            "execution_granted":false,"state_changed":false
        }));
    }
    let contracts = specs.iter().map(project).collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"schema_version":1,"count":contracts.len(),"returned_count":contracts.len(),
        "filtered_count":contracts.len(),"truncated":false,"contracts":contracts,
        "next_action":"Call work_on_project with the user's explicit authorized project or client_id + path; it initializes this chat automatically. runtime_status, agent_projects and agent_select_project remain optional discovery/selection paths.",
        "execution_granted":false,"state_changed":false}),
    )
}

fn project(spec: &Value) -> Result<Value, &'static str> {
    let name = spec
        .get("name")
        .and_then(Value::as_str)
        .ok_or("agent_contract_unavailable")?;
    let description = spec
        .get("description")
        .and_then(Value::as_str)
        .ok_or("agent_contract_unavailable")?;
    let schema = spec
        .get("inputSchema")
        .filter(|schema| schema.is_object())
        .ok_or("agent_contract_unavailable")?;
    Ok(
        json!({"name":name,"description":description,"input_schema":schema,
                "annotations":spec.get("annotations").cloned().unwrap_or_else(||json!({})),
                "availability":"direct","gateway_tool":"call_runtime_tool"
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_catalog_keeps_shared_gateways_and_unique_host_contracts() {
        let catalog = native_catalog();
        let specs = catalog["tools"].as_array().unwrap();
        for name in [
            "runtime_status",
            "tool_manifest",
            "run_shell",
            "run_process",
            "plugin_tool",
            "mcp_tool",
        ] {
            assert_eq!(
                specs.iter().filter(|spec| spec["name"] == name).count(),
                1,
                "{name}"
            );
            assert_eq!(
                discover(&catalog, &json!({"tool_name":name})).unwrap()["contract"]["name"],
                name
            );
        }
    }

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
        let result = discover(
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
            let result = discover(&catalog(), &json!({"tool_name":name})).unwrap();
            assert_eq!(result["contract"]["name"], name);
            assert_eq!(result["count"], 1);
        }
    }

    #[test]
    fn native_contract_rejects_legacy_nested_and_authority_arguments() {
        for arguments in [
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
            assert!(discover(&catalog(), &arguments).is_err());
        }
    }

    #[test]
    fn native_contract_missing_or_ambiguous_catalog_fails_closed() {
        assert!(discover(&json!({}), &json!({"tool_name":"goal_handoff"})).is_err());
        let mut duplicate = catalog();
        let spec = duplicate["tools"][0].clone();
        duplicate["tools"].as_array_mut().unwrap().push(spec);
        assert!(discover(&duplicate, &json!({"tool_name":"goal_handoff"})).is_err());
    }

    #[test]
    fn native_contract_discovery_lists_only_current_public_contracts() {
        let result = discover(&catalog(), &json!({"include_recommended_flows":true})).unwrap();
        assert_eq!(result["count"], 3);
        assert_eq!(result["contracts"][0]["name"], "goal_handoff");
        assert_eq!(result["state_changed"], false);
        assert!(!result.to_string().contains("private_output"));
    }
}
