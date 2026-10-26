//! Pool of sandboxed `lv-worker.exe` processes that decode HEIF files for the viewer.
//!
//! Each slot holds at most one worker (see [`sandbox`]). A call takes an idle slot, starts a
//! worker in it if needed, sends one request and waits for the answer on a channel fed by
//! the worker's reader thread (ruling R4): if no matching answer arrives within the timeout
//! (10 s), or the worker dies or breaks the protocol, its Job is terminated, the slot is
//! emptied and the call fails with `Corrupt("작업자가 응답하지 않아요")`; the next call starts
//! a fresh worker.
//!
//! A worker counts as started only once it has given a valid answer: failing to spawn, or
//! dying, hanging or breaking the protocol on its first request, is a start failure, and
//! after three in a row the slot gives up. Once every slot has, `decode_heif` answers
//! `Unsupported` at once.
//!
//! Only a worker that was already dead or stuck *before it took its request* (it had exited
//! before we sent, or left the request unread in its stdin pipe) makes the slot drop the
//! best-effort creation mitigations for later workers, in case they are what kills it. A
//! worker that dies after reading its first request may have been killed by the file, and a
//! hostile file must not be able to weaken the sandbox for the next one (ruling R11a).

mod sandbox;

use std::fs::File;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use lv_core::decode::DecodeError;
use lv_ipc::{read_response, write_request, FailKind, IpcError, Request, Response};

use sandbox::{log, Sandboxed, StdinMonitor};

/// How long a worker may take to answer one request, and a caller may wait for a free slot.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Consecutive failed starts after which a slot stops trying.
const MAX_SPAWN_FAILURES: u32 = 3;
const NOT_RESPONDING: &str = "작업자가 응답하지 않아요";
/// Longest worker stderr line copied into the log, in bytes.
const MAX_LOG_LINE: usize = 4096;
/// How long a failed first request waits for the writer thread to report its write.
const WRITE_SETTLE: Duration = Duration::from_secs(1);

type Decoded = (u32, u32, Vec<u8>);

pub struct WorkerPool {
    exe: PathBuf,
    max_pixels: u64,
    timeout: Duration,
    /// Extra arguments for each slot's first worker only (test fault injection).
    first_args: Vec<String>,
    /// Extra arguments for every worker (test fault injection).
    every_args: Vec<String>,
    state: Mutex<PoolState>,
    slot_freed: Condvar,
    next_id: AtomicU64,
}

struct PoolState {
    idle: Vec<Slot>,
    /// Slots that have not given up, idle or in use.
    live: usize,
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
    /// One entry per request from the writer thread: whether the write and flush succeeded.
    written: Receiver<bool>,
    responses: Receiver<Result<Option<Response>, IpcError>>,
    stdin_monitor: StdinMonitor,
}

impl WorkerPool {
    /// Starts `size` workers right away. Never panics: failures are logged and recorded in
    /// the slots, and later calls retry.
    pub fn start(exe: PathBuf, size: usize, max_pixels: u64) -> WorkerPool {
        Self::start_with(exe, size, max_pixels, DEFAULT_TIMEOUT, &[], &[])
    }

    fn start_with(
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
            timeout,
            first_args: first_args.iter().map(|a| a.to_string()).collect(),
            every_args: every_args.iter().map(|a| a.to_string()).collect(),
            state: Mutex::new(PoolState {
                idle: Vec::with_capacity(size),
                live: size,
            }),
            slot_freed: Condvar::new(),
            next_id: AtomicU64::new(1),
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

    /// Decodes a HEIF file in a worker to premultiplied sRGB BGRA8 (width, height, pixels).
    pub fn decode_heif(&self, data: Vec<u8>) -> Result<Decoded, DecodeError> {
        loop {
            let mut guard = self.acquire()?;
            if self.ensure_worker(guard.slot()) {
                return self.request(guard.slot(), data);
            }
            log("a worker slot gave up after repeated start failures");
            guard.give_up();
        }
    }

    fn lock(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes an idle slot, waiting at most `timeout` for one.
    fn acquire(&self) -> Result<SlotGuard<'_>, DecodeError> {
        let deadline = Instant::now() + self.timeout;
        let mut state = self.lock();
        loop {
            if state.live == 0 {
                return Err(DecodeError::Unsupported);
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
                return Err(DecodeError::Corrupt(NOT_RESPONDING.into()));
            }
            state = self
                .slot_freed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Makes sure `slot` has a worker; false once the slot has given up.
    fn ensure_worker(&self, slot: &mut Slot) -> bool {
        while slot.worker.is_none() {
            if slot.spawn_failures >= MAX_SPAWN_FAILURES {
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
                slot.worker = Some(worker);
                slot.started_once = true;
            }
            Err(e) => {
                let why = format!("cannot start {}: {e}", self.exe.display());
                start_failed(slot, &why, true);
            }
        }
    }

    /// Sends one request to the slot's worker and waits for its answer. Anything other than
    /// a matching answer ends the worker.
    fn request(&self, slot: &mut Slot, data: Vec<u8>) -> Result<Decoded, DecodeError> {
        let Some(worker) = slot.worker.as_mut() else {
            return Err(DecodeError::Corrupt(NOT_RESPONDING.into()));
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request::DecodeHeif {
            id,
            max_pixels: self.max_pixels,
            data,
        };
        let dead_before_send = worker.sandboxed.has_exited();
        let outcome = if worker.requests.send(request).is_err() {
            Err("the writer thread is gone".to_string())
        } else {
            match worker.responses.recv_timeout(self.timeout) {
                Ok(Ok(Some(response))) => self.check(id, response),
                Ok(Ok(None)) => Err("the worker exited".to_string()),
                Ok(Err(e)) => Err(format!("protocol error: {e}")),
                Err(RecvTimeoutError::Timeout) => {
                    Err(format!("no answer within {:?}", self.timeout))
                }
                Err(RecvTimeoutError::Disconnected) => Err("the reader thread is gone".to_string()),
            }
        };
        match outcome {
            Ok(result) => {
                worker.answered = true;
                slot.spawn_failures = 0;
                // Write reports only matter for the first request; don't let them pile up.
                while worker.written.try_recv().is_ok() {}
                result
            }
            Err(why) => {
                let (pid, answered) = (worker.sandboxed.pid, worker.answered);
                // Checked before the worker (and with it the stdin monitor) is dropped.
                let degrade = !answered && (dead_before_send || !took_request(worker));
                slot.worker = None; // terminates its Job
                if answered {
                    log(&format!("worker {pid} dropped: {why}"));
                } else {
                    let why = format!("worker {pid} failed its first request: {why}");
                    start_failed(slot, &why, degrade);
                }
                Err(DecodeError::Corrupt(NOT_RESPONDING.into()))
            }
        }
    }

    /// Maps the answer to request `id`; `Err` means the worker broke the protocol.
    fn check(&self, id: u64, response: Response) -> Result<Result<Decoded, DecodeError>, String> {
        match response {
            Response::Image {
                id: got,
                width,
                height,
                bgra,
            } if got == id => {
                if u64::from(width) * u64::from(height) > self.max_pixels {
                    Err(format!("{width}x{height} image is over the pixel limit"))
                } else {
                    Ok(Ok((width, height, bgra)))
                }
            }
            Response::Failed { id: got, kind } if got == id => Ok(Err(match kind {
                FailKind::TooLarge { width, height } => DecodeError::TooLarge { width, height },
                FailKind::Corrupt(msg) => DecodeError::Corrupt(msg),
                FailKind::Unsupported => DecodeError::Unsupported,
            })),
            Response::Image { id: got, .. } | Response::Failed { id: got, .. } => {
                Err(format!("answer for request {got}, expected {id}"))
            }
        }
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

/// Whether a worker that failed its first request had read that request in full. False when
/// the bytes are still in its stdin pipe, or the writer could not put them there (failed, or
/// is still blocked on a full pipe). If the pipe cannot be inspected, assume the worker read
/// it: wrongly keeping the mitigations only costs a start failure, wrongly dropping them
/// weakens the sandbox.
fn took_request(worker: &Worker) -> bool {
    match worker.written.recv_timeout(WRITE_SETTLE) {
        Ok(true) => {}
        Ok(false) | Err(_) => return false,
    }
    !matches!(worker.stdin_monitor.unread(), Ok(n) if n > 0)
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

/// Largest response the app accepts: an `N`-pixel BGRA image plus room for the fixed fields
/// (ruling R10).
fn response_cap(max_pixels: u64) -> u64 {
    max_pixels.saturating_mul(4).saturating_add(64 * 1024)
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

        let (response_tx, responses) = mpsc::channel();
        let cap = response_cap(max_pixels);
        let mut stdout = BufReader::with_capacity(64 * 1024, pipes.stdout);
        named("reader")
            .spawn(move || loop {
                let message = read_response(&mut stdout, cap);
                let last = !matches!(message, Ok(Some(_)));
                if response_tx.send(message).is_err() || last {
                    break;
                }
            })
            .map_err(|e| format!("reader thread: {e}"))?;

        let (requests, request_rx) = mpsc::channel::<Request>();
        let (written_tx, written) = mpsc::channel();
        let mut stdin = BufWriter::new(pipes.stdin);
        named("writer")
            .spawn(move || {
                for request in request_rx {
                    let sent = write_request(&mut stdin, &request).and_then(|()| stdin.flush());
                    let _ = written_tx.send(sent.is_ok());
                    if sent.is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("writer thread: {e}"))?;

        Ok(Worker {
            sandboxed,
            answered: false,
            requests,
            written,
            responses,
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
mod tests {
    use super::*;

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
        ProcessSystemCallDisablePolicy, WaitForSingleObject, PROCESS_QUERY_INFORMATION,
        PROCESS_SYNCHRONIZE,
    };

    const MAX_PIXELS: u64 = 100_000_000;

    /// `lv-worker.exe` next to the test binary's profile folder (`target/<profile>`).
    fn worker_exe() -> PathBuf {
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
        // max(100M * 4 * 3, 256 MiB), which Windows rounds down to whole pages.
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
        assert_decodes(&pool);
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
