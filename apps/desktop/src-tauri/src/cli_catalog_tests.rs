use super::*;

#[tokio::test]
async fn invalid_provider_is_rejected_before_any_process_is_started() {
    assert!(refresh_ai_office_cli("cmd.exe /c whoami".into())
        .await
        .is_err());
}

#[test]
fn desktop_catalog_preserves_the_shared_wire_contract() {
    let value = serde_json::to_value(CliCatalog::unavailable(Provider::WorkBuddy)).unwrap();
    assert_eq!(value["providerId"], "workbuddy");
    assert_eq!(value["status"], "unavailable");
    assert_eq!(value["source"], "unavailable");
    assert_eq!(value["accountAvailabilityVerified"], false);
    assert!(value["checkedAt"].is_string());
    assert!(value.get("provider_id").is_none());
}

#[test]
fn disabled_system_proxy_is_ignored_but_credentialed_proxy_is_explicitly_unsupported() {
    let enabled = platform::SystemProxyCandidate {
        url: "http://127.0.0.1:20081".into(),
        enabled: true,
    };
    for provider in [Provider::Codex, Provider::Claude] {
        assert_eq!(
            selected_system_proxy(provider, false, Some(enabled.clone()))
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:20081")
        );
    }
    assert_eq!(
        selected_system_proxy(Provider::Antigravity, false, Some(enabled.clone()))
            .unwrap()
            .as_deref(),
        Some("http://127.0.0.1:20081")
    );
    assert!(
        selected_system_proxy(Provider::Antigravity, true, Some(enabled.clone()))
            .unwrap()
            .is_none()
    );
    assert!(
        selected_system_proxy(Provider::WorkBuddy, false, Some(enabled.clone()))
            .unwrap()
            .is_none()
    );
    assert!(selected_system_proxy(
        Provider::Antigravity,
        false,
        Some(platform::SystemProxyCandidate {
            enabled: false,
            ..enabled
        })
    )
    .unwrap()
    .is_none());
    let error = selected_system_proxy(
        Provider::Antigravity,
        false,
        Some(platform::SystemProxyCandidate {
            url: "http://user:secret@127.0.0.1:20081".into(),
            enabled: true,
        }),
    )
    .unwrap_err();
    assert!(!error.contains("secret"));
    assert!(error.contains("不支持"));
}
