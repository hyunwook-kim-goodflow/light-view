//! The DLL's exported COM entry points.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

use windows::core::{Interface, GUID, HRESULT};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_FAIL, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::IClassFactory;

use crate::factory::Factory;
use crate::{can_unload, CLSID_THUMBNAIL};

/// Hands out the class factory for [`CLSID_THUMBNAIL`].
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
            if *clsid != CLSID_THUMBNAIL {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            let factory: IClassFactory = Factory::new().into();
            factory.query(iid, out)
        }
    }))
    .unwrap_or(E_FAIL)
}

/// `S_OK` only when no object, server lock or worker helper thread of this DLL is alive.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    match catch_unwind(can_unload) {
        Ok(true) => S_OK,
        _ => S_FALSE,
    }
}
