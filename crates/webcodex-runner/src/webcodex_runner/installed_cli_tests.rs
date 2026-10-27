use super::*;

/// Explicit operator smoke only. The spec contains a chosen local program,
/// discovered native arguments and a synthetic prompt, never a copied login.
#[test]
#[ignore = "Opt-in real installed CLI call; may contact the selected model provider"]
fn runner_real_process_installed_cli_operator_smoke() {
    use crate::webcodex_runner::config::{RunnerPolicy, ShellConfig};
    use crate::webcodex_runner::shell::{
        run_process_with_profiles_and_execution_state, PreparedShellProfileCache,
    };
    let raw =
        std::env::var("WEBCODEX_LOCAL_CLI_SMOKE_SPEC").expect("Explicit CLI smoke spec required");
    let spec: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let program = spec["program"].as_str().unwrap();
    let args: Vec<String> = serde_json::from_value(spec["args"].clone()).unwrap();
    let prompt = spec["stdin"].as_str().unwrap();
    let marker = spec["expected_result"].as_str().unwrap();
    assert!(!marker.is_empty());
    let temp = tempfile::tempdir().unwrap();
    assert!(
        resolve_installed(program).is_some(),
        "Selected program is not installed"
    );
    let result = run_process_with_profiles_and_execution_state(
        1,
        &RunnerPolicy {
            allowed_roots: vec![temp.path().to_path_buf()],
            ..Default::default()
        },
        &ShellConfig::default(),
        temp.path(),
        &PreparedShellProfileCache::default(),
        Some(temp.path().to_str().unwrap()),
        program,
        &args,
        Some(prompt),
        90,
        None,
    );
    assert_eq!(
        result.execution_state,
        webcodex_core::runner_protocol::ShellCommandExecutionState::Completed
    );
    assert_eq!(
        result.result.exit_code,
        Some(0),
        "CLI process failed; inspect local redacted diagnostics"
    );
    let output: serde_json::Value =
        serde_json::from_str(result.result.stdout.as_deref().unwrap_or(""))
            .expect("CLI did not return its requested JSON result");
    if let Some(path) = spec["diagnostic_path"].as_str() {
        let path = Path::new(path);
        assert!(
            path.is_absolute(),
            "Operator diagnostic path must be absolute"
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&output).unwrap()).unwrap();
    }
    // CodeBuddy's installed JSON mode returns an event array; some versions
    // return the terminal object directly. Ignore echoed user messages and
    // require one explicit successful terminal result in either envelope.
    let terminal: Vec<_> = match output.as_array() {
        Some(events) => events
            .iter()
            .filter(|event| event["type"] == "result")
            .collect(),
        None => vec![&output],
    };
    assert_eq!(
        terminal.len(),
        1,
        "Missing or ambiguous terminal CLI result"
    );
    let output = terminal[0];
    assert_eq!(output["type"], "result");
    assert_eq!(output["subtype"], "success");
    assert_eq!(
        output["is_error"], false,
        "CLI reported a provider error despite process exit"
    );
    assert_eq!(
        output["result"].as_str().unwrap_or("").trim(),
        marker,
        "CLI did not produce the requested result"
    );
}

#[test]
#[ignore = "Runs one real native child through Runner process ownership"]
fn runner_real_process_installed_cli_accepts_non_provider_executable() {
    use crate::webcodex_runner::config::{RunnerPolicy, ShellConfig};
    use crate::webcodex_runner::shell::{
        run_process_with_profiles_and_execution_state, PreparedShellProfileCache,
    };
    let temp = tempfile::tempdir().unwrap();
    let program = std::env::current_exe().unwrap();
    let result = run_process_with_profiles_and_execution_state(
        1,
        &RunnerPolicy {
            allowed_roots: vec![temp.path().to_path_buf()],
            ..Default::default()
        },
        &ShellConfig::default(),
        temp.path(),
        &PreparedShellProfileCache::default(),
        Some(temp.path().to_str().unwrap()),
        program.to_str().unwrap(),
        &["--list".into()],
        None,
        15,
        None,
    );
    assert_eq!(
        result.execution_state,
        webcodex_core::runner_protocol::ShellCommandExecutionState::Completed
    );
    assert_eq!(
        result.result.exit_code,
        Some(0),
        "{:?}",
        result.result.error
    );
    assert!(result
        .result
        .stdout
        .unwrap_or_default()
        .contains("installed_cli"));
}

fn fixture(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    });
    std::fs::write(&path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path.canonicalize().unwrap()
}

#[test]
fn installed_cli_accepts_arbitrary_products_and_refreshes_after_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let path = fixture(&bin, "third-party-model", b"version-one");
    assert_eq!(
        resolve_in_directories("third-party-model", &[bin.clone()]),
        Some(path.clone())
    );
    std::fs::write(&path, b"different-version-and-hash").unwrap();
    assert_eq!(
        resolve_in_directories("third-party-model", &[bin]),
        Some(path)
    );
}

#[test]
fn installed_cli_does_not_search_cwd_or_relative_path_entries() {
    assert_eq!(
        resolve_in_directories("new-cli", &[PathBuf::from("."), PathBuf::new()]),
        None
    );
    assert_eq!(resolve_in_directories("../new-cli", &[]), None);
    assert_eq!(resolve_in_directories("new-cli\nother", &[]), None);
}

#[test]
fn installed_cli_missing_program_is_explicit_and_discovery_does_not_spawn() {
    let inventory = discover(&LocalCliQuery {
        commands: vec!["webcodex-nonexistent-cli-fixture-54c6".into()],
    })
    .unwrap();
    assert!(inventory.programs[0].executable.is_none());
    assert_eq!(
        inventory.programs[0].reason_code.as_deref(),
        Some("local_cli_not_installed_or_not_executable")
    );
}

#[test]
fn installed_cli_supports_absolute_executables_and_rejects_directories() {
    let root = tempfile::tempdir().unwrap();
    let path = fixture(root.path(), "custom-tool", b"test");
    assert_eq!(
        resolve_in_directories(path.to_str().unwrap(), &[]),
        Some(path)
    );
    assert_eq!(
        resolve_in_directories(root.path().to_str().unwrap(), &[]),
        None
    );
}

#[cfg(windows)]
#[test]
fn installed_cli_resolves_versioned_installers_launcher_symlink() {
    use std::os::windows::fs::symlink_file;
    let root = tempfile::tempdir().unwrap();
    let old = fixture(&root.path().join("versions/one"), "model", b"one");
    let new = fixture(&root.path().join("versions/two"), "model", b"two");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let launcher = bin.join("model.exe");
    symlink_file(&old, &launcher).unwrap();
    assert_eq!(resolve_in_directories("model", &[bin.clone()]), Some(old));
    std::fs::remove_file(&launcher).unwrap();
    symlink_file(&new, &launcher).unwrap();
    assert_eq!(resolve_in_directories("model", &[bin]), Some(new));
}
