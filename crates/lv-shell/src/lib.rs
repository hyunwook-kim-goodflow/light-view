//! light-view's Explorer extension: a COM in-process server (pure Rust, no C code) with the
//! HEIC thumbnail handler (`IThumbnailProvider` + `IInitializeWithStream`) and, for all-users
//! installs, the HEIC preview handler (`IPreviewHandler` and friends, in `prevhost.exe`) and
//! the HEIC details handler (`IPropertyStore`, in `explorer.exe` and the SYSTEM search
//! indexer; [`properties`] only parses bytes and never reaches a worker).
//!
//! The thumbnail and preview handlers never decode: they read the file's bytes from
//! Explorer's stream (up to the shell's 64 MiB cap), have one sandboxed `lv-worker.exe` from
//! this DLL's folder make the image ([`lv_sandbox::run_once`], ruling R3) and show it as a
//! 32 bpp bitmap. Any failure is an error HRESULT (Explorer then shows the file's icon), an
//! empty preview or no details, and is not logged.
//!
//! Per-user installs load this DLL into `explorer.exe` itself (`DisableProcessIsolation`), so
//! no panic may cross a COM boundary ([`guarded`]) and `DllCanUnloadNow` refuses while any
//! object, server lock, preview window or helper thread is alive ([`can_unload`]).

mod bitmap;
mod dll;
mod factory;
mod preview;
mod properties;
mod stream;
mod thumbnail;

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::core::{Result, GUID, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};

/// Thumbnail handler CLSID (Global Constraints).
pub const CLSID_THUMBNAIL: GUID = GUID::from_u128(0x6A77F04F_33D3_40F0_885E_AA8CC5CC403C);
/// Preview handler CLSID (Global Constraints).
pub const CLSID_PREVIEW: GUID = GUID::from_u128(0x663472B2_A0DD_43AA_B3F1_97CEB0598154);
/// Details (property) handler CLSID (Global Constraints); all-users installs only.
pub const CLSID_DETAILS: GUID = GUID::from_u128(0x0EA2AA38_AE7B_4906_9BD9_605EA0348914);

/// Live COM objects of this DLL (class factories, handlers, and the state a preview window
/// and its background thread share).
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
/// `IClassFactory::LockServer(TRUE)` calls not yet undone.
static LOCKS: AtomicUsize = AtomicUsize::new(0);

/// Counts one COM object in [`OBJECTS`] while it lives; a field of every object.
pub(crate) struct ObjectCount;

impl ObjectCount {
    pub(crate) fn new() -> ObjectCount {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        ObjectCount
    }
}

impl Drop for ObjectCount {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) fn lock_server(lock: bool) {
    if lock {
        LOCKS.fetch_add(1, Ordering::SeqCst);
    } else {
        // An unbalanced unlock is the caller's bug; never wrap below zero.
        let _ = LOCKS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
    }
}

/// Whether nothing of this DLL is in use: no object, no server lock and no helper thread
/// (those of [`lv_sandbox::run_once`] may outlive a `GetThumbnail` by a moment, and a
/// preview's background thread outlives `DoPreview`; both are counted by lv-sandbox).
pub(crate) fn can_unload() -> bool {
    OBJECTS.load(Ordering::SeqCst) == 0
        && LOCKS.load(Ordering::SeqCst) == 0
        && lv_sandbox::live_threads() == 0
}

/// Runs `f`, turning a panic into `E_FAIL`: no panic may unwind into a COM caller.
pub(crate) fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(E_FAIL.into()))
}

/// The module holding this code (the DLL, or a test exe).
pub(crate) fn this_module() -> Result<HMODULE> {
    static ANCHOR: u8 = 0;
    let mut module = HMODULE::default();
    // SAFETY: FROM_ADDRESS takes any address inside the module (a static of ours);
    // UNCHANGED_REFCOUNT leaves the module's reference count alone, so nothing to release.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(std::ptr::from_ref(&ANCHOR).cast()),
            &mut module,
        )?
    };
    Ok(module)
}

/// `lv-worker.exe` in the folder of the module holding this code (the DLL, or a test exe).
pub(crate) fn worker_exe() -> Result<PathBuf> {
    #[cfg(test)]
    tests::WORKER_EXE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let module = this_module()?;
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: a valid module handle of this process and a writable buffer.
        let len = unsafe { GetModuleFileNameW(module, &mut buf) } as usize;
        if len == 0 {
            return Err(windows::core::Error::from_win32());
        }
        if len < buf.len() {
            let dll = PathBuf::from(OsString::from_wide(&buf[..len]));
            return Ok(dll.with_file_name("lv-worker.exe"));
        }
        // Truncated: try again with more room (long paths are up to 32 767 characters).
        if buf.len() >= 32 * 1024 {
            return Err(E_FAIL.into());
        }
        buf.resize(buf.len() * 4, 0);
    }
}

/// The HEIF box writer of lv-heif's thumbnail tests, shared for the details fixtures.
#[cfg(test)]
#[path = "../../lv-heif/tests/support/heif_build.rs"]
mod heif_build;
#[cfg(test)]
mod tests;
