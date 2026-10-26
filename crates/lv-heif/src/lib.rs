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
const MAX_MEMORY_BLOCK_BYTES: u64 = 256 * 1024 * 1024;
const MIN_TOTAL_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

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
fn security_limits(max_pixels: u64) -> SecurityLimits {
    let mut limits = SecurityLimits::new();
    // 0 would disable the check, so keep at least one pixel.
    limits.set_max_image_size_pixels(max_pixels.max(1));
    limits.set_max_number_of_tiles(MAX_TILES);
    limits.set_max_items(MAX_ITEMS);
    limits.set_max_color_profile_size(MAX_COLOR_PROFILE_BYTES);
    limits.set_max_memory_block_size(MAX_MEMORY_BLOCK_BYTES);
    limits.set_max_total_memory(max_pixels.saturating_mul(12).max(MIN_TOTAL_MEMORY_BYTES));
    limits
}

fn decode_inner(bytes: &[u8], max_pixels: u64) -> Result<HeifImage, HeifError> {
    let lib = LibHeif::new();
    let mut ctx = HeifContext::new().map_err(corrupt("context"))?;
    ctx.set_security_limits(&security_limits(max_pixels))
        .map_err(corrupt("security limits"))?;
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
    if stride < row_bytes || data.len() < stride * (h - 1) + row_bytes {
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
