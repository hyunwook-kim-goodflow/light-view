//! Safe removal of a drive: the folder handle's device-event registration and what each
//! `WM_DEVICECHANGE` event asks of the watch.

use std::io;
use std::path::Path;

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    RegisterDeviceNotificationW, UnregisterDeviceNotification, DBT_DEVICEQUERYREMOVE,
    DBT_DEVICEQUERYREMOVEFAILED, DBT_DEVICEREMOVECOMPLETE, DBT_DEVICEREMOVEPENDING,
    DBT_DEVTYP_HANDLE, DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_HANDLE, HDEVNOTIFY,
};

/// A folder handle's device-event registration (`RegisterDeviceNotificationW` with
/// `DBT_DEVTYP_HANDLE`); unregistered when dropped.
pub struct Registration {
    /// The `HDEVNOTIFY`, as events name it in `dbch_hdevnotify`.
    pub(super) id: isize,
    pub(super) unregister: Option<Box<dyn FnOnce()>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(unregister) = self.unregister.take() {
            unregister();
        }
    }
}

/// How `Watching` registers a folder handle (`dir_handle`, of the folder `dir`) for the
/// device events sent to `hwnd`; tests swap it.
pub(super) type Register = Box<dyn Fn(&Path, isize, HWND) -> io::Result<Registration>>;

/// Registers `dir_handle` so `hwnd` gets `WM_DEVICECHANGE` when its drive is removed.
pub(super) fn register_device(
    _dir: &Path,
    dir_handle: isize,
    hwnd: HWND,
) -> io::Result<Registration> {
    let filter = DEV_BROADCAST_HANDLE {
        dbch_size: std::mem::size_of::<DEV_BROADCAST_HANDLE>() as u32,
        dbch_devicetype: DBT_DEVTYP_HANDLE.0,
        dbch_handle: HANDLE(dir_handle as *mut _),
        ..Default::default()
    };
    // SAFETY: `filter` is a live DEV_BROADCAST_HANDLE naming an open directory handle:
    // the watch thread closes it only after it is told to stop, which happens after this
    // call returns.
    let h = unsafe {
        RegisterDeviceNotificationW(
            HANDLE(hwnd.0),
            std::ptr::from_ref(&filter).cast(),
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    }?;
    let id = h.0 as isize;
    Ok(Registration {
        id,
        unregister: Some(Box::new(move || {
            // SAFETY: `id` is the HDEVNOTIFY returned above, unregistered once (here).
            if let Err(e) = unsafe { UnregisterDeviceNotification(HDEVNOTIFY(id as *mut _)) } {
                eprintln!("light-view: could not unregister the device events: {e}");
            }
        })),
    })
}

/// What a `WM_DEVICECHANGE` event for a folder handle asks of the watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceStep {
    /// Stop the watch and close its handle; keep the registration for the outcome.
    Release,
    /// The removal was cancelled: watch the same folder again.
    Restart,
    /// The drive is gone (or going): unregister and stay unwatched.
    Forget,
    /// Not ours, or nothing to do.
    Ignore,
}

/// The step for device `event`: `live` when it names the running watch's registration,
/// `released` when it names the one kept after a query-remove. Anything else (a stale
/// registration, another device) is ignored.
pub fn device_step(event: u32, live: bool, released: bool) -> DeviceStep {
    match event {
        DBT_DEVICEQUERYREMOVE if live => DeviceStep::Release,
        DBT_DEVICEQUERYREMOVEFAILED if released => DeviceStep::Restart,
        DBT_DEVICEREMOVEPENDING | DBT_DEVICEREMOVECOMPLETE if live || released => {
            DeviceStep::Forget
        }
        _ => DeviceStep::Ignore,
    }
}
