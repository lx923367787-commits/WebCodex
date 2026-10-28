// Standalone newline-delimited WebCodex Tool Plugin fixture. Runner tests
// compile this file directly with rustc, so the native Plugin ABI has no SDK or
// external runtime dependency.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn main() -> io::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let scenario = args.first().map(String::as_str).unwrap_or("normal");
    let marker = args.get(1).map(Path::new);
    if scenario == "hold" {
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
    append(marker, "start\n")?;
    if scenario.starts_with("lifecycle") {
        append(marker, &format!("lifecycle-pid:{}\n", std::process::id()))?;
    }
    if scenario.starts_with("check_") || scenario.starts_with("candidate_") {
        append(marker, &format!("candidate-pid:{}\n", std::process::id()))?;
    }
    if scenario == "reload_new" {
        append(marker, "reload-new-start\n")?;
    }
    if scenario == "execution_context" {
        append(
            marker,
            if env::var("WEBCODEX_PLUGIN_TEST_ENV").as_deref() == Ok("profile-ready") {
                "profile-env-ok\n"
            } else {
                "profile-env-bad\n"
            },
        )?;
        append(
            marker,
            if env::var_os("WEBCODEX_AGENT_TOKEN").is_none() {
                "sensitive-env-cleared\n"
            } else {
                "sensitive-env-leaked\n"
            },
        )?;
        let cwd_ok = args
            .get(2)
            .and_then(|expected| env::current_dir().ok().map(|current| (expected, current)))
            .is_some_and(|(expected, current)| {
                std::fs::canonicalize(expected).ok() == std::fs::canonicalize(current).ok()
            });
        append(marker, if cwd_ok { "cwd-ok\n" } else { "cwd-bad\n" })?;
    }
    if scenario == "stderr" {
        eprintln!("diagnostic-only-secret-looking-stderr");
    } else if scenario == "stderr_flood" {
        let mut stderr = io::stderr().lock();
        for index in 0..256usize {
            writeln!(stderr, "stderr-flood-{index:03}-{}", "x".repeat(2 * 1024))?;
        }
        stderr.flush()?;
    }

    let mut reader = BufReader::new(io::stdin().lock());
    let mut writer = io::stdout().lock();
    let mut lists = 0usize;
    let mut calls = 0usize;
    let mut lifecycle_instance: Option<String> = None;
    let mut lifecycle_stopped = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let method = string_field(&line, "method").unwrap_or_default();
        let id = u64_field(&line, "id").unwrap_or(0);
        match method.as_str() {
            "initialize" => {
                append(marker, "initialize\n")?;
                if scenario == "init_crash" || scenario == "check_init_crash" {
                    return Ok(());
                }
                if scenario == "check_init_timeout" {
                    thread::sleep(Duration::from_secs(3));
                }
                if scenario == "check_bad_version_tree" {
                    let child = Command::new(env::current_exe()?)
                        .arg("hold")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()?;
                    append(marker, &format!("descendant-pid:{}\n", child.id()))?;
                    drop(child);
                }
                let version = if matches!(scenario, "bad_version" | "check_bad_version_tree") {
                    "unsupported-plugin-v0"
                } else {
                    "webcodex-plugin-v1"
                };
                let lifecycle = if scenario.starts_with("lifecycle") {
                    let provider = if scenario == "lifecycle_bad_capability" {
                        "other"
                    } else {
                        "chat-agents"
                    };
                    format!(
                        r#","runtimeLifecycle":{{"protocolVersion":"webcodex-chat-agents-lifecycle-v1","providerId":"{provider}"}}"#
                    )
                } else {
                    String::new()
                };
                send(
                    &mut writer,
                    &format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"{version}"{lifecycle}}}}}"#
                    ),
                )?;
            }
            "tools/list" => {
                lists += 1;
                append(marker, "list\n")?;
                if matches!(scenario, "reload_block_list" | "candidate_block_list_tree")
                    && lists == 1
                {
                    append(
                        marker,
                        if scenario == "reload_block_list" {
                            "reload-blocked\n"
                        } else {
                            "candidate-blocked\n"
                        },
                    )?;
                    if scenario == "candidate_block_list_tree" {
                        let child = Command::new(env::current_exe()?)
                            .arg("hold")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()?;
                        append(marker, &format!("descendant-pid:{}\n", child.id()))?;
                        drop(child);
                    }
                    let release = marker
                        .map(|path| path.with_extension("release"))
                        .expect("blocking candidate scenario requires marker path");
                    while !release.exists() {
                        thread::sleep(Duration::from_millis(10));
                    }
                    append(
                        marker,
                        if scenario == "reload_block_list" {
                            "reload-released\n"
                        } else {
                            "candidate-released\n"
                        },
                    )?;
                }
                if scenario == "split_timeout" && lists >= 2 {
                    thread::sleep(Duration::from_millis(750));
                }
                if scenario == "malformed" {
                    send(&mut writer, "{not-json")?;
                    continue;
                }
                if scenario == "oversized_message" {
                    send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"padding":"{}","tools":[]}}}}"#,
                            "x".repeat(1024 * 1024)
                        ),
                    )?;
                    continue;
                }
                if scenario == "check_malformed_tools_list" {
                    send(
                        &mut writer,
                        &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"notTools":[]}}}}"#),
                    )?;
                    continue;
                }
                if scenario == "check_duplicate_tools" {
                    send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"echo","inputSchema":{{"type":"object"}}}},{{"name":"echo","inputSchema":{{"type":"object"}}}}]}}}}"#
                        ),
                    )?;
                    continue;
                }
                if scenario == "check_invalid_tool_name" {
                    send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"bad name","inputSchema":{{"type":"object"}}}}]}}}}"#
                        ),
                    )?;
                    continue;
                }
                if matches!(scenario, "invalid_tools" | "check_invalid_tools") {
                    send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"echo","inputSchema":[]}}]}}}}"#
                        ),
                    )?;
                    continue;
                }
                if scenario == "check_oversized_schema" {
                    send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"echo","inputSchema":{{"type":"object","description":"{}"}}}}]}}}}"#,
                            "x".repeat(70 * 1024)
                        ),
                    )?;
                    continue;
                }
                if scenario == "check_unsupported_schema" {
                    send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"echo","inputSchema":{{"type":"object","$ref":"https://example.invalid/secret-schema"}}}}]}}}}"#
                        ),
                    )?;
                    continue;
                }
                let value_type = if scenario == "schema_change" && lists >= 2 {
                    "number"
                } else {
                    "string"
                };
                let tool_name = if scenario == "check_v2" {
                    "echo_v2"
                } else {
                    "echo"
                };
                let startup_padding = if scenario == "check_startup_large_schema" {
                    "x".repeat(40 * 1024)
                } else {
                    String::new()
                };
                if scenario == "check_success_tree" {
                    let child = Command::new(env::current_exe()?)
                        .arg("hold")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()?;
                    append(marker, &format!("descendant-pid:{}\n", child.id()))?;
                    drop(child);
                }
                let output_schema = if scenario == "output_schema_invalid" {
                    r#","outputSchema":{"type":"object","properties":{"call":{"type":"string"}},"required":["call"],"additionalProperties":false}"#.to_string()
                } else {
                    String::new()
                };
                if scenario == "check_stderr_at_list" {
                    let mut stderr = io::stderr().lock();
                    writeln!(stderr, "diagnostic-written-before-list-response")?;
                    stderr.flush()?;
                }
                send(
                    &mut writer,
                    &format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"{tool_name}","description":"Native plugin echo","inputSchema":{{"type":"object","description":"{startup_padding}","properties":{{"value":{{"type":"{value_type}"}}}}}}{output_schema}}}]}}}}"#
                    ),
                )?;
                if matches!(
                    scenario,
                    "block_after_preflight" | "block_after_preflight_tree"
                ) && lists >= 1
                {
                    append(marker, "stdin-blocked\n")?;
                    if scenario == "block_after_preflight_tree" {
                        let child = Command::new(env::current_exe()?)
                            .arg("hold")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()?;
                        append(marker, &format!("descendant-pid:{}\n", child.id()))?;
                        drop(child);
                    }
                    loop {
                        thread::sleep(Duration::from_secs(60));
                    }
                }
            }
            "webcodex/chat-agents/lifecycle" => {
                let action = string_field(&line, "action").unwrap_or_default();
                let instance = string_field(&line, "providerInstanceId").unwrap_or_default();
                let valid = string_field(&line, "protocolVersion").as_deref()
                    == Some("webcodex-chat-agents-lifecycle-v1")
                    && string_field(&line, "providerId").as_deref() == Some("chat-agents")
                    && instance.len() == 32
                    && instance
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    && matches!(action.as_str(), "activate" | "deactivate")
                    && lifecycle_instance
                        .as_ref()
                        .is_none_or(|old| old == &instance);
                append(marker, &format!("runtime-{action}:{instance}\n"))?;
                if !valid || (action == "activate" && lifecycle_stopped) {
                    send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32001,"message":"invalid lifecycle"}}}}"#
                        ),
                    )?;
                    continue;
                }
                lifecycle_instance = Some(instance.clone());
                let state = if action == "activate" {
                    append(marker, &format!("runtime-active:{instance}\n"))?;
                    if scenario == "lifecycle_lost_activate" {
                        continue;
                    }
                    "active"
                } else if scenario == "lifecycle_stop_unknown" {
                    "unknown"
                } else {
                    lifecycle_stopped = true;
                    append(marker, &format!("runtime-stopped:{instance}\n"))?;
                    "stopped"
                };
                let returned_instance = if scenario == "lifecycle_wrong_instance" {
                    "0".repeat(32)
                } else {
                    instance
                };
                send(
                    &mut writer,
                    &format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"webcodex-chat-agents-lifecycle-v1","providerId":"chat-agents","providerInstanceId":"{returned_instance}","state":"{state}"}}}}"#
                    ),
                )?;
            }
            "tools/call" => {
                calls += 1;
                append(marker, "call\n")?;
                match scenario {
                    "single_block_call" => {
                        let release = marker
                            .map(|path| path.with_extension("release"))
                            .expect("blocking call scenario requires marker path");
                        while !release.exists() {
                            thread::sleep(Duration::from_millis(10));
                        }
                        send_result(&mut writer, id, calls)?;
                    }
                    "crash" => return Ok(()),
                    "timeout" => thread::sleep(Duration::from_secs(3)),
                    "split_timeout" => {
                        thread::sleep(Duration::from_millis(750));
                        send_result(&mut writer, id, calls)?;
                    }
                    "slow" => {
                        thread::sleep(Duration::from_millis(750));
                        send_result(&mut writer, id, calls)?;
                    }
                    "rpc_error" => send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32001,"message":"fixture error"}}}}"#
                        ),
                    )?,
                    "bad_result" => send(
                        &mut writer,
                        &format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"content":[{{"type":"image","data":"AA=="}}],"isError":false}}}}"#
                        ),
                    )?,
                    _ => send_result(&mut writer, id, calls)?,
                }
            }
            _ => {}
        }
    }
}

fn send_result(writer: &mut impl Write, id: u64, calls: usize) -> io::Result<()> {
    send(
        writer,
        &format!(
            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"content":[{{"type":"text","text":"call-{calls}"}}],"structuredContent":{{"call":{calls}}},"isError":false}}}}"#
        ),
    )
}

fn send(writer: &mut impl Write, message: &str) -> io::Result<()> {
    writer.write_all(message.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn append(path: Option<&Path>, value: &str) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(value.as_bytes())
}

fn string_field(body: &str, field: &str) -> Option<String> {
    let prefix = format!(r#""{field}":"#);
    let start = body.find(&prefix)? + prefix.len();
    let value = body.get(start..)?.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_string())
}

fn u64_field(body: &str, field: &str) -> Option<u64> {
    let prefix = format!(r#""{field}":"#);
    let start = body.find(&prefix)? + prefix.len();
    body.get(start..)?
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}
