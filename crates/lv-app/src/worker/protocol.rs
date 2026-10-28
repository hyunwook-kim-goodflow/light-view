//! What the pool asks workers and what it accepts back: the request and answer types,
//! the check of each answer against its request, the size caps and deadlines, and the
//! decode, rotation and WebP encoding calls built on [`WorkerPool::request`].

use std::time::Duration;

use lv_core::decode::{max_file_bytes, DecodeError, DecodeLimits};
use lv_core::jpeg_check;
use lv_core::jpeg_extent::check_single_image;
use lv_core::resize::{target_size, FitOutcome, Target};
use lv_ipc::{
    max_bytes_response, max_request_data, webp_fit_fits, FailKind, FitTarget, Request, Response,
    BYTES_FIXED_LEN,
};

use super::{WorkerPool, DEFAULT_TIMEOUT, NOT_RESPONDING};

/// A rotation's deadline grows by 10 s per 100 MP (ruling R4): 100 ns per pixel.
const ROTATE_NANOS_PER_PIXEL: u64 = 100;

pub(super) type Decoded = (u32, u32, Vec<u8>);

/// What a worker is asked to do.
#[derive(Debug)]
pub enum PoolRequest {
    /// Decode a HEIF file to premultiplied sRGB BGRA8; answered by [`Reply::Image`].
    DecodeHeif { data: Vec<u8> },
    /// Rotate a JPEG losslessly by `quarters` (`1..=3`) clockwise quarter turns; answered
    /// by [`Reply::Bytes`].
    RotateJpeg { quarters: u8, data: Vec<u8> },
    /// Resize a source of straight RGBA8 pixels (`width * height * 4` bytes) to `target`
    /// and fit it into a lossy WebP from `start_quality` (`1..=100`) down, all in the
    /// worker; answered by [`Reply::Fit`].
    FitWebp {
        width: u32,
        height: u32,
        start_quality: u8,
        target: FitTarget,
        rgba: Vec<u8>,
    },
}

/// A worker's answer, always the kind the request asked for.
#[derive(Debug)]
pub enum Reply {
    Image {
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    },
    /// The rotated JPEG, at most `lv_ipc::max_bytes_response(input)` bytes. Not yet
    /// checked beyond that: the caller verifies it before using it.
    Bytes { data: Vec<u8> },
    /// The outcome of a WebP fit. A fitted WebP has been checked to be a RIFF/WEBP
    /// container of its stated length, no larger than the source or the budget, at most
    /// the start quality; a `CantFit` only answers a byte target, with `smallest` over it.
    Fit(FitOutcome),
}

#[derive(Debug)]
pub enum PoolError {
    /// As for any decode: `Unsupported` when no worker can run, `TooLarge`, `FileTooLarge`,
    /// and `Corrupt` for bad input or a worker that failed ("작업자가 응답하지 않아요").
    Failed(DecodeError),
    /// `RotateJpeg` only: the JPEG's size is not a multiple of its MCU, so its pixels cannot
    /// be rotated losslessly (the save falls back to the EXIF orientation).
    NotPerfect,
    /// `RotateJpeg` only (ruling R7): data after EOI, an MPF segment, or no EOI found. The
    /// rotation would lose what follows the first image (MPF images, gain maps, motion-photo
    /// video), so the save must fall back to the EXIF orientation and keep the bytes.
    HasTrailingData,
    /// No worker answered: it hung, died, broke the protocol or no slot came free in time
    /// ("작업자가 응답하지 않아요"). Says nothing about the file.
    WorkerUnavailable,
}

impl From<DecodeError> for PoolError {
    fn from(e: DecodeError) -> Self {
        PoolError::Failed(e)
    }
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolError::Failed(e) => e.fmt(f),
            PoolError::NotPerfect => write!(f, "the JPEG cannot be rotated losslessly"),
            PoolError::HasTrailingData => write!(f, "the JPEG holds more than one image"),
            PoolError::WorkerUnavailable => write!(f, "{NOT_RESPONDING}"),
        }
    }
}

/// The kind of answer a request expects.
#[derive(Clone, Copy)]
pub(super) enum Expect {
    Image,
    /// A rotated JPEG for an input of this many bytes.
    Bytes {
        input_len: u64,
    },
    /// A WebP fit of a `src` source sent as `input_len` bytes of pixels, from
    /// `start_quality` down, within `budget` bytes for a byte target.
    Fit {
        input_len: u64,
        src: (u32, u32),
        start_quality: u8,
        budget: Option<u64>,
    },
}

/// The wire request for `req` and the kind of answer it expects. A rotation by other
/// than 1 to 3 quarter turns is `Corrupt`, and so is a fit of an empty source, of pixels
/// that are not `width * height * 4` bytes, from a start quality outside 1 to 100 or to an
/// invalid target (the app's mistakes, which lv-ipc would refuse to write).
pub(super) fn to_request(
    req: PoolRequest,
    id: u64,
    max_pixels: u64,
) -> Result<(Request, Expect), PoolError> {
    Ok(match req {
        PoolRequest::DecodeHeif { data } => (
            Request::DecodeHeif {
                id,
                max_pixels,
                data,
            },
            Expect::Image,
        ),
        PoolRequest::RotateJpeg { quarters, data } => {
            if !(1..=3).contains(&quarters) {
                let msg = format!("{quarters} quarter turns");
                return Err(DecodeError::Corrupt(msg).into());
            }
            let input_len = data.len() as u64;
            let request = Request::RotateJpeg {
                id,
                quarters,
                max_pixels,
                data,
            };
            (request, Expect::Bytes { input_len })
        }
        PoolRequest::FitWebp {
            width,
            height,
            start_quality,
            target,
            rgba,
        } => {
            let expected = (u64::from(width) * u64::from(height)).checked_mul(4);
            if width == 0
                || height == 0
                || expected != Some(rgba.len() as u64)
                || !(1..=100).contains(&start_quality)
                || !target.is_valid()
            {
                let msg = format!(
                    "{} bytes of pixels for a {width}x{height} WebP fit from quality \
                     {start_quality} to {target:?}",
                    rgba.len()
                );
                return Err(DecodeError::Corrupt(msg).into());
            }
            let budget = match target {
                FitTarget::MaxBytes { bytes, .. } => Some(bytes),
                _ => None,
            };
            let expect = Expect::Fit {
                input_len: rgba.len() as u64,
                src: (width, height),
                start_quality,
                budget,
            };
            let request = Request::FitWebp {
                id,
                width,
                height,
                start_quality,
                target,
                rgba,
            };
            (request, expect)
        }
    })
}

/// Whether `data` is a RIFF/WEBP container whose RIFF size matches its length.
fn is_webp_file(data: &[u8]) -> bool {
    data.len() >= 12
        && data.starts_with(b"RIFF")
        && &data[8..12] == b"WEBP"
        && u64::from(u32::from_le_bytes([data[4], data[5], data[6], data[7]])) + 8
            == data.len() as u64
}

impl WorkerPool {
    /// Decodes a HEIF file in a worker to premultiplied sRGB BGRA8 (width, height, pixels).
    pub fn decode_heif(&self, data: Vec<u8>) -> Result<Decoded, DecodeError> {
        match self.request(PoolRequest::DecodeHeif { data }, self.timeout) {
            Ok(Reply::Image {
                width,
                height,
                bgra,
            }) => Ok((width, height, bgra)),
            Err(PoolError::Failed(e)) => Err(e),
            // `check` only lets an `Image` answer or a decode failure through for a decode.
            Err(PoolError::WorkerUnavailable)
            | Ok(Reply::Bytes { .. } | Reply::Fit(_))
            | Err(PoolError::NotPerfect)
            | Err(PoolError::HasTrailingData) => Err(DecodeError::Corrupt(NOT_RESPONDING.into())),
        }
    }

    /// Rotates a JPEG losslessly by `quarters` clockwise quarter turns in a worker, with
    /// the deadline its size calls for. The size comes from [`jpeg_check::inspect`]: a
    /// file it rejects is `Corrupt` and one over the pixel limit is `TooLarge`, both
    /// without being sent.
    ///
    /// Contract (ruling R7): input with data after EOI or an MPF APP2 (`"MPF\0"`) segment
    /// must not be pixel-rotated, since the rotation keeps only the first image. Such input
    /// (and input whose EOI cannot be found) is refused with
    /// [`PoolError::HasTrailingData`] before being sent; the worker checks again.
    pub fn rotate_jpeg(&self, quarters: u8, data: Vec<u8>) -> Result<Reply, PoolError> {
        let single = check_single_image(&data);
        let info = match jpeg_check::inspect(&data) {
            Ok(info) => info,
            // A JPEG whose end cannot be found (truncated, say) is refused like one with
            // trailing data (ruling R7); anything else unreadable is corrupt.
            Err(_) if data.starts_with(&[0xFF, 0xD8]) && single.is_err() => {
                return Err(PoolError::HasTrailingData)
            }
            Err(_) => return Err(DecodeError::Corrupt("JPEG 헤더를 읽을 수 없어요".into()).into()),
        };
        let (width, height) = (u32::from(info.width), u32::from(info.height));
        let pixels = u64::from(width) * u64::from(height);
        if pixels > self.max_pixels {
            return Err(DecodeError::TooLarge { width, height }.into());
        }
        if single.is_err() {
            return Err(PoolError::HasTrailingData);
        }
        let request = PoolRequest::RotateJpeg { quarters, data };
        match self.request(request, rotate_deadline(pixels)) {
            // The worker reports no size with TooLarge; the header gives it.
            Err(PoolError::Failed(DecodeError::TooLarge { .. })) => {
                Err(DecodeError::TooLarge { width, height }.into())
            }
            other => other,
        }
    }

    /// Resizes `rgba` (straight RGBA8 of `size`) to `target` and fits it into a lossy WebP
    /// with `lv_core::resize::fit`, from `start_quality` down, in one sandboxed worker of
    /// its own: the pixels are sent once and every trial runs there (spec §10). The worker
    /// is started for this photo and ended after it (ruling R21: its output is saved, so a
    /// worker that decoded a hostile file must not make it). Waits at most `deadline`.
    ///
    /// The app reduces a `MaxBytes` budget by the EXIF it will attach before calling this
    /// (`lv_core::resize::exif_overhead`). A source the worker would refuse for its size (an
    /// output side over 16383, over the pixel limit, over its Job by [`webp_fit_fits`], or
    /// over the request cap) is `TooLarge` without being sent; bad arguments (wrong pixel
    /// count, a start quality outside 1 to 100, a zero side or budget in `target`) are
    /// `Corrupt`, also unsent.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the resize session (M6 Task 5) fits WebP")
    )]
    pub fn fit_webp(
        &self,
        rgba: &[u8],
        size: (u32, u32),
        target: &Target,
        start_quality: u8,
        deadline: Duration,
    ) -> Result<FitOutcome, PoolError> {
        let (width, height) = size;
        let wire = match *target {
            Target::LongEdge(n) => FitTarget::LongEdge(n),
            Target::Width(n) => FitTarget::Width(n),
            Target::Height(n) => FitTarget::Height(n),
            Target::MaxBytes { bytes, long_edge } => FitTarget::MaxBytes { bytes, long_edge },
            Target::Percent(p) => FitTarget::Percent(p),
        };
        if width != 0 && height != 0 && wire.is_valid() {
            let fits = webp_fit_fits(size, target_size(size, target), self.max_pixels)
                && u64::from(width) * u64::from(height) * 4 <= self.data_cap;
            if !fits {
                return Err(DecodeError::TooLarge { width, height }.into());
            }
        }
        let request = PoolRequest::FitWebp {
            width,
            height,
            start_quality,
            target: wire,
            rgba: rgba.to_vec(),
        };
        match self.request(request, deadline)? {
            Reply::Fit(outcome) => Ok(outcome),
            // `check` only lets a `Fit` answer through for a fit.
            Reply::Image { .. } | Reply::Bytes { .. } => Err(PoolError::WorkerUnavailable),
        }
    }

    /// Maps the answer to request `id`; `Err` means the worker broke the protocol: another
    /// request's id, an answer of the wrong kind, or one over its limits.
    pub(super) fn check(
        &self,
        id: u64,
        expect: Expect,
        response: Response,
    ) -> Result<Result<Reply, PoolError>, String> {
        let got = match &response {
            Response::Image { id, .. }
            | Response::Failed { id, .. }
            | Response::Bytes { id, .. }
            | Response::Fitted { id, .. }
            | Response::CantFit { id, .. } => *id,
        };
        if got != id {
            return Err(format!("answer for request {got}, expected {id}"));
        }
        match (expect, response) {
            (
                Expect::Image,
                Response::Image {
                    width,
                    height,
                    bgra,
                    ..
                },
            ) => {
                if u64::from(width) * u64::from(height) > self.max_pixels {
                    Err(format!("{width}x{height} image is over the pixel limit"))
                } else {
                    Ok(Ok(Reply::Image {
                        width,
                        height,
                        bgra,
                    }))
                }
            }
            (Expect::Bytes { input_len }, Response::Bytes { data, .. }) => {
                let limit = max_bytes_response(input_len);
                if data.len() as u64 > limit {
                    Err(format!("{} bytes for a {input_len}-byte JPEG", data.len()))
                } else {
                    Ok(Ok(Reply::Bytes { data }))
                }
            }
            (
                Expect::Fit {
                    input_len,
                    src,
                    start_quality,
                    budget,
                },
                Response::Fitted {
                    width,
                    height,
                    quality,
                    data,
                    ..
                },
            ) => {
                let len = data.len() as u64;
                if len > max_bytes_response(input_len) || budget.is_some_and(|b| len > b) {
                    Err(format!("a {len}-byte WebP for {input_len} bytes of pixels"))
                } else if width > src.0 || height > src.1 || quality > start_quality {
                    Err(format!(
                        "a {width}x{height} WebP at quality {quality} for a {}x{} source from \
                         quality {start_quality}",
                        src.0, src.1
                    ))
                } else if !is_webp_file(&data) {
                    Err("a fit answer that is not a WebP file".to_string())
                } else {
                    Ok(Ok(Reply::Fit(FitOutcome::Fitted {
                        bytes: data,
                        size: (width, height),
                        quality,
                    })))
                }
            }
            (
                Expect::Fit {
                    budget: Some(budget),
                    ..
                },
                Response::CantFit { smallest, .. },
            ) => {
                if smallest <= budget {
                    Err(format!(
                        "can't fit {budget} bytes, yet {smallest} bytes seen"
                    ))
                } else {
                    Ok(Ok(Reply::Fit(FitOutcome::CantFit { smallest })))
                }
            }
            (
                Expect::Bytes { .. },
                Response::Failed {
                    kind: FailKind::NotPerfect,
                    ..
                },
            ) => Ok(Err(PoolError::NotPerfect)),
            (
                Expect::Bytes { .. },
                Response::Failed {
                    kind: FailKind::HasTrailingData,
                    ..
                },
            ) => Ok(Err(PoolError::HasTrailingData)),
            (_, Response::Failed { kind, .. }) => Ok(Err(PoolError::Failed(match kind {
                FailKind::TooLarge { width, height } => DecodeError::TooLarge { width, height },
                FailKind::Corrupt(msg) => DecodeError::Corrupt(msg),
                FailKind::Unsupported => DecodeError::Unsupported,
                FailKind::NotPerfect | FailKind::HasTrailingData => {
                    return Err(format!("{kind:?} answer to a decode or a fit"))
                }
            }))),
            (Expect::Image, Response::Bytes { .. }) => Err("a JPEG answer to a decode".to_string()),
            (Expect::Bytes { .. }, Response::Image { .. }) => {
                Err("an image answer to a rotation".to_string())
            }
            (Expect::Fit { .. }, Response::Image { .. } | Response::Bytes { .. }) => {
                Err("an image or JPEG answer to a fit".to_string())
            }
            (_, Response::Fitted { .. } | Response::CantFit { .. }) => {
                Err("a fit answer to another request".to_string())
            }
        }
    }
}

/// Largest file a worker accepts in a request: lv-core's file size limit (ruling R10),
/// clamped to what one frame carries. The worker derives its own cap from the same rule.
pub(super) fn request_data_cap(max_pixels: u64) -> u64 {
    max_request_data(max_file_bytes(&DecodeLimits { max_pixels }))
}

/// Largest response the app reads from a worker: an `N`-pixel BGRA image plus room for the
/// fixed fields (ruling R10), or a rotated JPEG of the largest file a request can carry,
/// whichever is larger. The reader thread reads ahead of any request, so this is one cap per
/// worker; [`WorkerPool::check`] then holds a rotated JPEG to its own input's
/// `max_bytes_response`.
pub(super) fn response_cap(max_pixels: u64) -> u64 {
    let image = max_pixels.saturating_mul(4).saturating_add(64 * 1024);
    let jpeg = BYTES_FIXED_LEN + max_bytes_response(request_data_cap(max_pixels));
    image.max(jpeg)
}

/// Ruling R4: a rotation of a `pixels`-pixel JPEG gets 10 s plus 10 s per 100 MP.
pub fn rotate_deadline(pixels: u64) -> Duration {
    DEFAULT_TIMEOUT + Duration::from_nanos(pixels.saturating_mul(ROTATE_NANOS_PER_PIXEL))
}
