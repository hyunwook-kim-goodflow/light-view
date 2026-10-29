//! Writing and reading requests (app to worker); see the crate documentation for the
//! layouts.

use std::io::{self, Read, Write};

use super::frame::{
    expect_consumed, invalid, pixel_bytes, read_exact_vec, read_header, read_u32, read_u64,
    read_u8, write_frame,
};
use super::{
    FitTarget, IpcError, Request, DECODE_FIXED_LEN, KIND_BYTES, KIND_CANT_FIT, KIND_DECODE_HEIF,
    KIND_FAILED, KIND_FITTED, KIND_FIT_WEBP, KIND_IMAGE, KIND_ROTATE_JPEG, KIND_SHUTDOWN,
    KIND_THUMBNAIL_HEIF, MAX_THUMBNAIL_SIDE, REQUEST_FIXED_LEN, ROTATE_FIXED_LEN,
    THUMBNAIL_FIXED_LEN,
};

pub fn write_request(w: &mut impl Write, r: &Request) -> io::Result<()> {
    match r {
        Request::DecodeHeif {
            id,
            max_pixels,
            data,
        } => {
            let len = DECODE_FIXED_LEN
                .checked_add(data.len() as u64)
                .ok_or_else(|| invalid("too long"))?;
            write_frame(w, KIND_DECODE_HEIF, len)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&max_pixels.to_le_bytes())?;
            w.write_all(data)
        }
        Request::RotateJpeg {
            id,
            quarters,
            max_pixels,
            data,
        } => {
            if !(1..=3).contains(quarters) {
                return Err(invalid("quarters must be 1..=3"));
            }
            let len = ROTATE_FIXED_LEN
                .checked_add(data.len() as u64)
                .ok_or_else(|| invalid("too long"))?;
            write_frame(w, KIND_ROTATE_JPEG, len)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&max_pixels.to_le_bytes())?;
            w.write_all(&[*quarters])?;
            w.write_all(data)
        }
        Request::FitWebp {
            id,
            width,
            height,
            start_quality,
            target,
            rgba,
        } => {
            if !(1..=100).contains(start_quality) {
                return Err(invalid("start quality must be 1..=100"));
            }
            let (tag, value, long_edge) = target.to_wire().ok_or_else(|| invalid("bad target"))?;
            if pixel_bytes(*width, *height) != Some(rgba.len() as u64) {
                return Err(invalid("rgba length does not match width*height*4"));
            }
            let len = REQUEST_FIXED_LEN
                .checked_add(rgba.len() as u64)
                .ok_or_else(|| invalid("too long"))?;
            write_frame(w, KIND_FIT_WEBP, len)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&width.to_le_bytes())?;
            w.write_all(&height.to_le_bytes())?;
            w.write_all(&[*start_quality, tag])?;
            w.write_all(&value.to_le_bytes())?;
            w.write_all(&long_edge.to_le_bytes())?;
            w.write_all(rgba)
        }
        Request::ThumbnailHeif {
            id,
            max_side,
            max_pixels,
            data,
        } => {
            if !(1..=MAX_THUMBNAIL_SIDE).contains(max_side) {
                return Err(invalid("max side must be 1..=MAX_THUMBNAIL_SIDE"));
            }
            let len = THUMBNAIL_FIXED_LEN
                .checked_add(data.len() as u64)
                .ok_or_else(|| invalid("too long"))?;
            write_frame(w, KIND_THUMBNAIL_HEIF, len)?;
            w.write_all(&id.to_le_bytes())?;
            w.write_all(&max_pixels.to_le_bytes())?;
            w.write_all(&max_side.to_le_bytes())?;
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
        KIND_ROTATE_JPEG => {
            let id = read_u64(&mut p)?;
            let max_pixels = read_u64(&mut p)?;
            let quarters = read_u8(&mut p)?;
            if !(1..=3).contains(&quarters) {
                return Err(IpcError::Malformed("quarters out of range"));
            }
            let rest = p.limit();
            let data = read_exact_vec(&mut p, rest)?;
            Ok(Some(Request::RotateJpeg {
                id,
                quarters,
                max_pixels,
                data,
            }))
        }
        KIND_FIT_WEBP => {
            let id = read_u64(&mut p)?;
            let width = read_u32(&mut p)?;
            let height = read_u32(&mut p)?;
            let start_quality = read_u8(&mut p)?;
            if !(1..=100).contains(&start_quality) {
                return Err(IpcError::Malformed("start quality out of range"));
            }
            let (tag, value, long_edge) = (read_u8(&mut p)?, read_u64(&mut p)?, read_u32(&mut p)?);
            let target = FitTarget::from_wire(tag, value, long_edge)
                .ok_or(IpcError::Malformed("bad fit target"))?;
            // `p.limit()` is bounded by the validated frame length, so a match bounds the
            // allocation below.
            if pixel_bytes(width, height) != Some(p.limit()) {
                return Err(IpcError::Malformed(
                    "rgba length does not match width*height*4",
                ));
            }
            let rest = p.limit();
            let rgba = read_exact_vec(&mut p, rest)?;
            Ok(Some(Request::FitWebp {
                id,
                width,
                height,
                start_quality,
                target,
                rgba,
            }))
        }
        KIND_THUMBNAIL_HEIF => {
            let id = read_u64(&mut p)?;
            let max_pixels = read_u64(&mut p)?;
            let max_side = read_u32(&mut p)?;
            if !(1..=MAX_THUMBNAIL_SIDE).contains(&max_side) {
                return Err(IpcError::Malformed("thumbnail max side out of range"));
            }
            let rest = p.limit();
            let data = read_exact_vec(&mut p, rest)?;
            Ok(Some(Request::ThumbnailHeif {
                id,
                max_side,
                max_pixels,
                data,
            }))
        }
        KIND_SHUTDOWN => {
            expect_consumed(&p)?;
            Ok(Some(Request::Shutdown))
        }
        KIND_IMAGE | KIND_FAILED | KIND_BYTES | KIND_FITTED | KIND_CANT_FIT => {
            Err(IpcError::Malformed("response kind where request expected"))
        }
        _ => Err(IpcError::Malformed("unknown kind")),
    }
}
