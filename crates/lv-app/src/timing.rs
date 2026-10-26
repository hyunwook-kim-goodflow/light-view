//! `--timing`: reports the time from process start to the first presented image.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Starts the clock; without this call `first_image_presented` does nothing.
pub fn enable(start: Instant) {
    let _ = START.set(start);
}

/// Call after a frame that drew an image was presented. Reports only the first time.
pub fn first_image_presented() {
    let Some(start) = START.get() else { return };
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let line = format!("first-present-ms={}", start.elapsed().as_millis());
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
