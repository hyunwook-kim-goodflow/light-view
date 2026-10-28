//! `FitWebp`: the whole fit-to-size search for one photo, with libwebp as the encoder, so
//! the app sends the pixels once and the trials repeat here (spec §10).

use std::panic::{self, AssertUnwindSafe};

use lv_core::resize::{
    fit, target_size, EncodeError, FitOutcome, OutputFormat, Quality, ResizeSpec, Target,
};
use lv_ipc::{max_bytes_response, webp_fit_fits, FailKind, FitTarget, Response};

/// The lv-core target a wire target stands for.
pub fn to_target(target: FitTarget) -> Target {
    match target {
        FitTarget::LongEdge(n) => Target::LongEdge(n),
        FitTarget::Width(n) => Target::Width(n),
        FitTarget::Height(n) => Target::Height(n),
        FitTarget::MaxBytes { bytes, long_edge } => Target::MaxBytes { bytes, long_edge },
        FitTarget::Percent(p) => Target::Percent(p),
    }
}

/// Resizes the app's straight RGBA source (`src`; lv-ipc has checked that `rgba` is
/// `src.0 * src.1 * 4` bytes) to `target` and encodes it as a lossy WebP with
/// `lv_core::resize::fit`, from `start_quality` down. A source over the worker's pixel
/// limit, over what its Job leaves for the fit, or whose output would have a side over
/// WebP's 16383 ([`webp_fit_fits`]) is `TooLarge` with the source size, before anything
/// runs. A failure inside libwebp (out of memory included) or a panic in the resampling
/// is `Corrupt`; crashes in libwebp end the process.
pub fn fit_webp(
    id: u64,
    src: (u32, u32),
    start_quality: u8,
    target: FitTarget,
    rgba: &[u8],
    max_pixels: u64,
) -> Response {
    let target = to_target(target);
    let (width, height) = src;
    if !webp_fit_fits(src, target_size(src, &target), max_pixels) {
        let kind = FailKind::TooLarge { width, height };
        return Response::Failed { id, kind };
    }
    let spec = ResizeSpec {
        target,
        format: OutputFormat::WebP,
        quality: Quality::Fixed(start_quality),
        strip_gps: true,
    };
    let encoder = |px: &[u8], size: (u32, u32), quality: u8| {
        lv_webp::encode_rgba(px, size.0, size.1, quality).map_err(EncodeError::Failed)
    };
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        fit(rgba, src, &spec, start_quality, &encoder)
    }));
    let kind = match outcome {
        Ok(Ok(FitOutcome::Fitted {
            bytes,
            size,
            quality,
        })) if bytes.len() as u64 <= max_bytes_response(rgba.len() as u64) => {
            return Response::Fitted {
                id,
                width: size.0,
                height: size.1,
                quality,
                data: bytes,
            }
        }
        Ok(Ok(FitOutcome::Fitted { bytes, .. })) => FailKind::Corrupt(format!(
            "{}-byte WebP for {} bytes of pixels",
            bytes.len(),
            rgba.len()
        )),
        Ok(Ok(FitOutcome::CantFit { smallest })) => return Response::CantFit { id, smallest },
        Ok(Err(e)) => FailKind::Corrupt(e.to_string()),
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            FailKind::Corrupt(format!("fit panicked: {what}"))
        }
    };
    Response::Failed { id, kind }
}
