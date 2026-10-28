//! Main window: class registration, window procedure, theme and DPI.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use lv_core::decode::DecodeLimits;
use lv_core::portable::DataDir;
use lv_core::settings::{update_settings, Settings};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, IsZoomed, PostQuitMessage,
    SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW, GWLP_USERDATA, MINMAXINFO, SWP_NOACTIVATE,
    SWP_NOZORDER, WM_CLOSE, WM_COPYDATA, WM_DESTROY, WM_DEVICECHANGE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_GETOBJECT, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETTINGCHANGE, WM_SIZE,
};

use crate::commands::{self, SaveDone, Saving};
use crate::delete::{CommitReport, Deleter};
use crate::exif_card::{self, CardInfo, ExifCard};
use crate::fullscreen::Fullscreen;
use crate::input::{self, Pointer};
use crate::instance;
use crate::menu::Menu;
use crate::overlay::Overlay;
use crate::placement;
use crate::render::{self, Renderer};
use crate::resize::ResizeState;
use crate::settings_win::{self, SettingsWindow};
use crate::start::{self, StartScreen};
use crate::theme::{self, system_prefers_dark};
use crate::toolbar::{pinned_bar_height, ToolbarInput};
use crate::tooltip;
use crate::uia;
use crate::uia_provider;
use crate::update::{self, UpdateState};
use crate::viewer::Viewer;
use crate::watch::{self, Watching};
use crate::worker::WorkerPool;

mod paint;
mod run;

pub use run::run;

pub const CLASS_NAME: PCWSTR = w!("goodflow.LightView.Window");

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
    pub overlay: Overlay,
    /// The EXIF card (spec §4.6), drawn under the overlay.
    pub exif_card: ExifCard,
    /// The right-click menu, drawn over the card and under the overlay.
    pub menu: Menu,
    /// The empty start screen's [열기] button.
    pub start: StartScreen,
    /// The HEIF decode and JPEG rotation workers, shared with the prefetcher.
    pub pool: Option<Arc<WorkerPool>>,
    /// The prefetcher's decode limits; a save checks its result within them.
    pub decode_limits: DecodeLimits,
    /// The rotation save running in the background, if any (one at a time).
    pub saving: Option<Saving>,
    /// Save threads report here, then post `WM_APP_SAVE_DONE`.
    pub save_tx: Sender<SaveDone>,
    pub save_rx: Receiver<SaveDone>,
    /// Deletes: one pending (undoable), earlier ones sent to the Recycle Bin.
    pub deleter: Deleter,
    /// Recycle results, announced with `WM_APP_DELETE_DONE`.
    pub delete_rx: Receiver<CommitReport>,
    /// The shown folder's change watch.
    pub watching: Watching,
    /// The settings window, while it is open.
    pub settings_win: Option<SettingsWindow>,
    /// The update question and the release found (spec §6.6).
    pub update: UpdateState,
    /// The resize screen (M6): its session, estimates and save.
    pub resize: ResizeState,
}

impl App {
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

    pub fn dark(&self) -> bool {
        self.dark
    }

    /// Where settings are saved; `None` for a measurement run (`--bench`).
    pub fn settings_file(&self) -> Option<&std::path::Path> {
        self.settings_path.as_deref()
    }

    /// The icon font's family name (empty until the renderer exists).
    pub fn icon_family(&self) -> &'static str {
        self.icon_family
    }

    /// Client area size in DIPs.
    pub fn client(&self) -> (f32, f32) {
        self.client_dips
    }

    /// The image area: the client area above a pinned toolbar; on the resize screen,
    /// its photo area.
    fn view_dips(&self) -> (f32, f32) {
        if let Some(r) = crate::resize::screen::photo_rect(self) {
            return (r.w, r.h);
        }
        let (w, h) = self.client_dips;
        let bar = if self.settings.toolbar_pinned {
            pinned_bar_height()
        } else {
            0.0
        };
        (w, (h - bar).max(0.0))
    }

    /// A point in client DIPs as the viewer sees it (its view starts at the resize
    /// screen's photo area).
    pub fn view_point(&self, (x, y): (f32, f32)) -> (f32, f32) {
        match crate::resize::screen::photo_rect(self) {
            Some(r) => (x - r.x, y - r.y),
            None => (x, y),
        }
    }

    /// The toolbar's layout input, or `None` when no file is open or the resize screen
    /// is (no toolbar).
    pub fn toolbar_input(&self) -> Option<ToolbarInput> {
        if !self.viewer.has_file() || self.resize.is_open() {
            return None;
        }
        Some(ToolbarInput {
            view_w: self.client_dips.0,
            view_h: self.client_dips.1,
            pinned: self.settings.toolbar_pinned,
            unsaved_rotation: self.viewer.unsaved_rotation(),
        })
    }

    /// The open EXIF card's state for the shown photo, or `None` when it is closed (or
    /// no file is open). Moves its selection back to the top when the photo changed.
    pub fn card_model(&mut self) -> Option<exif_card::Model> {
        if self.exif_card.is_open() {
            let file = self.viewer.has_file();
            self.exif_card.sync(file.then(|| self.viewer.generation()));
        }
        self.card_layout()
    }

    /// Like `card_model`, without touching the card's state (for hit tests).
    pub fn card_layout(&self) -> Option<exif_card::Model> {
        if !self.exif_card.is_open() || !self.viewer.has_file() || self.resize.is_open() {
            return None;
        }
        let info = CardInfo {
            exif: self.viewer.exif().cloned(),
            file: self.viewer.info(),
            modified: self.viewer.modified().and_then(exif_card::local_time),
            scaled_from: self.viewer.scaled_from(),
        };
        let pinned = self.toolbar_input().is_some_and(|i| i.pinned);
        let bottom = exif_card::card_bottom(self.client_dips, pinned);
        Some(
            self.exif_card
                .model(info, self.client_dips, bottom, crate::strings::current()),
        )
    }

    /// The empty start screen's layout, shown while no file is open.
    pub fn start_layout(&self) -> Option<start::StartLayout> {
        (!self.viewer.has_file()).then(|| start::start_layout(self.client_dips))
    }

    /// Pins or unpins the toolbar, saves the choice and resizes the image area.
    pub fn toggle_pin(&mut self, hwnd: HWND) {
        let pinned = !self.settings.toolbar_pinned;
        self.persist(|s| s.toolbar_pinned = pinned);
        self.on_size(hwnd);
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
        self.persist(|s| s.window = Some(wp.clone()));
    }

    /// Applies `change` to the settings and saves just that: the file is read again
    /// and only `change` is applied to it, so a change another window (another
    /// process, with "새 창") saved meanwhile is kept. The fields this window does not
    /// apply live are then taken from what was written. An open settings window
    /// repaints to show them. Returns false when the file could not be written (the
    /// change still holds for this run).
    pub fn persist(&mut self, change: impl Fn(&mut Settings)) -> bool {
        change(&mut self.settings);
        let mut saved = true;
        if let Some(path) = &self.settings_path {
            match update_settings(path, &change) {
                Ok(written) => self.settings.sync_from(&written),
                Err(e) => {
                    eprintln!("light-view: could not save settings: {e}");
                    saved = false;
                }
            }
        }
        if let Some(w) = &self.settings_win {
            invalidate(w.hwnd);
        }
        saved
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
            // The resize screen's layout and number fields first (they are windows).
            crate::resize::screen::sync(app, hwnd);
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
            uia_provider::flush(hwnd);
            Some(LRESULT(0))
        }
        WM_GETOBJECT => uia_provider::get_object(hwnd, wparam, lparam, || {
            app.try_borrow().ok().map(|a| uia::main_tree_of(&a, hwnd))
        }),
        render::WM_APP_MIPS_READY => {
            invalidate(hwnd);
            Some(LRESULT(0))
        }
        update::WM_APP_UPDATE => {
            update::on_result(app, hwnd);
            Some(LRESULT(0))
        }
        uia_provider::WM_APP_UIA => {
            if let Some(act) = uia_provider::action(hwnd, wparam) {
                uia::run_main(app, hwnd, act);
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
                settings_win::on_theme(app, dark);
                invalidate(hwnd);
            }
            None
        }
        WM_CLOSE => {
            // An open dialog must be answered first: the close is cancelled. An unsaved
            // rotation asks (or saves) first.
            if !input::dialog_open(app) {
                commands::request_close(app, hwnd);
            }
            Some(LRESULT(0))
        }
        WM_COPYDATA => Some(instance::on_copy_data(app, hwnd, lparam)),
        WM_DEVICECHANGE => watch::on_device_change(app, hwnd, wparam, lparam),
        WM_DESTROY => {
            watch::stop(app);
            uia_provider::detach(hwnd);
            // SAFETY: plain call.
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        WM_NCDESTROY => {
            uia_provider::forget(hwnd);
            // SAFETY: clears the App pointer so no later message can reach it.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            None
        }
        // The overlay sees input first: an open dialog takes it all.
        _ => input::overlay(app, hwnd, msg, wparam, lparam)
            .or_else(|| crate::resize::screen::on_message(app, hwnd, msg, wparam, lparam))
            .or_else(|| commands::handle(app, hwnd, msg, wparam, lparam))
            .or_else(|| input::handle(app, hwnd, msg, wparam, lparam)),
    }
}

/// Closes the window now: saves the placement and destroys it.
pub fn close(app: &RefCell<App>, hwnd: HWND) {
    // From here on, paths forwarded by later launches open their own window.
    instance::mark_closing();
    if let Ok(mut a) = app.try_borrow_mut() {
        a.save_placement(hwnd);
    }
    // SAFETY: destroying our own window; no App borrow is held.
    unsafe {
        let _ = DestroyWindow(hwnd);
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

/// The settings file: `%APPDATA%\light-view\settings.toml`, or `data\settings.toml` beside a
/// portable exe; `None` if the folder cannot be resolved.
pub fn settings_path(data: &DataDir) -> Option<PathBuf> {
    data.settings_file(|| known_folder(&FOLDERID_RoamingAppData))
}

/// The crash record folder (ruling R6): `%LOCALAPPDATA%\light-view\crashes`, or
/// `data\crashes` beside a portable exe.
pub fn crash_dir(data: &DataDir) -> Option<PathBuf> {
    data.crash_dir(|| known_folder(&FOLDERID_LocalAppData))
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    // SAFETY: on success the returned PWSTR is a NUL-terminated string we own and free
    // with CoTaskMemFree after copying it.
    let dir = unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        s.ok()?
    };
    Some(PathBuf::from(dir))
}
