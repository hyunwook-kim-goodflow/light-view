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

/// Levels 1 to `levels - 1` of `bgra`, each halved from the one before. `None` if
/// cancelled.
fn build(
    bgra: &[u8],
    width: u32,
    height: u32,
    levels: u8,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<MipLevel>> {
    let mut out: Vec<MipLevel> = Vec::new();
    for _ in 1..levels {
        let (src, w, h) = match out.last() {
            Some(l) => (&l.bgra[..], l.width, l.height),
            None => (bgra, width, height),
        };
        let half = halve(src, w, h, cancelled)?;
        let (width, height) = mip_dims(w, h, 1);
        out.push(MipLevel {
            width,
            height,
            bgra: half,
        });
    }
    Some(out)
}

/// A mip build running on its own thread. Dropping it cancels the build.
pub struct MipJob {
    rx: Receiver<Vec<MipLevel>>,
    cancel: Arc<AtomicBool>,
}

impl MipJob {
    /// Builds levels 1 to `levels - 1` of `frame` of `source`, then calls `wake`
    /// (from the build thread) unless the job was dropped first.
    pub fn spawn(
        source: DecodeResult,
        frame: usize,
        levels: u8,
        wake: impl FnOnce() + Send + 'static,
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
                if let Some(mips) = build(&f.bgra, img.width, img.height, levels, &cancelled) {
                    if !cancelled() && tx.send(mips).is_ok() {
                        wake();
                    }
                }
            });
        if let Err(e) = spawned {
            // The image still draws from its full-size tiles.
            eprintln!("light-view: mip thread failed to start: {e}");
        }
        MipJob { rx, cancel }
    }

    /// The finished levels, once.
    pub fn try_take(&self) -> Option<Vec<MipLevel>> {
        self.rx.try_recv().ok()
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

    #[test]
    fn build_chains_levels_down_to_the_requested_one() {
        let (w, h) = (37u32, 5u32);
        let src = vec![200u8; (w * h * 4) as usize];
        let levels = build(&src, w, h, 4, &never).unwrap();
        let dims: Vec<_> = levels.iter().map(|l| (l.width, l.height)).collect();
        assert_eq!(
            dims,
            vec![mip_dims(w, h, 1), mip_dims(w, h, 2), mip_dims(w, h, 3)]
        );
        assert!(levels
            .iter()
            .all(|l| l.bgra.len() == (l.width * l.height * 4) as usize
                && l.bgra.iter().all(|&v| v == 200)));
        assert!(build(&src, w, h, 1, &never).unwrap().is_empty());
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
        }))
    }

    #[test]
    fn job_builds_off_thread_and_wakes() {
        let (woke_tx, woke_rx) = channel();
        let job = MipJob::spawn(image(64, 32), 0, 3, move || {
            let _ = woke_tx.send(std::thread::current().name().map(str::to_owned));
        });
        let thread = woke_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(thread.as_deref(), Some("lv-mips"));
        let levels = job.try_take().expect("levels after the wake-up");
        assert_eq!(levels.len(), 2);
        assert_eq!((levels[1].width, levels[1].height), (16, 8));
        assert!(job.try_take().is_none());
    }

    #[test]
    fn dropped_job_never_wakes() {
        let (woke_tx, woke_rx) = channel::<()>();
        // Big enough that the build is still running when the job is dropped.
        let job = MipJob::spawn(image(4096, 4096), 0, 6, move || {
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
