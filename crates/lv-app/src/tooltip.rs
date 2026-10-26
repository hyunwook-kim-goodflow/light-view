//! comctl32 tooltip control: one tool per toolbar button rect.

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, TOOLTIPS_CLASSW,
    TTF_SUBCLASS, TTM_ADDTOOLW, TTM_DELTOOLW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, SendMessageW, CW_USEDEFAULT, WINDOW_STYLE, WS_EX_TOPMOST, WS_POPUP,
};

/// A tool: its rect in client pixels and its text.
pub type Tool = (RECT, &'static str);

/// Creates a tooltip control owned by `owner` (destroyed with it), or `None`.
pub fn create(owner: HWND) -> Option<HWND> {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_BAR_CLASSES,
    };
    // SAFETY: `icc` is a live local of the declared size; the window is created with
    // valid arguments and owned by our live window.
    unsafe {
        if !InitCommonControlsEx(&icc).as_bool() {
            return None;
        }
        CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            None,
            WINDOW_STYLE(WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            owner,
            None,
            GetModuleHandleW(None).ok()?,
            None,
        )
        .ok()
    }
}

/// Dark mode (spec §4.1): the dark Explorer visual style, or the default style.
pub fn set_dark(tooltip: HWND, dark: bool) {
    let theme = if dark {
        w!("DarkMode_Explorer")
    } else {
        PCWSTR::null()
    };
    // SAFETY: `tooltip` is our tooltip window; `theme` is static or null.
    unsafe {
        let _ = SetWindowTheme(tooltip, theme, PCWSTR::null());
    }
}

/// Replaces the `old_count` tools (ids `0..old_count`) with `tools` (ids by index).
/// Sends messages to the tooltip, which may notify `owner`: call without an App borrow.
pub fn set_tools(tooltip: HWND, owner: HWND, old_count: usize, tools: &[Tool]) {
    for id in 0..old_count {
        let mut ti = tool_info(owner, id);
        // SAFETY: `ti` is a live, correctly sized TTTOOLINFOW identifying our tool.
        unsafe { send(tooltip, TTM_DELTOOLW, &mut ti) };
    }
    for (id, (rect, text)) in tools.iter().enumerate() {
        let mut wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let mut ti = TTTOOLINFOW {
            uFlags: TTF_SUBCLASS,
            rect: *rect,
            lpszText: PWSTR(wide.as_mut_ptr()),
            ..tool_info(owner, id)
        };
        // SAFETY: `ti` and the NUL-terminated `wide` it points to are live for the
        // call; the control copies the text.
        unsafe { send(tooltip, TTM_ADDTOOLW, &mut ti) };
    }
}

fn tool_info(owner: HWND, id: usize) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
        hwnd: owner,
        uId: id,
        ..Default::default()
    }
}

/// # Safety
/// `ti` must be a valid TTTOOLINFOW for message `msg`.
unsafe fn send(tooltip: HWND, msg: u32, ti: &mut TTTOOLINFOW) {
    SendMessageW(
        tooltip,
        msg,
        WPARAM(0),
        LPARAM(std::ptr::from_mut(ti) as isize),
    );
}
