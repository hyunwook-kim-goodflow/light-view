//! Pointer input: toolbar auto-hide, hover, button clicks and tooltips; the wheel,
//! double-click zoom and drag-to-pan on the image. Also routes input to the in-window
//! dialog and toast first (`overlay`).

use std::cell::RefCell;
use std::time::Instant;

use lv_core::command::{command_for_wheel, Command, Mods};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{DragFinish, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, KillTimer, LoadCursorW, SetCursor, SetTimer, HTCLIENT, IDC_HAND, WHEEL_DELTA,
    WM_CAPTURECHANGED, WM_DROPFILES, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SETCURSOR, WM_SYSKEYDOWN, WM_TIMER,
};

use crate::commands;
use crate::overlay::{
    OverlayEvent, Prompt, Toast, TOAST_FILE_CHANGED, TOAST_SAVED_AS_TAG, TOAST_SAVE_FAILED,
    TOAST_UNSUPPORTED_FORMAT,
};
use crate::toolbar::{bar_rect, hit_test, layout, AutoHide, ToolButton};
use crate::tooltip::{self, Tool};
use crate::viewer::Redraw;
use crate::window::{invalidate, App};

const TIMER_ID: usize = 1;
/// One-shot: drops the toast when its six seconds are up.
const TOAST_TIMER: usize = 4;
/// How often visibility is re-evaluated while the floating toolbar is shown.
const TIMER_MS: u32 = 250;

pub struct Pointer {
    epoch: Instant,
    autohide: AutoHide,
    /// Pointer position in DIPs while it is inside the client area.
    cursor: Option<(f32, f32)>,
    /// Last WM_MOUSEMOVE position in pixels, to ignore synthetic repeats.
    last_px: Option<(i32, i32)>,
    tracking_leave: bool,
    timer_on: bool,
    /// Whether the toolbar is currently drawn.
    pub shown: bool,
    pressed: Option<ToolButton>,
    /// Last pointer position in pixels while dragging the image.
    drag: Option<(i32, i32)>,
    /// Wheel movement not yet turned into whole notches.
    wheel: i32,
    tooltip: Option<HWND>,
    tools: Vec<Tool>,
}

impl Default for Pointer {
    fn default() -> Pointer {
        Pointer {
            epoch: Instant::now(),
            autohide: AutoHide::default(),
            cursor: None,
            last_px: None,
            tracking_leave: false,
            timer_on: false,
            shown: false,
            pressed: None,
            drag: None,
            wheel: 0,
            tooltip: None,
            tools: Vec::new(),
        }
    }
}

impl Pointer {
    pub fn set_tooltip(&mut self, tooltip: Option<HWND>) {
        self.tooltip = tooltip;
    }

    pub fn tooltip(&self) -> Option<HWND> {
        self.tooltip
    }

    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
}

impl App {
    /// Re-evaluates toolbar visibility; true if it changed.
    pub fn refresh_toolbar(&mut self) -> bool {
        let now = self.pointer.now_ms();
        let visible = self.toolbar_input().is_some_and(|i| {
            let bar = bar_rect(&i);
            let hovering = self.pointer.cursor.is_some_and(|(x, y)| bar.contains(x, y));
            self.pointer.autohide.visible(now, hovering, i.pinned)
        });
        let changed = visible != self.pointer.shown;
        self.pointer.shown = visible;
        changed
    }

    /// True if the auto-hide timer must be started now (it is then marked running).
    fn start_timer(&mut self) -> bool {
        let start = self.pointer.shown && !self.settings.toolbar_pinned && !self.pointer.timer_on;
        self.pointer.timer_on |= start;
        start
    }

    fn enabled_button_at(&self, lparam: LPARAM) -> Option<ToolButton> {
        if !self.pointer.shown {
            return None;
        }
        let (x, y) = self.to_dips(point(lparam));
        hit_test(&layout(&self.toolbar_input()?), x, y).filter(|b| b.enabled())
    }

    fn to_dips(&self, (x, y): (i32, i32)) -> (f32, f32) {
        (x as f32 * self.dip, y as f32 * self.dip)
    }

    /// Whether `px` (client pixels) is on the toolbar while it is shown.
    fn on_toolbar(&self, px: (i32, i32)) -> bool {
        let (x, y) = self.to_dips(px);
        self.pointer.shown
            && self
                .toolbar_input()
                .is_some_and(|i| bar_rect(&i).contains(x, y))
    }

    /// Whether the hand cursor applies at `px`: dragging, or over a pannable image.
    fn hand_at(&self, px: (i32, i32)) -> bool {
        !self.overlay.dialog_open()
            && (self.pointer.drag.is_some() || (self.viewer.pannable() && !self.on_toolbar(px)))
    }

    /// The tooltip tools for the current layout, if they changed: (tooltip, old count, tools).
    /// None while a dialog is open.
    fn changed_tools(&mut self) -> Option<(HWND, usize, Vec<Tool>)> {
        let px = |v: f32| (v / self.dip).round() as i32;
        let tools: Vec<Tool> = self
            .toolbar_input()
            .filter(|_| !self.overlay.dialog_open())
            .map(|i| layout(&i))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(b, r)| {
                let rect = RECT {
                    left: px(r.x),
                    top: px(r.y),
                    right: px(r.x + r.w),
                    bottom: px(r.y + r.h),
                };
                Some((rect, b.tooltip()?))
            })
            .collect();
        if tools == self.pointer.tools {
            return None;
        }
        let old = std::mem::replace(&mut self.pointer.tools, tools.clone());
        Some((self.pointer.tooltip?, old.len(), tools))
    }
}

/// Brings the tooltip tools in line with the toolbar layout (after size, DPI or pin
/// changes). Takes the App borrow only briefly.
pub fn sync_tooltips(app: &RefCell<App>, hwnd: HWND) {
    let Some((tt, old, tools)) = app
        .try_borrow_mut()
        .ok()
        .and_then(|mut a| a.changed_tools())
    else {
        return;
    };
    tooltip::set_tools(tt, hwnd, old, &tools);
}

/// Mouse and timer messages. Returns `None` for messages it does not handle.
/// No App borrow is held across calls that may dispatch messages.
pub fn handle(
    app: &RefCell<App>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    match msg {
        WM_MOUSEMOVE => {
            let (track, timer, changed) = {
                let mut a = app.try_borrow_mut().ok()?;
                let px = point(lparam);
                if a.pointer.last_px == Some(px) {
                    return Some(LRESULT(0)); // synthetic move without movement
                }
                a.pointer.last_px = Some(px);
                a.pointer.cursor = Some(a.to_dips(px));
                if let Some(from) = a.pointer.drag.replace(px) {
                    let (dx, dy) = a.to_dips((px.0 - from.0, px.1 - from.1));
                    if a.viewer.pan(dx, dy) == Redraw::Full {
                        invalidate(hwnd);
                    }
                }
                let now = a.pointer.now_ms();
                a.pointer.autohide.on_mouse_move(now);
                let track = !a.pointer.tracking_leave;
                a.pointer.tracking_leave = true;
                let changed = a.refresh_toolbar();
                (track, a.start_timer(), changed)
            };
            if track {
                track_leave(hwnd);
            }
            after_change(hwnd, timer, changed);
            Some(LRESULT(0))
        }
        WM_MOUSELEAVE => {
            let mut a = app.try_borrow_mut().ok()?;
            a.pointer.tracking_leave = false;
            a.pointer.cursor = None;
            a.pointer.last_px = None;
            a.pointer.pressed = None;
            // The timer keeps running and hides the toolbar after the timeout.
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == TIMER_ID => {
            let (stop, changed) = {
                let mut a = app.try_borrow_mut().ok()?;
                let changed = a.refresh_toolbar();
                let stop = !a.pointer.shown || a.settings.toolbar_pinned;
                a.pointer.timer_on &= !stop;
                (stop, changed)
            };
            if stop {
                // SAFETY: plain call on our window's timer.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_ID);
                }
            }
            if changed {
                invalidate(hwnd);
            }
            Some(LRESULT(0))
        }
        // A double-click on a button counts as a second click on it.
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (drag, toggle) = {
                let mut a = app.try_borrow_mut().ok()?;
                let px = point(lparam);
                a.pointer.pressed = a.enabled_button_at(lparam);
                // Presses on the toolbar are clicks, never pans or zooms.
                let on_image = a.pointer.pressed.is_none() && !a.on_toolbar(px);
                let drag = on_image && a.viewer.pannable();
                if drag {
                    a.pointer.drag = Some(px);
                }
                let toggle = on_image && msg == WM_LBUTTONDBLCLK;
                (drag, toggle.then(|| a.to_dips(px)))
            };
            if drag {
                // SAFETY: plain call on our window; no App borrow is held.
                unsafe { SetCapture(hwnd) };
            }
            if let Some(anchor) = toggle {
                commands::run(app, hwnd, Command::ToggleFitActual, Some(anchor));
            }
            Some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            let (clicked, dragged) = {
                let mut a = app.try_borrow_mut().ok()?;
                let pressed = a.pointer.pressed.take();
                let dragged = a.pointer.drag.take().is_some();
                let clicked = pressed.filter(|b| a.enabled_button_at(lparam) == Some(*b));
                (clicked, dragged)
            };
            if dragged {
                // SAFETY: plain call; no App borrow is held (WM_CAPTURECHANGED re-enters).
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            if let Some(b) = clicked {
                on_button(app, hwnd, b);
            }
            Some(LRESULT(0))
        }
        WM_CAPTURECHANGED => {
            app.try_borrow_mut().ok()?.pointer.drag = None;
            Some(LRESULT(0))
        }
        WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
            let px = cursor_in_client(hwnd)?;
            if !app.try_borrow().ok()?.hand_at(px) {
                return None;
            }
            // SAFETY: plain calls; IDC_HAND is a system cursor.
            unsafe {
                let cursor = LoadCursorW(None, IDC_HAND).ok()?;
                SetCursor(cursor);
            }
            Some(LRESULT(1))
        }
        WM_MOUSEWHEEL => {
            let mods = commands::mods();
            if mods.alt {
                return Some(LRESULT(0));
            }
            let delta = i32::from(((wparam.0 >> 16) & 0xFFFF) as u16 as i16);
            let (notches, anchor) = {
                let mut a = app.try_borrow_mut().ok()?;
                a.pointer.wheel += delta;
                let notches = a.pointer.wheel / WHEEL_DELTA as i32;
                a.pointer.wheel -= notches * WHEEL_DELTA as i32;
                (notches, cursor_in_client(hwnd).map(|px| a.to_dips(px)))
            };
            let cmd = command_for_wheel(notches.signum() as i16, mods);
            for _ in 0..notches.unsigned_abs() {
                commands::run(app, hwnd, cmd, anchor);
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// Shows a dialog. Until it closes it takes every key, click and wheel turn, and
/// cancels WM_CLOSE; its answer goes to `commands::on_overlay_event`.
pub fn show_dialog(app: &RefCell<App>, hwnd: HWND, prompt: Prompt) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    a.overlay.show_dialog(prompt);
    a.pointer.pressed = None;
    drop(a);
    sync_tooltips(app, hwnd);
    invalidate(hwnd);
}

/// Shows a toast for six seconds, replacing any toast already up. `make` gets the
/// current time, e.g. `show_toast(app, hwnd, Toast::recycled)`.
pub fn show_toast(app: &RefCell<App>, hwnd: HWND, make: impl FnOnce(u64) -> Toast) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let toast = make(a.pointer.now_ms());
    let ms = toast.duration_ms;
    a.overlay.show_toast(toast);
    drop(a);
    // SAFETY: plain call on our window; setting the same id again restarts the timer.
    unsafe { SetTimer(hwnd, TOAST_TIMER, ms as u32, None) };
    invalidate(hwnd);
}

/// Input for the overlay, seen before everything else. An open dialog takes keys
/// (except Alt combinations, so Alt+F4 reaches WM_CLOSE), clicks, the wheel, pointer
/// moves and drops; a toast takes clicks on itself. `None` passes the message on.
pub fn overlay(
    app: &RefCell<App>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    let dialog = app.try_borrow().ok()?.overlay.dialog_open();
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let (vk, mods) = (wparam.0 as u16, commands::mods());
            if !dialog {
                return debug_overlay(app, hwnd, vk, mods);
            }
            if mods.alt {
                return None;
            }
            // Auto-repeats (a key held since before the dialog) never answer it.
            let repeat = lparam.0 & (1 << 30) != 0;
            let event = if repeat {
                None
            } else {
                app.try_borrow_mut().ok()?.overlay.on_key(vk, mods)
            };
            overlay_changed(app, hwnd, event);
            Some(LRESULT(0))
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let mut a = app.try_borrow_mut().ok()?;
            let (x, y) = a.to_dips(point(lparam));
            let (view, now) = (a.client(), a.pointer.now_ms());
            a.overlay.press(x, y, view, now).then_some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            let (taken, event) = {
                let mut a = app.try_borrow_mut().ok()?;
                let (x, y) = a.to_dips(point(lparam));
                let (view, now) = (a.client(), a.pointer.now_ms());
                a.overlay.release(x, y, view, now)
            };
            if !taken {
                return None;
            }
            overlay_changed(app, hwnd, event);
            Some(LRESULT(0))
        }
        WM_MOUSEMOVE | WM_MOUSEWHEEL if dialog => Some(LRESULT(0)),
        WM_DROPFILES if dialog => {
            // SAFETY: the HDROP from WM_DROPFILES, released once and not used again.
            unsafe { DragFinish(HDROP(wparam.0 as *mut _)) };
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == TOAST_TIMER => {
            // SAFETY: plain call on our window's timer (one-shot).
            unsafe {
                let _ = KillTimer(hwnd, TOAST_TIMER);
            }
            let mut a = app.try_borrow_mut().ok()?;
            let now = a.pointer.now_ms();
            if a.overlay.expire(now) {
                invalidate(hwnd);
            } else if let Some(t) = &a.overlay.toast {
                // Fired early: wait out the rest.
                let left = (t.shown_at_ms + t.duration_ms).saturating_sub(now).max(1);
                // SAFETY: plain call on our window.
                unsafe { SetTimer(hwnd, TOAST_TIMER, left as u32, None) };
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// After the overlay handled input: repaint and, once a dialog closed or a toast
/// action was used, restore the toolbar tooltips and act on it.
fn overlay_changed(app: &RefCell<App>, hwnd: HWND, event: Option<OverlayEvent>) {
    invalidate(hwnd);
    if let Some(event) = event {
        sync_tooltips(app, hwnd);
        commands::on_overlay_event(app, hwnd, event);
    }
}

/// Debug builds only: Ctrl+Shift+1 to 7 show each dialog and toast, for smoke tests
/// until Tasks 6 and 7 show them for real.
fn debug_overlay(app: &RefCell<App>, hwnd: HWND, vk: u16, mods: Mods) -> Option<LRESULT> {
    if !cfg!(debug_assertions) || !mods.ctrl || !mods.shift || mods.alt {
        return None;
    }
    let texts = [
        TOAST_SAVED_AS_TAG,
        TOAST_FILE_CHANGED,
        TOAST_SAVE_FAILED,
        TOAST_UNSUPPORTED_FORMAT,
    ];
    match vk {
        0x31 => show_dialog(app, hwnd, Prompt::SaveRotation),
        0x32 => {
            let name = "IMG_0001.jpg".to_owned();
            show_dialog(app, hwnd, Prompt::PermanentDelete { name });
        }
        0x33 => show_toast(app, hwnd, Toast::recycled),
        0x34..=0x37 => {
            let text = texts[usize::from(vk - 0x34)];
            show_toast(app, hwnd, |now| Toast::new(text, now));
        }
        _ => return None,
    }
    Some(LRESULT(0))
}

fn on_button(app: &RefCell<App>, hwnd: HWND, b: ToolButton) {
    if let Some(c) = b.command() {
        commands::run(app, hwnd, c, None);
    }
    let timer = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        if b == ToolButton::Pin {
            a.toggle_pin(hwnd);
            // Keep the floating pill up after unpinning, then let it auto-hide.
            let now = a.pointer.now_ms();
            a.pointer.autohide.on_mouse_move(now);
            a.refresh_toolbar();
        }
        a.start_timer()
    };
    sync_tooltips(app, hwnd);
    after_change(hwnd, timer, true);
}

fn after_change(hwnd: HWND, start_timer: bool, repaint: bool) {
    if start_timer {
        // SAFETY: plain call on our window; WM_TIMER is handled above.
        unsafe { SetTimer(hwnd, TIMER_ID, TIMER_MS, None) };
    }
    if repaint {
        invalidate(hwnd);
    }
}

fn track_leave(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: `tme` is a live, correctly sized local.
    unsafe {
        let _ = TrackMouseEvent(&mut tme);
    }
}

/// The cursor position in client pixels.
fn cursor_in_client(hwnd: HWND) -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    // SAFETY: `pt` is a live local; plain calls on our window.
    unsafe {
        GetCursorPos(&mut pt).ok()?;
        if !ScreenToClient(hwnd, &mut pt).as_bool() {
            return None;
        }
    }
    Some((pt.x, pt.y))
}

/// Client coordinates from a mouse message's lparam (signed 16-bit each).
fn point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16;
    (i32::from(x), i32::from(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_reads_signed_coordinates() {
        assert_eq!(point(LPARAM(0x0020_0010)), (16, 32));
        assert_eq!(point(LPARAM(0xFFFE_FFFF)), (-1, -2));
    }
}
