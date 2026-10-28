//! One measured run: starts `lv-app.exe --bench <file>`, reads its `--timing` line,
//! waits for its memory to settle (the neighbours' prefetch), adds up the private bytes
//! of the app and its HEIF workers, and closes the app with `WM_CLOSE` posted to its
//! own window (found by class and process id). Nothing else is touched: `--bench` makes
//! the app use default settings, save none and skip the single-instance check.
//!
//! A resize run ([`measure_resize`]) starts `lv-app.exe --bench --resize <files>…`
//! instead, waits for the app's `estimates-done-ms=` line (every photo has its estimate)
//! and watches the private bytes of the app and its workers all along for their peak.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

/// How long the app may take to present its first image.
const FIRST_PRESENT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the app may take to estimate every photo of a resize run.
const ESTIMATES_TIMEOUT: Duration = Duration::from_secs(300);
/// How often a resize run samples memory for its peak.
const PEAK_POLL: Duration = Duration::from_millis(20);
/// Memory counts as settled once it moved less than this over `SETTLE_WINDOW`.
const SETTLE_DELTA: u64 = 2 << 20;
const SETTLE_WINDOW: Duration = Duration::from_millis(600);
const SETTLE_MAX: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
const WINDOW_CLASS: &str = "goodflow.LightView.Window";

/// What one run measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// The app's own `--timing` line, as `name=value` pairs.
    pub steps: Vec<(String, f64)>,
    /// From the bench starting the process to the line arriving.
    pub spawn_to_line_ms: f64,
    /// Private bytes (MB) once memory settled: the app, and its workers together.
    pub app_mb: f64,
    pub workers_mb: f64,
    /// The app's peak private bytes (MB) up to then.
    pub app_peak_mb: f64,
}

impl Sample {
    pub fn step(&self, name: &str) -> Option<f64> {
        self.steps.iter().find(|(n, _)| n == name).map(|s| s.1)
    }
}

/// Parses a line of `name=value` pairs that starts with `first`, like
/// `first-present-ms=512 private-mb=180 decode-ms=230.5 …`; `None` if it does not.
pub fn parse_line(line: &str, first: &str) -> Option<Vec<(String, f64)>> {
    let line = line.trim();
    if !line.starts_with(first) {
        return None;
    }
    line.split_whitespace()
        .map(|pair| {
            let (name, value) = pair.split_once('=')?;
            Some((name.to_string(), value.parse().ok()?))
        })
        .collect()
}

/// What one resize run measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResizeSample {
    /// The app's `estimates-done-ms=` line, as `name=value` pairs.
    pub steps: Vec<(String, f64)>,
    /// The highest private bytes (MB) of the app and its workers together, sampled every
    /// `PEAK_POLL` from the start until every estimate was in.
    pub peak_total_mb: f64,
    /// The app's own peak private bytes (MB), as Windows counts it.
    pub app_peak_mb: f64,
    /// The highest worker total sampled (MB).
    pub workers_peak_mb: f64,
}

impl ResizeSample {
    pub fn step(&self, name: &str) -> Option<f64> {
        self.steps.iter().find(|(n, _)| n == name).map(|s| s.1)
    }

    /// The peak private bytes: the sampled total, or the app's own peak when sampling
    /// missed it.
    pub fn peak_mb(&self) -> f64 {
        self.peak_total_mb.max(self.app_peak_mb)
    }
}

/// Opens `files` on the resize screen (`--bench-webp`: as WebP) and measures until every
/// photo has its estimate.
pub fn measure_resize(app: &Path, files: &[PathBuf], webp: bool) -> Result<ResizeSample, String> {
    let mut child = Command::new(app)
        .arg(if webp { "--bench-webp" } else { "--bench" })
        .arg("--resize")
        .args(files)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", app.display()))?;
    let result = measure_resize_child(&mut child);
    close(&mut child);
    result
}

fn measure_resize_child(child: &mut Child) -> Result<ResizeSample, String> {
    let rx = read_lines(child, "estimates-done-ms=")?;
    let pid = child.id();
    let started = Instant::now();
    let (mut peak_total, mut workers_peak) = (0u64, 0u64);
    let steps = loop {
        let (app, workers) = memory(pid);
        peak_total = peak_total.max(app + workers);
        workers_peak = workers_peak.max(workers);
        match rx.recv_timeout(PEAK_POLL) {
            Ok(steps) => break steps,
            Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() < ESTIMATES_TIMEOUT => {}
            Err(_) => return Err("the app did not estimate every photo in time".into()),
        }
    };
    let app_peak_mb = counters(pid).map_or(0.0, |c| mb(c.PeakPagefileUsage as u64));
    Ok(ResizeSample {
        steps,
        peak_total_mb: mb(peak_total),
        app_peak_mb,
        workers_peak_mb: mb(workers_peak),
    })
}

/// Reads the child's stderr on a thread until it exits (so its pipe never fills up) and
/// sends each line starting with `first`, parsed; other lines are echoed.
fn read_lines(
    child: &mut Child,
    first: &'static str,
) -> Result<mpsc::Receiver<Vec<(String, f64)>>, String> {
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(steps) = parse_line(&line, first) {
                let _ = tx.send(steps);
            } else if !line.trim().is_empty() && !line.starts_with("first-present-ms=") {
                eprintln!("  app: {line}");
            }
        }
    });
    Ok(rx)
}

pub fn measure(app: &Path, file: &Path) -> Result<Sample, String> {
    let started = Instant::now();
    let mut child = Command::new(app)
        .arg("--bench")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", app.display()))?;
    let result = measure_child(&mut child, started);
    close(&mut child);
    result
}

fn measure_child(child: &mut Child, started: Instant) -> Result<Sample, String> {
    let rx = read_lines(child, "first-present-ms=")?;
    let steps = rx
        .recv_timeout(FIRST_PRESENT_TIMEOUT)
        .map_err(|_| "the app presented no image in time".to_string())?;
    let spawn_to_line_ms = started.elapsed().as_secs_f64() * 1000.0;
    let pid = child.id();
    let (app_mb, workers_mb) = settled_memory(pid);
    let app_peak_mb = counters(pid).map_or(0.0, |c| mb(c.PeakPagefileUsage as u64));
    Ok(Sample {
        steps,
        spawn_to_line_ms,
        app_mb,
        workers_mb,
        app_peak_mb,
    })
}

/// Polls until the total private bytes stop moving, then returns (app, workers) in MB.
fn settled_memory(pid: u32) -> (f64, f64) {
    let started = Instant::now();
    let mut last = memory(pid);
    let mut stable_since = Instant::now();
    while started.elapsed() < SETTLE_MAX {
        std::thread::sleep(POLL);
        let now = memory(pid);
        if (now.0 + now.1).abs_diff(last.0 + last.1) > SETTLE_DELTA {
            stable_since = Instant::now();
        }
        last = now;
        if stable_since.elapsed() >= SETTLE_WINDOW {
            break;
        }
    }
    (mb(last.0), mb(last.1))
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1 << 20)
}

/// Private bytes of process `pid` and, summed, of its child processes.
fn memory(pid: u32) -> (u64, u64) {
    let workers = children(pid).into_iter().filter_map(private_bytes).sum();
    (private_bytes(pid).unwrap_or(0), workers)
}

fn private_bytes(pid: u32) -> Option<u64> {
    counters(pid).map(|c| c.PrivateUsage as u64)
}

fn counters(pid: u32) -> Option<PROCESS_MEMORY_COUNTERS_EX> {
    // SAFETY: plain calls; the handle is closed below and `counters` is a live local of
    // the size passed.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        let ok = GetProcessMemoryInfo(
            process,
            std::ptr::from_mut(&mut counters).cast(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
        let _ = CloseHandle(process);
        ok.ok()?;
        Some(counters)
    }
}

/// Process ids whose parent is `pid`.
fn children(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    // SAFETY: plain calls on a snapshot handle closed below; `entry` is a live local
    // with `dwSize` set.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more {
            if entry.th32ParentProcessID == pid && entry.th32ProcessID != pid {
                out.push(entry.th32ProcessID);
            }
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Posts `WM_CLOSE` to the app's own window and waits for it to exit; a process that
/// does not exit in time (or has no window) is ended. It is the bench's own child.
fn close(child: &mut Child) {
    if let Some(hwnd) = app_window(child.id()) {
        // SAFETY: plain call; posting to a window that has just gone away only fails.
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let started = Instant::now();
        while started.elapsed() < CLOSE_TIMEOUT {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(POLL);
        }
        eprintln!("  the app did not close in time; ending it");
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The top-level window of class `goodflow.LightView.Window` owned by process `pid`.
fn app_window(pid: u32) -> Option<HWND> {
    struct Search {
        pid: u32,
        found: Option<HWND>,
    }
    extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `&mut Search` passed to EnumWindows below, alive for
        // the whole enumeration.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut owner = 0u32;
        // SAFETY: plain calls with live locals.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        if owner != search.pid {
            return true.into();
        }
        let mut class = [0u16; 64];
        // SAFETY: as above.
        let len = unsafe { GetClassNameW(hwnd, &mut class) };
        if String::from_utf16_lossy(&class[..len.max(0) as usize]) == WINDOW_CLASS {
            search.found = Some(hwnd);
            return false.into();
        }
        true.into()
    }
    let mut search = Search { pid, found: None };
    // The app's window may not exist yet only if it failed early; try for a moment.
    for _ in 0..20 {
        // SAFETY: `visit` only uses `search` during this call.
        let _ = unsafe {
            EnumWindows(
                Some(visit),
                LPARAM(std::ptr::from_mut(&mut search) as isize),
            )
        };
        if search.found.is_some() {
            break;
        }
        std::thread::sleep(POLL);
    }
    search.found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_line_parses_into_steps() {
        let steps = parse_line(
            "first-present-ms=512 private-mb=180 decode-ms=230.5 shown-at=40.0\r\n",
            "first-present-ms=",
        )
        .unwrap();
        assert_eq!(steps[0], ("first-present-ms".to_string(), 512.0));
        assert_eq!(steps[2], ("decode-ms".to_string(), 230.5));
        assert_eq!(steps.len(), 4);
        assert_eq!(
            parse_line("light-view: something else", "first-present-ms="),
            None
        );
        assert_eq!(parse_line("first-present-ms=x", "first-present-ms="), None);
        let done = parse_line(
            "estimates-done-ms=2400 photos=10 ready=10 first-estimate-ms=520 private-mb=310 peak-private-mb=640",
            "estimates-done-ms=",
        )
        .unwrap();
        assert_eq!(done[0], ("estimates-done-ms".to_string(), 2400.0));
        assert_eq!(done[2], ("ready".to_string(), 10.0));
        assert_eq!(done[5], ("peak-private-mb".to_string(), 640.0));
        assert_eq!(parse_line("first-present-ms=1", "estimates-done-ms="), None);
    }
}
