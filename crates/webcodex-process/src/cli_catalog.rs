//! Fixed installed-CLI metadata queries shared by Desktop and the Runner.
//! This is a trusted controller API: no model-selected path, argv, environment
//! or proxy is accepted. The caller owns exact executable authorization and,
//! where required, SHA/nofollow/path pins for the complete probe. This module
//! does not attest the launched image, account entitlement or LPAC readiness.
use crate::{ManagedChild, SpawnOptions};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;
use tokio::time::Instant;

const OUTPUT_LIMIT: usize = 64 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
static QUERY_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliCatalog {
    pub provider_id: String,
    pub label: String,
    pub installed: bool,
    pub version: String,
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    pub model_efforts: BTreeMap<String, Vec<String>>,
    pub source: &'static str,
    pub status: &'static str,
    pub message: String,
    /// Unix milliseconds; retained as a string for the Desktop wire contract.
    pub checked_at: String,
    pub account_availability_verified: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    WorkBuddy,
    Antigravity,
    Generic,
    Codex,
    Claude,
}
impl Provider {
    pub fn parse(id: &str) -> Result<Self, String> {
        match id {
            "workbuddy" => Ok(Self::WorkBuddy),
            "antigravity" => Ok(Self::Antigravity),
            _ => Err("Unsupported CLI provider".into()),
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::WorkBuddy => "workbuddy",
            Self::Antigravity => "antigravity",
            Self::Generic => "generic",
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    pub fn executable(self) -> &'static str {
        match (self, cfg!(windows)) {
            (Self::WorkBuddy, true) => "codebuddy.exe",
            (Self::WorkBuddy, false) => "codebuddy",
            (Self::Antigravity, true) => "agy.exe",
            (Self::Antigravity, false) => "agy",
            (Self::Generic, _) => "cli",
            (Self::Codex, true) => "codex.exe",
            (Self::Codex, false) => "codex",
            (Self::Claude, true) => "claude.exe",
            (Self::Claude, false) => "claude",
        }
    }
    pub fn installation_directory(self) -> &'static str {
        match self {
            Self::WorkBuddy => "codebuddy",
            Self::Antigravity => "agy",
            Self::Generic => "cli",
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::WorkBuddy => "WorkBuddy / CodeBuddy CLI",
            Self::Antigravity => "Antigravity CLI",
            Self::Generic => "CLI",
            Self::Codex => "Codex CLI",
            Self::Claude => "Claude Code",
        }
    }
    pub fn for_adapter(adapter: &webcodex_core::cli_adapter::CliAdapter) -> Self {
        use webcodex_core::cli_adapter::CliMetadataRecipe;
        match adapter.metadata {
            CliMetadataRecipe::CodebuddyHelp => Self::WorkBuddy,
            CliMetadataRecipe::AntigravityModels => Self::Antigravity,
            CliMetadataRecipe::Help => Self::Generic,
            CliMetadataRecipe::CodexAppServer => Self::Codex,
            CliMetadataRecipe::ClaudeInitialize => Self::Claude,
        }
    }
}
impl CliCatalog {
    pub fn unavailable(provider: Provider) -> Self {
        Self {
            provider_id: provider.id().into(),
            label: provider.label().into(),
            installed: false,
            version: String::new(),
            models: Vec::new(),
            efforts: Vec::new(),
            model_efforts: BTreeMap::new(),
            source: "unavailable",
            status: "unavailable",
            message: "未找到已安装的 CLI。安装后重新刷新。".into(),
            checked_at: timestamp_ms(),
            account_availability_verified: false,
        }
    }
}

/// A non-serializable snapshot created only by the trusted metadata controller.
/// Standard user-directory references let the CLI use its own official login
/// mechanism. Claude's gateway settings are projected through a fixed allowlist;
/// hooks/plugins/commands and unrelated credentials are never inherited.
pub struct TrustedMetadataContext {
    provider: Provider,
    executable: PathBuf,
    entrypoint: Option<PathBuf>,
    environment: BTreeMap<OsString, OsString>,
    system_proxy_used: bool,
}
impl std::fmt::Debug for TrustedMetadataContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustedMetadataContext")
            .field("provider", &self.provider)
            .field("environment", &"[REDACTED]")
            .finish()
    }
}
const PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];
const NO_PROXY_KEYS: [&str; 2] = ["NO_PROXY", "no_proxy"];
const PROXY_UNSUPPORTED: &str =
    "CLI 元数据查询不支持带凭据或无效的代理配置；请核查已有配置后刷新。";

/// Native execution retains the installed CLI's account routing even when its
/// invocation recipe disables user customizations. Values stay in this child;
/// this never returns credentials to a controller, log or model response.
pub(crate) fn apply_native_account_environment(
    adapter: &webcodex_core::cli_adapter::CliAdapter,
    command: &mut std::process::Command,
) -> Result<(), String> {
    if adapter.metadata != webcodex_core::cli_adapter::CliMetadataRecipe::ClaudeInitialize {
        return Ok(());
    }
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                environment.insert(key.into(), value.into());
            }
            None => {
                environment.remove(key);
            }
        }
    }
    cli_auth::load_claude_settings(&mut environment)
        .map_err(|_| "local_cli_account_configuration_invalid".to_string())?;
    for key in cli_auth::CLAUDE_KEYS {
        if let Some(value) = environment.get(std::ffi::OsStr::new(key)) {
            cli_auth::validate_value(key, value)
                .map_err(|_| "local_cli_account_configuration_invalid".to_string())?;
            command.env(key, value);
        }
    }
    Ok(())
}

impl TrustedMetadataContext {
    /// Only this provider's native account routing, never HOME, hooks, paths,
    /// main-Agent credentials or unrelated environment. Values stay private.
    pub fn account_environment(&self) -> BTreeMap<String, String> {
        let keys: &[&str] = match self.provider {
            Provider::Codex => &["CODEX_API_KEY"],
            Provider::Claude => &[
                "CLAUDE_CODE_OAUTH_TOKEN",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            ],
            _ => &[],
        };
        keys.iter()
            .filter_map(|key| {
                self.environment
                    .get(std::ffi::OsStr::new(key))
                    .and_then(|v| v.to_str())
                    .map(|v| ((*key).into(), v.into()))
            })
            .collect()
    }

    /// Trusted host adapter for an installed CodeBuddy Node package. These paths
    /// are controller-selected, never model arguments or shell command text.
    pub fn for_codebuddy_node(
        node: &Path,
        entrypoint: &Path,
        config_directory: &Path,
    ) -> Result<Self, String> {
        if !entrypoint.is_absolute()
            || !entrypoint.is_file()
            || !config_directory.is_absolute()
            || !config_directory.is_dir()
        {
            return Err("CodeBuddy CLI 安装或配置目录无效。".into());
        }
        let mut context = Self::for_current_user(Provider::WorkBuddy, node, None)?;
        context.entrypoint = Some(entrypoint.into());
        context
            .environment
            .insert("CODEBUDDY_CONFIG_DIR".into(), config_directory.into());
        Ok(context)
    }

    pub fn for_current_user(
        provider: Provider,
        trusted_executable: &Path,
        credential_free_system_proxy: Option<&str>,
    ) -> Result<Self, String> {
        let mut context = Self::from_environment(
            provider,
            trusted_executable,
            credential_free_system_proxy,
            |name| std::env::var_os(name),
        )?;
        if provider == Provider::Claude {
            cli_auth::load_claude_settings(&mut context.environment)?;
        }
        Ok(context)
    }

    fn from_environment(
        provider: Provider,
        executable: &Path,
        system_proxy: Option<&str>,
        mut lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> Result<Self, String> {
        if !executable.is_absolute()
            || executable.parent().is_none()
            || (cfg!(windows)
                && executable
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_none_or(|value| !value.eq_ignore_ascii_case("exe")))
        {
            return Err("CLI 元数据查询需要受信的绝对原生程序路径。".into());
        }
        let mut environment = BTreeMap::new();
        #[cfg(windows)]
        const LOGIN_KEYS: &[&str] = &[
            "SystemRoot",
            "WINDIR",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "HOMEDRIVE",
            "HOMEPATH",
        ];
        #[cfg(not(windows))]
        const LOGIN_KEYS: &[&str] = &[
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
        ];
        for name in LOGIN_KEYS
            .iter()
            .copied()
            .chain(["LANG", "LC_ALL", "LC_CTYPE"])
        {
            if let Some(value) = lookup(name) {
                environment.insert(name.into(), value);
            }
        }
        let native_keys: &[&str] = match provider {
            Provider::Codex => &["CODEX_HOME", "CODEX_API_KEY"],
            Provider::Claude => cli_auth::CLAUDE_KEYS,
            _ => &[],
        };
        for name in native_keys {
            if let Some(value) = lookup(name) {
                cli_auth::validate_value(name, &value)?;
                environment.insert((*name).into(), value);
            }
        }
        let mut paths = vec![executable
            .parent()
            .expect("checked executable parent")
            .to_path_buf()];
        #[cfg(windows)]
        {
            let system = environment
                .get(std::ffi::OsStr::new("SystemRoot"))
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or("CLI 元数据查询缺少有效系统目录。")?;
            paths.push(system.join("System32"));
        }
        #[cfg(not(windows))]
        paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
        environment.insert(
            "PATH".into(),
            std::env::join_paths(paths).map_err(|_| "CLI 元数据系统路径无效。")?,
        );
        let mut explicit_proxy = false;
        for name in PROXY_KEYS {
            if let Some(value) = lookup(name) {
                explicit_proxy = true;
                let text = value.to_str().ok_or(PROXY_UNSUPPORTED)?;
                validate_proxy(text)?;
                environment.insert(name.into(), value);
            }
        }
        for name in NO_PROXY_KEYS {
            if let Some(value) = lookup(name) {
                validate_no_proxy(value.to_str().ok_or(PROXY_UNSUPPORTED)?)?;
                environment.insert(name.into(), value);
            }
        }
        let system_proxy_used = matches!(
            provider,
            Provider::Antigravity | Provider::Codex | Provider::Claude
        ) && !explicit_proxy
            && system_proxy.is_some();
        if system_proxy_used {
            let proxy = system_proxy.expect("checked system proxy");
            validate_proxy(proxy)?;
            environment.insert("HTTP_PROXY".into(), proxy.into());
            environment.insert("HTTPS_PROXY".into(), proxy.into());
        }
        environment.insert("NO_COLOR".into(), "1".into());
        if provider == Provider::Antigravity {
            environment.insert("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into());
        }
        Ok(Self {
            provider,
            executable: executable.into(),
            entrypoint: None,
            environment,
            system_proxy_used,
        })
    }

    fn apply(&self, command: &mut Command, directory: &Path) {
        command
            .env_clear()
            .envs(&self.environment)
            .current_dir(directory);
        // Do not share project scratch files or the parent's temporary directory.
        command
            .env("TEMP", directory)
            .env("TMP", directory)
            .env("TMPDIR", directory);
    }
}

fn validate_proxy(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    if value.len() > 2048
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
        || value.contains('@')
    {
        return Err(PROXY_UNSUPPORTED.into());
    }
    let uri = url::Url::parse(value).map_err(|_| PROXY_UNSUPPORTED)?;
    if !matches!(
        uri.scheme(),
        "http" | "https" | "socks4" | "socks5" | "socks5h"
    ) || uri.host_str().is_none()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.query().is_some()
        || uri.fragment().is_some()
        || !matches!(uri.path(), "" | "/")
    {
        return Err(PROXY_UNSUPPORTED.into());
    }
    Ok(())
}
fn validate_no_proxy(value: &str) -> Result<(), String> {
    if value.len() > 4096 || value.split(',').count() > 128 || value.chars().any(char::is_control) {
        return Err(PROXY_UNSUPPORTED.into());
    }
    for host in value
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
    {
        if host.len() > 256
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:*[]/".contains(&b))
            || host.contains("://")
        {
            return Err(PROXY_UNSUPPORTED.into());
        }
    }
    Ok(())
}

/// Runs only --version, --help and (for Antigravity) models. This is metadata,
/// never a model task or an account entitlement check. The context must have
/// been constructed for this same trusted executable and provider.
pub async fn probe_installed_cli(
    provider: Provider,
    trusted_executable: &Path,
    context: &TrustedMetadataContext,
) -> Result<CliCatalog, String> {
    if context.provider != provider || context.executable != trusted_executable {
        return Err("CLI 元数据上下文与受信程序不一致。".into());
    }
    let mut catalog = CliCatalog::unavailable(provider);
    catalog.installed = true;
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let Ok(Ok(_slot)) = tokio::time::timeout_at(deadline, QUERY_SLOTS.acquire()).await else {
        catalog.status = "error";
        catalog.message = "等待 CLI 查询容量超时，请稍后刷新。".into();
        return Ok(catalog);
    };
    let directory = tempfile::Builder::new()
        .prefix("webcodex-cli-catalog-")
        .tempdir()
        .map_err(|_| "无法创建 CLI 元数据查询临时目录。")?;
    let result = async {
        let version = run_query(provider, trusted_executable, &["--version"], context, directory.path(), deadline).await?;
        catalog.version = parse_version(version.primary_text()).ok_or("无法识别 CLI 版本输出。")?;
        let help = run_query(provider, trusted_executable, &["--help"], context, directory.path(), deadline).await?;
        catalog.efforts = parse_efforts(help.primary_text());
        match provider {
            Provider::WorkBuddy => {
                catalog.models = parse_workbuddy_models(help.primary_text());
                if catalog.models.is_empty() { return Err("CLI 帮助未提供可识别的模型列表。请检查 CLI 版本。"); }
                catalog.source = "installed_help";
                catalog.message = "来自本机 CLI 帮助；尚未验证账号的实际模型权限。".into();
            }
            Provider::Antigravity => {
                let output = run_query(provider, trusted_executable, &["models"], context, directory.path(), deadline).await?;
                catalog.models = parse_antigravity_models(&output.stdout);
                if catalog.models.is_empty() { return Err("模型目录未返回可识别的模型。请检查 CLI 登录与网络后刷新。"); }
                catalog.source = "live_catalog";
                catalog.message = if context.system_proxy_used {
                    "来自 agy CLI 实时模型目录；查询沿用已启用的系统代理，未执行模型任务验证调用权限。"
                } else { "来自 agy CLI 实时模型目录；未执行模型任务验证调用权限。" }.into();
            }
            Provider::Generic => {
                catalog.models = parse_workbuddy_models(help.primary_text());
                catalog.source = "installed_help";
                if catalog.models.is_empty() { return Err("已发现 CLI，但帮助未提供可解析的模型目录；需要对应的能力适配器。"); }
                catalog.message = "来自本机 CLI 帮助；未验证账号或隔离执行能力。".into();
            }
            Provider::Codex | Provider::Claude => {
                let protocol = if provider == Provider::Codex { cli_rpc::Protocol::Codex } else { cli_rpc::Protocol::Claude };
                let output = run_protocol(trusted_executable, protocol.args(), context, directory.path(), deadline, Some(protocol)).await?;
                let choices = cli_rpc::parse_models(&output.stdout)?;
                catalog.models = choices.keys().cloned().collect();
                catalog.efforts = choices.values().flatten().cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                catalog.model_efforts = choices;
                catalog.source = "live_catalog";
                catalog.message = "来自本机 CLI 结构化模型目录；档位按模型区分，目录可能包含缓存，尚未验证账号调用权限。".into();
            }
        }
        Ok(())
    }.await;
    match result {
        Ok(()) => catalog.status = "ready",
        Err(message) => {
            catalog.status = "error";
            catalog.message = message.into();
        }
    }
    catalog.checked_at = timestamp_ms();
    Ok(catalog)
}

pub async fn probe_cli_adapter(
    adapter: &webcodex_core::cli_adapter::CliAdapter,
    executable: &Path,
    context: &TrustedMetadataContext,
) -> Result<CliCatalog, String> {
    adapter.validate()?;
    let mut catalog =
        probe_installed_cli(Provider::for_adapter(adapter), executable, context).await?;
    catalog.provider_id = adapter.id.clone();
    catalog.label = adapter.label.clone();
    Ok(catalog)
}

fn timestamp_ms() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}
fn cleanup_deadline(deadline: Instant) -> Instant {
    let now = Instant::now();
    if now < deadline {
        std::cmp::min(deadline, now + CLEANUP_TIMEOUT)
    } else {
        now + CLEANUP_TIMEOUT
    }
}
fn spawn_options() -> SpawnOptions {
    SpawnOptions {
        #[cfg(windows)]
        windows_creation_flags: windows_sys::Win32::System::Threading::CREATE_NO_WINDOW,
        #[cfg(not(windows))]
        windows_creation_flags: 0,
        windows_silent_child_breakaway: false,
    }
}

fn parse_version(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|word| {
        let version = word.trim_start_matches('v');
        (version.len() <= 48
            && version.contains('.')
            && version.starts_with(|character: char| character.is_ascii_digit())
            && version
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || ".-+".contains(character)))
        .then(|| version.to_string())
    })
}

fn option_text<'a>(help: &'a str, option: &str) -> Option<&'a str> {
    let start = help.lines().find(|line| {
        let line = line.trim_start();
        line.starts_with(option) && line[option.len()..].starts_with(char::is_whitespace)
    })?;
    Some(start)
}

fn parse_workbuddy_models(help: &str) -> Vec<String> {
    let Some(option) = option_text(help, "--model") else {
        return Vec::new();
    };
    let Some((_, supported)) = option.split_once("Currently supported:") else {
        return Vec::new();
    };
    parse_parenthesized_values(supported)
}

fn parse_efforts(help: &str) -> Vec<String> {
    option_text(help, "--effort")
        .map(parse_parenthesized_values)
        .unwrap_or_default()
}

fn parse_parenthesized_values(text: &str) -> Vec<String> {
    let Some((_, rest)) = text.split_once('(') else {
        return Vec::new();
    };
    let Some((values, _)) = rest.split_once(')') else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for value in values.split([',', '|']).map(str::trim) {
        if !valid_model_id(value) || result.len() >= 256 {
            return Vec::new();
        }
        if !result.iter().any(|existing| existing == value) {
            result.push(value.to_string());
        }
    }
    result
}

fn valid_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value.starts_with(|character: char| character.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._:/-".contains(character))
}

// Google documents `agy models` as `slug    Display name`. Accept that
// format (or bare slugs), never infer models from arbitrary diagnostic prose.
// https://antigravity.google/docs/cli/headless/#select-a-model-effort-or-agent
fn parse_antigravity_models(output: &str) -> Vec<String> {
    let mut models = Vec::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if matches!(line, "Available models:" | "Models:") {
            continue;
        }
        let (id, label) = line
            .split_once(char::is_whitespace)
            .map(|(id, label)| (id, label.trim()))
            .unwrap_or((line, ""));
        if !valid_model_id(id)
            || !id.contains(['-', '/', '.'])
            || id.contains(':')
            || id.chars().any(|character| character.is_ascii_uppercase())
            || models.len() >= 256
        {
            return Vec::new();
        }
        if !label.is_empty()
            && (!label.starts_with(|character: char| character.is_ascii_uppercase())
                || label.len() > 200
                || !label.chars().all(|character| {
                    character.is_ascii_alphanumeric() || " ()_.-/+".contains(character)
                }))
        {
            return Vec::new();
        }
        if !models.iter().any(|model| model == id) {
            models.push(id.to_string());
        }
    }
    models
}

type Reader = JoinHandle<Result<Vec<u8>, &'static str>>;

struct QueryOutput {
    stdout: String,
    stderr: String,
}

impl QueryOutput {
    fn primary_text(&self) -> &str {
        if self.stdout.trim().is_empty() {
            &self.stderr
        } else {
            &self.stdout
        }
    }
}

fn query_failure(stderr: &str) -> &'static str {
    let diagnostic = stderr.to_ascii_lowercase();
    if diagnostic.contains("gemini_api_key")
        && (diagnostic.contains("not set") || diagnostic.contains("missing"))
    {
        "agy 已配置 Gemini API 提供商，但查询进程缺少 GEMINI_API_KEY；请检查 CLI 配置与运行环境。"
    } else if diagnostic.contains("keyring")
        && (diagnostic.contains("locked") || diagnostic.contains("denied"))
    {
        "CLI 无法访问本机登录凭据，请检查系统凭据服务后刷新。"
    } else if diagnostic.contains("proxyconnect") || diagnostic.contains("proxy connection") {
        "CLI 无法连接已配置的代理，请检查代理服务后刷新。"
    } else {
        "CLI 查询失败，请检查 CLI 登录、网络与版本。"
    }
}

fn read_bounded(reader: impl Read) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::new();
    reader
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取 CLI 查询输出。")?;
    if bytes.len() > OUTPUT_LIMIT {
        return Err("CLI 查询输出超过大小限制。");
    }
    Ok(bytes)
}

async fn read_result(reader: &mut Reader, deadline: Instant) -> Result<Vec<u8>, &'static str> {
    tokio::time::timeout_at(deadline, reader)
        .await
        .map_err(|_| "CLI 查询超时，请检查网络与登录状态。")?
        .map_err(|_| "CLI 查询读取失败。")?
}

async fn run_query(
    _provider: Provider,
    executable: &Path,
    args: &[&str],
    context: &TrustedMetadataContext,
    directory: &Path,
    deadline: Instant,
) -> Result<QueryOutput, &'static str> {
    run_protocol(executable, args, context, directory, deadline, None).await
}

async fn run_protocol(
    executable: &Path,
    args: &[&str],
    context: &TrustedMetadataContext,
    directory: &Path,
    deadline: Instant,
    protocol: Option<cli_rpc::Protocol>,
) -> Result<QueryOutput, &'static str> {
    if Instant::now() >= deadline {
        return Err("CLI 查询超时，请检查网络与登录状态。");
    }
    let mut command = Command::new(executable);
    if let Some(entrypoint) = &context.entrypoint {
        command.arg(entrypoint);
    }
    command
        .args(args)
        .stdin(if protocol.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The context is private, bounded and does not inherit the host environment.
    context.apply(&mut command, directory);
    let mut child = ManagedChild::spawn_with_options(&mut command, spawn_options())
        .map_err(|_| "无法安全启动 CLI 查询。")?;
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or("无法捕获 CLI 输出。")?;
    let stderr = child
        .child_mut()
        .stderr
        .take()
        .ok_or("无法捕获 CLI 诊断。")?;
    let stdin = child.child_mut().stdin.take();
    let mut stdout = tokio::task::spawn_blocking(move || match protocol {
        Some(protocol) => cli_rpc::exchange(protocol, stdout, stdin.ok_or("CLI 查询输入不可用。")?),
        None => read_bounded(stdout),
    });
    let mut stderr = tokio::task::spawn_blocking(move || read_bounded(stderr));
    let result = async {
        let status = loop {
            if protocol.is_some() && stdout.is_finished() {
                let out = read_result(&mut stdout, deadline).await?;
                // RPC hosts keep running after a response. The common cleanup
                // below terminates and proves exit of the entire owned tree.
                return Ok(QueryOutput {
                    stdout: String::from_utf8_lossy(&out).into_owned(),
                    stderr: String::new(),
                });
            }
            if let Some(status) = child.try_wait().map_err(|_| "无法读取 CLI 查询状态。")?
            {
                break status;
            }
            if Instant::now() >= deadline {
                return Err("CLI 查询超时，请检查网络与登录状态。");
            }
            tokio::time::sleep_until(std::cmp::min(deadline, Instant::now() + POLL_INTERVAL)).await;
        };
        let out = read_result(&mut stdout, deadline).await?;
        let err = read_result(&mut stderr, deadline).await?;
        if !status.success() {
            // CLI diagnostics can contain account data; do not return them.
            return Err(query_failure(&String::from_utf8_lossy(&err)));
        }
        // Successful progress/diagnostics must never become model catalog rows.
        Ok(QueryOutput {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
        })
    }
    .await;
    // Direct-child exit does not prove descendant exit. Reclaim the owned
    // tree even on successful discovery before returning the catalog.
    let cleanup = cleanup_deadline(deadline);
    let _ = child.terminate_tree();
    while !child.try_tree_exit().unwrap_or(false) && Instant::now() < cleanup {
        let _ = child.try_wait();
        tokio::time::sleep_until(std::cmp::min(cleanup, Instant::now() + POLL_INTERVAL)).await;
    }
    let _ = child.try_wait();
    let tree_exited = child.try_tree_exit().unwrap_or(false);
    for reader in [&mut stdout, &mut stderr] {
        if !reader.is_finished() {
            reader.abort();
            let _ = tokio::time::timeout_at(cleanup, reader).await;
        }
    }
    if !tree_exited {
        return Err("CLI 查询进程未能确认完成清理，请稍后重试。");
    }
    result
}

#[path = "cli_catalog_rpc.rs"]
mod cli_rpc;

#[path = "cli_catalog_auth.rs"]
mod cli_auth;

#[cfg(test)]
#[path = "cli_catalog_tests.rs"]
mod tests;
