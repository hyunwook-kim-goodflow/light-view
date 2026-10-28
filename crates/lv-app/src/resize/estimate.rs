//! Making a photo's result (spec §5, §10): decode, turn, fit, metadata. The same code
//! makes the estimates, on background threads, and a save's bytes when no estimate is
//! kept, so a save writes exactly what the estimate measured (ruling R1).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use lv_core::decode::{decode_bytes, open_image, DecodeLimits, DecodedImage, Frame};
use lv_core::exif_info::raw_exif_block;
use lv_core::format::ImageFormat;
use lv_core::resize::{
    attach_exif, encode, estimate_jpeg_quality, exif_overhead, fit, to_straight_rgba,
    whitelist_exif, FitOutcome, OutputFormat, Quality, ResizeSpec, Target,
};

use super::session::{Estimate, Source};
use crate::commands::HwndCell;
use crate::worker::WorkerPool;

/// How long the worker may take to fit one WebP photo (the worker tests use the same).
const WEBP_DEADLINE: Duration = Duration::from_secs(60);
/// Quality for 원본 수준 when the source is not a JPEG (or its tables say nothing).
const DEFAULT_QUALITY: u8 = 90;
/// Photos estimated at once: each holds its decoded pixels while it is made (spec §9's
/// memory limit).
pub const ESTIMATE_THREADS: usize = 2;

/// A photo's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ready { size: (u32, u32), bytes: Vec<u8> },
    CantFit,
    Failed(String),
}

impl Outcome {
    /// What the list shows for it.
    pub fn estimate(&self) -> Estimate {
        match self {
            Outcome::Ready { size, bytes } => Estimate::Ready {
                size: *size,
                bytes: bytes.len() as u64,
            },
            Outcome::CantFit => Estimate::CantFit,
            Outcome::Failed(e) => Estimate::Failed(e.clone()),
        }
    }
}

/// What making a result needs besides the photo: the decode limits and the workers
/// (HEIF decoding, WebP encoding).
#[derive(Clone)]
pub struct Maker {
    pub limits: DecodeLimits,
    pub pool: Option<Arc<WorkerPool>>,
}

/// Makes the result of the photo at `path` turned by `quarters` (clockwise) for `spec`,
/// and tells what decoding found. Deterministic: the same file and spec give the same
/// bytes.
pub fn produce(
    path: &Path,
    quarters: u8,
    spec: &ResizeSpec,
    maker: &Maker,
) -> (Option<Source>, Outcome) {
    let (file, image) = match decode(path, maker) {
        Ok(d) => d,
        Err(e) => return (None, Outcome::Failed(e)),
    };
    let animated = image.frames.len() > 1;
    let scaled = image.scaled_from.is_some();
    let (format, decoded) = (image.format, (image.width, image.height));
    // Only the first frame of an animation ("애니메이션 → 첫 장면").
    let Some(frame) = image.frames.first() else {
        return (None, Outcome::Failed("no frames".into()));
    };
    let rgba = to_straight_rgba(frame);
    // The premultiplied copy is not needed any more.
    drop(image);
    let (rgba, size) = if quarters.is_multiple_of(4) {
        (rgba, decoded)
    } else {
        rotate_rgba(&rgba, decoded, quarters)
    };
    let source = Source {
        size,
        bytes: file.len() as u64,
        animated,
        scaled,
    };
    (Some(source), make(&rgba, size, &file, format, spec, maker))
}

/// The file's bytes and its decode: in this process, HEIF in a worker (spec §9).
fn decode(path: &Path, maker: &Maker) -> Result<(Vec<u8>, DecodedImage), String> {
    let file = open_image(path).map_err(|e| e.to_string())?;
    let format = file.format;
    let bytes = file.read_all(&maker.limits).map_err(|e| e.to_string())?;
    if format != ImageFormat::Heif {
        let image = decode_bytes(&bytes, &maker.limits).map_err(|e| e.to_string())?;
        return Ok((bytes, image));
    }
    let pool = maker.pool.as_ref().ok_or("HEIC needs lv-worker.exe")?;
    let (width, height, bgra) = pool.decode_heif(bytes.clone()).map_err(|e| e.to_string())?;
    let image = DecodedImage {
        width,
        height,
        frames: vec![Frame { bgra, delay_ms: 0 }],
        format,
        byte_len: bytes.len() as u64,
        exif: None,
        scaled_from: None,
    };
    Ok((bytes, image))
}

/// Fits `rgba` (straight RGBA8 of `size`) to `spec` and attaches the whitelisted EXIF of
/// the source file `src` (a `src_format` file). A byte limit leaves room for the EXIF,
/// and the finished bytes are checked against it once more.
fn make(
    rgba: &[u8],
    size: (u32, u32),
    src: &[u8],
    src_format: ImageFormat,
    spec: &ResizeSpec,
    maker: &Maker,
) -> Outcome {
    let start = match spec.quality {
        Quality::Fixed(q) => q,
        Quality::MatchOriginal if src_format == ImageFormat::Jpeg => {
            estimate_jpeg_quality(src).unwrap_or(DEFAULT_QUALITY)
        }
        Quality::MatchOriginal => DEFAULT_QUALITY,
    };
    // PNG output carries no metadata (spec §10).
    let exif = (spec.format != OutputFormat::Png)
        .then(|| whitelist_exif(raw_exif_block(src), !spec.strip_gps))
        .flatten();
    let (target, limit) = match spec.target {
        Target::MaxBytes { bytes, long_edge } => {
            let room = exif
                .as_ref()
                .map_or(0, |e| exif_overhead(spec.format, e.len()));
            let target = Target::MaxBytes {
                bytes: bytes.saturating_sub(room),
                long_edge,
            };
            (target, Some(bytes))
        }
        other => (other, None),
    };
    let fitted = match spec.format {
        OutputFormat::WebP => match &maker.pool {
            Some(pool) => pool
                .fit_webp(rgba, size, &target, start, WEBP_DEADLINE)
                .map_err(|e| e.to_string()),
            None => Err("WebP needs lv-worker.exe".to_owned()),
        },
        format => {
            let fit_spec = ResizeSpec { target, ..*spec };
            let encoder = |px: &[u8], s: (u32, u32), q: u8| encode(px, s, format, q);
            fit(rgba, size, &fit_spec, start, &encoder).map_err(|e| e.to_string())
        }
    };
    let (bytes, size) = match fitted {
        Err(e) => return Outcome::Failed(e),
        Ok(FitOutcome::CantFit { .. }) => return Outcome::CantFit,
        Ok(FitOutcome::Fitted { bytes, size, .. }) => (bytes, size),
    };
    let bytes = match &exif {
        Some(e) => match attach_exif(&bytes, spec.format, e) {
            Ok(b) => b,
            Err(e) => return Outcome::Failed(e),
        },
        None => bytes,
    };
    if limit.is_some_and(|l| bytes.len() as u64 > l) {
        return Outcome::CantFit;
    }
    Outcome::Ready { size, bytes }
}

/// Turns straight RGBA8 of `size` clockwise by `quarters` quarter turns.
pub fn rotate_rgba(rgba: &[u8], size: (u32, u32), quarters: u8) -> (Vec<u8>, (u32, u32)) {
    let q = quarters % 4;
    let (w, h) = (size.0 as usize, size.1 as usize);
    if q == 0 || rgba.len() != w * h * 4 {
        return (rgba.to_vec(), size);
    }
    let out_size = if q % 2 == 1 { (size.1, size.0) } else { size };
    let out_w = out_size.0 as usize;
    let mut out = vec![0; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match q {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let (from, to) = ((y * w + x) * 4, (ny * out_w + nx) * 4);
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    (out, out_size)
}

/// The results the estimates made, kept for the save (ruling R1): the newest per photo,
/// at most `cap` bytes in all (a quarter of the memory limit); the oldest go first and
/// are made again by the save.
#[derive(Debug, Default)]
pub struct ByteCache {
    entries: VecDeque<(PathBuf, u64, Vec<u8>)>,
    total: u64,
    cap: u64,
}

impl ByteCache {
    pub fn new(cap: u64) -> ByteCache {
        ByteCache {
            cap,
            ..ByteCache::default()
        }
    }

    /// Keeps `bytes` for `path` made under `generation`, replacing what it had.
    pub fn put(&mut self, path: &Path, generation: u64, bytes: Vec<u8>) {
        self.remove(|p, _| p == path);
        let len = bytes.len() as u64;
        if len > self.cap {
            return;
        }
        self.entries
            .push_back((path.to_path_buf(), generation, bytes));
        self.total += len;
        while self.total > self.cap {
            let Some((_, _, old)) = self.entries.pop_front() else {
                break;
            };
            self.total -= old.len() as u64;
        }
    }

    /// The bytes for `path` made under `generation`, taken out.
    pub fn take(&mut self, path: &Path, generation: u64) -> Option<Vec<u8>> {
        self.remove(|p, g| p == path && g == generation)
    }

    fn remove(&mut self, matches: impl Fn(&Path, u64) -> bool) -> Option<Vec<u8>> {
        let at = self.entries.iter().position(|(p, g, _)| matches(p, *g))?;
        let (_, _, bytes) = self.entries.remove(at)?;
        self.total -= bytes.len() as u64;
        Some(bytes)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.total = 0;
    }
}

/// One photo to estimate.
#[derive(Clone, Debug)]
pub struct Job {
    pub generation: u64,
    pub index: usize,
    pub path: PathBuf,
    pub quarters: u8,
    pub spec: ResizeSpec,
}

/// A finished estimate.
pub struct Done {
    pub job: Job,
    pub source: Option<Source>,
    pub outcome: Outcome,
}

#[derive(Default)]
struct Queue {
    generation: u64,
    jobs: VecDeque<Job>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

/// The estimate threads: jobs of the newest generation only, in the order given, each
/// result sent to `rx` and announced by posting `msg` to the window.
pub struct Estimator {
    shared: Arc<Shared>,
    pub rx: Receiver<Done>,
}

impl Estimator {
    pub fn start(maker: Maker, threads: usize, hwnd: HwndCell, msg: u32) -> Estimator {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let (tx, rx) = channel();
        for _ in 0..threads.max(1) {
            let (shared, maker, tx, hwnd) =
                (shared.clone(), maker.clone(), tx.clone(), hwnd.clone());
            std::thread::spawn(move || estimate_thread(&shared, &maker, &tx, &hwnd, msg));
        }
        Estimator { shared, rx }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        lock(&self.shared)
    }

    /// Queues `jobs` (all of one generation). A newer generation drops every job still
    /// queued; a job already running finishes and its result is stale.
    pub fn submit(&self, jobs: Vec<Job>) {
        let Some(first) = jobs.first() else {
            return;
        };
        let mut q = self.lock();
        if first.generation != q.generation {
            q.jobs.clear();
            q.generation = first.generation;
        }
        q.jobs.extend(jobs);
        drop(q);
        self.shared.wake.notify_all();
    }

    /// Puts the queued jobs in `order` (indices), the rest after them as they were.
    pub fn reorder(&self, order: &[usize]) {
        let mut q = self.lock();
        let mut rest: Vec<Job> = q.jobs.drain(..).collect();
        for &i in order {
            if let Some(at) = rest.iter().position(|j| j.index == i) {
                q.jobs.push_back(rest.remove(at));
            }
        }
        q.jobs.extend(rest);
    }

    /// Drops every queued job.
    pub fn cancel(&self) {
        self.lock().jobs.clear();
    }
}

impl Drop for Estimator {
    /// The threads end once their running job is done (its result has nowhere to go).
    fn drop(&mut self) {
        let mut q = self.lock();
        q.closed = true;
        q.jobs.clear();
        drop(q);
        self.shared.wake.notify_all();
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, Queue> {
    shared
        .queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn estimate_thread(shared: &Shared, maker: &Maker, tx: &Sender<Done>, hwnd: &HwndCell, msg: u32) {
    loop {
        let job = {
            let mut q = lock(shared);
            loop {
                if q.closed {
                    return;
                }
                if let Some(job) = q.jobs.pop_front() {
                    break job;
                }
                q = shared
                    .wake
                    .wait(q)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            produce(&job.path, job.quarters, &job.spec, maker)
        }));
        let (source, outcome) =
            made.unwrap_or_else(|_| (None, Outcome::Failed("the encoder panicked".into())));
        if tx
            .send(Done {
                job,
                source,
                outcome,
            })
            .is_err()
        {
            return;
        }
        hwnd.post(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::tests::worker_exe;

    fn folder() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap()
    }

    fn png(dir: &Path, name: &str, size: (u32, u32), px: impl Fn(u32, u32) -> [u8; 4]) -> PathBuf {
        let mut img = image::RgbaImage::new(size.0, size.1);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba(px(x, y));
        }
        let path = dir.join(name);
        img.save(&path).unwrap();
        path
    }

    /// Deterministic noise: hard to compress, like a grainy photo.
    fn noise(x: u32, y: u32) -> [u8; 4] {
        let h =
            (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)).wrapping_mul(2_246_822_519);
        [(h >> 8) as u8, (h >> 16) as u8, (h >> 24) as u8, 255]
    }

    fn maker() -> Maker {
        Maker {
            limits: DecodeLimits::default(),
            pool: None,
        }
    }

    fn spec(target: Target, format: OutputFormat) -> ResizeSpec {
        ResizeSpec {
            target,
            format,
            quality: Quality::MatchOriginal,
            strip_gps: true,
        }
    }

    fn ready(outcome: Outcome) -> ((u32, u32), Vec<u8>) {
        match outcome {
            Outcome::Ready { size, bytes } => (size, bytes),
            other => panic!("not ready: {other:?}"),
        }
    }

    #[test]
    fn rotation_turns_clockwise() {
        // [A B] turned once clockwise is a column A over B; twice, [B A].
        let (a, b) = ([1, 2, 3, 4], [5, 6, 7, 8]);
        let rgba = [a, b].concat();
        assert_eq!(rotate_rgba(&rgba, (2, 1), 1), ([a, b].concat(), (1, 2)));
        assert_eq!(rotate_rgba(&rgba, (2, 1), 2), ([b, a].concat(), (2, 1)));
        assert_eq!(rotate_rgba(&rgba, (2, 1), 3), ([b, a].concat(), (1, 2)));
        assert_eq!(rotate_rgba(&rgba, (2, 1), 4), (rgba.clone(), (2, 1)));
        // 2×2: [A B / C D] clockwise is [C A / D B].
        let (c, d) = ([9; 4], [10; 4]);
        let square = [a, b, c, d].concat();
        assert_eq!(rotate_rgba(&square, (2, 2), 1).0, [c, a, d, b].concat());
    }

    #[test]
    fn pending_rotation_is_applied() {
        let dir = folder();
        // Red on the left, blue on the right.
        let path = png(dir.path(), "a.png", (2, 1), |x, _| {
            if x == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        });
        let before = std::fs::read(&path).unwrap();
        let spec = spec(Target::LongEdge(1920), OutputFormat::Png);
        let (source, outcome) = produce(&path, 1, &spec, &maker());
        let source = source.expect("decoded");
        assert_eq!(source.size, (1, 2), "the size as shown, turned");
        assert!(!source.animated && !source.scaled);
        let (size, bytes) = ready(outcome);
        assert_eq!(size, (1, 2));
        let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (1, 2));
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255], "red on top");
        assert_eq!(img.get_pixel(0, 1).0, [0, 0, 255, 255]);
        // The original is untouched.
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn estimate_bytes_meet_the_limit_and_repeat_exactly() {
        let dir = folder();
        let path = png(dir.path(), "noise.png", (1200, 900), noise);
        let spec = spec(
            Target::MaxBytes {
                bytes: 600_000,
                long_edge: None,
            },
            OutputFormat::Jpeg,
        );
        let (_, first) = produce(&path, 0, &spec, &maker());
        let (size, bytes) = ready(first.clone());
        assert!(bytes.len() <= 600_000, "{}", bytes.len());
        assert!(size.0 <= 1200);
        // R1: made again from the same file and spec, the very same bytes.
        assert_eq!(produce(&path, 0, &spec, &maker()).1, first);
        // A limit nothing can meet: 맞추기 불가, not a failure.
        let tiny = ResizeSpec {
            target: Target::MaxBytes {
                bytes: 2_000,
                long_edge: None,
            },
            ..spec
        };
        assert_eq!(produce(&path, 0, &tiny, &maker()).1, Outcome::CantFit);
    }

    #[test]
    fn unreadable_photo_fails() {
        let dir = folder();
        let path = dir.path().join("broken.jpg");
        std::fs::write(&path, b"not a photo").unwrap();
        let spec = spec(Target::LongEdge(100), OutputFormat::Jpeg);
        let (source, outcome) = produce(&path, 0, &spec, &maker());
        assert_eq!(source, None);
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
        let (_, outcome) = produce(&dir.path().join("gone.jpg"), 0, &spec, &maker());
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
    }

    #[test]
    fn webp_goes_through_the_worker() {
        let dir = folder();
        let path = png(dir.path(), "noise.png", (300, 200), noise);
        let spec = spec(Target::LongEdge(150), OutputFormat::WebP);
        // Without workers WebP cannot be made.
        let (_, outcome) = produce(&path, 0, &spec, &maker());
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
        let pool = WorkerPool::start(worker_exe(), 1, DecodeLimits::default().max_pixels);
        let maker = Maker {
            limits: DecodeLimits::default(),
            pool: Some(Arc::new(pool)),
        };
        let (size, bytes) = ready(produce(&path, 0, &spec, &maker).1);
        assert_eq!(size, (150, 100));
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }

    #[test]
    fn cache_keeps_the_newest_within_its_cap() {
        let (a, b, c) = (Path::new("a"), Path::new("b"), Path::new("c"));
        let mut cache = ByteCache::new(10);
        cache.put(a, 1, vec![1; 4]);
        cache.put(b, 1, vec![2; 4]);
        // Replacing a photo's bytes frees the old ones.
        cache.put(a, 1, vec![3; 4]);
        // Over the cap: the oldest (b) goes.
        cache.put(c, 1, vec![4; 4]);
        assert_eq!(cache.take(b, 1), None);
        assert_eq!(cache.take(a, 1), Some(vec![3; 4]));
        assert_eq!(cache.take(a, 1), None, "taken out");
        // Another generation's bytes are not handed out.
        assert_eq!(cache.take(c, 2), None);
        assert_eq!(cache.take(c, 1), Some(vec![4; 4]));
        // Larger than the cap on its own: not kept.
        cache.put(a, 1, vec![0; 11]);
        assert_eq!(cache.take(a, 1), None);
    }

    #[test]
    fn estimator_runs_the_newest_jobs_in_order() {
        let dir = folder();
        let paths: Vec<PathBuf> = (0..3)
            .map(|i| png(dir.path(), &format!("{i}.png"), (8, 8), noise))
            .collect();
        let estimator = Estimator::start(maker(), 1, HwndCell::default(), 0);
        let spec = spec(Target::LongEdge(4), OutputFormat::Png);
        let job = |generation, index: usize| Job {
            generation,
            index,
            path: paths[index].clone(),
            quarters: 0,
            spec,
        };
        estimator.submit(vec![job(1, 0), job(1, 1), job(1, 2)]);
        estimator.reorder(&[2, 0, 1]);
        let got: Vec<usize> = (0..3)
            .map(|_| {
                let done = estimator.rx.recv_timeout(Duration::from_secs(30)).unwrap();
                assert!(matches!(done.outcome, Outcome::Ready { .. }));
                done.job.index
            })
            .collect();
        // The first job may have started before the reorder.
        assert!(got == [2, 0, 1] || got == [0, 2, 1], "{got:?}");
        // A new generation replaces what is queued; cancel drops it all.
        estimator.submit(vec![job(2, 0)]);
        let done = estimator.rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(done.job.generation, 2);
        estimator.cancel();
        assert!(estimator
            .rx
            .recv_timeout(Duration::from_millis(300))
            .is_err());
    }
}
