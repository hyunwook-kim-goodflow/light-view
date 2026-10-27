//! The Windows Recycle Bin: a [`Trash`](super::Trash) that recycles pending deletes with
//! `IFileOperation` on its own STA thread.

use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use lv_core::identity::to_extended_path;
use windows::core::{implement, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::E_ABORT;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
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

use super::{commit, CommitError, PendingDelete, Trash};

/// The shell takes no `\\?\` paths (E_INVALIDARG), and asked to delete a longer path
/// through its short (8.3) form it deletes for good: the Recycle Bin cannot hold it. So
/// only paths shorter than this (UTF-16 units) are recycled.
pub(super) const MAX_PATH: usize = 260;

/// Whether the shell can recycle `path` as given (the form `recycle_item` passes to it):
/// shorter than `MAX_PATH` UTF-16 units, as Windows counts (not bytes: Hangul is three
/// bytes in an `OsStr` but one unit).
pub(super) fn short_enough_for_shell(path: &Path) -> bool {
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
pub(super) fn recycle_item(path: &Path) -> Result<(), String> {
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
