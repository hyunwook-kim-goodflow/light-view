//! Leaving a photo with an unsaved rotation (the save prompt), its save and its delete.

use std::path::{Path, PathBuf};

use lv_core::command::Command;
use lv_core::identity::FileIdentity;

use super::{Redraw, Viewer};

/// What the user tried to do when leaving a photo with an unsaved rotation; done once
/// the save prompt is answered (or the save finished).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaveReason {
    /// `Command::Next` or `Command::Prev`.
    Move(Command),
    /// Open a file (a drop, a forwarded launch).
    Open(PathBuf),
    /// Close the window.
    Close,
    /// Delete the photo on screen.
    Delete,
    /// Show the pending deleted photo again.
    Undo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveDecision {
    /// Nothing to save: go ahead.
    Proceed,
    /// Show the save prompt; the reason waits for its answer.
    Ask,
    /// Save without asking (`auto_save_rotation`); the reason waits for the save.
    AutoSave,
}

/// What a save of the current photo needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    pub path: PathBuf,
    pub identity: Option<FileIdentity>,
    pub quarters: u8,
    pub generation: u64,
}

impl Viewer {
    pub fn set_auto_save(&mut self, on: bool) {
        self.auto_save = on;
    }

    /// Called before every way of leaving the current photo. With an unsaved rotation
    /// the reason is kept (`take_pending_leave`) and the caller asks or saves first. A
    /// move that would not move (at either end of the folder) never asks.
    pub fn before_leave(&mut self, reason: LeaveReason) -> LeaveDecision {
        let moves = match reason {
            LeaveReason::Move(Command::Next) => self.list.neighbor(1).is_some(),
            LeaveReason::Move(Command::Prev) => self.list.neighbor(-1).is_some(),
            _ => true,
        };
        if !moves || !self.unsaved_rotation() {
            return LeaveDecision::Proceed;
        }
        self.defer_leave(reason);
        if self.auto_save && !self.save_failed {
            LeaveDecision::AutoSave
        } else {
            LeaveDecision::Ask
        }
    }

    /// Keeps `reason` to do once a save ends. A close already waiting is not replaced.
    pub fn defer_leave(&mut self, reason: LeaveReason) {
        if self.pending_leave != Some(LeaveReason::Close) {
            self.pending_leave = Some(reason);
        }
    }

    pub fn take_pending_leave(&mut self) -> Option<LeaveReason> {
        self.pending_leave.take()
    }

    /// The prompt was cancelled: stay on the photo, rotated.
    pub fn cancel_leave(&mut self) {
        self.pending_leave = None;
    }

    /// A close waits for the running save (or the prompt's answer).
    pub fn closing_after_save(&self) -> bool {
        self.pending_leave == Some(LeaveReason::Close)
    }

    /// [저장 안 함]: drops the view rotation.
    pub fn discard_rotation(&mut self) -> Redraw {
        if self.quarters == 0 {
            return Redraw::None;
        }
        self.quarters = 0;
        if self.showing_current() {
            if let Some(s) = &mut self.shown {
                s.quarters = 0;
            }
        }
        self.touched = false;
        let (size, dip) = (self.view_size, self.dip);
        self.set_view(size, dip);
        Redraw::Full
    }

    /// The photo a delete acts on: the current one, if it is the one on screen (also
    /// right after a save, while it is decoded again), with its identity.
    pub fn delete_target(&self) -> Option<(PathBuf, Option<FileIdentity>)> {
        let s = self.shown.as_ref()?;
        (self.list.current() == Some(s.path.as_path()))
            .then(|| (s.path.clone(), s.identity.clone()))
    }

    /// Hides a deleted (pending) photo; the next one is shown, or the previous one at
    /// the end of the folder, or nothing once the folder is empty.
    pub fn hide(&mut self, path: &Path) -> Redraw {
        let was_current = self.list.current() == Some(path);
        if !self.list.hide(path) {
            return Redraw::None;
        }
        if !was_current {
            return Redraw::Full;
        }
        self.quarters = 0;
        self.backward = false;
        if self.list.current().is_none() {
            return self.clear_shown();
        }
        self.after_move()
    }

    /// Nothing left to show: the folder has no (listed) photo.
    pub(super) fn clear_shown(&mut self) -> Redraw {
        self.shown = None;
        self.generation += 1;
        self.loading = false;
        self.wanted.clear();
        self.prefetcher.request(&[]);
        Redraw::Full
    }

    /// Like `hide`, for good (the photo was deleted permanently).
    pub fn remove(&mut self, path: &Path) -> Redraw {
        let redraw = self.hide(path);
        self.list.forget(path);
        redraw
    }

    /// A hidden photo is gone from disk: it is never listed again.
    pub fn forget_hidden(&mut self, path: &Path) {
        self.list.forget(path);
    }

    /// A hidden photo stays on disk after all: listed again, without moving to it.
    pub fn unhide(&mut self, path: &Path) -> Redraw {
        let empty = !self.has_file();
        if !self.list.unhide(path) {
            return Redraw::None;
        }
        if empty {
            return self.after_move();
        }
        Redraw::Full
    }

    /// Undo: lists a hidden photo again and shows it; a photo from another folder is
    /// opened there.
    pub fn restore(&mut self, path: &Path) -> Redraw {
        if !self.list.unhide(path) || !self.list.go_to(path) {
            self.open(path);
            return Redraw::Full;
        }
        self.quarters = 0;
        self.backward = false;
        self.after_move()
    }

    /// The current photo with its unsaved rotation, if it is on screen.
    pub fn save_request(&self) -> Option<SaveRequest> {
        if !self.showing_current() || !self.unsaved_rotation() {
            return None;
        }
        let s = self.shown.as_ref()?;
        Some(SaveRequest {
            path: s.path.clone(),
            identity: s.identity.clone(),
            quarters: self.quarters,
            generation: self.generation,
        })
    }

    /// `path` was saved and now holds the rotation on screen: it is decoded again and
    /// shown unrotated, the old decode staying up until then. `identity` is the saved
    /// file's.
    pub fn on_saved(&mut self, path: &Path, identity: Option<FileIdentity>) -> Redraw {
        self.prefetcher.invalidate(path);
        if let Some(s) = self.shown.as_mut().filter(|s| s.path == path) {
            s.identity = identity;
            s.stale = true;
        }
        if self.list.current() == Some(path) {
            self.quarters = 0;
        }
        self.save_failed = false;
        self.show_current();
        Redraw::Full
    }

    /// A save of the shown photo did not happen: stay on it, and the next leave asks.
    pub fn on_save_failed(&mut self) {
        self.save_failed = true;
        self.pending_leave = None;
    }
}
