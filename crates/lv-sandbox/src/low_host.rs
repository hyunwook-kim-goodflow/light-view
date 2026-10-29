//! Test helpers (this crate's tests and other crates' with the `test-support` feature) for
//! running code the way the Explorer extension runs in a low integrity host: a test of the
//! same test binary restarted at low integrity inside a Job, and a worker staged in a folder
//! that lets AppContainers in, as the per-user installer grants.

use std::ffi::{c_void, OsStr, OsString};
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    SetHandleInformation, BOOL, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::SystemServices::SECURITY_MANDATORY_LOW_RID;
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, ResumeThread,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW,
};

use crate::appcontainer::folder_readable;
use crate::sandbox::low_integrity_token;

/// `built` (an `lv-worker.exe`) and the DLLs beside it, copied to a new `%TEMP%\lv-m7-*`
/// folder (deleted with the returned guard) that grants `ALL APPLICATION PACKAGES` read and
/// execute access, as the per-user installer does. Returns the guard and the copied exe.
pub fn staged_worker(built: &Path) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("lv-m7-sandbox-")
        .tempdir()
        .expect("temp folder");
    let status = Command::new("icacls")
        .arg(dir.path())
        .args(["/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/q"])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("icacls");
    assert!(status.success(), "icacls failed");
    for entry in std::fs::read_dir(built.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        let ext = path.extension().and_then(OsStr::to_str).unwrap_or("");
        if ext.eq_ignore_ascii_case("dll") || path == built {
            std::fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
        }
    }
    let exe = dir.path().join("lv-worker.exe");
    assert!(folder_readable(&exe).unwrap());
    (dir, exe)
}

/// Whether this process runs at low integrity inside a Job, as [`run_test_at_low_integrity`]
/// starts it.
pub fn is_low_integrity_in_job() -> bool {
    let mut in_job = BOOL::default();
    // SAFETY: pseudo handle of our own process; a null Job handle asks about any Job.
    unsafe { IsProcessInJob(GetCurrentProcess(), HANDLE::default(), &mut in_job) }
        .expect("IsProcessInJob");
    own_integrity() == SECURITY_MANDATORY_LOW_RID as u32 && in_job.as_bool()
}

/// The integrity level RID of our own process.
pub fn own_integrity() -> u32 {
    let mut token = HANDLE::default();
    // SAFETY: pseudo handle of our own process; `token` receives a new handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .expect("OpenProcessToken");
    // SAFETY: OpenProcessToken succeeded, so the handle is ours.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let mut buf = [0u64; 16];
    let mut len = 0u32;
    // SAFETY: `buf` is 128 writable, 8-aligned bytes, enough for a label and its SID.
    unsafe {
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            std::mem::size_of_val(&buf) as u32,
            &mut len,
        )
    }
    .expect("GetTokenInformation");
    // SAFETY: on success `buf` starts with a TOKEN_MANDATORY_LABEL whose SID points into it.
    unsafe {
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        *GetSidSubAuthority(label.Label.Sid, u32::from(count) - 1)
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Our environment plus `name=value`, as a CreateProcess environment block.
fn environment_with(name: &str, value: &OsStr) -> Vec<u16> {
    let mut block = Vec::new();
    let vars = std::env::vars_os().filter(|(n, _)| n != name);
    for (n, v) in vars.chain(Some((OsString::from(name), value.to_owned()))) {
        block.extend(n.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Runs the test `test` (its full path, `--exact`) of this test binary again in a child
/// process at low integrity inside a kill-on-close Job, with `name=value` added to its
/// environment (the child test does nothing without it), and asserts that it passed within
/// a minute. Returns its output.
pub fn run_test_at_low_integrity(test: &str, name: &str, value: &OsStr) -> String {
    let token = low_integrity_token().expect("low integrity token");
    // SAFETY: unnamed Job with default security; the handle is ours.
    let job =
        unsafe { OwnedHandle::from_raw_handle(CreateJobObjectW(None, PCWSTR::null()).unwrap().0) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `limits` is a valid structure of the size passed.
    unsafe {
        SetInformationJobObject(
            HANDLE(job.as_raw_handle()),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    }
    .expect("SetInformationJobObject");

    // The child's stdout and stderr, to show its log if it fails.
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: out-pointers to locals; on success both handles are new and ours.
    unsafe { CreatePipe(&mut read, &mut write, None, 0) }.expect("CreatePipe");
    // SAFETY: CreatePipe succeeded, so both handles are ours.
    let (read, write) = unsafe {
        (
            std::fs::File::from_raw_handle(read.0),
            OwnedHandle::from_raw_handle(write.0),
        )
    };
    let write_raw = HANDLE(write.as_raw_handle());
    // SAFETY: valid pipe handle we own.
    unsafe { SetHandleInformation(write_raw, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
        .expect("SetHandleInformation");

    let test_exe = std::env::current_exe().unwrap();
    let mut command_line = wide(OsStr::new(&format!(
        "\"{}\" --exact {test} --nocapture --test-threads=1",
        test_exe.display()
    )));
    let application = wide(test_exe.as_os_str());
    let environment = environment_with(name, value);
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESTDHANDLES,
        hStdOutput: write_raw,
        hStdError: write_raw,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: NUL-terminated strings and a double-NUL-terminated environment that outlive
    // the call; `info` receives two new handles.
    unsafe {
        CreateProcessAsUserW(
            HANDLE(token.as_raw_handle()),
            PCWSTR(application.as_ptr()),
            PWSTR(command_line.as_mut_ptr()),
            None,
            None,
            BOOL::from(true),
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast::<c_void>()),
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    }
    .expect("CreateProcessAsUserW");
    // SAFETY: CreateProcessAsUserW returned these handles to us.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess.0),
            OwnedHandle::from_raw_handle(info.hThread.0),
        )
    };
    drop(write);
    // SAFETY: valid handles; the child is suspended and has run no code.
    unsafe { AssignProcessToJobObject(HANDLE(job.as_raw_handle()), info.hProcess) }
        .expect("AssignProcessToJobObject");
    // SAFETY: valid thread handle of the suspended main thread.
    assert_ne!(
        unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) },
        u32::MAX
    );

    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        let _ = { read }.read_to_string(&mut output);
        output
    });
    // SAFETY: valid process handle; the wait is bounded.
    let ended = unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 60_000) };
    if ended != WAIT_OBJECT_0 {
        // SAFETY: valid Job handle.
        let _ = unsafe { TerminateJobObject(HANDLE(job.as_raw_handle()), 1) };
    }
    let mut code = 0u32;
    // SAFETY: valid process handle; `code` receives the exit code.
    unsafe { GetExitCodeProcess(HANDLE(process.as_raw_handle()), &mut code) }
        .expect("GetExitCodeProcess");
    let output = reader.join().unwrap_or_default();
    assert_eq!(ended, WAIT_OBJECT_0, "the child test hung:\n{output}");
    assert_eq!(code, 0, "the child test failed:\n{output}");
    assert!(
        output.contains("test result: ok. 1 passed"),
        "the child test did not run:\n{output}"
    );
    output
}
