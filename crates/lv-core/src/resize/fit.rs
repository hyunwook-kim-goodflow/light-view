//! The fit-to-size search: the resolution target first, then quality, then resolution.

use std::borrow::Cow;

use super::{
    floor_long_edge, resample, target_size, EncodeError, OutputFormat, ResizeSpec, Target,
};

/// The lowest quality the search goes to (or the start quality, when that is lower).
const QUALITY_FLOOR: u8 = 60;

/// Encodes straight RGBA8 of a size at a quality; `encode` for JPEG and PNG, the
/// worker for WebP.
pub type Encoder<'a> = dyn Fn(&[u8], (u32, u32), u8) -> Result<Vec<u8>, EncodeError> + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitOutcome {
    Fitted {
        bytes: Vec<u8>,
        size: (u32, u32),
        quality: u8,
    },
    /// Even the floor (quality 60, long edge `floor_long_edge`) is over the budget;
    /// `smallest` is the shortest result seen.
    CantFit { smallest: u64 },
}

/// Resizes `rgba` (straight RGBA8 of `src`) to `spec.target` and encodes it with
/// `encoder` at `start_quality` (the caller resolves `spec.quality`). For
/// `Target::MaxBytes` the result must be at most `bytes` long: quality is binary
/// searched from `start_quality` down to 60, then the long edge shrinks by 10% per
/// step down to `floor_long_edge`, searching quality again at each size. PNG has no
/// quality to lower, so only its resolution shrinks. Deterministic for the same input.
pub fn fit(
    rgba: &[u8],
    src: (u32, u32),
    spec: &ResizeSpec,
    start_quality: u8,
    encoder: &Encoder<'_>,
) -> Result<FitOutcome, EncodeError> {
    let start = start_quality.clamp(1, 100);
    let mut size = target_size(src, &spec.target);
    let at = |size: (u32, u32)| -> Cow<'_, [u8]> {
        if size == src {
            Cow::Borrowed(rgba)
        } else {
            Cow::Owned(resample(rgba, src, size))
        }
    };
    let Target::MaxBytes { bytes: budget, .. } = spec.target else {
        let bytes = encoder(&at(size), size, start)?;
        return Ok(FitOutcome::Fitted {
            bytes,
            size,
            quality: start,
        });
    };
    let floor_quality = QUALITY_FLOOR.min(start);
    let lowers_quality = spec.format != OutputFormat::Png && floor_quality < start;
    let floor_long = floor_long_edge(src).min(size.0.max(size.1));
    let mut smallest = u64::MAX;
    loop {
        let pixels = at(size);
        let fits = |bytes: &Vec<u8>| bytes.len() as u64 <= budget;
        let best = encoder(&pixels, size, start)?;
        // The shortest result at this size.
        let mut here = best.len() as u64;
        if fits(&best) {
            return Ok(fitted(best, size, start));
        }
        if lowers_quality {
            let low = encoder(&pixels, size, floor_quality)?;
            here = here.min(low.len() as u64);
            if fits(&low) {
                // Highest quality that fits: `lo` always fits, `hi` never does.
                let (mut lo, mut hi, mut kept) = (floor_quality, start, low);
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    let bytes = encoder(&pixels, size, mid)?;
                    if fits(&bytes) {
                        (lo, kept) = (mid, bytes);
                    } else {
                        hi = mid;
                    }
                }
                return Ok(fitted(kept, size, lo));
            }
        }
        smallest = smallest.min(here);
        if size.0.max(size.1) <= floor_long {
            return Ok(FitOutcome::CantFit { smallest });
        }
        // Shrink by 10% steps, skipping the steps that would still be over the budget
        // if bytes fell in proportion to pixels. Real images lose bytes more slowly than
        // pixels, so a skipped step is not expected to fit either; this saves the
        // resampling and encoding of sizes that cannot work.
        let from = size;
        size = shrink(src, size, floor_long);
        while size.0.max(size.1) > floor_long && scaled(here, from, size) > budget {
            size = shrink(src, size, floor_long);
        }
    }
}

/// The next size down: the long edge 10% shorter (rounded), never below `floor_long`.
fn shrink(src: (u32, u32), size: (u32, u32), floor_long: u32) -> (u32, u32) {
    let long = size.0.max(size.1);
    let next = ((u64::from(long) * 9 + 5) / 10) as u32;
    let next = next.min(long.saturating_sub(1)).max(floor_long);
    target_size(src, &Target::LongEdge(next))
}

/// `bytes` at size `from`, scaled by the pixel count of `to`.
fn scaled(bytes: u64, from: (u32, u32), to: (u32, u32)) -> u64 {
    let px = |(w, h): (u32, u32)| u128::from(w) * u128::from(h);
    (u128::from(bytes) * px(to) / px(from).max(1)) as u64
}

fn fitted(bytes: Vec<u8>, size: (u32, u32), quality: u8) -> FitOutcome {
    FitOutcome::Fitted {
        bytes,
        size,
        quality,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::decode::{decode_bytes, DecodeLimits};
    use crate::resize::{encode, to_straight_rgba, Quality, Target};
    use crate::test_fixtures::rgba_noise;

    fn spec(target: Target, format: OutputFormat) -> ResizeSpec {
        ResizeSpec {
            target,
            format,
            quality: Quality::MatchOriginal,
            strip_gps: true,
        }
    }

    fn max_bytes(bytes: u64) -> Target {
        Target::MaxBytes {
            bytes,
            long_edge: None,
        }
    }

    /// One encoder call: the size and quality tried.
    type Trial = ((u32, u32), u8);

    /// Runs `fit` with the in-process encoder, recording every (size, quality) tried.
    fn fit_logged(
        rgba: &[u8],
        src: (u32, u32),
        spec: &ResizeSpec,
        start: u8,
    ) -> (FitOutcome, Vec<Trial>) {
        let log = RefCell::new(Vec::new());
        let format = spec.format;
        let encoder = |px: &[u8], size: (u32, u32), q: u8| {
            log.borrow_mut().push((size, q));
            encode(px, size, format, q)
        };
        let outcome = fit(rgba, src, spec, start, &encoder).unwrap();
        (outcome, log.into_inner())
    }

    #[test]
    fn tiny_limit_hits_floor_and_fails() {
        let rgba = rgba_noise(4000, 3000, 1);
        let spec = spec(max_bytes(50_000), OutputFormat::Jpeg);
        let (outcome, log) = fit_logged(&rgba, (4000, 3000), &spec, 90);
        let FitOutcome::CantFit { smallest } = outcome else {
            panic!("expected CantFit, got {outcome:?}");
        };
        assert!(smallest > 50_000);
        assert!(!log.is_empty());
        for &((w, h), q) in &log {
            assert!(q >= 60, "quality {q}");
            assert!(w.max(h) >= 800, "size {w}x{h}");
        }
        // The floor itself was tried.
        assert!(log.contains(&((800, 600), 60)));
    }

    #[test]
    fn fits_under_limit() {
        let rgba = rgba_noise(4000, 3000, 1);
        let spec = spec(max_bytes(1_500_000), OutputFormat::Jpeg);
        let (outcome, _) = fit_logged(&rgba, (4000, 3000), &spec, 90);
        let FitOutcome::Fitted {
            bytes,
            size,
            quality,
        } = outcome
        else {
            panic!("expected Fitted, got {outcome:?}");
        };
        assert!(bytes.len() <= 1_500_000, "{}", bytes.len());
        assert!(size.0.max(size.1) >= 800 && size.0 <= 4000 && quality >= 60);
    }

    #[test]
    fn png_shrinks_resolution_only() {
        let rgba = rgba_noise(1200, 900, 5);
        let spec = spec(max_bytes(2_000_000), OutputFormat::Png);
        let (outcome, log) = fit_logged(&rgba, (1200, 900), &spec, 90);
        let FitOutcome::Fitted { bytes, size, .. } = outcome else {
            panic!("expected Fitted, got {outcome:?}");
        };
        assert!(bytes.len() <= 2_000_000);
        assert!(size.0 < 1200);
        assert!(log.iter().all(|&(_, q)| q == 90));
    }

    #[test]
    fn long_edge_limit_applies_before_the_budget() {
        let rgba = rgba_noise(1000, 500, 9);
        let target = Target::MaxBytes {
            bytes: 10_000_000,
            long_edge: Some(400),
        };
        let (outcome, log) = fit_logged(&rgba, (1000, 500), &spec(target, OutputFormat::Jpeg), 85);
        let FitOutcome::Fitted { size, quality, .. } = outcome else {
            panic!("expected Fitted, got {outcome:?}");
        };
        assert_eq!((size, quality), ((400, 200), 85));
        assert_eq!(log, vec![((400, 200), 85)]);
    }

    #[test]
    fn already_within_limits_is_not_enlarged_or_inflated() {
        let mut original = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut original, 70),
            &rgb(&rgba_noise(1000, 750, 4)),
            1000,
            750,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
        let img = decode_bytes(&original, &DecodeLimits::default()).unwrap();
        let rgba = to_straight_rgba(&img.frames[0]);
        let spec = spec(Target::LongEdge(1920), OutputFormat::Jpeg);
        let (outcome, _) = fit_logged(&rgba, (1000, 750), &spec, 70);
        let FitOutcome::Fitted { bytes, size, .. } = outcome else {
            panic!("expected Fitted, got {outcome:?}");
        };
        assert_eq!(size, (1000, 750));
        assert!(
            bytes.len() as f64 <= original.len() as f64 * 1.05,
            "{} vs {}",
            bytes.len(),
            original.len()
        );
    }

    #[test]
    fn fit_is_deterministic() {
        let rgba = rgba_noise(900, 700, 2);
        let spec = spec(max_bytes(120_000), OutputFormat::Jpeg);
        let (a, _) = fit_logged(&rgba, (900, 700), &spec, 90);
        let (b, _) = fit_logged(&rgba, (900, 700), &spec, 90);
        assert_eq!(a, b);
    }

    #[test]
    fn shrink_steps_by_ten_percent_down_to_the_floor() {
        let src = (4000, 3000);
        assert_eq!(shrink(src, src, 800), (3600, 2700));
        assert_eq!(shrink(src, (850, 638), 800), (800, 600));
        assert_eq!(shrink((5, 4), (5, 4), 1), (4, 3));
        assert_eq!(scaled(1000, (100, 100), (50, 50)), 250);
    }

    fn rgb(rgba: &[u8]) -> Vec<u8> {
        rgba.chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect()
    }
}
