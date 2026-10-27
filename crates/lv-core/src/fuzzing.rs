//! Entry functions of the fuzz targets in `fuzz/` (ruling R3).
//!
//! Each takes arbitrary bytes and must never panic. The cargo-fuzz targets call them on
//! nightly, and the stable test `fuzz_corpus_replays` (`tests/fuzz_corpus.rs`) calls them
//! on the seed corpus in `testdata/fuzz/<target>/`. Where a result can be checked against
//! the input cheaply (a rotation reads back), the entry asserts it, so a fuzzer also finds
//! wrong answers, not only crashes.
//!
//! Not part of the viewer's API.

use crate::decode::{self, DecodeLimits};
use crate::jpeg_markers::{skip_entropy_data, Markers};
use crate::{exif_edit, exif_info, heif_edit, jpeg_check, jpeg_extent};

/// The pixel limit `format_decode` decodes with. Small, so a fuzzer spends its time in the
/// parsers rather than in large allocations, and so small JPEG seeds over it take the
/// reduced-size path.
pub const FUZZ_MAX_PIXELS: u64 = 1 << 16;

/// Most of the input passed on to the edits after the parameter bytes.
const EXIF_SEGMENT_MAX: usize = 65_533 - 6;

/// Sniffs and decodes an image of any supported format, as the viewer does (the JPEG
/// pre-decode check, the reduced-size JPEG path over the limit, EXIF, ICC conversion and
/// orientation included). Calls the decoder without the panic guard of
/// [`decode::decode_bytes`], so a decoder panic reaches the fuzzer.
pub fn format_decode(data: &[u8]) {
    let limits = DecodeLimits {
        max_pixels: FUZZ_MAX_PIXELS,
    };
    if let Ok(image) = decode::decode_inner(data, &limits) {
        assert!(image.width > 0 && image.height > 0);
        let pixels = u64::from(image.width) * u64::from(image.height);
        assert!(pixels <= FUZZ_MAX_PIXELS, "decoded over the limit");
        for frame in &image.frames {
            assert_eq!(frame.bgra.len() as u64, pixels * 4);
        }
        if let Some((w, h)) = image.scaled_from {
            assert!(u64::from(w) * u64::from(h) > FUZZ_MAX_PIXELS);
        }
    }
}

/// Reads EXIF from an image file, and from the input taken as a raw TIFF block inside a
/// JPEG `APP1` segment (so the TIFF reader sees the input without a container to find).
pub fn exif_info(data: &[u8]) {
    let _ = exif_info::read_exif(data);
    let _ = decode::exif_orientation(data);
    let jpeg = jpeg_with_exif(&data[..data.len().min(EXIF_SEGMENT_MAX)]);
    let _ = exif_info::read_exif(&jpeg);
}

/// The JPEG EXIF edits. The first three bytes choose the orientation (1..=8) and the new
/// width and height for [`exif_edit::after_pixel_rotation`]; the rest is the JPEG. A
/// successful edit must read back as the orientation written.
pub fn exif_edit(data: &[u8]) {
    let [o, w, h, jpeg @ ..] = data else {
        return;
    };
    let orientation = u16::from(o % 8) + 1;
    let _ = exif_edit::read_orientation(jpeg);
    if let Ok(out) = exif_edit::set_orientation(jpeg, orientation) {
        assert_eq!(exif_edit::read_orientation(&out), orientation);
    }
    let (w, h) = (u32::from(*w) + 1, u32::from(*h) + 1);
    if let Ok(out) = exif_edit::after_pixel_rotation(jpeg, w, h, orientation) {
        assert_eq!(exif_edit::read_orientation(&out), orientation);
    }
    for quarters in 0..4 {
        let (o2, p) = exif_edit::compose(orientation, quarters);
        assert!((1..=8).contains(&o2) && p < 4);
        assert!((1..=8).contains(&exif_edit::compose_tag_only(orientation, quarters)));
    }
}

/// The HEIF `irot` edit and EXIF normalization. The first byte chooses the angle (0..=3
/// anti-clockwise quarter turns); the rest is the file. A successful rotation must read
/// back, and normalization never changes the size.
pub fn heif_edit(data: &[u8]) {
    let [q, heif @ ..] = data else {
        return;
    };
    let quarters = q % 4;
    let _ = heif_edit::read_rotation(heif);
    match heif_edit::set_rotation(heif, quarters) {
        Ok(out) => assert_eq!(heif_edit::read_rotation(&out), Ok(quarters)),
        Err(e) => {
            let _ = heif_edit::is_refusal(&e);
        }
    }
    if let Ok(out) = heif_edit::normalize_exif_orientation(heif) {
        assert_eq!(out.len(), heif.len());
    }
    let _ = heif_edit::exif_item_tiff(heif);
}

/// The JPEG marker walker and the checks built on it: every marker lies inside the input
/// and the walk moves forward.
pub fn jpeg_markers(data: &[u8]) {
    let mut last_end = 2;
    for marker in Markers::new(data) {
        let Ok(marker) = marker else {
            break;
        };
        assert!(marker.end <= data.len() && marker.end > last_end);
        last_end = marker.end;
    }
    let _ = skip_entropy_data(data, 0);
    if let Ok(info) = jpeg_check::inspect(data) {
        for quarters in 0..4 {
            let _ = jpeg_check::is_mcu_aligned(&info, quarters);
        }
    }
    let _ = jpeg_extent::check_single_image(data);
}

/// SOI, one `APP1` EXIF segment holding `tiff`, EOI.
fn jpeg_with_exif(tiff: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
    out.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
    out.extend_from_slice(b"Exif\0\0");
    out.extend_from_slice(tiff);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}
