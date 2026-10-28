//! Input routed to the in-window dialog and toast and to the EXIF card before anything
//! else: showing them, their keys and clicks, and acting on their answers.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::Shell::{DragFinish, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, WHEEL_DELTA, WM_CONTEXTMENU, WM_DROPFILES, WM_KEYDOWN, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SYSKEYDOWN, WM_TIMER,
};

use crate::clipboard;
use crate::commands;
use crate::exif_card::CardAction;
use crate::menu;
use crate::overlay::{DialogTarget, OverlayEvent, Prompt, Toast};
use crate::start;
use crate::strings::S;
use crate::watch;
use crate::window::{invalidate, App};

use super::{cursor_in_client, point, sync_tooltips, TOAST_TIMER};

/// Shows a dialog about `target`. Until it closes it takes every key, click and wheel
/// turn, cancels WM_CLOSE and defers forwarded opens; its answer, with `target`, goes
/// to `commands::on_overlay_event`. A drag in progress ends.
pub fn show_dialog(app: &RefCell<App>, hwnd: HWND, prompt: Prompt, target: DialogTarget) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    a.menu.close();
    a.overlay.show_dialog(prompt, target);
    let dragging = a.pointer.cancel_press();
    drop(a);
    if dragging {
        release_capture();
    }
    sync_tooltips(app, hwnd);
    invalidate(hwnd);
}

/// Ends mouse capture (a drag). No App borrow may be held: WM_CAPTURECHANGED re-enters.
pub fn release_capture() {
    // SAFETY: plain call.
    unsafe {
        let _ = ReleaseCapture();
    }
}

/// Whether a dialog is open. Anything that would change the current photo outside
/// the user's own input (a forwarded open, a folder refresh) waits while it is.
pub fn dialog_open(app: &RefCell<App>) -> bool {
    app.try_borrow().is_ok_and(|a| a.overlay.dialog_open())
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
    // A replaced "N장 대체 · 실행 취소" is final now.
    crate::resize::toasts::toast_check(app, hwnd);
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
    // The right-click menu sits under a dialog and over everything else.
    if !dialog {
        if let Some(result) = menu::route(app, hwnd, msg, wparam, lparam) {
            return Some(result);
        }
    }
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let (vk, mods) = (wparam.0 as u16, commands::mods());
            if !dialog {
                // Auto-repeats only scroll the card's list; a held Esc or I does not
                // close the card and then the window.
                let repeat = lparam.0 & (1 << 30) != 0;
                if !repeat || is_list_key(vk) {
                    if let Some(action) = card_key(app, vk, mods) {
                        card_action(app, hwnd, action);
                        return Some(LRESULT(0));
                    }
                }
                #[cfg(debug_assertions)]
                return debug_overlay(app, hwnd, vk, mods);
                #[cfg(not(debug_assertions))]
                return None;
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
        WM_CONTEXTMENU if dialog => Some(LRESULT(0)),
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let mut a = app.try_borrow_mut().ok()?;
            let (x, y) = a.to_dips(point(lparam));
            // The card and the start screen's button are under a dialog (which takes
            // every click) and over the photo.
            if !dialog {
                if let Some(m) = a.card_model() {
                    if a.exif_card.press(x, y, &m) {
                        return Some(LRESULT(0));
                    }
                }
                if let Some(l) = a.start_layout() {
                    if a.start.press(x, y, &l) {
                        return Some(LRESULT(0));
                    }
                }
            }
            let (view, now) = (a.client(), a.pointer.now_ms());
            a.overlay.press(x, y, view, now).then_some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            if !dialog {
                let start = {
                    let mut a = app.try_borrow_mut().ok()?;
                    let (x, y) = a.to_dips(point(lparam));
                    let l = a.start_layout();
                    l.and_then(|l| a.start.release(x, y, &l))
                };
                if let Some(clicked) = start {
                    if clicked {
                        start::open_dialog(app, hwnd);
                    }
                    return Some(LRESULT(0));
                }
                let card = {
                    let mut a = app.try_borrow_mut().ok()?;
                    let (x, y) = a.to_dips(point(lparam));
                    let m = a.card_model();
                    m.and_then(|m| a.exif_card.release(x, y, &m))
                };
                if let Some(action) = card {
                    if let Some(action) = action {
                        card_action(app, hwnd, action);
                    }
                    return Some(LRESULT(0));
                }
            }
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
        // The wheel over the card scrolls its list; elsewhere it navigates as usual.
        WM_MOUSEWHEEL => {
            let px = cursor_in_client(hwnd)?;
            {
                let mut a = app.try_borrow_mut().ok()?;
                let (x, y) = a.to_dips(px);
                let m = a.card_model()?;
                if !a.exif_card.contains(x, y, &m) {
                    return None;
                }
                let delta = i32::from(((wparam.0 >> 16) & 0xFFFF) as u16 as i16);
                a.pointer.wheel += delta;
                let notches = a.pointer.wheel / WHEEL_DELTA as i32;
                a.pointer.wheel -= notches * WHEEL_DELTA as i32;
                a.exif_card.on_wheel(x, y, notches, &m);
            }
            invalidate(hwnd);
            Some(LRESULT(0))
        }
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
                drop(a);
                invalidate(hwnd);
                // A replace whose toast went is final; a waiting report shows.
                crate::resize::toasts::toast_check(app, hwnd);
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

/// Keys the card's list repeats while held.
fn is_list_key(vk: u16) -> bool {
    matches!(vk, 0x21..=0x24 | 0x26 | 0x28)
}

/// A key for the open EXIF card; `None` if it is not the card's (or no card is open).
fn card_key(app: &RefCell<App>, vk: u16, mods: lv_core::command::Mods) -> Option<CardAction> {
    let mut a = app.try_borrow_mut().ok()?;
    let m = a.card_model()?;
    a.exif_card.on_key(vk, mods, &m)
}

/// Repaints after the card took an input; a copy goes to the clipboard and says
/// "복사했어요" (on failure only the log hears of it).
pub fn card_action(app: &RefCell<App>, hwnd: HWND, action: CardAction) {
    match action {
        CardAction::Redraw => invalidate(hwnd),
        CardAction::Copy(text) => match clipboard::set_text(hwnd, &text) {
            Ok(()) => show_toast(app, hwnd, |now| Toast::new(S::ToastCopied, now)),
            Err(e) => {
                eprintln!("light-view: could not copy to the clipboard: {e}");
                invalidate(hwnd);
            }
        },
    }
}

/// After the overlay handled input: repaint and, once a dialog closed or a toast
/// action was used, restore the toolbar tooltips and act on it. Then a path forwarded
/// while the dialog was open is opened, after the answer was applied to its target,
/// unless the answer closes the window.
pub fn overlay_changed(app: &RefCell<App>, hwnd: HWND, event: Option<OverlayEvent>) {
    invalidate(hwnd);
    let Some(event) = event else {
        return;
    };
    sync_tooltips(app, hwnd);
    if !commands::on_overlay_event(app, hwnd, event) {
        return;
    }
    let deferred = app
        .try_borrow_mut()
        .ok()
        .and_then(|mut a| a.overlay.take_deferred_open());
    if let Some(path) = deferred {
        commands::open(app, hwnd, &path);
    }
    // Photos sent to the resize screen meanwhile.
    crate::resize::flush_queued(app, hwnd);
    // Then a rescan held back by the dialog (of the newly opened folder, if any; it
    // waits again if the open asks to save first).
    watch::rescan_if_due(app, hwnd);
}

/// Debug builds only: Ctrl+Shift+1 and 3 to 7 show the save prompt and each toast, for
/// smoke tests. The prompt targets the current photo. (2, the permanent-delete question,
/// is gone: its [삭제] now deletes the photo for good.)
#[cfg(debug_assertions)]
fn debug_overlay(
    app: &RefCell<App>,
    hwnd: HWND,
    vk: u16,
    mods: lv_core::command::Mods,
) -> Option<LRESULT> {
    if !mods.ctrl || !mods.shift || mods.alt {
        return None;
    }
    let target = {
        let a = app.try_borrow().ok()?;
        DialogTarget {
            path: a
                .viewer
                .current_path()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default(),
            generation: a.viewer.generation(),
        }
    };
    let keys = [
        S::ToastSavedAsTag,
        S::ToastFileChanged,
        S::ToastSaveFailed,
        S::ToastUnsupportedFormat,
    ];
    match vk {
        0x31 => show_dialog(app, hwnd, Prompt::SaveRotation, target),
        0x33 => show_toast(app, hwnd, Toast::recycled),
        0x34..=0x37 => {
            let key = keys[usize::from(vk - 0x34)];
            show_toast(app, hwnd, |now| Toast::new(key, now));
        }
        _ => return None,
    }
    Some(LRESULT(0))
}
