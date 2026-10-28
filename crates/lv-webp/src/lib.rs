//! Lossy WebP encoding through libwebp's one-call API (`WebPEncodeRGBA`).
//!
//! Only lv-worker links this crate: libwebp is C code and runs in the sandboxed worker,
//! loaded as `libwebp.dll` (which imports `libsharpyuv.dll`) from the worker's own folder.
//!
//! The pixels come from the app (decoded and turned into straight RGBA there, resized by
//! the worker's `lv_core::resize::fit`), never from a file, and no metadata is written: the
//! app attaches the EXIF block to the result afterwards (`lv_core::resize::attach_exif`,
//! ruling P1).
//!
//! The encoding is libwebp's default lossy configuration (`WEBP_PRESET_DEFAULT`, method 4,
//! one thread) at the given quality. It is deterministic: the same pixels and quality give
//! the same bytes (ruling R1, the estimate is the saved file). Fully opaque images get no
//! alpha chunk; others get a losslessly compressed alpha plane.
//!
//! # Memory
//!
//! libwebp's lossy encoder keeps the tokens of the whole picture (method 4 searches over
//! them), so its memory grows with the output: measured on 12 MP images, about 2 bytes per
//! pixel for a smooth photo-like image and up to 28 (opaque noise at quality 100) or 33
//! (noise with alpha) bytes per pixel, the returned `Vec` included. The worker budgets 40
//! bytes per pixel for it in `lv_ipc::webp_fit_memory` and refuses a request beyond its Job.
//! An allocation libwebp cannot make is an error, not an abort.

use std::ffi::{c_float, c_int, c_void};
use std::ptr;

/// The largest width or height WebP can store (`WEBP_MAX_DIMENSION`).
pub const MAX_DIMENSION: u32 = 16383;

/// Encodes straight RGBA8 pixels (`width * height * 4` bytes, rows top to bottom, no
/// padding) as a lossy WebP file at `quality` (`0..=100`).
///
/// Fails for an empty image, a side over [`MAX_DIMENSION`], a quality over 100, a buffer of
/// the wrong length, or when libwebp reports an error (out of memory included).
pub fn encode_rgba(rgba: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err(format!("{width}x{height} image has no pixels"));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(format!(
            "{width}x{height} is over WebP's {MAX_DIMENSION}-pixel side limit"
        ));
    }
    if quality > 100 {
        return Err(format!("quality {quality} is over 100"));
    }
    // Both sides are at most 16383, so none of this overflows.
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(format!(
            "{} bytes of pixels for a {width}x{height} image, expected {expected}",
            rgba.len()
        ));
    }
    let (w, h, stride) = (width as c_int, height as c_int, (width * 4) as c_int);
    let mut out: *mut u8 = ptr::null_mut();
    // SAFETY: `rgba` holds `h` rows of `stride` bytes and outlives the call, which only
    // reads it; `out` is a valid place for libwebp to store its buffer's address.
    let len = unsafe {
        ffi::WebPEncodeRGBA(
            rgba.as_ptr(),
            w,
            h,
            stride,
            c_float::from(quality),
            &mut out,
        )
    };
    if out.is_null() {
        return Err("libwebp could not encode the image".into());
    }
    let result = if len == 0 {
        Err("libwebp could not encode the image".into())
    } else {
        // SAFETY: libwebp returned a buffer of `len` bytes at `out`, freed only below.
        Ok(unsafe { std::slice::from_raw_parts(out, len) }.to_vec())
    };
    // SAFETY: `out` was allocated by libwebp for this call and is not used afterwards.
    unsafe { ffi::WebPFree(out.cast::<c_void>()) };
    result
}

mod ffi {
    use std::ffi::{c_float, c_int, c_void};

    #[link(name = "libwebp", kind = "dylib")]
    extern "C" {
        pub fn WebPEncodeRGBA(
            rgba: *const u8,
            width: c_int,
            height: c_int,
            stride: c_int,
            quality_factor: c_float,
            output: *mut *mut u8,
        ) -> usize;
        pub fn WebPFree(ptr: *mut c_void);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32, alpha: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        let mut px = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                px.extend_from_slice(&[
                    (x * 255 / width.max(2).saturating_sub(1).max(1)) as u8,
                    (y * 255 / height.max(2).saturating_sub(1).max(1)) as u8,
                    ((x + y) % 256) as u8,
                    alpha(x, y),
                ]);
            }
        }
        px
    }

    fn is_webp(bytes: &[u8]) -> bool {
        bytes.len() >= 12
            && &bytes[..4] == b"RIFF"
            && &bytes[8..12] == b"WEBP"
            && u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize + 8 == bytes.len()
    }

    #[test]
    fn encodes_a_decodable_lossy_webp() {
        let px = gradient(64, 48, |_, _| 255);
        let webp = encode_rgba(&px, 64, 48, 80).unwrap();
        assert!(is_webp(&webp));
        assert_eq!(
            &webp[12..16],
            b"VP8 ",
            "opaque lossy WebP is a simple VP8 file"
        );
        let img = image::load_from_memory_with_format(&webp, image::ImageFormat::WebP).unwrap();
        assert_eq!((img.width(), img.height()), (64, 48));
        // Lossy but close: the mean error over the gradient stays small.
        let rgba = img.to_rgba8();
        let err: u64 = rgba
            .as_raw()
            .iter()
            .zip(&px)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        assert!(
            err / (px.len() as u64) < 8,
            "mean error {}",
            err / px.len() as u64
        );
    }

    #[test]
    fn keeps_alpha() {
        let px = gradient(32, 32, |x, _| if x < 16 { 0 } else { 255 });
        let webp = encode_rgba(&px, 32, 32, 90).unwrap();
        assert!(is_webp(&webp));
        assert_eq!(&webp[12..16], b"VP8X");
        let img = image::load_from_memory_with_format(&webp, image::ImageFormat::WebP).unwrap();
        let rgba = img.to_rgba8();
        assert_eq!(rgba.get_pixel(3, 3)[3], 0);
        assert_eq!(rgba.get_pixel(28, 3)[3], 255);
    }

    #[test]
    fn lower_quality_is_smaller_and_output_is_deterministic() {
        let px = gradient(128, 128, |_, _| 255);
        let high = encode_rgba(&px, 128, 128, 95).unwrap();
        let low = encode_rgba(&px, 128, 128, 60).unwrap();
        assert!(low.len() < high.len(), "{} vs {}", low.len(), high.len());
        assert_eq!(encode_rgba(&px, 128, 128, 60).unwrap(), low);
        // The quality range ends: 0 and 100 both encode.
        assert!(is_webp(&encode_rgba(&px, 128, 128, 0).unwrap()));
        assert!(is_webp(&encode_rgba(&px, 128, 128, 100).unwrap()));
    }

    #[test]
    fn tiny_and_edge_sizes_encode() {
        for (w, h) in [(1, 1), (2, 2), (1, 17), (17, 1)] {
            let px = gradient(w, h, |_, _| 255);
            let webp = encode_rgba(&px, w, h, 75).unwrap();
            let img = image::load_from_memory_with_format(&webp, image::ImageFormat::WebP).unwrap();
            assert_eq!((img.width(), img.height()), (w, h));
        }
    }

    #[test]
    fn bad_input_is_refused() {
        let px = gradient(4, 4, |_, _| 255);
        assert!(encode_rgba(&px, 0, 4, 80).is_err());
        assert!(encode_rgba(&px, 4, 0, 80).is_err());
        assert!(encode_rgba(&px, 4, 4, 101).is_err());
        assert!(encode_rgba(&px[..63], 4, 4, 80).is_err());
        assert!(encode_rgba(&px, 4, 3, 80).is_err());
        // A side over WebP's limit is refused before any pixel is read.
        assert!(encode_rgba(&[], MAX_DIMENSION + 1, 1, 80)
            .unwrap_err()
            .contains("side limit"));
        // The largest side WebP allows encodes.
        let row = vec![200u8; MAX_DIMENSION as usize * 4];
        assert!(is_webp(&encode_rgba(&row, MAX_DIMENSION, 1, 80).unwrap()));
    }
}
