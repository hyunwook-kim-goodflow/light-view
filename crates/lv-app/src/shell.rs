//! Handing things to the Windows shell: showing a file in Explorer, opening a file or a
//! URI with its default program, and the user-facing form of a path.

use std::ffi::{OsStr, OsString};
use std::marker::PhantomData;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `path` as people write it: without the `\\?\` (or `\\?\UNC\`) prefix of an
/// extended-length path. Other `\\?\` forms (volume GUIDs) and device paths stay.
pub fn user_path(path: &Path) -> PathBuf {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix = |p: &str| {
        let p: Vec<u16> = p.encode_utf16().collect();
        wide.starts_with(&p).then_some(p.len())
    };
    let rest = if let Some(n) = prefix(r"\\?\UNC\") {
        // \\?\UNC\server\share -> \\server\share
        let mut out: Vec<u16> = r"\\".encode_utf16().collect();
        out.extend_from_slice(&wide[n..]);
        out
    } else if let Some(n) = prefix(r"\\?\") {
        // Only drive paths (\\?\C:\...) have a user form.
        let letter = wide
            .get(n)
            .is_some_and(|&c| u8::try_from(c).is_ok_and(|c| c.is_ascii_alphabetic()));
        let drive = letter && wide.get(n + 1) == Some(&u16::from(b':'));
        if !drive {
            return path.to_path_buf();
        }
        wide[n..].to_vec()
    } else {
        return path.to_path_buf();
    };
    PathBuf::from(OsString::from_wide(&rest))
}

/// COM on this thread as a single-threaded apartment, as the shell's functions and
/// dialogs need; uninitialized when dropped. The UI thread makes one at start-up
/// (`main`) and holds it until the window is gone, so the calls below need no setup of
/// their own.
pub struct ComApartment {
    initialized: bool,
    /// Not `Send`: it must end on the thread it started on.
    _thread: PhantomData<*const ()>,
}

impl ComApartment {
    pub fn init() -> ComApartment {
        // SAFETY: initializes COM for this thread; balanced in `drop` only when it
        // succeeded (S_OK or S_FALSE).
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if let Err(e) = hr.ok() {
            eprintln!("light-view: COM did not start on this thread: {e}");
        }
        ComApartment {
            initialized: hr.is_ok(),
            _thread: PhantomData,
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: pairs with the successful CoInitializeEx in `init`, on this thread
            // (the guard is not Send).
            unsafe { CoUninitialize() };
        }
    }
}

/// Opens an Explorer window on `path`'s folder with `path` selected.
pub fn show_in_folder(path: &Path) -> windows::core::Result<()> {
    let name = HSTRING::from(user_path(path).as_os_str());
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: on the UI thread, whose COM apartment `main` holds; `name` outlives the
    // calls; `pidl` is a live local that receives an ID list we own, freed exactly once
    // with CoTaskMemFree.
    unsafe {
        SHParseDisplayName(&name, None, &mut pidl, 0, None)?;
        let shown = SHOpenFolderAndSelectItems(pidl, None, 0);
        CoTaskMemFree(Some(pidl as *const _));
        shown
    }
}

/// Opens `target` (a file or a URI such as `ms-settings:defaultapps`) with its default
/// program. False if the shell could not.
pub fn open(hwnd: HWND, target: &OsStr) -> bool {
    let target = HSTRING::from(target);
    // SAFETY: on the UI thread, whose COM apartment `main` holds; the strings outlive
    // the call. Values above 32 mean success.
    let result = unsafe { ShellExecuteW(hwnd, w!("open"), &target, None, None, SW_SHOWNORMAL) };
    let ok = result.0 as isize > 32;
    if !ok {
        eprintln!(
            "light-view: could not open {}: {}",
            target, result.0 as isize
        );
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
    use windows::Win32::System::Com::COINIT_MULTITHREADED;

    /// The UI thread's apartment is single-threaded while the guard lives, and ends with
    /// it.
    #[test]
    fn the_apartment_is_single_threaded_until_dropped() {
        std::thread::spawn(|| {
            let com = ComApartment::init();
            // SAFETY: plain calls on this test thread; each successful init is paired
            // with an uninit.
            unsafe {
                // Already an STA: another STA init only counts up...
                assert_eq!(CoInitializeEx(None, COINIT_APARTMENTTHREADED), S_FALSE);
                CoUninitialize();
                // ...and the multithreaded model is refused.
                assert_eq!(
                    CoInitializeEx(None, COINIT_MULTITHREADED),
                    RPC_E_CHANGED_MODE
                );
            }
            drop(com);
            // SAFETY: as above.
            unsafe {
                assert_eq!(CoInitializeEx(None, COINIT_MULTITHREADED), S_OK);
                CoUninitialize();
            }
        })
        .join()
        .unwrap();
    }

    fn up(s: &str) -> String {
        user_path(Path::new(s)).to_string_lossy().into_owned()
    }

    #[test]
    fn user_path_drops_the_extended_prefix() {
        assert_eq!(up(r"\\?\C:\사진\a.jpg"), r"C:\사진\a.jpg");
        assert_eq!(up(r"\\?\UNC\server\share\a.jpg"), r"\\server\share\a.jpg");
        // Already in user form.
        assert_eq!(up(r"C:\photos\a.jpg"), r"C:\photos\a.jpg");
        assert_eq!(up(r"\\server\share\a.jpg"), r"\\server\share\a.jpg");
        // Volume GUID paths and device paths have no user form: kept.
        let guid = r"\\?\Volume{12345678-1234-1234-1234-123456789abc}\a.jpg";
        assert_eq!(up(guid), guid);
        assert_eq!(up(r"\\.\C:\a.jpg"), r"\\.\C:\a.jpg");
        // A long path keeps everything after the prefix.
        let long = format!(r"\\?\D:\{}\b.png", "x".repeat(300));
        assert_eq!(up(&long), &long[4..]);
    }
}
