use super::*;

fn args(action: &str) -> Vec<String> {
    [
        action,
        "--config",
        "runner.toml",
        "--server-url",
        "http://127.0.0.1:9",
        "--client-id",
        "runner",
        "--token-file",
        "user-token",
        "--json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
fn options(temp: &tempfile::TempDir, token: &str) -> Options {
    let config = temp.path().join("runner.toml");
    let token_file = temp.path().join("user-token");
    std::fs::write(&config,"server_url=\"http://127.0.0.1:9\"\nclient_id=\"runner\"\ntoken=\"never-use-host-runner-secret\"\n").unwrap();
    std::fs::write(&token_file, token).unwrap();
    Options {
        action: Action::PolicyGet,
        config,
        server_url: "http://127.0.0.1:9".into(),
        client_id: "runner".into(),
        token_file,
    }
}

#[test]
fn office_parser_only_admits_human_configuration_read_and_public_task_creation() {
    for action in [
        "policy-get",
        "policy-save",
        "plan-list",
        "plan-read",
        "task-create",
        "cli-discover",
        "cli-models",
    ] {
        assert_eq!(parse(&args(action)).unwrap().action.action(), action);
    }
    for action in [
        "plan-register",
        "plan-cancel",
        "cli-start",
        "cli-observe",
        "agent-start",
        "agent-observe",
        "shell",
        "run",
    ] {
        assert!(parse(&args(action)).is_err());
    }
    let opts = parse(&args("task-create")).unwrap();
    let params=prepare_params(&opts,json!({"project":"agent:runner:repo","title":"Task","instruction":"Public brief","idempotency_key":"once"})).unwrap();
    assert_eq!(params["project"], "agent:runner:repo");
    assert_eq!(params["action"], "task-create");
    assert!(params.get("referenced_project_id").is_none());
    assert!(prepare_params(
        &opts,
        json!({"project":"agent:runner:repo","action":"cli-start"})
    )
    .is_err());
}

#[test]
fn office_input_is_one_bounded_json_object_and_errors_do_not_echo_secrets() {
    assert!(read_input(&b"{}"[..]).is_ok());
    for input in [b"[]".as_slice(), b"{} {}".as_slice(), b"null".as_slice()] {
        assert!(read_input(input).is_err());
    }
    assert!(read_input(vec![b' '; INPUT_LIMIT + 1].as_slice()).is_err());
    let token = "wc_pat_private-test-token";
    assert_eq!(
        render_response(401, Some(json!({"secret":token})), token).unwrap_err(),
        UNAUTHORIZED
    );
    assert_eq!(
        render_response(
            200,
            Some(json!({"success":true,"output":{"secret":token},"error":null})),
            token
        )
        .unwrap_err(),
        INVALID_RESPONSE
    );
    let denied = json!({"success":false,"output":{"state_changed":false,"recovery":{"next":"observe"}},"error":"policy_changed"});
    let rendered = render_response(400, Some(denied.clone()), token).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&rendered).unwrap(), denied);
}

#[tokio::test]
async fn office_rejects_retargeting_and_non_pat_before_network_use() {
    let temp = tempfile::tempdir().unwrap();
    let opts = options(&temp, "wc_pat_synthetic");
    let mut retarget = opts.clone();
    retarget.server_url = "https://untrusted.example".into();
    assert_eq!(
        run(retarget, json!({"project":"agent:runner:repo"}))
            .await
            .unwrap_err(),
        CONNECTION_CHANGED
    );
    for token in [
        "bootstrap-secret",
        "wc_agent_runner",
        "wc_ar_model",
        "",
        "wc_pat_",
    ] {
        let opts = options(&temp, token);
        assert_eq!(
            run(opts, json!({"project":"agent:runner:repo"}))
                .await
                .unwrap_err(),
            UNAUTHORIZED
        );
    }
}

// The authenticated wire/CLI bridge is tested against an actual Salvo server
// in root office_http_tests, not a listener that manufactures success JSON.
