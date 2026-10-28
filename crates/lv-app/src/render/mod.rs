//! Direct2D drawing: an `ID2D1HwndRenderTarget` per window, coordinates in DIPs.

use lv_core::prefetch::DecodeResult;
use lv_core::tiling::mip_level_for_scale;
use windows::core::{w, Interface, Result};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DeviceContext, ID2D1Factory1, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_INTERPOLATION_MODE,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, PostMessageW, WM_APP};

use self::mips::{mips_to_build, MipJob, MipLevel};
use self::tiles::{Pixels, TiledBitmap};
use crate::theme::Rgb;

mod mips;
mod text;
mod thumbs;
mod tiles;

/// Posted to the window by a mip build once its levels are ready to upload; the
/// window only needs repainting.
pub const WM_APP_MIPS_READY: u32 = WM_APP + 7;

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

/// Device-dependent resources; dropped and rebuilt on `D2DERR_RECREATE_TARGET`.
struct Device {
    target: ID2D1HwndRenderTarget,
    dc: ID2D1DeviceContext,
    brush: ID2D1SolidColorBrush,
    /// The image's full-size tiles.
    image: Option<TiledBitmap>,
    /// Levels 1, 2, … of the image, once built (tiled images only).
    mips: Vec<TiledBitmap>,
    /// Small bitmaps by id (the resize screen's filmstrip tiles).
    thumbs: thumbs::ThumbCache,
}

pub struct Renderer {
    hwnd: HWND,
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    text_format: IDWriteTextFormat,
    dpi: f32,
    /// Size of the image last set. No pixel copy is kept: after device loss the
    /// caller uploads again (see `needs_upload`).
    image: Option<(u32, u32)>,
    device: Option<Device>,
    /// The mip build for the image set last, until its levels are uploaded.
    mip_job: Option<MipJob>,
    /// The tiled image's decoded frame, kept (shared, not copied) so deeper mip levels
    /// can be built when the window gets smaller.
    mip_source: Option<(DecodeResult, usize)>,
    /// Clips pushed and not yet popped.
    clips: u32,
}

/// An image is set but its bitmap is missing (the device was lost, or not yet built).
fn needs_upload_for(image: Option<(u32, u32)>, has_bitmap: bool) -> bool {
    image.is_some() && !has_bitmap
}

impl Renderer {
    pub fn new(hwnd: HWND) -> Result<Renderer> {
        // SAFETY: plain factory constructors; no pointers are passed.
        let factory: ID2D1Factory1 = crate::timing::step("d2d.factory", || unsafe {
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
        })?;
        // SAFETY: as above.
        let dwrite: IDWriteFactory = crate::timing::step("d2d.dwrite", || unsafe {
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
        })?;
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
            image: None,
            device: None,
            mip_job: None,
            mip_source: None,
            clips: 0,
        };
        crate::timing::step("d2d.target", || r.ensure_device())?;
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
        // A smaller window may need deeper mip levels to fit the image (a DPI change
        // arrives here too, with the window's new size); a minimized one (0×0) none.
        self.ensure_mips((w, h));
    }

    /// Window DPI changed (`WM_DPICHANGED`); drawing stays in DIPs.
    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi;
        if let Some(d) = &self.device {
            // SAFETY: plain value call on a live render target.
            unsafe { d.target.SetDpi(dpi, dpi) };
        }
    }

    /// Uploads `frame` of `img` (already premultiplied BGRA) as is, or clears the image.
    /// A frame whose buffer length does not match `width * height * 4` is treated as no
    /// image. An image larger than one tile also gets mip levels for fitting it in the
    /// window, built on another thread (see `WM_APP_MIPS_READY`).
    pub fn set_image(&mut self, img: Option<&DecodeResult>, frame: usize) {
        // Cancels the previous image's build.
        self.mip_job = None;
        self.mip_source = None;
        let decoded = img.and_then(|r| r.as_ref().as_ref().ok());
        let pixels = decoded.and_then(|img| {
            let f = img.frames.get(frame)?;
            Pixels::new(&f.bgra, img.width, img.height)
        });
        self.image = pixels.as_ref().map(|p| (p.width, p.height));
        // A lost device is rebuilt here so the bitmaps can be created right away.
        let _ = self.ensure_device();
        let Some(d) = &mut self.device else { return };
        d.mips.clear();
        d.image = pixels
            .as_ref()
            .and_then(|p| TiledBitmap::upload(&d.target, p).ok());
        let (Some(source), Some(decoded), Some(tiled)) = (img, decoded, &d.image) else {
            return;
        };
        // One bitmap is downscaled well by the cubic filter; animations change too
        // often to be worth it.
        if tiled.tile_count() < 2 || decoded.frames.len() != 1 {
            return;
        }
        // SAFETY: plain query on a live render target.
        let view = unsafe { d.target.GetPixelSize() };
        self.mip_source = Some((source.clone(), frame));
        self.ensure_mips((view.width, view.height));
    }

    /// Starts building the mip levels fitting the tiled image in a `view`-pixel window
    /// needs, beyond those uploaded or already being built.
    fn ensure_mips(&mut self, view: (u32, u32)) {
        if self.mip_source.is_none() {
            return;
        }
        // Levels already finished are uploaded first, so a new build starts after them.
        self.take_mips();
        let (Some(d), Some((source, frame))) = (&self.device, &self.mip_source) else {
            return;
        };
        let Some(full) = &d.image else { return };
        let have = u8::try_from(d.mips.len() + 1).unwrap_or(u8::MAX);
        let planned = self.mip_job.as_ref().map(MipJob::levels);
        let Some((first, levels)) = mips_to_build((full.width, full.height), view, have, planned)
        else {
            return;
        };
        let (source, frame) = (source.clone(), *frame);
        let hwnd = self.hwnd.0 as isize;
        // Replaces (cancels) a shallower build.
        self.mip_job = Some(MipJob::spawn(source, frame, (first, levels), move || {
            // SAFETY: plain call; posting to a destroyed window just fails.
            unsafe {
                let _ = PostMessageW(
                    HWND(hwnd as *mut _),
                    WM_APP_MIPS_READY,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }));
    }

    /// Whether an image is set and its bitmaps could be created (video memory may run
    /// out).
    pub fn has_image(&self) -> bool {
        match &self.device {
            Some(d) => self.image.is_some() && d.image.is_some(),
            // No device to try yet; `needs_upload` asks again once there is one.
            None => self.image.is_some(),
        }
    }

    /// The device was rebuilt since the image was set: call `set_image` again (which
    /// also rebuilds the mip levels).
    pub fn needs_upload(&self) -> bool {
        let has_bitmap = self.device.as_ref().is_some_and(|d| d.image.is_some());
        needs_upload_for(self.image, has_bitmap)
    }

    /// Uploads the mip levels finished so far, freeing each one's pixels right after
    /// its upload. If one cannot be created, the build is dropped and the image keeps
    /// drawing from the levels before it.
    fn take_mips(&mut self) {
        let Some(job) = &self.mip_job else { return };
        let ready = job.take_ready();
        let last = job.levels().saturating_sub(1);
        let Some(d) = &mut self.device else { return };
        if d.image.is_none() {
            return;
        }
        for (level, mip) in ready {
            // Levels come in order; one not following the uploaded ones is from a build
            // that was replaced.
            if usize::from(level) != d.mips.len() + 1 {
                continue;
            }
            match upload_level(&d.target, &mip) {
                Some(bitmap) => d.mips.push(bitmap),
                None => {
                    self.mip_job = None;
                    return;
                }
            }
            if level >= last {
                self.mip_job = None;
                return;
            }
        }
    }

    pub fn begin(&mut self, bg: Rgb) {
        self.clips = 0;
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

    /// `rect` (x, y, w, h in DIPs) is the on-screen box of the image after turning it
    /// clockwise by `quarters` × 90° about its centre. Draws from the mip level that
    /// suits the scale, and only the tiles inside the window.
    pub fn draw_image(&mut self, rect: (f32, f32, f32, f32), quarters: u8) {
        self.take_mips();
        let (Some(d), Some((image_w, _))) = (&self.device, self.image) else {
            return;
        };
        let Some(full) = &d.image else { return };
        let (dest, transform) = unrotated_dest(rect, quarters);
        let px_per_dip = self.dpi / 96.0;
        let levels = u8::try_from(d.mips.len() + 1).unwrap_or(u8::MAX);
        let level = mip_level_for_scale(dest.2 * px_per_dip / image_w as f32, levels);
        let bitmap = match level {
            0 => full,
            l => &d.mips[usize::from(l) - 1],
        };
        let mode = interpolation_for(dest.2 * px_per_dip / bitmap.width as f32);
        // SAFETY: plain query on a live render target.
        let size = unsafe { d.target.GetSize() };
        let window = to_rect_f((0.0, 0.0, size.width, size.height));
        // The window in the unturned bitmap's space: turned back the other way.
        let center = (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
        let visible = rotate_rect_cw(window, center, 4 - quarters % 4);
        if let Some(m) = &transform {
            // SAFETY: `m` is a live local.
            unsafe { d.target.SetTransform(m) };
        }
        bitmap.draw(&d.target, &d.dc, dest, &visible, mode);
        if transform.is_some() {
            // SAFETY: as above.
            unsafe { d.target.SetTransform(&Matrix3x2::identity()) };
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

    /// Fills `rect` (x, y, w, h in DIPs) with `color` at `alpha` opacity.
    pub fn fill_rect_alpha(&mut self, rect: (f32, f32, f32, f32), color: Rgb, alpha: f32) {
        let Some(d) = &self.device else { return };
        let c = D2D1_COLOR_F {
            a: alpha,
            ..color.to_d2d()
        };
        // SAFETY: the rect and color are live locals.
        unsafe {
            d.brush.SetColor(&c);
            d.target.FillRectangle(&to_rect_f(rect), &d.brush);
        }
    }

    /// Clips what is drawn next to `rect` (DIPs) until [`pop_clip`](Self::pop_clip).
    pub fn push_clip(&mut self, rect: (f32, f32, f32, f32)) {
        if let Some(d) = &self.device {
            // SAFETY: a plain call on the live target between Begin/EndDraw.
            unsafe {
                d.target
                    .PushAxisAlignedClip(&to_rect_f(rect), D2D1_ANTIALIAS_MODE_ALIASED)
            };
            self.clips += 1;
        }
    }

    /// Ends the last [`push_clip`](Self::push_clip).
    pub fn pop_clip(&mut self) {
        if let Some(d) = &self.device {
            if self.clips > 0 {
                // SAFETY: pairs with the PushAxisAlignedClip above on the same target.
                unsafe { d.target.PopAxisAlignedClip() };
                self.clips -= 1;
            }
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
        // The bitmaps are rebuilt by the caller's next `set_image` (`needs_upload`).
        self.device = Some(Device {
            target,
            dc,
            brush,
            image: None,
            mips: Vec::new(),
            thumbs: thumbs::ThumbCache::default(),
        });
        Ok(())
    }
}

/// The GPU bitmaps of one mip level; `None` if they cannot be created.
fn upload_level(target: &ID2D1HwndRenderTarget, level: &MipLevel) -> Option<TiledBitmap> {
    let p = Pixels::new(&level.bgra, level.width, level.height)?;
    TiledBitmap::upload(target, &p).ok()
}

/// The unturned bitmap rect sharing `rect`'s centre, and the transform that turns it
/// clockwise by `quarters` × 90° onto `rect` (none for 0).
fn unrotated_dest(
    (x, y, w, h): (f32, f32, f32, f32),
    quarters: u8,
) -> ((f32, f32, f32, f32), Option<Matrix3x2>) {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (bw, bh) = if quarters % 2 == 1 { (h, w) } else { (w, h) };
    let dest = (cx - bw / 2.0, cy - bh / 2.0, bw, bh);
    // cos, sin of the clockwise angle (the y axis points down).
    let (c, s) = match quarters % 4 {
        0 => return (dest, None),
        1 => (0.0, 1.0),
        2 => (-1.0, 0.0),
        _ => (0.0, -1.0),
    };
    // Row-vector convention: x' = x*M11 + y*M21 + M31, y' = x*M12 + y*M22 + M32.
    let m = Matrix3x2 {
        M11: c,
        M12: s,
        M21: -s,
        M22: c,
        M31: cx - cx * c + cy * s,
        M32: cy - cx * s - cy * c,
    };
    (dest, Some(m))
}

/// `r` turned clockwise by `quarters` × 90° about `center` (still axis-aligned).
fn rotate_rect_cw(r: D2D_RECT_F, (cx, cy): (f32, f32), quarters: u8) -> D2D_RECT_F {
    // Clockwise with the y axis down: (x, y) -> (cx - (y - cy), cy + (x - cx)).
    let turn = |(x, y): (f32, f32)| (cx - (y - cy), cy + (x - cx));
    let (mut a, mut b) = ((r.left, r.top), (r.right, r.bottom));
    for _ in 0..quarters % 4 {
        a = turn(a);
        b = turn(b);
    }
    D2D_RECT_F {
        left: a.0.min(b.0),
        top: a.1.min(b.1),
        right: a.0.max(b.0),
        bottom: a.1.max(b.1),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Matrix3x2, (x, y): (f32, f32)) -> (f32, f32) {
        (x * m.M11 + y * m.M21 + m.M31, x * m.M12 + y * m.M22 + m.M32)
    }

    #[test]
    fn quarter_turn_maps_the_bitmap_onto_the_rotated_box() {
        // A 400x300 image turned once clockwise occupies a 300x400 box.
        let ((x, y, w, h), m) = unrotated_dest((100.0, 0.0, 300.0, 400.0), 1);
        assert_eq!((x, y, w, h), (50.0, 50.0, 400.0, 300.0));
        let m = m.expect("a transform for a quarter turn");
        // Clockwise: the bitmap's top-left corner lands at the box's top-right.
        assert_eq!(apply(&m, (x, y)), (400.0, 0.0));
        assert_eq!(apply(&m, (x + w, y + h)), (100.0, 400.0));
        assert!(unrotated_dest((0.0, 0.0, 4.0, 3.0), 0).1.is_none());
    }

    #[test]
    fn upload_is_needed_only_for_a_set_image_without_bitmap() {
        // Device lost or rebuilt: the image is still set but has no bitmap.
        assert!(needs_upload_for(Some((4000, 3000)), false));
        assert!(!needs_upload_for(Some((4000, 3000)), true));
        // No image (cleared, or released as too large): nothing to rebuild.
        assert!(!needs_upload_for(None, false));
    }

    #[test]
    fn window_turned_back_lands_in_bitmap_space() {
        // The rotated box, turned back, is the unturned bitmap rect.
        let rect = (100.0, 0.0, 300.0, 400.0);
        let center = (250.0, 200.0);
        for q in 0..4u8 {
            let (dest, _) = unrotated_dest(rect, q);
            let back = rotate_rect_cw(to_rect_f(rect), center, 4 - q % 4);
            let want = to_rect_f(dest);
            let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
            assert!(
                near(back.left, want.left)
                    && near(back.top, want.top)
                    && near(back.right, want.right)
                    && near(back.bottom, want.bottom),
                "quarters {q}"
            );
        }
        // Four quarter turns are the identity.
        let r = to_rect_f((1.0, 2.0, 3.0, 4.0));
        let same = rotate_rect_cw(r, (7.0, -3.0), 4);
        assert_eq!(
            (same.left, same.top, same.right, same.bottom),
            (1.0, 2.0, 4.0, 6.0)
        );
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
