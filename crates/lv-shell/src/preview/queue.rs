//! At most one preview render runs in the process (rulings I2 and its amendment), and each
//! handler has at most one request waiting: a handler's newer request replaces its waiting
//! one, so a request replaced before it starts never starts, and scrolling quickly through a
//! folder starts the first file's worker and the last one's only. Requests of different
//! handlers (two preview panes in one prevhost) all wait, so every pane gets its image, one
//! render after another.
//!
//! One counted background thread (`lv_sandbox::spawn_counted`) runs while there is work: it
//! drops the waiting requests whose preview is no longer current, renders the newest of the
//! others and hands the result to its window ([`Shared::deliver`]), until nothing waits.

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
    /// At most one request per handler, each with its submission number.
    waiting: Vec<(u64, Request)>,
    submitted: u64,
    /// Whether the background thread runs (it empties `waiting` before it ends).
    running: bool,
    /// Renders started (tests).
    #[cfg(test)]
    started: usize,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    waiting: Vec::new(),
    submitted: 0,
    running: false,
    #[cfg(test)]
    started: 0,
});

fn queue() -> MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Takes the waiting request of `shared` out of `waiting`, if any.
fn take_of(waiting: &mut Vec<(u64, Request)>, shared: &Shared) -> Option<Request> {
    let at = waiting
        .iter()
        .position(|(_, r)| std::ptr::eq(Arc::as_ptr(&r.shared), shared))?;
    Some(waiting.swap_remove(at).1)
}

/// Queues `request` in place of its handler's waiting one (dropped unstarted) and starts
/// the background thread if it does not run.
pub(crate) fn submit(request: Request) -> Result<()> {
    let shared = Arc::clone(&request.shared);
    let replaced;
    {
        let mut queue = queue();
        replaced = take_of(&mut queue.waiting, &shared);
        queue.submitted += 1;
        let number = queue.submitted;
        queue.waiting.push((number, request));
        if !queue.running {
            if lv_sandbox::spawn_counted("light-view preview".into(), run).is_err() {
                // Nothing will take it: drop it with the lock released.
                let mine = take_of(&mut queue.waiting, &shared);
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
    let mine = take_of(&mut queue().waiting, shared);
    drop(mine);
}

/// Clears `running` if the background thread unwinds, so that a later [`submit`] starts a
/// new one: previews never stall. (A normal end clears it under the lock, as it finds no
/// work.)
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        if std::thread::panicking() {
            queue().running = false;
        }
    }
}

/// The background thread: renders the newest current request until none is left.
fn run() {
    let _running = Running;
    loop {
        let (request, stale) = {
            let mut queue = queue();
            // Lock order: the queue, then a handler's mailbox (`is_current`); nothing takes
            // them the other way round.
            let (current, stale): (Vec<_>, Vec<_>) = std::mem::take(&mut queue.waiting)
                .into_iter()
                .partition(|(_, r)| r.shared.is_current(r.generation));
            queue.waiting = current;
            let newest = queue
                .waiting
                .iter()
                .enumerate()
                .max_by_key(|(_, (number, _))| *number)
                .map(|(at, _)| at);
            match newest {
                Some(at) => {
                    #[cfg(test)]
                    {
                        queue.started += 1;
                    }
                    (queue.waiting.swap_remove(at).1, stale)
                }
                None => {
                    queue.running = false;
                    drop(queue);
                    drop(stale);
                    return;
                }
            }
        };
        // Stale requests (whole files' bytes) are freed outside the lock.
        drop(stale);
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
