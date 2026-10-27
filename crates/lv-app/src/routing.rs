//! Picks the decoder for each file: HEIF goes to the sandboxed worker pool, every other
//! format is decoded in this process by lv-core.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use lv_core::decode::{decode_file, open_image, DecodeError, DecodeLimits, DecodedImage, Frame};
use lv_core::exif_info::ExifData;
use lv_core::format::ImageFormat;
use lv_core::prefetch::Decode;
use lv_core::profile;

use crate::timing;
use crate::worker::WorkerPool;

/// The worker's file name; it is looked for in the app's own folder only.
const WORKER_EXE: &str = "lv-worker.exe";

pub struct RoutingDecoder {
    limits: DecodeLimits,
    /// `None` when `lv-worker.exe` is missing: HEIF files are then `Unsupported`. (A pool
    /// whose workers keep failing to start answers `Unsupported` by itself, ruling R4.)
    pool: Option<Arc<WorkerPool>>,
}

impl RoutingDecoder {
    /// Starts `workers` HEIF workers from `lv-worker.exe` next to the app, in the
    /// background. Never fails: without the worker, or if it cannot start, only HEIF is
    /// affected.
    pub fn start(limits: DecodeLimits, workers: usize) -> RoutingDecoder {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|app| app.parent().and_then(worker_exe_in));
        Self::with_worker(limits, exe, workers)
    }

    fn with_worker(limits: DecodeLimits, exe: Option<PathBuf>, workers: usize) -> RoutingDecoder {
        let pool = match exe {
            // The workers start on a thread of their own: the window does not wait.
            Some(exe) => Some(WorkerPool::start_in_background(
                exe,
                workers,
                limits.max_pixels,
            )),
            None => {
                eprintln!("light-view: {WORKER_EXE} not found next to the app; HEIC is off");
                None
            }
        };
        RoutingDecoder { limits, pool }
    }
}

impl RoutingDecoder {
    /// The worker pool, shared with the rotation save; `None` without `lv-worker.exe`.
    pub fn pool(&self) -> Option<Arc<WorkerPool>> {
        self.pool.clone()
    }
}

/// `lv-worker.exe` in `dir`, if it is there.
fn worker_exe_in(dir: &Path) -> Option<PathBuf> {
    let exe = dir.join(WORKER_EXE);
    exe.is_file().then_some(exe)
}

impl Decode for RoutingDecoder {
    /// Ends the HEIF workers, so a prefetch thread waiting on a hung one returns at once.
    fn shutdown(&self) {
        if let Some(pool) = &self.pool {
            pool.shutdown();
        }
    }

    fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError> {
        if !timing::is_target(path) {
            return self.decode_any(path);
        }
        timing::mark("decode-start-at");
        let start = Instant::now();
        let (result, phases) = profile::collect(|| self.decode_any(path));
        timing::add("decode-ms", start.elapsed().as_secs_f64() * 1000.0);
        for (name, ms) in phases {
            timing::add(&format!("decode.{name}"), ms);
        }
        timing::mark("decoded-at");
        result
    }
}

impl RoutingDecoder {
    fn decode_any(&self, path: &Path) -> Result<DecodedImage, DecodeError> {
        let image = open_image(path)?;
        if image.format != ImageFormat::Heif {
            drop(image);
            return decode_file(path, &self.limits);
        }
        let Some(pool) = &self.pool else {
            return Err(DecodeError::Unsupported);
        };
        // Within `max_file_bytes`, which is also the most a worker accepts.
        let bytes = profile::phase("read", || image.read_all(&self.limits))?;
        let byte_len = bytes.len() as u64;
        // Read here, on the prefetch thread, from the bytes the worker decodes (R5).
        let exif = profile::phase("exif", || ExifData::read(&bytes).map(Arc::new));
        let (width, height, bgra) = profile::phase("worker", || pool.decode_heif(bytes))?;
        Ok(DecodedImage {
            width,
            height,
            frames: vec![Frame { bgra, delay_ms: 0 }],
            format: ImageFormat::Heif,
            byte_len,
            exif,
            scaled_from: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::tests::worker_exe;

    fn heic_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/heif/rainbow-451x461.heic")
    }

    /// A 1x1 24-bit BMP holding one red pixel.
    fn bmp_file(dir: &tempfile::TempDir) -> PathBuf {
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&58u32.to_le_bytes()); // file size
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
        bmp.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        bmp.extend_from_slice(&1i32.to_le_bytes());
        bmp.extend_from_slice(&1i32.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
        bmp.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
        bmp.extend_from_slice(&[0; 4]); // BI_RGB
        bmp.extend_from_slice(&4u32.to_le_bytes()); // image size
        bmp.extend_from_slice(&[0; 16]);
        bmp.extend_from_slice(&[0, 0, 255, 0]); // BGR + row padding
        let path = dir.path().join("red.bmp");
        std::fs::write(&path, bmp).unwrap();
        path
    }

    fn limits() -> DecodeLimits {
        DecodeLimits {
            max_pixels: 100_000_000,
        }
    }

    #[test]
    fn routes_heif_to_pool_and_others_in_process() {
        let decoder = RoutingDecoder::with_worker(limits(), Some(worker_exe()), 1);
        let heic = decoder.decode(&heic_path()).expect("the sample decodes");
        assert_eq!(heic.format, ImageFormat::Heif);
        assert_eq!((heic.width, heic.height), (451, 461));
        assert_eq!(heic.frames.len(), 1);
        assert_eq!(heic.frames[0].delay_ms, 0);
        assert_eq!(heic.frames[0].bgra.len(), 451 * 461 * 4);
        assert_eq!(heic.byte_len, std::fs::metadata(heic_path()).unwrap().len());
        // EXIF comes from the same bytes the worker decoded (the sample has none).
        let bytes = std::fs::read(heic_path()).unwrap();
        assert_eq!(heic.exif.as_deref(), ExifData::read(&bytes).as_ref());

        let dir = tempfile::tempdir().unwrap();
        let bmp = decoder.decode(&bmp_file(&dir)).expect("the BMP decodes");
        assert_eq!(bmp.format, ImageFormat::Bmp);
        assert_eq!((bmp.width, bmp.height), (1, 1));
        assert_eq!(bmp.frames[0].bgra, vec![0, 0, 255, 255]);
    }

    #[test]
    fn dropping_the_prefetcher_with_a_hung_worker_is_quick() {
        use std::time::{Duration, Instant};

        let pool = WorkerPool::start_with(
            worker_exe(),
            1,
            limits().max_pixels,
            Duration::from_secs(10),
            &["--test-mode=hang"],
            &[],
        );
        let decoder = RoutingDecoder {
            limits: limits(),
            pool: Some(Arc::new(pool)),
        };
        let prefetcher = lv_core::prefetch::Prefetcher::new(decoder, 1, u64::MAX, |_| {});
        prefetcher.request(&[heic_path()]);
        // Let the request reach the worker, which never answers.
        std::thread::sleep(Duration::from_millis(500));
        let start = Instant::now();
        drop(prefetcher);
        let took = start.elapsed();
        assert!(took < Duration::from_secs(3), "exit waited {took:?}");
    }

    #[test]
    fn missing_worker_degrades_to_unsupported() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(worker_exe_in(empty.path()), None);
        let decoder = RoutingDecoder::with_worker(limits(), worker_exe_in(empty.path()), 2);
        assert!(matches!(
            decoder.decode(&heic_path()),
            Err(DecodeError::Unsupported)
        ));
        // Other formats are not affected.
        let bmp = decoder.decode(&bmp_file(&empty)).expect("the BMP decodes");
        assert_eq!((bmp.width, bmp.height), (1, 1));

        // Found only under its own name in the given folder.
        std::fs::write(empty.path().join(WORKER_EXE), b"").unwrap();
        assert_eq!(
            worker_exe_in(empty.path()),
            Some(empty.path().join(WORKER_EXE))
        );
    }

    /// Every supported file under `testdata/private/` (the user's own photos, not in git)
    /// goes through the real worker pool. Run with
    /// `cargo test -p lv-app -- --ignored --nocapture real_samples`. Paths are printed
    /// relative to that folder, and only into the test output.
    #[test]
    #[ignore = "needs the user's own photos in testdata/private/"]
    fn real_samples() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private");
        if !root.is_dir() {
            println!("testdata/private 없음 — 건너뜀");
            return;
        }
        let mut files = Vec::new();
        collect_supported(&root, &mut files);
        files.sort();
        let decoder = RoutingDecoder::with_worker(limits(), Some(worker_exe()), 2);
        let (mut ok, mut failed) = (0usize, 0usize);
        for file in &files {
            let name = file.strip_prefix(&root).unwrap_or(file).display();
            let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
            let started = std::time::Instant::now();
            match decoder.decode(file) {
                Ok(image) => {
                    ok += 1;
                    println!(
                        "ok     {name}: {:?} {}x{} {size} bytes, {} ms",
                        image.format,
                        image.width,
                        image.height,
                        started.elapsed().as_millis()
                    );
                }
                Err(e) => {
                    failed += 1;
                    let kind = match &e {
                        DecodeError::Unsupported => "Unsupported",
                        DecodeError::TooLarge { .. } => "TooLarge",
                        DecodeError::FileTooLarge { .. } => "FileTooLarge",
                        DecodeError::Corrupt(_) => "Corrupt",
                        DecodeError::Io(_) => "Io",
                    };
                    let dims = match &e {
                        DecodeError::TooLarge { width, height } => format!(" {width}x{height}"),
                        _ => String::new(),
                    };
                    println!("FAILED {name}: {kind}{dims} ({e}), {size} bytes");
                }
            }
        }
        println!(
            "real_samples: {} files, {ok} ok, {failed} failed",
            files.len()
        );
        assert_eq!(failed, 0, "some private samples did not decode");
    }

    fn collect_supported(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_supported(&path, out);
            } else if lv_core::format::is_supported_extension(&path) {
                out.push(path);
            }
        }
    }

    #[test]
    fn oversized_heif_is_rejected_unread() {
        // max_pixels 1: files over 64 MiB are refused.
        let small = DecodeLimits { max_pixels: 1 };
        let decoder = RoutingDecoder::with_worker(small, Some(worker_exe()), 1);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.heic");
        let file = std::fs::File::create(&path).unwrap();
        std::io::Write::write_all(&mut &file, &std::fs::read(heic_path()).unwrap()).unwrap();
        file.set_len((64 << 20) + 1).unwrap();
        drop(file);
        match decoder.decode(&path) {
            Err(DecodeError::FileTooLarge { bytes }) => assert_eq!(bytes, (64 << 20) + 1),
            other => panic!("expected FileTooLarge, got {other:?}"),
        }
    }
}
