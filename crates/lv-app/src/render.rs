//! Direct2D drawing: an `ID2D1HwndRenderTarget` per window, coordinates in DIPs.

use lv_core::decode::DecodedImage;
use windows::core::{w, Interface, Result, HSTRING};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{BOOL, D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DeviceContext, ID2D1Factory1, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_INTERPOLATION_MODE,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::theme::Rgb;

/// From this many device pixels per image pixel upward, magnify with nearest-neighbor.
const NEAREST_FROM_SCALE: f32 = 2.0;
const TEXT_SIZE_DIP: f32 = 16.0;
/// Soft shadow layers (light mode): drawn from the widest, faintest layer inward.
const SHADOW_LAYERS: u32 = 7;

/// Horizontal placement of text inside its rect (always vertically centered).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Leading,
    Center,
    Trailing,
}

/// A styled piece of text; consecutive runs are laid out as one line.
#[derive(Clone, Copy, Debug)]
pub struct TextRun<'a> {
    pub text: &'a str,
    pub family: &'a str,
    pub size: f32,
    pub bold: bool,
    pub color: Rgb,
}

/// Premultiplied BGRA pixels kept so the bitmap can be rebuilt after device loss.
struct Pixels {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
}

/// Device-dependent resources; dropped and rebuilt on `D2DERR_RECREATE_TARGET`.
struct Device {
    target: ID2D1HwndRenderTarget,
    dc: ID2D1DeviceContext,
    brush: ID2D1SolidColorBrush,
    bitmap: Option<ID2D1Bitmap>,
}

pub struct Renderer {
    hwnd: HWND,
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    text_format: IDWriteTextFormat,
    dpi: f32,
    pixels: Option<Pixels>,
    device: Option<Device>,
}

impl Renderer {
    pub fn new(hwnd: HWND) -> Result<Renderer> {
        // SAFETY: plain factory constructors; no pointers are passed.
        let factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        // SAFETY: as above.
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        // SAFETY: the font family and locale are static NUL-terminated strings.
        let text_format = unsafe {
            let f = dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                TEXT_SIZE_DIP,
                w!("ko-KR"),
            )?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f
        };
        // SAFETY: `hwnd` is the caller's live window.
        let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
        let mut r = Renderer {
            hwnd,
            factory,
            dwrite,
            text_format,
            dpi: if dpi > 0.0 { dpi } else { 96.0 },
            pixels: None,
            device: None,
        };
        r.ensure_device()?;
        Ok(r)
    }

    /// Client size in device pixels.
    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(d) = &self.device {
            let size = D2D_SIZE_U {
                width: w,
                height: h,
            };
            // SAFETY: `size` is a live local.
            if unsafe { d.target.Resize(&size) }.is_err() {
                self.device = None;
            }
        }
    }

    /// Window DPI changed (`WM_DPICHANGED`); drawing stays in DIPs.
    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi;
        if let Some(d) = &self.device {
            // SAFETY: plain value call on a live render target.
            unsafe { d.target.SetDpi(dpi, dpi) };
        }
    }

    /// Uploads `frame` of `img` as premultiplied BGRA, or clears the image. A frame whose
    /// buffer length does not match `width * height * 4` is treated as no image.
    pub fn set_image(&mut self, img: Option<&DecodedImage>, frame: usize) {
        self.pixels = img.and_then(|img| {
            let f = img.frames.get(frame)?;
            Some(Pixels {
                bgra: premultiplied_bgra_for(&f.rgba, img.width, img.height)?,
                width: img.width,
                height: img.height,
            })
        });
        if let Some(d) = &mut self.device {
            d.bitmap = self
                .pixels
                .as_ref()
                .and_then(|p| create_bitmap(&d.target, p).ok());
        }
    }

    /// Whether an image is set and its bitmap could be created (it may exceed the
    /// device's maximum bitmap size).
    pub fn has_image(&self) -> bool {
        match &self.device {
            Some(d) => d.bitmap.is_some(),
            None => self.pixels.is_some(),
        }
    }

    pub fn begin(&mut self, bg: Rgb) {
        if self.ensure_device().is_err() {
            return;
        }
        if let Some(d) = &self.device {
            // SAFETY: drawing calls on a live render target; the color is a live local.
            unsafe {
                d.target.BeginDraw();
                d.target.Clear(Some(&bg.to_d2d()));
            }
        }
    }

    /// `rect` is x, y, w, h in DIPs.
    pub fn draw_image(&mut self, rect: (f32, f32, f32, f32)) {
        let (Some(d), Some(p)) = (&self.device, &self.pixels) else {
            return;
        };
        let Some(bitmap) = &d.bitmap else { return };
        let mode = interpolation_for(rect.2 * self.dpi / 96.0 / p.width as f32);
        let dest = to_rect_f(rect);
        // SAFETY: `dest` is a live local; `bitmap` belongs to this device context.
        unsafe { d.dc.DrawBitmap(bitmap, Some(&dest), 1.0, mode, None, None) };
    }

    /// Draws `text` centered in the whole client area.
    pub fn draw_text_center(&mut self, text: &str, color: Rgb) {
        let Some(d) = &self.device else { return };
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain calls on live COM objects; `wide` and `layout` outlive the call.
        unsafe {
            let size = d.target.GetSize();
            let layout = to_rect_f((0.0, 0.0, size.width, size.height));
            d.brush.SetColor(&color.to_d2d());
            d.target.DrawText(
                &wide,
                &self.text_format,
                &layout,
                &d.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// `rect` is x, y, w, h in DIPs.
    pub fn fill_round_rect(&mut self, rect: (f32, f32, f32, f32), radius: f32, color: Rgb) {
        let Some(d) = &self.device else { return };
        let rr = D2D1_ROUNDED_RECT {
            rect: to_rect_f(rect),
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: `rr` and the color are live locals.
        unsafe {
            d.brush.SetColor(&color.to_d2d());
            d.target.FillRoundedRectangle(&rr, &d.brush);
        }
    }

    /// A `width`-DIP outline whose stroke lies inside `rect`.
    pub fn stroke_round_rect(
        &mut self,
        rect: (f32, f32, f32, f32),
        radius: f32,
        color: Rgb,
        width: f32,
    ) {
        let Some(d) = &self.device else { return };
        let half = width / 2.0;
        let (x, y, w, h) = rect;
        let rr = D2D1_ROUNDED_RECT {
            rect: to_rect_f((x + half, y + half, w - width, h - width)),
            radiusX: (radius - half).max(0.0),
            radiusY: (radius - half).max(0.0),
        };
        // SAFETY: `rr` and the color are live locals.
        unsafe {
            d.brush.SetColor(&color.to_d2d());
            d.target.DrawRoundedRectangle(&rr, &d.brush, width, None);
        }
    }

    /// A soft drop shadow under the rounded `rect`: offset down by `offset_y`, spread
    /// over `blur` DIPs, peaking at `alpha` (like CSS `0 4px 14px rgba(c, .08)`).
    pub fn fill_soft_shadow(
        &mut self,
        rect: (f32, f32, f32, f32),
        radius: f32,
        color: Rgb,
        (offset_y, blur, alpha): (f32, f32, f32),
    ) {
        let Some(d) = &self.device else { return };
        // Stacked layers: where all overlap the opacity sums to `alpha`.
        let layer_alpha = 1.0 - (1.0 - alpha).powf(1.0 / SHADOW_LAYERS as f32);
        let c = color.to_d2d();
        let (x, y, w, h) = rect;
        for i in (0..SHADOW_LAYERS).rev() {
            let spread = blur / 2.0 * (i as f32 + 1.0) / SHADOW_LAYERS as f32;
            let rr = D2D1_ROUNDED_RECT {
                rect: to_rect_f((
                    x - spread,
                    y + offset_y - spread,
                    w + 2.0 * spread,
                    h + 2.0 * spread,
                )),
                radiusX: radius + spread,
                radiusY: radius + spread,
            };
            // SAFETY: `rr` and the color are live locals.
            unsafe {
                d.brush.SetColor(&D2D1_COLOR_F {
                    a: layer_alpha,
                    ..c
                });
                d.target.FillRoundedRectangle(&rr, &d.brush);
            }
        }
    }

    /// Whether `family` is installed (system font collection).
    pub fn font_family_exists(&self, family: &str) -> bool {
        let name = HSTRING::from(family);
        let mut collection: Option<IDWriteFontCollection> = None;
        let (mut index, mut exists) = (0u32, BOOL(0));
        // SAFETY: out-pointers are live locals; `name` outlives the call.
        unsafe {
            self.dwrite
                .GetSystemFontCollection(&mut collection, false)
                .is_ok()
                && collection.is_some_and(|c| {
                    c.FindFamilyName(&name, &mut index, &mut exists).is_ok() && exists.as_bool()
                })
        }
    }

    /// Draws `runs` as one unwrapped line in `rect` (x, y, w, h in DIPs), vertically
    /// centered and clipped to the rect.
    pub fn draw_runs(&mut self, rect: (f32, f32, f32, f32), runs: &[TextRun], align: Align) {
        if let Err(e) = self.try_draw_runs(rect, runs, align, false) {
            eprintln!("light-view: text drawing failed: {e}");
        }
    }

    /// Draws one icon-font glyph centered in `rect`, optionally mirrored horizontally.
    pub fn draw_glyph(
        &mut self,
        rect: (f32, f32, f32, f32),
        glyph: char,
        family: &str,
        size: f32,
        color: Rgb,
        mirrored: bool,
    ) {
        let mut buf = [0u8; 4];
        let run = TextRun {
            text: glyph.encode_utf8(&mut buf),
            family,
            size,
            bold: false,
            color,
        };
        if let Err(e) = self.try_draw_runs(rect, &[run], Align::Center, mirrored) {
            eprintln!("light-view: glyph drawing failed: {e}");
        }
    }

    fn try_draw_runs(
        &mut self,
        rect: (f32, f32, f32, f32),
        runs: &[TextRun],
        align: Align,
        mirrored: bool,
    ) -> Result<()> {
        let (Some(d), Some(first)) = (&self.device, runs.first()) else {
            return Ok(());
        };
        let text: String = runs.iter().map(|r| r.text).collect();
        let wide: Vec<u16> = text.encode_utf16().collect();
        let (x, y, w, h) = rect;
        if w <= 0.0 || h <= 0.0 {
            return Ok(());
        }
        // SAFETY: plain calls on live COM objects; every string, range and brush passed
        // is a live local that outlives the call using it.
        unsafe {
            let format = self.dwrite.CreateTextFormat(
                &HSTRING::from(first.family),
                None,
                weight(first.bold),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                first.size,
                w!("ko-KR"),
            )?;
            format.SetTextAlignment(text_alignment(align))?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let layout = self.dwrite.CreateTextLayout(&wide, &format, w, h)?;
            let mut start = 0u32;
            for run in runs {
                let length = run.text.encode_utf16().count() as u32;
                let range = DWRITE_TEXT_RANGE {
                    startPosition: start,
                    length,
                };
                layout.SetFontFamilyName(&HSTRING::from(run.family), range)?;
                layout.SetFontSize(run.size, range)?;
                layout.SetFontWeight(weight(run.bold), range)?;
                let brush = d.target.CreateSolidColorBrush(&run.color.to_d2d(), None)?;
                layout.SetDrawingEffect(&brush, range)?;
                start += length;
            }
            if mirrored {
                // x' = 2x + w - x: reflects about the rect's vertical center line.
                let m = Matrix3x2 {
                    M11: -1.0,
                    M22: 1.0,
                    M31: 2.0 * x + w,
                    ..Default::default()
                };
                d.target.SetTransform(&m);
            }
            d.brush.SetColor(&first.color.to_d2d());
            d.target.DrawTextLayout(
                D2D_POINT_2F { x, y },
                &layout,
                &d.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
            );
            if mirrored {
                d.target.SetTransform(&Matrix3x2::identity());
            }
        }
        Ok(())
    }

    /// Presents the frame. On device loss the device resources are dropped, the window
    /// is invalidated, and the next `begin` rebuilds them.
    pub fn end(&mut self) -> Result<()> {
        let Some(d) = &self.device else {
            return Ok(());
        };
        // SAFETY: matches the `BeginDraw` in `begin`.
        let result = unsafe { d.target.EndDraw(None, None) };
        match result {
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                self.device = None;
                // SAFETY: `hwnd` is this renderer's live window.
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
                Ok(())
            }
            other => other,
        }
    }

    fn ensure_device(&mut self) -> Result<()> {
        if self.device.is_some() {
            return Ok(());
        }
        let mut rc = RECT::default();
        // SAFETY: `rc` is a live local.
        unsafe { GetClientRect(self.hwnd, &mut rc)? };
        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: self.dpi,
            dpiY: self.dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U {
                width: (rc.right - rc.left).max(0) as u32,
                height: (rc.bottom - rc.top).max(0) as u32,
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        // SAFETY: both property structs are live locals.
        let target = unsafe {
            self.factory
                .CreateHwndRenderTarget(&rt_props, &hwnd_props)?
        };
        // On Windows 8+ an HWND render target created from ID2D1Factory1 is a device
        // context, which gives us HIGH_QUALITY_CUBIC interpolation.
        let dc: ID2D1DeviceContext = target.cast()?;
        // SAFETY: the color is a live local.
        let brush = unsafe { target.CreateSolidColorBrush(&Rgb(0, 0, 0).to_d2d(), None)? };
        let bitmap = self
            .pixels
            .as_ref()
            .and_then(|p| create_bitmap(&target, p).ok());
        self.device = Some(Device {
            target,
            dc,
            brush,
            bitmap,
        });
        Ok(())
    }
}

fn create_bitmap(target: &ID2D1HwndRenderTarget, p: &Pixels) -> Result<ID2D1Bitmap> {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    let size = D2D_SIZE_U {
        width: p.width,
        height: p.height,
    };
    // SAFETY: `Pixels` is only built by `premultiplied_bgra_for`, which guarantees
    // `p.bgra.len() == width * height * 4` and that the stride `width * 4` fits in u32;
    // D2D copies the data before returning.
    unsafe { target.CreateBitmap(size, Some(p.bgra.as_ptr().cast()), p.width * 4, &props) }
}

fn weight(bold: bool) -> DWRITE_FONT_WEIGHT {
    if bold {
        DWRITE_FONT_WEIGHT_BOLD
    } else {
        DWRITE_FONT_WEIGHT_NORMAL
    }
}

fn text_alignment(align: Align) -> DWRITE_TEXT_ALIGNMENT {
    match align {
        Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
        Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        Align::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
    }
}

fn to_rect_f((x, y, w, h): (f32, f32, f32, f32)) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

/// `device_scale` = device pixels per image pixel.
fn interpolation_for(device_scale: f32) -> D2D1_INTERPOLATION_MODE {
    if device_scale >= NEAREST_FROM_SCALE {
        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
    } else {
        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC
    }
}

/// Converts a `width` x `height` straight-alpha RGBA8 frame, or `None` if the buffer
/// length does not match (or the size overflows), so D2D never reads past it.
fn premultiplied_bgra_for(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    width.checked_mul(4)?;
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    (rgba.len() == expected).then(|| rgba_to_premultiplied_bgra(rgba))
}

/// Straight-alpha RGBA8 to premultiplied BGRA8 (rounded).
fn rgba_to_premultiplied_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len() / 4 * 4];
    for (src, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let a = u32::from(src[3]);
        let mul = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
        dst[0] = mul(src[2]);
        dst[1] = mul(src[1]);
        dst[2] = mul(src[0]);
        dst[3] = src[3];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies_and_swaps_to_bgra() {
        let rgba = [255, 128, 0, 255, 200, 100, 50, 128, 10, 20, 30, 0];
        let out = rgba_to_premultiplied_bgra(&rgba);
        assert_eq!(out, vec![0, 128, 255, 255, 25, 50, 100, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn frame_buffer_must_match_dimensions() {
        let rgba = [1u8, 2, 3, 255, 4, 5, 6, 255];
        assert_eq!(
            premultiplied_bgra_for(&rgba, 2, 1),
            Some(vec![3, 2, 1, 255, 6, 5, 4, 255])
        );
        assert_eq!(premultiplied_bgra_for(&rgba, 2, 2), None); // too short
        assert_eq!(premultiplied_bgra_for(&rgba, 1, 1), None); // too long
        assert_eq!(premultiplied_bgra_for(&rgba, u32::MAX, u32::MAX), None); // overflow
        assert_eq!(premultiplied_bgra_for(&[], 0, 0), Some(vec![]));
    }

    #[test]
    fn nearest_neighbor_from_2x_device_scale() {
        assert_eq!(
            interpolation_for(2.0),
            D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
        );
        assert_eq!(
            interpolation_for(8.0),
            D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
        );
        assert_eq!(
            interpolation_for(1.99),
            D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC
        );
        assert_eq!(
            interpolation_for(0.25),
            D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC
        );
    }
}
