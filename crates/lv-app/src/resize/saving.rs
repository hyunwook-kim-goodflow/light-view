//! Running a resize save (spec §6, rulings R1, R2): one photo at a time on its own
//! thread, each result reported as it ends; then the replaced photos' batch, committed
//! or undone later, also off the UI thread (each may wait up to 10 s on the Recycle
//! Bin).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::resize::{ResizeSpec, Target};
use windows::Win32::Foundation::HWND;

use super::estimate::{produce, Maker, Outcome};
use super::save::batch::{ReplaceBatch, Unrecycled, Unrestored};
use super::save::{revert_replace, save_new, save_replace, FailReason, ItemResult, ReplaceRefusal};
use super::{add, close_session, BatchReport, Estimate, SaveProgress, WM_APP_RESIZE_SAVED};
use crate::delete::{ShellTrash, Trash};
use crate::instance;
use crate::window::{self, invalidate, App};

/// What is known about a photo's result when the save starts.
#[derive(Debug)]
pub enum Known {
    /// Its estimate's bytes, made for the save's settings (R1).
    Bytes(Vec<u8>),
    /// Its estimate found no result within the limit (the same settings, the same answer).
    CantFit,
    /// Nothing kept: made now, the same way an estimate is.
    Make,
}

/// One photo to save.
#[derive(Debug)]
pub struct SaveItem {
    pub path: PathBuf,
    pub id: FileIdentity,
    pub quarters: u8,
    /// Known to be an animation or opened at reduced size: never replaced (R3).
    pub keep_original: Option<ReplaceRefusal>,
    pub known: Known,
}

/// A whole save.
pub struct SaveJob {
    pub items: Vec<SaveItem>,
    pub spec: ResizeSpec,
    pub replace: bool,
    pub maker: Maker,
}

/// What the save and batch threads report.
pub enum Event {
    /// One photo is done.
    Item { path: PathBuf, result: ItemResult },
    /// Every photo is done; `batch` holds the replaced ones.
    Finished { batch: ReplaceBatch },
    /// A commit could not send these originals to the Recycle Bin; they are visible.
    Committed(Vec<Unrecycled>),
    /// An undo ended: these it could not fully restore, of the photos (original,
    /// result) it was for.
    Undone {
        left: Vec<Unrestored>,
        paths: Vec<(PathBuf, PathBuf)>,
    },
}

/// Saves every photo of `job` in order, reporting each through `send`, then the batch.
/// Runs on the save thread.
pub fn save_all(job: SaveJob, send: &dyn Fn(Event)) {
    let mut batch = ReplaceBatch::new();
    for item in job.items {
        let result = save_one(&item, &job.spec, job.replace, &job.maker);
        if let ItemResult::Replaced { result, backup } = &result {
            batch.push(result, backup, &item.path);
        }
        send(Event::Item {
            path: item.path,
            result,
        });
    }
    send(Event::Finished { batch });
}

fn save_one(item: &SaveItem, spec: &ResizeSpec, replace: bool, maker: &Maker) -> ItemResult {
    let mut keep_original = item.keep_original;
    let bytes = match &item.known {
        Known::CantFit => return ItemResult::CantFit,
        Known::Bytes(bytes) => bytes.clone(),
        Known::Make => match produce(&item.path, item.quarters, spec, maker) {
            (source, Outcome::Ready { bytes, .. }) => {
                keep_original =
                    keep_original.or(source.and_then(|s| refusal(s.animated, s.scaled)));
                bytes
            }
            (_, Outcome::CantFit) => return ItemResult::CantFit,
            (_, Outcome::Failed(e)) => return ItemResult::Failed(FailReason::Io(e)),
        },
    };
    let result = if replace {
        if let Some(why) = keep_original {
            return ItemResult::Failed(FailReason::ReplaceRefused(why));
        }
        save_replace(&item.path, &item.id, &bytes, spec.format)
    } else {
        // The bytes were made from the photo as it was when it joined the session.
        if identity::verify(&item.path, &item.id) != Verify::Same {
            return ItemResult::Failed(FailReason::Changed);
        }
        save_new(&item.path, &bytes, spec.format)
    };
    match spec.target {
        Target::MaxBytes { bytes: limit, .. } => check_written(result, &item.path, limit),
        _ => result,
    }
}

/// Why a photo that is an animation or was opened at reduced size is never replaced.
pub fn refusal(animated: bool, scaled: bool) -> Option<ReplaceRefusal> {
    if animated {
        Some(ReplaceRefusal::Animated)
    } else if scaled {
        Some(ReplaceRefusal::Scaled)
    } else {
        None
    }
}

/// Spec §5: the file as written is checked against the 용량 limit. One over it is taken
/// back (a new file deleted, a replace reverted to the original) and fails the photo.
fn check_written(result: ItemResult, original: &Path, limit: u64) -> ItemResult {
    let written = match &result {
        ItemResult::Saved(p) | ItemResult::Replaced { result: p, .. } => p,
        _ => return result,
    };
    let len = match std::fs::metadata(to_extended_path(written)) {
        Ok(m) => m.len(),
        Err(e) => {
            eprintln!("light-view: cannot check {}: {e}", written.display());
            return result;
        }
    };
    if len <= limit {
        return result;
    }
    eprintln!(
        "light-view: {} is {len} bytes, over {limit}",
        written.display()
    );
    match &result {
        ItemResult::Saved(p) if std::fs::remove_file(to_extended_path(p)).is_err() => {
            eprintln!("light-view: could not remove {}", p.display());
        }
        // The result could not be deleted: the replace stands (it joins the batch, so
        // its undo and commit still apply).
        ItemResult::Replaced { result: r, backup } if !revert_replace(r, backup, original) => {
            return result;
        }
        _ => {}
    }
    ItemResult::Failed(FailReason::OverLimit)
}

/// How a save went, for its summary (spec §6: 성공 N, 맞추기 불가 N, 실패 N).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveSummary {
    pub saved: usize,
    pub replaced: usize,
    pub cant_fit: usize,
    /// Each failed photo with its reason.
    pub failed: Vec<(PathBuf, FailReason)>,
}

impl SaveSummary {
    pub fn of(results: &[(PathBuf, ItemResult)]) -> SaveSummary {
        let mut s = SaveSummary::default();
        for (path, result) in results {
            match result {
                ItemResult::Saved(_) => s.saved += 1,
                ItemResult::Replaced { .. } => s.replaced += 1,
                ItemResult::CantFit => s.cant_fit += 1,
                ItemResult::Failed(r) => s.failed.push((path.clone(), r.clone())),
            }
        }
        s
    }
}

// ---- The save and its batch, in the app ------------------------------------------

/// [저장 (N장)]: saves every photo on a thread of its own, one at a time, with the kept
/// estimate bytes where there are some (R1). A replaced batch from before is committed
/// first. False when nothing started (no session, a save running, replace blocked).
pub fn start_save(app: &RefCell<App>, hwnd: HWND) -> bool {
    let Ok(mut guard) = app.try_borrow_mut() else {
        return false;
    };
    let a = &mut *guard;
    let maker = Maker {
        limits: a.decode_limits,
        pool: a.pool.clone(),
    };
    let state = &mut a.resize;
    let Some(s) = &state.session else {
        return false;
    };
    if state.saving.is_some() || a.saving.is_some() || (s.replace && state.replace_block.is_some())
    {
        return false;
    }
    if let Some(e) = &state.estimator {
        e.cancel();
    }
    let items: Vec<SaveItem> = s
        .items
        .iter()
        .map(|i| SaveItem {
            path: i.path.clone(),
            id: i.id.clone(),
            quarters: i.quarters,
            keep_original: refusal(i.animated, i.scaled),
            known: match i.estimate {
                Estimate::CantFit => Known::CantFit,
                Estimate::Ready { .. } => state
                    .cache
                    .take(&i.path, s.generation)
                    .map_or(Known::Make, Known::Bytes),
                Estimate::Pending | Estimate::Failed(_) => Known::Make,
            },
        })
        .collect();
    let job = SaveJob {
        items,
        spec: s.spec,
        replace: s.replace,
        maker,
    };
    state.saving = Some(SaveProgress {
        total: job.items.len(),
        ..SaveProgress::default()
    });
    let pending = state.batch.take();
    let trash = state.trash();
    let (tx, post) = (state.events_tx.clone(), state.hwnd.clone());
    drop(guard);
    // The previous replace is final now: its backups go to the Recycle Bin first.
    if let Some(batch) = pending {
        spawn_batch(
            app,
            move |trash| Event::Committed(batch.commit(trash)),
            trash,
        );
    }
    std::thread::spawn(move || {
        let send = |e: Event| {
            let _ = tx.send(e);
            post.post(WM_APP_RESIZE_SAVED);
        };
        let run = std::panic::AssertUnwindSafe(|| save_all(job, &send));
        if std::panic::catch_unwind(run).is_err() {
            // Backups left by a panic come back at the next start (R2's recovery).
            send(Event::Finished {
                batch: ReplaceBatch::new(),
            });
        }
    });
    invalidate(hwnd);
    true
}

/// `WM_APP_RESIZE_SAVED`: a photo saved, the save done, or a commit or undo finished.
pub fn on_saved(app: &RefCell<App>, hwnd: HWND) {
    loop {
        let Ok(event) = app.try_borrow().map(|a| a.resize.events_rx.try_recv()) else {
            return;
        };
        let Ok(event) = event else {
            return;
        };
        match event {
            Event::Item { path, result } => {
                if let Ok(mut a) = app.try_borrow_mut() {
                    if let Some(p) = &mut a.resize.saving {
                        p.results.push((path, result));
                    }
                }
                invalidate(hwnd);
            }
            Event::Finished { batch } => {
                finish_save(app, hwnd, batch);
                super::toasts::after_save(app, hwnd);
            }
            Event::Committed(kept) => report(app, hwnd, BatchReport::Unrecycled(kept)),
            Event::Undone { left, paths } => {
                super::toasts::undone(app, hwnd, &paths);
                report(app, hwnd, BatchReport::Unrestored(left));
            }
        }
    }
}

fn report(app: &RefCell<App>, hwnd: HWND, r: BatchReport) {
    let empty = match &r {
        BatchReport::Unrecycled(v) => v.is_empty(),
        BatchReport::Unrestored(v) => v.is_empty(),
    };
    if !empty {
        if let Ok(mut a) = app.try_borrow_mut() {
            a.resize.reports.push(r);
        }
        // Shown once no toast is up (never over "N장 대체 · 실행 취소").
        super::toasts::toast_check(app, hwnd);
        invalidate(hwnd);
    }
}

/// The save ended: its summary and batch are kept. When every photo was saved the
/// screen closes back to where it was opened (spec §6); otherwise it stays on the
/// photos not saved, each with its reason in the list. Photos that came meanwhile
/// join it (or open it again, spec §10).
fn finish_save(app: &RefCell<App>, hwnd: HWND, batch: ReplaceBatch) {
    let Some((results, close, queued)) = app.try_borrow_mut().ok().and_then(|mut a| {
        let progress = a.resize.saving.take()?;
        a.resize.last_save = Some(SaveSummary::of(&progress.results));
        a.resize.batch = (!batch.is_empty()).then_some(batch);
        let queued = std::mem::take(&mut a.resize.queued);
        Some((progress.results, progress.close_after, queued))
    }) else {
        return;
    };
    if close || !keep_unsaved(app, hwnd, &results) {
        close_session(app, hwnd, &results);
    }
    if close {
        window::close(app, hwnd);
        return;
    }
    if !queued.is_empty() {
        add(app, hwnd, queued);
    }
}

/// Keeps the session on the photos `results` did not save (spec §6 ruling). Photos
/// replaced under their own names are decoded again. False when none is left.
fn keep_unsaved(app: &RefCell<App>, hwnd: HWND, results: &[(PathBuf, ItemResult)]) -> bool {
    let kept = {
        let Ok(mut guard) = app.try_borrow_mut() else {
            return false;
        };
        let a = &mut *guard;
        let Some(s) = &mut a.resize.session else {
            return false;
        };
        if !s.keep_unsaved(results) {
            return false;
        }
        for (original, result) in results {
            if let ItemResult::Replaced { result, .. } = result {
                a.viewer.forget_decode(original);
                a.viewer.forget_decode(result);
            }
        }
        // Estimates of what is left (their kept bytes went to the save) come again.
        let left: Vec<usize> = (0..s.items.len()).collect();
        a.resize.schedule(&left);
        a.resize.refresh_block();
        true
    };
    if kept {
        super::show_current(app, hwnd);
        invalidate(hwnd);
    }
    kept
}

/// WM_CLOSE while a resize save runs: the window closes once it ends (a replace is
/// never cut in half). True when the close waits.
pub fn defer_close(app: &RefCell<App>) -> bool {
    let Ok(mut a) = app.try_borrow_mut() else {
        return false;
    };
    let Some(p) = &mut a.resize.saving else {
        return false;
    };
    p.close_after = true;
    instance::mark_closing();
    true
}

/// The "N장 대체 · 실행 취소" toast went: the replaced originals go to the Recycle Bin,
/// on a thread of its own (it may wait up to 10 s).
pub fn commit_batch(app: &RefCell<App>) {
    let Some((batch, trash)) = app.try_borrow_mut().ok().and_then(|mut a| {
        let batch = a.resize.batch.take()?;
        Some((batch, a.resize.trash()))
    }) else {
        return;
    };
    spawn_batch(
        app,
        move |trash| Event::Committed(batch.commit(trash)),
        trash,
    );
}

/// [실행 취소] on that toast: every replaced photo comes back (spec §10), off the UI
/// thread; what could not be restored is reported (`reports`).
pub fn undo_batch(app: &RefCell<App>) {
    let Some((batch, trash)) = app.try_borrow_mut().ok().and_then(|mut a| {
        let batch = a.resize.batch.take()?;
        Some((batch, a.resize.trash()))
    }) else {
        return;
    };
    let paths = batch.paths();
    spawn_batch(
        app,
        move |trash| Event::Undone {
            left: batch.undo(trash),
            paths,
        },
        trash,
    );
}

fn spawn_batch(
    app: &RefCell<App>,
    work: impl FnOnce(&dyn Trash) -> Event + Send + 'static,
    trash: Arc<ShellTrash>,
) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let (tx, post) = (a.resize.events_tx.clone(), a.resize.hwnd.clone());
    a.resize.batch_threads.retain(|t| !t.is_finished());
    a.resize.batch_threads.push(std::thread::spawn(move || {
        let _ = tx.send(work(&*trash));
        post.post(WM_APP_RESIZE_SAVED);
    }));
}

/// At exit (and when Windows ends the session): waits for running commits and undos,
/// then commits the batch still waiting for its toast. The window is already gone or
/// going, so this may block (up to 10 s each, `ReplaceBatch`).
pub fn flush_at_exit(app: &RefCell<App>) {
    let Ok((batch, threads, trash)) = app.try_borrow_mut().map(|mut a| {
        let batch = a.resize.batch.take();
        let threads = std::mem::take(&mut a.resize.batch_threads);
        let trash = batch.as_ref().map(|_| a.resize.trash());
        (batch, threads, trash)
    }) else {
        return;
    };
    for t in threads {
        let _ = t.join();
    }
    if let (Some(batch), Some(trash)) = (batch, trash) {
        for kept in batch.commit(&*trash) {
            eprintln!(
                "light-view: the original of {} is kept at {}",
                kept.original.display(),
                kept.kept_at.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;

    use lv_core::decode::DecodeLimits;
    use lv_core::identity::capture;
    use lv_core::resize::{OutputFormat, Quality, Target};

    use super::*;

    fn png(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(40, 20, image::Rgba([10, 200, 30, 255]))
            .save(&path)
            .unwrap();
        path
    }

    fn job(items: Vec<SaveItem>, replace: bool) -> SaveJob {
        SaveJob {
            items,
            spec: ResizeSpec {
                target: Target::LongEdge(20),
                format: OutputFormat::Png,
                quality: Quality::MatchOriginal,
                strip_gps: true,
            },
            replace,
            maker: Maker {
                limits: DecodeLimits::default(),
                pool: None,
            },
        }
    }

    fn item(path: &Path, known: Known) -> SaveItem {
        SaveItem {
            path: path.to_path_buf(),
            id: capture(path).unwrap(),
            quarters: 0,
            keep_original: None,
            known,
        }
    }

    fn run(job: SaveJob) -> (Vec<(PathBuf, ItemResult)>, ReplaceBatch) {
        let results = RefCell::new(Vec::new());
        let batch = RefCell::new(None);
        save_all(job, &|e| match e {
            Event::Item { path, result } => results.borrow_mut().push((path, result)),
            Event::Finished { batch: b } => *batch.borrow_mut() = Some(b),
            _ => unreachable!(),
        });
        (results.into_inner(), batch.into_inner().expect("finished"))
    }

    #[test]
    fn save_writes_the_estimate_bytes() {
        let dir = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
        let (a, b, c) = (
            png(dir.path(), "a.png"),
            png(dir.path(), "b.png"),
            png(dir.path(), "c.png"),
        );
        let kept = b"the estimate's bytes".to_vec();
        let (results, batch) = run(job(
            vec![
                item(&a, Known::Bytes(kept.clone())),
                item(&b, Known::Make),
                item(&c, Known::CantFit),
            ],
            false,
        ));
        assert!(batch.is_empty());
        // R1: the kept bytes are what is written, unchanged.
        let a_small = dir.path().join("a_small.png");
        assert_eq!(results[0], (a.clone(), ItemResult::Saved(a_small.clone())));
        assert_eq!(std::fs::read(&a_small).unwrap(), kept);
        // Without kept bytes the result is made now.
        let b_small = dir.path().join("b_small.png");
        assert_eq!(results[1].1, ItemResult::Saved(b_small.clone()));
        let img = image::open(&b_small).unwrap();
        assert_eq!((img.width(), img.height()), (20, 10));
        assert_eq!(results[2], (c, ItemResult::CantFit));
        assert!(!dir.path().join("c_small.png").exists());
        let summary = SaveSummary::of(&results);
        assert_eq!(
            (summary.saved, summary.cant_fit, summary.failed.len()),
            (2, 1, 0)
        );
    }

    #[test]
    fn a_changed_photo_or_a_kept_original_is_not_saved() {
        let dir = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
        let a = png(dir.path(), "a.png");
        let changed = item(&a, Known::Bytes(b"kept from the estimate".to_vec()));
        std::fs::write(&a, b"changed since it joined").unwrap();
        let (results, _) = run(job(vec![changed], false));
        assert_eq!(results[0].1, ItemResult::Failed(FailReason::Changed));
        // An animation (or a photo opened at reduced size) is never replaced (R3).
        let b = png(dir.path(), "b.png");
        let mut anim = item(&b, Known::Bytes(vec![1, 2, 3]));
        anim.keep_original = Some(ReplaceRefusal::Animated);
        let (results, batch) = run(job(vec![anim], true));
        assert_eq!(
            results[0].1,
            ItemResult::Failed(FailReason::ReplaceRefused(ReplaceRefusal::Animated))
        );
        assert!(batch.is_empty());
        assert!(b.exists());
    }

    #[test]
    fn a_written_file_over_the_limit_is_taken_back() {
        let dir = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
        let limit = |job: &mut SaveJob| {
            job.spec.target = Target::MaxBytes {
                bytes: 10,
                long_edge: None,
            };
        };
        // A new file: deleted again.
        let a = png(dir.path(), "a.png");
        let mut new = job(vec![item(&a, Known::Bytes(vec![7; 11]))], false);
        limit(&mut new);
        let (results, _) = run(new);
        assert_eq!(results[0].1, ItemResult::Failed(FailReason::OverLimit));
        assert!(!dir.path().join("a_small.png").exists());
        // A replace: the original is back under its name, with its own bytes.
        let b = png(dir.path(), "b.png");
        let before = std::fs::read(&b).unwrap();
        let mut replace = job(vec![item(&b, Known::Bytes(vec![7; 11]))], true);
        limit(&mut replace);
        let (results, batch) = run(replace);
        assert_eq!(results[0].1, ItemResult::Failed(FailReason::OverLimit));
        assert!(batch.is_empty());
        assert_eq!(std::fs::read(&b).unwrap(), before);
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.png", "b.png"], "no backup or result left");
        // Within the limit: kept.
        let mut fits = job(vec![item(&a, Known::Bytes(vec![7; 10]))], false);
        limit(&mut fits);
        assert!(matches!(run(fits).0[0].1, ItemResult::Saved(_)));
    }

    #[test]
    fn replaced_photos_form_the_batch() {
        let dir = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
        let a = png(dir.path(), "a.png");
        let (results, batch) = run(job(vec![item(&a, Known::Make)], true));
        let ItemResult::Replaced { result, backup } = &results[0].1 else {
            panic!("{:?}", results[0]);
        };
        assert_eq!(result, &a);
        assert!(backup.exists());
        assert_eq!(batch.len(), 1);
        assert_eq!(SaveSummary::of(&results).replaced, 1);
        // Undone, the original is back (the fake bin takes the result).
        let trash = crate::delete::fake::FakeTrash::new(dir.path(), true);
        assert!(batch.undo(&trash).is_empty());
        assert!(!backup.exists());
        let img = image::open(&a).unwrap();
        assert_eq!((img.width(), img.height()), (40, 20));
    }
}
