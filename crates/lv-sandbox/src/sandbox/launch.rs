//! One CreateProcessAsUserW call for [`super::start_suspended`]: the wide strings, the
//! environment block and the PROC_THREAD_ATTRIBUTE_LIST with the inherited handles, the
//! best-effort mitigations, the AppContainer and the Job.

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::OwnedHandle;
use std::path::Path;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, HANDLE};
use windows::Win32::Security::SECURITY_CAPABILITIES;
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::{log, raw};
use crate::appcontainer::Container;

/// PROCESS_CREATION_MITIGATION_POLICY_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON.
const WIN32K_SYSTEM_CALL_DISABLE: u64 = 1 << 28;
/// PROCESS_CREATION_CHILD_PROCESS_RESTRICTED.
const CHILD_PROCESS_RESTRICTED: u32 = 1;

/// Wide strings for one CreateProcessAsUserW call.
pub(super) struct Launch {
    application: Vec<u16>,
    command_line: Vec<u16>,
    directory: Vec<u16>,
    /// `SystemRoot` alone (an AppContainer start uses [`Container::environment`]).
    environment: Vec<u16>,
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// An environment block (`NAME=value` strings, then an empty one) of these variables of
/// ours that are set.
fn environment_block(names: &[&str]) -> Vec<u16> {
    let mut block = Vec::new();
    for name in names {
        let value = match std::env::var_os(name) {
            Some(value) => value,
            None if *name == "SystemRoot" => r"C:\Windows".into(),
            None => continue,
        };
        block.extend(OsStr::new(name).encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

impl Launch {
    pub(super) fn new(exe: &Path, max_pixels: u64, extra_args: &[&str]) -> Result<Launch, String> {
        let directory = exe
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", exe.display()))?;
        let mut command_line = format!("\"{}\" --max-pixels {max_pixels}", exe.display());
        for arg in extra_args {
            command_line.push(' ');
            command_line.push_str(arg);
        }
        Ok(Launch {
            application: wide(exe.as_os_str()),
            command_line: wide(OsStr::new(&command_line)),
            directory: wide(directory.as_os_str()),
            environment: environment_block(&["SystemRoot"]),
        })
    }

    /// Creates the process suspended, inheriting only `child_ends` (stdin, stdout, stderr),
    /// in AppContainer `container` (with no capabilities) when given, and inside Job `job`
    /// from the start when given.
    pub(super) fn create(
        &self,
        token: &OwnedHandle,
        child_ends: [HANDLE; 3],
        mitigations: bool,
        container: Option<&Container>,
        job: Option<HANDLE>,
    ) -> Result<PROCESS_INFORMATION, String> {
        let win32k = WIN32K_SYSTEM_CALL_DISABLE;
        let child_policy = CHILD_PROCESS_RESTRICTED;
        let capabilities = container.map(|container| SECURITY_CAPABILITIES {
            AppContainerSid: container.sid().as_psid(),
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        });
        let jobs = job.map(|job| [job]);
        let count = 1
            + if mitigations { 2 } else { 0 }
            + u32::from(capabilities.is_some())
            + u32::from(jobs.is_some());
        let mut attributes = AttributeList::new(count)?;
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
        if let Some(capabilities) = &capabilities {
            // A failure here fails the start, and `spawn` retries without the container.
            attributes.set(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                std::ptr::from_ref(capabilities).cast(),
                std::mem::size_of_val(capabilities),
            )?;
        }
        if let Some(jobs) = &jobs {
            // A failure here fails the start, and `start_suspended` retries without it.
            attributes.set(
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                jobs.as_ptr().cast(),
                std::mem::size_of_val(jobs),
            )?;
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
        let container_environment = container.map(|c| environment_block(c.environment()));
        let environment = container_environment.as_ref().unwrap_or(&self.environment);
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: every string is NUL-terminated (the environment double-NUL-terminated)
        // and, like the attribute list and what it points to (the handle array, the
        // policies, the capabilities and their SID, the Job array), outlives the call.
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
                Some(environment.as_ptr().cast::<c_void>()),
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
        let mut buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: `buf` holds at least `size` writable bytes.
        unsafe {
            InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast()),
                count,
                0,
                &mut size,
            )
        }
        .map_err(|e| format!("InitializeProcThreadAttributeList: {e}"))?;
        // Only an initialized list gets the Drop that deletes it.
        Ok(AttributeList { buf })
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
