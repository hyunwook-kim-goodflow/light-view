//! Resizing to a target size or a byte budget: the geometry, the in-process encoders
//! (JPEG and PNG; WebP is encoded by the worker) and the fit-to-size search.
//! Pixels in and out are straight-alpha RGBA8 in sRGB, orientation already applied.

mod encode;
mod fit;
mod geometry;
mod meta;
mod quality;

pub use encode::{encode, EncodeError};
pub use fit::{fit, Encoder, FitOutcome};
pub use geometry::{floor_long_edge, resample, target_size};
pub use meta::{attach_exif, exif_overhead, whitelist_exif};
pub use quality::estimate_jpeg_quality;

use crate::decode::Frame;

/// What the result must meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The longer side at most this many pixels.
    LongEdge(u32),
    Width(u32),
    Height(u32),
    /// The file at most `bytes` long, the longer side at most `long_edge` if given.
    MaxBytes {
        bytes: u64,
        long_edge: Option<u32>,
    },
    /// Each side scaled to this percentage.
    Percent(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Jpeg,
    Png,
    WebP,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// The source JPEG's estimated quality; 90 for other sources.
    MatchOriginal,
    Fixed(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeSpec {
    pub target: Target,
    pub format: OutputFormat,
    pub quality: Quality,
    pub strip_gps: bool,
}

/// A decoded frame (sRGB, premultiplied BGRA8) as straight-alpha RGBA8: `c * 255 / a`,
/// rounded; fully transparent pixels become transparent black.
pub fn to_straight_rgba(frame: &Frame) -> Vec<u8> {
    let mut out = frame.bgra.clone();
    for px in out.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        let un = |c: u8| match a {
            0 => 0,
            _ => ((u32::from(c) * 255 + a / 2) / a).min(255) as u8,
        };
        let (b, g, r) = (px[0], px[1], px[2]);
        px[0] = un(r);
        px[1] = un(g);
        px[2] = un(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::premultiply_to_bgra;

    #[test]
    fn straight_rgba_undoes_premultiplication() {
        let rgba = vec![255, 0, 0, 128, 10, 200, 30, 255, 77, 77, 77, 0];
        let frame = Frame {
            bgra: premultiply_to_bgra(rgba),
            delay_ms: 0,
        };
        let back = to_straight_rgba(&frame);
        assert_eq!(back, vec![255, 0, 0, 128, 10, 200, 30, 255, 0, 0, 0, 0]);
    }
}
