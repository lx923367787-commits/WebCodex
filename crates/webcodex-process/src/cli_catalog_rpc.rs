//! Fixed metadata-only handshakes. Never send a prompt, start a thread/turn,
//! approve a tool, or echo CLI diagnostics/account fields to the caller.
use super::{valid_model_id, OUTPUT_LIMIT};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};

const INVALID: &str = "CLI 模型目录协议无效或超出限制。";
const CLOSED: &str = "CLI 模型目录查询未完成，请检查 CLI 登录和版本。";

#[derive(Clone, Copy)]
pub(super) enum Protocol {
    Codex,
    Claude,
}

impl Protocol {
    pub(super) fn args(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &["app-server", "--listen", "stdio://"],
            Self::Claude => &[
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
                "--safe-mode",
                "--restricted",
                "--strict-mcp-config",
                "--permission-prompts",
                "none",
            ],
        }
    }
}

fn send(writer: &mut impl Write, value: Value) -> Result<(), &'static str> {
    let mut bytes = serde_json::to_vec(&value).map_err(|_| INVALID)?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|_| writer.flush())
        .map_err(|_| CLOSED)
}

/// One total byte budget across notifications and all pages, not per line.
fn receive(reader: &mut impl BufRead, remaining: &mut usize) -> Result<Value, &'static str> {
    let mut bytes = Vec::new();
    reader
        .take((*remaining + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| CLOSED)?;
    if bytes.is_empty() {
        return Err(CLOSED);
    }
    if bytes.len() > *remaining {
        return Err(INVALID);
    }
    *remaining -= bytes.len();
    serde_json::from_slice(&bytes).map_err(|_| INVALID)
}

fn codex_reply(
    reader: &mut impl BufRead,
    remaining: &mut usize,
    id: u64,
) -> Result<Value, &'static str> {
    loop {
        let value = receive(reader, remaining)?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if value.get("error").is_some() {
                return Err(CLOSED);
            }
            return value
                .get("result")
                .filter(|v| v.is_object())
                .cloned()
                .ok_or(INVALID);
        }
        // No server-to-client request may be answered by a metadata query.
        if value.get("id").is_some() {
            return Err(INVALID);
        }
    }
}

pub(super) fn exchange(
    protocol: Protocol,
    output: impl Read,
    mut input: impl Write,
) -> Result<Vec<u8>, &'static str> {
    let mut reader = BufReader::new(output);
    let mut remaining = OUTPUT_LIMIT;
    let mut choices = BTreeMap::new();
    match protocol {
        Protocol::Codex => {
            send(
                &mut input,
                json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"webcodex_cli_catalog","title":"WebCodex CLI catalog","version":"1"}}}),
            )?;
            codex_reply(&mut reader, &mut remaining, 1)?;
            send(&mut input, json!({"method":"initialized"}))?;
            let mut cursor = Value::Null;
            let mut cursors = BTreeSet::new();
            for id in 2..=17 {
                send(
                    &mut input,
                    json!({"id":id,"method":"model/list","params":{"limit":100,"includeHidden":false,"cursor":cursor}}),
                )?;
                let response = codex_reply(&mut reader, &mut remaining, id)?;
                add_models(&mut choices, response.get("data").ok_or(INVALID)?, protocol)?;
                match response.get("nextCursor") {
                    None | Some(Value::Null) => break,
                    Some(Value::String(next))
                        if !next.is_empty()
                            && next.len() <= 1024
                            && cursors.insert(next.clone())
                            && id < 17 =>
                    {
                        cursor = Value::String(next.clone())
                    }
                    _ => return Err(INVALID),
                }
            }
        }
        Protocol::Claude => {
            send(
                &mut input,
                json!({"type":"control_request","request_id":"webcodex_catalog","request":{"subtype":"initialize","hooks":{},"sdkMcpServers":[]}}),
            )?;
            loop {
                let value = receive(&mut reader, &mut remaining)?;
                if value.get("type").and_then(Value::as_str) == Some("control_request") {
                    return Err(INVALID);
                }
                if value.get("type").and_then(Value::as_str) != Some("control_response") {
                    continue;
                }
                let response = value.get("response").ok_or(INVALID)?;
                if response.get("request_id").and_then(Value::as_str) != Some("webcodex_catalog") {
                    return Err(INVALID);
                }
                if response.get("subtype").and_then(Value::as_str) != Some("success") {
                    return Err(CLOSED);
                }
                add_models(
                    &mut choices,
                    response
                        .get("response")
                        .and_then(|v| v.get("models"))
                        .ok_or(INVALID)?,
                    protocol,
                )?;
                break;
            }
        }
    }
    if choices.is_empty() {
        return Err(CLOSED);
    }
    serde_json::to_vec(&choices).map_err(|_| INVALID)
}

fn valid_catalog_model(id: &str) -> bool {
    if valid_model_id(id) {
        return true;
    }
    id.split_once('[').is_some_and(|(base, suffix)| {
        valid_model_id(base)
            && suffix.strip_suffix(']').is_some_and(|size| {
                !size.is_empty() && size.bytes().all(|b| b.is_ascii_alphanumeric())
            })
    })
}

fn add_models(
    choices: &mut BTreeMap<String, Vec<String>>,
    models: &Value,
    protocol: Protocol,
) -> Result<(), &'static str> {
    for model in models
        .as_array()
        .filter(|rows| rows.len() <= 256)
        .ok_or(INVALID)?
    {
        if model.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let key = match protocol {
            Protocol::Codex => "model",
            Protocol::Claude => "value",
        };
        let id = (match protocol {
            Protocol::Claude => model.get("resolvedModel").or_else(|| model.get(key)),
            Protocol::Codex => model.get(key),
        })
        .and_then(Value::as_str)
        .filter(|id| id.len() <= 128 && valid_catalog_model(id))
        .ok_or(INVALID)?;
        let mut efforts = Vec::new();
        let levels = match protocol {
            Protocol::Codex => model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .ok_or(INVALID)?,
            Protocol::Claude => {
                if model.get("supportsEffort").is_none()
                    || model.get("supportsEffort").and_then(Value::as_bool) == Some(false)
                {
                    if choices.insert(id.into(), efforts).is_some() {
                        return Err(INVALID);
                    }
                    continue;
                }
                model
                    .get("supportedEffortLevels")
                    .and_then(Value::as_array)
                    .ok_or(INVALID)?
            }
        };
        if levels.len() > 32 {
            return Err(INVALID);
        }
        for level in levels {
            let value = match protocol {
                Protocol::Codex => level.get("reasoningEffort").and_then(Value::as_str),
                Protocol::Claude => level.as_str(),
            }
            .filter(|s| s.len() <= 64 && valid_model_id(s))
            .ok_or(INVALID)?;
            if efforts.iter().any(|existing| existing == value) {
                return Err(INVALID);
            }
            efforts.push(value.into());
        }
        if let Some(previous) = choices.get(id) {
            if !matches!(protocol, Protocol::Claude) || previous != &efforts {
                return Err(INVALID);
            }
        } else {
            choices.insert(id.into(), efforts);
        }
        if choices.len() > 256 {
            return Err(INVALID);
        }
    }
    Ok(())
}

pub(super) fn parse_models(output: &str) -> Result<BTreeMap<String, Vec<String>>, &'static str> {
    serde_json::from_str(output).map_err(|_| INVALID)
}

#[cfg(test)]
#[path = "cli_catalog_rpc_tests.rs"]
mod tests;
