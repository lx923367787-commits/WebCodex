use super::*;

#[test]
fn only_the_two_supported_providers_can_select_an_executable() {
    assert_eq!(Provider::parse("workbuddy"), Ok(Provider::WorkBuddy));
    assert_eq!(Provider::parse("antigravity"), Ok(Provider::Antigravity));
    for invalid in [
        "",
        "powershell",
        "../../agy",
        "agy.exe",
        "workbuddy --print",
    ] {
        assert!(Provider::parse(invalid).is_err());
    }
}

#[test]
fn workbuddy_models_are_scoped_to_the_exact_model_option() {
    let help = "  --fallback-model <model>  Currently supported: (wrong-model)\n\
                --model <model> Model ID. Currently supported: (hy4-preview, hy3, hy4-preview)\n\
                --effort <level> Reasoning effort level (minimal, low, medium, high, xhigh, max)";
    assert_eq!(parse_workbuddy_models(help), ["hy4-preview", "hy3"]);
    assert_eq!(
        parse_efforts(help),
        ["minimal", "low", "medium", "high", "xhigh", "max"]
    );
    assert!(parse_workbuddy_models("Use --model for model selection").is_empty());
    assert!(parse_workbuddy_models("--models Currently supported: (wrong-model)").is_empty());
    assert!(parse_workbuddy_models("--model Currently supported: (model-a; command)").is_empty());
    assert!(parse_workbuddy_models("--model Currently supported: (model-a, )").is_empty());
}

#[test]
fn antigravity_effort_is_read_from_its_actual_help_syntax() {
    assert_eq!(
        parse_efforts("  --effort  Reasoning effort (low|medium|high|xhigh|max)"),
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(parse_efforts("No effort option available").is_empty());
}

#[test]
fn antigravity_catalog_accepts_documented_rows_without_inventing_models() {
    // Synthetic fixtures use the documented slug/display-label format;
    // these model names are never shipped as a fallback catalog.
    let output = "Available models:\n\
                  test-model-a    Test Model A (High)\n\
                  test-model-b    Test Model B\n\
                  test-model-a    Test Model A (High)\n";
    assert_eq!(
        parse_antigravity_models(output),
        ["test-model-a", "test-model-b"]
    );
    assert_eq!(parse_antigravity_models("test-model-c\n"), ["test-model-c"]);
    assert!(parse_antigravity_models("").is_empty());
    assert!(parse_antigravity_models("Available models:\n").is_empty());
}

#[test]
fn diagnostic_prose_and_partial_catalogs_never_become_model_choices() {
    for invalid in [
        "Error: authentication required",
        "Please log in to list models",
        "https://example.test/login",
        "test-model-a    Test Model A\nWarning: sign in required",
        "test-model-a    missing credentials",
        "test-model-a    Test Model A\nLoading",
        "\u{1b}[31mtest-model-a\u{1b}[0m",
        "test-model-a;whoami",
    ] {
        assert!(parse_antigravity_models(invalid).is_empty(), "{invalid:?}");
    }
}

#[test]
fn catalog_limits_bound_entries_and_identifier_size() {
    let many = (0..257)
        .map(|index| format!("test-model-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parse_antigravity_models(&many).is_empty());
    assert!(parse_antigravity_models(&format!("model-{}", "x".repeat(160))).is_empty());
}

#[test]
fn query_output_is_bounded_without_silently_truncating_the_catalog() {
    assert_eq!(read_bounded(&b"models"[..]).unwrap(), b"models");
    assert!(read_bounded(&vec![b'x'; OUTPUT_LIMIT][..]).is_ok());
    assert!(read_bounded(&vec![b'x'; OUTPUT_LIMIT + 1][..]).is_err());
}

#[test]
fn version_parser_does_not_return_diagnostic_text() {
    assert_eq!(parse_version("2.162.0\n").as_deref(), Some("2.162.0"));
    assert_eq!(parse_version("agy v1.4.5\n").as_deref(), Some("1.4.5"));
    assert_eq!(parse_version("failed to authenticate"), None);
}

#[test]
fn progress_on_stderr_does_not_corrupt_successful_model_rows() {
    let output = QueryOutput {
        stdout: "test-model-a    Test Model A\n".into(),
        stderr: "Fetching available models...\n".into(),
    };
    assert_eq!(parse_antigravity_models(&output.stdout), ["test-model-a"]);
    assert_eq!(output.primary_text(), output.stdout);
    assert_eq!(
        query_failure("proxyconnect tcp: secret diagnostic"),
        "CLI 无法连接已配置的代理，请检查代理服务后刷新。"
    );
    assert!(!query_failure("GEMINI_API_KEY is not set; secret diagnostic").contains("secret"));
}

fn test_executable() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:/trusted-cli/agy.exe")
    } else {
        PathBuf::from("/trusted-cli/agy")
    }
}
fn test_environment() -> BTreeMap<String, OsString> {
    [
        ("SystemRoot", "C:/Windows"),
        ("USERPROFILE", "C:/official-user"),
        ("LOCALAPPDATA", "C:/official-user/AppData/Local"),
        ("APPDATA", "C:/official-user/AppData/Roaming"),
        ("HOME", "/official-user"),
        ("PATH", "C:/untrusted-project/bin"),
        ("WEBCODEX_TOKEN", "management-secret"),
        ("CODEX_HOME", "C:/private-codex-state"),
        ("OPENAI_API_KEY", "other-agent-secret"),
        ("UNRELATED_SECRET", "not-a-native-login-fact"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect()
}
fn context_from(
    provider: Provider,
    environment: &BTreeMap<String, OsString>,
    proxy: Option<&str>,
) -> Result<TrustedMetadataContext, String> {
    TrustedMetadataContext::from_environment(provider, &test_executable(), proxy, |key| {
        environment.get(key).cloned()
    })
}

#[test]
fn metadata_context_keeps_only_official_login_locations_and_rebuilds_path() {
    let environment = test_environment();
    let context = context_from(Provider::Antigravity, &environment, None).unwrap();
    for forbidden in [
        "WEBCODEX_TOKEN",
        "CODEX_HOME",
        "OPENAI_API_KEY",
        "UNRELATED_SECRET",
    ] {
        assert!(!context
            .environment
            .contains_key(std::ffi::OsStr::new(forbidden)));
    }
    assert_ne!(
        context.environment.get(std::ffi::OsStr::new("PATH")),
        environment.get("PATH")
    );
    #[cfg(windows)]
    assert_eq!(
        context.environment.get(std::ffi::OsStr::new("USERPROFILE")),
        environment.get("USERPROFILE")
    );
    #[cfg(not(windows))]
    assert_eq!(
        context.environment.get(std::ffi::OsStr::new("HOME")),
        environment.get("HOME")
    );
    let mut command = Command::new(test_executable());
    command.env("UNRELATED_SECRET", "should-be-cleared");
    let directory = if cfg!(windows) {
        Path::new("C:/isolated-query-temp")
    } else {
        Path::new("/isolated-query-temp")
    };
    context.apply(&mut command, directory);
    assert_eq!(command.get_current_dir(), Some(directory));
    assert!(!command
        .get_envs()
        .any(|(name, _)| name == "UNRELATED_SECRET"));
    assert!(command
        .get_envs()
        .any(|(name, value)| name == "TEMP" && value == Some(directory.as_os_str())));
    assert!(command
        .get_envs()
        .any(|(name, value)| name == "AGY_CLI_DISABLE_AUTO_UPDATE"
            && value == Some(std::ffi::OsStr::new("true"))));
    assert!(!format!("{context:?}").contains("official-user"));
}

#[test]
fn every_inherited_proxy_rejects_userinfo_query_fragment_and_embedded_secret_paths() {
    for key in PROXY_KEYS {
        for proxy in [
            "http://user:secret@127.0.0.1:8080",
            "http://%75ser@127.0.0.1:8080",
            "http://127.0.0.1:8080?token=secret",
            "http://127.0.0.1:8080#secret",
            "http://127.0.0.1:8080/private-secret",
            "http://127.0.0.1:8080\n",
        ] {
            let mut environment = test_environment();
            environment.insert(key.into(), proxy.into());
            let error = context_from(Provider::Antigravity, &environment, None).unwrap_err();
            assert_eq!(error, PROXY_UNSUPPORTED);
            assert!(!error.contains("secret"));
            assert!(!error.contains(proxy));
        }
    }
}

#[test]
fn no_proxy_is_only_a_bounded_host_list() {
    for value in [
        "",
        "localhost,127.0.0.1,.example.test",
        "[::1],10.0.0.0/8",
        "*",
    ] {
        assert!(validate_no_proxy(value).is_ok(), "{value}");
    }
    for value in [
        "user:secret@host",
        "https://host",
        "host?token=secret",
        "host;command",
        "host\n",
    ] {
        assert!(validate_no_proxy(value).is_err(), "{value}");
    }
    assert!(validate_no_proxy(&"x".repeat(4097)).is_err());
    assert!(validate_no_proxy(&vec!["host"; 129].join(",")).is_err());
}

#[test]
fn system_proxy_preserves_explicit_proxy_and_only_applies_to_antigravity() {
    let environment = test_environment();
    let proxy = "http://127.0.0.1:20081";
    let agy = context_from(Provider::Antigravity, &environment, Some(proxy)).unwrap();
    assert!(agy.system_proxy_used);
    assert_eq!(
        agy.environment.get(std::ffi::OsStr::new("HTTPS_PROXY")),
        Some(&OsString::from(proxy))
    );
    let buddy = context_from(Provider::WorkBuddy, &environment, Some(proxy)).unwrap();
    assert!(!buddy.system_proxy_used);
    assert!(!buddy
        .environment
        .contains_key(std::ffi::OsStr::new("AGY_CLI_DISABLE_AUTO_UPDATE")));
    let mut explicit = environment.clone();
    explicit.insert("HTTP_PROXY".into(), "http://127.0.0.1:3128".into());
    explicit.insert("NO_PROXY".into(), "localhost".into());
    let context = context_from(Provider::Antigravity, &explicit, Some(proxy)).unwrap();
    assert!(!context.system_proxy_used);
    assert_eq!(
        context.environment.get(std::ffi::OsStr::new("HTTP_PROXY")),
        explicit.get("HTTP_PROXY")
    );
    assert_eq!(
        context.environment.get(std::ffi::OsStr::new("NO_PROXY")),
        explicit.get("NO_PROXY")
    );
    explicit.insert("HTTP_PROXY".into(), OsString::new());
    assert!(
        !context_from(Provider::Antigravity, &explicit, Some(proxy))
            .unwrap()
            .system_proxy_used
    );
    assert!(context_from(
        Provider::Antigravity,
        &environment,
        Some("http://user:secret@host")
    )
    .is_err());
}

#[test]
fn context_rejects_non_native_or_relative_programs_before_probe() {
    let environment = test_environment();
    for program in [Path::new("agy.exe"), Path::new("../agy.exe")] {
        assert!(TrustedMetadataContext::from_environment(
            Provider::Antigravity,
            program,
            None,
            |key| environment.get(key).cloned()
        )
        .is_err());
    }
    #[cfg(windows)]
    assert!(TrustedMetadataContext::from_environment(
        Provider::Antigravity,
        Path::new("C:/trusted/agy.cmd"),
        None,
        |key| environment.get(key).cloned()
    )
    .is_err());
    assert!(!CliCatalog::unavailable(Provider::Antigravity).account_availability_verified);
}

#[test]
fn metadata_provider_names_remain_the_native_installed_cli_names() {
    assert_eq!(
        Provider::Antigravity.executable(),
        if cfg!(windows) { "agy.exe" } else { "agy" }
    );
    assert_eq!(
        Provider::WorkBuddy.executable(),
        if cfg!(windows) {
            "codebuddy.exe"
        } else {
            "codebuddy"
        }
    );
    let context = context_from(Provider::Antigravity, &test_environment(), None).unwrap();
    assert_eq!(
        context.environment.get(std::ffi::OsStr::new("NO_COLOR")),
        Some(&OsString::from("1"))
    );
}
