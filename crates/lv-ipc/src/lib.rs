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

fn check_len(len: u64) -> Result<(), IpcError> {
    if len > MAX_PAYLOAD {
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

/// Reads and validates the frame header; returns `(kind, len)`.
fn read_header(r: &mut impl Read) -> Result<(u8, u64), IpcError> {
    let mut header = [0u8; 13];
    r.read_exact(&mut header)?;
    if header[..4] != MAGIC {
        return Err(IpcError::BadMagic);
    }
    let len = u64::from_le_bytes(header[5..].try_into().expect("8 bytes"));
    check_len(len)?;
    Ok((header[4], len))
}

fn read_u8(r: &mut impl Read) -> Result<u8, IpcError> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u32(r: &mut impl Read) -> Result<u32, IpcError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> Result<u64, IpcError> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Reads exactly `n` bytes, allocating only as data actually arrives.
fn read_exact_vec(r: &mut impl Read, n: u64) -> Result<Vec<u8>, IpcError> {
    let mut buf = Vec::new();
    let got = r.take(n).read_to_end(&mut buf)?;
    if got as u64 != n {
        return Err(IpcError::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    Ok(buf)
}

fn expect_consumed(payload: &io::Take<&mut impl Read>) -> Result<(), IpcError> {
    if payload.limit() != 0 {
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
            let len = 16u64
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

pub fn read_request(r: &mut impl Read) -> Result<Request, IpcError> {
    let (kind, len) = read_header(r)?;
    let mut p = r.by_ref().take(len);
    match kind {
        KIND_DECODE_HEIF => {
            let id = read_u64(&mut p)?;
            let max_pixels = read_u64(&mut p)?;
            let rest = p.limit();
            let data = read_exact_vec(&mut p, rest)?;
            Ok(Request::DecodeHeif {
                id,
                max_pixels,
                data,
            })
        }
        KIND_SHUTDOWN => {
            expect_consumed(&p)?;
            Ok(Request::Shutdown)
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
            write_frame(w, KIND_IMAGE, 16 + expected)?;
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

pub fn read_response(r: &mut impl Read) -> Result<Response, IpcError> {
    let (kind, len) = read_header(r)?;
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
            Ok(Response::Image {
                id,
                width,
                height,
                bgra,
            })
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
                        return Err(IpcError::Io(io::ErrorKind::UnexpectedEof.into()));
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
            Ok(Response::Failed { id, kind })
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
        assert!(check_len(MAX_PAYLOAD).is_ok());
        assert!(check_len(0).is_ok());
        assert!(matches!(
            check_len(MAX_PAYLOAD + 1),
            Err(IpcError::TooLong(_))
        ));
        // A frame of exactly MAX_PAYLOAD passes the header check and only then hits EOF.
        let bytes = frame(1, MAX_PAYLOAD, &[0; 16]);
        match read_request(&mut Cursor::new(bytes)) {
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
}
