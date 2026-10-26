//! Main window: class registration, window procedure, theme and DPI.

use std::cell::RefCell;
use std::path::PathBuf;

use anyhow::Context;
use lv_core::settings::{load_settings, save_settings, Settings};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    DragAcceptFiles, FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IsZoomed, LoadCursorW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW,
    SetWindowPlacement, SetWindowPos, TranslateMessage, CREATESTRUCTW, CS_DBLCLKS, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, MINMAXINFO, MSG, SWP_NOACTIVATE,
    SWP_NOZORDER, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};

use crate::commands::{self, HwndCell};
use crate::fullscreen::Fullscreen;
use crate::input::{self, Pointer};
use crate::placement;
use crate::render::{Align, Renderer, TextRun};
use crate::theme::{self, palette, system_prefers_dark, Palette};
use crate::toolbar::{self, pinned_bar_height, ToolbarInput, ToolbarView};
use crate::tooltip;
use crate::viewer::{Scene, Viewer};

const CLASS_NAME: PCWSTR = w!("goodflow.LightView.Window");
const LOADING: &str = "불러오는 중…";
const LOADING_SIZE_DIP: f32 = 13.0;
const PILL_HEIGHT_DIP: f32 = 28.0;
const PILL_PAD_DIP: f32 = 14.0;

pub struct App {
    renderer: Option<Renderer>,
    pub viewer: Viewer,
    /// (generation, frame) of the image last uploaded to the renderer.
    uploaded: Option<(u64, usize)>,
    pub fullscreen: Option<Fullscreen>,
    dark: bool,
    pub settings: Settings,
    settings_path: Option<PathBuf>,
    /// True while the saved placement is being applied at startup.
    restoring: bool,
    /// Client area size in DIPs.
    client_dips: (f32, f32),
    /// DIPs per device pixel.
    pub dip: f32,
    icon_family: &'static str,
    pub pointer: Pointer,
}

pub fn run(initial: Option<PathBuf>) -> anyhow::Result<()> {
    let settings_path = settings_path();
    let settings = settings_path
        .as_deref()
        .map(load_settings)
        .unwrap_or_default();
    let hwnd_cell = HwndCell::default();
    let app = RefCell::new(App {
        renderer: None,
        viewer: commands::new_viewer(&settings, hwnd_cell.clone()),
        uploaded: None,
        fullscreen: None,
        dark: system_prefers_dark(),
        settings,
        settings_path,
        restoring: false,
        client_dips: (0.0, 0.0),
        dip: 1.0,
        icon_family: "",
        pointer: Pointer::default(),
    });

    // SAFETY: Win32 calls with valid arguments; `app` outlives the window because the
    // message loop below runs until the window is destroyed, and the window procedure
    // clears its pointer on WM_NCDESTROY.
    unsafe {
        let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
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
        hwnd_cell.set(hwnd);
        DragAcceptFiles(hwnd, true);

        theme::apply_title_bar(hwnd, app.borrow().dark);
        let tooltip = tooltip::create(hwnd);
        if let Some(tt) = tooltip {
            let dark = app.borrow().dark;
            tooltip::set_dark(tt, dark);
        }
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
            a.icon_family = toolbar::icon_family(&renderer);
            a.renderer = Some(renderer);
            a.pointer.set_tooltip(tooltip);
        }
        // Scans the folder and queues the decode; the window shows without waiting.
        if let Some(path) = &initial {
            commands::open(&app, hwnd, path);
        }

        let saved = app.borrow().settings.window.clone();
        let wp = placement::initial_placement(saved.as_ref());
        // Restoring onto a monitor with another DPI sends WM_DPICHANGED; the saved
        // rect is already in that monitor's pixels, so it must not be rescaled.
        app.borrow_mut().restoring = true;
        let restored = SetWindowPlacement(hwnd, &wp);
        app.borrow_mut().restoring = false;
        if let Err(e) = restored {
            let _ = DestroyWindow(hwnd);
            return Err(e).context("SetWindowPlacement");
        }
        // Size the image area for the final (possibly maximized) size, not the
        // CW_USEDEFAULT size seen during creation.
        {
            let mut a = app.borrow_mut();
            a.on_size(hwnd);
            a.refresh_toolbar();
        }
        input::sync_tooltips(&app, hwnd);
        let _ = InvalidateRect(hwnd, None, false);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

impl App {
    /// Uploads the shown image (or animation frame) if it changed since the last paint,
    /// and (re)starts the animation timer for a newly shown photo. Done at paint time
    /// so that a burst of key repeats uploads only the photo that ends up on screen.
    fn upload_image(&mut self, hwnd: HWND) {
        let key = (
            self.viewer.generation(),
            self.viewer.frame_to_upload().map_or(0, |f| f.1),
        );
        if self.uploaded == Some(key) {
            return;
        }
        let new_photo = self.uploaded.map(|k| k.0) != Some(key.0);
        self.uploaded = Some(key);
        let Some(r) = &mut self.renderer else { return };
        match self.viewer.frame_to_upload() {
            Some((img, frame)) => {
                r.set_image(Some(img), frame);
                if !r.has_image() {
                    // Larger than the GPU's maximum bitmap size; release the pixel copy.
                    r.set_image(None, 0);
                    self.viewer.too_large_to_draw();
                }
            }
            None => r.set_image(None, 0),
        }
        if new_photo {
            commands::set_animation_timer(hwnd, self.viewer.frame_delay());
        }
    }

    pub fn on_size(&mut self, hwnd: HWND) {
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
        self.dip = dip;
        self.client_dips = (pw as f32 * dip, ph as f32 * dip);
        let view = self.view_dips();
        self.viewer.set_view(view, dip);
    }

    /// The image area: the client area above a pinned toolbar.
    fn view_dips(&self) -> (f32, f32) {
        let (w, h) = self.client_dips;
        let bar = if self.settings.toolbar_pinned {
            pinned_bar_height()
        } else {
            0.0
        };
        (w, (h - bar).max(0.0))
    }

    /// The toolbar's layout input, or `None` when no file is open (no toolbar).
    pub fn toolbar_input(&self) -> Option<ToolbarInput> {
        if !self.viewer.has_file() {
            return None;
        }
        Some(ToolbarInput {
            view_w: self.client_dips.0,
            view_h: self.client_dips.1,
            pinned: self.settings.toolbar_pinned,
            unsaved_rotation: self.viewer.unsaved_rotation(),
        })
    }

    /// Pins or unpins the toolbar, saves the choice and resizes the image area.
    pub fn toggle_pin(&mut self, hwnd: HWND) {
        self.settings.toolbar_pinned = !self.settings.toolbar_pinned;
        self.persist_settings();
        self.on_size(hwnd);
    }

    fn paint(&mut self, hwnd: HWND) {
        self.upload_image(hwnd);
        let toolbar = self.toolbar_input().filter(|_| self.pointer.shown);
        let info = self
            .viewer
            .info()
            .map(|(w, h, bytes)| toolbar::info_text(w, h, bytes))
            .unwrap_or_default();
        let zoom = self
            .viewer
            .zoom()
            .map(toolbar::zoom_text)
            .unwrap_or_default();
        let (view_w, view_h) = self.view_dips();
        let Some(r) = &mut self.renderer else { return };
        let p = palette(self.dark);
        r.begin(p.paper);
        let loading = self.viewer.loading();
        match self.viewer.scene() {
            // The previous photo stays up while the next one loads.
            Scene::Image { rect, quarters } => r.draw_image(rect, quarters),
            Scene::Message(text) if !loading => r.draw_text_center(text, p.ink2),
            Scene::Message(_) | Scene::Empty => {}
        }
        if loading {
            draw_loading_pill(r, p, self.dark, (view_w, view_h));
        }
        if let Some(input) = toolbar {
            let view = ToolbarView {
                input,
                palette: p,
                dark: self.dark,
                icon_family: self.icon_family,
                position: self.viewer.position(),
                info: (&info, &zoom),
            };
            toolbar::draw(r, &view);
        }
        if let Err(e) = r.end() {
            eprintln!("light-view: EndDraw failed: {e}");
        }
    }

    /// Saves the normal-window placement (the one from before fullscreen, if active).
    fn save_placement(&mut self, hwnd: HWND) {
        let wp = match &self.fullscreen {
            Some(f) => f.placement_to_save(),
            None => placement::current_placement(hwnd),
        };
        let Some(wp) = wp else {
            return;
        };
        self.settings.window = Some(wp);
        self.persist_settings();
    }

    fn persist_settings(&self) {
        if let Some(path) = &self.settings_path {
            if let Err(e) = save_settings(path, &self.settings) {
                eprintln!("light-view: could not save settings: {e}");
            }
        }
    }
}

/// A small "불러오는 중…" pill centred in the image area: surface fill, plus a
/// hairline border in dark mode (no shadows there).
fn draw_loading_pill(r: &mut Renderer, p: &Palette, dark: bool, (view_w, view_h): (f32, f32)) {
    let run = TextRun {
        text: LOADING,
        family: "Segoe UI",
        size: LOADING_SIZE_DIP,
        bold: false,
        color: p.ink2,
    };
    let text_w = r.measure_runs(&[run]).unwrap_or(80.0);
    let (w, h) = (text_w + 2.0 * PILL_PAD_DIP, PILL_HEIGHT_DIP);
    let rect = ((view_w - w) / 2.0, (view_h - h) / 2.0, w, h);
    r.fill_round_rect(rect, h / 2.0, p.surface);
    if dark {
        r.stroke_round_rect(rect, h / 2.0, p.hairline, 1.0);
    }
    r.draw_runs(rect, &[run], Align::Center);
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
        WM_GETMINMAXINFO => {
            if let Some(width) = placement::min_window_width(hwnd) {
                // SAFETY: for WM_GETMINMAXINFO, lparam points to a writable MINMAXINFO.
                let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                mmi.ptMinTrackSize.x = mmi.ptMinTrackSize.x.max(width);
            }
            Some(LRESULT(0))
        }
        WM_SIZE => {
            app.try_borrow_mut().ok()?.on_size(hwnd);
            input::sync_tooltips(app, hwnd);
            Some(LRESULT(0))
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: paired BeginPaint/EndPaint on our window with a live PAINTSTRUCT.
            unsafe { BeginPaint(hwnd, &mut ps) };
            if let Ok(mut a) = app.try_borrow_mut() {
                a.paint(hwnd);
            }
            // SAFETY: as above.
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let dpi = f32::from((wparam.0 & 0xFFFF) as u16);
            let restoring = {
                let mut a = app.try_borrow_mut().ok()?;
                if let Some(r) = &mut a.renderer {
                    r.set_dpi(dpi);
                }
                a.restoring
            };
            // SAFETY: plain call on our window.
            let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
            if placement::should_apply_suggested_rect(restoring, maximized) {
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
            }
            // The pixel size may not change, so pass the new DPI to the viewer explicitly.
            app.try_borrow_mut().ok()?.on_size(hwnd);
            input::sync_tooltips(app, hwnd);
            invalidate(hwnd);
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            if theme::is_color_set_change(lparam) {
                let dark = system_prefers_dark();
                let tooltip = {
                    let mut a = app.try_borrow_mut().ok()?;
                    a.dark = dark;
                    a.pointer.tooltip()
                };
                theme::apply_title_bar(hwnd, dark);
                if let Some(tt) = tooltip {
                    tooltip::set_dark(tt, dark);
                }
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
        _ => commands::handle(app, hwnd, msg, wparam, lparam)
            .or_else(|| input::handle(app, hwnd, msg, wparam, lparam)),
    }
}

pub fn invalidate(hwnd: HWND) {
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
