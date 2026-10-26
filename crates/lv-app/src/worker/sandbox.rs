//! Launches one `lv-worker.exe` inside its sandbox (spec §6.1, ruling R1).
//!
//! Mandatory layers (a worker that misses one is not used): a restricted token at low
//! integrity, and a Job (kill on close, one active process, a memory cap) that the process
//! joins while still suspended, so it never runs outside the Job. Best-effort layers (logged
//! when they fail): win32k system call lockdown and the no-child-process policy at creation,
//! and the Job's UI restrictions. Only the three pipe ends are inherited, the working
//! directory is the worker's folder and the environment holds `SystemRoot` alone, so neither
//! the DLL search nor anything environment-driven reaches other folders.

use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{SetHandleInformation, BOOL, HANDLE, HANDLE_FLAG_INHERIT};
use windows::Win32::Security::{
    CreateRestrictedToken, CreateWellKnownSid, GetLengthSid, SetTokenInformation,
    TokenIntegrityLevel, WinLowLabelSid, DISABLE_MAX_PRIVILEGE, PSID, SID_AND_ATTRIBUTES,
    TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, DeleteProcThreadAttributeList, GetCurrentProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

const MIB: u64 = 1024 * 1024;
/// Pipe buffer for requests and responses; responses are tens of MB.
const PIPE_BUFFER: u32 = 1024 * 1024;
/// PROCESS_CREATION_MITIGATION_POLICY_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON.
const WIN32K_SYSTEM_CALL_DISABLE: u64 = 1 << 28;
/// PROCESS_CREATION_CHILD_PROCESS_RESTRICTED.
const CHILD_PROCESS_RESTRICTED: u32 = 1;

/// A running sandboxed worker. Dropping it terminates the Job and with it the process.
pub struct Sandboxed {
    pub pid: u32,
    /// Held so the PID cannot be reused while the worker is in use.
    pub process: OwnedHandle,
    pub job: OwnedHandle,
}

impl Drop for Sandboxed {
    fn drop(&mut self) {
        // SAFETY: `self.job` is a valid Job handle owned by `self`.
        if let Err(e) = unsafe { TerminateJobObject(raw(&self.job), 1) } {
            log(&format!(
                "TerminateJobObject for worker {} failed: {e}",
                self.pid
            ));
        }
    }
}

/// The app's ends of the worker's standard handles.
pub struct Pipes {
    pub stdin: File,
    pub stdout: File,
    pub stderr: File,
}

pub fn log(msg: &str) {
    eprintln!("light-view: worker: {msg}");
}

/// Starts `exe --max-pixels <max_pixels> <extra_args>` in the sandbox.
pub fn spawn(
    exe: &Path,
    max_pixels: u64,
    extra_args: &[&str],
) -> Result<(Sandboxed, Pipes), String> {
    let token = low_integrity_token().map_err(|e| format!("low integrity token: {e}"))?;
    let job = create_job(max_pixels).map_err(|e| format!("job object: {e}"))?;

    let (stdin_child, stdin_ours) = pipe().map_err(|e| format!("stdin pipe: {e}"))?;
    let (stdout_ours, stdout_child) = pipe().map_err(|e| format!("stdout pipe: {e}"))?;
    let (stderr_ours, stderr_child) = pipe().map_err(|e| format!("stderr pipe: {e}"))?;
    for end in [&stdin_child, &stdout_child, &stderr_child] {
        // SAFETY: `end` is a valid pipe handle we own.
        unsafe { SetHandleInformation(raw(end), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .map_err(|e| format!("SetHandleInformation: {e}"))?;
    }
    let child_ends = [raw(&stdin_child), raw(&stdout_child), raw(&stderr_child)];

    let launch = Launch::new(exe, max_pixels, extra_args)?;
    let info = match launch.create(&token, child_ends, true) {
        Ok(info) => info,
        Err(e) => {
            log(&format!(
                "start with mitigation attributes failed ({e}); retrying without"
            ));
            launch.create(&token, child_ends, false)?
        }
    };
    // The child holds its own copies now; ours would keep the pipes open after it dies.
    drop((stdin_child, stdout_child, stderr_child));
    // SAFETY: CreateProcessAsUserW returned these handles to us; we own them from here.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess.0),
            OwnedHandle::from_raw_handle(info.hThread.0),
        )
    };

    // SAFETY: both handles are valid; the process is suspended and has run no code.
    if let Err(e) = unsafe { AssignProcessToJobObject(raw(&job), raw(&process)) } {
        // SAFETY: valid process handle; the suspended process never ran outside the Job.
        let _ = unsafe { TerminateProcess(raw(&process), 1) };
        return Err(format!("AssignProcessToJobObject: {e}"));
    }
    let sandboxed = Sandboxed {
        pid: info.dwProcessId,
        process,
        job,
    };
    // SAFETY: valid thread handle of the suspended main thread.
    if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
        return Err(format!(
            "ResumeThread: {}",
            windows::core::Error::from_win32()
        ));
    }
    let pipes = Pipes {
        stdin: File::from(stdin_ours),
        stdout: File::from(stdout_ours),
        stderr: File::from(stderr_ours),
    };
    Ok((sandboxed, pipes))
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

/// # Safety
/// `handle` must be a valid handle that nothing else owns.
unsafe fn owned(handle: HANDLE) -> OwnedHandle {
    OwnedHandle::from_raw_handle(handle.0)
}

/// A non-inheritable anonymous pipe as (read end, write end).
fn pipe() -> windows::core::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: out-pointers to locals; on success both handles are new and ours.
    unsafe {
        CreatePipe(&mut read, &mut write, None, PIPE_BUFFER)?;
        Ok((owned(read), owned(write)))
    }
}

/// A primary token like ours with every privilege but SeChangeNotify removed, at low
/// integrity.
fn low_integrity_token() -> windows::core::Result<OwnedHandle> {
    let mut current = HANDLE::default();
    // SAFETY: pseudo handle of our own process; `current` receives a new handle.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
            &mut current,
        )?
    };
    // SAFETY: OpenProcessToken succeeded, so `current` is a new handle we own.
    let current = unsafe { owned(current) };
    let mut restricted = HANDLE::default();
    // SAFETY: valid token handle; `restricted` receives a new handle.
    unsafe {
        CreateRestrictedToken(
            raw(&current),
            DISABLE_MAX_PRIVILEGE,
            None,
            None,
            None,
            &mut restricted,
        )?
    };
    // SAFETY: CreateRestrictedToken succeeded, so `restricted` is a new handle we own.
    let restricted = unsafe { owned(restricted) };

    // SECURITY_MAX_SID_SIZE is 68 bytes; u32 elements give the SID its 4-byte alignment.
    let mut sid_buf = [0u32; 17];
    let sid = PSID(sid_buf.as_mut_ptr().cast());
    let mut sid_len = std::mem::size_of_val(&sid_buf) as u32;
    // SAFETY: `sid` points to `sid_len` writable bytes.
    unsafe { CreateWellKnownSid(WinLowLabelSid, PSID::default(), sid, &mut sid_len)? };
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    // SAFETY: `label` and the SID it points to live until the call returns; the length
    // covers both, as the API expects.
    unsafe {
        SetTokenInformation(
            raw(&restricted),
            TokenIntegrityLevel,
            std::ptr::from_ref(&label).cast(),
            std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid),
        )?
    };
    Ok(restricted)
}

/// Memory cap per worker: the decoded image three times over, at least 256 MiB.
pub fn memory_limit(max_pixels: u64) -> u64 {
    max_pixels.saturating_mul(4 * 3).max(256 * MIB)
}

fn create_job(max_pixels: u64) -> windows::core::Result<OwnedHandle> {
    // SAFETY: unnamed Job with default security.
    let job = unsafe { owned(CreateJobObjectW(None, PCWSTR::null())?) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // DIE_ON_UNHANDLED_EXCEPTION: a crash ends the process at once instead of waiting on a
    // Windows Error Reporting dialog.
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = usize::try_from(memory_limit(max_pixels)).unwrap_or(usize::MAX);
    // SAFETY: `limits` is a valid structure of the size passed.
    unsafe {
        SetInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )?
    };

    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    // SAFETY: `ui` is a valid structure of the size passed.
    let ui_result = unsafe {
        SetInformationJobObject(
            raw(&job),
            JobObjectBasicUIRestrictions,
            std::ptr::from_ref(&ui).cast(),
            std::mem::size_of_val(&ui) as u32,
        )
    };
    if let Err(e) = ui_result {
        log(&format!("job UI restrictions not applied: {e}"));
    }
    Ok(job)
}

/// Wide strings for one CreateProcessAsUserW call.
struct Launch {
    application: Vec<u16>,
    command_line: Vec<u16>,
    directory: Vec<u16>,
    environment: Vec<u16>,
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

impl Launch {
    fn new(exe: &Path, max_pixels: u64, extra_args: &[&str]) -> Result<Launch, String> {
        let directory = exe
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", exe.display()))?;
        let mut command_line = format!("\"{}\" --max-pixels {max_pixels}", exe.display());
        for arg in extra_args {
            command_line.push(' ');
            command_line.push_str(arg);
        }
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let mut environment = wide(OsStr::new("SystemRoot="));
        environment.pop();
        environment.extend(system_root.encode_wide());
        environment.extend([0, 0]);
        Ok(Launch {
            application: wide(exe.as_os_str()),
            command_line: wide(OsStr::new(&command_line)),
            directory: wide(directory.as_os_str()),
            environment,
        })
    }

    /// Creates the process suspended, inheriting only `child_ends` (stdin, stdout, stderr).
    fn create(
        &self,
        token: &OwnedHandle,
        child_ends: [HANDLE; 3],
        mitigations: bool,
    ) -> Result<PROCESS_INFORMATION, String> {
        let win32k = WIN32K_SYSTEM_CALL_DISABLE;
        let child_policy = CHILD_PROCESS_RESTRICTED;
        let mut attributes = AttributeList::new(if mitigations { 3 } else { 1 })?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            child_ends.as_ptr().cast(),
            std::mem::size_of_val(&child_ends),
        )?;
        if mitigations {
            if let Err(e) = attributes.set(
                PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
                std::ptr::from_ref(&win32k).cast(),
                std::mem::size_of_val(&win32k),
            ) {
                log(&format!("win32k lockdown not applied: {e}"));
            }
            if let Err(e) = attributes.set(
                PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
                std::ptr::from_ref(&child_policy).cast(),
                std::mem::size_of_val(&child_policy),
            ) {
                log(&format!("child process policy not applied: {e}"));
            }
        }

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        [
            startup.StartupInfo.hStdInput,
            startup.StartupInfo.hStdOutput,
            startup.StartupInfo.hStdError,
        ] = child_ends;
        startup.lpAttributeList = attributes.as_ptr();

        // DETACHED_PROCESS, not CREATE_NO_WINDOW: a console would need a conhost.exe child,
        // which the child process policy forbids (the worker then dies with
        // STATUS_DLL_INIT_FAILED). The standard handles are the pipes either way.
        // CreateProcessAsUserW may write into the command line buffer.
        let mut command_line = self.command_line.clone();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: every string is NUL-terminated (the environment double-NUL-terminated)
        // and, like the attribute list and the handle array it points to, outlives the call.
        unsafe {
            CreateProcessAsUserW(
                raw(token),
                PCWSTR(self.application.as_ptr()),
                PWSTR(command_line.as_mut_ptr()),
                None,
                None,
                BOOL::from(true),
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_SUSPENDED
                    | DETACHED_PROCESS
                    | CREATE_UNICODE_ENVIRONMENT,
                Some(self.environment.as_ptr().cast::<c_void>()),
                PCWSTR(self.directory.as_ptr()),
                &startup.StartupInfo,
                &mut info,
            )
        }
        .map_err(|e| format!("CreateProcessAsUserW: {e}"))?;
        Ok(info)
    }
}

/// An initialized PROC_THREAD_ATTRIBUTE_LIST, deleted on drop.
struct AttributeList {
    // usize elements keep the opaque list pointer-aligned.
    buf: Vec<usize>,
}

impl AttributeList {
    fn new(count: u32) -> Result<AttributeList, String> {
        let mut size = 0usize;
        // SAFETY: a null list asks for the required size only; this "fails" by design.
        let _ = unsafe {
            InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST::default(),
                count,
                0,
                &mut size,
            )
        };
        let mut list = AttributeList {
            buf: vec![0; size.div_ceil(std::mem::size_of::<usize>())],
        };
        // SAFETY: `buf` holds at least `size` writable bytes.
        unsafe { InitializeProcThreadAttributeList(list.as_ptr(), count, 0, &mut size) }
            .map_err(|e| format!("InitializeProcThreadAttributeList: {e}"))?;
        Ok(list)
    }

    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr().cast())
    }

    /// The list stores `value` by pointer: it must outlive every use of the list.
    fn set(&mut self, attribute: u32, value: *const c_void, size: usize) -> Result<(), String> {
        // SAFETY: initialized list; the caller keeps `value` (of `size` bytes) alive.
        unsafe {
            UpdateProcThreadAttribute(
                self.as_ptr(),
                0,
                attribute as usize,
                Some(value),
                size,
                None,
                None,
            )
        }
        .map_err(|e| format!("UpdateProcThreadAttribute({attribute:#x}): {e}"))
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialized in `new` and is deleted exactly once.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}
