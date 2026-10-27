//! Saving a view rotation into the photo's own file, losslessly and atomically (spec
//! §5.6, ruling R16).

use std::ffi::OsString;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::{FileTimesExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::SystemTime;

use lv_core::decode::{decode_bytes, max_file_bytes, DecodeError, DecodeLimits};
use lv_core::exif_edit::{self, read_orientation};
use lv_core::format::{sniff_format, ImageFormat, SNIFF_LEN};
use lv_core::heif_edit;
use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::jpeg_check::{inspect, is_mcu_aligned};
use lv_core::jpeg_extent::check_single_image;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::{
    ReplaceFileW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_WRITE_ATTRIBUTES, REPLACEFILE_IGNORE_MERGE_ERRORS,
};

use crate::worker::{PoolError, Reply, WorkerPool, NOT_RESPONDING};

/// How a save ended. Only `Saved` and `SavedAsOrientationTag` changed the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    SavedAsOrientationTag,
    Unsupported,
    Changed,
    ReadOnly,
    Failed(String),
}

/// What the save needs from the sandboxed workers: the lossless JPEG rotation and, to
/// check a HEIF result, its decoded size (ruling R6).
pub trait RotateService {
    fn rotate_jpeg(&self, data: Vec<u8>, quarters: u8) -> Result<Vec<u8>, PoolError>;
    fn heif_dims(&self, data: Vec<u8>) -> Result<(u32, u32), DecodeError>;
}

impl RotateService for WorkerPool {
    fn rotate_jpeg(&self, data: Vec<u8>, quarters: u8) -> Result<Vec<u8>, PoolError> {
        match WorkerPool::rotate_jpeg(self, quarters, data)? {
            Reply::Bytes { data } => Ok(data),
            // The pool only lets a `Bytes` answer through for a rotation.
            Reply::Image { .. } => Err(DecodeError::Corrupt(NOT_RESPONDING.into()).into()),
        }
    }

    fn heif_dims(&self, data: Vec<u8>) -> Result<(u32, u32), DecodeError> {
        self.decode_heif(data)
            .map(|(width, height, _)| (width, height))
    }
}

/// Stands in for the pool when `lv-worker.exe` is missing: JPEG pixel rotations and
/// HEIF checks fail, so only what needs no worker can be saved.
pub struct NoWorker;

impl RotateService for NoWorker {
    fn rotate_jpeg(&self, _data: Vec<u8>, _quarters: u8) -> Result<Vec<u8>, PoolError> {
        Err(DecodeError::Unsupported.into())
    }

    fn heif_dims(&self, _data: Vec<u8>) -> Result<(u32, u32), DecodeError> {
        Err(DecodeError::Unsupported)
    }
}

/// Saves `user_quarters` clockwise quarter turns into the file at `path`, which must
/// still be the file `id` was captured from. Runs for a while (a worker rotation, a full
/// decode to check the result): call it off the UI thread.
///
/// - JPEG: the stored orientation and the user's turns are split into pixel turns and a
///   new orientation ([`exif_edit::compose`]). The worker turns the pixels losslessly
///   when the file is one image (nothing after EOI, no MPF) whose size allows a perfect
///   turn; otherwise only the EXIF orientation changes (`SavedAsOrientationTag`) and
///   every other byte stays.
/// - HEIF: the primary item's `irot` changes and EXIF orientations become 1; files
///   `heif_edit` refuses are `Unsupported`.
/// - Anything else is `Unsupported`.
///
/// The result is written atomically ([`replace`]). Whatever fails, the original keeps
/// its bytes and no temporary file stays behind.
pub fn save_rotation(
    path: &Path,
    id: &FileIdentity,
    user_quarters: u8,
    worker: &dyn RotateService,
) -> SaveOutcome {
    match try_save(path, id, user_quarters % 4, worker) {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

fn try_save(
    path: &Path,
    id: &FileIdentity,
    quarters: u8,
    worker: &dyn RotateService,
) -> Result<SaveOutcome, SaveOutcome> {
    // `Missing` may also be a transient error (a sharing violation): never write then.
    if identity::verify(path, id) != Verify::Same {
        return Err(SaveOutcome::Changed);
    }
    let target = to_extended_path(path);
    let data = read_capped(&target)?;
    let format = sniff_format(&data[..data.len().min(SNIFF_LEN)]);
    if !matches!(format, Some(ImageFormat::Jpeg | ImageFormat::Heif)) {
        return Err(SaveOutcome::Unsupported);
    }
    if fs::metadata(&target)
        .map_err(failed)?
        .permissions()
        .readonly()
    {
        return Err(SaveOutcome::ReadOnly);
    }
    if quarters == 0 {
        return Ok(SaveOutcome::Saved);
    }
    let edit = match format {
        Some(ImageFormat::Jpeg) => edit_jpeg(&data, quarters, worker)?,
        _ => edit_heif(&data, quarters, worker)?,
    };
    replace(path, &target, id, &edit, worker)?;
    Ok(edit.outcome)
}

fn failed(e: impl std::fmt::Display) -> SaveOutcome {
    SaveOutcome::Failed(e.to_string())
}

/// The whole file, if it is within the decoder's file size limit.
fn read_capped(path: &Path) -> Result<Vec<u8>, SaveOutcome> {
    let cap = max_file_bytes(&DecodeLimits::default());
    let file = File::open(path).map_err(failed)?;
    let mut data = Vec::new();
    file.take(cap + 1).read_to_end(&mut data).map_err(failed)?;
    if data.len() as u64 > cap {
        return Err(failed("the file is too large to save"));
    }
    Ok(data)
}

/// The new file and how to check it once it is back from disk.
struct Edit {
    bytes: Vec<u8>,
    outcome: SaveOutcome,
    check: Check,
}

enum Check {
    Jpeg {
        /// Stored (not oriented) size.
        width: u16,
        height: u16,
        orientation: u16,
        /// Must be one image and nothing else (after a pixel rotation).
        single: bool,
    },
    Heif {
        /// Decoded (rotated) size.
        width: u32,
        height: u32,
        angle: u8,
    },
}

/// The JPEG decision ladder (ruling R16).
fn edit_jpeg(data: &[u8], quarters: u8, worker: &dyn RotateService) -> Result<Edit, SaveOutcome> {
    let info = inspect(data).map_err(failed)?;
    let orientation = read_orientation(data);
    let (new_orientation, pixel_turns) = exif_edit::compose(orientation, quarters);
    let tag_only = |outcome| -> Result<Edit, SaveOutcome> {
        let tag = exif_edit::compose_tag_only(orientation, quarters);
        Ok(Edit {
            bytes: exif_edit::set_orientation(data, tag).map_err(failed)?,
            outcome,
            check: Check::Jpeg {
                width: info.width,
                height: info.height,
                orientation: tag,
                single: false,
            },
        })
    };
    // The turn cancels the stored rotation: the pixels already stand right.
    if pixel_turns == 0 {
        return tag_only(SaveOutcome::Saved);
    }
    // A rotation keeps only the first image (ruling R7), and an imperfect one would
    // drop edge pixels.
    if check_single_image(data).is_err() || !is_mcu_aligned(&info, pixel_turns) {
        return tag_only(SaveOutcome::SavedAsOrientationTag);
    }
    let rotated = match worker.rotate_jpeg(data.to_vec(), pixel_turns) {
        Ok(rotated) => rotated,
        // Lossless refusals: the tag keeps every byte. A hung or missing worker is not
        // a verdict on the file.
        Err(PoolError::NotPerfect | PoolError::HasTrailingData) => {
            return tag_only(SaveOutcome::SavedAsOrientationTag)
        }
        Err(PoolError::Failed(DecodeError::Corrupt(msg))) if msg != NOT_RESPONDING => {
            return tag_only(SaveOutcome::SavedAsOrientationTag)
        }
        Err(e) => return Err(failed(e)),
    };
    // The worker's output is checked like any untrusted file.
    let (width, height) = if pixel_turns % 2 == 1 {
        (info.height, info.width)
    } else {
        (info.width, info.height)
    };
    let got = inspect(&rotated).map_err(failed)?;
    if (got.width, got.height) != (width, height) {
        return Err(failed("the worker's JPEG has the wrong size"));
    }
    if check_single_image(&rotated).is_err() {
        return Err(failed("the worker's JPEG is not one image"));
    }
    let bytes = exif_edit::after_pixel_rotation(
        &rotated,
        u32::from(width),
        u32::from(height),
        new_orientation,
    )
    .map_err(failed)?;
    Ok(Edit {
        bytes,
        outcome: SaveOutcome::Saved,
        check: Check::Jpeg {
            width,
            height,
            orientation: new_orientation,
            single: true,
        },
    })
}

/// Sets the primary item's `irot`. Files `heif_edit` refuses, including those whose
/// EXIF orientation cannot be reset (another viewer would turn them twice), are
/// `Unsupported`.
fn edit_heif(data: &[u8], quarters: u8, worker: &dyn RotateService) -> Result<Edit, SaveOutcome> {
    let unsupported = |_| SaveOutcome::Unsupported;
    let stored = heif_edit::read_rotation(data).map_err(unsupported)?;
    let angle = heif_edit::compose(stored, quarters);
    let rotated = heif_edit::set_rotation(data, angle).map_err(unsupported)?;
    let bytes = heif_edit::normalize_exif_orientation(&rotated).map_err(unsupported)?;
    let (w, h) = worker.heif_dims(data.to_vec()).map_err(failed)?;
    let (width, height) = if quarters % 2 == 1 { (h, w) } else { (w, h) };
    Ok(Edit {
        bytes,
        outcome: SaveOutcome::Saved,
        check: Check::Heif {
            width,
            height,
            angle,
        },
    })
}

/// Checks a new file as read back from disk: its structure and size, its orientation,
/// and that it decodes (JPEG in this process, HEIF in a worker) to the expected size.
fn check(check: &Check, bytes: &[u8], worker: &dyn RotateService) -> Result<(), String> {
    match *check {
        Check::Jpeg {
            width,
            height,
            orientation,
            single,
        } => {
            let info = inspect(bytes)?;
            if (info.width, info.height) != (width, height) {
                return Err("the new JPEG has the wrong size".into());
            }
            if single && check_single_image(bytes).is_err() {
                return Err("the new JPEG is not one image".into());
            }
            if read_orientation(bytes) != orientation {
                return Err("the new JPEG has the wrong orientation".into());
            }
            let shown = decode_bytes(bytes, &DecodeLimits::default()).map_err(|e| e.to_string())?;
            let (w, h) = (u32::from(width), u32::from(height));
            let expected = if orientation >= 5 { (h, w) } else { (w, h) };
            if (shown.width, shown.height) != expected {
                return Err("the new JPEG decodes to the wrong size".into());
            }
        }
        Check::Heif {
            width,
            height,
            angle,
        } => {
            if heif_edit::read_rotation(bytes)? != angle {
                return Err("the new HEIF has the wrong rotation".into());
            }
            if worker
                .heif_dims(bytes.to_vec())
                .map_err(|e| e.to_string())?
                != (width, height)
            {
                return Err("the new HEIF decodes to the wrong size".into());
            }
        }
    }
    Ok(())
}

/// Replaces the file at `target` (the extended form of `path`) with `edit.bytes`:
/// identity check → hidden temp file `.<name>.lvtmp-<pid>` in the same folder → flush →
/// read back and [`check`] → identity check → `ReplaceFileW` (the result keeps the
/// original's attributes and security) → creation and modification times restored. On
/// failure the original is untouched and the temp file is removed.
fn replace(
    path: &Path,
    target: &Path,
    id: &FileIdentity,
    edit: &Edit,
    worker: &dyn RotateService,
) -> Result<(), SaveOutcome> {
    let name = target.file_name().ok_or_else(|| failed("no file name"))?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".lvtmp-{}", std::process::id()));
    let temp = target.with_file_name(temp_name);

    if identity::verify(path, id) != Verify::Same {
        return Err(SaveOutcome::Changed);
    }
    let meta = fs::metadata(target).map_err(failed)?;
    let times = (meta.created().ok(), meta.modified().ok());
    // `create_new`: whatever already has the temp name is not ours and stays.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(FILE_ATTRIBUTE_HIDDEN.0)
        .open(&temp)
        .map_err(failed)?;
    let written = (|| {
        file.write_all(&edit.bytes).map_err(failed)?;
        // FlushFileBuffers.
        file.sync_all().map_err(failed)?;
        drop(file);
        let back = fs::read(&temp).map_err(failed)?;
        if back != edit.bytes {
            return Err(failed("the temporary file reads back differently"));
        }
        check(&edit.check, &back, worker).map_err(SaveOutcome::Failed)?;
        if identity::verify(path, id) != Verify::Same {
            return Err(SaveOutcome::Changed);
        }
        Ok(())
    })();
    if let Err(outcome) = written {
        remove_temp(&temp);
        return Err(outcome);
    }
    replace_file(target, &temp)?;
    restore_times(target, times);
    Ok(())
}

/// `ReplaceFileW` without a backup. Should it fail with the original already gone, the
/// temp file holds the only copy, so it is renamed into place instead of removed.
fn replace_file(target: &Path, temp: &Path) -> Result<(), SaveOutcome> {
    // SAFETY: both strings are NUL-terminated and live for the whole call; there is no
    // backup name and the reserved pointers are null.
    let replaced = unsafe {
        ReplaceFileW(
            &HSTRING::from(target.as_os_str()),
            &HSTRING::from(temp.as_os_str()),
            PCWSTR::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            None,
            None,
        )
    };
    let Err(e) = replaced else {
        return Ok(());
    };
    if fs::symlink_metadata(target).is_ok() {
        remove_temp(temp);
        return Err(failed(format!("ReplaceFileW: {e}")));
    }
    fs::rename(temp, target).map_err(|r| {
        eprintln!(
            "light-view: ReplaceFileW failed ({e}); the new file stays at {}: {r}",
            temp.display()
        );
        failed(format!("ReplaceFileW: {e}"))
    })?;
    // The temp file was hidden; the photo must not be.
    if let Ok(meta) = fs::metadata(target) {
        let attrs = FILE_FLAGS_AND_ATTRIBUTES(meta.file_attributes() & !FILE_ATTRIBUTE_HIDDEN.0);
        // SAFETY: the string is NUL-terminated and lives for the whole call.
        if let Err(e) = unsafe { SetFileAttributesW(&HSTRING::from(target.as_os_str()), attrs) } {
            eprintln!("light-view: {} stays hidden: {e}", target.display());
        }
    }
    Ok(())
}

fn remove_temp(temp: &Path) {
    if let Err(e) = fs::remove_file(temp) {
        eprintln!("light-view: could not remove {}: {e}", temp.display());
    }
}

/// Puts back the original's creation and modification times. The new content is saved
/// either way, so a failure is only logged.
fn restore_times(target: &Path, (created, modified): (Option<SystemTime>, Option<SystemTime>)) {
    let mut times = FileTimes::new();
    if let Some(t) = created {
        times = times.set_created(t);
    }
    if let Some(t) = modified {
        times = times.set_modified(t);
    }
    let restored = OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES.0)
        .open(target)
        .and_then(|f| f.set_times(times));
    if let Err(e) = restored {
        eprintln!(
            "light-view: could not restore the times of {}: {e}",
            target.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::io::Cursor;
    use std::os::windows::fs::{FileTimesExt, MetadataExt};
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, ImageFormat as ImgFormat, Rgb, RgbImage};
    use lv_core::decode::{decode_bytes, DecodeLimits, DecodedImage};
    use lv_core::exif_edit::{compose_tag_only, read_orientation, set_orientation};
    use lv_core::heif_edit::read_rotation;
    use lv_core::identity::{capture, to_extended_path};
    use lv_core::jpeg_check::inspect;

    use crate::worker::tests::worker_exe;
    use crate::worker::{WorkerPool, NOT_RESPONDING};

    /// Quadrant colours: top left, top right, bottom left, bottom right.
    const COLORS: [[u8; 3]; 4] = [[220, 30, 30], [30, 200, 30], [30, 30, 220], [235, 235, 235]];
    const FILE_ATTRIBUTE_HIDDEN: u32 = 2;

    /// A `w`×`h` JPEG (4:2:0, 16×16 MCUs) of four flat quadrants with EXIF `orientation`.
    fn jpeg(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| {
            Rgb(COLORS[usize::from(x >= w / 2) + 2 * usize::from(y >= h / 2)])
        });
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 95)
            .encode_image(&img)
            .unwrap();
        if orientation == 1 {
            out
        } else {
            set_orientation(&out, orientation).unwrap()
        }
    }

    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(RgbImage::new(4, 2))
            .write_to(&mut Cursor::new(&mut out), ImgFormat::Png)
            .unwrap();
        out
    }

    fn heic_sample() -> Vec<u8> {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/heif/rainbow-451x461.heic");
        fs::read(path).unwrap()
    }

    fn shown(bytes: &[u8]) -> DecodedImage {
        decode_bytes(bytes, &DecodeLimits::default()).expect("decodes")
    }

    /// The quadrant colours of a decoded image as shown (orientation applied).
    fn quadrants(img: &DecodedImage) -> [[u8; 3]; 4] {
        let (w, h) = (img.width, img.height);
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            let b = &img.frames[0].bgra[i..i + 3];
            [b[2], b[1], b[0]]
        };
        [
            at(w / 4, h / 4),
            at(3 * w / 4, h / 4),
            at(w / 4, 3 * h / 4),
            at(3 * w / 4, 3 * h / 4),
        ]
    }

    /// Quadrants after `q` clockwise quarter turns.
    fn turned(mut q4: [[u8; 3]; 4], q: u8) -> [[u8; 3]; 4] {
        for _ in 0..q {
            let [tl, tr, bl, br] = q4;
            q4 = [bl, tl, br, tr];
        }
        q4
    }

    fn assert_close(got: [[u8; 3]; 4], want: [[u8; 3]; 4], what: &str) {
        for (g, w) in got.iter().zip(want.iter()) {
            for (a, b) in g.iter().zip(w.iter()) {
                assert!(a.abs_diff(*b) < 40, "{what}: {got:?} vs {want:?}");
            }
        }
    }

    /// Asserts that `new` shows what `old` showed, turned `q` times clockwise.
    fn assert_shows_turned(old: &[u8], new: &[u8], q: u8, what: &str) {
        let (a, b) = (shown(old), shown(new));
        let (w, h) = if q % 2 == 1 {
            (a.height, a.width)
        } else {
            (a.width, a.height)
        };
        assert_eq!((b.width, b.height), (w, h), "{what}: shown size");
        assert_close(quadrants(&b), turned(quadrants(&a), q), what);
    }

    fn common_suffix(a: &[u8], b: &[u8]) -> usize {
        a.iter()
            .rev()
            .zip(b.iter().rev())
            .take_while(|(x, y)| x == y)
            .count()
    }

    enum Behavior {
        /// Turns the stored pixels (re-encoded; good enough for the pipeline's checks).
        Honest,
        Fail(fn() -> PoolError),
        Return(Vec<u8>),
        /// Writes `bytes` over the original while "rotating", then answers honestly.
        Touch(PathBuf, Vec<u8>),
        /// The service must not be asked.
        Never,
    }

    struct Fake {
        behavior: Behavior,
        calls: Cell<usize>,
    }

    impl Fake {
        fn new(behavior: Behavior) -> Fake {
            Fake {
                behavior,
                calls: Cell::new(0),
            }
        }
    }

    fn turn_pixels(data: &[u8], quarters: u8) -> Vec<u8> {
        let img = image::load_from_memory_with_format(data, ImgFormat::Jpeg)
            .unwrap()
            .to_rgb8();
        let img = match quarters {
            1 => image::imageops::rotate90(&img),
            2 => image::imageops::rotate180(&img),
            3 => image::imageops::rotate270(&img),
            q => panic!("{q} quarters"),
        };
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 95)
            .encode_image(&img)
            .unwrap();
        out
    }

    impl RotateService for Fake {
        fn rotate_jpeg(&self, data: Vec<u8>, quarters: u8) -> Result<Vec<u8>, PoolError> {
            self.calls.set(self.calls.get() + 1);
            match &self.behavior {
                Behavior::Honest => Ok(turn_pixels(&data, quarters)),
                Behavior::Fail(e) => Err(e()),
                Behavior::Return(bytes) => Ok(bytes.clone()),
                Behavior::Touch(path, bytes) => {
                    fs::write(path, bytes).unwrap();
                    Ok(turn_pixels(&data, quarters))
                }
                Behavior::Never => panic!("the worker must not be asked"),
            }
        }

        fn heif_dims(&self, _data: Vec<u8>) -> Result<(u32, u32), DecodeError> {
            panic!("not a HEIF test");
        }
    }

    /// The real pool; on its `touch_on`-th HEIF decode it first overwrites `path`.
    struct HeifTouch {
        pool: WorkerPool,
        calls: Cell<usize>,
        touch: RefCell<Option<(usize, PathBuf)>>,
    }

    impl RotateService for HeifTouch {
        fn rotate_jpeg(&self, data: Vec<u8>, quarters: u8) -> Result<Vec<u8>, PoolError> {
            RotateService::rotate_jpeg(&self.pool, data, quarters)
        }

        fn heif_dims(&self, data: Vec<u8>) -> Result<(u32, u32), DecodeError> {
            self.calls.set(self.calls.get() + 1);
            if let Some((n, path)) = &*self.touch.borrow() {
                if *n == self.calls.get() {
                    fs::write(path, b"changed meanwhile").unwrap();
                }
            }
            self.pool.heif_dims(data)
        }
    }

    fn pool() -> WorkerPool {
        WorkerPool::start(worker_exe(), 1, 100_000_000)
    }

    struct Fixture {
        dir: tempfile::TempDir,
        path: PathBuf,
        id: FileIdentity,
    }

    fn fixture(name: &str, bytes: &[u8]) -> Fixture {
        let dir = tempfile::Builder::new()
            .prefix("lv-m3-save")
            .tempdir()
            .unwrap();
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        let id = capture(&path).unwrap();
        Fixture { dir, path, id }
    }

    impl Fixture {
        fn save(&self, q: u8, worker: &dyn RotateService) -> SaveOutcome {
            save_rotation(&self.path, &self.id, q, worker)
        }

        fn bytes(&self) -> Vec<u8> {
            fs::read(&self.path).unwrap()
        }

        /// No temporary file is left in the folder.
        fn assert_no_temp(&self) {
            for entry in fs::read_dir(self.dir.path()).unwrap() {
                let name = entry.unwrap().file_name();
                assert!(
                    !name.to_string_lossy().contains(".lvtmp-"),
                    "left behind: {name:?}"
                );
            }
        }
    }

    #[test]
    fn saves_aligned_jpeg_pixels_and_resets_orientation() {
        for o in [1, 3, 6, 8] {
            for q in 1..=3 {
                let what = format!("orientation {o}, {q} turns");
                let old = jpeg(64, 32, o);
                let f = fixture("a.jpg", &old);
                let fake = Fake::new(Behavior::Honest);
                assert_eq!(f.save(q, &fake), SaveOutcome::Saved, "{what}");
                let new = f.bytes();
                assert_eq!(read_orientation(&new), 1, "{what}");
                assert_shows_turned(&old, &new, q, &what);
                // The pixels were turned unless the turn cancelled the stored one.
                let turns = fake.calls.get();
                let info = inspect(&new).unwrap();
                if turns == 1 {
                    assert_ne!(new, old, "{what}");
                } else {
                    assert_eq!((info.width, info.height), (64, 32), "{what}");
                }
                let attrs = fs::metadata(&f.path).unwrap().file_attributes();
                assert_eq!(attrs & FILE_ATTRIBUTE_HIDDEN, 0, "{what}");
                f.assert_no_temp();
            }
        }
    }

    #[test]
    fn mirrored_jpeg_saves_correct_orientation() {
        for o in [2, 4, 5, 7] {
            for q in 1..=3 {
                let what = format!("orientation {o}, {q} turns");
                let old = jpeg(64, 32, o);
                let f = fixture("m.jpg", &old);
                assert_eq!(
                    f.save(q, &Fake::new(Behavior::Honest)),
                    SaveOutcome::Saved,
                    "{what}"
                );
                let new = f.bytes();
                assert_eq!(read_orientation(&new), 2, "{what}: only the mirror stays");
                assert_shows_turned(&old, &new, q, &what);
            }
        }
    }

    #[test]
    fn unaligned_jpeg_saved_as_orientation_tag() {
        for (o, q) in [(1, 1), (1, 2), (6, 1), (5, 3)] {
            let what = format!("orientation {o}, {q} turns");
            let old = jpeg(66, 34, o);
            let f = fixture("u.jpg", &old);
            assert_eq!(
                f.save(q, &Fake::new(Behavior::Never)),
                SaveOutcome::SavedAsOrientationTag,
                "{what}"
            );
            let new = f.bytes();
            assert_eq!(read_orientation(&new), compose_tag_only(o, q), "{what}");
            // Only the EXIF segment changed: everything after it is byte-identical.
            assert!(common_suffix(&old, &new) >= old.len() - 100, "{what}");
            assert_shows_turned(&old, &new, q, &what);
            f.assert_no_temp();
        }
    }

    #[test]
    fn jpeg_with_trailing_data_uses_tag_path_and_keeps_trailer_bytes() {
        let trailer = b"\0\0MOTION-PHOTO-VIDEO-TRAILER\xFF\xD9 more".to_vec();
        let mut old = jpeg(64, 32, 1);
        old.extend_from_slice(&trailer);
        let f = fixture("t.jpg", &old);
        assert_eq!(
            f.save(1, &Fake::new(Behavior::Never)),
            SaveOutcome::SavedAsOrientationTag
        );
        let new = f.bytes();
        assert!(new.ends_with(&trailer));
        assert!(common_suffix(&old, &new) >= old.len() - 100);
        assert_eq!(read_orientation(&new), 6);
    }

    #[test]
    fn worker_refusals_fall_back_to_orientation_tag() {
        let refusals: [fn() -> PoolError; 3] = [
            || PoolError::NotPerfect,
            || PoolError::HasTrailingData,
            || PoolError::Failed(DecodeError::Corrupt("extraneous bytes".into())),
        ];
        for refusal in refusals {
            let old = jpeg(64, 32, 1);
            let f = fixture("r.jpg", &old);
            assert_eq!(
                f.save(1, &Fake::new(Behavior::Fail(refusal))),
                SaveOutcome::SavedAsOrientationTag
            );
            assert_eq!(read_orientation(&f.bytes()), 6);
        }
    }

    #[test]
    fn heif_rotation_saved_and_decodes() {
        let pool = pool();
        let f = fixture("h.heic", &heic_sample());
        assert_eq!(f.save(1, &pool), SaveOutcome::Saved);
        let new = f.bytes();
        // One clockwise turn is three anti-clockwise ones.
        assert_eq!(read_rotation(&new).unwrap(), 3);
        let (w, h, _) = pool.decode_heif(new).unwrap();
        assert_eq!((w, h), (461, 451));
        f.assert_no_temp();
    }

    #[test]
    fn png_is_unsupported_and_untouched() {
        let f = fixture("p.png", &png());
        let before = fs::metadata(&f.path).unwrap().modified().unwrap();
        assert_eq!(
            f.save(1, &Fake::new(Behavior::Never)),
            SaveOutcome::Unsupported
        );
        assert_eq!(f.bytes(), png());
        assert_eq!(fs::metadata(&f.path).unwrap().modified().unwrap(), before);
        f.assert_no_temp();
    }

    #[test]
    fn save_refuses_when_identity_changed() {
        let f = fixture("c.jpg", &jpeg(64, 32, 1));
        let other = jpeg(64, 32, 3);
        fs::write(&f.path, &other).unwrap();
        assert_eq!(f.save(1, &Fake::new(Behavior::Never)), SaveOutcome::Changed);
        assert_eq!(f.bytes(), other);
        // A deleted file is never recreated.
        fs::remove_file(&f.path).unwrap();
        assert_eq!(f.save(1, &Fake::new(Behavior::Never)), SaveOutcome::Changed);
        assert!(!f.path.exists());
        f.assert_no_temp();
    }

    #[test]
    fn failed_save_leaves_original_and_no_temp() {
        let old = jpeg(64, 32, 1);
        let failures: [fn() -> PoolError; 3] = [
            || PoolError::Failed(DecodeError::Unsupported),
            || PoolError::Failed(DecodeError::Corrupt(NOT_RESPONDING.into())),
            || PoolError::Failed(DecodeError::FileTooLarge { bytes: 1 }),
        ];
        // The worker fails (no worker, a hung one, a refusal that is not a fallback).
        for failure in failures {
            let f = fixture("f.jpg", &old);
            assert!(matches!(
                f.save(1, &Fake::new(Behavior::Fail(failure))),
                SaveOutcome::Failed(_)
            ));
            assert_eq!(f.bytes(), old);
            f.assert_no_temp();
        }

        // The worker's output fails the checks.
        let f = fixture("f.jpg", &old);
        let wrong = Fake::new(Behavior::Return(jpeg(64, 32, 1)));
        assert!(matches!(f.save(1, &wrong), SaveOutcome::Failed(_)));
        assert_eq!(f.bytes(), old);
        f.assert_no_temp();

        // A read-only original is not written.
        let f = fixture("ro.jpg", &old);
        let mut perms = fs::metadata(&f.path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&f.path, perms.clone()).unwrap();
        let f = Fixture {
            id: capture(&f.path).unwrap(),
            ..f
        };
        assert_eq!(
            f.save(1, &Fake::new(Behavior::Never)),
            SaveOutcome::ReadOnly
        );
        assert_eq!(f.bytes(), old);
        f.assert_no_temp();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&f.path, perms).unwrap();

        // The original changes while the worker turns it (before the temp file).
        let f = fixture("f.jpg", &old);
        let changed = jpeg(64, 32, 3);
        let touch = Fake::new(Behavior::Touch(f.path.clone(), changed.clone()));
        assert_eq!(f.save(1, &touch), SaveOutcome::Changed);
        assert_eq!(f.bytes(), changed);
        f.assert_no_temp();

        // The original changes while the written temp file is checked (before the
        // replace): the HEIF check decodes the original, then the temp file.
        let f = fixture("f.heic", &heic_sample());
        let touch = HeifTouch {
            pool: pool(),
            calls: Cell::new(0),
            touch: RefCell::new(Some((2, f.path.clone()))),
        };
        assert_eq!(f.save(1, &touch), SaveOutcome::Changed);
        assert_eq!(f.bytes(), b"changed meanwhile");
        f.assert_no_temp();

        // The temporary file cannot be created (a folder holds its name); what is there
        // is not ours and stays.
        let f = fixture("f.jpg", &old);
        let blocker = f
            .dir
            .path()
            .join(format!(".f.jpg.lvtmp-{}", std::process::id()));
        fs::create_dir(&blocker).unwrap();
        assert!(matches!(
            f.save(1, &Fake::new(Behavior::Honest)),
            SaveOutcome::Failed(_)
        ));
        assert_eq!(f.bytes(), old);
        assert!(blocker.is_dir());
    }

    #[test]
    fn bad_worker_output_is_rejected() {
        let old = jpeg(64, 32, 1);
        let rotated = turn_pixels(&old, 1);
        let mut with_trailer = rotated.clone();
        with_trailer.extend_from_slice(b"junk");
        let outputs = [
            // Not turned: the size did not change.
            old.clone(),
            // Not a JPEG.
            b"\xFF\xD8\xFF garbage".to_vec(),
            // Truncated.
            rotated[..rotated.len() / 2].to_vec(),
            // Data after EOI.
            with_trailer,
            Vec::new(),
        ];
        for (i, output) in outputs.into_iter().enumerate() {
            let f = fixture("b.jpg", &old);
            assert!(
                matches!(
                    f.save(1, &Fake::new(Behavior::Return(output))),
                    SaveOutcome::Failed(_)
                ),
                "output {i}"
            );
            assert_eq!(f.bytes(), old, "output {i}");
            f.assert_no_temp();
        }
    }

    #[test]
    fn timestamps_restored() {
        let f = fixture("d.jpg", &jpeg(64, 32, 1));
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_300_000_000);
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_400_000_123);
        let file = fs::File::options().write(true).open(&f.path).unwrap();
        file.set_times(
            fs::FileTimes::new()
                .set_modified(modified)
                .set_created(created),
        )
        .unwrap();
        drop(file);
        let f = Fixture {
            id: capture(&f.path).unwrap(),
            ..f
        };
        assert_eq!(f.save(1, &Fake::new(Behavior::Honest)), SaveOutcome::Saved);
        let meta = fs::metadata(&f.path).unwrap();
        assert_eq!(meta.modified().unwrap(), modified);
        assert_eq!(meta.created().unwrap(), created);
    }

    #[test]
    fn long_path_save() {
        let f = fixture("unused.jpg", b"");
        let mut dir = f.dir.path().to_path_buf();
        while dir.as_os_str().len() < 300 {
            dir.push("d".repeat(40));
        }
        fs::create_dir_all(to_extended_path(&dir)).unwrap();
        let path = dir.join("long.jpg");
        let old = jpeg(64, 32, 1);
        fs::write(to_extended_path(&path), &old).unwrap();
        let id = capture(&path).unwrap();
        assert_eq!(
            save_rotation(&path, &id, 1, &Fake::new(Behavior::Honest)),
            SaveOutcome::Saved
        );
        let new = fs::read(to_extended_path(&path)).unwrap();
        assert_shows_turned(&old, &new, 1, "long path");
        fs::remove_dir_all(to_extended_path(f.dir.path())).unwrap();
    }
}
