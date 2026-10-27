use super::super::support::*;

#[tokio::test]
async fn legacy_pat_tunnel_configuration_reports_migration_before_launch() {
    let _guard = env_test_guard();
    let _env = EnvGuard::new().set("WEBCODEX_ADDR", "127.0.0.1:48001");
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join("server.env");
    let token_file = tmp.path().join("legacy-pat");
    std::fs::write(
        &env_file,
        "WEBCODEX_ADDR=127.0.0.1:48001\nWEBCODEX_TOKEN=synthetic-bootstrap\n",
    )
    .unwrap();
    std::fs::write(&token_file, "wc_pat_synthetic_legacy").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let error = crate::webcodex_cli::server::run_server_tunnel(ServerTunnelOptions {
        env_file,
        mcp_token_file: Some(token_file),
    })
    .await
    .unwrap_err();
    assert!(error.contains("Bootstrap provisioning"), "{error}");
    assert!(error.contains("--mcp-token-file"));
    assert!(!error.contains("wc_pat_synthetic_legacy"));
    assert!(!error.contains("synthetic-bootstrap"));
}

#[test]
fn server_tunnel_parser_is_machine_owned_and_openai_only() {
    let parsed = parse_server_tunnel(&args(&[
        "--provider",
        "openai",
        "--env-file",
        "local.env",
        "--json",
        "--stop-on-stdin-eof",
    ]))
    .unwrap();
    assert_eq!(parsed.env_file, PathBuf::from("local.env"));
    assert!(parsed.mcp_token_file.is_none());

    let with_token = parse_server_tunnel(&args(&[
        "--provider",
        "openai",
        "--env-file",
        "local.env",
        "--mcp-token-file",
        "private-token",
        "--json",
        "--stop-on-stdin-eof",
    ]))
    .unwrap();
    assert_eq!(
        with_token.mcp_token_file,
        Some(PathBuf::from("private-token"))
    );

    assert!(parse_server_tunnel(&args(&[
        "--provider",
        "cloudflare",
        "--env-file",
        "local.env",
        "--json",
        "--stop-on-stdin-eof",
    ]))
    .unwrap_err()
    .contains("openai"));
    assert!(
        parse_server_tunnel(&args(&["--provider", "openai", "--env-file", "local.env",])).is_err()
    );
}

#[test]
fn regular_tunnel_mcp_token_file_is_explicit_and_never_falls_back_on_failure() {
    let _guard = env_test_guard();
    let _env = EnvGuard::new().set("WEBCODEX_TOKEN", "process-bootstrap");
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join("webcodex.env");
    let configured = tmp.path().join("configured-token");
    let explicit = tmp.path().join("explicit-token");
    std::fs::write(&configured, "wc_pat_configured_test\n").unwrap();
    std::fs::write(&explicit, "wc_pat_explicit_test\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&configured, &explicit] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    std::fs::write(
        &env_file,
        format!(
            "WEBCODEX_TOKEN=file-bootstrap\nWEBCODEX_TUNNEL_MCP_TOKEN_FILE={}\n",
            configured.display(),
        ),
    )
    .unwrap();
    let derive = crate::webcodex_cli::server::derive_regular_tunnel_mcp_token;
    assert_eq!(derive(&env_file, None).unwrap(), "wc_pat_configured_test");
    assert_eq!(
        derive(&env_file, Some(&explicit)).unwrap(),
        "wc_pat_explicit_test"
    );

    for invalid in [
        "",
        "wc_agent_private_test",
        "wc_pat_one\nInjected: value",
        "bootstrap-test",
    ] {
        std::fs::write(&explicit, invalid).unwrap();
        let error = derive(&env_file, Some(&explicit)).unwrap_err();
        if !invalid.is_empty() {
            assert!(!error.contains(invalid));
        }
    }
    std::fs::write(&explicit, "x".repeat(8193)).unwrap();
    assert!(derive(&env_file, Some(&explicit))
        .unwrap_err()
        .contains("8192"));
    assert!(derive(&env_file, Some(&tmp.path().join("missing"))).is_err());
    assert!(derive(&env_file, Some(Path::new("relative-token"))).is_err());
    #[cfg(windows)]
    for network in [
        r"\\server\share\token",
        r"\\?\UNC\server\share\token",
        r"\\.\pipe\token",
    ] {
        assert!(derive(&env_file, Some(Path::new(network)))
            .unwrap_err()
            .contains("local drive"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&explicit, "wc_pat_private_test").unwrap();
        std::fs::set_permissions(&explicit, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(derive(&env_file, Some(&explicit))
            .unwrap_err()
            .contains("private"));
    }
    std::fs::write(&env_file, "WEBCODEX_TOKEN=file-bootstrap\n").unwrap();
    assert_eq!(derive(&env_file, None).unwrap(), "process-bootstrap");
}

#[test]
fn regular_tunnel_bootstrap_token_follows_server_env_precedence() {
    let _guard = env_test_guard();
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join("webcodex.env");
    std::fs::write(&env_file, "WEBCODEX_TOKEN=file-bootstrap\n").unwrap();

    let _env = EnvGuard::new().remove("WEBCODEX_TOKEN");
    assert_eq!(
        crate::webcodex_cli::server::derive_regular_tunnel_bootstrap_token(&env_file).unwrap(),
        "file-bootstrap"
    );
    drop(_env);

    let _env = EnvGuard::new().set("WEBCODEX_TOKEN", "process-bootstrap");
    assert_eq!(
        crate::webcodex_cli::server::derive_regular_tunnel_bootstrap_token(&env_file).unwrap(),
        "process-bootstrap"
    );
}

#[test]
fn regular_tunnel_server_url_is_derived_from_loopback_env_only() {
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join("webcodex.env");
    std::fs::write(&env_file, "WEBCODEX_ADDR=0.0.0.0:18080\n").unwrap();
    assert_eq!(
        crate::webcodex_cli::server::derive_regular_tunnel_server_url(&env_file).unwrap(),
        "http://127.0.0.1:18080"
    );

    std::fs::write(&env_file, "WEBCODEX_ADDR=192.0.2.10:18080\n").unwrap();
    assert!(crate::webcodex_cli::server::derive_regular_tunnel_server_url(&env_file).is_err());
}
