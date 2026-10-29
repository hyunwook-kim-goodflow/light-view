//! SPIKE (M7 Task 1, ruling R1): the smallest per-user thumbnail handler that answers the
//! question "does Explorer call a handler registered under HKCU only, in which host, at what
//! integrity, and can that host start the sandboxed worker?". Task 4 rewrites this crate.
//!
//! `GetThumbnail` logs the host process (name, PID, integrity, AppContainer or not) to
//! `log.txt` in the parent folder of the folder holding this DLL, starts one sandboxed
//! `lv-worker.exe` (from this DLL's folder) with a copy of lv-app's start code, sends it the
//! stream's bytes as `DecodeHeif`, logs the outcome and the time, and answers a lime bitmap
//! whatever the worker said.
//!
//! Task 1b adds the preview class (`preview.rs`): a magenta child window whose `DoPreview`
//! logs its host the same way and runs one worker decode.

mod appcontainer;
mod host;
mod preview;
mod provider;
mod sandbox;
mod worker;

use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use windows::core::{implement, Interface, GUID, HRESULT};
use windows::Win32::Foundation::{
    BOOL, CLASS_E_CLASSNOTAVAILABLE, E_POINTER, HINSTANCE, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

/// Thumbnail handler CLSID (Global Constraints).
pub const CLSID_THUMBNAIL: GUID = GUID::from_u128(0x6A77F04F_33D3_40F0_885E_AA8CC5CC403C);
/// Preview handler CLSID (Global Constraints).
pub const CLSID_PREVIEW: GUID = GUID::from_u128(0x663472B2_A0DD_43AA_B3F1_97CEB0598154);

static MODULE: AtomicIsize = AtomicIsize::new(0);
/// Live COM objects (factories and providers), for `DllCanUnloadNow`.
static OBJECTS: AtomicUsize = AtomicUsize::new(0);

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

/// This DLL's module handle.
pub(crate) fn module_handle() -> HMODULE {
    HMODULE(MODULE.load(Ordering::SeqCst) as *mut c_void)
}

/// This DLL's full path.
pub(crate) fn dll_path() -> PathBuf {
    let module = module_handle();
    let mut buf = vec![0u16; 32 * 1024];
    // SAFETY: valid module handle of this DLL (set in DllMain) and a writable buffer.
    let len = unsafe { GetModuleFileNameW(module, &mut buf) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
}

/// Appends one line to the spike log (`<dll folder>\..\log.txt`). Failures are ignored.
pub(crate) fn spike_log(msg: &str) {
    let dll = dll_path();
    let Some(root) = dll.parent().and_then(|bin| bin.parent()) else {
        return;
    };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("log.txt"))
    {
        let _ = writeln!(f, "{ms} pid={pid} {msg}");
    }
}

#[no_mangle]
extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(module.0 as isize, Ordering::SeqCst);
    }
    BOOL::from(true)
}

/// # Safety
/// COM contract: valid GUID pointers and a writable out-pointer.
#[no_mangle]
unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    *out = std::ptr::null_mut();
    let preview = if *clsid == CLSID_THUMBNAIL {
        false
    } else if *clsid == CLSID_PREVIEW {
        true
    } else {
        return CLASS_E_CLASSNOTAVAILABLE;
    };
    spike_log(&format!("DllGetClassObject(preview={preview})"));
    let factory: IClassFactory = Factory {
        preview,
        _count: ObjectCount::new(),
    }
    .into();
    factory.query(iid, out)
}

#[no_mangle]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[implement(IClassFactory)]
struct Factory {
    /// Creates preview handlers (else thumbnail providers).
    preview: bool,
    _count: ObjectCount,
}

#[allow(non_snake_case)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Option<&windows::core::IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if self.preview {
            preview::create(outer, iid, out)
        } else {
            provider::create(outer, iid, out)
        }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}
