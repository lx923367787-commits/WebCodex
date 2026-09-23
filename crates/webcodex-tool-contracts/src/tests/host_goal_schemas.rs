use super::*;
use crate::test_support::validate_schema_instance;

#[test]
fn host_goal_execution_wrapper_is_closed_and_cannot_supply_authority() {
    let schema = crate::goal_context_input_schema();
    let context = json!({"goal_id":"hgoal_example","node_id":"step","attempt_id":"attempt","plan_version":1,"fence":1,"operation_id":"op","context_epoch":"conversation-1"});
    validate_schema_instance(&context, &schema).unwrap();
    for field in [
        "goal_id",
        "node_id",
        "attempt_id",
        "plan_version",
        "fence",
        "operation_id",
    ] {
        let mut missing = context.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            validate_schema_instance(&missing, &schema).is_err(),
            "{field}"
        );
    }
    for field in ["owner_key", "confirmed", "allowed_scopes"] {
        let mut injected = context.clone();
        injected[field] = json!("untrusted");
        assert!(
            validate_schema_instance(&injected, &schema).is_err(),
            "{field}"
        );
    }
}

#[test]
fn host_goal_read_history_selects_checkpoints_attempts_or_operations() {
    let specs = registered_tool_specs();
    let schema = &spec_named(&specs, "goal_read").input_schema;
    for history in ["checkpoints", "attempts", "operations"] {
        validate_schema_instance(&json!({"goal_id":"hgoal_example","history_kind":history,"history_after":2,"history_limit":10}),schema).unwrap();
    }
    assert!(validate_schema_instance(
        &json!({"goal_id":"hgoal_example","history_kind":"secrets"}),
        schema
    )
    .is_err());
}

fn plan() -> Value {
    json!({
        "objective":"Create the deliverable", "deliverables":["result.txt"], "verification_paths":["Cargo.toml"],
        "final_criteria":[{"kind":"file_exists","id":"delivery","description":"Deliverable exists","path":"result.txt"}],
        "nodes":[{
            "id":"implement","title":"Create result","work":"Write and inspect result",
            "outputs":["result.txt"],"criterion_ids":["delivery"],"verification_paths":["src/lib.rs"],
            "acceptance":[{"kind":"command","id":"tests","description":"Tests pass","program":"cargo","args":["test"],"timeout_secs":60}],
            "failure_action":"Record the failure and rework this node"
        }],
        "state_management":{
            "facts":"Keep observed facts", "decisions":"Save rationale", "issues":"Keep unresolved issues",
            "artifacts":"Keep paths and fingerprints", "checkpoint_triggers":["node boundary","handoff"],
            "resume_instructions":"Read persisted state and reconcile unresolved operations"
        }
    })
}

#[test]
fn host_goal_plan_and_confirmation_schemas_require_the_explicit_gate() {
    let specs = registered_tool_specs();
    let save = spec_named(&specs, "goal_plan_save");
    let input = json!({"goal_id":"hgoal_example","expected_revision":1,"idempotency_key":"save-1","plan":plan()});
    validate_schema_instance(&input, &save.input_schema).unwrap();
    for key in [
        "deliverables",
        "final_criteria",
        "nodes",
        "state_management",
    ] {
        let mut bad = input.clone();
        bad["plan"].as_object_mut().unwrap().remove(key);
        assert!(
            validate_schema_instance(&bad, &save.input_schema).is_err(),
            "{key}"
        );
    }
    for key in ["confirmed", "owner_key", "phase", "executing"] {
        let mut bad = input.clone();
        bad[key] = json!(true);
        assert!(
            validate_schema_instance(&bad, &save.input_schema).is_err(),
            "{key}"
        );
    }
    let confirm = spec_named(&specs, "goal_confirm_plan");
    let input = json!({
        "goal_id":"hgoal_example","expected_revision":2,"idempotency_key":"confirm-1",
        "plan_version":1,"plan_sha256":"a".repeat(64),"confirmation":"I reviewed the saved plan; start it."
    });
    validate_schema_instance(&input, &confirm.input_schema).unwrap();
    let mut wrong_driver = input.clone();
    wrong_driver["goal_id"] = json!("wc_goal_acp");
    assert!(validate_schema_instance(&wrong_driver, &confirm.input_schema).is_err());
    for key in ["plan_version", "plan_sha256", "confirmation"] {
        let mut bad = input.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(
            validate_schema_instance(&bad, &confirm.input_schema).is_err(),
            "{key}"
        );
    }
    assert!(confirm
        .description
        .contains("not independently authenticated"));
    assert!(save.description.contains("awaiting_confirmation"));
}

#[test]
fn host_goal_node_verification_paths_are_optional_bounded_and_separate_from_final_defaults() {
    let specs = registered_tool_specs();
    let input_schema = &spec_named(&specs, "goal_plan_save").input_schema;
    let mut input = json!({"goal_id":"hgoal_example","expected_revision":1,"idempotency_key":"plan","plan":plan()});
    // The node declares its current-stage source, while plan-level paths are
    // separately available as defaults and for final command acceptance.
    validate_schema_instance(&input, input_schema).unwrap();
    input["plan"]["verification_paths"] = json!([]);
    validate_schema_instance(&input, input_schema).unwrap();
    serde_json::from_value::<webcodex_core::host_goal::HostGoalPlan>(input["plan"].clone())
        .unwrap()
        .validate()
        .unwrap();
    input["plan"]["verification_paths"] = json!(["Cargo.toml"]);
    input["plan"]["nodes"][0]
        .as_object_mut()
        .unwrap()
        .remove("verification_paths");
    validate_schema_instance(&input, input_schema).unwrap();
    serde_json::from_value::<webcodex_core::host_goal::HostGoalPlan>(input["plan"].clone())
        .unwrap()
        .validate()
        .unwrap();
    input["plan"]["nodes"][0]["verification_paths"] = json!(vec!["input.txt"; 65]);
    assert!(validate_schema_instance(&input, input_schema).is_err());
    let node_schema = &spec_named(&specs, "goal_read").output_schema["properties"]["output"]
        ["properties"]["node"];
    let sample_plan = plan();
    let node = &sample_plan["nodes"][0];
    validate_schema_instance(node, node_schema).unwrap();
    assert_eq!(
        node_schema["properties"]["verification_paths"]["default"],
        json!([])
    );
}

#[test]
fn host_goal_metadata_is_project_scoped_and_visible_through_generic_dispatch() {
    let specs = registered_tool_specs();
    for name in [
        "goal_mode_start",
        "goal_plan_save",
        "goal_confirm_plan",
        "goal_claim_node",
        "goal_checkpoint",
        "goal_check_node",
    ] {
        let definition = lookup_tool_definition(name).unwrap();
        assert!(
            definition.model_spec.unwrap().description.chars().count()
                <= crate::MODEL_TOOL_DESCRIPTION_MAX_CHARS,
            "{name} must not lose its contract to truncation"
        );
        assert_eq!(definition.metadata.effect, ToolEffect::Mutate, "{name}");
        assert_eq!(
            definition.metadata.authority,
            ToolAuthorityPolicy::Require("project:write"),
            "{name}"
        );
        assert!(definition.runner_capability.is_none(), "{name}");
        let spec = spec_named(&specs, name);
        let flattened = generic_tool_call_flattened_args_for_spec(spec);
        for field in spec.input_schema["properties"].as_object().unwrap().keys() {
            assert!(flattened.contains(field), "{name}: {field}");
        }
        assert!(crate::tool_catalog::LOCAL_CODING_TOOL_NAMES.contains(&name));
    }
    let bootstrap = lookup_tool_definition("goal_bootstrap").unwrap();
    assert_eq!(
        bootstrap.metadata.authority,
        ToolAuthorityPolicy::Require("project:read")
    );
    assert_eq!(bootstrap.metadata.effect, ToolEffect::Observe);
    for name in ["goal_read", "goal_list"] {
        assert_eq!(
            lookup_tool_definition(name).unwrap().metadata.authority,
            ToolAuthorityPolicy::RequireAny(&["coding_agent:run", "project:read"])
        );
    }
}

#[test]
fn host_goal_checkpoint_cannot_mutate_authority_and_model_cannot_forge_machine_evidence() {
    let specs = registered_tool_specs();
    let checkpoint = spec_named(&specs, "goal_checkpoint");
    let input = json!({"goal_id":"hgoal_example","expected_revision":4,"idempotency_key":"cp-1","patch":{"action":"handoff","reason":"Context warning","state":{"issues":["Need tests"],"next_step":"Run tests"}}});
    validate_schema_instance(&input, &checkpoint.input_schema).unwrap();
    for field in ["phase", "confirmation", "completed_node_ids", "fence"] {
        let mut bad = input.clone();
        bad["patch"]["state"][field] = json!("injected");
        assert!(
            validate_schema_instance(&bad, &checkpoint.input_schema).is_err(),
            "{field}"
        );
    }
    let check = spec_named(&specs, "goal_check_node");
    let mut input = json!({
        "goal_id":"hgoal_example","node_id":"analysis","attempt_id":"attempt-1","expected_revision":4,
        "plan_version":1,"idempotency_key":"check-1",
        "evidence":[{"criterion_id":"review","passed":true,"source":"model_self_check","evidence":"Compared source","source_refs":["notes.md"]}]
    });
    validate_schema_instance(&input, &check.input_schema).unwrap();
    let mut missing_sources = input.clone();
    missing_sources["evidence"][0]
        .as_object_mut()
        .unwrap()
        .remove("source_refs");
    assert!(validate_schema_instance(&missing_sources, &check.input_schema).is_err());
    let mut overlong = input.clone();
    overlong["evidence"][0]["evidence"] = json!("x".repeat(8193));
    assert!(validate_schema_instance(&overlong, &check.input_schema).is_err());
    input["evidence"][0]["source"] = json!("automated");
    assert!(validate_schema_instance(&input, &check.input_schema).is_err());
}

#[test]
fn host_goal_checkpoint_schema_bounds_manual_reports_and_allows_single_fact_deletion() {
    let specs = registered_tool_specs();
    let schema = &spec_named(&specs, "goal_checkpoint").input_schema;
    let mut input = json!({"goal_id":"hgoal_example","expected_revision":1,"idempotency_key":"checkpoint",
        "patch":{"reason":"Correct current facts","state":{"facts":{"obsolete":null,"verified":"updated"}}}});
    validate_schema_instance(&input, schema).unwrap();
    input["patch"]["state"]["facts"]["verified"] = json!(42);
    assert!(validate_schema_instance(&input, schema).is_err());
    input["patch"].as_object_mut().unwrap().remove("state");
    let resolution = json!({"operation_id":"operation-1","status":"succeeded","evidence":"Observed effect","source_refs":["result.txt"],"user_confirmation":"I checked the result; record this outcome."});
    input["patch"]["operation_resolutions"] = json!(vec![resolution.clone(); 8]);
    validate_schema_instance(&input, schema).unwrap();
    input["patch"]["operation_resolutions"] = json!(vec![resolution.clone(); 9]);
    assert!(validate_schema_instance(&input, schema).is_err());
    input["patch"]["operation_resolutions"] = json!([resolution]);
    input["patch"]["operation_resolutions"][0]["source_refs"] = json!(vec!["source"; 17]);
    assert!(validate_schema_instance(&input, schema).is_err());
    input["patch"]["operation_resolutions"][0]["source_refs"] = json!(["x".repeat(1025)]);
    assert!(validate_schema_instance(&input, schema).is_err());
}

#[test]
fn host_goal_output_schemas_match_domain_serialization_and_hide_owner() {
    use webcodex_core::host_goal::{HostGoalPhase, HostGoalPlan, HostGoalRecord, HostGoalState};
    let goal = HostGoalRecord {
        goal_id: "hgoal_example".into(),
        owner_key: "PRIVATE_OWNER".into(),
        project: "project".into(),
        project_root: "E:/project".into(),
        objective: "Create the deliverable".into(),
        phase: HostGoalPhase::AwaitingConfirmation,
        revision: 2,
        plan_version: 1,
        acceptance_contract_version: 1,
        plan_hash: Some("a".repeat(64)),
        plan_materialized: true,
        confirmation: None,
        current_node_id: None,
        current_attempt_id: None,
        fence: 0,
        completed_node_ids: vec![],
        state: HostGoalState::default(),
        created_at: 1,
        updated_at: 2,
    };
    let mut header = serde_json::to_value(&goal).unwrap();
    header.as_object_mut().unwrap().remove("project_root");
    header.as_object_mut().unwrap().remove("state");
    let output = json!({"success":true,"output":{
        "driver":"host","projection":"summary","goal":header,
        "progress_summary":{"partial":true,"completed_summary":"","next_step":"Read the plan","issues":[],"counts":{"facts":0,"decisions":0,"issues":0,"artifacts":0}},
        "plan_artifacts":null,"continuation":{"tool":"goal_read","arguments":{"goal_id":"hgoal_example","view":"node"}}
    }});
    assert!(!output.to_string().contains("PRIVATE_OWNER"));
    let specs = registered_tool_specs();
    for name in [
        "goal_mode_start",
        "goal_plan_save",
        "goal_confirm_plan",
        "goal_read",
        "goal_claim_node",
        "goal_checkpoint",
        "goal_check_node",
        "goal_bootstrap",
    ] {
        let schema = &spec_named(&specs, name).output_schema;
        validate_schema_instance(&output, schema).unwrap_or_else(|error| panic!("{name}: {error}"));
        let mut bad = output.clone();
        bad["output"]["goal"]["owner_key"] = json!("injected");
        assert!(validate_schema_instance(&bad, schema).is_err(), "{name}");
        bad = output.clone();
        bad["output"]["goal"]["state"] = json!({"facts":{"private":"Task data"}});
        assert!(
            validate_schema_instance(&bad, schema).is_err(),
            "{name}: summary must not return a full state"
        );
    }
    let mut plan =
        serde_json::to_value(serde_json::from_value::<HostGoalPlan>(plan()).unwrap()).unwrap();
    let node = plan.as_object_mut().unwrap().remove("nodes").unwrap()[0].clone();
    let mut node_output = output;
    node_output["output"]["projection"] = json!("node_context");
    node_output["output"]["requirements"] = plan;
    node_output["output"]["node"] = node;
    node_output["output"]["context_read"] = json!({"goal_id":"hgoal_example","node_id":"implement","revision":2,"plan_version":1,"complete":true});
    validate_schema_instance(&node_output, &spec_named(&specs, "goal_read").output_schema).unwrap();
}

#[test]
fn host_goal_context_estimate_is_optional_typed_metadata_on_normal_tools() {
    let meter = json!({
        "context_epoch":"conversation-1","observed_tokens":32000,
        "warning_tokens":24000,"handoff_tokens":32000,"level":"handoff_recommended",
        "newly_crossed":true,"unmeasured_modalities":false,"sample_count":9,
        "measurement":"observed_estimate","algorithm":"o200k_base","coverage":"partial",
        "notice":"Save a handoff checkpoint and start a new conversation.","retry_accounting":"per_server_invocation"
    });
    let specs = registered_tool_specs();
    for name in [
        "goal_bootstrap",
        "goal_checkpoint",
        "read_file",
        "run_process",
        "job_status",
    ] {
        let schema = &spec_named(&specs, name).output_schema["properties"]["output"]["properties"]
            ["context_meter"];
        assert!(schema.is_object(), "{name}");
        validate_schema_instance(&meter, schema).unwrap();
        let mut unavailable = meter.clone();
        unavailable["context_epoch"] = Value::Null;
        unavailable["observed_tokens"] = Value::Null;
        unavailable["level"] = json!("unavailable");
        validate_schema_instance(&unavailable, schema).unwrap();
        let mut invalid = meter.clone();
        invalid["actual_context_used_percent"] = json!(80);
        assert!(
            validate_schema_instance(&invalid, schema).is_err(),
            "{name}"
        );
    }
}

#[test]
fn host_goal_native_run_recovery_fields_are_explicit_and_do_not_claim_goal_completion() {
    let specs = registered_tool_specs();
    let output = json!({"success":false,"error":"Original Run is still active","output":{
        "error_kind":"goal_run_pending","run_id":"wc_agent_run_example","state":"running","execution_state":"started",
        "continuation":{"tool":"coding_agent_observe","arguments":{"run_id":"wc_agent_run_example"}}
    }});
    for name in [
        "goal_claim_node",
        "goal_check_node",
        "goal_checkpoint",
        "goal_read",
    ] {
        let schema = &spec_named(&specs, name).output_schema;
        validate_schema_instance(&output, schema).unwrap_or_else(|error| panic!("{name}: {error}"));
        let mut invalid = output.clone();
        invalid["output"]["execution_state"] = json!("safe_to_restart");
        assert!(
            validate_schema_instance(&invalid, schema).is_err(),
            "{name}"
        );
    }
    let description = crate::context_epoch_input_schema()["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(description.contains("weak binding"));
    assert!(description.contains("no authority"));
}
