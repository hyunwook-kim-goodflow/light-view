//! Pure-Rust image decoding with EXIF orientation applied and size limits enforced.

use std::fmt;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::metadata::Orientation;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader, Limits};

use crate::format::{sniff_format, ImageFormat, SNIFF_LEN};

/// One RGBA8 (straight alpha) frame.
pub struct Frame {
    pub rgba: Vec<u8>,
    pub delay_ms: u32,
}

/// A fully decoded image with orientation already applied.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
    pub icc: Option<Vec<u8>>,
    pub format: ImageFormat,
    pub byte_len: u64,
}

impl fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("frames", &self.frames.len())
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl DecodedImage {
    /// Approximate memory held by the pixel data.
    pub fn memory_bytes(&self) -> u64 {
        self.frames.iter().map(|f| f.rgba.len() as u64).sum()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DecodeLimits {
    pub max_pixels: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_pixels: 268_435_456,
        }
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Unsupported,
    TooLarge { width: u32, height: u32 },
    Corrupt(String),
    Io(std::io::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(f, "unsupported image format"),
            Self::TooLarge { width, height } => write!(f, "image too large ({width}x{height})"),
            Self::Corrupt(msg) => write!(f, "corrupt image: {msg}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Map an `image` error from the decode phase; allocation-limit hits become `TooLarge`.
fn decode_err(e: image::ImageError, width: u32, height: u32) -> DecodeError {
    match e {
        image::ImageError::Limits(_) => DecodeError::TooLarge { width, height },
        e => corrupt(e),
    }
}

fn corrupt(e: impl fmt::Display) -> DecodeError {
    DecodeError::Corrupt(e.to_string())
}

/// Read EXIF orientation (1..=8) from a JPEG/WebP container; 1 if absent or invalid.
pub fn exif_orientation(bytes: &[u8]) -> u16 {
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) else {
        return 1;
    };
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .filter(|v| (1..=8).contains(v))
        .map_or(1, |v| v as u16)
}

pub fn decode_file(path: &Path, limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    let bytes = std::fs::read(path)?;
    decode_bytes(&bytes, limits)
}

pub fn decode_bytes(bytes: &[u8], limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    catch_unwind(AssertUnwindSafe(|| decode_inner(bytes, limits)))
        .unwrap_or_else(|_| Err(DecodeError::Corrupt("decoder panicked".into())))
}

fn to_image_format(f: ImageFormat) -> Option<image::ImageFormat> {
    match f {
        ImageFormat::Jpeg => Some(image::ImageFormat::Jpeg),
        ImageFormat::Png => Some(image::ImageFormat::Png),
        ImageFormat::Gif => Some(image::ImageFormat::Gif),
        ImageFormat::WebP => Some(image::ImageFormat::WebP),
        ImageFormat::Bmp => Some(image::ImageFormat::Bmp),
        ImageFormat::Heif => None,
    }
}

fn delay_ms(frame: &image::Frame) -> u32 {
    let (num, den) = frame.delay().numer_denom_ms();
    let ms = num.checked_div(den).unwrap_or(0);
    if ms < 10 {
        100
    } else {
        ms
    }
}

fn image_limits(l: &DecodeLimits) -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(l.max_pixels.saturating_mul(4));
    limits
}

fn decode_inner(bytes: &[u8], limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    let head = &bytes[..bytes.len().min(SNIFF_LEN)];
    let format = sniff_format(head).ok_or(DecodeError::Unsupported)?;
    let img_format = to_image_format(format).ok_or(DecodeError::Unsupported)?;

    let (width, height) = ImageReader::with_format(Cursor::new(bytes), img_format)
        .into_dimensions()
        .map_err(corrupt)?;
    if u64::from(width) * u64::from(height) > limits.max_pixels {
        return Err(DecodeError::TooLarge { width, height });
    }

    let byte_len = bytes.len() as u64;
    let finish = |frames: Vec<Frame>, w: u32, h: u32, icc: Option<Vec<u8>>| DecodedImage {
        width: w,
        height: h,
        frames,
        icc,
        format,
        byte_len,
    };

    match format {
        ImageFormat::Gif => {
            let mut dec = GifDecoder::new(Cursor::new(bytes)).map_err(corrupt)?;
            dec.set_limits(image_limits(limits)).map_err(corrupt)?;
            let icc = dec.icc_profile().map_err(corrupt)?;
            let frames = collect_frames(dec.into_frames(), limits, width, height)?;
            Ok(finish(frames, width, height, icc))
        }
        ImageFormat::WebP => {
            let mut dec = WebPDecoder::new(Cursor::new(bytes)).map_err(corrupt)?;
            dec.set_limits(image_limits(limits)).map_err(corrupt)?;
            let icc = dec.icc_profile().map_err(corrupt)?;
            if dec.has_animation() {
                let frames = collect_frames(dec.into_frames(), limits, width, height)?;
                let (frames, w, h) = orient_frames(frames, width, height, exif_orientation(bytes));
                Ok(finish(frames, w, h, icc))
            } else {
                decode_still(dec, bytes, icc, finish)
            }
        }
        _ => {
            let mut reader = ImageReader::with_format(Cursor::new(bytes), img_format);
            reader.limits(image_limits(limits));
            let mut dec = reader.into_decoder().map_err(corrupt)?;
            let icc = dec.icc_profile().map_err(corrupt)?;
            decode_still(dec, bytes, icc, finish)
        }
    }
}

/// Apply an EXIF orientation to every frame; width/height swap for orientations 5-8.
fn orient_frames(frames: Vec<Frame>, w: u32, h: u32, orientation: u16) -> (Vec<Frame>, u32, u32) {
    let Some(o) =
        Orientation::from_exif(orientation as u8).filter(|o| *o != Orientation::NoTransforms)
    else {
        return (frames, w, h);
    };
    let mut out_dims = (w, h);
    let frames = frames
        .into_iter()
        .map(|f| {
            let Some(buf) = image::RgbaImage::from_raw(w, h, f.rgba) else {
                unreachable!("frame buffer matches canvas size")
            };
            let mut img = DynamicImage::ImageRgba8(buf);
            img.apply_orientation(o);
            out_dims = (img.width(), img.height());
            Frame {
                rgba: img.into_rgba8().into_raw(),
                delay_ms: f.delay_ms,
            }
        })
        .collect();
    (frames, out_dims.0, out_dims.1)
}

/// Collect animation frames; total decoded bytes are capped at `max_pixels * 4`.
fn collect_frames(
    frames: image::Frames<'_>,
    limits: &DecodeLimits,
    width: u32,
    height: u32,
) -> Result<Vec<Frame>, DecodeError> {
    let budget = limits.max_pixels.saturating_mul(4);
    let mut used: u64 = 0;
    let mut out = Vec::new();
    for f in frames {
        let f = f.map_err(|e| decode_err(e, width, height))?;
        used = used.saturating_add(f.buffer().as_raw().len() as u64);
        if used > budget {
            return Err(DecodeError::TooLarge { width, height });
        }
        let delay_ms = delay_ms(&f);
        out.push(Frame {
            rgba: f.into_buffer().into_raw(),
            delay_ms,
        });
    }
    if out.is_empty() {
        return Err(DecodeError::Corrupt("no frames".into()));
    }
    Ok(out)
}

/// Decode a still image and apply EXIF orientation (JPEG and WebP only, read from the container).
fn decode_still(
    dec: impl ImageDecoder,
    bytes: &[u8],
    icc: Option<Vec<u8>>,
    finish: impl FnOnce(Vec<Frame>, u32, u32, Option<Vec<u8>>) -> DecodedImage,
) -> Result<DecodedImage, DecodeError> {
    let (w0, h0) = dec.dimensions();
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| decode_err(e, w0, h0))?;
    if matches!(
        sniff_format(bytes),
        Some(ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        if let Some(o) = Orientation::from_exif(exif_orientation(bytes) as u8) {
            img.apply_orientation(o);
        }
    }
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(finish(
        vec![Frame {
            rgba: rgba.into_raw(),
            delay_ms: 0,
        }],
        w,
        h,
        icc,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures as fixtures;

    #[test]
    fn decodes_png_rgba() {
        let img = decode_bytes(&fixtures::png_2x1(), &DecodeLimits::default()).unwrap();
        assert_eq!((img.width, img.height, img.frames.len()), (2, 1, 1));
        assert_eq!(&img.frames[0].rgba[0..4], &[255, 0, 0, 255]);
    }
    #[test]
    fn applies_exif_orientation_6() {
        let img = decode_bytes(
            &fixtures::jpeg_16x8_with_orientation(6),
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!((img.width, img.height), (8, 16));
        let i = ((2 * img.width + 4) * 4) as usize;
        let top = &img.frames[0].rgba[i..i + 3];
        assert!(
            top[0] > 200 && top[2] < 60,
            "top pixel should be red, got {top:?}"
        );
    }
    #[test]
    fn animated_gif_keeps_frames_and_delays() {
        let img = decode_bytes(&fixtures::gif_two_frames(70), &DecodeLimits::default()).unwrap();
        assert_eq!(img.frames.len(), 2);
        assert_eq!(img.frames[1].delay_ms, 70);
    }
    #[test]
    fn truncated_jpeg_is_corrupt_not_panic() {
        let mut b = fixtures::jpeg_16x8_with_orientation(1);
        b.truncate(b.len() / 2);
        assert!(matches!(
            decode_bytes(&b, &DecodeLimits::default()),
            Err(DecodeError::Corrupt(_))
        ));
    }
    #[test]
    fn oversized_header_rejected_before_alloc() {
        let b = fixtures::png_header_only(100_000, 100_000);
        match decode_bytes(&b, &DecodeLimits::default()) {
            Err(DecodeError::TooLarge { width, height }) => {
                assert_eq!((width, height), (100_000, 100_000))
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }
    #[test]
    fn heif_is_unsupported_in_m1() {
        assert!(matches!(
            decode_bytes(
                b"\0\0\0\x18ftypheic\0\0\0\0mif1heic",
                &DecodeLimits::default()
            ),
            Err(DecodeError::Unsupported)
        ));
    }
    #[test]
    fn garbage_is_unsupported() {
        assert!(matches!(
            decode_bytes(b"hello world, not an image", &DecodeLimits::default()),
            Err(DecodeError::Unsupported)
        ));
    }
    #[test]
    fn animation_total_frames_budget_enforced() {
        let gif = fixtures::gif_n_frames(10, 8);
        // One 8x8 frame (64 px) fits in 100 px; ten do not.
        let tight = DecodeLimits { max_pixels: 100 };
        assert!(matches!(
            decode_bytes(&gif, &tight),
            Err(DecodeError::TooLarge {
                width: 8,
                height: 8
            })
        ));
        assert_eq!(
            decode_bytes(&gif, &DecodeLimits::default())
                .unwrap()
                .frames
                .len(),
            10
        );
    }
    #[test]
    fn orient_frames_rotates_each_frame_and_swaps_dims() {
        // 2x1 frames: [red, blue]; orientation 6 (rotate 90 cw) -> 1x2, red on top.
        let px = [255, 0, 0, 255, 0, 0, 255, 255];
        let frames = vec![
            Frame {
                rgba: px.to_vec(),
                delay_ms: 10,
            },
            Frame {
                rgba: px.to_vec(),
                delay_ms: 20,
            },
        ];
        let (out, w, h) = orient_frames(frames, 2, 1, 6);
        assert_eq!((w, h, out.len()), (1, 2, 2));
        for f in &out {
            assert_eq!(&f.rgba[0..4], &[255, 0, 0, 255]);
            assert_eq!(&f.rgba[4..8], &[0, 0, 255, 255]);
        }
        assert_eq!(out[1].delay_ms, 20);
    }
    #[test]
    fn exif_orientation_defaults_to_1() {
        assert_eq!(exif_orientation(b"nope"), 1);
        assert_eq!(exif_orientation(&fixtures::png_2x1()), 1);
        assert_eq!(
            exif_orientation(&fixtures::jpeg_16x8_with_orientation(8)),
            8
        );
    }
}
