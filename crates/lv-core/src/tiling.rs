//! Tile and mipmap arithmetic for images larger than one GPU bitmap.
//! Pure computation; sizes are in image pixels.

/// A rectangle of image pixels: top-left corner and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Splits a `width`×`height` image into row-major tiles that cover it exactly once,
/// each side at most `max_tile` (a `max_tile` of 0 counts as 1). Empty for an empty
/// image.
pub fn tile_grid(width: u32, height: u32, max_tile: u32) -> Vec<TileRect> {
    let max = max_tile.max(1);
    let spans = |len: u32| {
        (0..len.div_ceil(max)).map(move |i| {
            let start = i * max;
            (start, max.min(len - start))
        })
    };
    let mut tiles = Vec::new();
    for (y, h) in spans(height) {
        for (x, w) in spans(width) {
            tiles.push(TileRect { x, y, w, h });
        }
    }
    tiles
}

/// The mip level to draw at `device_scale` (device pixels per image pixel): the
/// largest `k` with `device_scale <= 1 / 2^k`, at most `levels - 1`. Level 0 for a
/// scale of 1 or more, or one that is not a number.
pub fn mip_level_for_scale(device_scale: f32, levels: u8) -> u8 {
    if device_scale.is_nan() {
        return 0;
    }
    let mut level = 0u8;
    let mut limit = 0.5f32;
    while level + 1 < levels && device_scale <= limit {
        level += 1;
        limit /= 2.0;
    }
    level
}

/// Size of mip `level`: each side halved `level` times, rounded up, at least 1.
pub fn mip_dims(width: u32, height: u32, level: u8) -> (u32, u32) {
    let halve = |v: u32| -> u32 {
        if level >= 32 {
            return 1;
        }
        let step = 1u64 << level;
        (u64::from(v).div_ceil(step) as u32).max(1)
    };
    (halve(width), halve(height))
}

/// How many levels (the image itself included) to build so that fitting the image
/// in a `view` of device pixels, in either orientation, needs no smaller one. At
/// least 1; never more than halving down to a 1×1 level takes, and the levels past
/// the first together hold at most a third of the image's pixels.
pub fn mip_levels_for_fit(image: (u32, u32), view: (u32, u32)) -> u8 {
    let (w, h) = (image.0.max(1), image.1.max(1));
    let (iw, ih) = (w as f32, h as f32);
    let (vw, vh) = (view.0.max(1) as f32, view.1.max(1) as f32);
    // The smaller of the two orientations' fit scales (a quarter turn swaps sides).
    let fit = (vw / iw).min(vh / ih).min(vw / ih).min(vh / iw);
    // Levels until the longest side reaches 1: ceil(log2) halvings, plus level 0.
    let all = (u32::BITS - (w.max(h) - 1).leading_zeros()) as u8 + 1;
    let wanted = mip_level_for_scale(fit, all) + 1;
    let budget = u64::from(w) * u64::from(h) / 3;
    let mut used = 0u64;
    for level in 1..wanted {
        let (mw, mh) = mip_dims(w, h, level);
        used += u64::from(mw) * u64::from(mh);
        if used > budget {
            return level;
        }
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::Viewport;

    fn assert_exact_cover(width: u32, height: u32, max: u32) {
        let tiles = tile_grid(width, height, max);
        assert!(!tiles.is_empty(), "{width}x{height}: no tiles");
        let area: u64 = tiles.iter().map(|t| u64::from(t.w) * u64::from(t.h)).sum();
        assert_eq!(
            area,
            u64::from(width) * u64::from(height),
            "{width}x{height}"
        );
        for t in &tiles {
            let max = max.max(1);
            assert!(t.w >= 1 && t.h >= 1 && t.w <= max && t.h <= max, "{t:?}");
            assert!(t.x + t.w <= width && t.y + t.h <= height, "{t:?} outside");
        }
        // Same total area, every tile inside: no overlap iff no pair intersects.
        for (i, a) in tiles.iter().enumerate() {
            for b in &tiles[i + 1..] {
                let apart =
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn tiles_cover_image_within_max_texture() {
        assert_exact_cover(30000, 20000, 16384);
        assert_exact_cover(30000, 20000, 4096);
        assert_eq!(tile_grid(30000, 20000, 16384).len(), 4);
        assert_eq!(tile_grid(30000, 20000, 4096).len(), 8 * 5);
        // An image that fits is one tile.
        assert_eq!(
            tile_grid(4032, 3024, 4096),
            vec![TileRect {
                x: 0,
                y: 0,
                w: 4032,
                h: 3024
            }]
        );
        assert!(tile_grid(0, 10, 4096).is_empty());
        assert_exact_cover(5, 3, 0);
    }

    #[test]
    fn mip_level_for_scale() {
        assert_eq!(super::mip_level_for_scale(1.0, 8), 0);
        assert_eq!(super::mip_level_for_scale(3.0, 8), 0);
        assert_eq!(super::mip_level_for_scale(0.6, 8), 0);
        assert_eq!(super::mip_level_for_scale(0.5, 8), 1);
        assert_eq!(super::mip_level_for_scale(0.24, 8), 2);
        assert_eq!(super::mip_level_for_scale(0.25, 8), 2);
        assert_eq!(super::mip_level_for_scale(0.0001, 8), 7);
        assert_eq!(super::mip_level_for_scale(0.0001, 3), 2);
        assert_eq!(super::mip_level_for_scale(0.0, 4), 3);
        assert_eq!(super::mip_level_for_scale(f32::NAN, 4), 0);
        assert_eq!(super::mip_level_for_scale(0.1, 1), 0);
        assert_eq!(super::mip_level_for_scale(0.1, 0), 0);
    }

    #[test]
    fn mip_dims_halve_and_round_up() {
        assert_eq!(mip_dims(17000, 9001, 0), (17000, 9001));
        assert_eq!(mip_dims(17000, 9001, 1), (8500, 4501));
        assert_eq!(mip_dims(17000, 9001, 3), (2125, 1126));
        // Halving the previous level gives the same size.
        let (w, h) = mip_dims(30001, 19999, 4);
        assert_eq!(mip_dims(w, h, 1), mip_dims(30001, 19999, 5));
        assert_eq!(mip_dims(5, 3, 40), (1, 1));
    }

    #[test]
    fn levels_for_fit_follow_the_view() {
        // 17000x9000 in 1920x1080: the turned fit is 1080/17000 = 0.064 -> level 3.
        assert_eq!(mip_levels_for_fit((17000, 9000), (1920, 1080)), 4);
        // Fits already: only the image itself.
        assert_eq!(mip_levels_for_fit((800, 600), (1920, 1080)), 1);
        assert_eq!(mip_levels_for_fit((1, 1), (1, 1)), 1);
        assert_eq!(mip_levels_for_fit((0, 0), (0, 0)), 1);
        // A square power of two goes all the way down to 1x1 in a 1x1 view.
        let n = mip_levels_for_fit((1024, 1024), (1, 1));
        assert_eq!((n, mip_dims(1024, 1024, n - 1)), (11, (1, 1)));
    }

    #[test]
    fn mip_chain_is_at_most_a_third_of_the_image() {
        let sizes = [
            (30000u32, 20000u32),
            (30001, 19999),
            (17000, 9000),
            (20000, 8),
            (8, 20000),
            (3, 3),
            (1, 1),
        ];
        for (w, h) in sizes {
            for view in [(1u32, 1u32), (1920, 1080), (100, 100)] {
                let n = mip_levels_for_fit((w, h), view);
                assert!(n >= 1);
                let full = u64::from(w) * u64::from(h);
                let sum: u64 = (1..n)
                    .map(|l| {
                        let (mw, mh) = mip_dims(w, h, l);
                        u64::from(mw) * u64::from(mh)
                    })
                    .sum();
                assert!(sum * 3 <= full, "{w}x{h} in {view:?}: {sum} of {full}");
            }
        }
    }

    #[test]
    fn thin_image_tiles_and_zoom_bounds() {
        for (w, h) in [(20000u32, 8u32), (1, 1), (8, 20000)] {
            assert_exact_cover(w, h, 4096);
            assert_exact_cover(w, h, 16384);
            for level in 0..=40 {
                let (mw, mh) = mip_dims(w, h, level);
                assert!(mw >= 1 && mh >= 1, "{w}x{h} level {level}");
            }
            let v = Viewport::initial((800.0, 600.0), (w as f32, h as f32));
            let (min, max) = v.zoom_bounds();
            for s in [min, max, v.scale(), v.fit_scale()] {
                assert!(s.is_finite() && s > 0.0, "{w}x{h}: scale {s}");
            }
            assert!(min <= v.scale() && v.scale() <= max, "{w}x{h}");
            let (x, y, rw, rh) = v.image_rect();
            for c in [x, y, rw, rh] {
                assert!(c.is_finite(), "{w}x{h}: rect {c}");
            }
            assert!(rw > 0.0 && rh > 0.0, "{w}x{h}: empty rect");
        }
    }
}
