//! What the preview window shows: the worker's image, fitted into the window (aspect kept,
//! never enlarged, centered) over the background color the preview pane asks for.

use windows::core::Result;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject, ExcludeClipRect, FillRect,
    GetSysColor, RestoreDC, SaveDC, SelectObject, SetBrushOrgEx, SetStretchBltMode, StretchBlt,
    COLOR_WINDOW, HALFTONE, HBITMAP, HDC, SRCCOPY,
};

use crate::bitmap;

/// The worker's answer: `width * height` pixels of premultiplied BGRA, row 0 first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bgra: Vec<u8>,
}

/// Where an image of `image` (width, height) is drawn in `area`: as large as fits with its
/// aspect kept, never larger than the image itself, centered. Empty (at `area`'s top left)
/// when either is empty.
pub(crate) fn fit_rect(image: (u32, u32), area: RECT) -> RECT {
    let (w, h) = (i128::from(image.0), i128::from(image.1));
    let aw = i128::from(area.right) - i128::from(area.left);
    let ah = i128::from(area.bottom) - i128::from(area.top);
    if w == 0 || h == 0 || aw <= 0 || ah <= 0 {
        return RECT {
            left: area.left,
            top: area.top,
            right: area.left,
            bottom: area.top,
        };
    }
    let (dw, dh) = if w <= aw && h <= ah {
        (w, h)
    } else if w * ah >= h * aw {
        // Wider than the area, proportionally: the width fills it.
        (aw, ((h * aw + w / 2) / w).max(1))
    } else {
        (((w * ah + h / 2) / h).max(1), ah)
    };
    // `dw <= aw` and `dh <= ah`, so every value below lies within `area` and fits in i32.
    let left = i128::from(area.left) + (aw - dw) / 2;
    let top = i128::from(area.top) + (ah - dh) / 2;
    RECT {
        left: left as i32,
        top: top as i32,
        right: (left + dw) as i32,
        bottom: (top + dh) as i32,
    }
}

/// Composites premultiplied BGRA pixels over the opaque `background` (0x00BBGGRR), in place:
/// GDI's `StretchBlt` ignores alpha.
pub(crate) fn over_background(bgra: &mut [u8], background: COLORREF) {
    let [r, g, b, _] = background.0.to_le_bytes();
    for px in bgra.chunks_exact_mut(4) {
        let rest = 255 - u32::from(px[3]);
        if rest == 0 {
            continue;
        }
        for (c, under) in px[..3].iter_mut().zip([b, g, r]) {
            let blended = u32::from(*c) + (u32::from(under) * rest + 127) / 255;
            *c = blended.min(255) as u8;
        }
        px[3] = 255;
    }
}

/// A bitmap of ours, deleted with it. GDI objects belong to the process, not a thread.
struct OwnedBitmap(HBITMAP);

#[cfg(test)]
thread_local! {
    /// Bitmaps made on this thread and not deleted yet (tests: GDI may keep answering for a
    /// deleted handle for a while, so its handle cannot tell).
    static LIVE_BITMAPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Bitmaps of views made on this thread and not deleted yet.
#[cfg(test)]
pub(crate) fn live_bitmaps() -> usize {
    LIVE_BITMAPS.with(|n| n.get())
}

impl OwnedBitmap {
    fn new(bitmap: HBITMAP) -> OwnedBitmap {
        #[cfg(test)]
        LIVE_BITMAPS.with(|n| n.set(n.get() + 1));
        OwnedBitmap(bitmap)
    }
}

// SAFETY: a GDI bitmap handle may be used and deleted from any thread of the process.
unsafe impl Send for OwnedBitmap {}

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        // SAFETY: we made this bitmap and delete it once; it is not selected into a DC
        // (`paint` selects it out again).
        let deleted = unsafe { DeleteObject(self.0) };
        #[cfg(test)]
        if deleted.as_bool() {
            LIVE_BITMAPS.with(|n| n.set(n.get() - 1));
        }
        let _ = deleted;
    }
}

struct Shown {
    bitmap: OwnedBitmap,
    width: u32,
    height: u32,
    /// The premultiplied pixels again, kept only when some are not opaque, to composite
    /// them over another background later.
    translucent: Option<Vec<u8>>,
}

/// The preview window's content: the background color and the image shown, if any.
pub(crate) struct View {
    background: COLORREF,
    shown: Option<Shown>,
}

impl View {
    pub(crate) fn new() -> View {
        // SAFETY: plain call.
        let background = COLORREF(unsafe { GetSysColor(COLOR_WINDOW) });
        View {
            background,
            shown: None,
        }
    }

    /// Shows `image` instead of what was shown.
    pub(crate) fn show(&mut self, image: Image) -> Result<()> {
        self.shown = None;
        let Image {
            width,
            height,
            mut bgra,
        } = image;
        let translucent = bgra.chunks_exact(4).any(|px| px[3] != 255);
        let translucent = translucent.then(|| bgra.clone());
        over_background(&mut bgra, self.background);
        let bitmap = OwnedBitmap::new(bitmap::to_hbitmap(width, height, &bgra)?);
        self.shown = Some(Shown {
            bitmap,
            width,
            height,
            translucent,
        });
        Ok(())
    }

    /// Shows nothing (the bitmap is deleted).
    pub(crate) fn clear(&mut self) {
        self.shown = None;
    }

    pub(crate) fn set_background(&mut self, background: COLORREF) {
        self.background = background;
        let again = self.shown.as_ref().and_then(|s| {
            let bgra = s.translucent.clone()?;
            Some(Image {
                width: s.width,
                height: s.height,
                bgra,
            })
        });
        if let Some(image) = again {
            // On failure nothing is shown, which is all a failed preview shows anyway.
            let _ = self.show(image);
        }
    }

    /// The size of the image shown.
    #[cfg(test)]
    pub(crate) fn shown_size(&self) -> Option<(u32, u32)> {
        self.shown.as_ref().map(|s| (s.width, s.height))
    }

    #[cfg(test)]
    pub(crate) fn bitmap(&self) -> Option<HBITMAP> {
        self.shown.as_ref().map(|s| s.bitmap.0)
    }

    /// Paints `client` (the window's client area) of `dc`: the background, and the image
    /// fitted into it, smoothly scaled (`HALFTONE`).
    pub(crate) fn paint(&self, dc: HDC, client: RECT) {
        let target = self
            .shown
            .as_ref()
            .map(|s| (s, fit_rect((s.width, s.height), client)));
        // SAFETY: `dc` is a valid DC (BeginPaint's, or a test's memory DC); every object made
        // here is selected out and deleted again, and the DC's state is restored.
        unsafe {
            let saved = SaveDC(dc);
            if let Some((_, r)) = &target {
                // The background around the image only, so the image does not flicker.
                ExcludeClipRect(dc, r.left, r.top, r.right, r.bottom);
            }
            let brush = CreateSolidBrush(self.background);
            FillRect(dc, &client, brush);
            let _ = DeleteObject(brush);
            let _ = RestoreDC(dc, saved);
            let Some((shown, r)) = target else {
                return;
            };
            let memory = CreateCompatibleDC(dc);
            let old = SelectObject(memory, shown.bitmap.0);
            SetStretchBltMode(dc, HALFTONE);
            // HALFTONE needs the brush origin reset (SetStretchBltMode's documentation).
            let _ = SetBrushOrgEx(dc, 0, 0, None);
            let _ = StretchBlt(
                dc,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                memory,
                0,
                0,
                shown.width as i32,
                shown.height as i32,
                SRCCOPY,
            );
            SelectObject(memory, old);
            let _ = DeleteDC(memory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::CreateCompatibleBitmap;
    use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn preview_fit_rect_keeps_aspect_and_never_enlarges() {
        let area = rect(10, 20, 410, 320); // 400 x 300
                                           // Smaller than the area: its own size, centered.
        assert_eq!(fit_rect((100, 50), area), rect(160, 145, 260, 195));
        // Wider: fills the width, aspect kept (4000 x 1000 -> 400 x 100).
        assert_eq!(fit_rect((4000, 1000), area), rect(10, 120, 410, 220));
        // Taller: fills the height (1000 x 3000 -> 100 x 300).
        assert_eq!(fit_rect((1000, 3000), area), rect(160, 20, 260, 320));
        // Same aspect as the area: fills it exactly.
        assert_eq!(fit_rect((800, 600), area), area);
        // Exactly the area's size: unchanged.
        assert_eq!(fit_rect((400, 300), area), area);
        // Very thin images keep at least one pixel.
        assert_eq!(
            fit_rect((100_000, 1), area).bottom - fit_rect((100_000, 1), area).top,
            1
        );
        // An empty image or area draws nothing.
        assert_eq!(fit_rect((0, 10), area), rect(10, 20, 10, 20));
        assert_eq!(fit_rect((10, 10), rect(5, 5, 5, 50)), rect(5, 5, 5, 5));
        assert_eq!(fit_rect((10, 10), rect(5, 5, 0, 0)), rect(5, 5, 5, 5));
        // Extreme values do not overflow.
        let huge = rect(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let r = fit_rect((u32::MAX, 1), huge);
        assert_eq!((r.left, r.right), (i32::MIN, i32::MAX));
        for (w, h) in [
            (1, 1),
            (3, 7),
            (451, 461),
            (4032, 3024),
            (u32::MAX, u32::MAX),
        ] {
            let r = fit_rect((w, h), area);
            assert!(
                r.left >= area.left && r.right <= area.right,
                "{w}x{h}: {r:?}"
            );
            assert!(
                r.top >= area.top && r.bottom <= area.bottom,
                "{w}x{h}: {r:?}"
            );
            let (dw, dh) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
            assert!(dw <= w && dh <= h, "never enlarged: {w}x{h} -> {dw}x{dh}");
            // Aspect kept to within rounding.
            let off = (f64::from(dw) / f64::from(dh)) / (f64::from(w) / f64::from(h));
            assert!(
                (0.98..=1.02).contains(&off) || dw <= 2 || dh <= 2,
                "{w}x{h}: {dw}x{dh}"
            );
        }
    }

    #[test]
    fn translucent_pixels_are_composited_over_the_background() {
        // Opaque, fully transparent and half transparent premultiplied red.
        let mut px = vec![0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 128, 128];
        // 0x00BBGGRR: B 0x30, G 0x20, R 0x10.
        over_background(&mut px, COLORREF(0x0030_2010));
        assert_eq!(&px[0..4], &[0, 0, 255, 255], "opaque stays");
        assert_eq!(
            &px[4..8],
            &[0x30, 0x20, 0x10, 255],
            "transparent shows the background"
        );
        assert_eq!(&px[8..12], &[24, 16, 128 + 8, 255]);
    }

    /// The color of (`x`, `y`) of a `w x h` memory surface `paint`ed by `view`.
    fn painted(view: &View, w: i32, h: i32, points: &[(i32, i32)]) -> Vec<u32> {
        // SAFETY: a memory DC and bitmap of our own, selected out and deleted again.
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(screen);
            let surface = CreateCompatibleBitmap(screen, w, h);
            ReleaseDC(None, screen);
            let old = SelectObject(dc, surface);
            view.paint(dc, rect(0, 0, w, h));
            let colors = points.iter().map(|&(x, y)| GetPixel(dc, x, y).0).collect();
            SelectObject(dc, old);
            let _ = DeleteObject(surface);
            let _ = DeleteDC(dc);
            colors
        }
    }

    #[test]
    fn paint_centers_the_image_over_the_background() {
        let mut view = View::new();
        view.set_background(COLORREF(0x0000_FF00)); // green
        assert_eq!(painted(&view, 8, 8, &[(0, 0), (4, 4)]), [0xFF00, 0xFF00]);
        // A 2 x 2 opaque blue image in an 8 x 4 area: drawn at (3, 1)..(5, 3).
        view.show(Image {
            width: 2,
            height: 2,
            bgra: [255, 0, 0, 255].repeat(4),
        })
        .unwrap();
        let colors = painted(
            &view,
            8,
            4,
            &[(0, 0), (2, 1), (3, 1), (4, 2), (5, 2), (7, 3)],
        );
        let (green, blue) = (0x00_FF00, 0xFF_0000);
        assert_eq!(colors, [green, green, blue, blue, green, green]);
    }

    #[test]
    fn a_new_background_recomposites_a_translucent_image() {
        let mut view = View::new();
        view.set_background(COLORREF(0x0000_00FF)); // red
        view.show(Image {
            width: 1,
            height: 1,
            bgra: vec![0, 0, 0, 0],
        })
        .unwrap();
        assert_eq!(live_bitmaps(), 1);
        assert_eq!(painted(&view, 1, 1, &[(0, 0)]), [0x0000FF]);
        view.set_background(COLORREF(0x00FF_0000)); // blue
        assert_eq!(painted(&view, 1, 1, &[(0, 0)]), [0xFF0000]);
        assert_eq!(live_bitmaps(), 1, "the old bitmap is deleted");
        view.clear();
        assert_eq!(view.shown_size(), None);
        assert_eq!(live_bitmaps(), 0);
    }
}
