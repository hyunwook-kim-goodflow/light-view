//! Single instance: a named mutex marks the primary process; later launches forward
//! their path to its window with WM_COPYDATA and exit.
//!
//! Each portable copy is its own single instance, apart from the installed copy and
//! from other portable folders: its mutex name carries a hash of its folder, and a
//! launch forwards only to a window whose process runs from a folder with the same key.

use std::cell::RefCell;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lv_core::portable;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowExW, GetWindowThreadProcessId, IsIconic, PeekMessageW,
    SendMessageTimeoutW, SetForegroundWindow, ShowWindow, MSG, PM_NOREMOVE, SMTO_ABORTIFHUNG,
    SW_RESTORE, WM_CLOSE, WM_COPYDATA,
};

use crate::commands;
use crate::window::{App, CLASS_NAME};

/// `dwData` of the WM_COPYDATA message that carries a path ("LV" + version 1).
pub const COPYDATA_OPEN: usize = 0x4C56_0001;
/// The longest path accepted, in UTF-16 units, without the terminating NUL.
const MAX_PATH_UNITS: usize = 32767;
/// The installed copy's mutex; a portable copy appends `.<key>` (see `instance_key`).
const MUTEX_NAME: &str = r"Local\goodflow.LightView.Instance";
/// While the primary is still starting, its window is looked for this often, this long.
const FIND_INTERVAL: Duration = Duration::from_millis(50);
const FIND_TIMEOUT: Duration = Duration::from_secs(2);
const SEND_TIMEOUT_MS: u32 = 2000;

/// Holds the instance mutex; closing it on drop lets the next launch become primary.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        if self.0.is_invalid() {
            return;
        }
        // SAFETY: the handle came from CreateMutexW and is closed only here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub enum Launch {
    /// This process shows the window; keep the guard alive until it closes.
    Primary(InstanceGuard),
    /// The path went to the running window; exit without creating one.
    Forwarded,
}

/// Which single instance the copy whose exe is in `exe_dir` belongs to: `None` for an
/// installed copy (all share the original name), or a hash of the folder for a portable
/// one, so a portable copy never forwards to (or receives from) another copy. The folder
/// is compared without case and trailing separators, as Windows does.
pub fn instance_key(exe_dir: &Path) -> Option<String> {
    portable::data_dir(exe_dir)
        .is_portable()
        .then(|| folder_hash(exe_dir))
}

/// FNV-1a (64-bit) of the lower-cased folder, as 16 hex digits.
fn folder_hash(dir: &Path) -> String {
    let text = dir.to_string_lossy().to_lowercase();
    let text = text.trim_end_matches(['\\', '/']);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The mutex name for an instance key (see `instance_key`).
fn mutex_name(key: Option<&str>) -> String {
    match key {
        None => MUTEX_NAME.to_owned(),
        Some(k) => format!("{MUTEX_NAME}.{k}"),
    }
}

/// Becomes the primary instance, or hands `path` to the running one of the same copy
/// (`key`, from `instance_key`). Falls back to `Primary` when the running window cannot
/// be reached, so a launch is never lost.
pub fn acquire_or_forward(path: Option<&Path>, key: Option<&str>) -> Launch {
    let name: Vec<u16> = mutex_name(key)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `name` is NUL-terminated and outlives the call. GetLastError is read
    // right after it.
    let (handle, existed) = unsafe {
        match CreateMutexW(None, false, PCWSTR(name.as_ptr())) {
            Ok(h) => (h, GetLastError() == ERROR_ALREADY_EXISTS),
            // Without a mutex there is nothing to coordinate with; just run.
            Err(_) => return Launch::Primary(InstanceGuard(HANDLE::default())),
        }
    };
    let guard = InstanceGuard(handle);
    if existed && forward(path, key) {
        Launch::Forwarded
    } else {
        Launch::Primary(guard)
    }
}

/// Sends `path` to the primary window, or just brings it forward when there is no
/// path. False when the window was not found in time or did not answer.
fn forward(path: Option<&Path>, key: Option<&str>) -> bool {
    let Some(hwnd) = find_primary_window(key) else {
        return false;
    };
    let mut pid = 0u32;
    // SAFETY: plain calls; `pid` is a live local.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
    }
    let Some(path) = path else {
        // SAFETY: plain calls on the primary's window.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        return true;
    };
    // The primary has another working directory, so a relative path is resolved here.
    let data = encode_path(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    let cds = COPYDATASTRUCT {
        dwData: COPYDATA_OPEN,
        cbData: (data.len() * 2) as u32,
        lpData: data.as_ptr() as *mut _,
    };
    let mut answer = 0usize;
    // SAFETY: `cds` and `data` outlive the synchronous send; the system copies the
    // payload into the receiver. `answer` is a live local.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(std::ptr::from_ref(&cds) as isize),
            SMTO_ABORTIFHUNG,
            SEND_TIMEOUT_MS,
            Some(&mut answer),
        )
    };
    forwarded(sent, answer)
}

/// The primary's window: a viewer window whose process belongs to the instance `key`.
/// Retried while it may still be starting.
fn find_primary_window(key: Option<&str>) -> Option<HWND> {
    let start = Instant::now();
    loop {
        let mut after = HWND::default();
        // SAFETY: plain calls with a static class name; `after` is a window it returned.
        while let Ok(hwnd) = unsafe { FindWindowExW(None, after, CLASS_NAME, PCWSTR::null()) } {
            if hwnd.is_invalid() {
                break;
            }
            if window_key(hwnd).is_some_and(|k| k.as_deref() == key) {
                return Some(hwnd);
            }
            after = hwnd;
        }
        if start.elapsed() >= FIND_TIMEOUT {
            return None;
        }
        std::thread::sleep(FIND_INTERVAL);
    }
}

/// The instance key of the process that owns `hwnd`, from the folder of its exe; `None`
/// when that process cannot be asked.
fn window_key(hwnd: HWND) -> Option<Option<String>> {
    let mut pid = 0u32;
    // SAFETY: plain call; `pid` is a live local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    Some(instance_key(process_image(pid)?.parent()?))
}

/// This process's instance key. It is read the same way as `window_key` reads the
/// primary's (QueryFullProcessImageNameW), so a folder reached through a subst or
/// mapped drive or an 8.3 name gives the same spelling on both sides.
pub fn own_instance_key() -> Option<String> {
    // SAFETY: plain call.
    let pid = unsafe { GetCurrentProcessId() };
    let image = process_image(pid).or_else(|| std::env::current_exe().ok())?;
    instance_key(image.parent()?)
}

/// The full Win32 path of process `pid`'s exe; `None` when it cannot be asked.
fn process_image(pid: u32) -> Option<PathBuf> {
    let mut buf = vec![0u16; MAX_PATH_UNITS + 1];
    let mut len = buf.len() as u32;
    // SAFETY: the handle is closed below; `buf` holds `len` units and outlives the call,
    // which writes at most `len` units and sets `len` to the length without the NUL.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        ok.ok()?;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(
        &buf[..len as usize],
    )))
}

/// WM_COPYDATA from another launch: opens the path if it is an existing file (or a
/// folder, like a drop) and brings the window forward. The sender is not trusted.
/// Answers 1 when the path was taken (even if there is nothing to open), 0 when refused
/// while the window is closing (the sender then opens its own window) or malformed.
pub fn on_copy_data(app: &RefCell<App>, hwnd: HWND, lparam: LPARAM) -> LRESULT {
    // Any process can send WM_COPYDATA; a null lparam must not be dereferenced.
    if lparam.0 == 0 {
        return LRESULT(0);
    }
    let mut queued = MSG::default();
    // SAFETY: plain call on our window's queue; PM_NOREMOVE leaves the message there.
    let close_pending =
        unsafe { PeekMessageW(&mut queued, hwnd, WM_CLOSE, WM_CLOSE, PM_NOREMOVE) }.as_bool();
    if !accept_copy_data(close_pending, CLOSING.load(Ordering::Relaxed)) {
        return LRESULT(0);
    }
    // SAFETY: lparam is non-null (checked above). For WM_COPYDATA the system marshals
    // the sender's COPYDATASTRUCT into this process, so a non-null lparam points to a
    // valid COPYDATASTRUCT whose lpData (when non-null) holds cbData readable bytes for
    // the duration of this message.
    let path = unsafe {
        let cds = &*(lparam.0 as *const COPYDATASTRUCT);
        let bytes = if cds.lpData.is_null() || cds.cbData == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize)
        };
        decode_message(cds.dwData, bytes)
    };
    let Some(path) = path else {
        return LRESULT(0);
    };
    if !(path.is_file() || path.is_dir()) {
        // Taken, with nothing to open: the sender must not open a window for it either.
        return LRESULT(1);
    }
    // While a dialog is open the path waits for it to close (ruling R14), so the answer
    // still applies to the photo it asked about. The modal Open dialog (the window is
    // disabled) holds it the same way; it opens once that dialog closes without a pick.
    // Otherwise it opens now; an unsaved rotation is discarded (M1). Neither holds a
    // borrow across the calls below.
    // SAFETY: plain query on our own window handle.
    let blocked = !unsafe { IsWindowEnabled(hwnd) }.as_bool();
    let deferred = app
        .try_borrow_mut()
        .is_ok_and(|mut a| a.overlay.defer_open(path.clone(), blocked));
    if !deferred {
        commands::open(app, hwnd, &path);
    }
    // SAFETY: plain calls on our window; no App borrow is held (both may re-enter).
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    LRESULT(1)
}

/// Set once the window starts closing; later WM_COPYDATA messages are refused.
static CLOSING: AtomicBool = AtomicBool::new(false);

/// Called when the window closes: right before it is destroyed, or once the user chose
/// to close after a save that is still running.
pub fn mark_closing() {
    CLOSING.store(true, Ordering::Relaxed);
}

/// The save a close waited for failed, so the window stays open after all.
pub fn cancel_closing() {
    CLOSING.store(false, Ordering::Relaxed);
}

/// Whether a forwarded path may be taken: not while a WM_CLOSE waits in the queue
/// (`close_pending`) or is being handled (`closing`), since the window would go away with
/// it. Refused, the sender opens its own window (M1 ruling R18).
fn accept_copy_data(close_pending: bool, closing: bool) -> bool {
    !close_pending && !closing
}

/// Whether the primary took the path: the send completed (`sent`) and the window answered
/// 1 (`answer`). Anything else, the launch opens its own window.
fn forwarded(sent: LRESULT, answer: usize) -> bool {
    sent.0 != 0 && answer == 1
}

/// The path as UTF-16 with a terminating NUL.
pub fn encode_path(p: &Path) -> Vec<u16> {
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// The path before the first NUL. `None` when there is no NUL within
/// `MAX_PATH_UNITS + 1` units or the path is empty; data after the NUL is ignored.
pub fn decode_path(data: &[u16]) -> Option<PathBuf> {
    let window = &data[..data.len().min(MAX_PATH_UNITS + 1)];
    let end = window.iter().position(|&u| u == 0)?;
    (end > 0).then(|| PathBuf::from(std::ffi::OsString::from_wide(&window[..end])))
}

/// Decodes a WM_COPYDATA payload: `None` unless `dw_data` is `COPYDATA_OPEN` and
/// `bytes` is a whole number of UTF-16 units that `decode_path` accepts.
pub fn decode_message(dw_data: usize, bytes: &[u8]) -> Option<PathBuf> {
    if dw_data != COPYDATA_OPEN || !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .take(MAX_PATH_UNITS + 1)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    decode_path(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(units: &[u16]) -> Vec<u8> {
        units.iter().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn own_key_reads_this_exe_like_a_window_key() {
        let pid = unsafe { GetCurrentProcessId() };
        let image = process_image(pid).expect("own image");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            image.file_name().map(|n| n.to_ascii_lowercase()),
            exe.file_name().map(|n| n.to_ascii_lowercase())
        );
        // The test binary's folder has no portable.txt, so it is the installed name.
        assert_eq!(own_instance_key(), None);
    }

    #[test]
    fn portable_copies_get_their_own_instance_name() {
        let installed = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for d in [&a, &b] {
            std::fs::write(d.path().join(portable::MARKER), "").unwrap();
        }
        // The installed copy keeps the name every earlier version used.
        assert_eq!(instance_key(installed.path()), None);
        assert_eq!(mutex_name(None), r"Local\goodflow.LightView.Instance");
        // Each portable folder has its own name, apart from the installed one.
        let (ka, kb) = (
            instance_key(a.path()).unwrap(),
            instance_key(b.path()).unwrap(),
        );
        assert_ne!(ka, kb);
        assert_eq!(ka.len(), 16);
        assert!(ka.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            mutex_name(Some(&ka)),
            format!(r"Local\goodflow.LightView.Instance.{ka}")
        );
        // The same folder spelled with another case or a trailing separator is the same
        // copy (the window's process path may be spelled differently from current_exe).
        let upper = PathBuf::from(a.path().to_string_lossy().to_uppercase());
        let slash = PathBuf::from(format!("{}\\", a.path().display()));
        assert_eq!(folder_hash(&upper), ka);
        assert_eq!(folder_hash(&slash), ka);
    }

    #[test]
    fn copydata_refused_while_closing() {
        assert!(accept_copy_data(false, false));
        // A WM_CLOSE waiting in the queue, or one being handled: the sender opens its own
        // window instead of handing the path to a window about to go away.
        assert!(!accept_copy_data(true, false));
        assert!(!accept_copy_data(false, true));
        assert!(!accept_copy_data(true, true));
        // The sender counts only an explicit acceptance as forwarded.
        assert!(forwarded(LRESULT(1), 1));
        assert!(!forwarded(LRESULT(1), 0));
        assert!(!forwarded(LRESULT(0), 1));
    }

    #[test]
    fn path_roundtrip_long_and_unicode() {
        let long = format!(r"C:\사진\{}\여름 바다.jpg", "가".repeat(300));
        assert!(long.chars().count() > 300);
        let p = PathBuf::from(&long);
        let enc = encode_path(&p);
        assert_eq!(enc.last(), Some(&0));
        assert_eq!(decode_path(&enc), Some(p.clone()));
        assert_eq!(decode_message(COPYDATA_OPEN, &bytes(&enc)), Some(p));
    }

    #[test]
    fn decode_rejects_unterminated_or_oversized() {
        let unterminated: Vec<u16> = "C:\\a.jpg".encode_utf16().collect();
        assert_eq!(decode_path(&unterminated), None);

        let mut longest = vec![u16::from(b'a'); MAX_PATH_UNITS];
        longest.push(0);
        assert!(decode_path(&longest).is_some());

        let mut oversized = vec![u16::from(b'a'); MAX_PATH_UNITS + 1];
        oversized.push(0);
        assert_eq!(decode_path(&oversized), None);

        assert_eq!(decode_path(&[]), None);
        assert_eq!(decode_path(&[0]), None);
    }

    #[test]
    fn decode_stops_at_first_nul() {
        let mut data: Vec<u16> = "C:\\a.jpg".encode_utf16().collect();
        data.push(0);
        data.extend("ignored".encode_utf16());
        assert_eq!(decode_path(&data), Some(PathBuf::from("C:\\a.jpg")));
        // Trailing data without a second NUL is still ignored.
        data.extend(std::iter::repeat_n(u16::from(b'x'), MAX_PATH_UNITS * 2));
        assert_eq!(decode_path(&data), Some(PathBuf::from("C:\\a.jpg")));
    }

    #[test]
    fn decode_message_rejects_odd_bytes_and_wrong_tag() {
        let enc = encode_path(Path::new("C:\\a.jpg"));
        let mut b = bytes(&enc);
        assert!(decode_message(COPYDATA_OPEN, &b).is_some());
        assert_eq!(decode_message(COPYDATA_OPEN + 1, &b), None);
        assert_eq!(decode_message(0, &b), None);
        b.push(0);
        assert_eq!(decode_message(COPYDATA_OPEN, &b), None);
    }
}
