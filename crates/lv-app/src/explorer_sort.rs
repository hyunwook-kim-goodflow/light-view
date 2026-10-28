//! Which order Explorer shows a folder in (ruling R5): the sort column of the front-most
//! Explorer window on that folder. Only name, date modified, size and type are
//! followed; anything else, or no such window, is natural order. Asked once when a
//! folder is opened, on a thread of its own (Explorer may be slow or busy), and the
//! answer re-sorts the folder when it comes.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use lv_core::explorer_sort::{SortKey, SortSpec};
use lv_core::listing::same_path;
use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_DateModified, PKEY_ItemNameDisplay, PKEY_ItemTypeText, PKEY_Size,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, IServiceProvider, CLSCTX_LOCAL_SERVER,
};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::Win32::UI::Shell::{
    IFolderView2, IPersistFolder2, IShellBrowser, IShellWindows, SHGetNameFromIDList,
    SID_STopLevelBrowser, ShellWindows, SIGDN_FILESYSPATH, SORTCOLUMN, SORT_DESCENDING,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetTopWindow, GetWindow, GA_ROOT, GW_HWNDNEXT,
};

use crate::commands::HwndCell;
use crate::shell::{user_path, ComApartment};
use crate::viewer::SortSource;

/// Posted when an answer is waiting in `ExplorerSort`; carries no data.
pub const WM_APP_EXPLORER_SORT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 12;
/// How many top-level windows are looked through for the z-order (a guard against a
/// window list that keeps changing under the walk).
const MAX_WINDOWS: usize = 10_000;

/// The order Explorer's sort column `column` stands for.
pub fn spec_for_column(column: &SORTCOLUMN) -> SortSpec {
    let key = column.propkey;
    let same = |k: &PROPERTYKEY| k.fmtid == key.fmtid && k.pid == key.pid;
    let key = if same(&PKEY_ItemNameDisplay) {
        SortKey::Name
    } else if same(&PKEY_DateModified) {
        SortKey::Modified
    } else if same(&PKEY_Size) {
        SortKey::Size
    } else if same(&PKEY_ItemTypeText) {
        SortKey::Kind
    } else {
        return SortSpec::NATURAL;
    };
    SortSpec {
        key,
        descending: column.direction == SORT_DESCENDING,
    }
}

/// The order of the front-most Explorer window showing `dir`; `None` when no Explorer
/// window shows it or it cannot be asked. Needs COM on this thread; blocks while
/// Explorer answers.
pub fn explorer_sort_for(dir: &Path) -> Option<SortSpec> {
    let dir = user_path(dir);
    // SAFETY: COM calls on interfaces we hold for the whole block; the ID list and the
    // name are freed exactly once with CoTaskMemFree.
    unsafe {
        let windows: IShellWindows =
            CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;
        let count = windows.Count().ok()?;
        let order = z_order();
        let mut best: Option<(usize, SortSpec)> = None;
        for i in 0..count {
            let Some((hwnd, view)) = window_on(&windows, i, &dir) else {
                continue;
            };
            let rank = order.iter().position(|&h| h == hwnd).unwrap_or(usize::MAX);
            if best.is_some_and(|(r, _)| r <= rank) {
                continue;
            }
            let Some(spec) = first_sort_column(&view) else {
                continue;
            };
            best = Some((rank, spec));
        }
        best.map(|(_, spec)| spec)
    }
}

/// Explorer window `i` if it shows `dir`: its top-level window and its folder view.
unsafe fn window_on(windows: &IShellWindows, i: i32, dir: &Path) -> Option<(HWND, IFolderView2)> {
    let item = windows.Item(&VARIANT::from(i)).ok()?;
    let services: IServiceProvider = item.cast().ok()?;
    let browser: IShellBrowser = services.QueryService(&SID_STopLevelBrowser).ok()?;
    let view: IFolderView2 = browser.QueryActiveShellView().ok()?.cast().ok()?;
    let folder: IPersistFolder2 = view.GetFolder().ok()?;
    let pidl = folder.GetCurFolder().ok()?;
    let name = SHGetNameFromIDList(pidl, SIGDN_FILESYSPATH);
    CoTaskMemFree(Some(pidl as *const _));
    let name = name.ok()?;
    let shown = PathBuf::from(OsString::from_wide(name.as_wide()));
    CoTaskMemFree(Some(name.0 as *const _));
    if !same_path(&shown, dir) {
        return None;
    }
    let hwnd = GetAncestor(browser.GetWindow().ok()?, GA_ROOT);
    Some((hwnd, view))
}

unsafe fn first_sort_column(view: &IFolderView2) -> Option<SortSpec> {
    let n = view.GetSortColumnCount().ok()?;
    if n < 1 {
        return Some(SortSpec::NATURAL);
    }
    let mut columns = vec![SORTCOLUMN::default(); n as usize];
    view.GetSortColumns(&mut columns).ok()?;
    Some(spec_for_column(&columns[0]))
}

/// The top-level windows, front-most first.
fn z_order() -> Vec<HWND> {
    let mut out = Vec::new();
    // SAFETY: plain window-list reads; a window that goes away just ends the walk.
    let mut h = unsafe { GetTopWindow(None) }.ok();
    while let Some(hwnd) = h {
        if out.len() >= MAX_WINDOWS {
            break;
        }
        out.push(hwnd);
        h = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.ok();
    }
    out
}

/// Asks Explorer on a thread per question and posts `WM_APP_EXPLORER_SORT` when an
/// answer is waiting.
pub struct ExplorerSort {
    tx: Sender<(PathBuf, SortSpec)>,
    rx: Receiver<(PathBuf, SortSpec)>,
    hwnd: HwndCell,
}

impl ExplorerSort {
    pub fn new(hwnd: HwndCell) -> ExplorerSort {
        let (tx, rx) = channel();
        ExplorerSort { tx, rx, hwnd }
    }
}

impl SortSource for ExplorerSort {
    fn ask(&self, dir: &Path) {
        let (dir, tx, hwnd) = (dir.to_path_buf(), self.tx.clone(), self.hwnd.clone());
        std::thread::spawn(move || {
            let _com = ComApartment::init();
            let started = std::time::Instant::now();
            let spec = explorer_sort_for(&dir);
            crate::timing::add("explorer-sort-ms", started.elapsed().as_secs_f64() * 1000.0);
            // No Explorer window on it: the natural order it already has.
            if let Some(spec) = spec {
                if tx.send((dir, spec)).is_ok() {
                    hwnd.post(WM_APP_EXPLORER_SORT);
                }
            }
        });
    }

    fn answers(&self) -> Vec<(PathBuf, SortSpec)> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::GUID;
    use windows::Win32::UI::Shell::SORT_ASCENDING;

    fn column(key: PROPERTYKEY, descending: bool) -> SORTCOLUMN {
        SORTCOLUMN {
            propkey: key,
            direction: if descending {
                SORT_DESCENDING
            } else {
                SORT_ASCENDING
            },
        }
    }

    #[test]
    fn explorer_columns_map_to_orders() {
        let cases = [
            (PKEY_ItemNameDisplay, SortKey::Name),
            (PKEY_DateModified, SortKey::Modified),
            (PKEY_Size, SortKey::Size),
            (PKEY_ItemTypeText, SortKey::Kind),
        ];
        for (key, want) in cases {
            for descending in [false, true] {
                assert_eq!(
                    spec_for_column(&column(key, descending)),
                    SortSpec {
                        key: want,
                        descending
                    }
                );
            }
        }
    }

    #[test]
    fn unknown_key_falls_back_to_natural() {
        // "Date taken" (System.Photo.DateTaken) and a made-up key.
        let date_taken = PROPERTYKEY {
            fmtid: GUID::from_u128(0x14b81da1_0135_4d31_96d9_6cbfc9671a99),
            pid: 36867,
        };
        let other = PROPERTYKEY {
            fmtid: PKEY_Size.fmtid,
            pid: 99,
        };
        for key in [date_taken, other] {
            assert_eq!(spec_for_column(&column(key, true)), SortSpec::NATURAL);
        }
    }

    /// Against the real Explorer windows on this PC: prints what each open folder
    /// answers and how long it took. Run with
    /// `cargo test -p lv-app -- --ignored --nocapture explorer_windows_answer`.
    #[test]
    #[ignore = "reads the Explorer windows open on this PC"]
    fn explorer_windows_answer() {
        let _com = ComApartment::init();
        for dir in [std::env::temp_dir(), PathBuf::from(r"C:\Windows")] {
            let started = std::time::Instant::now();
            let spec = explorer_sort_for(&dir);
            println!("{}: {spec:?} in {:?}", dir.display(), started.elapsed());
        }
    }
}
