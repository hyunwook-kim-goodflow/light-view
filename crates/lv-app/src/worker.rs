//! Pool of sandboxed `lv-worker.exe` processes that decode HEIF files and rotate JPEGs
//! losslessly for the viewer.
//!
//! Decodes share the pool's long-lived workers. Each JPEG rotation runs in a worker of
//! its own, started for it and ended after it ([`WorkerPool::single_use`]), so a worker
//! that a hostile file took over cannot answer a rotation (ruling R21).
//!
//! Each slot holds at most one worker (see [`sandbox`]). A call ([`WorkerPool::request`])
//! takes an idle slot, starts a worker in it if needed, sends one request and waits for the
//! answer on a channel fed by the worker's reader thread: if no matching answer arrives
//! within the request's deadline (ruling R4: 10 s for a decode, 10 s + 10 s per 100 MP for
//! a rotation), or the worker dies or breaks the protocol (an answer of the wrong kind
//! included), its Job is terminated, the slot is emptied and the call fails with
//! `PoolError::WorkerUnavailable` (a decode with `Corrupt("작업자가 응답하지 않아요")`);
//! the next call starts a fresh worker. A worker that
//! died while idle, after answering, is replaced before the next request without failing
//! it. Decodes and rotations follow the same rules throughout.
//!
//! A worker counts as started only once it has given a valid answer: failing to spawn, or
//! dying, hanging or breaking the protocol on its first request, is a start failure, and
//! after three in a row the slot gives up. Once every slot has, every call answers
//! `Unsupported` at once.
//!
//! Only a worker that was already dead or stuck *before it took its request* (it had exited
//! before we sent, or left the request unread in its stdin pipe) makes the slot drop the
//! best-effort creation mitigations for later workers, in case they are what kills it. A
//! worker that dies after reading its first request may have been killed by the file, and a
//! hostile file must not be able to weaken the sandbox for the next one (ruling R11a).
//!
//! When the app exits, [`WorkerPool::shutdown`] ends every worker, busy ones included, so
//! no prefetch thread is left waiting out a hung worker's timeout.

mod protocol;
mod sandbox;

use std::fs::File;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};

use lv_core::decode::DecodeError;
use lv_ipc::{read_response, write_request, IpcError, Request, Response};

use protocol::{request_data_cap, response_cap, to_request, Expect};
pub use protocol::{PoolError, PoolRequest, Reply};
use sandbox::{log, terminate_job, Sandboxed, StdinMonitor};

/// How long a worker may take to decode one file (ruling R4), and a caller may wait for a
/// free slot.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Consecutive failed starts after which a slot stops trying.
const MAX_SPAWN_FAILURES: u32 = 3;
const NOT_RESPONDING: &str = "작업자가 응답하지 않아요";
/// Longest worker stderr line copied into the log, in bytes.
const MAX_LOG_LINE: usize = 4096;
/// How long a failed first request waits for the writer thread to report its write.
const WRITE_SETTLE: Duration = Duration::from_secs(1);

pub struct WorkerPool {
    exe: PathBuf,
    max_pixels: u64,
    /// Largest file sent to a worker; larger ones are `FileTooLarge` without being sent.
    data_cap: u64,
    timeout: Duration,
    /// Extra arguments for each slot's first worker only (test fault injection).
    first_args: Vec<String>,
    /// Extra arguments for every worker (test fault injection).
    every_args: Vec<String>,
    state: Mutex<PoolState>,
    slot_freed: Condvar,
    next_id: AtomicU64,
    /// Single-use workers start without the best-effort mitigations (sticky; set only
    /// when one was dead before it took its request).
    single_use_degraded: AtomicBool,
    /// Pid of every single-use worker started (tests).
    #[cfg(test)]
    single_use_pids: Mutex<Vec<u32>>,
}

struct PoolState {
    idle: Vec<Slot>,
    /// Slots that have not given up, idle or in use.
    live: usize,
    /// Set by `shutdown`: no new workers, every call answers `Unsupported`.
    closed: bool,
    /// The Job of every worker started, idle or busy, so `shutdown` can end busy ones too.
    jobs: Vec<(u32, Weak<OwnedHandle>)>,
}

#[derive(Default)]
struct Slot {
    worker: Option<Worker>,
    /// Consecutive start failures; reset by a worker's first valid answer.
    spawn_failures: u32,
    started_once: bool,
    /// Workers start without the best-effort mitigations (sticky; set only when a worker
    /// was dead before it took its request).
    degraded: bool,
}

struct Worker {
    sandboxed: Sandboxed,
    /// Has given at least one valid answer, so it counts as started.
    answered: bool,
    requests: Sender<Request>,
    /// What the writer and reader threads report, in one channel so a call waits on both.
    events: Receiver<Event>,
    stdin_monitor: StdinMonitor,
}

enum Event {
    /// The writer's outcome for request `id`.
    Written { id: u64, outcome: WriteOutcome },
    /// The next message from the worker, or the end of its output.
    Response(Result<Option<Response>, IpcError>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteOutcome {
    /// Written and flushed.
    Sent,
    /// The request could not be encoded. Nothing was written and the worker saw nothing:
    /// the app's mistake, not the worker's.
    Rejected,
    /// The pipe failed (the worker is gone or stopped reading); the writer has ended.
    Failed,
}

impl WorkerPool {
    /// Starts `size` workers right away. Never panics: failures are logged and recorded in
    /// the slots, and later calls retry.
    pub fn start(exe: PathBuf, size: usize, max_pixels: u64) -> WorkerPool {
        Self::start_with(exe, size, max_pixels, DEFAULT_TIMEOUT, &[], &[])
    }

    pub(crate) fn start_with(
        exe: PathBuf,
        size: usize,
        max_pixels: u64,
        timeout: Duration,
        first_args: &[&str],
        every_args: &[&str],
    ) -> WorkerPool {
        let pool = WorkerPool {
            exe,
            max_pixels,
            data_cap: request_data_cap(max_pixels),
            timeout,
            first_args: first_args.iter().map(|a| a.to_string()).collect(),
            every_args: every_args.iter().map(|a| a.to_string()).collect(),
            state: Mutex::new(PoolState {
                idle: Vec::with_capacity(size),
                live: size,
                closed: false,
                jobs: Vec::new(),
            }),
            slot_freed: Condvar::new(),
            next_id: AtomicU64::new(1),
            single_use_degraded: AtomicBool::new(false),
            #[cfg(test)]
            single_use_pids: Mutex::new(Vec::new()),
        };
        let slots: Vec<Slot> = (0..size)
            .map(|_| {
                let mut slot = Slot::default();
                pool.try_spawn(&mut slot);
                slot
            })
            .collect();
        pool.lock().idle = slots;
        pool
    }

    /// Sends one request to a worker and waits at most `deadline` for its answer (ruling
    /// R4 gives the deadlines: [`DEFAULT_TIMEOUT`] for a decode, [`rotate_deadline`](protocol::rotate_deadline) for a
    /// rotation). A file larger than the worker accepts ([`request_data_cap`]) is
    /// `FileTooLarge` and a rotation by other than 1 to 3 quarter turns is `Corrupt`, both
    /// without being sent: the worker would refuse the request unread and die, which would
    /// count against its slot.
    pub fn request(&self, req: PoolRequest, deadline: Duration) -> Result<Reply, PoolError> {
        let bytes = match &req {
            PoolRequest::DecodeHeif { data } | PoolRequest::RotateJpeg { data, .. } => {
                data.len() as u64
            }
        };
        if bytes > self.data_cap {
            return Err(DecodeError::FileTooLarge { bytes }.into());
        }
        let single_use = matches!(req, PoolRequest::RotateJpeg { .. });
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (request, expect) = to_request(req, id, self.max_pixels)?;
        if single_use {
            return self.single_use(request, expect, deadline);
        }
        loop {
            let mut guard = self.acquire()?;
            if self.ensure_worker(guard.slot()) {
                return self.exchange(guard.slot(), request, expect, deadline);
            }
            if self.is_closed() {
                return Err(DecodeError::Unsupported.into());
            }
            log("a worker slot gave up after repeated start failures");
            guard.give_up();
        }
    }

    /// Runs `request` in a sandboxed worker started for it alone (same sandbox as the
    /// pool's) and ended right after, so it never sees any other file (ruling R21). JPEG
    /// rotations go here: a pool worker taken over by a hostile HEIF it decoded must not
    /// be the one that answers a later rotation. The rules of [`exchange`](Self::exchange)
    /// apply; a worker that was dead before it took its request makes later single-use
    /// workers start without the best-effort mitigations, as a slot does.
    fn single_use(
        &self,
        request: Request,
        expect: Expect,
        deadline: Duration,
    ) -> Result<Reply, PoolError> {
        if self.is_closed() {
            return Err(DecodeError::Unsupported.into());
        }
        let mitigations = !self.single_use_degraded.load(Ordering::Relaxed);
        let args: Vec<&str> = self.every_args.iter().map(String::as_str).collect();
        let worker = match Worker::start(&self.exe, self.max_pixels, &args, mitigations) {
            Ok(worker) => worker,
            Err(e) => {
                log(&format!("cannot start {}: {e}", self.exe.display()));
                return Err(PoolError::WorkerUnavailable);
            }
        };
        let pid = worker.sandboxed.pid;
        {
            let mut state = self.lock();
            if state.closed {
                return Err(DecodeError::Unsupported.into()); // dropping `worker` ends it
            }
            state.jobs.retain(|(_, job)| job.strong_count() > 0);
            state
                .jobs
                .push((pid, Arc::downgrade(&worker.sandboxed.job)));
        }
        #[cfg(test)]
        self.single_use_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(pid);
        let mut slot = Slot {
            worker: Some(worker),
            started_once: true,
            degraded: !mitigations,
            ..Slot::default()
        };
        let result = self.exchange(&mut slot, request, expect, deadline);
        if slot.degraded && mitigations {
            self.single_use_degraded.store(true, Ordering::Relaxed);
        }
        if let Some(worker) = slot.worker.take() {
            terminate_job(&worker.sandboxed.job, pid);
            if !worker.sandboxed.wait_for_exit(WRITE_SETTLE) {
                log(&format!(
                    "worker {pid} has not exited after its one request"
                ));
            }
        }
        result
    }

    /// Ends every worker now, including ones busy with a request (whose callers then get an
    /// error at once), and makes later calls answer `Unsupported` without starting one.
    /// Called when the app exits, so a hung worker cannot hold up the prefetch threads.
    pub fn shutdown(&self) {
        let (jobs, idle) = {
            let mut state = self.lock();
            state.closed = true;
            let idle: Vec<Worker> = state
                .idle
                .iter_mut()
                .filter_map(|s| s.worker.take())
                .collect();
            (std::mem::take(&mut state.jobs), idle)
        };
        // Waiters for a slot must see `closed` rather than wait out their timeout.
        self.slot_freed.notify_all();
        drop(idle); // terminates their Jobs
        for (pid, job) in jobs {
            if let Some(job) = job.upgrade() {
                terminate_job(&job, pid);
            }
        }
    }

    fn is_closed(&self) -> bool {
        self.lock().closed
    }

    fn lock(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes an idle slot, waiting at most `timeout` for one.
    fn acquire(&self) -> Result<SlotGuard<'_>, PoolError> {
        let deadline = Instant::now() + self.timeout;
        let mut state = self.lock();
        loop {
            if state.live == 0 || state.closed {
                return Err(DecodeError::Unsupported.into());
            }
            if let Some(slot) = state.idle.pop() {
                return Ok(SlotGuard {
                    pool: self,
                    slot: Some(slot),
                });
            }
            let now = Instant::now();
            if now >= deadline {
                log("no worker slot became free in time");
                return Err(PoolError::WorkerUnavailable);
            }
            state = self
                .slot_freed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Makes sure `slot` has a worker; false once the slot has given up or the pool is shut
    /// down. A started worker
    /// that has exited since its last answer (killed from outside, say) is replaced
    /// without counting a failure: no request of ours was in it.
    fn ensure_worker(&self, slot: &mut Slot) -> bool {
        if let Some(worker) = slot
            .worker
            .take_if(|w| w.answered && w.sandboxed.has_exited())
        {
            log(&format!(
                "worker {} exited while idle; starting a new one",
                worker.sandboxed.pid
            ));
        }
        while slot.worker.is_none() {
            if slot.spawn_failures >= MAX_SPAWN_FAILURES || self.is_closed() {
                return false;
            }
            self.try_spawn(slot);
        }
        true
    }

    fn try_spawn(&self, slot: &mut Slot) {
        let first = if slot.started_once {
            &[][..]
        } else {
            &self.first_args[..]
        };
        let args: Vec<&str> = first
            .iter()
            .chain(&self.every_args)
            .map(String::as_str)
            .collect();
        match Worker::start(&self.exe, self.max_pixels, &args, !slot.degraded) {
            Ok(worker) => {
                let mut state = self.lock();
                if state.closed {
                    return; // dropping `worker` ends it
                }
                state.jobs.retain(|(_, job)| job.strong_count() > 0);
                let job = Arc::downgrade(&worker.sandboxed.job);
                state.jobs.push((worker.sandboxed.pid, job));
                drop(state);
                slot.worker = Some(worker);
                slot.started_once = true;
            }
            Err(e) => {
                let why = format!("cannot start {}: {e}", self.exe.display());
                start_failed(slot, &why, true);
            }
        }
    }

    /// Sends `request` to the slot's worker and waits up to `timeout` for its answer.
    /// Anything other than a matching answer of the `expect`ed kind ends the worker.
    fn exchange(
        &self,
        slot: &mut Slot,
        request: Request,
        expect: Expect,
        timeout: Duration,
    ) -> Result<Reply, PoolError> {
        let Some(worker) = slot.worker.as_mut() else {
            return Err(PoolError::WorkerUnavailable);
        };
        let id = request.id();
        let bytes = match &request {
            Request::DecodeHeif { data, .. } | Request::RotateJpeg { data, .. } => {
                data.len() as u64
            }
            Request::Shutdown => 0,
        };
        let dead_before_send = worker.sandboxed.has_exited();
        let deadline = Instant::now() + timeout;
        // Whether the writer put the whole request in the pipe, once it has said so.
        let mut wrote = None;
        let outcome = if worker.requests.send(request).is_err() {
            Err("the writer thread is gone".to_string())
        } else {
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match worker.events.recv_timeout(left) {
                    Ok(Event::Written { id: got, outcome }) if got == id => match outcome {
                        WriteOutcome::Rejected => {
                            // Our own encoding refused it before writing a byte: the worker
                            // is untouched, so no strike and no degrade.
                            log(&format!("request {id} ({bytes} bytes) cannot be encoded"));
                            return Err(DecodeError::FileTooLarge { bytes }.into());
                        }
                        outcome => wrote = Some(outcome == WriteOutcome::Sent),
                    },
                    // A late report for an earlier request.
                    Ok(Event::Written { .. }) => {}
                    Ok(Event::Response(Ok(Some(response)))) => {
                        break self.check(id, expect, response)
                    }
                    Ok(Event::Response(Ok(None))) => break Err("the worker exited".to_string()),
                    Ok(Event::Response(Err(e))) => break Err(format!("protocol error: {e}")),
                    Err(RecvTimeoutError::Timeout) => {
                        break Err(format!("no answer within {timeout:?}"))
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        break Err("the worker's threads are gone".to_string())
                    }
                }
            }
        };
        match outcome {
            Ok(result) => {
                worker.answered = true;
                slot.spawn_failures = 0;
                result
            }
            Err(why) if self.is_closed() => {
                // `shutdown` ended the worker: not its fault, nothing to count.
                log(&format!("request {id} ended by shutdown: {why}"));
                slot.worker = None;
                Err(PoolError::WorkerUnavailable)
            }
            Err(why) => {
                let (pid, answered) = (worker.sandboxed.pid, worker.answered);
                // End the worker before looking into its stdin pipe: while a live worker is
                // blocked reading stdin (one that answered wrongly and went on, say),
                // PeekNamedPipe on our copy of that read end waits for the read to finish.
                // If it will not end, its pipe cannot be inspected: assume it took the
                // request, as `took_request` does.
                terminate_job(&worker.sandboxed.job, pid);
                let exited = worker.sandboxed.wait_for_exit(WRITE_SETTLE);
                if !exited {
                    log(&format!(
                        "worker {pid} has not exited after TerminateJobObject"
                    ));
                }
                // Checked before the worker (and with it the stdin monitor) is dropped.
                let degrade =
                    !answered && (dead_before_send || (exited && !took_request(worker, id, wrote)));
                slot.worker = None; // terminates its Job
                if answered {
                    log(&format!("worker {pid} dropped: {why}"));
                } else {
                    let why = format!("worker {pid} failed its first request: {why}");
                    start_failed(slot, &why, degrade);
                }
                Err(PoolError::WorkerUnavailable)
            }
        }
    }

    #[cfg(test)]
    fn single_use_pids(&self) -> Vec<u32> {
        self.single_use_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[cfg(test)]
    fn workers(&self) -> Vec<WorkerInfo> {
        use std::os::windows::io::AsRawHandle;
        self.lock()
            .idle
            .iter()
            .filter_map(|slot| slot.worker.as_ref())
            .map(|w| WorkerInfo {
                pid: w.sandboxed.pid,
                job: windows::Win32::Foundation::HANDLE(w.sandboxed.job.as_raw_handle()),
            })
            .collect()
    }

    /// (consecutive start failures, degraded) of every idle slot.
    #[cfg(test)]
    fn slot_states(&self) -> Vec<(u32, bool)> {
        self.lock()
            .idle
            .iter()
            .map(|slot| (slot.spawn_failures, slot.degraded))
            .collect()
    }
}

/// Whether a worker that failed its first request (`id`) had read that request in full.
/// `wrote` is the writer's report if it already came. False when the bytes are still in its
/// stdin pipe, or the writer could not put them there (failed, or is still blocked on a full
/// pipe). If the pipe cannot be inspected, assume the worker read it: wrongly keeping the
/// mitigations only costs a start failure, wrongly dropping them weakens the sandbox.
fn took_request(worker: &Worker, id: u64, mut wrote: Option<bool>) -> bool {
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

/// Records a failed start. With `degrade`, the slot's next workers go without the
/// best-effort mitigations.
fn start_failed(slot: &mut Slot, why: &str, degrade: bool) {
    slot.spawn_failures += 1;
    log(&format!(
        "start failure {} of {MAX_SPAWN_FAILURES}: {why}",
        slot.spawn_failures
    ));
    if degrade && !slot.degraded {
        slot.degraded = true;
        log("this slot now starts workers without win32k lockdown and the child process policy");
    }
}

/// A slot taken out of the pool; it goes back on drop.
struct SlotGuard<'a> {
    pool: &'a WorkerPool,
    slot: Option<Slot>,
}

impl SlotGuard<'_> {
    fn slot(&mut self) -> &mut Slot {
        self.slot
            .as_mut()
            .expect("the slot stays until the guard is consumed")
    }

    /// Retires the slot for good.
    fn give_up(mut self) {
        self.slot = None;
        self.pool.lock().live -= 1;
        // Waiters must see `live == 0` rather than wait out their timeout.
        self.pool.slot_freed.notify_all();
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            self.pool.lock().idle.push(slot);
            self.pool.slot_freed.notify_one();
        }
    }
}

impl Worker {
    /// Spawns a sandboxed worker with its stderr, reader and writer threads. The threads end
    /// when the process does (its pipes close) or the `Worker` is dropped.
    fn start(
        exe: &Path,
        max_pixels: u64,
        args: &[&str],
        mitigations: bool,
    ) -> Result<Worker, String> {
        let (sandboxed, pipes) = sandbox::spawn(exe, max_pixels, args, mitigations)?;
        let pid = sandboxed.pid;
        let named = |what: &str| thread::Builder::new().name(format!("lv-worker {pid} {what}"));

        let stderr = pipes.stderr;
        named("stderr")
            .spawn(move || drain_stderr(pid, stderr))
            .map_err(|e| format!("stderr thread: {e}"))?;

        let (events_tx, events) = mpsc::channel();
        let cap = response_cap(max_pixels);
        let mut stdout = BufReader::with_capacity(64 * 1024, pipes.stdout);
        let response_tx = events_tx.clone();
        named("reader")
            .spawn(move || loop {
                let message = read_response(&mut stdout, cap);
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
struct WorkerInfo {
    pid: u32,
    job: windows::Win32::Foundation::HANDLE,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use lv_core::decode::DecodeLimits;

    use super::protocol::rotate_deadline;

    use std::time::Instant;

    use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    };
    use windows::Win32::System::JobObjects::{
        IsProcessInJob, JobObjectExtendedLimitInformation, QueryInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };
    use windows::Win32::System::SystemServices::SECURITY_MANDATORY_LOW_RID;
    use windows::Win32::System::Threading::{
        GetProcessMitigationPolicy, OpenProcess, OpenProcessToken, ProcessChildProcessPolicy,
        ProcessSystemCallDisablePolicy, TerminateProcess, WaitForSingleObject,
        PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    const MAX_PIXELS: u64 = 100_000_000;

    /// `lv-worker.exe` next to the test binary's profile folder (`target/<profile>`).
    pub(crate) fn worker_exe() -> PathBuf {
        let test_exe = std::env::current_exe().expect("current exe");
        // target/<profile>/deps/lv_app-<hash>.exe
        let profile_dir = test_exe
            .parent()
            .and_then(Path::parent)
            .expect("test exe lives in target/<profile>/deps");
        let exe = profile_dir.join("lv-worker.exe");
        assert!(
            exe.is_file(),
            "{} is missing: run `cargo build -p lv-worker` first",
            exe.display()
        );
        exe
    }

    fn sample() -> Vec<u8> {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/heif/rainbow-451x461.heic");
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn assert_decodes(pool: &WorkerPool) {
        let (width, height, bgra) = pool.decode_heif(sample()).expect("sample decodes");
        assert_eq!((width, height), (451, 461));
        assert_eq!(bgra.len(), 451 * 461 * 4);
    }

    fn assert_not_responding(result: Result<(u32, u32, Vec<u8>), DecodeError>) {
        match result {
            Err(DecodeError::Corrupt(msg)) => assert_eq!(msg, "작업자가 응답하지 않아요"),
            Err(e) => panic!("expected Corrupt, got {e}"),
            Ok((w, h, _)) => panic!("expected an error, got a {w}x{h} image"),
        }
    }

    /// An owned process handle for `pid`, closed on drop.
    struct Process(HANDLE);

    impl Process {
        fn open(pid: u32) -> Process {
            // SAFETY: plain call; the returned handle is owned by `Process` and closed on drop.
            let handle =
                unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) }
                    .unwrap_or_else(|e| panic!("OpenProcess({pid}): {e}"));
            Process(handle)
        }

        fn wait_for_exit(&self, timeout: Duration) -> bool {
            let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: `self.0` is a valid process handle with SYNCHRONIZE access.
            unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from OpenProcess and is closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    fn single_worker(pool: &WorkerPool) -> WorkerInfo {
        let mut workers = pool.workers();
        assert_eq!(workers.len(), 1, "expected one live worker");
        workers.remove(0)
    }

    #[test]
    fn pool_decodes_sample() {
        let pool = WorkerPool::start(worker_exe(), 2, MAX_PIXELS);
        assert_eq!(pool.workers().len(), 2, "both workers start right away");
        assert_decodes(&pool);
        // The worker survives and serves the next request too.
        assert_decodes(&pool);
    }

    #[test]
    fn worker_runs_at_low_integrity() {
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let process = Process::open(single_worker(&pool).pid);
        let mut token = HANDLE::default();
        // SAFETY: `process.0` is a valid process handle; `token` receives a new handle.
        unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) }.expect("OpenProcessToken");
        let token = Process(token); // closes the token handle on drop
        let mut buf = [0u64; 16];
        let mut len = 0u32;
        // SAFETY: `buf` is 128 writable, 8-aligned bytes, enough for a label and its SID.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenIntegrityLevel,
                Some(buf.as_mut_ptr().cast()),
                std::mem::size_of_val(&buf) as u32,
                &mut len,
            )
        }
        .expect("GetTokenInformation");
        // SAFETY: on success `buf` starts with a TOKEN_MANDATORY_LABEL whose SID points into
        // `buf`; the sub-authority pointers stay inside that SID.
        let rid = unsafe {
            let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
            let count = *GetSidSubAuthorityCount(label.Label.Sid);
            *GetSidSubAuthority(label.Label.Sid, u32::from(count) - 1)
        };
        assert_eq!(rid, SECURITY_MANDATORY_LOW_RID as u32);
        assert_decodes(&pool);
    }

    #[test]
    fn worker_has_creation_mitigations() {
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let process = Process::open(single_worker(&pool).pid);
        for (name, policy) in [
            ("win32k lockdown", ProcessSystemCallDisablePolicy),
            ("no child processes", ProcessChildProcessPolicy),
        ] {
            let mut flags = 0u32;
            // SAFETY: both policies are a single flags DWORD; bit 0 is DisallowWin32kSystemCalls
            // and NoChildProcessCreation respectively.
            unsafe {
                GetProcessMitigationPolicy(
                    process.0,
                    policy,
                    std::ptr::from_mut(&mut flags).cast(),
                    4,
                )
            }
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(flags & 1, 1, "{name} is off");
        }
        assert_decodes(&pool);
    }

    #[test]
    fn worker_is_in_job_with_kill_on_close() {
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let worker = single_worker(&pool);
        let process = Process::open(worker.pid);
        let mut in_job = BOOL(0);
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { IsProcessInJob(process.0, worker.job, &mut in_job) }.expect("IsProcessInJob");
        assert!(in_job.as_bool(), "worker is not in its job");

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: `info` is a writable JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the given size.
        unsafe {
            QueryInformationJobObject(
                worker.job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_mut(&mut info).cast(),
                std::mem::size_of_val(&info) as u32,
                None,
            )
        }
        .expect("QueryInformationJobObject");
        let flags = info.BasicLimitInformation.LimitFlags;
        assert!(flags.contains(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE));
        assert!(flags.contains(JOB_OBJECT_LIMIT_ACTIVE_PROCESS));
        assert!(flags.contains(JOB_OBJECT_LIMIT_PROCESS_MEMORY));
        assert_eq!(info.BasicLimitInformation.ActiveProcessLimit, 1);
        // worker_memory_limit(100M) = 12 * 100M, which Windows rounds down to whole pages.
        let limit = info.ProcessMemoryLimit;
        assert!(
            limit <= 1_200_000_000 && limit > 1_200_000_000 - 65_536,
            "limit {limit}"
        );
    }

    #[test]
    fn crash_then_next_request_succeeds() {
        let pool = WorkerPool::start_with(
            worker_exe(),
            1,
            MAX_PIXELS,
            Duration::from_secs(10),
            &["--test-mode=crash"],
            &[],
        );
        let crashed = Process::open(single_worker(&pool).pid);
        assert_not_responding(pool.decode_heif(sample()));
        assert!(crashed.wait_for_exit(Duration::from_secs(5)));
        assert_decodes(&pool);
    }

    #[test]
    fn hung_worker_times_out_and_is_replaced() {
        let pool = WorkerPool::start_with(
            worker_exe(),
            1,
            MAX_PIXELS,
            Duration::from_secs(1),
            &["--test-mode=hang"],
            &[],
        );
        let hung = Process::open(single_worker(&pool).pid);
        let start = Instant::now();
        assert_not_responding(pool.decode_heif(sample()));
        let waited = start.elapsed();
        assert!(
            waited >= Duration::from_secs(1) && waited < Duration::from_secs(5),
            "timed out after {waited:?}"
        );
        assert!(
            hung.wait_for_exit(Duration::from_secs(5)),
            "hung worker still runs"
        );
        // It hung after taking the request: counted, but the sandbox stays whole.
        assert_eq!(pool.slot_states(), vec![(1, false)]);
        assert_decodes(&pool);
    }

    #[test]
    fn oversized_request_is_rejected_before_sending() {
        // With max_pixels 1 the worker takes at most 64 MiB of file bytes.
        let pool = WorkerPool::start(worker_exe(), 1, 1);
        let pid = single_worker(&pool).pid;
        let data = vec![0u8; 64 << 20 | 1];
        match pool.decode_heif(data) {
            Err(DecodeError::FileTooLarge { bytes }) => assert_eq!(bytes, (64 << 20) + 1),
            Err(e) => panic!("expected FileTooLarge, got {e}"),
            Ok(_) => panic!("expected FileTooLarge, got an image"),
        }
        // Nothing reached the worker: same process, no start failure, not degraded.
        assert_eq!(single_worker(&pool).pid, pid);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
        assert!(matches!(
            pool.decode_heif(sample()),
            Err(DecodeError::TooLarge {
                width: 451,
                height: 461
            })
        ));
    }

    #[test]
    fn request_over_the_frame_limit_is_rejected_before_sending_at_the_default_limit() {
        // At the default pixel limit the file size rule allows more than one frame carries.
        let pool = WorkerPool::start(worker_exe(), 1, DecodeLimits::default().max_pixels);
        let pid = single_worker(&pool).pid;
        let over = lv_ipc::MAX_REQUEST_DATA + 1;
        // Zeroed pages are never touched: address space, not memory.
        match pool.decode_heif(vec![0u8; over as usize]) {
            Err(DecodeError::FileTooLarge { bytes }) => assert_eq!(bytes, over),
            Err(e) => panic!("expected FileTooLarge, got {e}"),
            Ok(_) => panic!("expected FileTooLarge, got an image"),
        }
        assert_eq!(single_worker(&pool).pid, pid, "the worker was touched");
        assert_eq!(pool.slot_states(), vec![(0, false)]);
        assert_decodes(&pool);
    }

    #[test]
    fn unencodable_request_is_an_app_error_without_strike_or_degrade() {
        let mut pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        // Skip the pool's own size check so the writer meets a request it cannot encode.
        pool.data_cap = u64::MAX;
        let pid = single_worker(&pool).pid;
        // One byte more than a DecodeHeif frame (16 fixed bytes) carries.
        let over = lv_ipc::MAX_PAYLOAD - 16 + 1;
        let start = Instant::now();
        match pool.decode_heif(vec![0u8; over as usize]) {
            Err(DecodeError::FileTooLarge { bytes }) => assert_eq!(bytes, over),
            Err(e) => panic!("expected FileTooLarge, got {e}"),
            Ok(_) => panic!("expected FileTooLarge, got an image"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "waited for the worker"
        );
        assert_eq!(single_worker(&pool).pid, pid, "the worker was replaced");
        assert_eq!(pool.slot_states(), vec![(0, false)]);
        // The same worker serves the next request with its sandbox whole.
        assert_decodes(&pool);
        assert_eq!(single_worker(&pool).pid, pid);
        assert!(win32k_lockdown(&pool));
    }

    #[test]
    fn shutdown_ends_a_hung_request_at_once() {
        let pool = std::sync::Arc::new(WorkerPool::start_with(
            worker_exe(),
            1,
            MAX_PIXELS,
            Duration::from_secs(10),
            &["--test-mode=hang"],
            &[],
        ));
        let hung = Process::open(single_worker(&pool).pid);
        let caller = {
            let pool = pool.clone();
            thread::spawn(move || pool.decode_heif(sample()))
        };
        // Let the request reach the worker, which never answers.
        thread::sleep(Duration::from_millis(500));
        let start = Instant::now();
        pool.shutdown();
        let result = caller.join().expect("caller thread");
        let took = start.elapsed();
        assert!(took < Duration::from_secs(3), "the caller waited {took:?}");
        assert!(result.is_err());
        assert!(
            hung.wait_for_exit(Duration::from_secs(5)),
            "worker still runs"
        );
        // Later calls answer at once without starting a worker.
        let start = Instant::now();
        assert!(matches!(
            pool.decode_heif(sample()),
            Err(DecodeError::Unsupported)
        ));
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(pool.workers().is_empty());
    }

    #[test]
    fn worker_killed_while_idle_is_replaced_without_failing() {
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        assert_decodes(&pool);
        let old = single_worker(&pool).pid;
        // Like `taskkill /f`: the worker dies between requests.
        // SAFETY: plain call; the handle is owned by `Process` and closed on drop.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, old) }
            .expect("OpenProcess");
        let killed = Process(handle);
        // SAFETY: valid process handle with PROCESS_TERMINATE access.
        unsafe { TerminateProcess(killed.0, 1) }.expect("TerminateProcess");
        assert!(killed.wait_for_exit(Duration::from_secs(5)));
        // The next request goes to a fresh worker and is not counted as a failure.
        assert_decodes(&pool);
        assert_ne!(single_worker(&pool).pid, old);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
    }

    #[test]
    fn dropping_pool_kills_workers() {
        let pool = WorkerPool::start(worker_exe(), 2, MAX_PIXELS);
        let processes: Vec<Process> = pool
            .workers()
            .iter()
            .map(|w| Process::open(w.pid))
            .collect();
        assert_eq!(processes.len(), 2);
        drop(pool);
        for process in &processes {
            assert!(
                process.wait_for_exit(Duration::from_secs(5)),
                "worker outlived the pool"
            );
        }
    }

    #[test]
    fn missing_exe_gives_up_with_unsupported() {
        let exe = worker_exe().with_file_name("no-such-worker.exe");
        let pool = WorkerPool::start(exe, 2, MAX_PIXELS);
        assert!(pool.workers().is_empty());
        assert!(matches!(
            pool.decode_heif(sample()),
            Err(DecodeError::Unsupported)
        ));
        // Every slot gave up: later calls answer at once without trying to spawn.
        let start = Instant::now();
        assert!(matches!(
            pool.decode_heif(sample()),
            Err(DecodeError::Unsupported)
        ));
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn worker_dying_at_start_gives_up_after_three_attempts() {
        let pool = WorkerPool::start_with(
            worker_exe(),
            1,
            MAX_PIXELS,
            Duration::from_secs(10),
            &[],
            &["--test-mode=die-at-start"],
        );
        // Spawning succeeds every time, but no worker ever answers.
        for attempt in 1..=3 {
            assert_not_responding(pool.decode_heif(sample()));
            assert_eq!(pool.slot_states(), vec![(attempt, true)]);
        }
        assert!(matches!(
            pool.decode_heif(sample()),
            Err(DecodeError::Unsupported)
        ));
        assert!(pool.slot_states().is_empty(), "the slot gave up");
    }

    #[test]
    fn death_before_request_degrades_and_first_answer_resets() {
        // Deterministic: the worker exits without reading stdin, and the pool holds a second
        // read end of that pipe, so the request always stays unread in the pipe buffer.
        let pool = WorkerPool::start_with(
            worker_exe(),
            1,
            MAX_PIXELS,
            Duration::from_secs(10),
            &["--test-mode=die-at-start"],
            &[],
        );
        assert_eq!(pool.slot_states(), vec![(0, false)]);
        assert_not_responding(pool.decode_heif(sample()));
        assert_eq!(pool.slot_states(), vec![(1, true)]);
        assert_decodes(&pool);
        assert_eq!(pool.slot_states(), vec![(0, true)]);
        assert!(
            !win32k_lockdown(&pool),
            "degraded slot still sets win32k lockdown"
        );
    }

    #[test]
    fn crash_during_first_decode_does_not_degrade() {
        // Deterministic: crash mode aborts only after `read_request` has read the whole
        // frame, so the stdin pipe is empty and the writer's flush has returned by then.
        let pool = WorkerPool::start_with(
            worker_exe(),
            1,
            MAX_PIXELS,
            Duration::from_secs(10),
            &["--test-mode=crash"],
            &[],
        );
        assert_not_responding(pool.decode_heif(sample()));
        assert_eq!(
            pool.slot_states(),
            vec![(1, false)],
            "counted, not degraded"
        );
        assert_decodes(&pool);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
        assert!(
            win32k_lockdown(&pool),
            "a crash while decoding weakened the sandbox"
        );
    }

    /// Whether the pool's only worker runs with win32k system calls disabled.
    fn win32k_lockdown(pool: &WorkerPool) -> bool {
        let process = Process::open(single_worker(pool).pid);
        let mut flags = 0u32;
        // SAFETY: ProcessSystemCallDisablePolicy is a single flags DWORD.
        unsafe {
            GetProcessMitigationPolicy(
                process.0,
                ProcessSystemCallDisablePolicy,
                std::ptr::from_mut(&mut flags).cast(),
                4,
            )
        }
        .expect("GetProcessMitigationPolicy");
        flags & 1 == 1
    }

    fn jpeg_sample(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/jpeg")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn rotate_request(quarters: u8, name: &str) -> PoolRequest {
        PoolRequest::RotateJpeg {
            quarters,
            data: jpeg_sample(name),
        }
    }

    fn assert_rotated(result: Result<Reply, PoolError>, dims: (u32, u32)) -> Vec<u8> {
        match result {
            Ok(Reply::Bytes { data }) => {
                let info = lv_core::jpeg_check::inspect(&data).expect("a valid JPEG");
                assert_eq!((u32::from(info.width), u32::from(info.height)), dims);
                data
            }
            Ok(Reply::Image { width, height, .. }) => {
                panic!("expected Bytes, got a {width}x{height} image")
            }
            Err(e) => panic!("expected Bytes, got {e}"),
        }
    }

    fn assert_pool_not_responding(result: Result<Reply, PoolError>) {
        match result {
            Err(PoolError::WorkerUnavailable) => {}
            Err(e) => panic!("expected WorkerUnavailable, got {e}"),
            Ok(_) => panic!("expected an error, got a reply"),
        }
    }

    #[test]
    fn rotate_request_roundtrip() {
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let pid = single_worker(&pool).pid;
        let deadline = rotate_deadline(64 * 32);
        let mut turned = jpeg_sample("gradient-64x32.jpg");
        for turn in 1..=4 {
            let request = PoolRequest::RotateJpeg {
                quarters: 1,
                data: turned,
            };
            let dims = if turn % 2 == 1 { (32, 64) } else { (64, 32) };
            turned = assert_rotated(pool.request(request, deadline), dims);
        }
        assert_rotated(
            pool.request(rotate_request(2, "gradient-64x32.jpg"), deadline),
            (64, 32),
        );
        assert_rotated(
            pool.rotate_jpeg(3, jpeg_sample("gradient-64x32.jpg")),
            (32, 64),
        );
        // Decoding goes through the pool's worker, which no rotation used.
        assert_decodes(&pool);
        assert_eq!(single_worker(&pool).pid, pid);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
        let rotations = pool.single_use_pids();
        assert_eq!(rotations.len(), 6);
        assert!(!rotations.contains(&pid));
    }

    /// Whether a process with this pid is still running.
    fn alive(pid: u32) -> bool {
        // SAFETY: plain call; the handle is owned by `Process` and closed on drop.
        match unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) } {
            Ok(handle) => !Process(handle).wait_for_exit(Duration::ZERO),
            Err(_) => false,
        }
    }

    #[test]
    fn each_rotation_runs_in_a_fresh_worker_that_ends_after_it() {
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let pool_pid = single_worker(&pool).pid;
        assert_decodes(&pool);
        for _ in 0..2 {
            assert_rotated(
                pool.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")),
                (32, 64),
            );
        }
        let rotations = pool.single_use_pids();
        assert_eq!(rotations.len(), 2);
        assert_ne!(rotations[0], rotations[1], "two rotations, two workers");
        for pid in &rotations {
            assert_ne!(*pid, pool_pid, "a pool worker rotated");
            assert!(!alive(*pid), "worker {pid} outlived its rotation");
        }
        // The pool's worker is still the one that decoded, and still serves decodes.
        assert_eq!(single_worker(&pool).pid, pool_pid);
        assert_decodes(&pool);
        assert_eq!(single_worker(&pool).pid, pool_pid);
    }

    #[test]
    fn single_use_worker_dead_before_its_request_degrades_later_ones() {
        let pool = WorkerPool::start_with(
            worker_exe(),
            0,
            MAX_PIXELS,
            Duration::from_secs(10),
            &[],
            &["--test-mode=die-at-start"],
        );
        assert_pool_not_responding(pool.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")));
        assert!(pool.single_use_degraded.load(Ordering::Relaxed));
    }

    #[test]
    fn rotate_failures_are_answers_not_worker_faults() {
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let pid = single_worker(&pool).pid;
        assert!(matches!(
            pool.rotate_jpeg(1, jpeg_sample("gradient-65x33.jpg")),
            Err(PoolError::NotPerfect)
        ));
        let mut broken = jpeg_sample("gradient-64x32.jpg");
        let len = broken.len();
        broken[len - 40..len - 2].fill(0xFF);
        assert!(matches!(
            pool.rotate_jpeg(1, broken),
            Err(PoolError::Failed(DecodeError::Corrupt(_)))
        ));
        assert_eq!(single_worker(&pool).pid, pid);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
    }

    #[test]
    fn rotate_is_checked_before_sending() {
        // 64x32 is 2048 pixels.
        let pool = WorkerPool::start(worker_exe(), 1, 2047);
        let pid = single_worker(&pool).pid;
        assert!(matches!(
            pool.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")),
            Err(PoolError::Failed(DecodeError::TooLarge {
                width: 64,
                height: 32
            }))
        ));
        assert!(matches!(
            pool.rotate_jpeg(1, b"not a jpeg".to_vec()),
            Err(PoolError::Failed(DecodeError::Corrupt(_)))
        ));
        for quarters in [0, 4] {
            assert!(matches!(
                pool.request(
                    rotate_request(quarters, "gradient-64x32.jpg"),
                    rotate_deadline(1)
                ),
                Err(PoolError::Failed(DecodeError::Corrupt(_)))
            ));
        }
        // Nothing reached the worker.
        assert_eq!(single_worker(&pool).pid, pid);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
    }

    /// `name` with an MPF APP2 segment right after its APP0.
    fn with_mpf(name: &str) -> Vec<u8> {
        let jpeg = jpeg_sample(name);
        let app0_end = 4 + usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]));
        let mpf = b"MPF\0MM\0\x2a\0\0\0\x08";
        let mut out = jpeg[..app0_end].to_vec();
        out.extend_from_slice(&[0xFF, 0xE2, 0, (mpf.len() + 2) as u8]);
        out.extend_from_slice(mpf);
        out.extend_from_slice(&jpeg[app0_end..]);
        out
    }

    #[test]
    fn rotate_refuses_trailing_data_before_sending() {
        // Ruling R7: TurboJPEG would drop everything after EOI.
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let pid = single_worker(&pool).pid;
        let mut trailer = jpeg_sample("gradient-64x32.jpg");
        trailer.extend_from_slice(&jpeg_sample("gradient-64x32.jpg"));
        let mut truncated = jpeg_sample("gradient-64x32.jpg");
        truncated.truncate(truncated.len() - 2);
        for data in [trailer, with_mpf("gradient-64x32.jpg"), truncated] {
            assert!(matches!(
                pool.rotate_jpeg(1, data),
                Err(PoolError::HasTrailingData)
            ));
        }
        assert_eq!(single_worker(&pool).pid, pid);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
        // A plain JPEG still rotates.
        assert_rotated(
            pool.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")),
            (32, 64),
        );
    }

    #[test]
    fn worker_refusal_of_trailing_data_is_an_answer() {
        // Sent past the app's own check: the worker refuses too, as an answer.
        let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
        let pid = single_worker(&pool).pid;
        let mut data = jpeg_sample("gradient-64x32.jpg");
        data.push(0);
        let request = PoolRequest::RotateJpeg { quarters: 1, data };
        assert!(matches!(
            pool.request(request, rotate_deadline(64 * 32)),
            Err(PoolError::HasTrailingData)
        ));
        assert_eq!(single_worker(&pool).pid, pid);
        assert_eq!(pool.slot_states(), vec![(0, false)]);
    }

    #[test]
    fn wrong_reply_kind_is_protocol_failure() {
        // A decode in the pool's worker.
        let pool = WorkerPool::start_with(
            worker_exe(),
            1,
            MAX_PIXELS,
            Duration::from_secs(10),
            &["--test-mode=wrong-kind"],
            &[],
        );
        let wrong = Process::open(single_worker(&pool).pid);
        let decode = PoolRequest::DecodeHeif { data: sample() };
        assert_pool_not_responding(pool.request(decode, Duration::from_secs(10)));
        assert!(wrong.wait_for_exit(Duration::from_secs(5)), "not killed");
        // It read the request and answered: a start failure, not a degrade.
        assert_eq!(pool.slot_states(), vec![(1, false)]);
        assert_decodes(&pool);
        assert_eq!(pool.slot_states(), vec![(0, false)]);

        // A rotation in its own worker.
        let pool = WorkerPool::start_with(
            worker_exe(),
            0,
            MAX_PIXELS,
            Duration::from_secs(10),
            &[],
            &["--test-mode=wrong-kind"],
        );
        let rotate = rotate_request(1, "gradient-64x32.jpg");
        assert_pool_not_responding(pool.request(rotate, Duration::from_secs(10)));
        let [pid] = pool.single_use_pids()[..] else {
            panic!("one rotation worker expected");
        };
        assert!(!alive(pid), "not killed");
        assert!(!pool.single_use_degraded.load(Ordering::Relaxed));
    }

    #[test]
    fn rotate_crash_after_delivery_does_not_degrade() {
        let pool = WorkerPool::start_with(
            worker_exe(),
            0,
            MAX_PIXELS,
            Duration::from_secs(10),
            &[],
            &["--test-mode=crash"],
        );
        assert_pool_not_responding(pool.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")));
        let [pid] = pool.single_use_pids()[..] else {
            panic!("one rotation worker expected");
        };
        assert!(!alive(pid));
        assert!(
            !pool.single_use_degraded.load(Ordering::Relaxed),
            "a crash while rotating weakened the sandbox"
        );
    }

    #[test]
    fn rotate_deadline_grows_with_megapixels() {
        assert_eq!(rotate_deadline(0), Duration::from_secs(10));
        // 10 s per 100 MP is 100 ns per pixel.
        assert_eq!(
            rotate_deadline(64 * 32),
            Duration::from_nanos(10_000_204_800)
        );
        assert_eq!(rotate_deadline(100_000_000), Duration::from_secs(20));
        assert_eq!(rotate_deadline(250_000_000), Duration::from_secs(35));
        assert_eq!(rotate_deadline(u64::MAX), rotate_deadline(u64::MAX - 1));
    }

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
