//! Folder change detection (spec §5.x, rulings R19/R20): a [`FolderWatch`] thread waits
//! on `ReadDirectoryChangesW` for the shown folder and, 200 ms after a burst of changes
//! settles, posts `WM_APP_FOLDER_CHANGED`. The window then lists the folder again,
//! unless a dialog is open or a save runs: the rescan then waits for it ([`Watching`],
//! ruling R14).
//!
//! The UI thread never touches the disk for a rescan: it takes a
//! [`RescanJob`](crate::viewer::RescanJob) (the folder, the current photo and the
//! identities to check), a background thread runs it (a slow or stalled network folder
//! blocks only that thread) and posts `WM_APP_RESCAN_DONE`, and the UI thread applies
//! the result unless the viewer moved on meanwhile (then it rescans again). One rescan
//! runs at a time; changes reported meanwhile give exactly one more after it. A slow
//! rescan stretches the debounce to twice its duration, so a busy network folder is not
//! listed back to back.
//!
//! Safe removal of a drive (M4 task 7): the folder handle is registered for device
//! events (`RegisterDeviceNotificationW`, `DBT_DEVTYP_HANDLE`, [`Registration`]). On
//! `DBT_DEVICEQUERYREMOVE` the watch stops and its handle closes before the window says
//! yes; `DBT_DEVICEQUERYREMOVEFAILED` watches the same folder again and
//! `DBT_DEVICEREMOVECOMPLETE` leaves it unwatched until the viewer moves to another
//! folder ([`device_step`]). The shown photo stays; saving or deleting it later meets the
//! identity check like any changed file.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lv_core::identity::to_extended_path;
use lv_core::listing::same_path;
use windows::Win32::Foundation::{
    ERROR_NOTIFY_ENUM_DIR, HANDLE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, RegisterDeviceNotificationW, UnregisterDeviceNotification, DBT_DEVICEQUERYREMOVE,
    DBT_DEVICEQUERYREMOVEFAILED, DBT_DEVICEREMOVECOMPLETE, DBT_DEVICEREMOVEPENDING,
    DBT_DEVTYP_HANDLE, DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_HANDLE, DEV_BROADCAST_HDR,
    HDEVNOTIFY, WM_APP,
};

use crate::commands;
use crate::input;
use crate::overlay::{Overlay, Toast};
use crate::strings::S;
use crate::viewer::{RescanFound, RescanNotice};
use crate::window::App;

/// Posted by a [`FolderWatch`] once a burst of changes settled. (`WM_APP + 3` is
/// `WM_APP_DELETE_DONE`.)
pub const WM_APP_FOLDER_CHANGED: u32 = WM_APP + 4;
/// Posted by a rescan thread once its result is in `Watching`'s channel.
pub const WM_APP_RESCAN_DONE: u32 = WM_APP + 5;
/// Quiet time after the last change of a burst.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// A steady stream of changes (a long copy) still refreshes the list this often.
const MAX_DELAY: Duration = Duration::from_secs(1);
/// File names (added, removed, renamed), sizes and write times; not sub-folders.
const FILTER: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0 | FILE_NOTIFY_CHANGE_SIZE.0 | FILE_NOTIFY_CHANGE_LAST_WRITE.0,
);
/// How long a query-remove waits for the watch thread to close the folder handle. The
/// cancelled read of a local drive ends at once; a stalled one must not hold up the
/// window (the system waits for its answer).
const RELEASE_WAIT: Duration = Duration::from_secs(1);
/// The change records are never read (any change means "list again"), so a small
/// buffer does; an overflow is reported as a change too.
const BUFFER_WORDS: usize = 1024;

/// Coalesces a burst of changes into one notice, `DEBOUNCE` after its last change but
/// no later than `MAX_DELAY` after its first; both stretch to twice the last rescan's
/// duration (`slow`) when that is longer.
#[derive(Debug, Default)]
pub struct Debounce {
    first: Option<Instant>,
    last: Option<Instant>,
    slow: Duration,
}

impl Debounce {
    pub fn event(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// How long the last rescan took.
    pub fn set_slow(&mut self, took: Duration) {
        self.slow = took;
    }

    /// When the pending burst is due; `None` with nothing pending.
    pub fn due(&self) -> Option<Instant> {
        let quiet = DEBOUNCE.max(self.slow * 2);
        let cap = MAX_DELAY.max(self.slow * 2);
        Some((self.last? + quiet).min(self.first? + cap))
    }

    /// True, once, when the pending burst is due at `now`.
    pub fn fire(&mut self, now: Instant) -> bool {
        if self.due().is_some_and(|due| now >= due) {
            self.first = None;
            self.last = None;
            return true;
        }
        false
    }
}

/// Watches one folder on its own thread until dropped.
pub struct FolderWatch {
    stop: Arc<OwnedHandle>,
    /// The last rescan's duration in ms, read by the thread's `Debounce`.
    slow_ms: Arc<AtomicU64>,
    /// The thread's directory handle (it owns and closes it), as a number: registered for
    /// device events and matched against their `dbch_handle`.
    dir_handle: isize,
    /// The device-event registration of `dir_handle`; dropped (unregistered) before the
    /// thread is told to stop, so before the handle closes.
    registration: Option<Registration>,
    /// Disconnected once the thread ended, so once the handle is closed.
    ended: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

/// The overlapped read: the system writes into it until the read completes or is
/// cancelled, so it lives in a `Box` (it never moves) owned by the watch thread, which
/// waits for the read to end before dropping it.
struct Read {
    ov: OVERLAPPED,
    buf: [u32; BUFFER_WORDS],
}

impl FolderWatch {
    /// Starts watching `dir`; `WM_APP_FOLDER_CHANGED` goes to `hwnd` (posting to a
    /// window that is gone does nothing). Fails where changes cannot be watched (some
    /// network locations): the caller then runs without a watch.
    pub fn start(dir: &Path, hwnd: HWND) -> io::Result<FolderWatch> {
        let target = hwnd.0 as isize;
        FolderWatch::start_with(dir, move || post(target, WM_APP_FOLDER_CHANGED))
    }

    /// Like `start`, calling `notify` instead; `notify` is dropped when the thread ends.
    fn start_with(dir: &Path, notify: impl Fn() + Send + 'static) -> io::Result<FolderWatch> {
        let stop = Arc::new(new_event()?);
        let slow_ms = Arc::new(AtomicU64::new(0));
        let (thread_stop, thread_slow) = (stop.clone(), slow_ms.clone());
        let dir = to_extended_path(dir);
        let (started_tx, started) = std::sync::mpsc::sync_channel(1);
        let (ended_tx, ended) = std::sync::mpsc::sync_channel::<()>(0);
        // The thread owns everything its pending read touches: the directory handle, the
        // event and the boxed buffer. Dropping a watch does not wait for it (a stalled
        // network read could hold it up); `release` waits a bounded time.
        let thread = std::thread::Builder::new()
            .name("folder-watch".into())
            .spawn(move || {
                let (file, io_done, mut read) = match start_reading(&dir) {
                    Ok(started) => started,
                    Err(e) => {
                        let _ = started_tx.send(Err(e));
                        return;
                    }
                };
                let _ = started_tx.send(Ok(file.as_raw_handle() as isize));
                let run = catch_unwind(AssertUnwindSafe(|| {
                    watch_loop(
                        &file,
                        &io_done,
                        &thread_stop,
                        &thread_slow,
                        &mut read,
                        &notify,
                    );
                }));
                if run.is_err() {
                    // A read may still be pending: the system may still write into
                    // `read` and set `io_done`, and closing `file` under it does not wait.
                    // Leak all three rather than free memory the kernel writes to.
                    std::mem::forget(read);
                    std::mem::forget(io_done);
                    std::mem::forget(file);
                } else {
                    // The read was cancelled and waited for: the handle closes now.
                    drop(read);
                    drop(file);
                }
                drop(ended_tx);
            })?;
        // The first read tells whether this location reports changes at all.
        let dir_handle = started
            .recv()
            .unwrap_or_else(|_| Err(io::Error::other("the folder watch thread ended")))?;
        Ok(FolderWatch {
            stop,
            slow_ms,
            dir_handle,
            registration: None,
            ended,
            thread: Some(thread),
        })
    }

    /// Stops the thread and waits up to `wait` for it to cancel its read and close the
    /// folder handle; false if it did not in time (it still closes the handle later).
    /// Take the registration out first: dropping it unregisters, as `drop` does.
    fn release(&mut self, wait: Duration) -> bool {
        signal(&self.stop);
        match self.ended.recv_timeout(wait) {
            Err(RecvTimeoutError::Timeout) => false,
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
                true
            }
        }
    }

    /// How long the last rescan of this folder took: the debounce stretches to twice it.
    pub fn set_last_rescan(&self, took: Duration) {
        let ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX);
        self.slow_ms.store(ms, Ordering::Relaxed);
    }
}

impl Drop for FolderWatch {
    /// Unregisters the folder handle from device events, then tells the thread to stop:
    /// it cancels its read (`CancelIoEx`), waits for it, closes the handle and ends on its
    /// own. Not joined.
    fn drop(&mut self) {
        drop(self.registration.take());
        signal(&self.stop);
    }
}

fn signal(event: &OwnedHandle) {
    // SAFETY: the event handle is owned by the caller and alive.
    let _ = unsafe { SetEvent(HANDLE(event.as_raw_handle())) };
}

/// A folder handle's device-event registration (`RegisterDeviceNotificationW` with
/// `DBT_DEVTYP_HANDLE`); unregistered when dropped.
pub struct Registration {
    /// The `HDEVNOTIFY`, as events name it in `dbch_hdevnotify`.
    id: isize,
    unregister: Option<Box<dyn FnOnce()>>,
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
type Register = Box<dyn Fn(&Path, isize, HWND) -> io::Result<Registration>>;

/// Registers `dir_handle` so `hwnd` gets `WM_DEVICECHANGE` when its drive is removed.
fn register_device(_dir: &Path, dir_handle: isize, hwnd: HWND) -> io::Result<Registration> {
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

/// Opens `dir` for overlapped change reads and starts the first one.
fn start_reading(dir: &Path) -> io::Result<(File, OwnedHandle, Box<Read>)> {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let file = OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY.0)
        .share_mode(share.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OVERLAPPED.0)
        .open(dir)?;
    let io_done = new_event()?;
    let mut read = Box::new(Read {
        ov: OVERLAPPED {
            hEvent: HANDLE(io_done.as_raw_handle()),
            ..Default::default()
        },
        buf: [0; BUFFER_WORDS],
    });
    read.issue(&file)?;
    Ok((file, io_done, read))
}

impl Read {
    /// Starts the next overlapped read of `dir`'s changes.
    fn issue(&mut self, dir: &File) -> io::Result<()> {
        let event = self.ov.hEvent;
        self.ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        // SAFETY: `dir` is a directory handle opened for overlapped I/O; the buffer and
        // the OVERLAPPED live in this boxed `Read`, which the watch thread keeps until
        // the read has completed or been cancelled and waited for (or leaks on a panic).
        unsafe {
            ReadDirectoryChangesW(
                HANDLE(dir.as_raw_handle()),
                self.buf.as_mut_ptr().cast(),
                std::mem::size_of_val(&self.buf) as u32,
                false,
                FILTER,
                None,
                Some(&mut self.ov),
                None,
            )
        }
        .map_err(io::Error::from)
    }

    /// The finished read's byte count (0: too many changes to report, which also means
    /// "something changed").
    fn result(&self, dir: &File, wait: bool) -> windows::core::Result<u32> {
        let mut bytes = 0;
        // SAFETY: `self.ov` is the OVERLAPPED of a read issued on `dir`; `bytes` is a
        // live local.
        unsafe { GetOverlappedResult(HANDLE(dir.as_raw_handle()), &self.ov, &mut bytes, wait) }
            .map(|()| bytes)
    }
}

fn new_event() -> io::Result<OwnedHandle> {
    // SAFETY: plain call; an unnamed auto-reset event, initially not set.
    let h = unsafe { CreateEventW(None, false, false, None) }?;
    // SAFETY: `h` is a new handle owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

fn post(target: isize, msg: u32) {
    if target == 0 {
        return;
    }
    // SAFETY: plain call; posting to a destroyed window just fails.
    unsafe {
        let _ = PostMessageW(HWND(target as *mut _), msg, WPARAM(0), LPARAM(0));
    }
}

fn watch_loop(
    dir: &File,
    io_done: &OwnedHandle,
    stop: &OwnedHandle,
    slow_ms: &AtomicU64,
    read: &mut Read,
    notify: &dyn Fn(),
) {
    let handles = [
        HANDLE(stop.as_raw_handle()),
        HANDLE(io_done.as_raw_handle()),
    ];
    let mut reading = true;
    let mut debounce = Debounce::default();
    loop {
        debounce.set_slow(Duration::from_millis(slow_ms.load(Ordering::Relaxed)));
        let timeout = debounce.due().map_or(INFINITE, |due| {
            let left = due.saturating_duration_since(Instant::now()).as_millis();
            u32::try_from(left)
                .unwrap_or(INFINITE - 1)
                .saturating_add(1)
        });
        let waiting = if reading { &handles[..] } else { &handles[..1] };
        // SAFETY: both handles are owned events alive for the whole loop.
        let woke = unsafe { WaitForMultipleObjects(waiting, false, timeout) };
        if woke == WAIT_OBJECT_0 {
            break;
        }
        if woke.0 == WAIT_OBJECT_0.0 + 1 {
            debounce.event(Instant::now());
            match read.result(dir, false) {
                Ok(_) => {}
                Err(e) if e.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => {}
                Err(e) => {
                    // The folder went away, or the location stopped reporting: one last
                    // rescan finds out what is left.
                    eprintln!("light-view: no longer watching the folder: {e}");
                    reading = false;
                }
            }
            if reading {
                if let Err(e) = read.issue(dir) {
                    eprintln!("light-view: no longer watching the folder: {e}");
                    reading = false;
                }
            }
        } else if woke != WAIT_TIMEOUT {
            eprintln!("light-view: folder watch wait failed");
            break;
        }
        if debounce.fire(Instant::now()) {
            notify();
        }
    }
    if reading {
        // SAFETY: cancels our own read on `dir`; the wait below lets it end before
        // `read` (its buffer and OVERLAPPED) can be dropped.
        unsafe {
            let _ = CancelIoEx(HANDLE(dir.as_raw_handle()), Some(&read.ov));
        }
        let _ = read.result(dir, true);
    }
}

/// The window's folder watch and its rescans: one due (waiting for a dialog or a save
/// to end, or for the running one) and the one running.
pub struct Watching {
    watch: Option<FolderWatch>,
    /// The watched folder, or the folder that could not be watched (or whose drive was
    /// removed).
    dir: Option<PathBuf>,
    /// The registration kept after a query-remove released the watch, until the removal
    /// completes or fails.
    released: Option<Registration>,
    register: Register,
    rescan_pending: bool,
    /// The id of the rescan running for this folder.
    running: Option<u64>,
    next_id: u64,
    results_tx: Sender<(u64, RescanFound)>,
    results: Receiver<(u64, RescanFound)>,
}

impl Default for Watching {
    fn default() -> Watching {
        let (results_tx, results) = channel();
        Watching {
            watch: None,
            dir: None,
            released: None,
            register: Box::new(register_device),
            rescan_pending: false,
            running: None,
            next_id: 0,
            results_tx,
            results,
        }
    }
}

impl Watching {
    /// Watches `dir` from now on (nothing changes if it is already the one). A rescan
    /// still running for the old folder no longer holds up rescans of the new one; its
    /// result is not applied.
    fn follow(&mut self, dir: Option<&Path>, hwnd: HWND) {
        let same = match (dir, &self.dir) {
            (Some(a), Some(b)) => same_path(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.stop();
        self.running = None;
        self.dir = dir.map(Path::to_path_buf);
        self.start(hwnd);
    }

    /// Starts watching `self.dir` and registers its handle for device events.
    fn start(&mut self, hwnd: HWND) {
        let Some(dir) = &self.dir else { return };
        match FolderWatch::start(dir, hwnd) {
            Ok(mut w) => {
                match (self.register)(dir, w.dir_handle, hwnd) {
                    Ok(r) => w.registration = Some(r),
                    // Still watched; only a removal of its drive goes unnoticed.
                    Err(e) => eprintln!(
                        "light-view: removal of the drive of {} is not noticed: {e}",
                        dir.display()
                    ),
                }
                self.watch = Some(w);
            }
            Err(e) => eprintln!(
                "light-view: changes in {} are not watched: {e}",
                dir.display()
            ),
        }
    }

    /// Stops watching; each registration ends before its handle closes.
    pub fn stop(&mut self) {
        self.watch = None;
        self.released = None;
    }

    /// A `WM_DEVICECHANGE` `event` for the folder handle `handle` registered as
    /// `hdevnotify` (see [`device_step`]).
    pub fn on_device(&mut self, event: u32, hdevnotify: isize, handle: isize, hwnd: HWND) {
        let live = self.watch.as_ref().is_some_and(|w| {
            w.dir_handle == handle && w.registration.as_ref().is_some_and(|r| r.id == hdevnotify)
        });
        let released = self.released.as_ref().is_some_and(|r| r.id == hdevnotify);
        match device_step(event, live, released) {
            DeviceStep::Release => {
                if let Some(mut w) = self.watch.take() {
                    self.released = w.registration.take();
                    if !w.release(RELEASE_WAIT) {
                        eprintln!("light-view: the folder watch did not stop in time");
                    }
                }
            }
            DeviceStep::Restart => {
                self.released = None;
                self.start(hwnd);
            }
            // `dir` stays: following the same folder does not watch it again.
            DeviceStep::Forget => self.stop(),
            DeviceStep::Ignore => {}
        }
    }

    /// The folder changed: a rescan is due.
    pub fn changed(&mut self) {
        self.rescan_pending = true;
    }

    /// Starts a rescan if one is due, none runs and nothing `blocked` it; returns its
    /// id. A rescan held back stays due.
    pub fn take_rescan(&mut self, blocked: bool) -> Option<u64> {
        if blocked || !self.rescan_pending || self.running.is_some() {
            return None;
        }
        self.rescan_pending = false;
        self.next_id += 1;
        self.running = Some(self.next_id);
        self.running
    }

    /// Rescan `id` ended after `took`.
    pub fn finish(&mut self, id: u64, took: Duration) {
        if self.running == Some(id) {
            self.running = None;
            if let Some(w) = &self.watch {
                w.set_last_rescan(took);
            }
        }
    }
}

/// A rescan waits while a dialog is open (ruling R14: its answer must apply to the photo
/// it asked about) and while a save runs (its own replace sends changes too; once it
/// ended the shown identity is the saved file's, so the rescan finds nothing changed).
pub fn rescan_blocked(overlay: &Overlay, saving: bool) -> bool {
    overlay.dialog_open() || saving
}

/// Keeps the watch on the shown folder: after every change of the viewer (an open, a
/// drop, a forwarded launch, an undo into another folder).
pub fn follow_folder(app: &RefCell<App>, hwnd: HWND) {
    if let Ok(mut a) = app.try_borrow_mut() {
        let dir = a.viewer.folder().map(Path::to_path_buf);
        a.watching.follow(dir.as_deref(), hwnd);
    }
}

/// `WM_DEVICECHANGE`: handle events (`DBT_DEVTYP_HANDLE`) go to the watch and are
/// answered TRUE (a query-remove is never refused); anything else falls through.
pub fn on_device_change(
    app: &RefCell<App>,
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    let event = u32::try_from(wparam.0).ok()?;
    let ours = matches!(
        event,
        DBT_DEVICEQUERYREMOVE
            | DBT_DEVICEQUERYREMOVEFAILED
            | DBT_DEVICEREMOVEPENDING
            | DBT_DEVICEREMOVECOMPLETE
    );
    if !ours || lparam.0 == 0 {
        return None;
    }
    // SAFETY: for these events lparam points to a DEV_BROADCAST_HDR (checked non-null).
    let hdr = unsafe { &*(lparam.0 as *const DEV_BROADCAST_HDR) };
    if hdr.dbch_devicetype != DBT_DEVTYP_HANDLE
        || (hdr.dbch_size as usize) < std::mem::size_of::<DEV_BROADCAST_HANDLE>()
    {
        return None;
    }
    // SAFETY: a DBT_DEVTYP_HANDLE header at least this large is a DEV_BROADCAST_HANDLE.
    let ev = unsafe { &*(lparam.0 as *const DEV_BROADCAST_HANDLE) };
    if let Ok(mut a) = app.try_borrow_mut() {
        a.watching.on_device(
            event,
            ev.dbch_hdevnotify.0 as isize,
            ev.dbch_handle.0 as isize,
            hwnd,
        );
    }
    Some(LRESULT(1))
}

/// `WM_DESTROY`: the watch ends while the window still exists.
pub fn stop(app: &RefCell<App>) {
    if let Ok(mut a) = app.try_borrow_mut() {
        a.watching.stop();
    }
}

/// `WM_APP_FOLDER_CHANGED`.
pub fn on_changed(app: &RefCell<App>, hwnd: HWND) {
    if let Ok(mut a) = app.try_borrow_mut() {
        a.watching.changed();
    }
    rescan_if_due(app, hwnd);
}

/// Starts a rescan on a background thread if one is due and nothing blocks it. Also
/// called once a dialog closed (after a forwarded open it deferred, which lists its
/// folder anyway), once a save ended and once a rescan ended.
pub fn rescan_if_due(app: &RefCell<App>, hwnd: HWND) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let blocked = rescan_blocked(&a.overlay, a.saving.is_some());
    let Some(id) = a.watching.take_rescan(blocked) else {
        return;
    };
    let Some(job) = a.viewer.rescan_job() else {
        a.watching.finish(id, Duration::ZERO);
        return;
    };
    let tx = a.watching.results_tx.clone();
    let target = hwnd.0 as isize;
    let spawned = std::thread::Builder::new()
        .name("rescan".into())
        .spawn(move || {
            let nothing = job.nothing();
            let found = catch_unwind(AssertUnwindSafe(|| job.run())).unwrap_or(nothing);
            let _ = tx.send((id, found));
            post(target, WM_APP_RESCAN_DONE);
        });
    if let Err(e) = spawned {
        eprintln!("light-view: could not rescan the folder: {e}");
        a.watching.finish(id, Duration::ZERO);
    }
}

/// `WM_APP_RESCAN_DONE`: applies finished rescans, then starts the one due meanwhile.
/// A result that came while a dialog opened or a save started, or that is out of date
/// (the viewer moved on), is dropped and the folder rescanned again.
pub fn on_rescan_done(app: &RefCell<App>, hwnd: HWND) {
    loop {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        let Ok((id, found)) = a.watching.results.try_recv() else {
            break;
        };
        a.watching.finish(id, found.took);
        if rescan_blocked(&a.overlay, a.saving.is_some()) {
            a.watching.changed();
            continue;
        }
        let Some((redraw, notice)) = a.viewer.apply_rescan(found) else {
            a.watching.changed();
            continue;
        };
        drop(a);
        commands::after(app, hwnd, redraw);
        let key = match notice {
            Some(RescanNotice::Vanished) => S::ToastPhotoVanished,
            Some(RescanNotice::Reloaded) => S::ToastPhotoReloaded,
            None => continue,
        };
        input::show_toast(app, hwnd, |now| Toast::new(key, now));
    }
    rescan_if_due(app, hwnd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{DialogTarget, Prompt};
    use crate::viewer::Viewer;
    use lv_core::command::{Mods, VK_ESCAPE};
    use lv_core::decode::{DecodeError, DecodedImage, Frame};
    use lv_core::format::ImageFormat;
    use lv_core::prefetch::{Decode, Prefetcher};
    use std::sync::mpsc::RecvTimeoutError;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn debounce_coalesces_bursts() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        assert_eq!(d.due(), None);
        assert!(!d.fire(t0 + 10 * DEBOUNCE));
        // A burst: one notice, 200 ms after its last change.
        for ms in [0, 50, 120] {
            d.event(t0 + ms * MS);
        }
        assert_eq!(d.due(), Some(t0 + 320 * MS));
        assert!(!d.fire(t0 + 319 * MS));
        assert!(d.fire(t0 + 320 * MS));
        assert!(!d.fire(t0 + 400 * MS));
        assert_eq!(d.due(), None);
        // A steady stream still gives a notice every second.
        let t1 = t0 + 2 * MAX_DELAY;
        let mut fired = 0;
        for i in 0..30 {
            let now = t1 + i * 100 * MS;
            d.event(now);
            fired += usize::from(d.fire(now));
        }
        assert_eq!(fired, 2);
    }

    #[test]
    fn a_slow_rescan_stretches_the_debounce() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        d.set_slow(1500 * MS);
        d.event(t0);
        assert_eq!(d.due(), Some(t0 + 3000 * MS));
        // A steady stream: one notice per 3 s instead of per second.
        let mut fired = 0;
        for i in 0..60 {
            let now = t0 + i * 100 * MS;
            d.event(now);
            fired += usize::from(d.fire(now));
        }
        assert_eq!(fired, 1);
        // A fast one changes nothing.
        d.set_slow(20 * MS);
        let t1 = t0 + 10 * MAX_DELAY;
        d.event(t1);
        assert!(d.fire(t1 + DEBOUNCE));
    }

    #[test]
    fn watch_reports_new_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"a").unwrap();
        let (tx, rx) = channel();
        let watch = FolderWatch::start_with(dir.path(), move || {
            let _ = tx.send(Instant::now());
        })
        .unwrap();
        let written = Instant::now();
        std::fs::write(dir.path().join("b.jpg"), b"b").unwrap();
        let notified = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no change reported");
        assert!(notified >= written + DEBOUNCE);
        // Quiet folder: nothing more. Sub-folder contents are not watched.
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let _ = rx.recv_timeout(DEBOUNCE * 3);
        std::fs::write(dir.path().join("sub").join("c.jpg"), b"c").unwrap();
        assert_eq!(
            rx.recv_timeout(DEBOUNCE * 3),
            Err(RecvTimeoutError::Timeout)
        );
        // Dropping returns at once; the thread then ends on its own (dropping `notify`).
        let dropped = Instant::now();
        drop(watch);
        assert!(dropped.elapsed() < Duration::from_millis(100));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)),
            Err(RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn a_missing_folder_is_not_watched() {
        let dir = tempfile::tempdir().unwrap();
        assert!(FolderWatch::start_with(&dir.path().join("gone"), || {}).is_err());
    }

    #[test]
    fn changes_during_a_rescan_give_exactly_one_more() {
        let mut w = Watching::default();
        assert_eq!(w.take_rescan(false), None);
        w.changed();
        let first = w.take_rescan(false).expect("due");
        // Changes while it runs: no second rescan at the same time.
        w.changed();
        w.changed();
        assert_eq!(w.take_rescan(false), None);
        // A stale id (an old folder's rescan) does not end the running one.
        w.finish(first + 100, Duration::ZERO);
        assert_eq!(w.take_rescan(false), None);
        w.finish(first, Duration::ZERO);
        let second = w.take_rescan(false).expect("one follow-up");
        w.finish(second, Duration::ZERO);
        assert_eq!(w.take_rescan(false), None);
    }

    /// True while some handle to `dir` is open: an open sharing nothing then fails. Other
    /// processes (an indexer, a scanner) may hold it briefly too, so "not held" retries.
    fn held_open(dir: &Path) -> bool {
        OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY.0)
            .share_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(dir)
            .is_err()
    }

    fn released_soon(dir: &Path) -> bool {
        (0..50).any(|_| {
            let free = !held_open(dir);
            if !free {
                std::thread::sleep(20 * MS);
            }
            free
        })
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Reg {
        /// id, folder handle
        On(isize, isize),
        /// id, whether the folder handle was still open
        Off(isize, bool),
    }

    type Log = std::rc::Rc<RefCell<Vec<Reg>>>;

    /// A `Watching` whose registrations log to the returned list instead of reaching the
    /// system; each unregister notes whether its folder was still held open.
    fn logged_watching() -> (Watching, Log) {
        let log = Log::default();
        let next = std::rc::Rc::new(std::cell::Cell::new(100));
        let mut w = Watching::default();
        let l = log.clone();
        w.register = Box::new(move |dir, handle, _| {
            let id = next.get();
            next.set(id + 1);
            l.borrow_mut().push(Reg::On(id, handle));
            let (l, dir) = (l.clone(), dir.to_path_buf());
            Ok(Registration {
                id,
                unregister: Some(Box::new(move || {
                    l.borrow_mut().push(Reg::Off(id, held_open(&dir)));
                })),
            })
        });
        (w, log)
    }

    fn live(w: &Watching) -> (isize, isize) {
        let watch = w.watch.as_ref().expect("watching");
        let reg = watch.registration.as_ref().expect("registered");
        (reg.id, watch.dir_handle)
    }

    #[test]
    fn device_steps() {
        use DeviceStep::*;
        assert_eq!(device_step(DBT_DEVICEQUERYREMOVE, true, false), Release);
        assert_eq!(device_step(DBT_DEVICEQUERYREMOVE, false, true), Ignore);
        assert_eq!(device_step(DBT_DEVICEQUERYREMOVE, false, false), Ignore);
        assert_eq!(
            device_step(DBT_DEVICEQUERYREMOVEFAILED, false, true),
            Restart
        );
        assert_eq!(
            device_step(DBT_DEVICEQUERYREMOVEFAILED, true, false),
            Ignore
        );
        assert_eq!(device_step(DBT_DEVICEREMOVECOMPLETE, false, true), Forget);
        assert_eq!(device_step(DBT_DEVICEREMOVECOMPLETE, true, false), Forget);
        assert_eq!(device_step(DBT_DEVICEREMOVEPENDING, true, false), Forget);
        assert_eq!(device_step(DBT_DEVICEREMOVECOMPLETE, false, false), Ignore);
        assert_eq!(device_step(0x8000, true, true), Ignore);
    }

    #[test]
    fn query_remove_releases_watch() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, log) = logged_watching();
        let none = HWND::default();
        w.follow(Some(dir.path()), none);
        let (id, handle) = live(&w);
        assert_eq!(*log.borrow(), [Reg::On(id, handle)]);
        assert!(held_open(dir.path()));

        // Someone else's registration, or a stale handle: nothing happens.
        w.on_device(DBT_DEVICEQUERYREMOVE, id + 50, handle, none);
        w.on_device(DBT_DEVICEQUERYREMOVE, id, handle + 4, none);
        assert!(w.watch.is_some() && held_open(dir.path()));

        // Query-remove: the handle is closed before it returns; the registration stays
        // so the outcome arrives.
        w.on_device(DBT_DEVICEQUERYREMOVE, id, handle, none);
        assert!(w.watch.is_none());
        assert!(released_soon(dir.path()));
        assert_eq!(log.borrow().len(), 1);

        // The removal was refused: the same folder is watched again.
        w.on_device(DBT_DEVICEQUERYREMOVEFAILED, id, handle, none);
        let (id2, handle2) = live(&w);
        assert_eq!(
            log.borrow()[1..],
            [Reg::Off(id, false), Reg::On(id2, handle2)]
        );
        assert!(held_open(dir.path()));
        // A late event for the old registration changes nothing.
        w.on_device(DBT_DEVICEREMOVECOMPLETE, id, handle, none);
        assert!(w.watch.is_some());

        // Removed: unregistered, and the same folder stays unwatched.
        w.on_device(DBT_DEVICEQUERYREMOVE, id2, handle2, none);
        w.on_device(DBT_DEVICEREMOVECOMPLETE, id2, handle2, none);
        assert_eq!(log.borrow()[3..], [Reg::Off(id2, false)]);
        assert!(w.watch.is_none() && w.released.is_none());
        w.follow(Some(dir.path()), none);
        assert!(w.watch.is_none());
        assert_eq!(log.borrow().len(), 4);
        // Another folder is watched again.
        let other = tempfile::tempdir().unwrap();
        w.follow(Some(other.path()), none);
        assert!(w.watch.is_some());
        assert_eq!(log.borrow().len(), 5);
    }

    #[test]
    fn registration_ends_before_the_handle_closes() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (mut w, log) = logged_watching();
        let none = HWND::default();
        // Moving to another folder.
        w.follow(Some(a.path()), none);
        let (id_a, _) = live(&w);
        w.follow(Some(b.path()), none);
        let (id_b, handle_b) = live(&w);
        assert_eq!(log.borrow()[1], Reg::Off(id_a, true));
        assert!(released_soon(a.path()));
        // A drive pulled out without asking: removal completes on the live watch.
        w.on_device(DBT_DEVICEREMOVECOMPLETE, id_b, handle_b, none);
        assert_eq!(log.borrow()[3], Reg::Off(id_b, true));
        assert!(released_soon(b.path()));
        // The window closing.
        w.follow(Some(a.path()), none);
        let (id_c, _) = live(&w);
        w.stop();
        assert_eq!(log.borrow()[5], Reg::Off(id_c, true));
        assert!(released_soon(a.path()));
        // A query-remove released one: stopping unregisters it (its handle is closed).
        w.follow(Some(b.path()), none);
        let (id_d, handle_d) = live(&w);
        w.on_device(DBT_DEVICEQUERYREMOVE, id_d, handle_d, none);
        w.stop();
        assert_eq!(log.borrow()[7], Reg::Off(id_d, false));
    }

    #[test]
    fn a_folder_handle_registers_for_device_events() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
        };
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: a message-only window of the system STATIC class, destroyed below.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                None,
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                None,
                None,
            )
        }
        .unwrap();
        let mut w = Watching::default();
        w.follow(Some(dir.path()), hwnd);
        let (id, _) = live(&w);
        assert_ne!(id, 0);
        w.stop();
        assert!(released_soon(dir.path()));
        // SAFETY: the window created above, on this thread.
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    struct AnyDecoder;

    impl Decode for AnyDecoder {
        fn decode(&self, _path: &Path) -> Result<DecodedImage, DecodeError> {
            Ok(DecodedImage {
                width: 4,
                height: 3,
                frames: vec![Frame {
                    bgra: vec![0; 48],
                    delay_ms: 0,
                }],
                format: ImageFormat::Png,
                byte_len: 0,
                exif: None,
            })
        }
    }

    #[test]
    fn a_rescan_waits_for_an_open_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        std::fs::write(&a, b"a").unwrap();
        let mut viewer = Viewer::new(Prefetcher::new(AnyDecoder, 1, u64::MAX, |_| {}));
        viewer.open(&a);
        let mut overlay = Overlay::default();
        let target = DialogTarget {
            path: a.clone(),
            generation: viewer.generation(),
        };
        overlay.show_dialog(Prompt::SaveRotation, target);
        let mut w = Watching::default();

        std::fs::write(dir.path().join("b.jpg"), b"b").unwrap();
        w.changed();
        assert_eq!(w.take_rescan(rescan_blocked(&overlay, false)), None);
        assert_eq!(viewer.position(), (1, 1));
        // Closed: the waiting rescan runs, once.
        assert!(overlay.on_key(VK_ESCAPE, Mods::default()).is_some());
        let id = w.take_rescan(rescan_blocked(&overlay, false)).unwrap();
        viewer.rescan();
        w.finish(id, Duration::ZERO);
        assert_eq!(viewer.position(), (1, 2));
        assert_eq!(w.take_rescan(rescan_blocked(&overlay, false)), None);
        // A running save holds it back too.
        w.changed();
        assert_eq!(w.take_rescan(rescan_blocked(&overlay, true)), None);
        assert!(w.take_rescan(rescan_blocked(&overlay, false)).is_some());
    }
}
