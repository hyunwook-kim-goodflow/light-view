//! Resizing photos (M6): the resize screen's session, the ways into it (Explorer's
//! `--resize`, forwarded launches, drops, the menu), its background estimates and its
//! save. Drawing and the sidebar are the screen's own (`draw`, `sidebar`); this module
//! gives them the state and the actions.
//!
//! Every function here takes the `RefCell<App>` and holds no borrow across calls that may
//! re-enter the window procedure.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use lv_core::command::Command;
use lv_core::format::is_supported_extension;
use lv_core::identity;
use lv_core::listing::same_path;
use lv_core::settings::ResizeMemory;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

use crate::commands::{self, HwndCell};
use crate::delete::{self, ShellTrash, Trash};
use crate::input;
use crate::overlay::{DialogTarget, Prompt, Toast};
use crate::strings::S;
use crate::viewer::Redraw;
use crate::window::{invalidate, App};

pub mod draw;
pub mod estimate;
mod fields;
pub mod layout;
pub mod save;
pub mod saving;
pub mod screen;
pub mod session;
pub mod sidebar;
pub mod text;
pub mod thumb;
pub mod toasts;

use estimate::{ByteCache, Estimator, Job, Maker, ESTIMATE_THREADS};
use save::batch::{ReplaceBatch, Unrecycled, Unrestored};
use save::{longest_replace_path, ItemResult};
pub use saving::{defer_close, flush_at_exit, on_saved};
use saving::{Event, SaveSummary};
pub use session::{
    drop_action, estimate_order, DropAction, Estimate, ReplaceBlock, ResizeItem, ResizeSession,
    Return,
};

/// Posted by an estimate thread once its result is in `ResizeState`'s estimator.
pub const WM_APP_RESIZE_ESTIMATE: u32 = WM_APP + 9;
/// Posted by the save thread and the batch threads once an event is queued.
pub const WM_APP_RESIZE_SAVED: u32 = WM_APP + 10;

/// Paths shorter than this (UTF-16 units) can go to the Recycle Bin (`ShellTrash`).
const SHELL_MAX_PATH: usize = 260;

/// A save in progress (spec §6: the progress is shown as photos done of `total`).
#[derive(Debug, Default)]
pub struct SaveProgress {
    pub total: usize,
    /// Each photo done so far, with how it ended.
    pub results: Vec<(PathBuf, ItemResult)>,
    /// The window closes once the save ends.
    close_after: bool,
}

/// What a commit or an undo of a replace could not do, for the screen to tell.
#[derive(Debug)]
pub enum BatchReport {
    /// Originals the Recycle Bin did not take, kept visible as `(원본)`.
    Unrecycled(Vec<Unrecycled>),
    /// Photos an undo did not fully restore.
    Unrestored(Vec<Unrestored>),
}

/// The resize screen's state, in `App`.
pub struct ResizeState {
    /// The open session; `None` while the viewer is shown.
    pub session: Option<ResizeSession>,
    /// Why 원본 대체 is off (R3), for the sidebar; kept up to date as photos and their
    /// estimates come in and the format changes.
    pub replace_block: Option<ReplaceBlock>,
    /// The running save.
    pub saving: Option<SaveProgress>,
    /// The last save's counts, until the screen showed them (`take`).
    pub last_save: Option<SaveSummary>,
    /// The photos the last save replaced: committed when its "N장 대체 · 실행 취소" toast
    /// goes ([`commit_batch`]) or undone ([`undo_batch`]); committed at exit.
    pub batch: Option<ReplaceBatch>,
    /// What commits and undos could not do, until the screen showed it.
    pub reports: Vec<BatchReport>,
    estimator: Option<Estimator>,
    cache: ByteCache,
    hwnd: HwndCell,
    /// Photos that came while a save ran or a dialog was open; added afterwards.
    queued: Vec<PathBuf>,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    /// The resize saves' own Recycle Bin: its reports never reach the delete toasts.
    trash: Option<Arc<ShellTrash>>,
    /// Running commits and undos.
    batch_threads: Vec<JoinHandle<()>>,
    /// Whether a folder (and a path that short) has a Recycle Bin.
    bins: HashMap<(PathBuf, bool), bool>,
    /// The highest estimate generation handed out, across sessions: a job still running
    /// from an earlier session can never match a later one.
    last_generation: u64,
    /// The screen's sidebar focus and number fields.
    pub ui: screen::Ui,
}

impl Default for ResizeState {
    fn default() -> Self {
        let (events_tx, events_rx) = channel();
        ResizeState {
            session: None,
            replace_block: None,
            saving: None,
            last_save: None,
            batch: None,
            reports: Vec::new(),
            estimator: None,
            cache: ByteCache::default(),
            hwnd: HwndCell::default(),
            queued: Vec::new(),
            events_tx,
            events_rx,
            trash: None,
            batch_threads: Vec::new(),
            bins: HashMap::new(),
            last_generation: 0,
            ui: screen::Ui::default(),
        }
    }
}

impl ResizeState {
    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// Opens a session whose generations follow every earlier session's.
    fn open_session(&mut self, entered_from: Option<PathBuf>, quarters: u8, memory: ResizeMemory) {
        self.last_generation += 1;
        let generation = self.last_generation;
        self.session = Some(ResizeSession::new(
            entered_from,
            quarters,
            memory,
            generation,
        ));
    }

    /// Ends the session: queued estimates and kept bytes go; running ones finish stale.
    fn end_session(&mut self) -> Option<ResizeSession> {
        let s = self.session.take()?;
        self.last_generation = self.last_generation.max(s.generation);
        if let Some(e) = &self.estimator {
            e.cancel();
        }
        self.cache.clear();
        self.replace_block = None;
        Some(s)
    }

    fn trash(&mut self) -> Arc<ShellTrash> {
        self.trash
            .get_or_insert_with(|| {
                Arc::new(ShellTrash::new(Box::new(|path, result| {
                    if let Err(e) = result {
                        eprintln!("light-view: {} was not recycled: {e:?}", path.display());
                    }
                })))
            })
            .clone()
    }

    /// Queues estimates for `indices` of the session, the shown photo's first.
    fn schedule(&self, indices: &[usize]) {
        let (Some(s), Some(estimator)) = (&self.session, &self.estimator) else {
            return;
        };
        let jobs = estimate_order(s.current, s.items.len())
            .into_iter()
            .filter(|i| indices.contains(i))
            .map(|i| Job {
                generation: s.generation,
                index: i,
                path: s.items[i].path.clone(),
                quarters: s.items[i].quarters,
                spec: s.spec,
            })
            .collect();
        estimator.submit(jobs);
    }

    /// Works out `replace_block` again; 원본 대체 turns off when it is blocked.
    fn refresh_block(&mut self) {
        let trash = self.trash();
        let Some(s) = &mut self.session else {
            self.replace_block = None;
            return;
        };
        let format = s.spec.format;
        let mut has: HashMap<PathBuf, bool> = HashMap::new();
        for item in &s.items {
            let longest = longest_replace_path(&item.path, format);
            let short = longest.as_os_str().encode_wide().count() < SHELL_MAX_PATH;
            let dir = item.path.parent().unwrap_or(Path::new("")).to_path_buf();
            let bin = *self
                .bins
                .entry((dir, short))
                .or_insert_with(|| trash.has_recycle_bin(&longest));
            has.insert(item.path.clone(), bin);
        }
        self.replace_block = s.replace_block(&|p| has.get(p).copied().unwrap_or(false));
        if self.replace_block.is_some() {
            s.replace = false;
        }
    }
}

pub fn is_open(app: &RefCell<App>) -> bool {
    app.try_borrow().is_ok_and(|a| a.resize.is_open())
}

/// Opens the resize screen with `paths`, or adds them when it is open (R4). Photos only:
/// folders and other files are skipped; with nothing left, a toast says so. While a save
/// runs they wait for it (spec §10).
pub fn enter(app: &RefCell<App>, hwnd: HWND, paths: Vec<PathBuf>) {
    add(app, hwnd, paths);
}

/// Adds `paths` to the resize screen, opening it if need be (see [`enter`]).
pub fn add(app: &RefCell<App>, hwnd: HWND, paths: Vec<PathBuf>) {
    let opened = {
        let Ok(mut guard) = app.try_borrow_mut() else {
            return;
        };
        let a = &mut *guard;
        if a.resize.saving.is_some() {
            a.resize.queued.extend(paths);
            return;
        }
        let shown = a.viewer.current_path().map(Path::to_path_buf);
        let items = new_items(&paths, shown.as_deref(), a.viewer.view_rotation_quarters());
        if items.is_empty() {
            drop(guard);
            input::show_toast(app, hwnd, |now| Toast::new(S::ToastNoPhotos, now));
            return;
        }
        let opened = a.resize.session.is_none();
        if opened {
            let mut memory = a.settings.resize.unwrap_or_default();
            memory.quality = sidebar::snap_quality(memory.quality);
            // The shown photo's pending rotation comes back when the screen closes.
            let quarters = a.viewer.view_rotation_quarters();
            a.resize.open_session(shown, quarters, memory);
            // R1: kept results take at most a quarter of the memory limit.
            a.resize.cache = ByteCache::new(commands::memory_budget(&a.settings) / 4);
            if a.resize.estimator.is_none() {
                a.resize.hwnd.set(hwnd);
                let maker = Maker {
                    limits: a.decode_limits,
                    pool: a.pool.clone(),
                };
                let hwnd = a.resize.hwnd.clone();
                a.resize.estimator = Some(Estimator::start(
                    maker,
                    ESTIMATE_THREADS,
                    hwnd,
                    WM_APP_RESIZE_ESTIMATE,
                ));
            }
        }
        let added = a
            .resize
            .session
            .as_mut()
            .map(|s| s.add(items))
            .unwrap_or_default();
        a.resize.schedule(&added);
        a.resize.refresh_block();
        opened
    };
    if opened {
        show_current(app, hwnd);
    }
    invalidate(hwnd);
}

/// The photo files among `paths` (made absolute), as session items. The one the viewer
/// shows (`shown`) keeps its unsaved rotation `quarters` (spec §10).
fn new_items(paths: &[PathBuf], shown: Option<&Path>, quarters: u8) -> Vec<ResizeItem> {
    paths
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .filter(|p| p.is_file() && is_supported_extension(p))
        .filter_map(|p| {
            let id = identity::capture(&p)
                .map_err(|e| eprintln!("light-view: cannot resize {}: {e}", p.display()))
                .ok()?;
            let turned = shown.is_some_and(|s| same_path(s, &p));
            Some(ResizeItem::new(p, id, if turned { quarters } else { 0 }))
        })
        .collect()
}

/// The menu's 크기 조정 (and `Command::Resize`): the screen with the photo shown.
pub fn enter_current(app: &RefCell<App>, hwnd: HWND) {
    let Some(path) = app
        .try_borrow()
        .ok()
        .and_then(|a| a.viewer.current_path().map(Path::to_path_buf))
    else {
        return;
    };
    enter(app, hwnd, vec![path]);
}

/// A path another launch sent with `COPYDATA_RESIZE`. While a dialog (or the modal Open
/// dialog, `blocked`) is up it waits for it, like a forwarded open.
pub fn forwarded(app: &RefCell<App>, hwnd: HWND, path: PathBuf, blocked: bool) {
    let waits = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        let waits = blocked || a.overlay.dialog_open();
        if waits {
            a.resize.queued.push(path.clone());
        }
        waits
    };
    if !waits {
        add(app, hwnd, vec![path]);
    }
}

/// After a dialog closed: the paths that waited for it.
pub fn flush_queued(app: &RefCell<App>, hwnd: HWND) {
    let queued = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        if a.overlay.dialog_open() || a.resize.saving.is_some() {
            return;
        }
        std::mem::take(&mut a.resize.queued)
    };
    if !queued.is_empty() {
        add(app, hwnd, queued);
    }
}

/// Several photos dropped on the viewer: "보기 / 크기 조정" (spec §3).
pub fn ask_drop(app: &RefCell<App>, hwnd: HWND, paths: Vec<PathBuf>) {
    let Some(target) = app.try_borrow().ok().and_then(|a| {
        Some(DialogTarget {
            path: paths.first()?.clone(),
            generation: a.viewer.generation(),
        })
    }) else {
        return;
    };
    input::show_dialog(app, hwnd, Prompt::DropMany { paths }, target);
}

/// A viewer command while the screen is open: ←/→ move through its photos, Esc leaves
/// it, Ctrl+S saves; rotating, deleting and opening do nothing here (spec §10: no
/// rotation on this screen). False when the command is not the screen's (or it is closed).
pub fn command(app: &RefCell<App>, hwnd: HWND, cmd: Command) -> bool {
    let Ok((open, full)) = app
        .try_borrow()
        .map(|a| (a.resize.is_open(), a.fullscreen.is_some()))
    else {
        return false;
    };
    if !open {
        return false;
    }
    match cmd {
        Command::Next => step(app, hwnd, 1),
        Command::Prev => step(app, hwnd, -1),
        Command::Escape if !full => leave(app, hwnd),
        // Ctrl+S saves the resized photos, as [저장 (N장)] does.
        Command::Save => screen::save(app, hwnd),
        Command::RotateLeft
        | Command::RotateRight
        | Command::Delete
        | Command::Undo
        | Command::Open
        | Command::Resize => {}
        _ => return false,
    }
    true
}

/// Shows the session's photo `index`; its estimate (if still queued) goes first.
pub fn go_to(app: &RefCell<App>, hwnd: HWND, index: usize) {
    {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        let state = &mut a.resize;
        let Some(s) = &mut state.session else {
            return;
        };
        if index >= s.items.len() || index == s.current {
            return;
        }
        s.current = index;
        if let Some(e) = &state.estimator {
            e.reorder(&estimate_order(index, s.items.len()));
        }
    }
    show_current(app, hwnd);
    invalidate(hwnd);
}

/// The next (`delta` 1) or previous (-1) photo of the session, stopping at the ends.
pub fn step(app: &RefCell<App>, hwnd: HWND, delta: isize) {
    let Some(next) = app.try_borrow().ok().and_then(|a| {
        let s = a.resize.session.as_ref()?;
        let next = s.current.checked_add_signed(delta)?;
        (next < s.items.len()).then_some(next)
    }) else {
        return;
    };
    go_to(app, hwnd, next);
}

/// The viewer shows the session's current photo (it draws the screen's centre). Not
/// while a rotation save runs: the viewer is busy with its photo.
pub fn show_current(app: &RefCell<App>, hwnd: HWND) {
    {
        let Ok(mut guard) = app.try_borrow_mut() else {
            return;
        };
        let a = &mut *guard;
        if a.saving.is_some() {
            return;
        }
        let Some(path) = a
            .resize
            .session
            .as_ref()
            .and_then(|s| s.items.get(s.current))
            .map(|i| i.path.clone())
        else {
            return;
        };
        if a.viewer.current_path() == Some(path.as_path()) {
            return;
        }
        delete::cancel_if_pending(a, &path);
        a.viewer.open(&path);
    }
    commands::after(app, hwnd, Redraw::Full);
}

/// New sidebar values: saved to the settings; when the result changes, the estimates
/// start over (the old ones and their kept bytes are dropped).
pub fn set_memory(app: &RefCell<App>, hwnd: HWND, memory: ResizeMemory) {
    {
        let Ok(mut guard) = app.try_borrow_mut() else {
            return;
        };
        let a = &mut *guard;
        let Some(s) = &mut a.resize.session else {
            return;
        };
        if s.set_memory(memory) {
            let all: Vec<usize> = (0..s.items.len()).collect();
            a.resize.cache.clear();
            a.resize.schedule(&all);
            a.resize.refresh_block();
        }
        a.persist(|st| st.resize = Some(memory));
    }
    invalidate(hwnd);
}

/// 원본 대체 on or off. Refused (false) while it is blocked (R3) or a save runs.
pub fn set_replace(app: &RefCell<App>, hwnd: HWND, on: bool) -> bool {
    let done = app.try_borrow_mut().is_ok_and(|mut a| {
        let state = &mut a.resize;
        let free = state.saving.is_none() && (!on || state.replace_block.is_none());
        match &mut state.session {
            Some(s) if free => {
                s.replace = on;
                true
            }
            _ => false,
        }
    });
    invalidate(hwnd);
    done
}

/// `WM_APP_RESIZE_ESTIMATE`: takes the finished estimates; a result made for the current
/// settings is kept for the save (R1).
pub fn on_estimate(app: &RefCell<App>, hwnd: HWND) {
    let Ok(mut guard) = app.try_borrow_mut() else {
        return;
    };
    let state = &mut guard.resize;
    let Some(estimator) = &state.estimator else {
        return;
    };
    let done: Vec<_> = estimator.rx.try_iter().collect();
    let mut changed = false;
    for d in done {
        let Some(s) = &mut state.session else {
            break;
        };
        let estimate = d.outcome.estimate();
        let job = &d.job;
        if s.apply_estimate(job.generation, job.index, &job.path, d.source, estimate) {
            changed = true;
            if let estimate::Outcome::Ready { bytes, .. } = d.outcome {
                state.cache.put(&job.path, job.generation, bytes);
            }
        }
    }
    if changed {
        if let Some(s) = &state.session {
            let pending = s
                .items
                .iter()
                .filter(|i| i.estimate == Estimate::Pending)
                .count();
            crate::timing::estimate_arrived(s.items.len(), pending);
        }
        state.refresh_block();
        drop(guard);
        invalidate(hwnd);
    }
}

/// [취소] and Esc: closes the screen without saving, back to where it was opened.
pub fn leave(app: &RefCell<App>, hwnd: HWND) {
    if app.try_borrow().is_ok_and(|a| a.resize.saving.is_none()) {
        close_session(app, hwnd, &[]);
    }
}

/// Ends the session; the viewer shows [`ResizeSession::return_plan`]. Photos
/// replaced under their own names are decoded again.
fn close_session(app: &RefCell<App>, hwnd: HWND, results: &[(PathBuf, ItemResult)]) {
    let redraw = {
        let Ok(mut guard) = app.try_borrow_mut() else {
            return;
        };
        let a = &mut *guard;
        let Some(s) = a.resize.end_session() else {
            return;
        };
        for (original, result) in results {
            if let ItemResult::Replaced { result, .. } = result {
                a.viewer.forget_decode(original);
                a.viewer.forget_decode(result);
            }
        }
        // The filmstrip's tiles go with the session.
        a.clear_thumbs();
        if a.saving.is_some() {
            Redraw::Full
        } else {
            // Unless a save rewrote it, the photo gets its pending rotation back, without
            // asking (M6 ruling); a rewritten one is read again.
            match s.return_plan(results) {
                Return::Restore(target, quarters) => {
                    delete::cancel_if_pending(a, &target);
                    a.viewer.return_to(&target, quarters)
                }
                Return::Fresh(target) => {
                    delete::cancel_if_pending(a, &target);
                    a.viewer.forget_decode(&target);
                    a.viewer.open(&target);
                    Redraw::Full
                }
                Return::Empty => a.viewer.close(),
            }
        }
    };
    commands::after(app, hwnd, redraw);
    invalidate(hwnd);
}

#[cfg(test)]
mod tests {
    use lv_core::identity::FileIdentity;

    use super::*;

    fn item(name: &str) -> ResizeItem {
        let id = FileIdentity {
            volume_serial: 1,
            file_index: 2,
            size: 3,
            modified: 4,
        };
        ResizeItem::new(PathBuf::from(name), id, 0)
    }

    fn ready() -> Estimate {
        Estimate::Ready {
            size: (1, 1),
            bytes: 1,
        }
    }

    /// I1: a job still running when the screen closed must not be taken (and its bytes
    /// saved, R1) by the next session, whatever generation it carries.
    #[test]
    fn a_result_from_an_earlier_session_is_rejected() {
        let path = PathBuf::from("a.jpg");
        let mut state = ResizeState::default();
        state.open_session(None, 0, ResizeMemory::default());
        let s = state.session.as_mut().unwrap();
        s.add(vec![item("a.jpg")]);
        let first = s.generation;
        // A setting changed mid-session: a second generation.
        s.set_memory(ResizeMemory {
            long_edge: 640,
            ..s.memory
        });
        let second = s.generation;
        assert!(state.end_session().is_some());
        // Esc, then 크기 조정 again with the same photo.
        state.open_session(None, 0, ResizeMemory::default());
        let s = state.session.as_mut().unwrap();
        s.add(vec![item("a.jpg")]);
        assert!(s.generation > second);
        for stale in [first, second] {
            assert!(!s.apply_estimate(stale, 0, &path, None, ready()), "{stale}");
        }
        assert_eq!(s.items[0].estimate, Estimate::Pending);
        let now = s.generation;
        assert!(s.apply_estimate(now, 0, &path, None, ready()));
    }

    #[test]
    fn the_session_keeps_the_pending_rotation_it_was_opened_with() {
        let mut state = ResizeState::default();
        state.open_session(Some(PathBuf::from("c.jpg")), 3, ResizeMemory::default());
        let s = state.end_session().unwrap();
        assert_eq!(s.entered_from, Some(PathBuf::from("c.jpg")));
        assert_eq!(s.entered_quarters, 3);
        assert!(state.session.is_none());
    }
}
