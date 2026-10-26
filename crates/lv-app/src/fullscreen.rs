//! Borderless fullscreen covering the window's monitor.

use std::cell::RefCell;

use lv_core::settings::WindowPlacement;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowPlacement, SetWindowLongPtrW, SetWindowPlacement, SetWindowPos,
    GWL_STYLE, HWND_TOP, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    WINDOWPLACEMENT, WS_OVERLAPPEDWINDOW, WS_POPUP,
};

use crate::placement;
use crate::window::App;

/// The window state from before fullscreen, restored on exit.
pub struct Fullscreen {
    style: isize,
    placement: WINDOWPLACEMENT,
    /// What to save as the window placement if the window closes while fullscreen.
    saved: Option<WindowPlacement>,
}

impl Fullscreen {
    pub fn placement_to_save(&self) -> Option<WindowPlacement> {
        self.saved.clone()
    }
}

/// Enters or leaves fullscreen. No App borrow is held while the window changes.
pub fn toggle(app: &RefCell<App>, hwnd: HWND) {
    let Ok(previous) = app.try_borrow_mut().map(|mut a| a.fullscreen.take()) else {
        return;
    };
    match previous {
        Some(f) => leave(hwnd, &f),
        None => enter(app, hwnd),
    }
}

fn enter(app: &RefCell<App>, hwnd: HWND) {
    let mut wp = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `wp` and `mi` are live locals with their sizes set; plain calls on our window.
    let ok = unsafe {
        GetWindowPlacement(hwnd, &mut wp).is_ok()
            && GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool()
    };
    if !ok {
        return;
    }
    // SAFETY: plain call on our window.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    let saved = placement::current_placement(hwnd);
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    a.fullscreen = Some(Fullscreen {
        style,
        placement: wp,
        saved,
    });
    drop(a);
    let r = mi.rcMonitor;
    let popup = (style & !(WS_OVERLAPPEDWINDOW.0 as isize)) | WS_POPUP.0 as isize;
    // SAFETY: plain calls on our window; no App borrow is held (WM_SIZE re-enters).
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, popup);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }
}

fn leave(hwnd: HWND, f: &Fullscreen) {
    // SAFETY: plain calls on our window with the state saved by `enter`; no App borrow
    // is held (WM_SIZE re-enters).
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, f.style);
        let _ = SetWindowPlacement(hwnd, &f.placement);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }
}
