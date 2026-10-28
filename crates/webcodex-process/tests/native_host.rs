//! Real Windows process coverage for the normal host execution carrier.
#![cfg(windows)]
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use webcodex_process::windows_lpac::{AppContainerSid, LpacChild, LpacLaunch, LpacNetwork};

#[test]
fn host_execution_preserves_argv_env_cwd_stdin_streams_and_nonzero_exit() {
    let workspace = tempfile::Builder::new()
        .prefix("webcodex host shared ")
        .tempdir()
        .unwrap();
    let script = workspace.path().join("probe.ps1");
    std::fs::write(&script, r#"param([string]$Value)
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::Out.WriteLine('ARG=' + $Value)
[Console]::Out.WriteLine('ENV=' + $env:WEBCODEX_HOST_TEST_VALUE)
[Console]::Out.WriteLine('PATH_OK=' + ($env:PATH -eq $env:WEBCODEX_EXPECTED_PATH))
[Console]::Out.WriteLine('DRIVE_OK=' + ([Environment]::GetEnvironmentVariable('=C:') -eq $env:WEBCODEX_EXPECTED_DRIVE))
[Console]::Out.WriteLine('CWD=' + (Get-Location).Path)
[Console]::Out.WriteLine('STDIN=' + [Console]::In.ReadToEnd())
for ($i = 0; $i -lt 8; $i++) { & $env:ComSpec /d /c 'cmd /d /c exit 0' }
[Console]::Error.WriteLine('stderr-real-process')
exit 23
"#).unwrap();
    let program = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let value = "normal argument with spaces & quote \"kept\"";
    let args = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-File".into(),
        script.to_string_lossy().into_owned(),
        value.into(),
    ];
    let mut environment: BTreeMap<String, String> = std::env::vars().collect();
    environment.insert("WEBCODEX_HOST_TEST_VALUE".into(), "ordinary-value".into());
    let effective_path = std::env::var("PATH").unwrap();
    environment.insert("PATH".into(), "wrong-case-alias".into());
    environment.insert("Path".into(), effective_path.clone());
    environment.insert("WEBCODEX_EXPECTED_PATH".into(), effective_path);
    environment.insert(
        "=C:".into(),
        workspace.path().to_string_lossy().into_owned(),
    );
    environment.insert(
        "WEBCODEX_EXPECTED_DRIVE".into(),
        workspace.path().to_string_lossy().into_owned(),
    );
    let sid = AppContainerSid::derive("webcodex-normal-host-process-test").unwrap();
    let canonical_cwd = workspace.path().canonicalize().unwrap();
    let mut child = LpacChild::spawn_host(LpacLaunch {
        sid: &sid,
        program: &program,
        args: &args,
        cwd: &canonical_cwd,
        environment: &environment,
        network: LpacNetwork::None,
    })
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"ordinary-stdin").unwrap();
    drop(stdin);
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).unwrap();
        text
    });
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        stderr.read_to_string(&mut text).unwrap();
        text
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let exit = loop {
        assert!(Instant::now() < deadline, "real process timed out");
        child.capture_member_handles(deadline).unwrap();
        if let Some(code) = child.try_wait().unwrap() {
            break code;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(child.wait_tree_exit(Duration::from_secs(2)).unwrap());
    let stdout = output.join().unwrap();
    let stderr = errors.join().unwrap();
    assert_eq!(exit, 23);
    assert!(stdout.contains(&format!("ARG={value}")), "{stdout}");
    assert!(stdout.contains("ENV=ordinary-value"), "{stdout}");
    assert!(
        stdout.contains("PATH_OK=True"),
        "effective PATH alias was not preserved"
    );
    assert!(
        stdout.contains("DRIVE_OK=True"),
        "drive current-directory variable was not preserved"
    );
    assert!(
        stdout.contains(&format!("CWD={}", workspace.path().display())),
        "{stdout}"
    );
    assert!(stdout.contains("STDIN=ordinary-stdin"), "{stdout}");
    assert_eq!(stderr.trim(), "stderr-real-process");
    assert!(!stdout.contains("UNC"));
    // Kernel membership includes exited short-lived children/grandchildren;
    // their historical process handles were deliberately not collected.
    let stopped = child
        .observe_completed_tree(deadline)
        .unwrap()
        .expect("kernel Job must be empty after real processes exit");
    assert_eq!(stopped.leader_exit_code(), 23);
    assert!(stopped.member_count() >= 17);
    assert_eq!(child.observed_member_count(), 1);
}

#[test]
fn host_preflight_reports_invalid_environment_as_not_started() {
    let program = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    let cwd = std::env::current_dir().unwrap();
    let args = vec!["/c".into(), "exit 0".into()];
    let environment = BTreeMap::from([("invalid=ordinary-key".into(), "value".into())]);
    let sid = AppContainerSid::derive("webcodex-host-invalid-env-test").unwrap();
    let error = LpacChild::validate_host_launch(LpacLaunch {
        sid: &sid,
        program: &program,
        args: &args,
        cwd: &cwd,
        environment: &environment,
        network: LpacNetwork::None,
    })
    .unwrap_err();
    assert_eq!(error.code, "lpac_environment_invalid");
    assert_eq!(error.execution_state, "not_started");
}
