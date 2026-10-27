//! Viewer commands from keys, the wheel and the toolbar, plus the messages that drive
//! the viewer: worker wake-ups, the loading and animation timers, and dropped files.
//! Also the rotation save: every way of leaving a photo goes through [`leave`], which
//! asks (or saves) first when the view rotation is unsaved, and saves run on their own
//! thread ([`start_save`]) and report back with `WM_APP_SAVE_DONE`.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lv_core::command::{command_for_key, Command, Mods};
use lv_core::decode::DecodeLimits;
use lv_core::listing::first_supported_in;
use lv_core::prefetch::Prefetcher;
use lv_core::settings::Settings;
use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, PostMessageW, SetTimer, SetWindowTextW, WM_APP, WM_CLOSE, WM_DROPFILES,
    WM_ENDSESSION, WM_KEYDOWN, WM_SYSKEYDOWN, WM_TIMER,
};

use crate::about;
use crate::delete;
use crate::fullscreen;
use crate::input;
use crate::overlay::{OverlayEvent, Prompt, PromptResult, Toast, ToastAction};
use crate::routing::RoutingDecoder;
use crate::start;
use crate::strings::S;
use crate::update;
use crate::viewer::{LeaveReason, Redraw, Viewer};
use crate::watch::{self, WM_APP_FOLDER_CHANGED, WM_APP_RESCAN_DONE};
use crate::window::{invalidate, App};
use crate::worker::WorkerPool;

mod save_flow;

pub use save_flow::{leave, request_close, SaveDone, Saving};
use save_flow::{on_save_done, on_save_prompt, start_save};

/// Posted by prefetch workers when a decode finished; carries no data.
pub const WM_APP_IMAGE_READY: u32 = WM_APP + 1;
/// Posted by a save thread once its `SaveDone` is in `App::save_rx`.
pub const WM_APP_SAVE_DONE: u32 = WM_APP + 2;
/// Posted by the recycle thread once a report is in `App::delete_rx`.
pub const WM_APP_DELETE_DONE: u32 = WM_APP + 3;
const ANIMATION_TIMER: usize = 2;
const LOADING_TIMER: usize = 3;
/// One-shot: a save still running after `LOADING_DELAY_MS` shows "저장하는 중…".
const SAVING_TIMER: usize = 5;
/// How long a running save may hold up the end of a Windows session.
const END_SESSION_WAIT: Duration = Duration::from_secs(5);
/// How long the previous photo stays up before "불러오는 중…" replaces it.
const LOADING_DELAY_MS: u32 = 150;
const PREFETCH_WORKERS: usize = 2;
const MAX_DEFAULT_BUDGET: u64 = 2 << 30;
/// Used when the physical memory size cannot be read.
const FALLBACK_BUDGET: u64 = 512 << 20;

/// The window handle, shared with prefetch workers so they can post wake-ups.
#[derive(Clone, Default)]
pub struct HwndCell(Arc<AtomicIsize>);

impl HwndCell {
    pub fn set(&self, hwnd: HWND) {
        self.0.store(hwnd.0 as isize, Ordering::Release);
    }

    /// Posts `msg` to the window, once there is one.
    pub fn post(&self, msg: u32) {
        let h = self.0.load(Ordering::Acquire);
        if h != 0 {
            // SAFETY: plain call; posting to a destroyed window just fails.
            unsafe {
                let _ = PostMessageW(HWND(h as *mut _), msg, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// The viewer with its prefetcher, the worker pool and the decode limits (also used to
/// check saved files). HEIF workers (one per prefetch thread, ruling R2) start here,
/// before the window shows; a missing or failing worker only affects HEIF and JPEG pixel
/// rotation.
pub fn new_viewer(
    settings: &Settings,
    hwnd: HwndCell,
) -> (Viewer, Option<Arc<WorkerPool>>, DecodeLimits) {
    let budget = memory_budget(settings);
    let limits = DecodeLimits {
        max_pixels: max_pixels_for_budget(budget),
    };
    let decoder = crate::timing::step("pool-ms", || {
        RoutingDecoder::start(limits, PREFETCH_WORKERS)
    });
    let pool = decoder.pool();
    let prefetcher = Prefetcher::new(decoder, PREFETCH_WORKERS, budget, move |_path| {
        hwnd.post(WM_APP_IMAGE_READY);
    });
    let mut viewer = Viewer::new(prefetcher);
    viewer.set_auto_save(settings.auto_save_rotation);
    (viewer, pool, limits)
}

/// The decode pixel limit for a prefetch memory budget. Decoding peaks at about twice
/// the RGBA size (conversion or rotation copies) on each of `PREFETCH_WORKERS` (two)
/// workers, so one image may use `budget / 8` pixels, capped at the decoder default.
fn max_pixels_for_budget(budget: u64) -> u64 {
    (budget / 8).min(DecodeLimits::default().max_pixels)
}

/// `memory_budget_mb` if set and not 0, else a quarter of physical memory, at most 2 GiB.
fn memory_budget(settings: &Settings) -> u64 {
    if let Some(mb) = settings.memory_budget_mb.filter(|&mb| mb > 0) {
        return u64::from(mb) << 20;
    }
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a live local with `dwLength` set.
    match unsafe { GlobalMemoryStatusEx(&mut status) } {
        Ok(()) => (status.ullTotalPhys / 4).min(MAX_DEFAULT_BUDGET),
        Err(_) => FALLBACK_BUDGET,
    }
}

/// Opens a file, or the first supported file of a folder. Nothing is moved or deleted.
/// Every open (start-up, drops, forwarded launches) goes through the save prompt.
pub fn open(app: &RefCell<App>, hwnd: HWND, path: &Path) {
    if let Some(file) = file_to_open(path) {
        leave(app, hwnd, LeaveReason::Open(file));
    }
}

/// `path` itself, or for a folder its first supported file (`None` if it has none).
pub fn file_to_open(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        first_supported_in(path).ok().flatten()
    } else {
        Some(path.to_path_buf())
    }
}

/// Runs a command. `anchor` (view DIPs) is the zoom centre for pointer-driven zooms.
pub fn run(app: &RefCell<App>, hwnd: HWND, cmd: Command, anchor: Option<(f32, f32)>) {
    match cmd {
        Command::Next | Command::Prev => leave(app, hwnd, LeaveReason::Move(cmd)),
        // Ctrl+S and the Save button save without asking.
        Command::Save => start_save(app, hwnd),
        Command::Delete => delete::request(app, hwnd),
        Command::Undo => delete::request_undo(app, hwnd),
        // The rotation being saved must not change under the save.
        Command::RotateLeft | Command::RotateRight
            if app.try_borrow().map_or(true, |a| a.saving.is_some()) => {}
        Command::ToggleFullscreen => fullscreen::toggle(app, hwnd),
        Command::Open => start::open_dialog(app, hwnd),
        Command::Settings => open_settings(app, hwnd),
        Command::ToggleExif => {
            let Ok(mut a) = app.try_borrow_mut() else {
                return;
            };
            if a.viewer.has_file() || a.exif_card.is_open() {
                a.exif_card.toggle();
                drop(a);
                invalidate(hwnd);
            }
        }
        Command::Escape => {
            let Ok(full) = app.try_borrow().map(|a| a.fullscreen.is_some()) else {
                return;
            };
            if full {
                fullscreen::toggle(app, hwnd);
            } else {
                // Through WM_CLOSE so the placement is saved.
                // SAFETY: plain call on our window.
                unsafe {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        _ => {
            let Ok(mut a) = app.try_borrow_mut() else {
                return;
            };
            let redraw = match anchor {
                Some(p) => a.viewer.apply_at(cmd, p),
                None => a.viewer.apply(cmd),
            };
            drop(a);
            after(app, hwnd, redraw);
        }
    }
}

/// After the viewer changed: title, toolbar tools (Save comes and goes), the loading
/// and animation timers and a repaint.
pub fn after(app: &RefCell<App>, hwnd: HWND, redraw: Redraw) {
    if redraw == Redraw::None {
        return;
    }
    let Ok((title, pending, rearm)) = app.try_borrow_mut().map(|mut a| {
        let rearm = a
            .viewer
            .take_animation_rearm()
            .then(|| a.viewer.frame_delay());
        (a.viewer.title(), a.viewer.pending(), rearm)
    }) else {
        return;
    };
    if let Some(delay) = rearm {
        set_animation_timer(hwnd, delay);
    }
    // SAFETY: plain calls on our window; no App borrow is held (WM_SETTEXT re-enters).
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
        if pending {
            // Restarted on every move, so holding a key never flashes the loading text.
            SetTimer(hwnd, LOADING_TIMER, LOADING_DELAY_MS, None);
        }
    }
    // The file may have come or gone (e.g. opened from the start screen): a pinned
    // toolbar shows at once, not only after the next mouse move.
    input::refresh_toolbar_now(app, hwnd);
    input::sync_tooltips(app, hwnd);
    invalidate(hwnd);
    watch::follow_folder(app, hwnd);
}

/// Acts on a closed dialog or a used toast action. Returns false when the answer closes
/// the window (now, or once its save ends): a path forwarded while the dialog was open
/// must then not be opened.
pub fn on_overlay_event(app: &RefCell<App>, hwnd: HWND, event: OverlayEvent) -> bool {
    match event {
        OverlayEvent::Dialog {
            prompt: Prompt::SaveRotation,
            result,
            dont_ask,
            target,
        } => on_save_prompt(app, hwnd, result, dont_ask, &target),
        OverlayEvent::Dialog {
            prompt: Prompt::PermanentDelete { .. },
            result,
            target,
            ..
        } => {
            delete::on_permanent_answer(app, hwnd, result, &target);
            true
        }
        OverlayEvent::Dialog {
            prompt: Prompt::About,
            result,
            ..
        } => {
            if result == PromptResult::Notices {
                about::open_notices(app, hwnd);
            }
            true
        }
        OverlayEvent::Dialog {
            prompt: Prompt::UpdateCheck,
            result,
            ..
        } => {
            update::on_answer(app, hwnd, result);
            true
        }
        OverlayEvent::Toast(ToastAction::Undo) => {
            delete::request_undo(app, hwnd);
            true
        }
        OverlayEvent::Toast(ToastAction::ViewUpdate) => {
            update::open_release(app, hwnd);
            true
        }
    }
}

/// Ctrl+, and the menu's 설정: the settings window.
pub fn open_settings(app: &RefCell<App>, hwnd: HWND) {
    crate::settings_win::open(app, hwnd);
}

/// Starts the animation timer for `delay_ms`, or stops it.
pub fn set_animation_timer(hwnd: HWND, delay_ms: Option<u32>) {
    // SAFETY: plain calls on our window's timer; neither dispatches messages.
    unsafe {
        match delay_ms {
            Some(ms) => {
                SetTimer(hwnd, ANIMATION_TIMER, ms, None);
            }
            None => {
                let _ = KillTimer(hwnd, ANIMATION_TIMER);
            }
        }
    }
}

/// Keyboard, wake-up, timer and drop messages. Returns `None` for others.
pub fn handle(
    app: &RefCell<App>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    match msg {
        // Alt combinations map to nothing and fall through to DefWindowProc.
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let cmd = command_for_key(wparam.0 as u16, mods())?;
            let repeat = lparam.0 & (1 << 30) != 0;
            let repeats = matches!(
                cmd,
                Command::Next | Command::Prev | Command::ZoomIn | Command::ZoomOut
            );
            if !repeat || repeats {
                run(app, hwnd, cmd, None);
            }
            Some(LRESULT(0))
        }
        WM_APP_IMAGE_READY => {
            let redraw = app.try_borrow_mut().ok()?.viewer.check_current();
            after(app, hwnd, redraw);
            // The first photo is up: time for the update question (once).
            update::maybe_ask(app, hwnd);
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == LOADING_TIMER => {
            // SAFETY: plain call on our window's timer (one-shot).
            unsafe {
                let _ = KillTimer(hwnd, LOADING_TIMER);
            }
            if app.try_borrow_mut().ok()?.viewer.loading_overdue() == Redraw::Full {
                invalidate(hwnd);
            }
            Some(LRESULT(0))
        }
        WM_APP_DELETE_DONE => {
            delete::on_reports(app, hwnd);
            Some(LRESULT(0))
        }
        WM_APP_RESCAN_DONE => {
            watch::on_rescan_done(app, hwnd);
            Some(LRESULT(0))
        }
        WM_APP_FOLDER_CHANGED => {
            watch::on_changed(app, hwnd);
            Some(LRESULT(0))
        }
        WM_APP_SAVE_DONE => {
            loop {
                let done = app.try_borrow().ok()?.save_rx.try_recv();
                let Ok(done) = done else { break };
                on_save_done(app, hwnd, done);
            }
            Some(LRESULT(0))
        }
        // Windows is ending the session: the process may be ended once this returns, so
        // a running save gets up to `END_SESSION_WAIT` to finish its replace (blocking
        // the shutdown with WM_QUERYENDSESSION would be worse), and so does sending the
        // pending delete to the Recycle Bin.
        WM_ENDSESSION if wparam.0 != 0 => {
            let mut a = app.try_borrow_mut().ok()?;
            if a.saving.is_some() {
                let _ = a.save_rx.recv_timeout(END_SESSION_WAIT);
            }
            a.deleter.commit_all(END_SESSION_WAIT);
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == SAVING_TIMER => {
            // SAFETY: plain call on our window's timer (one-shot).
            unsafe {
                let _ = KillTimer(hwnd, SAVING_TIMER);
            }
            if let Some(s) = &mut app.try_borrow_mut().ok()?.saving {
                s.overdue = true;
                invalidate(hwnd);
            }
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == ANIMATION_TIMER => {
            let delay = app.try_borrow_mut().ok()?.viewer.advance_frame();
            set_animation_timer(hwnd, delay);
            if delay.is_some() {
                invalidate(hwnd);
            }
            Some(LRESULT(0))
        }
        WM_DROPFILES => {
            let dropped = dropped_paths(HDROP(wparam.0 as *mut _));
            match start::pick_drop(&dropped) {
                Some(path) => open(app, hwnd, &path),
                None => input::show_toast(app, hwnd, |now| Toast::new(S::ToastNoPhotos, now)),
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain call.
    unsafe { GetKeyState(i32::from(vk.0)) < 0 }
}

pub fn mods() -> Mods {
    Mods {
        ctrl: key_down(VK_CONTROL),
        shift: key_down(VK_SHIFT),
        alt: key_down(VK_MENU),
    }
}

/// The dropped paths, in order; releases the drop handle.
fn dropped_paths(drop: HDROP) -> Vec<PathBuf> {
    // SAFETY: `drop` is the HDROP from WM_DROPFILES, released exactly once below;
    // `buf` is a live buffer whose length is passed with it.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(drop, i, Some(&mut buf));
            if got > 0 {
                paths.push(PathBuf::from(OsString::from_wide(&buf[..got as usize])));
            }
        }
        DragFinish(drop);
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_pixels_follow_the_memory_budget() {
        // Peak ≈ 2 × RGBA (conversion/rotation) on each of two workers: budget / 8.
        assert_eq!(max_pixels_for_budget(512 << 20), (512 << 20) / 8);
        assert_eq!(max_pixels_for_budget(2 << 30), 268_435_456);
        // Never above the decoder's own ceiling, however large the budget.
        assert_eq!(max_pixels_for_budget(64 << 30), 268_435_456);
        assert_eq!(max_pixels_for_budget(0), 0);
    }

    #[test]
    fn zero_budget_uses_default() {
        let zero = Settings {
            memory_budget_mb: Some(0),
            ..Settings::default()
        };
        let default = memory_budget(&Settings::default());
        assert!(default > 0);
        assert_eq!(memory_budget(&zero), default);
        let set = Settings {
            memory_budget_mb: Some(512),
            ..Settings::default()
        };
        assert_eq!(memory_budget(&set), 512 << 20);
    }
}
