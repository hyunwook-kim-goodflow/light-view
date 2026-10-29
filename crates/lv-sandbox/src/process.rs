//! One running worker process: its sandbox, its pipes and the threads that write requests
//! to it, read its answers and copy its diagnostics into the app's log.

use std::fs::File;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use lv_ipc::{read_response, write_request, IpcError, Request, Response};

use crate::appcontainer::Container;
use crate::sandbox::{self, log, Sandboxed, StdinMonitor};

/// How long a failed first request waits for the writer thread to report its write.
pub const WRITE_SETTLE: Duration = Duration::from_secs(1);

/// Longest worker stderr line copied into the log, in bytes.
const MAX_LOG_LINE: usize = 4096;

pub struct Worker {
    pub sandboxed: Sandboxed,
    /// Has given at least one valid answer, so it counts as started.
    pub answered: bool,
    pub requests: Sender<Request>,
    /// What the writer and reader threads report, in one channel so a call waits on both.
    pub events: Receiver<Event>,
    pub stdin_monitor: StdinMonitor,
}

pub enum Event {
    /// The writer's outcome for request `id`.
    Written { id: u64, outcome: WriteOutcome },
    /// The next message from the worker, or the end of its output.
    Response(Result<Option<Response>, IpcError>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Written and flushed.
    Sent,
    /// The request could not be encoded. Nothing was written and the worker saw nothing:
    /// the app's mistake, not the worker's.
    Rejected,
    /// The pipe failed (the worker is gone or stopped reading); the writer has ended.
    Failed,
}

/// Whether a worker that failed its first request (`id`) had read that request in full.
/// `wrote` is the writer's report if it already came. False when the bytes are still in its
/// stdin pipe, or the writer could not put them there (failed, or is still blocked on a full
/// pipe). If the pipe cannot be inspected, assume the worker read it: wrongly keeping the
/// mitigations only costs a start failure, wrongly dropping them weakens the sandbox.
pub fn took_request(worker: &Worker, id: u64, mut wrote: Option<bool>) -> bool {
    let deadline = Instant::now() + WRITE_SETTLE;
    while wrote.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        match worker.events.recv_timeout(left) {
            Ok(Event::Written { id: got, outcome }) if got == id => {
                wrote = Some(outcome == WriteOutcome::Sent)
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    wrote == Some(true) && !matches!(worker.stdin_monitor.unread(), Ok(n) if n > 0)
}

impl Worker {
    /// Spawns a sandboxed worker with its stderr, reader and writer threads. The threads end
    /// when the process does (its pipes close) or the `Worker` is dropped. The reader
    /// refuses any message whose payload is over `response_cap` bytes.
    pub fn start(
        exe: &Path,
        max_pixels: u64,
        args: &[&str],
        mitigations: bool,
        container: Option<&Container>,
        response_cap: u64,
    ) -> Result<Worker, String> {
        let (sandboxed, pipes) = sandbox::spawn(exe, max_pixels, args, mitigations, container)?;
        let pid = sandboxed.pid;
        let named = |what: &str| thread::Builder::new().name(format!("lv-worker {pid} {what}"));

        let stderr = pipes.stderr;
        named("stderr")
            .spawn(move || drain_stderr(pid, stderr))
            .map_err(|e| format!("stderr thread: {e}"))?;

        let (events_tx, events) = mpsc::channel();
        let mut stdout = BufReader::with_capacity(64 * 1024, pipes.stdout);
        let response_tx = events_tx.clone();
        named("reader")
            .spawn(move || loop {
                let message = read_response(&mut stdout, response_cap);
                let last = !matches!(message, Ok(Some(_)));
                if response_tx.send(Event::Response(message)).is_err() || last {
                    break;
                }
            })
            .map_err(|e| format!("reader thread: {e}"))?;

        let (requests, request_rx) = mpsc::channel::<Request>();
        let mut stdin = BufWriter::new(pipes.stdin);
        named("writer")
            .spawn(move || {
                for request in request_rx {
                    let id = request.id();
                    // lv-ipc validates a message before writing any of it, so InvalidInput
                    // leaves the pipe untouched and the writer can go on.
                    let outcome = match write_request(&mut stdin, &request) {
                        Err(e) if e.kind() == ErrorKind::InvalidInput => WriteOutcome::Rejected,
                        Err(_) => WriteOutcome::Failed,
                        Ok(()) if stdin.flush().is_ok() => WriteOutcome::Sent,
                        Ok(()) => WriteOutcome::Failed,
                    };
                    let _ = events_tx.send(Event::Written { id, outcome });
                    if outcome == WriteOutcome::Failed {
                        break;
                    }
                }
            })
            .map_err(|e| format!("writer thread: {e}"))?;

        Ok(Worker {
            sandboxed,
            answered: false,
            requests,
            events,
            stdin_monitor: pipes.stdin_monitor,
        })
    }
}

/// Copies the worker's diagnostics into the app's log so the pipe never fills up.
fn drain_stderr(pid: u32, stderr: File) {
    for_each_log_line(stderr, |line| log(&format!("lv-worker {pid}: {line}")));
}

/// Splits untrusted `input` into log lines with bounded memory: each line keeps at most
/// [`MAX_LOG_LINE`] bytes (the rest up to the next newline is dropped and the line marked
/// `[truncated]`), and control characters are escaped so the worker cannot forge log lines.
fn for_each_log_line(mut input: impl Read, mut emit: impl FnMut(String)) {
    let mut chunk = [0u8; 4096];
    let mut line = Vec::with_capacity(MAX_LOG_LINE);
    let mut truncated = false;
    loop {
        let n = match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &byte in &chunk[..n] {
            if byte == b'\n' {
                emit(log_line(&line, truncated));
                line.clear();
                truncated = false;
            } else if line.len() < MAX_LOG_LINE {
                line.push(byte);
            } else {
                truncated = true;
            }
        }
    }
    if !line.is_empty() || truncated {
        emit(log_line(&line, truncated));
    }
}

fn log_line(bytes: &[u8], truncated: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    if truncated {
        out.push_str(" [truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines_are_bounded_and_escaped() {
        let mut input = b"a\nb\r\n".to_vec();
        input.extend(std::iter::repeat_n(b'x', 3 * MAX_LOG_LINE));
        input.extend(b"\nc\x1b[31m\rforged\nlast");
        let mut lines = Vec::new();
        for_each_log_line(&input[..], |line| lines.push(line));
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines[0], "a");
        assert_eq!(lines[1], "b");
        assert_eq!(
            lines[2],
            format!("{} [truncated]", "x".repeat(MAX_LOG_LINE))
        );
        assert_eq!(lines[3], "c\\u{1b}[31m\\rforged");
        assert_eq!(lines[4], "last");
    }
}
