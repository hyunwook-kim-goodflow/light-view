//! Safe wrapper around libheif (via `libheif-rs`) for light-view's HEIC worker.
//!
//! Only the worker process links this crate: libheif and libde265 are C/C++ code and are
//! loaded as DLLs (`heif.dll`, `libde265.dll`) next to the executable.

use std::panic::{self, AssertUnwindSafe};

use libheif_rs::{
    color_profile_types, ColorProfile, ColorSpace, DecodingOptions, HeifContext, ImageHandle,
    LibHeif, RgbChroma, SecurityLimits,
};

/// A decoded primary image: straight-alpha RGBA8, `irot`/`imir`/`clap` already applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeifImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no padding.
    pub rgba: Vec<u8>,
    pub color: ColorInfo,
}

/// Colour description attached to the primary image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorInfo {
    None,
    /// Raw ICC profile (`prof` or `rICC`).
    Icc(Vec<u8>),
    /// `nclx` code points (ITU-T H.273). Unknown values are reported as 2 (unspecified).
    Nclx {
        primaries: u16,
        transfer: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeifError {
    /// The primary image is bigger than the caller's `max_pixels`. Nothing was decoded.
    TooLarge { width: u32, height: u32 },
    /// Anything libheif could not read or decode, including internal panics.
    Corrupt(String),
}

/// Upper bounds handed to libheif before it parses anything (see [`security_limits`]).
const MAX_ITEMS: u32 = 1000;
const MAX_TILES: u64 = 4096;
const MAX_COLOR_PROFILE_BYTES: u32 = 4 * 1024 * 1024;
/// Floor of the single-allocation limit, so small pixel limits still take ordinary files.
const MIN_MEMORY_BLOCK_BYTES: u64 = 256 * 1024 * 1024;
/// Room on top of the RGBA output in one block (row alignment, padding).
const MEMORY_BLOCK_HEADROOM: u64 = 1024 * 1024;

/// Decodes the primary image of a HEIF file.
///
/// The display size (after transformations) and the coded size (`ispe`) are both checked
/// against `max_pixels` before any pixel is decoded. Never panics: libheif errors and
/// panics inside the bindings become [`HeifError::Corrupt`].
pub fn decode_heif(bytes: &[u8], max_pixels: u64) -> Result<HeifImage, HeifError> {
    match panic::catch_unwind(AssertUnwindSafe(|| decode_inner(bytes, max_pixels))) {
        Ok(result) => result,
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(HeifError::Corrupt(format!(
                "libheif wrapper panicked: {what}"
            )))
        }
    }
}

fn corrupt(context: &str) -> impl FnOnce(libheif_rs::HeifError) -> HeifError + '_ {
    move |e| HeifError::Corrupt(format!("{context}: {e}"))
}

/// Explicit libheif limits for one decode. Values that are not derived from `max_pixels`
/// are fixed so that a libheif upgrade cannot silently loosen them.
///
/// Memory, for `N = max_pixels` (the worker passes `N` already clamped to what one response
/// carries, `lv_ipc::max_response_pixels`, at most 2^28 - 4):
/// - single allocation: `max(4N + 1 MiB, 256 MiB)`. libheif allocates the interleaved RGBA
///   output (4 bytes per pixel, RGB 3) as one block, so a smaller limit would turn an image
///   within `N` into `Corrupt` (256 MiB alone stopped RGBA at about 67 MP, RGB at 89 MP).
/// - total: `lv_ipc::decoder_memory(N)` = `max(8N, 256 MiB)`, never below the block size:
///   YCbCr planes (1.5N for 8-bit 4:2:0, 3N for 10-bit) plus the RGBA output (4N).
/// - The worker's Job cap, `lv_ipc::worker_memory_limit(N)` = `max(12N, 256 MiB + 4N)`, is
///   this total plus the worker's own RGBA copy (4N), so libheif reaching its limit is an
///   error, not the Job killing the worker. libde265's own picture buffers (about 1.5N at
///   8 bits, 3N at 10) are not counted by libheif, but they are freed before the worker
///   makes its copy, so the decode peak (about 7N at 8 bits, 10N at 10) stays under 12N.
fn security_limits(max_pixels: u64) -> SecurityLimits {
    let mut limits = SecurityLimits::new();
    // 0 would disable the check, so keep at least one pixel.
    limits.set_max_image_size_pixels(max_pixels.max(1));
    limits.set_max_number_of_tiles(MAX_TILES);
    limits.set_max_items(MAX_ITEMS);
    limits.set_max_color_profile_size(MAX_COLOR_PROFILE_BYTES);
    let block = max_pixels
        .saturating_mul(4)
        .saturating_add(MEMORY_BLOCK_HEADROOM)
        .max(MIN_MEMORY_BLOCK_BYTES);
    limits.set_max_memory_block_size(block);
    limits.set_max_total_memory(lv_ipc::decoder_memory(max_pixels).max(block));
    limits
}

/// Decoding threads for one decode, given the number of logical CPUs: half of them (the
/// app runs two workers at once), at least 1 and at most 4.
fn threads_for(cpus: usize) -> u32 {
    (cpus / 2).clamp(1, 4) as u32
}

fn decoding_threads() -> u32 {
    threads_for(std::thread::available_parallelism().map_or(1, |n| n.get()))
}

fn decode_inner(bytes: &[u8], max_pixels: u64) -> Result<HeifImage, HeifError> {
    let lib = LibHeif::new();
    let mut ctx = HeifContext::new().map_err(corrupt("context"))?;
    ctx.set_security_limits(&security_limits(max_pixels))
        .map_err(corrupt("security limits"))?;
    // libheif's threads decode grid tiles in parallel; libde265's (below) split one HEVC
    // image. Both are capped because two workers decode at the same time.
    let threads = decoding_threads();
    ctx.set_max_decoding_threads(threads);
    ctx.read_bytes(bytes).map_err(corrupt("read"))?;
    let handle = ctx
        .primary_image_handle()
        .map_err(corrupt("primary image"))?;

    let (width, height) = (handle.width(), handle.height());
    // `ispe` is the coded size libheif allocates before `clap` crops it (-1 when absent).
    let coded = u64::try_from(handle.ispe_width()).unwrap_or(0)
        * u64::try_from(handle.ispe_height()).unwrap_or(0);
    if u64::from(width) * u64::from(height) > max_pixels || coded > max_pixels {
        return Err(HeifError::TooLarge { width, height });
    }
    if width == 0 || height == 0 {
        return Err(HeifError::Corrupt("image has no pixels".to_string()));
    }

    let has_alpha = handle.has_alpha_channel();
    let chroma = if has_alpha {
        RgbChroma::Rgba
    } else {
        RgbChroma::Rgb
    };
    let mut options =
        DecodingOptions::new().ok_or_else(|| HeifError::Corrupt("decoding options".into()))?;
    options.set_convert_hdr_to_8bit(true);
    options.set_num_codec_threads(threads);
    let image = lib
        .decode(&handle, ColorSpace::Rgb(chroma), Some(options))
        .map_err(corrupt("decode"))?;

    if image.width() != width || image.height() != height {
        return Err(HeifError::Corrupt(format!(
            "decoded {}x{}, expected {width}x{height}",
            image.width(),
            image.height()
        )));
    }
    let planes = image.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| HeifError::Corrupt("no interleaved plane".into()))?;
    if plane.storage_bits_per_pixel != if has_alpha { 32 } else { 24 } {
        return Err(HeifError::Corrupt(format!(
            "unexpected {} bits per pixel",
            plane.storage_bits_per_pixel
        )));
    }

    let mut rgba = to_rgba(plane.data, plane.stride, width, height, has_alpha)?;
    if has_alpha && image.is_premultiplied_alpha() {
        unpremultiply(&mut rgba);
    }

    Ok(HeifImage {
        width,
        height,
        rgba,
        color: color_info(&handle),
    })
}

/// Packs an interleaved RGB or RGBA plane (rows `stride` bytes apart) into tight RGBA8.
fn to_rgba(
    data: &[u8],
    stride: usize,
    width: u32,
    height: u32,
    has_alpha: bool,
) -> Result<Vec<u8>, HeifError> {
    let (w, h) = (width as usize, height as usize);
    let src_px = if has_alpha { 4 } else { 3 };
    let row_bytes = w * src_px;
    // `h >= 1`: zero-sized images are rejected before decoding.
    let needed = stride
        .checked_mul(h - 1)
        .and_then(|n| n.checked_add(row_bytes))
        .ok_or_else(|| HeifError::Corrupt("pixel plane size overflows".into()))?;
    if stride < row_bytes || data.len() < needed {
        return Err(HeifError::Corrupt("pixel plane is too small".into()));
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in data.chunks(stride).take(h) {
        let row = &row[..row_bytes];
        if has_alpha {
            rgba.extend_from_slice(row);
        } else {
            for px in row.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
    }
    Ok(rgba)
}

fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = u16::from(px[3]);
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

fn color_info(handle: &ImageHandle) -> ColorInfo {
    if let Some(raw) = handle.color_profile_raw() {
        let typ = raw.profile_type();
        if typ == color_profile_types::PROF || typ == color_profile_types::R_ICC {
            return ColorInfo::Icc(raw.data);
        }
    }
    if let Some(nclx) = handle.color_profile_nclx() {
        let primaries = match nclx.color_primaries() {
            libheif_rs::ColorPrimaries::Unknown => 2,
            p => p as u16,
        };
        let transfer = match nclx.transfer_characteristics() {
            libheif_rs::TransferCharacteristics::Unknown => 2,
            t => t as u16,
        };
        return ColorInfo::Nclx {
            primaries,
            transfer,
        };
    }
    ColorInfo::None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    /// The largest image a worker decodes at the default pixel limit (2^28, clamped to
    /// what one response carries).
    fn largest() -> u64 {
        lv_ipc::max_response_pixels(1 << 28)
    }

    #[test]
    fn one_block_holds_the_largest_allowed_rgba_image() {
        for n in [largest(), 100_000_000, 12_192_768] {
            let limits = security_limits(n);
            // libheif allocates the interleaved RGB(A) output as one block.
            assert!(limits.max_memory_block_size() >= n * 4, "{n} pixels");
            assert!(limits.max_total_memory() >= limits.max_memory_block_size());
        }
        assert_eq!(security_limits(1).max_memory_block_size(), 256 * MIB);
    }

    #[test]
    fn decoding_threads_are_half_the_cpus_from_one_to_four() {
        for (cpus, threads) in [(1, 1), (2, 1), (4, 2), (6, 3), (8, 4), (16, 4), (64, 4)] {
            assert_eq!(threads_for(cpus), threads, "{cpus} CPUs");
        }
    }

    #[test]
    fn plane_size_overflow_is_corrupt() {
        let data = [0u8; 64];
        for (stride, height) in [(usize::MAX / 2, 3), (usize::MAX, 2)] {
            assert!(
                matches!(
                    to_rgba(&data, stride, 4, height, true),
                    Err(HeifError::Corrupt(_))
                ),
                "stride {stride}, height {height}"
            );
        }
        // A plane that fits still packs.
        assert_eq!(to_rgba(&data, 16, 4, 2, true).unwrap().len(), 32);
    }

    #[test]
    fn job_limit_stays_above_what_libheif_may_allocate() {
        for n in [1, 1 << 20, 12_192_768, 40_000_000, 100_000_000, largest()] {
            let limits = security_limits(n);
            // libheif's own total plus the worker's RGBA copy fit in the Job's cap.
            assert!(
                limits.max_total_memory() + 4 * n <= lv_ipc::worker_memory_limit(n),
                "{n} pixels"
            );
        }
    }
}
