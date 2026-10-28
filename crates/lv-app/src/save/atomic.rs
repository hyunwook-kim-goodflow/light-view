//! The atomic replace of a photo with its edited bytes: the hidden temp file, the
//! identity checks, `ReplaceFileW`, what is kept when it fails, and the sweep of stale
//! temp files.

use std::ffi::{OsStr, OsString};
use std::fs::{self, FileTimes, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{FileTimesExt, MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use lv_core::decode::DecodeLimits;
use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::listing::same_path;
use windows::core::{HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FindClose, FindFirstFileW, FindNextFileW, GetFileInformationByHandle, MoveFileExW,
    ReplaceFileW, SetFileAttributesW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_ATTRIBUTES, MOVEFILE_WRITE_THROUGH, REPLACEFILE_IGNORE_MERGE_ERRORS,
    WIN32_FIND_DATAW,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_UPDATEITEM, SHCNF_PATHW};

use super::{check, failed, Edit, RotateService, SaveOutcome};

/// Replaces the file at `target` (the extended form of `path`) with `edit.bytes`:
/// stale temp files of this photo swept ([`sweep_stale_temps`]) → identity check → hidden
/// temp file `.<name>.lvtmp-<pid>` in the same folder → flush → read back and [`check`] →
/// identity check → `ReplaceFileW` (the result keeps the original's attributes and
/// security) → creation and modification times restored → Explorer told. On failure (a
/// panic included) the original is untouched and the temp file is removed, except when
/// it may be the only copy left ([`after_replace_error`]).
pub(super) fn replace(
    path: &Path,
    target: &Path,
    id: &FileIdentity,
    edit: &Edit,
    worker: &dyn RotateService,
    limits: &DecodeLimits,
) -> Result<(), SaveOutcome> {
    let name = target.file_name().ok_or_else(|| failed("no file name"))?;
    let own = std::process::id();
    let temp = temp_name(name, own);
    if temp.encode_wide().count() > MAX_NAME {
        return Err(failed(format!(
            "the temporary file name would be longer than {MAX_NAME} characters"
        )));
    }
    let temp_path = target.with_file_name(temp);
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
        Err(e) => after_replace_error(e.code(), path, id, target, &mut temp)?,
    }
    restore_times(target, times);
    notify_shell(path);
    Ok(())
}

/// Longest file name (UTF-16 units) NTFS takes.
pub(crate) const MAX_NAME: usize = 255;

/// Tells Explorer the photo changed, so it redraws the thumbnail even though the
/// modification time was put back.
pub(crate) fn notify_shell(path: &Path) {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and lives for the whole call; SHCNF_PATHW says
    // that is what the first item points to, and the second is unused.
    unsafe {
        SHChangeNotify(
            SHCNE_UPDATEITEM,
            SHCNF_PATHW,
            Some(wide.as_ptr().cast()),
            None,
        )
    };
}

/// `.<name>.lvtmp-<pid>`.
pub(crate) fn temp_name(name: &OsStr, pid: u32) -> OsString {
    let mut temp = OsString::from(".");
    temp.push(name);
    temp.push(format!(".lvtmp-{pid}"));
    temp
}

/// A save's temp file: removed when dropped unless `keep` is set (it became the photo,
/// or it may be the only copy left).
pub(crate) struct TempFile {
    pub(crate) path: PathBuf,
    pub(crate) keep: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.keep {
            remove_temp(&self.path);
        }
    }
}

/// Acts on a failed `ReplaceFileW` (`code`). The temp file is removed only when the
/// original provably is intact: `path` still verifies as `id`, so the photo keeps its
/// bytes and the save is `Failed`. Otherwise the temp file may be the only copy of the
/// photo (without a backup name, ERROR_UNABLE_TO_MOVE_REPLACEMENT means the original is
/// already gone, and a file now at `target` is someone else's):
/// - nothing is at `target`: the temp file is moved into place without replacing
///   anything, and the save counts as done; if that fails it is kept ([`keep_copy`]);
/// - something else is at `target` (or it cannot be told): it is kept ([`keep_copy`]).
pub(super) fn after_replace_error(
    code: HRESULT,
    path: &Path,
    id: &FileIdentity,
    target: &Path,
    temp: &mut TempFile,
) -> Result<(), SaveOutcome> {
    let error = windows::core::Error::from(code);
    if identity::verify(path, id) == Verify::Same {
        return Err(failed(format!("ReplaceFileW: {error}")));
    }
    // From here on the temp file is never deleted.
    temp.keep = true;
    eprintln!(
        "light-view: ReplaceFileW failed ({error}) and {} is no longer the photo that was saved",
        target.display()
    );
    let absent = matches!(
        fs::symlink_metadata(target),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    if !absent {
        return Err(keep_copy(target, &temp.path));
    }
    match move_no_replace(&temp.path, target) {
        Ok(()) => {
            unhide(target);
            Ok(())
        }
        Err(e) => {
            eprintln!("light-view: the new file could not take the photo's place ({e})");
            Err(keep_copy(target, &temp.path))
        }
    }
}

/// `MoveFileExW` without MOVEFILE_REPLACE_EXISTING: an existing file at `to` is never
/// replaced.
pub(crate) fn move_no_replace(from: &Path, to: &Path) -> windows::core::Result<()> {
    // SAFETY: both strings are NUL-terminated and live for the whole call.
    unsafe {
        MoveFileExW(
            &HSTRING::from(from.as_os_str()),
            &HSTRING::from(to.as_os_str()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
}

/// A temp file kept as the only copy of a photo under its temp name (the rename to a
/// copy name failed): its extended path and, when it could be read, its file ID.
pub(super) struct Kept {
    path: PathBuf,
    id: Option<(u32, u64)>,
}

impl Kept {
    pub(super) fn new(path: &Path) -> Kept {
        Kept {
            path: to_extended_path(path),
            id: file_id(path),
        }
    }

    /// `path`, whose file ID is `id` (if read), is this file: the same file ID when both
    /// are known (ruling R22), else the same path whatever its case or `\\?\` spelling.
    pub(super) fn matches(&self, path: &Path, id: Option<(u32, u64)>) -> bool {
        match (self.id, id) {
            (Some(a), Some(b)) => a == b,
            _ => same_path(&self.path, &to_extended_path(path)),
        }
    }
}

/// The kept temp files of this process. [`sweep_stale_temps`] never removes them.
static KEPT: Mutex<Vec<Kept>> = Mutex::new(Vec::new());

fn kept() -> MutexGuard<'static, Vec<Kept>> {
    KEPT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the file at `path` is a kept temp file.
#[cfg(test)]
pub(super) fn is_kept(path: &Path) -> bool {
    let id = file_id(path);
    kept().iter().any(|k| k.matches(path, id))
}

/// The volume serial number and file index of `path`, read through a handle that asks
/// only for attributes, so a file another program holds open without sharing answers
/// too. `None` when it cannot be read or the file system has no file IDs (index 0).
pub(super) fn file_id(path: &Path) -> Option<(u32, u64)> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .open(to_extended_path(path))
        .ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle comes from a live `File` borrowed for the whole call, and `info`
    // is a valid, writable out-parameter of the expected type.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    (index != 0).then_some((info.dwVolumeSerialNumber, index))
}

/// Most numbered copy names tried (`<stem> (light-view 사본 N).<ext>`).
const MAX_COPY_NAMES: u32 = 100;

/// Keeps the temp file at `temp`, the only copy of the photo saved to `target`, where no
/// sweep takes it for a stale temp file: renamed (never over another file) to
/// `<stem> (light-view 사본).<ext>` (then ` 2`, ` 3`… before the extension) and
/// unhidden, or, if no rename works (another program holds it open), left under its temp
/// name, unhidden if possible, and remembered in [`KEPT`].
fn keep_copy(target: &Path, temp: &Path) -> SaveOutcome {
    let renamed = copy_names(target)
        .into_iter()
        .find(|copy| move_no_replace(temp, copy).is_ok());
    let at = match renamed {
        Some(copy) => copy,
        None => {
            kept().push(Kept::new(temp));
            temp.to_path_buf()
        }
    };
    unhide(&at);
    eprintln!(
        "light-view: the original is gone or replaced; the only copy of the saved photo stays at {}",
        at.display()
    );
    SaveOutcome::KeptCopy(at)
}

/// The names a kept copy of the photo at `target` may take, in order.
fn copy_names(target: &Path) -> Vec<PathBuf> {
    let stem = target.file_stem().unwrap_or_default();
    let ext = target.extension();
    (1..=MAX_COPY_NAMES)
        .map(|n| {
            let mut name = stem.to_os_string();
            name.push(" (light-view 사본");
            if n > 1 {
                name.push(format!(" {n}"));
            }
            name.push(")");
            if let Some(ext) = ext {
                name.push(".");
                name.push(ext);
            }
            target.with_file_name(name)
        })
        .collect()
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

/// The pid in `file`, if it is exactly the temp name `.<photo>.lvtmp-<pid>` this
/// process would make for that pid (no sign, no leading zeros).
pub(super) fn temp_pid(file: &OsStr, photo: &OsStr) -> Option<u32> {
    let prefix: Vec<u16> = temp_name(photo, 0).encode_wide().collect();
    let prefix = &prefix[..prefix.len() - 1]; // without the "0"
    let wide: Vec<u16> = file.encode_wide().collect();
    let digits = wide.strip_prefix(prefix)?;
    let pid: u32 = String::from_utf16(digits).ok()?.parse().ok()?;
    (temp_name(photo, pid) == file).then_some(pid)
}

/// Whether a temp file named for process `pid` was left behind by a save that never
/// finished (a crash or power loss): it is still hidden (a kept copy is not), and it is
/// either this process's name (saves run one at a time, and a crashed earlier process may
/// have had the same pid) or no running process has that pid.
pub(super) fn is_stale(pid: u32, own: u32, hidden: bool, running: impl Fn(u32) -> bool) -> bool {
    hidden && (pid == own || !running(pid))
}

/// Removes the stale temp files of the photo at `target`: only regular files named
/// exactly `.<name>.lvtmp-<pid>` in its folder ([`temp_pid`], [`is_stale`]), found with
/// the pattern `.<name>.lvtmp-*`. A kept copy is never removed: one still under its temp
/// name ([`KEPT`]) gets another try at a copy name, so the save can use the name.
pub(crate) fn sweep_stale_temps(target: &Path, own: u32) {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return;
    };
    let mut pattern = OsString::from(".");
    pattern.push(name);
    pattern.push(".lvtmp-*");
    for (file, attrs) in find_files(&dir.join(pattern)) {
        let Some(pid) = temp_pid(&file, name) else {
            continue;
        };
        let path = dir.join(&file);
        let id = file_id(&path);
        let is_kept = kept().iter().any(|k| k.matches(&path, id));
        if is_kept {
            if let Some(copy) = copy_names(target)
                .into_iter()
                .find(|copy| move_no_replace(&path, copy).is_ok())
            {
                kept().retain(|k| !k.matches(&path, id));
                unhide(&copy);
                eprintln!(
                    "light-view: the kept copy {} is now {}",
                    path.display(),
                    copy.display()
                );
            }
            continue;
        }
        // A link or a folder is never a temp file of ours.
        let regular = attrs & (FILE_ATTRIBUTE_DIRECTORY.0 | FILE_ATTRIBUTE_REPARSE_POINT.0) == 0;
        let hidden = attrs & FILE_ATTRIBUTE_HIDDEN.0 != 0;
        if regular && is_stale(pid, own, hidden, process_running) {
            eprintln!("light-view: removing a stale {}", path.display());
            remove_temp(&path);
        }
    }
}

/// The names and attributes of the entries matching `pattern` (`FindFirstFileW`).
pub(crate) fn find_files(pattern: &Path) -> Vec<(OsString, u32)> {
    let mut found = Vec::new();
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: the string is NUL-terminated and lives for the whole call; `data` is a
    // writable out-parameter.
    let Ok(find) = (unsafe { FindFirstFileW(&HSTRING::from(pattern.as_os_str()), &mut data) })
    else {
        return found;
    };
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        found.push((
            OsString::from_wide(&data.cFileName[..len]),
            data.dwFileAttributes,
        ));
        // SAFETY: `find` is a live search handle and `data` a writable out-parameter.
        if unsafe { FindNextFileW(find, &mut data) }.is_err() {
            break;
        }
    }
    // SAFETY: `find` is a live search handle, closed once.
    let _ = unsafe { FindClose(find) };
    found
}

/// Whether a process with this pid exists; when that cannot be told (access denied),
/// it counts as running.
pub(crate) fn process_running(pid: u32) -> bool {
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
pub(crate) fn restore_times(
    target: &Path,
    (created, modified): (Option<SystemTime>, Option<SystemTime>),
) {
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
