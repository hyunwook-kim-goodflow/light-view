//! Lossless JPEG rotation through TurboJPEG (libjpeg-turbo 3.x, the `tj3*` API).
//!
//! Only lv-worker links this crate: libjpeg-turbo is C code and runs in the sandboxed worker,
//! loaded as `turbojpeg.dll` from the worker's own folder.
//!
//! The rotation moves DCT coefficients, never pixels: nothing is decoded or re-compressed.
//! `TJXOPT_PERFECT` makes TurboJPEG refuse an image whose size is not a multiple of its MCU
//! where the rotation needs it ([`TransformError::NotPerfect`]), rather than leave the
//! partial MCUs in place. Every extra marker (Exif, ICC, XMP, IPTC, COM, other APPn) is
//! copied unchanged; fixing up Exif is the app's job. The output keeps the input's coding
//! (baseline or progressive, Huffman or arithmetic), with optimized Huffman tables so that it
//! stays about as large as the input (`lv_ipc::max_bytes_response`).
//!
//! TurboJPEG rebuilds the file from the coefficients and the saved markers and stops at EOI,
//! so anything after EOI (MPF secondary images, Ultra HDR / iOS gain maps, motion-photo
//! video) would be lost, and an MPF APP2 segment copied with offsets pointing at nothing.
//! Such files are refused with [`TransformError::HasTrailingData`] (ruling R7, checked by
//! `lv_core::jpeg_extent`); the app keeps their bytes and changes the EXIF orientation only.
//!
//! # Memory
//!
//! A quarter turn keeps two full sets of DCT coefficients in memory: the source's and the
//! rotated copy (libjpeg's transupp needs a workspace for every rotation, 180 degrees
//! included), 2 bytes per coefficient over the image padded to whole MCUs. That is 4 bytes per
//! pixel for grayscale, 6 for 4:2:0, 8 for 4:2:2 and 4:4:0, 12 for 4:4:4 and up to 16 for
//! CMYK. The worker's Job allows `lv_ipc::worker_memory_limit(N)` = `decoder_memory(N) + 4N`
//! for an `N`-pixel limit; [`rotate_lossless`] gives the coefficients
//! `lv_ipc::decoder_memory(max_pixels)` (at least `8 * max_pixels`, and 256 MiB) and refuses
//! a larger need up front as [`TransformError::TooLarge`] (TurboJPEG's `TJPARAM_MAXMEMORY`
//! backs that up). The remaining `4N` holds the input and the output buffer, which start at
//! `input + 1 MiB`, so any JPEG of up to about `2N` bytes fits, far more than real photos of
//! `N` pixels take. So the Job limit stays as M2 set it: 4:2:0, 4:2:2 and grayscale JPEGs
//! rotate at the full pixel limit, 4:4:4 up to two thirds of it, CMYK up to half; beyond
//! that the answer is `TooLarge`, never the Job killing the worker.

use std::ffi::{c_char, c_int, CStr};
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};

/// A clockwise rotation in quarter turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quarter {
    Cw90,
    Cw180,
    Cw270,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformError {
    /// The image size is not a multiple of its MCU where this rotation needs it.
    NotPerfect,
    /// Not a JPEG TurboJPEG can transform, damaged data included (warnings are errors here).
    Corrupt(String),
    /// More pixels than `max_pixels`, or more coefficient memory than its budget.
    TooLarge,
    /// Data after EOI, an MPF segment, or no EOI to be found (ruling R7): rotating would
    /// lose what follows the first image, so the pixels must stay as they are.
    HasTrailingData,
}

/// Most progressive scans accepted (libjpeg-turbo's suggested guard against crafted
/// progressive JPEGs with huge scan counts).
const MAX_SCANS: c_int = 500;

const MIB: u64 = 1024 * 1024;
/// Largest `TJPARAM_MAXMEMORY` TurboJPEG accepts on Windows: `LONG_MAX / 1 MiB`.
const MAX_MEMORY_MB: u64 = 2047;

/// Rotates `jpeg` losslessly by `q`. Fails with `TooLarge` before touching the image data
/// when the header reports more than `max_pixels` pixels.
///
/// Contract (ruling R7): `jpeg` must be one image and nothing else. Input with data after
/// EOI or an MPF APP2 (`"MPF\0"`) segment must not be pixel-rotated; it is refused with
/// `HasTrailingData` (as is input whose EOI cannot be found).
pub fn rotate_lossless(
    jpeg: &[u8],
    q: Quarter,
    max_pixels: u64,
) -> Result<Vec<u8>, TransformError> {
    panic::catch_unwind(AssertUnwindSafe(|| rotate(jpeg, q, max_pixels))).unwrap_or_else(
        |payload| {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(TransformError::Corrupt(format!(
                "rotation panicked: {what}"
            )))
        },
    )
}

fn rotate(jpeg: &[u8], q: Quarter, max_pixels: u64) -> Result<Vec<u8>, TransformError> {
    if jpeg.is_empty() {
        return Err(TransformError::Corrupt("empty file".into()));
    }
    let budget = lv_ipc::decoder_memory(max_pixels);
    let tj = Handle::new()?;
    tj.set(ffi::TJPARAM_STOPONWARNING, 1)?;
    tj.set(ffi::TJPARAM_SCANLIMIT, MAX_SCANS)?;
    tj.set(ffi::TJPARAM_MAXPIXELS, clamp_int(max_pixels))?;
    // libjpeg keeps the limit in a C `long` (32 bits on Windows), so TurboJPEG takes at most
    // 2047 MB; above that only the estimate below guards the budget.
    if budget / MIB <= MAX_MEMORY_MB {
        tj.set(ffi::TJPARAM_MAXMEMORY, clamp_int(budget / MIB))?;
    }
    tj.set(ffi::TJPARAM_SAVEMARKERS, ffi::SAVEMARKERS_ALL)?;

    // SAFETY: `jpeg` is a live slice of `jpeg.len()` bytes; TurboJPEG only reads it.
    if unsafe { ffi::tj3DecompressHeader(tj.raw(), jpeg.as_ptr(), jpeg.len()) } != 0 {
        return Err(TransformError::Corrupt(tj.error()));
    }
    let (width, height) = (
        tj.get(ffi::TJPARAM_JPEGWIDTH),
        tj.get(ffi::TJPARAM_JPEGHEIGHT),
    );
    // A tables-only stream has a valid header but no image.
    let (Ok(width @ 1..), Ok(height @ 1..)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(TransformError::Corrupt("no image in the JPEG data".into()));
    };
    if u64::from(width) * u64::from(height) > max_pixels {
        return Err(TransformError::TooLarge);
    }
    let subsamp = tj.get(ffi::TJPARAM_SUBSAMP);
    let colorspace = tj.get(ffi::TJPARAM_COLORSPACE);
    if coefficient_bytes(width, height, subsamp, colorspace) > budget {
        return Err(TransformError::TooLarge);
    }
    if lv_core::jpeg_extent::check_single_image(jpeg).is_err() {
        return Err(TransformError::HasTrailingData);
    }
    if let Some(mcu) = mcu_size(subsamp) {
        if !is_perfect(width, height, mcu, q) {
            return Err(TransformError::NotPerfect);
        }
    }

    let mut options = ffi::TJXOPT_PERFECT | ffi::TJXOPT_OPTIMIZE;
    if tj.get(ffi::TJPARAM_PROGRESSIVE) == 1 {
        options |= ffi::TJXOPT_PROGRESSIVE;
    }
    if tj.get(ffi::TJPARAM_ARITHMETIC) == 1 {
        options |= ffi::TJXOPT_ARITHMETIC;
    }
    let transform = ffi::TjTransform {
        r: ffi::TjRegion::default(),
        op: match q {
            Quarter::Cw90 => ffi::TJXOP_ROT90,
            Quarter::Cw180 => ffi::TJXOP_ROT180,
            Quarter::Cw270 => ffi::TJXOP_ROT270,
        },
        options,
        data: ptr::null_mut(),
        custom_filter: None,
    };
    let mut out = OutBuf::with_capacity(lv_ipc::max_bytes_response(jpeg.len() as u64));
    // SAFETY: `tj` is a transform instance whose header was read above; `jpeg` is a live
    // slice; `out.ptr`/`out.len` are a tj3Alloc'd buffer and its size (or null and 0), which
    // TurboJPEG may free and replace (TJPARAM_NOREALLOC is off) and `out` frees on drop;
    // `transform` is one valid tjtransform with no callback.
    let rc = unsafe {
        ffi::tj3Transform(
            tj.raw(),
            jpeg.as_ptr(),
            jpeg.len(),
            1,
            &mut out.ptr,
            &mut out.len,
            &transform,
        )
    };
    if rc != 0 {
        let msg = tj.error();
        return Err(if msg.contains("not perfect") {
            TransformError::NotPerfect
        } else {
            TransformError::Corrupt(msg)
        });
    }
    Ok(out.to_vec())
}

fn clamp_int(n: u64) -> c_int {
    c_int::try_from(n).unwrap_or(c_int::MAX)
}

/// iMCU (width, height) for a TurboJPEG subsampling level (`tjMCUWidth`/`tjMCUHeight`);
/// `None` when TurboJPEG cannot classify the sampling factors.
fn mcu_size(subsamp: c_int) -> Option<(u32, u32)> {
    const WIDTH: [u32; 9] = [8, 16, 16, 8, 8, 32, 8, 32, 16];
    const HEIGHT: [u32; 9] = [8, 8, 16, 8, 16, 8, 32, 16, 32];
    let i = usize::try_from(subsamp).ok().filter(|&i| i < WIDTH.len())?;
    Some((WIDTH[i], HEIGHT[i]))
}

/// libjpeg's `jtransform_perfect_transform` for the three rotations: a clockwise quarter
/// turn moves the bottom edge to the left, so the height must be whole MCUs; a
/// counter-clockwise one moves the right edge to the top; half a turn moves both.
fn is_perfect(width: u32, height: u32, (mcu_w, mcu_h): (u32, u32), q: Quarter) -> bool {
    let whole_w = width.is_multiple_of(mcu_w);
    let whole_h = height.is_multiple_of(mcu_h);
    match q {
        Quarter::Cw90 => whole_h,
        Quarter::Cw270 => whole_w,
        Quarter::Cw180 => whole_w && whole_h,
    }
}

/// Memory for the DCT coefficients of a quarter turn (see the crate docs): two sets of
/// 2 bytes per sample over the image padded to whole iMCUs. Components: 1 for grayscale,
/// 3 for YCbCr/RGB, 4 for CMYK/YCCK (two of them at full resolution); unknown sampling
/// counts every component at full resolution with the largest (32x32) iMCU.
fn coefficient_bytes(width: u32, height: u32, subsamp: c_int, colorspace: c_int) -> u64 {
    // Each chroma component's samples per luma sample, in eighths, by TJSAMP.
    const CHROMA_EIGHTHS: [u64; 9] = [8, 4, 2, 0, 4, 2, 2, 1, 1];
    let (mcu_w, mcu_h) = mcu_size(subsamp).unwrap_or((32, 32));
    let padded = u64::from(width.div_ceil(mcu_w) * mcu_w)
        * u64::from(height.div_ceil(mcu_h))
        * u64::from(mcu_h);
    let gray = subsamp == ffi::TJSAMP_GRAY || colorspace == ffi::TJCS_GRAY;
    let four = colorspace == ffi::TJCS_CMYK || colorspace == ffi::TJCS_YCCK;
    let (full, chroma) = match (gray, four) {
        (true, _) => (1, 0),
        (false, true) => (2, 2),
        (false, false) => (1, 2),
    };
    let chroma_eighths = usize::try_from(subsamp)
        .ok()
        .and_then(|i| CHROMA_EIGHTHS.get(i).copied())
        .unwrap_or(8);
    let samples_eighths = padded * (full * 8 + chroma * chroma_eighths);
    // 2 sets x 2 bytes per sample.
    samples_eighths * 4 / 8
}

/// A TurboJPEG transform instance, destroyed on drop.
struct Handle(NonNull<std::ffi::c_void>);

impl Handle {
    fn new() -> Result<Handle, TransformError> {
        // SAFETY: plain call; a non-null result is a new instance owned by `Handle`.
        let raw = unsafe { ffi::tj3InitVersion(ffi::TJINIT_TRANSFORM, ffi::API_VERSION) };
        NonNull::new(raw).map(Handle).ok_or_else(|| {
            // SAFETY: a null handle asks for the thread's last global error.
            TransformError::Corrupt(unsafe { error_string(ptr::null_mut()) })
        })
    }

    fn raw(&self) -> ffi::TjHandle {
        self.0.as_ptr()
    }

    fn set(&self, param: c_int, value: c_int) -> Result<(), TransformError> {
        // SAFETY: `self` is a live instance; tj3Set validates `param` and `value`.
        if unsafe { ffi::tj3Set(self.raw(), param, value) } == 0 {
            Ok(())
        } else {
            Err(TransformError::Corrupt(self.error()))
        }
    }

    fn get(&self, param: c_int) -> c_int {
        // SAFETY: `self` is a live instance; an unknown `param` returns -1.
        unsafe { ffi::tj3Get(self.raw(), param) }
    }

    fn error(&self) -> String {
        // SAFETY: `self` is a live instance.
        unsafe { error_string(self.raw()) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the instance came from tj3InitVersion and is destroyed exactly once.
        unsafe { ffi::tj3Destroy(self.raw()) }
    }
}

/// TurboJPEG's error message for `handle` (null: the global one).
///
/// # Safety
/// `handle` must be null or a live TurboJPEG instance.
unsafe fn error_string(handle: ffi::TjHandle) -> String {
    // SAFETY: the caller passes a valid handle or null; the result is a NUL-terminated
    // string owned by TurboJPEG (or null), copied before any further call.
    let msg: *const c_char = unsafe { ffi::tj3GetErrorStr(handle) };
    if msg.is_null() {
        return "unknown TurboJPEG error".to_string();
    }
    // SAFETY: non-null, NUL-terminated, valid until the next TurboJPEG call on this thread.
    let text = unsafe { CStr::from_ptr(msg) }
        .to_string_lossy()
        .into_owned();
    if text.is_empty() {
        "unknown TurboJPEG error".to_string()
    } else {
        text
    }
}

/// A TurboJPEG destination buffer (`tj3Alloc`), freed with `tj3Free` on drop.
struct OutBuf {
    ptr: *mut u8,
    len: usize,
}

impl OutBuf {
    /// Pre-allocates `capacity` bytes so the usual output needs no reallocation; if that
    /// fails, starts empty and lets TurboJPEG allocate.
    fn with_capacity(capacity: u64) -> OutBuf {
        let len = usize::try_from(capacity).unwrap_or(0);
        // SAFETY: plain allocation; null on failure.
        let ptr = unsafe { ffi::tj3Alloc(len) }.cast::<u8>();
        OutBuf {
            len: if ptr.is_null() { 0 } else { len },
            ptr,
        }
    }

    fn to_vec(&self) -> Vec<u8> {
        if self.ptr.is_null() || self.len == 0 {
            return Vec::new();
        }
        // SAFETY: after a successful tj3Transform, `ptr` holds `len` initialized bytes.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }.to_vec()
    }
}

impl Drop for OutBuf {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: `ptr` came from tj3Alloc or from TurboJPEG's reallocation, both
            // released with tj3Free, exactly once.
            unsafe { ffi::tj3Free(self.ptr.cast()) }
        }
    }
}

/// The few TurboJPEG 3.x declarations used here (turbojpeg.h of libjpeg-turbo 3.2.0).
mod ffi {
    use std::ffi::{c_char, c_int, c_uchar, c_void};

    pub type TjHandle = *mut c_void;

    /// `TURBOJPEG_VERSION_NUMBER`, which `tj3Init` passes to `tj3InitVersion`.
    pub const API_VERSION: c_int = 3_002_000;
    pub const TJINIT_TRANSFORM: c_int = 2;

    // enum TJPARAM
    pub const TJPARAM_STOPONWARNING: c_int = 0;
    pub const TJPARAM_SUBSAMP: c_int = 4;
    pub const TJPARAM_JPEGWIDTH: c_int = 5;
    pub const TJPARAM_JPEGHEIGHT: c_int = 6;
    pub const TJPARAM_COLORSPACE: c_int = 8;
    pub const TJPARAM_PROGRESSIVE: c_int = 12;
    pub const TJPARAM_SCANLIMIT: c_int = 13;
    pub const TJPARAM_ARITHMETIC: c_int = 14;
    pub const TJPARAM_MAXMEMORY: c_int = 23;
    pub const TJPARAM_MAXPIXELS: c_int = 24;
    pub const TJPARAM_SAVEMARKERS: c_int = 25;
    /// `TJPARAM_SAVEMARKERS` value: copy all extra markers.
    pub const SAVEMARKERS_ALL: c_int = 2;

    pub const TJSAMP_GRAY: c_int = 3;
    pub const TJCS_GRAY: c_int = 2;
    pub const TJCS_CMYK: c_int = 3;
    pub const TJCS_YCCK: c_int = 4;

    // enum TJXOP
    pub const TJXOP_ROT90: c_int = 5;
    pub const TJXOP_ROT180: c_int = 6;
    pub const TJXOP_ROT270: c_int = 7;

    pub const TJXOPT_PERFECT: c_int = 1 << 0;
    pub const TJXOPT_PROGRESSIVE: c_int = 1 << 5;
    pub const TJXOPT_ARITHMETIC: c_int = 1 << 7;
    pub const TJXOPT_OPTIMIZE: c_int = 1 << 8;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct TjRegion {
        pub x: c_int,
        pub y: c_int,
        pub w: c_int,
        pub h: c_int,
    }

    pub type CustomFilter = unsafe extern "C" fn(
        coeffs: *mut i16,
        array_region: TjRegion,
        plane_region: TjRegion,
        component_id: c_int,
        transform_id: c_int,
        transform: *mut TjTransform,
    ) -> c_int;

    #[repr(C)]
    pub struct TjTransform {
        pub r: TjRegion,
        pub op: c_int,
        pub options: c_int,
        pub data: *mut c_void,
        pub custom_filter: Option<CustomFilter>,
    }

    extern "C" {
        pub fn tj3InitVersion(init_type: c_int, api_version: c_int) -> TjHandle;
        pub fn tj3Destroy(handle: TjHandle);
        pub fn tj3GetErrorStr(handle: TjHandle) -> *mut c_char;
        pub fn tj3Set(handle: TjHandle, param: c_int, value: c_int) -> c_int;
        pub fn tj3Get(handle: TjHandle, param: c_int) -> c_int;
        pub fn tj3Alloc(bytes: usize) -> *mut c_void;
        pub fn tj3Free(buffer: *mut c_void);
        pub fn tj3DecompressHeader(
            handle: TjHandle,
            jpeg_buf: *const c_uchar,
            jpeg_size: usize,
        ) -> c_int;
        pub fn tj3Transform(
            handle: TjHandle,
            jpeg_buf: *const c_uchar,
            jpeg_size: usize,
            n: c_int,
            dst_bufs: *mut *mut c_uchar,
            dst_sizes: *mut usize,
            transforms: *const TjTransform,
        ) -> c_int;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coefficient_memory_per_pixel() {
        // 1024x1024 is whole MCUs for every subsampling.
        let px = 1024 * 1024;
        let per_pixel =
            |subsamp, colorspace| coefficient_bytes(1024, 1024, subsamp, colorspace) / px;
        let ycbcr = 1;
        assert_eq!(per_pixel(0, ycbcr), 12); // 4:4:4
        assert_eq!(per_pixel(1, ycbcr), 8); // 4:2:2
        assert_eq!(per_pixel(2, ycbcr), 6); // 4:2:0
        assert_eq!(per_pixel(3, ffi::TJCS_GRAY), 4);
        assert_eq!(per_pixel(0, ffi::TJCS_CMYK), 16);
        assert_eq!(per_pixel(-1, ycbcr), 12); // unknown: every component full size
                                              // Padding to whole MCUs counts: 17x9 at 4:2:0 is 32x16.
        assert_eq!(coefficient_bytes(17, 9, 2, ycbcr), 32 * 16 * 6);
    }

    #[test]
    fn budget_takes_common_jpegs_at_the_full_pixel_limit() {
        let n = 100_000_000u64;
        let budget = lv_ipc::decoder_memory(n);
        // 10000 x 10000 = n pixels.
        assert!(coefficient_bytes(10_000, 10_000, 2, 1) <= budget); // 4:2:0
        assert!(coefficient_bytes(10_000, 10_000, 3, ffi::TJCS_GRAY) <= budget);
        assert!(coefficient_bytes(10_000, 10_000, 0, 1) > budget); // 4:4:4
        assert!(coefficient_bytes(8_000, 8_000, 0, 1) <= budget); // 4:4:4 at 64%
                                                                  // The Job leaves 4n for the input and an output buffer of input + 1 MiB.
        assert_eq!(lv_ipc::worker_memory_limit(n) - budget, 4 * n);
    }

    #[test]
    fn perfect_rules_follow_transupp() {
        let mcu = (16, 8); // 4:2:2
        assert!(is_perfect(17, 8, mcu, Quarter::Cw90));
        assert!(!is_perfect(16, 9, mcu, Quarter::Cw90));
        assert!(is_perfect(16, 9, mcu, Quarter::Cw270));
        assert!(!is_perfect(17, 8, mcu, Quarter::Cw270));
        assert!(is_perfect(32, 16, mcu, Quarter::Cw180));
        assert!(!is_perfect(32, 17, mcu, Quarter::Cw180));
        assert_eq!(mcu_size(-1), None);
        assert_eq!(mcu_size(9), None);
        assert_eq!(mcu_size(2), Some((16, 16)));
    }
}
