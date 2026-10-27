//! Desktop-only provider lookup and proxy selection. All fixed queries, bounded
//! process ownership and parsers live in the shared process metadata helper.
use crate::platform;
pub use webcodex_process::cli_catalog::CliCatalog;
use webcodex_process::cli_catalog::{probe_cli_adapter, Provider, TrustedMetadataContext};
use webcodex_process::cli_registry::{discover_cli_installations, CliInstallation};

#[tauri::command]
pub fn discover_ai_office_clis() -> Result<Vec<CliInstallation>, String> {
    discover_cli_installations(&[])
}

#[tauri::command]
pub async fn refresh_ai_office_cli(provider_id: String) -> Result<CliCatalog, String> {
    let adapters = webcodex_process::cli_registry::configured_cli_adapters()?;
    let adapter = adapters.iter().find(|adapter| adapter.id == provider_id).ok_or("CLI adapter is not registered")?;
    let provider = Provider::for_adapter(adapter);
    let Some(installation) = discover_cli_installations(&[])?.into_iter().find(|entry| entry.provider_id == provider_id) else {
        let mut catalog = CliCatalog::unavailable(provider);
        catalog.provider_id = adapter.id.clone(); catalog.label = adapter.label.clone();
        return Ok(catalog);
    };
    if !installation.native { return Err(installation.message.into()); }
    let executable = installation.executable;
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
    probe_cli_adapter(adapter, &executable, &context).await
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

#[cfg(test)]
#[path = "cli_catalog_tests.rs"]
mod tests;
