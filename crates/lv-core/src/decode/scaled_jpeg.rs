//! Opening a JPEG over the pixel limit at 1/2, 1/4 or 1/8 size (ruling R1), with
//! `jpeg-decoder`'s DCT scaling, so the full-size pixels are never held.
//!
//! Only Huffman-coded sequential (SOF0, SOF1) and progressive (SOF2) frames are scaled.
//! A progressive frame keeps every coefficient of the full-size image while it decodes,
//! so it is scaled only when those fit in the same `max_pixels * 4` bytes the full
//! decode may allocate; otherwise, like anything over the limit even at 1/8, it is
//! `TooLarge`.
//!
//! `jpeg-decoder` decodes every block of the full-size image and cannot be cancelled.
//! Every JPEG has passed `jpeg_budget::check` before it gets here (scans in proportion
//! to the image, rulings R9–R11); on top of that a source over `MAX_SOURCE_PIXELS` is
//! `TooLarge`. Worst cases measured (release, 16-thread laptop): a sequential 32768²
//! about 12 s; a progressive frame is bounded by the coefficient cap (gray 23168² at
//! the default limit) and the work budget: a noise-filled gray 23168² written with
//! libjpeg's default progressive script (191 of the 192 coefficients per block allowed,
//! 354 MB) took 21 s.

use std::io::Cursor;

use jpeg_decoder::{Decoder, PixelFormat};

use super::jpeg_budget::JpegFrame;
use super::{corrupt, DecodeError, DecodeLimits, RgbaFrame};

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
/// the largest size within it. `frame` is what `jpeg_budget::check` found (`None`: a
/// frame other than SOF0..2, which is not scaled).
pub(super) fn decode(
    bytes: &[u8],
    width: u32,
    height: u32,
    limits: &DecodeLimits,
    frame: Option<JpegFrame>,
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
    if !frame.is_some_and(|f| scalable(&f, alloc_cap)) {
        return Err(too_large());
    }

    let side = |len: u32| u16::try_from(len.div_ceil(u32::from(denom)));
    let (Ok(want_w), Ok(want_h)) = (side(width), side(height)) else {
        return Err(too_large());
    };
    let mut dec = Decoder::new(Cursor::new(bytes));
    // Bounds only the final output; `jpeg_budget`, `scalable` and `pick_scale` guard.
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

/// Whether a checked frame can be decoded at reduced size: a progressive frame's
/// full-size coefficients must fit in `alloc_cap` bytes.
fn scalable(frame: &JpegFrame, alloc_cap: u64) -> bool {
    !frame.progressive || frame.blocks.saturating_mul(COEFFICIENT_BYTES) <= alloc_cap
}

/// The bytes a progressive decode holds per block: 64 `i16` coefficients.
const COEFFICIENT_BYTES: u64 = 64 * 2;

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
pub(crate) mod tests {
    use super::*;
    use crate::decode::jpeg_budget::{check, scan_data_len, Sof};
    use crate::decode::{decode_bytes, DecodedImage};
    use crate::jpeg_markers::{is_frame_header, Markers, SOS};
    use crate::test_fixtures as fixtures;

    /// The checks before a scaled decode: `Err(None)` where it would be `TooLarge`.
    fn check_frame(bytes: &[u8], alloc_cap: u64) -> Result<(), Option<DecodeError>> {
        match check(bytes) {
            Err(e) => Err(Some(e)),
            Ok(Some(f)) if scalable(&f, alloc_cap) => Ok(()),
            Ok(_) => Err(None),
        }
    }

    fn frame_blocks(sof: &[u8]) -> Option<u64> {
        Sof::parse(sof).map(|s| s.blocks())
    }

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
        // The data check runs on every JPEG before the size decides the path.
        for side in [32768, 65535] {
            assert!(matches!(
                refused_fast(&crafted(side)),
                DecodeError::Corrupt(_)
            ));
        }
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
            decode(
                b"not a jpeg",
                64,
                48,
                &DecodeLimits { max_pixels: 47 },
                None
            ),
            Err(DecodeError::TooLarge {
                width: 64,
                height: 48
            })
        ));
        // Over 32768² is too slow to decode whatever the limit (R9).
        assert!(matches!(
            decode(b"not a jpeg", 40000, 30000, &DecodeLimits::default(), None),
            Err(DecodeError::TooLarge {
                width: 40000,
                height: 30000
            })
        ));
    }

    /// SOF payload: 8-bit, `w`x`h`, components with the given (H, V) sampling.
    pub(crate) fn sof(w: u16, h: u16, sampling: &[(u8, u8)]) -> Vec<u8> {
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
            // One interleaved scan of all three components.
            b.extend(crate::jpeg_markers::tests::segment(SOS, &ALL_THREE));
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

    /// SOS payload: the components `ids`, spectral range `ss..=se`, and `ah`.
    pub(crate) fn sos(ids: &[u8], ss: u8, se: u8, ah: u8) -> Vec<u8> {
        let mut p = vec![ids.len() as u8];
        for id in ids {
            p.extend([*id, 0]);
        }
        p.extend([ss, se, ah << 4]);
        p
    }

    /// One interleaved sequential scan of components 1, 2 and 3.
    const ALL_THREE: [u8; 10] = [3, 1, 0, 2, 0, 3, 0, 0, 63, 0];

    /// SOI, a frame header, each scan followed by that many bytes of data, EOI.
    fn file(code: u8, sof_payload: &[u8], scans: &[(Vec<u8>, usize)]) -> Vec<u8> {
        let seg = crate::jpeg_markers::tests::segment;
        let mut b = vec![0xFF, 0xD8];
        b.extend(seg(code, sof_payload));
        for (header, data) in scans {
            b.extend(seg(SOS, header));
            b.resize(b.len() + data, 0);
        }
        b.extend([0xFF, 0xD9]);
        b
    }

    #[test]
    fn scan_data_counts_stuffing_and_restarts_up_to_the_next_marker() {
        let mut b = vec![0; 4];
        b.extend([1, 2, 0xFF, 0x00, 3, 0xFF, 0xD0, 4, 0xFF, 0xC4, 0, 2]);
        assert_eq!(scan_data_len(&b, 4), 8);
        assert_eq!(scan_data_len(&[0, 0, 1, 2, 3], 2), 3);
    }

    #[test]
    fn scan_blocks_pad_only_interleaved_scans() {
        let s = Sof::parse(&sof(648, 480, &[(2, 2), (1, 1), (1, 1)])).unwrap();
        // Alone, Y has its own 81x60 blocks; MCU padding would make it 82x60.
        assert_eq!(s.scan_blocks(&[1]), Some(81 * 60));
        assert_eq!(s.scan_blocks(&[2]), Some(41 * 30));
        // Interleaved: 41x30 MCUs of 4 + 1 + 1 blocks.
        assert_eq!(s.scan_blocks(&[1, 2, 3]), Some(41 * 30 * 6));
        assert_eq!(s.scan_blocks(&[4]), None);
    }

    /// A real 64x48 gray JPEG claiming 32768x32768, its scan data replaced by enough
    /// zero bytes for every block, then `repeats` more scans of the same component with
    /// one byte each.
    fn gray_crafted(repeats: usize) -> Vec<u8> {
        crafted_scans(32768, false, repeats)
    }

    /// A real 64x48 gray JPEG claiming `side`x`side` (as progressive if asked): one
    /// scan with enough zero bytes for every block (sequential, or progressive DC-first),
    /// then `extra` scans with one byte each (the same scan again, or AC 1..63).
    fn crafted_scans(side: u16, progressive: bool, extra: usize) -> Vec<u8> {
        let gray = image::GrayImage::from_pixel(64, 48, image::Luma([128]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&gray)
            .unwrap();
        let (sof_at, sos_at, sos_end) = {
            let markers: Vec<_> = Markers::new(&jpeg).map_while(Result::ok).collect();
            let at = |m: &crate::jpeg_markers::Marker| {
                m.payload.as_ptr() as usize - jpeg.as_ptr() as usize
            };
            let sof = markers.iter().find(|m| is_frame_header(m.code)).unwrap();
            let sos = markers.iter().find(|m| m.code == SOS).unwrap();
            (at(sof), at(sos) - 4, sos.end)
        };
        let seg = crate::jpeg_markers::tests::segment;
        let (first, more) = if progressive {
            (
                seg(SOS, &sos(&[1], 0, 0, 0)),
                seg(SOS, &sos(&[1], 1, 63, 0)),
            )
        } else {
            let header = jpeg[sos_at..sos_end].to_vec();
            (header.clone(), header)
        };
        let mut out = jpeg[..sos_at].to_vec();
        let s = side.to_be_bytes();
        out[sof_at + 1..sof_at + 5].copy_from_slice(&[s[0], s[1], s[0], s[1]]);
        if progressive {
            out[sof_at - 3] = 0xC2;
        }
        out.extend(first);
        let blocks = usize::from(side).div_ceil(8).pow(2);
        out.resize(out.len() + blocks.div_ceil(8), 0);
        for _ in 0..extra {
            out.extend_from_slice(&more);
            out.push(0);
        }
        out.extend([0xFF, 0xD9]);
        out
    }

    #[test]
    fn crafted_scans_fail_fast_on_both_paths() {
        // 16384² is the default limit (full decode); with 2^23 it is decoded at 1/8.
        let limits = [
            DecodeLimits::default(),
            DecodeLimits {
                max_pixels: 1 << 23,
            },
        ];
        // Honest, these pass the check (they are not decoded here: that takes seconds).
        assert!(check(&crafted_scans(16384, true, 0)).unwrap().is_some());
        assert!(check(&crafted_scans(16384, false, 0)).unwrap().is_some());
        for file in [
            crafted_scans(16384, true, 63),
            crafted_scans(16384, false, 16),
        ] {
            for limits in &limits {
                let start = std::time::Instant::now();
                let result = decode_bytes(&file, limits);
                assert!(matches!(result, Err(DecodeError::Corrupt(_))), "{result:?}");
                assert!(start.elapsed() < std::time::Duration::from_secs(1));
            }
        }
    }

    #[test]
    fn real_jpegs_still_decode() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/jpeg");
        for name in ["gradient-64x32.jpg", "gradient-65x33.jpg"] {
            let bytes = std::fs::read(format!("{dir}/{name}")).unwrap();
            assert!(check(&bytes).unwrap().is_some(), "{name}");
            decode_bytes(&bytes, &DecodeLimits::default()).expect(name);
            // And at reduced size.
            let img = decode_bytes(&bytes, &DecodeLimits { max_pixels: 600 }).expect(name);
            assert!(img.scaled_from.is_some(), "{name}");
        }
    }

    #[test]
    fn repeated_sequential_scans_fail_fast() {
        // One full scan passes the check (it is not decoded here: that takes seconds).
        assert!(check_frame(&gray_crafted(0), u64::MAX).is_ok());
        assert!(matches!(
            check_frame(&gray_crafted(4), u64::MAX),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        assert!(matches!(
            refused_fast(&gray_crafted(4)),
            DecodeError::Corrupt(_)
        ));
        // Even repeats with enough data each: a component is coded once.
        let gray = sof(32768, 32768, &[(1, 1)]);
        let full = (sos(&[1], 0, 63, 0), 4096 * 4096 / 8);
        assert!(matches!(
            check_frame(&file(0xC0, &gray, &[full.clone(), full]), u64::MAX),
            Err(Some(DecodeError::Corrupt(_)))
        ));
    }

    #[test]
    fn sequential_scans_are_checked_one_by_one() {
        // 4:2:0 640x480: Y alone has 80x60 blocks; Cb and Cr interleaved, 40x30 MCUs
        // of one block each.
        let s = sof(640, 480, &[(2, 2), (1, 1), (1, 1)]);
        let scans = |second: usize| {
            file(
                0xC0,
                &s,
                &[(sos(&[1], 0, 63, 0), 600), (sos(&[2, 3], 0, 63, 0), second)],
            )
        };
        assert!(check_frame(&scans(300), u64::MAX).is_ok());
        assert!(matches!(
            check_frame(&scans(299), u64::MAX),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        // A scan of a component the frame does not have, and a frame without scans.
        let unknown = file(0xC0, &s, &[(sos(&[9], 0, 63, 0), 10_000)]);
        assert!(matches!(
            check_frame(&unknown, u64::MAX),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        assert!(matches!(
            check_frame(&file(0xC0, &s, &[]), u64::MAX),
            Err(Some(DecodeError::Corrupt(_)))
        ));
    }

    #[test]
    fn progressive_scans_are_capped_and_only_dc_first_needs_data() {
        let s = sof(640, 480, &[(1, 1), (1, 1), (1, 1)]);
        // A DC-first scan of all 3 * 4800 blocks, then near-empty AC scans.
        let scans = |total: usize| {
            let mut v = vec![(sos(&[1, 2, 3], 0, 0, 0), 3 * 4800 / 8)];
            v.extend((1..total).map(|_| (sos(&[1], 1, 1, 0), 1)));
            file(0xC2, &s, &v)
        };
        let start = std::time::Instant::now();
        assert!(check_frame(&scans(64), u64::MAX).is_ok());
        assert!(matches!(
            check_frame(&scans(65), u64::MAX),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        // A short DC-first scan, and a DC refinement (Ah = 1) needing no floor.
        let short = file(0xC2, &s, &[(sos(&[1, 2, 3], 0, 0, 0), 100)]);
        assert!(matches!(
            check_frame(&short, u64::MAX),
            Err(Some(DecodeError::Corrupt(_)))
        ));
        let refine = file(
            0xC2,
            &s,
            &[
                (sos(&[1, 2, 3], 0, 0, 0), 1800),
                (sos(&[1, 2, 3], 0, 0, 1), 1),
            ],
        );
        assert!(check_frame(&refine, u64::MAX).is_ok());
    }
}
