//! Saving a view rotation into the photo's own file, losslessly and atomically (spec
//! §5.6, ruling R16).

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{FileTimesExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lv_core::decode::{decode_bytes, max_file_bytes, DecodeError, DecodeLimits};
use lv_core::exif_edit::{self, read_orientation};
use lv_core::format::{sniff_format, ImageFormat, SNIFF_LEN};
use lv_core::heif_edit;
use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::jpeg_check::{inspect, is_mcu_aligned};
use lv_core::jpeg_extent::check_single_image;
use windows::core::{HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_UNABLE_TO_MOVE_REPLACEMENT,
    ERROR_UNABLE_TO_MOVE_REPLACEMENT_2,
};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, ReplaceFileW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_WRITE_ATTRIBUTES, MOVEFILE_WRITE_THROUGH,
    REPLACEFILE_IGNORE_MERGE_ERRORS,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::worker::{PoolError, Reply, WorkerPool};

/// How a save ended. Only `Saved` and `SavedAsOrientationTag` changed the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    SavedAsOrientationTag,
    Unsupported,
    Changed,
    ReadOnly,
    Failed(String),
    /// The original vanished during the replace and the new file could not take its
    /// place: it stays, unhidden, at this path (the only copy).
    KeptCopy(PathBuf),
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
            Reply::Image { .. } => Err(PoolError::WorkerUnavailable),
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
/// `limits` are the viewer's decode limits: the file size cap and the in-process check
/// decode stay within the memory the app budgets for one image.
///
/// The result is written atomically ([`replace`]). Whatever fails, the original keeps
/// its bytes and no temporary file stays behind.
pub fn save_rotation(
    path: &Path,
    id: &FileIdentity,
    user_quarters: u8,
    worker: &dyn RotateService,
    limits: &DecodeLimits,
) -> SaveOutcome {
    match try_save(path, id, user_quarters % 4, worker, limits) {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

fn try_save(
    path: &Path,
    id: &FileIdentity,
    quarters: u8,
    worker: &dyn RotateService,
    limits: &DecodeLimits,
) -> Result<SaveOutcome, SaveOutcome> {
    // `Missing` may also be a transient error (a sharing violation): never write then.
    if identity::verify(path, id) != Verify::Same {
        return Err(SaveOutcome::Changed);
    }
    let target = to_extended_path(path);
    let data = read_capped(&target, limits)?;
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
    replace(path, &target, id, &edit, worker, limits)?;
    Ok(edit.outcome)
}

fn failed(e: impl std::fmt::Display) -> SaveOutcome {
    SaveOutcome::Failed(e.to_string())
}

/// The whole file, if it is within the decoder's file size limit.
fn read_capped(path: &Path, limits: &DecodeLimits) -> Result<Vec<u8>, SaveOutcome> {
    let cap = max_file_bytes(limits);
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
        // Lossless refusals (the worker could not turn this file): the tag keeps every
        // byte. A hung or missing worker (`WorkerUnavailable`, `Unsupported`) is no
        // verdict on the file, so nothing is written (ruling R17).
        Err(
            PoolError::NotPerfect
            | PoolError::HasTrailingData
            | PoolError::Failed(DecodeError::Corrupt(_)),
        ) => return tag_only(SaveOutcome::SavedAsOrientationTag),
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
fn check(
    check: &Check,
    bytes: &[u8],
    worker: &dyn RotateService,
    limits: &DecodeLimits,
) -> Result<(), String> {
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
            let shown = decode_bytes(bytes, limits).map_err(|e| e.to_string())?;
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
/// stale temp files of this photo swept ([`sweep_stale_temps`]) → identity check → hidden
/// temp file `.<name>.lvtmp-<pid>` in the same folder → flush → read back and [`check`] →
/// identity check → `ReplaceFileW` (the result keeps the original's attributes and
/// security) → creation and modification times restored. On failure (a panic included)
/// the original is untouched and the temp file is removed, except in the one case where
/// it is the only copy left ([`after_replace_error`]).
fn replace(
    path: &Path,
    target: &Path,
    id: &FileIdentity,
    edit: &Edit,
    worker: &dyn RotateService,
    limits: &DecodeLimits,
) -> Result<(), SaveOutcome> {
    let name = target.file_name().ok_or_else(|| failed("no file name"))?;
    let own = std::process::id();
    let temp_path = target.with_file_name(temp_name(name, own));
    sweep_stale_temps(target, own);

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
        .open(&temp_path)
        .map_err(failed)?;
    // From here on the temp file is removed on every way out, unwinding included.
    let mut temp = TempFile {
        path: temp_path,
        keep: false,
    };
    file.write_all(&edit.bytes).map_err(failed)?;
    // FlushFileBuffers.
    file.sync_all().map_err(failed)?;
    drop(file);
    let back = fs::read(&temp.path).map_err(failed)?;
    if back != edit.bytes {
        return Err(failed("the temporary file reads back differently"));
    }
    check(&edit.check, &back, worker, limits).map_err(SaveOutcome::Failed)?;
    if identity::verify(path, id) != Verify::Same {
        return Err(SaveOutcome::Changed);
    }
    // SAFETY: both strings are NUL-terminated and live for the whole call; there is no
    // backup name and the reserved pointers are null.
    let replaced = unsafe {
        ReplaceFileW(
            &HSTRING::from(target.as_os_str()),
            &HSTRING::from(temp.path.as_os_str()),
            PCWSTR::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            None,
            None,
        )
    };
    match replaced {
        // The temp file is now the photo.
        Ok(()) => temp.keep = true,
        Err(e) => after_replace_error(e.code(), target, &mut temp)?,
    }
    restore_times(target, times);
    Ok(())
}

/// `.<name>.lvtmp-<pid>`.
fn temp_name(name: &OsStr, pid: u32) -> OsString {
    let mut temp = OsString::from(".");
    temp.push(name);
    temp.push(format!(".lvtmp-{pid}"));
    temp
}

/// A save's temp file: removed when dropped unless `keep` is set (it became the photo,
/// or it is the only copy left).
struct TempFile {
    path: PathBuf,
    keep: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.keep {
            remove_temp(&self.path);
        }
    }
}

/// Acts on a failed `ReplaceFileW` (`code`). Without a backup name, the error
/// ERROR_UNABLE_TO_MOVE_REPLACEMENT (or its `_2` form) means the original is already gone
/// and the temp file holds the only copy: it is moved into place (never over a file that
/// appeared meanwhile), or, if that fails too, kept, unhidden, as `KeptCopy`. Any other
/// failure removes the temp file: `Changed` if the photo is gone (deleted or moved
/// meanwhile; it is never recreated), else `Failed`.
fn after_replace_error(
    code: HRESULT,
    target: &Path,
    temp: &mut TempFile,
) -> Result<(), SaveOutcome> {
    let gone = fs::symlink_metadata(target).is_err();
    let moved_away = [
        ERROR_UNABLE_TO_MOVE_REPLACEMENT,
        ERROR_UNABLE_TO_MOVE_REPLACEMENT_2,
    ]
    .iter()
    .any(|e| HRESULT::from_win32(e.0) == code);
    if !(moved_away && gone) {
        return Err(if gone {
            SaveOutcome::Changed
        } else {
            failed(format!(
                "ReplaceFileW: {}",
                windows::core::Error::from(code)
            ))
        });
    }
    temp.keep = true;
    // SAFETY: both strings are NUL-terminated and live for the whole call. Without
    // MOVEFILE_REPLACE_EXISTING an existing file at `target` is never replaced.
    let moved = unsafe {
        MoveFileExW(
            &HSTRING::from(temp.path.as_os_str()),
            &HSTRING::from(target.as_os_str()),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    match moved {
        Ok(()) => {
            unhide(target);
            Ok(())
        }
        Err(e) => {
            unhide(&temp.path);
            eprintln!(
                "light-view: the original is gone and the new file could not take its place \
                 ({e}); the only copy stays at {}",
                temp.path.display()
            );
            Err(SaveOutcome::KeptCopy(temp.path.clone()))
        }
    }
}

/// Clears the hidden attribute the temp file was created with.
fn unhide(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let attrs = FILE_FLAGS_AND_ATTRIBUTES(meta.file_attributes() & !FILE_ATTRIBUTE_HIDDEN.0);
    // SAFETY: the string is NUL-terminated and lives for the whole call.
    if let Err(e) = unsafe { SetFileAttributesW(&HSTRING::from(path.as_os_str()), attrs) } {
        eprintln!("light-view: {} stays hidden: {e}", path.display());
    }
}

fn remove_temp(temp: &Path) {
    if let Err(e) = fs::remove_file(temp) {
        eprintln!("light-view: could not remove {}: {e}", temp.display());
    }
}

/// The pid in `file`, if it is exactly the temp name `.<photo>.lvtmp-<pid>`.
fn temp_pid(file: &OsStr, photo: &OsStr) -> Option<u32> {
    let prefix: Vec<u16> = temp_name(photo, 0).encode_wide().collect();
    let prefix = &prefix[..prefix.len() - 1]; // without the "0"
    let file: Vec<u16> = file.encode_wide().collect();
    let digits = file.strip_prefix(prefix)?;
    if digits.is_empty()
        || !digits
            .iter()
            .all(|&c| (u16::from(b'0')..=u16::from(b'9')).contains(&c))
    {
        return None;
    }
    String::from_utf16(digits).ok()?.parse().ok()
}

/// Whether a temp file named for process `pid` was left behind by a save that never
/// finished (a crash or power loss): it is still hidden (a kept copy is not), and it is
/// either this process's name (saves run one at a time, and a crashed earlier process may
/// have had the same pid) or no running process has that pid.
fn is_stale(pid: u32, own: u32, hidden: bool, running: impl Fn(u32) -> bool) -> bool {
    hidden && (pid == own || !running(pid))
}

/// Removes the stale temp files of the photo at `target`: only regular files named
/// exactly `.<name>.lvtmp-<pid>` in its folder ([`temp_pid`], [`is_stale`]).
fn sweep_stale_temps(target: &Path, own: u32) {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = temp_pid(&entry.file_name(), name) else {
            continue;
        };
        // `symlink_metadata`: a link or a folder is never a temp file of ours.
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let hidden = meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN.0 != 0;
        if meta.is_file() && is_stale(pid, own, hidden, process_running) {
            eprintln!("light-view: removing a stale {}", entry.path().display());
            remove_temp(&entry.path());
        }
    }
}

/// Whether a process with this pid exists; when that cannot be told (access denied),
/// it counts as running.
fn process_running(pid: u32) -> bool {
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: plain call; the handle is closed below.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            let mut code = 0u32;
            // SAFETY: `handle` is a live process handle with query access; `code` is a
            // writable local. The handle is closed once.
            unsafe {
                let read = GetExitCodeProcess(handle, &mut code);
                let _ = CloseHandle(handle);
                read.is_err() || code == STILL_ACTIVE
            }
        }
        Err(e) => e.code() != HRESULT::from_win32(ERROR_INVALID_PARAMETER.0),
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
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use crate::worker::WorkerPool;

    /// Quadrant colours: top left, top right, bottom left, bottom right.
    const COLORS: [[u8; 3]; 4] = [[220, 30, 30], [30, 200, 30], [30, 30, 220], [235, 235, 235]];
    const FILE_ATTRIBUTE_HIDDEN: u32 = 2;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;

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
            save_rotation(&self.path, &self.id, q, worker, &DecodeLimits::default())
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
            || PoolError::WorkerUnavailable,
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

        // The check decode is held to the viewer's limits (here 2048 px > 1000).
        let f = fixture("f.jpg", &old);
        let small = DecodeLimits { max_pixels: 1000 };
        let outcome = save_rotation(&f.path, &f.id, 1, &Fake::new(Behavior::Honest), &small);
        assert!(matches!(outcome, SaveOutcome::Failed(_)), "{outcome:?}");
        assert_eq!(f.bytes(), old);
        f.assert_no_temp();

        // ReplaceFileW fails: another program holds the original open without
        // FILE_SHARE_DELETE (a sharing violation).
        let f = fixture("f.jpg", &old);
        let holder = fs::File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&f.path)
            .unwrap();
        let outcome = f.save(1, &Fake::new(Behavior::Honest));
        assert!(matches!(outcome, SaveOutcome::Failed(_)), "{outcome:?}");
        drop(holder);
        assert_eq!(f.bytes(), old);
        f.assert_no_temp();
    }

    /// A HEIF service that answers the original's size, then panics checking the temp.
    struct PanicsInCheck {
        calls: Cell<usize>,
    }

    impl RotateService for PanicsInCheck {
        fn rotate_jpeg(&self, _data: Vec<u8>, _quarters: u8) -> Result<Vec<u8>, PoolError> {
            panic!("not a JPEG test");
        }

        fn heif_dims(&self, _data: Vec<u8>) -> Result<(u32, u32), DecodeError> {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() == 2 {
                panic!("the check panicked");
            }
            Ok((451, 461))
        }
    }

    #[test]
    fn a_panic_after_the_temp_file_removes_it() {
        let old = heic_sample();
        let f = fixture("p.heic", &old);
        let service = PanicsInCheck {
            calls: Cell::new(0),
        };
        let result = catch_unwind(AssertUnwindSafe(|| f.save(1, &service)));
        assert!(result.is_err());
        assert_eq!(service.calls.get(), 2);
        assert_eq!(f.bytes(), old);
        f.assert_no_temp();
    }

    /// A hidden temp file holding `bytes`, as the save makes it.
    fn hidden_file(path: &Path, bytes: &[u8]) {
        let mut file = fs::File::options()
            .write(true)
            .create_new(true)
            .attributes(FILE_ATTRIBUTE_HIDDEN)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    fn hidden(path: &Path) -> bool {
        fs::metadata(path).unwrap().file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
    }

    const UNABLE_TO_MOVE: u32 = 1176;
    const FILE_NOT_FOUND: u32 = 2;

    #[test]
    fn replace_error_with_the_original_gone_moves_the_copy_into_place() {
        let dir = tempfile::Builder::new()
            .prefix("lv-m3-save")
            .tempdir()
            .unwrap();
        let target = dir.path().join("x.jpg");
        let temp_path = dir.path().join(".x.jpg.lvtmp-1");
        hidden_file(&temp_path, b"new");
        let mut temp = TempFile {
            path: temp_path.clone(),
            keep: false,
        };
        let code = HRESULT::from_win32(UNABLE_TO_MOVE);
        assert_eq!(after_replace_error(code, &target, &mut temp), Ok(()));
        drop(temp);
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!hidden(&target));
        assert!(!temp_path.exists());
    }

    #[test]
    fn replace_error_after_the_photo_was_deleted_never_recreates_it() {
        let dir = tempfile::Builder::new()
            .prefix("lv-m3-save")
            .tempdir()
            .unwrap();
        let target = dir.path().join("x.jpg");
        let temp_path = dir.path().join(".x.jpg.lvtmp-1");
        hidden_file(&temp_path, b"new");
        let mut temp = TempFile {
            path: temp_path.clone(),
            keep: false,
        };
        let code = HRESULT::from_win32(FILE_NOT_FOUND);
        assert_eq!(
            after_replace_error(code, &target, &mut temp),
            Err(SaveOutcome::Changed)
        );
        drop(temp);
        assert!(!target.exists());
        assert!(!temp_path.exists());

        // With the photo still there, any error removes the temp file and fails.
        fs::write(&target, b"old").unwrap();
        hidden_file(&temp_path, b"new");
        let mut temp = TempFile {
            path: temp_path.clone(),
            keep: false,
        };
        let code = HRESULT::from_win32(UNABLE_TO_MOVE);
        let outcome = after_replace_error(code, &target, &mut temp);
        assert!(
            matches!(outcome, Err(SaveOutcome::Failed(_))),
            "{outcome:?}"
        );
        drop(temp);
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(!temp_path.exists());
    }

    #[test]
    fn replace_error_keeps_the_only_copy_when_it_cannot_move() {
        let dir = tempfile::Builder::new()
            .prefix("lv-m3-save")
            .tempdir()
            .unwrap();
        let target = dir.path().join("x.jpg");
        let temp_path = dir.path().join(".x.jpg.lvtmp-1");
        hidden_file(&temp_path, b"new");
        // Nobody may share the temp file: it cannot be moved.
        let lock = fs::File::options()
            .read(true)
            .share_mode(0)
            .open(&temp_path)
            .unwrap();
        let mut temp = TempFile {
            path: temp_path.clone(),
            keep: false,
        };
        let code = HRESULT::from_win32(UNABLE_TO_MOVE);
        assert_eq!(
            after_replace_error(code, &target, &mut temp),
            Err(SaveOutcome::KeptCopy(temp_path.clone()))
        );
        drop(temp);
        drop(lock);
        assert_eq!(fs::read(&temp_path).unwrap(), b"new");
        assert!(!target.exists());
    }

    #[test]
    fn temp_names_are_matched_exactly() {
        let photo = OsStr::new("a.jpg");
        let pid = |name: &str| temp_pid(OsStr::new(name), photo);
        assert_eq!(pid(".a.jpg.lvtmp-1234"), Some(1234));
        assert_eq!(pid(".a.jpg.lvtmp-0"), Some(0));
        for other in [
            "a.jpg",
            ".a.jpg.lvtmp-",
            ".a.jpg.lvtmp-12a",
            ".a.jpg.lvtmp-+12",
            ".a.jpg.lvtmp-12.jpg",
            ".a.jpg.lvtmp-99999999999",
            ".b.jpg.lvtmp-12",
            ".aa.jpg.lvtmp-12",
            "a.jpg.lvtmp-12",
            ".A.JPG.lvtmp-12",
            "..a.jpg.lvtmp-12",
        ] {
            assert_eq!(pid(other), None, "{other}");
        }
    }

    #[test]
    fn only_hidden_temps_of_dead_or_own_pids_are_stale() {
        let running = |pid: u32| pid == 8;
        assert!(is_stale(5, 7, true, running), "no such process");
        assert!(is_stale(7, 7, true, running), "this process: a reused pid");
        assert!(
            !is_stale(8, 7, true, running),
            "another live process may be saving"
        );
        assert!(!is_stale(5, 7, false, running), "a kept copy is not hidden");
        assert!(process_running(std::process::id()));
    }

    #[test]
    fn stale_temps_of_the_photo_are_swept_before_saving() {
        let old = jpeg(64, 32, 1);
        let f = fixture("s.jpg", &old);
        let dir = f.dir.path();
        let own = dir.join(format!(".s.jpg.lvtmp-{}", std::process::id()));
        hidden_file(&own, b"left by a crash");
        // Pid 4 is the System process: running, so its file stays.
        let live = dir.join(".s.jpg.lvtmp-4");
        hidden_file(&live, b"x");
        let kept = dir.join(".s.jpg.lvtmp-99999999");
        fs::write(&kept, b"a kept copy").unwrap();
        let other = dir.join(".t.jpg.lvtmp-99999999");
        hidden_file(&other, b"another photo's");
        assert_eq!(f.save(1, &Fake::new(Behavior::Honest)), SaveOutcome::Saved);
        assert!(!own.exists());
        assert!(live.exists() && kept.exists() && other.exists());
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
            save_rotation(
                &path,
                &id,
                1,
                &Fake::new(Behavior::Honest),
                &DecodeLimits::default()
            ),
            SaveOutcome::Saved
        );
        let new = fs::read(to_extended_path(&path)).unwrap();
        assert_shows_turned(&old, &new, 1, "long path");
        fs::remove_dir_all(to_extended_path(f.dir.path())).unwrap();
    }
}
