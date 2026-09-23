use crate::tool_audit::{session_log_arguments_for_tool_request, session_log_result_for_tool};
use crate::ToolCall;
use serde_json::{json, Value};

fn goal_start_arguments() -> Value {
    json!({
        "project":"agent:runner:project","provider_id":"codex",
        "objective":"PRIVATE_OBJECTIVE",
        "criteria":[{"kind":"file_contains","id":"content","description":"PRIVATE_CRITERION","path":"PRIVATE_PATH","text":"PRIVATE_TEXT"}],
        "artifact_paths":["PRIVATE_PATH"],"config":{"model":"PRIVATE_MODEL"},
        "idempotency_key":"PRIVATE_REPLAY_KEY"
    })
}

#[test]
fn goal_tool_parser_preserves_typed_criteria_and_closes_untrusted_verdict_fields() {
    let input = goal_start_arguments();
    let call = ToolCall::from_tool_name("goal_start", input.clone()).unwrap();
    assert_eq!(call.tool_name(), "goal_start");
    assert_eq!(call.project(), Some("agent:runner:project"));
    assert!(matches!(call, ToolCall::GoalStart{criteria,limits:None,..} if criteria.len()==1));
    let mut bad = input.clone();
    bad["criteria"][0]["passed"] = json!(true);
    assert!(ToolCall::from_tool_name("goal_start", bad).is_err());
    let mut bad = input;
    bad["worker_epoch"] = json!("forged-owner");
    assert!(ToolCall::from_tool_name("goal_start", bad).is_err());
    for name in ["goal_read", "goal_cancel", "goal_resume"] {
        let call = ToolCall::from_tool_name(name, json!({"goal_id":"wc_goal_example"})).unwrap();
        assert_eq!(call.tool_name(), name);
        assert_eq!(call.project(), None);
        assert!(ToolCall::from_tool_name(
            name,
            json!({"goal_id":"wc_goal_example","project":"other"})
        )
        .is_err());
    }
    assert!(matches!(
        ToolCall::from_tool_name("goal_list", json!({})).unwrap(),
        ToolCall::GoalList {
            project: None,
            limit: None
        }
    ));
    assert!(matches!(
        ToolCall::from_tool_name(
            "goal_read",
            json!({"goal_id":"wc_goal_example","round_offset":2,"round_limit":5})
        )
        .unwrap(),
        ToolCall::GoalRead {
            round_offset: Some(2),
            round_limit: Some(5),
            ..
        }
    ));
    assert!(
        ToolCall::from_tool_name("goal_complete", json!({"goal_id":"wc_goal_example"})).is_err()
    );
}

#[test]
fn goal_audit_never_retains_objective_criteria_paths_keys_or_execution_output() {
    let request = session_log_arguments_for_tool_request("goal_start", &goal_start_arguments());
    assert_eq!(request["criterion_count"], 1);
    assert_eq!(request["artifact_count"], 1);
    assert_eq!(request["idempotency_key_present"], true);
    assert!(!request.to_string().contains("PRIVATE_"));
    let output = json!({
        "goal":{"goal_id":"wc_goal_example","state":"running","spec":{"objective":"PRIVATE_OBJECTIVE"},
        "rounds":[{"execution_output":"PRIVATE_OUTPUT","checks":[{"evidence":"PRIVATE_EVIDENCE"}]}]},
        "replayed":false,"total_rounds":9,"delivery_freshness":"unavailable","delivery_ready":false
    });
    for name in ["goal_start", "goal_read", "goal_cancel", "goal_resume"] {
        let summary = session_log_result_for_tool(name, &output);
        assert_eq!(summary["goal_id"], "wc_goal_example");
        assert_eq!(summary["returned_round_count"], 1);
        assert_eq!(summary["total_rounds"], 9);
        assert_eq!(summary["delivery_ready"], false);
        assert!(!summary.to_string().contains("PRIVATE_"), "{name}");
    }
    let summary =
        session_log_result_for_tool("goal_list", &json!({"goals":[output["goal"].clone()]}));
    assert_eq!(summary["goal_count"], 1);
    assert!(!summary.to_string().contains("PRIVATE_"));
}

#[test]
fn host_goal_parser_requires_confirmation_identity_and_preserves_continuation_fields() {
    let start = ToolCall::from_tool_name("goal_mode_start",json!({
        "project":"project","objective":"Plan first","idempotency_key":"start-key",
        "context_warning_tokens":24000,"context_handoff_tokens":32000
    })).unwrap();
    assert_eq!(start.project(),Some("project"));
    assert_eq!(start.tool_name(),"goal_mode_start");
    let bootstrap = ToolCall::from_tool_name("goal_bootstrap",json!({"project":"project"})).unwrap();
    assert_eq!(bootstrap.project(),Some("project"));
    assert!(matches!(bootstrap,ToolCall::GoalBootstrap { activate:true, .. }));
    let confirm = json!({
        "goal_id":"hgoal_example","expected_revision":2,"plan_version":1,
        "plan_sha256":"a".repeat(64),"confirmation":"PRIVATE_USER_MESSAGE","idempotency_key":"PRIVATE_KEY"
    });
    assert!(matches!(ToolCall::from_tool_name("goal_confirm_plan",confirm.clone()).unwrap(),ToolCall::GoalConfirmPlan{plan_version:1,..}));
    for missing in ["plan_sha256","confirmation","expected_revision","plan_version"] {
        let mut bad = confirm.clone();
        bad.as_object_mut().unwrap().remove(missing);
        assert!(ToolCall::from_tool_name("goal_confirm_plan",bad).is_err(),"{missing}");
    }
    let mut bad = confirm.clone();
    bad["authenticated_human"] = json!(true);
    assert!(ToolCall::from_tool_name("goal_confirm_plan",bad).is_err());
    let audit = session_log_arguments_for_tool_request("goal_confirm_plan",&confirm);
    assert!(!audit.to_string().contains("PRIVATE_"));
    assert_eq!(audit["plan_version"],1);
    let claim = ToolCall::from_tool_name("goal_claim_node",json!({
        "goal_id":"hgoal_example","node_id":"step","expected_revision":3,"plan_version":1,
        "idempotency_key":"claim-key","context_epoch":"context-1","takeover_attempt_id":"old-attempt"
    })).unwrap();
    assert!(matches!(claim,ToolCall::GoalClaimNode{rework:false,takeover_attempt_id:Some(_),..}));
    let read = ToolCall::from_tool_name("goal_read",json!({"goal_id":"hgoal_example","history_after":3,"history_limit":20,"history_kind":"attempts"})).unwrap();
    assert!(matches!(read,ToolCall::GoalRead{history_after:Some(3),history_limit:Some(20),history_kind:Some(ref kind),..} if kind=="attempts"));
}

#[test]
fn host_goal_audit_redacts_plan_drafts_state_and_evidence() {
    for (name,input) in [
        ("goal_plan_save",json!({"goal_id":"hgoal_example","expected_revision":1,"idempotency_key":"PRIVATE_KEY","plan":{"objective":"PRIVATE_PLAN"}})),
        ("goal_checkpoint",json!({"goal_id":"hgoal_example","expected_revision":2,"idempotency_key":"PRIVATE_KEY","patch":{"reason":"PRIVATE_REASON","state":{"plan_draft":"PRIVATE_DRAFT"}},"context_epoch":"PRIVATE_CONTEXT"})),
        ("goal_check_node",json!({"goal_id":"hgoal_example","node_id":"node","attempt_id":"attempt","expected_revision":3,"plan_version":1,"idempotency_key":"PRIVATE_KEY","evidence":[{"evidence":"PRIVATE_EVIDENCE"}]})),
    ] {
        let parsed = ToolCall::from_tool_name(name,input.clone()).unwrap();
        assert_eq!(parsed.tool_name(),name);
        let audit = session_log_arguments_for_tool_request(name,&input);
        assert!(!audit.to_string().contains("PRIVATE_"),"{name}: {audit}");
        let output = session_log_result_for_tool(name,&json!({
            "goal":{"goal_id":"hgoal_example","phase":"planning","revision":2,"plan_version":1,"state":{"issues":["PRIVATE_ISSUE"]}},
            "plan":{"objective":"PRIVATE_PLAN"},"replayed":false
        }));
        assert!(!output.to_string().contains("PRIVATE_"),"{name}");
        assert_eq!(output["goal_id"],"hgoal_example");
    }
    let read = session_log_result_for_tool("goal_read",&json!({"goal":{
        "goal_id":"hgoal_example","phase":"planning","state":{"facts":{"credential":"PRIVATE_FACT"},"next_step":"PRIVATE_STEP"}
    }}));
    assert!(!read.to_string().contains("PRIVATE_"));
    assert_eq!(read["phase"],"planning");
}
