//! The resize screen's toasts (M6 Task 6): the summary after a save, "N장 대체 · 실행
//! 취소" (its end commits the batch, its undo restores), and what a commit or an undo
//! could not do. Every function takes the `RefCell<App>` and holds no borrow across
//! showing a toast.

use std::cell::RefCell;
use std::path::PathBuf;

use lv_core::listing::same_path;
use windows::Win32::Foundation::HWND;

use super::saving::{self, SaveSummary};
use super::text::ToastText;
use super::BatchReport;
use crate::commands;
use crate::input;
use crate::overlay::Toast;
use crate::viewer::Redraw;
use crate::window::{invalidate, App};

/// A save ended (the screen closed): its summary, with [실행 취소] when it replaced
/// photos.
pub fn after_save(app: &RefCell<App>, hwnd: HWND) {
    let Some((summary, undo)) = app.try_borrow_mut().ok().and_then(|mut a| {
        let s = a.resize.last_save.take()?;
        // "N장 대체" counts the photos its [실행 취소] brings back.
        let n = a.resize.batch.as_ref().map_or(0, |b| b.len());
        let undo = n > 0;
        let replaced = if undo { n } else { s.replaced };
        Some((SaveSummary { replaced, ..s }, undo))
    }) else {
        return;
    };
    input::show_toast(app, hwnd, |now| {
        Toast::resize(ToastText::Summary(summary), undo, now)
    });
}

/// After the toast changed or went: a replace whose "N장 대체 · 실행 취소" is no
/// longer up is final (its originals go to the Recycle Bin), and once no toast is up
/// the next report of a commit or undo is shown.
pub fn toast_check(app: &RefCell<App>, hwnd: HWND) {
    let Ok((commit, report)) = app.try_borrow_mut().map(|mut a| {
        let now = a.pointer.now_ms();
        let toast = a.overlay.toast.as_ref().filter(|t| t.visible(now));
        let undo_up = toast.is_some_and(Toast::undoes_replace);
        let free = toast.is_none();
        let report = (free && !a.resize.reports.is_empty()).then(|| a.resize.reports.remove(0));
        (a.resize.batch.is_some() && !undo_up, report)
    }) else {
        return;
    };
    if commit {
        saving::commit_batch(app);
    }
    if let Some(r) = report {
        let what = match r {
            BatchReport::Unrecycled(v) => ToastText::Unrecycled(v),
            BatchReport::Unrestored(v) => ToastText::Unrestored(v),
        };
        input::show_toast(app, hwnd, |now| Toast::resize(what, false, now));
    }
}

/// [실행 취소] on "N장 대체": every replaced photo comes back (off the UI thread).
pub fn undo(app: &RefCell<App>, hwnd: HWND) {
    saving::undo_batch(app);
    invalidate(hwnd);
}

/// An undo ended: the photos it touched are decoded again, and the viewer shows the
/// original again if it showed one of them.
pub fn undone(app: &RefCell<App>, hwnd: HWND, paths: &[(PathBuf, PathBuf)]) {
    let redraw = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        for (original, result) in paths {
            a.viewer.forget_decode(original);
            a.viewer.forget_decode(result);
        }
        // Still open on photos not saved: Esc goes back to what the undo restored.
        if let Some(s) = &mut a.resize.session {
            s.undo_entered(paths);
        }
        let shown = a.viewer.current_path().map(std::path::Path::to_path_buf);
        let back = shown.and_then(|c| {
            paths
                .iter()
                .find(|(o, r)| same_path(o, &c) || same_path(r, &c))
                .map(|(o, _)| o.clone())
        });
        match back {
            Some(original) if !a.resize.is_open() => {
                a.viewer.open(&original);
                Redraw::Full
            }
            _ => Redraw::None,
        }
    };
    commands::after(app, hwnd, redraw);
}
