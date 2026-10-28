//! WebP fits through the pool's sandboxed workers: one worker and one send per photo.

use std::sync::atomic::Ordering;
use std::time::Duration;

use lv_core::decode::DecodeError;
use lv_core::resize::{FitOutcome, Target};
use lv_ipc::{FitTarget, Response};

use super::protocol::Expect;
use super::tests::{alive, assert_decodes, assert_pool_not_responding, single_worker, worker_exe};
use super::{PoolError, PoolRequest, Reply, WorkerPool};

const MAX_PIXELS: u64 = 100_000_000;
const DEADLINE: Duration = Duration::from_secs(60);

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

fn fitted(outcome: Result<FitOutcome, PoolError>) -> (Vec<u8>, (u32, u32), u8) {
    match outcome {
        Ok(FitOutcome::Fitted {
            bytes,
            size,
            quality,
        }) => (bytes, size, quality),
        other => panic!("expected Fitted, got {other:?}"),
    }
}

#[test]
fn pool_fits_webp_in_sandbox() {
    let pool = WorkerPool::start(worker_exe(), 1, MAX_PIXELS);
    let pool_pid = single_worker(&pool).pid;
    let (w, h) = (640, 480);
    let px = picture(w, h, |x, _| if x < 64 { 0 } else { 255 });
    let at = |q| {
        let outcome = pool.fit_webp(&px, (w, h), &Target::Percent(100), q, DEADLINE);
        fitted(outcome).0.len() as u64
    };
    // A budget that quality 90 misses and quality 65 meets: the search has to come down.
    let (high, low) = (at(90), at(65));
    assert!(low < high, "{low} vs {high}");
    let limit = low + (high - low) / 4;
    let before = pool.single_use_pids().len();
    let target = Target::MaxBytes {
        bytes: limit,
        long_edge: None,
    };
    let (bytes, size, quality) = fitted(pool.fit_webp(&px, (w, h), &target, 90, DEADLINE));
    assert!(bytes.len() as u64 <= limit, "{} > {limit}", bytes.len());
    assert_eq!(size, (w, h));
    assert!((65..90).contains(&quality), "quality {quality}");
    let img = decode_webp(&bytes);
    assert_eq!(img.dimensions(), (w, h));
    assert_eq!(img.get_pixel(2, 2)[3], 0, "alpha is kept");
    assert_eq!(img.get_pixel(400, 2)[3], 255);
    // The whole search ran in one worker, started for this photo, which has ended.
    let spawned = pool.single_use_pids();
    assert_eq!(spawned.len(), before + 1, "one worker per photo");
    // Deterministic (ruling R1).
    let again = fitted(pool.fit_webp(&px, (w, h), &target, 90, DEADLINE));
    assert_eq!(again, (bytes, size, quality));
    // A resolution target.
    let (bytes, size, quality) =
        fitted(pool.fit_webp(&px, (w, h), &Target::LongEdge(320), 80, DEADLINE));
    assert_eq!((size, quality), ((320, 240), 80));
    assert_eq!(decode_webp(&bytes).dimensions(), (320, 240));
    // Every fit ran in a worker of its own, never the pool's, and none outlived its fit
    // (ruling R21); the pool's worker still decodes.
    for pid in pool.single_use_pids() {
        assert_ne!(pid, pool_pid);
        assert!(!alive(pid), "worker {pid} outlived its fit");
    }
    assert_decodes(&pool);
    assert_eq!(single_worker(&pool).pid, pool_pid);
    assert_eq!(pool.slot_states(), vec![(0, false)]);
}

#[test]
fn cant_fit_is_an_answer() {
    let pool = WorkerPool::start(worker_exe(), 0, MAX_PIXELS);
    let px = picture(200, 100, |_, _| 255);
    let target = Target::MaxBytes {
        bytes: 10,
        long_edge: None,
    };
    match pool.fit_webp(&px, (200, 100), &target, 90, DEADLINE) {
        Ok(FitOutcome::CantFit { smallest }) => assert!(smallest > 10),
        other => panic!("expected CantFit, got {other:?}"),
    }
    assert_eq!(pool.single_use_pids().len(), 1);
    assert!(!pool.single_use_degraded.load(Ordering::Relaxed));
}

#[test]
fn twelve_megapixels_fit_one_request() {
    // 48 MB of RGBA in one frame, to one worker, made into a 1920 px WebP there.
    let pool = WorkerPool::start(worker_exe(), 0, MAX_PIXELS);
    let (w, h) = (4000, 3000);
    let px = picture(w, h, |_, _| 255);
    let (bytes, size, _) =
        fitted(pool.fit_webp(&px, (w, h), &Target::LongEdge(1920), 85, DEADLINE));
    assert_eq!(size, (1920, 1440));
    assert_eq!(decode_webp(&bytes).dimensions(), (1920, 1440));
    assert_eq!(pool.single_use_pids().len(), 1);
}

#[test]
fn webp_fits_within_the_job_memory_limit() {
    // A worker limited to 12 MP runs in a 302 MiB Job. The largest source the shared rule
    // lets through, of libwebp's worst case (noise with alpha at quality 100), is searched
    // in it: the full-size encodes at quality 100 and 60, a resample to the 800 px floor and
    // its encodes, while the source stays in memory. 12 MP is refused as TooLarge before
    // any worker starts.
    let max_pixels = 12_000_000;
    assert_eq!(lv_ipc::worker_memory_limit(max_pixels), 316_435_456);
    let pool = WorkerPool::start(worker_exe(), 0, max_pixels);
    let (w, h) = (2900, 2000);
    assert!(lv_ipc::webp_fit_fits((w, h), (w, h), max_pixels));
    assert!(!lv_ipc::webp_fit_fits(
        (w, h + 100),
        (w, h + 100),
        max_pixels
    ));
    let target = Target::MaxBytes {
        bytes: 1,
        long_edge: None,
    };
    match pool.fit_webp(&noise(w, h), (w, h), &target, 100, DEADLINE) {
        Ok(FitOutcome::CantFit { smallest }) => assert!(smallest > 1),
        other => panic!("expected CantFit, got {other:?}"),
    }
    assert_eq!(pool.single_use_pids().len(), 1);

    for (w, h) in [(4000, 3000), (3000, 2000)] {
        let px = picture(w, h, |_, _| 255);
        assert!(matches!(
            pool.fit_webp(&px, (w, h), &Target::LongEdge(1920), 80, DEADLINE),
            Err(PoolError::Failed(DecodeError::TooLarge { width, height })) if (width, height) == (w, h)
        ));
    }
    assert_eq!(pool.single_use_pids().len(), 1, "nothing was sent");
}

/// Pixels, source size, target and start quality of one fit.
type Case<'a> = (&'a [u8], (u32, u32), Target, u8);

#[test]
fn bad_fit_requests_are_refused_before_sending() {
    let pool = WorkerPool::start(worker_exe(), 0, MAX_PIXELS);
    let px = picture(4, 4, |_, _| 255);
    let edge = Target::LongEdge(4);
    // Wrong pixel count, an empty image, start quality outside 1..=100, bad targets: the
    // app's own mistakes.
    let cases: [Case<'_>; 9] = [
        (&px[..63], (4, 4), edge, 80),
        (&px[..], (4, 3), edge, 80),
        (&[], (0, 4), edge, 80),
        (&px[..], (4, 4), edge, 0),
        (&px[..], (4, 4), edge, 101),
        (&px[..], (4, 4), Target::LongEdge(0), 80),
        (&px[..], (4, 4), Target::Percent(0), 80),
        (
            &px[..],
            (4, 4),
            Target::MaxBytes {
                bytes: 0,
                long_edge: None,
            },
            80,
        ),
        (
            &px[..],
            (4, 4),
            Target::MaxBytes {
                bytes: 100,
                long_edge: Some(0),
            },
            80,
        ),
    ];
    for (px, size, target, quality) in cases {
        assert!(
            matches!(
                pool.fit_webp(px, size, &target, quality, DEADLINE),
                Err(PoolError::Failed(DecodeError::Corrupt(_)))
            ),
            "{size:?} {target:?} q{quality}"
        );
    }
    // An output side over WebP's 16383; the same source made shorter would be sent.
    let row = vec![0u8; 16384 * 4];
    assert!(matches!(
        pool.fit_webp(&row, (16384, 1), &Target::Percent(100), 80, DEADLINE),
        Err(PoolError::Failed(DecodeError::TooLarge {
            width: 16384,
            height: 1
        }))
    ));
    // The same checks hold for a request made directly, sizes that overflow included.
    for (width, height) in [(u32::MAX, u32::MAX), (0, 0), (4, 4)] {
        let request = PoolRequest::FitWebp {
            width,
            height,
            start_quality: 80,
            target: FitTarget::LongEdge(4),
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
fn fit_answers_are_checked_against_the_request() {
    let pool = WorkerPool::start(worker_exe(), 0, MAX_PIXELS);
    let webp = |len: usize| {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(len as u32 - 8).to_le_bytes());
        v.extend_from_slice(b"WEBP");
        v.resize(len, 0);
        v
    };
    let expect = |budget| Expect::Fit {
        input_len: 64 * 48 * 4,
        src: (64, 48),
        start_quality: 80,
        budget,
    };
    let fitted = |width, height, quality, data| Response::Fitted {
        id: 7,
        width,
        height,
        quality,
        data,
    };
    let ok = |r: Result<Result<Reply, PoolError>, String>| matches!(r, Ok(Ok(Reply::Fit(_))));
    assert!(ok(pool.check(
        7,
        expect(None),
        fitted(64, 48, 80, webp(100))
    )));
    assert!(ok(pool.check(
        7,
        expect(Some(100)),
        fitted(32, 24, 60, webp(100))
    )));
    assert!(ok(pool.check(
        7,
        expect(Some(100)),
        Response::CantFit {
            id: 7,
            smallest: 101
        }
    )));
    // Protocol failures: over the budget, larger than the source, above the start quality,
    // not a WebP, a CantFit within the budget or for a resolution target, other answers.
    for (budget, response) in [
        (Some(99), fitted(64, 48, 80, webp(100))),
        (None, fitted(65, 48, 80, webp(100))),
        (None, fitted(64, 49, 80, webp(100))),
        (None, fitted(64, 48, 81, webp(100))),
        (None, fitted(64, 48, 80, vec![0; 100])),
        (
            Some(100),
            Response::CantFit {
                id: 7,
                smallest: 100,
            },
        ),
        (
            None,
            Response::CantFit {
                id: 7,
                smallest: 100,
            },
        ),
        (
            None,
            Response::Bytes {
                id: 7,
                data: webp(100),
            },
        ),
        (
            None,
            Response::Image {
                id: 7,
                width: 1,
                height: 1,
                bgra: vec![0; 4],
            },
        ),
        (
            None,
            Response::Fitted {
                id: 8,
                width: 64,
                height: 48,
                quality: 80,
                data: webp(100),
            },
        ),
    ] {
        assert!(pool.check(7, expect(budget), response).is_err());
    }
    // A fit answer to a rotation or a decode.
    let fit = fitted(1, 1, 50, webp(20));
    assert!(pool
        .check(7, Expect::Bytes { input_len: 10 }, fit.clone())
        .is_err());
    assert!(pool.check(7, Expect::Image, fit).is_err());
}

#[test]
fn wrong_answer_to_a_fit_is_a_protocol_failure() {
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
        pool.fit_webp(&px, (4, 4), &Target::LongEdge(4), 80, DEADLINE),
        Err(PoolError::WorkerUnavailable)
    ));
    let request = PoolRequest::FitWebp {
        width: 4,
        height: 4,
        start_quality: 80,
        target: FitTarget::LongEdge(4),
        rgba: px,
    };
    assert_pool_not_responding(pool.request(request, DEADLINE));
    let pids = pool.single_use_pids();
    assert_eq!(pids.len(), 2);
    assert!(pids.iter().all(|pid| !alive(*pid)), "not killed");
    assert!(!pool.single_use_degraded.load(Ordering::Relaxed));
}
