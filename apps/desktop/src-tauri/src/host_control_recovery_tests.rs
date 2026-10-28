use super::*;

fn stale_carrier(source: &Source) -> (Carrier, PathBuf) {
    crate::bundled_tools::prepare_host_control_directory(source.control_path.parent().unwrap())
        .unwrap();
    let credential = source
        .env_file
        .parent()
        .unwrap()
        .join("old-bootstrap.token");
    std::fs::write(&credential, b"synthetic-old-credential").unwrap();
    (
        Carrier {
            schema_version: 2,
            server_url: source.connection.server_url.clone(),
            bootstrap_token_file: credential.clone(),
            source_ref: format!("desktop-local-bootstrap:{}", source.env_file.display()),
            owner_user: "previous-user-id".into(),
            projects: vec!["agent:runner:previous-project".into()],
        },
        credential,
    )
}

fn backups(source: &Source) -> Vec<PathBuf> {
    std::fs::read_dir(source.control_path.parent().unwrap())
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("control.stale-")
        })
        .collect()
}

#[test]
fn stale_same_source_carrier_is_preserved_and_new_publication_can_start() {
    let (root, source) = super::tests::fixture();
    let (carrier, credential) = stale_carrier(&source);
    let original = serde_json::to_vec(&carrier).unwrap();
    std::fs::write(&source.control_path, &original).unwrap();

    let startup_lock = prepare_owner(
        &source.control_path,
        Some(&source.connection.server_url),
        Some(&source.env_file),
    )
    .unwrap();
    assert!(!source.control_path.exists());
    let saved = backups(&source);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].parent(), source.control_path.parent());
    assert_eq!(std::fs::read(&saved[0]).unwrap(), original);
    assert_eq!(
        std::fs::read(&credential).unwrap(),
        b"synthetic-old-credential"
    );
    drop(startup_lock);

    let publication =
        Publication::create(source.clone(), "current-user-id".into(), vec![]).unwrap();
    let published: Carrier =
        serde_json::from_slice(&std::fs::read(&source.control_path).unwrap()).unwrap();
    assert_eq!(published.owner_user, "current-user-id");
    assert!(published.projects.is_empty());
    assert_ne!(published.bootstrap_token_file, credential);
    drop(publication);
    assert!(
        credential.is_file(),
        "the former credential has no new cleanup owner"
    );
    assert_eq!(std::fs::read(&saved[0]).unwrap(), original);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn publication_creation_also_recovers_same_source_crash_residue() {
    let (root, source) = super::tests::fixture();
    let (carrier, credential) = stale_carrier(&source);
    let original = serde_json::to_vec(&carrier).unwrap();
    std::fs::write(&source.control_path, &original).unwrap();
    let publication =
        Publication::create(source.clone(), "current-user-id".into(), vec![]).unwrap();
    assert_eq!(backups(&source).len(), 1);
    assert!(credential.is_file());
    drop(publication);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn active_publication_owner_is_not_recovered_or_overwritten() {
    let (root, source) = super::tests::fixture();
    let publication =
        Publication::create(source.clone(), "current-user-id".into(), vec![]).unwrap();
    let original = std::fs::read(&source.control_path).unwrap();
    let failure = prepare_owner(
        &source.control_path,
        Some(&source.connection.server_url),
        Some(&source.env_file),
    )
    .unwrap_err();
    assert_eq!(failure.code, "host_control_owner_busy");
    assert_eq!(std::fs::read(&source.control_path).unwrap(), original);
    assert!(backups(&source).is_empty());
    assert!(publication.credential.path().is_file());
    drop(publication);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unrelated_invalid_or_unconfirmed_carriers_remain_untouched() {
    for variant in ["server", "source", "version", "malformed", "unconfirmed"] {
        let (root, source) = super::tests::fixture();
        let (mut carrier, credential) = stale_carrier(&source);
        match variant {
            "server" => carrier.server_url = "http://127.0.0.1:19501".into(),
            "source" => {
                let other = root.join("another-server.env");
                std::fs::write(&other, b"unrelated").unwrap();
                carrier.source_ref = format!("desktop-local-bootstrap:{}", other.display());
            }
            "version" => carrier.schema_version = 3,
            _ => {}
        }
        let original = if variant == "malformed" {
            b"operator replacement".to_vec()
        } else {
            serde_json::to_vec(&carrier).unwrap()
        };
        std::fs::write(&source.control_path, &original).unwrap();
        let failure = prepare_owner(
            &source.control_path,
            Some(&source.connection.server_url),
            (variant != "unconfirmed").then_some(source.env_file.as_path()),
        )
        .unwrap_err();
        assert_eq!(failure.code, "host_control_carrier_conflict", "{variant}");
        assert_eq!(
            std::fs::read(&source.control_path).unwrap(),
            original,
            "{variant}"
        );
        assert!(credential.is_file());
        assert!(backups(&source).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn source_comparison_uses_canonical_paths_and_each_recovery_keeps_its_backup() {
    let (root, source) = super::tests::fixture();
    let (mut carrier, credential) = stale_carrier(&source);
    let directory = root.join("path-spelling");
    std::fs::create_dir(&directory).unwrap();
    carrier.source_ref = format!(
        "desktop-local-bootstrap:{}",
        directory.join("..").join("server.env").display()
    );
    for index in 0..2 {
        carrier.projects = vec![format!("agent:runner:project-{index}")];
        std::fs::write(&source.control_path, serde_json::to_vec(&carrier).unwrap()).unwrap();
        drop(
            prepare_owner(
                &source.control_path,
                Some(&source.connection.server_url),
                Some(&source.env_file),
            )
            .unwrap(),
        );
    }
    assert_eq!(backups(&source).len(), 2);
    assert!(credential.is_file());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn linked_carrier_is_never_recovered() {
    let (root, source) = super::tests::fixture();
    let (carrier, _) = stale_carrier(&source);
    let target = root.join("operator-carrier.json");
    let original = serde_json::to_vec(&carrier).unwrap();
    std::fs::write(&target, &original).unwrap();
    std::os::unix::fs::symlink(&target, &source.control_path).unwrap();
    let failure = prepare_owner(
        &source.control_path,
        Some(&source.connection.server_url),
        Some(&source.env_file),
    )
    .unwrap_err();
    assert_eq!(failure.code, "host_control_source_invalid");
    assert!(source.control_path.is_symlink());
    assert_eq!(std::fs::read(&target).unwrap(), original);
    assert!(backups(&source).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
