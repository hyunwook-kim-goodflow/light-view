//! Frame headers and the fixed-field readers every message kind uses.

use std::io::{self, Read, Write};

use crate::{IpcError, MAGIC, MAX_PAYLOAD};

pub(crate) fn check_len(len: u64, max_payload: u64) -> Result<(), IpcError> {
    if len > max_payload.min(MAX_PAYLOAD) {
        Err(IpcError::TooLong(len))
    } else {
        Ok(())
    }
}

pub(crate) fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

pub(crate) fn write_frame(w: &mut impl Write, kind: u8, len: u64) -> io::Result<()> {
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
pub(crate) fn read_header(
    r: &mut impl Read,
    max_payload: u64,
) -> Result<Option<(u8, u64)>, IpcError> {
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

pub(crate) type Payload<'a, R> = io::Take<&'a mut R>;

/// Reads `N` fixed bytes; a payload too short for them is `Malformed`, not an I/O error.
pub(crate) fn read_fixed<const N: usize, R: Read>(
    p: &mut Payload<'_, R>,
) -> Result<[u8; N], IpcError> {
    if p.limit() < N as u64 {
        return Err(IpcError::Malformed("payload shorter than fixed fields"));
    }
    let mut b = [0u8; N];
    p.read_exact(&mut b)?;
    Ok(b)
}

pub(crate) fn read_u8<R: Read>(p: &mut Payload<'_, R>) -> Result<u8, IpcError> {
    Ok(read_fixed::<1, R>(p)?[0])
}

pub(crate) fn read_u32<R: Read>(p: &mut Payload<'_, R>) -> Result<u32, IpcError> {
    Ok(u32::from_le_bytes(read_fixed(p)?))
}

pub(crate) fn read_u64<R: Read>(p: &mut Payload<'_, R>) -> Result<u64, IpcError> {
    Ok(u64::from_le_bytes(read_fixed(p)?))
}

/// Reads exactly `n` bytes into a buffer allocated once (`n` must already be bounded by the
/// validated frame length). Allocation failure is `Io(OutOfMemory)`.
pub(crate) fn read_exact_vec<R: Read>(p: &mut Payload<'_, R>, n: u64) -> Result<Vec<u8>, IpcError> {
    let n = usize::try_from(n).map_err(|_| IpcError::Io(io::ErrorKind::OutOfMemory.into()))?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(n)
        .map_err(|_| IpcError::Io(io::ErrorKind::OutOfMemory.into()))?;
    buf.resize(n, 0);
    p.read_exact(&mut buf)?;
    Ok(buf)
}

/// `width * height * 4` for a non-empty image; `None` for a zero side or an overflow.
pub(crate) fn pixel_bytes(width: u32, height: u32) -> Option<u64> {
    if width == 0 || height == 0 {
        return None;
    }
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)
}

pub(crate) fn expect_consumed<R: Read>(p: &Payload<'_, R>) -> Result<(), IpcError> {
    if p.limit() != 0 {
        return Err(IpcError::Malformed("trailing bytes in payload"));
    }
    Ok(())
}
