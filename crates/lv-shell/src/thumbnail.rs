//! The thumbnail handler: `IInitializeWithStream` + `IThumbnailProvider`.
//!
//! `Initialize` keeps Explorer's stream; `GetThumbnail(cx)` reads it (up to [`READ_CAP`]),
//! has one sandboxed worker make a thumbnail whose long side is `cx` at most (never
//! upscaled; `cx` clamped to [`MAX_SIDE`]) and answers it as a premultiplied ARGB bitmap.
//! Anything else is an error HRESULT, so Explorer shows the icon.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use lv_core::decode::DecodeLimits;
use lv_ipc::{Request, Response};
use windows::core::{implement, Result};
use windows::Win32::Foundation::{
    ERROR_ALREADY_INITIALIZED, E_FAIL, E_INVALIDARG, E_POINTER, E_UNEXPECTED,
};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::IStream;
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
/// How long the worker may take; `run_once` may return up to a second later (ruling R3's
/// 10 s bound).
const DEADLINE: Duration = Duration::from_secs(9);
/// Each worker answers one request only, so one id does.
const REQUEST_ID: u64 = 1;

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
            let stream = self.stream().clone().ok_or(E_UNEXPECTED)?;
            let data = stream::read_capped(&stream, READ_CAP)?;
            let max_side = cx.clamp(1, MAX_SIDE);
            let (width, height, bgra) = ask_worker(&self.exe, max_side, data).ok_or(E_FAIL)?;
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

/// The worker's (`exe`) image of the HEIC file `data`, its long side `max_side` at most (the
/// caller clamps it well under lv-ipc's 4096): width, height and premultiplied BGRA. `None`
/// for an empty file or any failure. The thumbnail and the preview handler both ask so.
pub(crate) fn ask_worker(exe: &Path, max_side: u32, data: Vec<u8>) -> Option<(u32, u32, Vec<u8>)> {
    if data.is_empty() {
        return None;
    }
    let max_pixels = DecodeLimits::default().max_pixels;
    let request = Request::ThumbnailHeif {
        id: REQUEST_ID,
        max_side,
        max_pixels,
        data,
    };
    // The request owns the file's bytes; run_once writes them without copying.
    let response = lv_sandbox::run_once(exe, max_pixels, request, DEADLINE).ok()?;
    checked(response, max_side)
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
