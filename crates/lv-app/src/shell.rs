//! Handing things to the Windows shell: showing a file in Explorer, opening a file or a
//! URI with its default program, and the user-facing form of a path.

use std::ffi::{OsStr, OsString};
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

/// Runs `f` with COM initialized (single-threaded) on this thread, as the shell needs.
pub fn with_com<R>(f: impl FnOnce() -> R) -> R {
    // SAFETY: initializes COM for this thread; balanced below only when it succeeded
    // (S_OK or S_FALSE). If the thread already runs another model, `f` still runs.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let result = f();
    if com.is_ok() {
        // SAFETY: pairs with the successful CoInitializeEx above, on this thread.
        unsafe { CoUninitialize() };
    }
    result
}

/// Opens an Explorer window on `path`'s folder with `path` selected.
pub fn show_in_folder(path: &Path) -> windows::core::Result<()> {
    let name = HSTRING::from(user_path(path).as_os_str());
    with_com(|| {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: `name` outlives the calls; `pidl` is a live local that receives an
        // ID list we own, freed exactly once with CoTaskMemFree.
        unsafe {
            SHParseDisplayName(&name, None, &mut pidl, 0, None)?;
            let shown = SHOpenFolderAndSelectItems(pidl, None, 0);
            CoTaskMemFree(Some(pidl as *const _));
            shown
        }
    })
}

/// Opens `target` (a file or a URI such as `ms-settings:defaultapps`) with its default
/// program. False if the shell could not.
pub fn open(hwnd: HWND, target: &OsStr) -> bool {
    let target = HSTRING::from(target);
    with_com(|| {
        // SAFETY: plain call; the strings outlive it. Values above 32 mean success.
        let result = unsafe { ShellExecuteW(hwnd, w!("open"), &target, None, None, SW_SHOWNORMAL) };
        let ok = result.0 as isize > 32;
        if !ok {
            eprintln!(
                "light-view: could not open {}: {}",
                target, result.0 as isize
            );
        }
        ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
