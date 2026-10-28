//! Lossy WebP encoding through the pool's sandboxed workers.

use std::sync::atomic::Ordering;
use std::time::Duration;

use lv_core::decode::DecodeError;
use lv_core::resize::{fit, EncodeError, FitOutcome, OutputFormat, Quality, ResizeSpec, Target};

use super::tests::{alive, assert_decodes, assert_pool_not_responding, single_worker, worker_exe};
use super::{PoolError, PoolRequest, WorkerPool};

const MAX_PIXELS: u64 = 100_000_000;
const DEADLINE: Duration = Duration::from_secs(30);

/// A smooth gradient with some texture, opaque unless `alpha` says otherwise.
fn picture(width: u32, height: u32, alpha: impl Fn(u32, u32) -> u8) -> Vec<u8> {
    let mut px = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let t = ((x ^ y) & 15) as u8;
            px.extend_from_slice(&[
                (x * 200 / width) as u8 + t,
                (y * 200 / height) as u8 + t,
                ((x + y) * 100 / (width + height)) as u8 + t,
                alpha(x, y),
            ]);
        }
    }
    px
}

/// Every channel random (alpha included): libwebp's worst case for memory.
fn noise(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    let mut px = vec![0u8; width as usize * height as usize * 4];
    for chunk in px.chunks_exact_mut(4) {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    px
}

fn decode_webp(bytes: &[u8]) -> image::RgbaImage {
    image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)
        .expect("a WebP the image crate decodes")
        .to_rgba8()
}

#[test]
fn pool_encodes_webp_in_sandbox() {
    let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
    let pool_pid = single_worker(&pool).pid;
    let px = picture(64, 64, |x, _| if x < 8 { 0 } else { 255 });
    let webp = pool.encode_webp(&px, (64, 64), 80, DEADLINE).unwrap();
    assert_eq!(&webp[..4], b"RIFF");
    assert_eq!(&webp[8..12], b"WEBP");
    let img = decode_webp(&webp);
    assert_eq!(img.dimensions(), (64, 64));
    assert_eq!(img.get_pixel(2, 2)[3], 0, "alpha is kept");
    assert_eq!(img.get_pixel(40, 2)[3], 255);
    // Deterministic (ruling R1), and lower quality is smaller.
    assert_eq!(pool.encode_webp(&px, (64, 64), 80, DEADLINE).unwrap(), webp);
    assert!(pool.encode_webp(&px, (64, 64), 60, DEADLINE).unwrap().len() < webp.len());
    // Each encode ran in a worker of its own, which ended after it (ruling R21); the pool's
    // worker still decodes.
    let encoders = pool.single_use_pids();
    assert_eq!(encoders.len(), 3);
    for pid in &encoders {
        assert_ne!(*pid, pool_pid);
        assert!(!alive(*pid), "worker {pid} outlived its encode");
    }
    assert_decodes(&pool);
    assert_eq!(single_worker(&pool).pid, pool_pid);
    assert_eq!(pool.slot_states(), vec![(0, false)]);
}

#[test]
fn webp_fit_under_limit() {
    let pool = WorkerPool::start(worker_exe(), 0, MAX_PIXELS);
    let (w, h) = (640, 480);
    let px = picture(w, h, |_, _| 255);
    let at = |q| pool.encode_webp(&px, (w, h), q, DEADLINE).unwrap().len() as u64;
    // A budget that quality 90 misses and quality 65 meets: the search has to come down.
    let (high, low) = (at(90), at(65));
    assert!(low < high, "{low} vs {high}");
    let limit = low + (high - low) / 4;
    let encoder = |rgba: &[u8], size: (u32, u32), quality: u8| {
        pool.encode_webp(rgba, size, quality, DEADLINE)
            .map_err(|e| EncodeError::Failed(e.to_string()))
    };
    let spec = ResizeSpec {
        target: Target::MaxBytes {
            bytes: limit,
            long_edge: None,
        },
        format: OutputFormat::WebP,
        quality: Quality::Fixed(90),
        strip_gps: true,
    };
    match fit(&px, (w, h), &spec, 90, &encoder).unwrap() {
        FitOutcome::Fitted {
            bytes,
            size,
            quality,
        } => {
            assert!(bytes.len() as u64 <= limit, "{} > {limit}", bytes.len());
            assert_eq!(size, (w, h));
            assert!((65..90).contains(&quality), "quality {quality}");
            assert_eq!(decode_webp(&bytes).dimensions(), (w, h));
        }
        other => panic!("expected Fitted, got {other:?}"),
    }
}

#[test]
fn twelve_megapixels_fit_one_request() {
    // 48 MB of RGBA at the default-sized limit used by these tests: one frame, one worker.
    let pool = WorkerPool::start(worker_exe(), 0, MAX_PIXELS);
    let (w, h) = (4000, 3000);
    let px = picture(w, h, |_, _| 255);
    let webp = pool.encode_webp(&px, (w, h), 75, DEADLINE).unwrap();
    assert_eq!(decode_webp(&webp).dimensions(), (w, h));
}

#[test]
fn webp_encodes_within_the_job_memory_limit() {
    // A worker limited to 12 MP runs in a 302 MiB Job. The largest encode the shared rule
    // lets through, of libwebp's worst case (noise with alpha at quality 100), succeeds in
    // it; 12 MP is refused as TooLarge before any worker starts.
    let max_pixels = 12_000_000;
    assert_eq!(lv_ipc::worker_memory_limit(max_pixels), 316_435_456);
    let pool = WorkerPool::start(worker_exe(), 0, max_pixels);
    let (w, h) = (3200, 2000);
    assert!(lv_ipc::webp_encode_fits(w, h, max_pixels));
    assert!(!lv_ipc::webp_encode_fits(w, h + 100, max_pixels));
    let webp = pool
        .encode_webp(&noise(w, h), (w, h), 100, DEADLINE)
        .unwrap();
    assert_eq!(decode_webp(&webp).dimensions(), (w, h));
    assert_eq!(pool.single_use_pids().len(), 1);

    let px = picture(4000, 3000, |_, _| 255);
    assert!(matches!(
        pool.encode_webp(&px, (4000, 3000), 80, DEADLINE),
        Err(PoolError::Failed(DecodeError::TooLarge {
            width: 4000,
            height: 3000
        }))
    ));
    assert_eq!(pool.single_use_pids().len(), 1, "nothing was sent");
}

#[test]
fn bad_encode_requests_are_refused_before_sending() {
    let pool = WorkerPool::start(worker_exe(), 0, MAX_PIXELS);
    let px = picture(4, 4, |_, _| 255);
    // Wrong pixel count, an empty image, quality over 100: the app's own mistakes.
    for (px, size, quality) in [
        (&px[..63], (4, 4), 80),
        (&px[..], (4, 3), 80),
        (&[][..], (0, 4), 80),
        (&px[..], (4, 4), 101),
    ] {
        assert!(
            matches!(
                pool.encode_webp(px, size, quality, DEADLINE),
                Err(PoolError::Failed(DecodeError::Corrupt(_)))
            ),
            "{size:?} q{quality}"
        );
    }
    // A side over WebP's 16383.
    let row = vec![0u8; 16384 * 4];
    assert!(matches!(
        pool.encode_webp(&row, (16384, 1), 80, DEADLINE),
        Err(PoolError::Failed(DecodeError::TooLarge {
            width: 16384,
            height: 1
        }))
    ));
    // The same checks hold for a request made directly, sizes that overflow included.
    for (width, height) in [(u32::MAX, u32::MAX), (0, 0), (4, 4)] {
        let request = PoolRequest::EncodeWebp {
            width,
            height,
            quality: 80,
            rgba: vec![0; 4],
        };
        assert!(matches!(
            pool.request(request, DEADLINE),
            Err(PoolError::Failed(DecodeError::Corrupt(_)))
        ));
    }
    assert!(pool.single_use_pids().is_empty(), "nothing was sent");
}

#[test]
fn wrong_answer_to_an_encode_is_a_protocol_failure() {
    let pool = WorkerPool::start_with(
        worker_exe(),
        0,
        MAX_PIXELS,
        Duration::from_secs(10),
        &[],
        &["--test-mode=wrong-kind"],
    );
    let px = picture(4, 4, |_, _| 255);
    assert!(matches!(
        pool.encode_webp(&px, (4, 4), 80, DEADLINE),
        Err(PoolError::WorkerUnavailable)
    ));
    let request = PoolRequest::EncodeWebp {
        width: 4,
        height: 4,
        quality: 80,
        rgba: px,
    };
    assert_pool_not_responding(pool.request(request, DEADLINE));
    let pids = pool.single_use_pids();
    assert_eq!(pids.len(), 2);
    assert!(pids.iter().all(|pid| !alive(*pid)), "not killed");
    assert!(!pool.single_use_degraded.load(Ordering::Relaxed));
}
