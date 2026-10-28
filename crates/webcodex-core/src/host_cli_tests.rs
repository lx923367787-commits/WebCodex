use super::*;

fn registration(id: &str) -> HostCliRegistration {
    HostCliRegistration {
        adapter: crate::cli_adapter::cli_adapters(&[])
            .unwrap()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap(),
        executable_sha256: "a".repeat(64),
        worker_sid: "S-1-5-21-123-456-789-1001".into(),
    }
}

#[test]
fn native_registration_binds_executable_adapter_and_ignores_legacy_worker() {
    let original = registration("workbuddy");
    let hash = original.manifest("native-workbuddy").unwrap();
    let mut changed = original.clone();
    changed.worker_sid.push('2');
    assert_eq!(hash, changed.manifest("native-workbuddy").unwrap());
    changed = original.clone();
    changed.executable_sha256 = "b".repeat(64);
    assert_ne!(hash, changed.manifest("native-workbuddy").unwrap());
    assert_ne!(hash, original.manifest("other-resource").unwrap());
}

#[test]
fn native_argv_preserves_cli_default_and_normal_extra_flags() {
    let registration = registration("workbuddy");
    for (model, effort) in [
        (None, None),
        (Some("hy3"), None),
        (Some("hy3"), Some("minimal")),
    ] {
        let (args, _) = registration
            .adapter
            .invocation_selected(model, effort, "task")
            .unwrap();
        registration.validate_args(&args).unwrap();
        let mut injected = args.clone();
        injected.extend(["--settings".into(), "attacker.json".into()]);
        registration.validate_args(&injected).unwrap();
    }
}

#[test]
fn native_codex_reasoning_round_trips_its_own_config_grammar() {
    let registration = registration("codex");
    let (args, _) = registration
        .adapter
        .invocation_selected(Some("discovered-model"), Some("high"), "task")
        .unwrap();
    registration.validate_args(&args).unwrap();
    let mut malformed = args;
    let index = malformed.iter().position(|a| a == "--config").unwrap() + 1;
    malformed[index] = "unrelated_key=\"high\"".into();
    registration.validate_args(&malformed).unwrap();
}

#[test]
fn native_and_reviewed_resource_contracts_cannot_be_substituted() {
    let registration = registration("workbuddy");
    let mut descriptor = crate::runner_protocol::AgentExecutionResourceDescriptor {
        host_cli: Some(registration.clone()),
        resource_set_id: "native-workbuddy".into(),
        manifest_hash: registration.manifest("native-workbuddy").unwrap(),
        executable_identity: format!("sha256:{}", registration.executable_sha256),
        provider: Some(crate::agent_cli::AgentCliProvider::WorkBuddy),
        reviewed_controls: None,
        project_workspace_copy: false,
    };
    descriptor.validate().unwrap();
    descriptor.provider = Some(crate::agent_cli::AgentCliProvider::parse("codex").unwrap());
    assert!(descriptor.validate().is_err());
    descriptor.provider = Some(crate::agent_cli::AgentCliProvider::WorkBuddy);
    descriptor.manifest_hash = "f".repeat(64);
    assert!(descriptor.validate().is_err());
}
