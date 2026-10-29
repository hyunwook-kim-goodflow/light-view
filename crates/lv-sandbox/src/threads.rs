//! The helper threads this crate starts (a worker's writer, reader and stderr threads),
//! counted until their OS threads have ended.
//!
//! The Explorer extension is a DLL that per-user installs load into `explorer.exe`: it must
//! not be unloaded while one of these threads still runs its code, and [`run_once`]
//! (crate::run_once) returns before its threads have ended. Its `DllCanUnloadNow` asks
//! [`live_threads`].
//!
//! A thread runs code of the module that started it until its OS thread exits: after its
//! closure come the rest of std's thread start routine and the thread-local destructors.
//! So a thread counts as alive until its thread object is signalled (`WaitForSingleObject`),
//! not until its closure returns.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::{Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;

/// The handles of started threads whose OS threads may not have exited yet.
pub(crate) struct Registry(Mutex<Vec<JoinHandle<()>>>);

impl Registry {
    pub(crate) const fn new() -> Registry {
        Registry(Mutex::new(Vec::new()))
    }

    fn lock(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forgets (joins: they are gone, so at once) the threads whose OS threads have
    /// exited, and returns how many are left.
    fn live(&self) -> usize {
        prune(&mut self.lock())
    }

    /// Starts a thread named `name` running `f`, registered before anyone can ask
    /// [`Registry::live`] again.
    fn spawn<F>(&self, name: String, f: F) -> io::Result<()>
    where
        F: FnOnce() + Send + 'static,
    {
        let mut threads = self.lock();
        // Pruned here too, so the list stays short in a process that never asks.
        prune(&mut threads);
        let handle = thread::Builder::new().name(name).spawn(f)?;
        threads.push(handle);
        Ok(())
    }
}

fn prune(threads: &mut Vec<JoinHandle<()>>) -> usize {
    let mut i = 0;
    while i < threads.len() {
        if exited(&threads[i]) {
            // Its OS thread has exited: join returns at once (a panic is its own business).
            let _ = threads.swap_remove(i).join();
        } else {
            i += 1;
        }
    }
    threads.len()
}

fn exited(thread: &JoinHandle<()>) -> bool {
    let handle = HANDLE(thread.as_raw_handle());
    // SAFETY: the JoinHandle owns this thread handle and keeps it open while borrowed.
    unsafe { WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 }
}

/// Every helper thread this crate starts.
static LIVE: Registry = Registry::new();

/// Helper threads (writer, reader, stderr) of this crate's workers whose OS threads have not
/// exited yet, in every caller of the process. Zero means none of them runs code any more.
pub fn live_threads() -> usize {
    LIVE.live()
}

/// Starts a thread named `name` running `f`, counted in [`live_threads`] until its OS thread
/// has exited.
pub(crate) fn spawn<F>(name: String, f: F) -> io::Result<()>
where
    F: FnOnce() + Send + 'static,
{
    LIVE.spawn(name, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn wait_for(registry: &Registry, want: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if registry.live() == want {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        registry.live() == want
    }

    #[test]
    fn a_thread_is_counted_until_it_ends() {
        static REGISTRY: Registry = Registry::new();
        let (go, wait) = mpsc::channel::<()>();
        REGISTRY
            .spawn("test".into(), move || {
                let _ = wait.recv();
            })
            .unwrap();
        assert_eq!(REGISTRY.live(), 1, "counted from the spawn on");
        drop(go);
        assert!(wait_for(&REGISTRY, 0), "not uncounted after it ended");
    }

    /// Blocks in its destructor, which runs after the thread's closure has returned (thread
    /// local destructors run as the thread exits).
    struct Linger(Option<mpsc::Receiver<()>>);

    impl Drop for Linger {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() {
                let _ = release.recv();
            }
        }
    }

    thread_local! {
        static LINGER: std::cell::RefCell<Linger> = const { std::cell::RefCell::new(Linger(None)) };
    }

    #[test]
    fn a_thread_is_counted_after_its_closure_until_its_os_thread_exits() {
        static REGISTRY: Registry = Registry::new();
        let (returned_tx, returned) = mpsc::channel::<()>();
        let (release, release_rx) = mpsc::channel::<()>();
        REGISTRY
            .spawn("test".into(), move || {
                LINGER.with(|l| l.borrow_mut().0 = Some(release_rx));
                // The closure's last statement.
                let _ = returned_tx.send(());
            })
            .unwrap();
        returned.recv().unwrap();
        // The closure has returned; the thread still runs (its thread-local destructor).
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            REGISTRY.live(),
            1,
            "uncounted while its OS thread still runs"
        );
        drop(release);
        assert!(
            wait_for(&REGISTRY, 0),
            "not uncounted after its OS thread exited"
        );
    }

    #[test]
    fn a_panicking_thread_is_uncounted() {
        static REGISTRY: Registry = Registry::new();
        REGISTRY
            .spawn("test".into(), || panic!("expected in this test"))
            .unwrap();
        assert!(wait_for(&REGISTRY, 0));
    }
}
