//! Fixed CodeBuddy transport adapter. Authority and recovery intent belong to
//! the existing per-Job journal, not this OS adapter. No model-provided URL/SID.
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use webcodex_core::runner_protocol::AgentExecutionBinding;

fn validate_proxy(value: &str) -> Result<(), String> {
    if value.len() > 128
        || !value.starts_with("http://")
        || value.contains(['@', '?', '#', '\\', '%'])
    {
        return Err("agent_proxy_unsupported_format".into());
    }
    let authority = value[7..].strip_suffix('/').unwrap_or(&value[7..]);
    let address: SocketAddr = authority
        .parse()
        .map_err(|_| "agent_proxy_numeric_address_required")?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("agent_proxy_numeric_loopback_required".into());
    }
    Ok(())
}

fn preserve_others(
    before: &BTreeMap<String, u32>,
    after: &BTreeMap<String, u32>,
    own: &str,
) -> Result<(), String> {
    if before
        .iter()
        .any(|(sid, attributes)| sid != own && after.get(sid) != Some(attributes))
    {
        return Err("agent_proxy_other_entry_changed".into());
    }
    Ok(())
}

pub(crate) fn read_user_https_proxy() -> Result<String, String> {
    #[cfg(windows)]
    {
        windows::read_proxy()
    }
    #[cfg(not(windows))]
    {
        Err("agent_proxy_platform_unavailable".into())
    }
}

pub(crate) fn require_absent(
    binding: &AgentExecutionBinding,
    deadline: Instant,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let sid = windows::execution_sid(binding)?;
        if windows::snapshot(deadline)?.contains_key(&sid) {
            return Err("agent_proxy_sid_already_exempt".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (binding, deadline);
        Err("agent_proxy_platform_unavailable".into())
    }
}

/// Caller must first durably record this binding's grant intent after proving
/// absence. This function neither creates an owner record nor replays a start.
pub(crate) fn grant(binding: &AgentExecutionBinding, deadline: Instant) -> Result<(), String> {
    #[cfg(windows)]
    {
        let sid = windows::execution_sid(binding)?;
        let before = windows::snapshot(deadline)?;
        windows::change(&sid, true, deadline)?;
        let after = windows::snapshot(deadline)?;
        preserve_others(&before, &after, &sid)?;
        if !after.contains_key(&sid) {
            return Err("agent_proxy_grant_unconfirmed".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (binding, deadline);
        Err("agent_proxy_platform_unavailable".into())
    }
}

/// Only a durable grant-intent owner may call this. Removal is targeted at the
/// SID derived from the immutable binding; never restore a stale whole list.
pub(crate) fn revoke(binding: &AgentExecutionBinding, deadline: Instant) -> Result<(), String> {
    #[cfg(windows)]
    {
        let sid = windows::execution_sid(binding)?;
        // Revoke first. The fixed official -d operation addresses only this
        // SID; an optional/read-only snapshot cannot delay revocation.
        let removed = windows::change(&sid, false, deadline);
        let after = windows::snapshot(deadline)?;
        if after.contains_key(&sid) {
            return Err("agent_proxy_revoke_unconfirmed".into());
        }
        // An already absent entry is an idempotent recovery result, but a helper
        // timeout/unknown stop cannot be hidden by a subsequent snapshot.
        match removed {
            Ok(()) => Ok(()),
            Err(error) if error.starts_with("agent_proxy_control_exit:") => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (binding, deadline);
        Err("agent_proxy_platform_unavailable".into())
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::NetworkManagement::WindowsFirewall::NetworkIsolationGetAppContainerConfig;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{PSID, SID_AND_ATTRIBUTES};
    use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapFree};
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    pub(super) fn read_proxy() -> Result<String, String> {
        let key: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
        let name: Vec<u16> = "HTTPS_PROXY".encode_utf16().chain(Some(0)).collect();
        let mut buffer = [0u16; 256];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        let code = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if code != 0 {
            return Err(format!("agent_proxy_registry_read_failed:{code}"));
        }
        let end = buffer
            .iter()
            .position(|value| *value == 0)
            .ok_or("agent_proxy_value_invalid")?;
        let value = String::from_utf16(&buffer[..end]).map_err(|_| "agent_proxy_value_invalid")?;
        validate_proxy(&value)?;
        Ok(value)
    }

    fn sid_text(sid: PSID) -> Result<String, String> {
        let mut text = null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err("agent_proxy_sid_encoding_failed".into());
        }
        let mut length = 0;
        unsafe {
            while *text.add(length) != 0 {
                length += 1;
            }
        }
        let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| "agent_proxy_sid_encoding_failed".into());
        unsafe {
            LocalFree(text.cast());
        }
        result
    }

    pub(super) fn execution_sid(binding: &AgentExecutionBinding) -> Result<String, String> {
        binding.validate()?;
        let identity = binding.identity_hash()?;
        let sid = webcodex_process::windows_lpac::AppContainerSid::derive(&identity)
            .map_err(|_| "agent_proxy_sid_derivation_failed")?;
        sid_text(sid.as_ptr())
    }

    struct SnapshotBuffer {
        count: u32,
        entries: *mut SID_AND_ATTRIBUTES,
    }
    impl Drop for SnapshotBuffer {
        fn drop(&mut self) {
            unsafe {
                if !self.entries.is_null() {
                    for i in 0..self.count {
                        HeapFree(GetProcessHeap(), 0, (*self.entries.add(i as usize)).Sid);
                    }
                    HeapFree(GetProcessHeap(), 0, self.entries.cast());
                }
            }
        }
    }
    fn read_snapshot() -> Result<BTreeMap<String, u32>, String> {
        let mut buffer = SnapshotBuffer {
            count: 0,
            entries: null_mut(),
        };
        let code = unsafe {
            NetworkIsolationGetAppContainerConfig(&mut buffer.count, &mut buffer.entries)
        };
        if code != 0 {
            return Err(format!("agent_proxy_snapshot_failed:{code}"));
        }
        if buffer.count > 16384 || (buffer.count > 0 && buffer.entries.is_null()) {
            return Err("agent_proxy_snapshot_limit".into());
        }
        let mut result = BTreeMap::new();
        for i in 0..buffer.count {
            let item = unsafe { &*buffer.entries.add(i as usize) };
            if result
                .insert(sid_text(item.Sid)?, item.Attributes)
                .is_some()
            {
                return Err("agent_proxy_duplicate_entry".into());
            }
        }
        Ok(result)
    }
    pub(super) fn snapshot(deadline: Instant) -> Result<BTreeMap<String, u32>, String> {
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(1500));
        if wait.is_zero() {
            return Err("agent_proxy_deadline".into());
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        // A timed-out worker can finish only this read, never a deferred write.
        std::thread::Builder::new()
            .name("agent-proxy-snapshot".into())
            .spawn(move || {
                let _ = sender.send(read_snapshot());
            })
            .map_err(|_| "agent_proxy_snapshot_worker_failed")?;
        receiver
            .recv_timeout(wait)
            .map_err(|_| "agent_proxy_snapshot_deadline".to_owned())?
    }
    fn system_directory() -> Result<PathBuf, String> {
        let mut text = vec![0u16; 32768];
        let length = unsafe { GetWindowsDirectoryW(text.as_mut_ptr(), text.len() as u32) };
        if length == 0 || length as usize >= text.len() {
            return Err("agent_proxy_windows_directory_failed".into());
        }
        Ok(PathBuf::from(
            String::from_utf16(&text[..length as usize])
                .map_err(|_| "agent_proxy_windows_directory_failed")?,
        )
        .join("System32"))
    }
    pub(super) fn change(sid: &str, add: bool, deadline: Instant) -> Result<(), String> {
        let end = deadline.min(Instant::now() + Duration::from_secs(4));
        if end.saturating_duration_since(Instant::now()) < Duration::from_millis(500) {
            return Err("agent_proxy_deadline".into());
        }
        let directory = system_directory()?;
        let mut command = Command::new(directory.join("CheckNetIsolation.exe"));
        command
            .args([
                "LoopbackExempt",
                if add { "-a" } else { "-d" },
                &format!("-p={sid}"),
            ])
            .current_dir(&directory)
            .env_clear()
            .env("SystemRoot", directory.parent().unwrap())
            .env("WINDIR", directory.parent().unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = webcodex_process::ManagedChild::spawn_with_options(
            &mut command,
            webcodex_process::SpawnOptions {
                windows_creation_flags: CREATE_NO_WINDOW,
                ..Default::default()
            },
        )
        .map_err(|_| "agent_proxy_control_spawn_failed")?;
        let wait_end = end.checked_sub(Duration::from_millis(500)).unwrap_or(end);
        let outcome = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    break if status.success() {
                        Ok(())
                    } else {
                        Err(format!(
                            "agent_proxy_control_exit:{}",
                            status.code().unwrap_or(-1)
                        ))
                    }
                }
                Ok(None) => {}
                Err(_) => break Err("agent_proxy_control_wait_failed".into()),
            }
            if Instant::now() >= wait_end {
                break Err("agent_proxy_control_deadline".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let _ = child.terminate_tree();
        if !child
            .wait_tree_exit(end.saturating_duration_since(Instant::now()))
            .unwrap_or(false)
        {
            return Err("agent_proxy_control_stop_unconfirmed".into());
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_proxy_rejects_credential_or_nonloopback_routes() {
        for value in ["http://127.0.0.1:8080", "http://[::1]:8080/"] {
            assert!(validate_proxy(value).is_ok());
        }
        for value in [
            "http://user:secret@127.0.0.1:8080",
            "http://localhost:8080",
            "http://192.0.2.1:8080",
            "https://127.0.0.1:8080",
            "http://127.0.0.1:0",
            "http://127.0.0.1:8080/?token=secret",
        ] {
            let error = validate_proxy(value).unwrap_err();
            assert!(!error.contains(value));
        }
    }
    #[test]
    fn production_proxy_never_accepts_loss_or_mutation_of_other_entries() {
        let before = BTreeMap::from([("other".into(), 7), ("ours".into(), 4)]);
        assert!(preserve_others(&before, &BTreeMap::from([("other".into(), 7)]), "ours").is_ok());
        assert!(preserve_others(&before, &BTreeMap::from([("other".into(), 3)]), "ours").is_err());
        assert!(preserve_others(&before, &BTreeMap::new(), "ours").is_err());
    }
}
