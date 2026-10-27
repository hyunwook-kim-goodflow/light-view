//! Mipmaps for tiled images: box-filtered half-size levels, built on a background
//! thread so the UI stays responsive, and only down to the level that fitting the
//! image in the window needs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use lv_core::prefetch::DecodeResult;
use lv_core::tiling::mip_dims;

/// Rows halved between checks for cancellation.
const CANCEL_CHECK_ROWS: u32 = 64;

/// One mip level: premultiplied BGRA, `width * height * 4` bytes.
pub struct MipLevel {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Halves premultiplied BGRA `src` (`width`×`height`) to `mip_dims(.., 1)`: each
/// output pixel is the rounded mean of its 2×2 block, the last row or column repeated
/// where a side is odd. Averaging premultiplied values keeps edges against
/// transparency clean. `None` once `cancelled` returns true, or if `src` does not
/// hold `width * height` pixels.
pub fn halve(src: &[u8], width: u32, height: u32, cancelled: &dyn Fn() -> bool) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || src.len() != w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let (dw, dh) = mip_dims(width, height, 1);
    let (dw, dh) = (dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh * 4];
    for (dy, row) in out.chunks_exact_mut(dw * 4).enumerate() {
        if (dy as u32).is_multiple_of(CANCEL_CHECK_ROWS) && cancelled() {
            return None;
        }
        let y0 = (2 * dy) * w * 4;
        let y1 = (2 * dy + 1).min(h - 1) * w * 4;
        for (dx, px) in row.chunks_exact_mut(4).enumerate() {
            let x0 = 2 * dx * 4;
            let x1 = (2 * dx + 1).min(w - 1) * 4;
            for c in 0..4 {
                let sum = u32::from(src[y0 + x0 + c])
                    + u32::from(src[y0 + x1 + c])
                    + u32::from(src[y1 + x0 + c])
                    + u32::from(src[y1 + x1 + c]);
                px[c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    Some(out)
}

/// Levels `first` to `levels - 1` of `bgra`, each halved from the one before, given to
/// `emit` in order with their level number. A level is handed over once the next one
/// has been made from it (or it is the last), so at most two levels are held here at a
/// time and the receiver can free each after uploading it. Stops, returning false, if
/// cancelled or when `emit` returns false.
fn build_each(
    bgra: &[u8],
    (width, height): (u32, u32),
    (first, levels): (u8, u8),
    cancelled: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(u8, MipLevel) -> bool,
) -> bool {
    let mut prev: Option<MipLevel> = None;
    for level in 1..levels {
        let (src, w, h) = match &prev {
            Some(l) => (&l.bgra[..], l.width, l.height),
            None => (bgra, width, height),
        };
        let Some(half) = halve(src, w, h, cancelled) else {
            return false;
        };
        let (w, h) = mip_dims(w, h, 1);
        let made = MipLevel {
            width: w,
            height: h,
            bgra: half,
        };
        if let Some(done) = prev.replace(made) {
            if level > first && !emit(level - 1, done) {
                return false;
            }
        }
    }
    match prev {
        Some(last) if levels > first => emit(levels - 1, last),
        _ => true,
    }
}

/// A mip build running on its own thread. Dropping it cancels the build.
pub struct MipJob {
    rx: Receiver<(u8, MipLevel)>,
    cancel: Arc<AtomicBool>,
    /// Levels the build goes down to, counting level 0 (so the last is `levels - 1`).
    levels: u8,
}

impl MipJob {
    /// Builds levels `first` to `levels - 1` of `frame` of `source` (`first` ≥ 1; the
    /// levels before it are made only as steps), calling `wake` from the build thread
    /// after each level is ready, unless the job was dropped first.
    pub fn spawn(
        source: DecodeResult,
        frame: usize,
        (first, levels): (u8, u8),
        wake: impl Fn() + Send + 'static,
    ) -> MipJob {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("lv-mips".into())
            .spawn(move || {
                let Ok(img) = source.as_ref() else { return };
                let Some(f) = img.frames.get(frame) else {
                    return;
                };
                let cancelled = || flag.load(Ordering::Relaxed);
                let mut emit = |level, mip| {
                    if cancelled() || tx.send((level, mip)).is_err() {
                        return false;
                    }
                    wake();
                    true
                };
                let size = (img.width, img.height);
                build_each(&f.bgra, size, (first.max(1), levels), &cancelled, &mut emit);
            });
        if let Err(e) = spawned {
            // The image still draws from its full-size tiles.
            eprintln!("light-view: mip thread failed to start: {e}");
        }
        MipJob { rx, cancel, levels }
    }

    /// The levels finished since the last call, in order, with their level numbers.
    pub fn take_ready(&self) -> Vec<(u8, MipLevel)> {
        self.rx.try_iter().collect()
    }

    /// The level count (counting level 0) this build goes down to.
    pub fn levels(&self) -> u8 {
        self.levels
    }
}

impl Drop for MipJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lv_core::decode::{DecodedImage, Frame};
    use lv_core::format::ImageFormat;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    fn never() -> bool {
        false
    }

    #[test]
    fn halving_averages_two_by_two_blocks() {
        // 3x3, one channel varying; the odd last column and row repeat themselves.
        let mut src = Vec::new();
        for v in [0u8, 4, 8, 12, 16, 20, 24, 28, 32] {
            src.extend_from_slice(&[v, 0, 0, 255]);
        }
        let out = halve(&src, 3, 3, &never).unwrap();
        assert_eq!(out.len(), 2 * 2 * 4);
        let b: Vec<u8> = out.chunks(4).map(|p| p[0]).collect();
        // (0+4+12+16)/4, (8+8+20+20)/4, (24+28+24+28)/4, (32*4)/4
        assert_eq!(b, vec![8, 14, 26, 32]);
        assert!(out.chunks(4).all(|p| p[3] == 255));
    }

    #[test]
    fn halving_rejects_bad_buffers_and_stops_when_cancelled() {
        assert!(halve(&[0; 12], 2, 2, &never).is_none());
        assert!(halve(&[], 0, 0, &never).is_none());
        assert!(halve(&[0; 16], 2, 2, &|| true).is_none());
        // A 1-pixel-wide strip stays 1 wide.
        let out = halve(&[10, 20, 30, 40, 30, 40, 50, 60], 1, 2, &never).unwrap();
        assert_eq!(out, vec![20, 30, 40, 50]);
    }

    /// Every level `build_each` hands over, with its number.
    fn built(src: &[u8], size: (u32, u32), range: (u8, u8)) -> Vec<(u8, MipLevel)> {
        let mut out = Vec::new();
        let done = build_each(src, size, range, &never, &mut |level, mip| {
            out.push((level, mip));
            true
        });
        assert!(done);
        out
    }

    #[test]
    fn build_chains_levels_down_to_the_requested_one() {
        let (w, h) = (37u32, 5u32);
        let src = vec![200u8; (w * h * 4) as usize];
        let levels = built(&src, (w, h), (1, 4));
        let numbers: Vec<_> = levels.iter().map(|l| l.0).collect();
        assert_eq!(numbers, [1, 2, 3]);
        let dims: Vec<_> = levels.iter().map(|l| (l.1.width, l.1.height)).collect();
        assert_eq!(
            dims,
            vec![mip_dims(w, h, 1), mip_dims(w, h, 2), mip_dims(w, h, 3)]
        );
        assert!(levels
            .iter()
            .all(|(_, l)| l.bgra.len() == (l.width * l.height * 4) as usize
                && l.bgra.iter().all(|&v| v == 200)));
        assert!(built(&src, (w, h), (1, 1)).is_empty());
        assert!(built(&src, (w, h), (1, 0)).is_empty());
    }

    #[test]
    fn build_hands_over_from_first_and_stops_when_told() {
        let (w, h) = (64u32, 64u32);
        let src = vec![9u8; (w * h * 4) as usize];
        // Levels before `first` are made as steps only.
        let levels = built(&src, (w, h), (3, 5));
        let got: Vec<_> = levels.iter().map(|(n, l)| (*n, l.width)).collect();
        assert_eq!(got, [(3, 8), (4, 4)]);
        assert!(built(&src, (w, h), (5, 5)).is_empty());
        // A receiver that refuses stops the build.
        let mut calls = 0;
        let done = build_each(&src, (w, h), (1, 5), &never, &mut |_, _| {
            calls += 1;
            false
        });
        assert!(!done);
        assert_eq!(calls, 1);
    }

    fn image(width: u32, height: u32) -> DecodeResult {
        Arc::new(Ok(DecodedImage {
            width,
            height,
            frames: vec![Frame {
                bgra: vec![100; (width * height * 4) as usize],
                delay_ms: 0,
            }],
            format: ImageFormat::Png,
            byte_len: 0,
            exif: None,
            scaled_from: None,
        }))
    }

    #[test]
    fn job_builds_off_thread_and_wakes_per_level() {
        let (woke_tx, woke_rx) = channel();
        let job = MipJob::spawn(image(64, 32), 0, (1, 3), move || {
            let _ = woke_tx.send(std::thread::current().name().map(str::to_owned));
        });
        assert_eq!(job.levels(), 3);
        let mut levels = Vec::new();
        while levels.len() < 2 {
            let thread = woke_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(thread.as_deref(), Some("lv-mips"));
            levels.extend(job.take_ready());
        }
        let got: Vec<_> = levels
            .iter()
            .map(|(n, l)| (*n, l.width, l.height))
            .collect();
        assert_eq!(got, [(1, 32, 16), (2, 16, 8)]);
        assert!(job.take_ready().is_empty());
    }

    #[test]
    fn dropped_job_never_wakes() {
        let (woke_tx, woke_rx) = channel::<()>();
        // Big enough that the build is still running when the job is dropped.
        let job = MipJob::spawn(image(4096, 4096), 0, (1, 6), move || {
            let _ = woke_tx.send(());
        });
        drop(job);
        // The sender is dropped with the closure without being called.
        assert_eq!(
            woke_rx.recv_timeout(Duration::from_secs(30)),
            Err(RecvTimeoutError::Disconnected)
        );
    }
}
