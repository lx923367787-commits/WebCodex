//! Explicit Windows filesystem probes in a fresh validation directory. These
//! tests never execute a provider binary or read its credentials/environment.
use super::*;
use std::fs;
use std::os::windows::fs::{symlink_dir, symlink_file, OpenOptionsExt};
use webcodex_core::agent_cli::{
    sha256, AgentCliDeliveryManifest, AgentCliDeliveryManifestFile,
    AGENT_CLI_DELIVERY_FILE_MAX_BYTES, AGENT_CLI_MANIFEST_MAX_BYTES,
};

struct Fixture {
    temporary: tempfile::TempDir,
    binding: AgentExecutionBinding,
    resource_root: PathBuf,
    publish: PathBuf,
}

#[test]
#[ignore = "explicit isolated Windows validation root; no provider or real auth access"]
fn manifest_v2_computes_hashes_and_reuses_path_and_v1_checks() {
    let fixture = Fixture::new();
    let file = fixture.file("files/result.txt", b"UPDATED\n");
    fs::write(
        fixture.publish.join("manifest.json"),
        br#"{"version":2,"summary":"done","files":[{"path":"files/result.txt"}]}"#,
    )
    .unwrap();
    let bundle = fixture.read().unwrap();
    assert_eq!(bundle.files[0].sha256, sha256(b"UPDATED\n"));
    assert_eq!(bundle.files[0].bytes, 8);
    fixture.manifest("done", vec![file]);
    fs::write(fixture.publish.join("files/result.txt"), b"DIFFERENT\n").unwrap();
    assert!(
        fixture.read().is_err(),
        "v1 must still reject the supplied wrong hash/length"
    );
    for path in [
        "../home/private",
        "files/../../home/private",
        "files/CON",
        "C:/private",
    ] {
        fs::write(
            fixture.publish.join("manifest.json"),
            serde_json::to_vec(
                &serde_json::json!({"version":2,"summary":"done","files":[{"path":path}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(fixture.read().is_err());
    }
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(
            std::env::var_os("WEBCODEX_AGENT_EXECUTION_TEST_ROOT")
                .expect("explicit validation root required"),
        );
        assert!(root.is_absolute());
        let temporary = tempfile::Builder::new()
            .prefix("resource-snapshot-")
            .tempdir_in(&root)
            .unwrap();
        assert!(temporary
            .path()
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()));
        let profiles = validated(profile());
        let binding = start(&profiles).binding;
        let resource_root = temporary.path().join("resources");
        let publish = resource_root
            .join(format!("agent-{}", binding.identity_hash().unwrap()))
            .join("workspace/publish");
        fs::create_dir_all(publish.join("files")).unwrap();
        Self {
            temporary,
            binding,
            resource_root,
            publish,
        }
    }

    fn file(&self, relative: &str, bytes: &[u8]) -> AgentCliDeliveryManifestFile {
        webcodex_core::agent_cli::validate_delivery_path(relative).unwrap();
        let path = self.publish.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        AgentCliDeliveryManifestFile {
            path: relative.into(),
            sha256: sha256(bytes),
            bytes: bytes.len() as u64,
        }
    }

    fn manifest(&self, summary: &str, files: Vec<AgentCliDeliveryManifestFile>) {
        let manifest = AgentCliDeliveryManifest {
            version: 1,
            summary: summary.into(),
            files,
        };
        fs::write(
            self.publish.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn read(&self) -> Result<AgentCliDeliveryBundle, String> {
        read_delivery_bundle(&self.binding, &self.resource_root)
    }

    fn auth_copy(&self) -> PathBuf {
        self.resource_root
            .join(format!("agent-{}", self.binding.identity_hash().unwrap()))
            .join("home")
            .join(webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE)
    }
}

#[test]
#[ignore = "explicit isolated Windows validation root; synthetic auth bytes only"]
fn native_auth_cleanup_deletes_only_the_bound_copy_and_is_idempotent() {
    let fixture = Fixture::new();
    cleanup_native_auth(&fixture.binding, &fixture.resource_root).unwrap();
    let target = fixture.auth_copy();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"synthetic copied auth").unwrap();
    let sibling = target.with_file_name("keep-sibling.txt");
    fs::write(&sibling, b"synthetic sibling").unwrap();
    let host = fixture.temporary.path().join("fake-host-auth.txt");
    fs::write(&host, b"synthetic host source").unwrap();
    let mut other = fixture.binding.clone();
    other.executor.task_id = "another-worker-task".into();
    let other_target = fixture
        .resource_root
        .join(format!("agent-{}", other.identity_hash().unwrap()))
        .join("home")
        .join(webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE);
    fs::create_dir_all(other_target.parent().unwrap()).unwrap();
    fs::write(&other_target, b"synthetic other worker").unwrap();
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
        .open(&target)
        .unwrap();
    assert!(cleanup_native_auth(&fixture.binding, &fixture.resource_root).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"synthetic copied auth");
    drop(pin);
    cleanup_native_auth(&fixture.binding, &fixture.resource_root).unwrap();
    cleanup_native_auth(&fixture.binding, &fixture.resource_root).unwrap();
    assert!(!target.exists());
    assert!(target.parent().unwrap().is_dir());
    assert!(fixture.publish.is_dir());
    assert_eq!(fs::read(&sibling).unwrap(), b"synthetic sibling");
    assert_eq!(fs::read(&host).unwrap(), b"synthetic host source");
    assert_eq!(fs::read(&other_target).unwrap(), b"synthetic other worker");
}

#[test]
#[ignore = "explicit isolated Windows validation root; synthetic auth bytes only"]
fn native_auth_cleanup_rejects_hardlinks_and_directory_targets() {
    let fixture = Fixture::new();
    let target = fixture.auth_copy();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"synthetic hardlinked auth").unwrap();
    let alias = fixture.temporary.path().join("synthetic-alias.txt");
    fs::hard_link(&target, &alias).unwrap();
    assert_eq!(
        cleanup_native_auth(&fixture.binding, &fixture.resource_root).unwrap_err(),
        "agent_resources_reparse_or_link_rejected"
    );
    assert_eq!(fs::read(&target).unwrap(), b"synthetic hardlinked auth");
    assert_eq!(fs::read(&alias).unwrap(), b"synthetic hardlinked auth");
    let directory = Fixture::new();
    fs::create_dir_all(directory.auth_copy()).unwrap();
    assert!(cleanup_native_auth(&directory.binding, &directory.resource_root).is_err());
    assert!(directory.auth_copy().is_dir());
}

#[test]
#[ignore = "explicit isolated Windows validation root and symlink privilege; synthetic auth only"]
fn native_auth_cleanup_rejects_reparse_leaf_and_ancestors() {
    let leaf = Fixture::new();
    let source = leaf.temporary.path().join("synthetic-source.txt");
    fs::write(&source, b"synthetic source must survive").unwrap();
    let target = leaf.auth_copy();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    symlink_file(&source, &target).unwrap();
    assert!(cleanup_native_auth(&leaf.binding, &leaf.resource_root).is_err());
    assert_eq!(fs::read(&source).unwrap(), b"synthetic source must survive");
    assert!(fs::symlink_metadata(&target)
        .unwrap()
        .file_type()
        .is_symlink());
    let ancestor = Fixture::new();
    let target = ancestor.auth_copy();
    let parent = target.parent().unwrap();
    fs::create_dir_all(parent.parent().unwrap()).unwrap();
    let external = ancestor.temporary.path().join("synthetic-external-auth");
    fs::create_dir(&external).unwrap();
    let external_file = external.join(target.file_name().unwrap());
    fs::write(&external_file, b"synthetic external must survive").unwrap();
    symlink_dir(&external, parent).unwrap();
    assert!(cleanup_native_auth(&ancestor.binding, &ancestor.resource_root).is_err());
    assert_eq!(
        fs::read(&external_file).unwrap(),
        b"synthetic external must survive"
    );
}

#[test]
#[ignore = "explicit isolated Windows validation root; no provider execution"]
fn catalog_pins_only_the_mapped_verified_executable() {
    let fixture = Fixture::new();
    let executable = fixture.temporary.path().join("catalog-fixture.exe");
    fs::write(&executable, b"synthetic executable never launched").unwrap();
    let p = AgentExecutionResourceProfile {
        id: "catalog-fixture".into(),
        provider: Some(AgentCliProvider::WorkBuddy),
        reviewed_controls: None,
        user_https_proxy: false,
        executable: executable.clone(),
        executable_sha256: sha256(b"synthetic executable never launched"),
        files: vec![],
    };
    let profiles = validated(p.clone());
    let pinned = pin_catalog_executable(&profiles, AgentCliProvider::WorkBuddy).unwrap();
    assert_eq!(pinned.program, executable);
    assert_eq!(
        pinned.descriptor.provider,
        Some(AgentCliProvider::WorkBuddy)
    );
    assert_eq!(
        pinned.descriptor.manifest_hash,
        profiles.manifest(&p.id).unwrap()
    );
    assert!(fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .is_err());
    let renamed = fixture.temporary.path().join("renamed.exe");
    assert!(
        executable.starts_with(fixture.temporary.path())
            && renamed.starts_with(fixture.temporary.path())
    );
    assert!(fs::rename(&executable, &renamed).is_err());
    drop(pinned);
    let mut wrong = p.clone();
    wrong.executable_sha256 = "0".repeat(64);
    assert!(pin_catalog_executable(&validated(wrong), AgentCliProvider::WorkBuddy).is_err());
    assert!(pin_catalog_executable(&profiles, AgentCliProvider::Antigravity).is_err());
    fs::hard_link(&executable, fixture.temporary.path().join("linked.exe")).unwrap();
    assert!(pin_catalog_executable(&profiles, AgentCliProvider::WorkBuddy).is_err());
}

#[test]
#[ignore = "explicit isolated Windows validation root; no provider execution"]
fn delivery_preserves_exact_text_and_reads_only_declared_outputs() {
    let fixture = Fixture::new();
    let b = fixture.file("files/z.txt", "中文\r\nexact\n".as_bytes());
    let a = fixture.file("files/sub/a.md", b"# finished\n");
    fixture.file("files/unlisted.txt", b"UNLISTED_PRIVATE_MARKER");
    fs::write(
        fixture.temporary.path().join("private-memory.txt"),
        b"PRIVATE_MEMORY_MARKER",
    )
    .unwrap();
    fixture.manifest("result summary", vec![b, a]);
    let bundle = fixture.read().unwrap();
    assert_eq!(bundle.files.len(), 2);
    assert_eq!(bundle.files[0].path, "files/sub/a.md");
    assert_eq!(bundle.files[1].text, "中文\r\nexact\n");
    let bytes = bundle.canonical_bytes(&fixture.binding).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("PRIVATE_MARKER") && !text.contains("PRIVATE_MEMORY_MARKER"));
    assert!(!text.contains(fixture.temporary.path().to_str().unwrap()));
    assert_eq!(fixture.read().unwrap(), bundle);
    fixture.manifest("summary only", vec![]);
    assert!(fixture.read().unwrap().files.is_empty());
}

#[test]
#[ignore = "explicit isolated Windows validation root; no provider execution"]
fn delivery_rejects_half_writes_invalid_text_aliases_and_size_limits() {
    let fixture = Fixture::new();
    let file = fixture.file("files/result.txt", b"complete");
    fixture.manifest("", vec![file.clone()]);
    fs::write(fixture.publish.join(&file.path), b"part").unwrap();
    assert!(fixture.read().is_err());
    for bytes in [&b"nul\0text"[..], &b"\xffinvalid"[..]] {
        let file = fixture.file("files/result.txt", bytes);
        fixture.manifest("", vec![file]);
        assert!(fixture.read().is_err());
    }
    let file = fixture.file("files/result.txt", b"complete");
    let mut alias = file.clone();
    alias.path = "files/RESULT.txt".into();
    fixture.manifest("", vec![file.clone(), alias]);
    assert!(fixture.read().is_err());
    let mut invalid = file.clone();
    invalid.path = "files/../../home/private.txt".into();
    fixture.manifest("", vec![invalid]);
    assert!(fixture.read().is_err());
    let too_large = fixture.file(
        "files/result.txt",
        &vec![b'x'; AGENT_CLI_DELIVERY_FILE_MAX_BYTES + 1],
    );
    fixture.manifest("", vec![too_large]);
    assert!(fixture.read().is_err());
    fs::write(
        fixture.publish.join("manifest.json"),
        vec![b' '; AGENT_CLI_MANIFEST_MAX_BYTES + 1],
    )
    .unwrap();
    assert!(fixture.read().is_err());
    fixture.manifest("", vec![]);
    assert!(fixture.read().is_err());
}

#[test]
#[ignore = "explicit isolated Windows validation root; no provider execution"]
fn delivery_rejects_held_writers_and_hardlinked_manifest_or_files() {
    let fixture = Fixture::new();
    let file = fixture.file("files/result.txt", b"complete");
    fixture.manifest("", vec![file.clone()]);
    let writer = fs::OpenOptions::new()
        .write(true)
        .share_mode(7)
        .open(fixture.publish.join(&file.path))
        .unwrap();
    assert!(fixture.read().is_err());
    drop(writer);
    assert!(fixture.read().is_ok());
    fs::hard_link(
        fixture.publish.join(&file.path),
        fixture.temporary.path().join("private-alias"),
    )
    .unwrap();
    assert!(fixture.read().is_err());
    fixture.manifest("summary only", vec![]);
    fs::hard_link(
        fixture.publish.join("manifest.json"),
        fixture.temporary.path().join("manifest-alias"),
    )
    .unwrap();
    assert!(fixture.read().is_err());
}

#[test]
#[ignore = "explicit isolated Windows validation root and symlink privilege; no provider execution"]
fn delivery_rejects_reparse_leaf_parent_and_manifest() {
    let fixture = Fixture::new();
    let private = fixture.temporary.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::write(private.join("secret.txt"), b"PRIVATE").unwrap();
    symlink_file(
        private.join("secret.txt"),
        fixture.publish.join("files/linked.txt"),
    )
    .unwrap();
    let entry = AgentCliDeliveryManifestFile {
        path: "files/linked.txt".into(),
        bytes: 7,
        sha256: sha256(b"PRIVATE"),
    };
    fixture.manifest("", vec![entry]);
    assert!(fixture.read().is_err());
    symlink_dir(&private, fixture.publish.join("files/linked-dir")).unwrap();
    let entry = AgentCliDeliveryManifestFile {
        path: "files/linked-dir/secret.txt".into(),
        bytes: 7,
        sha256: sha256(b"PRIVATE"),
    };
    fixture.manifest("", vec![entry]);
    assert!(fixture.read().is_err());
    let second = Fixture::new();
    let external_manifest = second.temporary.path().join("external-manifest.json");
    fs::write(
        &external_manifest,
        br#"{"version":1,"summary":"secret","files":[]}"#,
    )
    .unwrap();
    symlink_file(external_manifest, second.publish.join("manifest.json")).unwrap();
    assert!(second.read().is_err());
}

#[test]
#[ignore = "synthetic reviewed subject in explicit validation root; no provider execution"]
fn reviewed_catalog_rechecks_and_pins_the_entire_readonly_payload() {
    use webcodex_core::agent_cli_controls::{
        ReviewedAgentCliControls, ReviewedControlArg, ReviewedControlFile,
    };
    let fixture = Fixture::new();
    let executable = fixture.temporary.path().join("synthetic-provider.exe");
    let control = fixture.temporary.path().join("synthetic-control.json");
    let dependency = fixture.temporary.path().join("synthetic-runtime.dll");
    fs::write(&executable, b"never executed").unwrap();
    fs::write(&control, b"synthetic readonly policy").unwrap();
    fs::write(&dependency, b"synthetic dependency").unwrap();
    let mut p = AgentExecutionResourceProfile {
        id: "reviewed-fixture".into(),
        provider: Some(AgentCliProvider::WorkBuddy),
        reviewed_controls: None,
        user_https_proxy: false,
        executable,
        executable_sha256: sha256(b"never executed"),
        files: vec![
            AgentExecutionResourceFile {
                source: control.clone(),
                destination: "input/control.json".into(),
                sha256: sha256(b"synthetic readonly policy"),
            },
            AgentExecutionResourceFile {
                source: dependency.clone(),
                destination: "bin/runtime.dll".into(),
                sha256: sha256(b"synthetic dependency"),
            },
        ],
    };
    let payload = validated(p.clone()).manifest(&p.id).unwrap().to_owned();
    p.reviewed_controls = Some(ReviewedAgentCliControls {
        version: 1,
        record_id: "synthetic-unverified-record".into(),
        provider: AgentCliProvider::WorkBuddy,
        cli_version: "synthetic-0.0.0".into(),
        executable_sha256: p.executable_sha256.clone(),
        resource_set_id: p.id.clone(),
        payload_sha256: payload,
        control_files: vec![ReviewedControlFile {
            path: "input/control.json".into(),
            sha256: p.files[0].sha256.clone(),
        }],
        loading_method: "synthetic-unverified-mechanism".into(),
        fixed_args: vec![ReviewedControlArg {
            flag: "--fixture-control".into(),
            value: Some("input/control.json".into()),
        }],
        verification_evidence_sha256: "d".repeat(64),
        independent_review_sha256: "e".repeat(64),
    });
    let profiles = validated(p);
    profiles.verify_sources().unwrap();
    let pinned = pin_catalog_executable(&profiles, AgentCliProvider::WorkBuddy).unwrap();
    assert!(pinned.descriptor.reviewed_controls.is_some());
    for file in [&control, &dependency] {
        assert!(fs::OpenOptions::new().write(true).open(file).is_err());
    }
    drop(pinned);
    fs::write(&control, b"changed after observation").unwrap();
    assert!(profiles.verify_sources().is_err());
    assert!(pin_catalog_executable(&profiles, AgentCliProvider::WorkBuddy).is_err());
    fs::write(&control, b"synthetic readonly policy").unwrap();
    fs::write(&dependency, b"changed non-control payload").unwrap();
    assert!(profiles.verify_sources().is_err());
    assert!(pin_catalog_executable(&profiles, AgentCliProvider::WorkBuddy).is_err());
}
