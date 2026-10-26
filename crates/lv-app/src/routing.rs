//! Picks the decoder for each file: HEIF goes to the sandboxed worker pool, every other
//! format is decoded in this process by lv-core.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lv_core::decode::{decode_file, open_image, DecodeError, DecodeLimits, DecodedImage, Frame};
use lv_core::format::ImageFormat;
use lv_core::prefetch::Decode;

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
    /// Starts `workers` HEIF workers from `lv-worker.exe` next to the app right away.
    /// Never fails: without the worker, or if it cannot start, only HEIF is affected.
    pub fn start(limits: DecodeLimits, workers: usize) -> RoutingDecoder {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|app| app.parent().and_then(worker_exe_in));
        Self::with_worker(limits, exe, workers)
    }

    fn with_worker(limits: DecodeLimits, exe: Option<PathBuf>, workers: usize) -> RoutingDecoder {
        let pool = match exe {
            Some(exe) => Some(Arc::new(WorkerPool::start(exe, workers, limits.max_pixels))),
            None => {
                eprintln!("light-view: {WORKER_EXE} not found next to the app; HEIC is off");
                None
            }
        };
        RoutingDecoder { limits, pool }
    }
}

/// `lv-worker.exe` in `dir`, if it is there.
fn worker_exe_in(dir: &Path) -> Option<PathBuf> {
    let exe = dir.join(WORKER_EXE);
    exe.is_file().then_some(exe)
}

impl Decode for RoutingDecoder {
    fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError> {
        let image = open_image(path)?;
        if image.format != ImageFormat::Heif {
            drop(image);
            return decode_file(path, &self.limits);
        }
        let Some(pool) = &self.pool else {
            return Err(DecodeError::Unsupported);
        };
        // Within `max_file_bytes`, which is also the most a worker accepts.
        let bytes = image.read_all(&self.limits)?;
        let byte_len = bytes.len() as u64;
        let (width, height, bgra) = pool.decode_heif(bytes)?;
        Ok(DecodedImage {
            width,
            height,
            frames: vec![Frame { bgra, delay_ms: 0 }],
            format: ImageFormat::Heif,
            byte_len,
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

        let dir = tempfile::tempdir().unwrap();
        let bmp = decoder.decode(&bmp_file(&dir)).expect("the BMP decodes");
        assert_eq!(bmp.format, ImageFormat::Bmp);
        assert_eq!((bmp.width, bmp.height), (1, 1));
        assert_eq!(bmp.frames[0].bgra, vec![0, 0, 255, 255]);
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
