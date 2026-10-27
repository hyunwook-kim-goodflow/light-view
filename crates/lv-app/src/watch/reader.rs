//! The watch thread: overlapped `ReadDirectoryChangesW` reads on the folder handle, the
//! debounce loop and the message it posts.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_NOTIFY_ENUM_DIR, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::Threading::{
    CreateEventW, WaitForMultipleObjects, WaitForSingleObject, INFINITE,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{Debounce, BUFFER_WORDS, FILTER};

/// The overlapped read: the system writes into it until the read completes or is
/// cancelled, so it lives in a `Box` (it never moves) owned by the watch thread, which
/// waits for the read to end before dropping it.
pub(super) struct Read {
    ov: OVERLAPPED,
    buf: [u32; BUFFER_WORDS],
}
/// Opens `dir` for overlapped change reads and starts the first one.
pub(super) fn start_reading(dir: &Path) -> io::Result<(File, OwnedHandle, Box<Read>)> {
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

pub(super) fn new_event() -> io::Result<OwnedHandle> {
    // SAFETY: plain call; an unnamed auto-reset event, initially not set.
    let h = unsafe { CreateEventW(None, false, false, None) }?;
    // SAFETY: `h` is a new handle owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

pub(super) fn post(target: isize, msg: u32) {
    if target == 0 {
        return;
    }
    // SAFETY: plain call; posting to a destroyed window just fails.
    unsafe {
        let _ = PostMessageW(HWND(target as *mut _), msg, WPARAM(0), LPARAM(0));
    }
}

pub(super) fn watch_loop(
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
    let mut wait_failed = false;
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
            wait_failed = true;
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
    if wait_failed {
        // The handle stays open until the window lets go of its device registration and
        // says stop (a registration must end before its handle closes).
        // SAFETY: `stop` is an owned event alive for the whole call.
        unsafe {
            let _ = WaitForSingleObject(HANDLE(stop.as_raw_handle()), INFINITE);
        }
    }
}
