//! The class factory of the thumbnail and the preview handler.

use std::ffi::c_void;

use windows::core::{implement, IUnknown, Interface, Result, GUID};
use windows::Win32::Foundation::{BOOL, CLASS_E_NOAGGREGATION, E_POINTER};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::Shell::{IPreviewHandler, IThumbnailProvider};

use crate::preview::Handler;
use crate::thumbnail::Provider;
use crate::{guarded, lock_server, worker_exe, ObjectCount, CLSID_PREVIEW};

#[implement(IClassFactory)]
pub(crate) struct Factory {
    /// The class this factory makes: [`CLSID_PREVIEW`], else the thumbnail handler.
    clsid: GUID,
    _count: ObjectCount,
}

impl Factory {
    /// The factory of `clsid`, which the caller has checked is one of ours.
    pub(crate) fn new(clsid: GUID) -> Factory {
        Factory {
            clsid,
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
            let exe = worker_exe()?;
            let made: IUnknown = if self.clsid == CLSID_PREVIEW {
                IPreviewHandler::from(Handler::new(exe)).cast()?
            } else {
                IThumbnailProvider::from(Provider::new(exe)).cast()?
            };
            // SAFETY: COM contract: `iid` points to a GUID and `out` is writable.
            unsafe { made.query(iid, out).ok() }
        })
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        guarded(|| {
            lock_server(lock.as_bool());
            Ok(())
        })
    }
}
