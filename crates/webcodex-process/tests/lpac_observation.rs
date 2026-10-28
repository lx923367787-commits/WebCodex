//! Explicit, ignored real-process probes. All ACL changes and helper execution
//! stay in fresh directories below the operator-selected validation root.
#![cfg(windows)]
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use webcodex_process::windows_lpac::{AppContainerSid, LpacChild, LpacLaunch};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Threading::*;

const ROUNDS: usize = 8;

fn sid_text(sid: PSID) -> String {
    let mut text = null_mut();
    assert_ne!(unsafe { ConvertSidToStringSidW(sid, &mut text) }, 0);
    let mut length = 0;
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
    }
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) }).unwrap();
    unsafe {
        LocalFree(text.cast());
    }
    result
}

fn user_sid() -> String {
    let mut token = null_mut();
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
        0
    );
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut size = 0;
    unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size);
    }
    assert!(size > 0);
    let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
    assert_ne!(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                data.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        },
        0
    );
    sid_text(unsafe { (*data.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}

fn set_exact_acl(root: &Path, path: &Path, owner: &str, sid: &str, rights: &str, low: bool) {
    assert!(path.canonicalize().unwrap().starts_with(root));
    assert!(!fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    let mut sddl = format!("D:P(A;OICI;FA;;;{owner})(A;OICI;FA;;;SY)(A;OICI;{rights};;;{sid})");
    if low {
        sddl.push_str("S:(ML;OICI;NW;;;LW)");
    }
    let text = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut descriptor = null_mut();
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            )
        },
        0
    );
    let (mut dacl, mut sacl) = (null_mut(), null_mut());
    let (mut present, mut defaulted) = (0, 0);
    assert_ne!(
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) },
        0
    );
    if low {
        assert_ne!(
            unsafe {
                GetSecurityDescriptorSacl(descriptor, &mut present, &mut sacl, &mut defaulted)
            },
            0
        );
    }
    let flags = DACL_SECURITY_INFORMATION
        | PROTECTED_DACL_SECURITY_INFORMATION
        | if low { LABEL_SECURITY_INFORMATION } else { 0 };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            flags,
            null_mut(),
            null_mut(),
            dacl,
            sacl,
        )
    };
    unsafe {
        LocalFree(descriptor);
    }
    assert_eq!(result, 0, "exact temporary test ACL");
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
    workspace: PathBuf,
    sid: AppContainerSid,
    environment: BTreeMap<String, String>,
    network_identity: String,
}

#[test]
#[ignore = "real Windows per-AppContainer local TCP policy; explicit fresh validation root required"]
fn lpac_local_service_allows_only_owned_port_and_revokes_permission() {
    run_local_service(false);
}
#[test]
#[ignore = "real Windows policy-controller loss; explicit fresh validation root required"]
fn lpac_local_service_controller_loss_keeps_network_blocked() {
    run_local_service(true);
}

fn run_local_service(controller_loss: bool) {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use webcodex_process::windows_local_service::LocalServiceNetwork;
    let fixture = Fixture::new(99);
    let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = reserved.local_addr().unwrap().port();
    let forbidden = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let denied_port = forbidden.local_addr().unwrap().port();
    drop(reserved);
    let audit = PathBuf::from(std::env::var_os("WEBCODEX_LPAC_OBSERVATION_ROOT").unwrap())
        .join(format!("local-service-{}.txt", fixture.network_identity));
    fs::write(
        &audit,
        format!(
            "identity={}\nport={port}\nstate=installing\n",
            fixture.network_identity
        ),
    )
    .unwrap();
    let mut network = Some(
        LocalServiceNetwork::install(&fixture.sid, &fixture.network_identity, port)
            .expect("install exact local network policy"),
    );
    let mode = network.as_ref().unwrap().mode();
    let args = vec![
        "lpac-local-service".into(),
        port.to_string(),
        denied_port.to_string(),
        "service-ready".into(),
        "release".into(),
    ];
    let launched = LpacChild::spawn(LpacLaunch {
        network: mode,
        sid: &fixture.sid,
        program: &fixture.program,
        args: &args,
        cwd: &fixture.workspace,
        environment: &fixture.environment,
    });
    let mut child = match launched {
        Ok(c) => c,
        Err(e) => {
            if e.execution_state == "not_started" {
                network.take().unwrap().remove_after_stop().unwrap();
            }
            panic!("local service spawn: {e}");
        }
    };
    drop(child.stdin.take());
    let result = (|| -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !fixture.workspace.join("service-ready").exists() {
            if let Some(exit) = child.try_wait().map_err(|e| e.to_string())? {
                let mut text = String::new();
                if let Some(stderr) = child.stderr.take() {
                    let _ = stderr.take(4096).read_to_string(&mut text);
                }
                return Err(format!("child exited {exit}: {text}"));
            }
            if Instant::now() >= deadline {
                return Err("service readiness expired".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let report = fs::read_to_string(fixture.workspace.join("service-ready"))
            .map_err(|e| e.to_string())?;
        let listeners = child.local_listeners().map_err(|e| e.to_string())?;
        if !listeners.contains(&(child.id(), port))
            || listeners.iter().any(|(_, p)| *p == denied_port)
        {
            return Err("kernel listener ownership mismatch".into());
        }
        if !report.contains("other_port_denied=true") {
            return Err(report);
        }
        let mut stream =
            TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(2))
                .map_err(|e| format!("host connection: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .map_err(|e| e.to_string())?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|e| e.to_string())?;
        if !response.ends_with("\r\n\r\nOK") {
            return Err("HTTP response mismatch".into());
        }
        if controller_loss {
            drop(network.take());
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                return Err("service exited before controller-loss verification".into());
            }
            if TcpStream::connect_timeout(
                &(Ipv4Addr::LOCALHOST, port).into(),
                Duration::from_millis(300),
            )
            .is_ok()
            {
                return Err("controller loss left network permission active".into());
            }
        }
        Ok(report)
    })();
    child.terminate_tree().unwrap();
    assert!(child.wait_tree_exit(Duration::from_secs(5)).unwrap());
    drop(child);
    if let Some(network) = network.take() {
        network.remove_after_stop().unwrap();
    }
    LocalServiceNetwork::remove_stopped(&fixture.network_identity).unwrap();
    assert!(LocalServiceNetwork::cleanup_complete(&fixture.network_identity).unwrap());
    fs::write(
        &audit,
        format!(
            "identity={}\nport={port}\nstate=revoked\nresult={result:?}\n",
            fixture.network_identity
        ),
    )
    .unwrap();
    assert!(LpacChild::spawn(LpacLaunch {
        network: mode,
        sid: &fixture.sid,
        program: &fixture.program,
        args: &args,
        cwd: &fixture.workspace,
        environment: &fixture.environment
    })
    .is_err());
    println!(
        "{}",
        result.expect("bounded local service and other-port denial")
    );
}

impl Fixture {
    fn new(round: usize) -> Self {
        // No fallback to an old harness directory or global/default temp tree.
        let base = PathBuf::from(
            std::env::var_os("WEBCODEX_LPAC_OBSERVATION_ROOT")
                .expect("set the explicit new validation root"),
        );
        assert!(base.is_absolute());
        assert!(base.is_dir());
        assert!(!fs::symlink_metadata(&base)
            .unwrap()
            .file_type()
            .is_symlink());
        let temporary = tempfile::Builder::new()
            .prefix("observation-")
            .tempdir_in(&base)
            .unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let bin = root.join("bin");
        let workspace = root.join("workspace");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&workspace).unwrap();
        let program = bin.join("process_tree_helper.exe");
        fs::copy(env!("CARGO_BIN_EXE_process_tree_helper"), &program).unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let network_identity = format!(
            "{nonce:032x}{:032x}",
            (std::process::id() as u128) * 1024 + round as u128
        );
        let sid = AppContainerSid::derive(&network_identity).unwrap();
        let sid_string = sid_text(sid.as_ptr());
        let owner = user_sid();
        set_exact_acl(&root, &root, &owner, &sid_string, "GRGX", false);
        set_exact_acl(&root, &bin, &owner, &sid_string, "GRGX", false);
        set_exact_acl(&root, &program, &owner, &sid_string, "GRGX", false);
        set_exact_acl(&root, &workspace, &owner, &sid_string, "GRGWGX", true);
        let windows = std::env::var("SystemRoot").expect("trusted OS installation root");
        let environment = BTreeMap::from([
            ("SystemRoot".into(), windows.clone()),
            ("WINDIR".into(), windows),
            ("PATH".into(), bin.to_str().unwrap().into()),
            ("TEMP".into(), workspace.to_str().unwrap().into()),
            ("TMP".into(), workspace.to_str().unwrap().into()),
            ("HOME".into(), workspace.to_str().unwrap().into()),
            ("USERPROFILE".into(), workspace.to_str().unwrap().into()),
        ]);
        Self {
            _temporary: temporary,
            root,
            program,
            workspace,
            sid,
            environment,
            network_identity,
        }
    }

    fn spawn(&self, args: &[String]) -> LpacChild {
        assert!(self.program.starts_with(&self.root));
        let mut child = LpacChild::spawn(LpacLaunch {
            network: Default::default(),
            sid: &self.sid,
            program: &self.program,
            args,
            cwd: &self.workspace,
            environment: &self.environment,
        })
        .unwrap();
        drop(child.stdin.take());
        // Helpers normally emit no output. Keep stderr for a bounded diagnosis
        // if the synthetic fixture fails before its readiness handshake.
        drop(child.stdout.take());
        assert_eq!(child.observed_member_count(), 1);
        assert_eq!(child.observed_total_processes(), 1);
        child
    }
}

fn wait_markers(paths: &[PathBuf], child: &mut LpacChild, deadline: Instant) {
    while !paths.iter().all(|path| path.is_file()) {
        assert!(
            Instant::now() < deadline,
            "synthetic helper readiness deadline"
        );
        if let Some(code) = child.try_wait().unwrap() {
            let diagnostic = child.stderr.take().map(|stderr| {
                let reader = std::thread::spawn(move || {
                    let mut text = String::new();
                    let _ = stderr.take(4096).read_to_string(&mut text);
                    text
                });
                let deadline = Instant::now() + Duration::from_secs(1);
                while !reader.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if reader.is_finished() {
                    reader.join().unwrap_or_default()
                } else {
                    "stderr did not finish within bounded diagnosis".into()
                }
            });
            panic!("synthetic leader exited before ready: exit={code}, stderr={diagnostic:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn report_members(child: &mut LpacChild, label: &str) {
    let pids = child.process_ids().unwrap();
    let mut names = Vec::new();
    for pid in pids {
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            names.push(format!("{pid}:open-error"));
            continue;
        }
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut text = vec![0u16; 32768];
        let mut length = text.len() as u32;
        if unsafe {
            QueryFullProcessImageNameW(process.as_raw_handle(), 0, text.as_mut_ptr(), &mut length)
        } == 0
        {
            names.push(format!("{pid}:image-error"));
        } else {
            let path = PathBuf::from(String::from_utf16_lossy(&text[..length as usize]));
            names.push(format!(
                "{pid}:{}",
                path.file_name().unwrap().to_string_lossy()
            ));
        }
    }
    eprintln!(
        "{label}: retained={} lifetime_total={} active={} members={names:?}",
        child.observed_member_count(),
        child.observed_total_processes(),
        child.active_processes().unwrap()
    );
}

#[test]
#[ignore = "real LPAC process/ACL lane; requires explicit isolated validation root and approval"]
fn lpac_observation_real_handshake_tree_complete_with_held_handles() {
    let mut complete = 0;
    for round in 0..ROUNDS {
        let fixture = Fixture::new(round);
        let ready = fixture.workspace.join("ready");
        let release = fixture.workspace.join("release");
        let args = vec![
            "lpac-observe-tree".into(),
            ready.to_str().unwrap().into(),
            release.to_str().unwrap().into(),
            "2".into(),
        ];
        let mut child = fixture.spawn(&args);
        let deadline = Instant::now() + Duration::from_secs(10);
        let markers = (0..=2)
            .map(|depth| PathBuf::from(format!("{}.{}", ready.display(), depth)))
            .collect::<Vec<_>>();
        wait_markers(&markers, &mut child, deadline);
        child.capture_member_handles(deadline).unwrap();
        report_members(&mut child, "handshake-ready");
        // Windows may create a conhost for each synthetic helper even with
        // CREATE_NO_WINDOW. Include every Job member in coverage; never filter
        // them out to make the expected helper-only count pass.
        while child.observed_member_count() != child.observed_total_processes() as usize {
            child.capture_member_handles(deadline).unwrap();
        }
        let complete_count = child.observed_total_processes();
        assert!(complete_count >= 3);
        child.terminate_tree().unwrap();
        loop {
            match child.observe_completed_tree(deadline).unwrap() {
                Some(proof) => {
                    assert_eq!(proof.member_count(), complete_count);
                    assert_eq!(child.active_processes().unwrap(), 0);
                    // All retained member handles are still owned by child here.
                    assert_eq!(child.observed_member_count(), complete_count as usize);
                    complete += 1;
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
    eprintln!("lpac_observation handshake rounds={ROUNDS} complete={complete} held_handles_active_zero={complete}");
    assert_eq!(complete, ROUNDS);
}

#[test]
#[ignore = "real LPAC process/ACL lane; deliberately misses terminated members"]
fn lpac_observation_real_missed_short_members_remain_unknown() {
    let mut incomplete = 0;
    for round in 0..ROUNDS {
        let fixture = Fixture::new(round + ROUNDS);
        let done = fixture.workspace.join("short-finished");
        let release = fixture.workspace.join("release");
        let args = vec![
            "lpac-observe-short".into(),
            done.to_str().unwrap().into(),
            release.to_str().unwrap().into(),
            "4".into(),
        ];
        let mut child = fixture.spawn(&args);
        let deadline = Instant::now() + Duration::from_secs(10);
        // No descendant capture until all four short children have really exited.
        wait_markers(&[done], &mut child, deadline);
        child.capture_member_handles(deadline).unwrap();
        report_members(&mut child, "short-completed");
        // The leader's still-running conhost is allowed to be captured too.
        // At least the four deliberately completed child handles are missing;
        // no platform-created process is subtracted from lifetime accounting.
        assert!(child.observed_total_processes() >= 5);
        assert!(child.observed_total_processes() as usize >= child.observed_member_count() + 4);
        child.terminate_tree().unwrap();
        while child.active_processes().unwrap() != 0 || child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(child.observe_completed_tree(deadline).is_err());
        assert_eq!(
            child.member_observation_error(),
            Some("lpac_member_coverage_missing")
        );
        assert!(
            child.observe_completed_tree(deadline).is_err(),
            "missing historical handles cannot be repaired by an empty snapshot"
        );
        incomplete += 1;
    }
    eprintln!("lpac_observation deliberately_missed rounds={ROUNDS} unknown={incomplete} false_complete=0");
    assert_eq!(incomplete, ROUNDS);
}
