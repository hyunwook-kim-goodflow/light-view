//! Output sizes and resampling.

use image::imageops::{self, FilterType};
use image::{ImageBuffer, Rgba};

use super::Target;

/// The lowest long edge the fit search goes to: 800 px, or the source's own long edge
/// when that is smaller.
pub fn floor_long_edge(src: (u32, u32)) -> u32 {
    800.min(src.0.max(src.1))
}

/// The output size for `target`: aspect ratio kept, never larger than `src`, at least
/// 1 px per side. `MaxBytes` limits only by its optional long edge.
pub fn target_size(src: (u32, u32), target: &Target) -> (u32, u32) {
    let (w, h) = (src.0.max(1), src.1.max(1));
    // `a * num / den`, rounded half up, at least 1.
    let scale = |a: u32, num: u32, den: u32| -> u32 {
        let v = (u64::from(a) * u64::from(num) + u64::from(den) / 2) / u64::from(den.max(1));
        (v as u32).max(1)
    };
    let by_width = |nw: u32| {
        if nw >= w {
            (w, h)
        } else {
            (nw.max(1), scale(h, nw.max(1), w))
        }
    };
    let by_height = |nh: u32| {
        if nh >= h {
            (w, h)
        } else {
            (scale(w, nh.max(1), h), nh.max(1))
        }
    };
    let by_long_edge = |n: u32| {
        if w >= h {
            by_width(n)
        } else {
            by_height(n)
        }
    };
    match *target {
        Target::LongEdge(n) => by_long_edge(n),
        Target::Width(n) => by_width(n),
        Target::Height(n) => by_height(n),
        Target::MaxBytes { long_edge, .. } => long_edge.map_or((w, h), by_long_edge),
        Target::Percent(p) if p >= 100 => (w, h),
        Target::Percent(p) => (scale(w, p.into(), 100), scale(h, p.into(), 100)),
    }
}

/// Lanczos3 resample of straight-alpha RGBA8 from `src` to `dst`. Images with any
/// transparency are filtered premultiplied (16 bits per channel), so transparent
/// pixels' color does not bleed into the edges.
pub fn resample(rgba: &[u8], src: (u32, u32), dst: (u32, u32)) -> Vec<u8> {
    if src == dst {
        return rgba.to_vec();
    }
    let filter = FilterType::Lanczos3;
    if rgba.chunks_exact(4).all(|p| p[3] == 255) {
        let Some(img) = ImageBuffer::<Rgba<u8>, &[u8]>::from_raw(src.0, src.1, rgba) else {
            unreachable!("buffer matches the source size")
        };
        return imageops::resize(&img, dst.0, dst.1, filter).into_raw();
    }
    // Premultiply into 16 bits: c * a / 255 scaled by 257 keeps full precision.
    let pre: Vec<u16> = rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let a = u32::from(p[3]);
            let m = |c: u8| (u32::from(c) * a * 257 / 255) as u16;
            [m(p[0]), m(p[1]), m(p[2]), (a * 257) as u16]
        })
        .collect();
    let Some(img) = ImageBuffer::<Rgba<u16>, Vec<u16>>::from_raw(src.0, src.1, pre) else {
        unreachable!("buffer matches the source size")
    };
    let out = imageops::resize(&img, dst.0, dst.1, filter);
    out.into_raw()
        .chunks_exact(4)
        .flat_map(|p| {
            let a = u32::from(p[3]);
            let a8 = ((a + 128) / 257) as u8;
            let un = |c: u16| match a {
                0 => 0,
                _ => ((u32::from(c) * 255 + a / 2) / a).min(255) as u8,
            };
            [un(p[0]), un(p[1]), un(p[2]), a8]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{decode_bytes, DecodeLimits};
    use crate::test_fixtures as fixtures;

    #[test]
    fn target_size_table() {
        let cases = [
            ((4032, 3024), Target::LongEdge(1920), (1920, 1440)),
            ((3024, 4032), Target::LongEdge(1920), (1440, 1920)),
            ((4032, 3024), Target::Height(1000), (1333, 1000)),
            ((800, 600), Target::LongEdge(1920), (800, 600)),
            ((4031, 3023), Target::Percent(50), (2016, 1512)),
            ((20000, 8), Target::LongEdge(100), (100, 1)),
        ];
        for (src, target, want) in cases {
            assert_eq!(target_size(src, &target), want, "{src:?} {target:?}");
        }
    }

    #[test]
    fn target_size_never_enlarges() {
        let src = (640, 480);
        for target in [
            Target::Width(2000),
            Target::Height(2000),
            Target::Percent(150),
            Target::MaxBytes {
                bytes: 1,
                long_edge: Some(5000),
            },
            Target::MaxBytes {
                bytes: 1,
                long_edge: None,
            },
        ] {
            assert_eq!(target_size(src, &target), src, "{target:?}");
        }
        let limited = Target::MaxBytes {
            bytes: 1,
            long_edge: Some(320),
        };
        assert_eq!(target_size(src, &limited), (320, 240));
        assert_eq!(target_size((3, 1), &Target::Percent(0)), (1, 1));
    }

    #[test]
    fn floor_is_800_or_the_source_long_edge() {
        assert_eq!(floor_long_edge((4000, 3000)), 800);
        assert_eq!(floor_long_edge((600, 700)), 700);
    }

    #[test]
    fn orientation_is_applied_before_limits() {
        // Stored 4032x3024 with orientation 6: the decoder hands over 3024x4032.
        let jpeg = fixtures::jpeg_halves(4032, 3024, 6, None);
        let img = decode_bytes(&jpeg, &DecodeLimits::default()).unwrap();
        let src = (img.width, img.height);
        assert_eq!(src, (3024, 4032));
        assert_eq!(target_size(src, &Target::Width(1000)), (1000, 1333));
    }

    #[test]
    fn resample_keeps_flat_colors_and_size() {
        let rgba: Vec<u8> = [10, 120, 230, 255].repeat(40 * 30);
        let out = resample(&rgba, (40, 30), (20, 15));
        assert_eq!(out.len(), 20 * 15 * 4);
        assert!(out.chunks_exact(4).all(|p| p == [10, 120, 230, 255]));
        assert_eq!(resample(&rgba, (40, 30), (40, 30)), rgba);
    }

    #[test]
    fn resample_does_not_bleed_transparent_color() {
        // Opaque red left half, transparent black right half: edge pixels stay red.
        let (w, h) = (16u32, 4u32);
        let mut rgba = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(if x < w / 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 0, 0]
                });
            }
        }
        let out = resample(&rgba, (w, h), (8, 2));
        for p in out.chunks_exact(4).filter(|p| p[3] > 16) {
            assert!(p[0] > 240 && p[1] < 16 && p[2] < 16, "{p:?}");
        }
    }
}
