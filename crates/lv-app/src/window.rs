//! Main window: class registration, window procedure, theme, DPI and placement.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::Context;
use lv_core::decode::{decode_file, DecodeError, DecodeLimits, DecodedImage};
use lv_core::settings::{load_settings, save_settings, Settings, WindowPlacement};
use lv_core::viewport::Viewport;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MonitorFromRect,
    MONITORINFO, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Shell::{FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, GetWindowPlacement, IsZoomed, LoadCursorW, PostQuitMessage,
    RegisterClassExW, SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, TranslateMessage,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, MSG,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWNORMAL,
    WINDOWPLACEMENT, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WNDCLASSEXW,
    WPF_RESTORETOMAXIMIZED, WS_OVERLAPPEDWINDOW,
};

use crate::render::Renderer;
use crate::theme::{palette, system_prefers_dark};

const CLASS_NAME: PCWSTR = w!("goodflow.LightView.Window");
const DEFAULT_SIZE_DIP: (i32, i32) = (1200, 800);
const CANNOT_OPEN: &str = "이 사진은 열 수 없어요";

/// A window rectangle in pixels: x, y, width, height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PxRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

enum Content {
    Empty,
    /// `view` is built once the client size is known (and rebuilt on DPI change).
    Image {
        img: DecodedImage,
        view: Option<Viewport>,
    },
    Message(String),
}

struct App {
    renderer: Option<Renderer>,
    content: Content,
    dark: bool,
    settings: Settings,
    settings_path: Option<PathBuf>,
}

pub fn run(initial: Option<PathBuf>) -> anyhow::Result<()> {
    let settings_path = settings_path();
    let settings = settings_path
        .as_deref()
        .map(load_settings)
        .unwrap_or_default();
    let app = RefCell::new(App {
        renderer: None,
        content: initial.as_deref().map_or(Content::Empty, load_content),
        dark: system_prefers_dark(),
        settings,
        settings_path,
    });

    // SAFETY: Win32 calls with valid arguments; `app` outlives the window because the
    // message loop below runs until the window is destroyed, and the window procedure
    // clears its pointer on WM_NCDESTROY.
    unsafe {
        let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).context("LoadCursorW")?,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(windows::core::Error::from_win32()).context("RegisterClassExW");
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("light-view"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            instance,
            Some(std::ptr::from_ref(&app).cast()),
        )
        .context("CreateWindowExW")?;

        apply_title_bar(hwnd, app.borrow().dark);
        // On failure, destroy the window before `app` goes away (no borrow is held).
        let renderer = match Renderer::new(hwnd) {
            Ok(r) => r,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                return Err(e).context("Direct2D renderer");
            }
        };
        {
            let mut a = app.borrow_mut();
            a.renderer = Some(renderer);
            a.upload_image();
        }

        let saved = app.borrow().settings.window.clone();
        let wp = initial_placement(saved.as_ref());
        if let Err(e) = SetWindowPlacement(hwnd, &wp) {
            let _ = DestroyWindow(hwnd);
            return Err(e).context("SetWindowPlacement");
        }
        // Build the viewport for the final (possibly maximized) size, not the
        // CW_USEDEFAULT size seen during creation.
        {
            let mut a = app.borrow_mut();
            a.reset_view();
            a.on_size(hwnd);
        }
        let _ = InvalidateRect(hwnd, None, false);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn load_content(path: &Path) -> Content {
    match decode_file(path, &DecodeLimits::default()) {
        Ok(img) if !img.frames.is_empty() => Content::Image { img, view: None },
        Ok(_) => Content::Message(CANNOT_OPEN.to_owned()),
        Err(e) => Content::Message(error_message(&e)),
    }
}

fn error_message(e: &DecodeError) -> String {
    match e {
        DecodeError::TooLarge { width, height } => {
            format!("너무 커서 열 수 없어요 ({width}×{height})")
        }
        _ => CANNOT_OPEN.to_owned(),
    }
}

impl App {
    fn upload_image(&mut self) {
        let Some(r) = &mut self.renderer else { return };
        if let Content::Image { img, .. } = &self.content {
            r.set_image(Some(img), 0);
            if !r.has_image() {
                // e.g. larger than the GPU's maximum bitmap size
                self.content = Content::Message(CANNOT_OPEN.to_owned());
            }
        } else {
            r.set_image(None, 0);
        }
    }

    fn reset_view(&mut self) {
        if let Content::Image { view, .. } = &mut self.content {
            *view = None;
        }
    }

    fn on_size(&mut self, hwnd: HWND) {
        let mut rc = RECT::default();
        // SAFETY: `rc` is a live local and `hwnd` is our window.
        if unsafe { GetClientRect(hwnd, &mut rc) }.is_err() {
            return;
        }
        let (pw, ph) = ((rc.right - rc.left).max(0), (rc.bottom - rc.top).max(0));
        if let Some(r) = &mut self.renderer {
            r.resize(pw as u32, ph as u32);
        }
        let dip = dip_per_px(hwnd);
        let view_dips = (pw as f32 * dip, ph as f32 * dip);
        if let Content::Image { img, view } = &mut self.content {
            match view {
                Some(v) => v.resize(view_dips),
                None => {
                    // Image pixels map 1:1 to device pixels at scale 1.0 ("100%").
                    let image_dips = (img.width as f32 * dip, img.height as f32 * dip);
                    *view = Some(Viewport::initial(view_dips, image_dips));
                }
            }
        }
    }

    fn paint(&mut self) {
        let Some(r) = &mut self.renderer else { return };
        let p = palette(self.dark);
        r.begin(p.paper);
        match &self.content {
            Content::Image { view: Some(v), .. } => r.draw_image(v.image_rect()),
            Content::Message(text) => r.draw_text_center(text, p.ink2),
            Content::Image { view: None, .. } | Content::Empty => {}
        }
        if let Err(e) = r.end() {
            eprintln!("light-view: EndDraw failed: {e}");
        }
    }

    fn save_placement(&mut self, hwnd: HWND) {
        let mut wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        // SAFETY: `wp` is a live local with `length` set.
        if unsafe { GetWindowPlacement(hwnd, &mut wp) }.is_err() {
            return;
        }
        let r = wp.rcNormalPosition;
        // SAFETY: `hwnd` is our live window.
        let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
        let min_from_max =
            wp.showCmd == SW_SHOWMINIMIZED.0 as u32 && wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0;
        self.settings.window = Some(WindowPlacement {
            x: r.left,
            y: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
            maximized: zoomed || min_from_max,
        });
        if let Some(path) = &self.settings_path {
            if let Err(e) = save_settings(path, &self.settings) {
                eprintln!("light-view: could not save settings: {e}");
            }
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE, lparam points to the CREATESTRUCTW whose
        // lpCreateParams is the `&RefCell<App>` passed to CreateWindowExW in `run`.
        unsafe {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
    }
    // SAFETY: GWLP_USERDATA is either 0 or the `RefCell<App>` pointer stored above,
    // which stays valid until WM_NCDESTROY clears it (see `run`).
    let app = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>).as_ref() };
    let Some(app) = app else {
        // SAFETY: default handling for messages before WM_NCCREATE / after WM_NCDESTROY.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match handle(app, hwnd, msg, wparam, lparam) {
        Some(result) => result,
        // SAFETY: default handling with the arguments we were given.
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Returns `None` to fall through to `DefWindowProcW`. No `App` borrow is held across
/// calls that may re-enter the window procedure (SetWindowPos, DestroyWindow).
fn handle(
    app: &RefCell<App>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    match msg {
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_SIZE => {
            app.try_borrow_mut().ok()?.on_size(hwnd);
            Some(LRESULT(0))
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: paired BeginPaint/EndPaint on our window with a live PAINTSTRUCT.
            unsafe { BeginPaint(hwnd, &mut ps) };
            if let Ok(mut a) = app.try_borrow_mut() {
                a.paint();
            }
            // SAFETY: as above.
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let dpi = f32::from((wparam.0 & 0xFFFF) as u16);
            {
                let mut a = app.try_borrow_mut().ok()?;
                if let Some(r) = &mut a.renderer {
                    r.set_dpi(dpi);
                }
                a.reset_view();
            }
            // SAFETY: for WM_DPICHANGED, lparam points to the suggested window RECT.
            let rc = unsafe { *(lparam.0 as *const RECT) };
            // SAFETY: plain call on our window; no App borrow is held (WM_SIZE re-enters).
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            // The pixel size may not change, so rebuild the viewport explicitly.
            app.try_borrow_mut().ok()?.on_size(hwnd);
            invalidate(hwnd);
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            if is_color_set_change(lparam) {
                let dark = system_prefers_dark();
                app.try_borrow_mut().ok()?.dark = dark;
                apply_title_bar(hwnd, dark);
                invalidate(hwnd);
            }
            None
        }
        WM_CLOSE => {
            app.try_borrow_mut().ok()?.save_placement(hwnd);
            // SAFETY: destroying our own window; no App borrow is held.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            // SAFETY: plain call.
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        WM_NCDESTROY => {
            // SAFETY: clears the App pointer so no later message can reach it.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            None
        }
        _ => None,
    }
}

fn is_color_set_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: for WM_SETTINGCHANGE a non-null lparam is a NUL-terminated wide string.
    let name = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
    name.is_ok_and(|s| s == "ImmersiveColorSet")
}

fn apply_title_bar(hwnd: HWND, dark: bool) {
    let value = BOOL::from(dark);
    // SAFETY: `value` is a live BOOL and the size passed matches it.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            std::ptr::from_ref(&value).cast(),
            std::mem::size_of::<BOOL>() as u32,
        );
    }
}

fn invalidate(hwnd: HWND) {
    // SAFETY: plain call on our window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn dip_per_px(hwnd: HWND) -> f32 {
    // SAFETY: plain call on our window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        96.0 / dpi as f32
    }
}

/// `%APPDATA%\light-view\settings.toml`, or `None` if the folder cannot be resolved.
fn settings_path() -> Option<PathBuf> {
    // SAFETY: on success the returned PWSTR is a NUL-terminated string we own and free
    // with CoTaskMemFree after copying it.
    let dir = unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        s.ok()?
    };
    Some(PathBuf::from(dir).join("light-view").join("settings.toml"))
}

/// Builds the WINDOWPLACEMENT to restore: the saved one when it is still on a
/// connected monitor, otherwise centered on the primary monitor.
fn initial_placement(saved: Option<&WindowPlacement>) -> WINDOWPLACEMENT {
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
    fn too_large_error_names_the_size() {
        let e = DecodeError::TooLarge {
            width: 30000,
            height: 20000,
        };
        assert_eq!(error_message(&e), "너무 커서 열 수 없어요 (30000×20000)");
        assert_eq!(error_message(&DecodeError::Unsupported), CANNOT_OPEN);
    }
}
