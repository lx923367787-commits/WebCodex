//! Preserve native account routing while restricted CLI modes ignore user
//! customizations. Private values stay only in the selected child environment.
use std::{collections::BTreeMap, ffi::OsString, io::Read, path::PathBuf};

pub(super) const CLAUDE_KEYS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];
const INVALID: &str = "CLI 原生账号或模型配置无效；请检查本机配置后刷新。";

pub(super) fn validate_value(name: &str, value: &std::ffi::OsStr) -> Result<(), String> {
    let text = value.to_str().ok_or(INVALID)?;
    if text.len() > 64 * 1024 || text.chars().any(char::is_control) {
        return Err(INVALID.into());
    }
    if matches!(name, "CLAUDE_CONFIG_DIR" | "CODEX_HOME") && !PathBuf::from(value).is_absolute() {
        return Err(INVALID.into());
    }
    if name == "ANTHROPIC_BASE_URL" && !text.is_empty() {
        let url = url::Url::parse(text).map_err(|_| INVALID)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(INVALID.into());
        }
    }
    Ok(())
}

pub(super) fn load_claude_settings(
    environment: &mut BTreeMap<OsString, OsString>,
) -> Result<(), String> {
    let directory = environment
        .get(std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"))
        .map(PathBuf::from)
        .or_else(|| {
            environment
                .get(std::ffi::OsStr::new(if cfg!(windows) {
                    "USERPROFILE"
                } else {
                    "HOME"
                }))
                .map(|p| PathBuf::from(p).join(".claude"))
        });
    let Some(directory) = directory else {
        return Ok(());
    };
    if !directory.is_absolute() {
        return Err(INVALID.into());
    }
    let file = match std::fs::File::open(directory.join("settings.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(INVALID.into()),
    };
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() > 256 * 1024 {
        return Err(INVALID.into());
    }
    let settings: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    project_settings(&settings, environment)
}

fn project_settings(
    settings: &serde_json::Value,
    environment: &mut BTreeMap<OsString, OsString>,
) -> Result<(), String> {
    let Some(env) = settings.get("env").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    let account_keys = [
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ];
    let account_from_environment = account_keys
        .iter()
        .any(|key| environment.contains_key(std::ffi::OsStr::new(key)));
    for &name in CLAUDE_KEYS
        .iter()
        .filter(|&&name| name != "CLAUDE_CONFIG_DIR")
    {
        // Explicit process environment wins. Never copy arbitrary settings,
        // apiKeyHelper shell commands, subagent models, hooks, or MCP servers.
        if environment.contains_key(std::ffi::OsStr::new(name))
            || (account_from_environment && account_keys.contains(&name))
        {
            continue;
        }
        if let Some(value) = env.get(name) {
            let value = OsString::from(value.as_str().ok_or(INVALID)?);
            validate_value(name, &value)?;
            environment.insert(name.into(), value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_native_gateway_values_are_forwarded_and_errors_are_redacted() {
        let settings = serde_json::json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example/v1", "ANTHROPIC_AUTH_TOKEN":"synthetic-token", "ANTHROPIC_MODEL":"actual-model", "WEBCODEX_TOKEN":"wrong-audience", "NODE_OPTIONS":"--require malware", "CLAUDE_CODE_SUBAGENT_MODEL":"nested"},"hooks":{"start":"arbitrary command"}});
        let mut env = BTreeMap::new();
        project_settings(&settings, &mut env).unwrap();
        assert_eq!(env.len(), 3);
        assert_eq!(
            env.get(std::ffi::OsStr::new("ANTHROPIC_MODEL")),
            Some(&OsString::from("actual-model"))
        );
        env.insert("ANTHROPIC_MODEL".into(), "explicit-model".into());
        project_settings(&settings, &mut env).unwrap();
        assert_eq!(
            env.get(std::ffi::OsStr::new("ANTHROPIC_MODEL")),
            Some(&OsString::from("explicit-model"))
        );
        let error = validate_value(
            "ANTHROPIC_BASE_URL",
            std::ffi::OsStr::new("https://user:secret@example.com"),
        )
        .unwrap_err();
        assert!(!error.contains("secret"));
        let mut override_route = BTreeMap::from([(
            "ANTHROPIC_BASE_URL".into(),
            "https://different.example".into(),
        )]);
        project_settings(&settings, &mut override_route).unwrap();
        assert!(!override_route.contains_key(std::ffi::OsStr::new("ANTHROPIC_AUTH_TOKEN")));
    }
}
