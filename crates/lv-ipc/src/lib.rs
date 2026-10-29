#![forbid(unsafe_code)]
//! Bounded message format between the viewer app and its sandboxed decode worker.
//!
//! Both sides treat the other as untrusted: every length is checked against
//! [`MAX_PAYLOAD`] and against the bytes the frame header promised *before* anything is
//! allocated, and a frame must be consumed exactly (no trailing bytes).
//!
//! # Frame
//!
//! ```text
//! MAGIC "LVW1" (4) | kind: u8 | len: u64 LE | payload (len bytes)
//! ```
//!
//! `len > MAX_PAYLOAD` is rejected with [`IpcError::TooLong`] before any payload is read.
//! All integers are little-endian. A string is `u32 len` + UTF-8 bytes.
//!
//! # Kinds and payloads
//!
//! Requests (app to worker):
//! - `0x01` `DecodeHeif`: `id: u64 | max_pixels: u64 | data` (`data` is the rest of the payload).
//! - `0x02` `Shutdown`: empty payload.
//! - `0x03` `RotateJpeg`: `id: u64 | max_pixels: u64 | quarters: u8 | data` (`data` is the
//!   rest of the payload). `quarters` is the clockwise rotation in quarter turns, `1..=3`.
//! - `0x04` `FitWebp`: `id: u64 | width: u32 | height: u32 | start_quality: u8 | target |
//!   rgba` (`rgba` is the rest of the payload: the source's straight RGBA8, rows top to
//!   bottom). `width` and `height` must be non-zero, `start_quality` `1..=100`, and
//!   `width * height * 4` (checked u64) must equal `rgba.len()`. `target` is
//!   `tag: u8 | value: u64 | long_edge: u32`: tag `0` long edge, `1` width, `2` height
//!   (`value` a side of at least 1 that fits a u32, `long_edge` 0), `3` max bytes (`value`
//!   the budget, at least 1; `long_edge` the optional long-edge limit, 0 for none), `4`
//!   percent (`value` `1..=100`, `long_edge` 0). See [`FitTarget`].
//! - `0x05` `ThumbnailHeif`: `id: u64 | max_pixels: u64 | max_side: u32 | data` (`data` is
//!   the rest of the payload). `max_side`, the longest side of the answer, is
//!   `1..=`[`MAX_THUMBNAIL_SIDE`].
//!
//! Responses (worker to app):
//! - `0x81` `Image`: `id: u64 | width: u32 | height: u32 | bgra` (the rest of the payload).
//!   `width` and `height` must be non-zero and `width * height * 4` (checked u64) must equal
//!   `bgra.len()`. The answer to `DecodeHeif` and `ThumbnailHeif`.
//! - `0x82` `Failed`: `id: u64 | tag: u8 | fields`, with tag `0` `TooLarge`
//!   (`width: u32 | height: u32`; `0 x 0` when the worker does not report the size), tag `1`
//!   `Corrupt` (string), tag `2` `Unsupported` (none), tag `3` `NotPerfect` (none: the JPEG
//!   cannot be rotated losslessly because its size is not a multiple of its MCU), tag `4`
//!   `HasTrailingData` (none: data after EOI or an MPF segment, ruling R7).
//! - `0x83` `Bytes`: `id: u64 | data` (the rest of the payload), the answer to `RotateJpeg`
//!   (the rotated JPEG).
//! - `0x84` `Fitted`: `id: u64 | width: u32 | height: u32 | quality: u8 | data` (the rest of
//!   the payload), the WebP file a `FitWebp` produced, its size and quality. `width` and
//!   `height` must be non-zero and `quality` `1..=100`.
//! - `0x85` `CantFit`: `id: u64 | smallest: u64`: no WebP met the `FitWebp` byte budget,
//!   `smallest` is the shortest one tried.
//!
//! # Reading
//!
//! `read_request`/`read_response` take a `max_payload` cap; frames with `len` above
//! `min(max_payload, MAX_PAYLOAD)` are rejected with `TooLong` before any payload is read.
//! Callers should pass the largest payload they can legitimately expect (for the app, about
//! `max_pixels * 4 + 16`; for the worker, its memory budget). A clean EOF exactly at a frame
//! boundary yields `Ok(None)`; EOF anywhere else is `Io(UnexpectedEof)` ("the pipe died").
//! A payload shorter than its kind's fixed fields is `Malformed`.
//!
//! Any read error leaves the stream desynchronized: the caller must drop the stream (restart the
//! worker) rather than try to read another frame. Wrap pipes in `BufWriter`/`BufReader` to avoid
//! many tiny syscalls.
//!
//! # Writing
//!
//! `write_request`/`write_response` validate the whole message before writing anything: a
//! message that cannot be encoded (a payload over [`MAX_PAYLOAD`], an inconsistent image) is
//! `Err` with kind `InvalidInput` and leaves the stream untouched, so it is the sender's
//! mistake and the stream stays usable. Any other error is the stream's.
//! [`max_request_data`] and [`max_response_pixels`] give the sizes that always fit.
//!
//! # Corrupt message policy
//!
//! Writers truncate the message to at most [`MAX_CORRUPT_LEN`] (4 KiB) bytes on a char boundary.
//! Readers *reject* (`Malformed`) a longer string rather than truncating it, and also reject
//! invalid UTF-8.

mod fit;
mod frame;
#[doc(hidden)]
pub mod fuzzing;
mod limits;
mod request;

pub use fit::FitTarget;
pub use limits::{
    decoder_memory, max_bytes_response, max_request_data, max_response_pixels, webp_fit_fits,
    webp_fit_memory, worker_memory_limit, WEBP_MAX_DIMENSION,
};
pub use request::{read_request, write_request};

use std::io::{self, Read, Write};

use frame::{
    expect_consumed, invalid, read_exact_vec, read_header, read_u32, read_u64, read_u8, write_frame,
};

pub const MAGIC: [u8; 4] = *b"LVW1";
pub const MAX_PAYLOAD: u64 = 1 << 30;
/// Maximum byte length of a `Corrupt` message on the wire.
pub const MAX_CORRUPT_LEN: usize = 4096;
/// The most fixed bytes in front of a request's file bytes or pixels: `FitWebp`'s `id`,
/// `width`, `height`, `start_quality` and target (`ThumbnailHeif` has 20,
/// `RotateJpeg` 17, `DecodeHeif` 16).
pub const REQUEST_FIXED_LEN: u64 = 17 + fit::TARGET_LEN;
/// Fixed fields in front of a `DecodeHeif` request's file bytes (`id`, `max_pixels`).
const DECODE_FIXED_LEN: u64 = 16;
/// Fixed fields in front of a `ThumbnailHeif` request's file bytes (`id`, `max_pixels`,
/// `max_side`).
const THUMBNAIL_FIXED_LEN: u64 = 20;
/// The largest `max_side` a `ThumbnailHeif` request may ask for.
pub const MAX_THUMBNAIL_SIDE: u32 = 4096;
/// Fixed fields in front of a `RotateJpeg` request's file bytes (`id`, `max_pixels`,
/// `quarters`).
const ROTATE_FIXED_LEN: u64 = 17;
/// Fixed fields in front of a `Fitted` response's WebP (`id`, `width`, `height`,
/// `quality`).
pub const FITTED_FIXED_LEN: u64 = 17;
/// Fixed fields in front of an `Image` response's pixels (`id`, `width`, `height`).
pub const IMAGE_FIXED_LEN: u64 = 16;
/// Fixed field in front of a `Bytes` response's data (`id`).
pub const BYTES_FIXED_LEN: u64 = 8;
/// The most file bytes (or pixel bytes) one request can carry.
pub const MAX_REQUEST_DATA: u64 = MAX_PAYLOAD - REQUEST_FIXED_LEN;
/// The most pixels one `Image` response can carry (4 bytes each).
pub const MAX_RESPONSE_PIXELS: u64 = (MAX_PAYLOAD - IMAGE_FIXED_LEN) / 4;

const KIND_DECODE_HEIF: u8 = 0x01;
const KIND_SHUTDOWN: u8 = 0x02;
const KIND_ROTATE_JPEG: u8 = 0x03;
const KIND_FIT_WEBP: u8 = 0x04;
const KIND_THUMBNAIL_HEIF: u8 = 0x05;
const KIND_IMAGE: u8 = 0x81;
const KIND_FAILED: u8 = 0x82;
const KIND_BYTES: u8 = 0x83;
const KIND_FITTED: u8 = 0x84;
const KIND_CANT_FIT: u8 = 0x85;

const TAG_TOO_LARGE: u8 = 0;
const TAG_CORRUPT: u8 = 1;
const TAG_UNSUPPORTED: u8 = 2;
const TAG_NOT_PERFECT: u8 = 3;
const TAG_HAS_TRAILING_DATA: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    DecodeHeif {
        id: u64,
        max_pixels: u64,
        data: Vec<u8>,
    },
    /// Rotate a JPEG losslessly by `quarters` (`1..=3`) clockwise quarter turns.
    RotateJpeg {
        id: u64,
        quarters: u8,
        max_pixels: u64,
        data: Vec<u8>,
    },
    /// Resize the source (straight RGBA8, `width * height * 4` bytes) to `target` and encode
    /// it as a lossy WebP, searching quality down from `start_quality` and then resolution
    /// for a byte budget (`lv_core::resize::fit`); answered by `Fitted` or `CantFit`.
    FitWebp {
        id: u64,
        width: u32,
        height: u32,
        start_quality: u8,
        target: FitTarget,
        rgba: Vec<u8>,
    },
    /// A thumbnail of a HEIF file, its longer side at most `max_side`
    /// (`1..=MAX_THUMBNAIL_SIDE`): the smallest embedded thumbnail that is big enough, else
    /// the primary image scaled down (`lv_heif::thumbnail_heif`); answered by `Image`.
    ThumbnailHeif {
        id: u64,
        max_side: u32,
        max_pixels: u64,
        data: Vec<u8>,
    },
    Shutdown,
}

impl Request {
    /// The request's id (`Shutdown` has none and reports 0).
    pub fn id(&self) -> u64 {
        match self {
            Request::DecodeHeif { id, .. }
            | Request::RotateJpeg { id, .. }
            | Request::FitWebp { id, .. }
            | Request::ThumbnailHeif { id, .. } => *id,
            Request::Shutdown => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Image {
        id: u64,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    },
    Failed {
        id: u64,
        kind: FailKind,
    },
    /// The rotated JPEG, the answer to `RotateJpeg`.
    Bytes {
        id: u64,
        data: Vec<u8>,
    },
    /// The WebP file a `FitWebp` produced, at `width` x `height` and `quality`.
    Fitted {
        id: u64,
        width: u32,
        height: u32,
        quality: u8,
        data: Vec<u8>,
    },
    /// No WebP met the `FitWebp` byte budget; `smallest` is the shortest one tried.
    CantFit {
        id: u64,
        smallest: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailKind {
    TooLarge {
        width: u32,
        height: u32,
    },
    Corrupt(String),
    Unsupported,
    /// `RotateJpeg` only: the image size is not a multiple of its MCU, so the pixels
    /// cannot be rotated losslessly.
    NotPerfect,
    /// `RotateJpeg` only: data after EOI, an MPF segment, or no EOI found (ruling R7);
    /// rotating would lose what follows the first image.
    HasTrailingData,
}

#[derive(Debug)]
pub enum IpcError {
    Io(io::Error),
    BadMagic,
    TooLong(u64),
    Malformed(&'static str),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::Io(e) => write!(f, "io error: {e}"),
            IpcError::BadMagic => write!(f, "bad magic"),
            IpcError::TooLong(n) => write!(f, "payload too long: {n} bytes"),
            IpcError::Malformed(m) => write!(f, "malformed message: {m}"),
        }
    }
}

impl std::error::Error for IpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IpcError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for IpcError {
    fn from(e: io::Error) -> Self {
        IpcError::Io(e)
    }
}

/// Largest prefix of `s` that is at most `max` bytes and ends on a char boundary.
fn truncate_on_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn write_response(w: &mut impl Write, r: &Response) -> io::Result<()> {
    match r {
        Response::Image {
            id,
            width,
            height,
            bgra,
        } => {
            if *width == 0 || *height == 0 {
                return Err(invalid("image dimensions must be non-zero"));
            }
            let expected = u64::from(*width)
                .checked_mul(u64::from(*height))
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(|| invalid("image size overflows"))?;
            if expected != bgra.len() as u64 {
                return Err(invalid("bgra length does not match width*height*4"));
            }
            write_frame(w, KIND_IMAGE, IMAGE_FIXED_LEN + expected)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&width.to_le_bytes())?;
            w.write_all(&height.to_le_bytes())?;
            w.write_all(bgra)
        }
        Response::Failed { id, kind } => {
            let mut p = id.to_le_bytes().to_vec();
            match kind {
                FailKind::TooLarge { width, height } => {
                    p.push(TAG_TOO_LARGE);
                    p.extend_from_slice(&width.to_le_bytes());
                    p.extend_from_slice(&height.to_le_bytes());
                }
                FailKind::Corrupt(msg) => {
                    let msg = truncate_on_boundary(msg, MAX_CORRUPT_LEN);
                    p.push(TAG_CORRUPT);
                    p.extend_from_slice(&(msg.len() as u32).to_le_bytes());
                    p.extend_from_slice(msg.as_bytes());
                }
                FailKind::Unsupported => p.push(TAG_UNSUPPORTED),
                FailKind::NotPerfect => p.push(TAG_NOT_PERFECT),
                FailKind::HasTrailingData => p.push(TAG_HAS_TRAILING_DATA),
            }
            write_frame(w, KIND_FAILED, p.len() as u64)?;
            w.write_all(&p)
        }
        Response::Bytes { id, data } => {
            let len = BYTES_FIXED_LEN
                .checked_add(data.len() as u64)
                .ok_or_else(|| invalid("too long"))?;
            write_frame(w, KIND_BYTES, len)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(data)
        }
        Response::Fitted {
            id,
            width,
            height,
            quality,
            data,
        } => {
            if *width == 0 || *height == 0 || !(1..=100).contains(quality) {
                return Err(invalid("fitted size must be non-zero and quality 1..=100"));
            }
            let len = FITTED_FIXED_LEN
                .checked_add(data.len() as u64)
                .ok_or_else(|| invalid("too long"))?;
            write_frame(w, KIND_FITTED, len)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&width.to_le_bytes())?;
            w.write_all(&height.to_le_bytes())?;
            w.write_all(&[*quality])?;
            w.write_all(data)
        }
        Response::CantFit { id, smallest } => {
            write_frame(w, KIND_CANT_FIT, 16)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&smallest.to_le_bytes())
        }
    }
}

/// Reads one response; `Ok(None)` means clean EOF at a frame boundary.
pub fn read_response(r: &mut impl Read, max_payload: u64) -> Result<Option<Response>, IpcError> {
    let Some((kind, len)) = read_header(r, max_payload)? else {
        return Ok(None);
    };
    let mut p = r.by_ref().take(len);
    match kind {
        KIND_IMAGE => {
            let id = read_u64(&mut p)?;
            let width = read_u32(&mut p)?;
            let height = read_u32(&mut p)?;
            if width == 0 || height == 0 {
                return Err(IpcError::Malformed("zero image dimension"));
            }
            let expected = u64::from(width)
                .checked_mul(u64::from(height))
                .and_then(|n| n.checked_mul(4))
                .ok_or(IpcError::Malformed("image size overflows"))?;
            // `p.limit()` is what the header promised and is already <= MAX_PAYLOAD, so this
            // comparison bounds the allocation below.
            if expected != p.limit() {
                return Err(IpcError::Malformed(
                    "bgra length does not match width*height*4",
                ));
            }
            let bgra = read_exact_vec(&mut p, expected)?;
            Ok(Some(Response::Image {
                id,
                width,
                height,
                bgra,
            }))
        }
        KIND_FAILED => {
            let id = read_u64(&mut p)?;
            let kind = match read_u8(&mut p)? {
                TAG_TOO_LARGE => {
                    let width = read_u32(&mut p)?;
                    let height = read_u32(&mut p)?;
                    FailKind::TooLarge { width, height }
                }
                TAG_CORRUPT => {
                    let n = u64::from(read_u32(&mut p)?);
                    if n > MAX_CORRUPT_LEN as u64 {
                        return Err(IpcError::Malformed("corrupt message too long"));
                    }
                    if n > p.limit() {
                        return Err(IpcError::Malformed("string longer than payload"));
                    }
                    let bytes = read_exact_vec(&mut p, n)?;
                    let msg = String::from_utf8(bytes)
                        .map_err(|_| IpcError::Malformed("corrupt message is not UTF-8"))?;
                    FailKind::Corrupt(msg)
                }
                TAG_UNSUPPORTED => FailKind::Unsupported,
                TAG_NOT_PERFECT => FailKind::NotPerfect,
                TAG_HAS_TRAILING_DATA => FailKind::HasTrailingData,
                _ => return Err(IpcError::Malformed("unknown fail tag")),
            };
            expect_consumed(&p)?;
            Ok(Some(Response::Failed { id, kind }))
        }
        KIND_BYTES => {
            let id = read_u64(&mut p)?;
            let rest = p.limit();
            let data = read_exact_vec(&mut p, rest)?;
            Ok(Some(Response::Bytes { id, data }))
        }
        KIND_FITTED => {
            let id = read_u64(&mut p)?;
            let width = read_u32(&mut p)?;
            let height = read_u32(&mut p)?;
            let quality = read_u8(&mut p)?;
            if width == 0 || height == 0 || !(1..=100).contains(&quality) {
                return Err(IpcError::Malformed("fitted size or quality out of range"));
            }
            let rest = p.limit();
            let data = read_exact_vec(&mut p, rest)?;
            Ok(Some(Response::Fitted {
                id,
                width,
                height,
                quality,
                data,
            }))
        }
        KIND_CANT_FIT => {
            let id = read_u64(&mut p)?;
            let smallest = read_u64(&mut p)?;
            expect_consumed(&p)?;
            Ok(Some(Response::CantFit { id, smallest }))
        }
        KIND_DECODE_HEIF | KIND_SHUTDOWN | KIND_ROTATE_JPEG | KIND_FIT_WEBP
        | KIND_THUMBNAIL_HEIF => Err(IpcError::Malformed("request kind where response expected")),
        _ => Err(IpcError::Malformed("unknown kind")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const MIB: u64 = 1024 * 1024;

    // Strict wrappers: clean EOF is reported as UnexpectedEof so older tests stay terse.
    fn read_request(r: &mut impl Read) -> Result<Request, IpcError> {
        super::read_request(r, MAX_PAYLOAD)?
            .ok_or(IpcError::Io(io::ErrorKind::UnexpectedEof.into()))
    }

    fn read_response(r: &mut impl Read) -> Result<Response, IpcError> {
        super::read_response(r, MAX_PAYLOAD)?
            .ok_or(IpcError::Io(io::ErrorKind::UnexpectedEof.into()))
    }

    fn frame(kind: u8, len: u64, payload: &[u8]) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        v.push(kind);
        v.extend_from_slice(&len.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn req_bytes(r: &Request) -> Vec<u8> {
        let mut v = Vec::new();
        write_request(&mut v, r).unwrap();
        v
    }

    fn resp_bytes(r: &Response) -> Vec<u8> {
        let mut v = Vec::new();
        write_response(&mut v, r).unwrap();
        v
    }

    fn rt_resp(r: &Response) -> Response {
        read_response(&mut Cursor::new(resp_bytes(r))).unwrap()
    }

    fn failed_payload(id: u64, tag: u8, rest: &[u8]) -> Vec<u8> {
        let mut p = id.to_le_bytes().to_vec();
        p.push(tag);
        p.extend_from_slice(rest);
        p
    }

    #[test]
    fn request_data_cap_fits_one_frame() {
        assert_eq!(MAX_REQUEST_DATA + REQUEST_FIXED_LEN, MAX_PAYLOAD);
        assert_eq!(max_request_data(64 << 20), 64 << 20);
        assert_eq!(max_request_data(u64::MAX), MAX_REQUEST_DATA);
        // The default pixel limit (2^28) gives a file cap of 2^30 + 1 MiB: clamped.
        assert_eq!(max_request_data((1 << 30) + (1 << 20)), MAX_REQUEST_DATA);
    }

    #[test]
    fn response_pixel_cap_fits_one_frame() {
        const {
            assert!(MAX_RESPONSE_PIXELS * 4 + IMAGE_FIXED_LEN <= MAX_PAYLOAD);
            assert!((MAX_RESPONSE_PIXELS + 1) * 4 + IMAGE_FIXED_LEN > MAX_PAYLOAD);
        }
        assert_eq!(max_response_pixels(1 << 28), MAX_RESPONSE_PIXELS);
        assert_eq!(max_response_pixels(100_000_000), 100_000_000);
        assert_eq!(max_response_pixels(u64::MAX), MAX_RESPONSE_PIXELS);
    }

    #[test]
    fn job_limit_covers_decoder_memory_and_the_worker_copy() {
        for n in [1, 1 << 20, 30_000_000, 40_000_000, 100_000_000, 1 << 28] {
            assert!(decoder_memory(n) >= 8 * n && decoder_memory(n) >= 256 * MIB);
            assert_eq!(worker_memory_limit(n), decoder_memory(n) + 4 * n);
            assert_eq!(worker_memory_limit(n), (12 * n).max(256 * MIB + 4 * n));
        }
        assert_eq!(worker_memory_limit(100_000_000), 1_200_000_000);
        assert_eq!(worker_memory_limit(u64::MAX), u64::MAX);
    }

    #[test]
    fn unencodable_request_is_invalid_input_and_writes_nothing() {
        // One byte more than a frame can carry. Zeroed pages are not touched, so this
        // costs address space, not memory.
        let requests = [
            Request::DecodeHeif {
                id: 1,
                max_pixels: 1,
                data: vec![0u8; (MAX_PAYLOAD - DECODE_FIXED_LEN + 1) as usize],
            },
            Request::RotateJpeg {
                id: 1,
                quarters: 1,
                max_pixels: 1,
                data: vec![0u8; (MAX_PAYLOAD - ROTATE_FIXED_LEN + 1) as usize],
            },
        ];
        for request in &requests {
            let mut out = Vec::new();
            let err = write_request(&mut out, request).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(out.is_empty(), "nothing may be written before the error");
        }
    }

    #[test]
    fn roundtrip_all_variants() {
        let reqs = [
            Request::DecodeHeif {
                id: 7,
                max_pixels: 100_000_000,
                data: vec![1, 2, 3, 4, 5],
            },
            Request::DecodeHeif {
                id: 0,
                max_pixels: 0,
                data: vec![],
            },
            Request::Shutdown,
        ];
        for r in &reqs {
            let got = read_request(&mut Cursor::new(req_bytes(r))).unwrap();
            assert_eq!(&got, r);
        }
        let resps = [
            Response::Image {
                id: 9,
                width: 2,
                height: 3,
                bgra: vec![0xAB; 24],
            },
            Response::Failed {
                id: 1,
                kind: FailKind::TooLarge {
                    width: 20000,
                    height: 30000,
                },
            },
            Response::Failed {
                id: 2,
                kind: FailKind::Corrupt("손상된 파일이에요".into()),
            },
            Response::Failed {
                id: 3,
                kind: FailKind::Corrupt(String::new()),
            },
            Response::Failed {
                id: 4,
                kind: FailKind::Unsupported,
            },
        ];
        for r in &resps {
            assert_eq!(&rt_resp(r), r);
        }
    }

    #[test]
    fn rotate_and_bytes_roundtrip() {
        for quarters in 1..=3 {
            let r = Request::RotateJpeg {
                id: 11,
                quarters,
                max_pixels: 50_000_000,
                data: vec![0xFF, 0xD8, 0xFF, 0xD9],
            };
            assert_eq!(read_request(&mut Cursor::new(req_bytes(&r))).unwrap(), r);
        }
        let empty = Request::RotateJpeg {
            id: 0,
            quarters: 2,
            max_pixels: 0,
            data: vec![],
        };
        assert_eq!(
            read_request(&mut Cursor::new(req_bytes(&empty))).unwrap(),
            empty
        );
        for data in [vec![], vec![1, 2, 3], vec![0xAB; 70_000]] {
            let r = Response::Bytes { id: 12, data };
            assert_eq!(rt_resp(&r), r);
        }
        for kind in [FailKind::NotPerfect, FailKind::HasTrailingData] {
            let r = Response::Failed { id: 13, kind };
            assert_eq!(rt_resp(&r), r);
        }
    }

    #[test]
    fn rotate_request_layout_and_fixed_len() {
        let bytes = req_bytes(&Request::RotateJpeg {
            id: 0x0102,
            quarters: 3,
            max_pixels: 0x0304,
            data: vec![0xEE; 5],
        });
        let mut expected = frame(0x03, ROTATE_FIXED_LEN + 5, &[]);
        expected.extend_from_slice(&0x0102u64.to_le_bytes());
        expected.extend_from_slice(&0x0304u64.to_le_bytes());
        expected.push(3);
        expected.extend_from_slice(&[0xEE; 5]);
        assert_eq!(bytes, expected);
        // A DecodeHeif request carries one fixed byte less; the cap (FitWebp's) fits all.
        let decode = req_bytes(&Request::DecodeHeif {
            id: 1,
            max_pixels: 1,
            data: vec![0; 5],
        });
        assert_eq!(decode.len() as u64, 13 + ROTATE_FIXED_LEN - 1 + 5);
        const { assert!(ROTATE_FIXED_LEN <= REQUEST_FIXED_LEN) }
    }

    #[test]
    fn rotate_quarters_out_of_range_are_rejected() {
        for quarters in [0u8, 4, 255] {
            let r = Request::RotateJpeg {
                id: 1,
                quarters,
                max_pixels: 1,
                data: vec![1],
            };
            let mut out = Vec::new();
            let err = write_request(&mut out, &r).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(out.is_empty(), "nothing may be written before the error");

            let mut p = 1u64.to_le_bytes().to_vec();
            p.extend_from_slice(&1u64.to_le_bytes());
            p.push(quarters);
            let bytes = frame(0x03, p.len() as u64, &p);
            assert!(matches!(
                read_request(&mut Cursor::new(bytes)),
                Err(IpcError::Malformed(_))
            ));
        }
        // Shorter than its fixed fields.
        let bytes = frame(0x03, 16, &[0; 16]);
        assert!(matches!(
            read_request(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn thumbnail_round_trips() {
        for (max_side, data) in [
            (1, vec![]),
            (256, b"heif-bytes".to_vec()),
            (MAX_THUMBNAIL_SIDE, vec![0xAB; 70_000]),
        ] {
            let r = Request::ThumbnailHeif {
                id: 21,
                max_side,
                max_pixels: 1 << 28,
                data,
            };
            assert_eq!(read_request(&mut Cursor::new(req_bytes(&r))).unwrap(), r);
            assert_eq!(r.id(), 21);
        }
        // Layout: id | max_pixels | max_side | data.
        let bytes = req_bytes(&Request::ThumbnailHeif {
            id: 0x0102,
            max_side: 0x0506,
            max_pixels: 0x0304,
            data: vec![0xEE; 3],
        });
        let mut expected = frame(0x05, THUMBNAIL_FIXED_LEN + 3, &[]);
        expected.extend_from_slice(&0x0102u64.to_le_bytes());
        expected.extend_from_slice(&0x0304u64.to_le_bytes());
        expected.extend_from_slice(&0x0506u32.to_le_bytes());
        expected.extend_from_slice(&[0xEE; 3]);
        assert_eq!(bytes, expected);
        const { assert!(THUMBNAIL_FIXED_LEN <= REQUEST_FIXED_LEN) }
        // A response kind is not a request, and the other way round.
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn thumbnail_side_out_of_range_is_rejected() {
        assert_eq!(MAX_THUMBNAIL_SIDE, 4096);
        for max_side in [0, MAX_THUMBNAIL_SIDE + 1, u32::MAX] {
            let r = Request::ThumbnailHeif {
                id: 1,
                max_side,
                max_pixels: 1,
                data: vec![1],
            };
            let mut out = Vec::new();
            let err = write_request(&mut out, &r).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(out.is_empty(), "nothing may be written before the error");

            let mut p = 1u64.to_le_bytes().to_vec();
            p.extend_from_slice(&1u64.to_le_bytes());
            p.extend_from_slice(&max_side.to_le_bytes());
            let bytes = frame(0x05, p.len() as u64, &p);
            assert!(matches!(
                read_request(&mut Cursor::new(bytes)),
                Err(IpcError::Malformed(_))
            ));
        }
        // Shorter than its fixed fields.
        let bytes = frame(0x05, 19, &[0; 19]);
        assert!(matches!(
            read_request(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
        // Too long to encode: nothing is written.
        let big = Request::ThumbnailHeif {
            id: 1,
            max_side: 1,
            max_pixels: 1,
            data: vec![0u8; (MAX_PAYLOAD - THUMBNAIL_FIXED_LEN + 1) as usize],
        };
        let mut out = Vec::new();
        assert_eq!(
            write_request(&mut out, &big).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(out.is_empty());
    }

    fn fit_request(
        width: u32,
        height: u32,
        start_quality: u8,
        target: FitTarget,
        len: usize,
    ) -> Request {
        Request::FitWebp {
            id: 21,
            width,
            height,
            start_quality,
            target,
            rgba: (0..len).map(|i| i as u8).collect(),
        }
    }

    const TARGETS: [FitTarget; 8] = [
        FitTarget::LongEdge(1920),
        FitTarget::LongEdge(u32::MAX),
        FitTarget::Width(1),
        FitTarget::Height(600),
        FitTarget::MaxBytes {
            bytes: 2_000_000,
            long_edge: None,
        },
        FitTarget::MaxBytes {
            bytes: u64::MAX,
            long_edge: Some(1920),
        },
        FitTarget::Percent(1),
        FitTarget::Percent(100),
    ];

    #[test]
    fn fit_webp_round_trips() {
        for target in TARGETS {
            for (w, h, q) in [(1, 1, 1), (2, 2, 80), (64, 48, 100)] {
                let r = fit_request(w, h, q, target, (w * h * 4) as usize);
                assert_eq!(read_request(&mut Cursor::new(req_bytes(&r))).unwrap(), r);
                assert_eq!(r.id(), 21);
            }
        }
        // Layout, and the fixed part is the largest of all requests.
        let target = FitTarget::MaxBytes {
            bytes: 0x0A0B,
            long_edge: Some(0x0C),
        };
        let bytes = req_bytes(&fit_request(2, 1, 75, target, 8));
        let mut expected = frame(0x04, REQUEST_FIXED_LEN + 8, &[]);
        expected.extend_from_slice(&21u64.to_le_bytes());
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&[75, 3]);
        expected.extend_from_slice(&0x0A0Bu64.to_le_bytes());
        expected.extend_from_slice(&0x0Cu32.to_le_bytes());
        expected.extend(0..8u8);
        assert_eq!(bytes, expected);
        assert_eq!(REQUEST_FIXED_LEN, 30);
        // The answers.
        for r in [
            Response::Fitted {
                id: 21,
                width: 1920,
                height: 1080,
                quality: 73,
                data: b"RIFF\x04\0\0\0WEBP".to_vec(),
            },
            Response::Fitted {
                id: 22,
                width: 1,
                height: 1,
                quality: 100,
                data: vec![],
            },
            Response::CantFit {
                id: 23,
                smallest: 2_500_000,
            },
        ] {
            assert_eq!(rt_resp(&r), r);
        }
    }

    #[test]
    fn fit_webp_bad_messages_are_refused_both_ways() {
        // Writer: wrong pixel count, zero sides, start quality outside 1..=100, bad targets.
        // Nothing is written.
        let good = FitTarget::LongEdge(100);
        let bad_targets = [
            FitTarget::LongEdge(0),
            FitTarget::Width(0),
            FitTarget::Height(0),
            FitTarget::MaxBytes {
                bytes: 0,
                long_edge: None,
            },
            FitTarget::MaxBytes {
                bytes: 10,
                long_edge: Some(0),
            },
            FitTarget::Percent(0),
            FitTarget::Percent(101),
        ];
        let mut requests = vec![
            fit_request(2, 2, 80, good, 15),
            fit_request(2, 2, 80, good, 17),
            fit_request(0, 2, 80, good, 0),
            fit_request(2, 0, 80, good, 0),
            fit_request(2, 2, 0, good, 16),
            fit_request(2, 2, 101, good, 16),
            fit_request(u32::MAX, u32::MAX, 80, good, 4),
        ];
        for target in bad_targets {
            assert!(!target.is_valid());
            requests.push(fit_request(2, 2, 80, target, 16));
        }
        for r in &requests {
            let mut out = Vec::new();
            let err = write_request(&mut out, r).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{r:?}");
            assert!(out.is_empty(), "nothing may be written before the error");
        }
        for target in TARGETS {
            assert!(target.is_valid());
        }
        // Reader: the same faults, and targets the writer never produces, in hand-made
        // frames are Malformed.
        let payload = |w: u32, h: u32, q: u8, (tag, value, edge): (u8, u64, u32), n: usize| {
            let mut p = 3u64.to_le_bytes().to_vec();
            p.extend_from_slice(&w.to_le_bytes());
            p.extend_from_slice(&h.to_le_bytes());
            p.extend_from_slice(&[q, tag]);
            p.extend_from_slice(&value.to_le_bytes());
            p.extend_from_slice(&edge.to_le_bytes());
            p.extend(std::iter::repeat_n(7u8, n));
            p
        };
        let ok = (0u8, 100u64, 0u32);
        for p in [
            payload(2, 2, 80, ok, 15),
            payload(2, 2, 80, ok, 17),
            payload(0, 2, 80, ok, 0),
            payload(2, 0, 80, ok, 0),
            payload(2, 2, 0, ok, 16),
            payload(2, 2, 101, ok, 16),
            payload(u32::MAX, u32::MAX, 80, ok, 4),
            payload(2, 2, 80, (0, 0, 0), 16),
            payload(2, 2, 80, (0, 100, 5), 16),
            payload(2, 2, 80, (1, u64::from(u32::MAX) + 1, 0), 16),
            payload(2, 2, 80, (3, 0, 0), 16),
            payload(2, 2, 80, (4, 0, 0), 16),
            payload(2, 2, 80, (4, 101, 0), 16),
            payload(2, 2, 80, (4, 50, 1), 16),
            payload(2, 2, 80, (5, 1, 0), 16),
            // Shorter than the fixed fields.
            payload(2, 2, 80, ok, 0)[..29].to_vec(),
        ] {
            let bytes = frame(0x04, p.len() as u64, &p);
            assert!(
                matches!(
                    read_request(&mut Cursor::new(bytes)),
                    Err(IpcError::Malformed(_))
                ),
                "{p:?}"
            );
        }
        // Well-formed ones are read.
        for (wire, target) in [
            (ok, FitTarget::LongEdge(100)),
            (
                (3, 5000, 0),
                FitTarget::MaxBytes {
                    bytes: 5000,
                    long_edge: None,
                },
            ),
            ((4, 50, 0), FitTarget::Percent(50)),
        ] {
            let p = payload(2, 2, 80, wire, 16);
            let bytes = frame(0x04, p.len() as u64, &p);
            match read_request(&mut Cursor::new(bytes)).unwrap() {
                Request::FitWebp {
                    id: 3,
                    width: 2,
                    height: 2,
                    start_quality: 80,
                    target: got,
                    ..
                } => assert_eq!(got, target),
                other => panic!("{other:?}"),
            }
        }
        // Responses: the writer refuses, and the reader rejects, a zero size or a quality
        // outside 1..=100; CantFit is exactly 16 bytes.
        for (w, h, q) in [(0, 1, 50), (1, 0, 50), (1, 1, 0), (1, 1, 101)] {
            let r = Response::Fitted {
                id: 1,
                width: w,
                height: h,
                quality: q,
                data: vec![1],
            };
            let mut out = Vec::new();
            assert_eq!(
                write_response(&mut out, &r).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(out.is_empty());
            let mut p = 1u64.to_le_bytes().to_vec();
            p.extend_from_slice(&w.to_le_bytes());
            p.extend_from_slice(&h.to_le_bytes());
            p.push(q);
            let bytes = frame(0x84, p.len() as u64, &p);
            assert!(matches!(
                read_response(&mut Cursor::new(bytes)),
                Err(IpcError::Malformed(_))
            ));
        }
        for len in [15, 17] {
            let bytes = frame(0x85, len, &vec![0; len as usize]);
            assert!(matches!(
                read_response(&mut Cursor::new(bytes)),
                Err(IpcError::Malformed(_))
            ));
        }
        // Wrong direction.
        let bytes = req_bytes(&fit_request(1, 1, 50, good, 4));
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
        let bytes = resp_bytes(&Response::CantFit { id: 1, smallest: 2 });
        assert!(matches!(
            read_request(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn webp_fit_budget_fits_the_job() {
        assert_eq!(webp_fit_memory(0), 32 * MIB);
        assert_eq!(webp_fit_memory(1_000_000), 48_000_000 + 32 * MIB);
        assert_eq!(webp_fit_memory(u64::MAX), u64::MAX);
        let fits = |w: u32, h: u32, max: u64| webp_fit_fits((w, h), (w, h), max);
        // The default limit (2^28 pixels, a 3 GiB Job) takes a 12 MP photo, and about 66 MP.
        let default = 1u64 << 28;
        assert!(fits(4000, 3000, default));
        assert!(fits(11_000, 6000, default));
        assert!(!fits(11_000, 6100, default));
        // A worker limited to 12 MP (a 302 MiB Job) takes about 5.9 MP.
        assert!(fits(2900, 2000, 12_000_000));
        assert!(!fits(3000, 2000, 12_000_000));
        // Never over the pixel limit, even when memory would allow it.
        assert!(fits(100, 10, 1000));
        assert!(!fits(1001, 1, 1000));
        // The side limit applies to the output, not to the source.
        assert!(webp_fit_fits((20_000, 8), (1920, 1), default));
        assert!(!webp_fit_fits(
            (20_000, 8),
            (WEBP_MAX_DIMENSION + 1, 1),
            default
        ));
        assert!(webp_fit_fits(
            (WEBP_MAX_DIMENSION, 1),
            (WEBP_MAX_DIMENSION, 1),
            default
        ));
        assert!(!webp_fit_fits(
            (1, 20_000),
            (1, WEBP_MAX_DIMENSION + 1),
            default
        ));
        assert!(!fits(u32::MAX, u32::MAX, u64::MAX));
        for n in [1u64, 1 << 20, 12_000_000, 1 << 28] {
            for side in [1u32, 100, 1000, 3000, 9000, 16383] {
                if fits(side, side, n) {
                    let pixels = u64::from(side) * u64::from(side);
                    assert!(webp_fit_memory(pixels) <= worker_memory_limit(n));
                }
            }
        }
        // A 12 MP RGBA request (48 MB) fits one frame and the worker's request cap at any
        // limit (the file cap is at least 64 MiB).
        const { assert!(12_000_000 * 4 <= 64 * MIB) }
    }

    #[test]
    fn bytes_response_cap() {
        assert_eq!(max_bytes_response(0), MIB);
        assert_eq!(max_bytes_response(5_000_000), 5_000_000 + MIB);
        assert_eq!(max_bytes_response(MAX_REQUEST_DATA), MAX_PAYLOAD - 16);
        assert_eq!(max_bytes_response(u64::MAX), MAX_PAYLOAD - 16);
        const { assert!(MAX_PAYLOAD - 16 + BYTES_FIXED_LEN <= MAX_PAYLOAD) }
        // The reader enforces the caller's cap on a Bytes frame like any other.
        let resp = resp_bytes(&Response::Bytes {
            id: 1,
            data: vec![0; 100],
        });
        assert!(matches!(
            super::read_response(&mut Cursor::new(&resp), 107),
            Err(IpcError::TooLong(108))
        ));
        assert!(super::read_response(&mut Cursor::new(&resp), 108)
            .unwrap()
            .is_some());
        // A Bytes frame shorter than its id is malformed.
        let bytes = frame(0x83, 7, &[0; 7]);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn unencodable_bytes_response_is_invalid_input_and_writes_nothing() {
        // Zeroed pages are not touched: address space, not memory.
        let data = vec![0u8; (MAX_PAYLOAD - BYTES_FIXED_LEN + 1) as usize];
        let mut out = Vec::new();
        let err = write_response(&mut out, &Response::Bytes { id: 1, data }).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(out.is_empty());
    }

    #[test]
    fn two_frames_back_to_back() {
        let mut v = req_bytes(&Request::Shutdown);
        v.extend(req_bytes(&Request::DecodeHeif {
            id: 1,
            max_pixels: 2,
            data: vec![9],
        }));
        let mut c = Cursor::new(v);
        assert_eq!(read_request(&mut c).unwrap(), Request::Shutdown);
        assert!(matches!(
            read_request(&mut c).unwrap(),
            Request::DecodeHeif { id: 1, .. }
        ));
    }

    #[test]
    fn oversized_length_rejected_before_alloc() {
        // Header promises u64::MAX bytes but no payload follows: must be TooLong, not Io/alloc.
        let bytes = frame(1, u64::MAX, &[]);
        match read_request(&mut Cursor::new(bytes)) {
            Err(IpcError::TooLong(n)) => assert_eq!(n, u64::MAX),
            other => panic!("expected TooLong, got {other:?}"),
        }
        let bytes = frame(0x81, MAX_PAYLOAD + 1, &[]);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::TooLong(_))
        ));
    }

    #[test]
    fn max_payload_boundary() {
        assert!(frame::check_len(MAX_PAYLOAD, MAX_PAYLOAD).is_ok());
        assert!(frame::check_len(0, MAX_PAYLOAD).is_ok());
        assert!(matches!(
            frame::check_len(MAX_PAYLOAD + 1, u64::MAX),
            Err(IpcError::TooLong(_))
        ));
        // A frame of exactly the cap passes the header check and only then hits EOF
        // (small cap here so the test does not allocate 1 GiB).
        let bytes = frame(1, 4096, &[0; 16]);
        match super::read_request(&mut Cursor::new(bytes), 4096) {
            Err(IpcError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            other => panic!("expected UnexpectedEof, got {other:?}"),
        }
    }

    #[test]
    fn truncated_stream_is_error() {
        let full = resp_bytes(&Response::Image {
            id: 1,
            width: 4,
            height: 4,
            bgra: vec![1; 64],
        });
        for cut in [0, 3, 4, 8, 12, 13, 20, full.len() - 1] {
            match read_response(&mut Cursor::new(&full[..cut])) {
                Err(IpcError::Io(e)) => {
                    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "cut {cut}")
                }
                other => panic!("cut {cut}: expected Io(UnexpectedEof), got {other:?}"),
            }
        }
        let full = req_bytes(&Request::DecodeHeif {
            id: 1,
            max_pixels: 1,
            data: vec![1; 32],
        });
        let r = read_request(&mut Cursor::new(&full[..full.len() - 5]));
        assert!(matches!(r, Err(IpcError::Io(_))));
    }

    #[test]
    fn bad_magic_is_error() {
        let mut bytes = req_bytes(&Request::Shutdown);
        bytes[0] = b'X';
        assert!(matches!(
            read_request(&mut Cursor::new(bytes)),
            Err(IpcError::BadMagic)
        ));
    }

    #[test]
    fn image_length_mismatch_is_malformed() {
        // Frame is well-formed but pixel bytes don't match width*height*4.
        let mut p = 1u64.to_le_bytes().to_vec();
        p.extend_from_slice(&2u32.to_le_bytes());
        p.extend_from_slice(&2u32.to_le_bytes());
        p.extend_from_slice(&[0; 15]);
        let bytes = frame(0x81, p.len() as u64, &p);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
        // Writer refuses to emit an inconsistent image.
        let bad = Response::Image {
            id: 1,
            width: 2,
            height: 2,
            bgra: vec![0; 15],
        };
        assert!(write_response(&mut Vec::new(), &bad).is_err());
    }

    #[test]
    fn image_dimension_edge_cases_are_malformed() {
        for (w, h) in [(0u32, 4u32), (4, 0), (u32::MAX, u32::MAX)] {
            let mut p = 1u64.to_le_bytes().to_vec();
            p.extend_from_slice(&w.to_le_bytes());
            p.extend_from_slice(&h.to_le_bytes());
            p.extend_from_slice(&[0; 16]);
            let bytes = frame(0x81, p.len() as u64, &p);
            assert!(
                matches!(
                    read_response(&mut Cursor::new(bytes)),
                    Err(IpcError::Malformed(_))
                ),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn long_error_string_is_truncated_or_rejected() {
        // Policy: the reader rejects (Malformed); the writer truncates to <= 4 KiB on a char boundary.
        let mut rest = ((MAX_CORRUPT_LEN + 1) as u32).to_le_bytes().to_vec();
        rest.extend(vec![b'a'; MAX_CORRUPT_LEN + 1]);
        let p = failed_payload(1, 1, &rest);
        let bytes = frame(0x82, p.len() as u64, &p);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));

        // Exactly 4 KiB is accepted.
        let ok = "a".repeat(MAX_CORRUPT_LEN);
        let r = Response::Failed {
            id: 1,
            kind: FailKind::Corrupt(ok),
        };
        assert_eq!(rt_resp(&r), r);

        // Writer truncates: 3-byte chars so 4096 falls mid-char (4096 = 3 * 1365 + 1).
        let long = "한".repeat(3000);
        let r = Response::Failed {
            id: 5,
            kind: FailKind::Corrupt(long.clone()),
        };
        match rt_resp(&r) {
            Response::Failed {
                id: 5,
                kind: FailKind::Corrupt(s),
            } => {
                assert!(s.len() <= MAX_CORRUPT_LEN);
                assert!(s.len() > MAX_CORRUPT_LEN - 4);
                assert!(long.starts_with(&s));
            }
            other => panic!("unexpected {other:?}"),
        }
        let r = Response::Failed {
            id: 5,
            kind: FailKind::Corrupt("a".repeat(10_000)),
        };
        match rt_resp(&r) {
            Response::Failed {
                kind: FailKind::Corrupt(s),
                ..
            } => {
                assert_eq!(s.len(), MAX_CORRUPT_LEN)
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn invalid_utf8_corrupt_string_is_malformed() {
        let mut rest = 2u32.to_le_bytes().to_vec();
        rest.extend_from_slice(&[0xFF, 0xFE]);
        let p = failed_payload(1, 1, &rest);
        let bytes = frame(0x82, p.len() as u64, &p);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn string_length_beyond_payload_is_rejected() {
        let rest = 100u32.to_le_bytes().to_vec(); // promises 100 bytes, none present
        let p = failed_payload(1, 1, &rest);
        let bytes = frame(0x82, p.len() as u64, &p);
        assert!(read_response(&mut Cursor::new(bytes)).is_err());
    }

    #[test]
    fn trailing_bytes_are_malformed() {
        let p = failed_payload(1, 2, &[0xEE]); // Unsupported + junk
        let bytes = frame(0x82, p.len() as u64, &p);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
        let bytes = frame(2, 1, &[0]); // Shutdown with payload
        assert!(matches!(
            read_request(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
        let mut p = failed_payload(1, 0, &[]);
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.push(0);
        let bytes = frame(0x82, p.len() as u64, &p);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn unknown_kind_and_wrong_direction_are_malformed() {
        let bytes = frame(0x55, 0, &[]);
        match read_request(&mut Cursor::new(bytes.clone())) {
            Err(IpcError::Malformed(m)) => assert_eq!(m, "unknown kind"),
            other => panic!("{other:?}"),
        }
        match read_response(&mut Cursor::new(bytes)) {
            Err(IpcError::Malformed(m)) => assert_eq!(m, "unknown kind"),
            other => panic!("{other:?}"),
        }
        // Request frame read as a Response and vice versa.
        let req = req_bytes(&Request::Shutdown);
        assert!(matches!(
            read_response(&mut Cursor::new(req)),
            Err(IpcError::Malformed(_))
        ));
        let resp = resp_bytes(&Response::Failed {
            id: 1,
            kind: FailKind::Unsupported,
        });
        assert!(matches!(
            read_request(&mut Cursor::new(resp)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn unknown_fail_tag_is_malformed() {
        let p = failed_payload(1, 9, &[]);
        let bytes = frame(0x82, p.len() as u64, &p);
        assert!(matches!(
            read_response(&mut Cursor::new(bytes)),
            Err(IpcError::Malformed(_))
        ));
    }

    #[test]
    fn caller_cap_enforced_for_both_directions() {
        let req = req_bytes(&Request::DecodeHeif {
            id: 1,
            max_pixels: 1,
            data: vec![0; 100],
        });
        assert!(matches!(
            super::read_request(&mut Cursor::new(&req), 115),
            Err(IpcError::TooLong(116))
        ));
        assert!(super::read_request(&mut Cursor::new(&req), 116)
            .unwrap()
            .is_some());
        let resp = resp_bytes(&Response::Image {
            id: 1,
            width: 5,
            height: 5,
            bgra: vec![0; 100],
        });
        assert!(matches!(
            super::read_response(&mut Cursor::new(&resp), 100),
            Err(IpcError::TooLong(116))
        ));
        assert!(super::read_response(&mut Cursor::new(&resp), 116)
            .unwrap()
            .is_some());
        // Cap above MAX_PAYLOAD is clamped to it.
        let bytes = frame(0x81, MAX_PAYLOAD + 1, &[]);
        assert!(matches!(
            super::read_response(&mut Cursor::new(bytes), u64::MAX),
            Err(IpcError::TooLong(_))
        ));
    }

    #[test]
    fn clean_eof_is_none_but_truncation_is_error() {
        assert!(
            super::read_request(&mut Cursor::new(Vec::new()), MAX_PAYLOAD)
                .unwrap()
                .is_none()
        );
        assert!(
            super::read_response(&mut Cursor::new(Vec::new()), MAX_PAYLOAD)
                .unwrap()
                .is_none()
        );
        let mut c = Cursor::new(req_bytes(&Request::Shutdown));
        assert!(super::read_request(&mut c, MAX_PAYLOAD).unwrap().is_some());
        assert!(super::read_request(&mut c, MAX_PAYLOAD).unwrap().is_none());
        for cut in [1, 4, 12] {
            let bytes = req_bytes(&Request::Shutdown);
            match super::read_request(&mut Cursor::new(&bytes[..cut]), MAX_PAYLOAD) {
                Err(IpcError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
                other => panic!("cut {cut}: {other:?}"),
            }
        }
    }

    #[test]
    fn payload_shorter_than_fixed_fields_is_malformed() {
        for p in [&[][..], &[0u8; 15][..]] {
            let bytes = frame(1, p.len() as u64, p);
            assert!(matches!(
                read_request(&mut Cursor::new(bytes)),
                Err(IpcError::Malformed(_))
            ));
        }
        for p in [&[][..], &[0u8; 15][..]] {
            let bytes = frame(0x81, p.len() as u64, p);
            assert!(matches!(
                read_response(&mut Cursor::new(bytes)),
                Err(IpcError::Malformed(_))
            ));
        }
        for p in [&[][..], &[0u8; 8][..], &failed_payload(1, 0, &[0; 4])[..]] {
            let bytes = frame(0x82, p.len() as u64, p);
            assert!(matches!(
                read_response(&mut Cursor::new(bytes)),
                Err(IpcError::Malformed(_))
            ));
        }
    }
}
