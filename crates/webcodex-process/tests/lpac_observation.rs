//! Explicit, ignored real-process probes. All ACL changes and helper execution
//! stay in fresh directories below the operator-selected validation root.
#![cfg(windows)]
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
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
        let sid = AppContainerSid::derive(&format!(
            "WebCodex.Observation.{}.{}.{nonce}",
            std::process::id(),
            round
        ))
        .unwrap();
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
