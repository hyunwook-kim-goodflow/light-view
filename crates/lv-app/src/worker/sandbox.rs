//! Launches one `lv-worker.exe` inside its sandbox (spec §6.1, ruling R1).
//!
//! Mandatory layers (a worker that misses one is not used): a restricted token at low
//! integrity, and a Job (kill on close, one active process, a memory cap) that the process
//! is created in (PROC_THREAD_ATTRIBUTE_JOB_LIST; where Windows refuses that, it joins
//! while still suspended), so it never runs outside the Job. Best-effort layers (logged
//! when they fail): the worker's AppContainer with no capabilities (ruling R2, see
//! [`appcontainer`](super::appcontainer)), win32k system call lockdown and the
//! no-child-process policy at creation, and the Job's UI restrictions. Only the three pipe
//! ends are inherited, the working directory is the worker's folder and the environment
//! holds `SystemRoot` alone (plus `LOCALAPPDATA` in an AppContainer, which Windows points at
//! the container's own folder), so neither the DLL search nor anything environment-driven
//! reaches other folders.

use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
};
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
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, ResumeThread, TerminateProcess, WaitForSingleObject,
};

use super::appcontainer::Container;
use launch::Launch;

mod launch;

/// Pipe buffer for requests and responses; responses are tens of MB.
const PIPE_BUFFER: u32 = 1024 * 1024;

/// A running sandboxed worker. Dropping it terminates the Job and with it the process.
pub struct Sandboxed {
    pub pid: u32,
    /// Held so the PID cannot be reused while the worker is in use, and to poll for exit.
    pub process: OwnedHandle,
    /// Shared so the pool can end the worker from another thread ([`terminate_job`]).
    pub job: Arc<OwnedHandle>,
    /// Why the start in the requested AppContainer failed, if it did (the worker then runs
    /// without it). The pool logs this once and stops asking.
    pub container_failure: Option<String>,
}

impl Sandboxed {
    /// Whether the process has already ended.
    pub fn has_exited(&self) -> bool {
        self.wait_for_exit(std::time::Duration::ZERO)
    }

    /// Waits at most `timeout` for the process to end; whether it has.
    pub fn wait_for_exit(&self, timeout: std::time::Duration) -> bool {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: valid process handle owned by `self`; the wait is bounded.
        unsafe { WaitForSingleObject(raw(&self.process), ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for Sandboxed {
    fn drop(&mut self) {
        terminate_job(&self.job, self.pid);
    }
}

/// Ends every process in the worker's Job (`pid` is for the log). Harmless if it has ended.
pub fn terminate_job(job: &OwnedHandle, pid: u32) {
    // SAFETY: `job` is a valid Job handle, kept open by the borrow.
    if let Err(e) = unsafe { TerminateJobObject(raw(job), 1) } {
        log(&format!("TerminateJobObject for worker {pid} failed: {e}"));
    }
}

/// The app's ends of the worker's standard handles.
pub struct Pipes {
    pub stdin: File,
    /// Our copy of the worker's stdin read end, to see whether the worker took its request.
    pub stdin_monitor: StdinMonitor,
    pub stdout: File,
    pub stderr: File,
}

/// A second read end of the worker's stdin pipe, never read from.
///
/// While it is open, writes to a dead worker land in the pipe buffer instead of failing, so
/// after a worker dies [`StdinMonitor::unread`] tells whether it had taken its request.
/// Dropping it (with the worker) makes a writer blocked on a full pipe fail and end.
pub struct StdinMonitor(OwnedHandle);

impl StdinMonitor {
    /// Bytes written to the worker's stdin that nobody has read.
    pub fn unread(&self) -> windows::core::Result<u32> {
        let mut available = 0u32;
        // SAFETY: valid pipe read handle; only the byte count is written (no buffer).
        unsafe { PeekNamedPipe(raw(&self.0), None, 0, None, Some(&mut available), None)? };
        Ok(available)
    }
}

pub fn log(msg: &str) {
    eprintln!("light-view: worker: {msg}");
}

/// Whether CreateProcessAsUserW refused PROC_THREAD_ATTRIBUTE_JOB_LIST once (before
/// Windows 10): later starts go straight to the assign-after-creation path.
static JOB_LIST_REFUSED: AtomicBool = AtomicBool::new(false);

/// Starts `exe --max-pixels <max_pixels> <extra_args>` in the sandbox, inside `container`
/// when given. With `mitigations` false the best-effort creation attributes (AppContainer,
/// win32k lockdown, child process policy) are left out; the mandatory layers are always
/// applied. A failed start drops the best-effort layers in steps: first the AppContainer
/// alone, then the mitigation attributes.
pub fn spawn(
    exe: &Path,
    max_pixels: u64,
    extra_args: &[&str],
    mitigations: bool,
    container: Option<&Container>,
) -> Result<(Sandboxed, Pipes), String> {
    start_suspended(exe, max_pixels, extra_args, mitigations, container)?.resume()
}

/// A worker created suspended and already inside its Job, not yet resumed. Dropping it
/// ends the Job and with it the process.
pub struct Suspended {
    sandboxed: Sandboxed,
    thread: OwnedHandle,
    pipes: Pipes,
    /// The process was created inside the Job (PROC_THREAD_ATTRIBUTE_JOB_LIST), not
    /// assigned to it afterwards. Read by the tests only.
    #[cfg_attr(not(test), allow(dead_code))]
    born_in_job: bool,
}

impl Suspended {
    /// Lets the worker run.
    pub fn resume(self) -> Result<(Sandboxed, Pipes), String> {
        // SAFETY: valid thread handle of the suspended main thread.
        if unsafe { ResumeThread(raw(&self.thread)) } == u32::MAX {
            // `self.sandboxed` drops here and ends the Job.
            return Err(format!(
                "ResumeThread: {}",
                windows::core::Error::from_win32()
            ));
        }
        Ok((self.sandboxed, self.pipes))
    }
}

/// [`spawn`] up to the point before the worker runs: the process exists, suspended, inside
/// its kill-on-close Job. Where Windows accepts PROC_THREAD_ATTRIBUTE_JOB_LIST the process
/// is born inside the Job, so a parent that dies at any moment takes it along; otherwise
/// it is assigned to the Job right after creation (a parent killed in between would leave
/// it suspended outside any Job).
pub fn start_suspended(
    exe: &Path,
    max_pixels: u64,
    extra_args: &[&str],
    mitigations: bool,
    container: Option<&Container>,
) -> Result<Suspended, String> {
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
    // One step of the AppContainer/mitigation fallback: inside the Job from creation when
    // Windows takes the attribute, else the same step without it. The bool says which.
    let create = |mitigations: bool, container: Option<&Container>| {
        let without_job = || launch.create(&token, child_ends, mitigations, container, None);
        if JOB_LIST_REFUSED.load(Ordering::Relaxed) {
            return without_job().map(|info| (info, false));
        }
        match launch.create(&token, child_ends, mitigations, container, Some(raw(&job))) {
            Ok(info) => Ok((info, true)),
            Err(with_job) => {
                let info = without_job()?;
                log(&format!(
                    "start inside the job refused ({with_job}); assigning after creation"
                ));
                JOB_LIST_REFUSED.store(true, Ordering::Relaxed);
                Ok((info, false))
            }
        }
    };
    let container = container.filter(|_| mitigations);
    let mut container_failure = None;
    let first = match container {
        Some(container) => create(mitigations, Some(container)).or_else(|e| {
            container_failure = Some(e);
            create(mitigations, None)
        }),
        None => create(mitigations, None),
    };
    let (info, born_in_job) = match first {
        Ok(created) => created,
        Err(e) if mitigations => {
            log(&format!(
                "start with mitigation attributes failed ({e}); retrying without"
            ));
            create(false, None)?
        }
        Err(e) => return Err(e),
    };
    // SAFETY: CreateProcessAsUserW returned these handles to us; we own them from here, so
    // every early return below closes them.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess.0),
            OwnedHandle::from_raw_handle(info.hThread.0),
        )
    };
    // The child holds its own copies now; ours of its output ends would keep those pipes
    // open after it dies. The stdin read end stays (no longer inheritable) as the monitor.
    drop((stdout_child, stderr_child));
    // SAFETY: `stdin_child` is a valid pipe handle we own.
    if let Err(e) =
        unsafe { SetHandleInformation(raw(&stdin_child), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
    {
        // SAFETY: valid process handle; the suspended process never ran and may not be in
        // the Job yet, so it must not be left behind.
        let _ = unsafe { TerminateProcess(raw(&process), 1) };
        return Err(format!("SetHandleInformation: {e}"));
    }

    if !born_in_job {
        // SAFETY: both handles are valid; the process is suspended and has run no code.
        if let Err(e) = unsafe { AssignProcessToJobObject(raw(&job), raw(&process)) } {
            // SAFETY: valid process handle; the suspended process never ran outside the Job.
            let _ = unsafe { TerminateProcess(raw(&process), 1) };
            return Err(format!("AssignProcessToJobObject: {e}"));
        }
    }
    Ok(Suspended {
        sandboxed: Sandboxed {
            pid: info.dwProcessId,
            process,
            job: Arc::new(job),
            container_failure,
        },
        thread,
        pipes: Pipes {
            stdin: File::from(stdin_ours),
            stdin_monitor: StdinMonitor(stdin_child),
            stdout: File::from(stdout_ours),
            stderr: File::from(stderr_ours),
        },
        born_in_job,
    })
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

/// Memory cap per worker: what libheif may allocate plus the worker's own RGBA copy,
/// `max(12 * max_pixels, 256 MiB + 4 * max_pixels)` (see `lv_ipc::worker_memory_limit`).
pub fn memory_limit(max_pixels: u64) -> u64 {
    lv_ipc::worker_memory_limit(max_pixels)
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::JobObjects::IsProcessInJob;

    use crate::worker::tests::{worker_exe, Process};

    fn in_job(sandboxed: &Sandboxed) -> bool {
        let mut in_job = BOOL::default();
        // SAFETY: both handles are valid and owned by `sandboxed`.
        unsafe { IsProcessInJob(raw(&sandboxed.process), raw(&sandboxed.job), &mut in_job) }
            .expect("IsProcessInJob");
        in_job.as_bool()
    }

    #[test]
    fn worker_is_born_inside_its_job_before_it_runs() {
        let suspended =
            start_suspended(&worker_exe(), 100_000_000, &[], true, None).expect("starts");
        assert!(
            suspended.born_in_job,
            "Windows 10+ creates the worker inside the Job"
        );
        assert!(in_job(&suspended.sandboxed), "in the Job while suspended");
        assert!(!suspended.sandboxed.has_exited());

        // Never resumed: dropping it ends the Job, which ends the process.
        let watch = Process::open(suspended.sandboxed.pid);
        drop(suspended);
        assert!(watch.wait_for_exit(Duration::from_secs(10)));
    }

    #[test]
    fn resumed_worker_without_mitigations_is_in_its_job() {
        let suspended =
            start_suspended(&worker_exe(), 100_000_000, &[], false, None).expect("starts");
        assert!(suspended.born_in_job);
        let (sandboxed, _pipes) = suspended.resume().expect("resumes");
        assert!(in_job(&sandboxed));
    }
}
