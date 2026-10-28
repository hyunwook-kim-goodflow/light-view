//! Folder rescans: what a rescan reads from disk (off the UI thread) and how the viewer
//! applies the result.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::listing::{same_path, Entry, FolderList};

use super::{Redraw, RescanNotice, Viewer};

/// The disk side of a folder rescan, taken by `Viewer::rescan_job`. `run` may block
/// (a slow or stalled network folder), so it runs off the UI thread.
#[derive(Debug, Clone)]
pub struct RescanJob {
    pub folder: PathBuf,
    /// The viewer's generation and current photo when the job was taken: a result for
    /// another photo or folder is not applied.
    generation: u64,
    current: Option<PathBuf>,
    /// The shown photo and the prefetched ones, with the identity they were decoded from.
    checks: Vec<(PathBuf, FileIdentity)>,
}

/// What `RescanJob::run` found.
#[derive(Debug, Clone)]
pub struct RescanFound {
    folder: PathBuf,
    generation: u64,
    current: Option<PathBuf>,
    /// The folder's files with their sizes and times; `None` if it could not be read.
    paths: Option<Vec<Entry>>,
    /// The current photo is not listed and its metadata reads "not found".
    current_absent: bool,
    /// The checked files whose identity changed (`Verify::Changed` only: a file that
    /// cannot be read may be shared, locked or behind a network blip).
    changed: Vec<(PathBuf, FileIdentity)>,
    /// How long the run took.
    pub took: Duration,
}

impl RescanJob {
    /// Reads the folder and the identities. Blocks as long as the disk does.
    pub fn run(self) -> RescanFound {
        let started = Instant::now();
        let paths = FolderList::read_entries(&self.folder).ok();
        let current_absent = match (&paths, &self.current) {
            (Some(paths), Some(c)) => {
                !paths.iter().any(|(p, _)| same_path(p, c)) && confirmed_absent(c)
            }
            _ => false,
        };
        let changed = self
            .checks
            .into_iter()
            .filter(|(path, id)| identity::verify(path, id) == Verify::Changed)
            .collect();
        RescanFound {
            folder: self.folder,
            generation: self.generation,
            current: self.current,
            paths,
            current_absent,
            changed,
            took: started.elapsed(),
        }
    }

    /// A result that changes nothing (for a run that failed).
    pub fn nothing(&self) -> RescanFound {
        RescanFound {
            folder: self.folder.clone(),
            generation: self.generation,
            current: self.current.clone(),
            paths: None,
            current_absent: false,
            changed: Vec::new(),
            took: Duration::ZERO,
        }
    }
}

/// `path` is gone: its metadata reads "not found" (any other error may be passing).
fn confirmed_absent(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(to_extended_path(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

impl Viewer {
    /// What a rescan has to read from disk, taken on the UI thread; `RescanJob::run`
    /// reads it on another thread and `apply_rescan` applies the result here. `None`
    /// without a folder.
    pub fn rescan_job(&self) -> Option<RescanJob> {
        let current = self.list.current().map(Path::to_path_buf);
        let mut checks: Vec<(PathBuf, FileIdentity)> = Vec::new();
        if let (Some(s), Some(c)) = (&self.shown, &current) {
            if let Some(id) = s.identity.as_ref().filter(|_| same_path(&s.path, c)) {
                checks.push((s.path.clone(), id.clone()));
            }
        }
        for path in &self.wanted {
            if let Some((_, Some(id))) = self.prefetcher.get_with_identity(path) {
                if !checks.iter().any(|(p, i)| p == path && *i == id) {
                    checks.push((path.clone(), id));
                }
            }
        }
        Some(RescanJob {
            folder: self.list.dir()?.to_path_buf(),
            generation: self.generation,
            current,
            checks,
        })
    }

    /// Applies a rescan (see `RescanJob`). The current photo stays current (by path);
    /// pending deletes stay hidden. A current photo gone from the folder (not listed and
    /// confirmed absent) gives way to the one at its place, its unsaved rotation dropped:
    /// there is no file left to save it to. A current photo changed by another program
    /// is decoded again, unrotated (ruling R20: the notice only when a rotation was
    /// dropped), as is a changed prefetched neighbour. A file that only fails to read
    /// (`Verify::Missing`: shared, locked, a network blip) is left alone, and so is
    /// everything when the folder could not be read.
    ///
    /// `None` when the result is out of date (another folder or photo since the job was
    /// taken): nothing is applied and the caller rescans again.
    pub fn apply_rescan(&mut self, found: RescanFound) -> Option<(Redraw, Option<RescanNotice>)> {
        let current = self.list.current().map(Path::to_path_buf);
        let same_current = match (&found.current, &current) {
            (Some(a), Some(b)) => same_path(a, b),
            (None, None) => true,
            _ => false,
        };
        let same_folder = self.list.dir().is_some_and(|d| same_path(d, &found.folder));
        if !same_folder || found.generation != self.generation || !same_current {
            return None;
        }
        let Some(mut paths) = found.paths else {
            return Some((Redraw::None, None));
        };
        if let Some(c) = &current {
            if !paths.iter().any(|(p, _)| same_path(p, c)) && !found.current_absent {
                // Not listed this time, yet still there.
                paths.push((c.clone(), None));
            }
        }
        let rotated = self.unsaved_rotation();
        let position = self.list.position();
        self.list.refresh_entries(paths, current.as_deref());
        let stayed = match (&current, self.list.current()) {
            (Some(old), Some(new)) => same_path(old, new),
            (None, None) => true,
            _ => false,
        };
        if !stayed {
            self.quarters = 0;
            self.backward = false;
            let notice = (current.is_some() && rotated).then_some(RescanNotice::Vanished);
            let redraw = if self.list.current().is_none() {
                self.clear_shown()
            } else {
                self.after_move()
            };
            return Some((redraw, notice));
        }
        self.request_wanted();
        let reloaded = self.reload_if_changed(&found.changed);
        for (path, id) in &found.changed {
            if reloaded && current.as_ref().is_some_and(|c| same_path(c, path)) {
                continue; // already decoding again
            }
            // Only the decode that was checked: a newer one is of the changed file.
            let cached = self.prefetcher.get_with_identity(path);
            if cached.is_some_and(|(_, cached)| cached.as_ref() == Some(id)) {
                self.prefetcher.invalidate(path);
            }
        }
        if reloaded {
            return Some((Redraw::Full, rotated.then_some(RescanNotice::Reloaded)));
        }
        if self.list.position() != position {
            return Some((Redraw::Full, None));
        }
        Some((Redraw::None, None))
    }

    /// Takes and applies a rescan on this thread (tests).
    #[cfg(test)]
    pub fn rescan(&mut self) -> (Redraw, Option<RescanNotice>) {
        let Some(job) = self.rescan_job() else {
            return (Redraw::None, None);
        };
        self.apply_rescan(job.run())
            .expect("nothing changed in between")
    }

    /// The current photo on screen was changed by another program (its checked
    /// identity is in `changed`): it stays drawn until its new decode replaces it (saves
    /// and deletes refuse meanwhile, as its identity is the old file's), and its
    /// rotation is dropped. True if it changed.
    fn reload_if_changed(&mut self, changed: &[(PathBuf, FileIdentity)]) -> bool {
        let Some(current) = self.list.current().map(Path::to_path_buf) else {
            return false;
        };
        let Some(s) = self.shown.as_mut().filter(|s| same_path(&s.path, &current)) else {
            return false;
        };
        let checked = changed
            .iter()
            .any(|(p, id)| *p == s.path && s.identity.as_ref() == Some(id));
        if !checked {
            return false;
        }
        s.stale = true;
        self.prefetcher.invalidate(&s.path);
        self.quarters = 0;
        self.touched = false;
        self.show_current();
        true
    }
}
