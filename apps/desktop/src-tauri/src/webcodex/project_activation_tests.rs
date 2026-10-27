use super::*;
use std::os::windows::process::CommandExt;

struct Fixture {
    root: PathBuf,
    desktop: PathBuf,
    connection: RuntimeConnection,
    adapter: WebCodexAdapter,
}

impl Fixture {
    fn new() -> Self {
        let cli = PathBuf::from(
            std::env::var_os("WEBCODEX_DESKTOP_PROJECT_TEST_CLI")
                .expect("supply the source-matched test CLI explicitly"),
        );
        assert!(cli.is_absolute() && cli.is_file());
        let root = std::env::temp_dir().join(format!(
            "webcodex-project-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let desktop = root.join("Desktop");
        std::fs::create_dir_all(&desktop).unwrap();
        // Reserve an unused loopback port; these negative tests need no Server.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let connection = RuntimeConnection {
            runner_config: root.join("runner.toml"),
            user_token_file: root.join("user-token"),
            server_url,
        };
        std::fs::write(&connection.user_token_file, "wc_pat_project_link_test").unwrap();
        let adapter = WebCodexAdapter {
            binaries: Some(ResolvedBinaries {
                directory: cli.parent().unwrap().to_path_buf(),
                webcodex: cli,
                server: PathBuf::new(),
                runner: PathBuf::new(),
                version: "test".into(),
                git_commit: "test".into(),
                source: super::super::cli::ResolvedBinarySource::Environment,
            }),
            bundled_runtime_dir: None,
        };
        Self {
            root,
            desktop,
            connection,
            adapter,
        }
    }

    fn reset_config(&self) -> Vec<u8> {
        std::fs::write(
            &self.connection.runner_config,
            format!(
                "server_url = {:?}\ntoken = 'fixture-runner-token'\nclient_id = 'client'\nproject_registry_dir = {:?}\n[policy]\nallow_cwd_anywhere = false\nallowed_roots = [{:?}]\n",
                self.connection.server_url,
                self.root.join("registry").to_string_lossy(),
                self.desktop.to_string_lossy(),
            ),
        ).unwrap();
        std::fs::read(&self.connection.runner_config).unwrap()
    }

    fn identity(&self) -> ProjectRuntimeIdentity {
        ProjectRuntimeIdentity {
            project_id: "desktop".into(),
            runtime_project_id: "agent:client:desktop".into(),
            project_path: self.desktop.to_string_lossy().into_owned(),
            runner_config: self.connection.runner_config.clone(),
            user_token_file: self.connection.user_token_file.clone(),
            server_url: self.connection.server_url.clone(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Unique fixture only. Windows remove_dir_all removes junctions themselves.
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn directory_link(path: &Path, target: &Path, junction: bool) {
    if junction {
        let result = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "New-Item -ItemType Junction -Path $env:WC_JUNCTION_PATH -Target $env:WC_JUNCTION_TARGET -ErrorAction Stop | Out-Null"])
            .env("WC_JUNCTION_PATH", path)
            .env("WC_JUNCTION_TARGET", target)
            .creation_flags(0x08000000)
            .output().unwrap();
        assert!(result.status.success(), "junction creation failed");
    } else {
        std::os::windows::fs::symlink_dir(target, path).unwrap();
    }
    assert!(std::fs::symlink_metadata(path).unwrap().is_symlink());
}

#[tokio::test]
#[ignore = "requires a source-matched CLI and Windows link creation; no GUI or live runtime"]
async fn desktop_project_links_cannot_extend_authority_after_inspection() {
    let mut fixture = Fixture::new();
    let outside = fixture.root.join("outside");
    std::fs::create_dir_all(outside.join("nested")).unwrap();
    let mut expanded = Vec::new();
    for junction in [true, false] {
        let link = fixture
            .desktop
            .join(if junction { "junction" } else { "symlink" });
        directory_link(&link, &outside, junction);
        for selected in [link.clone(), link.join("nested")] {
            let raw = selected.to_string_lossy();
            let before = fixture.reset_config();
            // Prove the CLI already rejects the original entry, including junction ancestors.
            let direct = Command::new(&fixture.adapter.binaries().unwrap().webcodex)
                .args(["project", "activate", "--config"])
                .arg(&fixture.connection.runner_config)
                .arg("--user-token-file")
                .arg(&fixture.connection.user_token_file)
                .arg(&selected)
                .arg("--json")
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(!direct.status.success());
            assert_eq!(
                std::fs::read(&fixture.connection.runner_config).unwrap(),
                before
            );
            let project = fixture.adapter.inspect_project(&raw).await.unwrap();
            assert!(same_path(
                &project.path,
                &selected.canonicalize().unwrap().to_string_lossy()
            ));
            let result = fixture
                .adapter
                .activate_connection_project(
                    &fixture.connection,
                    "client",
                    &raw,
                    &project,
                    &CancellationContext::never(),
                )
                .await;
            assert!(result.is_err());
            if std::fs::read(&fixture.connection.runner_config).unwrap() != before {
                expanded.push(format!("activate: {}", selected.display()));
            }

            let before = fixture.reset_config();
            let identity = fixture.identity();
            let result = fixture
                .adapter
                .legacy_register_project(
                    &identity,
                    "client",
                    &raw,
                    &project,
                    &CancellationContext::never(),
                )
                .await;
            if result.is_ok() || std::fs::read(&fixture.connection.runner_config).unwrap() != before
            {
                expanded.push(format!("register: {}", selected.display()));
            }
        }
    }
    assert!(
        expanded.is_empty(),
        "Desktop expanded authority via links: {expanded:?}"
    );
}

#[tokio::test]
#[ignore = "requires a source-matched CLI and Windows junction creation; no GUI or live runtime"]
async fn desktop_project_registration_preserves_inheritance_and_explicit_selection() {
    let mut fixture = Fixture::new();
    let child = fixture.desktop.join("801所PFA").join("nested");
    let outside = fixture.root.join("outside");
    let sibling = fixture.root.join("Desktop2");
    for path in [&child, &outside, &sibling] {
        std::fs::create_dir_all(path).unwrap();
    }
    let internal_link = fixture.desktop.join("internal-link");
    directory_link(&internal_link, &child, true);
    for (selected, extends) in [
        (&child, false),
        (&internal_link, false),
        (&outside, true),
        (&sibling, true),
    ] {
        let before = fixture.reset_config();
        let raw = selected.to_string_lossy();
        let project = fixture.adapter.inspect_project(&raw).await.unwrap();
        let identity = fixture.identity();
        let result = fixture
            .adapter
            .legacy_register_project(
                &identity,
                "client",
                &raw,
                &project,
                &CancellationContext::never(),
            )
            .await
            .unwrap();
        assert!(same_path(&result.project_path, &project.path));
        let after = std::fs::read(&fixture.connection.runner_config).unwrap();
        if !extends {
            assert_eq!(after, before);
        }
        let config: toml_edit::DocumentMut = std::str::from_utf8(&after).unwrap().parse().unwrap();
        let roots = config["policy"]["allowed_roots"].as_array().unwrap();
        assert_eq!(roots.len(), if extends { 2 } else { 1 });
        if extends {
            assert!(same_path(
                roots.get(1).unwrap().as_str().unwrap(),
                &project.path
            ));
        }
    }
}

#[tokio::test]
#[ignore = "requires a source-matched CLI and Windows links; isolated loopback peer, no GUI or live runtime"]
async fn desktop_project_retarget_after_inspection_has_no_side_effects() {
    use std::io::{Read, Write};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    let mut side_effects = Vec::new();
    for junction in [true, false] {
        for legacy in [false, true] {
            let mut fixture = Fixture::new();
            let a = fixture.desktop.join("已选择A");
            let b = fixture.desktop.join("未选择B");
            std::fs::create_dir(&a).unwrap();
            std::fs::create_dir(&b).unwrap();
            let link = fixture.desktop.join("selected");
            directory_link(&link, &a, junction);
            let raw = link.to_string_lossy();
            let inspected = fixture.adapter.inspect_project(&raw).await.unwrap();
            assert!(same_path(
                &inspected.path,
                &a.canonicalize().unwrap().to_string_lossy()
            ));
            std::fs::remove_dir(&link).unwrap();
            directory_link(&link, &b, junction);

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            fixture.connection.server_url = format!("http://{}", listener.local_addr().unwrap());
            let before = fixture.reset_config();
            let registry = fixture.root.join("registry");
            std::fs::create_dir(&registry).unwrap();
            let stopped = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(AtomicUsize::new(0));
            let stop_peer = stopped.clone();
            let peer_requests = requests.clone();
            let peer_registry = registry.clone();
            let target = b.canonicalize().unwrap();
            let peer = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                                .unwrap();
                            let mut request = [0; 65536];
                            let read = stream.read(&mut request).unwrap();
                            assert!(std::str::from_utf8(&request[..read])
                                .unwrap()
                                .starts_with("POST /api/projects/resolve-or-register "));
                            peer_requests.fetch_add(1, Ordering::SeqCst);
                            // A minimal operator peer makes the registration side effect observable.
                            std::fs::write(peer_registry.join("b.toml"), "id = 'b'\n").unwrap();
                            let body = serde_json::json!({"success":true,"output":{
                                "id":"agent:client:b", "agent_project_id":"b", "client_id":"client",
                                "path":target.to_string_lossy(), "revision":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "changed":true
                            }}).to_string();
                            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop_peer.load(Ordering::SeqCst) {
                                break;
                            }
                            assert!(
                                std::time::Instant::now() < deadline,
                                "operator peer deadline elapsed"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => panic!("operator peer failed: {error}"),
                    }
                }
            });
            let result = if legacy {
                fixture
                    .adapter
                    .legacy_register_project(
                        &fixture.identity(),
                        "client",
                        &raw,
                        &inspected,
                        &CancellationContext::never(),
                    )
                    .await
            } else {
                fixture
                    .adapter
                    .activate_connection_project(
                        &fixture.connection,
                        "client",
                        &raw,
                        &inspected,
                        &CancellationContext::never(),
                    )
                    .await
            };
            stopped.store(true, Ordering::SeqCst);
            peer.join().unwrap();
            assert!(result.is_err(), "retargeted selection must be rejected");
            assert_eq!(
                std::fs::read(&fixture.connection.runner_config).unwrap(),
                before
            );
            if requests.load(Ordering::SeqCst) != 0
                || std::fs::read_dir(&registry).unwrap().count() != 0
            {
                side_effects.push((junction, legacy));
            }
        }
    }
    assert!(
        side_effects.is_empty(),
        "retargeted selections reached registration: {side_effects:?}"
    );
}
