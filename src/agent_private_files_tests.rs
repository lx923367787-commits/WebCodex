use super::*;
use webcodex_store::Database;

fn fixture() -> (
    tempfile::TempDir,
    AgentPrivateFiles,
    VerifiedAgentRun,
    VerifiedAgentRun,
) {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(&temp.path().join("state.db")).unwrap();
    db.init_agent_isolation_tables().unwrap();
    let first = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let second = db
        .create_isolated_agent("owner-a", "project-a", None)
        .unwrap();
    let first = db.authenticate_agent_run(&first.token).unwrap();
    let second = db.authenticate_agent_run(&second.token).unwrap();
    let files = AgentPrivateFiles::new(temp.path()).unwrap();
    files.create(&first).unwrap();
    files.create(&second).unwrap();
    (temp, files, first, second)
}

#[test]
fn agent_private_files_two_agents_have_distinct_storage_and_bounded_projections() {
    let (_temp, files, first, second) = fixture();
    let a = files
        .write(&first, "memory/note.txt", b"first sentinel\nneedle", None)
        .unwrap();
    files
        .write(&second, "memory/note.txt", b"second sentinel", None)
        .unwrap();
    assert_eq!(
        files.read(&first, "memory/note.txt").unwrap().bytes,
        b"first sentinel\nneedle"
    );
    assert_eq!(
        files.read(&second, "memory/note.txt").unwrap().bytes,
        b"second sentinel"
    );
    let listing = files.list(&first, "", 8).unwrap();
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].path, "memory/note.txt");
    assert_eq!(listing[0].sha256, a.sha256);
    let found = files.search(&first, "memory", "needle", 8).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].line, 2);
    assert!(files.search(&second, "", "needle", 8).unwrap().is_empty());
    let json = serde_json::to_string(&listing).unwrap();
    assert!(!json.contains(first.agent_id()));
    assert!(!json.contains("runtime"));
}

#[test]
fn agent_private_files_updates_require_cas_and_publish_whole_snapshots() {
    let (_temp, files, first, _) = fixture();
    let previous = files
        .write(&first, "checkpoints/state.json", b"old snapshot", None)
        .unwrap();
    for hash in [
        None,
        Some("0000000000000000000000000000000000000000000000000000000000000000"),
    ] {
        assert_eq!(
            files
                .write(&first, "checkpoints/state.json", b"wrong", hash)
                .err()
                .unwrap(),
            "agent_private_hash_conflict"
        );
        assert_eq!(
            files.read(&first, "checkpoints/state.json").unwrap().bytes,
            b"old snapshot"
        );
    }
    let bytes = vec![b'a'; MAX_FILE_BYTES];
    let updated = files
        .write(
            &first,
            "checkpoints/state.json",
            &bytes,
            Some(&previous.sha256),
        )
        .unwrap();
    assert_eq!(updated.bytes, bytes);
    assert_eq!(files.list(&first, "checkpoints", 8).unwrap().len(), 1);
    assert!(files
        .write(&first, "memory/missing", b"x", Some(&previous.sha256))
        .is_err());
}

#[test]
fn agent_private_files_concurrent_cas_has_one_winner() {
    let (_temp, files, first, _) = fixture();
    let original = files
        .write(&first, "tasks/state", b"initial", None)
        .unwrap();
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            files.write(
                &first,
                "tasks/state",
                b"complete first snapshot",
                Some(&original.sha256),
            )
        });
        let b = scope.spawn(|| {
            files.write(
                &first,
                "tasks/state",
                b"complete second snapshot",
                Some(&original.sha256),
            )
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        files.read(&first, "tasks/state").unwrap().bytes,
        winner.bytes
    );
}

#[test]
fn agent_private_files_symbolic_file_links_are_not_followed() {
    let (_temp, files, first, second) = fixture();
    files
        .write(&second, "memory/sentinel", b"second-only", None)
        .unwrap();
    let target = files.private_path(&second).unwrap().join("memory/sentinel");
    let link = files.private_path(&first).unwrap().join("workspace/alias");
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file(&target, &link) {
        if error.raw_os_error() == Some(1314) {
            eprintln!("SYMLINK TEST UNVERIFIED: host lacks symbolic-link privilege; junction and hardlink tests remain mandatory");
            return;
        }
        panic!("synthetic file symlink creation failed: {error}");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(files.read(&first, "workspace/alias").is_err());
    assert!(files
        .write(&first, "workspace/alias", b"bad", None)
        .is_err());
    assert!(files.list(&first, "workspace", 8).is_err());
    assert_eq!(fs::read(target).unwrap(), b"second-only");
    fs::remove_file(&link).unwrap();
}

#[test]
fn agent_private_files_reject_windows_aliases_and_traversal_before_io() {
    let (_temp, files, first, second) = fixture();
    let sibling = format!("memory/../../{}/private/memory/note.txt", second.agent_id());
    for path in [
        "",
        "identity",
        "../memory/x",
        "/memory/x",
        "memory//x",
        "memory/./x",
        "memory/../x",
        "memory\\x",
        "C:/memory/x",
        "//server/share/x",
        "\\\\?\\C:\\x",
        "memory/x:stream",
        "memory/con",
        "memory/con.txt",
        "memory/com1.txt",
        "memory/lpt9",
        "memory/x.",
        "memory/x ",
        "Memory/X.txt",
        "memory/CoN.txt",
        "memory/COM1.log",
        "memory/LpT9",
        "memory/AuX",
        "memory/NUL.md",
        "private/memory/x",
        sibling.as_str(),
    ] {
        assert!(
            files.write(&first, path, b"x", None).is_err(),
            "accepted {path}"
        );
        assert!(files.read(&first, path).is_err(), "read {path}");
    }
    assert!(files.list(&first, "../", 1).is_err());
    assert!(files.list(&first, "", 0).is_err());
    assert!(files.list(&first, "", 129).is_err());
    assert!(files.search(&first, "", "", 1).is_err());
}

#[test]
fn agent_private_files_common_source_names_and_dotfiles_round_trip() {
    let (_temp, files, first, _) = fixture();
    let names = [
        "README.md",
        "Cargo.toml",
        "AGENTS.md",
        ".gitignore",
        ".config/Settings.json",
    ];
    for name in names {
        let path = format!("workspace/{name}");
        let content = format!("compatible filename: {name}");
        files
            .write(&first, &path, content.as_bytes(), None)
            .unwrap();
        assert_eq!(files.read(&first, &path).unwrap().bytes, content.as_bytes());
    }
    let mut expected = names.map(|name| format!("workspace/{name}")).to_vec();
    expected.sort();
    let listed = files.list(&first, "workspace", 8).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
        expected
    );
    let found = files
        .search(&first, "workspace", "compatible filename:", 8)
        .unwrap();
    assert_eq!(
        found
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(files.read(&first, "Workspace/README.md").is_err());
}

#[cfg(windows)]
#[test]
fn agent_private_files_windows_case_aliases_cannot_bypass_cas() {
    let (_temp, files, first, _) = fixture();
    let original = files
        .write(&first, "workspace/README.md", b"first", None)
        .unwrap();
    assert_eq!(
        files.read(&first, "workspace/readme.MD").unwrap().bytes,
        b"first"
    );
    for expected in [
        None,
        Some("0000000000000000000000000000000000000000000000000000000000000000"),
    ] {
        assert_eq!(
            files
                .write(&first, "workspace/readme.md", b"wrong", expected)
                .err()
                .unwrap(),
            "agent_private_hash_conflict"
        );
    }
    assert_eq!(
        files.read(&first, "workspace/README.md").unwrap().bytes,
        b"first"
    );
    files
        .write(
            &first,
            "workspace/readme.md",
            b"complete replacement",
            Some(&original.sha256),
        )
        .unwrap();
    assert_eq!(
        files.read(&first, "workspace/README.md").unwrap().bytes,
        b"complete replacement"
    );
    assert_eq!(files.list(&first, "workspace", 8).unwrap().len(), 1);
}

#[test]
fn agent_private_files_staging_case_aliases_are_not_public_paths() {
    let (_temp, files, first, _) = fixture();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let original = format!(".snapshot-{id}");
    let staging = files
        .private_path(&first)
        .unwrap()
        .join("memory")
        .join(&original);
    fs::write(&staging, b"unpublished staging content").unwrap();
    for name in [
        original,
        format!(".SNAPSHOT-{id}"),
        format!(".Snapshot-{}", id.to_uppercase()),
        ".snapshot-not-a-uuid".into(),
    ] {
        let path = format!("memory/{name}");
        assert!(
            files.read(&first, &path).is_err(),
            "read internal path {path}"
        );
        assert!(
            files.write(&first, &path, b"replacement", None).is_err(),
            "write internal path {path}"
        );
    }
    assert!(files.list(&first, "memory", 8).unwrap().is_empty());
    assert!(files
        .search(&first, "memory", "unpublished", 8)
        .unwrap()
        .is_empty());
    assert_eq!(fs::read(staging).unwrap(), b"unpublished staging content");
}

#[test]
fn agent_private_files_hardlinks_cannot_read_or_overwrite_another_agent() {
    let (_temp, files, first, second) = fixture();
    let secret = files
        .write(&second, "memory/sentinel", b"second-only", None)
        .unwrap();
    let target = files.private_path(&second).unwrap().join("memory/sentinel");
    let link = files.private_path(&first).unwrap().join("memory/alias");
    fs::hard_link(&target, &link).unwrap();
    assert_eq!(
        files.read(&first, "memory/alias").err().unwrap(),
        "agent_private_hardlink_rejected"
    );
    assert!(files
        .write(&first, "memory/alias", b"changed", Some(&secret.sha256))
        .is_err());
    assert!(files.list(&first, "memory", 8).is_err());
    assert!(files.search(&first, "memory", "second", 8).is_err());
    assert_eq!(fs::read(target).unwrap(), b"second-only");
}

#[test]
fn agent_private_files_reparse_directory_cannot_escape_private_root() {
    let (_temp, files, first, second) = fixture();
    files
        .write(&second, "memory/sentinel", b"second-only", None)
        .unwrap();
    let target = files.private_path(&second).unwrap().join("memory");
    // cmd's mklink treats a forward-slash path suffix as another switch.
    let link = files
        .private_path(&first)
        .unwrap()
        .join("workspace")
        .join("alias");
    #[cfg(windows)]
    {
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "synthetic junction creation failed: status={} stderr={:?}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(files.read(&first, "workspace/alias/sentinel").is_err());
    assert!(files
        .write(&first, "workspace/alias/new", b"bad", None)
        .is_err());
    assert!(files.list(&first, "workspace", 8).is_err());
    assert!(files.search(&first, "workspace", "second", 8).is_err());
    assert!(!target.join("new").exists());
    assert_eq!(
        files.read(&second, "memory/sentinel").unwrap().bytes,
        b"second-only"
    );
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
}

#[test]
fn agent_private_files_rejects_linked_trusted_root_and_regular_parent() {
    let (temp, files, first, _) = fixture();
    let target = temp.path().join("external");
    fs::create_dir(&target).unwrap();
    let link = temp.path().join("root-alias");
    #[cfg(windows)]
    {
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "synthetic root junction creation failed"
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let linked = AgentPrivateFiles::new(&link).unwrap();
    assert!(linked.create(&first).is_err());
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    files
        .write(&first, "memory/ordinary", b"file", None)
        .unwrap();
    assert!(files
        .write(&first, "memory/ordinary/child", b"bad", None)
        .is_err());
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
}

#[test]
fn agent_private_files_size_and_scan_limits_fail_without_partial_output() {
    let (_temp, files, first, _) = fixture();
    assert!(files
        .write(&first, "memory/large", &vec![0; MAX_FILE_BYTES + 1], None)
        .is_err());
    assert!(!files
        .private_path(&first)
        .unwrap()
        .join("memory/large")
        .exists());
    files
        .write(&first, "memory/a", b"match\nmatch", None)
        .unwrap();
    files.write(&first, "memory/b", b"match", None).unwrap();
    assert_eq!(
        files.list(&first, "memory", 1).err().unwrap(),
        "agent_private_result_limit"
    );
    assert_eq!(
        files.search(&first, "memory", "match", 1).err().unwrap(),
        "agent_private_result_limit"
    );
    fs::write(
        files.private_path(&first).unwrap().join("memory/oversized"),
        vec![0; MAX_FILE_BYTES + 1],
    )
    .unwrap();
    assert!(files.read(&first, "memory/oversized").is_err());
}

#[test]
fn agent_private_files_unpublished_staging_is_hidden_without_erasing_evidence() {
    let (_temp, files, first, _) = fixture();
    files
        .write(&first, "memory/published", b"complete", None)
        .unwrap();
    let name = format!(".snapshot-{}", uuid::Uuid::new_v4().simple());
    let staging = files
        .private_path(&first)
        .unwrap()
        .join("memory")
        .join(&name);
    fs::write(&staging, b"unpublished half snapshot").unwrap();
    let listed = files.list(&first, "memory", 8).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, "memory/published");
    assert!(files
        .search(&first, "memory", "half", 8)
        .unwrap()
        .is_empty());
    assert_eq!(
        files.read(&first, "memory/published").unwrap().bytes,
        b"complete"
    );
    assert!(files.read(&first, &format!("memory/{name}")).is_err());
    assert!(files
        .write(&first, &format!("memory/{name}"), b"replacement", None)
        .is_err());
    assert_eq!(fs::read(&staging).unwrap(), b"unpublished half snapshot");
    let invalid = files
        .private_path(&first)
        .unwrap()
        .join("memory/.snapshot-not-a-uuid");
    fs::write(invalid, b"not an internal name").unwrap();
    assert!(files.list(&first, "memory", 8).is_err());
}

#[test]
fn agent_private_files_staging_names_do_not_hide_links_or_directories() {
    let (_temp, files, first, second) = fixture();
    files
        .write(&second, "memory/sentinel", b"second-only", None)
        .unwrap();
    let target = files.private_path(&second).unwrap().join("memory/sentinel");
    let name = format!(".snapshot-{}", uuid::Uuid::new_v4().simple());
    let staging = files
        .private_path(&first)
        .unwrap()
        .join("memory")
        .join(name);
    fs::hard_link(&target, &staging).unwrap();
    assert_eq!(
        files.list(&first, "memory", 8).err().unwrap(),
        "agent_private_hardlink_rejected"
    );
    fs::remove_file(&staging).unwrap();
    fs::create_dir(&staging).unwrap();
    assert!(files.list(&first, "memory", 8).is_err());
    assert_eq!(fs::read(target).unwrap(), b"second-only");
}

#[cfg(windows)]
#[test]
fn agent_private_files_directory_handles_prevent_replacement_during_io() {
    let (temp, files, first, _) = fixture();
    let path = files.private_path(&first).unwrap().join("memory");
    let mut directory = Directory::open(&path, false).unwrap();
    assert!(fs::rename(&path, temp.path().join("moved-memory")).is_err());
    directory.publish("sentinel", b"owned", false).unwrap();
    assert_eq!(directory.read("sentinel").unwrap(), b"owned");
}
