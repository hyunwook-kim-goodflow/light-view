//! Folder change detection (spec §5.x, ruling R19): a [`FolderWatch`] thread waits on
//! `ReadDirectoryChangesW` for the shown folder and, 200 ms after a burst of changes
//! settles, posts `WM_APP_FOLDER_CHANGED`. The window then lists the folder again
//! ([`Viewer::rescan`](crate::viewer::Viewer::rescan)), unless a dialog is open or a save
//! runs: the rescan then waits for it ([`Watching`], ruling R14).
//!
//! Rescans run on the UI thread, like the scan made when a folder is opened, and cost
//! about the same: one folder read and sort, plus the identity of at most five files
//! (measured on a local folder of 2000 files: 13–17 ms per rescan in a release build,
//! 12.5 ms for the scan at open). At most one runs per `DEBOUNCE`, and none while
//! nothing changes. A slow network folder blocks a rescan as long as it blocks an open.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lv_core::identity::to_extended_path;
use lv_core::listing::same_path;
use windows::Win32::Foundation::{
    ERROR_NOTIFY_ENUM_DIR, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::commands;
use crate::input;
use crate::overlay::{Overlay, Toast, TOAST_PHOTO_RELOADED, TOAST_PHOTO_VANISHED};
use crate::viewer::RescanNotice;
use crate::window::App;

/// Posted by a [`FolderWatch`] once a burst of changes settled. (`WM_APP + 3` is
/// `WM_APP_DELETE_DONE`.)
pub const WM_APP_FOLDER_CHANGED: u32 = WM_APP + 4;
/// Quiet time after the last change of a burst.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// A steady stream of changes (a long copy) still refreshes the list this often.
const MAX_DELAY: Duration = Duration::from_secs(1);
/// File names (added, removed, renamed), sizes and write times; not sub-folders.
const FILTER: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0 | FILE_NOTIFY_CHANGE_SIZE.0 | FILE_NOTIFY_CHANGE_LAST_WRITE.0,
);
/// The change records are never read (any change means "list again"), so a small
/// buffer does; an overflow is reported as a change too.
const BUFFER_WORDS: usize = 1024;

/// Coalesces a burst of changes into one notice, `DEBOUNCE` after its last change but
/// no later than `MAX_DELAY` after its first.
#[derive(Debug, Default)]
pub struct Debounce {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Debounce {
    pub fn event(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// When the pending burst is due; `None` with nothing pending.
    pub fn due(&self) -> Option<Instant> {
        Some((self.last? + DEBOUNCE).min(self.first? + MAX_DELAY))
    }

    /// True, once, when the pending burst is due at `now`.
    pub fn fire(&mut self, now: Instant) -> bool {
        if self.due().is_some_and(|due| now >= due) {
            *self = Debounce::default();
            return true;
        }
        false
    }
}

/// Watches one folder on its own thread until dropped.
pub struct FolderWatch {
    stop: Arc<OwnedHandle>,
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
        FolderWatch::start_with(dir, move || {
            // SAFETY: plain call; posting to a destroyed window just fails.
            unsafe {
                let _ = PostMessageW(
                    HWND(target as *mut _),
                    WM_APP_FOLDER_CHANGED,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        })
    }

    fn start_with(dir: &Path, notify: impl Fn() + Send + 'static) -> io::Result<FolderWatch> {
        let stop = Arc::new(new_event()?);
        let thread_stop = stop.clone();
        let dir = to_extended_path(dir);
        let (started_tx, started) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("folder-watch".into())
            .spawn(move || match start_reading(&dir) {
                Ok((file, io_done, mut read)) => {
                    let _ = started_tx.send(Ok(()));
                    watch_loop(&file, &io_done, &thread_stop, &mut read, &notify);
                }
                Err(e) => {
                    let _ = started_tx.send(Err(e));
                }
            })?;
        // The first read tells whether this location reports changes at all.
        let started = started
            .recv()
            .unwrap_or_else(|_| Err(io::Error::other("the folder watch thread ended")));
        if let Err(e) = started {
            let _ = thread.join();
            return Err(e);
        }
        Ok(FolderWatch {
            stop,
            thread: Some(thread),
        })
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

impl Drop for FolderWatch {
    /// Stops the thread: it cancels its read (`CancelIoEx`) and ends.
    fn drop(&mut self) {
        // SAFETY: the event handle is owned by `self.stop` and alive.
        let _ = unsafe { SetEvent(HANDLE(self.stop.as_raw_handle())) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
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
        // the read has completed or been cancelled and waited for.
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

fn watch_loop(
    dir: &File,
    io_done: &OwnedHandle,
    stop: &OwnedHandle,
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
        let timeout = debounce.due().map_or(INFINITE, |due| {
            let left = due.saturating_duration_since(Instant::now()).as_millis();
            u32::try_from(left).unwrap_or(INFINITE - 1) + 1
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

/// The window's folder watch, and a rescan waiting for a dialog or a save to end.
#[derive(Default)]
pub struct Watching {
    watch: Option<FolderWatch>,
    /// The watched folder, or the folder that could not be watched.
    dir: Option<PathBuf>,
    rescan_pending: bool,
}

impl Watching {
    /// Watches `dir` from now on (nothing changes if it is already the one).
    fn follow(&mut self, dir: Option<&Path>, hwnd: HWND) {
        let same = match (dir, &self.dir) {
            (Some(a), Some(b)) => same_path(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.watch = None;
        self.dir = dir.map(Path::to_path_buf);
        let Some(dir) = dir else { return };
        match FolderWatch::start(dir, hwnd) {
            Ok(w) => self.watch = Some(w),
            Err(e) => eprintln!(
                "light-view: changes in {} are not watched: {e}",
                dir.display()
            ),
        }
    }

    /// The folder changed: a rescan is due.
    pub fn changed(&mut self) {
        self.rescan_pending = true;
    }

    /// Whether to rescan now: one is due and nothing `blocked` it. A blocked rescan
    /// stays due.
    pub fn take_rescan(&mut self, blocked: bool) -> bool {
        if blocked || !self.rescan_pending {
            return false;
        }
        self.rescan_pending = false;
        true
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

/// `WM_APP_FOLDER_CHANGED`.
pub fn on_changed(app: &RefCell<App>, hwnd: HWND) {
    if let Ok(mut a) = app.try_borrow_mut() {
        a.watching.changed();
    }
    rescan_if_due(app, hwnd);
}

/// Rescans if one is due and nothing blocks it. Also called once a dialog closed (after
/// a forwarded open it deferred, which lists its folder anyway) and once a save ended.
pub fn rescan_if_due(app: &RefCell<App>, hwnd: HWND) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let blocked = rescan_blocked(&a.overlay, a.saving.is_some());
    if !a.watching.take_rescan(blocked) {
        return;
    }
    let (redraw, notice) = a.viewer.rescan();
    drop(a);
    commands::after(app, hwnd, redraw);
    let text = match notice {
        Some(RescanNotice::Vanished) => TOAST_PHOTO_VANISHED,
        Some(RescanNotice::Reloaded) => TOAST_PHOTO_RELOADED,
        None => return,
    };
    input::show_toast(app, hwnd, |now| Toast::new(text, now));
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
    use std::sync::mpsc::channel;

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
        assert!(rx.recv_timeout(DEBOUNCE * 3).is_err());
        // Dropping ends the thread promptly.
        let dropped = Instant::now();
        drop(watch);
        assert!(dropped.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_missing_folder_is_not_watched() {
        let dir = tempfile::tempdir().unwrap();
        assert!(FolderWatch::start_with(&dir.path().join("gone"), || {}).is_err());
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
        assert!(!w.take_rescan(rescan_blocked(&overlay, false)));
        assert_eq!(viewer.position(), (1, 1));
        // Closed: the waiting rescan runs, once.
        assert!(overlay.on_key(VK_ESCAPE, Mods::default()).is_some());
        assert!(w.take_rescan(rescan_blocked(&overlay, false)));
        viewer.rescan();
        assert_eq!(viewer.position(), (1, 2));
        assert!(!w.take_rescan(rescan_blocked(&overlay, false)));
        // A running save holds it back too.
        w.changed();
        assert!(!w.take_rescan(rescan_blocked(&overlay, true)));
        assert!(w.take_rescan(rescan_blocked(&overlay, false)));
    }
}
