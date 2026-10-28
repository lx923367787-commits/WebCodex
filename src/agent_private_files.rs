//! Trusted-server broker for agent-private files. The caller authenticates and
//! revalidates `VerifiedAgentRun` in the store before every operation. No model
//! receives the host root or chooses the owner. This is an API ownership check,
//! not an OS boundary against arbitrary programs running under the server account.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use webcodex_store::VerifiedAgentRun;

const AREAS: [&str; 8] = [
    "identity",
    "goals",
    "tasks",
    "memory",
    "checkpoints",
    "handoff",
    "workspace",
    "cli_sessions",
];
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_SCAN_BYTES: usize = 16 * MAX_FILE_BYTES;
static FILE_OPERATIONS: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize)]
pub(crate) struct PrivateFileSnapshot {
    pub path: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct PrivateFileEntry {
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct PrivateSearchMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
}

pub(crate) struct AgentPrivateFiles {
    root: PathBuf,
}

fn failure(_: impl std::fmt::Display) -> String {
    // OS errors can contain absolute host paths. Keep them out of model results.
    "agent_private_files_unavailable".to_string()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn component_valid(value: &str) -> bool {
    webcodex_core::agent_private_path::component_valid(value)
}

fn identity_component_valid(value: &str) -> bool {
    component_valid(value)
        && !value.starts_with('.')
        && !value.bytes().any(|b| b.is_ascii_uppercase())
}

fn staging_name(value: &str) -> bool {
    value.strip_prefix(".snapshot-").is_some_and(|id| {
        let bytes = id.as_bytes();
        // This is exactly the lowercase simple UUID-v4 form generated below.
        bytes.len() == 32
            && bytes
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            && bytes[12] == b'4'
            && matches!(bytes[16], b'8' | b'9' | b'a' | b'b')
    })
}

fn relative_parts(path: &str, file: bool) -> Result<Vec<&str>, String> {
    if path.len() > 1024 {
        return Err("agent_private_path_invalid".into());
    }
    if path.is_empty() && !file {
        return Ok(Vec::new());
    }
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() > 16
        || (file && parts.len() < 2)
        || !parts.iter().all(|part| component_valid(part))
        || !AREAS.contains(&parts[0])
    {
        return Err("agent_private_path_invalid".into());
    }
    Ok(parts)
}

impl AgentPrivateFiles {
    /// `root` is the trusted Server state parent, never a model argument.
    pub(crate) fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        if !root.is_absolute()
            || root
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err("agent_private_root_invalid".into());
        }
        #[cfg(windows)]
        if !matches!(root.components().next(), Some(Component::Prefix(p))
            if matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
        {
            return Err("agent_private_root_invalid".into());
        }
        Ok(Self { root })
    }

    fn private_path(&self, binding: &VerifiedAgentRun) -> Result<PathBuf, String> {
        if binding.project().is_empty() || !identity_component_valid(binding.agent_id()) {
            return Err("agent_private_binding_invalid".into());
        }
        Ok(self
            .root
            .join("runtime")
            .join("projects")
            .join(digest(binding.project().as_bytes()))
            .join("agents")
            .join(binding.agent_id())
            .join("private"))
    }

    pub(crate) fn create(&self, binding: &VerifiedAgentRun) -> Result<(), String> {
        let _lock = FILE_OPERATIONS.lock().map_err(failure)?;
        let root = self.private_path(binding)?;
        for area in AREAS {
            Directory::open(&root.join(area), true)?;
        }
        Ok(())
    }

    pub(crate) fn write(
        &self,
        binding: &VerifiedAgentRun,
        path: &str,
        bytes: &[u8],
        expected_sha256: Option<&str>,
    ) -> Result<PrivateFileSnapshot, String> {
        let parts = relative_parts(path, true)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("agent_private_file_too_large".into());
        }
        if expected_sha256.is_some_and(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err("agent_private_hash_invalid".into());
        }
        let _lock = FILE_OPERATIONS.lock().map_err(failure)?;
        let mut parent = self.private_path(binding)?;
        for part in &parts[..parts.len() - 1] {
            parent.push(part);
        }
        let mut directory = Directory::open(&parent, true)?;
        let name = parts[parts.len() - 1];
        match directory.read_optional(name)? {
            Some(previous) if expected_sha256 == Some(digest(&previous).as_str()) => {}
            None if expected_sha256.is_none() => {}
            _ => return Err("agent_private_hash_conflict".into()),
        }
        directory.publish(name, bytes, expected_sha256.is_some())?;
        let snapshot = snapshot(
            path,
            directory
                .read(name)
                .map_err(|_| "agent_private_publish_uncertain")?,
        );
        if snapshot.bytes != bytes {
            return Err("agent_private_publish_uncertain".into());
        }
        Ok(snapshot)
    }

    pub(crate) fn read(
        &self,
        binding: &VerifiedAgentRun,
        path: &str,
    ) -> Result<PrivateFileSnapshot, String> {
        let parts = relative_parts(path, true)?;
        let _lock = FILE_OPERATIONS.lock().map_err(failure)?;
        let mut parent = self.private_path(binding)?;
        for part in &parts[..parts.len() - 1] {
            parent.push(part);
        }
        let directory = Directory::open(&parent, false)?;
        Ok(snapshot(path, directory.read(parts[parts.len() - 1])?))
    }

    /// Lists files recursively within a private area. Empty prefix selects only
    /// the eight private areas. Limits are hard bounds, not silent truncation.
    pub(crate) fn list(
        &self,
        binding: &VerifiedAgentRun,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<PrivateFileEntry>, String> {
        Ok(self
            .scan(binding, prefix, limit)?
            .into_iter()
            .map(|file| PrivateFileEntry {
                path: file.path,
                bytes: file.bytes.len(),
                sha256: file.sha256,
            })
            .collect())
    }

    pub(crate) fn search(
        &self,
        binding: &VerifiedAgentRun,
        prefix: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<PrivateSearchMatch>, String> {
        if query.is_empty() || query.len() > 256 || !(1..=128).contains(&limit) {
            return Err("agent_private_search_invalid".into());
        }
        let files = self.scan(binding, prefix, 128)?;
        let mut matches = Vec::new();
        for file in files {
            let text = std::str::from_utf8(&file.bytes)
                .map_err(|_| "agent_private_search_requires_utf8")?;
            for (index, line) in text.lines().enumerate() {
                if line.contains(query) {
                    if matches.len() == limit {
                        return Err("agent_private_result_limit".into());
                    }
                    matches.push(PrivateSearchMatch {
                        path: file.path.clone(),
                        line: index + 1,
                        text: line.chars().take(1024).collect(),
                    });
                }
            }
        }
        Ok(matches)
    }

    fn scan(
        &self,
        binding: &VerifiedAgentRun,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<PrivateFileSnapshot>, String> {
        relative_parts(prefix, false)?;
        if !(1..=128).contains(&limit) {
            return Err("agent_private_result_limit".into());
        }
        let _lock = FILE_OPERATIONS.lock().map_err(failure)?;
        let root = self.private_path(binding)?;
        let mut pending = VecDeque::from([prefix.to_string()]);
        let mut files = Vec::new();
        let (mut visited, mut total_bytes) = (0, 0);
        while let Some(relative) = pending.pop_front() {
            let directory = Directory::open(&root.join(&relative), false)?;
            for (name, is_directory) in directory.entries()? {
                visited += 1;
                if visited > MAX_ENTRIES {
                    return Err("agent_private_scan_limit".into());
                }
                let child = if relative.is_empty() {
                    name.clone()
                } else {
                    format!("{relative}/{name}")
                };
                relative_parts(&child, !is_directory)?;
                if is_directory {
                    pending.push_back(child);
                } else {
                    if files.len() == limit {
                        return Err("agent_private_result_limit".into());
                    }
                    let bytes = directory.read(&name)?;
                    total_bytes += bytes.len();
                    if total_bytes > MAX_SCAN_BYTES {
                        return Err("agent_private_scan_limit".into());
                    }
                    files.push(snapshot(&child, bytes));
                }
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }
}

fn snapshot(path: &str, bytes: Vec<u8>) -> PrivateFileSnapshot {
    PrivateFileSnapshot {
        path: path.to_string(),
        sha256: digest(&bytes),
        bytes,
    }
}

fn plain(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

/// Windows: pins every ancestor without sharing write/delete, preventing a
/// validated directory from becoming a junction or being replaced during I/O.
/// Unix: every child is opened relative to its pinned parent with O_NOFOLLOW;
/// file I/O and publication use that directory descriptor, never a joined path.
struct Directory {
    path: PathBuf,
    handles: Vec<File>,
}

impl Directory {
    fn open(path: &Path, create: bool) -> Result<Self, String> {
        let mut current = PathBuf::new();
        let mut handles = Vec::new();
        for component in path.components() {
            current.push(component.as_os_str());
            if matches!(component, Component::Prefix(_)) {
                continue;
            }
            if !matches!(component, Component::RootDir | Component::Normal(_)) {
                return Err("agent_private_path_invalid".into());
            }
            #[cfg(windows)]
            let file = {
                use std::os::windows::fs::OpenOptionsExt;
                if create && !current.try_exists().map_err(failure)? {
                    match fs::create_dir(&current) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(failure(e)),
                    }
                }
                OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .custom_flags(0x02000000 | 0x00200000)
                    .open(&current)
                    .map_err(failure)?
            };
            #[cfg(unix)]
            let file = {
                use std::os::fd::{AsRawFd, FromRawFd};
                use std::os::unix::ffi::OsStrExt;
                let name =
                    std::ffi::CString::new(component.as_os_str().as_bytes()).map_err(failure)?;
                let parent = handles
                    .last()
                    .map_or(libc::AT_FDCWD, |f: &File| f.as_raw_fd());
                if create && matches!(component, Component::Normal(_)) {
                    let result = unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) };
                    if result != 0
                        && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                    {
                        return Err(failure(std::io::Error::last_os_error()));
                    }
                }
                let fd = unsafe {
                    libc::openat(
                        parent,
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    )
                };
                if fd < 0 {
                    return Err(failure(std::io::Error::last_os_error()));
                }
                unsafe { File::from_raw_fd(fd) }
            };
            let metadata = file.metadata().map_err(failure)?;
            if !metadata.is_dir() || !plain(&metadata) {
                return Err("agent_private_link_rejected".into());
            }
            handles.push(file);
        }
        if handles.is_empty() {
            return Err("agent_private_root_invalid".into());
        }
        Ok(Self {
            path: current,
            handles,
        })
    }

    fn open_file(&self, name: &str, create: bool) -> Result<File, std::io::Error> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .write(create)
                .create_new(create)
                // DELETE is required for handle-relative atomic publication.
                .access_mode(if create { 0xc0010000 } else { 0x80000000 })
                .share_mode(1)
                .custom_flags(0x00200000)
                .open(self.path.join(name))
        }
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name =
                std::ffi::CString::new(name).map_err(|_| std::io::ErrorKind::InvalidInput)?;
            let flags = libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | if create {
                    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL
                } else {
                    libc::O_RDONLY
                };
            let fd = unsafe {
                libc::openat(
                    self.handles.last().unwrap().as_raw_fd(),
                    name.as_ptr(),
                    flags,
                    0o600,
                )
            };
            if fd < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(unsafe { File::from_raw_fd(fd) })
            }
        }
    }

    fn read_optional(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let file = match self.open_file(name, false) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(failure(e)),
        };
        verify_file(&file)?;
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(failure)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("agent_private_file_too_large".into());
        }
        Ok(Some(bytes))
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        self.read_optional(name)?
            .ok_or_else(|| "agent_private_file_not_found".into())
    }

    fn entries(&self) -> Result<Vec<(String, bool)>, String> {
        #[cfg(windows)]
        let directory = fs::read_dir(&self.path).map_err(failure)?;
        #[cfg(unix)]
        let directory = {
            use std::os::fd::AsRawFd;
            // /proc traversal refers to our pinned descriptor, not a mutable
            // user-selected path; systems without procfs fail closed.
            fs::read_dir(format!(
                "/proc/self/fd/{}",
                self.handles.last().unwrap().as_raw_fd()
            ))
            .map_err(failure)?
        };
        let mut entries = Vec::new();
        for (index, entry) in directory.enumerate() {
            if index == MAX_ENTRIES {
                return Err("agent_private_scan_limit".into());
            }
            let entry = entry.map_err(failure)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "agent_private_path_invalid")?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(failure)?;
            if !plain(&metadata) || (!metadata.is_file() && !metadata.is_dir()) {
                return Err("agent_private_link_rejected".into());
            }
            if metadata.is_file() && staging_name(&name) {
                // An interrupted publication can leave this private staging
                // name. Verify metadata (including link count), never read its
                // contents, expose it, or delete recovery evidence here.
                verify_file(&self.open_file(&name, false).map_err(failure)?)?;
                continue;
            }
            if !component_valid(&name) {
                return Err("agent_private_path_invalid".into());
            }
            entries.push((name, metadata.is_dir()));
        }
        entries.sort();
        Ok(entries)
    }

    fn publish(&mut self, name: &str, bytes: &[u8], replace: bool) -> Result<(), String> {
        let temporary = format!(".snapshot-{}", uuid::Uuid::new_v4().simple());
        let mut file = self.open_file(&temporary, true).map_err(failure)?;
        let prepared = file.write_all(bytes).and_then(|_| file.sync_all());
        if let Err(error) = prepared {
            drop(file);
            self.unlink(&temporary);
            return Err(failure(error));
        }
        #[cfg(windows)]
        let result = self.publish_windows(&file, name, replace);
        #[cfg(unix)]
        let result = {
            use std::os::fd::AsRawFd;
            let parent = self.handles.last().unwrap().as_raw_fd();
            let source = std::ffi::CString::new(temporary.as_str()).unwrap();
            let destination = std::ffi::CString::new(name).unwrap();
            let outcome = if replace {
                unsafe { libc::renameat(parent, source.as_ptr(), parent, destination.as_ptr()) }
            } else {
                // linkat publishes without replacement; immediately remove the
                // staging name, leaving the published file with one hard link.
                unsafe { libc::linkat(parent, source.as_ptr(), parent, destination.as_ptr(), 0) }
            };
            if outcome != 0 {
                Err(std::io::Error::last_os_error())
            } else {
                if !replace {
                    self.unlink(&temporary);
                }
                self.handles.last().unwrap().sync_all()
            }
        };
        drop(file);
        if result.is_err() {
            // On Windows, a failed restoration of the leaf's no-write pin may
            // mean its reparse data changed. Never clean up through that path.
            #[cfg(unix)]
            self.unlink(&temporary);
            // The publication syscall was attempted. Preserve uncertainty even
            // if an OS error was returned; callers must re-read before retrying.
            return Err("agent_private_publish_uncertain".into());
        }
        Ok(())
    }

    #[cfg(windows)]
    fn publish_windows(&mut self, file: &File, name: &str, replace: bool) -> std::io::Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;

        #[repr(C)]
        struct IoStatus {
            status: usize,
            information: usize,
        }
        #[repr(C)]
        struct Rename {
            replace: u8,
            root: HANDLE,
            length: u32,
            name: [u16; 128],
        }
        #[link(name = "ntdll")]
        extern "system" {
            fn NtSetInformationFile(
                file: HANDLE,
                status: *mut IoStatus,
                information: *const core::ffi::c_void,
                length: u32,
                class: u32,
            ) -> i32;
        }

        let open_leaf = |share| {
            let handle = OpenOptions::new()
                .read(true)
                .share_mode(share)
                .custom_flags(0x02000000 | 0x00200000)
                .open(&self.path)?;
            let metadata = handle.metadata()?;
            if !metadata.is_dir() || !plain(&metadata) {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
            }
            Ok(handle)
        };
        // Windows rename requires write sharing on the containing directory.
        // First acquire a second no-delete pin, then release the no-write pin:
        // there is never an unpinned namespace window. All ancestors stay
        // no-write/no-delete. During this interval use only the already-open
        // source file and the directory handle, never a path-based rename.
        let writable_leaf = open_leaf(3)?;
        let last = self.handles.len() - 1;
        drop(std::mem::replace(&mut self.handles[last], writable_leaf));
        let mut rename = Rename {
            replace: u8::from(replace),
            root: self.handles[last].as_raw_handle(),
            length: (name.len() * 2) as u32,
            name: [0; 128],
        };
        for (slot, code_unit) in rename.name.iter_mut().zip(name.encode_utf16()) {
            *slot = code_unit;
        }
        let mut status = IoStatus {
            status: 0,
            information: 0,
        };
        // FileRenameInformation (10) accepts a handle-relative destination.
        // The Win32 FileRenameInfo wrapper instead requires a full path.
        let outcome = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &mut status,
                &rename as *const Rename as *const core::ffi::c_void,
                (std::mem::offset_of!(Rename, name) + name.len() * 2).max(std::mem::size_of::<
                    windows_sys::Win32::Storage::FileSystem::FILE_RENAME_INFO,
                >()) as u32,
                10,
            )
        };
        // Re-establish the no-write pin before any subsequent path operation;
        // OPEN_REPARSE_POINT and metadata verification fail closed on mutation.
        let pinned_leaf = open_leaf(1)?;
        drop(std::mem::replace(&mut self.handles[last], pinned_leaf));
        if outcome < 0 {
            Err(std::io::Error::from(std::io::ErrorKind::Other))
        } else {
            file.sync_all()
        }
    }

    fn unlink(&self, name: &str) {
        #[cfg(windows)]
        {
            let _ = fs::remove_file(self.path.join(name));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(name).unwrap();
            unsafe {
                libc::unlinkat(self.handles.last().unwrap().as_raw_fd(), name.as_ptr(), 0);
            }
        }
    }
}

fn verify_file(file: &File) -> Result<(), String> {
    let metadata = file.metadata().map_err(failure)?;
    if !metadata.is_file() || !plain(&metadata) {
        return Err("agent_private_link_rejected".into());
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err("agent_private_file_too_large".into());
    }
    #[cfg(windows)]
    let links = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(failure(std::io::Error::last_os_error()));
        }
        u64::from(info.nNumberOfLinks)
    };
    #[cfg(unix)]
    let links = {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink()
    };
    if links != 1 {
        return Err("agent_private_hardlink_rejected".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "agent_private_files_tests.rs"]
mod tests;
