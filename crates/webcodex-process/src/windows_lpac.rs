//! Strict LPAC launcher. No profile creation, ACL mutation, login, shell parsing
//! or unsandboxed fallback. The trusted caller prepares the exact read/write
//! resources for this per-run SID and supplies a complete credential-free env.
//! Lifecycle mirrors ManagedChild: atomic job assignment, kill-on-close, no
//! breakaway, explicit tree termination, and a separate whole-tree wait.
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::Isolation::DeriveAppContainerSidFromAppContainerName;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::LibraryLoader::*;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::*;

// winnt.h: opt out of the broad ALL APPLICATION PACKAGES access grant.
const PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 1;
const SE_GROUP_ENABLED: u32 = 0x4;

#[derive(Debug)]
pub struct SpawnFailure {
    pub code: &'static str,
    pub os_code: u32,
    pub execution_state: &'static str,
}
impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.code, self.os_code, self.execution_state)
    }
}
impl std::error::Error for SpawnFailure {}
fn fail(code: &'static str) -> SpawnFailure {
    SpawnFailure {
        code,
        os_code: unsafe { GetLastError() },
        execution_state: "not_started",
    }
}
fn invalid(code: &'static str) -> SpawnFailure {
    SpawnFailure {
        code,
        os_code: 0,
        execution_state: "not_started",
    }
}
fn wide(value: &OsStr) -> Result<Vec<u16>, SpawnFailure> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid("lpac_nul_argument"));
    }
    value.push(0);
    Ok(value)
}

/// Derives a fresh execution identity without creating a user profile. This
/// changes no account, directory ACL, registry registration or host setting.
pub struct AppContainerSid(Vec<u32>);
impl AppContainerSid {
    pub fn derive(moniker: &str) -> Result<Self, SpawnFailure> {
        if moniker.is_empty()
            || moniker.len() > 128
            || !moniker
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        {
            return Err(invalid("lpac_moniker_invalid"));
        }
        let name = wide(OsStr::new(moniker))?;
        let mut sid = null_mut();
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if hr < 0 || sid.is_null() {
            return Err(SpawnFailure {
                code: "lpac_sid_unavailable",
                os_code: hr as u32,
                execution_state: "not_started",
            });
        }
        let bytes = unsafe { GetLengthSid(sid) };
        let mut owned = vec![0u32; (bytes as usize).div_ceil(4)];
        let copied = unsafe { CopySid(bytes, owned.as_mut_ptr().cast(), sid) };
        unsafe {
            FreeSid(sid);
        }
        if copied == 0 {
            return Err(fail("lpac_sid_copy_failed"));
        }
        Ok(Self(owned))
    }
    pub fn as_ptr(&self) -> PSID {
        self.0.as_ptr() as PSID
    }
}

fn lowbox_token(sid: &AppContainerSid, network: LpacNetwork) -> Result<OwnedHandle, SpawnFailure> {
    type NtCreateLowBoxToken = unsafe extern "system" fn(
        *mut HANDLE,
        HANDLE,
        u32,
        *const core::ffi::c_void,
        PSID,
        u32,
        *const SID_AND_ATTRIBUTES,
        u32,
        *const HANDLE,
    ) -> i32;
    let module_name = wide(OsStr::new("ntdll.dll"))?;
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        return Err(fail("lpac_native_module_unavailable"));
    }
    let function = unsafe { GetProcAddress(module, c"NtCreateLowBoxToken".as_ptr().cast()) }
        .ok_or_else(|| fail("lpac_lowbox_api_unavailable"))?;
    let function: NtCreateLowBoxToken = unsafe { std::mem::transmute(function) };
    let mut parent = null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut parent,
        )
    } == 0
    {
        return Err(fail("lpac_parent_token_unavailable"));
    }
    let parent = unsafe { OwnedHandle::from_raw_handle(parent) };
    // LPAC's documented read-only registry capability is needed by the Win32
    // process startup path. A fixed trusted file-worker may additionally request
    // internetClient only; no server/LAN, COM, broad file or proxy changes.
    let cap_name = wide(OsStr::new("registryRead"))?;
    let (mut groups, mut caps) = (null_mut(), null_mut());
    let (mut group_count, mut cap_count) = (0, 0);
    if unsafe {
        DeriveCapabilitySidsFromName(
            cap_name.as_ptr(),
            &mut groups,
            &mut group_count,
            &mut caps,
            &mut cap_count,
        )
    } == 0
    {
        return Err(fail("lpac_capability_unavailable"));
    }
    let mut capabilities = (0..cap_count)
        .map(|i| SID_AND_ATTRIBUTES {
            Sid: unsafe { *caps.add(i as usize) },
            Attributes: SE_GROUP_ENABLED,
        })
        .collect::<Vec<_>>();
    let mut internet_sid = [0usize; 16];
    if network == LpacNetwork::CodeBuddyFilesInternetClient {
        let mut length = std::mem::size_of_val(&internet_sid) as u32;
        if unsafe {
            CreateWellKnownSid(
                WinCapabilityInternetClientSid,
                null_mut(),
                internet_sid.as_mut_ptr().cast(),
                &mut length,
            )
        } == 0
        {
            unsafe {
                for i in 0..group_count {
                    LocalFree(*groups.add(i as usize));
                }
                LocalFree(groups.cast());
                for i in 0..cap_count {
                    LocalFree(*caps.add(i as usize));
                }
                LocalFree(caps.cast());
            }
            return Err(fail("lpac_internet_capability_unavailable"));
        }
        capabilities.push(SID_AND_ATTRIBUTES {
            Sid: internet_sid.as_mut_ptr().cast(),
            Attributes: SE_GROUP_ENABLED,
        });
    }
    let mut token = null_mut();
    let status = unsafe {
        function(
            &mut token,
            parent.as_raw_handle(),
            TOKEN_ALL_ACCESS,
            null(),
            sid.as_ptr(),
            capabilities.len() as u32,
            capabilities.as_ptr(),
            0,
            null(),
        )
    };
    unsafe {
        for i in 0..group_count {
            LocalFree(*groups.add(i as usize));
        }
        LocalFree(groups.cast());
        for i in 0..cap_count {
            LocalFree(*caps.add(i as usize));
        }
        LocalFree(caps.cast());
    }
    if status < 0 || token.is_null() {
        return Err(SpawnFailure {
            code: "lpac_lowbox_token_failed",
            os_code: status as u32,
            execution_state: "not_started",
        });
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

fn pin_launch_paths(
    token: &OwnedHandle,
    program: &[u16],
    cwd: &[u16],
) -> Result<(OwnedHandle, OwnedHandle), SpawnFailure> {
    let mut prior = null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut prior) } != 0 {
        drop(unsafe { OwnedHandle::from_raw_handle(prior) });
        return Err(invalid("lpac_impersonated_caller_rejected"));
    }
    if unsafe { GetLastError() } != ERROR_NO_TOKEN {
        return Err(fail("lpac_caller_token_query_failed"));
    }
    if unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) } == 0 {
        return Err(fail("lpac_path_token_failed"));
    }
    let image = unsafe {
        CreateFileW(
            program.as_ptr(),
            GENERIC_READ | FILE_EXECUTE,
            FILE_SHARE_READ,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    let image_error = if image == INVALID_HANDLE_VALUE {
        Some(fail("lpac_image_access_denied"))
    } else {
        None
    };
    let directory = unsafe {
        CreateFileW(
            cwd.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    let directory_error = if directory == INVALID_HANDLE_VALUE {
        Some(fail("lpac_cwd_access_denied"))
    } else {
        None
    };
    let image =
        (image != INVALID_HANDLE_VALUE).then(|| unsafe { OwnedHandle::from_raw_handle(image) });
    let directory = (directory != INVALID_HANDLE_VALUE)
        .then(|| unsafe { OwnedHandle::from_raw_handle(directory) });
    if unsafe { RevertToSelf() } == 0 {
        return Err(fail("lpac_revert_failed"));
    }
    if let Some(error) = image_error.or(directory_error) {
        return Err(error);
    }
    for handle in [image.as_ref().unwrap(), directory.as_ref().unwrap()] {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut info) } == 0 {
            return Err(fail("lpac_path_metadata_failed"));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid("lpac_launch_link_rejected"));
        }
    }
    Ok((image.unwrap(), directory.unwrap()))
}

struct Attributes {
    storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new(count: u32) -> Result<Self, SpawnFailure> {
        let mut size = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            return Err(fail("lpac_attribute_size_failed"));
        }
        let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
        let list = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(list, count, 0, &mut size) } == 0 {
            return Err(fail("lpac_attribute_init_failed"));
        }
        Ok(Self { storage, list })
    }
    fn set<T>(&mut self, attribute: u32, value: &T) -> Result<(), SpawnFailure> {
        self.set_raw(attribute, value as *const T as *const _, size_of::<T>())
    }
    fn set_raw(
        &mut self,
        attribute: u32,
        value: *const core::ffi::c_void,
        size: usize,
    ) -> Result<(), SpawnFailure> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.list,
                0,
                attribute as usize,
                value,
                size,
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(fail("lpac_attribute_update_failed"));
        }
        Ok(())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
        let _ = &self.storage;
    }
}

fn pipe() -> Result<(OwnedHandle, OwnedHandle), SpawnFailure> {
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let (mut read, mut write) = (null_mut(), null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, &attrs, 0) } == 0 {
        return Err(fail("lpac_pipe_failed"));
    }
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}
fn no_inherit(handle: &OwnedHandle) -> Result<(), SpawnFailure> {
    if unsafe { SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(fail("lpac_pipe_noninherit_failed"));
    }
    Ok(())
}

/// Uses standard Windows CRT argv quoting. No shell, batch or PowerShell input.
fn quote(value: &OsStr) -> Result<Vec<u16>, SpawnFailure> {
    let value = wide(value)?;
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for &c in &value[..value.len() - 1] {
        if c == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        if c == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        }
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    Ok(out)
}

/// Trusted native call-site policy, never deserialized from Agent arguments.
/// The caller must pin the fixed CodeBuddy binary/control policy before opting
/// in; the default and all other launch paths retain no network capability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LpacNetwork {
    #[default]
    None,
    CodeBuddyFilesInternetClient,
}

fn token_has_capability(token: HANDLE, kind: WELL_KNOWN_SID_TYPE) -> Result<bool, SpawnFailure> {
    let mut wanted = [0usize; 16];
    let mut length = std::mem::size_of_val(&wanted) as u32;
    if unsafe { CreateWellKnownSid(kind, null_mut(), wanted.as_mut_ptr().cast(), &mut length) } == 0
    {
        return Err(fail("lpac_network_policy_query_failed"));
    }
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, TokenCapabilities, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(fail("lpac_network_policy_query_failed"));
    }
    let mut data = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenCapabilities,
            data.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(fail("lpac_network_policy_query_failed"));
    }
    let groups = unsafe { &*data.as_ptr().cast::<TOKEN_GROUPS>() };
    let entries =
        unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize) };
    Ok(entries
        .iter()
        .any(|entry| unsafe { EqualSid(entry.Sid, wanted.as_mut_ptr().cast()) != 0 }))
}

fn verify_network_policy(token: HANDLE, network: LpacNetwork) -> Result<(), SpawnFailure> {
    if token_has_capability(token, WinCapabilityInternetClientSid)?
        != (network == LpacNetwork::CodeBuddyFilesInternetClient)
        || token_has_capability(token, WinCapabilityInternetClientServerSid)?
        || token_has_capability(token, WinCapabilityPrivateNetworkClientServerSid)?
    {
        return Err(invalid("lpac_network_policy_mismatch"));
    }
    Ok(())
}

pub struct LpacLaunch<'a> {
    pub sid: &'a AppContainerSid,
    pub program: &'a Path,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub environment: &'a BTreeMap<String, String>,
    pub network: LpacNetwork,
}

pub struct LpacChild {
    job: Option<OwnedHandle>,
    process: OwnedHandle,
    pid: u32,
    observation: MemberObservation,
    pub stdin: Option<File>,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
}

/// The limit includes the leader. A limit or observation failure permanently
/// prevents this child from producing a complete-tree observation.
pub const LPAC_MAX_OBSERVED_MEMBERS: usize = 4096;
const MEMBER_CAPTURE_ROUND: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    created_at: u64,
}

struct ObservedMember {
    identity: ProcessIdentity,
    handle: OwnedHandle,
}

struct MemberObservation {
    leader: ProcessIdentity,
    members: Vec<ObservedMember>,
    initial_total: u32,
    maximum_total: u32,
    taint: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobProcessAccounting {
    pub total_processes: u32,
    pub active_processes: u32,
}

/// Physical observations only. Admission closure, worker completion and durable
/// Store acceptance must be proven separately by the trusted Runner caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletedTreeObservation {
    leader_exit_code: u32,
    member_count: u32,
}

impl CompletedTreeObservation {
    pub fn leader_exit_code(&self) -> u32 {
        self.leader_exit_code
    }
    pub fn member_count(&self) -> u32 {
        self.member_count
    }
}

fn process_identity(handle: HANDLE) -> io::Result<ProcessIdentity> {
    let pid = unsafe { GetProcessId(handle) };
    if pid == 0 {
        return Err(io::Error::last_os_error());
    }
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let created_at = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    if created_at == 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(ProcessIdentity { pid, created_at })
}

fn job_accounting(handle: HANDLE) -> io::Result<JobProcessAccounting> {
    let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    if unsafe {
        QueryInformationJobObject(
            handle,
            JobObjectBasicAccountingInformation,
            &mut accounting as *mut _ as *mut _,
            size_of_val(&accounting) as u32,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(JobProcessAccounting {
        total_processes: accounting.TotalProcesses,
        active_processes: accounting.ActiveProcesses,
    })
}

fn handle_signaled(handle: HANDLE) -> io::Result<bool> {
    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}
impl LpacChild {
    pub fn spawn(request: LpacLaunch<'_>) -> Result<Self, SpawnFailure> {
        if !request.program.is_absolute()
            || !request.cwd.is_absolute()
            || request
                .program
                .extension()
                .and_then(|s| s.to_str())
                .is_none_or(|s| !s.eq_ignore_ascii_case("exe"))
        {
            return Err(invalid("lpac_absolute_native_executable_required"));
        }
        // Win32 process creation does not consistently normalize '/' in the
        // explicit image name (unlike ordinary file I/O). Use native separators.
        let mut program = wide(request.program.as_os_str())?;
        let mut cwd = wide(request.cwd.as_os_str())?;
        for path in [&mut program, &mut cwd] {
            for c in path.iter_mut() {
                if *c == b'/' as u16 {
                    *c = b'\\' as u16;
                }
            }
        }
        let mut command = quote(request.program.as_os_str())?;
        for arg in request.args {
            command.push(b' ' as u16);
            command.extend(quote(OsStr::new(arg))?);
        }
        command.push(0);
        if command.len() > 32767 {
            return Err(invalid("lpac_arguments_too_large"));
        }
        let mut environment = Vec::new();
        let mut keys = std::collections::BTreeSet::new();
        let mut ordered = request.environment.iter().collect::<Vec<_>>();
        ordered.sort_by_cached_key(|(key, _)| key.to_ascii_uppercase());
        for (key, value) in ordered {
            if key.is_empty() || key.contains(['=', '\0']) || !keys.insert(key.to_ascii_uppercase())
            {
                return Err(invalid("lpac_environment_invalid"));
            }
            environment.extend(wide(OsStr::new(&format!("{key}={value}")))?);
        }
        environment.push(0);
        if environment.len() == 1 {
            environment.push(0);
        }
        if environment.len() > 32767 {
            return Err(invalid("lpac_environment_too_large"));
        }

        let raw_job = unsafe { CreateJobObjectW(null(), null()) };
        if raw_job.is_null() {
            return Err(fail("lpac_job_create_failed"));
        }
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                raw_job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(fail("lpac_job_limits_failed"));
        }
        let (stdin_read, stdin_write) = pipe()?;
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        for parent in [&stdin_write, &stdout_read, &stderr_read] {
            no_inherit(parent)?;
        }
        let handles = [
            stdin_read.as_raw_handle(),
            stdout_write.as_raw_handle(),
            stderr_write.as_raw_handle(),
        ];
        let token = lowbox_token(request.sid, request.network)?;
        let _pinned_launch_paths = pin_launch_paths(&token, &program, &cwd)?;
        let opt_out: u32 = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
        let jobs = [raw_job];
        let mut attrs = Attributes::new(3)?;
        attrs.set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &opt_out,
        )?;
        attrs.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
        attrs.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attrs.list;
        let mut info = PROCESS_INFORMATION::default();
        let flags = EXTENDED_STARTUPINFO_PRESENT
            | CREATE_SUSPENDED
            | CREATE_NO_WINDOW
            | CREATE_UNICODE_ENVIRONMENT;
        if unsafe {
            CreateProcessAsUserW(
                token.as_raw_handle(),
                program.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                flags,
                environment.as_ptr().cast(),
                cwd.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        } == 0
        {
            return Err(fail("lpac_process_create_failed"));
        }
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        let mut in_job = 0;
        if unsafe { IsProcessInJob(process.as_raw_handle(), raw_job, &mut in_job) } == 0
            || in_job != 1
        {
            unsafe {
                TerminateProcess(process.as_raw_handle(), 1);
                WaitForSingleObject(process.as_raw_handle(), 5000);
            }
            return Err(fail("lpac_job_verification_failed"));
        }
        let actual_token = match verify_token(process.as_raw_handle(), request.sid) {
            Ok(token) => token,
            Err(error) => {
                unsafe {
                    TerminateJobObject(raw_job, 1);
                    WaitForSingleObject(process.as_raw_handle(), 5000);
                }
                return Err(error);
            }
        };
        if let Err(error) = verify_network_policy(actual_token.as_raw_handle(), request.network) {
            unsafe {
                TerminateJobObject(raw_job, 1);
                WaitForSingleObject(process.as_raw_handle(), 5000);
            }
            return Err(error);
        }
        // CreateProcessAsUser opens the image as the trusted creator. Recheck
        // under the final opt-out token before running any untrusted code, so
        // an AAP-only executable/cwd cannot use the creator's access as a bypass.
        let _final_launch_pins = match pin_launch_paths(&actual_token, &program, &cwd) {
            Ok(pins) => pins,
            Err(error) => {
                unsafe {
                    TerminateJobObject(raw_job, 1);
                    WaitForSingleObject(process.as_raw_handle(), 5000);
                }
                return Err(error);
            }
        };
        // Register the original leader handle and the fresh Job's cumulative
        // baseline while the process is still suspended. No child/user code has
        // run, and later PID-list samples cannot replace this birth identity.
        let initial_observation = (|| -> io::Result<MemberObservation> {
            let leader = process_identity(process.as_raw_handle())?;
            let accounting = job_accounting(raw_job)?;
            if leader.pid != info.dwProcessId
                || accounting.total_processes != 1
                || accounting.active_processes != 1
            {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            Ok(MemberObservation {
                leader,
                members: Vec::new(),
                initial_total: accounting.total_processes,
                maximum_total: accounting.total_processes,
                taint: None,
            })
        })();
        let observation = match initial_observation {
            Ok(observation) => observation,
            Err(error) => {
                unsafe {
                    TerminateJobObject(raw_job, 1);
                    WaitForSingleObject(process.as_raw_handle(), 5000);
                }
                return Err(SpawnFailure {
                    code: "lpac_member_observation_initialization_failed",
                    os_code: error.raw_os_error().unwrap_or(0) as u32,
                    execution_state: "not_started",
                });
            }
        };
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            let error = fail("lpac_resume_failed");
            unsafe {
                TerminateJobObject(raw_job, 1);
                WaitForSingleObject(process.as_raw_handle(), 5000);
            }
            return Err(error);
        }
        Ok(Self {
            job: Some(job),
            process,
            pid: info.dwProcessId,
            observation,
            stdin: Some(File::from(stdin_write)),
            stdout: Some(File::from(stdout_read)),
            stderr: Some(File::from(stderr_read)),
        })
    }
    pub fn id(&self) -> u32 {
        self.pid
    }

    fn observation_error<T>(&mut self, code: &'static str) -> io::Result<T> {
        self.observation.taint.get_or_insert(code);
        Err(io::Error::other(self.observation.taint.unwrap()))
    }

    fn require_observation(&self) -> io::Result<()> {
        match self.observation.taint {
            Some(code) => Err(io::Error::other(code)),
            None => Ok(()),
        }
    }

    /// Non-authoritative diagnostic count; it cannot replace retained handles.
    pub fn observed_member_count(&self) -> usize {
        self.observation.members.len() + 1
    }

    pub fn observed_total_processes(&self) -> u32 {
        self.observation.maximum_total
    }

    pub fn member_observation_error(&self) -> Option<&'static str> {
        self.observation.taint
    }

    fn checked_accounting(&mut self) -> io::Result<JobProcessAccounting> {
        self.require_observation()?;
        let Some(job) = self.job.as_ref() else {
            return self.observation_error("lpac_member_job_handle_missing");
        };
        let value = match job_accounting(job.as_raw_handle()) {
            Ok(value) => value,
            Err(_) => return self.observation_error("lpac_member_accounting_failed"),
        };
        if value.total_processes < self.observation.initial_total
            || value.total_processes < self.observation.maximum_total
            || value.total_processes as usize > LPAC_MAX_OBSERVED_MEMBERS
            || value.active_processes > value.total_processes
            || self.observed_member_count() > value.total_processes as usize
        {
            return self.observation_error("lpac_member_accounting_invalid");
        }
        self.observation.maximum_total = value.total_processes;
        Ok(value)
    }

    /// Capture only readable/synchronizable process handles proven to belong to
    /// this exact Job. PID lists find candidates, never prove completeness.
    /// Returns false when this bounded round used its 25 ms budget. The caller
    /// supplies its fixed overall deadline; repeated rounds must not extend it.
    pub fn capture_member_handles(&mut self, deadline: Instant) -> io::Result<bool> {
        self.require_observation()?;
        if Instant::now() >= deadline {
            return self.observation_error("lpac_member_observation_deadline");
        }
        let round_deadline = deadline.min(Instant::now() + MEMBER_CAPTURE_ROUND);
        self.checked_accounting()?;
        let pids = match self.process_ids() {
            Ok(pids) => pids,
            Err(_) => return self.observation_error("lpac_member_enumeration_failed"),
        };
        for pid in pids {
            if Instant::now() >= deadline {
                return self.observation_error("lpac_member_observation_deadline");
            }
            if Instant::now() >= round_deadline {
                return Ok(false);
            }
            // These PIDs belong to still-retained original handles. They cannot
            // be replaced with a later process to fill a historical coverage gap.
            if pid == self.observation.leader.pid
                || self
                    .observation
                    .members
                    .iter()
                    .any(|member| member.identity.pid == pid)
            {
                continue;
            }
            if self.observed_member_count() >= LPAC_MAX_OBSERVED_MEMBERS {
                return self.observation_error("lpac_member_handle_limit");
            }
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if raw.is_null() {
                return self.observation_error("lpac_member_open_failed");
            }
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            let identity = match process_identity(handle.as_raw_handle()) {
                Ok(identity) if identity.pid == pid => identity,
                _ => return self.observation_error("lpac_member_identity_failed"),
            };
            let mut in_job = 0;
            if unsafe {
                IsProcessInJob(
                    handle.as_raw_handle(),
                    self.job.as_ref().unwrap().as_raw_handle(),
                    &mut in_job,
                )
            } == 0
                || in_job != 1
            {
                return self.observation_error("lpac_member_membership_unproven");
            }
            if unsafe { CompareObjectHandles(handle.as_raw_handle(), self.process.as_raw_handle()) }
                != 0
                || self.observation.members.iter().any(|member| unsafe {
                    CompareObjectHandles(handle.as_raw_handle(), member.handle.as_raw_handle()) != 0
                })
            {
                return self.observation_error("lpac_member_identity_conflict");
            }
            self.observation
                .members
                .push(ObservedMember { identity, handle });
        }
        self.checked_accounting()?;
        if Instant::now() >= deadline {
            return self.observation_error("lpac_member_observation_deadline");
        }
        Ok(true)
    }

    /// Physical completion can be used only after the caller durably seals all
    /// future admission/assignment to this private Job. It does not assert that
    /// the Runner's worker/output tasks have finished or that Store accepted it.
    pub fn observe_completed_tree(
        &mut self,
        deadline: Instant,
    ) -> io::Result<Option<CompletedTreeObservation>> {
        if !self.capture_member_handles(deadline)? {
            return Ok(None);
        }
        let before = self.checked_accounting()?;
        if before.active_processes != 0 {
            return Ok(None);
        }
        if before.total_processes as usize != self.observed_member_count() {
            return self.observation_error("lpac_member_coverage_missing");
        }
        match handle_signaled(self.process.as_raw_handle()) {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(_) => return self.observation_error("lpac_leader_wait_failed"),
        }
        for index in 0..self.observation.members.len() {
            if Instant::now() >= deadline {
                return self.observation_error("lpac_member_observation_deadline");
            }
            match handle_signaled(self.observation.members[index].handle.as_raw_handle()) {
                Ok(true) => {}
                Ok(false) => return Ok(None),
                Err(_) => return self.observation_error("lpac_member_wait_failed"),
            }
        }
        let leader_exit_code = match self.try_wait() {
            Ok(Some(code)) => code,
            Ok(None) => return Ok(None),
            Err(_) => return self.observation_error("lpac_leader_exit_query_failed"),
        };
        let after = self.checked_accounting()?;
        if before != after {
            return self.observation_error("lpac_member_accounting_changed_after_exit");
        }
        if Instant::now() >= deadline {
            return self.observation_error("lpac_member_observation_deadline");
        }
        Ok(Some(CompletedTreeObservation {
            leader_exit_code,
            member_count: after.total_processes,
        }))
    }
    pub fn try_wait(&mut self) -> io::Result<Option<u32>> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(code))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
    pub fn terminate_tree(&mut self) -> io::Result<()> {
        if let Some(job) = &self.job {
            if unsafe { TerminateJobObject(job.as_raw_handle(), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    pub fn active_processes(&self) -> io::Result<u32> {
        let Some(job) = &self.job else {
            return Ok(0);
        };
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        if unsafe {
            QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                &mut accounting as *mut _ as *mut _,
                size_of_val(&accounting) as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }
    pub fn process_ids(&self) -> io::Result<Vec<u32>> {
        let Some(job) = &self.job else {
            return Ok(Vec::new());
        };
        for capacity in [16usize, 64, 256, 1024, 4096] {
            let mut data =
                vec![0usize; (8 + capacity * size_of::<usize>()).div_ceil(size_of::<usize>())];
            let ok = unsafe {
                QueryInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectBasicProcessIdList,
                    data.as_mut_ptr().cast(),
                    (data.len() * size_of::<usize>()) as u32,
                    null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_MORE_DATA as i32) {
                    continue;
                }
                return Err(error);
            }
            let count = unsafe { *(data.as_ptr().cast::<u32>().add(1)) } as usize;
            if count > capacity {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            let pids = unsafe {
                std::slice::from_raw_parts(data.as_ptr().cast::<u8>().add(8).cast::<usize>(), count)
            };
            return pids
                .iter()
                .map(|pid| {
                    u32::try_from(*pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
                })
                .collect();
        }
        Err(io::Error::from(io::ErrorKind::OutOfMemory))
    }
    pub fn wait_tree_exit(&self, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.active_processes()? == 0 {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for LpacChild {
    fn drop(&mut self) {
        drop(self.job.take());
    }
}

fn verify_token(process: HANDLE, sid: &AppContainerSid) -> Result<OwnedHandle, SpawnFailure> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY | TOKEN_DUPLICATE, &mut token) } == 0 {
        return Err(fail("lpac_token_open_failed"));
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    for class in [TokenIsAppContainer, TokenIsLessPrivilegedAppContainer] {
        let (mut value, mut size) = (0u32, 0u32);
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                class,
                &mut value as *mut _ as *mut _,
                4,
                &mut size,
            )
        } == 0
        {
            if class == TokenIsLessPrivilegedAppContainer {
                if unsafe { GetLastError() } != ERROR_INVALID_PARAMETER {
                    return Err(fail("lpac_query_failed"));
                }
                let allowed = aap_access(token.as_raw_handle())?;
                if allowed {
                    return Err(invalid("lpac_aap_access_not_restricted"));
                }
                let mut text = null_mut();
                if unsafe { ConvertSidToStringSidW(sid.as_ptr(), &mut text) } == 0 {
                    return Err(fail("lpac_sid_text_failed"));
                }
                let mut len = 0;
                unsafe {
                    while *text.add(len) != 0 {
                        len += 1;
                    }
                }
                let sid_text =
                    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
                unsafe {
                    LocalFree(text.cast());
                }
                if !descriptor_access(
                    token.as_raw_handle(),
                    &format!("O:SYG:SYD:(A;;0x1;;;WD)(A;;0x1;;;{sid_text})"),
                )? {
                    return Err(invalid("lpac_exact_sid_access_failed"));
                }
                // Windows 10 cannot query token information class 46. Verify
                // the actual effective ACL behavior instead: AAP access must
                // fail while this exact container SID must still pass.
                continue;
            }
            return Err(fail("appcontainer_query_failed"));
        }
        if value != 1 {
            return Err(invalid(if class == TokenIsAppContainer {
                "appcontainer_flag_false"
            } else {
                "lpac_flag_false"
            }));
        }
    }
    let mut buffer = [0usize; 32];
    let mut size = 0;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenAppContainerSid,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            &mut size,
        )
    } == 0
    {
        return Err(fail("lpac_token_sid_unavailable"));
    }
    let info = unsafe { &*buffer.as_ptr().cast::<TOKEN_APPCONTAINER_INFORMATION>() };
    if unsafe { EqualSid(info.TokenAppContainer, sid.as_ptr()) } == 0 {
        return Err(invalid("lpac_token_sid_mismatch"));
    }
    Ok(token)
}

fn aap_access(token: HANDLE) -> Result<bool, SpawnFailure> {
    descriptor_access(token, "O:SYG:SYD:(A;;0x1;;;WD)(A;;0x1;;;AC)")
}
fn descriptor_access(token: HANDLE, descriptor: &str) -> Result<bool, SpawnFailure> {
    let mut impersonation = null_mut();
    if unsafe {
        DuplicateTokenEx(
            token,
            TOKEN_QUERY,
            null(),
            SecurityIdentification,
            TokenImpersonation,
            &mut impersonation,
        )
    } == 0
    {
        return Err(fail("lpac_access_token_failed"));
    }
    let impersonation = unsafe { OwnedHandle::from_raw_handle(impersonation) };
    let sddl = wide(OsStr::new(descriptor))?;
    let mut sd = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, null_mut())
    } == 0
    {
        return Err(fail("lpac_access_descriptor_failed"));
    }
    let map = GENERIC_MAPPING {
        GenericRead: 1,
        GenericWrite: 2,
        GenericExecute: 4,
        GenericAll: 7,
    };
    let mut privileges = [0usize; 64];
    let mut size = size_of_val(&privileges) as u32;
    let (mut granted, mut status) = (0u32, 0i32);
    let ok = unsafe {
        AccessCheck(
            sd,
            impersonation.as_raw_handle(),
            1,
            &map,
            privileges.as_mut_ptr().cast(),
            &mut size,
            &mut granted,
            &mut status,
        )
    };
    let error = if ok == 0 {
        Some(fail("lpac_access_check_failed"))
    } else {
        None
    };
    unsafe {
        LocalFree(sd);
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok(status != 0)
}
pub fn diagnostic_current_aap_access() -> Result<bool, SpawnFailure> {
    let mut token = null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut token,
        )
    } == 0
    {
        return Err(fail("lpac_diagnostic_token_failed"));
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    aap_access(token.as_raw_handle())
}

#[cfg(test)]
mod codebuddy_network_tests {
    use super::*;
    #[test]
    fn fixed_codebuddy_network_is_internet_client_only_and_default_is_off() {
        let sid = AppContainerSid::derive("codebuddy-network-unit").unwrap();
        for policy in [LpacNetwork::None, LpacNetwork::CodeBuddyFilesInternetClient] {
            let token = lowbox_token(&sid, policy).unwrap();
            verify_network_policy(token.as_raw_handle(), policy).unwrap();
            let other = if policy == LpacNetwork::None {
                LpacNetwork::CodeBuddyFilesInternetClient
            } else {
                LpacNetwork::None
            };
            assert!(verify_network_policy(token.as_raw_handle(), other).is_err());
        }
    }
}
