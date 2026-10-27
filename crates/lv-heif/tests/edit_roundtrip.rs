//! `lv_core::heif_edit` results decoded by libheif: the edit must be lossless and turn the
//! displayed image the way the viewer's clockwise rotation does.

use lv_core::heif_edit::{compose, read_rotation, set_rotation};
use lv_heif::{decode_heif, HeifImage};

const NO_LIMIT: u64 = 100_000_000;

fn sample(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/heif")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn decode(bytes: &[u8]) -> HeifImage {
    decode_heif(bytes, NO_LIMIT).unwrap_or_else(|e| panic!("decode: {e:?}"))
}

/// `bytes` turned `user_cw` clockwise quarter turns, as a rotation save does it.
fn turn(bytes: &[u8], user_cw: u8) -> Vec<u8> {
    set_rotation(bytes, compose(read_rotation(bytes).unwrap(), user_cw)).unwrap()
}

/// Mean absolute RGBA difference between `rotated` and `upright` sampled at
/// `source(x, y, upright_w, upright_h)`. libheif rotates the 4:2:0 planes before chroma
/// upsampling, so a correct rotation differs slightly (well under 1) and a wrong one a lot.
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
            for c in 0..4 {
                sum += u64::from(rotated.rgba[r + c].abs_diff(upright.rgba[u + c]));
            }
        }
    }
    sum as f64 / (rw * rh * 4) as f64
}

/// Display pixel (x, y) of an image turned clockwise once comes from (y, h - 1 - x).
fn cw(x: usize, y: usize, _w: usize, h: usize) -> (usize, usize) {
    (y, h - 1 - x)
}

fn ccw(x: usize, y: usize, w: usize, _h: usize) -> (usize, usize) {
    (w - 1 - y, x)
}

#[test]
fn add_irot_shifts_iloc_offsets_and_still_decodes() {
    // The alpha sample is refused (see `alpha_rotation_is_refused`).
    let src = sample("rainbow-451x461.heic");
    let upright = decode(&src);
    let out = turn(&src, 1);
    assert_eq!(out.len(), src.len() + 10, "an irot was added");
    let rotated = decode(&out);
    assert_eq!((rotated.width, rotated.height), (461, 451));
    // One clockwise turn, not anti-clockwise.
    let right = mean_abs_diff(&upright, &rotated, cw);
    let wrong = mean_abs_diff(&upright, &rotated, ccw);
    assert!(right < 1.0, "clockwise mean difference {right}");
    assert!(wrong > 10.0, "anti-clockwise mean difference {wrong}");
}

#[test]
fn half_turn_decodes_upside_down() {
    let src = sample("rainbow-451x461.heic");
    let upright = decode(&src);
    let rotated = decode(&turn(&src, 2));
    assert_eq!((rotated.width, rotated.height), (451, 461));
    let diff = mean_abs_diff(&upright, &rotated, |x, y, w, h| (w - 1 - x, h - 1 - y));
    assert!(diff < 1.0, "mean difference {diff}");
}

#[test]
fn set_existing_irot_in_place() {
    // The sample is stored 90 degrees anti-clockwise; one clockwise turn makes it upright.
    let src = sample("rainbow-451x461-irot90.heic");
    let out = turn(&src, 1);
    assert_eq!(out.len(), src.len());
    let upright = decode(&sample("rainbow-451x461.heic"));
    let decoded = decode(&out);
    assert_eq!((decoded.width, decoded.height), (451, 461));
    let diff = mean_abs_diff(&upright, &decoded, |x, y, _, _| (x, y));
    assert!(diff < 1.0, "mean difference {diff}");
}

#[test]
fn four_quarter_turns_restore_bytes() {
    let src = sample("rainbow-451x461-irot90.heic");
    let mut file = src.clone();
    for _ in 0..4 {
        file = turn(&file, 1);
        decode(&file);
    }
    assert_eq!(file, src);
}

#[test]
fn alpha_rotation_is_refused() {
    // libheif applies `irot` to the colour item only: an `irot` on the primary item would
    // leave the alpha plane unturned, so the edit refuses such files.
    let src = sample("with-alpha-512x512.heic");
    assert!(set_rotation(&src, compose(0, 1)).is_err());
}
