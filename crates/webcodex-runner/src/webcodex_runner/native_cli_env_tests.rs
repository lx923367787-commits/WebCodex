use super::*;

fn workbuddy_args() -> Vec<String> {
    [
        "--model",
        "selected-model",
        "--effort",
        "high",
        "--print",
        "--input-format",
        "text",
        "--output-format",
        "json",
    ]
    .map(str::to_owned)
    .to_vec()
}
fn agy_args() -> Vec<String> {
    [
        "--model",
        "selected-model",
        "--effort",
        "high",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
    ]
    .map(str::to_owned)
    .to_vec()
}
fn install(root: &Path, cli: NativeCli) -> PathBuf {
    let directory = root.join(cli.alias()).join("bin");
    std::fs::create_dir_all(&directory).unwrap();
    let executable = directory.join(cli.executable());
    std::fs::write(&executable, b"MZ-test-only-not-executed").unwrap();
    executable.canonicalize().unwrap()
}

#[test]
fn native_cli_env_recognizes_only_the_verified_metadata_and_task_arguments() {
    assert!(supported_args(NativeCli::WorkBuddy, &workbuddy_args()));
    assert!(supported_args(NativeCli::Antigravity, &agy_args()));
    for metadata in ["--version", "--help", "models"] {
        assert!(supported_args(NativeCli::Antigravity, &[metadata.into()]));
    }
    for invalid in [
        vec!["update".into()],
        vec!["--print".into(), "arbitrary prompt".into()],
        vec!["--help".into(), "--exec".into()],
        workbuddy_args(),
    ] {
        assert!(!supported_args(NativeCli::Antigravity, &invalid));
    }
    let mut additional = agy_args();
    additional.push("--dangerously-skip-permissions".into());
    assert!(!supported_args(NativeCli::Antigravity, &additional));
    let mut invalid = agy_args();
    invalid[3] = "invented-effort".into();
    assert!(!supported_args(NativeCli::Antigravity, &invalid));
}

#[test]
fn native_cli_env_fallback_is_only_fixed_bare_alias_and_canonical_installation() {
    let temp = tempfile::tempdir().unwrap();
    let agy = install(temp.path(), NativeCli::Antigravity);
    let buddy = install(temp.path(), NativeCli::WorkBuddy);
    assert_eq!(
        fallback_program("agy", &agy_args(), Some(temp.path().as_os_str())),
        Some(agy.clone())
    );
    assert_eq!(
        fallback_program(
            "codebuddy",
            &workbuddy_args(),
            Some(temp.path().as_os_str())
        ),
        Some(buddy)
    );
    for program in ["other", "agy.exe", "./agy", "../agy", "tool/agy.exe"] {
        assert!(fallback_program(program, &agy_args(), Some(temp.path().as_os_str())).is_none());
    }
    assert!(fallback_program("agy", &["update".into()], Some(temp.path().as_os_str())).is_none());
    assert!(fallback_program("agy", &agy_args(), Some(OsStr::new("relative"))).is_none());
    assert!(fallback_program("agy", &agy_args(), None).is_none());
    assert_eq!(
        recognized_program(
            "agy",
            &agy_args(),
            agy.as_os_str(),
            Some(temp.path().as_os_str())
        ),
        Some(NativeCli::Antigravity)
    );
    assert_eq!(
        recognized_program(
            &agy.to_string_lossy(),
            &["models".into()],
            agy.as_os_str(),
            Some(temp.path().as_os_str())
        ),
        Some(NativeCli::Antigravity)
    );
}

#[test]
fn native_cli_env_same_named_project_or_path_executables_receive_no_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let installed = install(temp.path(), NativeCli::Antigravity);
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let impostor = project.join("agy.exe");
    std::fs::write(&impostor, b"MZ-test").unwrap();
    for requested in ["agy".to_string(), impostor.to_string_lossy().into_owned()] {
        assert_eq!(
            recognized_program(
                &requested,
                &agy_args(),
                impostor.as_os_str(),
                Some(temp.path().as_os_str())
            ),
            None
        );
    }
    assert_eq!(
        recognized_program(
            "other",
            &agy_args(),
            installed.as_os_str(),
            Some(temp.path().as_os_str())
        ),
        None
    );
    assert_eq!(
        recognized_program(
            "agy",
            &["update".into()],
            installed.as_os_str(),
            Some(temp.path().as_os_str())
        ),
        None
    );
}

#[test]
fn native_cli_env_injects_only_credential_free_proxy_and_disables_its_updater() {
    let environment = HashMap::from([
        ("NO_PROXY".into(), "localhost".into()),
        ("UNCHANGED".into(), "kept".into()),
    ]);
    let before = environment.clone();
    let additions = environment_overrides(
        NativeCli::Antigravity,
        &environment,
        Some("http=127.0.0.1:7890;https=127.0.0.1:7891"),
    );
    assert_eq!(
        additions,
        vec![
            ("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()),
            ("HTTP_PROXY".into(), "http://127.0.0.1:7891".into()),
            ("HTTPS_PROXY".into(), "http://127.0.0.1:7891".into())
        ]
    );
    assert_eq!(
        environment, before,
        "selection must never mutate caller or process environment"
    );
    assert_eq!(
        environment_overrides(NativeCli::Antigravity, &environment, None),
        vec![("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into())]
    );
    assert!(environment_overrides(
        NativeCli::WorkBuddy,
        &environment,
        Some("http://127.0.0.1:7890")
    )
    .is_empty());
    for invalid in [
        "http://user:secret@127.0.0.1:7890",
        "socks5://127.0.0.1:7890",
        "http://proxy.test/path",
        "http://proxy.test?secret=x",
        "http://proxy.test#x",
        "proxy\nvalue",
        "",
    ] {
        assert_eq!(
            environment_overrides(NativeCli::Antigravity, &environment, Some(invalid)).len(),
            1
        );
    }
}

#[test]
fn native_cli_env_explicit_proxy_including_empty_or_mixed_case_always_wins() {
    for name in ["HTTP_PROXY", "https_proxy", "aLl_PrOxY"] {
        for value in ["", "http://explicit.test:3128"] {
            let environment = HashMap::from([(name.into(), value.into())]);
            assert_eq!(
                environment_overrides(
                    NativeCli::Antigravity,
                    &environment,
                    Some("http://system.test:3128")
                ),
                vec![("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into())]
            );
        }
    }
}

#[cfg(windows)]
#[test]
fn native_cli_env_existing_path_choice_still_wins_and_impostor_gets_no_extra_environment() {
    use super::super::{configured_process_command, ShellConfig};
    let temp = tempfile::tempdir().unwrap();
    let impostor = temp.path().join("agy.exe");
    std::fs::write(&impostor, b"MZ-test-only").unwrap();
    let shell = ShellConfig {
        env: HashMap::from([("PATH".into(), temp.path().to_string_lossy().into_owned())]),
        ..Default::default()
    };
    let command =
        configured_process_command(&shell, None, "agy", &agy_args(), Some(temp.path())).unwrap();
    assert_eq!(command.get_program(), impostor.as_os_str());
    for (key, _) in command.get_envs() {
        assert_ne!(key, OsStr::new("AGY_CLI_DISABLE_AUTO_UPDATE"));
        assert_ne!(key, OsStr::new("HTTP_PROXY"));
        assert_ne!(key, OsStr::new("HTTPS_PROXY"));
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
