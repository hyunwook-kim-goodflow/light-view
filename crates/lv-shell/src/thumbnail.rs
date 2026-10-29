//! The thumbnail handler: `IInitializeWithStream` + `IThumbnailProvider`.
//!
//! `Initialize` keeps Explorer's stream; `GetThumbnail(cx)` reads it (up to [`READ_CAP`]),
//! has one sandboxed worker make a thumbnail whose long side is `cx` at most (never
//! upscaled; `cx` clamped to [`MAX_SIDE`]) and answers it as a premultiplied ARGB bitmap.
//! Anything else is an error HRESULT, so Explorer shows the icon.
//!
//! Explorer asks for many thumbnails at once (a folder of photos), so at most
//! [`MAX_WORKERS`] thumbnail workers run at once in a process, each decoding at most
//! [`THUMBNAIL_MAX_PIXELS`] (ruling I2); a call that cannot get a worker by its deadline
//! fails like any other.

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use lv_ipc::{Request, Response};
use windows::core::{implement, Result};
use windows::Win32::Foundation::{
    ERROR_ALREADY_INITIALIZED, E_FAIL, E_INVALIDARG, E_POINTER, E_UNEXPECTED,
};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{IStream, STREAM_SEEK_SET};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    IThumbnailProvider, IThumbnailProvider_Impl, WTSAT_ARGB, WTS_ALPHATYPE,
};

use crate::stream::READ_CAP;
use crate::{bitmap, guarded, stream, ObjectCount};

/// The largest long side asked of the worker, whatever `cx` Explorer asks for (Explorer's
/// sizes go to 1024 or so; well under lv-ipc's 4096, which costs memory in the worker).
pub(crate) const MAX_SIDE: u32 = 1024;
/// How long a call may take, waiting for a worker included; `run_once` may return up to a
/// second later (ruling R3's 10 s bound).
pub(crate) const DEADLINE: Duration = Duration::from_secs(9);
/// Each worker answers one request only, so one id does.
pub(crate) const REQUEST_ID: u64 = 1;
/// The most pixels a thumbnail worker decodes (ruling I2): 64 MP, well above the 48 MP of
/// phone cameras, so each worker's Job is a quarter of the app's. Larger images get no
/// thumbnail.
pub(crate) const THUMBNAIL_MAX_PIXELS: u64 = 1 << 26;
/// Thumbnail workers that may run at once in this process (ruling I2).
pub(crate) const MAX_WORKERS: usize = 2;
/// The free thumbnail worker slots of this process.
static WORKERS: Slots = Slots::new(MAX_WORKERS);

#[implement(IThumbnailProvider, IInitializeWithStream)]
pub(crate) struct Provider {
    /// The worker to start.
    exe: PathBuf,
    stream: Mutex<Option<IStream>>,
    _count: ObjectCount,
}

impl Provider {
    pub(crate) fn new(exe: PathBuf) -> Provider {
        Provider {
            exe,
            stream: Mutex::new(None),
            _count: ObjectCount::new(),
        }
    }

    fn stream(&self) -> std::sync::MutexGuard<'_, Option<IStream>> {
        self.stream.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[allow(non_snake_case)]
impl IInitializeWithStream_Impl for Provider_Impl {
    fn Initialize(&self, stream: Option<&IStream>, _mode: u32) -> Result<()> {
        guarded(|| {
            let stream = stream.ok_or(E_INVALIDARG)?;
            let mut slot = self.stream();
            if slot.is_some() {
                return Err(ERROR_ALREADY_INITIALIZED.to_hresult().into());
            }
            *slot = Some(stream.clone());
            Ok(())
        })
    }
}

#[allow(non_snake_case)]
impl IThumbnailProvider_Impl for Provider_Impl {
    fn GetThumbnail(&self, cx: u32, bitmap: *mut HBITMAP, alpha: *mut WTS_ALPHATYPE) -> Result<()> {
        guarded(|| {
            if bitmap.is_null() || alpha.is_null() {
                return Err(E_POINTER.into());
            }
            if cx == 0 {
                return Err(E_INVALIDARG.into());
            }
            let deadline = Instant::now() + DEADLINE;
            let stream = self.stream().clone().ok_or(E_UNEXPECTED)?;
            // From the start, like the preview and details handlers; a stream that cannot
            // seek is read from where it is.
            // SAFETY: a valid stream.
            let _ = unsafe { stream.Seek(0, STREAM_SEEK_SET, None) };
            let data = stream::read_capped(&stream, READ_CAP)?;
            let max_side = cx.clamp(1, MAX_SIDE);
            let (width, height, bgra) =
                thumbnail_of(&self.exe, max_side, data, deadline).ok_or(E_FAIL)?;
            let made = bitmap::to_hbitmap(width, height, &bgra)?;
            // SAFETY: checked non-null; COM contract: writable.
            unsafe {
                *bitmap = made;
                *alpha = WTSAT_ARGB;
            }
            Ok(())
        })
    }
}

/// [`ask_worker`] for a thumbnail: once one of the [`MAX_WORKERS`] slots is free, with
/// [`THUMBNAIL_MAX_PIXELS`], all by `deadline`. `None` if no slot frees up by then.
fn thumbnail_of(
    exe: &Path,
    max_side: u32,
    data: Vec<u8>,
    deadline: Instant,
) -> Option<(u32, u32, Vec<u8>)> {
    if data.is_empty() {
        return None;
    }
    let _slot = WORKERS.acquire(deadline)?;
    let left = deadline.saturating_duration_since(Instant::now());
    ask_worker(exe, max_side, THUMBNAIL_MAX_PIXELS, data, left)
}

/// The worker's (`exe`) image of the HEIC file `data`, its long side `max_side` at most (the
/// caller clamps it well under lv-ipc's 4096), decoding at most `max_pixels`, within
/// `timeout`: width, height and premultiplied BGRA. `None` for an empty file or any failure.
/// The thumbnail and the preview handler both ask so.
pub(crate) fn ask_worker(
    exe: &Path,
    max_side: u32,
    max_pixels: u64,
    data: Vec<u8>,
    timeout: Duration,
) -> Option<(u32, u32, Vec<u8>)> {
    if data.is_empty() {
        return None;
    }
    let request = Request::ThumbnailHeif {
        id: REQUEST_ID,
        max_side,
        max_pixels,
        data,
    };
    // The request owns the file's bytes; run_once writes them without copying.
    let response = run(exe, max_pixels, request, timeout)?;
    checked(response, max_side)
}

/// What tests put in place of a worker: given `max_pixels` and the request, the answer.
#[cfg(test)]
pub(crate) type WorkerHook = std::sync::Arc<dyn Fn(u64, Request) -> Option<Response> + Send + Sync>;
/// When set, [`run`] calls it instead of starting a worker (tests).
#[cfg(test)]
pub(crate) static WORKER_HOOK: Mutex<Option<WorkerHook>> = Mutex::new(None);

/// One sandboxed worker's answer to `request` ([`lv_sandbox::run_once`]).
fn run(exe: &Path, max_pixels: u64, request: Request, timeout: Duration) -> Option<Response> {
    #[cfg(test)]
    {
        let hook = WORKER_HOOK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            return hook(max_pixels, request);
        }
    }
    lv_sandbox::run_once(exe, max_pixels, request, timeout).ok()
}

/// A counting semaphore: `n` slots, taken by [`Slots::acquire`] and given back when the
/// [`Slot`] drops.
pub(crate) struct Slots {
    free: Mutex<usize>,
    freed: Condvar,
}

/// One taken slot of a [`Slots`]; dropping it gives the slot back.
pub(crate) struct Slot<'a>(&'a Slots);

impl Slots {
    pub(crate) const fn new(n: usize) -> Slots {
        Slots {
            free: Mutex::new(n),
            freed: Condvar::new(),
        }
    }

    /// A slot, waiting for one until `deadline`; `None` if none frees up by then.
    pub(crate) fn acquire(&self, deadline: Instant) -> Option<Slot<'_>> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *free > 0 {
                *free -= 1;
                return Some(Slot(self));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            free = self
                .freed
                .wait_timeout(free, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.0.freed.notify_one();
    }
}

/// The image in `response` if it is what was asked for: an `Image` answering
/// [`REQUEST_ID`], non-empty, its long side at most `max_side` and its pixels complete.
fn checked(response: Response, max_side: u32) -> Option<(u32, u32, Vec<u8>)> {
    let Response::Image {
        id,
        width,
        height,
        bgra,
    } = response
    else {
        return None;
    };
    let len = u64::from(width) * u64::from(height) * 4;
    let fits = (1..=max_side).contains(&width) && (1..=max_side).contains(&height);
    (id == REQUEST_ID && fits && bgra.len() as u64 == len).then_some((width, height, bgra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lv_core::decode::DecodeLimits;
    use lv_ipc::FailKind;

    fn image(id: u64, width: u32, height: u32, len: usize) -> Response {
        Response::Image {
            id,
            width,
            height,
            bgra: vec![0; len],
        }
    }

    #[test]
    fn a_slot_is_waited_for_until_the_deadline_only() {
        let slots = Slots::new(2);
        let a = slots.acquire(Instant::now()).expect("first slot");
        let _b = slots.acquire(Instant::now()).expect("second slot");
        let start = Instant::now();
        let wait = Duration::from_millis(100);
        assert!(slots.acquire(start + wait).is_none(), "a third slot");
        assert!(start.elapsed() >= wait, "gave up before its deadline");
        drop(a);
        assert!(slots.acquire(Instant::now()).is_some(), "the freed slot");
    }

    #[test]
    fn thumbnails_decode_at_most_64_mp() {
        assert_eq!(THUMBNAIL_MAX_PIXELS, 1 << 26);
        assert!(THUMBNAIL_MAX_PIXELS < DecodeLimits::default().max_pixels);
        assert_eq!(MAX_WORKERS, 2);
    }

    #[test]
    fn only_the_image_asked_for_is_taken() {
        assert_eq!(
            checked(image(REQUEST_ID, 2, 3, 24), 3),
            Some((2, 3, vec![0; 24]))
        );
        assert_eq!(checked(image(REQUEST_ID + 1, 2, 3, 24), 3), None, "id");
        assert_eq!(checked(image(REQUEST_ID, 4, 3, 48), 3), None, "too wide");
        assert_eq!(checked(image(REQUEST_ID, 3, 4, 48), 3), None, "too tall");
        assert_eq!(checked(image(REQUEST_ID, 0, 3, 0), 3), None, "empty");
        assert_eq!(checked(image(REQUEST_ID, 2, 3, 20), 3), None, "short");
        assert_eq!(checked(image(REQUEST_ID, 2, 3, 28), 3), None, "long");
        let failed = Response::Failed {
            id: REQUEST_ID,
            kind: FailKind::Unsupported,
        };
        assert_eq!(checked(failed, 3), None);
    }
}
