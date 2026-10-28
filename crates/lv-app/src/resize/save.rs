//! Saving resized photos (spec §6, §10, ruling R2): as new files beside the originals, or
//! in place of the originals with a hidden backup that goes to the Recycle Bin later and
//! an undo for the whole save.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the resize session (M6 Task 5) saves")
)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::resize::{new_file_name, replace_target, OutputFormat};
use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::{
    SetFileAttributesW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES,
};

use crate::delete::{check_file, PendingDelete, Refusal, Trash};
use crate::save::atomic::{
    find_files, move_no_replace, notify_shell, process_running, restore_times, sweep_stale_temps,
    temp_name, TempFile, MAX_NAME,
};

/// Why one photo was not saved. Nothing of it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailReason {
    /// The name the result needs belongs to another file ("같은 이름의 파일 있음").
    NameTaken,
    /// The folder or the original cannot be written.
    ReadOnly,
    /// The original changed since it was shown, is gone, or is not a regular file.
    Changed,
    Io(String),
}

/// How saving one photo ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemResult {
    Saved(PathBuf),
    /// The result is at `result`; the original waits, hidden, at `backup` until the save
    /// is committed or undone ([`ReplaceBatch`]).
    Replaced {
        result: PathBuf,
        backup: PathBuf,
    },
    /// Not saved: no result meets the limit ("맞추기 불가").
    #[cfg_attr(
        test,
        expect(dead_code, reason = "the resize session (M6 Task 5) reports it")
    )]
    CantFit,
    Failed(FailReason),
}

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_WRITE_PROTECT: i32 = 19;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_FILE_EXISTS: i32 = 80;
const ERROR_ALREADY_EXISTS: i32 = 183;
/// How many times a new file looks for a free name when another program takes the one
/// it found first.
const NEW_NAME_TRIES: u32 = 3;
/// How long an undo waits for the Recycle Bin to take the results.
const UNDO_WAIT: Duration = Duration::from_secs(10);

/// Saves `bytes` (a `format` file) as a new file beside `src`: [`new_file_name`] → hidden
/// temp file `.<name>.lvtmp-<pid>` → flush → read back → moved to the name without
/// replacing anything → unhidden. Fails with [`FailReason::ReadOnly`] where the folder
/// cannot be written. Saves run one at a time (the temp-file sweep counts this process's
/// own temp files as stale).
pub fn save_new(src: &Path, bytes: &[u8], format: OutputFormat) -> ItemResult {
    for _ in 0..NEW_NAME_TRIES {
        let target = new_file_name(src, format, &taken);
        let ext = to_extended_path(&target);
        let temp = match write_temp(&ext, bytes) {
            Ok(t) => t,
            Err(r) => return ItemResult::Failed(r),
        };
        match place(temp, &ext) {
            Ok(()) => return ItemResult::Saved(target),
            // Another program took the name meanwhile: look again.
            Err(FailReason::NameTaken) => continue,
            Err(r) => return ItemResult::Failed(r),
        }
    }
    ItemResult::Failed(FailReason::NameTaken)
}

/// Saves `bytes` (a `format` file) in place of the photo at `src`, shown with identity
/// `id` (ruling R2): ① the original is checked (unchanged, a regular file, not
/// read-only; the result's name, [`replace_target`], free unless it is the original's
/// own) ② the original is renamed to its hidden backup `.<name>.lvbak-<pid>-<n>` and
/// checked again ③ the result is written to a hidden temp file and moved to its name
/// without replacing anything ④ the result takes the original's creation and
/// modification times (and its hidden attribute). A failure at any step puts the
/// original back under its name (a panic included) and fails the photo. Saves run one
/// at a time.
pub fn save_replace(
    src: &Path,
    id: &FileIdentity,
    bytes: &[u8],
    format: OutputFormat,
) -> ItemResult {
    match replace(src, id, bytes, format) {
        Ok((result, backup)) => ItemResult::Replaced { result, backup },
        Err(r) => ItemResult::Failed(r),
    }
}

fn replace(
    src: &Path,
    id: &FileIdentity,
    bytes: &[u8],
    format: OutputFormat,
) -> Result<(PathBuf, PathBuf), FailReason> {
    let name = src.file_name().ok_or(FailReason::Changed)?;
    let target = replace_target(src, format);
    let own_name = target == src;
    let backup = src.with_file_name(backup_name(name, std::process::id(), next_backup()));
    if backup.file_name().unwrap_or_default().encode_wide().count() > MAX_NAME {
        return Err(FailReason::Io(format!(
            "the backup file name would be longer than {MAX_NAME} characters"
        )));
    }
    let (src_ext, target_ext, backup_ext) = (
        to_extended_path(src),
        to_extended_path(&target),
        to_extended_path(&backup),
    );

    // ① The original, as shown.
    match check_file(src, id) {
        Ok(()) => {}
        Err(Refusal::NotAFile | Refusal::Changed) => return Err(FailReason::Changed),
        Err(r) => return Err(FailReason::Io(format!("{r:?}"))),
    }
    let meta = fs::metadata(&src_ext).map_err(|_| FailReason::Changed)?;
    if meta.permissions().readonly() {
        return Err(FailReason::ReadOnly);
    }
    let times = (meta.created().ok(), meta.modified().ok());
    let was_hidden = meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN.0 != 0;
    if !own_name && taken(&target) {
        return Err(FailReason::NameTaken);
    }

    // ② Aside, hidden. From here on every way out puts it back unless `done`.
    move_no_replace(&src_ext, &backup_ext).map_err(|e| win_reason(&e))?;
    let mut aside = Aside {
        backup: backup_ext,
        original: src_ext,
        was_hidden,
        done: false,
    };
    set_hidden(&aside.backup, true);
    if identity::verify(&aside.backup, id) != Verify::Same {
        return Err(FailReason::Changed);
    }

    // ③ The result under its name. The temp file is removed (when dropped) before
    // `aside` puts the original back.
    let temp = write_temp(&target_ext, bytes)?;
    place(temp, &target_ext)?;
    aside.done = true;

    // ④ The original's dates and hidden attribute; a failure is only logged.
    restore_times(&target_ext, times);
    if was_hidden {
        set_hidden(&target_ext, true);
    }
    if own_name {
        notify_shell(&target);
    }
    Ok((target, backup))
}

/// The original moved aside to its backup name during a replace: put back under its
/// name when dropped, unless the replace is `done`.
struct Aside {
    backup: PathBuf,
    original: PathBuf,
    was_hidden: bool,
    done: bool,
}

impl Drop for Aside {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        match move_no_replace(&self.backup, &self.original) {
            Ok(()) => set_hidden(&self.original, self.was_hidden),
            Err(e) => {
                // Shown, so it is not lost from sight; the next start puts it back
                // ([`recover_backups`]) once its name is free.
                set_hidden(&self.backup, false);
                eprintln!(
                    "light-view: the original stays at {} ({e})",
                    self.backup.display()
                );
            }
        }
    }
}

/// Writes `bytes` to the hidden temp file of `target` (extended) after sweeping its
/// stale ones: created new, flushed and read back.
fn write_temp(target: &Path, bytes: &[u8]) -> Result<TempFile, FailReason> {
    let name = target.file_name().ok_or(FailReason::Changed)?;
    let own = std::process::id();
    let temp = temp_name(name, own);
    if temp.encode_wide().count() > MAX_NAME {
        return Err(FailReason::Io(format!(
            "the temporary file name would be longer than {MAX_NAME} characters"
        )));
    }
    sweep_stale_temps(target, own);
    let path = target.with_file_name(temp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(FILE_ATTRIBUTE_HIDDEN.0)
        .open(&path)
        .map_err(|e| io_reason(&e))?;
    // From here on the temp file is removed on every way out, unwinding included.
    let temp = TempFile { path, keep: false };
    file.write_all(bytes).map_err(|e| io_reason(&e))?;
    // FlushFileBuffers.
    file.sync_all().map_err(|e| io_reason(&e))?;
    drop(file);
    let back = fs::read(&temp.path).map_err(|e| io_reason(&e))?;
    if back != bytes {
        return Err(FailReason::Io(
            "the temporary file reads back differently".to_owned(),
        ));
    }
    Ok(temp)
}

/// Moves `temp` to `target` (extended) without replacing anything, and unhides it.
fn place(mut temp: TempFile, target: &Path) -> Result<(), FailReason> {
    move_no_replace(&temp.path, target).map_err(|e| win_reason(&e))?;
    temp.keep = true;
    set_hidden(target, false);
    Ok(())
}

/// Whether something (or something that cannot be read) has the name `path`.
fn taken(path: &Path) -> bool {
    !matches!(
        fs::symlink_metadata(to_extended_path(path)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    )
}

fn io_reason(e: &io::Error) -> FailReason {
    match e.raw_os_error() {
        Some(ERROR_ACCESS_DENIED | ERROR_WRITE_PROTECT) => FailReason::ReadOnly,
        Some(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS) => FailReason::NameTaken,
        Some(ERROR_SHARING_VIOLATION) => FailReason::Io("the file is in use".to_owned()),
        _ if e.kind() == io::ErrorKind::NotFound => FailReason::Changed,
        _ => FailReason::Io(e.to_string()),
    }
}

/// [`io_reason`] for a failed Win32 call.
fn win_reason(e: &windows::core::Error) -> FailReason {
    let code = e.code().0 as u32;
    // HRESULT_FROM_WIN32: severity 1, facility 7.
    if code >> 16 == 0x8007 {
        io_reason(&io::Error::from_raw_os_error((code & 0xFFFF) as i32))
    } else {
        FailReason::Io(e.to_string())
    }
}

/// Sets or clears the hidden attribute of `path` (extended); a failure is only logged.
fn set_hidden(path: &Path, hidden: bool) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let attrs = meta.file_attributes();
    let want = if hidden {
        attrs | FILE_ATTRIBUTE_HIDDEN.0
    } else {
        attrs & !FILE_ATTRIBUTE_HIDDEN.0
    };
    if want == attrs {
        return;
    }
    // SAFETY: the string is NUL-terminated and lives for the whole call.
    let set = unsafe {
        SetFileAttributesW(
            &HSTRING::from(path.as_os_str()),
            FILE_FLAGS_AND_ATTRIBUTES(want),
        )
    };
    if let Err(e) = set {
        eprintln!(
            "light-view: could not change the hidden attribute of {}: {e}",
            path.display()
        );
    }
}

fn is_hidden(path: &Path) -> bool {
    fs::metadata(to_extended_path(path))
        .is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN.0 != 0)
}

/// The `n` of this process's next backup name.
fn next_backup() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// `.<name>.lvbak-<pid>-<n>`.
fn backup_name(name: &OsStr, pid: u32, n: u32) -> OsString {
    let mut backup = OsString::from(".");
    backup.push(name);
    backup.push(format!(".lvbak-{pid}-{n}"));
    backup
}

/// The original name and the pid in `file`, if it is exactly a name [`backup_name`]
/// makes (a non-empty name; no sign or leading zeros in the numbers).
fn parse_backup_name(file: &OsStr) -> Option<(OsString, u32)> {
    let wide: Vec<u16> = file.encode_wide().collect();
    let marker: Vec<u16> = ".lvbak-".encode_utf16().collect();
    let at = wide.windows(marker.len()).rposition(|w| w == marker)?;
    let rest = String::from_utf16(&wide[at + marker.len()..]).ok()?;
    let (pid, n) = rest.split_once('-')?;
    let (pid, n): (u32, u32) = (pid.parse().ok()?, n.parse().ok()?);
    let name = OsString::from_wide(wide.get(1..at)?);
    (!name.is_empty() && backup_name(&name, pid, n) == file).then_some((name, pid))
}

/// The photos one save replaced. Either committed when its toast goes or the app ends
/// (the backups go to the Recycle Bin) or undone; a batch only dropped leaves its
/// backups to [`recover_backups`] on a later start.
#[derive(Default)]
pub struct ReplaceBatch {
    items: Vec<Replaced>,
}

/// One replaced photo, with the identities its files had right after the save.
struct Replaced {
    result: PathBuf,
    result_id: Option<FileIdentity>,
    backup: PathBuf,
    backup_id: Option<FileIdentity>,
    original: PathBuf,
}

impl ReplaceBatch {
    pub fn new() -> ReplaceBatch {
        ReplaceBatch::default()
    }

    /// Adds a photo [`save_replace`] just replaced: its result and backup (from
    /// [`ItemResult::Replaced`]) and the original's path. Their identities are read now;
    /// commit and undo act only on files that still have them.
    pub fn push(&mut self, result: &Path, backup: &Path, original: &Path) {
        self.items.push(Replaced {
            result: result.to_path_buf(),
            result_id: identity::capture(result).ok(),
            backup: backup.to_path_buf(),
            backup_id: identity::capture(backup).ok(),
            original: original.to_path_buf(),
        });
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Sends the backups to the Recycle Bin through `trash` (which re-checks each). The
    /// caller flushes `trash` before the app ends.
    pub fn commit(self, trash: &dyn Trash) {
        for item in &self.items {
            recycle_backup(item, trash);
        }
    }

    /// Undoes the save (spec §10): every result unchanged since the save goes to the
    /// Recycle Bin, then every backup goes back under its original name if that name is
    /// free. Returns the originals not fully restored, with the reason: one whose name is
    /// still taken (its result changed, or the Recycle Bin did not take it) — its backup
    /// then goes to the Recycle Bin as on commit — and one that came back beside a result
    /// that stays.
    pub fn undo(self, trash: &dyn Trash) -> Vec<(PathBuf, String)> {
        let mut sent = Vec::with_capacity(self.items.len());
        for item in &self.items {
            let id = item
                .result_id
                .as_ref()
                .filter(|id| check_file(&item.result, id).is_ok());
            // The result has the original's hidden attribute (`replace` copied it).
            let hidden = id.is_some() && is_hidden(&item.result);
            if let Some(id) = id {
                trash.recycle(PendingDelete {
                    path: item.result.clone(),
                    id: id.clone(),
                });
            }
            sent.push((id.is_some(), hidden));
        }
        trash.flush(UNDO_WAIT);

        let mut failed = Vec::new();
        for (item, (sent, hidden)) in self.items.iter().zip(sent) {
            let result_stays = !sent
                || item
                    .result_id
                    .as_ref()
                    .is_some_and(|id| identity::verify(&item.result, id) != Verify::Missing);
            match restore_backup(item, hidden) {
                Ok(()) if !result_stays => {}
                Ok(()) => failed.push((
                    item.original.clone(),
                    if sent {
                        "the resized file could not go to the Recycle Bin"
                    } else {
                        "the resized file changed after the save and stays"
                    }
                    .to_owned(),
                )),
                Err(why) => {
                    recycle_backup(item, trash);
                    failed.push((item.original.clone(), why));
                }
            }
        }
        failed
    }
}

/// Puts `item`'s backup back under the original's name, hidden if `hidden`.
fn restore_backup(item: &Replaced, hidden: bool) -> Result<(), String> {
    if taken(&item.original) {
        return Err("another file has the original's name".to_owned());
    }
    let unchanged = item
        .backup_id
        .as_ref()
        .is_some_and(|id| identity::verify(&item.backup, id) == Verify::Same);
    if !unchanged {
        return Err("the original changed or is gone".to_owned());
    }
    let original = to_extended_path(&item.original);
    move_no_replace(&to_extended_path(&item.backup), &original)
        .map_err(|e| format!("the original could not get its name back: {e}"))?;
    set_hidden(&original, hidden);
    Ok(())
}

fn recycle_backup(item: &Replaced, trash: &dyn Trash) {
    match &item.backup_id {
        Some(id) => trash.recycle(PendingDelete {
            path: item.backup.clone(),
            id: id.clone(),
        }),
        None => eprintln!(
            "light-view: the backup {} could not be read and stays",
            item.backup.display()
        ),
    }
}

/// Crash recovery (ruling R2): puts every backup in `dir` left by a process that is no
/// longer running back under its original name, unhidden, when that name is free. A
/// backup whose name is taken stays; nothing is deleted. Only regular files named
/// exactly as [`backup_name`] makes count. Returns how many came back.
pub fn recover_backups(dir: &Path) -> usize {
    let dir = to_extended_path(dir);
    let own = std::process::id();
    let mut restored = 0;
    for (file, attrs) in find_files(&dir.join(".*.lvbak-*")) {
        let Some((name, pid)) = parse_backup_name(&file) else {
            continue;
        };
        // A link or a folder is never a backup of ours.
        if attrs & (FILE_ATTRIBUTE_DIRECTORY.0 | FILE_ATTRIBUTE_REPARSE_POINT.0) != 0 {
            continue;
        }
        // This process's backups, and a running one's, wait for their commit or undo.
        if pid == own || process_running(pid) {
            continue;
        }
        let (backup, original) = (dir.join(&file), dir.join(&name));
        if taken(&original) {
            continue;
        }
        match move_no_replace(&backup, &original) {
            Ok(()) => {
                set_hidden(&original, false);
                eprintln!(
                    "light-view: put back {} after an unfinished save",
                    original.display()
                );
                restored += 1;
            }
            Err(e) => eprintln!("light-view: {} stays: {e}", backup.display()),
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, FileTimes, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{FileTimesExt, MetadataExt};
    use std::process::Command;
    use std::time::{Duration, SystemTime};

    use lv_core::identity::{capture, to_extended_path};
    use tempfile::TempDir;

    use crate::delete::fake::FakeTrash;
    use crate::save::atomic::process_running;

    const HIDDEN: u32 = 0x2;

    fn folder() -> TempDir {
        tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap()
    }

    fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(to_extended_path(&p), bytes).unwrap();
        p
    }

    fn read(p: &Path) -> Vec<u8> {
        fs::read(to_extended_path(p)).unwrap()
    }

    fn hidden(p: &Path) -> bool {
        fs::metadata(to_extended_path(p)).unwrap().file_attributes() & HIDDEN != 0
    }

    /// The names in `dir`, sorted, without the fake Recycle Bin.
    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(to_extended_path(dir))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "bin")
            .collect();
        v.sort();
        v
    }

    fn file_name(p: &Path) -> String {
        p.file_name().unwrap().to_string_lossy().into_owned()
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn set_dates(p: &Path, created: SystemTime, modified: SystemTime) {
        OpenOptions::new()
            .write(true)
            .open(to_extended_path(p))
            .unwrap()
            .set_times(FileTimes::new().set_created(created).set_modified(modified))
            .unwrap();
    }

    fn dates(p: &Path) -> (SystemTime, SystemTime) {
        let m = fs::metadata(to_extended_path(p)).unwrap();
        (m.created().unwrap(), m.modified().unwrap())
    }

    fn replaced(r: ItemResult) -> (PathBuf, PathBuf) {
        match r {
            ItemResult::Replaced { result, backup } => (result, backup),
            other => panic!("not replaced: {other:?}"),
        }
    }

    /// A pid no process has.
    fn dead_pid() -> u32 {
        (0xFFFF_FF00..u32::MAX)
            .step_by(4)
            .find(|&p| !process_running(p))
            .unwrap()
    }

    /// A deny-write ACE for the current user on a folder (ruling P2), removed on drop.
    struct DenyWrite {
        dir: PathBuf,
        user: String,
    }

    impl DenyWrite {
        fn new(dir: &Path) -> DenyWrite {
            let user = format!(
                "{}\\{}",
                std::env::var("USERDOMAIN").unwrap(),
                std::env::var("USERNAME").unwrap()
            );
            let status = Command::new("icacls")
                .arg(dir)
                // The write rights alone: the generic W would deny SYNCHRONIZE and
                // READ_CONTROL too, and the folder could not even be listed.
                .args(["/deny", &format!("{user}:(WD,AD,WEA,WA)")])
                .output()
                .unwrap();
            assert!(status.status.success(), "{status:?}");
            DenyWrite {
                dir: dir.to_path_buf(),
                user,
            }
        }
    }

    impl Drop for DenyWrite {
        fn drop(&mut self) {
            let _ = Command::new("icacls")
                .arg(&self.dir)
                .args(["/remove:d", &self.user])
                .output();
        }
    }

    #[test]
    fn new_file_name_counts_up() {
        let f = folder();
        let a = put(f.path(), "a.jpg", b"original");
        put(f.path(), "a_small.jpg", b"earlier");
        let r = save_new(&a, b"resized", OutputFormat::Jpeg);
        let to = f.path().join("a_small (2).jpg");
        assert_eq!(r, ItemResult::Saved(to.clone()));
        assert_eq!(read(&to), b"resized");
        assert!(!hidden(&to));
        assert_eq!(read(&f.path().join("a_small.jpg")), b"earlier");
        assert_eq!(names(f.path()), ["a.jpg", "a_small (2).jpg", "a_small.jpg"]);
    }

    #[test]
    fn save_new_changes_the_extension_to_the_format() {
        let f = folder();
        let src = put(f.path(), "IMG.HEIC", b"heic");
        let r = save_new(&src, b"webp", OutputFormat::WebP);
        assert_eq!(r, ItemResult::Saved(f.path().join("IMG_small.webp")));
        assert_eq!(names(f.path()), ["IMG.HEIC", "IMG_small.webp"]);
    }

    #[test]
    fn replace_refuses_existing_target_name() {
        let f = folder();
        let heic = put(f.path(), "IMG.HEIC", b"heic");
        put(f.path(), "IMG.jpg", b"someone else's");
        let id = capture(&heic).unwrap();
        let r = save_replace(&heic, &id, b"resized", OutputFormat::Jpeg);
        assert_eq!(r, ItemResult::Failed(FailReason::NameTaken));
        assert_eq!(read(&heic), b"heic");
        assert_eq!(read(&f.path().join("IMG.jpg")), b"someone else's");
        assert_eq!(names(f.path()), ["IMG.HEIC", "IMG.jpg"]);
    }

    #[test]
    fn replace_keeps_dates() {
        let f = folder();
        let created = at(1_000_000_000);
        let modified = at(1_100_000_000);
        for (name, format, result_name) in [
            ("a.jpg", OutputFormat::Jpeg, "a.jpg"),
            ("IMG.HEIC", OutputFormat::Jpeg, "IMG.jpg"),
        ] {
            let src = put(f.path(), name, b"original");
            set_dates(&src, created, modified);
            let id = capture(&src).unwrap();
            let (result, backup) = replaced(save_replace(&src, &id, b"resized", format));
            assert_eq!(result, f.path().join(result_name));
            assert_eq!(read(&result), b"resized");
            assert_eq!(dates(&result), (created, modified), "{name}");
            assert!(!hidden(&result));
            // The original waits, hidden, under its backup name.
            assert_eq!(read(&backup), b"original");
            assert!(hidden(&backup));
            assert!(file_name(&backup).starts_with(&format!(".{name}.lvbak-")));
        }
    }

    #[test]
    fn undo_restores_all() {
        let f = folder();
        let trash = FakeTrash::new(f.path(), true);
        let mut batch = ReplaceBatch::new();
        for (name, format) in [
            ("a.jpg", OutputFormat::Jpeg),
            ("b.HEIC", OutputFormat::Jpeg),
            ("c.png", OutputFormat::WebP),
        ] {
            let src = put(f.path(), name, name.as_bytes());
            let id = capture(&src).unwrap();
            let (result, backup) = replaced(save_replace(&src, &id, b"resized", format));
            batch.push(&result, &backup, &src);
        }
        assert_eq!(names(f.path()).len(), 6);
        assert_eq!(batch.len(), 3);
        assert!(!batch.is_empty() && ReplaceBatch::new().is_empty());
        let failed = batch.undo(&trash);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(names(f.path()), ["a.jpg", "b.HEIC", "c.png"]);
        for name in ["a.jpg", "b.HEIC", "c.png"] {
            let p = f.path().join(name);
            assert_eq!(read(&p), name.as_bytes());
            assert!(!hidden(&p), "{name}");
        }
        // The Recycle Bin got the three results.
        assert_eq!(trash.binned(), ["a.jpg", "b.jpg", "c.webp"]);
        assert!(trash.results.borrow().iter().all(|(_, r)| r.is_ok()));
    }

    #[test]
    fn commit_trashes_backups() {
        let f = folder();
        let trash = FakeTrash::new(f.path(), true);
        let mut batch = ReplaceBatch::new();
        let mut backups = Vec::new();
        for name in ["a.jpg", "b.HEIC"] {
            let src = put(f.path(), name, b"original");
            let id = capture(&src).unwrap();
            let (result, backup) =
                replaced(save_replace(&src, &id, b"resized", OutputFormat::Jpeg));
            batch.push(&result, &backup, &src);
            backups.push(file_name(&backup));
        }
        batch.commit(&trash);
        backups.sort();
        assert_eq!(trash.binned(), backups);
        assert_eq!(names(f.path()), ["a.jpg", "b.jpg"]);
        assert_eq!(read(&f.path().join("a.jpg")), b"resized");
        assert_eq!(read(&f.path().join("b.jpg")), b"resized");
    }

    #[test]
    fn changed_original_is_not_replaced() {
        let f = folder();
        let src = put(f.path(), "a.jpg", b"original");
        let id = capture(&src).unwrap();
        fs::write(&src, b"edited elsewhere").unwrap();
        let r = save_replace(&src, &id, b"resized", OutputFormat::Jpeg);
        assert_eq!(r, ItemResult::Failed(FailReason::Changed));
        assert_eq!(read(&src), b"edited elsewhere");
        assert_eq!(names(f.path()), ["a.jpg"]);
        // Gone is changed too.
        fs::remove_file(&src).unwrap();
        let r = save_replace(&src, &id, b"resized", OutputFormat::Jpeg);
        assert_eq!(r, ItemResult::Failed(FailReason::Changed));
        assert!(names(f.path()).is_empty());
    }

    #[test]
    fn read_only_folder_fails_that_item() {
        let locked = folder();
        let open = folder();
        let a = put(locked.path(), "a.jpg", b"a");
        let b = put(open.path(), "b.jpg", b"b");
        let _deny = DenyWrite::new(locked.path());
        assert_eq!(
            save_new(&a, b"resized", OutputFormat::Jpeg),
            ItemResult::Failed(FailReason::ReadOnly)
        );
        assert_eq!(
            save_new(&b, b"resized", OutputFormat::Jpeg),
            ItemResult::Saved(open.path().join("b_small.jpg"))
        );
        let id = capture(&a).unwrap();
        assert_eq!(
            save_replace(&a, &id, b"resized", OutputFormat::Jpeg),
            ItemResult::Failed(FailReason::ReadOnly)
        );
        assert_eq!(read(&a), b"a");
        assert_eq!(names(locked.path()), ["a.jpg"]);
    }

    #[test]
    fn read_only_file_is_not_replaced() {
        let f = folder();
        let src = put(f.path(), "a.jpg", b"original");
        let mut perms = fs::metadata(&src).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&src, perms.clone()).unwrap();
        let id = capture(&src).unwrap();
        let r = save_replace(&src, &id, b"resized", OutputFormat::Jpeg);
        assert_eq!(r, ItemResult::Failed(FailReason::ReadOnly));
        assert_eq!(read(&src), b"original");
        assert_eq!(names(f.path()), ["a.jpg"]);
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&src, perms).unwrap();
    }

    #[test]
    fn failed_replace_puts_the_original_back() {
        let f = folder();
        let src = put(f.path(), "a.jpg", b"original");
        set_dates(&src, at(1_000_000_000), at(1_100_000_000));
        // A folder where the temp file goes: the sweep leaves it and the write fails
        // after the original was moved aside.
        let blocker = format!(".a.jpg.lvtmp-{}", std::process::id());
        fs::create_dir(f.path().join(&blocker)).unwrap();
        let id = capture(&src).unwrap();
        let r = save_replace(&src, &id, b"resized", OutputFormat::Jpeg);
        assert!(matches!(r, ItemResult::Failed(_)), "{r:?}");
        assert_eq!(read(&src), b"original");
        assert!(!hidden(&src));
        assert_eq!(capture(&src).unwrap(), id);
        assert_eq!(names(f.path()), [blocker.as_str(), "a.jpg"]);
    }

    #[test]
    fn undo_reports_what_it_cannot_restore() {
        let f = folder();
        let trash = FakeTrash::new(f.path(), true);
        let mut batch = ReplaceBatch::new();
        // Same name: the edited result keeps the name, so the original cannot come back.
        let a = put(f.path(), "a.jpg", b"a");
        let (ra, ba) = replaced(save_replace(
            &a,
            &capture(&a).unwrap(),
            b"resized",
            OutputFormat::Jpeg,
        ));
        batch.push(&ra, &ba, &a);
        // New name: the original comes back beside the edited result.
        let b = put(f.path(), "b.HEIC", b"b");
        let (rb, bb) = replaced(save_replace(
            &b,
            &capture(&b).unwrap(),
            b"resized",
            OutputFormat::Jpeg,
        ));
        batch.push(&rb, &bb, &b);
        fs::write(&ra, b"edited after").unwrap();
        fs::write(&rb, b"edited after").unwrap();
        let failed = batch.undo(&trash);
        let failed: Vec<PathBuf> = failed.into_iter().map(|(p, _)| p).collect();
        assert_eq!(failed, [a.clone(), b.clone()]);
        // Edited results are never thrown away.
        assert_eq!(read(&ra), b"edited after");
        assert_eq!(read(&rb), b"edited after");
        assert_eq!(read(&b), b"b");
        // The original that could not come back went to the Recycle Bin, as on commit.
        assert_eq!(trash.binned(), [file_name(&ba)]);
        assert_eq!(names(f.path()), ["a.jpg", "b.HEIC", "b.jpg"]);
    }

    #[test]
    fn recover_backups_restores_after_crash() {
        let f = folder();
        let dead = dead_pid();
        // Crashed after moving the original aside: it comes back.
        let lost = put(f.path(), &format!(".IMG.HEIC.lvbak-{dead}-0"), b"heic");
        let hide = Command::new("attrib")
            .arg("+h")
            .arg(&lost)
            .status()
            .unwrap();
        assert!(hide.success());
        // Crashed after the result took the name: the backup stays.
        put(f.path(), "a.jpg", b"result");
        put(f.path(), &format!(".a.jpg.lvbak-{dead}-1"), b"a");
        // A running process's backup (this one's) is not touched.
        put(
            f.path(),
            &format!(".b.jpg.lvbak-{}-2", std::process::id()),
            b"b",
        );
        // Not a backup name.
        put(f.path(), &format!(".c.jpg.lvbak-{dead}-x"), b"c");
        assert_eq!(recover_backups(f.path()), 1);
        let back = f.path().join("IMG.HEIC");
        assert_eq!(read(&back), b"heic");
        assert!(!hidden(&back));
        let mut want = vec![
            format!(".a.jpg.lvbak-{dead}-1"),
            format!(".b.jpg.lvbak-{}-2", std::process::id()),
            format!(".c.jpg.lvbak-{dead}-x"),
            "IMG.HEIC".to_owned(),
            "a.jpg".to_owned(),
        ];
        want.sort();
        assert_eq!(names(f.path()), want);
        // Nothing left to do the second time.
        assert_eq!(recover_backups(f.path()), 0);
    }

    #[test]
    fn long_paths_save_and_replace() {
        let f = folder();
        let deep = f.path().join("가".repeat(120)).join("나".repeat(120));
        fs::create_dir_all(to_extended_path(&deep)).unwrap();
        let src = put(&deep, "IMG.HEIC", b"heic");
        assert!(src.as_os_str().encode_wide().count() > 260);
        assert_eq!(
            save_new(&src, b"new", OutputFormat::Jpeg),
            ItemResult::Saved(deep.join("IMG_small.jpg"))
        );
        let id = capture(&src).unwrap();
        let (result, backup) = replaced(save_replace(&src, &id, b"resized", OutputFormat::Jpeg));
        assert_eq!(result, deep.join("IMG.jpg"));
        assert_eq!(read(&result), b"resized");
        assert_eq!(read(&backup), b"heic");
        // A crash now: the original comes back on the next start.
        assert_eq!(recover_backups(&deep), 0, "this process is still running");
    }
}
