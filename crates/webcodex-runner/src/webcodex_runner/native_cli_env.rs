//! Windows defaults for the two fixed native team CLI installations only.
//! No shell, process spawn, global environment mutation, or config writes.
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeCli {
    WorkBuddy,
    Antigravity,
}

impl NativeCli {
    fn alias(self) -> &'static str {
        match self {
            Self::WorkBuddy => "codebuddy",
            Self::Antigravity => "agy",
        }
    }
    fn executable(self) -> &'static str {
        match self {
            Self::WorkBuddy => "codebuddy.exe",
            Self::Antigravity => "agy.exe",
        }
    }
}

fn valid_selection(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn supported_args(cli: NativeCli, args: &[String]) -> bool {
    if args == ["--version"] || args == ["--help"] {
        return true;
    }
    if cli == NativeCli::Antigravity && args == ["models"] {
        return true;
    }
    let [model_flag, model, effort_flag, effort, tail @ ..] = args else {
        return false;
    };
    if model_flag != "--model" || effort_flag != "--effort" || !valid_selection(model) {
        return false;
    }
    let common_effort = matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max");
    match cli {
        NativeCli::WorkBuddy => {
            (common_effort || effort == "minimal")
                && tail
                    == [
                        "--print",
                        "--input-format",
                        "text",
                        "--output-format",
                        "json",
                    ]
        }
        NativeCli::Antigravity => {
            common_effort
                && tail
                    == [
                        "--input-format",
                        "stream-json",
                        "--output-format",
                        "stream-json",
                    ]
        }
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

fn fixed_binary(cli: NativeCli, local_appdata: &OsStr) -> Option<PathBuf> {
    let local = Path::new(local_appdata);
    if !local.is_absolute() {
        return None;
    }
    let local = local.canonicalize().ok()?;
    let bin = local.join(cli.alias()).join("bin").canonicalize().ok()?;
    // A directory junction or executable symlink must not expand the default
    // search outside the current user's fixed installation directory.
    if !bin.starts_with(&local) {
        return None;
    }
    let executable = bin.join(cli.executable()).canonicalize().ok()?;
    if !executable.is_file()
        || !executable
            .parent()
            .is_some_and(|parent| same_path(parent, &bin))
        || !executable.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .eq_ignore_ascii_case(cli.executable())
        })
    {
        return None;
    }
    Some(executable)
}

/// Called only after the normal PATH lookup failed, and only when the caller
/// has ruled out isolated/prepared/explicit-PATH environments.
pub(super) fn fallback_program(
    program: &str,
    args: &[String],
    local_appdata: Option<&OsStr>,
) -> Option<PathBuf> {
    let cli = match program.to_ascii_lowercase().as_str() {
        "codebuddy" => NativeCli::WorkBuddy,
        "agy" => NativeCli::Antigravity,
        _ => return None,
    };
    if !supported_args(cli, args) {
        return None;
    }
    fixed_binary(cli, local_appdata?)
}

/// A same-named executable in a project/PATH receives no extra environment.
/// Even bare aliases must resolve to the exact canonical fixed installation.
pub(super) fn recognized_program(
    program: &str,
    args: &[String],
    resolved: &OsStr,
    local_appdata: Option<&OsStr>,
) -> Option<NativeCli> {
    let requested = Path::new(program);
    for cli in [NativeCli::WorkBuddy, NativeCli::Antigravity] {
        let alias = program.eq_ignore_ascii_case(cli.alias());
        let absolute_native = requested.is_absolute()
            && requested.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .eq_ignore_ascii_case(cli.executable())
            });
        if !alias && !absolute_native {
            continue;
        }
        if !supported_args(cli, args) {
            continue;
        }
        let Some(fixed) = local_appdata.and_then(|local| fixed_binary(cli, local)) else {
            continue;
        };
        let resolved = Path::new(resolved).canonicalize().ok()?;
        if !same_path(&resolved, &fixed) {
            continue;
        }
        if alias {
            return Some(cli);
        }
        if requested.is_absolute()
            && requested
                .canonicalize()
                .ok()
                .is_some_and(|path| same_path(&path, &fixed))
        {
            return Some(cli);
        }
    }
    None
}

pub(super) fn environment_overrides(
    cli: NativeCli,
    environment: &HashMap<String, String>,
    system_proxy: Option<&str>,
) -> Vec<(String, String)> {
    if cli != NativeCli::Antigravity {
        return Vec::new();
    }
    let mut result = vec![("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into())];
    let explicit = environment.keys().any(|key| {
        ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name))
    });
    if !explicit {
        if let Some(proxy) = system_proxy.and_then(normalize_proxy_server) {
            result.extend([
                ("HTTP_PROXY".into(), proxy.clone()),
                ("HTTPS_PROXY".into(), proxy),
            ]);
        }
    }
    result
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
