//! Product-neutral CLI adapter definitions. These describe native protocols,
//! never installation, account access, resource approval or execution authority.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_CLI_ADAPTERS: usize = 32;

pub fn valid_cli_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CliAdapter {
    pub id: String,
    pub label: String,
    /// Basenames searched in PATH. No shell launchers are executed.
    pub executables: Vec<String>,
    /// Per-user locations, relative to USERPROFILE/HOME or LOCALAPPDATA.
    #[serde(default)]
    pub user_paths: Vec<CliUserPath>,
    /// Metadata recipe, owned by the local adapter registry.
    pub metadata: CliMetadataRecipe,
    /// None means discovered installation only; invocation needs an adapter.
    pub invocation: Option<CliInvocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CliUserPath {
    pub root: CliPathRoot,
    pub path: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliPathRoot {
    Home,
    LocalAppData,
    AppData,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliMetadataRecipe {
    Help,
    CodebuddyHelp,
    AntigravityModels,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CliInvocation {
    #[serde(default)]
    pub prefix_args: Vec<String>,
    pub model_flag: String,
    pub effort_flag: Option<String>,
    #[serde(default)]
    pub effort_required: bool,
    #[serde(default)]
    pub fixed_args: Vec<String>,
    pub input: CliInputFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CliResultContract>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CliResultContract {
    pub format: CliResultFormat,
    /// A top-level boolean reported by the CLI protocol, not the model text.
    pub failure_flag: String,
    pub terminal_field: Option<String>,
    pub terminal_value: Option<String>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliResultFormat { Json, JsonLines }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliInputFormat {
    Text,
    UserMessageJsonLine,
}

impl CliAdapter {
    pub fn validate(&self) -> Result<(), String> {
        let bounded = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        if !valid_cli_id(&self.id)
            || !bounded(&self.label, 128)
            || self.executables.is_empty()
            || self.executables.len() > 8
            || self.user_paths.len() > 16
            || self.executables.iter().any(|name| {
                !bounded(name, 128)
                    || name.contains(['/', '\\', ':'])
                    || name == "."
                    || name == ".."
            })
        {
            return Err("cli_adapter_invalid_identity".into());
        }
        for location in &self.user_paths {
            if !bounded(&location.path, 512)
                || location.path.starts_with(['/', '\\'])
                || location.path.contains(':')
                || location
                    .path
                    .split(['/', '\\'])
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err("cli_adapter_invalid_location".into());
            }
        }
        if let Some(invocation) = &self.invocation {
            if let Some(result) = &invocation.result {
                if !valid_cli_id(&result.failure_flag) || result.terminal_field.is_some() != result.terminal_value.is_some()
                    || result.terminal_field.as_ref().is_some_and(|s| !valid_cli_id(s))
                    || result.terminal_value.as_ref().is_some_and(|s| !bounded(s, 128))
                    || (result.format == CliResultFormat::JsonLines && result.terminal_field.is_none()) {
                    return Err("cli_adapter_invalid_result_contract".into());
                }
            }
            let flag = |s: &str| {
                s.starts_with("--")
                    && s.len() > 2
                    && s.len() <= 64
                    && s[2..]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            };
            if !flag(&invocation.model_flag)
                || invocation.effort_flag.as_ref() == Some(&invocation.model_flag)
                || (invocation.effort_required && invocation.effort_flag.is_none())
                || invocation.effort_flag.as_ref().is_some_and(|s| !flag(s))
                || invocation.prefix_args.len() + invocation.fixed_args.len() > 32
                || invocation
                    .prefix_args
                    .iter()
                    .chain(&invocation.fixed_args)
                    .any(|s| !bounded(s, 256))
                || invocation.prefix_args.iter().chain(&invocation.fixed_args)
                    .any(|s| s == "--" || s == &invocation.model_flag || invocation.effort_flag.as_ref() == Some(s))
            {
                return Err("cli_adapter_invalid_invocation".into());
            }
        }
        Ok(())
    }

    pub fn invocation(
        &self,
        model: &str,
        effort: Option<&str>,
        instruction: &str,
    ) -> Result<(Vec<String>, String), String> {
        self.validate()?;
        let invocation = self
            .invocation
            .as_ref()
            .ok_or("cli_adapter_invocation_unavailable")?;
        if model.trim().is_empty()
            || model.starts_with(['-', '@'])
            || model.len() > 256
            || model.chars().any(char::is_control)
            || effort.is_some_and(|e| {
                e.trim().is_empty() || e.starts_with(['-', '@']) || e.len() > 64 || e.chars().any(char::is_control)
            })
            || instruction.is_empty()
            || instruction.len() > 512 * 1024
            || instruction.contains('\0')
        {
            return Err("cli_adapter_invalid_selection".into());
        }
        let mut args = invocation.prefix_args.clone();
        args.extend([invocation.model_flag.clone(), model.into()]);
        if let Some(effort) = effort {
            let flag = invocation
                .effort_flag
                .as_ref()
                .ok_or("cli_adapter_effort_unsupported")?;
            args.extend([flag.clone(), effort.into()]);
        } else if invocation.effort_required {
            return Err("cli_adapter_effort_required".into());
        }
        args.extend(invocation.fixed_args.clone());
        let stdin = match invocation.input {
            CliInputFormat::Text => instruction.into(),
            CliInputFormat::UserMessageJsonLine => format!(
                "{}\n",
                serde_json::json!({"event":"user","message":{"content":instruction}})
            ),
        };
        Ok((args, stdin))
    }
}

/// Explicit local definitions extend or replace default protocol recipes.
/// Duplicate local IDs are rejected. This is not a model-facing setter;
/// installation/resource identity and execution approval are checked separately.
pub fn cli_adapters(additions: &[CliAdapter]) -> Result<Vec<CliAdapter>, String> {
    let mut adapters: Vec<CliAdapter> = serde_json::from_str(include_str!("cli_adapters.json"))
        .map_err(|_| "cli_builtin_registry_invalid")?;
    let mut local_ids = BTreeSet::new();
    for addition in additions {
        if !local_ids.insert(&addition.id) { return Err("cli_adapter_duplicate_id".into()); }
        if let Some(existing) = adapters.iter_mut().find(|entry| entry.id == addition.id) {
            *existing = addition.clone();
        } else { adapters.push(addition.clone()); }
    }
    if adapters.len() > MAX_CLI_ADAPTERS {
        return Err("cli_adapter_limit".into());
    }
    let mut seen = BTreeSet::new();
    for adapter in &adapters {
        adapter.validate()?;
        if !seen.insert(&adapter.id) {
            return Err("cli_adapter_duplicate_id".into());
        }
    }
    Ok(adapters)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_cli_extends_registry_without_becoming_a_shell_command() {
        let custom = CliAdapter {
            id: "local-model".into(),
            label: "Local model".into(),
            executables: vec!["local-model.exe".into()],
            user_paths: vec![],
            metadata: CliMetadataRecipe::Help,
            invocation: Some(CliInvocation {
                prefix_args: vec!["run".into()],
                model_flag: "--model".into(),
                effort_flag: None,
                effort_required: false,
                fixed_args: vec![],
                    input: CliInputFormat::Text,
                    result: None,
            }),
        };
        let entries = cli_adapters(&[custom.clone()]).unwrap();
        assert!(entries.iter().any(|entry| entry.id == "local-model"));
        let (args, stdin) = custom
            .invocation("model-name", None, "a\nb; not shell")
            .unwrap();
        assert_eq!(args, ["run", "--model", "model-name"]);
        assert_eq!(stdin, "a\nb; not shell");
        assert!(custom
            .invocation("model-name", Some("invented"), "task")
            .is_err());
        assert!(cli_adapters(&[custom.clone(), custom]).is_err());
        assert!(!valid_cli_id("cmd.exe /c whoami"));
    }
}
