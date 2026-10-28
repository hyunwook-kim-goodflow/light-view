//! `--timing`: reports the time from process start to the first presented image, with
//! the steps on the way, as one line of `name=value` pairs:
//!
//! `first-present-ms=512 private-mb=180 decode-ms=230 decode.read=3.1 … upload-ms=20`
//!
//! `first-present-ms` comes first (older scripts read only it). Every other value is a
//! step's time in milliseconds, except `*-at`, which is a time since the start, and
//! `private-mb`, this process's private bytes when the image was presented. lv-bench
//! reads the line.
//!
//! On the resize screen a second line follows once every photo has its estimate:
//!
//! `estimates-done-ms=2400 photos=10 ready=10 first-estimate-ms=520 private-mb=310 peak-private-mb=640`
//!
//! (times since the start; `ready` counts the photos with a result, not 맞추기 불가 or a
//! failure; `peak-private-mb` is this process's highest private bytes so far, the HEIF
//! and WebP workers not included).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use lv_core::listing::same_path;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

static START: OnceLock<Instant> = OnceLock::new();
/// The file given on the command line: only its decode is reported.
static TARGET: OnceLock<PathBuf> = OnceLock::new();
static STEPS: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());
static REPORTED: AtomicBool = AtomicBool::new(false);
/// The first resize estimate's time since the start, and whether the estimates line was
/// printed.
static FIRST_ESTIMATE: OnceLock<f64> = OnceLock::new();
static ESTIMATES_REPORTED: AtomicBool = AtomicBool::new(false);

/// Starts the clock; without this call nothing here does anything.
pub fn enable(start: Instant) {
    let _ = START.set(start);
}

/// Whether `--timing` is on and the first image has not been reported yet.
pub fn active() -> bool {
    START.get().is_some() && !REPORTED.load(Ordering::Relaxed)
}

/// The file whose decode is reported (the one opened at start-up).
pub fn set_target(path: &Path) {
    if START.get().is_some() {
        let _ = TARGET.set(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    }
}

/// Whether `path` is the file opened at start-up and its steps are still wanted.
pub fn is_target(path: &Path) -> bool {
    active() && TARGET.get().is_some_and(|t| same_path(t, path))
}

/// Records a step (milliseconds). The first value given for a name is kept.
pub fn add(name: &str, ms: f64) {
    if !active() {
        return;
    }
    let mut steps = STEPS.lock().unwrap_or_else(|e| e.into_inner());
    if !steps.iter().any(|(n, _)| n == name) {
        steps.push((name.to_string(), ms));
    }
}

/// Records `name` as the time since the start (`<name>` should end in `-at`).
pub fn mark(name: &str) {
    if let Some(start) = START.get() {
        add(name, start.elapsed().as_secs_f64() * 1000.0);
    }
}

/// Runs `f`, recording its time as `name` while timing is active.
pub fn step<T>(name: &str, f: impl FnOnce() -> T) -> T {
    if !active() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    add(name, start.elapsed().as_secs_f64() * 1000.0);
    out
}

/// Call after a frame that drew an image was presented. Reports only the first time.
pub fn first_image_presented() {
    let Some(start) = START.get() else { return };
    let elapsed = start.elapsed().as_millis();
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let steps = std::mem::take(&mut *STEPS.lock().unwrap_or_else(|e| e.into_inner()));
    emit(&format_line(elapsed, private_mb(), &steps));
}

/// Call when a resize estimate arrived, with the session's photo count, how many still
/// wait for theirs and how many have a result. The first time none waits, prints the
/// estimates line.
pub fn estimate_arrived(photos: usize, pending: usize, ready: usize) {
    let Some(start) = START.get() else { return };
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let first = *FIRST_ESTIMATE.get_or_init(|| ms);
    if pending > 0 || ESTIMATES_REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let mem = counters();
    let mb = |b: usize| (b as u64) >> 20;
    emit(&format_estimates_line(
        ms,
        photos,
        ready,
        first,
        // PeakPagefileUsage is the peak of the commit charge, which for a process is its
        // private bytes: the peak of PrivateUsage.
        mem.map(|c| (mb(c.PrivateUsage), mb(c.PeakPagefileUsage))),
    ));
}

fn format_estimates_line(
    ms: f64,
    photos: usize,
    ready: usize,
    first: f64,
    mem: Option<(u64, u64)>,
) -> String {
    let mut line = format!(
        "estimates-done-ms={ms:.0} photos={photos} ready={ready} first-estimate-ms={first:.0}"
    );
    if let Some((now, peak)) = mem {
        line.push_str(&format!(" private-mb={now} peak-private-mb={peak}"));
    }
    line
}

/// Prints `line` to stderr and appends it to the timing log.
fn emit(line: &str) {
    eprintln!("{line}");
    // The release build has no console, so stderr may go nowhere: also log to a file.
    let path = std::env::temp_dir().join("light-view-timing.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn format_line(first_present_ms: u128, private_mb: Option<u64>, steps: &[(String, f64)]) -> String {
    let mut line = format!("first-present-ms={first_present_ms}");
    if let Some(mb) = private_mb {
        line.push_str(&format!(" private-mb={mb}"));
    }
    for (name, ms) in steps {
        line.push_str(&format!(" {name}={ms:.1}"));
    }
    line
}

/// This process's private bytes, in MB (2^20).
fn private_mb() -> Option<u64> {
    counters().map(|c| c.PrivateUsage as u64 >> 20)
}

/// This process's memory counters.
fn counters() -> Option<PROCESS_MEMORY_COUNTERS_EX> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: `counters` is a live local of `size` bytes; the pseudo handle needs no
    // closing.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::from_mut(&mut counters).cast(),
            size,
        )
    }
    .ok()?;
    Some(counters)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_with_first_present_and_lists_steps_in_order() {
        let steps = vec![
            ("decode-ms".to_string(), 12.34),
            ("shown-at".to_string(), 5.0),
        ];
        assert_eq!(
            format_line(40, Some(7), &steps),
            "first-present-ms=40 private-mb=7 decode-ms=12.3 shown-at=5.0"
        );
        assert_eq!(format_line(3, None, &[]), "first-present-ms=3");
    }

    #[test]
    fn estimates_line_names_its_values() {
        assert_eq!(
            format_estimates_line(2400.4, 10, 9, 519.6, Some((310, 640))),
            "estimates-done-ms=2400 photos=10 ready=9 first-estimate-ms=520 private-mb=310 peak-private-mb=640"
        );
        assert_eq!(
            format_estimates_line(9.0, 1, 0, 9.0, None),
            "estimates-done-ms=9 photos=1 ready=0 first-estimate-ms=9"
        );
    }

    #[test]
    fn private_bytes_are_readable() {
        assert!(private_mb().is_some());
    }
}
