//! Bounded reviewed-control facts and shared pure P/R/F/argv operations.
//! Shape/hash validity never proves trusted publication, provider verification,
//! independent review or execution authority. The pinned CodeBuddy file-worker
//! mapping is an implementation candidate, not real control-effect evidence or
//! a production approval. There is no caller-supplied approval bit.
use crate::agent_cli::AgentCliProvider;
use crate::runner_protocol::{validate_process_argv, ShellProcessArgv};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const REVIEWED_CONTROLS_MAX_BYTES: usize = 32 * 1024;
pub const REVIEWED_CONTROL_MAX_FILES: usize = 16;
pub const REVIEWED_CONTROL_MAX_ARGS: usize = 32;
pub const RESOURCE_PAYLOAD_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const RESOURCE_PAYLOAD_HASH_DOMAIN: &[u8] = b"webcodex.agent_execution.resources.v1\0";
pub const REVIEWED_CONTROLS_HASH_DOMAIN: &[u8] = b"webcodex.agent_cli.reviewed_controls.v1\0";
pub const REVIEWED_MANIFEST_HASH_DOMAIN: &[u8] = b"webcodex.agent_execution.reviewed_manifest.v1\0";
pub const CODEBUDDY_FILES_LOADING: &str = "codebuddy-2.162.0-files-v1";
pub const CODEBUDDY_FILES_LOADING_V2: &str = "codebuddy-2.162.0-files-v2";
pub const CODEBUDDY_FILES_VERSION: &str = "2.162.0";
pub const CODEBUDDY_FILES_EXE_SHA256: &str =
    "5d06f03f55fe07aa2748a1b02d6898fca5932c52cd85db05487765851282f94a";
pub const CODEBUDDY_NODE_FILES_LOADING: &str = "codebuddy-2.175.0-node-files-v1";
pub const CODEBUDDY_NODE_FILES_VERSION: &str = "2.175.0";
pub const CODEBUDDY_NODE_EXE_SHA256: &str =
    "ae1a50511be58e987483fdbc12125407443926d2d394669ade2352776e920dd3";
pub const CODEBUDDY_FILES_TOOLS: &str = "Read,Glob,Grep,Edit,Write";
pub const CODEBUDDY_FILES_MCP_PATH: &str = "input/empty-mcp.json";
pub const CODEBUDDY_FILES_SETTINGS_PATH: &str = "input/worker-settings.json";
pub const CODEBUDDY_FILES_MCP_BYTES: &[u8] = b"{\"mcpServers\":{}}\n";
pub const CODEBUDDY_FILES_SETTINGS_BYTES: &[u8] =
    b"{\"disableWorkflows\":true,\"disableAllHooks\":true,\"permissions\":{\"deny\":[\"Read(~/AppData/Local/CodeBuddyExtension/Data/Public/auth/**)\",\"Edit(~/AppData/Local/CodeBuddyExtension/Data/Public/auth/**)\"]}}\n";
pub const CODEBUDDY_NATIVE_AUTH_RELATIVE: &str =
    "AppData/Local/CodeBuddyExtension/Data/Public/auth/Tencent-Cloud.coding-copilot.info";
pub const ANTIGRAVITY_FILES_LOADING: &str = "antigravity-1.4.7-files-v1";
pub const ANTIGRAVITY_FILES_VERSION: &str = "1.4.7";
pub const ANTIGRAVITY_FILES_EXE_SHA256: &str =
    "22588b6d5a6f6d2d66091c73dfb9255edc7bdafe9aff9dd20d958ce116496695";
// Read-only 1.4.8 discovery loaded the same fixed agent template in a fresh,
// credential-free home. Mapping recognition is not account/model verification.
pub const ANTIGRAVITY_148_FILES_LOADING: &str = "antigravity-1.4.8-files-v1";
pub const ANTIGRAVITY_148_FILES_VERSION: &str = "1.4.8";
pub const ANTIGRAVITY_148_FILES_EXE_SHA256: &str =
    "074c7b99b7727dcd2c7437973454d0856181a1006c97a4fad91a1ed23c10ad44";
pub const ANTIGRAVITY_NATIVE_AUTH_RELATIVE: &str =
    ".gemini/antigravity-cli/antigravity-oauth-token";
pub const ANTIGRAVITY_SETTINGS_PATH: &str = "input/antigravity-settings.json";
pub const ANTIGRAVITY_AGENT_PATH: &str = "input/antigravity-agent.md";
// These reviewed templates are materialized only by the compiled loader. The
// two substitutions are execution-root paths, never business prompt content.
pub const ANTIGRAVITY_SETTINGS_BYTES: &[u8] = br#"{"agentMode":"accept-edits","permissions":{"deny":["read_file(@WEBCODEX_HOME@)","command(*)","unsandboxed(*)","read_url(*)","execute_url(*)","mcp(*)","write_file(.agents)","write_file(.gemini)"]}}"#;
pub const ANTIGRAVITY_AGENT_BYTES: &[u8] = b"---\nname: webcodex-file-worker\ndescription: Isolated WebCodex file worker\nexcludeDefaultComponents: true\ninheritMcp: false\ntools:\n  - view_file\n  - list_dir\n  - find_by_name\n  - grep_search\n  - write_to_file\n  - replace_file_content\n  - multi_replace_file_content\nmainAgent: true\nsubagent: false\nmodel: inherit\ncommandExecutionPolicy: off\nmcpServers: []\nskills: []\nplugins: []\n---\n# Instructions\nComplete the supplied task using the available file tools. The workspace absolute path is @WEBCODEX_WORKSPACE@. File tools require absolute paths. Follow the supplied delivery contract and return a final response when finished.\n";

pub fn antigravity_file_worker_control_files() -> Vec<ReviewedControlFile> {
    [
        (ANTIGRAVITY_AGENT_PATH, ANTIGRAVITY_AGENT_BYTES),
        (ANTIGRAVITY_SETTINGS_PATH, ANTIGRAVITY_SETTINGS_BYTES),
    ]
    .into_iter()
    .map(|(path, bytes)| ReviewedControlFile {
        path: path.into(),
        sha256: crate::agent_cli::sha256(bytes),
    })
    .collect()
}

pub fn antigravity_file_worker_args() -> Vec<ReviewedControlArg> {
    [
        ("--agent", Some("webcodex-file-worker")),
        ("--mode", Some("accept-edits")),
        ("--disable-slash-commands", None),
    ]
    .into_iter()
    .map(|(flag, value)| ReviewedControlArg {
        flag: flag.into(),
        value: value.map(str::to_owned),
    })
    .collect()
}

/// Fixed operator-only diagnostic argv, not a production Agent start request.
pub fn codebuddy_file_probe_args() -> Vec<String> {
    let mut args = [
        "--model",
        "hy3",
        "--effort",
        "minimal",
        "--print",
        "--input-format",
        "text",
        "--output-format",
        "stream-json",
        "--verbose",
    ]
    .map(str::to_owned)
    .to_vec();
    for arg in codebuddy_file_worker_args() {
        args.push(arg.flag);
        if let Some(value) = arg.value {
            args.push(value);
        }
    }
    args
}

/// Exact candidate bytes/argv, not a constructor for an approved record.
pub fn codebuddy_file_worker_control_files() -> Vec<ReviewedControlFile> {
    [
        (CODEBUDDY_FILES_MCP_PATH, CODEBUDDY_FILES_MCP_BYTES),
        (
            CODEBUDDY_FILES_SETTINGS_PATH,
            CODEBUDDY_FILES_SETTINGS_BYTES,
        ),
    ]
    .into_iter()
    .map(|(path, bytes)| ReviewedControlFile {
        path: path.into(),
        sha256: crate::agent_cli::sha256(bytes),
    })
    .collect()
}

pub fn codebuddy_file_worker_args() -> Vec<ReviewedControlArg> {
    [
        ("--tools", Some(CODEBUDDY_FILES_TOOLS)),
        ("--disallowedTools", Some("Agent")),
        ("--strict-mcp-config", None),
        ("--mcp-config", Some(CODEBUDDY_FILES_MCP_PATH)),
        ("--settings", Some(CODEBUDDY_FILES_SETTINGS_PATH)),
        ("--setting-sources", Some("")),
        ("--permission-mode", Some("acceptEdits")),
        ("--no-session-persistence", None),
    ]
    .into_iter()
    .map(|(flag, value)| ReviewedControlArg {
        flag: flag.into(),
        value: value.map(str::to_owned),
    })
    .collect()
}

/// The installed 2.162.0 parser treats an empty settingSources as absent.
/// Its explicit `none` selector disables external settings sources. Keep v1
/// records readable without silently changing their approved argv or hashes.
pub fn codebuddy_file_worker_args_v2() -> Vec<ReviewedControlArg> {
    let mut args = codebuddy_file_worker_args();
    args.iter_mut()
        .find(|arg| arg.flag == "--setting-sources")
        .expect("compiled settings selector")
        .value = Some("none".into());
    args
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedControlFile {
    /// Execution-root-relative, read-only input file. Not a host source path.
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedControlArg {
    /// One canonical long option, never an argv string or a command fragment.
    pub flag: String,
    pub value: Option<String>,
}

/// Immutable subject data from a separately trusted publication path. R and F
/// are deliberately absent: R hashes this subject; F wraps the true P and R.
/// Evidence/record IDs are correlation data, not an issuer or approval token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedAgentCliControls {
    pub version: u32,
    pub record_id: String,
    pub provider: AgentCliProvider,
    pub cli_version: String,
    pub executable_sha256: String,
    pub resource_set_id: String,
    /// P must be recomputed from actual pinned Runner payload, not accepted
    /// merely because this field is a well-formed SHA256.
    pub payload_sha256: String,
    pub control_files: Vec<ReviewedControlFile>,
    /// Bounded identifier only. It is never resolved as a plugin or command.
    /// Only an exact compiled loading contract may be selected.
    pub loading_method: String,
    pub fixed_args: Vec<ReviewedControlArg>,
    pub verification_evidence_sha256: String,
    pub independent_review_sha256: String,
    /// Administrator-reviewed protocol for an extensible native CLI. It is
    /// covered by the same review/resource digest and is never a model input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<crate::cli_adapter::CliAdapter>,
}

fn digest(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("agent_cli_controls_invalid_sha256".into());
    }
    Ok(())
}
fn text(value: &str, max: usize) -> Result<(), String> {
    if value.is_empty()
        || value != value.trim()
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err("agent_cli_controls_invalid_text".into());
    }
    Ok(())
}
fn identifier(value: &str, max: usize) -> Result<(), String> {
    text(value, max)?;
    if matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err("agent_cli_controls_invalid_identifier".into());
    }
    Ok(())
}
fn control_path(value: &str) -> Result<(), String> {
    if value.len() > 256 || !value.starts_with("input/") || value.contains('\\') {
        return Err("agent_cli_controls_invalid_file".into());
    }
    let parts: Vec<_> = value.split('/').collect();
    if !(2..=16).contains(&parts.len()) {
        return Err("agent_cli_controls_invalid_file".into());
    }
    for part in parts {
        identifier(part, 128)?;
        if part.ends_with(['.', ' ']) {
            return Err("agent_cli_controls_invalid_file".into());
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err("agent_cli_controls_invalid_file".into());
        }
    }
    Ok(())
}
fn hash(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Shared D4-compatible P algorithm. Runner supplies its existing canonical
/// payload tuple after validating/pinning real sources; this function is pure
/// and cannot attest either the source bytes or canonicalization by itself.
pub fn resource_payload_sha256(canonical_payload: &[u8]) -> Result<String, String> {
    if canonical_payload.is_empty() || canonical_payload.len() > RESOURCE_PAYLOAD_MAX_BYTES {
        return Err("agent_cli_controls_payload_limit".into());
    }
    Ok(hash(RESOURCE_PAYLOAD_HASH_DOMAIN, canonical_payload))
}

/// F = H(domain, typed JSON tuple (1, P, R)); neither P nor R contains F.
pub fn final_reviewed_manifest_sha256(
    payload_sha256: &str,
    review_sha256: &str,
) -> Result<String, String> {
    digest(payload_sha256)?;
    digest(review_sha256)?;
    let bytes = serde_json::to_vec(&(1u32, payload_sha256, review_sha256))
        .map_err(|_| "agent_cli_controls_encoding_failed")?;
    Ok(hash(REVIEWED_MANIFEST_HASH_DOMAIN, &bytes))
}

const RESERVED_FLAGS: &[&str] = &[
    "--model",
    "--effort",
    "--print",
    "--input-format",
    "--output-format",
    "--help",
    "--version",
    "--prompt",
    "--resume",
    "--continue",
    "--dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "--dangerously-bypass-hook-trust",
    "--no-sandbox",
    "--yolo",
    "--allow-all-tools",
    "--allowedtools",
];

impl ReviewedAgentCliControls {
    pub fn validate_shape(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("agent_cli_controls_unsupported_version".into());
        }
        if let Some(adapter) = &self.adapter {
            adapter.validate()?;
            if adapter.id != self.provider.id() || adapter.invocation.is_none() {
                return Err("agent_cli_controls_adapter_mismatch".into());
            }
        }
        identifier(&self.record_id, 128)?;
        text(&self.cli_version, 128)?;
        identifier(&self.resource_set_id, 512)?;
        identifier(&self.loading_method, 96)?;
        for value in [
            &self.executable_sha256,
            &self.payload_sha256,
            &self.verification_evidence_sha256,
            &self.independent_review_sha256,
        ] {
            digest(value)?;
        }
        if self.control_files.len() > REVIEWED_CONTROL_MAX_FILES
            || self.fixed_args.is_empty()
            || self.fixed_args.len() > REVIEWED_CONTROL_MAX_ARGS
        {
            return Err("agent_cli_controls_limit".into());
        }
        let mut files = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for file in &self.control_files {
            control_path(&file.path)?;
            digest(&file.sha256)?;
            if previous.is_some_and(|old| old >= file.path.as_str())
                || !files.insert(file.path.to_ascii_lowercase())
            {
                return Err("agent_cli_controls_files_not_unique_sorted".into());
            }
            previous = Some(&file.path);
        }
        let mut flags = BTreeSet::new();
        for arg in &self.fixed_args {
            let name = arg
                .flag
                .strip_prefix("--")
                .ok_or("agent_cli_controls_invalid_flag")?;
            if name.is_empty()
                || name.len() > 64
                || !name.as_bytes()[0].is_ascii_alphabetic()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err("agent_cli_controls_invalid_flag".into());
            }
            let flag_key = arg.flag.to_ascii_lowercase();
            let overrides_selection = self
                .adapter
                .as_ref()
                .and_then(|adapter| adapter.invocation.as_ref())
                .is_some_and(|invocation| {
                    arg.flag == invocation.model_flag
                        || invocation.effort_flag.as_ref() == Some(&arg.flag)
                });
            if RESERVED_FLAGS.contains(&flag_key.as_str())
                || overrides_selection
                || !flags.insert(flag_key)
            {
                return Err("agent_cli_controls_argument_override".into());
            }
            if let Some(value) = &arg.value {
                // Explicit empty is a real argv token (e.g. disable-all tools),
                // not the absence of a value. Never trim or normalize it away.
                if value.is_empty() {
                    continue;
                }
                text(value, 512)?;
                let fixed_file_tools = self.provider == AgentCliProvider::WorkBuddy
                    && matches!(
                        self.loading_method.as_str(),
                        CODEBUDDY_FILES_LOADING
                            | CODEBUDDY_FILES_LOADING_V2
                            | CODEBUDDY_NODE_FILES_LOADING
                    )
                    && arg.flag == "--tools"
                    && value == CODEBUDDY_FILES_TOOLS;
                if value.starts_with(['-', '@', '/'])
                    || (!fixed_file_tools
                        && !value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte)))
                {
                    return Err("agent_cli_controls_invalid_argument_value".into());
                }
                if value.contains('/') {
                    control_path(value)?;
                    if !self.control_files.iter().any(|file| &file.path == value) {
                        return Err("agent_cli_controls_unbound_file_argument".into());
                    }
                } else if matches!(value.as_str(), "." | "..") {
                    return Err("agent_cli_controls_invalid_argument_value".into());
                }
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| "agent_cli_controls_encoding_failed")?
            .len()
            > REVIEWED_CONTROLS_MAX_BYTES
        {
            return Err("agent_cli_controls_limit".into());
        }
        Ok(())
    }

    pub fn review_sha256(&self) -> Result<String, String> {
        self.validate_shape()?;
        let bytes = serde_json::to_vec(self).map_err(|_| "agent_cli_controls_encoding_failed")?;
        Ok(hash(REVIEWED_CONTROLS_HASH_DOMAIN, &bytes))
    }
    pub fn execution_manifest_sha256(&self) -> Result<String, String> {
        final_reviewed_manifest_sha256(&self.payload_sha256, &self.review_sha256()?)
    }
    /// Pure consistency, not support or trusted-publication verification.
    pub fn validate_resource(
        &self,
        resource_set_id: &str,
        provider: Option<AgentCliProvider>,
        executable_identity: &str,
        manifest_hash: &str,
    ) -> Result<(), String> {
        self.validate_shape()?;
        if self.resource_set_id != resource_set_id
            || provider != Some(self.provider)
            || executable_identity.strip_prefix("sha256:") != Some(self.executable_sha256.as_str())
            || manifest_hash != self.execution_manifest_sha256()?
        {
            return Err("agent_cli_controls_resource_mismatch".into());
        }
        Ok(())
    }
    /// Recognize the exact compiled file-worker mapping, not real enforcement
    /// evidence. Callers also verify the existing resource/P-R-F binding and
    /// obtain the record through the trusted configuration boundary.
    pub fn require_supported_loading(&self) -> Result<(), String> {
        self.validate_shape()?;
        if self.loading_method == "registered-native-cli-v1" {
            return self
                .adapter
                .as_ref()
                .map(|_| ())
                .ok_or("agent_cli_controls_adapter_required".into());
        }
        if self.adapter.is_some() {
            return Err("agent_cli_controls_legacy_adapter_override".into());
        }
        if self.provider == AgentCliProvider::Antigravity {
            let recognized = [
                (
                    ANTIGRAVITY_FILES_VERSION,
                    ANTIGRAVITY_FILES_EXE_SHA256,
                    ANTIGRAVITY_FILES_LOADING,
                ),
                (
                    ANTIGRAVITY_148_FILES_VERSION,
                    ANTIGRAVITY_148_FILES_EXE_SHA256,
                    ANTIGRAVITY_148_FILES_LOADING,
                ),
            ]
            .iter()
            .any(|(version, binary, loader)| {
                self.cli_version == *version
                    && self.executable_sha256 == *binary
                    && self.loading_method == *loader
            });
            if !recognized {
                return Err("agent_cli_controls_mechanism_unverified".into());
            }
            if self.fixed_args != antigravity_file_worker_args()
                || self.control_files != antigravity_file_worker_control_files()
            {
                return Err("agent_cli_controls_fixed_policy_mismatch".into());
            }
            return Ok(());
        }
        let native = self.cli_version == CODEBUDDY_FILES_VERSION
            && self.executable_sha256 == CODEBUDDY_FILES_EXE_SHA256
            && matches!(
                self.loading_method.as_str(),
                CODEBUDDY_FILES_LOADING | CODEBUDDY_FILES_LOADING_V2
            );
        let node = self.cli_version == CODEBUDDY_NODE_FILES_VERSION
            && self.executable_sha256 == CODEBUDDY_NODE_EXE_SHA256
            && self.loading_method == CODEBUDDY_NODE_FILES_LOADING;
        if self.provider != AgentCliProvider::WorkBuddy || !(native || node) {
            return Err("agent_cli_controls_mechanism_unverified".into());
        }
        let expected_args = if self.loading_method == CODEBUDDY_FILES_LOADING_V2 {
            codebuddy_file_worker_args_v2()
        } else {
            codebuddy_file_worker_args()
        };
        if self.fixed_args != expected_args
            || self.control_files != codebuddy_file_worker_control_files()
        {
            return Err("agent_cli_controls_fixed_policy_mismatch".into());
        }
        Ok(())
    }
}

fn selected_adapter(
    record: &ReviewedAgentCliControls,
) -> Result<crate::cli_adapter::CliAdapter, String> {
    if let Some(adapter) = &record.adapter {
        return Ok(adapter.clone());
    }
    crate::cli_adapter::cli_adapters(&[])?
        .into_iter()
        .find(|adapter| adapter.id == record.provider.id())
        .ok_or("agent_cli_controls_adapter_required".into())
}
fn base_len(record: &ReviewedAgentCliControls, args: &[String]) -> Result<usize, String> {
    let adapter = selected_adapter(record)?;
    let invocation = adapter
        .invocation
        .as_ref()
        .ok_or("agent_cli_controls_adapter_required")?;
    let model_count = if args.get(invocation.prefix_args.len()) == Some(&invocation.model_flag) {
        2
    } else {
        0
    };
    let effort_index = invocation.prefix_args.len() + model_count;
    let explicit_effort = invocation
        .effort_flag
        .as_ref()
        .is_some_and(|flag| args.get(effort_index) == Some(flag));
    Ok(invocation.prefix_args.len()
        + model_count
        + if explicit_effort { 2 } else { 0 }
        + invocation.fixed_args.len())
}
fn validate_base(record: &ReviewedAgentCliControls, args: &[String]) -> Result<(), String> {
    let adapter = selected_adapter(record)?;
    let invocation = adapter
        .invocation
        .as_ref()
        .ok_or("agent_cli_controls_adapter_required")?;
    let index = invocation.prefix_args.len();
    if args.len() != base_len(record, args)? {
        return Err("agent_cli_controls_invalid_business_argv".into());
    }
    let explicit_model = args.get(index) == Some(&invocation.model_flag);
    let model = if explicit_model {
        Some(
            args.get(index + 1)
                .ok_or("agent_cli_controls_invalid_business_argv")?
                .as_str(),
        )
    } else {
        None
    };
    let effort_index = index + if explicit_model { 2 } else { 0 };
    let explicit_effort = invocation
        .effort_flag
        .as_ref()
        .is_some_and(|flag| args.get(effort_index) == Some(flag));
    let effort = if explicit_effort {
        let raw = args
            .get(effort_index + 1)
            .ok_or("agent_cli_controls_invalid_business_argv")?;
        Some(if let Some(key) = &invocation.effort_config_key {
            let encoded = raw
                .strip_prefix(&format!("{key}="))
                .ok_or("agent_cli_controls_invalid_business_argv")?;
            serde_json::from_str::<String>(encoded)
                .map_err(|_| "agent_cli_controls_invalid_business_argv")?
        } else {
            raw.clone()
        })
    } else {
        None
    };
    for value in model.into_iter().chain(effort.as_deref()) {
        if value.starts_with(['-', '@']) {
            return Err("agent_cli_controls_invalid_business_value".into());
        }
    }
    let (expected, _) = adapter
        .invocation_selected(model, effort.as_deref(), "validate")
        .map_err(|_| "agent_cli_controls_invalid_business_argv")?;
    if expected != args {
        return Err("agent_cli_controls_invalid_business_argv".into());
    }
    Ok(())
}

/// The private shape composition is tested with synthetic records only. Public
/// callers must pass the independent support gate below; no generic loader is
/// executed or selected by this concatenation.
fn compose_shape(
    record: &ReviewedAgentCliControls,
    base: &[String],
) -> Result<Vec<String>, String> {
    record.validate_shape()?;
    validate_base(record, base)?;
    let mut args = base.to_vec();
    for arg in &record.fixed_args {
        args.push(arg.flag.clone());
        if let Some(value) = &arg.value {
            args.push(value.clone());
        }
    }
    validate_process_argv(&ShellProcessArgv {
        executable: format!("sha256:{}", record.executable_sha256),
        args: args.clone(),
    })?;
    Ok(args)
}

/// C replaces args.extend(control_args) with this closed shared composition.
/// This validates a compiled mapping, not evidence provenance or real effects.
pub fn compose_reviewed_cli_argv(
    record: &ReviewedAgentCliControls,
    fixed_business_argv: &[String],
) -> Result<Vec<String>, String> {
    let args = compose_shape(record, fixed_business_argv)?;
    record.require_supported_loading()?;
    Ok(args)
}

/// Registry/Runner recheck the complete argv, including order and absence of
/// appended overrides. Unknown loading contracts remain rejected.
pub fn validate_reviewed_cli_argv(
    record: &ReviewedAgentCliControls,
    actual_argv: &[String],
) -> Result<(), String> {
    let count = base_len(record, actual_argv)?;
    if actual_argv.len() < count || compose_shape(record, &actual_argv[..count])? != actual_argv {
        return Err("agent_cli_controls_argument_override".into());
    }
    record.require_supported_loading()
}

#[cfg(test)]
#[path = "agent_cli_controls_tests.rs"]
mod tests;
