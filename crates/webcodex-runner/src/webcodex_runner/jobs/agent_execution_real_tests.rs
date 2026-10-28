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
            native_cli: None,
            browser: None,
            profiles: vec![AgentExecutionResourceProfile {
                host_cli: None,
                native_worker: None,
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
#[ignore = "real Python stdlib sandbox; explicit resource manifest and fresh validation root"]
fn agent_program_real_python_sqlite_and_child_restart_preserve_filesystem_isolation() {
    exercise_python_program(false, false, false);
}

#[test]
#[ignore = "real Python relative-path regression at production-length sandbox roots; no business code"]
fn agent_program_real_long_root_keeps_relative_files_imports_and_restart_readable() {
    exercise_python_program(false, false, true);
}

#[test]
#[ignore = "real isolated Edge UI; explicit prepared browser resources and fresh validation root required"]
fn agent_program_real_browser_interaction_uses_private_profile_and_local_service() {
    exercise_python_program(true, false, false);
}

#[test]
#[ignore = "real fixed browser controller, explicit Python manifest/browser path and fresh validation root"]
fn agent_program_real_fixed_browser_is_owned_durable_and_bounded() {
    exercise_python_program(true, true, false);
}

struct ProgramFixtureCleanup(JobManager);
impl Drop for ProgramFixtureCleanup {
    fn drop(&mut self) {
        self.0.stop_accepting_work();
        self.0.cancel_queued_for_shutdown();
        let deadline = Instant::now() + Duration::from_secs(15);
        let batch = self.0.signal_all_for_shutdown();
        let _ = self.0.drain_shutdown(batch, deadline);
        let _ = self.0.wait_for_workers(deadline);
    }
}
fn exercise_python_program(with_browser: bool, fixed_browser: bool, long_paths: bool) {
    use webcodex_core::agent_program::{AgentProgramBinding, AgentProgramStart};
    use webcodex_core::runner_protocol::{AgentExecutionTarget, AgentRunRef};
    let base = PathBuf::from(std::env::var_os("WEBCODEX_AGENT_EXECUTION_TEST_ROOT").unwrap());
    let temp = tempfile::Builder::new()
        .prefix("ordinary-python-")
        .tempdir_in(base)
        .unwrap();
    let manifest = PathBuf::from(
        std::env::var_os("WEBCODEX_PROGRAM_PYTHON_RESOURCES")
            .expect("prepared Python resource manifest"),
    );
    let mut settings =
        serde_json::from_slice::<AgentExecutionResourcesConfig>(&fs::read(manifest).unwrap())
            .unwrap();
    if with_browser && !fixed_browser {
        let browser_manifest = PathBuf::from(
            std::env::var_os("WEBCODEX_PROGRAM_BROWSER_RESOURCES")
                .expect("prepared browser resources"),
        );
        settings.profiles.extend(
            serde_json::from_slice::<AgentExecutionResourcesConfig>(
                &fs::read(browser_manifest).unwrap(),
            )
            .unwrap()
            .profiles,
        );
    }
    let profiles = validate_profiles(&settings).unwrap();
    let manager = JobManager::new(2);
    let _cleanup = ProgramFixtureCleanup(manager.clone_for_worker());
    let state = if long_paths {
        let length = 151usize
            .checked_sub(temp.path().to_str().unwrap().len() + 1 + "-agent-resources".len())
            .expect("short explicit test root required");
        temp.path().join("s".repeat(length))
    } else {
        temp.path().join("state")
    };
    manager
        .configure_agent_execution_resources(profiles.clone(), state)
        .unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("public.txt"), b"public-input").unwrap();
    if long_paths {
        fs::create_dir(project.join("collaboration")).unwrap();
        fs::write(
            project.join("collaboration/contract-v1.json"),
            b"synthetic-long-path",
        )
        .unwrap();
        fs::create_dir(project.join("framework_module")).unwrap();
        fs::write(
            project.join("framework_module/important_feature.py"),
            b"VALUE = 47\n",
        )
        .unwrap();
    }
    let outside = temp.path().join("other-agent-private.txt");
    fs::write(&outside, b"private-sentinel").unwrap();
    let canonical = crate::webcodex_runner::shell::canonicalize_existing(&project).unwrap();
    let registry = temp.path().join("registry");
    fs::create_dir(&registry).unwrap();
    fs::write(
        registry.join("repo.toml"),
        format!(
            "id=\"repo\"\npath={}\nallow_patch=true\n",
            serde_json::to_string(canonical.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let code = format!(
        r#"import sqlite3, subprocess, sys, pathlib, json
assert pathlib.Path('public.txt').read_text() == 'public-input'
{snapshot_assertions}
try:
    pathlib.Path({outside}).read_text()
except PermissionError:
    denied = True
else:
    raise RuntimeError('outside private file was readable')
db=sqlite3.connect('synthetic.db')
db.execute('create table evidence(value integer)')
db.execute('insert into evidence values (47)')
db.commit()
db.close()
child=subprocess.run([sys.executable,'-c',"import sqlite3; assert sqlite3.connect('synthetic.db').execute('select value from evidence').fetchone()[0] == 47; print('reopened')"],capture_output=True,text=True,timeout=5)
assert child.returncode == 0, child.stderr
assert child.stdout.strip() == 'reopened'
print(json.dumps({{'framework_fixture': True, 'sqlite_reopen': 47, 'private_read_denied': denied, 'child_exit':child.returncode}}))
"#,
        outside = serde_json::to_string(outside.to_str().unwrap()).unwrap(),
        snapshot_assertions = if long_paths {
            "assert pathlib.Path('collaboration/contract-v1.json').read_text() == 'synthetic-long-path'\nfrom framework_module.important_feature import VALUE\nassert VALUE == 47"
        } else {
            ""
        }
    );
    let mut start = AgentProgramStart {
        binding: AgentProgramBinding {
            local_service_port: None,
            resume_from_program: None,
            version: 1,
            program_id: "python-program".into(),
            enrollment_id: "own-enrollment".into(),
            task_id: "own-task".into(),
            run: AgentRunRef {
                agent_id: "own-agent".into(),
                agent_run_id: "own-run".into(),
                generation: 1,
            },
            project: "logical-project".into(),
            target: AgentExecutionTarget {
                client_id: "native-runner".into(),
                runner_instance_id: "native-instance".into(),
            },
            job_id: "python-job".into(),
            start_request_id: "python-request".into(),
            workspace_project: "agent:native-runner:repo".into(),
            workspace_root: canonical.to_str().unwrap().into(),
            relative_cwd: ".".into(),
            resource_set_id: "python-stdlib".into(),
            resource_manifest_hash: profiles.manifest("python-stdlib").unwrap().into(),
            launch_intent_hash: "a".repeat(64),
            expires_at_unix_ms: chrono::Utc::now().timestamp_millis() + 60000,
        },
        args: vec!["-c".into(), code],
        stdin: None,
        timeout_secs: 30,
    };
    start.binding.launch_intent_hash = start.compute_launch_hash().unwrap();
    let wire = RunnerInvocation {
        metadata: RunnerInvocationMetadata {
            request_id: start.binding.start_request_id.clone(),
            client_id: "native-runner".into(),
            requested_by: "framework-fixture".into(),
            created_at: chrono::Utc::now().timestamp(),
        },
        operation: RunnerOperation::Job(start.clone().into_job()),
    }
    .into_v2_request()
    .unwrap();
    let mut pending = PendingJobStart::from_wire(
        1,
        RunnerPolicy::default(),
        ShellConfig::default(),
        SshConfig::default(),
        registry.clone(),
        wire,
    );
    pending.policy.allowed_roots = vec![project.clone()];
    let (tx, _rx) = tokio::sync::mpsc::channel(256);
    let sink = RunnerSink::WebSocket {
        tx,
        client_id: "native-runner".into(),
        runner_instance_id: "native-instance".into(),
    };
    manager.enqueue(sink.clone(), pending);
    let deadline = Instant::now() + Duration::from_secs(35);
    let snapshot = loop {
        let snapshot = lock_unpoison(&manager.jobs)
            .get(&start.binding.job_id)
            .unwrap()
            .snapshot
            .clone();
        if super::super::lifecycle::runner_job_is_terminal(&snapshot.status) {
            break snapshot;
        }
        assert!(Instant::now() < deadline, "Python sandbox timeout");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(snapshot.exit_code, Some(0), "{snapshot:?}");
    assert_eq!(snapshot.status, "completed", "{snapshot:?}");
    assert!(
        snapshot.stdout.tail.contains("sqlite_reopen"),
        "{snapshot:?}"
    );
    println!("{}", snapshot.stdout.tail);
    assert!(manager.wait_for_workers(Instant::now() + Duration::from_secs(5)));
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let service_code = r#"import http.server, sqlite3, os, json
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path=='/slow':
            import time
            time.sleep(20)
        if self.path=='/ui':
            body=b'<!doctype html><title>Framework fixture</title><input id="name"><button id="send" onclick="document.getElementById(\'result\').textContent=\'Hello \'+document.getElementById(\'name\').value">Submit</button><p id="result">Ready</p>'
            self.send_response(200)
            self.send_header('Content-Type','text/html')
            self.send_header('Content-Length',str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path=='/favicon.ico':
            self.send_response(204)
            self.end_headers()
            return
        with sqlite3.connect('synthetic.db') as db:
            value=db.execute('select value from evidence').fetchone()[0]
            db.execute('update evidence set value=?',(value+1,))
        body=json.dumps({'value':value}).encode()
        self.send_response(200)
        self.send_header('Content-Length',str(len(body)))
        self.end_headers()
        self.wfile.write(body)
http.server.HTTPServer(('127.0.0.1',int(os.environ['WEBCODEX_PORT'])),Handler).serve_forever()
"#;
    let mut previous = start.binding.program_id.clone();
    for round in 0..2 {
        let mut service = start.clone();
        service.binding.program_id = format!("http-program-{round}");
        service.binding.job_id = format!("http-job-{round}");
        service.binding.start_request_id = format!("http-request-{round}");
        service.binding.local_service_port = Some(port);
        service.binding.resume_from_program = Some(previous);
        service.args = vec!["-c".into(), service_code.into()];
        service.binding.launch_intent_hash = service.compute_launch_hash().unwrap();
        let audit = PathBuf::from(std::env::var_os("WEBCODEX_AGENT_EXECUTION_TEST_ROOT").unwrap())
            .join(format!(
                "http-policy-{}.txt",
                service.binding.identity_hash().unwrap()
            ));
        fs::write(
            &audit,
            format!(
                "identity={}\nport={port}\nstate=issued\n",
                service.binding.identity_hash().unwrap()
            ),
        )
        .unwrap();
        let wire = RunnerInvocation {
            metadata: RunnerInvocationMetadata {
                request_id: service.binding.start_request_id.clone(),
                client_id: "native-runner".into(),
                requested_by: "framework-local-service".into(),
                created_at: chrono::Utc::now().timestamp(),
            },
            operation: RunnerOperation::Job(service.clone().into_job()),
        }
        .into_v2_request()
        .unwrap();
        let mut pending = PendingJobStart::from_wire(
            1,
            RunnerPolicy::default(),
            ShellConfig::default(),
            SshConfig::default(),
            registry.clone(),
            wire,
        );
        pending.policy.allowed_roots = vec![project.clone()];
        manager.enqueue(sink.clone(), pending);
        let query = webcodex_core::agent_program::AgentProgramInspect {
            target: service.binding.target.clone(),
            binding: service.binding.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let observation = manager.inspect_agent_program(&sink, &query).unwrap();
            if let Some(endpoint) = observation.service {
                assert_eq!(endpoint.port, port);
                assert!(endpoint.process_id > 0);
                break;
            }
            assert!(
                observation
                    .job
                    .as_ref()
                    .is_none_or(|j| !super::super::lifecycle::runner_job_is_terminal(&j.status)),
                "{observation:?}"
            );
            assert!(
                Instant::now() < deadline,
                "service did not listen: {observation:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut client =
            TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(2))
                .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["value"], 47 + round);
        if with_browser && round == 0 && fixed_browser {
            use webcodex_core::agent_browser::{
                AgentBrowserAction, AgentBrowserRequest, BrowserStep,
            };
            let binary = PathBuf::from(
                std::env::var_os("WEBCODEX_LOCAL_BROWSER_BINARY")
                    .expect("explicit installed browser binary"),
            );
            let config = super::super::agent_resources::LocalBrowserConfig {
                python: settings.profiles[0].executable.clone(),
                python_sha256: settings.profiles[0].executable_sha256.clone(),
                browser: binary.clone(),
                browser_sha256: format!("{:x}", Sha256::digest(fs::read(binary).unwrap())),
            };
            let action = AgentBrowserAction {
                action_id: format!("wc_browser_{}", uuid::Uuid::new_v4().simple()),
                service: service.binding.clone(),
                timeout_secs: 15,
                steps: vec![
                    BrowserStep::Navigate { path: "/ui".into() },
                    BrowserStep::Fill {
                        selector: "#name".into(),
                        text: "沙箱".into(),
                    },
                    BrowserStep::Click {
                        accept_dialog: false,
                        selector: "#send".into(),
                    },
                    BrowserStep::ExpectText {
                        selector: "#result".into(),
                        text: "Hello 沙箱".into(),
                    },
                    BrowserStep::Screenshot {},
                ],
            };
            let query = AgentBrowserRequest {
                target: service.binding.target.clone(),
                action: action.clone(),
                observe_only: false,
                image_step: None,
            };
            let result = manager
                .agent_local_browser(&sink, &query, Some(&config))
                .unwrap();
            assert_eq!(result.state, "completed", "{result:?}");
            assert_eq!(result.exit_code, Some(0));
            assert!(result.process_id.is_some());
            assert_eq!(
                result.result.as_ref().unwrap()["result"]["steps"][3]["text"],
                "Hello 沙箱"
            );
            let store = lock_unpoison(&manager.agent_execution)
                .configuration
                .clone()
                .unwrap()
                .store;
            assert!(
                fs::metadata(
                    store
                        .browser_artifact_directory(&action)
                        .unwrap()
                        .join("evidence/step-04.png")
                )
                .unwrap()
                .len()
                    > 1000
            );
            assert_eq!(
                manager.agent_local_browser(&sink, &query, None).unwrap(),
                result,
                "replay must not launch even without current config"
            );
            let mut image_query = query.clone();
            image_query.observe_only = true;
            image_query.image_step = Some(4);
            let image = manager
                .agent_local_browser_response(&sink, &image_query, None)
                .unwrap()
                .image
                .unwrap();
            assert!(image.file_bytes > 1000);
            assert_eq!(image.step, 4);
            image_query.image_step = Some(1);
            assert!(manager
                .agent_local_browser_response(&sink, &image_query, None)
                .is_err());
            let mut changed = query.clone();
            changed.action.steps.push(BrowserStep::Text {
                selector: "body".into(),
            });
            assert!(manager
                .agent_local_browser(&sink, &changed, Some(&config))
                .is_err());
            let mut unknown = query.clone();
            unknown.action.action_id = format!("wc_browser_{}", uuid::Uuid::new_v4().simple());
            let fenced = webcodex_core::agent_browser::AgentBrowserReceipt {
                action_id: unknown.action.action_id.clone(),
                identity_hash: unknown.action.identity_hash().unwrap(),
                state: "dispatch_fenced".into(),
                process_id: None,
                exit_code: None,
                result: None,
                error: None,
            };
            assert!(store.fence_browser(&unknown.action, &fenced).unwrap());
            assert_eq!(
                manager
                    .agent_local_browser(&sink, &unknown, Some(&config))
                    .unwrap()
                    .state,
                "outcome_unknown"
            );
            assert!(!store
                .browser_artifact_directory(&unknown.action)
                .unwrap()
                .exists());
            let mut wrong = query.clone();
            wrong.action.action_id = format!("wc_browser_{}", uuid::Uuid::new_v4().simple());
            wrong.action.service.program_id = "foreign-program".into();
            assert_eq!(
                manager
                    .agent_local_browser(&sink, &wrong, Some(&config))
                    .unwrap()
                    .state,
                "not_started"
            );
            let mut slow = query.clone();
            slow.action.action_id = format!("wc_browser_{}", uuid::Uuid::new_v4().simple());
            slow.action.steps = vec![BrowserStep::Navigate {
                path: "/slow".into(),
            }];
            slow.action.timeout_secs = 30;
            let (worker_manager, worker_sink, worker_config, worker_query) = (
                manager.clone_for_worker(),
                sink.clone(),
                config.clone(),
                slow.clone(),
            );
            let worker = std::thread::spawn(move || {
                worker_manager.agent_local_browser(
                    &worker_sink,
                    &worker_query,
                    Some(&worker_config),
                )
            });
            let wait_until = Instant::now() + Duration::from_secs(10);
            loop {
                if store
                    .browser_metadata(&slow.action.action_id)
                    .unwrap()
                    .is_some_and(|r| r.receipt.process_id.is_some())
                    && store
                        .browser_artifact_directory(&slow.action)
                        .unwrap()
                        .join("evidence/engine.json")
                        .is_file()
                {
                    assert!(!worker.is_finished());
                    break;
                }
                assert!(
                    !worker.is_finished(),
                    "browser worker ended before process evidence"
                );
                assert!(Instant::now() < wait_until, "browser did not start");
                std::thread::sleep(Duration::from_millis(10));
            }
            manager.stop(&service.binding.job_id).unwrap();
            let wait_until = Instant::now() + Duration::from_secs(10);
            while !worker.is_finished() {
                assert!(
                    Instant::now() < wait_until,
                    "browser did not stop with its service"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let stopped_browser = worker.join().unwrap().unwrap();
            assert_eq!(stopped_browser.state, "failed", "{stopped_browser:?}");
            assert!(stopped_browser.exit_code.is_some());
            assert_eq!(
                manager.agent_local_browser(&sink, &slow, None).unwrap(),
                stopped_browser
            );
            println!("framework fixed browser: real Edge interaction/screenshot, original action replay retained, unknown not resent, foreign service rejected, service stop terminates browser tree");
        }
        if with_browser && round == 0 && !fixed_browser {
            let mut browser = start.clone();
            browser.binding.program_id = "browser-program".into();
            browser.binding.job_id = "browser-job".into();
            browser.binding.start_request_id = "browser-request".into();
            browser.binding.resource_set_id = "python-browser".into();
            browser.binding.resource_manifest_hash =
                profiles.manifest("python-browser").unwrap().into();
            browser.binding.local_service_port = Some(port);
            browser.timeout_secs = 60;
            let fixture_root =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../windows/program-runtime");
            let browser_code = fs::read_to_string(fixture_root.join("browser_fixture.py")).unwrap();
            browser.args = vec![
                "-u".into(),
                "-c".into(),
                format!(
                    "{}\n{}",
                    fs::read_to_string(fixture_root.join("webcodex_asyncio.py")).unwrap(),
                    browser_code
                ),
            ];
            browser.binding.launch_intent_hash = browser.compute_launch_hash().unwrap();
            let wire = RunnerInvocation {
                metadata: RunnerInvocationMetadata {
                    request_id: browser.binding.start_request_id.clone(),
                    client_id: "native-runner".into(),
                    requested_by: "framework-browser-fixture".into(),
                    created_at: chrono::Utc::now().timestamp(),
                },
                operation: RunnerOperation::Job(browser.clone().into_job()),
            }
            .into_v2_request()
            .unwrap();
            let mut pending = PendingJobStart::from_wire(
                1,
                RunnerPolicy::default(),
                ShellConfig::default(),
                SshConfig::default(),
                registry.clone(),
                wire,
            );
            pending.policy.allowed_roots = vec![project.clone()];
            manager.enqueue(sink.clone(), pending);
            let deadline = Instant::now() + Duration::from_secs(65);
            let snapshot = loop {
                let s = lock_unpoison(&manager.jobs)
                    .get(&browser.binding.job_id)
                    .unwrap()
                    .snapshot
                    .clone();
                if super::super::lifecycle::runner_job_is_terminal(&s.status) {
                    break s;
                }
                assert!(Instant::now() < deadline, "browser fixture deadline");
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(snapshot.exit_code, Some(0), "{snapshot:?}");
            assert!(
                snapshot.stdout.tail.contains("Hello sandbox"),
                "{snapshot:?}"
            );
            println!("{}", snapshot.stdout.tail);
        }
        manager.stop(&service.binding.job_id).unwrap();
        assert!(manager.wait_for_workers(Instant::now() + Duration::from_secs(5)));
        let stopped = manager.inspect_agent_program(&sink, &query).unwrap();
        assert!(stopped.service.is_none());
        assert_eq!(stopped.state, "stopped");
        assert!(
            webcodex_process::windows_local_service::LocalServiceNetwork::cleanup_complete(
                &service.binding.identity_hash().unwrap()
            )
            .unwrap()
        );
        fs::write(
            &audit,
            format!(
                "identity={}\nport={port}\nstate=revoked\n",
                service.binding.identity_hash().unwrap()
            ),
        )
        .unwrap();
        previous = service.binding.program_id.clone();
        println!(
            "framework HTTP round={round}; actual_port={port}; persisted_value={}; stopped=true",
            body["value"]
        );
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
#[ignore = "real ordinary program sandbox; explicit fresh validation root/helper required"]
fn agent_program_real_job_writes_owned_files_denies_private_files_and_stops_without_cli_worker() {
    use webcodex_core::agent_program::{AgentProgramBinding, AgentProgramStart};
    use webcodex_core::runner_protocol::{AgentExecutionTarget, AgentRunRef};
    let fixture = Fixture::new();
    let project = fixture._temporary.path().join("ordinary-project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("public.txt"), b"authorized-source").unwrap();
    let canonical = crate::webcodex_runner::shell::canonicalize_existing(&project).unwrap();
    let registry = fixture._temporary.path().join("ordinary-registry");
    fs::create_dir(&registry).unwrap();
    fs::write(
        registry.join("repo.toml"),
        format!(
            "id = \"repo\"\npath = {}\nallow_patch = true\n",
            serde_json::to_string(canonical.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let mut start = AgentProgramStart {
        binding: AgentProgramBinding {
            local_service_port: None,
            resume_from_program: None,
            version: 1,
            program_id: "native-program".into(),
            enrollment_id: "original-enrollment".into(),
            task_id: "original-task".into(),
            run: AgentRunRef {
                agent_id: "original-agent".into(),
                agent_run_id: "original-run".into(),
                generation: 1,
            },
            project: "logical-project".into(),
            target: AgentExecutionTarget {
                client_id: "native-runner".into(),
                runner_instance_id: "native-instance".into(),
            },
            job_id: "program-job".into(),
            start_request_id: "program-request".into(),
            workspace_project: "agent:native-runner:repo".into(),
            workspace_root: canonical.to_str().unwrap().into(),
            relative_cwd: ".".into(),
            resource_set_id: "synthetic-profile".into(),
            resource_manifest_hash: fixture
                .profiles
                .manifest("synthetic-profile")
                .unwrap()
                .into(),
            launch_intent_hash: "a".repeat(64),
            expires_at_unix_ms: chrono::Utc::now().timestamp_millis() + 60000,
        },
        args: vec![
            "lpac-resource-boundary".into(),
            "../input/authorized.txt".into(),
            fixture.outside.to_str().unwrap().into(),
            "program-ready".into(),
            "program-release".into(),
        ],
        stdin: None,
        timeout_secs: 60,
    };
    start.binding.launch_intent_hash = start.compute_launch_hash().unwrap();
    let wire = RunnerInvocation {
        metadata: RunnerInvocationMetadata {
            request_id: start.binding.start_request_id.clone(),
            client_id: "native-runner".into(),
            requested_by: "framework-test".into(),
            created_at: chrono::Utc::now().timestamp(),
        },
        operation: RunnerOperation::Job(start.clone().into_job()),
    }
    .into_v2_request()
    .unwrap();
    let mut pending = PendingJobStart::from_wire(
        1,
        RunnerPolicy::default(),
        ShellConfig::default(),
        SshConfig::default(),
        registry,
        wire,
    );
    pending.policy.allowed_roots = vec![project.clone()];
    fixture
        .manager
        .enqueue(fixture.sink.clone(), pending.clone());
    let workspace = super::super::agent_resources::program_resource_directory(
        &fixture.configuration.resource_root,
        &start.binding,
    )
    .unwrap()
    .join("workspace");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !workspace.join("program-ready").exists() {
        let snapshot = lock_unpoison(&fixture.manager.jobs)
            .get(&start.binding.job_id)
            .unwrap()
            .snapshot
            .clone();
        assert!(
            !super::super::lifecycle::runner_job_is_terminal(&snapshot.status),
            "{snapshot:?}"
        );
        assert!(
            Instant::now() < deadline,
            "ordinary sandbox readiness: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fs::read(workspace.join("public.txt")).unwrap(),
        b"authorized-source"
    );
    assert!(fs::read_to_string(workspace.join("program-ready"))
        .unwrap()
        .contains("outside-read-denied"));
    assert_eq!(
        fs::read(workspace.join("owned-output.txt")).unwrap(),
        b"workspace-write-ok"
    );
    assert!(!project.join("owned-output.txt").exists());
    let pid = lock_unpoison(&fixture.manager.jobs)
        .get(&start.binding.job_id)
        .unwrap()
        .child
        .as_ref()
        .map(|c| lock_unpoison(c).id())
        .unwrap();
    assert!(pid > 0);
    fixture.manager.enqueue(fixture.sink.clone(), pending); // same Job: no second native start
    fixture.manager.stop(&start.binding.job_id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows = fixture
            .configuration
            .store
            .read_agent_program_metadata()
            .unwrap();
        assert_eq!(rows.len(), 1);
        if rows[0].worker_finished {
            assert!(rows[0].tree_stopped);
            assert_eq!(rows[0].snapshot.as_ref().unwrap().status, "stopped");
            break;
        }
        assert!(Instant::now() < deadline, "program did not settle");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fixture
        .configuration
        .store
        .read_agent_execution_metadata()
        .unwrap()
        .jobs
        .is_empty());
    // Diagnostic of the real OS network boundary; no exemption, external
    // endpoint, model or business application is involved.
    let mut network = start.clone();
    network.binding.program_id = "native-network-probe".into();
    network.binding.job_id = "network-job".into();
    network.binding.start_request_id = "network-request".into();
    network.args = vec!["lpac-loopback-probe".into(), "loopback-report".into()];
    network.binding.launch_intent_hash = network.compute_launch_hash().unwrap();
    let operation = network.clone().into_job();
    let mut pending = fixture.pending(&fixture.start(99, vec!["sleep".into(), "0".into()], None));
    pending.project_registry_dir = fixture._temporary.path().join("ordinary-registry");
    pending.policy.allowed_roots = vec![project];
    pending.metadata.request_id = network.binding.start_request_id.clone();
    pending.operation = operation;
    fixture.manager.enqueue(fixture.sink.clone(), pending);
    let output = super::super::agent_resources::program_resource_directory(
        &fixture.configuration.resource_root,
        &network.binding,
    )
    .unwrap()
    .join("workspace/loopback-report");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !output.exists() {
        assert!(
            Instant::now() < deadline,
            "loopback probe did not produce output"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "ordinary program OS network probe: {}",
        fs::read_to_string(output).unwrap()
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
