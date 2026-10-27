//! The rotation save flow: every way of leaving a photo ([`leave`]), the save prompt's
//! answer, and the save thread and its report (`WM_APP_SAVE_DONE`).

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use lv_core::identity;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use crate::delete;
use crate::input;
use crate::instance;
use crate::overlay::{DialogTarget, Prompt, PromptResult, Toast};
use crate::save::{save_rotation, NoWorker, SaveOutcome};
use crate::strings::S;
use crate::viewer::{LeaveDecision, LeaveReason, Redraw};
use crate::watch;
use crate::window::{self, App};

use super::{after, LOADING_DELAY_MS, SAVING_TIMER, WM_APP_SAVE_DONE};

/// A rotation save in progress. While it runs, nothing else may save, delete or leave
/// the photo: leaving waits for it ([`leave`]).
pub struct Saving {
    /// The save has run longer than `LOADING_DELAY_MS`: "저장하는 중…" is shown.
    pub overdue: bool,
}

/// A save thread's report.
pub struct SaveDone {
    path: PathBuf,
    outcome: SaveOutcome,
    /// The saved file's identity, read right after the save.
    identity: Option<identity::FileIdentity>,
}
/// WM_CLOSE (no dialog open): closes, after the save prompt or save if needed.
pub fn request_close(app: &RefCell<App>, hwnd: HWND) {
    leave(app, hwnd, LeaveReason::Close);
}

/// Every way of leaving the current photo. With an unsaved rotation it shows the save
/// prompt, or saves first with `auto_save_rotation`; the leave then waits for the answer
/// or the save. While a save runs, a leave waits for it to end.
pub fn leave(app: &RefCell<App>, hwnd: HWND, reason: LeaveReason) {
    let decision = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        if a.saving.is_some() {
            if reason == LeaveReason::Close {
                // Paths forwarded meanwhile open their own window.
                instance::mark_closing();
            }
            a.viewer.defer_leave(reason);
            return;
        }
        a.viewer.before_leave(reason.clone())
    };
    match decision {
        LeaveDecision::Proceed => perform(app, hwnd, reason),
        LeaveDecision::Ask => {
            if let Some(target) = current_target(app) {
                input::show_dialog(app, hwnd, Prompt::SaveRotation, target);
            }
        }
        LeaveDecision::AutoSave => start_save(app, hwnd),
    }
}

/// Does what the user asked for once nothing (or nothing more) needs saving.
fn perform(app: &RefCell<App>, hwnd: HWND, reason: LeaveReason) {
    let redraw = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        match reason {
            LeaveReason::Move(cmd) => a.viewer.apply(cmd),
            LeaveReason::Open(path) => {
                delete::cancel_if_pending(&mut a, &path);
                a.viewer.open(&path);
                Redraw::Full
            }
            LeaveReason::Close => {
                drop(a);
                window::close(app, hwnd);
                return;
            }
            LeaveReason::Delete => {
                drop(a);
                delete::delete_current(app, hwnd);
                return;
            }
            LeaveReason::Undo => {
                drop(a);
                delete::undo(app, hwnd);
                return;
            }
        }
    };
    after(app, hwnd, redraw);
}

/// The current photo, as a dialog's target.
fn current_target(app: &RefCell<App>) -> Option<DialogTarget> {
    let a = app.try_borrow().ok()?;
    Some(DialogTarget {
        path: a.viewer.current_path()?.to_path_buf(),
        generation: a.viewer.generation(),
    })
}

/// Saves the current photo's rotation on a background thread. Ctrl+S and the Save button
/// come here directly; leaving comes here after the prompt or with auto-save, and then
/// waits for the save. Does nothing while another save runs.
pub(super) fn start_save(app: &RefCell<App>, hwnd: HWND) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    if a.saving.is_some() {
        return;
    }
    let Some(request) = a.viewer.save_request() else {
        // Nothing to save after all: a waiting leave goes ahead.
        let pending = a.viewer.take_pending_leave();
        drop(a);
        if let Some(reason) = pending {
            perform(app, hwnd, reason);
        }
        return;
    };
    let Some(id) = request.identity else {
        // The file could not be read when it was shown.
        a.viewer.on_save_failed();
        drop(a);
        input::show_toast(app, hwnd, |now| Toast::new(S::ToastFileChanged, now));
        return;
    };
    if a.viewer.closing_after_save() {
        // Paths forwarded meanwhile open their own window.
        instance::mark_closing();
    }
    a.saving = Some(Saving { overdue: false });
    let pool = a.pool.clone();
    let limits = a.decode_limits;
    let tx = a.save_tx.clone();
    drop(a);
    let target = hwnd.0 as isize;
    std::thread::spawn(move || {
        let path = request.path;
        let save = || match &pool {
            Some(pool) => save_rotation(&path, &id, request.quarters, &**pool, &limits),
            None => save_rotation(&path, &id, request.quarters, &NoWorker, &limits),
        };
        let outcome = lv_core::unwind::catch(AssertUnwindSafe(save))
            .unwrap_or_else(|_| SaveOutcome::Failed("the save panicked".into()));
        let saved = matches!(
            outcome,
            SaveOutcome::Saved | SaveOutcome::SavedAsOrientationTag
        );
        let identity = saved.then(|| identity::capture(&path).ok()).flatten();
        let _ = tx.send(SaveDone {
            path,
            outcome,
            identity,
        });
        // SAFETY: plain call; posting to a destroyed window just fails.
        unsafe {
            let _ = PostMessageW(
                HWND(target as *mut _),
                WM_APP_SAVE_DONE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
    // SAFETY: plain call on our window's timer.
    unsafe { SetTimer(hwnd, SAVING_TIMER, LOADING_DELAY_MS, None) };
}

/// A save ended: the photo is decoded again unrotated and a waiting leave goes ahead;
/// or it stays rotated, the leave is dropped and a toast says why.
pub(super) fn on_save_done(app: &RefCell<App>, hwnd: HWND, done: SaveDone) {
    // SAFETY: plain call on our window's timer.
    unsafe {
        let _ = KillTimer(hwnd, SAVING_TIMER);
    }
    let toast = match &done.outcome {
        SaveOutcome::Saved => None,
        SaveOutcome::SavedAsOrientationTag => Some((S::ToastSavedAsTag, String::new())),
        SaveOutcome::Unsupported => Some((S::ToastUnsupportedFormat, String::new())),
        SaveOutcome::Changed => Some((S::ToastFileChanged, String::new())),
        SaveOutcome::ReadOnly => Some((S::ToastReadOnly, String::new())),
        SaveOutcome::Failed(e) => {
            eprintln!("light-view: could not save {}: {e}", done.path.display());
            Some((S::ToastSaveFailed, String::new()))
        }
        SaveOutcome::KeptCopy(copy) => {
            let name = copy.file_name().unwrap_or_default().to_string_lossy();
            Some((S::ToastKeptCopy, name.into_owned()))
        }
    };
    let pending = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        a.saving = None;
        if matches!(
            done.outcome,
            SaveOutcome::Saved | SaveOutcome::SavedAsOrientationTag
        ) {
            a.viewer.on_saved(&done.path, done.identity);
            a.viewer.take_pending_leave()
        } else {
            if a.viewer.closing_after_save() {
                instance::cancel_closing();
            }
            a.viewer.on_save_failed();
            None
        }
    };
    if let Some((key, name)) = toast {
        input::show_toast(app, hwnd, |now| Toast::with_suffix(key, &name, now));
    }
    after(app, hwnd, Redraw::Full);
    let closing = pending == Some(LeaveReason::Close);
    if let Some(reason) = pending {
        perform(app, hwnd, reason);
    }
    // A rescan held back by the save runs now (a closing window needs none).
    if !closing {
        watch::rescan_if_due(app, hwnd);
    }
}
/// [저장] saves (and with the checkbox, from now on without asking), [저장 안 함] drops
/// the rotation, and either then does what the user tried to do; [취소] stays. An answer
/// about a photo no longer current does nothing.
pub(super) fn on_save_prompt(
    app: &RefCell<App>,
    hwnd: HWND,
    result: PromptResult,
    dont_ask: bool,
    target: &DialogTarget,
) -> bool {
    let stale = current_target(app).as_ref() != Some(target);
    let Ok(mut a) = app.try_borrow_mut() else {
        return true;
    };
    if stale {
        a.viewer.cancel_leave();
        return true;
    }
    match result {
        PromptResult::Save => {
            if dont_ask {
                a.persist(|s| s.auto_save_rotation = true);
                a.viewer.set_auto_save(true);
            }
            let closing = a.viewer.closing_after_save();
            drop(a);
            start_save(app, hwnd);
            !closing
        }
        PromptResult::Discard => {
            let redraw = a.viewer.discard_rotation();
            let pending = a.viewer.take_pending_leave();
            drop(a);
            after(app, hwnd, redraw);
            let closing = pending == Some(LeaveReason::Close);
            if let Some(reason) = pending {
                perform(app, hwnd, reason);
            }
            !closing
        }
        PromptResult::Cancel
        | PromptResult::Confirm
        | PromptResult::Notices
        | PromptResult::Close
        | PromptResult::NotifyMe
        | PromptResult::NoThanks => {
            a.viewer.cancel_leave();
            true
        }
    }
}
