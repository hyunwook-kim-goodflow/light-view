//! At most one preview render runs in the process (ruling I2): a newer request replaces the
//! waiting one, so a request replaced before it starts never starts, and scrolling quickly
//! through a folder starts the first file's worker and the last one's only.
//!
//! One counted background thread (`lv_sandbox::spawn_counted`) runs while there is work: it
//! takes the waiting request, renders it unless its preview is no longer current, and hands
//! the result to its window ([`Shared::deliver`]), until nothing waits.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::Result;
use windows::Win32::Foundation::E_FAIL;

use super::window::Shared;
use super::Render;

/// One preview to render: the file's bytes for the preview `generation` of `shared`.
pub(crate) struct Request {
    pub(crate) shared: Arc<Shared>,
    pub(crate) generation: u64,
    pub(crate) data: Vec<u8>,
    pub(crate) max_side: u32,
    pub(crate) render: Render,
}

struct Queue {
    waiting: Option<Request>,
    /// Whether the background thread runs (it takes `waiting` before it ends).
    running: bool,
    /// Renders started (tests).
    #[cfg(test)]
    started: usize,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    waiting: None,
    running: false,
    #[cfg(test)]
    started: 0,
});

fn queue() -> MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Queues `request` in place of any waiting one (dropped unstarted) and starts the
/// background thread if it does not run.
pub(crate) fn submit(request: Request) -> Result<()> {
    let replaced;
    {
        let mut queue = queue();
        replaced = queue.waiting.replace(request);
        if !queue.running {
            if lv_sandbox::spawn_counted("light-view preview".into(), run).is_err() {
                // Nothing will take it: drop it with the lock released.
                let mine = queue.waiting.take();
                drop(queue);
                drop((mine, replaced));
                return Err(E_FAIL.into());
            }
            queue.running = true;
        }
    }
    // The replaced request (a whole file's bytes) is freed outside the lock.
    drop(replaced);
    Ok(())
}

/// Drops the waiting request of `shared`, if any: its preview is over (Unload).
pub(crate) fn cancel(shared: &Shared) {
    let mine = {
        let mut queue = queue();
        let is_mine = queue
            .waiting
            .as_ref()
            .is_some_and(|r| std::ptr::eq(Arc::as_ptr(&r.shared), shared));
        if is_mine {
            queue.waiting.take()
        } else {
            None
        }
    };
    drop(mine);
}

/// The background thread: renders waiting requests until none is left.
fn run() {
    loop {
        let request = {
            let mut queue = queue();
            match queue.waiting.take() {
                Some(request) => request,
                None => {
                    queue.running = false;
                    return;
                }
            }
        };
        if !request.shared.is_current(request.generation) {
            continue;
        }
        #[cfg(test)]
        {
            queue().started += 1;
        }
        let Request {
            shared,
            generation,
            data,
            max_side,
            render,
        } = request;
        // A panic in a render is a failed preview, never output (no logging).
        if let Ok(Some(image)) = catch_unwind(AssertUnwindSafe(|| render(data, max_side))) {
            shared.deliver(generation, image);
        }
    }
}

/// Renders started so far in this process (tests).
#[cfg(test)]
pub(crate) fn started() -> usize {
    queue().started
}
