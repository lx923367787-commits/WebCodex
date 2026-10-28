//! Pure projection/admission checks. No CLI process, account or proxy is probed.
use super::*;
use webcodex_core::runner_protocol::AgentExecutionTarget;

fn query() -> AgentCliCatalogQuery {
    AgentCliCatalogQuery {
        version: 1,
        target: AgentExecutionTarget {
            client_id: "runner".into(),
            runner_instance_id: "instance".into(),
        },
        provider: AgentCliProvider::WorkBuddy,
        deadline_at_ms: chrono::Utc::now().timestamp_millis() + 40_000,
    }
}
fn resource() -> AgentExecutionResourceDescriptor {
    AgentExecutionResourceDescriptor {
        host_cli: None,
        project_workspace_copy: false,
        resource_set_id: "fixed-profile".into(),
        manifest_hash: "a".repeat(64),
        executable_identity: format!("sha256:{}", "b".repeat(64)),
        provider: Some(AgentCliProvider::WorkBuddy),
        reviewed_controls: None,
    }
}
fn helper() -> CliCatalog {
    CliCatalog {
        provider_id: "workbuddy".into(),
        label: "must-not-be-projected".into(),
        installed: true,
        version: "1.2.3".into(),
        models: vec!["synthetic-model".into()],
        efforts: vec!["high".into()],
        model_efforts: Default::default(),
        source: "installed_help",
        status: "ready",
        message: "Synthetic fixed metadata only".into(),
        checked_at: chrono::Utc::now().timestamp_millis().to_string(),
        account_availability_verified: false,
    }
}

#[test]
fn catalog_projection_is_explicit_bounded_and_not_an_account_attestation() {
    let query = query();
    let deadline = ControlDeadline::new(query.deadline_at_ms).unwrap();
    let projected = project_catalog(&query, &resource(), helper(), &deadline).unwrap();
    assert_eq!(projected.expires_at_ms - projected.observed_at_ms, 60_000);
    assert!(!projected.account_availability_verified);
    assert!(!projected.nested_delegation_verified_disabled);
    let output = bounded_json_result(Ok(projected), 128 * 1024);
    assert_eq!(output.exit_code, Some(0));
    let value = output.stdout.unwrap();
    assert!(!value.contains("must-not-be-projected"));
    assert!(!value.contains("checkedAt"));
    assert!(value.contains("synthetic-model"));
}

#[test]
fn catalog_rejects_wrong_provider_state_claims_messages_and_duplicate_values() {
    let query = query();
    let deadline = ControlDeadline::new(query.deadline_at_ms).unwrap();
    for kind in 0..7 {
        let mut value = helper();
        match kind {
            0 => value.provider_id = "antigravity".into(),
            1 => value.account_availability_verified = true,
            2 => value.source = "live_catalog",
            3 => value.status = "accepted",
            4 => value.message = "unexpected\ncontrol".into(),
            5 => value.models.push("synthetic-model".into()),
            6 => value.checked_at = "0".into(),
            _ => unreachable!(),
        }
        assert!(
            project_catalog(&query, &resource(), value, &deadline).is_err(),
            "kind={kind}"
        );
    }
}

#[test]
fn metadata_budget_must_include_fixed_query_and_cleanup_before_start() {
    assert!(ControlDeadline::new(chrono::Utc::now().timestamp_millis() - 1).is_err());
    let short = ControlDeadline::new(chrono::Utc::now().timestamp_millis() + 31_000).unwrap();
    assert!(short.require(Duration::from_secs(33)).is_err());
    let budget = ControlDeadline::new(chrono::Utc::now().timestamp_millis() + 40_000).unwrap();
    budget.require(Duration::from_secs(33)).unwrap();
}

#[test]
fn catalog_target_is_the_authenticated_sink_instance() {
    let (tx, _receiver) = tokio::sync::mpsc::channel(1);
    let sink = RunnerSink::WebSocket {
        tx,
        client_id: "runner".into(),
        runner_instance_id: "instance".into(),
    };
    validate_target(&sink, "runner", "instance").unwrap();
    assert!(validate_target(&sink, "foreign", "instance").is_err());
    assert!(validate_target(&sink, "runner", "retired-instance").is_err());
}

#[test]
fn oversized_typed_result_never_falls_back_to_raw_stdout() {
    let output = bounded_json_result(Ok("x".repeat(128)), 16);
    assert!(output.stdout.is_none());
    assert_eq!(
        output.error.as_deref(),
        Some("agent_cli_response_too_large")
    );
}

#[test]
fn cli_load_error_keeps_installation_separate_from_execution_readiness() {
    let inventory = serde_json::from_value(serde_json::json!({
        "version":1,"target":{"client_id":"runner","runner_instance_id":"instance"},
        "observed_at_ms":1,"expires_at_ms":60001,"installations":[{
            "providerId":"codex","label":"Codex","installed":true,"native":true,
            "adapterAvailable":true,"status":"discovered","message":"Installed",
            "executionReady":true,"executionMode":"native_host"
        }]
    }))
    .unwrap();
    let code = "cli_resource_configuration_failed:agent_execution_metadata_invalid";
    let projected = inventory_with_load_status(Ok(inventory), Some(code.into())).unwrap();
    let entry = &projected.installations[0];
    assert!(entry.installed);
    assert_eq!(entry.execution_ready, Some(false));
    assert_eq!(entry.reason_code.as_deref(), Some(code));
    assert!(entry.message.contains(code));
    assert_eq!(
        inventory_with_load_status(Err("metadata_query_failed".into()), Some(code.into()))
            .unwrap_err(),
        code
    );
}

#[test]
fn optional_cli_load_error_does_not_disable_another_registered_cli() {
    let inventory = serde_json::from_value(serde_json::json!({
        "version":1,"target":{"client_id":"runner","runner_instance_id":"instance"},
        "observed_at_ms":1,"expires_at_ms":60001,"installations":[
            {"providerId":"codex","label":"Codex","installed":true,"native":true,"adapterAvailable":true,"status":"discovered","message":"Loaded","executionReady":true},
            {"providerId":"claude","label":"Claude","installed":true,"native":true,"adapterAvailable":true,"status":"discovered","message":"Found","executionReady":false}
        ]
    })).unwrap();
    let errors =
        std::collections::BTreeMap::from([("claude".into(), "native_cli_open_failed:32".into())]);
    let projected = inventory_with_resource_errors(Ok(inventory), &errors).unwrap();
    assert_eq!(projected.installations[0].execution_ready, Some(true));
    assert!(projected.installations[0].reason_code.is_none());
    assert!(projected.installations[1].installed);
    assert_eq!(
        projected.installations[1].reason_code.as_deref(),
        Some("native_cli_open_failed:32")
    );
}
