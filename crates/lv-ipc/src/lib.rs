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
//!
//! Responses (worker to app):
//! - `0x81` `Image`: `id: u64 | width: u32 | height: u32 | bgra` (the rest of the payload).
//!   `width` and `height` must be non-zero and `width * height * 4` (checked u64) must equal
//!   `bgra.len()`.
//! - `0x82` `Failed`: `id: u64 | tag: u8 | fields`, with tag `0` `TooLarge`
//!   (`width: u32 | height: u32`), tag `1` `Corrupt` (string), tag `2` `Unsupported` (none).
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

use std::io::{self, Read, Write};

pub const MAGIC: [u8; 4] = *b"LVW1";
pub const MAX_PAYLOAD: u64 = 1 << 30;
/// Maximum byte length of a `Corrupt` message on the wire.
pub const MAX_CORRUPT_LEN: usize = 4096;
/// Fixed fields in front of a `DecodeHeif` request's file bytes (`id`, `max_pixels`).
pub const REQUEST_FIXED_LEN: u64 = 16;
/// Fixed fields in front of an `Image` response's pixels (`id`, `width`, `height`).
pub const IMAGE_FIXED_LEN: u64 = 16;
/// The most file bytes one `DecodeHeif` request can carry.
pub const MAX_REQUEST_DATA: u64 = MAX_PAYLOAD - REQUEST_FIXED_LEN;
/// The most pixels one `Image` response can carry (4 bytes each).
pub const MAX_RESPONSE_PIXELS: u64 = (MAX_PAYLOAD - IMAGE_FIXED_LEN) / 4;

const MIB: u64 = 1024 * 1024;
/// Floor of [`decoder_memory`].
const MIN_DECODER_MEMORY: u64 = 256 * MIB;

/// The most file bytes the app sends and the worker accepts in one request: the app's file
/// size cap (`lv_core::decode::max_file_bytes`), clamped to what one frame can carry. Both
/// sides use this one rule, so a file the app sends is never one the worker refuses unread.
pub fn max_request_data(file_cap: u64) -> u64 {
    file_cap.min(MAX_REQUEST_DATA)
}

/// The pixel limit a worker can honour: `max_pixels`, clamped so that the decoded image
/// still fits in one `Image` response. A larger image is reported as too large instead.
pub fn max_response_pixels(max_pixels: u64) -> u64 {
    max_pixels.min(MAX_RESPONSE_PIXELS)
}

/// Memory libheif may allocate for one decode of at most `max_pixels` pixels (its
/// `max_total_memory`): twice the RGBA output (YCbCr planes, the RGB(A) image and conversion
/// scratch), at least 256 MiB so that small limits still take ordinary files.
pub fn decoder_memory(max_pixels: u64) -> u64 {
    max_pixels.saturating_mul(8).max(MIN_DECODER_MEMORY)
}

/// The worker's Job memory cap: what libheif may allocate ([`decoder_memory`]) plus the
/// worker's own RGBA copy of the image (4 bytes per pixel), so libheif reaching its own
/// limit is an error it reports, never the Job killing the worker. This is
/// `max(12 * max_pixels, 256 MiB + 4 * max_pixels)`.
pub fn worker_memory_limit(max_pixels: u64) -> u64 {
    decoder_memory(max_pixels).saturating_add(max_pixels.saturating_mul(4))
}

const KIND_DECODE_HEIF: u8 = 0x01;
const KIND_SHUTDOWN: u8 = 0x02;
const KIND_IMAGE: u8 = 0x81;
const KIND_FAILED: u8 = 0x82;

const TAG_TOO_LARGE: u8 = 0;
const TAG_CORRUPT: u8 = 1;
const TAG_UNSUPPORTED: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    DecodeHeif {
        id: u64,
        max_pixels: u64,
        data: Vec<u8>,
    },
    Shutdown,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailKind {
    TooLarge { width: u32, height: u32 },
    Corrupt(String),
    Unsupported,
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

fn check_len(len: u64, max_payload: u64) -> Result<(), IpcError> {
    if len > max_payload.min(MAX_PAYLOAD) {
        Err(IpcError::TooLong(len))
    } else {
        Ok(())
    }
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn write_frame(w: &mut impl Write, kind: u8, len: u64) -> io::Result<()> {
    if len > MAX_PAYLOAD {
        return Err(invalid("payload exceeds MAX_PAYLOAD"));
    }
    let mut header = [0u8; 13];
    header[..4].copy_from_slice(&MAGIC);
    header[4] = kind;
    header[5..].copy_from_slice(&len.to_le_bytes());
    w.write_all(&header)
}

/// Reads and validates the frame header; returns `None` on a clean EOF (zero bytes of the
/// next header available), `(kind, len)` otherwise.
fn read_header(r: &mut impl Read, max_payload: u64) -> Result<Option<(u8, u64)>, IpcError> {
    let mut header = [0u8; 13];
    let mut filled = 0;
    while filled < header.len() {
        match r.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(IpcError::Io(io::ErrorKind::UnexpectedEof.into())),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    if header[..4] != MAGIC {
        return Err(IpcError::BadMagic);
    }
    let len = u64::from_le_bytes(header[5..].try_into().expect("8 bytes"));
    check_len(len, max_payload)?;
    Ok(Some((header[4], len)))
}

type Payload<'a, R> = io::Take<&'a mut R>;

/// Reads `N` fixed bytes; a payload too short for them is `Malformed`, not an I/O error.
fn read_fixed<const N: usize, R: Read>(p: &mut Payload<'_, R>) -> Result<[u8; N], IpcError> {
    if p.limit() < N as u64 {
        return Err(IpcError::Malformed("payload shorter than fixed fields"));
    }
    let mut b = [0u8; N];
    p.read_exact(&mut b)?;
    Ok(b)
}

fn read_u8<R: Read>(p: &mut Payload<'_, R>) -> Result<u8, IpcError> {
    Ok(read_fixed::<1, R>(p)?[0])
}

fn read_u32<R: Read>(p: &mut Payload<'_, R>) -> Result<u32, IpcError> {
    Ok(u32::from_le_bytes(read_fixed(p)?))
}

fn read_u64<R: Read>(p: &mut Payload<'_, R>) -> Result<u64, IpcError> {
    Ok(u64::from_le_bytes(read_fixed(p)?))
}

/// Reads exactly `n` bytes into a buffer allocated once (`n` must already be bounded by the
/// validated frame length). Allocation failure is `Io(OutOfMemory)`.
fn read_exact_vec<R: Read>(p: &mut Payload<'_, R>, n: u64) -> Result<Vec<u8>, IpcError> {
    let n = usize::try_from(n).map_err(|_| IpcError::Io(io::ErrorKind::OutOfMemory.into()))?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(n)
        .map_err(|_| IpcError::Io(io::ErrorKind::OutOfMemory.into()))?;
    buf.resize(n, 0);
    p.read_exact(&mut buf)?;
    Ok(buf)
}

fn expect_consumed<R: Read>(p: &Payload<'_, R>) -> Result<(), IpcError> {
    if p.limit() != 0 {
        return Err(IpcError::Malformed("trailing bytes in payload"));
    }
    Ok(())
}

pub fn write_request(w: &mut impl Write, r: &Request) -> io::Result<()> {
    match r {
        Request::DecodeHeif {
            id,
            max_pixels,
            data,
        } => {
            let len = REQUEST_FIXED_LEN
                .checked_add(data.len() as u64)
                .ok_or_else(|| invalid("too long"))?;
            write_frame(w, KIND_DECODE_HEIF, len)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&max_pixels.to_le_bytes())?;
            w.write_all(data)
        }
        Request::Shutdown => write_frame(w, KIND_SHUTDOWN, 0),
    }
}

/// Reads one request; `Ok(None)` means clean EOF at a frame boundary.
pub fn read_request(r: &mut impl Read, max_payload: u64) -> Result<Option<Request>, IpcError> {
    let Some((kind, len)) = read_header(r, max_payload)? else {
        return Ok(None);
    };
    let mut p = r.by_ref().take(len);
    match kind {
        KIND_DECODE_HEIF => {
            let id = read_u64(&mut p)?;
            let max_pixels = read_u64(&mut p)?;
            let rest = p.limit();
            let data = read_exact_vec(&mut p, rest)?;
            Ok(Some(Request::DecodeHeif {
                id,
                max_pixels,
                data,
            }))
        }
        KIND_SHUTDOWN => {
            expect_consumed(&p)?;
            Ok(Some(Request::Shutdown))
        }
        KIND_IMAGE | KIND_FAILED => {
            Err(IpcError::Malformed("response kind where request expected"))
        }
        _ => Err(IpcError::Malformed("unknown kind")),
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
            }
            write_frame(w, KIND_FAILED, p.len() as u64)?;
            w.write_all(&p)
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
                _ => return Err(IpcError::Malformed("unknown fail tag")),
            };
            expect_consumed(&p)?;
            Ok(Some(Response::Failed { id, kind }))
        }
        KIND_DECODE_HEIF | KIND_SHUTDOWN => {
            Err(IpcError::Malformed("request kind where response expected"))
        }
        _ => Err(IpcError::Malformed("unknown kind")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
        let data = vec![0u8; (MAX_REQUEST_DATA + 1) as usize];
        let request = Request::DecodeHeif {
            id: 1,
            max_pixels: 1,
            data,
        };
        let mut out = Vec::new();
        let err = write_request(&mut out, &request).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(out.is_empty(), "nothing may be written before the error");
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
        assert!(check_len(MAX_PAYLOAD, MAX_PAYLOAD).is_ok());
        assert!(check_len(0, MAX_PAYLOAD).is_ok());
        assert!(matches!(
            check_len(MAX_PAYLOAD + 1, u64::MAX),
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
