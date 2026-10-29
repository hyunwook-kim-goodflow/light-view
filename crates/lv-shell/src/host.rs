//! SPIKE: who is hosting us (process name, integrity, AppContainer), for the log.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TokenIsAppContainer, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// "exe=<path> integrity=<name> (0x<rid>) appcontainer=<bool>" for the current process.
pub fn describe_self() -> String {
    let mut buf = vec![0u16; 32 * 1024];
    // SAFETY: None means the process's exe; writable buffer.
    let len = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    let exe = String::from_utf16_lossy(&buf[..len]);
    // SAFETY: pseudo handle of our own process.
    let token = describe_process(unsafe { GetCurrentProcess() });
    format!(
        "exe={exe} cmdline={:?} {token}",
        std::env::args().collect::<Vec<_>>().join(" ")
    )
}

/// "integrity=<name> (0x<rid>) appcontainer=<bool>" for `process` (needs query access).
pub fn describe_process(process: HANDLE) -> String {
    let mut token = HANDLE::default();
    // SAFETY: valid process handle; `token` receives a new handle on success.
    if let Err(e) = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } {
        return format!("token=? ({e})");
    }
    // SAFETY: OpenProcessToken succeeded, so the handle is new and ours.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let token = HANDLE(token.as_raw_handle());
    let integrity = integrity(token).unwrap_or_else(|e| format!("? ({e})"));
    let mut is_app = 0u32;
    let mut len = 0u32;
    // SAFETY: a u32 out-buffer of the size passed.
    let app = unsafe {
        GetTokenInformation(
            token,
            TokenIsAppContainer,
            Some(std::ptr::from_mut(&mut is_app).cast()),
            4,
            &mut len,
        )
    }
    .map(|()| (is_app != 0).to_string())
    .unwrap_or_else(|e| format!("? ({e})"));
    format!("integrity={integrity} appcontainer={app}")
}

fn integrity(token: HANDLE) -> windows::core::Result<String> {
    let mut buf = [0u64; 16];
    let mut len = 0u32;
    // SAFETY: an 8-byte aligned buffer of the size passed.
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            std::mem::size_of_val(&buf) as u32,
            &mut len,
        )?
    };
    // SAFETY: GetTokenInformation filled a TOKEN_MANDATORY_LABEL whose SID points into `buf`.
    let rid = unsafe {
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        *GetSidSubAuthority(sid, u32::from(count) - 1)
    };
    let name = match rid {
        0x0000 => "untrusted",
        0x1000 => "low",
        0x2000 => "medium",
        0x2100 => "medium-plus",
        0x3000 => "high",
        0x4000 => "system",
        _ => "other",
    };
    Ok(format!("{name}(0x{rid:04x})"))
}
