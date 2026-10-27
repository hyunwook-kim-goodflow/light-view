//! Opening a JPEG over the pixel limit at 1/2, 1/4 or 1/8 size (ruling R1), with
//! `jpeg-decoder`'s DCT scaling, so the full-size pixels are never held.
//!
//! Only Huffman-coded sequential (SOF0, SOF1) and progressive (SOF2) frames are scaled.
//! A progressive frame keeps every coefficient of the full-size image while it decodes,
//! so it is scaled only when those fit in the same `max_pixels * 4` bytes the full
//! decode may allocate; otherwise, like anything over the limit even at 1/8, it is
//! `TooLarge`.
//!
//! `jpeg-decoder` decodes every block of the full-size image (zero-filling what the file
//! lacks) and cannot be cancelled, so a header claiming a huge image over a few bytes of
//! data would keep a decoding thread busy for many seconds (ruling R9). So, before
//! decoding: a source over `MAX_SOURCE_PIXELS` (32768², about 7 s at worst) is
//! `TooLarge`, and a file whose entropy-coded data (the bytes of its scans, not its
//! metadata, tables or anything after EOI) has fewer bits than the frame has blocks
//! (every block takes at least one bit) is `Corrupt`.

use std::io::Cursor;

use jpeg_decoder::{Decoder, PixelFormat};

use super::{corrupt, DecodeError, DecodeLimits, RgbaFrame};
use crate::jpeg_markers::{is_frame_header, skip_entropy_data, Markers, SOS};

/// The largest source a scaled decode takes on (R9): 32768 x 32768.
const MAX_SOURCE_PIXELS: u64 = 1 << 30;

/// The denominators `jpeg-decoder` can scale by.
const DENOMINATORS: [u8; 4] = [1, 2, 4, 8];

/// The smallest denominator (1, 2, 4 or 8) whose scaled image, each side rounded up,
/// has at most `max_pixels` pixels; `None` if even 1/8 has more.
pub fn pick_scale(width: u32, height: u32, max_pixels: u64) -> Option<u8> {
    DENOMINATORS.into_iter().find(|&d| {
        let side = |len: u32| u64::from(len.div_ceil(u32::from(d)));
        side(width) * side(height) <= max_pixels
    })
}

/// A JPEG decoded at reduced size: straight-alpha RGBA8 in the source color space,
/// unoriented, with its ICC profile.
pub(super) struct Scaled {
    pub frame: RgbaFrame,
    pub width: u32,
    pub height: u32,
    pub icc: Option<Vec<u8>>,
}

/// Decodes the `width`x`height` JPEG in `bytes`, which is over `limits.max_pixels`, at
/// the largest size within it.
pub(super) fn decode(
    bytes: &[u8],
    width: u32,
    height: u32,
    limits: &DecodeLimits,
) -> Result<Scaled, DecodeError> {
    let too_large = || DecodeError::TooLarge { width, height };
    // Over 64 times the limit cannot fit even at 1/8; over `MAX_SOURCE_PIXELS` takes
    // too long to decode (R9).
    let source_cap = limits.max_pixels.saturating_mul(64).min(MAX_SOURCE_PIXELS);
    if u64::from(width) * u64::from(height) > source_cap {
        return Err(too_large());
    }
    let denom = pick_scale(width, height, limits.max_pixels).ok_or_else(too_large)?;
    let alloc_cap = limits.max_pixels.saturating_mul(4);
    check_frame(bytes, alloc_cap).map_err(|e| e.unwrap_or_else(too_large))?;

    let side = |len: u32| u16::try_from(len.div_ceil(u32::from(denom)));
    let (Ok(want_w), Ok(want_h)) = (side(width), side(height)) else {
        return Err(too_large());
    };
    let mut dec = Decoder::new(Cursor::new(bytes));
    // Bounds only the final output; `check_frame` and `pick_scale` are the real guards.
    dec.set_max_decoding_buffer_size(usize::try_from(alloc_cap).unwrap_or(usize::MAX));
    let (w, h) = dec.scale(want_w, want_h).map_err(corrupt)?;
    let (w, h) = (u32::from(w), u32::from(h));
    if u64::from(w) * u64::from(h) > limits.max_pixels {
        return Err(too_large());
    }
    let pixels = dec.decode().map_err(corrupt)?;
    let info = dec.info().ok_or_else(|| corrupt("no frame"))?;
    let rgba = to_rgba(&pixels, info.pixel_format, w as usize * h as usize)?;
    Ok(Scaled {
        frame: RgbaFrame { rgba, delay_ms: 0 },
        width: w,
        height: h,
        icc: dec.icc_profile(),
    })
}

/// Checks the first frame header: `Err(None)` if it cannot be scaled within
/// `alloc_cap` bytes, `Err(Some(Corrupt))` if there is none, it is malformed, or the
/// file is too small for its blocks.
fn check_frame(bytes: &[u8], alloc_cap: u64) -> Result<(), Option<DecodeError>> {
    let frame = Markers::new(bytes)
        .map_while(Result::ok)
        .find(|m| is_frame_header(m.code))
        .ok_or_else(|| Some(corrupt("no frame header")))?;
    let progressive = match frame.code {
        0xC0 | 0xC1 => false,
        0xC2 => true,
        _ => return Err(None),
    };
    let blocks = frame_blocks(frame.payload).ok_or_else(|| Some(corrupt("bad frame header")))?;
    if entropy_bytes(bytes).saturating_mul(8) < blocks {
        return Err(Some(corrupt("too little data for the image size")));
    }
    if progressive && blocks.saturating_mul(COEFFICIENT_BYTES) > alloc_cap {
        return Err(None);
    }
    Ok(())
}

/// The bytes of entropy-coded data in all scans up to EOI: from each SOS header's end to
/// the next marker that is not `RSTn` or a stuffed `FF00` (or the end of the file).
fn entropy_bytes(bytes: &[u8]) -> u64 {
    Markers::new(bytes)
        .map_while(Result::ok)
        .filter(|m| m.code == SOS)
        .map(|m| {
            let next = skip_entropy_data(bytes, m.end).unwrap_or(bytes.len());
            next.saturating_sub(m.end) as u64
        })
        .sum()
}

/// The bytes a progressive decode holds per block: 64 `i16` coefficients.
const COEFFICIENT_BYTES: u64 = 64 * 2;

/// The blocks of the frame whose SOF payload this is: per component, its blocks over the
/// MCU-padded image. `None` if the header is malformed.
fn frame_blocks(sof: &[u8]) -> Option<u64> {
    let height = u64::from(u16::from_be_bytes([*sof.get(1)?, *sof.get(2)?]));
    let width = u64::from(u16::from_be_bytes([*sof.get(3)?, *sof.get(4)?]));
    let count = usize::from(*sof.get(5)?);
    let sampling: Vec<(u64, u64)> = sof
        .get(6..6 + 3 * count)?
        .chunks_exact(3)
        .map(|c| (u64::from(c[1] >> 4), u64::from(c[1] & 0xF)))
        .collect();
    let h_max = sampling.iter().map(|s| s.0).max()?;
    let v_max = sampling.iter().map(|s| s.1).max()?;
    if h_max == 0 || v_max == 0 || sampling.iter().any(|s| s.0 == 0 || s.1 == 0) {
        return None;
    }
    let mcus_x = width.div_ceil(8 * h_max);
    let mcus_y = height.div_ceil(8 * v_max);
    Some(sampling.iter().map(|(h, v)| mcus_x * h * mcus_y * v).sum())
}

/// Expands `jpeg-decoder` output of `pixels` pixels to RGBA8.
fn to_rgba(data: &[u8], format: PixelFormat, pixels: usize) -> Result<Vec<u8>, DecodeError> {
    let channels = match format {
        PixelFormat::L8 => 1,
        PixelFormat::RGB24 => 3,
        PixelFormat::CMYK32 => 4,
        PixelFormat::L16 => return Err(corrupt("16-bit JPEG")),
    };
    if data.len() != pixels * channels {
        return Err(corrupt("decoded size mismatch"));
    }
    let mut out = Vec::with_capacity(pixels * 4);
    for px in data.chunks_exact(channels) {
        let rgb = match *px {
            [l] => [l, l, l],
            [r, g, b] => [r, g, b],
            // Inverted CMYK as `jpeg-decoder` returns it, to RGB (as `image` 0.24 did).
            [c, m, y, k] => {
                let mix = |v: u8| ((255 - u16::from(v)) * (255 - u16::from(k)) / 255) as u8;
                [mix(c), mix(m), mix(y)]
            }
            _ => unreachable!("chunks_exact yields `channels` bytes"),
        };
        out.extend_from_slice(&rgb);
        out.push(255);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{decode_bytes, DecodedImage};
    use crate::test_fixtures as fixtures;

    const P3_ICC: &[u8] = include_bytes!("../../../../testdata/icc/DisplayP3-compact.icc");

    fn open(bytes: &[u8], max_pixels: u64) -> Result<DecodedImage, DecodeError> {
        decode_bytes(bytes, &DecodeLimits { max_pixels })
    }

    #[test]
    fn pick_scale_table() {
        let cases: &[(u32, u32, u64, Option<u8>)] = &[
            (64, 48, 3072, Some(1)),
            (64, 48, 3071, Some(2)),
            (64, 48, 768, Some(2)),
            (64, 48, 767, Some(4)),
            (64, 48, 192, Some(4)),
            (64, 48, 48, Some(8)),
            (64, 48, 47, None),
            // Sides round up: 9x9 at 1/8 is 2x2.
            (9, 9, 4, Some(8)),
            (9, 9, 3, None),
            (20000, 8, 20000, Some(4)),
            (20000, 8, 2500, Some(8)),
            (1, 1, 1, Some(1)),
            (1, 1, 0, None),
            (65535, 65535, 268_435_456, Some(4)),
        ];
        for &(w, h, max, want) in cases {
            assert_eq!(pick_scale(w, h, max), want, "{w}x{h} within {max}");
        }
    }

    #[test]
    fn jpeg_over_budget_opens_scaled() {
        let jpeg = fixtures::jpeg_halves(64, 48, 1, None);
        let img = open(&jpeg, 3071).unwrap();
        assert_eq!(img.scaled_from, Some((64, 48)));
        assert_eq!((img.width, img.height), (32, 24));
        assert!(u64::from(img.width) * u64::from(img.height) <= 3071);
        assert_eq!(img.frames[0].bgra.len(), 32 * 24 * 4);
        // Left half red, right half blue (BGRA).
        let px = |x: usize| &img.frames[0].bgra[(12 * 32 + x) * 4..][..4];
        assert!(px(4)[2] > 200 && px(4)[0] < 60, "left {:?}", px(4));
        assert!(px(28)[0] > 200 && px(28)[2] < 60, "right {:?}", px(28));
        let img = open(&jpeg, 100).unwrap();
        assert_eq!((img.width, img.height), (8, 6));
        assert_eq!(img.scaled_from, Some((64, 48)));
    }

    #[test]
    fn jpeg_at_budget_opens_full() {
        let jpeg = fixtures::jpeg_halves(64, 48, 1, None);
        let img = open(&jpeg, 3072).unwrap();
        assert_eq!((img.width, img.height, img.scaled_from), (64, 48, None));
    }

    #[test]
    fn jpeg_over_budget_even_at_an_eighth_is_too_large() {
        let jpeg = fixtures::jpeg_halves(64, 48, 1, None);
        assert!(matches!(
            open(&jpeg, 47),
            Err(DecodeError::TooLarge {
                width: 64,
                height: 48
            })
        ));
    }

    #[test]
    fn png_over_budget_is_still_too_large() {
        let png = fixtures::png_2x1();
        assert!(matches!(
            open(&png, 1),
            Err(DecodeError::TooLarge {
                width: 2,
                height: 1
            })
        ));
    }

    #[test]
    fn scaled_jpeg_applies_orientation_like_the_full_one() {
        let jpeg = fixtures::jpeg_halves(64, 48, 6, None);
        let full = open(&jpeg, 3072).unwrap();
        let scaled = open(&jpeg, 3071).unwrap();
        assert_eq!((full.width, full.height), (48, 64));
        assert_eq!((scaled.width, scaled.height), (24, 32));
        // Oriented, like `width` and `height`.
        assert_eq!(scaled.scaled_from, Some((48, 64)));
        // Rotated 90 degrees clockwise: red on top, blue at the bottom.
        let px = |y: usize| &scaled.frames[0].bgra[(y * 24 + 12) * 4..][..4];
        assert!(px(4)[2] > 200 && px(4)[0] < 60, "top {:?}", px(4));
        assert!(px(28)[0] > 200 && px(28)[2] < 60, "bottom {:?}", px(28));
    }

    #[test]
    fn scaled_jpeg_converts_icc_like_the_full_one() {
        let tagged = fixtures::jpeg_halves(64, 48, 1, Some(P3_ICC));
        let untagged = fixtures::jpeg_halves(64, 48, 1, None);
        let left =
            |img: &DecodedImage| img.frames[0].bgra[(img.width as usize / 4) * 4..][..4].to_vec();
        let full = left(&open(&tagged, 3072).unwrap());
        let scaled = left(&open(&tagged, 3071).unwrap());
        assert_ne!(
            full,
            left(&open(&untagged, 3072).unwrap()),
            "the profile applies"
        );
        let diff = full.iter().zip(&scaled).map(|(a, b)| a.abs_diff(*b)).max();
        assert!(diff <= Some(4), "full {full:?} scaled {scaled:?}");
    }

    #[test]
    fn scaled_grayscale_jpeg_expands_to_gray() {
        let mut gray = image::GrayImage::new(64, 48);
        for (x, _, p) in gray.enumerate_pixels_mut() {
            *p = image::Luma([if x < 32 { 40 } else { 220 }]);
        }
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&gray)
            .unwrap();
        let img = open(&jpeg, 3071).unwrap();
        assert_eq!(
            (img.width, img.height, img.scaled_from),
            (32, 24, Some((64, 48)))
        );
        for (x, want) in [(4usize, 40u8), (28, 220)] {
            let p = &img.frames[0].bgra[(12 * 32 + x) * 4..][..4];
            assert!(p[..3].iter().all(|&c| c.abs_diff(want) <= 6), "{x}: {p:?}");
            assert!(p[0] == p[1] && p[1] == p[2], "gray: {p:?}");
            assert_eq!(p[3], 255);
        }
    }

    /// The real 64x32 `gradient-64x32.jpg` with its SOF claiming `side`x`side` and its
    /// scan data cut right after the scan header.
    fn crafted(side: u16) -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testdata/jpeg/gradient-64x32.jpg"
        );
        let mut bytes = std::fs::read(path).unwrap();
        let (sof_at, sos_end) = {
            let markers: Vec<_> = Markers::new(&bytes).map_while(Result::ok).collect();
            let sof = markers.iter().find(|m| is_frame_header(m.code)).unwrap();
            let sof_at = sof.payload.as_ptr() as usize - bytes.as_ptr() as usize;
            let sos = markers.iter().find(|m| m.code == SOS);
            (sof_at, sos.unwrap().end)
        };
        bytes.truncate(sos_end);
        bytes.extend([0xFF, 0xD9]);
        let side = side.to_be_bytes();
        bytes[sof_at + 1..sof_at + 5].copy_from_slice(&[side[0], side[1], side[0], side[1]]);
        bytes
    }

    /// Decodes with the default limits; must not succeed, and must be quick about it.
    fn refused_fast(bytes: &[u8]) -> DecodeError {
        let start = std::time::Instant::now();
        let result = decode_bytes(bytes, &DecodeLimits::default());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        result.expect_err("a crafted file must not decode")
    }

    #[test]
    fn huge_header_over_little_data_fails_fast() {
        // At the source cap the data check refuses it; past the cap, the size does.
        assert!(matches!(
            refused_fast(&crafted(32768)),
            DecodeError::Corrupt(_)
        ));
        assert!(matches!(
            refused_fast(&crafted(65535)),
            DecodeError::TooLarge { .. }
        ));
    }

    #[test]
    fn bytes_after_eoi_are_not_scan_data() {
        let mut bytes = crafted(32768);
        bytes.resize(bytes.len() + (30 << 20), 0);
        assert!(matches!(refused_fast(&bytes), DecodeError::Corrupt(_)));
    }

    #[test]
    fn metadata_segments_are_not_scan_data() {
        // About 32 MB of APP5 segments right after SOI.
        let bytes = crafted(32768);
        let app = crate::jpeg_markers::tests::segment(0xE5, &[0x41; 65533]);
        let mut big = bytes[..2].to_vec();
        for _ in 0..500 {
            big.extend_from_slice(&app);
        }
        big.extend_from_slice(&bytes[2..]);
        assert!(matches!(refused_fast(&big), DecodeError::Corrupt(_)));
    }

    #[test]
    fn far_over_the_limit_is_too_large_before_the_frame_is_read() {
        // Over 64 times the limit cannot fit at 1/8; the bytes are not even looked at.
        assert!(matches!(
            decode(b"not a jpeg", 64, 48, &DecodeLimits { max_pixels: 47 }),
            Err(DecodeError::TooLarge {
                width: 64,
                height: 48
            })
        ));
        // Over 32768² is too slow to decode whatever the limit (R9).
        assert!(matches!(
            decode(b"not a jpeg", 40000, 30000, &DecodeLimits::default()),
            Err(DecodeError::TooLarge {
                width: 40000,
                height: 30000
            })
        ));
    }

    /// SOF payload: 8-bit, `w`x`h`, components with the given (H, V) sampling.
    fn sof(w: u16, h: u16, sampling: &[(u8, u8)]) -> Vec<u8> {
        let mut p = vec![8];
        p.extend_from_slice(&h.to_be_bytes());
        p.extend_from_slice(&w.to_be_bytes());
        p.push(sampling.len() as u8);
        for (i, (hs, vs)) in sampling.iter().enumerate() {
            p.extend_from_slice(&[i as u8 + 1, hs << 4 | vs, 0]);
        }
        p
    }

    #[test]
    fn blocks_are_counted_per_component() {
        // 4:2:0, 64x48: MCUs of 16x16 -> 4x3; Y has 8x6 blocks, Cb and Cr 4x3 each.
        let blocks = frame_blocks(&sof(64, 48, &[(2, 2), (1, 1), (1, 1)]));
        assert_eq!(blocks, Some(48 + 12 + 12));
        // 4:4:4, 9x9 pads to 2x2 blocks per component.
        assert_eq!(
            frame_blocks(&sof(9, 9, &[(1, 1), (1, 1), (1, 1)])),
            Some(3 * 4)
        );
        assert_eq!(frame_blocks(&sof(9, 9, &[(0, 1)])), None);
        assert_eq!(frame_blocks(&sof(9, 9, &[])), None);
        assert_eq!(frame_blocks(&[8, 0, 9]), None);
    }

    #[test]
    fn only_sequential_or_small_progressive_frames_are_scaled() {
        // A frame header, then one scan with `data` bytes of entropy-coded data.
        let jpeg = |code: u8, payload: &[u8], data: usize| {
            let mut b = vec![0xFF, 0xD8];
            b.extend(crate::jpeg_markers::tests::segment(code, payload));
            b.extend(crate::jpeg_markers::tests::segment(
                SOS,
                &[1, 1, 0, 0, 63, 0],
            ));
            b.resize(b.len() + data, 0);
            b.extend([0xFF, 0xD9]);
            b
        };
        // 3 * 1250 * 1250 blocks, each taking at least one bit.
        let big = sof(10000, 10000, &[(1, 1), (1, 1), (1, 1)]);
        let blocks: usize = 3 * 1250 * 1250;
        let cap = 500_000_000;
        assert!(check_frame(&jpeg(0xC0, &big, blocks.div_ceil(8)), cap).is_ok());
        assert!(check_frame(&jpeg(0xC1, &big, blocks.div_ceil(8)), cap).is_ok());
        assert!(matches!(
            check_frame(&jpeg(0xC0, &big, blocks / 8 - 30), cap),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        // Progressive with enough data, but 600 MB of coefficients is over a 500 MB cap.
        assert!(matches!(
            check_frame(&jpeg(0xC2, &big, blocks.div_ceil(8)), cap),
            Err(None)
        ));
        let small = sof(640, 480, &[(1, 1), (1, 1), (1, 1)]);
        assert!(check_frame(&jpeg(0xC2, &small, 3 * 4800 / 8), cap).is_ok());
        assert!(matches!(
            check_frame(&jpeg(0xC2, &small, 3 * 4800 / 8 - 30), cap),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        // Lossless and arithmetic-coded frames are not scaled.
        for code in [0xC3, 0xC9, 0xCA] {
            assert!(matches!(
                check_frame(&jpeg(code, &small, 0), cap),
                Err(None)
            ));
        }
        assert!(matches!(
            check_frame(&jpeg(0xC2, &[8], 0), cap),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        assert!(matches!(
            check_frame(&[0xFF, 0xD8, 0xFF, 0xD9], cap),
            Err(Some(DecodeError::Corrupt(_)))
        ));
    }

    #[test]
    fn entropy_bytes_count_scans_only() {
        let seg = crate::jpeg_markers::tests::segment;
        let mut b = vec![0xFF, 0xD8];
        b.extend(seg(0xE1, &[0; 100]));
        b.extend(seg(SOS, &[0; 6]));
        // 4 data bytes, a stuffed FF00 and a restart marker inside the scan.
        b.extend([1, 2, 0xFF, 0x00, 3, 0xFF, 0xD0, 4]);
        b.extend(seg(0xC4, &[0; 50]));
        b.extend(seg(SOS, &[0; 6]));
        b.extend([5, 6]);
        b.extend([0xFF, 0xD9]);
        b.extend([7; 1000]);
        assert_eq!(entropy_bytes(&b), 8 + 2);
    }
}
