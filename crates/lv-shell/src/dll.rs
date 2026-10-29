//! The DLL's exported COM entry points.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

use windows::core::{Interface, GUID, HRESULT};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_FAIL, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::IClassFactory;

use crate::factory::Factory;
use crate::{can_unload, CLSID_PREVIEW, CLSID_THUMBNAIL};

/// Hands out the class factory for [`CLSID_THUMBNAIL`] or [`CLSID_PREVIEW`].
///
/// # Safety
/// COM contract: `clsid` and `iid` point to GUIDs and `out` is writable (null pointers are
/// refused).
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        if out.is_null() {
            return E_POINTER;
        }
        // SAFETY: checked non-null; COM contract: writable. Nulled before any failure.
        unsafe { *out = std::ptr::null_mut() };
        if clsid.is_null() || iid.is_null() {
            return E_POINTER;
        }
        // SAFETY: checked non-null; the caller guarantees they are valid.
        unsafe {
            if *clsid != CLSID_THUMBNAIL && *clsid != CLSID_PREVIEW {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            let factory: IClassFactory = Factory::new(*clsid).into();
            factory.query(iid, out)
        }
    }))
    .unwrap_or(E_FAIL)
}

/// `S_OK` only when no object, server lock or helper thread of this DLL is alive; then the
/// preview window class (no window of it is left) is unregistered too.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    let unloadable = || {
        let yes = can_unload();
        if yes {
            // No window is left: unregister the preview window class before the DLL goes.
            crate::preview::unregister_class();
        }
        yes
    };
    match catch_unwind(unloadable) {
        Ok(true) => S_OK,
        _ => S_FALSE,
    }
}
