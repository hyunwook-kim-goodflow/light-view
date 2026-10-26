//! Background prefetching of decoded images with cancellation and a memory budget.

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::decode::{decode_file, DecodeError, DecodeLimits, DecodedImage};

pub type DecodeResult = Arc<Result<DecodedImage, DecodeError>>;

pub trait Decode: Send + Sync + 'static {
    fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError>;
}

/// Decodes real files with `decode_file`.
pub struct FileDecoder {
    pub limits: DecodeLimits,
}

impl Decode for FileDecoder {
    fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError> {
        decode_file(path, &self.limits)
    }
}

struct Entry {
    result: DecodeResult,
    bytes: u64,
    last_used: u64,
}

#[derive(Default)]
struct State {
    cache: HashMap<PathBuf, Entry>,
    wanted: Vec<PathBuf>,
    queue: VecDeque<PathBuf>,
    in_flight: HashSet<PathBuf>,
    tick: u64,
    shutdown: bool,
}

impl State {
    fn touch(&mut self, path: &Path) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(entry) = self.cache.get_mut(path) {
            entry.last_used = tick;
        }
    }

    /// Returns false if the entry was evicted again in the same pass.
    fn insert(&mut self, path: PathBuf, result: DecodeResult, budget: u64) -> bool {
        let bytes = match result.as_ref() {
            Ok(img) => img.memory_bytes(),
            Err(_) => 0,
        };
        self.tick += 1;
        let last_used = self.tick;
        self.cache.insert(
            path.clone(),
            Entry {
                result,
                bytes,
                last_used,
            },
        );
        self.evict(budget);
        self.cache.contains_key(&path)
    }

    /// Evict until within budget: entries not wanted first (oldest-used first; normally
    /// none, as `request` drops them), then wanted entries farthest from the current
    /// one first. `wanted[0]` is never evicted.
    fn evict(&mut self, budget: u64) {
        let mut total: u64 = self.cache.values().map(|e| e.bytes).sum();
        if total <= budget {
            return;
        }
        let current = self.wanted.first();
        let rank: HashMap<&PathBuf, usize> = self
            .wanted
            .iter()
            .enumerate()
            .rev() // first occurrence wins
            .map(|(i, p)| (p, i))
            .collect();
        let mut victims: Vec<(bool, u64, PathBuf)> = self
            .cache
            .iter()
            .filter(|(path, _)| Some(*path) != current)
            .map(|(path, e)| match rank.get(path) {
                Some(&i) => (true, (usize::MAX - i) as u64, path.clone()),
                None => (false, e.last_used, path.clone()),
            })
            .collect();
        // Unwanted (false) first, oldest first; then wanted, highest index first.
        victims.sort();
        for (_, _, path) in victims {
            if total <= budget {
                break;
            }
            if let Some(entry) = self.cache.remove(&path) {
                total -= entry.bytes;
            }
        }
    }
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    budget: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Prefetcher {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl Prefetcher {
    pub fn new(
        decoder: impl Decode,
        workers: usize,
        budget_bytes: u64,
        on_ready: impl Fn(PathBuf) + Send + Sync + 'static,
    ) -> Prefetcher {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            budget: budget_bytes,
        });
        let decoder = Arc::new(decoder);
        let on_ready = Arc::new(on_ready);
        let workers = (0..workers.max(1))
            .map(|_| {
                let shared = shared.clone();
                let decoder = decoder.clone();
                let on_ready = on_ready.clone();
                std::thread::spawn(move || worker_loop(&shared, &*decoder, &*on_ready))
            })
            .collect();
        Prefetcher { shared, workers }
    }

    /// `wanted[0]` is the current image and highest priority; the rest follow in order.
    /// Cached entries and pending work not in `wanted` are dropped at once, so memory
    /// holds only the wanted set; running work finishes but its result is only cached
    /// if still wanted. The budget stays an upper cap on the wanted set.
    pub fn request(&self, wanted: &[PathBuf]) {
        let mut st = self.shared.lock();
        st.wanted = wanted.to_vec();
        st.cache.retain(|path, _| wanted.contains(path));
        let mut queue = VecDeque::new();
        for path in wanted {
            if st.cache.contains_key(path) {
                st.touch(path);
            } else if !st.in_flight.contains(path) && !queue.contains(path) {
                queue.push_back(path.clone());
            }
        }
        st.queue = queue;
        drop(st);
        self.shared.cond.notify_all();
    }

    pub fn get(&self, path: &Path) -> Option<DecodeResult> {
        let mut st = self.shared.lock();
        st.touch(path);
        st.cache.get(path).map(|e| e.result.clone())
    }
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        {
            let mut st = self.shared.lock();
            st.shutdown = true;
            st.queue.clear();
        }
        self.shared.cond.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker_loop(shared: &Shared, decoder: &dyn Decode, on_ready: &dyn Fn(PathBuf)) {
    loop {
        let path = {
            let mut st = shared.lock();
            loop {
                if st.shutdown {
                    return;
                }
                if let Some(path) = st.queue.pop_front() {
                    st.in_flight.insert(path.clone());
                    break path;
                }
                st = shared.cond.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        };

        // A panicking decoder must not kill the worker or leave `path` in flight forever.
        let result: DecodeResult = Arc::new(
            catch_unwind(AssertUnwindSafe(|| decoder.decode(&path)))
                .unwrap_or_else(|_| Err(DecodeError::Corrupt("decoder panicked".into()))),
        );

        let inserted = {
            let mut st = shared.lock();
            st.in_flight.remove(&path);
            if st.wanted.contains(&path) && !st.cache.contains_key(&path) {
                st.insert(path.clone(), result, shared.budget)
            } else {
                false
            }
        };
        if inserted {
            on_ready(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::ImageFormat;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);

    fn p(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    fn one_px() -> DecodedImage {
        DecodedImage {
            width: 1,
            height: 1,
            frames: vec![crate::decode::Frame {
                bgra: vec![0; 4],
                delay_ms: 0,
            }],
            format: ImageFormat::Png,
            byte_len: 0,
        }
    }

    /// Fake decoder: records calls, reports starts, optionally blocks per path
    /// until released, and fails for chosen paths.
    struct SlowDecoder {
        calls: Arc<Mutex<Vec<PathBuf>>>,
        started: Mutex<Sender<PathBuf>>,
        gates: Mutex<HashMap<PathBuf, Receiver<()>>>,
        failing: HashSet<PathBuf>,
    }

    struct Harness {
        calls: Arc<Mutex<Vec<PathBuf>>>,
        started: Receiver<PathBuf>,
        ready: Receiver<PathBuf>,
        ready_count: Arc<AtomicUsize>,
        releases: HashMap<PathBuf, Sender<()>>,
        prefetcher: Prefetcher,
    }

    fn harness(workers: usize, budget: u64, gated: &[&str], failing: &[&str]) -> Harness {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started) = channel();
        let mut gates = HashMap::new();
        let mut releases = HashMap::new();
        for g in gated {
            let (tx, rx) = channel();
            gates.insert(p(g), rx);
            releases.insert(p(g), tx);
        }
        let decoder = SlowDecoder {
            calls: calls.clone(),
            started: Mutex::new(started_tx),
            gates: Mutex::new(gates),
            failing: failing.iter().map(|f| p(f)).collect(),
        };
        let (ready_tx, ready) = channel();
        let ready_tx = Mutex::new(ready_tx);
        let ready_count = Arc::new(AtomicUsize::new(0));
        let rc = ready_count.clone();
        let prefetcher = Prefetcher::new(decoder, workers, budget, move |path| {
            rc.fetch_add(1, Ordering::SeqCst);
            let _ = ready_tx.lock().unwrap().send(path);
        });
        Harness {
            calls,
            started,
            ready,
            ready_count,
            releases,
            prefetcher,
        }
    }

    impl Decode for SlowDecoder {
        fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError> {
            self.calls.lock().unwrap().push(path.to_path_buf());
            let _ = self.started.lock().unwrap().send(path.to_path_buf());
            let gate = self.gates.lock().unwrap().remove(path);
            if let Some(rx) = gate {
                rx.recv_timeout(WAIT).expect("gate was never released");
            }
            if self.failing.contains(path) {
                Err(DecodeError::Unsupported)
            } else {
                Ok(one_px())
            }
        }
    }

    impl Harness {
        fn wait_ready(&self, n: usize) -> Vec<PathBuf> {
            (0..n)
                .map(|_| {
                    self.ready
                        .recv_timeout(WAIT)
                        .expect("timed out waiting for on_ready")
                })
                .collect()
        }
        fn wait_started(&self) -> PathBuf {
            self.started
                .recv_timeout(WAIT)
                .expect("timed out waiting for decode start")
        }
    }

    #[test]
    fn current_is_decoded_first() {
        let h = harness(1, u64::MAX, &[], &[]);
        h.prefetcher.request(&[p("a"), p("b"), p("c")]);
        assert_eq!(h.wait_started(), p("a"));
        h.wait_ready(3);
        assert_eq!(*h.calls.lock().unwrap(), vec![p("a"), p("b"), p("c")]);
    }

    #[test]
    fn obsolete_requests_are_dropped() {
        let h = harness(1, u64::MAX, &["a"], &[]);
        h.prefetcher.request(&[p("a")]);
        assert_eq!(h.wait_started(), p("a"));
        for name in ["b", "c", "d", "e", "y"] {
            h.prefetcher.request(&[p(name)]);
        }
        h.prefetcher.request(&[p("z")]);
        h.releases[&p("a")].send(()).unwrap();
        assert_eq!(h.wait_ready(1), vec![p("z")]);
        assert_eq!(*h.calls.lock().unwrap(), vec![p("a"), p("z")]);
        // a finished after it stopped being wanted: discarded, never reported.
        assert!(h.prefetcher.get(&p("a")).is_none());
        assert_eq!(h.ready_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn errors_are_cached() {
        let h = harness(1, u64::MAX, &[], &["a"]);
        h.prefetcher.request(&[p("a")]);
        h.wait_ready(1);
        h.prefetcher.request(&[p("a")]);
        h.prefetcher.request(&[p("a"), p("b")]);
        assert_eq!(h.wait_ready(1), vec![p("b")]);
        assert_eq!(*h.calls.lock().unwrap(), vec![p("a"), p("b")]);
        let got = h.prefetcher.get(&p("a")).expect("error result cached");
        assert!(got.is_err());
    }

    #[test]
    fn rerequest_keeps_still_wanted_entries() {
        // Each 1x1 image is 4 bytes; budget holds two. b is dropped by the request.
        let h = harness(1, 8, &[], &[]);
        h.prefetcher.request(&[p("a"), p("b")]);
        h.wait_ready(2);
        h.prefetcher.request(&[p("c"), p("a")]);
        h.wait_ready(1);
        assert!(h.prefetcher.get(&p("b")).is_none());
        assert!(h.prefetcher.get(&p("a")).is_some());
        assert!(h.prefetcher.get(&p("c")).is_some());
    }

    #[test]
    fn request_drops_unwanted_entries_immediately() {
        let h = harness(1, u64::MAX, &[], &["b"]);
        h.prefetcher.request(&[p("a"), p("b"), p("c")]);
        h.wait_ready(3);
        h.prefetcher.request(&[p("c")]);
        // Far below the budget, yet only the wanted set stays (errors included).
        assert!(h.prefetcher.get(&p("a")).is_none());
        assert!(h.prefetcher.get(&p("b")).is_none());
        assert!(h.prefetcher.get(&p("c")).is_some());
    }

    #[test]
    fn current_kept_even_if_over_budget() {
        let h = harness(1, 1, &[], &[]);
        h.prefetcher.request(&[p("a")]);
        h.wait_ready(1);
        assert!(h.prefetcher.get(&p("a")).is_some());
    }

    #[test]
    fn evicts_lowest_priority_wanted_first() {
        let h = harness(1, 8, &[], &[]);
        h.prefetcher.request(&[p("a"), p("b"), p("c")]);
        h.wait_ready(2);
        // Budget fits two: a (current) and b (next) stay; c, the farthest, goes.
        assert!(h.prefetcher.get(&p("a")).is_some());
        assert!(h.prefetcher.get(&p("b")).is_some());
        assert!(h.prefetcher.get(&p("c")).is_none());
    }

    #[test]
    fn entry_evicted_on_insert_does_not_fire_on_ready() {
        let h = harness(1, 8, &[], &[]);
        h.prefetcher.request(&[p("a"), p("b"), p("c")]);
        for _ in 0..3 {
            h.wait_started();
        }
        let Harness {
            ready_count,
            prefetcher,
            ..
        } = h;
        drop(prefetcher); // joins workers: c's in-flight decode finishes first
        assert_eq!(ready_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn on_ready_fires_once_per_decode() {
        let h = harness(2, u64::MAX, &[], &[]);
        h.prefetcher.request(&[p("a"), p("b")]);
        h.wait_ready(2);
        h.prefetcher.request(&[p("a"), p("b"), p("c")]);
        assert_eq!(h.wait_ready(1), vec![p("c")]);
        assert_eq!(h.ready_count.load(Ordering::SeqCst), 3);
        assert_eq!(h.calls.lock().unwrap().len(), 3);
    }

    /// Panics for "boom", decodes everything else.
    struct PanickingDecoder;

    impl Decode for PanickingDecoder {
        fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError> {
            assert!(path != Path::new("boom"), "decoder blew up");
            Ok(one_px())
        }
    }

    #[test]
    fn panicking_decoder_yields_an_error_and_the_worker_goes_on() {
        let (tx, ready) = channel();
        let tx = Mutex::new(tx);
        let prefetcher = Prefetcher::new(PanickingDecoder, 1, u64::MAX, move |path| {
            let _ = tx.lock().unwrap().send(path);
        });
        prefetcher.request(&[p("boom")]);
        assert_eq!(ready.recv_timeout(WAIT).unwrap(), p("boom"));
        let got = prefetcher
            .get(&p("boom"))
            .expect("the panic is cached as an error");
        assert!(matches!(got.as_ref(), Err(DecodeError::Corrupt(m)) if m == "decoder panicked"));
        // The single worker survived and serves the next request.
        prefetcher.request(&[p("next")]);
        assert_eq!(ready.recv_timeout(WAIT).unwrap(), p("next"));
        assert!(prefetcher.get(&p("next")).unwrap().is_ok());
    }

    #[test]
    fn drop_joins_workers_without_hanging() {
        let (done_tx, done_rx) = channel();
        std::thread::spawn(move || {
            let h = harness(2, u64::MAX, &["a"], &[]);
            h.prefetcher.request(&[p("a"), p("b")]);
            assert_eq!(h.wait_started(), p("a"));
            h.releases[&p("a")].send(()).unwrap();
            drop(h);
            done_tx.send(()).unwrap();
        });
        done_rx.recv_timeout(WAIT).expect("drop hung");
    }
}
