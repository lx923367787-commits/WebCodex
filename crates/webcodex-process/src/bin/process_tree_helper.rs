//! Test helper for `webcodex-process` integration tests.
//!
//! Self-contained (no cmd, PowerShell, bash, or Git Bash) so the same binary
//! runs on Windows and Unix. Modes are selected by `argv[1]`:
//!
//! * `sleep <secs> [exit_code]` — sleep then exit; used for normal completion.
//! * `spawn-grandchild <marker> <delay> <total>` — spawn ourself again as a
//!   grandchild, print `GRANDCHILD_PID=<pid>`, then exit immediately while the
//!   grandchild keeps running.
//! * `spawn-grandchild-keepalive <marker> <delay> <total> <keepalive>` — spawn
//!   ourself again as a grandchild, print `GRANDCHILD_PID=<pid>`, then sleep
//!   `keepalive` so the direct child stays alive while the grandchild runs.
//! * `grandchild <marker> <delay> <total>` — sleep `delay`, write our own PID
//!   to `marker`, sleep until `total`, then exit 0.
//! * `hold-stdout` — write `PING` and keep the stdout write end open forever.
//! * `spawn-parent-lease-child <marker>` — spawn a child with piped stdin and
//!   exit immediately; used to model an owner process disappearing.
//! * `parent-lease-child <marker>` — wait for stdin EOF, write `marker`, exit.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("sleep");

    match mode {
        "--version" => println!("synthetic-metadata-cli 1.2.3"),
        "--help" => {
            println!("--model <model> Currently supported: (synthetic-model-a, synthetic-model-b)");
            println!("--effort <effort> Supported: (low, high)");
        }
        "models" => {
            println!("synthetic-model-a Synthetic Model A\nsynthetic-model-b Synthetic Model B")
        }
        "lpac-publish-delivery" => {
            let manifest = args.get(2).expect("explicit synthetic manifest");
            let artifact = args.get(3).expect("synthetic artifact text");
            let ready = args.get(4).expect("readiness file");
            let release = args.get(5).expect("release file");
            std::fs::create_dir_all("publish/files").expect("create explicit publish directory");
            std::fs::write("publish/files/artifact.txt", artifact)
                .expect("write declared artifact");
            std::fs::write("publish/manifest.json", manifest).expect("write explicit manifest");
            std::fs::write("raw-log.txt", b"must-not-be-delivered-log")
                .expect("write excluded synthetic log");
            std::fs::write("../home/private.txt", b"must-not-be-delivered-home")
                .expect("write excluded synthetic home");
            std::fs::write(ready, b"explicit-publish-ready").expect("write ready marker");
            wait_observation_release(release);
        }
        "lpac-resource-boundary" => {
            let authorized = args.get(2).expect("authorized relative input");
            let outside = args.get(3).expect("outside absolute sentinel");
            let ready = args.get(4).expect("ready relative path");
            let release = args.get(5).expect("release relative path");
            assert_eq!(
                std::fs::read(authorized).expect("read authorized input"),
                b"authorized-input"
            );
            std::fs::write("owned-output.txt", b"workspace-write-ok")
                .expect("write owned workspace");
            let denied_read =
                std::fs::read(outside).expect_err("outside private sentinel must be denied");
            let denied_write = std::fs::OpenOptions::new()
                .write(true)
                .open(authorized)
                .expect_err("immutable input write must be denied");
            std::fs::write(ready, format!("authorized-input-read; workspace-write; outside-read-denied={:?}; input-write-denied={:?}", denied_read.raw_os_error(), denied_write.raw_os_error()))
                .expect("write boundary readiness");
            wait_observation_release(release);
        }
        "lpac-observe-tree" => {
            let ready = args.get(2).expect("ready base");
            let release = args.get(3).expect("release file");
            let depth: usize = args.get(4).expect("depth").parse().expect("depth integer");
            assert!(depth <= 2);
            let mut child = if depth > 0 {
                let mut command = observation_command();
                command.args([
                    "lpac-observe-tree",
                    ready,
                    release,
                    &(depth - 1).to_string(),
                ]);
                Some(command.spawn().expect("spawn bounded observation child"))
            } else {
                None
            };
            std::fs::write(format!("{ready}.{depth}"), b"ready").expect("write ready");
            wait_observation_release(release);
            if let Some(child) = child.as_mut() {
                let _ = child.wait();
            }
        }
        "lpac-observe-short" => {
            let done = args.get(2).expect("done file");
            let release = args.get(3).expect("release file");
            let count: usize = args.get(4).expect("count").parse().expect("count integer");
            assert!((1..=16).contains(&count));
            for _ in 0..count {
                assert!(observation_command()
                    .args(["sleep", "0"])
                    .status()
                    .expect("short child")
                    .success());
            }
            std::fs::write(done, b"finished").expect("write completion marker");
            wait_observation_release(release);
        }
        "sleep" => {
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
            let code: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_secs(secs));
            std::process::exit(code);
        }
        "spawn-grandchild" => {
            spawn_grandchild(&args);
            std::process::exit(0);
        }
        "spawn-grandchild-keepalive" => {
            spawn_grandchild(&args);
            let keepalive: u64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(60);
            std::thread::sleep(std::time::Duration::from_secs(keepalive));
            std::process::exit(0);
        }
        "grandchild" => {
            let marker = args.get(2).expect("marker path");
            let delay: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
            let total: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(60);
            std::thread::sleep(std::time::Duration::from_secs(delay));
            std::fs::write(marker, std::process::id().to_string()).expect("write marker");
            let rest = total.saturating_sub(delay);
            std::thread::sleep(std::time::Duration::from_secs(rest));
        }
        "hold-stdout" => {
            println!("PING");
            std::io::stdout().flush().expect("flush stdout");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        "spawn-parent-lease-child" => {
            let marker = args.get(2).expect("marker path");
            let self_exe = std::env::current_exe().expect("current_exe");
            let mut cmd = Command::new(self_exe);
            cmd.args(["parent-lease-child", marker])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[allow(clippy::zombie_processes)]
            let child = cmd.spawn().expect("spawn parent-lease child");
            println!("LEASE_CHILD_PID={}", child.id());
            std::io::stdout().flush().expect("flush stdout");
            // process::exit intentionally skips Rust destructors. The kernel
            // still closes the parent's pipe write handle, exactly as it does
            // when the owning Desktop process disappears unexpectedly.
            std::process::exit(0);
        }
        "parent-lease-child" => {
            let marker = args.get(2).expect("marker path");
            let mut bytes = Vec::new();
            std::io::stdin()
                .read_to_end(&mut bytes)
                .expect("read parent lease stdin");
            std::fs::write(marker, b"parent_eof").expect("write parent EOF marker");
        }
        other => {
            eprintln!("process_tree_helper: unknown mode: {other}");
            std::process::exit(2);
        }
    }
}

fn observation_command() -> Command {
    let mut command = Command::new(std::env::current_exe().expect("current_exe"));
    // The strict LPAC fixture has no NUL-device grant. Reuse the already
    // granted pipe handles instead of opening another device for child stdio.
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW: synthetic probes never open UI.
    }
    command
}

fn wait_observation_release(path: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !std::path::Path::new(path).is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "observation release deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Spawn ourself again as a `grandchild` that inherits stdin/stdout/stderr so
/// it keeps the piped stdout write end open, and print its pid on the parent's
/// stdout. Args are indexed as in the `spawn-grandchild` family of modes.
fn spawn_grandchild(args: &[String]) {
    let marker = args.get(2).expect("marker path");
    let delay: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    let total: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(60);
    let self_exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(self_exe);
    cmd.args(["grandchild", marker, &delay.to_string(), &total.to_string()]);
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // The grandchild must survive this process: `spawn-grandchild` exits
    // immediately so tests can verify process-tree termination (Job Object /
    // process group) reaches the orphaned grandchild. Waiting for it here
    // would change the test contract.
    #[allow(clippy::zombie_processes)]
    let child = cmd.spawn().expect("spawn grandchild");
    println!("GRANDCHILD_PID={}", child.id());
    std::io::stdout().flush().expect("flush stdout");
}
