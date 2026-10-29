//! SPIKE: one single-use sandboxed worker, one `DecodeHeif` request, one answer.

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lv_ipc::{read_response, write_request, Request, Response};
use windows::Win32::Foundation::HANDLE;

use crate::{appcontainer, host, sandbox, spike_log};

/// The app's default `max_pixels` (lv_core::decode::DecodeLimits::default()).
const MAX_PIXELS: u64 = 268_435_456;
const DEADLINE: Duration = Duration::from_secs(10);

/// Starts a worker next to this DLL, decodes `data`, logs every step. Returns a summary.
pub fn decode_once(data: Vec<u8>) -> String {
    let started = Instant::now();
    let dll = crate::dll_path();
    let exe = dll.with_file_name("lv-worker.exe");
    spike_log(&format!("worker exe {}", exe.display()));

    let t = Instant::now();
    let container = appcontainer::for_worker(&exe, appcontainer::profile_sid);
    spike_log(&format!(
        "container lookup: {} in {:?}",
        if container.is_some() {
            "AppContainer profile available"
        } else {
            "no AppContainer"
        },
        t.elapsed()
    ));

    let t = Instant::now();
    let suspended = match sandbox::start_suspended(&exe, MAX_PIXELS, &[], true, container.as_ref())
    {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("WORKER START FAILED: {e}");
            spike_log(&msg);
            return msg;
        }
    };
    let born_in_job = suspended.born_in_job;
    let (sandboxed, pipes) = match suspended.resume() {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("WORKER RESUME FAILED: {e}");
            spike_log(&msg);
            return msg;
        }
    };
    let spawn_time = t.elapsed();
    let in_container = container.is_some() && sandboxed.container_failure.is_none();
    spike_log(&format!(
        "worker pid={} started in {spawn_time:?}; born_in_job={born_in_job}; requested_container={}; container_failure={:?}; worker token: {}",
        sandboxed.pid,
        container.is_some(),
        sandboxed.container_failure,
        host::describe_process(HANDLE(sandboxed.process.as_raw_handle())),
    ));

    let mut stderr = pipes.stderr;
    let stderr_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let (tx, rx) = mpsc::channel();
    let mut stdout = BufReader::new(pipes.stdout);
    std::thread::spawn(move || {
        let _ = tx.send(read_response(
            &mut stdout,
            MAX_PIXELS.saturating_mul(4) + 64 * 1024,
        ));
    });

    let t = Instant::now();
    let mut stdin = BufWriter::new(pipes.stdin);
    let request = Request::DecodeHeif {
        id: 1,
        max_pixels: MAX_PIXELS,
        data,
    };
    if let Err(e) = write_request(&mut stdin, &request).and_then(|()| stdin.flush()) {
        spike_log(&format!("write request failed: {e}"));
    }
    let outcome = match rx.recv_timeout(DEADLINE) {
        Ok(Ok(Some(Response::Image {
            id, width, height, ..
        }))) => format!("DECODED id={id} {width}x{height}"),
        Ok(Ok(Some(other))) => format!("WORKER ANSWERED {other:?}"),
        Ok(Ok(None)) => "WORKER CLOSED OUTPUT WITHOUT ANSWER".to_string(),
        Ok(Err(e)) => format!("PROTOCOL ERROR {e:?}"),
        Err(_) => "TIMEOUT".to_string(),
    };
    let decode_time = t.elapsed();
    drop(stdin);
    drop(sandboxed); // terminates the Job
    let worker_stderr = stderr_thread.join().unwrap_or_default();
    let summary = format!(
        "{outcome}; appcontainer={in_container}; spawn={spawn_time:?} request+decode={decode_time:?} total={:?}",
        started.elapsed()
    );
    spike_log(&format!(
        "worker result: {summary}; worker stderr: {:?}",
        worker_stderr.trim()
    ));
    summary
}
