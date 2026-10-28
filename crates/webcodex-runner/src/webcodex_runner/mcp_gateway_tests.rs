use super::*;
use crate::mcp_gateway::{
    McpGatewayContent, McpGatewayResponsePayload, McpGatewaySchemaObservation,
};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tempfile::TempDir;

static FAKE_SERVER: OnceLock<Mutex<Weak<FakeBinary>>> = OnceLock::new();

struct FakeBinary {
    _temp: TempDir,
    path: PathBuf,
}

fn fake_binary() -> Arc<FakeBinary> {
    let cache = FAKE_SERVER.get_or_init(|| Mutex::new(Weak::new()));
    let mut cached = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(binary) = cached.upgrade() {
        return binary;
    }
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join(format!(
        "webcodex-mcp-bridge-fake{}",
        env::consts::EXE_SUFFIX
    ));
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/webcodex_runner/fake_mcp_gateway.rs");
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let result = Command::new(rustc)
        .env(
            "WEBCODEX_TEST_MCP_FRAME_LIMIT",
            crate::mcp_gateway::MCP_GATEWAY_MAX_MESSAGE_BYTES.to_string(),
        )
        .env(
            "WEBCODEX_TEST_MCP_TEXT_LIMIT",
            crate::mcp_gateway::MCP_GATEWAY_MAX_TEXT_CONTENT_BYTES.to_string(),
        )
        .arg("--edition=2021")
        .arg("--crate-name=webcodex_mcp_gateway_fake")
        .arg(source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let binary = Arc::new(FakeBinary {
        _temp: temp,
        path: output,
    });
    *cached = Arc::downgrade(&binary);
    binary
}

struct Fixture {
    manager: McpGatewayManager,
    marker: PathBuf,
    _fake: Arc<FakeBinary>,
    _temp: TempDir,
}

impl Fixture {
    fn new(scenario: &str, timeout_secs: u64) -> Self {
        Self::with_provider_timeout(scenario, timeout_secs, None)
    }

    fn with_provider_timeout(
        scenario: &str,
        default_timeout_secs: u64,
        provider_timeout_secs: Option<u64>,
    ) -> Self {
        Self::with_execution_context(
            scenario,
            default_timeout_secs,
            provider_timeout_secs,
            None,
            BTreeMap::new(),
        )
    }

    fn with_execution_context(
        scenario: &str,
        default_timeout_secs: u64,
        provider_timeout_secs: Option<u64>,
        cwd: Option<String>,
        env_from_env: BTreeMap<String, String>,
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("marker.log");
        let fake = fake_binary();
        let mut args = vec![scenario.to_string(), marker.to_string_lossy().to_string()];
        if let Some(cwd) = cwd.as_ref() {
            args.push(cwd.clone());
        }
        let manager = McpGatewayManager::new(&McpGatewayConfig {
            request_timeout_secs: default_timeout_secs,
            providers: vec![McpGatewayProviderConfig {
                id: "fake".to_string(),
                name: "Fake provider".to_string(),
                executable: fake.path.to_string_lossy().to_string(),
                args,
                cwd,
                env_from_env,
                url: None,
                headers: BTreeMap::new(),
                env: BTreeMap::new(),
                timeout_secs: provider_timeout_secs,
            }],
        });
        Self {
            manager,
            marker,
            _fake: fake,
            _temp: temp,
        }
    }

    fn provider(&self) -> McpGatewayProvider {
        self.manager
            .provider_inventory()
            .into_iter()
            .next()
            .unwrap()
    }

    fn list(&self, provider: &McpGatewayProvider) -> McpGatewayResponse {
        self.manager.handle(McpGatewayRequest::ToolsList {
            provider_id: provider.provider_id.clone(),
            provider_instance_id: provider.provider_instance_id.clone(),
        })
    }

    fn call(&self, provider: &McpGatewayProvider) -> McpGatewayResponse {
        self.manager.handle(McpGatewayRequest::ToolsCall {
            provider_id: provider.provider_id.clone(),
            provider_instance_id: provider.provider_instance_id.clone(),
            name: "echo".to_string(),
            arguments: json!({"value": "hello"}),
            expected_schema: McpGatewaySchemaObservation {
                input_schema: json!({
                    "type": "object",
                    "properties": {"value": {"type": "string"}}
                }),
                output_schema: None,
                annotations: None,
            },
        })
    }

    fn marker_count(&self, value: &str) -> usize {
        fs::read_to_string(&self.marker)
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == value)
            .count()
    }
}

#[test]
fn persistent_provider_initializes_once_and_serves_repeated_calls() {
    let fixture = Fixture::new("normal", 5);
    let provider = fixture.provider();
    let listed = fixture.list(&provider);
    let Some(McpGatewayResponsePayload::Tools { tools }) = listed.payload else {
        panic!("tools payload missing: {:?}", listed.error);
    };
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    for expected in ["call-1", "call-2"] {
        let response = fixture.call(&provider);
        let Some(McpGatewayResponsePayload::ToolResult { result }) = response.payload else {
            panic!("call payload missing: {:?}", response.error);
        };
        assert_eq!(
            result.content,
            vec![McpGatewayContent::Text {
                text: expected.to_string()
            }]
        );
    }
    assert_eq!(fixture.marker_count("start"), 1);
    assert_eq!(fixture.marker_count("initialize"), 1);
    assert_eq!(fixture.marker_count("initialized"), 1);
    assert_eq!(fixture.marker_count("call"), 2);
}

#[test]
fn schema_change_blocks_effectful_call_without_retiring_provider() {
    let fixture = Fixture::new("schema_change", 3);
    let provider = fixture.provider();
    let described = fixture.list(&provider);
    assert!(matches!(
        described.payload,
        Some(McpGatewayResponsePayload::Tools { .. })
    ));

    let response = fixture.call(&provider);
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(
        response.error.as_ref().map(|error| error.code.as_str()),
        Some("provider_schema_changed")
    );
    assert_eq!(fixture.marker_count("call"), 0);

    let listed_again = fixture.list(&provider);
    assert!(matches!(
        listed_again.payload,
        Some(McpGatewayResponsePayload::Tools { .. })
    ));
}

#[test]
fn tools_call_sends_only_gateway_owned_name_and_arguments() {
    let fixture = Fixture::new("meta_forbidden", 2);
    let provider = fixture.provider();
    let response = fixture.call(&provider);
    assert!(matches!(
        response.payload,
        Some(McpGatewayResponsePayload::ToolResult { .. })
    ));
    assert_eq!(fixture.marker_count("call"), 1);
}

#[test]
fn provider_execution_context_is_explicit_cleared_and_private() {
    let _guard = crate::tests::test_env_lock();
    let _env = crate::tests::EnvGuard::new()
        .set("GITHUB_TOKEN", "github-provider-secret-value")
        .set("WEBCODEX_MCP_MAPPED_SOURCE", "mapped-provider-secret-value")
        .set("WEBCODEX_MCP_UNLISTED", "must-not-reach-provider");
    let cwd = tempfile::tempdir().unwrap();
    let fixture = Fixture::with_execution_context(
        "execution_context",
        2,
        None,
        Some(cwd.path().to_string_lossy().into_owned()),
        BTreeMap::from([
            ("GITHUB_TOKEN".to_string(), "GITHUB_TOKEN".to_string()),
            (
                "WEBCODEX_MCP_MAPPED_CHILD".to_string(),
                "WEBCODEX_MCP_MAPPED_SOURCE".to_string(),
            ),
        ]),
    );
    let provider = fixture.provider();
    let inventory = serde_json::to_string(&fixture.manager.provider_inventory()).unwrap();
    assert!(!inventory.contains("github-provider-secret-value"));
    assert!(!inventory.contains("mapped-provider-secret-value"));

    let response = fixture.list(&provider);
    assert!(response.error.is_none(), "{:?}", response.error);
    for marker in [
        "github-env-ok",
        "mapped-env-ok",
        "unlisted-env-cleared",
        "path-cleared",
        "cwd-ok",
    ] {
        assert_eq!(fixture.marker_count(marker), 1, "missing marker {marker}");
    }
    let encoded = serde_json::to_string(&response).unwrap();
    assert!(!encoded.contains("github-provider-secret-value"));
    assert!(!encoded.contains("mapped-provider-secret-value"));
    assert!(!encoded.contains("must-not-reach-provider"));
}

#[test]
fn missing_mapped_source_fails_before_provider_spawn() {
    let _guard = crate::tests::test_env_lock();
    let _env = crate::tests::EnvGuard::new().remove("WEBCODEX_MCP_TEST_MISSING_SOURCE");
    let fixture = Fixture::with_execution_context(
        "normal",
        2,
        None,
        None,
        BTreeMap::from([(
            "PROVIDER_CREDENTIAL".to_string(),
            "WEBCODEX_MCP_TEST_MISSING_SOURCE".to_string(),
        )]),
    );
    let response = fixture.list(&fixture.provider());
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(
        response.error.as_ref().unwrap().code,
        "provider_env_missing"
    );
    assert_eq!(fixture.marker_count("start"), 0);
}

#[test]
fn sensitive_runner_env_mapping_is_blocked_before_provider_spawn() {
    let fixture = Fixture::with_execution_context(
        "normal",
        2,
        None,
        None,
        BTreeMap::from([(
            "PROVIDER_CREDENTIAL".to_string(),
            "WEBCODEX_AGENT_TOKEN".to_string(),
        )]),
    );
    let response = fixture.list(&fixture.provider());
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(
        response.error.as_ref().unwrap().code,
        "provider_env_forbidden"
    );
    assert_eq!(fixture.marker_count("start"), 0);
    assert!(!serde_json::to_string(&response)
        .unwrap()
        .contains("WEBCODEX_AGENT_TOKEN"));
}

#[test]
fn unavailable_provider_cwd_fails_before_provider_spawn() {
    let cwd_parent = tempfile::tempdir().unwrap();
    let missing = cwd_parent.path().join("missing-provider-cwd");
    let fixture = Fixture::with_execution_context(
        "normal",
        2,
        None,
        Some(missing.to_string_lossy().into_owned()),
        BTreeMap::new(),
    );
    let response = fixture.list(&fixture.provider());
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(
        response.error.as_ref().unwrap().code,
        "provider_cwd_unavailable"
    );
    assert_eq!(fixture.marker_count("start"), 0);
}

#[test]
fn provider_notifications_are_consumed_without_retiring_the_session() {
    let fixture = Fixture::new("notifications", 2);
    let provider = fixture.provider();

    let listed = fixture.list(&provider);
    assert!(listed.error.is_none(), "{:?}", listed.error);
    // Let the post-response logging notification reach the bounded reader queue
    // so the next operation exercises the pre-dispatch drain path as well.
    std::thread::sleep(Duration::from_millis(25));

    let called = fixture.call(&provider);
    let Some(McpGatewayResponsePayload::ToolResult { result }) = called.payload else {
        panic!("call payload missing: {:?}", called.error);
    };
    assert_eq!(
        result.content,
        vec![McpGatewayContent::Text {
            text: "call-1".to_string()
        }]
    );
    assert!(fixture.list(&provider).error.is_none());
    assert_eq!(fixture.marker_count("start"), 1);
    assert_eq!(fixture.marker_count("initialize"), 1);
}

#[test]
fn provider_callback_after_dispatch_remains_unsupported_and_unknown() {
    let fixture = Fixture::new("callback", 2);
    let provider = fixture.provider();
    assert!(fixture.list(&provider).error.is_none());

    let response = fixture.call(&provider);
    assert_eq!(
        response.dispatch_state,
        McpGatewayDispatchState::OutcomeUnknown
    );
    assert_eq!(
        response.error.as_ref().unwrap().code,
        "provider_callbacks_unsupported"
    );
    assert_eq!(
        fixture.call(&provider).error.as_ref().unwrap().code,
        "provider_callbacks_unsupported"
    );
    assert_eq!(fixture.marker_count("start"), 2);
}

#[test]
fn provider_notification_flood_is_bounded_and_retires_after_dispatch() {
    let fixture = Fixture::new("notification_flood", 2);
    let provider = fixture.provider();

    let response = fixture.list(&provider);
    assert_eq!(
        response.dispatch_state,
        McpGatewayDispatchState::OutcomeUnknown
    );
    assert_eq!(
        response.error.as_ref().unwrap().code,
        "provider_notification_flood"
    );
    assert_eq!(
        fixture.list(&provider).error.as_ref().unwrap().code,
        "provider_notification_flood"
    );
    assert_eq!(fixture.marker_count("start"), 2);
}

#[test]
fn crash_is_not_replayed_and_a_later_request_starts_a_new_session() {
    let fixture = Fixture::new("crash", 2);
    let provider = fixture.provider();
    assert!(fixture.list(&provider).error.is_none());
    let first = fixture.call(&provider);
    assert_eq!(
        first.dispatch_state,
        McpGatewayDispatchState::OutcomeUnknown
    );
    assert_eq!(first.error.as_ref().unwrap().code, "provider_eof");

    assert_eq!(fixture.marker_count("call"), 1);
    let second = fixture.call(&provider);
    assert_eq!(
        second.dispatch_state,
        McpGatewayDispatchState::OutcomeUnknown
    );
    assert_eq!(second.error.as_ref().unwrap().code, "provider_eof");
    assert_eq!(fixture.marker_count("start"), 2);
    assert_eq!(fixture.marker_count("call"), 2);
}

#[test]
fn initialization_failure_is_not_misreported_as_tool_dispatch() {
    for (scenario, code) in [
        ("init_crash", "provider_eof"),
        ("init_timeout", "provider_timeout"),
        ("init_missing_tools", "provider_initialize_invalid"),
    ] {
        let fixture = Fixture::new(scenario, 1);
        let provider = fixture.provider();
        let response = fixture.call(&provider);
        assert_eq!(
            response.dispatch_state,
            McpGatewayDispatchState::NotStarted,
            "{scenario}"
        );
        assert_eq!(response.error.as_ref().unwrap().code, code, "{scenario}");
        assert_eq!(fixture.marker_count("call"), 0, "{scenario}");
        assert_eq!(fixture.marker_count("start"), 1, "{scenario}");
        assert_eq!(
            fixture.call(&provider).error.as_ref().unwrap().code,
            code,
            "{scenario}"
        );
        assert_eq!(fixture.marker_count("start"), 2, "{scenario}");
    }
}

#[test]
fn malformed_unknown_and_duplicate_responses_fail_closed() {
    for (scenario, code) in [
        ("malformed", "provider_malformed_json"),
        ("unknown_id", "provider_unknown_response_id"),
    ] {
        let fixture = Fixture::new(scenario, 2);
        let response = fixture.list(&fixture.provider());
        assert_eq!(response.error.as_ref().unwrap().code, code, "{scenario}");
        assert_eq!(
            response.dispatch_state,
            McpGatewayDispatchState::OutcomeUnknown
        );
    }

    let fixture = Fixture::new("duplicate_id", 2);
    let provider = fixture.provider();
    assert!(fixture.list(&provider).error.is_none());
    let response = fixture.call(&provider);
    assert_eq!(
        response.error.as_ref().unwrap().code,
        "provider_duplicate_response_id"
    );
}

#[test]
fn timeout_and_invalid_untrusted_outputs_are_bounded() {
    let timeout = Fixture::new("timeout", 1);
    let provider = timeout.provider();
    assert!(timeout.list(&provider).error.is_none());
    let response = timeout.call(&provider);
    assert_eq!(response.error.as_ref().unwrap().code, "provider_timeout");
    assert_eq!(
        response.dispatch_state,
        McpGatewayDispatchState::OutcomeUnknown
    );

    for (scenario, operation, code, state) in [
        (
            "bad_tools",
            "list",
            "invalid_provider_tools",
            McpGatewayDispatchState::Completed,
        ),
        (
            "oversized_message",
            "list",
            "provider_message_too_large",
            McpGatewayDispatchState::OutcomeUnknown,
        ),
        (
            "bad_result",
            "call",
            "unsupported_provider_content",
            McpGatewayDispatchState::Completed,
        ),
        (
            "oversized_result",
            "call",
            "invalid_provider_result",
            McpGatewayDispatchState::Completed,
        ),
        (
            "paginated_list",
            "list",
            "invalid_provider_cursor",
            McpGatewayDispatchState::Completed,
        ),
    ] {
        let fixture = Fixture::new(scenario, 2);
        let provider = fixture.provider();
        let response = if operation == "list" {
            fixture.list(&provider)
        } else {
            assert!(fixture.list(&provider).error.is_none());
            fixture.call(&provider)
        };
        assert_eq!(response.error.as_ref().unwrap().code, code, "{scenario}");
        assert_eq!(response.dispatch_state, state, "{scenario}");
    }
}

#[test]
fn correlated_invalid_result_does_not_retire_provider_instance() {
    let fixture = Fixture::new("bad_result", 2);
    let provider = fixture.provider();
    assert!(fixture.list(&provider).error.is_none());

    let response = fixture.call(&provider);
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::Completed);
    assert_eq!(
        response.error.as_ref().unwrap().code,
        "unsupported_provider_content"
    );

    // The response was correlated, so the stdio session is still synchronized.
    // A V1 result-format rejection must not turn the configured provider into a
    // permanently stale instance or force a hidden respawn.
    assert!(fixture.list(&provider).error.is_none());
    assert_eq!(fixture.marker_count("start"), 1);
    assert_eq!(fixture.marker_count("initialize"), 1);
}

#[test]
fn provider_timeout_override_and_default_fallback_are_enforced() {
    let inherited = Fixture::new("slow", 2);
    let inherited_provider = inherited.provider();
    assert!(inherited.list(&inherited_provider).error.is_none());
    assert!(inherited.call(&inherited_provider).error.is_none());

    let overridden = Fixture::with_provider_timeout("slow", 2, Some(1));
    let overridden_provider = overridden.provider();
    assert!(overridden.list(&overridden_provider).error.is_none());
    let response = overridden.call(&overridden_provider);
    assert_eq!(
        response.dispatch_state,
        McpGatewayDispatchState::OutcomeUnknown
    );
    assert_eq!(response.error.as_ref().unwrap().code, "provider_timeout");
}

#[test]
fn correlated_jsonrpc_error_is_completed_without_retry() {
    let fixture = Fixture::new("rpc_error", 2);
    let provider = fixture.provider();
    assert!(fixture.list(&provider).error.is_none());
    let response = fixture.call(&provider);
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::Completed);
    assert_eq!(response.error.as_ref().unwrap().code, "provider_rpc_error");
    assert_eq!(fixture.marker_count("call"), 1);
}

#[test]
fn stale_provider_instance_fails_without_starting_child() {
    let fixture = Fixture::new("normal", 2);
    let mut provider = fixture.provider();
    provider.provider_instance_id = "stale".to_string();
    let response = fixture.list(&provider);
    assert_eq!(response.dispatch_state, McpGatewayDispatchState::NotStarted);
    assert_eq!(response.error.as_ref().unwrap().code, "stale_provider");
    assert_eq!(fixture.marker_count("start"), 0);
}

#[test]
fn prepare_reuses_identical_config_and_failed_replacement_keeps_old_provider() {
    let fixture = Fixture::new("normal", 2);
    let status = fixture.manager.connection_status();
    assert_eq!(status[0].state, "configured");
    assert_eq!(status[0].tool_count, None);
    let old = fixture.provider();
    let config = fixture.manager.config("fake").unwrap();
    let prepared = fixture.manager.prepare(&config).unwrap();
    assert_eq!(prepared.inventory(), old);
    assert_eq!(prepared.tool_count(), 1);
    fixture.manager.commit(prepared);
    let status = fixture.manager.connection_status();
    assert_eq!(status[0].state, "connected");
    assert_eq!(status[0].tool_count, Some(1));
    let prepared = fixture.manager.prepare(&config).unwrap();
    fixture.manager.commit(prepared);
    assert_eq!(fixture.marker_count("start"), 1);
    let mut replacement = config.clone();
    replacement.executable = fixture
        ._temp
        .path()
        .join("missing-provider.exe")
        .to_string_lossy()
        .into_owned();
    assert!(fixture.manager.prepare(&replacement).is_err());
    assert_eq!(fixture.manager.config("fake"), Some(config));
    assert_eq!(fixture.manager.connection_status()[0].state, "connected");
    assert!(fixture.call(&old).error.is_none());
    let prepared = fixture
        .manager
        .prepare_reconnect(&fixture.manager.config("fake").unwrap())
        .unwrap();
    let new = fixture.manager.commit(prepared);
    assert_ne!(old.provider_instance_id, new.provider_instance_id);
    assert_eq!(fixture.call(&old).error.unwrap().code, "stale_provider");
    assert!(fixture.call(&new).error.is_none());
    assert!(fixture.manager.remove("fake"));
    assert_eq!(fixture.call(&old).error.unwrap().code, "stale_provider");
}

#[test]
fn remove_allows_in_flight_call_to_complete() {
    let fixture = Fixture::new("slow", 5);
    let provider = fixture.provider();
    assert!(fixture.list(&provider).error.is_none());
    std::thread::scope(|scope| {
        let running = scope.spawn(|| fixture.call(&provider));
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.marker_count("call") == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(fixture.manager.remove("fake"));
        assert_eq!(
            fixture.call(&provider).error.unwrap().code,
            "stale_provider"
        );
        assert!(running.join().unwrap().error.is_none());
    });
    assert_eq!(fixture.marker_count("call"), 1);
}

#[test]
fn replacement_fences_new_calls_but_preserves_old_in_flight_call() {
    let fixture = Fixture::new("slow", 5);
    let old = fixture.provider();
    assert!(fixture.list(&old).error.is_none());
    std::thread::scope(|scope| {
        let running = scope.spawn(|| fixture.call(&old));
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.marker_count("call") == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut replacement = fixture.manager.config("fake").unwrap();
        replacement.args[0] = "normal".into();
        let prepared = fixture.manager.prepare(&replacement).unwrap();
        let new = fixture.manager.commit(prepared);
        assert_ne!(old.provider_instance_id, new.provider_instance_id);
        assert_eq!(fixture.call(&old).error.unwrap().code, "stale_provider");
        assert!(fixture.call(&new).error.is_none());
        assert!(running.join().unwrap().error.is_none());
    });
    assert_eq!(fixture.marker_count("call"), 2);
}
