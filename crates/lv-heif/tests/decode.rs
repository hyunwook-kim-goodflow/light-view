//! Decoding tests against the samples in `testdata/heif/` (see its README for provenance).

use lv_heif::{decode_heif, ColorInfo, HeifError, HeifImage};

const NO_LIMIT: u64 = 100_000_000;

fn sample(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/heif")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn decode_ok(name: &str) -> HeifImage {
    match decode_heif(&sample(name), NO_LIMIT) {
        Ok(img) => img,
        Err(e) => panic!("{name}: {e:?}"),
    }
}

#[test]
fn decodes_sample_dimensions() {
    let img = decode_ok("rainbow-451x461.heic");
    assert_eq!((img.width, img.height), (451, 461));
    assert_eq!(img.rgba.len(), 451 * 461 * 4);
    assert!(
        img.rgba.chunks_exact(4).all(|px| px[3] == 255),
        "opaque image must have alpha 255"
    );
}

#[test]
fn icc_profile_is_returned() {
    let img = decode_ok("rainbow-451x461.heic");
    match img.color {
        // An ICC profile starts with its size and has the signature "acsp" at offset 36.
        ColorInfo::Icc(icc) => assert_eq!(&icc[36..40], b"acsp"),
        other => panic!("expected ICC, got {other:?}"),
    }
}

#[test]
fn alpha_sample_keeps_alpha() {
    let img = decode_ok("with-alpha-512x512.heic");
    assert_eq!((img.width, img.height), (512, 512));
    assert_eq!(img.rgba.len(), 512 * 512 * 4);
    let alphas: Vec<u8> = img.rgba.chunks_exact(4).map(|px| px[3]).collect();
    assert!(alphas.contains(&0), "expected fully transparent pixels");
    assert!(alphas.contains(&255), "expected opaque pixels");
}

#[test]
fn too_large_rejected_before_decode() {
    match decode_heif(&sample("rainbow-451x461.heic"), 16) {
        Err(HeifError::TooLarge { width, height }) => assert_eq!((width, height), (451, 461)),
        other => panic!(
            "expected TooLarge, got {:?}",
            other.map(|i| (i.width, i.height))
        ),
    }
}

#[test]
fn garbage_is_corrupt_not_panic() {
    let mut bytes = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
    bytes.extend((0u32..4096).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
    match decode_heif(&bytes, NO_LIMIT) {
        Err(HeifError::Corrupt(msg)) => assert!(!msg.is_empty()),
        other => panic!(
            "expected Corrupt, got {:?}",
            other.map(|i| (i.width, i.height))
        ),
    }
}

#[test]
fn empty_input_is_corrupt() {
    assert!(matches!(
        decode_heif(&[], NO_LIMIT),
        Err(HeifError::Corrupt(_))
    ));
}

#[test]
fn truncated_file_is_corrupt() {
    let bytes = sample("rainbow-451x461.heic");
    let half = &bytes[..bytes.len() / 2];
    match decode_heif(half, NO_LIMIT) {
        Err(HeifError::Corrupt(msg)) => assert!(!msg.is_empty()),
        other => panic!(
            "expected Corrupt, got {:?}",
            other.map(|i| (i.width, i.height))
        ),
    }
}

#[test]
fn irot_sample_is_rotated() {
    let upright = decode_ok("rainbow-451x461.heic");
    let rotated = decode_ok("rainbow-451x461-irot90.heic");
    // irot 90 = 90 degrees anti-clockwise: the display size swaps.
    assert_eq!((rotated.width, rotated.height), (461, 451));
    // Same bitstream, so rotated(x, y) = upright(W - 1 - y, x). libheif rotates the 4:2:0
    // planes before chroma upsampling, so colour edges differ slightly: compare the mean
    // difference (about 0.15 for anti-clockwise, about 86 for clockwise).
    let ccw = mean_abs_diff(&upright, &rotated, |x, y, w, _| (w - 1 - y, x));
    let cw = mean_abs_diff(&upright, &rotated, |x, y, _, h| (y, h - 1 - x));
    assert!(ccw < 1.0, "anti-clockwise mean difference {ccw}");
    assert!(cw > 20.0, "clockwise mean difference {cw}");
}

/// Mean absolute RGB difference between `rotated` and `upright` sampled at
/// `source(x, y, upright_w, upright_h)`.
fn mean_abs_diff(
    upright: &HeifImage,
    rotated: &HeifImage,
    source: impl Fn(usize, usize, usize, usize) -> (usize, usize),
) -> f64 {
    let (w, h) = (upright.width as usize, upright.height as usize);
    let (rw, rh) = (rotated.width as usize, rotated.height as usize);
    let mut sum = 0u64;
    for y in 0..rh {
        for x in 0..rw {
            let (ux, uy) = source(x, y, w, h);
            let r = (y * rw + x) * 4;
            let u = (uy * w + ux) * 4;
            for c in 0..3 {
                sum += u64::from(rotated.rgba[r + c].abs_diff(upright.rgba[u + c]));
            }
        }
    }
    sum as f64 / (rw * rh * 3) as f64
}
