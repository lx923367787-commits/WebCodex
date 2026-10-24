use crate::tool_call_test_support::sample_tool_args;
use crate::ToolCall;
use serde_json::json;
use webcodex_core::authority::ToolAuthorityPolicy;
use webcodex_tool_contracts::{
    lookup_tool_definition, registered_tool_specs, ToolApprovalPolicy, ToolEffect,
};

const TOOLS: &[&str] = &[
    "team_policy_get",
    "team_policy_save",
    "team_plan_register",
    "team_plan_list",
    "team_plan_read",
    "team_plan_cancel",
    "team_cli_start",
    "team_cli_observe",
    "team_agent_start",
    "team_agent_observe",
];

#[test]
fn team_tools_are_registered_typed_and_project_scoped() {
    let specs = registered_tool_specs();
    for name in TOOLS {
        let definition = lookup_tool_definition(name).unwrap();
        let spec = specs.iter().find(|spec| spec.name == *name).unwrap();
        assert!(definition.metadata.requires_project, "{name}");
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert!(spec.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("project")));
        let call = ToolCall::from_tool_name(name, sample_tool_args(name)).unwrap();
        assert_eq!(call.tool_name(), *name);
        assert_eq!(call.project(), Some("agent:oe:private-drop"));
        assert_eq!(call.session_id(), None);
        let mut extra = sample_tool_args(name);
        extra["authority_override"] = json!(true);
        assert!(ToolCall::from_tool_name(name, extra).is_err(), "{name}");
        let mut missing = sample_tool_args(name);
        missing.as_object_mut().unwrap().remove("project");
        assert!(ToolCall::from_tool_name(name, missing).is_err(), "{name}");
    }
}

#[test]
fn team_tools_preserve_scopes_and_observation_is_pure() {
    for name in [
        "team_policy_get",
        "team_plan_list",
        "team_plan_read",
        "team_cli_observe",
        "team_agent_observe",
    ] {
        let definition = lookup_tool_definition(name).unwrap();
        assert_eq!(definition.metadata.effect, ToolEffect::Observe);
        assert_eq!(definition.metadata.approval, ToolApprovalPolicy::None);
        assert_eq!(
            definition.metadata.authority,
            ToolAuthorityPolicy::RequireAll(&["project:read", "communication:read"])
        );
    }
    for name in ["team_policy_save", "team_plan_register"] {
        let definition = lookup_tool_definition(name).unwrap();
        assert_eq!(definition.metadata.effect, ToolEffect::Mutate);
        assert_eq!(
            definition.metadata.authority,
            ToolAuthorityPolicy::RequireAll(&["project:write", "communication:manage"])
        );
    }
    let cancel = lookup_tool_definition("team_plan_cancel").unwrap();
    assert_eq!(cancel.metadata.effect, ToolEffect::Mutate);
    assert_eq!(
        cancel.metadata.authority,
        ToolAuthorityPolicy::RequireAll(&["project:write", "communication:manage", "job:run"])
    );
    let definition = lookup_tool_definition("team_cli_start").unwrap();
    assert_eq!(definition.metadata.effect, ToolEffect::Execute);
    assert!(definition.requires_permission());
    assert!(definition.is_shell_like());
    assert_eq!(
        definition.metadata.authority,
        ToolAuthorityPolicy::RequireAll(&[
            "project:write",
            "communication:manage",
            "job:run",
            "plugin:inspect",
            "plugin:invoke"
        ])
    );
    let ordinary = lookup_tool_definition("team_agent_start").unwrap();
    assert_eq!(ordinary.metadata.effect, ToolEffect::Execute);
    assert!(ordinary.requires_permission());
    assert!(!ordinary.is_shell_like());
    assert_eq!(
        ordinary.metadata.authority,
        ToolAuthorityPolicy::RequireAll(&[
            "project:write",
            "communication:manage",
            "plugin:inspect",
            "plugin:invoke"
        ])
    );
}

#[test]
fn team_nested_authority_fields_and_invalid_call_bounds_are_rejected() {
    let mut policy = sample_tool_args("team_policy_save");
    policy["policy"]["subagents"]["fallback_limit"] = json!(100);
    assert!(ToolCall::from_tool_name("team_policy_save", policy).is_err());
    let mut plan = sample_tool_args("team_plan_register");
    plan["plan"]["manager"]["executable"] = json!("unauthorized");
    assert!(ToolCall::from_tool_name("team_plan_register", plan).is_err());
    for (field, value) in [
        ("call_index", json!(32)),
        ("timeout_secs", json!(0)),
        ("timeout_secs", json!(3601)),
    ] {
        let mut args = sample_tool_args("team_cli_start");
        args[field] = value;
        assert!(ToolCall::from_tool_name("team_cli_start", args).is_err());
    }
    let mut args = sample_tool_args("team_cli_start");
    args.as_object_mut()
        .unwrap()
        .remove("fallback_thinking_profile_id");
    assert!(ToolCall::from_tool_name("team_cli_start", args).is_err());
    let mut args = sample_tool_args("team_policy_save");
    args["expected_revision"] = json!(-1);
    assert!(ToolCall::from_tool_name("team_policy_save", args).is_err());
}

#[test]
fn team_audit_omits_provider_payloads_replay_keys_and_profile_values() {
    let mut args = sample_tool_args("team_cli_start");
    args["idempotency_key"] = json!("private-replay-key");
    args["fallback_thinking_profile_id"] = json!("private-profile-choice");
    let call = ToolCall::from_tool_name("team_cli_start", args).unwrap();
    let audit = call.session_log_arguments();
    let text = audit.to_string();
    assert!(!text.contains("private-replay-key"));
    assert!(!text.contains("private-profile-choice"));
    assert_eq!(audit["call_index"], 0);
    let mut args = sample_tool_args("team_policy_save");
    args["policy"]["providers"] =
        json!([{"provider_id":"workbuddy","enabled":true,"model":"private-model-choice"}]);
    let call = ToolCall::from_tool_name("team_policy_save", args).unwrap();
    assert!(!call
        .session_log_arguments()
        .to_string()
        .contains("private-model-choice"));
    let result = crate::tool_audit::session_log_result_for_tool(
        "team_cli_start",
        &json!({
            "attempt":{"attempt_id":"source-attempt","job_id":"job-123","instruction":"private task instruction"},
            "process_result":{"output":{"stdout_tail":"private CLI text"}},"replayed":false
        }),
    );
    assert_eq!(result["source_attempt_id"], "source-attempt");
    assert_eq!(result["job_id"], "job-123");
    assert!(!result.to_string().contains("private"));
}

#[test]
fn ordinary_agent_input_cannot_claim_recovery_authority_and_audit_only_keeps_ids() {
    for field in [
        "fallback",
        "is_recovery",
        "counts_toward_regular_max",
        "instruction",
        "command",
    ] {
        let mut args = sample_tool_args("team_agent_start");
        args[field] = json!("untrusted override");
        assert!(
            ToolCall::from_tool_name("team_agent_start", args).is_err(),
            "{field}"
        );
    }
    let mut args = sample_tool_args("team_agent_start");
    args["thinking_profile_id"] = json!("private-profile");
    args["idempotency_key"] = json!("private-key");
    let call = ToolCall::from_tool_name("team_agent_start", args).unwrap();
    let audit = call.session_log_arguments().to_string();
    assert!(!audit.contains("private-profile"));
    assert!(!audit.contains("private-key"));
    let result = crate::tool_audit::session_log_result_for_tool(
        "team_agent_start",
        &json!({
            "attempt":{"team_agent_attempt_id":"attempt-1","root_task_id":"root","source_task_id":"child","state":"queued","instruction":"private instruction","thinking_profile_id":"private profile"},
            "queued":true,"created":true,"replayed":false
        }),
    );
    assert_eq!(result["team_agent_attempt_id"], "attempt-1");
    assert_eq!(result["state"], "queued");
    assert!(!result.to_string().contains("private"));
}

#[test]
fn plan_read_cli_pagination_is_bounded_and_does_not_expand_cancel_arguments() {
    let mut args = sample_tool_args("team_plan_read");
    args["attempts_offset"] = json!(20);
    args["attempts_limit"] = json!(10);
    let call = ToolCall::from_tool_name("team_plan_read", args.clone()).unwrap();
    let encoded = serde_json::to_value(&call).unwrap();
    assert_eq!(encoded["params"]["attempts_offset"], 20);
    assert_eq!(encoded["params"]["attempts_limit"], 10);
    assert_eq!(call.session_log_arguments()["attempts_offset"], 20);
    for (field, value) in [
        ("attempts_offset", json!(-1)),
        ("attempts_offset", json!(65537)),
        ("attempts_limit", json!(0)),
        ("attempts_limit", json!(21)),
    ] {
        let mut invalid = args.clone();
        invalid[field] = value;
        assert!(
            ToolCall::from_tool_name("team_plan_read", invalid).is_err(),
            "{field}"
        );
    }
    assert!(ToolCall::from_tool_name("team_plan_cancel", args).is_err());
}
