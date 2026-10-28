//! Window placement: save/restore via WINDOWPLACEMENT with an off-monitor fallback.

use lv_core::settings::WindowPlacement;

use crate::toolbar;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONULL,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongW, GetWindowPlacement, IsZoomed, GWL_EXSTYLE, GWL_STYLE, SW_SHOWMAXIMIZED,
    SW_SHOWMINIMIZED, SW_SHOWNORMAL, WINDOWPLACEMENT, WINDOW_EX_STYLE, WINDOW_STYLE,
    WPF_RESTORETOMAXIMIZED,
};

const DEFAULT_SIZE_DIP: (i32, i32) = (1200, 800);

/// A window rectangle in pixels: x, y, width, height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PxRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// The window's normal rect and maximized state, for saving.
pub fn current_placement(hwnd: HWND) -> Option<WindowPlacement> {
    let mut wp = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: `wp` is a live local with `length` set.
    unsafe { GetWindowPlacement(hwnd, &mut wp) }.ok()?;
    let r = wp.rcNormalPosition;
    // SAFETY: plain call on the caller's window.
    let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
    let min_from_max =
        wp.showCmd == SW_SHOWMINIMIZED.0 as u32 && wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0;
    Some(WindowPlacement {
        x: r.left,
        y: r.top,
        width: r.right - r.left,
        height: r.bottom - r.top,
        maximized: zoomed || min_from_max,
    })
}

/// Whether WM_DPICHANGED should move the window to the suggested rect. Not while the
/// saved placement is being restored (it is already in the target monitor's pixels)
/// and not while maximized (the system sizes maximized windows itself).
pub fn should_apply_suggested_rect(restoring: bool, maximized: bool) -> bool {
    !restoring && !maximized
}

/// Builds the WINDOWPLACEMENT to restore: the saved one when it is still on a
/// connected monitor, otherwise centered on the primary monitor.
pub fn initial_placement(saved: Option<&WindowPlacement>) -> WINDOWPLACEMENT {
    let work = primary_work_area();
    // SAFETY: plain call.
    let sys_dpi = unsafe { GetDpiForSystem() }.max(96) as i32;
    let default_size = (
        DEFAULT_SIZE_DIP.0 * sys_dpi / 96,
        DEFAULT_SIZE_DIP.1 * sys_dpi / 96,
    );
    // rcNormalPosition is in workspace coordinates (relative to the primary work
    // area); shift by the work-area origin to test it against screen monitors.
    let on_monitor = |r: PxRect| {
        let screen = RECT {
            left: r.x + work.x,
            top: r.y + work.y,
            right: r.x + work.x + r.w,
            bottom: r.y + work.y + r.h,
        };
        // SAFETY: `screen` is a live local.
        !unsafe { MonitorFromRect(&screen, MONITOR_DEFAULTTONULL) }.is_invalid()
    };
    let rect = restore_rect(saved, on_monitor, default_size, (work.w, work.h));
    WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        showCmd: if saved.is_some_and(|s| s.maximized) {
            SW_SHOWMAXIMIZED.0 as u32
        } else {
            SW_SHOWNORMAL.0 as u32
        },
        rcNormalPosition: RECT {
            left: rect.x,
            top: rect.y,
            right: rect.x + rect.w,
            bottom: rect.y + rect.h,
        },
        ..Default::default()
    }
}

/// Primary monitor work area in screen coordinates.
fn primary_work_area() -> PxRect {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `mi` is a live local with `cbSize` set.
    let ok = unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        GetMonitorInfoW(mon, &mut mi).as_bool()
    };
    let r = if ok {
        mi.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        }
    };
    PxRect {
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
    }
}

/// Pure placement logic, in workspace coordinates (primary work area at 0,0 with size
/// `work`). Keeps a valid saved rect that `on_monitor` accepts; otherwise centers the
/// saved (or default) size on the primary work area, shrunk to fit.
fn restore_rect(
    saved: Option<&WindowPlacement>,
    on_monitor: impl Fn(PxRect) -> bool,
    default_size: (i32, i32),
    work: (i32, i32),
) -> PxRect {
    let saved = saved
        .filter(|s| s.width > 0 && s.height > 0)
        .map(|s| PxRect {
            x: s.x,
            y: s.y,
            w: s.width,
            h: s.height,
        });
    if let Some(r) = saved.filter(|r| on_monitor(*r)) {
        return r;
    }
    let (w, h) = saved.map_or(default_size, |r| (r.w, r.h));
    let (w, h) = (w.min(work.0).max(1), h.min(work.1).max(1));
    PxRect {
        x: (work.0 - w) / 2,
        y: (work.1 - h) / 2,
        w,
        h,
    }
}

/// Outer window width whose client area fits the widest toolbar
/// (`toolbar::min_client_width`) at the window's DPI.
pub fn min_window_width(hwnd: HWND) -> Option<i32> {
    // SAFETY: plain calls on our window; `rc` is a live local.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            return None;
        }
        let client = (toolbar::min_client_width() * dpi as f32 / 96.0).ceil() as i32;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: client,
            bottom: 0,
        };
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let ex_style = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        AdjustWindowRectExForDpi(&mut rc, style, false, ex_style, dpi).ok()?;
        Some(rc.right - rc.left)
    }
}

/// Outer window height whose client area is `client_dip` DIPs tall at the window's DPI.
pub fn window_height_for(hwnd: HWND, client_dip: f32) -> Option<i32> {
    // SAFETY: plain calls on our window; `rc` is a live local.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            return None;
        }
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: (client_dip * dpi as f32 / 96.0).ceil() as i32,
        };
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let ex_style = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        AdjustWindowRectExForDpi(&mut rc, style, false, ex_style, dpi).ok()?;
        Some(rc.bottom - rc.top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: (i32, i32) = (1200, 800);
    const WORK: (i32, i32) = (1920, 1040);

    fn saved(x: i32, y: i32, width: i32, height: i32) -> WindowPlacement {
        WindowPlacement {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PxRect {
        PxRect { x, y, w, h }
    }

    #[test]
    fn keeps_saved_rect_on_a_monitor() {
        let r = restore_rect(Some(&saved(100, 50, 800, 600)), |_| true, DEFAULT, WORK);
        assert_eq!(r, rect(100, 50, 800, 600));
    }

    #[test]
    fn off_monitor_rect_moves_to_primary_center_keeping_size() {
        let r = restore_rect(Some(&saved(5000, 3000, 800, 600)), |_| false, DEFAULT, WORK);
        assert_eq!(r, rect(560, 220, 800, 600));
    }

    #[test]
    fn off_monitor_rect_larger_than_work_area_is_shrunk() {
        let r = restore_rect(Some(&saved(-4000, 0, 3000, 2000)), |_| false, DEFAULT, WORK);
        assert_eq!(r, rect(0, 0, 1920, 1040));
    }

    #[test]
    fn no_or_degenerate_saved_rect_uses_centered_default() {
        let expected = rect(360, 120, 1200, 800);
        assert_eq!(restore_rect(None, |_| true, DEFAULT, WORK), expected);
        let zero = saved(10, 10, 0, 600);
        assert_eq!(restore_rect(Some(&zero), |_| true, DEFAULT, WORK), expected);
    }

    #[test]
    fn suggested_rect_only_outside_restore_and_maximize() {
        assert!(should_apply_suggested_rect(false, false));
        assert!(!should_apply_suggested_rect(true, false));
        assert!(!should_apply_suggested_rect(false, true));
        assert!(!should_apply_suggested_rect(true, true));
    }
}
