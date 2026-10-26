//! Main window: class registration, window procedure, theme and DPI.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::Context;
use lv_core::command::Command;
use lv_core::decode::{decode_file, DecodeError, DecodeLimits, DecodedImage};
use lv_core::settings::{load_settings, save_settings, Settings};
use lv_core::viewport::{Viewport, ZOOM_STEP};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IsZoomed, LoadCursorW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW,
    SetWindowPlacement, SetWindowPos, SetWindowTextW, TranslateMessage, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, MSG, SWP_NOACTIVATE, SWP_NOZORDER,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};

use crate::input::{self, Pointer};
use crate::placement;
use crate::render::Renderer;
use crate::theme::{palette, system_prefers_dark};
use crate::toolbar::{self, pinned_bar_height, ToolbarInput, ToolbarView};
use crate::tooltip;

const CLASS_NAME: PCWSTR = w!("goodflow.LightView.Window");
const CANNOT_OPEN: &str = "이 사진은 열 수 없어요";

enum Content {
    Empty,
    /// `view` is built once the client size is known (and rebuilt on DPI change).
    Image {
        img: DecodedImage,
        view: Option<Viewport>,
    },
    Message(String),
}

pub struct App {
    renderer: Option<Renderer>,
    content: Content,
    /// Size of the opened file in bytes, for the pinned bar's info text.
    file_bytes: Option<u64>,
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
    let title = initial
        .as_deref()
        .and_then(Path::file_name)
        .map_or_else(|| "light-view".into(), |n| n.to_string_lossy().into_owned());
    let app = RefCell::new(App {
        renderer: None,
        content: initial.as_deref().map_or(Content::Empty, load_content),
        file_bytes: initial
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len()),
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
        // Task 10 appends the position: "IMG_4821.HEIC · 12 / 128".
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
        let tooltip = tooltip::create(hwnd);
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
            a.upload_image();
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
        // Build the viewport for the final (possibly maximized) size, not the
        // CW_USEDEFAULT size seen during creation.
        {
            let mut a = app.borrow_mut();
            a.reset_view();
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
                // Larger than the GPU's maximum bitmap size; release the pixel copy.
                r.set_image(None, 0);
                let (width, height) = (img.width, img.height);
                self.content =
                    Content::Message(error_message(&DecodeError::TooLarge { width, height }));
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
        self.dip = dip;
        self.client_dips = (pw as f32 * dip, ph as f32 * dip);
        let view_dips = self.view_dips();
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
        if matches!(self.content, Content::Empty) {
            return None;
        }
        Some(ToolbarInput {
            view_w: self.client_dips.0,
            view_h: self.client_dips.1,
            pinned: self.settings.toolbar_pinned,
            unsaved_rotation: false, // Task 10 tracks rotation
        })
    }

    pub fn on_command(&mut self, command: Command) {
        match command {
            Command::ZoomIn => self.zoom(ZOOM_STEP),
            Command::ZoomOut => self.zoom(1.0 / ZOOM_STEP),
            // TODO(Task 10): navigate to the previous / next photo in the folder.
            Command::Prev | Command::Next => {}
            _ => {}
        }
    }

    /// Zooms about the centre of the image area.
    fn zoom(&mut self, factor: f32) {
        let (w, h) = self.view_dips();
        if let Content::Image { view: Some(v), .. } = &mut self.content {
            v.zoom_at(factor, (w / 2.0, h / 2.0));
        }
    }

    /// Pins or unpins the toolbar, saves the choice and resizes the image area.
    pub fn toggle_pin(&mut self, hwnd: HWND) {
        self.settings.toolbar_pinned = !self.settings.toolbar_pinned;
        self.persist_settings();
        self.on_size(hwnd);
    }

    fn paint(&mut self) {
        let toolbar = self.toolbar_input().filter(|_| self.pointer.shown);
        let (info, zoom) = match &self.content {
            Content::Image { img, view } => (
                self.file_bytes
                    .map(|b| toolbar::info_text(img.width, img.height, b))
                    .unwrap_or_default(),
                view.as_ref()
                    .map(|v| toolbar::zoom_text(v.scale()))
                    .unwrap_or_default(),
            ),
            _ => (String::new(), String::new()),
        };
        let Some(r) = &mut self.renderer else { return };
        let p = palette(self.dark);
        r.begin(p.paper);
        match &self.content {
            Content::Image { view: Some(v), .. } => r.draw_image(v.image_rect()),
            Content::Message(text) => r.draw_text_center(text, p.ink2),
            Content::Image { view: None, .. } | Content::Empty => {}
        }
        if let Some(input) = toolbar {
            let view = ToolbarView {
                input,
                palette: p,
                dark: self.dark,
                icon_family: self.icon_family,
                position: (1, 1), // Task 10 supplies the folder position
                info: (&info, &zoom),
            };
            toolbar::draw(r, &view);
        }
        if let Err(e) = r.end() {
            eprintln!("light-view: EndDraw failed: {e}");
        }
    }

    fn save_placement(&mut self, hwnd: HWND) {
        let Some(wp) = placement::current_placement(hwnd) else {
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
            input::sync_tooltips(app, hwnd);
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
            let restoring = {
                let mut a = app.try_borrow_mut().ok()?;
                if let Some(r) = &mut a.renderer {
                    r.set_dpi(dpi);
                }
                a.reset_view();
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
            // The pixel size may not change, so rebuild the viewport explicitly.
            app.try_borrow_mut().ok()?.on_size(hwnd);
            input::sync_tooltips(app, hwnd);
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
        _ => input::handle(app, hwnd, msg, wparam, lparam),
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
    let paper = palette(dark).paper;
    // COLORREF is 0x00BBGGRR.
    let caption = COLORREF(u32::from(paper.0) | u32::from(paper.1) << 8 | u32::from(paper.2) << 16);
    // SAFETY: `value` and `caption` are live locals and each size passed matches its
    // value. DWMWA_CAPTION_COLOR fails on Windows 10; that error is ignored.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            std::ptr::from_ref(&value).cast(),
            std::mem::size_of::<BOOL>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            std::ptr::from_ref(&caption).cast(),
            std::mem::size_of::<COLORREF>() as u32,
        );
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

#[cfg(test)]
mod tests {
    use super::*;

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
