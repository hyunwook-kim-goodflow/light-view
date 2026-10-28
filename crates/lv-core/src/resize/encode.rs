//! In-process encoders: JPEG and PNG (pure Rust). WebP is encoded by the worker.

use std::fmt;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use super::OutputFormat;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The format is encoded by the worker (WebP), not in-process.
    NeedsWorker,
    Failed(String),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsWorker => write!(f, "format is encoded by the worker"),
            Self::Failed(msg) => write!(f, "encoding failed: {msg}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encodes straight-alpha RGBA8 of `size`. JPEG composites alpha onto white and uses
/// `quality` (clamped to 1..=100); PNG keeps alpha and ignores `quality`. The same input
/// always gives the same bytes.
pub fn encode(
    rgba: &[u8],
    size: (u32, u32),
    format: OutputFormat,
    quality: u8,
) -> Result<Vec<u8>, EncodeError> {
    let (w, h) = size;
    let expected = u64::from(w) * u64::from(h) * 4;
    if format == OutputFormat::WebP {
        return Err(EncodeError::NeedsWorker);
    }
    if w == 0 || h == 0 || rgba.len() as u64 != expected {
        return Err(EncodeError::Failed(format!(
            "{} bytes for {w}x{h} RGBA",
            rgba.len()
        )));
    }
    let failed = |e: image::ImageError| EncodeError::Failed(e.to_string());
    let mut out = Vec::new();
    match format {
        OutputFormat::Jpeg => {
            let rgb = flatten_on_white(rgba);
            JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
                .write_image(&rgb, w, h, ExtendedColorType::Rgb8)
                .map_err(failed)?;
        }
        OutputFormat::Png => {
            PngEncoder::new(&mut out)
                .write_image(rgba, w, h, ExtendedColorType::Rgba8)
                .map_err(failed)?;
        }
        OutputFormat::WebP => unreachable!("handled above"),
    }
    Ok(out)
}

/// Straight RGBA8 composited onto white, as RGB8: `c * a / 255 + 255 - a`, rounded.
fn flatten_on_white(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|p| {
            let a = u32::from(p[3]);
            let f = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
            [f(p[0]), f(p[1]), f(p[2])]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{decode_bytes, DecodeLimits};
    use crate::resize::to_straight_rgba;
    use crate::test_fixtures as fixtures;

    use fixtures::rgba_noise as noise;

    #[test]
    fn alpha_flattens_on_white_for_jpeg() {
        let png = fixtures::png_solid(2, 2, [0, 0, 0, 0]);
        let img = decode_bytes(&png, &DecodeLimits::default()).unwrap();
        let rgba = to_straight_rgba(&img.frames[0]);
        let jpeg = encode(&rgba, (2, 2), OutputFormat::Jpeg, 90).unwrap();
        let back = decode_bytes(&jpeg, &DecodeLimits::default()).unwrap();
        assert_eq!((back.width, back.height), (2, 2));
        for px in back.frames[0].bgra.chunks_exact(4) {
            assert!(px[..3].iter().all(|&c| c >= 250), "{px:?}");
        }
    }

    #[test]
    fn png_keeps_alpha() {
        let rgba = vec![255, 0, 0, 128, 0, 255, 0, 0];
        let png = encode(&rgba, (2, 1), OutputFormat::Png, 0).unwrap();
        let back = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(back.into_raw(), rgba);
    }

    #[test]
    fn webp_needs_the_worker_and_bad_input_fails() {
        let rgba = vec![0; 4];
        assert_eq!(
            encode(&rgba, (1, 1), OutputFormat::WebP, 80),
            Err(EncodeError::NeedsWorker)
        );
        assert!(matches!(
            encode(&rgba, (2, 1), OutputFormat::Jpeg, 80),
            Err(EncodeError::Failed(_))
        ));
    }

    #[test]
    fn encoding_is_deterministic() {
        let rgba = noise(300, 200, 7);
        for format in [OutputFormat::Jpeg, OutputFormat::Png] {
            let a = encode(&rgba, (300, 200), format, 75).unwrap();
            let b = encode(&rgba, (300, 200), format, 75).unwrap();
            assert_eq!(a, b, "{format:?}");
        }
    }

    #[test]
    fn lower_quality_is_smaller() {
        let rgba = noise(200, 200, 3);
        let hi = encode(&rgba, (200, 200), OutputFormat::Jpeg, 95).unwrap();
        let lo = encode(&rgba, (200, 200), OutputFormat::Jpeg, 60).unwrap();
        assert!(lo.len() < hi.len());
    }
}
