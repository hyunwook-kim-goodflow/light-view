//! The worker's pixels as the bitmap Explorer takes.

use std::ffi::c_void;

use windows::core::Result;
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG};
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};

/// A 32 bpp top-down DIB section holding `bgra_premultiplied` (`width * height` pixels of
/// premultiplied BGRA, as the worker answers), for `WTSAT_ARGB`. The caller owns it.
pub fn to_hbitmap(width: u32, height: u32, bgra_premultiplied: &[u8]) -> Result<HBITMAP> {
    let len = u64::from(width) * u64::from(height) * 4;
    let (Ok(w), Ok(h)) = (i32::try_from(width), i32::try_from(height)) else {
        return Err(E_INVALIDARG.into());
    };
    if w == 0 || h == 0 || bgra_premultiplied.len() as u64 != len {
        return Err(E_INVALIDARG.into());
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative: top-down, row 0 first, as the worker's pixels are.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: a valid BITMAPINFO; `bits` receives the pixel pointer.
    let bitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
    if bits.is_null() {
        // SAFETY: we own the bitmap just made.
        let _ = unsafe { DeleteObject(bitmap) };
        return Err(E_FAIL.into());
    }
    // SAFETY: a 32 bpp DIB section of `w x h` has rows of exactly `4 * w` bytes (no padding),
    // so `len` writable bytes at `bits`; the source has `len` bytes (checked above).
    unsafe {
        std::ptr::copy_nonoverlapping(
            bgra_premultiplied.as_ptr(),
            bits.cast::<u8>(),
            bgra_premultiplied.len(),
        );
    }
    Ok(bitmap)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetObjectW, GetPixel, SelectObject, DIBSECTION,
    };

    /// The pixel GDI shows at (`x`, `y`) of `bitmap`, as 0x00BBGGRR.
    fn pixel_at(bitmap: HBITMAP, x: i32, y: i32) -> u32 {
        // SAFETY: a memory DC of our own; the bitmap is selected out again before it goes.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, bitmap);
            let color = GetPixel(dc, x, y);
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
            color.0
        }
    }

    /// The DIB section behind `bitmap` (header and pixels), then deletes it.
    pub(crate) fn take(bitmap: HBITMAP) -> (DIBSECTION, Vec<u8>) {
        let mut dib = DIBSECTION::default();
        // SAFETY: a valid bitmap handle and an out-buffer of the size passed.
        let got = unsafe {
            GetObjectW(
                bitmap,
                std::mem::size_of::<DIBSECTION>() as i32,
                Some(std::ptr::from_mut(&mut dib).cast()),
            )
        };
        assert_eq!(
            got as usize,
            std::mem::size_of::<DIBSECTION>(),
            "not a DIB section"
        );
        let len = dib.dsBm.bmWidthBytes as usize * dib.dsBm.bmHeight as usize;
        // SAFETY: a DIB section's bits stay valid until it is deleted, below.
        let pixels = unsafe { std::slice::from_raw_parts(dib.dsBm.bmBits.cast::<u8>(), len) };
        let pixels = pixels.to_vec();
        // SAFETY: we own the bitmap and delete it once.
        assert!(unsafe { DeleteObject(bitmap) }.as_bool());
        (dib, pixels)
    }

    #[test]
    fn bitmap_is_top_down_32bpp_with_the_pixels() {
        let (w, h) = (3u32, 2u32);
        let bgra: Vec<u8> = (0..w * h * 4).map(|i| i as u8).collect();
        let bitmap = to_hbitmap(w, h, &bgra).unwrap();
        // Top-down: the first pixel given (B 0, G 1, R 2) is the top left one, and the last
        // (B 20, G 21, R 22) the bottom right one.
        assert_eq!(pixel_at(bitmap, 0, 0), 0x0000_0102);
        assert_eq!(pixel_at(bitmap, 2, 1), 0x0014_1516);
        let (dib, pixels) = take(bitmap);
        assert_eq!(dib.dsBm.bmWidth, 3);
        assert_eq!(dib.dsBm.bmHeight, 2);
        assert_eq!(dib.dsBm.bmBitsPixel, 32);
        assert_eq!(pixels, bgra, "row 0 first, bytes as given");
    }

    #[test]
    fn wrong_sizes_are_refused() {
        assert!(to_hbitmap(0, 1, &[]).is_err());
        assert!(to_hbitmap(1, 0, &[]).is_err());
        assert!(to_hbitmap(2, 2, &[0; 15]).is_err());
        assert!(to_hbitmap(2, 2, &[0; 17]).is_err());
        assert!(to_hbitmap(u32::MAX, 1, &[0; 4]).is_err());
    }
}
