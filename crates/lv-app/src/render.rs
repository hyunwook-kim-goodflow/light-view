//! Direct2D drawing: an `ID2D1HwndRenderTarget` per window, coordinates in DIPs.

use lv_core::decode::DecodedImage;
use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DeviceContext, ID2D1Factory1, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_INTERPOLATION_MODE,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::theme::Rgb;

/// From this many device pixels per image pixel upward, magnify with nearest-neighbor.
const NEAREST_FROM_SCALE: f32 = 2.0;
const TEXT_SIZE_DIP: f32 = 16.0;

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

    /// Uploads `frame` of `img` as premultiplied BGRA, or clears the image.
    pub fn set_image(&mut self, img: Option<&DecodedImage>, frame: usize) {
        self.pixels = img.and_then(|img| {
            img.frames.get(frame).map(|f| Pixels {
                bgra: rgba_to_premultiplied_bgra(&f.rgba),
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
    #[allow(dead_code)] // first used by the toolbar (Task 9)
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
    // SAFETY: `p.bgra` holds exactly width*height*4 bytes with a stride of width*4,
    // and D2D copies the data before returning.
    unsafe { target.CreateBitmap(size, Some(p.bgra.as_ptr().cast()), p.width * 4, &props) }
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
