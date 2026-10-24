use super::tests::{activation_config, spawn_operator_server_with_hook};
use super::*;
use crate::webcodex_cli::test_support::canonical_test_tempdir;

#[tokio::test]
async fn activation_inherits_parent_authority_with_or_without_pending_reload() {
    for pending_reload in [false, true] {
        let tmp = canonical_test_tempdir();
        let parent = tmp.path().join("Desktop");
        let project = parent.join("801所PFA").join("nested");
        std::fs::create_dir_all(&project).unwrap();
        let canonical_project = project.canonicalize().unwrap();
        let mut responses = Vec::new();
        if pending_reload {
            responses.extend([
                (
                    "/api/projects/resolve-or-register",
                    json!({"success":false,"output":{"error_kind":"path_outside_allowed_roots"}}),
                ),
                (
                    "/api/runners/config/check",
                    json!({"success":true,"output":{"valid":true,"current_generation":1,"restart_required":false}}),
                ),
                (
                    "/api/runners/config/reload",
                    json!({"success":true,"output":{"current_generation":2,"restart_required":false}}),
                ),
            ]);
        }
        responses.push((
            "/api/projects/resolve-or-register",
            json!({
                "success": true,
                "output": {
                    "id": "agent:client:child",
                    "agent_project_id": "child",
                    "client_id": "client",
                    "path": canonical_project.to_string_lossy(),
                    "revision": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "changed": true
                }
            }),
        ));
        let (server_url, server) = spawn_operator_server_with_hook(responses, |_| {});
        let config_path = tmp.path().join("runner.toml");
        activation_config(&config_path, &server_url, &[parent], false);
        let token_file = tmp.path().join("user-token");
        std::fs::write(&token_file, "wc_pat_project_activation_test").unwrap();
        let before = std::fs::read(&config_path).unwrap();

        let output = run_project_activate(ProjectActivateOptions {
            expected_project_path: Some(canonical_project.clone()),
            config: config_path.clone(),
            user_token_file: token_file,
            project,
            json: true,
        })
        .await
        .unwrap();
        let output: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(output["policy"]["authority_changed"], false);
        assert_eq!(output["policy"]["config_reloaded"], pending_reload);
        assert_eq!(output["project"]["runtime_project"], "agent:client:child");
        assert_eq!(std::fs::read(&config_path).unwrap(), before);
        assert_eq!(
            server.join().unwrap().len(),
            if pending_reload { 4 } else { 1 }
        );
    }
}

#[cfg(any(unix, windows))]
fn create_directory_link(link: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Windows PowerShell's junction target must use Win32 spelling, while
        // the CLI request below keeps its original (possibly verbatim) path.
        let link_text = link.to_string_lossy();
        let target_text = target.to_string_lossy();
        let created = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "New-Item -ItemType Junction -Path $env:WC_JUNCTION_PATH -Target $env:WC_JUNCTION_TARGET -ErrorAction Stop | Out-Null",
            ])
            .env("WC_JUNCTION_PATH", link_text.trim_start_matches(r"\\?\"))
            .env("WC_JUNCTION_TARGET", target_text.trim_start_matches(r"\\?\"))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "junction creation failed: {}",
            String::from_utf8_lossy(&created.stderr)
        );
    }
    assert_eq!(link.canonicalize().unwrap(), target.canonicalize().unwrap());
}

#[test]
fn expected_project_identity_requires_an_absolute_valid_path() {
    let tmp = canonical_test_tempdir();
    let canonical = tmp.path().canonicalize().unwrap();
    validate_expected_project_path(&canonical, None).unwrap();
    validate_expected_project_path(&canonical, Some(&canonical)).unwrap();
    let traversal = PathBuf::from(format!(
        "{}{}..",
        canonical.display(),
        std::path::MAIN_SEPARATOR
    ));
    for expected in [PathBuf::from("relative"), traversal] {
        let error = validate_expected_project_path(&canonical, Some(&expected)).unwrap_err();
        assert!(
            error.starts_with("expected_project_path_invalid:"),
            "{error}"
        );
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn project_commands_reject_changed_inspected_identity_before_effects() {
    for replace_observed_path in [false, true] {
        let tmp = canonical_test_tempdir();
        let parent = tmp.path().join("Desktop");
        let original = parent.join("original");
        let replacement = parent.join("replacement");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        let selected = parent.join("selected");
        create_directory_link(&selected, &original);
        let observed_project = selected.canonicalize().unwrap();
        if replace_observed_path {
            std::fs::remove_dir(&original).unwrap();
            create_directory_link(&original, &replacement);
        } else {
            #[cfg(unix)]
            std::fs::remove_file(&selected).unwrap();
            #[cfg(windows)]
            std::fs::remove_dir(&selected).unwrap();
            create_directory_link(&selected, &replacement);
        }
        assert_eq!(
            selected.canonicalize().unwrap(),
            replacement.canonicalize().unwrap()
        );

        let config_path = tmp.path().join("runner.toml");
        let registry = tmp.path().join("registry");
        activation_config(&config_path, "http://127.0.0.1:1", &[parent], false);
        let mut config = std::fs::read_to_string(&config_path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        config["project_registry_dir"] = toml_edit::value(registry.to_string_lossy().as_ref());
        std::fs::write(&config_path, config.to_string()).unwrap();
        let before = std::fs::read(&config_path).unwrap();
        let token_file = tmp.path().join("user-token");
        std::fs::write(&token_file, "wc_pat_project_activation_test").unwrap();

        let activation_error = run_project_activate(ProjectActivateOptions {
            config: config_path.clone(),
            user_token_file: token_file,
            project: selected.clone(),
            expected_project_path: Some(observed_project.clone()),
            json: true,
        })
        .await
        .unwrap_err();
        let registration_error = run_project_register(ProjectRegisterOptions {
            config: config_path.clone(),
            project: selected.clone(),
            expected_project_path: Some(observed_project),
            json: true,
        })
        .unwrap_err();
        for error in [activation_error, registration_error] {
            assert!(
                error.starts_with("project_path_changed:"),
                "replace_observed_path={replace_observed_path}: {error}"
            );
        }
        assert_eq!(std::fs::read(&config_path).unwrap(), before);
        assert!(
            !registry.exists(),
            "changed identity must not publish a registration"
        );

        run_project_register(ProjectRegisterOptions {
            config: config_path.clone(),
            project: selected,
            expected_project_path: Some(replacement.canonicalize().unwrap()),
            json: true,
        })
        .unwrap();
        assert!(registry.is_dir());
        assert_eq!(std::fs::read(&config_path).unwrap(), before);
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn activation_config_retry_preserves_selected_path_authority_and_identity() {
    for generation_conflict in [false, true] {
        for retarget in [false, true] {
            let tmp = canonical_test_tempdir();
            let parent = tmp.path().join("Desktop");
            let original = tmp.path().join("original");
            let replacement = tmp.path().join("replacement");
            for directory in [&parent, &original, &replacement] {
                std::fs::create_dir(directory).unwrap();
            }
            let project = parent.join("selected");
            create_directory_link(&project, &original);
            let config_path = tmp.path().join("runner.toml");
            let mut responses = vec![
                (
                    "/api/projects/resolve-or-register",
                    json!({"success":false,"output":{"error_kind":"path_outside_allowed_roots"}}),
                ),
                (
                    "/api/runners/config/check",
                    json!({"success":true,"output":{"valid":true,"current_generation":1,"restart_required":false}}),
                ),
            ];
            if generation_conflict {
                responses.push((
                    "/api/runners/config/reload",
                    json!({"success":false,"output":{"error_code":"config_generation_conflict"}}),
                ));
            }
            let replacement_at = responses.len();
            let edited_path = config_path.clone();
            let selected_path = project.clone();
            let retained_root = parent.clone();
            let new_target = replacement.clone();
            let expected_config = std::sync::Arc::new(std::sync::Mutex::new(None));
            let edited_config = expected_config.clone();
            let (server_url, server) = spawn_operator_server_with_hook(responses, move |index| {
                if index != replacement_at {
                    return;
                }
                let current = std::fs::read_to_string(&edited_path).unwrap();
                let next = if retarget {
                    #[cfg(unix)]
                    std::fs::remove_file(&selected_path).unwrap();
                    #[cfg(windows)]
                    std::fs::remove_dir(&selected_path).unwrap();
                    create_directory_link(&selected_path, &new_target);
                    format!("{current}# concurrent selected path change\n")
                } else {
                    let mut next = current.parse::<toml_edit::DocumentMut>().unwrap();
                    let mut roots = toml_edit::Array::new();
                    roots.push(retained_root.to_string_lossy().as_ref());
                    next["policy"]["allowed_roots"] = toml_edit::value(roots);
                    next.to_string()
                };
                std::fs::write(&edited_path, &next).unwrap();
                *edited_config.lock().unwrap() = Some(next);
            });
            activation_config(
                &config_path,
                &server_url,
                &[parent, original, replacement],
                false,
            );
            let token_file = tmp.path().join("user-token");
            std::fs::write(&token_file, "wc_pat_project_activation_test").unwrap();

            let error = run_project_activate(ProjectActivateOptions {
                expected_project_path: None,
                config: config_path.clone(),
                user_token_file: token_file,
                project,
                json: true,
            })
            .await
            .unwrap_err();
            assert!(
                expected_config.lock().unwrap().is_some(),
                "mock did not reach the final request (generation_conflict={generation_conflict}, retarget={retarget}): {error}"
            );
            if retarget {
                assert!(
                    error.starts_with("project_activation_config_conflict:"),
                    "generation_conflict={generation_conflict}, retarget={retarget}: {error}"
                );
            } else {
                assert!(
                    error.contains("symlink resolves outside allowed_roots"),
                    "generation_conflict={generation_conflict}, retarget={retarget}: {error}"
                );
            }
            assert_eq!(server.join().unwrap().len(), replacement_at);
            assert_eq!(
                std::fs::read_to_string(&config_path).unwrap(),
                expected_config.lock().unwrap().as_ref().unwrap().as_str(),
                "a retry must not reauthorize a revoked link target or mutate another project"
            );
        }
    }
}
