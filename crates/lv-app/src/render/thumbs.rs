//! Small bitmaps drawn as they are (the resize screen's filmstrip tiles), kept per
//! device by the caller's id.

use std::collections::HashMap;

use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use super::{to_rect_f, Renderer};

/// At most this many bitmaps are kept; past it the cache starts over.
const MAX_THUMBS: usize = 512;

#[derive(Default)]
pub(super) struct ThumbCache {
    bitmaps: HashMap<u64, ID2D1Bitmap>,
}

impl Renderer {
    /// Drops every kept bitmap (the resize screen closed).
    pub fn clear_thumbs(&mut self) {
        if let Some(d) = &mut self.device {
            d.thumbs.bitmaps.clear();
        }
    }

    /// Draws premultiplied BGRA pixels of `size` filling `rect` (DIPs), cropped to its
    /// aspect ratio around the centre. `id` names the pixels: the bitmap is made once.
    pub fn draw_thumb(
        &mut self,
        id: u64,
        size: (u32, u32),
        bgra: &[u8],
        rect: (f32, f32, f32, f32),
    ) {
        let Some(d) = &mut self.device else { return };
        if size.0 == 0 || size.1 == 0 || bgra.len() != size.0 as usize * size.1 as usize * 4 {
            return;
        }
        if !d.thumbs.bitmaps.contains_key(&id) {
            if d.thumbs.bitmaps.len() >= MAX_THUMBS {
                d.thumbs.bitmaps.clear();
            }
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let px = D2D_SIZE_U {
                width: size.0,
                height: size.1,
            };
            // SAFETY: `bgra` holds `size.0 * size.1 * 4` bytes (checked above) with a
            // row pitch of `size.0 * 4`; D2D copies them before returning.
            let made = unsafe {
                d.target
                    .CreateBitmap(px, Some(bgra.as_ptr().cast()), size.0 * 4, &props)
            };
            match made {
                Ok(b) => {
                    d.thumbs.bitmaps.insert(id, b);
                }
                Err(e) => {
                    eprintln!("light-view: could not make a thumbnail bitmap: {e}");
                    return;
                }
            }
        }
        let Some(bitmap) = d.thumbs.bitmaps.get(&id) else {
            return;
        };
        let source = crop(size, (rect.2, rect.3));
        // SAFETY: drawing a live bitmap on the live target between Begin/EndDraw.
        unsafe {
            d.target.DrawBitmap(
                bitmap,
                Some(&to_rect_f(rect)),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                Some(&to_rect_f(source)),
            );
        }
    }
}

/// The centred part of a `size` image with the aspect ratio of `to` (x, y, w, h).
fn crop(size: (u32, u32), to: (f32, f32)) -> (f32, f32, f32, f32) {
    let (w, h) = (size.0 as f32, size.1 as f32);
    if to.0 <= 0.0 || to.1 <= 0.0 {
        return (0.0, 0.0, w, h);
    }
    let want = to.0 / to.1;
    if w / h > want {
        let cw = h * want;
        ((w - cw) / 2.0, 0.0, cw, h)
    } else {
        let ch = w / want;
        (0.0, (h - ch) / 2.0, w, ch)
    }
}

#[cfg(test)]
mod tests {
    use super::crop;

    #[test]
    fn tiles_are_cropped_to_the_cell() {
        // A wide photo in a 38×28 cell keeps its full height, centred.
        let (x, y, w, h) = crop((64, 32), (38.0, 28.0));
        assert_eq!((y, h), (0.0, 32.0));
        assert!((w - 32.0 * 38.0 / 28.0).abs() < 1e-3 && (x - (64.0 - w) / 2.0).abs() < 1e-3);
        // A tall one keeps its full width.
        let (x, _, w, h) = crop((24, 64), (38.0, 28.0));
        assert_eq!((x, w), (0.0, 24.0));
        assert!((h - 24.0 * 28.0 / 38.0).abs() < 1e-3);
    }
}
