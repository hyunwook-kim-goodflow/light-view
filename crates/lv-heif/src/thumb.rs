//! Thumbnails for Explorer (ruling R4): the smallest embedded thumbnail (`thmb`) whose long
//! side reaches the requested size, else the primary image scaled down; turned like the
//! primary image is shown.
//!
//! **Orientation.** libheif decodes an item with its own transformations. iPhone files give
//! their thumbnail the primary image's `irot`, but nothing requires it, so the thumbnail's
//! own turn (`T`, its `irot`/`imir` in `ipma` order) is undone and the primary image's (`P`)
//! applied: `P` after `T⁻¹`. The thumbnail's `clap` stays as libheif applied it. Like the
//! app's HEIC decode, EXIF Orientation is not applied (`irot`/`imir` describe the turn). A
//! file whose properties cannot be read (`lv_core::heif_edit::turn_steps`) does not use its
//! thumbnail, nor does one whose thumbnail fails to decode: the primary image is used.
//!
//! **Scaling.** Never up. Down by an integer box filter (alpha-weighted) to between one and
//! about three times the target, then `lv_core::resize::resample` (Lanczos3) to the size
//! `target_size(LongEdge(max_side))` gives. Colour stays as decoded: the caller converts
//! [`HeifImage::color`] to sRGB.

use libheif_rs::{ImageHandle, ItemId, LibHeif};
use lv_core::heif_edit::{turn_steps, TurnStep};
use lv_core::resize::{resample, target_size, Target};
use lv_ipc::MAX_THUMBNAIL_SIDE;

use crate::{corrupt, decode_handle, fits, guarded, open, HeifError, HeifImage, MAX_ITEMS};

/// A thumbnail of a HEIF file whose longer side is at most `max_side`
/// (`1..=MAX_THUMBNAIL_SIDE`, else [`HeifError::Corrupt`]), as the module documentation
/// describes. `max_pixels` limits the image that is decoded, as in
/// [`decode_heif`](crate::decode_heif): an embedded thumbnail over it is not used, a primary
/// image over it is [`HeifError::TooLarge`]. Never panics.
pub fn thumbnail_heif(
    bytes: &[u8],
    max_side: u32,
    max_pixels: u64,
) -> Result<HeifImage, HeifError> {
    guarded(|| thumbnail_inner(bytes, max_side, max_pixels))
}

fn thumbnail_inner(bytes: &[u8], max_side: u32, max_pixels: u64) -> Result<HeifImage, HeifError> {
    if !(1..=MAX_THUMBNAIL_SIDE).contains(&max_side) {
        return Err(HeifError::Corrupt(format!(
            "thumbnail side {max_side} is not 1..={MAX_THUMBNAIL_SIDE}"
        )));
    }
    let lib = LibHeif::new();
    let ctx = open(bytes, max_pixels)?;
    let primary = ctx
        .primary_image_handle()
        .map_err(corrupt("primary image"))?;
    if let Some(image) = from_embedded(&lib, bytes, &primary, max_side, max_pixels) {
        return Ok(image);
    }
    Ok(shrink(decode_handle(&lib, &primary, max_pixels)?, max_side))
}

fn long_side(handle: &ImageHandle) -> u32 {
    handle.width().max(handle.height())
}

/// The thumbnail made from the smallest embedded thumbnail whose long side is at least
/// `max_side` and that fits `max_pixels`; `None` when there is none, when the turns cannot
/// be read or when it does not decode.
fn from_embedded(
    lib: &LibHeif,
    bytes: &[u8],
    primary: &ImageHandle,
    max_side: u32,
    max_pixels: u64,
) -> Option<HeifImage> {
    let count = primary.number_of_thumbnails().min(MAX_ITEMS as usize);
    if count == 0 {
        return None;
    }
    let mut ids: Vec<ItemId> = vec![0; count];
    let found = primary.thumbnail_ids(&mut ids);
    ids.truncate(found);
    let handle = ids
        .into_iter()
        .filter_map(|id| primary.thumbnail(id).ok())
        .filter(|h| long_side(h) >= max_side && fits(h, max_pixels))
        .min_by_key(|h| (long_side(h), u64::from(h.width()) * u64::from(h.height())))?;
    let turn = Turn::of(bytes, handle.item_id())?
        .inverse()
        .then(Turn::of(bytes, primary.item_id())?);
    let image = decode_handle(lib, &handle, max_pixels).ok()?;
    Some(turn.apply(shrink(image, max_side)))
}

/// `image` with its longer side at most `max_side` (never scaled up).
fn shrink(image: HeifImage, max_side: u32) -> HeifImage {
    let src = (image.width, image.height);
    if src.0.max(src.1) <= max_side {
        return image;
    }
    let dst = target_size(src, &Target::LongEdge(max_side));
    // Box-filter by half the (whole) ratio, or by 2 when it is 2 or 3, so Lanczos (and its
    // 16-byte-per-pixel intermediate rows) works on at most about 3x the target per side.
    let ratio = (src.0 / dst.0).min(src.1 / dst.1);
    let factor = (ratio / 2).max(ratio.min(2));
    let (rgba, size) = if factor >= 2 {
        let size = (src.0 / factor, src.1 / factor);
        (box_reduce(&image.rgba, src, size), size)
    } else {
        (image.rgba, src)
    };
    HeifImage {
        width: dst.0,
        height: dst.1,
        rgba: resample(&rgba, size, dst),
        color: image.color,
    }
}

/// Straight RGBA8 `src` averaged down to `dst` (each side at most `src`'s, at least 1):
/// every output pixel is the alpha-weighted mean of its block of source pixels, the blocks
/// tiling the source exactly.
fn box_reduce(rgba: &[u8], src: (u32, u32), dst: (u32, u32)) -> Vec<u8> {
    let (w, h) = (src.0 as usize, src.1 as usize);
    let (ow, oh) = (dst.0 as usize, dst.1 as usize);
    let xs: Vec<usize> = (0..=ow).map(|i| i * w / ow).collect();
    let mut out = Vec::with_capacity(ow * oh * 4);
    // Per output column: sum of a, r * a, g * a, b * a.
    let mut sums = vec![0u64; ow * 4];
    for oy in 0..oh {
        let (y0, y1) = (oy * h / oh, (oy + 1) * h / oh);
        sums.fill(0);
        for row in rgba[y0 * w * 4..y1 * w * 4].chunks_exact(w * 4) {
            for (ox, s) in sums.chunks_exact_mut(4).enumerate() {
                for px in row[xs[ox] * 4..xs[ox + 1] * 4].chunks_exact(4) {
                    let a = u64::from(px[3]);
                    s[0] += a;
                    s[1] += u64::from(px[0]) * a;
                    s[2] += u64::from(px[1]) * a;
                    s[3] += u64::from(px[2]) * a;
                }
            }
        }
        for (ox, s) in sums.chunks_exact(4).enumerate() {
            if s[0] == 0 {
                out.extend_from_slice(&[0; 4]);
                continue;
            }
            let n = ((xs[ox + 1] - xs[ox]) * (y1 - y0)) as u64;
            // `c * a <= 255 * a`, so each mean is at most 255.
            let mean = |v: u64| ((v + s[0] / 2) / s[0]) as u8;
            out.extend_from_slice(&[
                mean(s[1]),
                mean(s[2]),
                mean(s[3]),
                ((s[0] + n / 2) / n) as u8,
            ]);
        }
    }
    out
}

/// A turn of the image: mirrored left to right when `mirror`, then `ccw` anti-clockwise
/// quarter turns. The eight of them form the dihedral group `then` composes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Turn {
    mirror: bool,
    ccw: u8,
}

impl Turn {
    const NONE: Turn = Turn {
        mirror: false,
        ccw: 0,
    };

    /// The turn of one property, as libheif applies it: `imir` axis 1 mirrors left to
    /// right and axis 0 top to bottom (that is, left to right and then half a turn).
    fn step(step: TurnStep) -> Turn {
        match step {
            TurnStep::Irot(quarters) => Turn {
                mirror: false,
                ccw: quarters & 3,
            },
            TurnStep::Imir(1) => Turn {
                mirror: true,
                ccw: 0,
            },
            TurnStep::Imir(_) => Turn {
                mirror: true,
                ccw: 2,
            },
        }
    }

    /// The turn libheif gives `item` of the file `heif`; `None` when it cannot be read.
    fn of(heif: &[u8], item: ItemId) -> Option<Turn> {
        let steps = turn_steps(heif, item).ok()?;
        Some(
            steps
                .into_iter()
                .map(Turn::step)
                .fold(Turn::NONE, Turn::then),
        )
    }

    /// `self`, then `next`. With `M` the mirror and `R` a quarter turn, `M R = R⁻¹ M`, so
    /// `R^b M^n R^a M^m = R^(b ± a) M^(m + n)`, minus when `next` mirrors.
    fn then(self, next: Turn) -> Turn {
        let carried = if next.mirror { 4 - self.ccw } else { self.ccw };
        Turn {
            mirror: self.mirror ^ next.mirror,
            ccw: (next.ccw + carried) % 4,
        }
    }

    /// The turn that undoes `self`: a mirrored turn undoes itself.
    fn inverse(self) -> Turn {
        Turn {
            mirror: self.mirror,
            ccw: if self.mirror {
                self.ccw
            } else {
                (4 - self.ccw) % 4
            },
        }
    }

    fn apply(self, image: HeifImage) -> HeifImage {
        if self == Turn::NONE {
            return image;
        }
        let (w, h) = (image.width as usize, image.height as usize);
        let (ow, oh) = if self.ccw % 2 == 1 { (h, w) } else { (w, h) };
        let mut rgba = Vec::with_capacity(image.rgba.len());
        for y in 0..oh {
            for x in 0..ow {
                // The pixel of the mirrored image that the turn brings to (x, y).
                let (mx, my) = match self.ccw {
                    0 => (x, y),
                    1 => (w - 1 - y, x),
                    2 => (w - 1 - x, h - 1 - y),
                    _ => (y, h - 1 - x),
                };
                let sx = if self.mirror { w - 1 - mx } else { mx };
                let at = (my * w + sx) * 4;
                rgba.extend_from_slice(&image.rgba[at..at + 4]);
            }
        }
        HeifImage {
            width: ow as u32,
            height: oh as u32,
            rgba,
            color: image.color,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColorInfo;

    fn all_turns() -> Vec<Turn> {
        (0..8)
            .map(|i| Turn {
                mirror: i >= 4,
                ccw: i % 4,
            })
            .collect()
    }

    /// A 3 x 2 image whose pixels are all different.
    fn image() -> HeifImage {
        HeifImage {
            width: 3,
            height: 2,
            rgba: (0..24).collect(),
            color: ColorInfo::None,
        }
    }

    #[test]
    fn single_turns_move_pixels_as_libheif_does() {
        let px = |img: &HeifImage, x: usize, y: usize| {
            let at = (y * img.width as usize + x) * 4;
            img.rgba[at]
        };
        let src = image();
        // irot 1: anti-clockwise, rotated(x, y) = upright(W - 1 - y, x).
        let ccw = Turn::step(TurnStep::Irot(1)).apply(image());
        assert_eq!((ccw.width, ccw.height), (2, 3));
        for (x, y) in [(0, 0), (1, 0), (0, 2), (1, 2)] {
            assert_eq!(px(&ccw, x, y), px(&src, 2 - y, x));
        }
        let lr = Turn::step(TurnStep::Imir(1)).apply(image());
        let tb = Turn::step(TurnStep::Imir(0)).apply(image());
        for (x, y) in [(0, 0), (2, 0), (1, 1)] {
            assert_eq!(px(&lr, x, y), px(&src, 2 - x, y));
            assert_eq!(px(&tb, x, y), px(&src, x, 1 - y));
        }
    }

    #[test]
    fn then_and_inverse_match_applying_turns_in_order() {
        for a in all_turns() {
            assert_eq!(a.then(a.inverse()), Turn::NONE, "{a:?}");
            assert_eq!(a.inverse().then(a), Turn::NONE, "{a:?}");
            for b in all_turns() {
                assert_eq!(
                    b.apply(a.apply(image())),
                    a.then(b).apply(image()),
                    "{a:?} then {b:?}"
                );
            }
        }
    }

    #[test]
    fn box_reduce_averages_alpha_weighted_blocks() {
        // 4 x 2 to 2 x 1: blocks of 2 x 2.
        let rgba = [
            [255, 0, 0, 255],
            [0, 0, 255, 255],
            [10, 20, 30, 0],
            [100, 100, 100, 255],
            [255, 0, 0, 255],
            [0, 0, 255, 255],
            [200, 200, 200, 0],
            [100, 100, 100, 255],
        ]
        .concat();
        let out = box_reduce(&rgba, (4, 2), (2, 1));
        // Opaque block: the plain mean. Half transparent block: the transparent pixels'
        // colour does not count, alpha is the mean.
        assert_eq!(out, vec![128, 0, 128, 255, 100, 100, 100, 128]);
        // Uneven blocks still cover the source: 5 x 1 to 2 x 1 is blocks of 2 and 3.
        let row = [
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [90, 90, 90, 255],
            [90, 90, 90, 255],
            [90, 90, 90, 255],
        ]
        .concat();
        assert_eq!(
            box_reduce(&row, (5, 1), (2, 1)),
            vec![0, 0, 0, 255, 90, 90, 90, 255]
        );
        // All transparent: transparent black.
        assert_eq!(
            box_reduce(&[7, 7, 7, 0, 9, 9, 9, 0], (2, 1), (1, 1)),
            vec![0; 4]
        );
    }

    #[test]
    fn shrink_keeps_small_images_and_meets_the_long_side() {
        let small = image();
        assert_eq!(shrink(image(), 3), small);
        assert_eq!(shrink(image(), 100), small);
        let big = HeifImage {
            width: 1000,
            height: 10,
            rgba: vec![200; 1000 * 10 * 4],
            color: ColorInfo::None,
        };
        let out = shrink(big, 100);
        assert_eq!((out.width, out.height), (100, 1));
        assert_eq!(out.rgba.len(), 100 * 4);
        assert!(
            out.rgba.iter().all(|&c| c.abs_diff(200) <= 1),
            "{:?}",
            &out.rgba[..8]
        );
    }
}
