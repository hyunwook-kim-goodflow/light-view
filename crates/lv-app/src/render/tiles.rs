//! Images larger than one GPU bitmap: uploaded as a grid of tiles and drawn tile by
//! tile, each with a narrow apron of neighbouring pixels so filtering never shows the
//! seams.

use lv_core::tiling::{tile_grid, TileRect};
use windows::core::Result;
use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1DeviceContext, ID2D1HwndRenderTarget, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_PROPERTIES, D2D1_INTERPOLATION_MODE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

/// Tiles are at most this many pixels a side, even where the device allows more, so
/// no single upload is huge.
const TILE_CAP: u32 = 4096;
/// Pixels of each neighbour uploaded around a tile (cut off at the image edge), so the
/// cubic filter samples real pixels across tile edges.
const APRON: u32 = 4;

/// A frame's premultiplied BGRA pixels, borrowed while the bitmaps are created from
/// them. Only built by `Pixels::new`, which validates the length.
pub struct Pixels<'a> {
    pub bgra: &'a [u8],
    pub width: u32,
    pub height: u32,
}

impl<'a> Pixels<'a> {
    /// `None` unless `bgra.len() == width * height * 4` and the stride `width * 4`
    /// fits in u32 (no overflow), so D2D never reads past the buffer.
    pub fn new(bgra: &'a [u8], width: u32, height: u32) -> Option<Self> {
        width.checked_mul(4)?;
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (bgra.len() == expected).then_some(Pixels {
            bgra,
            width,
            height,
        })
    }
}

struct Tile {
    /// The image pixels this tile draws; tiles never overlap.
    core: TileRect,
    /// The uploaded pixels: `core` plus the apron.
    bitmap: ID2D1Bitmap,
    /// Where `core` starts inside `bitmap`.
    inset: (u32, u32),
}

/// One image (or mip level) as GPU bitmaps.
pub struct TiledBitmap {
    pub width: u32,
    pub height: u32,
    tiles: Vec<Tile>,
}

/// Side of the non-overlapping grid cells for a device whose bitmaps may be
/// `max_bitmap` pixels a side: room is left for the apron on both sides.
fn grid_side(max_bitmap: u32) -> u32 {
    max_bitmap.min(TILE_CAP).saturating_sub(2 * APRON).max(1)
}

/// `core` grown by `apron` on every side, cut off at the `width`×`height` image.
fn with_apron(core: TileRect, width: u32, height: u32, apron: u32) -> TileRect {
    let x = core.x.saturating_sub(apron);
    let y = core.y.saturating_sub(apron);
    let right = (core.x + core.w).saturating_add(apron).min(width);
    let bottom = (core.y + core.h).saturating_add(apron).min(height);
    TileRect {
        x,
        y,
        w: right - x,
        h: bottom - y,
    }
}

/// The part of `dest` (x, y, w, h) that grid cell `core` of a `size` image covers.
/// Neighbouring cells compute their shared edge with the same expression, so the
/// pieces meet exactly.
fn tile_dest(core: TileRect, size: (u32, u32), dest: (f32, f32, f32, f32)) -> D2D_RECT_F {
    let (x, y, w, h) = dest;
    let sx = w / size.0 as f32;
    let sy = h / size.1 as f32;
    D2D_RECT_F {
        left: x + core.x as f32 * sx,
        top: y + core.y as f32 * sy,
        right: x + (core.x + core.w) as f32 * sx,
        bottom: y + (core.y + core.h) as f32 * sy,
    }
}

fn intersects(a: &D2D_RECT_F, b: &D2D_RECT_F) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

impl TiledBitmap {
    /// Uploads `p` as tiles no larger than the device allows. Fails if any tile cannot
    /// be created (out of video memory), or for an empty image.
    pub fn upload(target: &ID2D1HwndRenderTarget, p: &Pixels) -> Result<TiledBitmap> {
        // SAFETY: plain query on a live render target.
        let max_bitmap = unsafe { target.GetMaximumBitmapSize() };
        let grid = tile_grid(p.width, p.height, grid_side(max_bitmap));
        if grid.is_empty() {
            return Err(E_INVALIDARG.into());
        }
        let tiles = grid
            .into_iter()
            .map(|core| {
                let outer = with_apron(core, p.width, p.height, APRON);
                let bitmap = create_bitmap(target, p, outer)?;
                Ok(Tile {
                    core,
                    bitmap,
                    inset: (core.x - outer.x, core.y - outer.y),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(TiledBitmap {
            width: p.width,
            height: p.height,
            tiles,
        })
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Draws the whole bitmap stretched onto `dest` (x, y, w, h in DIPs, in the
    /// target's current transform), skipping tiles outside `visible` (same space).
    pub fn draw(
        &self,
        target: &ID2D1HwndRenderTarget,
        dc: &ID2D1DeviceContext,
        dest: (f32, f32, f32, f32),
        visible: &D2D_RECT_F,
        mode: D2D1_INTERPOLATION_MODE,
    ) {
        let several = self.tiles.len() > 1;
        if several {
            // Shared edges land on whole pixels for both neighbours: no hairline gaps.
            // SAFETY: plain state call on a live render target.
            unsafe { target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED) };
        }
        for tile in &self.tiles {
            let to = tile_dest(tile.core, (self.width, self.height), dest);
            if !intersects(&to, visible) {
                continue;
            }
            let (ix, iy) = (tile.inset.0 as f32, tile.inset.1 as f32);
            // Bitmaps are 96 DPI, so their DIPs are pixels.
            let from = D2D_RECT_F {
                left: ix,
                top: iy,
                right: ix + tile.core.w as f32,
                bottom: iy + tile.core.h as f32,
            };
            // SAFETY: both rects are live locals; the bitmap belongs to this device
            // context, which shares the render target's transform.
            unsafe { dc.DrawBitmap(&tile.bitmap, Some(&to), 1.0, mode, Some(&from), None) };
        }
        if several {
            // SAFETY: as above.
            unsafe { target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        }
    }
}

/// A bitmap of the `area` of `p`, read in place (no copy) with `p`'s row pitch.
fn create_bitmap(
    target: &ID2D1HwndRenderTarget,
    p: &Pixels,
    area: TileRect,
) -> Result<ID2D1Bitmap> {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    let size = D2D_SIZE_U {
        width: area.w,
        height: area.h,
    };
    let pitch = p.width * 4;
    let start = (area.y as usize * p.width as usize + area.x as usize) * 4;
    let src = &p.bgra[start..];
    // SAFETY: `Pixels::new` guarantees `bgra.len() == width * height * 4` and that the
    // pitch `width * 4` fits in u32. `area` lies inside the image (it comes from
    // `tile_grid` and `with_apron`), so the last row D2D reads ends at
    // `((y + h - 1) * width + x + w) * 4 <= bgra.len()`, inside `src`'s allocation.
    // D2D copies the data before returning.
    unsafe { target.CreateBitmap(size, Some(src.as_ptr().cast()), pitch, &props) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_buffer_must_match_dimensions() {
        let bgra = [3u8, 2, 1, 255, 6, 5, 4, 255];
        let p = Pixels::new(&bgra, 2, 1).expect("exact length is accepted");
        // Passed through untouched: no conversion on the UI thread.
        assert_eq!((p.bgra, p.width, p.height), (&bgra[..], 2, 1));
        assert!(Pixels::new(&bgra, 2, 2).is_none()); // too short
        assert!(Pixels::new(&bgra, 1, 1).is_none()); // too long
        assert!(Pixels::new(&bgra, u32::MAX, u32::MAX).is_none()); // overflow
        assert!(Pixels::new(&[], 0, 0).is_some());
    }

    #[test]
    fn grid_leaves_room_for_the_apron() {
        assert_eq!(grid_side(16384), 4096 - 2 * APRON);
        assert_eq!(grid_side(2048), 2048 - 2 * APRON);
        assert_eq!(grid_side(0), 1);
        // With the apron added, every upload still fits the device.
        for max in [16384u32, 8192, 4096, 2048] {
            let side = grid_side(max);
            for core in tile_grid(30000, 20000, side) {
                let outer = with_apron(core, 30000, 20000, APRON);
                assert!(outer.w <= max.min(TILE_CAP) && outer.h <= max.min(TILE_CAP));
            }
        }
    }

    #[test]
    fn apron_is_cut_at_the_image_edge() {
        let core = TileRect {
            x: 0,
            y: 4088,
            w: 4088,
            h: 12,
        };
        let outer = with_apron(core, 5000, 4100, 4);
        assert_eq!(
            outer,
            TileRect {
                x: 0,
                y: 4084,
                w: 4092,
                h: 16
            }
        );
        // The core starts this far into the uploaded bitmap.
        assert_eq!((core.x - outer.x, core.y - outer.y), (0, 4));
    }

    #[test]
    fn neighbouring_tiles_share_their_edges() {
        let size = (17000u32, 9000u32);
        let dest = (13.3, 7.7, 1903.1, 1007.5);
        let grid = tile_grid(size.0, size.1, 4088);
        for a in &grid {
            let ra = tile_dest(*a, size, dest);
            for b in &grid {
                let rb = tile_dest(*b, size, dest);
                if a.y == b.y && a.x + a.w == b.x {
                    assert_eq!(ra.right, rb.left);
                }
                if a.x == b.x && a.y + a.h == b.y {
                    assert_eq!(ra.bottom, rb.top);
                }
            }
        }
        let last = tile_dest(*grid.last().unwrap(), size, dest);
        assert!((last.right - (dest.0 + dest.2)).abs() < 1e-3);
        assert!((last.bottom - (dest.1 + dest.3)).abs() < 1e-3);
    }
}
