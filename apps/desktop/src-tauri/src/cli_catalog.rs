//! Desktop-only provider lookup and proxy selection. All fixed queries, bounded
//! process ownership and parsers live in the shared process metadata helper.
use crate::platform;
use std::path::PathBuf;
pub use webcodex_process::cli_catalog::CliCatalog;
use webcodex_process::cli_catalog::{probe_installed_cli, Provider, TrustedMetadataContext};

#[tauri::command]
pub async fn refresh_ai_office_cli(provider_id: String) -> Result<CliCatalog, String> {
    let provider = Provider::parse(&provider_id)?;
    let Some(executable) = find_executable(provider) else {
        return Ok(CliCatalog::unavailable(provider));
    };
    let explicit_proxy = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some());
    let context = match selected_system_proxy(
        provider,
        explicit_proxy,
        platform::system_http_proxy_candidate(),
    )
    .and_then(|system_proxy| {
        TrustedMetadataContext::for_current_user(provider, &executable, system_proxy.as_deref())
    }) {
        Ok(context) => context,
        Err(message) => {
            let mut catalog = CliCatalog::unavailable(provider);
            catalog.installed = true;
            catalog.status = "error";
            catalog.message = message;
            return Ok(catalog);
        }
    };
    probe_installed_cli(provider, &executable, &context).await
}

fn selected_system_proxy(
    provider: Provider,
    explicit_proxy: bool,
    candidate: Option<platform::SystemProxyCandidate>,
) -> Result<Option<String>, String> {
    if provider != Provider::Antigravity || explicit_proxy {
        return Ok(None);
    }
    let Some(candidate) = candidate.filter(|candidate| candidate.enabled) else {
        return Ok(None);
    };
    platform::normalize_proxy_server(&candidate.url)
        .map(Some)
        .ok_or_else(|| {
            "CLI 元数据查询不支持带凭据或无效的系统代理配置；请核查已有配置后刷新。".into()
        })
}

fn find_executable(provider: Provider) -> Option<PathBuf> {
    let mut directories = Vec::new();
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            directories.push(
                PathBuf::from(local)
                    .join(provider.installation_directory())
                    .join("bin"),
            );
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    directories
        .into_iter()
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(provider.executable()))
        .find(|path| path.is_file())
}

#[cfg(test)]
#[path = "cli_catalog_tests.rs"]
mod tests;
