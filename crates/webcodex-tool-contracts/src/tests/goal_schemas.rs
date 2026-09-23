use super::*;
use crate::test_support::validate_schema_instance;
use webcodex_core::goal::{GoalRecord, GoalRound, GoalSpec, GoalState};

fn start_input() -> Value {
    json!({
        "project":"agent:runner:project","provider_id":"codex",
        "objective":"Produce a verified result",
        "criteria":[
            {"kind":"file_exists","id":"exists","description":"Result exists","path":"result.txt"},
            {"kind":"file_contains","id":"content","description":"Expected content","path":"result.txt","text":"ready"},
            {"kind":"command","id":"tests","description":"Tests pass","program":"cargo","args":["test"],"timeout_secs":60},
            {"kind":"review","id":"review","description":"Independent review passes"}
        ],
        "reviewer_provider_id":"reviewer","artifact_paths":["result.txt"],
        "idempotency_key":"goal-initiation"
    })
}

#[test]
fn goal_contracts_keep_execution_authority_explicit_and_cancel_inherited() {
    for name in ["goal_start", "goal_resume"] {
        let metadata = lookup_tool_definition(name).unwrap().metadata;
        assert_eq!(metadata.effect, ToolEffect::Execute);
        assert_eq!(metadata.approval, ToolApprovalPolicy::Standard);
        assert_eq!(
            metadata.authority,
            ToolAuthorityPolicy::RequireAll(&[
                "coding_agent:run",
                "project:write",
                "project:read",
                "job:run",
                "job:detach"
            ])
        );
    }
    let cancel = lookup_tool_definition("goal_cancel").unwrap().metadata;
    assert_eq!(cancel.risk, ToolRisk::RunControl);
    assert_eq!(cancel.approval, ToolApprovalPolicy::InheritFromStart);
    assert!(!cancel.approval.requires_permission());
    for name in ["goal_read", "goal_list"] {
        let metadata = lookup_tool_definition(name).unwrap().metadata;
        assert_eq!(metadata.effect, ToolEffect::Observe);
        assert_eq!(metadata.approval, ToolApprovalPolicy::None);
    }
    assert!(lookup_tool_definition("goal_complete").is_none());
}

#[test]
fn goal_start_schema_closes_criteria_and_rejects_authority_or_verdict_inputs() {
    let specs = registered_tool_specs();
    let spec = spec_named(&specs, "goal_start");
    let input = start_input();
    validate_schema_instance(&input, &spec.input_schema).unwrap();
    for field in [
        "owner_key",
        "worker_epoch",
        "goal_id",
        "state",
        "target",
        "verdict",
        "credentials",
    ] {
        let mut bad = input.clone();
        bad[field] = json!("injected");
        assert!(
            validate_schema_instance(&bad, &spec.input_schema).is_err(),
            "{field}"
        );
    }
    let mut bad = input.clone();
    bad["criteria"][0]["passed"] = json!(true);
    assert!(validate_schema_instance(&bad, &spec.input_schema).is_err());
    let read = spec_named(&specs, "goal_read");
    validate_schema_instance(
        &json!({"goal_id":"wc_goal_example","round_offset":0,"round_limit":5}),
        &read.input_schema,
    )
    .unwrap();
    for limit in [0, 6] {
        assert!(validate_schema_instance(
            &json!({"goal_id":"wc_goal_example","round_limit":limit}),
            &read.input_schema
        )
        .is_err());
    }
    let mut bad = input.clone();
    bad["limits"] = json!({"max_rounds":0});
    assert!(validate_schema_instance(&bad, &spec.input_schema).is_err());
    for name in [
        "goal_start",
        "goal_read",
        "goal_list",
        "goal_cancel",
        "goal_resume",
    ] {
        let spec = spec_named(&specs, name);
        let flattened = generic_tool_call_flattened_args_for_spec(spec);
        for field in spec.input_schema["properties"].as_object().unwrap().keys() {
            assert!(flattened.contains(field), "{name} flattened field {field}");
        }
        assert!(crate::tool_catalog::LOCAL_CODING_TOOL_NAMES.contains(&name));
    }
}

#[test]
fn goal_record_output_schema_matches_serialized_record_without_private_ownership() {
    let mut input = start_input();
    input.as_object_mut().unwrap().remove("idempotency_key");
    let goal = GoalRecord {
        goal_id: format!("wc_goal_{}", "a".repeat(32)),
        revision: 1,
        spec: serde_json::from_value::<GoalSpec>(input).unwrap(),
        state: GoalState::Running,
        target: None,
        rounds: vec![GoalRound::new(1, 1000)],
        created_at: 1000,
        deadline_at: 2000,
        updated_at: 1000,
        reason: None,
        cancel_requested: false,
        owner_key: "PRIVATE_OWNER".to_string(),
        worker_epoch: Some("PRIVATE_EPOCH".to_string()),
    };
    let goal_json = serde_json::to_value(&goal).unwrap();
    assert!(goal_json.get("owner_key").is_none());
    assert!(goal_json.get("worker_epoch").is_none());
    let specs = registered_tool_specs();
    for name in ["goal_start", "goal_read", "goal_cancel", "goal_resume"] {
        let output = json!({"success":true,"output":{
            "goal":goal_json,"replayed":false,"total_rounds":1,"round_offset":0,
            "next_round_offset":null,"has_more":false,
            "delivery_freshness":"not_completed","delivery_ready":false
        }});
        validate_schema_instance(&output, &spec_named(&specs, name).output_schema).unwrap();
        let mut bad = output;
        bad["output"]["goal"]["rounds"] = serde_json::to_value(
            (1..=6)
                .map(|number| GoalRound::new(number, 1000))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(validate_schema_instance(&bad, &spec_named(&specs, name).output_schema).is_err());
    }
    let summary = json!({
        "goal_id":goal.goal_id,"revision":goal.revision,
        "spec":{"project":goal.spec.project,"provider_id":goal.spec.provider_id,"objective":goal.spec.objective},
        "state":goal.state,"round_count":goal.rounds.len(),"last_verdict":null,
        "created_at":goal.created_at,"deadline_at":goal.deadline_at,"updated_at":goal.updated_at,
        "reason":goal.reason,"cancel_requested":goal.cancel_requested
    });
    let output = json!({"success":true,"output":{"goals":[summary]}});
    validate_schema_instance(&output, &spec_named(&specs, "goal_list").output_schema).unwrap();
    let bad = json!({"success":true,"output":{"goals":[goal_json]}});
    assert!(
        validate_schema_instance(&bad, &spec_named(&specs, "goal_list").output_schema).is_err()
    );
}
