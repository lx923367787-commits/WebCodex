//! Resolve installed CLIs using the Runner user's environment, never a product
//! version/hash allowlist or a copied login. Resolve again for each invocation so
//! an installer's stable launcher may move to a newer version without a rebuild.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use webcodex_core::local_cli::{InstalledCli, LocalCliInventory, LocalCliQuery};

fn command_name(command: &str) -> Option<&str> {
    if command.is_empty()
        || command.len() > 256
        || command.chars().any(char::is_control)
        || command.contains(['/', '\\', ':'])
        || command == "."
        || command == ".."
    {
        return None;
    }
    Some(command)
}

fn executable_candidates(directory: &Path, command: &str) -> Vec<PathBuf> {
    let exact = directory.join(command);
    #[cfg(windows)]
    {
        if Path::new(command).extension().is_some() {
            return vec![exact];
        }
        ["exe", "com", "cmd", "bat"]
            .iter()
            .map(|extension| directory.join(format!("{command}.{extension}")))
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![exact]
    }
}

fn executable_file(path: &Path) -> Option<PathBuf> {
    let actual = path.canonicalize().ok()?;
    let metadata = actual.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    #[cfg(windows)]
    if !actual
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            ["exe", "com", "cmd", "bat"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
    {
        return None;
    }
    Some(actual)
}

/// Search only absolute installation directories. Empty/relative PATH entries
/// and the task's working directory are not executable discovery locations.
pub(crate) fn resolve_in_directories(command: &str, directories: &[PathBuf]) -> Option<PathBuf> {
    let requested = Path::new(command);
    if requested.is_absolute() {
        return executable_file(requested);
    }
    command_name(command)?;
    directories
        .iter()
        .filter(|directory| directory.is_absolute())
        .flat_map(|directory| executable_candidates(directory, command))
        .find_map(|path| executable_file(&path))
}

pub(crate) fn installation_directories(command: &str) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    #[cfg(windows)]
    for path in windows_user_paths() {
        directories.extend(std::env::split_paths(&path));
    }
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        directories.push(Path::new(&home).join(".local/bin"));
        directories.push(Path::new(&home).join(".cargo/bin"));
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            directories.push(Path::new(&appdata).join("npm"));
        }
        if let (Some(local), Some(name)) = (std::env::var_os("LOCALAPPDATA"), command_name(command))
        {
            let stem = Path::new(name)
                .file_stem()
                .unwrap_or_else(|| OsStr::new(name));
            directories.push(Path::new(&local).join(stem).join("bin"));
            directories.push(Path::new(&local).join("Programs").join(stem).join("bin"));
        }
    }
    directories
}

pub(crate) fn resolve_installed(command: &str) -> Option<PathBuf> {
    resolve_in_directories(command, &installation_directories(command))
}

pub(crate) fn discover(query: &LocalCliQuery) -> Result<LocalCliInventory, String> {
    query.validate()?;
    Ok(LocalCliInventory {
        execution_mode: "runner_user".into(),
        programs: query
            .commands
            .iter()
            .map(|command| {
                let executable =
                    resolve_installed(command).and_then(|path| path.to_str().map(str::to_owned));
                InstalledCli {
                    command: command.clone(),
                    reason_code: executable
                        .is_none()
                        .then(|| "local_cli_not_installed_or_not_executable".into()),
                    executable,
                }
            })
            .collect(),
    })
}

#[cfg(windows)]
fn windows_user_paths() -> Vec<OsString> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };
    let mut paths = Vec::new();
    for (hive, key) in [
        (
            HKEY_LOCAL_MACHINE,
            "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
        ),
        (HKEY_CURRENT_USER, "Environment"),
    ] {
        let key: Vec<u16> = key.encode_utf16().chain([0]).collect();
        let name: Vec<u16> = "Path".encode_utf16().chain([0]).collect();
        let mut buffer = vec![0u16; 32768];
        let mut bytes = (buffer.len() * 2) as u32;
        // RegGetValue expands REG_EXPAND_SZ using the current user's environment.
        let status = unsafe {
            RegGetValueW(
                hive,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status == 0 && bytes % 2 == 0 && bytes as usize <= buffer.len() * 2 {
            if let Ok(path) = String::from_utf16(&buffer[..bytes as usize / 2]) {
                paths.push(OsString::from(path.trim_end_matches('\0')));
            }
        }
    }
    paths
}

#[cfg(test)]
#[path = "installed_cli_tests.rs"]
mod tests;
