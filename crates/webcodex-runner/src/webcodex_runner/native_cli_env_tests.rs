use super::*;
use std::ffi::OsStr;

fn agy_args() -> Vec<String> {
    vec!["--help".into()]
}

#[test]
fn native_cli_env_uses_system_proxy_for_any_cli_without_mutating_environment() {
    let environment = HashMap::from([("NO_PROXY".into(), "localhost".into())]);
    let before = environment.clone();
    assert_eq!(
        environment_overrides(
            &environment,
            Some("http=127.0.0.1:7890;https=127.0.0.1:7891")
        ),
        vec![
            ("HTTP_PROXY".into(), "http://127.0.0.1:7891".into()),
            ("HTTPS_PROXY".into(), "http://127.0.0.1:7891".into())
        ]
    );
    assert_eq!(environment, before);
    for invalid in [
        "http://user:secret@127.0.0.1:7890",
        "socks5://127.0.0.1:7890",
        "http://proxy.test/path",
        "http://proxy.test?secret=x",
        "http://proxy.test#x",
        "proxy\nvalue",
        "",
    ] {
        assert!(environment_overrides(&environment, Some(invalid)).is_empty());
    }
}

#[test]
fn native_cli_env_explicit_proxy_including_empty_or_mixed_case_always_wins() {
    for name in ["HTTP_PROXY", "https_proxy", "aLl_PrOxY"] {
        for value in ["", "http://explicit.test:3128"] {
            assert!(environment_overrides(
                &HashMap::from([(name.into(), value.into())]),
                Some("http://system.test:3128")
            )
            .is_empty());
        }
    }
}
#[cfg(windows)]
#[test]
fn native_cli_env_explicit_and_isolated_environments_do_not_fallback() {
    use super::super::{
        native_cli_environment, resolve_process_program, PreparedShellProfile, ShellConfig,
        ShellDialect, ShellEnvironmentMode,
    };
    let temp = tempfile::tempdir().unwrap();
    let empty = temp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let path = empty.to_string_lossy().into_owned();
    let explicit = ShellConfig {
        env: HashMap::from([("PATH".into(), path.clone())]),
        ..Default::default()
    };
    assert!(
        resolve_process_program(&explicit, None, "agy", &agy_args(), Some(temp.path())).is_err()
    );
    let isolated = ShellConfig {
        environment_mode: ShellEnvironmentMode::Isolated,
        ..Default::default()
    };
    assert!(
        native_cli_environment(&isolated, None, "agy", &agy_args(), OsStr::new("agy.exe"))
            .unwrap()
            .is_empty()
    );
    let profile = PreparedShellProfile {
        profile_name: "explicit".into(),
        program: "unused".into(),
        args: vec![],
        dialect: ShellDialect::PowerShell,
        env_snapshot: HashMap::from([("PATH".into(), path)]),
    };
    assert!(resolve_process_program(
        &ShellConfig::default(),
        Some(&profile),
        "agy",
        &agy_args(),
        Some(temp.path())
    )
    .is_err());
}

#[cfg(windows)]
#[test]
fn native_cli_env_detached_preparation_preserves_policy_and_working_directory_guards() {
    use super::super::{
        prepare_detached_process_launch, PreparedShellProfileCache, RunnerPolicy, ShellConfig,
    };
    let temp = tempfile::tempdir().unwrap();
    let cache = PreparedShellProfileCache::default();
    let denied = RunnerPolicy {
        allow_raw_shell: false,
        ..Default::default()
    };
    let result = prepare_detached_process_launch(
        1,
        &denied,
        &ShellConfig::default(),
        temp.path(),
        &cache,
        Some(&temp.path().to_string_lossy()),
        "agy",
        &agy_args(),
        900,
        None,
    );
    assert!(result
        .unwrap_err()
        .contains("disabled by local Runner policy"));
    let result = prepare_detached_process_launch(
        1,
        &RunnerPolicy::default(),
        &ShellConfig::default(),
        temp.path(),
        &cache,
        Some(&temp.path().to_string_lossy()),
        "agy",
        &agy_args(),
        900,
        None,
    );
    assert!(
        result.is_err(),
        "the native fallback must not broaden allowed cwd roots"
    );
}
