//! Runner-local resources for the existing LPAC Job carrier. A profile is an
//! explicit administrator allowlist, not Store authorization or backend readiness.
//! No inherited environment, recursive source directory grant, or source ACL edit.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use webcodex_core::agent_cli::{AgentCliDeliveryBundle, AgentCliProvider};
use webcodex_core::agent_cli_controls::{
    final_reviewed_manifest_sha256, resource_payload_sha256, validate_reviewed_cli_argv,
    ReviewedAgentCliControls,
};
use webcodex_core::runner_protocol::{
    AgentExecutionBinding, AgentExecutionResourceDescriptor, AgentExecutionStart,
};

#[path = "host_cli.rs"]
pub(crate) mod host_cli;
use host_cli::{NativeCliConfig, NativeCliWorker};
const MAX_PROFILES: usize = 32;
const MAX_FILES: usize = 512;
const MAX_RESOURCE_BYTES: u64 = 1024 * 1024 * 1024;
const PROGRAM: &str = "bin/program.exe";
const WRITABLE_DIRS: &[&str] = &["workspace", "home", "state", "cache", "temp", "session"];
// Sorted JSON pairs (execution-relative path, SHA-256), covering the complete
// 2.175.0 runtime package including all four regional product overlays. Source
// paths remain separately bound by the ordinary reviewed payload manifest.
const CODEBUDDY_NODE_PACKAGE_SHA256: &str =
    "b73fa4b0ff76193035e67efe06b80176688424c9d8a77d4ce5a20aa42837d478";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentExecutionResourcesConfig {
    #[serde(default)]
    pub(crate) native_cli: Option<NativeCliConfig>,
    #[serde(default)]
    pub(crate) browser: Option<LocalBrowserConfig>,
    #[serde(default)]
    pub(crate) profiles: Vec<AgentExecutionResourceProfile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalBrowserConfig {
    pub(crate) python: PathBuf,
    pub(crate) python_sha256: String,
    pub(crate) browser: PathBuf,
    pub(crate) browser_sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentExecutionResourceProfile {
    #[serde(default)]
    pub(crate) host_cli: Option<webcodex_core::host_cli::HostCliRegistration>,
    #[serde(default)]
    pub(crate) native_worker: Option<NativeCliWorker>,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) provider: Option<AgentCliProvider>,
    /// Candidate subject supplied only through the existing trusted startup
    /// configuration. This is not a publication/approval API or a host path.
    #[serde(default)]
    pub(crate) reviewed_controls: Option<ReviewedAgentCliControls>,
    #[serde(default)]
    pub(crate) user_https_proxy: bool,
    /// Operator-selected metadata configuration for the reviewed Node package.
    /// Bound into the resource review; never selected by a model or inherited env.
    #[serde(default)]
    pub(crate) codebuddy_config_directory: Option<PathBuf>,
    pub(crate) executable: PathBuf,
    pub(crate) executable_sha256: String,
    #[serde(default)]
    pub(crate) files: Vec<AgentExecutionResourceFile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentExecutionResourceFile {
    pub(crate) source: PathBuf,
    pub(crate) destination: PathBuf,
    pub(crate) sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct ResourceFile {
    source: String,
    destination: String,
    sha256: String,
}

#[derive(Debug, Clone)]
struct ValidatedProfile {
    host_cli: Option<webcodex_core::host_cli::HostCliRegistration>,
    native_worker: Option<NativeCliWorker>,
    provider: Option<AgentCliProvider>,
    reviewed_controls: Option<ReviewedAgentCliControls>,
    user_https_proxy: bool,
    codebuddy_config_directory: Option<String>,
    executable_identity: String,
    files: Vec<ResourceFile>,
    manifest: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ValidatedResourceProfiles(BTreeMap<String, ValidatedProfile>);

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && !value.ends_with(['.', ' '])
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !matches!(
            value
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase()
                .as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}

fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn absolute_path(path: &Path) -> Result<String, String> {
    let text = path.to_str().ok_or("agent_resources_path_encoding")?;
    if !path.is_absolute() || text.len() > 4096 || text.chars().any(char::is_control) {
        return Err("agent_resources_absolute_path_required".into());
    }
    #[cfg(windows)]
    {
        let bytes = text.as_bytes();
        // Only a local drive path. No UNC, device namespace, alternate stream,
        // Win32 trailing-dot normalization, or drive-relative spelling.
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'/' | b'\\')
            || text[2..].contains(':')
            || text[3..].split(['/', '\\']).any(|p| {
                p.is_empty()
                    || p == "."
                    || p == ".."
                    || p.ends_with(['.', ' '])
                    || p.contains(['<', '>', '"', '|', '?', '*'])
            })
        {
            return Err("agent_resources_unsafe_absolute_path".into());
        }
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("agent_resources_unsafe_absolute_path".into());
    }
    Ok(text.replace('\\', "/"))
}

fn destination(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or("agent_resources_path_encoding")?
        .replace('\\', "/");
    let parts: Vec<_> = text.split('/').collect();
    if !(2..=16).contains(&parts.len())
        || !matches!(parts[0], "bin" | "input")
        || parts.iter().any(|part| !safe_component(part))
    {
        return Err("agent_resources_unsafe_destination".into());
    }
    Ok(text)
}

fn payload_manifest(
    id: &str,
    provider: Option<AgentCliProvider>,
    files: &[ResourceFile],
    user_https_proxy: bool,
    codebuddy_config_directory: Option<&str>,
) -> Result<String, String> {
    // Default-off retains the exact D4 payload. Opt-in is a new reviewed
    // subject, so an existing review cannot authorize a flipped proxy flag.
    let encoded = match (provider, user_https_proxy) {
        (None, false) => serde_json::to_vec(&(1u32, id, files)),
        (Some(provider), false) => serde_json::to_vec(&(1u32, id, files, provider)),
        (_, true) => {
            serde_json::to_vec(&(1u32, id, files, provider, "codebuddy-user-https-proxy-v1"))
        }
    }
    .map_err(|_| "agent_resources_manifest_encoding")?;
    let payload = resource_payload_sha256(&encoded)?;
    let Some(directory) = codebuddy_config_directory else {
        return Ok(payload);
    };
    let encoded = serde_json::to_vec(&("codebuddy-metadata-config-v1", payload, directory))
        .map_err(|_| "agent_resources_manifest_encoding")?;
    resource_payload_sha256(&encoded)
}

fn codebuddy_config_directory(
    provider: Option<AgentCliProvider>,
    controls: Option<&ReviewedAgentCliControls>,
    directory: Option<&Path>,
) -> Result<Option<String>, String> {
    let node = provider == Some(AgentCliProvider::WorkBuddy)
        && controls.is_some_and(|control| {
            control.loading_method
                == webcodex_core::agent_cli_controls::CODEBUDDY_NODE_FILES_LOADING
        });
    match (node, directory) {
        (true, Some(directory)) => absolute_path(directory).map(Some),
        (true, None) => Err("agent_resources_codebuddy_config_required".into()),
        (false, Some(_)) => Err("agent_resources_codebuddy_config_mapping_mismatch".into()),
        (false, None) => Ok(None),
    }
}

fn reviewed_file_worker(
    provider: Option<AgentCliProvider>,
    executable_identity: &str,
    controls: Option<&ReviewedAgentCliControls>,
) -> bool {
    controls.is_some_and(|review| {
        provider == Some(review.provider)
            && executable_identity == format!("sha256:{}", review.executable_sha256)
            && review.require_supported_loading().is_ok()
    })
}

fn legacy_credential_loading(
    controls: Option<&ReviewedAgentCliControls>,
) -> Option<AgentCliProvider> {
    use webcodex_core::agent_cli_controls::*;
    let controls = controls?;
    controls.require_supported_loading().ok()?;
    match controls.loading_method.as_str() {
        CODEBUDDY_FILES_LOADING | CODEBUDDY_FILES_LOADING_V2 | CODEBUDDY_NODE_FILES_LOADING => {
            Some(AgentCliProvider::WorkBuddy)
        }
        ANTIGRAVITY_FILES_LOADING | ANTIGRAVITY_148_FILES_LOADING => {
            Some(AgentCliProvider::Antigravity)
        }
        _ => None,
    }
}

fn validate_codebuddy_node_package(files: &[ResourceFile]) -> Result<(), String> {
    use webcodex_core::agent_cli_controls::{
        CODEBUDDY_FILES_MCP_PATH, CODEBUDDY_FILES_SETTINGS_PATH,
    };
    let mut package: Vec<_> = files
        .iter()
        .filter(|file| {
            !matches!(
                file.destination.as_str(),
                PROGRAM | CODEBUDDY_FILES_MCP_PATH | CODEBUDDY_FILES_SETTINGS_PATH
            )
        })
        .map(|file| (file.destination.as_str(), file.sha256.as_str()))
        .collect();
    package.sort_unstable();
    let bytes = serde_json::to_vec(&package).map_err(|_| "agent_resources_package_encoding")?;
    if format!("{:x}", Sha256::digest(bytes)) != CODEBUDDY_NODE_PACKAGE_SHA256 {
        return Err("agent_resources_codebuddy_node_package_mismatch".into());
    }
    Ok(())
}

fn execution_manifest(
    id: &str,
    provider: Option<AgentCliProvider>,
    executable_identity: &str,
    files: &[ResourceFile],
    controls: Option<&ReviewedAgentCliControls>,
    user_https_proxy: bool,
    codebuddy_config_directory: Option<&str>,
) -> Result<String, String> {
    if user_https_proxy && !reviewed_file_worker(provider, executable_identity, controls) {
        return Err("agent_resources_proxy_requires_reviewed_file_worker".into());
    }
    let payload = payload_manifest(
        id,
        provider,
        files,
        user_https_proxy,
        codebuddy_config_directory,
    )?;
    let Some(controls) = controls else {
        return Ok(payload);
    };
    if controls.loading_method == webcodex_core::agent_cli_controls::CODEBUDDY_NODE_FILES_LOADING {
        validate_codebuddy_node_package(files)?;
    }
    if matches!(
        controls.loading_method.as_str(),
        webcodex_core::agent_cli_controls::ANTIGRAVITY_FILES_LOADING
            | webcodex_core::agent_cli_controls::ANTIGRAVITY_148_FILES_LOADING
    ) && (files.len() != 3
        || files.iter().any(|file| {
            !matches!(
                file.destination.as_str(),
                PROGRAM
                    | webcodex_core::agent_cli_controls::ANTIGRAVITY_AGENT_PATH
                    | webcodex_core::agent_cli_controls::ANTIGRAVITY_SETTINGS_PATH
            )
        }))
    {
        return Err("agent_resources_antigravity_package_mismatch".into());
    }
    controls.validate_shape()?;
    if controls.payload_sha256 != payload {
        return Err("agent_resources_control_payload_mismatch".into());
    }
    for control in &controls.control_files {
        if !files
            .iter()
            .any(|file| file.destination == control.path && file.sha256 == control.sha256)
        {
            return Err("agent_resources_control_file_mismatch".into());
        }
    }
    let manifest = final_reviewed_manifest_sha256(&payload, &controls.review_sha256()?)?;
    controls.validate_resource(id, provider, executable_identity, &manifest)?;
    Ok(manifest)
}

fn validate_profile_manifest(id: &str, profile: &ValidatedProfile) -> Result<(), String> {
    if let Some(host) = &profile.host_cli {
        host.validate()?;
        if host.manifest(id)? != profile.manifest
            || profile.reviewed_controls.is_some()
            || profile.executable_identity != format!("sha256:{}", host.executable_sha256)
            || profile.provider.as_ref().map(|p| p.id()) != Some(host.adapter.id.as_str())
        {
            return Err("native_cli_profile_binding_mismatch".into());
        }
        return Ok(());
    }
    if execution_manifest(
        id,
        profile.provider,
        &profile.executable_identity,
        &profile.files,
        profile.reviewed_controls.as_ref(),
        profile.user_https_proxy,
        profile.codebuddy_config_directory.as_deref(),
    )? != profile.manifest
    {
        return Err("agent_resources_manifest_mismatch".into());
    }
    Ok(())
}

fn validate_execution_args(
    profile: &ValidatedProfile,
    start: &AgentExecutionStart,
) -> Result<(), String> {
    if let Some(host) = &profile.host_cli {
        return host.validate_args(&start.process.args);
    }
    if profile.provider.is_some() {
        let controls = profile
            .reviewed_controls
            .as_ref()
            .ok_or("agent_resources_control_review_required")?;
        // Exact shared argv grammar and the compiled loading contract apply.
        validate_reviewed_cli_argv(controls, &start.process.args)?;
    }
    Ok(())
}

/// Resolve only reviewed control operands after validating the frozen abstract
/// argv. Original launch intent is unchanged, like opaque executable resolution.
fn materialized_execution_args(
    profile: &ValidatedProfile,
    start: &AgentExecutionStart,
    root: &Path,
) -> Result<Vec<String>, String> {
    validate_execution_args(profile, start)?;
    let mut args = start.process.args.clone();
    let Some(controls) = &profile.reviewed_controls else {
        return Ok(args);
    };
    absolute_path(root)?;
    let fixed_len: usize = controls
        .fixed_args
        .iter()
        .map(|a| 1 + usize::from(a.value.is_some()))
        .sum();
    let mut index = args
        .len()
        .checked_sub(fixed_len)
        .ok_or("agent_cli_controls_argument_override")?;
    for arg in &controls.fixed_args {
        index += 1;
        if let Some(value) = &arg.value {
            if matches!(arg.flag.as_str(), "--settings" | "--mcp-config") {
                if !controls
                    .control_files
                    .iter()
                    .any(|file| file.path == *value)
                {
                    return Err("agent_cli_controls_unbound_file_argument".into());
                }
                args[index] = absolute_path(&root.join(value))?;
            }
            index += 1;
        }
    }
    if controls.loading_method == webcodex_core::agent_cli_controls::CODEBUDDY_NODE_FILES_LOADING {
        args.splice(
            0..0,
            [
                "--preserve-symlinks".into(),
                "--preserve-symlinks-main".into(),
                absolute_path(&root.join("bin/codebuddy/bin/codebuddy"))?,
            ],
        );
    }
    Ok(args)
}

fn codebuddy_node_environment(
    root: &Path,
    environment: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    environment.insert(
        "CODEBUDDY_CONFIG_DIR".into(),
        absolute_path(&root.join("state/codebuddy"))?,
    );
    let instance = format!("{:x}", Sha256::digest(root.to_string_lossy().as_bytes()));
    environment.insert(
        "CLIENT_INFO_MACHINE_ID".into(),
        format!(
            "{}-{}-{}-{}-{}",
            &instance[..8],
            &instance[8..12],
            &instance[12..16],
            &instance[16..20],
            &instance[20..32]
        ),
    );
    environment.insert(
        "CODEBUDDY_USER_ID".into(),
        format!("isolated-{}", &instance[..16]),
    );
    environment.insert("CODEBUDDY_TEAM_MEMORY_ENABLED".into(), "false".into());
    environment.insert("CODEBUDDY_DISABLE_SHELL_SNAPSHOT".into(), "1".into());
    Ok(())
}

fn antigravity_control_bytes(root: &Path) -> Result<(Vec<u8>, Vec<u8>), String> {
    use webcodex_core::agent_cli_controls::*;
    let home = absolute_path(&root.join("home"))?;
    let workspace = absolute_path(&root.join("workspace"))?;
    let mut settings: serde_json::Value = serde_json::from_slice(ANTIGRAVITY_SETTINGS_BYTES)
        .map_err(|_| "antigravity_settings_encoding")?;
    settings["permissions"]["deny"][0] = serde_json::Value::String(format!("read_file({home})"));
    let settings = serde_json::to_vec(&settings).map_err(|_| "antigravity_settings_encoding")?;
    let agent = std::str::from_utf8(ANTIGRAVITY_AGENT_BYTES)
        .map_err(|_| "antigravity_agent_encoding")?
        .replace("@WEBCODEX_WORKSPACE@", &workspace)
        .into_bytes();
    Ok((settings, agent))
}

/// Pure, deterministic validation of the trusted startup configuration. Shape
/// and P/R/F consistency are facts, not proof of provider support. The caller
/// must separately require backend readiness and `verify_sources` before
/// advertising resource facts; business starts also require validate_execution.
pub(crate) fn validate_profiles(
    config: &AgentExecutionResourcesConfig,
) -> Result<ValidatedResourceProfiles, String> {
    if config.profiles.len() > MAX_PROFILES {
        return Err("agent_resources_too_many_profiles".into());
    }
    let mut profiles = BTreeMap::new();
    let mut providers = BTreeSet::new();
    for profile in &config.profiles {
        if !safe_component(&profile.id)
            || profile.files.len() >= MAX_FILES
            || !digest_valid(&profile.executable_sha256)
        {
            return Err("agent_resources_invalid_profile".into());
        }
        if profile
            .provider
            .is_some_and(|provider| !providers.insert(provider))
        {
            return Err("agent_resources_duplicate_provider".into());
        }
        let executable = absolute_path(&profile.executable)?;
        if !executable.to_ascii_lowercase().ends_with(".exe") {
            return Err("agent_resources_native_executable_required".into());
        }
        let mut files = vec![ResourceFile {
            source: executable.clone(),
            destination: PROGRAM.into(),
            sha256: profile.executable_sha256.clone(),
        }];
        for file in &profile.files {
            if !digest_valid(&file.sha256) {
                return Err("agent_resources_invalid_digest".into());
            }
            files.push(ResourceFile {
                source: absolute_path(&file.source)?,
                destination: destination(&file.destination)?,
                sha256: file.sha256.clone(),
            });
        }
        files.sort_by(|a, b| a.destination.cmp(&b.destination));
        let mut destinations = BTreeSet::new();
        for file in &files {
            // Always reject case collisions: these resources target Windows.
            let key = file.destination.to_ascii_lowercase();
            if !destinations.insert(key.clone()) {
                return Err("agent_resources_destination_collision".into());
            }
        }
        for key in &destinations {
            if key.split('/').enumerate().skip(1).any(|(i, _)| {
                destinations.contains(&key.split('/').take(i).collect::<Vec<_>>().join("/"))
            }) {
                return Err("agent_resources_destination_collision".into());
            }
        }
        let executable_identity = format!("sha256:{}", profile.executable_sha256);
        let codebuddy_config_directory = codebuddy_config_directory(
            profile.provider,
            profile.reviewed_controls.as_ref(),
            profile.codebuddy_config_directory.as_deref(),
        )?;
        let manifest = if let Some(host) = &profile.host_cli {
            if profile.reviewed_controls.is_some()
                || profile.user_https_proxy
                || profile.codebuddy_config_directory.is_some()
                || !profile.files.is_empty()
                || host.executable_sha256 != profile.executable_sha256
                || profile.provider.as_ref().map(|p| p.id()) != Some(host.adapter.id.as_str())
            {
                return Err("native_cli_profile_binding_mismatch".into());
            }
            host.manifest(&profile.id)?
        } else {
            execution_manifest(
                &profile.id,
                profile.provider,
                &executable_identity,
                &files,
                profile.reviewed_controls.as_ref(),
                profile.user_https_proxy,
                codebuddy_config_directory.as_deref(),
            )?
        };
        if profiles
            .insert(
                profile.id.clone(),
                ValidatedProfile {
                    host_cli: profile.host_cli.clone(),
                    native_worker: profile.native_worker.clone(),
                    provider: profile.provider,
                    reviewed_controls: profile.reviewed_controls.clone(),
                    user_https_proxy: profile.user_https_proxy,
                    codebuddy_config_directory,
                    executable_identity,
                    files,
                    manifest,
                },
            )
            .is_some()
        {
            return Err("agent_resources_duplicate_profile".into());
        }
    }
    Ok(ValidatedResourceProfiles(profiles))
}

impl ValidatedResourceProfiles {
    pub(crate) fn validate_program(
        &self,
        start: &webcodex_core::agent_program::AgentProgramStart,
    ) -> Result<(), String> {
        start.validate()?;
        let profile = self
            .0
            .get(&start.binding.resource_set_id)
            .ok_or("agent_program_resource_unavailable")?;
        if profile.provider.is_some()
            || profile.reviewed_controls.is_some()
            || profile.user_https_proxy
            || profile.codebuddy_config_directory.is_some()
            || profile.manifest != start.binding.resource_manifest_hash
        {
            return Err("agent_program_resource_not_ordinary".into());
        }
        validate_profile_manifest(&start.binding.resource_set_id, profile)
    }
    pub(crate) fn cli_installations(&self) -> Vec<webcodex_core::agent_cli::AgentCliInstallation> {
        self.0
            .values()
            .filter_map(|profile| {
                let provider = profile.provider?;
                let adapter = profile
                    .host_cli
                    .as_ref()
                    .map(|host| host.adapter.clone())
                    .or_else(|| {
                        profile
                            .reviewed_controls
                            .as_ref()
                            .and_then(|record| record.adapter.clone())
                            .or_else(|| {
                                webcodex_core::cli_adapter::cli_adapters(&[])
                                    .ok()?
                                    .into_iter()
                                    .find(|entry| entry.id == provider.id())
                            })
                    });
                let installed = profile
                    .files
                    .iter()
                    .all(|file| Path::new(&file.source).is_file());
                let adapter_available = adapter
                    .as_ref()
                    .is_some_and(|entry| entry.invocation.is_some());
                Some(webcodex_core::agent_cli::AgentCliInstallation {
                    command: profile
                        .files
                        .iter()
                        .find(|file| file.destination == PROGRAM)?
                        .source
                        .clone(),
                    execution_ready: Some(
                        installed
                            && (profile.host_cli.is_some()
                                || profile
                                    .reviewed_controls
                                    .as_ref()
                                    .is_some_and(|c| c.require_supported_loading().is_ok())),
                    ),
                    execution_mode: Some(
                        if profile.host_cli.is_some() {
                            "native_host"
                        } else {
                            "isolated"
                        }
                        .into(),
                    ),
                    reason_code: (!installed).then(|| "cli_resource_file_missing".into()),
                    provider_id: provider.id().into(),
                    label: adapter
                        .map(|entry| entry.label)
                        .unwrap_or_else(|| provider.id().into()),
                    installed,
                    native: true,
                    adapter_available,
                    status: if installed {
                        "discovered"
                    } else {
                        "unavailable"
                    }
                    .into(),
                    message: if installed {
                        "已注册为任务执行资源，调用前会复核程序与账号可用性。"
                    } else {
                        "已登记的 CLI 文件当前不可用，请重新配置。"
                    }
                    .into(),
                })
            })
            .collect()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn requires_proxy_controller(&self) -> bool {
        false
    }
    pub(crate) fn manifest(&self, resource_set_id: &str) -> Option<&str> {
        self.0.get(resource_set_id).map(|p| p.manifest.as_str())
    }
    /// Path-free configuration facts. Registration must first verify the actual
    /// sources and backend readiness; these facts grant no task authority.
    pub(crate) fn descriptors(&self) -> Vec<AgentExecutionResourceDescriptor> {
        self.0
            .iter()
            .map(|(id, profile)| AgentExecutionResourceDescriptor {
                host_cli: profile.host_cli.clone(),
                project_workspace_copy: cfg!(windows) && profile.host_cli.is_none(),
                resource_set_id: id.clone(),
                manifest_hash: profile.manifest.clone(),
                executable_identity: profile.executable_identity.clone(),
                provider: profile.provider,
                reviewed_controls: profile.reviewed_controls.clone(),
            })
            .collect()
    }
    pub(crate) fn verify_sources(&self) -> Result<(), String> {
        if self.is_empty() {
            return Err("agent_resources_profile_required".into());
        }
        #[cfg(windows)]
        {
            windows::verify_sources(self)
        }
        #[cfg(not(windows))]
        {
            Err("agent_resources_platform_unsupported".into())
        }
    }
    pub(crate) fn validate_execution(&self, start: &AgentExecutionStart) -> Result<(), String> {
        selected_profile(self, start).map(|_| ())
    }
    /// Pure frozen-profile checks; no credential, registry or filesystem I/O.
    pub(crate) fn cleanup_requirements(
        &self,
        start: &AgentExecutionStart,
    ) -> Result<(bool, bool), String> {
        selected_profile(self, start)?;
        Ok((false, false))
    }
}

/// The native metadata probe must keep this value alive through process cleanup.
/// It conveys an administrator-selected binary, not task or credential authority.
pub(crate) struct PinnedCatalogExecutable {
    pub(crate) program: PathBuf,
    pub(crate) entrypoint: Option<PathBuf>,
    pub(crate) codebuddy_config_directory: Option<PathBuf>,
    pub(crate) descriptor: AgentExecutionResourceDescriptor,
    _pins: Vec<std::fs::File>,
}

/// Pin a discovered installation for fixed metadata commands only. This
/// temporary profile is never registered and grants no isolated Job execution.
pub(crate) fn pin_discovered_catalog_executable(
    program: &Path,
    provider: AgentCliProvider,
) -> Result<PinnedCatalogExecutable, String> {
    let (program, source, digest) = open_host_executable(program, None)?;
    let profiles = validate_profiles(&AgentExecutionResourcesConfig {
        native_cli: None,
        browser: None,
        profiles: vec![AgentExecutionResourceProfile {
            host_cli: None,
            native_worker: None,
            id: format!("metadata-{}", provider.id()),
            provider: Some(provider),
            reviewed_controls: None,
            user_https_proxy: false,
            codebuddy_config_directory: None,
            executable: program.clone(),
            executable_sha256: digest,
            files: vec![],
        }],
    })?;
    Ok(PinnedCatalogExecutable {
        program,
        entrypoint: None,
        codebuddy_config_directory: None,
        descriptor: profiles
            .descriptors()
            .into_iter()
            .next()
            .ok_or("agent_cli_metadata_profile_missing")?,
        _pins: vec![source],
    })
}

/// Host installations may be installer symlinks, directory junctions or hard
/// links. Open their actual file normally and verify content, without the old
/// copied-resource requirement that every ancestor/file have a unique link.
fn open_host_executable(
    program: &Path,
    expected: Option<&str>,
) -> Result<(PathBuf, std::fs::File, String), String> {
    use std::io::Read;
    let canonical = program.canonicalize().map_err(|e| {
        format!(
            "native_cli_resolve_failed:{}",
            e.raw_os_error().unwrap_or(0)
        )
    })?;
    let mut source = std::fs::File::open(&canonical)
        .map_err(|e| format!("native_cli_open_failed:{}", e.raw_os_error().unwrap_or(0)))?;
    let metadata = source.metadata().map_err(|e| {
        format!(
            "native_cli_metadata_failed:{}",
            e.raw_os_error().unwrap_or(0)
        )
    })?;
    if !metadata.is_file() || metadata.len() > MAX_RESOURCE_BYTES {
        return Err("native_cli_executable_size_invalid".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0u64;
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|e| format!("native_cli_read_failed:{}", e.raw_os_error().unwrap_or(0)))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_RESOURCE_BYTES {
            return Err("native_cli_executable_size_invalid".into());
        }
        hash.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hash.finalize());
    if expected.is_some_and(|expected| expected != digest) {
        return Err("native_cli_executable_changed".into());
    }
    // Reuse ordinary Win32 path spelling accepted by native argv/CreateProcess.
    let text = canonical.to_string_lossy();
    let program = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        canonical
    };
    Ok((program, source, digest))
}

fn catalog_entrypoint(profile: &ValidatedProfile) -> Result<Option<PathBuf>, String> {
    if profile.reviewed_controls.as_ref().is_some_and(|control| {
        control.loading_method == webcodex_core::agent_cli_controls::CODEBUDDY_NODE_FILES_LOADING
    }) {
        let entrypoint = PathBuf::from(
            &profile
                .files
                .iter()
                .find(|file| file.destination == "bin/codebuddy/bin/codebuddy")
                .ok_or("agent_resources_codebuddy_entrypoint_missing")?
                .source,
        );
        let package = entrypoint
            .parent()
            .and_then(Path::parent)
            .ok_or("agent_resources_codebuddy_package_layout")?;
        // Metadata runs at the installed source path. Its relative imports
        // must resolve to the very files pinned by the reviewed manifest.
        for file in &profile.files {
            if let Some(relative) = file.destination.strip_prefix("bin/codebuddy/") {
                if Path::new(&file.source) != package.join(relative) {
                    return Err("agent_resources_codebuddy_package_layout".into());
                }
            }
        }
        Ok(Some(entrypoint))
    } else {
        Ok(None)
    }
}

impl std::fmt::Debug for PinnedCatalogExecutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedCatalogExecutable")
            .finish_non_exhaustive()
    }
}

pub(crate) fn pin_catalog_executable(
    profiles: &ValidatedResourceProfiles,
    provider: AgentCliProvider,
) -> Result<PinnedCatalogExecutable, String> {
    let (id, profile) = profiles
        .0
        .iter()
        .find(|(_, profile)| profile.provider == Some(provider))
        .ok_or("agent_cli_provider_unconfigured")?;
    #[cfg(windows)]
    {
        windows::pin_catalog_executable(id, profile)
    }
    #[cfg(not(windows))]
    {
        let _ = (id, profile);
        Err("agent_resources_platform_unsupported".into())
    }
}

/// The JobManager must first prove the exact binding has stopped and its close
/// fence/receipt are durable. This function only reads its fixed publish area.
pub(crate) fn read_delivery_bundle(
    binding: &AgentExecutionBinding,
    resource_root: &Path,
) -> Result<AgentCliDeliveryBundle, String> {
    binding.validate()?;
    if !matches!(
        binding.carrier,
        webcodex_core::runner_protocol::AgentExecutionCarrier::Job { .. }
    ) {
        return Err("agent_cli_delivery_job_required".into());
    }
    absolute_path(resource_root)?;
    #[cfg(windows)]
    {
        windows::read_delivery_bundle(binding, resource_root)
    }
    #[cfg(not(windows))]
    {
        Err("agent_resources_platform_unsupported".into())
    }
}

/// The caller must prove process cleanup and release PreparedAgentResources
/// pins first. Only the binding's fixed native-auth copy may be deleted.
pub(crate) fn cleanup_native_auth(
    binding: &AgentExecutionBinding,
    resource_root: &Path,
) -> Result<(), String> {
    binding.validate()?;
    if !matches!(
        binding.carrier,
        webcodex_core::runner_protocol::AgentExecutionCarrier::Job { .. }
    ) {
        return Err("agent_resources_cleanup_job_required".into());
    }
    absolute_path(resource_root)?;
    #[cfg(windows)]
    {
        windows::cleanup_native_auth(binding, resource_root)
    }
    #[cfg(not(windows))]
    {
        Err("agent_resources_platform_unsupported".into())
    }
}

fn selected_profile<'a>(
    profiles: &'a ValidatedResourceProfiles,
    start: &AgentExecutionStart,
) -> Result<&'a ValidatedProfile, String> {
    start.validate()?;
    let profile = profiles
        .0
        .get(&start.resource_set_id)
        .ok_or("agent_resources_profile_required")?;
    validate_profile_manifest(&start.resource_set_id, profile)?;
    if profile.manifest != start.binding.resource_manifest_hash {
        return Err("agent_resources_manifest_mismatch".into());
    }
    if start.process.executable != profile.executable_identity {
        return Err("agent_resources_executable_mismatch".into());
    }
    validate_execution_args(profile, start)?;
    Ok(profile)
}

fn environment(root: &Path, windows_directory: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for (key, path) in [
        ("SystemRoot", windows_directory.to_path_buf()),
        ("WINDIR", windows_directory.to_path_buf()),
        ("HOME", root.join("home")),
        ("USERPROFILE", root.join("home")),
        ("APPDATA", root.join("state")),
        ("LOCALAPPDATA", root.join("state")),
        ("XDG_CONFIG_HOME", root.join("state")),
        ("XDG_STATE_HOME", root.join("state")),
        ("XDG_CACHE_HOME", root.join("cache")),
        ("XDG_DATA_HOME", root.join("state")),
        ("TEMP", root.join("temp")),
        ("TMP", root.join("temp")),
        ("PATH", root.join("bin")),
        ("WEBCODEX_SESSION_HOME", root.join("session")),
    ] {
        let value = path.to_str().ok_or("agent_resources_path_encoding")?;
        if value.contains(['\0', ';']) {
            return Err("agent_resources_invalid_environment_path".into());
        }
        env.insert(key.into(), value.into());
    }
    Ok(env)
}

/// Keep this value alive until complete process-handle evidence, worker finish,
/// and the existing Store/Job close decision authorize resource release.
/// It intentionally does not remove directories on Drop: Job recovery owns them.
pub(crate) struct PreparedAgentResources {
    #[cfg(windows)]
    pub(crate) native_host: bool,
    result_contract: Option<webcodex_core::cli_adapter::CliResultContract>,
    pub(crate) program: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) workspace_snapshot:
        Option<webcodex_core::runner_protocol::AgentExecutionWorkspaceSnapshot>,
    args: Vec<String>,
    proxy_requested: bool,
    #[cfg(windows)]
    network: webcodex_process::windows_lpac::LpacNetwork,
    #[cfg(windows)]
    local_network: Option<webcodex_process::windows_local_service::LocalServiceNetwork>,
    #[cfg(windows)]
    sid: webcodex_process::windows_lpac::AppContainerSid,
    #[cfg(windows)]
    _pins: Vec<std::fs::File>,
}

impl PreparedAgentResources {
    pub(crate) fn revoke_local_network_after_stop(&mut self) -> Result<(), String> {
        #[cfg(windows)]
        if let Some(guard) = self.local_network.take() {
            guard.remove_after_stop()?;
        }
        Ok(())
    }
    pub(crate) fn result_contract(&self) -> Option<&webcodex_core::cli_adapter::CliResultContract> {
        self.result_contract.as_ref()
    }
    pub(crate) fn proxy_requested(&self) -> bool {
        self.proxy_requested
    }
}

impl std::fmt::Debug for PreparedAgentResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedAgentResources")
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
impl PreparedAgentResources {
    pub(crate) fn launch_spec(&self) -> webcodex_process::windows_lpac::LpacLaunch<'_> {
        webcodex_process::windows_lpac::LpacLaunch {
            sid: &self.sid,
            program: &self.program,
            args: &self.args,
            cwd: &self.cwd,
            environment: &self.environment,
            network: self.network,
        }
    }
}

/// Initialize only the dedicated resource root. Keep the returned directory
/// handles for the lifetime of the JobManager configuration; existing parent
/// directories and ACLs are never created or changed by this function.
pub(crate) fn initialize_resource_root(path: &Path) -> Result<Vec<std::fs::File>, String> {
    absolute_path(path)?;
    #[cfg(windows)]
    {
        windows::initialize_resource_root(path)
    }
    #[cfg(not(windows))]
    {
        Err("agent_resources_platform_unsupported".into())
    }
}

/// Open an existing root for verified delivery history. This never creates
/// directories or changes ACLs and does not enable new business execution.
pub(crate) fn pin_existing_resource_root(path: &Path) -> Result<Vec<std::fs::File>, String> {
    absolute_path(path)?;
    #[cfg(windows)]
    {
        windows::pin_existing_resource_root(path)
    }
    #[cfg(not(windows))]
    {
        Err("agent_resources_platform_unsupported".into())
    }
}

pub(crate) fn prepare(
    profiles: &ValidatedResourceProfiles,
    start: &AgentExecutionStart,
    durable_job_root: &Path,
) -> Result<PreparedAgentResources, String> {
    let profile = selected_profile(profiles, start)?;
    absolute_path(durable_job_root)?;
    #[cfg(windows)]
    {
        let identity = start.binding.identity_hash()?;
        let mut prepared = prepare_host(
            profile,
            &start.process.args,
            start
                .workspace_source
                .as_ref()
                .map(|source| PathBuf::from(&source.root))
                .unwrap_or(
                    std::env::current_dir()
                        .map_err(|_| "native_cli_working_directory_unavailable")?,
                ),
            &identity,
        )?;
        let instance_root = prepared
            .cwd
            .join(".webcodex/instances")
            .join(format!("cli-{identity}"));
        for (name, directory) in [
            ("WEBCODEX_MEMORY_DIR", "memory"),
            ("WEBCODEX_OBJECTIVES_DIR", "objectives"),
            ("WEBCODEX_ACCEPTANCE_DIR", "acceptance"),
        ] {
            let path = instance_root.join(directory);
            std::fs::create_dir_all(&path)
                .map_err(|error| format!("cli_instance_directory_create_failed:{error}"))?;
            prepared
                .environment
                .insert(name.into(), path.to_string_lossy().into_owned());
        }
        Ok(prepared)
    }
    #[cfg(not(windows))]
    {
        let _ = profile;
        Err("agent_resources_platform_unsupported".into())
    }
}

#[cfg(windows)]
fn prepare_host(
    profile: &ValidatedProfile,
    args: &[String],
    cwd: PathBuf,
    identity: &str,
) -> Result<PreparedAgentResources, String> {
    let program = profile
        .files
        .iter()
        .find(|file| file.destination == PROGRAM)
        .ok_or("native_cli_program_missing")?;
    let program = PathBuf::from(&program.source);
    let cwd = cwd
        .canonicalize()
        .map_err(|error| format!("native_execution_working_directory_unavailable:{error}"))?;
    if !cwd.is_dir() {
        return Err("native_execution_working_directory_not_directory".into());
    }
    let cwd = webcodex_process::windows_lpac::native_host_path(&cwd);
    let mut environment = BTreeMap::new();
    for (key, value) in std::env::vars() {
        if !crate::webcodex_runner::shell::is_sensitive_env_key(&key) {
            let effective = std::env::var(&key).unwrap_or(value);
            environment.insert(key.to_ascii_uppercase(), effective);
        }
    }
    if profile.provider == Some(AgentCliProvider::WorkBuddy) {
        // Installed CodeBuddy's MemoryContextInterceptor reads this exact "1"
        // before shared project-memory injection. Keep explicit instance-file
        // CRUD and the rest of the normal process environment unchanged.
        environment.insert("CODEBUDDY_DISABLE_AUTO_MEMORY".into(), "1".into());
    }
    Ok(PreparedAgentResources {
        native_host: true,
        result_contract: profile
            .host_cli
            .as_ref()
            .and_then(|host| host.adapter.invocation.as_ref()?.result.clone()),
        program,
        cwd,
        environment,
        workspace_snapshot: None,
        args: args.to_vec(),
        proxy_requested: false,
        network: webcodex_process::windows_lpac::LpacNetwork::None,
        local_network: None,
        sid: webcodex_process::windows_lpac::AppContainerSid::derive(identity)
            .map_err(|error| error.to_string())?,
        _pins: Vec::new(),
    })
}

/// Ordinary programs use only credential-free resource profiles. The current
/// authenticated project source is supplied by Server/Runner, never by CLI
/// controls or a model-selected host executable.
pub(crate) fn prepare_program(
    profiles: &ValidatedResourceProfiles,
    start: &webcodex_core::agent_program::AgentProgramStart,
    durable_job_root: &Path,
    resume_source: Option<&Path>,
) -> Result<PreparedAgentResources, String> {
    profiles.validate_program(start)?;
    let profile = profiles
        .0
        .get(&start.binding.resource_set_id)
        .ok_or("agent_program_resource_unavailable")?;
    if profile.provider.is_some()
        || profile.reviewed_controls.is_some()
        || profile.user_https_proxy
        || profile.codebuddy_config_directory.is_some()
        || profile.manifest != start.binding.resource_manifest_hash
    {
        return Err("agent_program_resource_not_ordinary".into());
    }
    #[cfg(windows)]
    {
        let _ = (durable_job_root, resume_source);
        prepare_host(
            profile,
            &start.args,
            PathBuf::from(&start.binding.workspace_root).join(&start.binding.relative_cwd),
            &start.binding.identity_hash()?,
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (profile, durable_job_root);
        Err("agent_program_platform_unavailable".into())
    }
}

/// Shorten filesystem names without truncating the 256-bit execution identity.
/// Python and other Win32 programs can fail on relative paths even when the
/// Runner successfully copied those files using extended absolute paths.
/// Existing execution directories remain authoritative and are never moved.
pub(crate) fn program_resource_directory(
    durable_root: &Path,
    binding: &webcodex_core::agent_program::AgentProgramBinding,
) -> Result<PathBuf, String> {
    use base64::Engine;
    let identity = binding.identity_hash()?;
    let bytes = (0..identity.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&identity[index..index + 2], 16)
                .map_err(|_| "agent_program_resource_identity_invalid".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let compact = durable_root.join(format!(
        "p-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ));
    let legacy = durable_root.join(format!("program-{identity}"));
    let exists = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("agent_program_resource_layout_unavailable".to_string()),
    };
    if exists(&legacy)? {
        if exists(&compact)? {
            return Err("agent_program_resource_layout_ambiguous".into());
        }
        Ok(legacy)
    } else {
        Ok(compact)
    }
}

/// Local operator diagnostic only: not a Store admission, model Tool, review
/// record, or production StartIsolatedProcess route. Uses the same preparation
/// implementation as the corresponding reviewed file worker.
#[cfg(windows)]
pub(crate) fn prepare_codebuddy_files_probe(
    config: &AgentExecutionResourcesConfig,
    start: &AgentExecutionStart,
    durable_root: &Path,
) -> Result<PreparedAgentResources, String> {
    use webcodex_core::agent_cli_controls::*;
    start.validate()?;
    if config.profiles.len() != 1
        || config.profiles[0].reviewed_controls.is_some()
        || config.profiles[0].user_https_proxy
        || config.profiles[0].provider != Some(AgentCliProvider::WorkBuddy)
        || start.process.executable != format!("sha256:{CODEBUDDY_FILES_EXE_SHA256}")
        || start.process.args != codebuddy_file_probe_args()
        || start.binding.project != "operator-codebuddy-files-probe"
    {
        return Err("codebuddy_probe_fixed_contract_required".into());
    }
    let profiles = validate_profiles(config)?;
    let profile = profiles
        .0
        .get(&start.resource_set_id)
        .ok_or("agent_resources_profile_required")?;
    if profile.manifest != start.binding.resource_manifest_hash {
        return Err("agent_resources_manifest_mismatch".into());
    }
    for control in codebuddy_file_worker_control_files() {
        if !profile
            .files
            .iter()
            .any(|f| f.destination == control.path && f.sha256 == control.sha256)
        {
            return Err("codebuddy_probe_fixed_contract_required".into());
        }
    }
    if profile.files.len() != 3 {
        return Err("codebuddy_probe_fixed_contract_required".into());
    }
    windows::prepare_inner(profile, start, durable_root, true)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::Authorization::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;

    fn error(code: &str) -> String {
        format!("agent_resources_{code}:{}", unsafe { GetLastError() })
    }
    fn wide(path: &Path) -> Vec<u16> {
        // Public/source paths still pass absolute_path and component checks.
        // Only internal Win32 I/O receives an extended local-drive spelling;
        // otherwise the fixed auth subtree can exceed MAX_PATH in real roots.
        let value = path.to_string_lossy().replace('/', "\\");
        let bytes = value.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            format!("\\\\?\\{value}")
                .encode_utf16()
                .chain(Some(0))
                .collect()
        } else {
            path.as_os_str().encode_wide().chain(Some(0)).collect()
        }
    }

    fn inspect(file: &File, directory: bool) -> Result<u64, String> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(error("inspect_failed"));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
            || (!directory && info.nNumberOfLinks != 1)
        {
            return Err("agent_resources_reparse_or_link_rejected".into());
        }
        Ok(((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64)
    }

    fn file_identity(file: &File) -> Result<[u32; 3], String> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(error("file_identity_failed"));
        }
        Ok([
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ])
    }

    fn open(path: &Path, directory: bool, access: u32, share: u32) -> Result<File, String> {
        let name = wide(path);
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                access,
                share,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(error("open_failed"));
        }
        let file = unsafe { File::from_raw_handle(handle as _) };
        inspect(&file, directory)?;
        Ok(file)
    }

    // Pin every ancestor without FILE_SHARE_DELETE, not just the final path.
    // Existing ancestors are only opened; their ACLs are never modified.
    fn pin_directories(path: &Path, pins: &mut Vec<File>) -> Result<(), String> {
        let mut ancestors = path.ancestors().collect::<Vec<_>>();
        ancestors.reverse();
        for ancestor in ancestors {
            pins.push(open(
                ancestor,
                true,
                FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            )?);
        }
        Ok(())
    }

    fn open_cleanup_path(path: &Path, directory: bool) -> Result<Option<File>, String> {
        let name = wide(path);
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_READ_ATTRIBUTES | if directory { FILE_TRAVERSE } else { DELETE },
                if directory {
                    FILE_SHARE_READ | FILE_SHARE_WRITE
                } else {
                    0
                },
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            return if matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
                Ok(None)
            } else {
                Err(format!(
                    "agent_resources_native_auth_cleanup_open_failed:{code}"
                ))
            };
        }
        let file = unsafe { File::from_raw_handle(handle as _) };
        inspect(&file, directory)?;
        Ok(Some(file))
    }

    pub(super) fn cleanup_native_auth(
        binding: &AgentExecutionBinding,
        resource_root: &Path,
    ) -> Result<(), String> {
        let home = resource_root
            .join(format!("agent-{}", binding.identity_hash()?))
            .join("home");
        let codebuddy =
            home.join(webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE);
        cleanup_auth_directory(
            codebuddy.parent().ok_or("agent_resources_cleanup_parent")?,
            0,
            &mut 0,
        )?;
        cleanup_auth_path(
            &home.join(webcodex_core::agent_cli_controls::ANTIGRAVITY_NATIVE_AUTH_RELATIVE),
        )?;
        cleanup_auth_path(&home.join(".codex/auth.json"))?;
        cleanup_auth_path(&home.join(".claude/.credentials.json"))?;
        Ok(())
    }

    // A native refresh may atomically replace the private copy and create a
    // lock/temporary file beside it. Clean only that execution's credential
    // leaf after the process tree stops; never follow links or read contents.
    fn cleanup_auth_directory(path: &Path, depth: usize, count: &mut usize) -> Result<(), String> {
        if depth > 4 {
            return Err("agent_resources_auth_cleanup_limit".into());
        }
        let mut pins = Vec::new();
        let mut ancestors = path.ancestors().collect::<Vec<_>>();
        ancestors.reverse();
        for ancestor in ancestors {
            let Some(pin) = open_cleanup_path(ancestor, true)? else {
                return Ok(());
            };
            pins.push(pin);
        }
        for entry in
            std::fs::read_dir(path).map_err(|_| "agent_resources_auth_cleanup_read_failed")?
        {
            *count += 1;
            if *count > 64 {
                return Err("agent_resources_auth_cleanup_limit".into());
            }
            let entry = entry.map_err(|_| "agent_resources_auth_cleanup_read_failed")?;
            let kind = entry
                .file_type()
                .map_err(|_| "agent_resources_auth_cleanup_read_failed")?;
            if kind.is_dir() {
                cleanup_auth_directory(&entry.path(), depth + 1, count)?;
            } else {
                cleanup_auth_path(&entry.path())?;
            }
        }
        Ok(())
    }

    fn cleanup_auth_path(target: &Path) -> Result<(), String> {
        let parent = target.parent().ok_or("agent_resources_cleanup_parent")?;
        let mut ancestors = parent.ancestors().collect::<Vec<_>>();
        ancestors.reverse();
        let mut pins = Vec::new();
        // Match pin_directories: inspect and pin each ancestor without delete
        // sharing. An absent subtree is already clean; every other error fails.
        for ancestor in ancestors {
            let Some(file) = open_cleanup_path(ancestor, true)? else {
                return Ok(());
            };
            pins.push(file);
        }
        let Some(file) = open_cleanup_path(&target, false)? else {
            return Ok(());
        };
        // No content read and no path-based delete. The inspected exclusive
        // handle identifies the sole file while the ancestor pins stay alive.
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as _,
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(error("native_auth_cleanup_delete_failed"));
        }
        drop(file);
        Ok(())
    }

    pub(super) fn initialize_resource_root(path: &Path) -> Result<Vec<File>, String> {
        let mut pins = Vec::new();
        pin_directories(
            path.parent().ok_or("agent_resources_root_parent")?,
            &mut pins,
        )?;
        match std::fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(error("root_create_failed")),
        }
        // Even AlreadyExists must be opened without following a reparse point
        // and inspected as a directory. The parent handles prevent replacement
        // of the path's ancestors during creation and while this root is used.
        pins.push(open(
            path,
            true,
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )?);
        Ok(pins)
    }

    pub(super) fn pin_existing_resource_root(path: &Path) -> Result<Vec<File>, String> {
        let mut pins = Vec::new();
        pin_directories(path, &mut pins)?;
        Ok(pins)
    }

    fn source(
        path: &Path,
        expected: &str,
        total: &mut u64,
        pins: &mut Vec<File>,
    ) -> Result<File, String> {
        pin_directories(path.parent().ok_or("agent_resources_source_parent")?, pins)?;
        let mut file = open(path, false, GENERIC_READ, FILE_SHARE_READ)?;
        let size = inspect(&file, false)?;
        *total = total
            .checked_add(size)
            .ok_or("agent_resources_size_limit")?;
        if *total > MAX_RESOURCE_BYTES {
            return Err("agent_resources_size_limit".into());
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|_| error("source_read_failed"))?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if format!("{:x}", hash.finalize()) != expected {
            return Err("agent_resources_source_hash_mismatch".into());
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| error("source_seek_failed"))?;
        Ok(file)
    }

    pub(super) fn verify_sources(profiles: &ValidatedResourceProfiles) -> Result<(), String> {
        for (id, profile) in &profiles.0 {
            if profile.host_cli.is_some() {
                for item in &profile.files {
                    open_host_executable(Path::new(&item.source), Some(&item.sha256))?;
                }
                validate_profile_manifest(id, profile)?;
                continue;
            }
            let mut total = 0;
            let mut pins = Vec::new();
            for item in &profile.files {
                let file = source(Path::new(&item.source), &item.sha256, &mut total, &mut pins)?;
                pins.push(file);
            }
            if let Some(directory) = &profile.codebuddy_config_directory {
                pin_directories(Path::new(directory), &mut pins)?;
            }
            // Every configured content hash has just been verified through a
            // retained reader. Recompute P and the bound review/F from it.
            validate_profile_manifest(id, profile)?;
        }
        Ok(())
    }

    pub(super) fn pin_catalog_executable(
        id: &str,
        profile: &ValidatedProfile,
    ) -> Result<PinnedCatalogExecutable, String> {
        let executable = profile
            .files
            .iter()
            .find(|file| file.destination == PROGRAM)
            .ok_or("agent_resources_profile_required")?;
        let program = PathBuf::from(&executable.source);
        if profile.host_cli.is_some() {
            validate_profile_manifest(id, profile)?;
            let (program, source, _) = open_host_executable(&program, Some(&executable.sha256))?;
            return Ok(PinnedCatalogExecutable {
                program,
                entrypoint: None,
                codebuddy_config_directory: None,
                descriptor: AgentExecutionResourceDescriptor {
                    host_cli: profile.host_cli.clone(),
                    project_workspace_copy: false,
                    resource_set_id: id.into(),
                    manifest_hash: profile.manifest.clone(),
                    executable_identity: profile.executable_identity.clone(),
                    provider: profile.provider,
                    reviewed_controls: None,
                },
                _pins: vec![source],
            });
        }
        let mut pins = Vec::new();
        let mut bytes = 0;
        if profile.reviewed_controls.is_some() {
            // Returning reviewed facts requires their complete payload to stay
            // pinned for the probe, not just the executable's matching name.
            for item in &profile.files {
                let file = source(Path::new(&item.source), &item.sha256, &mut bytes, &mut pins)?;
                pins.push(file);
            }
        } else {
            let file = source(&program, &executable.sha256, &mut bytes, &mut pins)?;
            pins.push(file);
        }
        validate_profile_manifest(id, profile)?;
        let entrypoint = catalog_entrypoint(profile)?;
        let codebuddy_config_directory = profile
            .codebuddy_config_directory
            .as_ref()
            .map(PathBuf::from);
        if let Some(directory) = &codebuddy_config_directory {
            // Keep the selected directory and its ancestors stable for the
            // bounded metadata query without reading or pinning credentials.
            pin_directories(directory, &mut pins)?;
        }
        Ok(PinnedCatalogExecutable {
            program,
            entrypoint,
            codebuddy_config_directory,
            descriptor: AgentExecutionResourceDescriptor {
                host_cli: profile.host_cli.clone(),
                project_workspace_copy: cfg!(windows) && profile.host_cli.is_none(),
                resource_set_id: id.into(),
                manifest_hash: profile.manifest.clone(),
                executable_identity: profile.executable_identity.clone(),
                provider: profile.provider,
                reviewed_controls: profile.reviewed_controls.clone(),
            },
            _pins: pins,
        })
    }

    struct PinnedDeliveryFile {
        file: File,
        identity: [u32; 3],
        bytes: usize,
        sha256: String,
    }

    fn delivery_bytes(
        file: &mut File,
        max_bytes: usize,
    ) -> Result<(Vec<u8>, [u32; 3], String), String> {
        let length = inspect(file, false)?;
        if length > max_bytes as u64 {
            return Err("agent_cli_delivery_file_limit".into());
        }
        let identity = file_identity(file)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "agent_cli_delivery_read_failed")?;
        let mut bytes = Vec::new();
        (&mut *file)
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "agent_cli_delivery_read_failed")?;
        if bytes.len() as u64 != length
            || inspect(file, false)? != length
            || file_identity(file)? != identity
        {
            return Err("agent_cli_delivery_file_changed".into());
        }
        let digest = webcodex_core::agent_cli::sha256(&bytes);
        Ok((bytes, identity, digest))
    }

    fn pin_delivery_file(
        path: &Path,
        max_bytes: usize,
    ) -> Result<(Vec<u8>, PinnedDeliveryFile), String> {
        let mut file = open(path, false, GENERIC_READ, FILE_SHARE_READ)?;
        let (bytes, identity, sha256) = delivery_bytes(&mut file, max_bytes)?;
        let pin = PinnedDeliveryFile {
            file,
            identity,
            bytes: bytes.len(),
            sha256,
        };
        Ok((bytes, pin))
    }

    fn delivery_publication_error(error: String) -> String {
        match error.as_str() {
            "agent_resources_open_failed:2" | "agent_resources_open_failed:3" => {
                "agent_cli_delivery_not_published".into()
            }
            _ => error,
        }
    }

    pub(super) fn read_delivery_bundle(
        binding: &AgentExecutionBinding,
        resource_root: &Path,
    ) -> Result<AgentCliDeliveryBundle, String> {
        use webcodex_core::agent_cli::{
            AgentCliDeliveryFile, AGENT_CLI_DELIVERY_FILE_MAX_BYTES, AGENT_CLI_MANIFEST_MAX_BYTES,
        };
        let publish = resource_root
            .join(format!("agent-{}", binding.identity_hash()?))
            .join("workspace/publish");
        absolute_path(&publish)?;
        let mut directories = Vec::new();
        // A missing execution workspace is unavailable evidence, not proof of
        // absent publication. Classify only the worker's publish subtree.
        pin_directories(
            publish.parent().ok_or("agent_cli_delivery_invalid_path")?,
            &mut directories,
        )?;
        // Only this stopped Job's publication subtree denies write sharing.
        // Shared host ancestors retain the normal no-delete directory pins.
        directories.push(
            open(
                &publish,
                true,
                FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
                FILE_SHARE_READ,
            )
            .map_err(delivery_publication_error)?,
        );
        let (manifest_bytes, manifest_pin) =
            pin_delivery_file(&publish.join("manifest.json"), AGENT_CLI_MANIFEST_MAX_BYTES)
                .map_err(delivery_publication_error)?;
        let manifest = webcodex_core::agent_cli::parse_delivery_manifest(&manifest_bytes)?;
        let mut identities = BTreeSet::from([manifest_pin.identity]);
        let mut readers = vec![manifest_pin];
        let mut files = Vec::with_capacity(manifest.files.len());
        let mut total = manifest.summary.len();
        for entry in manifest.files {
            // The core validator has rejected absolute paths, devices, streams,
            // traversal and aliases. Pin each explicit parent without scanning.
            let relative = Path::new(&entry.path);
            let mut directory = publish.clone();
            for part in relative
                .parent()
                .ok_or("agent_cli_delivery_invalid_path")?
                .components()
            {
                directory.push(part);
                directories.push(open(
                    &directory,
                    true,
                    FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
                    FILE_SHARE_READ,
                )?);
            }
            let (bytes, pin) =
                pin_delivery_file(&publish.join(relative), AGENT_CLI_DELIVERY_FILE_MAX_BYTES)?;
            if entry
                .expected
                .as_ref()
                .is_some_and(|(size, hash)| pin.bytes as u64 != *size || &pin.sha256 != hash)
            {
                return Err("agent_cli_delivery_content_mismatch".into());
            }
            total = total
                .checked_add(pin.bytes)
                .ok_or("agent_cli_delivery_manifest_limit")?;
            if total > webcodex_core::agent_cli::AGENT_CLI_DELIVERY_TEXT_MAX_BYTES {
                return Err("agent_cli_delivery_manifest_limit".into());
            }
            if !identities.insert(pin.identity) {
                return Err("agent_cli_delivery_file_alias".into());
            }
            let text = String::from_utf8(bytes).map_err(|_| "agent_cli_delivery_text_required")?;
            if text.contains('\0') {
                return Err("agent_cli_delivery_text_required".into());
            }
            files.push(AgentCliDeliveryFile {
                path: entry.path,
                sha256: pin.sha256.clone(),
                bytes: pin.bytes as u64,
                text,
            });
            readers.push(pin);
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let bundle = AgentCliDeliveryBundle {
            version: 1,
            binding_sha256: binding.binding_hash()?,
            summary: manifest.summary,
            files,
        };
        bundle.canonical_bytes(binding)?;
        // Every reader (including the manifest) and all parent handles remain
        // held until the entire bounded in-memory snapshot has been rechecked.
        for pin in &mut readers {
            let (bytes, identity, sha256) = delivery_bytes(&mut pin.file, pin.bytes)?;
            if bytes.len() != pin.bytes || identity != pin.identity || sha256 != pin.sha256 {
                return Err("agent_cli_delivery_file_changed".into());
            }
        }
        Ok(bundle)
    }

    struct Descriptor(PSECURITY_DESCRIPTOR, bool);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    impl Descriptor {
        fn new(
            owner: &str,
            sid: &str,
            mask: u32,
            directory: bool,
            low_integrity: bool,
        ) -> Result<Self, String> {
            let inheritance = if directory { "OICI" } else { "" };
            // OW removes implicit owner WRITE_DAC. The restricted package gets
            // only this mask; never WRITE_DAC, WRITE_OWNER, or broad AAP access.
            let mut sddl = format!("D:P(A;{inheritance};RC;;;OW)(A;{inheritance};FA;;;{owner})(A;{inheritance};FA;;;SY)(A;{inheritance};0x{mask:08x};;;{sid})");
            if low_integrity {
                sddl.push_str(&format!("S:(ML;{inheritance};NW;;;LW)"));
            }
            let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
            let mut descriptor = null_mut();
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            } == 0
            {
                return Err(error("acl_descriptor_failed"));
            }
            Ok(Self(descriptor, low_integrity))
        }
        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.0,
                bInheritHandle: 0,
            }
        }
    }

    fn acl_entries(descriptor: PSECURITY_DESCRIPTOR) -> Result<Vec<Vec<u8>>, String> {
        let mut control = 0;
        let mut revision = 0;
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = null_mut();
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0
            || unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
            } == 0
            || present == 0
            || acl.is_null()
            || unsafe { IsValidAcl(acl) } == 0
        {
            return Err("agent_resources_acl_not_protected".into());
        }
        let mut entries = Vec::new();
        for index in 0..unsafe { (*acl).AceCount } as u32 {
            let mut ace = null_mut();
            if unsafe { GetAce(acl, index, &mut ace) } == 0 || ace.is_null() {
                return Err(error("acl_read_failed"));
            }
            let length = unsafe { (*(ace.cast::<ACE_HEADER>())).AceSize } as usize;
            entries.push(unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), length).to_vec() });
        }
        Ok(entries)
    }

    fn verify_acl(file: &File, expected: &Descriptor) -> Result<(), String> {
        let mut descriptor = null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as _,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION
                    | if expected.1 {
                        LABEL_SECURITY_INFORMATION
                    } else {
                        0
                    },
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS || descriptor.is_null() {
            return Err(format!("agent_resources_acl_read_failed:{status}"));
        }
        let actual = Descriptor(descriptor, false);
        if acl_entries(actual.0)? != acl_entries(expected.0)? {
            return Err("agent_resources_acl_mismatch".into());
        }
        // Compare the actual single label ACE byte-for-byte with the explicit
        // low label, including type, OI/CI flags, NW policy, and integrity SID.
        if expected.1 && mandatory_label(actual.0)? != mandatory_label(expected.0)? {
            return Err("agent_resources_integrity_label_mismatch".into());
        }
        Ok(())
    }

    fn mandatory_label(descriptor: PSECURITY_DESCRIPTOR) -> Result<Vec<u8>, String> {
        let (mut present, mut defaulted) = (0, 0);
        let mut acl = null_mut();
        if unsafe { GetSecurityDescriptorSacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            == 0
            || present == 0
            || acl.is_null()
            || unsafe { IsValidAcl(acl) } == 0
            || unsafe { (*acl).AceCount } != 1
        {
            return Err("agent_resources_integrity_label_missing".into());
        }
        let mut ace = null_mut();
        if unsafe { GetAce(acl, 0, &mut ace) } == 0 || ace.is_null() {
            return Err(error("integrity_label_read_failed"));
        }
        let length = unsafe { (*(ace.cast::<ACE_HEADER>())).AceSize } as usize;
        if !(std::mem::size_of::<ACE_HEADER>()..=256).contains(&length) {
            return Err("agent_resources_integrity_label_invalid".into());
        }
        Ok(unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), length).to_vec() })
    }

    fn sid_text(sid: PSID) -> Result<String, String> {
        let mut text = null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(error("sid_text_failed"));
        }
        let value = unsafe {
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let result = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text as _);
            result
        };
        Ok(value)
    }

    fn current_user() -> Result<String, String> {
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(error("user_token_failed"));
        }
        let result = (|| {
            let mut length = 0;
            unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length) };
            if length == 0 {
                return Err(error("user_size_failed"));
            }
            let mut data = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    data.as_mut_ptr().cast(),
                    length,
                    &mut length,
                )
            } == 0
            {
                return Err(error("user_read_failed"));
            }
            sid_text(unsafe { (*(data.as_ptr().cast::<TOKEN_USER>())).User.Sid })
        })();
        unsafe { CloseHandle(token) };
        result
    }

    fn create_directory(
        path: &Path,
        descriptor: &Descriptor,
        pins: &mut Vec<File>,
    ) -> Result<(), String> {
        let name = wide(path);
        let attributes = descriptor.attributes();
        // Fail on existing paths, including remnants of interrupted preparation.
        if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
            return Err(error("directory_create_failed"));
        }
        let file = open(
            path,
            true,
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )?;
        verify_acl(&file, descriptor)?;
        pins.push(file);
        Ok(())
    }

    fn copy_file(
        source: &mut impl Read,
        path: &Path,
        expected: &str,
        descriptor: &Descriptor,
    ) -> Result<File, String> {
        let name = wide(path);
        let attributes = descriptor.attributes();
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ,
                &attributes,
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(error("file_create_failed"));
        }
        let mut destination = unsafe { File::from_raw_handle(raw as _) };
        inspect(&destination, false)?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|_| error("copy_read_failed"))?;
            if count == 0 {
                break;
            }
            destination
                .write_all(&buffer[..count])
                .map_err(|_| error("copy_write_failed"))?;
            hash.update(&buffer[..count]);
        }
        if format!("{:x}", hash.finalize()) != expected {
            return Err("agent_resources_copy_hash_mismatch".into());
        }
        destination
            .sync_all()
            .map_err(|_| error("copy_sync_failed"))?;
        destination
            .seek(SeekFrom::Start(0))
            .map_err(|_| error("copy_seek_failed"))?;
        let mut destination_hash = Sha256::new();
        loop {
            let count = destination
                .read(&mut buffer)
                .map_err(|_| error("copy_verify_failed"))?;
            if count == 0 {
                break;
            }
            destination_hash.update(&buffer[..count]);
        }
        if format!("{:x}", destination_hash.finalize()) != expected {
            return Err("agent_resources_copy_hash_mismatch".into());
        }
        inspect(&destination, false)?;
        verify_acl(&destination, descriptor)?;
        let identity = file_identity(&destination)?;
        // A duplicate retains the writer's file-object sharing state. Close it
        // and reopen read-only; parent pins and the read-only package DACL stay
        // in force, and every property is rechecked before the file can launch.
        drop(destination);
        let mut pinned = open(path, false, GENERIC_READ, FILE_SHARE_READ)?;
        if file_identity(&pinned)? != identity {
            return Err("agent_resources_copy_identity_mismatch".into());
        }
        let mut pinned_hash = Sha256::new();
        loop {
            let count = pinned
                .read(&mut buffer)
                .map_err(|_| error("copy_reopen_verify_failed"))?;
            if count == 0 {
                break;
            }
            pinned_hash.update(&buffer[..count]);
        }
        if format!("{:x}", pinned_hash.finalize()) != expected {
            return Err("agent_resources_copy_hash_mismatch".into());
        }
        inspect(&pinned, false)?;
        verify_acl(&pinned, descriptor)?;
        Ok(pinned)
    }

    /// Copy authorized business files into the Job's writable tree. Source ACLs
    /// are never changed, and source handles are released before launching the CLI.
    fn copy_project_workspace(
        source_root: &Path,
        root: &Path,
        directory_acl: &Descriptor,
        file_acl: &Descriptor,
        readonly_acl: &Descriptor,
        pins: &mut Vec<File>,
        timeout_secs: u64,
    ) -> Result<webcodex_core::runner_protocol::AgentExecutionWorkspaceSnapshot, String> {
        use webcodex_core::runner_protocol::AgentExecutionWorkspaceSnapshot;
        const MAX_FILES: usize = 20_000;
        const MAX_BYTES: u64 = 256 * 1024 * 1024;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.min(60));
        let mut source_pins = Vec::new();
        pin_directories(source_root, &mut source_pins)?;
        let mut pending = vec![PathBuf::new()];
        let mut manifest = BTreeMap::<String, (String, u64)>::new();
        let mut total = 0u64;
        let mut entries_seen = 0usize;
        while let Some(relative_dir) = pending.pop() {
            if std::time::Instant::now() >= deadline || relative_dir.components().count() > 32 {
                return Err("agent_workspace_capture_limit".into());
            }
            let directory = source_root.join(&relative_dir);
            source_pins.push(open(
                &directory,
                true,
                FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            )?);
            let entries =
                std::fs::read_dir(&directory).map_err(|_| "agent_workspace_read_failed")?;
            for entry in entries {
                entries_seen += 1;
                if entries_seen > MAX_FILES * 2 || std::time::Instant::now() >= deadline {
                    return Err("agent_workspace_capture_limit".into());
                }
                let entry = entry.map_err(|_| "agent_workspace_read_failed")?;
                let relative = relative_dir.join(entry.file_name());
                let name = relative
                    .to_str()
                    .ok_or("agent_workspace_path_encoding")?
                    .replace('\\', "/");
                // Private runtime, credentials and generated output never become
                // source inputs. Dependencies may be copied within the hard budget.
                if webcodex_core::sensitive_paths::is_secret_path(&name)
                    || name.split('/').any(|p| {
                        matches!(
                            p.to_ascii_lowercase().as_str(),
                            ".local"
                                | ".codex"
                                | ".claude"
                                | "target"
                                | "dist"
                                | "build"
                                | "__pycache__"
                                | ".cache"
                        )
                    })
                    || name == "publish"
                    || name.starts_with("publish/")
                {
                    continue;
                }
                let kind = entry
                    .file_type()
                    .map_err(|_| "agent_workspace_metadata_failed")?;
                let destination = root.join("workspace").join(&relative);
                if kind.is_dir() {
                    // Opening every ancestor rejects junction/reparse substitution.
                    source_pins.push(open(
                        &entry.path(),
                        true,
                        FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                    )?);
                    create_directory(&destination, directory_acl, pins)?;
                    pending.push(relative);
                } else if kind.is_file() {
                    if manifest.len() >= MAX_FILES {
                        return Err("agent_workspace_capture_limit".into());
                    }
                    let mut file = open(&entry.path(), false, GENERIC_READ, FILE_SHARE_READ)?;
                    let size = inspect(&file, false)?;
                    total = total
                        .checked_add(size)
                        .ok_or("agent_workspace_capture_limit")?;
                    if total > MAX_BYTES {
                        return Err("agent_workspace_capture_limit".into());
                    }
                    let mut hasher = Sha256::new();
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err("agent_workspace_capture_limit".into());
                        }
                        let n = file
                            .read(&mut buffer)
                            .map_err(|_| "agent_workspace_read_failed")?;
                        if n == 0 {
                            break;
                        }
                        hasher.update(&buffer[..n]);
                    }
                    let hash = format!("{:x}", hasher.finalize());
                    file.seek(SeekFrom::Start(0))
                        .map_err(|_| "agent_workspace_read_failed")?;
                    // Drop the destination pin so the CLI can edit its own copy.
                    drop(copy_file(&mut file, &destination, &hash, file_acl)?);
                    manifest.insert(name, (hash, size));
                } else {
                    return Err("agent_workspace_link_or_special_file_denied".into());
                }
            }
        }
        let bytes = serde_json::to_vec(&manifest).map_err(|_| "agent_workspace_manifest_failed")?;
        let manifest_sha256 = format!("{:x}", Sha256::digest(&bytes));
        pins.push(copy_file(
            &mut bytes.as_slice(),
            &root.join("input/project-manifest.json"),
            &manifest_sha256,
            readonly_acl,
        )?);
        Ok(AgentExecutionWorkspaceSnapshot {
            manifest_sha256,
            file_count: manifest.len(),
            total_bytes: total,
        })
    }

    // Copy the one authorized native-auth file as opaque bytes. Never parse,
    // hash, log, export or write back credentials; do not retain host-home pins.
    fn copy_native_auth(
        user_home: &Path,
        root: &Path,
        directory_acl: &Descriptor,
        file_acl: &Descriptor,
        refresh_acl: Option<(&Descriptor, &Descriptor)>,
        destination_pins: &mut Vec<File>,
    ) -> Result<(), String> {
        copy_native_auth_file(
            user_home,
            root,
            webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE,
            directory_acl,
            file_acl,
            refresh_acl,
            destination_pins,
        )
    }

    fn copy_native_auth_file(
        user_home: &Path,
        root: &Path,
        relative: &str,
        directory_acl: &Descriptor,
        file_acl: &Descriptor,
        refresh_acl: Option<(&Descriptor, &Descriptor)>,
        destination_pins: &mut Vec<File>,
    ) -> Result<(), String> {
        absolute_path(user_home)?;
        let source_path = user_home.join(relative);
        let mut source_pins = Vec::new();
        pin_directories(
            source_path
                .parent()
                .ok_or("codebuddy_native_auth_source_unavailable")?,
            &mut source_pins,
        )?;
        let mut source = open(&source_path, false, GENERIC_READ, FILE_SHARE_READ)
            .map_err(|_| "codebuddy_native_auth_source_unavailable")?;
        let size = inspect(&source, false)?;
        if size == 0 || size > 2 * 1024 * 1024 {
            return Err("codebuddy_native_auth_source_invalid".into());
        }
        let identity = file_identity(&source)?;
        let destination_path = root.join("home").join(relative);
        let mut directory = root.join("home");
        for part in Path::new(relative).parent().unwrap().components() {
            directory.push(part);
            let acl = if Some(directory.as_path()) == destination_path.parent() {
                refresh_acl.map(|pair| pair.0).unwrap_or(directory_acl)
            } else {
                directory_acl
            };
            create_directory(&directory, acl, destination_pins)?;
        }
        let file_acl = refresh_acl.map(|pair| pair.1).unwrap_or(file_acl);
        let name = wide(&destination_path);
        let attributes = file_acl.attributes();
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                FILE_SHARE_READ,
                &attributes,
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(error("native_auth_copy_create_failed"));
        }
        let mut destination = unsafe { File::from_raw_handle(raw as _) };
        let mut buffer = [0u8; 8192];
        let mut copied = 0u64;
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|_| "codebuddy_native_auth_copy_failed")?;
            if count == 0 {
                break;
            }
            copied += count as u64;
            if copied > size {
                buffer.fill(0);
                return Err("codebuddy_native_auth_source_changed".into());
            }
            let result = destination.write_all(&buffer[..count]);
            buffer.fill(0);
            result.map_err(|_| "codebuddy_native_auth_copy_failed")?;
        }
        if copied != size || inspect(&source, false)? != size || file_identity(&source)? != identity
        {
            return Err("codebuddy_native_auth_source_changed".into());
        }
        destination
            .sync_all()
            .map_err(|_| "codebuddy_native_auth_copy_failed")?;
        verify_acl(&destination, file_acl)?;
        let destination_identity = file_identity(&destination)?;
        drop(destination);
        // Ancestors remain pinned. Only this execution's leaf is writable;
        // native authentication refresh must be able to replace its own copy.
        // The source was opened read-only and is never written back.
        if refresh_acl.is_some() {
            return Ok(());
        }
        let pinned = open(
            &destination_path,
            false,
            GENERIC_READ | READ_CONTROL,
            FILE_SHARE_READ,
        )?;
        if inspect(&pinned, false)? != size || file_identity(&pinned)? != destination_identity {
            return Err("codebuddy_native_auth_copy_changed".into());
        }
        verify_acl(&pinned, file_acl)?;
        destination_pins.push(pinned);
        Ok(())
    }

    fn copy_antigravity_auth(
        root: &Path,
        owner: &str,
        package: &str,
        pins: &mut Vec<File>,
    ) -> Result<PathBuf, String> {
        use windows_sys::Win32::Security::Credentials::{
            CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        };
        struct Credential(*mut CREDENTIALW);
        impl Drop for Credential {
            fn drop(&mut self) {
                unsafe {
                    let value = &mut *self.0;
                    if !value.CredentialBlob.is_null()
                        && value.CredentialBlobSize <= 2 * 1024 * 1024
                    {
                        std::ptr::write_bytes(
                            value.CredentialBlob,
                            0,
                            value.CredentialBlobSize as usize,
                        );
                    }
                    CredFree(self.0.cast());
                }
            }
        }
        let target: Vec<u16> = "gemini:antigravity".encode_utf16().chain(Some(0)).collect();
        let mut pointer = null_mut();
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
            return Err("antigravity_native_auth_unavailable".into());
        }
        let credential = Credential(pointer);
        let value = unsafe { &*credential.0 };
        if value.CredentialBlob.is_null()
            || value.CredentialBlobSize == 0
            || value.CredentialBlobSize > 2 * 1024 * 1024
        {
            return Err("antigravity_native_auth_invalid".into());
        }
        let directory_acl = Descriptor::new(
            owner,
            package,
            FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_GENERIC_EXECUTE
                | DELETE
                | FILE_DELETE_CHILD,
            true,
            true,
        )?;
        let file_acl = Descriptor::new(owner, package, FILE_GENERIC_READ, false, false)?;
        let mut directory = root.join("home");
        for name in [".gemini", "antigravity-cli"] {
            directory.push(name);
            create_directory(&directory, &directory_acl, pins)?;
        }
        // Confirmed by a non-secret malformed-file probe against this pinned
        // CLI version. Keep the native token profile opaque; no host writeback.
        let path = directory.join("antigravity-oauth-token");
        let name = wide(&path);
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                FILE_SHARE_READ,
                &file_acl.attributes(),
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(error("native_auth_copy_create_failed"));
        }
        let mut file = unsafe { File::from_raw_handle(raw as _) };
        let bytes = unsafe {
            std::slice::from_raw_parts(value.CredentialBlob, value.CredentialBlobSize as usize)
        };
        file.write_all(bytes)
            .map_err(|_| "antigravity_native_auth_copy_failed")?;
        file.sync_all()
            .map_err(|_| "antigravity_native_auth_copy_failed")?;
        verify_acl(&file, &file_acl)?;
        pins.push(file);
        Ok(path)
    }

    fn install_antigravity_controls(
        root: &Path,
        owner: &str,
        package: &str,
        pins: &mut Vec<File>,
    ) -> Result<(), String> {
        let ro = Descriptor::new(owner, package, FILE_GENERIC_READ, false, false)?;
        let ro_dir = Descriptor::new(
            owner,
            package,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            true,
            false,
        )?;
        let config = root.join("home/.gemini/config");
        // Native CLI creates its config lock/state beside the agent directory.
        // The agent directory and both policy files remain read-only and pinned.
        let config_dir = Descriptor::new(
            owner,
            package,
            FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_GENERIC_EXECUTE
                | DELETE
                | FILE_DELETE_CHILD,
            true,
            true,
        )?;
        create_directory(&config, &config_dir, pins)?;
        create_directory(&config.join("agents"), &ro_dir, pins)?;
        let (settings, agent) = antigravity_control_bytes(root)?;
        for (target, bytes) in [
            (
                root.join("home/.gemini/antigravity-cli/settings.json"),
                settings,
            ),
            (config.join("agents/webcodex-file-worker.md"), agent),
        ] {
            let digest = format!("{:x}", Sha256::digest(&bytes));
            pins.push(copy_file(
                &mut std::io::Cursor::new(bytes),
                &target,
                &digest,
                &ro,
            )?);
        }
        Ok(())
    }

    fn write_auth_canary(
        root: &Path,
        descriptor: &Descriptor,
        pins: &mut Vec<File>,
    ) -> Result<(), String> {
        let path = root
            .join("home")
            .join(webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE)
            .with_file_name("permission-canary.txt");
        let name = wide(&path);
        let attributes = descriptor.attributes();
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                FILE_SHARE_READ,
                &attributes,
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(error("auth_canary_create_failed"));
        }
        let mut file = unsafe { File::from_raw_handle(raw as _) };
        file.write_all(b"CODEBUDDY_AUTH_PATH_NON_SECRET_CANARY\n")
            .map_err(|_| "auth_canary_write_failed")?;
        file.sync_all().map_err(|_| "auth_canary_write_failed")?;
        drop(file);
        let pinned = open(&path, false, GENERIC_READ | READ_CONTROL, FILE_SHARE_READ)?;
        verify_acl(&pinned, descriptor)?;
        pins.push(pinned);
        Ok(())
    }

    #[cfg(test)]
    mod native_auth_tests {
        use super::*;
        #[test]
        fn native_auth_copy_is_single_opaque_readonly_file_without_host_writeback() {
            let temp = tempfile::tempdir().unwrap();
            let host = temp.path().join("fake-host-home");
            let root = temp.path().join("w".repeat(80)).join("x".repeat(80));
            let source =
                host.join(webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE);
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::create_dir_all(root.join("home")).unwrap();
            let fixture = b"{\"auth\":\"synthetic-not-a-credential\"}";
            std::fs::write(&source, fixture).unwrap();
            std::fs::write(host.join("history-private.txt"), b"not authorized").unwrap();
            let sid =
                webcodex_process::windows_lpac::AppContainerSid::derive("codebuddy-auth-copy-unit")
                    .unwrap();
            let owner = current_user().unwrap();
            let package = sid_text(sid.as_ptr()).unwrap();
            let dir = Descriptor::new(
                &owner,
                &package,
                FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
                true,
                false,
            )
            .unwrap();
            let file = Descriptor::new(&owner, &package, FILE_GENERIC_READ, false, false).unwrap();
            let mut pins = Vec::new();
            copy_native_auth(&host, &root, &dir, &file, None, &mut pins).unwrap();
            let copied = root
                .join("home")
                .join(webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE);
            assert!(copied.to_string_lossy().len() > 260);
            assert_eq!(std::fs::read(&copied).unwrap(), fixture);
            assert!(!root.join("home/history-private.txt").exists());
            assert!(std::fs::OpenOptions::new()
                .write(true)
                .open(&copied)
                .is_err());
            assert_eq!(std::fs::read(&source).unwrap(), fixture);
            std::fs::write(&source, b"host can refresh independently").unwrap();
            assert_eq!(std::fs::read(&copied).unwrap(), fixture);
        }

        #[test]
        fn native_refresh_is_private_replaceable_and_all_copies_are_cleaned() {
            let temp = tempfile::tempdir().unwrap();
            let host = temp.path().join("host");
            let root = temp.path().join("execution");
            let relative = webcodex_core::agent_cli_controls::CODEBUDDY_NATIVE_AUTH_RELATIVE;
            let source = host.join(relative);
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::create_dir_all(root.join("home")).unwrap();
            std::fs::write(&source, b"synthetic original").unwrap();
            let sid =
                webcodex_process::windows_lpac::AppContainerSid::derive("private-refresh-unit")
                    .unwrap();
            let owner = current_user().unwrap();
            let package = sid_text(sid.as_ptr()).unwrap();
            let ro_dir = Descriptor::new(
                &owner,
                &package,
                FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
                true,
                false,
            )
            .unwrap();
            let ro_file =
                Descriptor::new(&owner, &package, FILE_GENERIC_READ, false, false).unwrap();
            let rw_dir = Descriptor::new(
                &owner,
                &package,
                FILE_GENERIC_READ
                    | FILE_GENERIC_WRITE
                    | FILE_GENERIC_EXECUTE
                    | DELETE
                    | FILE_DELETE_CHILD,
                true,
                true,
            )
            .unwrap();
            let rw_file = Descriptor::new(
                &owner,
                &package,
                FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
                false,
                true,
            )
            .unwrap();
            let mut pins = Vec::new();
            copy_native_auth(
                &host,
                &root,
                &ro_dir,
                &ro_file,
                Some((&rw_dir, &rw_file)),
                &mut pins,
            )
            .unwrap();
            let copied = root.join("home").join(relative);
            let parent = copied.parent().unwrap();
            let lock = parent.join(".auth.workbuddy.lock");
            std::fs::create_dir(&lock).unwrap();
            std::fs::write(lock.join("lease"), b"synthetic lease").unwrap();
            std::fs::rename(&copied, parent.join("auth.backup")).unwrap();
            std::fs::write(&copied, b"synthetic refreshed").unwrap();
            assert_eq!(std::fs::read(&source).unwrap(), b"synthetic original");
            cleanup_auth_directory(parent, 0, &mut 0).unwrap();
            assert!(!copied.exists());
            assert!(!parent.join("auth.backup").exists());
            assert!(!lock.join("lease").exists());
            assert_eq!(std::fs::read(&source).unwrap(), b"synthetic original");
            // Reject aliases before deleting a possible host file.
            std::fs::hard_link(&source, parent.join("aliased")).unwrap();
            assert!(cleanup_auth_directory(parent, 0, &mut 0).is_err());
            assert_eq!(std::fs::read(&source).unwrap(), b"synthetic original");
        }
    }

    pub(super) fn prepare(
        profile: &ValidatedProfile,
        start: &AgentExecutionStart,
        durable_root: &Path,
    ) -> Result<PreparedAgentResources, String> {
        prepare_inner(profile, start, durable_root, false)
    }

    pub(super) fn prepare_program(
        profile: &ValidatedProfile,
        start: &webcodex_core::agent_program::AgentProgramStart,
        durable_root: &Path,
        resume_source: Option<&Path>,
    ) -> Result<PreparedAgentResources, String> {
        validate_profile_manifest(&start.binding.resource_set_id, profile)?;
        let identity = start.binding.identity_hash()?;
        let sid = webcodex_process::windows_lpac::AppContainerSid::derive(&identity)
            .map_err(|e| e.to_string())?;
        let owner = current_user()?;
        let package = sid_text(sid.as_ptr())?;
        let root = program_resource_directory(durable_root, &start.binding)?;
        let mut pins = Vec::new();
        pin_directories(durable_root, &mut pins)?;
        let traverse = Descriptor::new(
            &owner,
            &package,
            FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            true,
            false,
        )?;
        let ro_dir = Descriptor::new(
            &owner,
            &package,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            true,
            false,
        )?;
        let rw_dir = Descriptor::new(
            &owner,
            &package,
            FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_GENERIC_EXECUTE
                | DELETE
                | FILE_DELETE_CHILD,
            true,
            true,
        )?;
        let ro_file = Descriptor::new(&owner, &package, FILE_GENERIC_READ, false, false)?;
        let binary = Descriptor::new(
            &owner,
            &package,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            false,
            false,
        )?;
        let rw_file = Descriptor::new(
            &owner,
            &package,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE,
            false,
            true,
        )?;
        create_directory(&root, &traverse, &mut pins)?;
        for name in ["bin", "input"] {
            create_directory(&root.join(name), &ro_dir, &mut pins)?;
        }
        for name in WRITABLE_DIRS {
            create_directory(&root.join(name), &rw_dir, &mut pins)?;
        }
        let mut created = BTreeSet::from(["bin".to_owned(), "input".to_owned()]);
        let mut total = 0;
        for item in &profile.files {
            let mut file = source(Path::new(&item.source), &item.sha256, &mut total, &mut pins)?;
            let relative = Path::new(&item.destination);
            let mut directory = PathBuf::new();
            for part in relative
                .parent()
                .ok_or("agent_resources_destination_parent")?
                .components()
            {
                directory.push(part);
                if created.insert(directory.to_string_lossy().to_ascii_lowercase()) {
                    create_directory(&root.join(&directory), &ro_dir, &mut pins)?;
                }
            }
            pins.push(copy_file(
                &mut file,
                &root.join(relative),
                &item.sha256,
                if item.destination.starts_with("bin/") {
                    &binary
                } else {
                    &ro_file
                },
            )?);
        }
        let source = resume_source.map(Path::to_path_buf).unwrap_or_else(|| {
            Path::new(&start.binding.workspace_root).join(&start.binding.relative_cwd)
        });
        let snapshot = copy_project_workspace(
            &source,
            &root,
            &rw_dir,
            &rw_file,
            &ro_file,
            &mut pins,
            start.timeout_secs,
        )?;
        let mut windows_path = vec![0u16; 32768];
        let size =
            unsafe { GetWindowsDirectoryW(windows_path.as_mut_ptr(), windows_path.len() as u32) };
        if size == 0 || size as usize >= windows_path.len() {
            return Err(error("windows_directory_failed"));
        }
        let windows_path = PathBuf::from(
            String::from_utf16(&windows_path[..size as usize])
                .map_err(|_| "agent_resources_windows_path_encoding")?,
        );
        let mut environment = environment(&root, &windows_path)?;
        if let Some(port) = start.binding.local_service_port {
            environment.insert("WEBCODEX_PORT".into(), port.to_string());
        }
        if profile
            .files
            .iter()
            .any(|f| f.destination == "bin/browser/msedge.exe")
        {
            environment.insert(
                "WEBCODEX_BROWSER_EXECUTABLE".into(),
                absolute_path(&root.join("bin/browser/msedge.exe"))?,
            );
            environment.insert(
                "PLAYWRIGHT_BROWSERS_PATH".into(),
                absolute_path(&root.join("bin/browser"))?,
            );
            environment.insert(
                "NODE_OPTIONS".into(),
                "--preserve-symlinks --preserve-symlinks-main".into(),
            );
        }
        validate_profile_manifest(&start.binding.resource_set_id, profile)?;
        let local_network = start
            .binding
            .local_service_port
            .map(|port| {
                webcodex_process::windows_local_service::LocalServiceNetwork::install(
                    &sid, &identity, port,
                )
            })
            .transpose()?;
        let network = local_network
            .as_ref()
            .map_or(webcodex_process::windows_lpac::LpacNetwork::None, |g| {
                g.mode()
            });
        Ok(PreparedAgentResources {
            native_host: false,
            result_contract: None,
            program: root.join(PROGRAM),
            cwd: root.join("workspace"),
            environment,
            workspace_snapshot: Some(snapshot),
            args: start.args.clone(),
            proxy_requested: false,
            network,
            local_network,
            sid,
            _pins: pins,
        })
    }

    pub(super) fn prepare_inner(
        profile: &ValidatedProfile,
        start: &AgentExecutionStart,
        durable_root: &Path,
        operator_probe: bool,
    ) -> Result<PreparedAgentResources, String> {
        use webcodex_core::agent_cli_controls::*;
        let file_worker = operator_probe
            || reviewed_file_worker(
                profile.provider,
                &profile.executable_identity,
                profile.reviewed_controls.as_ref(),
            );
        if operator_probe
            && profile.executable_identity != format!("sha256:{CODEBUDDY_FILES_EXE_SHA256}")
        {
            return Err("codebuddy_native_binary_mismatch".into());
        }
        let identity = start.binding.identity_hash()?;
        // The native name limit is 64 characters. The domain-separated identity
        // already fits exactly, retaining all 256 bits without an added prefix.
        let sid = webcodex_process::windows_lpac::AppContainerSid::derive(&identity)
            .map_err(|e| e.to_string())?;
        let owner = current_user()?;
        let package = sid_text(sid.as_ptr())?;
        let root = durable_root.join(format!("agent-{identity}"));
        let mut pins = Vec::new();
        pin_directories(durable_root, &mut pins)?;
        let traverse = Descriptor::new(
            &owner,
            &package,
            FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            true,
            false,
        )?;
        let ro_dir = Descriptor::new(
            &owner,
            &package,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            true,
            false,
        )?;
        let rw_dir = Descriptor::new(
            &owner,
            &package,
            FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_GENERIC_EXECUTE
                | DELETE
                | FILE_DELETE_CHILD,
            true,
            true,
        )?;
        let input_file = Descriptor::new(&owner, &package, FILE_GENERIC_READ, false, false)?;
        let binary_file = Descriptor::new(
            &owner,
            &package,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            false,
            false,
        )?;
        create_directory(&root, &traverse, &mut pins)?;
        create_directory(&root.join("bin"), &ro_dir, &mut pins)?;
        create_directory(&root.join("input"), &ro_dir, &mut pins)?;
        for name in WRITABLE_DIRS {
            create_directory(&root.join(name), &rw_dir, &mut pins)?;
        }
        let mut created = BTreeSet::from(["bin".to_owned(), "input".to_owned()]);
        let mut total = 0;
        for item in &profile.files {
            let mut source = source(Path::new(&item.source), &item.sha256, &mut total, &mut pins)?;
            let relative = Path::new(&item.destination);
            let mut directory = PathBuf::new();
            for part in relative
                .parent()
                .ok_or("agent_resources_destination_parent")?
                .components()
            {
                directory.push(part);
                let key = directory.to_string_lossy().to_ascii_lowercase();
                if created.insert(key) {
                    create_directory(&root.join(&directory), &ro_dir, &mut pins)?;
                }
            }
            let descriptor = if item.destination.starts_with("bin/") {
                &binary_file
            } else {
                &input_file
            };
            pins.push(copy_file(
                &mut source,
                &root.join(relative),
                &item.sha256,
                descriptor,
            )?);
        }
        // The read-only materialized bytes match P. Recheck the same subject
        // and fixed argv before returning any launch specification.
        validate_profile_manifest(&start.resource_set_id, profile)?;
        if !operator_probe {
            validate_execution_args(profile, start)?;
        }
        let credential_loading = if profile.host_cli.is_some() {
            profile.provider
        } else {
            legacy_credential_loading(profile.reviewed_controls.as_ref())
        };
        let antigravity_files = credential_loading == Some(AgentCliProvider::Antigravity);
        if operator_probe || credential_loading == Some(AgentCliProvider::WorkBuddy) {
            let user_home = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .ok_or("codebuddy_native_auth_source_unavailable")?;
            let refresh_file = Descriptor::new(
                &owner,
                &package,
                FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
                false,
                true,
            )?;
            let refresh = (!operator_probe).then_some((&rw_dir, &refresh_file));
            copy_native_auth(&user_home, &root, &ro_dir, &input_file, refresh, &mut pins)?;
            if operator_probe {
                write_auth_canary(&root, &input_file, &mut pins)?;
            }
        }
        let mut windows_path = vec![0u16; 32768];
        let length =
            unsafe { GetWindowsDirectoryW(windows_path.as_mut_ptr(), windows_path.len() as u32) };
        if length == 0 || length as usize >= windows_path.len() {
            return Err(error("windows_directory_failed"));
        }
        let windows_path = PathBuf::from(
            String::from_utf16(&windows_path[..length as usize])
                .map_err(|_| "agent_resources_windows_path_encoding")?,
        );
        absolute_path(&windows_path)?;
        let mut args = if operator_probe {
            start.process.args.clone()
        } else {
            materialized_execution_args(profile, start, &root)?
        };
        if operator_probe {
            for index in 0..args.len().saturating_sub(1) {
                if matches!(args[index].as_str(), "--settings" | "--mcp-config") {
                    args[index + 1] = absolute_path(&root.join(&args[index + 1]))?;
                }
            }
        }
        let mut environment = environment(&root, &windows_path)?;
        if profile
            .reviewed_controls
            .as_ref()
            .is_some_and(|control| control.loading_method == CODEBUDDY_NODE_FILES_LOADING)
        {
            codebuddy_node_environment(&root, &mut environment)?;
        }
        let proxy_requested = !operator_probe && profile.user_https_proxy;
        if proxy_requested {
            environment.insert(
                "HTTPS_PROXY".into(),
                super::super::agent_proxy::read_user_https_proxy()?,
            );
        }
        if antigravity_files {
            // The durable Job already owns cleanup intent before preparation.
            copy_antigravity_auth(&root, &owner, &package, &mut pins)?;
            if profile.host_cli.is_none() {
                install_antigravity_controls(&root, &owner, &package, &mut pins)?;
            }
        }
        let workspace_snapshot = if let Some(source) = &start
            .workspace_source
            .as_ref()
            .filter(|_| profile.host_cli.is_none())
        {
            let writable_file = Descriptor::new(
                &owner,
                &package,
                FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE,
                false,
                true,
            )?;
            let snapshot = copy_project_workspace(
                Path::new(&source.root),
                &root,
                &rw_dir,
                &writable_file,
                &input_file,
                &mut pins,
                start.timeout_secs,
            )?;
            environment.insert(
                "WEBCODEX_PROJECT_MANIFEST".into(),
                absolute_path(&root.join("input/project-manifest.json"))?,
            );
            Some(snapshot)
        } else {
            None
        };
        Ok(PreparedAgentResources {
            native_host: false,
            result_contract: profile
                .host_cli
                .as_ref()
                .map(|host| host.adapter.clone())
                .or_else(|| {
                    profile.reviewed_controls.as_ref().and_then(|record| {
                        record.adapter.clone().or_else(|| {
                            webcodex_core::cli_adapter::cli_adapters(&[])
                                .ok()?
                                .into_iter()
                                .find(|adapter| adapter.id == record.provider.id())
                        })
                    })
                })
                .and_then(|adapter| adapter.invocation?.result),
            workspace_snapshot,
            program: root.join(PROGRAM),
            cwd: if profile.host_cli.is_some() {
                start
                    .workspace_source
                    .as_ref()
                    .map(|s| PathBuf::from(&s.root))
                    .unwrap_or_else(|| root.join("workspace"))
            } else {
                root.join("workspace")
            },
            environment,
            args,
            proxy_requested,
            network: if file_worker {
                webcodex_process::windows_lpac::LpacNetwork::ReviewedFileWorkerInternetClient
            } else {
                webcodex_process::windows_lpac::LpacNetwork::None
            },
            local_network: None,
            sid,
            _pins: pins,
        })
    }
}

#[cfg(test)]
#[path = "agent_resources_tests.rs"]
mod tests;
