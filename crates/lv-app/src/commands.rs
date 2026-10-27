//! Viewer commands from keys, the wheel and the toolbar, plus the messages that drive
//! the viewer: worker wake-ups, the loading and animation timers, and dropped files.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;

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
    KillTimer, PostMessageW, SetTimer, SetWindowTextW, WM_APP, WM_CLOSE, WM_DROPFILES, WM_KEYDOWN,
    WM_SYSKEYDOWN, WM_TIMER,
};

use crate::fullscreen;
use crate::input;
use crate::overlay::OverlayEvent;
use crate::routing::RoutingDecoder;
use crate::viewer::{Redraw, Viewer};
use crate::window::{invalidate, App};

/// Posted by prefetch workers when a decode finished; carries no data.
pub const WM_APP_IMAGE_READY: u32 = WM_APP + 1;
const ANIMATION_TIMER: usize = 2;
const LOADING_TIMER: usize = 3;
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
}

/// The viewer with its prefetcher. HEIF workers (one per prefetch thread, ruling R2) start
/// here, before the window shows; a missing or failing worker only affects HEIF.
pub fn new_viewer(settings: &Settings, hwnd: HwndCell) -> Viewer {
    let budget = memory_budget(settings);
    let limits = DecodeLimits {
        max_pixels: max_pixels_for_budget(budget),
    };
    let decoder = RoutingDecoder::start(limits, PREFETCH_WORKERS);
    let prefetcher = Prefetcher::new(decoder, PREFETCH_WORKERS, budget, move |_path| {
        let h = hwnd.0.load(Ordering::Acquire);
        if h != 0 {
            // SAFETY: plain call; posting to a destroyed window just fails.
            unsafe {
                let _ = PostMessageW(HWND(h as *mut _), WM_APP_IMAGE_READY, WPARAM(0), LPARAM(0));
            }
        }
    });
    Viewer::new(prefetcher)
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
pub fn open(app: &RefCell<App>, hwnd: HWND, path: &Path) {
    let file = if path.is_dir() {
        match first_supported_in(path) {
            Ok(Some(file)) => file,
            _ => return,
        }
    } else {
        path.to_path_buf()
    };
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    a.viewer.open(&file);
    drop(a);
    after(app, hwnd, Redraw::Full);
}

/// Runs a command. `anchor` (view DIPs) is the zoom centre for pointer-driven zooms.
pub fn run(app: &RefCell<App>, hwnd: HWND, cmd: Command, anchor: Option<(f32, f32)>) {
    match cmd {
        Command::ToggleFullscreen => fullscreen::toggle(app, hwnd),
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
fn after(app: &RefCell<App>, hwnd: HWND, redraw: Redraw) {
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
    input::sync_tooltips(app, hwnd);
    invalidate(hwnd);
}

/// Acts on a closed dialog or a used toast action. Nothing shows them for real yet:
/// the save prompt arrives in Task 6, the delete undo and permanent-delete
/// confirmation in Task 7.
pub fn on_overlay_event(_app: &RefCell<App>, _hwnd: HWND, event: OverlayEvent) {
    if cfg!(debug_assertions) {
        eprintln!("light-view: overlay {event:?}");
    }
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
        WM_TIMER if wparam.0 == ANIMATION_TIMER => {
            let delay = app.try_borrow_mut().ok()?.viewer.advance_frame();
            set_animation_timer(hwnd, delay);
            if delay.is_some() {
                invalidate(hwnd);
            }
            Some(LRESULT(0))
        }
        WM_DROPFILES => {
            if let Some(path) = first_dropped(HDROP(wparam.0 as *mut _)) {
                open(app, hwnd, &path);
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

/// The first dropped path; releases the drop handle.
fn first_dropped(drop: HDROP) -> Option<PathBuf> {
    // SAFETY: `drop` is the HDROP from WM_DROPFILES, released exactly once below;
    // `buf` is a live buffer whose length is passed with it.
    unsafe {
        let len = DragQueryFileW(drop, 0, None);
        let mut buf = vec![0u16; len as usize + 1];
        let got = DragQueryFileW(drop, 0, Some(&mut buf));
        DragFinish(drop);
        (got > 0).then(|| PathBuf::from(OsString::from_wide(&buf[..got as usize])))
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
