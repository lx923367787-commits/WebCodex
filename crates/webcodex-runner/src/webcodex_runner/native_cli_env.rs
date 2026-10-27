//! Per-process use of the current user's credential-free system proxy.
//! No product/version recognition, global environment writes or login copies.
use std::collections::HashMap;

pub(super) fn environment_overrides(
    environment: &HashMap<String, String>,
    system_proxy: Option<&str>,
) -> Vec<(String, String)> {
    if environment.keys().any(|key| {
        ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name))
    }) {
        return Vec::new();
    }
    let Some(proxy) = system_proxy.and_then(normalize_proxy_server) else {
        return Vec::new();
    };
    vec![
        ("HTTP_PROXY".into(), proxy.clone()),
        ("HTTPS_PROXY".into(), proxy),
    ]
}
fn normalize_proxy_server(value: &str) -> Option<String> {
    if value.len() > 8192 {
        return None;
    }
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    let selected = if value.contains('=') {
        let entries = value
            .split(';')
            .filter_map(|entry| entry.split_once('='))
            .map(|(scheme, target)| (scheme.trim().to_ascii_lowercase(), target.trim()))
            .collect::<Vec<_>>();
        entries
            .iter()
            .find(|(scheme, _)| scheme == "https")
            .or_else(|| entries.iter().find(|(scheme, _)| scheme == "http"))?
            .1
    } else {
        value
    };
    let candidate = if selected.contains("://") {
        selected.to_string()
    } else {
        format!("http://{selected}")
    };
    let parsed = url::Url::parse(&candidate).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return None;
    }
    Some(candidate.trim_end_matches('/').to_string())
}

#[cfg(windows)]
pub(super) fn system_proxy() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };
    let key = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let enabled_name = "ProxyEnable\0".encode_utf16().collect::<Vec<_>>();
    let mut enabled = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: terminated UTF-16 names and a writable DWORD buffer of `size`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            enabled_name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut enabled as *mut u32).cast(),
            &mut size,
        )
    };
    if status != 0 || enabled == 0 {
        return None;
    }
    let name = "ProxyServer\0".encode_utf16().collect::<Vec<_>>();
    let mut buffer = vec![0u16; 8193];
    let mut bytes = (buffer.len() * 2) as u32;
    // SAFETY: bounded writable UTF-16 buffer; RegGetValueW checks its byte size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != 0 || bytes % 2 != 0 || bytes as usize > buffer.len() * 2 {
        return None;
    }
    let text = String::from_utf16(&buffer[..bytes as usize / 2]).ok()?;
    normalize_proxy_server(text.trim_end_matches('\0'))
}

#[cfg(test)]
#[path = "native_cli_env_tests.rs"]
mod tests;
