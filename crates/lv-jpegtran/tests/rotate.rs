//! Rotates JPEGs made on the spot by TurboJPEG's own encoder (declared below for the tests
//! only; the crate itself exposes no encoder).

use std::ffi::{c_int, c_uchar, c_void};

use lv_jpegtran::{rotate_lossless, Quarter, TransformError};

const BIG: u64 = 1 << 28;

// TurboJPEG 3.x values used by the encoder helpers below (see turbojpeg.h).
const TJINIT_COMPRESS: c_int = 0;
const TJINIT_DECOMPRESS: c_int = 1;
const TJPARAM_QUALITY: c_int = 3;
const TJPARAM_SUBSAMP: c_int = 4;
const TJPARAM_OPTIMIZE: c_int = 11;
const TJPARAM_PROGRESSIVE: c_int = 12;
const TJSAMP_422: c_int = 1;
const TJSAMP_GRAY: c_int = 3;
const TJPF_RGB: c_int = 0;
const TJPF_GRAY: c_int = 6;

extern "C" {
    fn tj3InitVersion(init_type: c_int, api_version: c_int) -> *mut c_void;
    fn tj3Destroy(handle: *mut c_void);
    fn tj3Set(handle: *mut c_void, param: c_int, value: c_int) -> c_int;
    fn tj3Free(buffer: *mut c_void);
    fn tj3Compress8(
        handle: *mut c_void,
        src: *const c_uchar,
        width: c_int,
        pitch: c_int,
        height: c_int,
        pixel_format: c_int,
        jpeg: *mut *mut c_uchar,
        jpeg_size: *mut usize,
    ) -> c_int;
    fn tj3DecompressHeader(handle: *mut c_void, jpeg: *const c_uchar, size: usize) -> c_int;
    fn tj3Decompress8(
        handle: *mut c_void,
        jpeg: *const c_uchar,
        size: usize,
        dst: *mut c_uchar,
        pitch: c_int,
        pixel_format: c_int,
    ) -> c_int;
}

struct Options {
    subsamp: c_int,
    progressive: bool,
}

const BASELINE_422: Options = Options {
    subsamp: TJSAMP_422,
    progressive: false,
};

/// Encodes `pixels` (RGB, or one byte per pixel for grayscale) at quality 90.
fn encode(pixels: &[u8], width: usize, height: usize, options: Options) -> Vec<u8> {
    let gray = options.subsamp == TJSAMP_GRAY;
    assert_eq!(pixels.len(), width * height * if gray { 1 } else { 3 });
    // SAFETY: test-only calls with valid arguments; the handle and the output buffer are
    // released before returning.
    unsafe {
        let handle = tj3InitVersion(TJINIT_COMPRESS, 3002000);
        assert!(!handle.is_null());
        assert_eq!(tj3Set(handle, TJPARAM_QUALITY, 90), 0);
        assert_eq!(tj3Set(handle, TJPARAM_SUBSAMP, options.subsamp), 0);
        assert_eq!(tj3Set(handle, TJPARAM_OPTIMIZE, 1), 0);
        assert_eq!(
            tj3Set(
                handle,
                TJPARAM_PROGRESSIVE,
                c_int::from(options.progressive)
            ),
            0
        );
        let mut out: *mut c_uchar = std::ptr::null_mut();
        let mut size = 0usize;
        let format = if gray { TJPF_GRAY } else { TJPF_RGB };
        let rc = tj3Compress8(
            handle,
            pixels.as_ptr(),
            width as c_int,
            0,
            height as c_int,
            format,
            &mut out,
            &mut size,
        );
        assert_eq!(rc, 0, "tj3Compress8 failed");
        let jpeg = std::slice::from_raw_parts(out, size).to_vec();
        tj3Free(out.cast());
        tj3Destroy(handle);
        jpeg
    }
}

/// A colourful `width` x `height` JPEG.
fn jpeg(width: usize, height: usize, options: Options) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            rgb.extend_from_slice(&[(x * 255 / width) as u8, (y * 255 / height) as u8, 128]);
        }
    }
    encode(&rgb, width, height, options)
}

/// Decodes a grayscale JPEG to (width, height, one byte per pixel).
fn decode_gray(jpeg: &[u8]) -> (usize, usize, Vec<u8>) {
    let (width, height) = sof_dims(jpeg);
    let mut pixels = vec![0u8; width * height];
    // SAFETY: test-only calls; `pixels` holds width * height bytes, as TJPF_GRAY with an
    // unscaled decode writes.
    unsafe {
        let handle = tj3InitVersion(TJINIT_DECOMPRESS, 3002000);
        assert!(!handle.is_null());
        assert_eq!(tj3DecompressHeader(handle, jpeg.as_ptr(), jpeg.len()), 0);
        let rc = tj3Decompress8(
            handle,
            jpeg.as_ptr(),
            jpeg.len(),
            pixels.as_mut_ptr(),
            0,
            TJPF_GRAY,
        );
        tj3Destroy(handle);
        assert_eq!(rc, 0, "tj3Decompress8 failed");
    }
    (width, height, pixels)
}

/// One element of a JPEG stream: a marker segment (code, payload without the length) or,
/// with code 0, the entropy-coded bytes of a scan (restart markers included).
type Part = (u8, Vec<u8>);

/// Splits a JPEG into its marker segments and entropy-coded data, in order.
fn parts(jpeg: &[u8]) -> Vec<Part> {
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "no SOI");
    let mut out = vec![(0xD8, Vec::new())];
    let mut i = 2;
    while i < jpeg.len() {
        assert_eq!(jpeg[i], 0xFF, "expected a marker at {i}");
        let code = jpeg[i + 1];
        i += 2;
        if code == 0xD9 {
            out.push((code, Vec::new()));
            assert_eq!(i, jpeg.len(), "bytes after EOI");
            break;
        }
        let len = usize::from(u16::from_be_bytes([jpeg[i], jpeg[i + 1]]));
        out.push((code, jpeg[i + 2..i + len].to_vec()));
        i += len;
        if code == 0xDA {
            // Entropy-coded data runs to the next marker that is not a stuffed 0xFF00 or
            // a restart marker.
            let start = i;
            while !(jpeg[i] == 0xFF && jpeg[i + 1] != 0 && !(0xD0..=0xD7).contains(&jpeg[i + 1])) {
                i += 1;
            }
            out.push((0, jpeg[start..i].to_vec()));
        }
    }
    out
}

fn sof_dims(jpeg: &[u8]) -> (usize, usize) {
    let (code, payload) = parts(jpeg)
        .into_iter()
        .find(|(code, _)| matches!(code, 0xC0..=0xCF) && ![0xC4, 0xC8, 0xCC].contains(code))
        .expect("an SOF marker");
    assert!(matches!(code, 0xC0..=0xC2), "SOF{:X}", code - 0xC0);
    let height = usize::from(u16::from_be_bytes([payload[1], payload[2]]));
    let width = usize::from(u16::from_be_bytes([payload[3], payload[4]]));
    (width, height)
}

fn sof_code(jpeg: &[u8]) -> u8 {
    parts(jpeg)
        .into_iter()
        .map(|(code, _)| code)
        .find(|code| matches!(code, 0xC0..=0xC2))
        .expect("an SOF marker")
}

fn segment(code: u8, payload: &[u8]) -> Vec<u8> {
    let mut s = vec![0xFF, code];
    s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    s.extend_from_slice(payload);
    s
}

/// The markers `markers_preserved` looks for: Exif (APP1), ICC (APP2) and a comment.
fn extra_markers() -> Vec<Part> {
    let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08".to_vec();
    exif.extend_from_slice(&[0x5A; 40]);
    let mut icc = b"ICC_PROFILE\0\x01\x01".to_vec();
    icc.extend_from_slice(&[0x3C; 60]);
    vec![
        (0xE1, exif),
        (0xE2, icc),
        (0xFE, b"light-view test".to_vec()),
    ]
}

/// `jpeg` with [`extra_markers`] right after its JFIF APP0 segment.
fn with_markers(jpeg: &[u8]) -> Vec<u8> {
    let p = parts(jpeg);
    assert_eq!(p[1].0, 0xE0, "TurboJPEG writes JFIF APP0 first");
    let app0_end = 2 + 2 + 2 + p[1].1.len();
    let mut out = jpeg[..app0_end].to_vec();
    for (code, payload) in extra_markers() {
        out.extend(segment(code, &payload));
    }
    out.extend_from_slice(&jpeg[app0_end..]);
    out
}

#[test]
fn rotate_aligned_jpeg_swaps_dims() {
    // 4:2:2 has 16x8 MCUs, so 16x8 is aligned; a quarter turn makes it 4:4:0, 8x16.
    let source = jpeg(16, 8, BASELINE_422);
    assert_eq!(sof_dims(&source), (16, 8));
    for (quarter, dims) in [
        (Quarter::Cw90, (8, 16)),
        (Quarter::Cw180, (16, 8)),
        (Quarter::Cw270, (8, 16)),
    ] {
        let rotated = rotate_lossless(&source, quarter, BIG).expect("aligned JPEG rotates");
        assert_eq!(sof_dims(&rotated), dims, "{quarter:?}");
    }
    // A bigger aligned image too.
    let source = jpeg(64, 32, BASELINE_422);
    let rotated = rotate_lossless(&source, Quarter::Cw90, BIG).unwrap();
    assert_eq!(sof_dims(&rotated), (32, 64));
}

#[test]
fn quarters_turn_clockwise() {
    // 16x8 grayscale: a black 8x8 block on the left, a white one on the right.
    let mut pixels = Vec::new();
    for _ in 0..8 {
        pixels.extend_from_slice(&[0u8; 8]);
        pixels.extend_from_slice(&[255u8; 8]);
    }
    let gray = Options {
        subsamp: TJSAMP_GRAY,
        progressive: false,
    };
    let source = encode(&pixels, 16, 8, gray);
    let mean = |p: &[u8]| p.iter().map(|&v| u32::from(v)).sum::<u32>() / p.len() as u32;

    // Clockwise: the left (black) half ends up on top.
    let (w, h, cw) = decode_gray(&rotate_lossless(&source, Quarter::Cw90, BIG).unwrap());
    assert_eq!((w, h), (8, 16));
    assert!(mean(&cw[..64]) < 32 && mean(&cw[64..]) > 223, "{cw:?}");
    // Counter-clockwise: the right (white) half ends up on top.
    let (_, _, ccw) = decode_gray(&rotate_lossless(&source, Quarter::Cw270, BIG).unwrap());
    assert!(mean(&ccw[..64]) > 223 && mean(&ccw[64..]) < 32, "{ccw:?}");
    // Half a turn: white on the left.
    let (w, h, half) = decode_gray(&rotate_lossless(&source, Quarter::Cw180, BIG).unwrap());
    assert_eq!((w, h), (16, 8));
    let left: Vec<u8> = half.chunks(16).flat_map(|row| row[..8].to_vec()).collect();
    assert!(mean(&left) > 223, "{half:?}");
}

#[test]
fn four_quarter_turns_restore_bytes() {
    let source = with_markers(&jpeg(16, 8, BASELINE_422));
    let mut turned = source.clone();
    for _ in 0..4 {
        turned = rotate_lossless(&turned, Quarter::Cw90, BIG).unwrap();
    }
    // Ruling R5: the markers (types and payloads, in order) and the entropy-coded data
    // must match. With an optimized-Huffman source the whole file comes back byte for byte.
    assert_eq!(parts(&turned), parts(&source));
    assert_eq!(turned, source, "four quarter turns are byte-identical");
}

#[test]
fn unaligned_is_not_perfect() {
    // 17x9 is not a multiple of 4:2:2's 16x8 MCU in either direction.
    let source = jpeg(17, 9, BASELINE_422);
    for quarter in [Quarter::Cw90, Quarter::Cw180, Quarter::Cw270] {
        assert_eq!(
            rotate_lossless(&source, quarter, BIG),
            Err(TransformError::NotPerfect),
            "{quarter:?}"
        );
    }
}

#[test]
fn markers_preserved() {
    let source = with_markers(&jpeg(32, 16, BASELINE_422));
    for quarter in [Quarter::Cw90, Quarter::Cw180, Quarter::Cw270] {
        let rotated = rotate_lossless(&source, quarter, BIG).unwrap();
        let kept: Vec<Part> = parts(&rotated)
            .into_iter()
            .filter(|(code, _)| matches!(code, 0xE1 | 0xE2 | 0xFE))
            .collect();
        assert_eq!(kept, extra_markers(), "{quarter:?}");
    }
}

#[test]
fn progressive_stays_progressive() {
    let progressive = Options {
        subsamp: TJSAMP_422,
        progressive: true,
    };
    let source = jpeg(32, 16, progressive);
    assert_eq!(sof_code(&source), 0xC2);
    let rotated = rotate_lossless(&source, Quarter::Cw90, BIG).unwrap();
    assert_eq!(sof_code(&rotated), 0xC2);
    assert_eq!(sof_dims(&rotated), (16, 32));
    let baseline = rotate_lossless(&jpeg(32, 16, BASELINE_422), Quarter::Cw90, BIG).unwrap();
    assert_eq!(sof_code(&baseline), 0xC0);
}

#[test]
fn garbage_is_corrupt() {
    let source = jpeg(16, 8, BASELINE_422);
    // The header and scan data, but not the EOI.
    let truncated = &source[..source.len() - 2];
    let mut bad_scan = source.clone();
    let len = bad_scan.len();
    bad_scan[len - 6..len - 2].copy_from_slice(&[0xFF, 0xC4, 0xFF, 0xC4]);
    for (name, data) in [
        ("empty", &[][..]),
        ("text", &b"not a jpeg at all"[..]),
        ("SOI only", &[0xFF, 0xD8][..]),
    ] {
        match rotate_lossless(data, Quarter::Cw90, BIG) {
            Err(TransformError::Corrupt(msg)) => assert!(!msg.is_empty(), "{name}"),
            other => panic!("{name}: expected Corrupt, got {other:?}"),
        }
    }
    // A readable header but no EOI to be found: whatever follows cannot be ruled out.
    for (name, data) in [("truncated", truncated), ("broken scan", &bad_scan[..])] {
        assert_eq!(
            rotate_lossless(data, Quarter::Cw90, BIG),
            Err(TransformError::HasTrailingData),
            "{name}"
        );
    }
}

#[test]
fn data_after_eoi_is_refused() {
    // Ruling R7: TurboJPEG would drop everything after EOI (gain maps, MPF images, video).
    let source = jpeg(16, 8, BASELINE_422);
    for trailer in [&[0u8][..], &source[..], b"ftypmp42 motion photo video"] {
        let mut data = source.clone();
        data.extend_from_slice(trailer);
        assert_eq!(
            rotate_lossless(&data, Quarter::Cw90, BIG),
            Err(TransformError::HasTrailingData)
        );
    }
    assert!(rotate_lossless(&source, Quarter::Cw90, BIG).is_ok());
}

#[test]
fn mpf_segment_is_refused() {
    let source = jpeg(16, 8, BASELINE_422);
    let p = parts(&source);
    let app0_end = 2 + 2 + 2 + p[1].1.len();
    let mut data = source[..app0_end].to_vec();
    data.extend(segment(0xE2, b"MPF\0MM\0\x2a\0\0\0\x08"));
    data.extend_from_slice(&source[app0_end..]);
    assert_eq!(
        rotate_lossless(&data, Quarter::Cw90, BIG),
        Err(TransformError::HasTrailingData)
    );
}

#[test]
fn pixel_limit_is_checked_before_transforming() {
    let source = jpeg(16, 8, BASELINE_422);
    assert_eq!(
        rotate_lossless(&source, Quarter::Cw90, 127),
        Err(TransformError::TooLarge)
    );
    assert!(rotate_lossless(&source, Quarter::Cw90, 128).is_ok());
}
