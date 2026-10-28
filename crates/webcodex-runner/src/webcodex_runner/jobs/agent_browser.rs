//! Trusted fixed browser controller. Business Python never runs on this path.
//! Page code uses Chromium's sandbox in a fresh profile. The only reachable
//! HTTP origin is the retained Job's kernel-observed loopback listener.
use super::super::agent_resources::LocalBrowserConfig;
use super::*;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use webcodex_core::agent_browser::{AgentBrowserAction, AgentBrowserReceipt, AgentBrowserRequest};
use webcodex_core::agent_program::AgentProgramInspect;

const HELPER: &str = include_str!("../../../../../windows/program-runtime/local_browser.py");
static BROWSER_SLOT: Mutex<()> = Mutex::new(());

fn receipt(
    action: &AgentBrowserAction,
    state: &str,
    error: Option<&str>,
) -> Result<AgentBrowserReceipt, String> {
    Ok(AgentBrowserReceipt {
        action_id: action.action_id.clone(),
        identity_hash: action.identity_hash()?,
        state: state.into(),
        process_id: None,
        exit_code: None,
        result: None,
        error: error.map(str::to_owned),
    })
}

fn pin_binary(path: &Path, expected: &str) -> Result<Vec<File>, String> {
    use sha2::{Digest, Sha256};
    if !path.is_absolute()
        || path
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("exe"))
        || expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("agent_browser_runtime_invalid".into());
    }
    let mut pins = Vec::new();
    for item in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let metadata =
            fs::symlink_metadata(item).map_err(|_| "agent_browser_runtime_unavailable")?;
        if metadata.file_type().is_symlink() {
            return Err("agent_browser_runtime_linked".into());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("agent_browser_runtime_linked".into());
            }
            options
                .share_mode(if item == path { 1 } else { 3 })
                .custom_flags(0x00200000 | 0x02000000);
        }
        let file = options
            .open(item)
            .map_err(|_| "agent_browser_runtime_pin_failed")?;
        if item == path {
            if !metadata.is_file() {
                return Err("agent_browser_runtime_invalid".into());
            }
            let mut hash = Sha256::new();
            let mut reader = &file;
            let mut buffer = [0u8; 65536];
            loop {
                let n = reader
                    .read(&mut buffer)
                    .map_err(|_| "agent_browser_runtime_read_failed")?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            if format!("{:x}", hash.finalize()) != expected {
                return Err("agent_browser_runtime_changed".into());
            }
        } else if !metadata.is_dir() {
            return Err("agent_browser_runtime_invalid".into());
        }
        pins.push(file);
    }
    Ok(pins)
}

impl JobManager {
    pub(crate) fn agent_local_browser_response(
        &self,
        sink: &RunnerSink,
        query: &AgentBrowserRequest,
        config: Option<&LocalBrowserConfig>,
    ) -> Result<webcodex_core::agent_browser::AgentBrowserResponse, String> {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        use webcodex_core::agent_browser::{AgentBrowserImage, AgentBrowserResponse};
        let receipt = self.agent_local_browser(sink, query, config)?;
        let image = if let Some(step) = query.image_step {
            if !matches!(receipt.state.as_str(), "completed" | "failed") {
                return Err("agent_browser_image_not_settled".into());
            }
            let configuration = lock_unpoison(&self.agent_execution)
                .configuration
                .clone()
                .ok_or("agent_browser_history_unavailable")?;
            let path = configuration
                .store
                .browser_artifact_directory(&query.action)?
                .join("evidence")
                .join(format!("step-{step:02}.png"));
            for ancestor in path.ancestors() {
                let metadata =
                    fs::symlink_metadata(ancestor).map_err(|_| "agent_browser_image_missing")?;
                if metadata.file_type().is_symlink() {
                    return Err("agent_browser_image_linked".into());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err("agent_browser_image_linked".into());
                    }
                }
            }
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(1).custom_flags(0x00200000);
            }
            let file = options
                .open(&path)
                .map_err(|_| "agent_browser_image_unavailable")?;
            if !file
                .metadata()
                .map_err(|_| "agent_browser_image_unavailable")?
                .is_file()
            {
                return Err("agent_browser_image_invalid".into());
            }
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "agent_browser_image_unavailable")?;
            if bytes.len() > 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err("agent_browser_image_invalid".into());
            }
            Some(AgentBrowserImage {
                step,
                content_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                file_bytes: bytes.len(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            })
        } else {
            None
        };
        let result = AgentBrowserResponse { receipt, image };
        result.validate_for(query)?;
        Ok(result)
    }
    pub(crate) fn agent_local_browser(
        &self,
        sink: &RunnerSink,
        query: &AgentBrowserRequest,
        config: Option<&LocalBrowserConfig>,
    ) -> Result<AgentBrowserReceipt, String> {
        query.validate()?;
        if query.target.client_id != sink.client_id()
            || query.target.runner_instance_id != sink.runner_instance_id()
        {
            return Err("agent_browser_stale_target".into());
        }
        let configuration = lock_unpoison(&self.agent_execution)
            .configuration
            .clone()
            .ok_or("agent_browser_history_unavailable")?;
        // Browser history failures disable this optional adapter only. They
        // must not prevent ordinary program startup or reset unknown effects.
        configuration.store.initialize_browser_metadata()?;
        if let Some(old) = configuration
            .store
            .browser_metadata(&query.action.action_id)?
        {
            if old.action != query.action {
                return Err("agent_browser_intent_changed".into());
            }
            return Ok(if old.receipt.state == "dispatch_fenced" {
                let mut value = old.receipt;
                value.state = "outcome_unknown".into();
                value.error = Some("agent_browser_original_execution_unsettled".into());
                value
            } else {
                old.receipt
            });
        }
        if query.observe_only {
            return receipt(
                &query.action,
                "outcome_unknown",
                Some("agent_browser_original_delivery_unobserved"),
            );
        }
        let initial = receipt(&query.action, "dispatch_fenced", None)?;
        if !configuration.store.fence_browser(&query.action, &initial)? {
            return receipt(
                &query.action,
                "outcome_unknown",
                Some("agent_browser_original_execution_unsettled"),
            );
        }
        let mut attempted = false;
        let result = self.run_fixed_browser(
            sink,
            &query.action,
            config,
            &configuration.store,
            &mut attempted,
        );
        let final_receipt = match result {
            Ok(result) => result,
            Err(code) => receipt(
                &query.action,
                if attempted {
                    "outcome_unknown"
                } else {
                    "not_started"
                },
                Some(&code),
            )?,
        };
        configuration
            .store
            .finish_browser(&query.action, &final_receipt)?;
        Ok(configuration
            .store
            .browser_metadata(&query.action.action_id)?
            .ok_or("agent_browser_history_missing")?
            .receipt)
    }

    fn run_fixed_browser(
        &self,
        sink: &RunnerSink,
        action: &AgentBrowserAction,
        config: Option<&LocalBrowserConfig>,
        store: &DetachedJobStore,
        attempted: &mut bool,
    ) -> Result<AgentBrowserReceipt, String> {
        let _slot = BROWSER_SLOT.try_lock().map_err(|_| "agent_browser_busy")?;
        let config = config.ok_or("agent_browser_runtime_unavailable")?;
        let _python_pin = pin_binary(&config.python, &config.python_sha256)?;
        let _browser_pin = pin_binary(&config.browser, &config.browser_sha256)?;
        if action.service.expires_at_unix_ms <= chrono::Utc::now().timestamp_millis()
            || self.shutting_down.load(Ordering::SeqCst)
        {
            return Err("agent_browser_admission_closed".into());
        }
        let inspected = self.inspect_agent_program(
            sink,
            &AgentProgramInspect {
                target: action.service.target.clone(),
                binding: action.service.clone(),
            },
        )?;
        let service = inspected
            .service
            .ok_or("agent_browser_service_not_running")?;
        let child = lock_unpoison(&self.agent_execution)
            .live
            .get(&action.service.job_id)
            .and_then(|c| c.child.clone())
            .ok_or("agent_browser_service_not_running")?;
        let alive = || -> bool {
            let mut child = lock_unpoison(&child);
            #[cfg(windows)]
            if let JobChild::Isolated(native) = &mut *child {
                return matches!(native.try_wait(), Ok(None))
                    && native.local_listeners().is_ok_and(|listeners| {
                        listeners.contains(&(service.process_id, service.port))
                    });
            }
            false
        };
        if !alive() {
            return Err("agent_browser_service_not_running".into());
        }
        let folder = store.browser_artifact_directory(action)?;
        if folder.exists() {
            return Err("agent_browser_artifact_collision".into());
        }
        for sub in ["home", "home/AppData/Local", "home/AppData/Roaming", "temp"] {
            fs::create_dir_all(folder.join(sub))
                .map_err(|_| "agent_browser_artifact_unavailable")?;
        }
        let mut command = Command::new(&config.python);
        command
            .args(["-I", "-c", HELPER])
            .current_dir(&folder)
            .env_clear()
            .env("USERPROFILE", folder.join("home"))
            .env("HOME", folder.join("home"))
            .env("APPDATA", folder.join("home/AppData/Roaming"))
            .env("LOCALAPPDATA", folder.join("home/AppData/Local"))
            .env("TEMP", folder.join("temp"))
            .env("TMP", folder.join("temp"))
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let wire = serde_json::json!({"origin":format!("http://127.0.0.1:{}",service.port),"steps":action.steps,"browser_executable":config.browser,"artifact_directory":folder.join("evidence"),"timeout":action.timeout_secs});
        let input =
            serde_json::to_string(&wire).map_err(|_| "agent_browser_encoding_failed")? + "\n";
        if input.len() > 65536 {
            return Err("agent_browser_input_limit".into());
        }
        *attempted = true;
        let mut process = webcodex_process::ManagedChild::spawn_with_options(
            &mut command,
            webcodex_process::SpawnOptions {
                windows_creation_flags: 0x08000000,
                windows_silent_child_breakaway: false,
            },
        )
        .map_err(|_| "agent_browser_process_creation_unknown")?;
        let pid = process.id();
        let mut started = receipt(action, "dispatch_fenced", None)?;
        started.process_id = Some(pid);
        store.finish_browser(action, &started)?;
        let mut stdin = process
            .child_mut()
            .stdin
            .take()
            .ok_or("agent_browser_input_unavailable")?;
        let stdout = process
            .child_mut()
            .stdout
            .take()
            .ok_or("agent_browser_output_unavailable")?;
        let stderr = process
            .child_mut()
            .stderr
            .take()
            .ok_or("agent_browser_output_unavailable")?;
        let input_thread = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let output_thread = std::thread::spawn(move || {
            let mut data = Vec::new();
            stdout
                .take(1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map(|_| data)
        });
        let error_thread = std::thread::spawn(move || {
            let mut data = Vec::new();
            stderr.take(65537).read_to_end(&mut data).map(|_| data)
        });
        let deadline = Instant::now() + Duration::from_secs(action.timeout_secs + 5);
        let mut interrupted = false;
        let exit = loop {
            if Instant::now() >= deadline || self.shutting_down.load(Ordering::SeqCst) || !alive() {
                interrupted = true;
                process
                    .terminate_tree()
                    .map_err(|_| "agent_browser_stop_unknown")?;
            }
            if let Some(exit) = process
                .try_wait()
                .map_err(|_| "agent_browser_exit_unknown")?
            {
                break exit;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if !process
            .wait_tree_exit(Duration::from_secs(3))
            .map_err(|_| "agent_browser_stop_unknown")?
        {
            process
                .terminate_tree()
                .map_err(|_| "agent_browser_stop_unknown")?;
            if !process
                .wait_tree_exit(Duration::from_secs(2))
                .map_err(|_| "agent_browser_stop_unknown")?
            {
                return Err("agent_browser_stop_unknown".into());
            }
            interrupted = true;
        }
        let settle = Instant::now() + Duration::from_secs(2);
        while (!input_thread.is_finished()
            || !output_thread.is_finished()
            || !error_thread.is_finished())
            && Instant::now() < settle
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !input_thread.is_finished()
            || !output_thread.is_finished()
            || !error_thread.is_finished()
        {
            return Err("agent_browser_output_unsettled".into());
        }
        let _ = input_thread.join();
        let output = output_thread
            .join()
            .map_err(|_| "agent_browser_output_unavailable")?
            .map_err(|_| "agent_browser_output_unavailable")?;
        let error = error_thread
            .join()
            .map_err(|_| "agent_browser_output_unavailable")?
            .map_err(|_| "agent_browser_output_unavailable")?;
        fs::write(folder.join("stderr.log"), error)
            .map_err(|_| "agent_browser_artifact_unavailable")?;
        let value = if output.len() <= 1024 * 1024 {
            serde_json::from_slice::<serde_json::Value>(&output).ok()
        } else {
            None
        };
        let success =
            !interrupted && exit.success() && value.as_ref().is_some_and(|v| v["success"] == true);
        let mut result = receipt(
            action,
            if success { "completed" } else { "failed" },
            if success {
                None
            } else {
                Some(if interrupted {
                    "agent_browser_interrupted"
                } else {
                    "agent_browser_helper_failed"
                })
            },
        )?;
        result.process_id = Some(pid);
        result.exit_code = exit.code();
        result.result = value;
        result.validate_for(action)?;
        Ok(result)
    }
}
