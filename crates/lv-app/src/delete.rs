//! Deleting the photo on screen (spec §5.5, ruling R2): "pending, then Recycle Bin".
//!
//! A delete only hides the file and keeps it pending, so it can be undone. The pending
//! file goes to the Recycle Bin when the next delete happens or the app ends
//! ([`Deleter::commit_all`]); if the app crashes first, the file stays where it was.
//! Where there is no Recycle Bin the caller asks first, and [`Deleter::confirm_permanent`]
//! deletes for good. Every step acts only on a regular file (never a folder or a link)
//! whose identity is unchanged since it was shown, re-checked right before acting.

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::listing::same_path;
use windows::core::{implement, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{E_ABORT, HANDLE, HWND};
use windows::Win32::Storage::FileSystem::{
    DeleteFileW, FileAttributeTagInfo, GetDriveTypeW, GetFileInformationByHandleEx,
    GetVolumePathNameW, FILE_ATTRIBUTE_TAG_INFO,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl,
    IShellItem, SHCreateItemFromParsingName, SHQueryRecycleBinW, FOFX_EARLYFAILURE,
    FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
    SHQUERYRBINFO, SIGDN_FILESYSPATH, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};

use crate::commands;
use crate::input;
use crate::overlay::{DialogTarget, Prompt, PromptResult, Toast, ToastAction};
use crate::strings::{t, tf, S};
use crate::viewer::LeaveReason;
use crate::window::App;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// `DELETE` access right.
const DELETE: u32 = 0x0001_0000;
const FILE_SHARE_ALL: u32 = 0x7;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;

/// Why a delete did not happen. Nothing was changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The file changed or could not be read since it was shown.
    Changed,
    ReadOnly,
    NoPermission,
    /// Another program holds the file open.
    InUse,
    /// A folder, link or other non-regular file.
    NotAFile,
    /// The permanent delete itself failed.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteStep {
    /// Hidden and pending; `committed` is the earlier pending file now sent to the
    /// Recycle Bin.
    Pending {
        committed: Option<PathBuf>,
    },
    /// No Recycle Bin here: ask before deleting for good. Nothing changed.
    NeedsConfirm,
    Refused(Refusal),
}

/// A file deleted on screen but still on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingDelete {
    pub path: PathBuf,
    pub id: FileIdentity,
}

/// Why a pending file was not sent to the Recycle Bin. It stays where it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitError {
    Changed,
    NotAFile,
    Failed(String),
}

/// Where deletes go.
pub trait Trash {
    /// Whether deleting at `path` can go to a Recycle Bin. When unsure, false.
    fn has_recycle_bin(&self, path: &Path) -> bool;
    /// Sends `p` to the Recycle Bin through [`commit`] (which re-checks it first), now
    /// or on another thread.
    fn recycle(&self, p: PendingDelete);
    /// Waits up to `timeout` for the recycles queued so far.
    fn flush(&self, timeout: Duration);
}

/// Checks that `p` is still the regular file that was shown, then hands its path to
/// `recycle`.
pub fn commit(
    p: &PendingDelete,
    recycle: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), CommitError> {
    match check_file(&p.path, &p.id) {
        Ok(()) => {}
        Err(Refusal::NotAFile) => return Err(CommitError::NotAFile),
        Err(_) => return Err(CommitError::Changed),
    }
    recycle(&p.path).map_err(CommitError::Failed)
}

pub struct Deleter {
    trash: Box<dyn Trash>,
    pending: Option<PendingDelete>,
}

impl Deleter {
    pub fn new(trash: Box<dyn Trash>) -> Deleter {
        Deleter {
            trash,
            pending: None,
        }
    }

    /// Deletes `path` (shown with identity `id`). Where there is a Recycle Bin the file
    /// becomes pending and the earlier pending file is sent there; otherwise the caller
    /// must ask ([`DeleteStep::NeedsConfirm`]). Nothing on disk changes here.
    pub fn delete(&mut self, path: &Path, id: &FileIdentity) -> DeleteStep {
        if let Err(r) = check(path, id) {
            return DeleteStep::Refused(r);
        }
        if !self.trash.has_recycle_bin(path) {
            return DeleteStep::NeedsConfirm;
        }
        let committed = self.commit_pending();
        self.pending = Some(PendingDelete {
            path: path.to_path_buf(),
            id: id.clone(),
        });
        DeleteStep::Pending { committed }
    }

    /// Deletes `path` for good, after the user confirmed it, if it is still the file
    /// that was shown. There is no undo.
    pub fn confirm_permanent(&mut self, path: &Path, id: &FileIdentity) -> Result<(), Refusal> {
        check(path, id)?;
        delete_file(&to_extended_path(path))
    }

    /// Cancels the pending delete; its path, to show it again.
    pub fn undo(&mut self) -> Option<PathBuf> {
        self.pending.take().map(|p| p.path)
    }

    pub fn pending_path(&self) -> Option<&Path> {
        self.pending.as_ref().map(|p| p.path.as_path())
    }

    /// At exit: sends the pending file to the Recycle Bin and waits up to `timeout`.
    pub fn commit_all(&mut self, timeout: Duration) {
        self.commit_pending();
        self.trash.flush(timeout);
    }

    fn commit_pending(&mut self) -> Option<PathBuf> {
        let p = self.pending.take()?;
        let path = p.path.clone();
        self.trash.recycle(p);
        Some(path)
    }
}

/// Everything a delete needs: a regular file, unchanged, writable, deletable now.
fn check(path: &Path, id: &FileIdentity) -> Result<(), Refusal> {
    check_file(path, id)?;
    let ext = to_extended_path(path);
    let meta = fs::symlink_metadata(&ext).map_err(|_| Refusal::Changed)?;
    if meta.permissions().readonly() {
        return Err(Refusal::ReadOnly);
    }
    // Opening for DELETE access asks the same question the delete will, without
    // deleting anything.
    fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&ext)
        .map(drop)
        .map_err(|e| io_refusal(&e))
}

/// A regular file (no folder, link or junction) with the identity `id`. A reparse point
/// passes only with a tag that keeps it a plain file ([`file_like_reparse_tag`]), such as a
/// OneDrive placeholder.
fn check_file(path: &Path, id: &FileIdentity) -> Result<(), Refusal> {
    let ext = to_extended_path(path);
    let meta = fs::symlink_metadata(&ext).map_err(|_| Refusal::Changed)?;
    if !meta.is_file() {
        return Err(Refusal::NotAFile);
    }
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && !reparse_tag(&ext).is_ok_and(file_like_reparse_tag)
    {
        return Err(Refusal::NotAFile);
    }
    // `Missing` may be a transient error: never delete then either.
    if identity::verify(path, id) != Verify::Same {
        return Err(Refusal::Changed);
    }
    Ok(())
}

const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
/// The cloud tags `IO_REPARSE_TAG_CLOUD_1` to `_F` differ from it only in these bits.
const CLOUD_VARIANT_BITS: u32 = 0x0000_F000;
const IO_REPARSE_TAG_DEDUP: u32 = 0x8000_0013;
const IO_REPARSE_TAG_WOF: u32 = 0x8000_0017;
/// The name-surrogate bit (`IsReparseTagNameSurrogate`): the reparse point stands for
/// another named entity (a symbolic link, a junction and the like).
const NAME_SURROGATE: u32 = 0x2000_0000;

/// Whether a reparse point with `tag` is still the file itself: cloud placeholders
/// (OneDrive, every `IO_REPARSE_TAG_CLOUD*` variant), deduplicated and WOF-compressed
/// files. Name surrogates (links, junctions) and unknown tags are not.
fn file_like_reparse_tag(tag: u32) -> bool {
    if tag & NAME_SURROGATE != 0 {
        return false;
    }
    tag & !CLOUD_VARIANT_BITS == IO_REPARSE_TAG_CLOUD
        || tag == IO_REPARSE_TAG_DEDUP
        || tag == IO_REPARSE_TAG_WOF
}

/// The reparse tag of the entry at `ext` itself (never what it points to), read through a
/// handle that has no data access (opening it recalls no cloud file).
fn reparse_tag(ext: &Path) -> io::Result<u32> {
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(ext)?;
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: the handle is open for the whole call and `info` is a writable buffer of
    // exactly the size passed.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }?;
    Ok(info.ReparseTag)
}

fn io_refusal(e: &io::Error) -> Refusal {
    match e.raw_os_error() {
        Some(ERROR_ACCESS_DENIED) => Refusal::NoPermission,
        Some(ERROR_SHARING_VIOLATION) => Refusal::InUse,
        _ if e.kind() == io::ErrorKind::NotFound => Refusal::Changed,
        _ => Refusal::Failed(e.to_string()),
    }
}

/// `DeleteFileW` on the extended path (never removes a folder).
fn delete_file(ext: &Path) -> Result<(), Refusal> {
    // SAFETY: plain call with a NUL-terminated wide string that outlives it.
    unsafe { DeleteFileW(&HSTRING::from(ext.as_os_str())) }
        .map_err(|e| io_refusal(&io::Error::from_raw_os_error(e.code().0 & 0xFFFF)))
}

// ---- The Windows Recycle Bin ---------------------------------------------------

/// The shell takes no `\\?\` paths (E_INVALIDARG), and asked to delete a longer path
/// through its short (8.3) form it deletes for good: the Recycle Bin cannot hold it. So
/// only paths shorter than this (UTF-16 units) are recycled.
const MAX_PATH: usize = 260;

/// Whether the shell can recycle `path` as given (the form `recycle_item` passes to it):
/// shorter than `MAX_PATH` UTF-16 units, as Windows counts (not bytes: Hangul is three
/// bytes in an `OsStr` but one unit).
fn short_enough_for_shell(path: &Path) -> bool {
    path.as_os_str().encode_wide().count() < MAX_PATH
}
const DRIVE_FIXED: u32 = 3;

/// Called on the recycle thread with each pending file's result.
pub type Report = Box<dyn Fn(PathBuf, Result<(), CommitError>) + Send>;

enum Job {
    Recycle(PendingDelete),
    Flush(Sender<()>),
}

/// The real Recycle Bin: `IFileOperation` on its own single-threaded COM thread, so
/// the window never waits for the shell. Each result goes to `report`.
pub struct ShellTrash {
    jobs: Sender<Job>,
}

impl ShellTrash {
    pub fn new(report: Report) -> ShellTrash {
        let (jobs, rx) = channel::<Job>();
        std::thread::spawn(move || recycle_thread(&rx, &report));
        ShellTrash { jobs }
    }
}

impl Trash for ShellTrash {
    /// Only fixed local drives whose Recycle Bin answers, and only paths shorter than
    /// `MAX_PATH`. Network, removable and unknown locations and long paths need the
    /// permanent-delete question.
    fn has_recycle_bin(&self, path: &Path) -> bool {
        if !short_enough_for_shell(path) {
            return false;
        }
        let Some(root) = volume_root(path) else {
            return false;
        };
        let root = HSTRING::from(root.as_os_str());
        // SAFETY: plain call with a NUL-terminated wide string that outlives it.
        if unsafe { GetDriveTypeW(&root) } != DRIVE_FIXED {
            return false;
        }
        let mut info = SHQUERYRBINFO {
            cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `root` is NUL-terminated and `info` is a live, sized out-parameter.
        unsafe { SHQueryRecycleBinW(&root, &mut info) }.is_ok()
    }

    fn recycle(&self, p: PendingDelete) {
        // The thread only ends with the process.
        let _ = self.jobs.send(Job::Recycle(p));
    }

    fn flush(&self, timeout: Duration) {
        let (done, wait) = channel();
        if self.jobs.send(Job::Flush(done)).is_ok() {
            let _ = wait.recv_timeout(timeout);
        }
    }
}

/// The root of the volume holding `path` (a mount folder counts as a root).
fn volume_root(path: &Path) -> Option<PathBuf> {
    let ext = HSTRING::from(to_extended_path(path).as_os_str());
    let mut buf = vec![0u16; ext.len() + 2];
    // SAFETY: `ext` is NUL-terminated and `buf` is a live buffer passed with its length.
    unsafe { GetVolumePathNameW(&ext, &mut buf) }.ok()?;
    let len = buf.iter().position(|&c| c == 0)?;
    Some(PathBuf::from(OsString::from_wide(&buf[..len])))
}

fn recycle_thread(jobs: &Receiver<Job>, report: &Report) {
    // SAFETY: initializes COM for this thread only; balanced below.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    for job in jobs {
        match job {
            Job::Recycle(p) => {
                let r = if com.is_ok() {
                    commit(&p, recycle_item)
                } else {
                    Err(CommitError::Failed(format!("COM: {com:?}")))
                };
                if let Err(e) = &r {
                    eprintln!("light-view: {} stays: {e:?}", p.path.display());
                }
                report(p.path, r);
            }
            Job::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
    if com.is_ok() {
        // SAFETY: pairs with the successful CoInitializeEx above, on this thread.
        unsafe { CoUninitialize() };
    }
}

/// Sends one file to the Recycle Bin. The shell deletes for good what it cannot
/// recycle (too large for the bin, bin turned off) even with `FOFX_RECYCLEONDELETE`;
/// [`RecycleOnly`] stops such a delete before it starts.
fn recycle_item(path: &Path) -> Result<(), String> {
    if !short_enough_for_shell(path) {
        return Err("the path is too long for the Recycle Bin".into());
    }
    let refused = Arc::new(AtomicBool::new(false));
    // SAFETY: COM calls on this STA thread with live interface pointers; the shell
    // item and the sink stay alive until `PerformOperations` returns.
    unsafe {
        let op: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        op.SetOperationFlags(
            FOF_ALLOWUNDO
                | FOFX_RECYCLEONDELETE
                | FOF_NOCONFIRMATION
                | FOF_SILENT
                | FOF_NOERRORUI
                | FOFX_EARLYFAILURE,
        )
        .map_err(|e| e.to_string())?;
        let sink: IFileOperationProgressSink = RecycleOnly {
            refused: refused.clone(),
        }
        .into();
        op.Advise(&sink).map_err(|e| e.to_string())?;
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)
            .map_err(|e| format!("shell item: {e}"))?;
        op.DeleteItem(&item, None).map_err(|e| e.to_string())?;
        let done = op.PerformOperations();
        let aborted = op.GetAnyOperationsAborted().map_or(true, |b| b.as_bool());
        if refused.load(Ordering::Acquire) {
            return Err("cannot be recycled here (it would be deleted for good)".into());
        }
        done.map_err(|e| e.to_string())?;
        if aborted {
            return Err("the shell aborted the operation".into());
        }
    }
    if fs::symlink_metadata(to_extended_path(path)).is_ok() {
        return Err("still there after the operation".into());
    }
    Ok(())
}

/// A shell item's file-system path, for logs.
fn display_path(item: &IShellItem) -> String {
    // SAFETY: a live item; the returned string is shell-allocated, read once and freed
    // once with CoTaskMemFree.
    unsafe {
        let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) else {
            return String::new();
        };
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Cancels a delete the shell would do permanently instead of recycling.
#[implement(IFileOperationProgressSink)]
struct RecycleOnly {
    refused: Arc<AtomicBool>,
}

type ComResult = windows::core::Result<()>;
type Item<'a> = Option<&'a IShellItem>;

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for RecycleOnly_Impl {
    fn PreDeleteItem(&self, dwflags: u32, _item: Item) -> ComResult {
        if dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
            self.refused.store(true, Ordering::Release);
            return Err(E_ABORT.into());
        }
        Ok(())
    }
    fn StartOperations(&self) -> ComResult {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> ComResult {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Item, _: &PCWSTR) -> ComResult {
        Ok(())
    }
    fn PostRenameItem(&self, _: u32, _: Item, _: &PCWSTR, _: HRESULT, _: Item) -> ComResult {
        Ok(())
    }
    fn PreMoveItem(&self, _: u32, _: Item, _: Item, _: &PCWSTR) -> ComResult {
        Ok(())
    }
    fn PostMoveItem(&self, _: u32, _: Item, _: Item, _: &PCWSTR, _: HRESULT, _: Item) -> ComResult {
        Ok(())
    }
    fn PreCopyItem(&self, _: u32, _: Item, _: Item, _: &PCWSTR) -> ComResult {
        Ok(())
    }
    fn PostCopyItem(&self, _: u32, _: Item, _: Item, _: &PCWSTR, _: HRESULT, _: Item) -> ComResult {
        Ok(())
    }
    fn PostDeleteItem(&self, _: u32, item: Item, hr: HRESULT, recycled: Item) -> ComResult {
        // Deleted, yet no item was created in the Recycle Bin: it did not go there.
        if hr.is_ok() && recycled.is_none() {
            let name = item.map(display_path).unwrap_or_default();
            eprintln!("light-view: warning: {name} was deleted without a Recycle Bin item");
        }
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Item, _: &PCWSTR) -> ComResult {
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn PostNewItem(
        &self,
        _: u32,
        _: Item,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Item,
    ) -> ComResult {
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> ComResult {
        Ok(())
    }
    fn ResetTimer(&self) -> ComResult {
        Ok(())
    }
    fn PauseTimer(&self) -> ComResult {
        Ok(())
    }
    fn ResumeTimer(&self) -> ComResult {
        Ok(())
    }
}

// ---- The window's side ---------------------------------------------------------

/// What a recycle thread reported: the file is in the Recycle Bin, or it stays.
pub type CommitReport = (PathBuf, Result<(), CommitError>);

/// The Delete key and the 🗑 button: deletes the photo on screen, after the save prompt
/// if its rotation is unsaved. Nothing happens while a save runs (it may be saving this
/// very file) or while the current photo is still loading.
pub fn request(app: &RefCell<App>, hwnd: HWND) {
    let ready = app
        .try_borrow()
        .is_ok_and(|a| a.saving.is_none() && a.viewer.delete_target().is_some());
    if ready {
        commands::leave(app, hwnd, LeaveReason::Delete);
    }
}

/// Ctrl+Z and the toast's [실행 취소]: shows the pending deleted photo again, after the
/// save prompt if the current one's rotation is unsaved. Once the photo went to the
/// Recycle Bin there is nothing to undo.
pub fn request_undo(app: &RefCell<App>, hwnd: HWND) {
    if app
        .try_borrow()
        .is_ok_and(|a| a.deleter.pending_path().is_some())
    {
        commands::leave(app, hwnd, LeaveReason::Undo);
    }
}

/// Deletes the photo on screen (the save prompt was answered, or not needed).
pub fn delete_current(app: &RefCell<App>, hwnd: HWND) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    if a.saving.is_some() {
        return;
    }
    let Some((path, id)) = a.viewer.delete_target() else {
        return;
    };
    let Some(id) = id else {
        drop(a);
        input::show_toast(app, hwnd, |now| Toast::new(t(S::ToastFileChanged), now));
        return;
    };
    match a.deleter.delete(&path, &id) {
        DeleteStep::Pending { .. } => {
            let redraw = a.viewer.hide(&path);
            drop(a);
            commands::after(app, hwnd, redraw);
            input::show_toast(app, hwnd, Toast::recycled);
        }
        DeleteStep::NeedsConfirm => {
            let target = DialogTarget {
                path: path.clone(),
                generation: a.viewer.generation(),
            };
            drop(a);
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into();
            input::show_dialog(app, hwnd, Prompt::PermanentDelete { name }, target);
        }
        DeleteStep::Refused(r) => {
            drop(a);
            refused(app, hwnd, &r);
        }
    }
}

/// The permanent-delete question was answered. [삭제] deletes its target for good if
/// it is still the photo on screen and unchanged; anything else does nothing.
pub fn on_permanent_answer(
    app: &RefCell<App>,
    hwnd: HWND,
    result: PromptResult,
    target: &DialogTarget,
) {
    if result != PromptResult::Confirm {
        return;
    }
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let current = a.viewer.delete_target();
    let same = a.saving.is_none()
        && a.viewer.generation() == target.generation
        && current.as_ref().is_some_and(|(p, _)| *p == target.path);
    let Some(id) = current.and_then(|(_, id)| id).filter(|_| same) else {
        drop(a);
        input::show_toast(app, hwnd, |now| Toast::new(t(S::ToastFileChanged), now));
        return;
    };
    match a.deleter.confirm_permanent(&target.path, &id) {
        Ok(()) => {
            let redraw = a.viewer.remove(&target.path);
            drop(a);
            commands::after(app, hwnd, redraw);
        }
        Err(r) => {
            drop(a);
            refused(app, hwnd, &r);
        }
    }
}

/// Shows the pending deleted photo again (the save prompt was answered, or not needed).
pub fn undo(app: &RefCell<App>, hwnd: HWND) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let Some(path) = a.deleter.undo() else {
        return;
    };
    let redraw = a.viewer.restore(&path);
    let undo_toast = a
        .overlay
        .toast
        .as_ref()
        .and_then(|t| t.action.as_ref())
        .is_some_and(|(_, action)| *action == ToastAction::Undo);
    if undo_toast {
        a.overlay.toast = None;
    }
    drop(a);
    commands::after(app, hwnd, redraw);
}

/// Opening the pending deleted photo itself cancels its delete.
pub fn cancel_if_pending(a: &mut App, path: &Path) {
    if a.deleter.pending_path().is_some_and(|p| same_path(p, path)) {
        if let Some(p) = a.deleter.undo() {
            a.viewer.forget_hidden(&p);
        }
    }
}

/// Recycle results: a recycled photo is forgotten; one that stays is listed again
/// (and a failed recycle is reported).
pub fn on_reports(app: &RefCell<App>, hwnd: HWND) {
    loop {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        let Ok((path, result)) = a.delete_rx.try_recv() else {
            return;
        };
        // Pending again (a new file under the same name): that delete decides.
        if a.deleter
            .pending_path()
            .is_some_and(|p| same_path(p, &path))
        {
            continue;
        }
        match result {
            Ok(()) => a.viewer.forget_hidden(&path),
            // Not a regular file any more, or gone: never listed again (a folder must
            // never enter the list).
            Err(CommitError::NotAFile) => a.viewer.forget_hidden(&path),
            Err(CommitError::Changed) if !exists(&path) => a.viewer.forget_hidden(&path),
            Err(e) => {
                let redraw = a.viewer.unhide(&path);
                drop(a);
                commands::after(app, hwnd, redraw);
                if matches!(e, CommitError::Failed(_)) {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let text = tf(S::ToastRecycleFailed, &[&name]);
                    input::show_toast(app, hwnd, |now| Toast::new(&text, now));
                }
            }
        }
    }
}

/// The path still names something (a read error other than "not found" counts too).
fn exists(path: &Path) -> bool {
    match fs::symlink_metadata(to_extended_path(path)) {
        Ok(_) => true,
        Err(e) => !matches!(e.kind(), io::ErrorKind::NotFound),
    }
}

fn refused(app: &RefCell<App>, hwnd: HWND, r: &Refusal) {
    if let Refusal::Failed(e) = r {
        eprintln!("light-view: could not delete: {e}");
    }
    let text = match r {
        Refusal::Changed => t(S::ToastFileChanged),
        Refusal::ReadOnly => t(S::ToastReadOnly),
        Refusal::NoPermission => t(S::ToastNoPermission),
        Refusal::InUse => t(S::ToastInUse),
        Refusal::NotAFile => t(S::ToastNotAFile),
        Refusal::Failed(_) => t(S::ToastDeleteFailed),
    };
    input::show_toast(app, hwnd, |now| Toast::new(text, now));
}

#[cfg(test)]
mod tests {
    use super::*;
    use lv_core::identity::capture;
    use lv_core::listing::FolderList;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Reported = (PathBuf, Result<(), CommitError>);

    /// Moves "recycled" files into its own folder, synchronously, through `commit`.
    #[derive(Clone)]
    struct FakeTrash {
        bin: PathBuf,
        has_bin: bool,
        results: Rc<RefCell<Vec<Reported>>>,
    }

    impl FakeTrash {
        fn new(dir: &Path, has_bin: bool) -> FakeTrash {
            let bin = dir.join("bin");
            fs::create_dir(&bin).unwrap();
            FakeTrash {
                bin,
                has_bin,
                results: Rc::default(),
            }
        }

        fn binned(&self) -> Vec<String> {
            let mut v: Vec<String> = fs::read_dir(&self.bin)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        }
    }

    impl Trash for FakeTrash {
        fn has_recycle_bin(&self, _path: &Path) -> bool {
            self.has_bin
        }

        fn recycle(&self, p: PendingDelete) {
            let bin = self.bin.clone();
            let r = commit(&p, |path| {
                fs::rename(path, bin.join(path.file_name().unwrap())).map_err(|e| e.to_string())
            });
            self.results.borrow_mut().push((p.path, r));
        }

        fn flush(&self, _timeout: Duration) {}
    }

    struct Folder {
        dir: tempfile::TempDir,
    }

    impl Folder {
        fn new(names: &[&str]) -> Folder {
            let dir = tempfile::tempdir().unwrap();
            for n in names {
                fs::write(dir.path().join(n), n.as_bytes()).unwrap();
            }
            Folder { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn id(&self, name: &str) -> FileIdentity {
            capture(&self.path(name)).unwrap()
        }

        fn exists(&self, name: &str) -> bool {
            self.path(name).exists()
        }
    }

    fn deleter(trash: &FakeTrash) -> Deleter {
        Deleter::new(Box::new(trash.clone()))
    }

    const WAIT: Duration = Duration::from_secs(1);

    #[test]
    fn delete_hides_and_moves_to_next() {
        let f = Folder::new(&["a.jpg", "b.jpg", "c.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), true);
        let mut d = deleter(&trash);
        let mut list = FolderList::scan(&f.path("b.jpg")).unwrap();

        let step = d.delete(&f.path("b.jpg"), &f.id("b.jpg"));
        assert_eq!(step, DeleteStep::Pending { committed: None });
        assert!(list.hide(&f.path("b.jpg")));
        assert_eq!(list.current(), Some(f.path("c.jpg").as_path()));
        assert_eq!(list.position(), Some((2, 2)));
        // Only hidden: still on disk, nothing recycled.
        assert!(f.exists("b.jpg"));
        assert!(trash.binned().is_empty());

        // The last photo: the previous one is shown.
        assert!(matches!(
            d.delete(&f.path("c.jpg"), &f.id("c.jpg")),
            DeleteStep::Pending { .. }
        ));
        list.hide(&f.path("c.jpg"));
        assert_eq!(list.current(), Some(f.path("a.jpg").as_path()));
    }

    #[test]
    fn undo_restores_until_next_delete() {
        let f = Folder::new(&["a.jpg", "b.jpg", "c.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), true);
        let mut d = deleter(&trash);
        d.delete(&f.path("a.jpg"), &f.id("a.jpg"));
        assert_eq!(d.pending_path(), Some(f.path("a.jpg").as_path()));
        assert_eq!(d.undo(), Some(f.path("a.jpg")));
        assert_eq!(d.undo(), None);
        assert!(f.exists("a.jpg"));

        d.delete(&f.path("a.jpg"), &f.id("a.jpg"));
        d.delete(&f.path("b.jpg"), &f.id("b.jpg"));
        // Only the latest delete can be undone; the earlier one is in the bin.
        assert_eq!(d.undo(), Some(f.path("b.jpg")));
        assert_eq!(d.undo(), None);
        assert!(f.exists("b.jpg"));
        assert_eq!(trash.binned(), ["a.jpg"]);
        // After an undo, exiting recycles nothing more.
        d.commit_all(WAIT);
        assert_eq!(trash.binned(), ["a.jpg"]);
    }

    #[test]
    fn next_delete_commits_previous() {
        let f = Folder::new(&["a.jpg", "b.jpg", "c.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), true);
        let mut d = deleter(&trash);
        d.delete(&f.path("a.jpg"), &f.id("a.jpg"));
        assert!(trash.binned().is_empty());
        let step = d.delete(&f.path("b.jpg"), &f.id("b.jpg"));
        assert_eq!(
            step,
            DeleteStep::Pending {
                committed: Some(f.path("a.jpg"))
            }
        );
        assert!(!f.exists("a.jpg"));
        assert_eq!(trash.binned(), ["a.jpg"]);
        // Exit commits the last one.
        d.commit_all(WAIT);
        assert_eq!(trash.binned(), ["a.jpg", "b.jpg"]);
        assert!(f.exists("c.jpg"));
        assert_eq!(d.pending_path(), None);
    }

    #[test]
    fn crash_before_commit_keeps_file() {
        let f = Folder::new(&["a.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), true);
        let mut d = deleter(&trash);
        d.delete(&f.path("a.jpg"), &f.id("a.jpg"));
        drop(d);
        assert!(f.exists("a.jpg"));
        assert!(trash.binned().is_empty());
        assert!(trash.results.borrow().is_empty());
    }

    #[test]
    fn delete_refuses_when_identity_changed() {
        let f = Folder::new(&["a.jpg", "b.jpg", "c.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), true);
        let mut d = deleter(&trash);
        let id = f.id("a.jpg");
        fs::write(f.path("a.jpg"), b"edited elsewhere").unwrap();
        let step = d.delete(&f.path("a.jpg"), &id);
        assert_eq!(step, DeleteStep::Refused(Refusal::Changed));
        assert_eq!(d.pending_path(), None);
        // Gone (or unreadable) counts as changed too.
        let id = f.id("c.jpg");
        fs::remove_file(f.path("c.jpg")).unwrap();
        assert_eq!(
            d.delete(&f.path("c.jpg"), &id),
            DeleteStep::Refused(Refusal::Changed)
        );

        // Changed while pending: not recycled at commit, only reported.
        d.delete(&f.path("b.jpg"), &f.id("b.jpg"));
        fs::write(f.path("b.jpg"), b"edited while pending").unwrap();
        d.commit_all(WAIT);
        assert!(trash.binned().is_empty());
        assert_eq!(
            trash.results.borrow().as_slice(),
            [(f.path("b.jpg"), Err(CommitError::Changed))]
        );
        assert_eq!(fs::read(f.path("b.jpg")).unwrap(), b"edited while pending");
        assert_eq!(fs::read(f.path("a.jpg")).unwrap(), b"edited elsewhere");

        // A permanent delete re-checks too.
        let id = f.id("a.jpg");
        fs::write(f.path("a.jpg"), b"edited again").unwrap();
        assert_eq!(
            d.confirm_permanent(&f.path("a.jpg"), &id),
            Err(Refusal::Changed)
        );
        assert!(f.exists("a.jpg"));
    }

    #[test]
    fn no_recycle_bin_needs_confirm() {
        let f = Folder::new(&["a.jpg", "b.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), false);
        let mut d = deleter(&trash);
        let step = d.delete(&f.path("a.jpg"), &f.id("a.jpg"));
        assert_eq!(step, DeleteStep::NeedsConfirm);
        assert_eq!(d.pending_path(), None);
        assert!(f.exists("a.jpg"));

        // Confirmed: deleted for good, not recycled, no undo.
        assert_eq!(
            d.confirm_permanent(&f.path("a.jpg"), &f.id("a.jpg")),
            Ok(())
        );
        assert!(!f.exists("a.jpg"));
        assert!(trash.binned().is_empty());
        assert_eq!(d.undo(), None);
        assert!(f.exists("b.jpg"));
    }

    #[test]
    fn only_file_like_reparse_tags_pass() {
        // OneDrive placeholders: IO_REPARSE_TAG_CLOUD and CLOUD_1 to CLOUD_F.
        assert!(file_like_reparse_tag(0x9000_001A));
        for n in 1..=0xF_u32 {
            let tag = 0x9000_001A | (n << 12);
            assert!(file_like_reparse_tag(tag), "{tag:#x}");
        }
        assert!(file_like_reparse_tag(0x8000_0013), "dedup");
        assert!(file_like_reparse_tag(0x8000_0017), "WOF");
        for tag in [
            0xA000_000C, // symbolic link
            0xA000_0003, // junction (mount point)
            0x8000_001B, // AppExecLink
            0x8000_0014, // NFS
            0x9000_101B, // not a cloud tag, though close to one
            0x9000_001B,
            0xB000_001A, // a cloud tag with the name-surrogate bit
            0,
        ] {
            assert!(!file_like_reparse_tag(tag), "{tag:#x}");
        }
        // A real symbolic link reads as one (creating one may need Developer Mode).
        let f = Folder::new(&["a.jpg"]);
        let link = f.path("link.jpg");
        if std::os::windows::fs::symlink_file(f.path("a.jpg"), &link).is_ok() {
            assert_eq!(reparse_tag(&to_extended_path(&link)).unwrap(), 0xA000_000C);
        }
    }

    #[test]
    fn never_deletes_directories() {
        let f = Folder::new(&["a.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), true);
        let mut d = deleter(&trash);
        let dir = f.path("sub.jpg");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("inside.jpg"), b"x").unwrap();
        // Whatever identity is claimed for it.
        let id = f.id("a.jpg");
        assert_eq!(d.delete(&dir, &id), DeleteStep::Refused(Refusal::NotAFile));
        assert_eq!(d.confirm_permanent(&dir, &id), Err(Refusal::NotAFile));

        // A file replaced by a folder while pending is not recycled.
        let id = f.id("a.jpg");
        d.delete(&f.path("a.jpg"), &id);
        fs::rename(f.path("a.jpg"), f.path("moved.jpg")).unwrap();
        fs::create_dir(f.path("a.jpg")).unwrap();
        d.commit_all(WAIT);
        assert!(trash.binned().is_empty());
        assert!(f.path("a.jpg").is_dir());
        assert!(dir.join("inside.jpg").exists());

        // A link to a file is refused too (creating one may need Developer Mode).
        let link = f.path("link.jpg");
        if std::os::windows::fs::symlink_file(f.path("moved.jpg"), &link).is_ok() {
            let id = capture(&link).unwrap();
            assert_eq!(d.delete(&link, &id), DeleteStep::Refused(Refusal::NotAFile));
            assert_eq!(d.confirm_permanent(&link, &id), Err(Refusal::NotAFile));
            assert!(link.exists());
        }
        assert!(f.exists("moved.jpg"));
    }

    #[test]
    fn readonly_reports_and_keeps() {
        let f = Folder::new(&["a.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), false);
        let mut d = deleter(&trash);
        let path = f.path("a.jpg");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms.clone()).unwrap();
        let id = f.id("a.jpg");
        assert_eq!(d.delete(&path, &id), DeleteStep::Refused(Refusal::ReadOnly));
        assert_eq!(d.confirm_permanent(&path, &id), Err(Refusal::ReadOnly));
        // Untouched, attribute included.
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        assert_eq!(fs::read(&path).unwrap(), b"a.jpg");
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn a_file_held_open_without_delete_sharing_is_in_use() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Folder::new(&["a.jpg"]);
        let trash = FakeTrash::new(f.dir.path(), false);
        let mut d = deleter(&trash);
        let id = f.id("a.jpg");
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .open(f.path("a.jpg"))
            .unwrap();
        assert_eq!(
            d.delete(&f.path("a.jpg"), &id),
            DeleteStep::Refused(Refusal::InUse)
        );
        drop(held);
        assert!(f.exists("a.jpg"));
    }

    #[test]
    fn hangul_paths_are_measured_in_utf16_units() {
        // 100 Hangul syllables: 300 bytes in the OsStr, 100 UTF-16 units.
        let name = format!("{}.jpg", "가".repeat(100));
        let path = PathBuf::from(r"C:\사진").join(&name);
        assert!(path.as_os_str().len() > MAX_PATH);
        assert!(short_enough_for_shell(&path));
        let long = PathBuf::from(r"C:\사진").join(format!("{}.jpg", "가".repeat(255)));
        assert!(!short_enough_for_shell(&long));
        // On a real fixed drive with a Recycle Bin, length alone does not refuse it.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(&name);
        fs::write(&file, b"x").unwrap();
        let trash = ShellTrash::new(Box::new(|_, _| {}));
        let fixed_with_bin = trash.has_recycle_bin(&dir.path().join("x.jpg"));
        assert_eq!(trash.has_recycle_bin(&file), fixed_with_bin);
    }

    #[test]
    fn unknown_drives_have_no_recycle_bin() {
        let trash = ShellTrash::new(Box::new(|_, _| {}));
        let unused = (b'D'..=b'Z')
            .map(|c| format!("{}:\\", c as char))
            .find(|root| !Path::new(root).exists())
            .expect("a free drive letter");
        assert!(!trash.has_recycle_bin(&Path::new(&unused).join("x.jpg")));
    }

    /// A fresh `%TEMP%\lv-m3-del-*` folder (on the system drive in a usual setup).
    fn real_temp_folder(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lv-m3-del-{tag}-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn shell_trash() -> (ShellTrash, Receiver<Reported>) {
        let (tx, rx) = channel();
        let trash = ShellTrash::new(Box::new(move |path, r| {
            let _ = tx.send((path, r));
        }));
        (trash, rx)
    }

    /// Sends a file this test creates to the real Recycle Bin. Manual run:
    /// `cargo test -p lv-app delete::tests::recycles_to_the_real_bin -- --ignored`, then
    /// look for `lv-m3-recycle-test-<pid>.txt` in the Recycle Bin (read-only check):
    /// `(New-Object -ComObject Shell.Application).Namespace(10).Items() |
    ///   ? Name -like 'lv-m3-recycle-test-*' | select Name, Path`.
    #[test]
    #[ignore = "uses the real Recycle Bin"]
    fn recycles_to_the_real_bin() {
        let dir = real_temp_folder("bin");
        let path = dir.join(format!("lv-m3-recycle-test-{}.txt", std::process::id()));
        fs::write(&path, b"light-view recycle test").unwrap();
        let (trash, rx) = shell_trash();
        assert!(
            trash.has_recycle_bin(&path),
            "no Recycle Bin for {}",
            dir.display()
        );
        let mut d = Deleter::new(Box::new(trash));
        let step = d.delete(&path, &capture(&path).unwrap());
        assert_eq!(step, DeleteStep::Pending { committed: None });
        assert!(path.exists());
        d.commit_all(Duration::from_secs(30));
        let (reported, r) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!((reported, r), (path.clone(), Ok(())));
        assert!(!path.exists());
        fs::remove_dir(&dir).unwrap();
    }

    /// A path longer than 260 characters is not offered to the Recycle Bin (the user is
    /// asked before a permanent delete instead). Handed to the shell anyway through its
    /// short (8.3) form, the shell would delete it for good; `RecycleOnly` stops that and
    /// the file stays. Manual run as for `recycles_to_the_real_bin`.
    #[test]
    #[ignore = "uses the real Recycle Bin"]
    fn long_paths_are_never_deleted_for_good_by_a_recycle() {
        let dir = real_temp_folder("long");
        let mut deep = dir.clone();
        while deep.as_os_str().len() < 280 {
            deep.push("d".repeat(40));
        }
        fs::create_dir_all(to_extended_path(&deep)).unwrap();
        let path = deep.join(format!("lv-m3-recycle-long-{}.txt", std::process::id()));
        fs::write(to_extended_path(&path), b"light-view long recycle test").unwrap();
        let (trash, _rx) = shell_trash();
        assert!(!trash.has_recycle_bin(&path));
        assert!(on_sta(&path).is_err());
        let short = short_path(&path).expect("8.3 names on the temp drive");
        assert!(short_enough_for_shell(&short));
        let r = on_sta(&short);
        assert_eq!(
            r,
            Err("cannot be recycled here (it would be deleted for good)".into())
        );
        assert!(fs::symlink_metadata(to_extended_path(&path)).is_ok());
        fs::remove_dir_all(to_extended_path(&dir)).unwrap();
    }

    /// `recycle_item` on a new single-threaded COM thread.
    fn on_sta(path: &Path) -> Result<(), String> {
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            // SAFETY: COM for this short-lived thread only; it ends with the thread.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.unwrap();
            recycle_item(&path)
        })
        .join()
        .unwrap()
    }

    /// The short (8.3) form of a long path, without the `\\?\` prefix.
    fn short_path(path: &Path) -> Option<PathBuf> {
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;
        let ext = HSTRING::from(to_extended_path(path).as_os_str());
        // SAFETY: `ext` is NUL-terminated; the first call only asks for the length.
        let need = unsafe { GetShortPathNameW(&ext, None) } as usize;
        if need == 0 {
            return None;
        }
        let mut buf = vec![0u16; need];
        // SAFETY: as above, with a live buffer passed with its length.
        let len = unsafe { GetShortPathNameW(&ext, Some(&mut buf)) } as usize;
        if len == 0 || len >= buf.len() {
            return None;
        }
        let short = String::from_utf16(&buf[..len]).ok()?;
        let short = match short.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => short.strip_prefix(r"\\?\")?.to_owned(),
        };
        (short.encode_utf16().count() < MAX_PATH).then(|| PathBuf::from(short))
    }
}
