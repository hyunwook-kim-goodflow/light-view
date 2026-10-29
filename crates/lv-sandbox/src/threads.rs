//! The helper threads this crate starts (a worker's writer, reader and stderr threads),
//! counted while they run.
//!
//! The Explorer extension is a DLL that per-user installs load into `explorer.exe`: it must
//! not be unloaded while one of these threads still runs its code, and [`run_once`]
//! (crate::run_once) returns before its threads have ended. Its `DllCanUnloadNow` asks
//! [`live_threads`].

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// A number of running threads.
pub(crate) struct ThreadCount(AtomicUsize);

impl ThreadCount {
    pub(crate) const fn new() -> ThreadCount {
        ThreadCount(AtomicUsize::new(0))
    }

    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// Every helper thread this crate starts.
static LIVE: ThreadCount = ThreadCount::new();

/// Helper threads (writer, reader, stderr) of this crate's workers that have not ended yet,
/// in every caller of the process. Zero means none of them runs this crate's code any more,
/// except for the few instructions after a thread's last step (its count drops as its
/// closure returns, then the thread exits).
pub fn live_threads() -> usize {
    LIVE.get()
}

/// Starts a thread named `name` running `f`, counted in [`live_threads`] until `f` returns
/// or unwinds.
pub(crate) fn spawn<F>(name: String, f: F) -> io::Result<()>
where
    F: FnOnce() + Send + 'static,
{
    spawn_counted(&LIVE, name, f)
}

/// Counts one thread on `count` while it lives.
struct Live(&'static ThreadCount);

impl Live {
    fn new(count: &'static ThreadCount) -> Live {
        count.0.fetch_add(1, Ordering::SeqCst);
        Live(count)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn spawn_counted<F>(count: &'static ThreadCount, name: String, f: F) -> io::Result<()>
where
    F: FnOnce() + Send + 'static,
{
    // Counted before the thread exists, so there is no moment where it runs uncounted. If
    // the spawn fails, the closure and with it `live` are dropped here.
    let live = Live::new(count);
    thread::Builder::new()
        .name(name)
        .spawn(move || {
            let _live = live;
            f();
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn wait_for(count: &ThreadCount, want: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if count.get() == want {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        count.get() == want
    }

    #[test]
    fn a_thread_is_counted_until_it_ends() {
        static COUNT: ThreadCount = ThreadCount::new();
        let (go, wait) = mpsc::channel::<()>();
        spawn_counted(&COUNT, "test".into(), move || {
            let _ = wait.recv();
        })
        .unwrap();
        assert_eq!(COUNT.get(), 1, "counted from the spawn on");
        drop(go);
        assert!(wait_for(&COUNT, 0), "not uncounted after it ended");
    }

    #[test]
    fn a_panicking_thread_is_uncounted() {
        static COUNT: ThreadCount = ThreadCount::new();
        spawn_counted(&COUNT, "test".into(), || panic!("expected in this test")).unwrap();
        assert!(wait_for(&COUNT, 0));
    }
}
