//! Putting text on the Windows clipboard (the EXIF list's copy).

use windows::core::Result;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// `CF_UNICODETEXT` (from `Win32_System_Ole`, which this crate does not otherwise need).
const CF_UNICODETEXT: u32 = 13;

/// `text` as NUL-terminated UTF-16, the clipboard's text format.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Replaces the clipboard's contents with `text`, owned by `hwnd`.
pub fn set_text(hwnd: HWND, text: &str) -> Result<()> {
    let data = wide(text);
    let bytes = data.len() * 2;
    // SAFETY: the clipboard is opened and always closed again below; the global block is
    // `bytes` long, written only through its lock and freed unless the clipboard took it.
    unsafe {
        OpenClipboard(hwnd)?;
        let result = (|| {
            EmptyClipboard()?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
            let ptr = GlobalLock(mem).cast::<u16>();
            if ptr.is_null() {
                let _ = GlobalFree(mem);
                return Err(windows::core::Error::from_win32());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            // Returns an "error" with code 0 once the block is unlocked; nothing to do.
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, HANDLE(mem.0)) {
                let _ = GlobalFree(mem);
                return Err(e);
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_nul_terminated_utf16() {
        assert_eq!(wide("가a"), vec![0xAC00, 0x61, 0]);
        assert_eq!(wide(""), vec![0]);
    }
}
