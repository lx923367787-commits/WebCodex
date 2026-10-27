//! Explicit operator-only acceptance of the existing pinned native mapping.
//! This does not publish a review record or reserve a real team's CLI slot.
use super::*;
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    thread,
    time::{Duration, Instant},
};
use webcodex_core::agent_cli_controls::*;
use webcodex_core::runner_protocol::{compute_launch_intent_hash, AgentExecutionProcess};
use webcodex_process::windows_lpac::LpacChild;

#[test]
#[ignore = "uses an explicitly selected installed CodeBuddy account and real LPAC process"]
fn installed_codebuddy_operator_lpac_probe() {
    let base = PathBuf::from(
        std::env::var_os("WEBCODEX_CODEBUDDY_PROBE_ROOT")
            .expect("explicit validation root required"),
    );
    let program = PathBuf::from(
        std::env::var_os("WEBCODEX_CODEBUDDY_PROBE_EXE")
            .expect("explicit installed executable required"),
    );
    assert!(base.is_absolute() && base.is_dir());
    assert!(program.is_absolute() && program.is_file());
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&program).unwrap())),
        CODEBUDDY_FILES_EXE_SHA256
    );
    let temporary = tempfile::Builder::new()
        .prefix("codebuddy-lpac-")
        .tempdir_in(&base)
        .unwrap();
    let root = temporary.path().join("resources");
    let pins = initialize_resource_root(&root).unwrap();
    let mut files = Vec::new();
    for (name, bytes) in [
        (CODEBUDDY_FILES_MCP_PATH, CODEBUDDY_FILES_MCP_BYTES),
        (
            CODEBUDDY_FILES_SETTINGS_PATH,
            CODEBUDDY_FILES_SETTINGS_BYTES,
        ),
    ] {
        let path = temporary.path().join(Path::new(name).file_name().unwrap());
        fs::write(&path, bytes).unwrap();
        files.push(AgentExecutionResourceFile {
            source: path,
            destination: name.into(),
            sha256: webcodex_core::agent_cli::sha256(bytes),
        });
    }
    let config = AgentExecutionResourcesConfig {
        profiles: vec![AgentExecutionResourceProfile {
            id: "operator-codebuddy-native".into(),
            provider: Some(AgentCliProvider::WorkBuddy),
            reviewed_controls: None,
            user_https_proxy: false,
            codebuddy_config_directory: None,
            executable: program,
            executable_sha256: CODEBUDDY_FILES_EXE_SHA256.into(),
            files,
        }],
    };
    let profiles = validate_profiles(&config).unwrap();
    let instruction = "Using only Write, create runner-cli-smoke.txt in the current workspace containing exactly WEBCODEX_ISOLATED_CLI_OK. Do not read any other files. Return WEBCODEX_ISOLATED_CLI_OK.";
    let process = AgentExecutionProcess {
        executable: format!("sha256:{CODEBUDDY_FILES_EXE_SHA256}"),
        args: codebuddy_file_probe_args(),
    };
    let binding = serde_json::from_value(json!({
        "version":1,"project":"operator-codebuddy-files-probe",
        "requester":{"enrollment_id":"operator-enrollment","run":{"agent_id":"operator","agent_run_id":"operator-run","generation":1},"root_task_id":"operator-task","source_task_id":"operator-task","call_index":0,"cli_attempt_id":"operator-probe-attempt"},
        "executor":{"enrollment_id":"probe-enrollment","run":{"agent_id":"probe-worker","agent_run_id":"probe-run","generation":1},"task_id":"probe-task","cli_session_scope_id":"probe-session"},
        "target":{"client_id":"operator-runner","runner_instance_id":"operator-instance"},
        "carrier":{"kind":"job","job_id":"operator-probe-job","start_request_id":"operator-probe-request"},
        "launch_intent_hash":compute_launch_intent_hash(&process,Some(instruction),60).unwrap(),
        "resource_manifest_hash":profiles.manifest("operator-codebuddy-native").unwrap()
    })).unwrap();
    let start = AgentExecutionStart {
        workspace_source: None,
        binding,
        resource_set_id: "operator-codebuddy-native".into(),
        process,
        stdin: Some(instruction.into()),
        timeout_secs: 60,
    };
    let prepared = prepare_codebuddy_files_probe(&config, &start, &root).unwrap();
    let workspace = prepared.cwd.clone();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(60);
    let mut child = LpacChild::spawn(prepared.launch_spec()).unwrap();
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let capture = |pipe: fs::File| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(2 * 1024 * 1024).read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = capture(out);
    let stderr = capture(err);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(instruction.as_bytes())
        .unwrap();
    let mut timed_out = false;
    loop {
        if child.try_wait().unwrap().is_some() && child.active_processes().unwrap() == 0 {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            child.terminate_tree().unwrap();
            assert!(child.wait_tree_exit(Duration::from_secs(10)).unwrap());
            break;
        }
        child.capture_member_handles(deadline).unwrap();
        thread::sleep(Duration::from_millis(50));
    }
    let close_deadline = Instant::now() + Duration::from_secs(5);
    let tree = loop {
        if let Some(proof) = child.observe_completed_tree(close_deadline).unwrap() {
            break proof;
        }
        assert!(
            Instant::now() < close_deadline,
            "complete tree proof required"
        );
        thread::sleep(Duration::from_millis(10));
    };
    let output = stdout.join().unwrap();
    let error = stderr.join().unwrap();
    let diagnostic_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output),
        String::from_utf8_lossy(&error)
    )
    .to_ascii_lowercase();
    let records: Vec<serde_json::Value> = String::from_utf8_lossy(&output)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let tools: Vec<_> = records
        .iter()
        .filter(|v| v["type"] == "system" && v["subtype"] == "init")
        .flat_map(|v| v["tools"].as_array().cloned().unwrap_or_default())
        .collect();
    let marker = fs::read_to_string(workspace.join("runner-cli-smoke.txt"))
        .ok()
        .is_some_and(|s| s.trim() == "WEBCODEX_ISOLATED_CLI_OK");
    let evidence = json!({"kind":"operator_lpac_probe_not_team_job","version":CODEBUDDY_FILES_VERSION,"model":"hy3","effort":"minimal","exit_code":tree.leader_exit_code(),"members":tree.member_count(),"timed_out":timed_out,"elapsed_seconds":started.elapsed().as_secs_f64(),"file_marker_verified":marker,"tools":tools,"stdout_sha256":webcodex_core::agent_cli::sha256(&output),"stderr_sha256":webcodex_core::agent_cli::sha256(&error),"stderr_bytes":error.len()});
    drop(child);
    drop(prepared);
    cleanup_native_auth(&start.binding, &root).unwrap();
    drop(pins);
    let mut evidence = evidence;
    evidence["diagnostic_signals"] = json!({
        "permission_denied": diagnostic_text.contains("eacces") || diagnostic_text.contains("permission denied") || diagnostic_text.contains("access denied"),
        "authentication": diagnostic_text.contains("unauthorized") || diagnostic_text.contains("not logged") || diagnostic_text.contains("401"),
        "network": diagnostic_text.contains("fetch failed") || diagnostic_text.contains("connect") || diagnostic_text.contains("timeout"),
        "stdout_bytes": output.len()
    });
    evidence["record_states"] = json!(records
        .iter()
        .map(|r| json!({
            "type":r.get("type"),"subtype":r.get("subtype"),"is_error":r.get("is_error"),
            "model":r.get("model"),"has_result":r.get("result").is_some(),
            "message_role":r.get("message").and_then(|m| m.get("role"))
        }))
        .collect::<Vec<_>>());
    fs::write(
        base.join("operator-lpac-probe.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    println!("{evidence}");
    assert!(!timed_out);
    assert_eq!(tree.leader_exit_code(), 0);
    assert!(
        marker,
        "isolated file marker missing; see redacted operator evidence"
    );
    assert!(!tools.is_empty());
    assert!(tools.iter().all(|tool| matches!(
        tool.as_str(),
        Some("Read" | "Glob" | "Grep" | "Edit" | "Write")
    )));
}
