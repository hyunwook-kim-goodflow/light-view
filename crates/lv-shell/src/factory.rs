//! The class factory of the thumbnail handler.

use std::ffi::c_void;

use windows::core::{implement, IUnknown, Interface, Result, GUID};
use windows::Win32::Foundation::{BOOL, CLASS_E_NOAGGREGATION, E_POINTER};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::Shell::IThumbnailProvider;

use crate::thumbnail::Provider;
use crate::{guarded, lock_server, worker_exe, ObjectCount};

#[implement(IClassFactory)]
pub(crate) struct Factory {
    _count: ObjectCount,
}

impl Factory {
    pub(crate) fn new() -> Factory {
        Factory {
            _count: ObjectCount::new(),
        }
    }
}

#[allow(non_snake_case)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Option<&IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> Result<()> {
        guarded(|| {
            if iid.is_null() || out.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: checked non-null; COM contract: writable.
            unsafe { *out = std::ptr::null_mut() };
            if outer.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let provider: IThumbnailProvider = Provider::new(worker_exe()?).into();
            // SAFETY: COM contract: `iid` points to a GUID and `out` is writable.
            unsafe { provider.query(iid, out).ok() }
        })
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        guarded(|| {
            lock_server(lock.as_bool());
            Ok(())
        })
    }
}
