//! Pointer input for the toolbar: auto-hide timer, hover, button clicks, tooltips.

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_TIMER,
};

use crate::toolbar::{bar_rect, hit_test, layout, AutoHide, ToolButton};
use crate::tooltip::{self, Tool};
use crate::window::{invalidate, App};

const TIMER_ID: usize = 1;
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

    fn now_ms(&self) -> u64 {
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

    /// The tooltip tools for the current layout, if they changed: (tooltip, old count, tools).
    fn changed_tools(&mut self) -> Option<(HWND, usize, Vec<Tool>)> {
        let px = |v: f32| (v / self.dip).round() as i32;
        let tools: Vec<Tool> = self
            .toolbar_input()
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
        WM_LBUTTONDOWN => {
            let mut a = app.try_borrow_mut().ok()?;
            a.pointer.pressed = a.enabled_button_at(lparam);
            Some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            let clicked = {
                let mut a = app.try_borrow_mut().ok()?;
                let pressed = a.pointer.pressed.take();
                pressed.filter(|b| a.enabled_button_at(lparam) == Some(*b))
            };
            if let Some(b) = clicked {
                on_button(app, hwnd, b);
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

fn on_button(app: &RefCell<App>, hwnd: HWND, b: ToolButton) {
    let timer = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        match b.command() {
            Some(c) => a.on_command(c),
            None if b == ToolButton::Pin => {
                a.toggle_pin(hwnd);
                // Keep the floating pill up after unpinning, then let it auto-hide.
                let now = a.pointer.now_ms();
                a.pointer.autohide.on_mouse_move(now);
                a.refresh_toolbar();
            }
            None => {}
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
