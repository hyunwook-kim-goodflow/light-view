//! One request in one sandboxed worker (ruling R3): the Explorer extension starts a worker
//! for each thumbnail or preview, sends it the request, takes one answer and ends it.
//!
//! The worker gets the same sandbox as the app's pool workers: the AppContainer when its
//! folder lets `ALL APPLICATION PACKAGES` in and the profile can be had, else the low
//! integrity sandbox with its mitigations (ruling R2), and the stepwise fallback of
//! [`start_suspended`](crate::sandbox::start_suspended) if a start with them fails. It works
//! from a medium or a low integrity host, and from one that is itself in a Job (the worker's
//! Job nests in it).
//!
//! The calling thread only waits on a channel: the request is written and the answer read on
//! threads of their own, so the call returns by its deadline whatever the pipes do. At the
//! deadline the worker's Job is terminated, and the call returns only once the worker has
//! exited (or a short grace period has passed, which is logged).

use std::io::{BufReader, Write};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use lv_ipc::{
    max_bytes_response, read_response, write_request, Request, Response, FITTED_FIXED_LEN,
};

use crate::appcontainer::{for_worker, profile_sid};
use crate::process::{drain_stderr, WRITE_SETTLE};
use crate::sandbox::{log, start_suspended, terminate_job, Pipes, Suspended};
use crate::threads;

/// Room for an answer's fixed fields and a `Failed` message, on top of its pixels.
const ANSWER_SLACK: u64 = 64 * 1024;

/// Why [`run_once`] has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The worker could not be started, or the request could not be encoded (then no worker
    /// was started).
    Start(String),
    /// No answer within the deadline; the worker was ended.
    Timeout,
    /// The worker exited without answering, sent something unreadable or answered another
    /// request.
    Protocol(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Start(e) => write!(f, "the worker did not start: {e}"),
            RunError::Timeout => write!(f, "the worker did not answer in time"),
            RunError::Protocol(e) => write!(f, "the worker failed: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Starts `exe` (`lv-worker.exe`) in its sandbox with `--max-pixels max_pixels`, sends it
/// `request` and returns its one answer, whatever its kind: the caller checks that it is
/// the kind it asked for. `deadline` counts from the call, the worker's start included; when
/// it passes, the worker is ended and the call fails with [`RunError::Timeout`]. The worker
/// never outlives the call.
pub fn run_once(
    exe: &Path,
    max_pixels: u64,
    request: &Request,
    deadline: Duration,
) -> Result<Response, RunError> {
    run(exe, max_pixels, &[], request, deadline, |_| {})
}

/// [`run_once`] with extra worker arguments, and `on_start` called with the suspended worker
/// before it runs (tests).
fn run(
    exe: &Path,
    max_pixels: u64,
    args: &[&str],
    request: &Request,
    deadline: Duration,
    on_start: impl FnOnce(&Suspended),
) -> Result<Response, RunError> {
    let started = Instant::now();
    // Encoded up front: a request lv-ipc refuses starts no worker, and the writer thread owns
    // its bytes.
    let mut bytes = Vec::new();
    write_request(&mut bytes, request)
        .map_err(|e| RunError::Start(format!("the request cannot be encoded: {e}")))?;
    let id = request.id();
    let cap = response_cap(request, max_pixels);

    let container = for_worker(exe, profile_sid);
    let suspended = start_suspended(exe, max_pixels, args, true, container.as_ref())
        .map_err(|e| RunError::Start(format!("{}: {e}", exe.display())))?;
    if let Some(e) = &suspended.sandboxed.container_failure {
        log(&format!(
            "start in the AppContainer failed ({e}); the worker runs without it"
        ));
    }
    on_start(&suspended);
    let (sandboxed, pipes) = suspended.resume().map_err(RunError::Start)?;
    let pid = sandboxed.pid;
    let Pipes {
        stdin,
        stdin_monitor,
        stdout,
        stderr,
    } = pipes;
    // Without our copy of its read end, a write to a worker that died fails at once.
    drop(stdin_monitor);

    // Counted threads (`live_threads`): they may outlive this call by a moment, and a DLL
    // caller must not be unloaded under them.
    let named = |what: &str| format!("lv-worker {pid} {what}");
    let (answers, answer) = mpsc::channel();
    let threads = threads::spawn(named("stderr"), move || drain_stderr(pid, stderr))
        .and_then(|()| {
            threads::spawn(named("writer"), move || {
                let mut stdin = stdin;
                // A failed write shows as the worker's missing answer. Dropping `stdin` then
                // tells the worker there is nothing more, so it exits after answering.
                let _ = stdin.write_all(&bytes).and_then(|()| stdin.flush());
            })
        })
        .and_then(|()| {
            threads::spawn(named("reader"), move || {
                let mut stdout = BufReader::with_capacity(64 * 1024, stdout);
                let _ = answers.send(read_response(&mut stdout, cap));
            })
        });

    let outcome = match threads {
        Err(e) => Err(RunError::Start(format!("worker thread: {e}"))),
        Ok(_) => match answer.recv_timeout(deadline.saturating_sub(started.elapsed())) {
            Ok(Ok(Some(response))) if response_id(&response) == id => Ok(response),
            Ok(Ok(Some(response))) => Err(RunError::Protocol(format!(
                "an answer to request {} instead of {id}",
                response_id(&response)
            ))),
            Ok(Ok(None)) => Err(RunError::Protocol("the worker exited".into())),
            Ok(Err(e)) => Err(RunError::Protocol(format!("protocol error: {e}"))),
            Err(RecvTimeoutError::Timeout) => Err(RunError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                Err(RunError::Protocol("the reader thread is gone".into()))
            }
        },
    };
    // Normally the worker has exited after its answer already; this is harmless then.
    terminate_job(&sandboxed.job, pid);
    if !sandboxed.wait_for_exit(WRITE_SETTLE) {
        log(&format!(
            "worker {pid} has not exited after TerminateJobObject"
        ));
    }
    if let Err(e) = &outcome {
        log(&format!("worker {pid}: {e}"));
    }
    outcome
}

/// Largest answer payload read for `request`: a thumbnail's pixels, else an image of
/// `max_pixels` pixels as the app's pool allows, or for a request that carries bytes (a
/// rotation or a WebP fit) an answer about its size, whichever is larger.
fn response_cap(request: &Request, max_pixels: u64) -> u64 {
    let image = max_pixels.saturating_mul(4).saturating_add(ANSWER_SLACK);
    match request {
        Request::ThumbnailHeif { max_side, .. } => {
            let side = u64::from(*max_side);
            image.min(
                side.saturating_mul(side)
                    .saturating_mul(4)
                    .saturating_add(ANSWER_SLACK),
            )
        }
        Request::RotateJpeg { data, .. } | Request::FitWebp { rgba: data, .. } => {
            image.max(FITTED_FIXED_LEN + max_bytes_response(data.len() as u64))
        }
        Request::DecodeHeif { .. } | Request::Shutdown => image,
    }
}

fn response_id(response: &Response) -> u64 {
    match response {
        Response::Image { id, .. }
        | Response::Failed { id, .. }
        | Response::Bytes { id, .. }
        | Response::Fitted { id, .. }
        | Response::CantFit { id, .. } => *id,
    }
}

#[cfg(test)]
mod tests;
