use super::*;
use crate::test_support::validate_schema_instance;

#[test]
fn host_goal_workflow_overview_is_bounded_and_only_declared_on_entry_outputs() {
    let specs = registered_tool_specs();
    for name in ["goal_mode_start", "goal_bootstrap"] {
        let schema = &spec_named(&specs, name).output_schema;
        assert_eq!(
            schema["properties"]["output"]["additionalProperties"],
            false
        );
        let guidance = &schema["properties"]["output"]["properties"]["workflow_overview"];
        validate_schema_instance(&json!("Map only; follow the current phase."), guidance).unwrap();
        for invalid in [
            json!(true),
            json!({"phase":"ready"}),
            json!(""),
            json!("x".repeat(3073)),
        ] {
            assert!(validate_schema_instance(&invalid, guidance).is_err());
        }
    }
    for name in [
        "goal_plan_save",
        "goal_confirm_plan",
        "goal_claim_node",
        "goal_checkpoint",
        "goal_check_node",
    ] {
        assert!(
            spec_named(&specs, name).output_schema["properties"]["output"]["properties"]
                .get("workflow_overview")
                .is_none(),
            "{name}"
        );
    }
}

#[test]
fn host_goal_review_schema_limits_integer_requirement_and_preserves_optional_legacy_fields() {
    let schema = crate::registry::input_schemas::host_goals::host_plan_schema();
    let node_schema = &schema["properties"]["nodes"]["items"];
    let legacy = json!({"id":"one","title":"Edit text","work":"Replace exact text","outputs":["result.txt"],"criterion_ids":["check"],"acceptance":[{"kind":"file_exists","id":"check","description":"Output exists","path":"result.txt"}],"failure_action":"Repair output"});
    validate_schema_instance(&legacy, node_schema).unwrap();
    for allowed in [0, 1] {
        let mut node = legacy.clone();
        node["review"] = json!(allowed);
        validate_schema_instance(&node, node_schema).unwrap();
    }
    for rejected in [
        json!(true),
        json!(false),
        json!(-1),
        json!(2),
        json!("1"),
        json!(null),
    ] {
        let mut node = legacy.clone();
        node["review"] = rejected;
        assert!(validate_schema_instance(&node, node_schema).is_err());
    }
    for rounds in [0, 6] {
        let mut node = legacy.clone();
        node["review_max_rounds"] = json!(rounds);
        assert!(validate_schema_instance(&node, node_schema).is_err());
    }
    let specs = registered_tool_specs();
    validate_schema_instance(
        &json!({"goal_id":"hgoal_one","view":"detail","section":"reviews"}),
        &spec_named(&specs, "goal_read").input_schema,
    )
    .unwrap();
}

#[test]
fn host_goal_execution_wrapper_is_closed_and_cannot_supply_authority() {
    let schema = crate::goal_context_input_schema();
    let context = json!({"goal_id":"hgoal_example","node_id":"step","attempt_id":"attempt","plan_version":1,"fence":1,"operation_id":"op","context_epoch":"conversation-1"});
    validate_schema_instance(&context, &schema).unwrap();
    let mut repair = context.clone();
    repair["repair"] = json!({"repair_id":"hrepair_example","diagnosis":"Correct the failed import and repeat original acceptance"});
    validate_schema_instance(&repair, &schema).unwrap();
    for field in ["passed", "max_rounds", "approved", "criteria"] {
        let mut forged = repair.clone();
        forged["repair"][field] = json!(true);
        assert!(
            validate_schema_instance(&forged, &schema).is_err(),
            "{field}"
        );
    }
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

#[test]
fn host_goal_handoff_read_requires_a_bounded_source_filter_without_authority() {
    let specs = registered_tool_specs();
    let schema = &spec_named(&specs, "goal_read").input_schema;
    let input = json!({"goal_id":"hgoal_example","view":"handoff","source_context_epoch":"chat-A"});
    validate_schema_instance(&input, schema).unwrap();
    for invalid in [
        json!({"goal_id":"hgoal_example","view":"handoff"}),
        json!({"goal_id":"hgoal_example","source_context_epoch":"chat-A"}),
        json!({"goal_id":"hgoal_example","view":"node","source_context_epoch":"chat-A"}),
        json!({"goal_id":"wc_goal_example","view":"handoff","source_context_epoch":"chat-A"}),
        json!({"goal_id":"hgoal_example","view":"handoff","source_context_epoch":""}),
        json!({"goal_id":"hgoal_example","view":"handoff","source_context_epoch":"a".repeat(129)}),
    ] {
        assert!(
            validate_schema_instance(&invalid, schema).is_err(),
            "{invalid}"
        );
    }
    let description = schema["properties"]["source_context_epoch"]["description"]
        .as_str()
        .unwrap();
    assert!(description.contains("never sets caller context_epoch"));
    let start = &spec_named(&specs, "goal_mode_start").input_schema;
    assert_eq!(
        start["properties"]["context_warning_tokens"]["default"],
        600000
    );
    assert_eq!(
        start["properties"]["context_handoff_tokens"]["default"],
        800000
    );
}

#[test]
fn host_goal_handoff_output_is_source_specific_closed_and_keeps_unknown_provenance() {
    let specs = registered_tool_specs();
    let schema = &spec_named(&specs, "goal_read").output_schema;
    let mut output = json!({"success":true,"output":{
        "driver":"host","projection":"handoff",
        "goal":{
            "goal_id":"hgoal_example","project":"project","objective":"Recover the same goal",
            "phase":"executing","revision":7,"plan_version":1,"acceptance_contract_version":1,
            "plan_hash":null,"plan_materialized":true,"confirmation":null,
            "current_node_id":"step","current_attempt_id":"attempt-B","fence":2,
            "completed_node_ids":[],"created_at":1,"updated_at":7
        },
        "source_context":{"context_epoch":"chat-A","identity_source":"window_bound","lifecycle_state":"retired","retired_at":5},
        "checkpoint":{"sequence":1,"revision":5,"action":"handoff","source_context_epoch":"chat-A","source_fence":1,"created_at":5},
        "current_attempt":{
            "binding":{"goal_id":"hgoal_example","node_id":"step","attempt_id":"attempt-B","plan_version":1,"fence":2},
            "context_epoch":"chat-B","identity_source":"window_bound","status":"passed","started_at":6,"finished_at":7
        },
        "unresolved_operation_count":0,"business_verified":true,
        "business_evidence":{"operation_id":"business-B","attempt_id":"attempt-B","status":"succeeded","check_passed":true}
    }});
    validate_schema_instance(&output, schema).unwrap();
    for pointer in [
        "/output/source_context/owner_key",
        "/output/checkpoint/owner_key",
        "/output/current_attempt/owner_key",
        "/output/owner_key",
    ] {
        let mut invalid = output.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        invalid.pointer_mut(parent).unwrap()[field] = json!("private-owner");
        assert!(
            validate_schema_instance(&invalid, schema).is_err(),
            "{pointer}"
        );
    }
    let mut missing_evidence = output.clone();
    missing_evidence["output"]["business_evidence"] = Value::Null;
    assert!(validate_schema_instance(&missing_evidence, schema).is_err());
    let mut forged_status = output.clone();
    forged_status["output"]["business_evidence"]["status"] = json!("running");
    assert!(validate_schema_instance(&forged_status, schema).is_err());
    output["output"]["source_context"]["identity_source"] = Value::Null;
    output["output"]["source_context"]["lifecycle_state"] = json!("unknown");
    output["output"]["source_context"]["retired_at"] = Value::Null;
    output["output"]["checkpoint"]["action"] = Value::Null;
    output["output"]["checkpoint"]["source_context_epoch"] = Value::Null;
    output["output"]["checkpoint"]["source_fence"] = Value::Null;
    output["output"]["current_attempt"]["context_epoch"] = Value::Null;
    output["output"]["current_attempt"]["identity_source"] = Value::Null;
    output["output"]["business_verified"] = json!(false);
    output["output"]["business_evidence"] = Value::Null;
    validate_schema_instance(&output, schema).unwrap();
    output["output"]["checkpoint"] = Value::Null;
    output["output"]["current_attempt"] = Value::Null;
    validate_schema_instance(&output, schema).unwrap();
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
        task_directory: None,
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
