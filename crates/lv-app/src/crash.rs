//! Local crash records (ruling R6, spec §6.6): a crash leaves a log and a minidump
//! (`MiniDumpNormal`) in the data folder's `crashes` directory, nothing is sent
//! anywhere, and only the latest ten records are kept. A record is a pair
//! `crash-<UTC time>-<pid>-<n>.log` / `.dmp`; names sort by time.
//!
//! Two sources:
//! - Panics, through a panic hook that then calls the hook that was there before (the
//!   message still goes to stderr). The rule for which panics count: a panic is
//!   recorded unless its thread is inside `lv_core::unwind::catch`, the scope the
//!   decoders, saves and folder scans run in to turn a panic into an error and carry
//!   on. Every other panic ends its thread or, on the UI thread, the app (unwinding out
//!   of the window procedure aborts). One record per thread: a panic while unwinding
//!   from a recorded one (the abort at the `extern "system"` boundary) adds none.
//!   Whether the thread is already panicking (`std::thread::panicking`) cannot decide
//!   this: it is true in the hook for every panic.
//! - Unhandled SEH exceptions (an access violation in native code, say), through
//!   `SetUnhandledExceptionFilter`. The filter runs in the faulting process, so it
//!   stays small: the folder path is prepared at install time, names and the log are
//!   built in fixed stack buffers, nothing is pruned there, and then the previous
//!   filter (or the default handling) takes over.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, BOOL, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE,
};
use windows::Win32::System::Diagnostics::Debug::{
    MiniDumpNormal, MiniDumpWriteDump, SetUnhandledExceptionFilter, EXCEPTION_CONTINUE_SEARCH,
    EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER, MINIDUMP_EXCEPTION_INFORMATION,
};
use windows::Win32::System::SystemInformation::GetSystemTime;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};

/// Records kept in the folder.
pub const KEEP: usize = 10;
const PREFIX: &str = "crash-";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The crash folder as a wide string ending in `\`, without a NUL (for the filter).
static DIR_WIDE: OnceLock<Vec<u16>> = OnceLock::new();
/// Numbers records within this process.
static SEQ: AtomicU32 = AtomicU32::new(0);
/// The filter that was installed before ours (a function pointer, or 0).
static PREVIOUS_FILTER: AtomicUsize = AtomicUsize::new(0);
/// The filter writes at most one record.
static FILTER_RAN: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// This thread already left a panic record.
    static RECORDED: Cell<bool> = const { Cell::new(false) };
}

/// Starts recording crashes into `dir` (created if missing): the panic hook and the
/// unhandled-exception filter. Old records are pruned on another thread to leave room
/// for this run's.
pub fn install(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("light-view: no crash records ({}): {e}", dir.display());
        return;
    }
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    if wide.last() != Some(&u16::from(b'\\')) {
        wide.push(u16::from(b'\\'));
    }
    if DIR_WIDE.set(wide).is_err() {
        return; // Installed already.
    }
    let prune_dir = dir.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("crash-prune".into())
        .spawn(move || prune(&prune_dir, KEEP - 1));

    let hook_dir = dir.to_path_buf();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let caught = lv_core::unwind::inside_catch();
        if should_record(caught, RECORDED.try_with(Cell::get).unwrap_or(false)) {
            let _ = RECORDED.try_with(|r| r.set(true));
            if let Err(e) = record_panic(&hook_dir, info) {
                eprintln!("light-view: could not write a crash record: {e}");
            }
        }
        previous(info);
    }));

    // SAFETY: installs a filter function that lives for the whole program.
    let before = unsafe { SetUnhandledExceptionFilter(Some(on_exception)) };
    PREVIOUS_FILTER.store(before.map_or(0, |f| f as usize), Ordering::SeqCst);
}

/// Record a panic? Not one a `catch` scope turns into an error, nor a second one on a
/// thread that already left a record.
pub fn should_record(inside_catch: bool, recorded_on_thread: bool) -> bool {
    !inside_catch && !recorded_on_thread
}

/// Deletes all but the newest `keep` records in `dir`; other files stay.
pub fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let stems: BTreeSet<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name
                .strip_suffix(".log")
                .or_else(|| name.strip_suffix(".dmp"))?;
            stem.starts_with(PREFIX).then(|| stem.to_owned())
        })
        .collect();
    // Oldest first: the names start with the UTC time.
    let old = stems.len().saturating_sub(keep);
    for stem in stems.iter().take(old) {
        for ext in ["log", "dmp"] {
            let _ = std::fs::remove_file(dir.join(format!("{stem}.{ext}")));
        }
    }
}

/// `crash-20261027-091500-1234-0`: UTC time, process id and a per-process number.
fn new_stem() -> String {
    // SAFETY: plain call.
    let t = unsafe { GetSystemTime() };
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{PREFIX}{:04}{:02}{:02}-{:02}{:02}{:02}-{}-{n}",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        std::process::id()
    )
}

/// Writes a panic's log and a dump of the process, making room first.
fn record_panic(dir: &Path, info: &PanicHookInfo) -> std::io::Result<PathBuf> {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(not a string)".to_owned());
    let location = info
        .location()
        .map_or_else(|| "unknown".to_owned(), ToString::to_string);
    let thread = std::thread::current();
    let mut log = String::new();
    let _ = writeln!(log, "light-view {VERSION} panic");
    let _ = writeln!(log, "thread: {}", thread.name().unwrap_or("(unnamed)"));
    let _ = writeln!(log, "at: {location}");
    let _ = writeln!(log, "message: {message}");
    let _ = writeln!(
        log,
        "\nbacktrace:\n{}",
        std::backtrace::Backtrace::force_capture()
    );
    write_record(dir, &log)
}

/// Writes `<stem>.log` with `log` and `<stem>.dmp`, after pruning to `KEEP - 1`.
/// Returns the log's path.
fn write_record(dir: &Path, log: &str) -> std::io::Result<PathBuf> {
    prune(dir, KEEP - 1);
    let stem = new_stem();
    let log_path = dir.join(format!("{stem}.log"));
    std::fs::write(&log_path, log)?;
    let dump = File::create_new(dir.join(format!("{stem}.dmp")))?;
    // SAFETY: plain call; the process pseudo-handle and the file handle are valid for
    // the call. Without exception information the dump shows every thread as it is.
    unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            HANDLE(dump.as_raw_handle()),
            MiniDumpNormal,
            None,
            None,
            None,
        )
    }
    .map_err(std::io::Error::other)?;
    Ok(log_path)
}

/// The unhandled-exception filter: one record, then whatever was there before.
unsafe extern "system" fn on_exception(info: *const EXCEPTION_POINTERS) -> i32 {
    if !FILTER_RAN.swap(true, Ordering::SeqCst) {
        // SAFETY: `info` is the exception the system passes to the filter.
        unsafe { write_exception_record(info) };
    }
    let previous = PREVIOUS_FILTER.load(Ordering::SeqCst);
    if previous == 0 {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    // SAFETY: a non-zero value is the filter function SetUnhandledExceptionFilter
    // returned in `install`, stored as its address.
    let previous: LPTOP_LEVEL_EXCEPTION_FILTER = unsafe { std::mem::transmute(previous) };
    match previous {
        // SAFETY: calling the previous filter with the same argument, as chaining expects.
        Some(f) => unsafe { f(info) },
        None => EXCEPTION_CONTINUE_SEARCH,
    }
}

/// A fixed-size text buffer: no allocation in the filter.
struct Buf<T: Copy + Default, const N: usize> {
    data: [T; N],
    len: usize,
    overflow: bool,
}

impl<T: Copy + Default + From<u8>, const N: usize> Buf<T, N> {
    fn new() -> Self {
        Buf {
            data: [T::default(); N],
            len: 0,
            overflow: false,
        }
    }

    fn push(&mut self, c: T) {
        if self.len < N {
            self.data[self.len] = c;
            self.len += 1;
        } else {
            self.overflow = true;
        }
    }

    fn str(&mut self, s: &str) {
        for b in s.bytes() {
            self.push(T::from(b));
        }
    }

    fn dec(&mut self, v: u64, width: usize) {
        let mut digits = [0u8; 20];
        let (mut v, mut n) = (v, 0);
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
            if v == 0 {
                break;
            }
        }
        for _ in n..width {
            self.push(T::from(b'0'));
        }
        for i in (0..n).rev() {
            self.push(T::from(digits[i]));
        }
    }

    fn hex(&mut self, v: u64, width: usize) {
        self.str("0x");
        for i in (0..width).rev() {
            let d = ((v >> (i * 4)) & 0xF) as u8;
            self.push(T::from(if d < 10 { b'0' + d } else { b'A' + d - 10 }));
        }
    }

    fn as_slice(&self) -> &[T] {
        &self.data[..self.len]
    }
}

/// Writes `<stem>.dmp` with the exception and `<stem>.log` with its code and address.
///
/// # Safety
/// `info` must be the filter's argument (or null).
unsafe fn write_exception_record(info: *const EXCEPTION_POINTERS) {
    let Some(dir) = DIR_WIDE.get() else {
        return;
    };
    // SAFETY: plain calls.
    let (t, pid, tid) = unsafe { (GetSystemTime(), GetCurrentProcessId(), GetCurrentThreadId()) };
    let mut stem: Buf<u16, 64> = Buf::new();
    stem.str(PREFIX);
    stem.dec(t.wYear.into(), 4);
    stem.dec(t.wMonth.into(), 2);
    stem.dec(t.wDay.into(), 2);
    stem.str("-");
    stem.dec(t.wHour.into(), 2);
    stem.dec(t.wMinute.into(), 2);
    stem.dec(t.wSecond.into(), 2);
    stem.str("-");
    stem.dec(pid.into(), 1);
    stem.str("-");
    stem.dec(SEQ.fetch_add(1, Ordering::Relaxed).into(), 1);
    let path = |ext: &str| {
        let mut p: Buf<u16, 600> = Buf::new();
        for &c in dir {
            p.push(c);
        }
        for &c in stem.as_slice() {
            p.push(c);
        }
        p.str(ext);
        p.push(0);
        p
    };

    let dump_path = path(".dmp");
    if !dump_path.overflow {
        // SAFETY: a NUL-terminated path in a live buffer; the handle is closed below.
        if let Ok(file) = unsafe { create_new(&dump_path) } {
            let exception = MINIDUMP_EXCEPTION_INFORMATION {
                ThreadId: tid,
                ExceptionPointers: info.cast_mut(),
                ClientPointers: BOOL(0),
            };
            // SAFETY: the exception information points at the filter's argument, which
            // lives until the filter returns; the handles are valid.
            unsafe {
                let _ = MiniDumpWriteDump(
                    GetCurrentProcess(),
                    pid,
                    file,
                    MiniDumpNormal,
                    (!info.is_null()).then_some(&exception as *const _),
                    None,
                    None,
                );
                let _ = CloseHandle(file);
            }
        }
    }

    let mut log: Buf<u8, 256> = Buf::new();
    log.str("light-view ");
    log.str(VERSION);
    log.str(" unhandled exception\r\n");
    // SAFETY: the system's EXCEPTION_POINTERS and its record, read if present.
    if let Some(record) = unsafe { info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) } {
        log.str("code: ");
        log.hex(u64::from(record.ExceptionCode.0 as u32), 8);
        log.str("\r\naddress: ");
        log.hex(record.ExceptionAddress as u64, 16);
        log.str("\r\n");
    }
    log.str("thread: ");
    log.dec(tid.into(), 1);
    log.str("\r\n");
    let log_path = path(".log");
    if !log_path.overflow {
        // SAFETY: as for the dump; the bytes live for the call.
        unsafe {
            if let Ok(file) = create_new(&log_path) {
                let _ = WriteFile(file, Some(log.as_slice()), None, None);
                let _ = CloseHandle(file);
            }
        }
    }
}

/// Creates a new file for writing.
///
/// # Safety
/// `path` must hold a NUL-terminated wide path.
unsafe fn create_new<const N: usize>(path: &Buf<u16, N>) -> windows::core::Result<HANDLE> {
    // SAFETY: the caller guarantees the NUL terminator; the buffer outlives the call.
    unsafe {
        CreateFileW(
            PCWSTR(path.as_slice().as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn stems(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn crash_dir_keeps_ten() {
        let dir = tempfile::tempdir().unwrap();
        // Fifteen records over three days, a lone log and a lone dump, and files that
        // are not records.
        let mut made = Vec::new();
        for i in 0..15u32 {
            let stem = format!("crash-202610{:02}-1200{:02}-77-{i}", 20 + i / 5, i);
            made.push(stem.clone());
            std::fs::write(dir.path().join(format!("{stem}.log")), "log").unwrap();
            std::fs::write(dir.path().join(format!("{stem}.dmp")), "dmp").unwrap();
        }
        for other in ["notes.txt", "crash.txt", "readme.log"] {
            std::fs::write(dir.path().join(other), "keep").unwrap();
        }
        prune(dir.path(), KEEP);
        let left = stems(dir.path());
        let want: Vec<String> = made[5..]
            .iter()
            .flat_map(|s| [format!("{s}.dmp"), format!("{s}.log")])
            .chain(["crash.txt", "notes.txt", "readme.log"].map(String::from))
            .collect();
        let mut want = want;
        want.sort();
        assert_eq!(left, want);

        // A new record makes room first: still ten, the oldest gone, the new one in.
        let log = write_record(dir.path(), "light-view test record\n").unwrap();
        assert!(log.exists());
        assert!(log.with_extension("dmp").metadata().unwrap().len() > 0);
        let records: BTreeSet<String> = stems(dir.path())
            .into_iter()
            .filter(|n| n.starts_with(PREFIX) && n.ends_with(".log"))
            .collect();
        assert_eq!(records.len(), KEEP);
        assert!(!records.contains(&format!("{}.log", made[5])));
        // A missing folder is not an error.
        prune(&dir.path().join("missing"), KEEP);
    }

    #[test]
    fn panic_rule() {
        assert!(should_record(false, false));
        assert!(!should_record(true, false), "caught on purpose");
        assert!(!should_record(false, true), "one record per thread");
    }

    #[test]
    fn filter_buffers_format_without_allocating() {
        let mut b: Buf<u8, 32> = Buf::new();
        b.dec(7, 2);
        b.str("-");
        b.dec(2026, 4);
        b.str(" ");
        b.hex(0xC000_0005, 8);
        assert_eq!(b.as_slice(), b"07-2026 0xC0000005");
        assert!(!b.overflow);
        let mut small: Buf<u16, 4> = Buf::new();
        small.str("abcdef");
        assert!(small.overflow);
        assert_eq!(small.as_slice().len(), 4);
    }

    /// Runs `child` in a new process of this test binary.
    fn run_child(mode: &str, dir: &Path) -> std::process::Output {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash::tests::child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("LV_CRASH_CHILD", mode)
            .env("LV_CRASH_DIR", dir)
            .output()
            .unwrap()
    }

    fn records(dir: &Path) -> Vec<String> {
        stems(dir)
            .into_iter()
            .filter(|n| n.starts_with(PREFIX))
            .collect()
    }

    /// A panic on a thread that does not catch it is recorded; one inside
    /// `lv_core::unwind::catch` (a decoder's, say) is not; the panic message still
    /// reaches the previous hook (stderr).
    #[test]
    fn uncaught_panics_are_recorded_and_caught_ones_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_child("panic", dir.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
        assert!(stderr.contains("caught on purpose"), "{stderr}");
        assert!(stderr.contains("uncaught boom"), "{stderr}");
        let names = records(dir.path());
        assert_eq!(names.len(), 2, "{names:?}");
        let log = names.iter().find(|n| n.ends_with(".log")).unwrap();
        let text = std::fs::read_to_string(dir.path().join(log)).unwrap();
        assert!(text.contains("uncaught boom"), "{text}");
        assert!(text.contains("thread: boom-thread"), "{text}");
        assert!(!text.contains("caught on purpose"), "{text}");
    }

    /// An unhandled SEH exception leaves a dump and a log with its code; the process
    /// still ends as it would have.
    #[test]
    fn unhandled_exceptions_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_child("seh", dir.path());
        assert!(!out.status.success());
        let names = records(dir.path());
        assert_eq!(names.len(), 2, "{names:?}");
        let dmp = names.iter().find(|n| n.ends_with(".dmp")).unwrap();
        let bytes = std::fs::read(dir.path().join(dmp)).unwrap();
        assert!(bytes.starts_with(b"MDMP"), "not a minidump");
        let log = names.iter().find(|n| n.ends_with(".log")).unwrap();
        let text = std::fs::read_to_string(dir.path().join(log)).unwrap();
        assert!(text.contains("code: 0xE0000042"), "{text}");
    }

    /// The child side of the two tests above; does nothing when run directly.
    #[test]
    #[ignore = "run by the crash tests in a child process"]
    fn child() {
        let (Ok(mode), Ok(dir)) = (
            std::env::var("LV_CRASH_CHILD"),
            std::env::var("LV_CRASH_DIR"),
        ) else {
            return;
        };
        install(Path::new(&dir));
        match mode.as_str() {
            "panic" => {
                let caught = std::thread::spawn(|| {
                    lv_core::unwind::catch(|| panic!("caught on purpose")).is_err()
                });
                assert!(caught.join().unwrap());
                let boom = std::thread::Builder::new()
                    .name("boom-thread".into())
                    .spawn(|| panic!("uncaught boom"))
                    .unwrap();
                assert!(boom.join().is_err());
            }
            "seh" => {
                use windows::Win32::System::Diagnostics::Debug::{
                    RaiseException, SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
                };
                // SAFETY: plain calls; the exception is meant to end this child process
                // (no crash dialog: the error mode is set first).
                unsafe {
                    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
                    RaiseException(0xE000_0042, 0, None);
                }
                unreachable!("the exception ends the process");
            }
            _ => panic!("unknown mode {mode}"),
        }
    }
}
