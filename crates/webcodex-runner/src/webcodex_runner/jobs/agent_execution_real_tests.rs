//! Explicit Windows native lane. Synthetic resources/processes only; no model,
//! network, GUI, live server or Store authorization is simulated as real proof.
#![cfg(windows)]
use super::*;
use crate::webcodex_runner::jobs::agent_resources::{
    validate_profiles, AgentExecutionResourceFile, AgentExecutionResourceProfile,
    AgentExecutionResourcesConfig,
};
use crate::webcodex_runner::{RunnerPolicy, ShellConfig, SshConfig};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use webcodex_core::agent_cli::AgentCliProvider;
use webcodex_core::runner_operation::{
    RunnerInvocation, RunnerInvocationMetadata, RunnerJobIsolatedProcessOperation, RunnerOperation,
};
use webcodex_core::runner_protocol::{
    compute_launch_intent_hash, compute_members_sha256, isolated_job_context,
    AgentExecutionCloseRef, AgentExecutionProcess, RunnerEnvelope, PROCESS_STDIN_MAX_BYTES,
};

#[path = "agent_cli_real_tests.rs"]
mod agent_cli_tests;

struct Fixture {
    manager: JobManager,
    configuration: AgentExecutionConfiguration,
    profiles: ValidatedResourceProfiles,
    sink: RunnerSink,
    receiver: tokio::sync::mpsc::Receiver<RunnerEnvelope>,
    program: PathBuf,
    input: PathBuf,
    outside: PathBuf,
    metadata_root: PathBuf,
    namespace: String,
    _temporary: tempfile::TempDir,
}

fn regular_path(path: &Path, directory: bool) {
    use std::os::windows::fs::MetadataExt;
    assert!(path.is_absolute());
    let metadata = fs::symlink_metadata(path).unwrap();
    assert_eq!(metadata.file_attributes() & 0x400, 0, "no reparse path");
    assert_eq!(metadata.is_dir(), directory);
}

fn rename_target(path: &Path) -> PathBuf {
    let base = PathBuf::from(std::env::var_os("WEBCODEX_AGENT_EXECUTION_TEST_ROOT").unwrap());
    let resolved_base = base.canonicalize().unwrap();
    let resolved = path.canonicalize().unwrap();
    assert!(
        resolved.starts_with(&resolved_base) && resolved != resolved_base,
        "rename is confined to a newly created validation child"
    );
    regular_path(path, path.is_dir());
    let target = path.with_file_name(format!(
        "{}.probe-moved",
        path.file_name().unwrap().to_string_lossy()
    ));
    assert!(!target.exists());
    assert!(target
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .starts_with(resolved_base));
    target
}

fn assert_pinned_rename(path: &Path) {
    let target = rename_target(path);
    match fs::rename(path, &target) {
        Ok(()) => {
            // Both exact paths are fresh validation artifacts. Restore before
            // failing so a missing pin cannot orphan the TempDir's own root.
            fs::rename(&target, path).unwrap();
            panic!("retained pin allowed validation path rename");
        }
        Err(error) => assert_eq!(
            error.raw_os_error(),
            Some(32),
            "rename must fail due to sharing protection"
        ),
    }
}

fn assert_released_rename(path: &Path) {
    let target = rename_target(path);
    fs::rename(path, &target).expect("same rename must work after pins are released");
    fs::rename(&target, path).unwrap();
}

impl Fixture {
    fn new() -> Self {
        Self::new_with_provider(None)
    }

    fn new_with_provider(provider: Option<AgentCliProvider>) -> Self {
        let base = PathBuf::from(
            std::env::var_os("WEBCODEX_AGENT_EXECUTION_TEST_ROOT")
                .expect("explicit new validation root is required"),
        );
        let helper = PathBuf::from(
            std::env::var_os("WEBCODEX_AGENT_EXECUTION_TEST_HELPER")
                .expect("explicit newly compiled stage helper is required"),
        );
        regular_path(&base, true);
        regular_path(&helper, false);
        let temporary = tempfile::Builder::new()
            .prefix("runner-native-")
            .tempdir_in(&base)
            .unwrap();
        let namespace = temporary
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let program = temporary.path().join("synthetic-helper.exe");
        fs::copy(&helper, &program).unwrap();
        let input = temporary.path().join("authorized-input.txt");
        fs::write(&input, b"authorized-input").unwrap();
        let other = temporary.path().join("other-agent");
        fs::create_dir(&other).unwrap();
        let outside = other.join("private-sentinel.txt");
        fs::write(&outside, b"other-private-sentinel").unwrap();
        let profiles = validate_profiles(&AgentExecutionResourcesConfig {
            profiles: vec![AgentExecutionResourceProfile {
                id: "synthetic-profile".into(),
                provider,
                reviewed_controls: None,
                user_https_proxy: false,
                codebuddy_config_directory: None,
                executable: program.clone(),
                executable_sha256: format!("{:x}", Sha256::digest(fs::read(&program).unwrap())),
                files: ["input/authorized.txt", "input/nested/authorized.txt"]
                    .into_iter()
                    .map(|destination| AgentExecutionResourceFile {
                        source: input.clone(),
                        destination: PathBuf::from(destination),
                        sha256: format!("{:x}", Sha256::digest(b"authorized-input")),
                    })
                    .collect(),
            }],
        })
        .unwrap();
        let metadata_root = temporary.path().join("metadata");
        let manager = JobManager::new(1);
        // Production configuration must create and safely pin the resource
        // sibling. The fixture never pre-creates that missing production root.
        manager
            .configure_agent_execution_resources(profiles.clone(), metadata_root.clone())
            .unwrap();
        assert!(manager.agent_execution_ready());
        let configuration = lock_unpoison(&manager.agent_execution)
            .configuration
            .clone()
            .unwrap();
        assert!(configuration.resource_root.is_dir());
        assert!(
            fs::rename(
                &configuration.resource_root,
                temporary.path().join("moved-resource-root")
            )
            .is_err(),
            "configured ancestors/root must remain pinned against rename"
        );
        let (tx, receiver) = tokio::sync::mpsc::channel(256);
        Self {
            manager,
            configuration,
            profiles,
            sink: RunnerSink::WebSocket {
                tx,
                client_id: "native-runner".into(),
                runner_instance_id: "native-instance".into(),
            },
            receiver,
            program,
            input,
            outside,
            metadata_root,
            namespace,
            _temporary: temporary,
        }
    }

    fn start(&self, index: usize, args: Vec<String>, stdin: Option<String>) -> AgentExecutionStart {
        let process = AgentExecutionProcess {
            // Frozen catalog identity, never a host path or a fresh hash of a
            // potentially modified source. Negative cases must retain it too.
            executable: self
                .profiles
                .descriptors()
                .into_iter()
                .find(|descriptor| descriptor.resource_set_id == "synthetic-profile")
                .unwrap()
                .executable_identity,
            args,
        };
        let binding = serde_json::from_value(json!({
            "version":1, "project":"synthetic-current-goal-project",
            "requester": {
                "enrollment_id":"logical-agent-enrollment",
                "run":{"agent_id":"logical-agent", "agent_run_id":format!("{}-logical-run-one",self.namespace), "generation":1},
                "root_task_id":"original-goal-task", "source_task_id":"original-goal-task", "call_index":0,
                "cli_attempt_id":format!("native-attempt-{index:03}")
            },
            "executor": {
                "enrollment_id":format!("cli-worker-enrollment-{index}"),
                "run":{"agent_id":format!("cli-worker-{index}"), "agent_run_id":format!("{}-cli-worker-run-{index}",self.namespace), "generation":1},
                "task_id":format!("cli-worker-task-{index}"), "cli_session_scope_id":format!("cli-worker-session-{index}")
            },
            "target":{"client_id":"native-runner", "runner_instance_id":"native-instance"},
            "carrier":{"kind":"job", "job_id":format!("native-job-{index}"), "start_request_id":format!("native-request-{index}")},
            "launch_intent_hash":compute_launch_intent_hash(&process, stdin.as_deref(), 60).unwrap(),
            "resource_manifest_hash":self.profiles.manifest("synthetic-profile").unwrap()
        })).unwrap();
        let start = AgentExecutionStart {
            workspace_source: None,
            binding,
            resource_set_id: "synthetic-profile".into(),
            process,
            stdin,
            timeout_secs: 60,
        };
        start.validate().unwrap();
        start
    }

    fn pending(&self, start: &AgentExecutionStart) -> PendingJobStart {
        let AgentExecutionCarrier::Job {
            start_request_id, ..
        } = &start.binding.carrier
        else {
            unreachable!()
        };
        // The same canonical wire codec used by transport must accept the
        // operation; the test does not call the native worker directly.
        let wire = RunnerInvocation {
            metadata: RunnerInvocationMetadata {
                request_id: start_request_id.clone(),
                client_id: "native-runner".into(),
                requested_by: "synthetic-trusted-server".into(),
                created_at: chrono::Utc::now().timestamp(),
            },
            operation: RunnerOperation::Job(RunnerJobOperation::StartIsolatedProcess(
                RunnerJobIsolatedProcessOperation {
                    context: isolated_job_context(start),
                    start: start.clone(),
                },
            )),
        }
        .into_v2_request()
        .unwrap();
        PendingJobStart::from_wire(
            1,
            RunnerPolicy::default(),
            ShellConfig::default(),
            SshConfig::default(),
            self._temporary.path().join("unused-registry"),
            wire,
        )
    }

    fn enqueue(&self, start: &AgentExecutionStart) {
        self.manager.enqueue(self.sink.clone(), self.pending(start));
    }

    fn workspace(&self, start: &AgentExecutionStart) -> PathBuf {
        self.configuration
            .resource_root
            .join(format!("agent-{}", start.binding.identity_hash().unwrap()))
            .join("workspace")
    }

    fn assert_pinned_tree(&self, start: &AgentExecutionStart) {
        let execution = self.workspace(start).parent().unwrap().to_owned();
        for path in [
            self._temporary.path().to_owned(),
            self.configuration.resource_root.clone(),
            execution.clone(),
            execution.join("bin"),
            execution.join("input"),
            execution.join("input/nested"),
            execution.join("workspace"),
            execution.join("bin/program.exe"),
            execution.join("input/authorized.txt"),
        ] {
            assert_pinned_rename(&path);
        }
        let replacement = self._temporary.path().join("replacement-input.tmp");
        fs::write(&replacement, b"replacement-must-not-publish").unwrap();
        let target = execution.join("input/nested/authorized.txt");
        let error = fs::rename(&replacement, &target)
            .expect_err("pinned immutable file cannot be replaced");
        assert!(
            matches!(error.raw_os_error(), Some(5) | Some(32)),
            "replacement must fail with access/sharing denial: {error}"
        );
        assert_eq!(fs::read(&target).unwrap(), b"authorized-input");
        assert_eq!(
            fs::read(&replacement).unwrap(),
            b"replacement-must-not-publish"
        );
    }

    fn wait_ready(&self, start: &AgentExecutionStart, paths: &[PathBuf], minimum_members: usize) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let child = lock_unpoison(&self.manager.agent_execution)
                .live
                .get(job_id(&start.binding).unwrap())
                .and_then(|custody| custody.child.clone());
            let observed = child.as_ref().is_some_and(|child| {
                let mut child = lock_unpoison(child);
                let JobChild::Isolated(native) = &mut *child else {
                    return false;
                };
                assert!(
                    native.member_observation_error().is_none(),
                    "running member observation tainted"
                );
                native.observed_member_count() >= minimum_members
                    && native.observed_member_count() == native.observed_total_processes() as usize
                    && native.observed_member_count() == native.active_processes().unwrap() as usize
                    && native.try_wait().unwrap().is_none()
            });
            if observed && paths.iter().all(|path| path.is_file()) {
                return;
            }
            let snapshot = lock_unpoison(&self.manager.jobs)
                .get(job_id(&start.binding).unwrap())
                .map(|job| job.snapshot.clone());
            assert!(
                Instant::now() < deadline,
                "native readiness deadline; snapshot={snapshot:?}"
            );
            if let Some(snapshot) = snapshot {
                assert!(
                    !super::super::lifecycle::runner_job_is_terminal(&snapshot.status),
                    "native start failed: {snapshot:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn close(&self, members: &[AgentExecutionStart]) -> AgentExecutionClose {
        let bindings = members
            .iter()
            .map(|start| start.binding.clone())
            .collect::<Vec<_>>();
        let close = AgentExecutionClose {
            version: 1,
            close_ref: AgentExecutionCloseRef {
                close_id: "native-close".into(),
                source: members[0].binding.requester.run.clone(),
            },
            target: members[0].binding.target.clone(),
            deadline_at_ms: chrono::Utc::now().timestamp_millis() + 15_000,
            members_sha256: compute_members_sha256(&bindings).unwrap(),
            members: bindings,
        };
        close.validate().unwrap();
        close
    }

    fn assert_tree_receipt(
        &self,
        start: &AgentExecutionStart,
        receipt: &AgentExecutionStopReceipt,
    ) {
        assert_eq!(
            receipt.binding_sha256,
            start.binding.binding_hash().unwrap()
        );
        assert!(matches!(
            receipt.outcome,
            AgentExecutionStopOutcome::TreeExited {
                leader_exit_code: Some(_),
                leader_handle_signaled: true,
                membership_complete: true,
                member_handles_signaled: true,
                worker_finished: true,
            }
        ));
        let runtime = lock_unpoison(&self.manager.agent_execution);
        let custody = runtime.live.get(job_id(&start.binding).unwrap()).unwrap();
        assert!(custody.worker_joined && custody.helpers_finished);
        assert!(custody.worker.is_none());
        assert!(
            custody.resources.is_some(),
            "transport has not authorized resource release"
        );
        let mut child = lock_unpoison(custody.child.as_ref().unwrap());
        let JobChild::Isolated(native) = &mut *child else {
            panic!("ambient child used");
        };
        assert_eq!(native.active_processes().unwrap(), 0);
        let proof = native
            .observe_completed_tree(Instant::now() + Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(
            proof.member_count() as usize,
            native.observed_member_count()
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.stop_accepting_work();
        self.manager.cancel_queued_for_shutdown();
        let deadline = Instant::now() + Duration::from_secs(3);
        let batch = self.manager.signal_all_for_shutdown();
        let _ = self.manager.drain_shutdown(batch, deadline);
        let _ = self.manager.wait_for_workers(deadline);
    }
}

#[test]
#[ignore = "real Windows LPAC Job lane; explicit isolated validation root/helper required"]
fn agent_execution_real_typed_job_closes_replays_and_fences_old_run() {
    let mut fixture = Fixture::new();
    let start = fixture.start(
        0,
        vec![
            "lpac-observe-tree".into(),
            "ready".into(),
            "release".into(),
            "2".into(),
        ],
        None,
    );
    fixture.enqueue(&start);
    let workspace = fixture.workspace(&start);
    let ready = (0..=2)
        .map(|depth| workspace.join(format!("ready.{depth}")))
        .collect::<Vec<_>>();
    fixture.wait_ready(&start, &ready, 3);
    fixture.assert_pinned_tree(&start);
    let close = fixture.close(&[start.clone()]);
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &close)
        .unwrap();
    receipt.validate_for(&close).unwrap();
    fixture.assert_tree_receipt(&start, &receipt.receipts[0]);
    assert_eq!(
        fixture
            .manager
            .close_agent_execution(&fixture.sink, &close)
            .unwrap(),
        receipt
    );
    fixture
        .sink
        .send_agent_execution_close_receipt("native-close-request", &receipt)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match fixture.receiver.try_recv() {
            Ok(RunnerEnvelope::AgentExecutionCloseReceipt {
                request_id,
                receipt: sent,
            }) => {
                assert_eq!(request_id, "native-close-request");
                assert_eq!(sent, receipt);
                break;
            }
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    fixture.assert_tree_receipt(&start, &receipt.receipts[0]);
    let late = fixture.start(1, vec!["sleep".into(), "30".into()], None);
    fixture.enqueue(&late);
    assert!(!fixture.workspace(&late).exists());
    assert_eq!(
        lock_unpoison(&fixture.manager.jobs)[job_id(&late.binding).unwrap()]
            .snapshot
            .error
            .as_deref(),
        Some("agent_execution_admission_closed")
    );
    let reopened = JobManager::new(1);
    reopened
        .configure_agent_execution_resources(
            fixture.profiles.clone(),
            fixture.metadata_root.clone(),
        )
        .unwrap();
    assert_eq!(
        reopened
            .close_agent_execution(&fixture.sink, &close)
            .unwrap(),
        receipt
    );
    reopened.enqueue(fixture.sink.clone(), fixture.pending(&late));
    assert_eq!(
        fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap()
            .jobs
            .len(),
        1
    );
    assert!(lock_unpoison(&reopened.agent_execution).live.is_empty());
    // This is admission shape only, not a Store handoff grant: a future run of
    // the same logical Agent is distinct from the closed source run. Dedicated
    // CLI workers are separate executor identities and are not handoff Agents.
    let mut successor = fixture.start(2, vec!["sleep".into(), "30".into()], None);
    successor.binding.requester.run.agent_run_id = "logical-run-two".into();
    successor.binding.requester.run.generation = 2;
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(
        successor.binding.requester.run.agent_id,
        start.binding.requester.run.agent_id
    );
    assert!(!closed_for(&metadata, &successor.binding));
}

#[test]
#[ignore = "real Windows LPAC resources and blocked stdin cancellation"]
fn agent_execution_real_resource_boundary_and_cancelled_stdin_join() {
    let fixture = Fixture::new();
    let start = fixture.start(
        0,
        vec![
            "lpac-resource-boundary".into(),
            "../input/authorized.txt".into(),
            fixture.outside.to_str().unwrap().into(),
            "boundary-ready".into(),
            "release".into(),
        ],
        Some("x".repeat(PROCESS_STDIN_MAX_BYTES)),
    );
    fixture.enqueue(&start);
    let workspace = fixture.workspace(&start);
    fixture.wait_ready(&start, &[workspace.join("boundary-ready")], 1);
    fixture.assert_pinned_tree(&start);
    let result = fs::read_to_string(workspace.join("boundary-ready")).unwrap();
    assert!(result.contains("authorized-input-read; workspace-write; outside-read-denied=Some("));
    assert!(result.contains("input-write-denied=Some("));
    assert_eq!(
        fs::read(workspace.join("owned-output.txt")).unwrap(),
        b"workspace-write-ok"
    );
    assert_eq!(fs::read(&fixture.input).unwrap(), b"authorized-input");
    assert_eq!(
        fs::read(&fixture.outside).unwrap(),
        b"other-private-sentinel"
    );
    assert_eq!(
        fs::read(workspace.parent().unwrap().join("input/authorized.txt")).unwrap(),
        b"authorized-input"
    );
    let close = fixture.close(&[start.clone()]);
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &close)
        .unwrap();
    receipt.validate_for(&close).unwrap();
    fixture.assert_tree_receipt(&start, &receipt.receipts[0]);
}

#[test]
#[ignore = "real Windows LPAC project-copy execution; fresh validation root/helper only"]
fn agent_execution_real_project_workspace_reads_edits_and_validates_without_host_access() {
    use webcodex_core::runner_protocol::{
        compute_workspace_launch_intent_hash, AgentExecutionWorkspaceSource,
    };
    let fixture = Fixture::new();
    let project = fixture._temporary.path().join("authorized-project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.txt"), "source-v1").unwrap();
    fs::write(project.join(".env"), "synthetic-secret").unwrap();
    let canonical = crate::webcodex_runner::shell::canonicalize_existing(&project).unwrap();
    let registry = fixture._temporary.path().join("project-registry");
    fs::create_dir(&registry).unwrap();
    fs::write(
        registry.join("repo.toml"),
        format!(
            "id = \"repo\"\npath = {}\nallow_patch = true\n",
            serde_json::to_string(canonical.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let mut start = fixture.start(
        0,
        vec![
            "lpac-project-workspace".into(),
            project.join("src/main.txt").to_str().unwrap().into(),
            fixture.outside.to_str().unwrap().into(),
        ],
        None,
    );
    start.workspace_source = Some(AgentExecutionWorkspaceSource {
        project: "agent:native-runner:repo".into(),
        root: canonical.to_str().unwrap().into(),
    });
    start.binding.launch_intent_hash = compute_workspace_launch_intent_hash(
        &start.process,
        start.stdin.as_deref(),
        start.timeout_secs,
        start.workspace_source.as_ref(),
    )
    .unwrap();
    let mut pending = fixture.pending(&start);
    pending.project_registry_dir = registry.clone();
    pending.policy.allowed_roots = vec![project.clone()];
    fixture.manager.enqueue(fixture.sink.clone(), pending);
    let workspace = fixture.workspace(&start);
    // This scenario deliberately joins a short-lived validation child. The
    // tree-hold fixture's active == observed readiness rule does not apply.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = lock_unpoison(&fixture.manager.jobs)
            .get(job_id(&start.binding).unwrap())
            .unwrap()
            .snapshot
            .clone();
        assert!(
            !super::super::lifecycle::runner_job_is_terminal(&snapshot.status),
            "project workspace execution failed: {snapshot:?}"
        );
        if workspace.join("project-ready").is_file() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "project workspace readiness: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(fs::read_to_string(workspace.join("project-ready"))
        .unwrap()
        .contains("child-validation-passed"));
    assert!(!workspace.join(".env").exists());
    assert_eq!(
        fs::read_to_string(project.join("src/main.txt")).unwrap(),
        "source-v1"
    );
    let metadata = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert_eq!(
        metadata.jobs[0]
            .workspace_snapshot
            .as_ref()
            .unwrap()
            .file_count,
        1
    );
    let close = fixture.close(&[start.clone()]);
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &close)
        .unwrap();
    receipt.validate_for(&close).unwrap();
    assert!(matches!(
        receipt.receipts[0].outcome,
        AgentExecutionStopOutcome::TreeExited {
            leader_exit_code: Some(_),
            leader_handle_signaled: true,
            membership_complete: true,
            member_handles_signaled: true,
            worker_finished: true,
        }
    ));
    {
        // Current close semantics release copy handles after durable stop proof.
        let runtime = lock_unpoison(&fixture.manager.agent_execution);
        let custody = runtime.live.get(job_id(&start.binding).unwrap()).unwrap();
        assert!(custody.worker_joined && custody.helpers_finished);
        assert!(custody.resources.is_none());
        let mut child = lock_unpoison(custody.child.as_ref().unwrap());
        let JobChild::Isolated(native) = &mut *child else {
            panic!("ambient process");
        };
        assert_eq!(native.active_processes().unwrap(), 0);
        assert!(native
            .observe_completed_tree(Instant::now() + Duration::from_secs(1))
            .unwrap()
            .is_some());
    }
    // A stale registered root never launches even when the project id is unchanged.
    let mut other = start.clone();
    other.workspace_source.as_mut().unwrap().root =
        fixture._temporary.path().to_str().unwrap().into();
    assert_eq!(
        validate_workspace_source(
            &other,
            &RunnerPolicy {
                allowed_roots: vec![project],
                ..Default::default()
            },
            &registry
        )
        .unwrap_err(),
        "agent_workspace_project_changed"
    );
}

#[test]
#[ignore = "real Windows LPAC running/queued admission closure"]
fn agent_execution_real_running_tree_and_queued_job_close_as_exact_set() {
    let fixture = Fixture::new();
    let first = fixture.start(
        0,
        vec![
            "lpac-observe-tree".into(),
            "ready".into(),
            "release".into(),
            "1".into(),
        ],
        None,
    );
    fixture.enqueue(&first);
    let workspace = fixture.workspace(&first);
    fixture.wait_ready(
        &first,
        &[workspace.join("ready.0"), workspace.join("ready.1")],
        2,
    );
    let queued = fixture.start(1, vec!["sleep".into(), "30".into()], None);
    fixture.enqueue(&queued);
    assert_eq!(lock_unpoison(&fixture.manager.queued).len(), 1);
    assert!(!fixture.workspace(&queued).exists());
    let close = fixture.close(&[first.clone(), queued.clone()]);
    let receipt = fixture
        .manager
        .close_agent_execution(&fixture.sink, &close)
        .unwrap();
    receipt.validate_for(&close).unwrap();
    fixture.assert_tree_receipt(&first, &receipt.receipts[0]);
    assert!(matches!(
        receipt.receipts[1].outcome,
        AgentExecutionStopOutcome::NeverCreated {
            request_cancelled: true,
            worker_finished: true
        }
    ));
    assert!(lock_unpoison(&fixture.manager.queued).is_empty());
    assert!(!fixture.workspace(&queued).exists());
    let stored = fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap();
    assert!(stored.jobs[0].native_create_attempted);
    assert!(!stored.jobs[1].native_create_attempted);
    assert!(stored
        .jobs
        .iter()
        .all(|job| job.worker_finished && job.stop_receipt.is_some()));
    assert_eq!(stored.closes[0].receipt.as_ref(), Some(&receipt));
}

#[test]
#[ignore = "real Windows resource admission negatives in fresh validation copies; no native launch"]
fn agent_execution_real_rejects_source_hardlink_hash_and_delete_handle_before_spawn() {
    use std::os::windows::fs::OpenOptionsExt;
    for mutation in 0..3 {
        let fixture = Fixture::new();
        let _delete_handle = if mutation == 0 {
            fs::hard_link(
                &fixture.program,
                fixture._temporary.path().join("source-alias.exe"),
            )
            .expect("create a hardlink only to this fresh synthetic helper copy");
            None
        } else if mutation == 1 {
            fs::write(&fixture.program, b"modified synthetic helper copy")
                .expect("change only this fixture copy after profile verification");
            None
        } else {
            Some(
                fs::OpenOptions::new()
                    .access_mode(0x00010000)
                    .share_mode(7)
                    .open(&fixture.program)
                    .expect("open DELETE access only on synthetic source copy"),
            )
        };
        assert!(fixture.profiles.verify_sources().is_err());
        let start = fixture.start(0, vec!["sleep".into(), "30".into()], None);
        fixture.enqueue(&start);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = lock_unpoison(&fixture.manager.jobs)
                .get(job_id(&start.binding).unwrap())
                .unwrap()
                .snapshot
                .clone();
            if super::super::lifecycle::runner_job_is_terminal(&snapshot.status) {
                assert_eq!(
                    snapshot.command_execution_state,
                    Some(ShellCommandExecutionState::NotStarted)
                );
                let error = snapshot.error.unwrap();
                assert!(
                    match mutation {
                        0 => error.contains("link_rejected"),
                        1 => error.contains("hash_mismatch"),
                        _ => error.contains("open_failed"),
                    },
                    "unexpected source rejection: {error}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "source rejection did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(fixture.manager.wait_for_workers(deadline));
        let metadata = fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap();
        assert_eq!(metadata.jobs.len(), 1);
        assert!(
            !metadata.jobs[0].native_create_attempted,
            "unsafe source reached native creation"
        );
        let runtime = lock_unpoison(&fixture.manager.agent_execution);
        assert!(runtime
            .live
            .get(job_id(&start.binding).unwrap())
            .unwrap()
            .child
            .is_none());
        drop(runtime);
        let close = fixture.close(&[start]);
        let receipt = fixture
            .manager
            .close_agent_execution(&fixture.sink, &close)
            .unwrap();
        receipt.validate_for(&close).unwrap();
        assert!(matches!(
            receipt.receipts[0].outcome,
            AgentExecutionStopOutcome::NeverCreated {
                request_cancelled: true,
                worker_finished: true
            }
        ));
    }
}

#[test]
#[ignore = "real Windows pins on fresh prepared resources; no native process"]
fn agent_execution_real_directory_and_file_pins_release_without_acl_changes() {
    let fixture = Fixture::new();
    let start = fixture.start(0, vec!["sleep".into(), "30".into()], None);
    let execution = fixture.workspace(&start).parent().unwrap().to_owned();
    {
        let _prepared = super::super::agent_resources::prepare(
            &fixture.profiles,
            &start,
            &fixture.configuration.resource_root,
        )
        .unwrap();
        fixture.assert_pinned_tree(&start);
        assert!(fixture
            .configuration
            .store
            .read_agent_execution_metadata()
            .unwrap()
            .jobs
            .is_empty());
    }
    // Retry the exact replacement that was denied above, with the same source
    // and destination and no ACL changes. Success proves the earlier denial
    // was temporary handle protection, not an always-forbidden operation.
    let replacement = fixture._temporary.path().join("replacement-input.tmp");
    let target = execution.join("input/nested/authorized.txt");
    assert_eq!(fs::read(&target).unwrap(), b"authorized-input");
    assert_eq!(
        fs::read(&replacement).unwrap(),
        b"replacement-must-not-publish"
    );
    fs::rename(&replacement, &target).expect("exact replacement succeeds after Prepared pins drop");
    assert!(!replacement.exists());
    assert_eq!(fs::read(&target).unwrap(), b"replacement-must-not-publish");
    for path in [
        execution.join("bin/program.exe"),
        execution.join("input/nested/authorized.txt"),
        execution.join("input/nested"),
        execution.join("input"),
        execution.join("bin"),
        execution.join("workspace"),
        execution,
    ] {
        assert_released_rename(&path);
    }
    // Isolate the shared ancestor/root lease from the configured manager's
    // longer lifetime: each of these paths is another fresh validation child.
    let base = PathBuf::from(std::env::var_os("WEBCODEX_AGENT_EXECUTION_TEST_ROOT").unwrap());
    let temporary = tempfile::Builder::new()
        .prefix("root-lease-")
        .tempdir_in(base)
        .unwrap();
    let root = temporary.path().join("resources");
    {
        let _pins = super::super::agent_resources::initialize_resource_root(&root).unwrap();
        assert_pinned_rename(temporary.path());
        assert_pinned_rename(&root);
    }
    assert_released_rename(&root);
    assert_released_rename(temporary.path());
}
