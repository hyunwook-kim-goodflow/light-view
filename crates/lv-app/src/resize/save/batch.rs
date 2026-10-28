//! The photos one save replaced: committed (the backups go to the Recycle Bin) or undone.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lv_core::identity::{self, to_extended_path, FileIdentity, Verify};
use lv_core::listing::same_path;

use super::{is_hidden, keep_visible, move_no_replace, set_hidden, taken, UNDO_WAIT};
use crate::delete::{check_file, PendingDelete, Trash};

/// The photos one save replaced. Either committed when its toast goes or the app ends
/// (the backups go to the Recycle Bin) or undone; a batch only dropped leaves its
/// backups to [`recover_backups`](super::recover_backups) on a later start.
#[derive(Default)]
pub struct ReplaceBatch {
    items: Vec<Replaced>,
}

/// One replaced photo, with the identities its files had right after the save.
struct Replaced {
    result: PathBuf,
    result_id: Option<FileIdentity>,
    backup: PathBuf,
    backup_id: Option<FileIdentity>,
    original: PathBuf,
}

/// Why an undo did not fully restore a photo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoReason {
    /// The Recycle Bin did not take the resized file.
    ResultNotRecycled,
    /// The resized file changed after the save, so it stays.
    ResultChanged,
    /// Another file now has the original's name.
    NameTaken,
    /// The backup changed or is gone; it is left as it is.
    OriginalChanged,
}

/// A photo an undo did not fully restore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unrestored {
    /// The original's own path.
    pub original: PathBuf,
    pub reason: UndoReason,
    /// Where the original is now, visible, when it could not get its own name back
    /// (`<stem> (원본).<ext>`, [`keep_visible`](super::keep_visible)); `None` when it is back at `original`
    /// (only the resized file stays) or was left alone ([`UndoReason::OriginalChanged`]).
    pub kept_at: Option<PathBuf>,
}

/// A backup a commit could not send to the Recycle Bin: the original of `original`,
/// now visible at `kept_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unrecycled {
    pub original: PathBuf,
    pub kept_at: PathBuf,
}

/// How long a commit waits for the Recycle Bin to take the backups.
const COMMIT_WAIT: Duration = Duration::from_secs(10);

impl ReplaceBatch {
    pub fn new() -> ReplaceBatch {
        ReplaceBatch::default()
    }

    /// Adds a photo [`save_replace`](super::save_replace) just replaced: its result and backup (from
    /// [`ItemResult::Replaced`](super::ItemResult::Replaced)) and the original's path. Their identities are read now;
    /// commit and undo act only on files that still have them.
    pub fn push(&mut self, result: &Path, backup: &Path, original: &Path) {
        self.items.push(Replaced {
            result: result.to_path_buf(),
            result_id: identity::capture(result).ok(),
            backup: backup.to_path_buf(),
            backup_id: identity::capture(backup).ok(),
            original: original.to_path_buf(),
        });
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the resize screen's toast (M6 Task 6) shows it")
    )]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Sends the backups to the Recycle Bin through `trash` (which re-checks each) and
    /// waits up to 10 s for it. A backup still there afterwards is never left hidden: it
    /// is moved to a visible name beside its result ([`keep_visible`](super::keep_visible)) and returned.
    /// Blocks, so it runs off the UI thread.
    pub fn commit(self, trash: &dyn Trash) -> Vec<Unrecycled> {
        for item in &self.items {
            if let Some(id) = &item.backup_id {
                trash.recycle(PendingDelete {
                    path: item.backup.clone(),
                    id: id.clone(),
                });
            }
        }
        trash.flush(COMMIT_WAIT);
        self.items
            .iter()
            .filter(|item| still_there(&item.backup, item.backup_id.as_ref()))
            .map(|item| Unrecycled {
                original: item.original.clone(),
                kept_at: keep_visible(&item.backup, &item.original),
            })
            .collect()
    }

    /// Undoes the save (spec §10): every result unchanged since the save goes to the
    /// Recycle Bin, then every backup goes back under its original name if that name is
    /// free, with the original's hidden attribute. An original whose name is still taken
    /// is never sent to the Recycle Bin: it is moved to a visible name beside it
    /// ([`keep_visible`](super::keep_visible)). Returns every photo not fully restored. Blocks up to 10 s on
    /// the Recycle Bin, so it runs off the UI thread.
    pub fn undo(self, trash: &dyn Trash) -> Vec<Unrestored> {
        let mut sent = Vec::with_capacity(self.items.len());
        for item in &self.items {
            // The result has the original's hidden attribute (`replace` copied it).
            let hidden = is_hidden(&item.result);
            let id = item
                .result_id
                .as_ref()
                .filter(|id| check_file(&item.result, id).is_ok());
            if let Some(id) = id {
                trash.recycle(PendingDelete {
                    path: item.result.clone(),
                    id: id.clone(),
                });
            }
            sent.push((id.is_some(), hidden));
        }
        trash.flush(UNDO_WAIT);

        let mut failed = Vec::new();
        for (item, (sent, hidden)) in self.items.iter().zip(sent) {
            let result_stays = !sent || still_there(&item.result, item.result_id.as_ref());
            let result_reason = if sent {
                UndoReason::ResultNotRecycled
            } else {
                UndoReason::ResultChanged
            };
            if !still_there(&item.backup, item.backup_id.as_ref()) {
                failed.push(Unrestored {
                    original: item.original.clone(),
                    reason: UndoReason::OriginalChanged,
                    kept_at: None,
                });
                continue;
            }
            if !taken(&item.original) {
                let original = to_extended_path(&item.original);
                match move_no_replace(&to_extended_path(&item.backup), &original) {
                    Ok(()) => {
                        set_hidden(&original, hidden);
                        if result_stays {
                            failed.push(Unrestored {
                                original: item.original.clone(),
                                reason: result_reason,
                                kept_at: None,
                            });
                        }
                        continue;
                    }
                    Err(e) => eprintln!(
                        "light-view: {} could not get its name back: {e}",
                        item.original.display()
                    ),
                }
            }
            // The name is taken: by our own result when it has the original's name and
            // stayed, else by another file.
            let ours = result_stays && same_path(&item.result, &item.original);
            failed.push(Unrestored {
                original: item.original.clone(),
                reason: if ours {
                    result_reason
                } else {
                    UndoReason::NameTaken
                },
                kept_at: Some(keep_visible(&item.backup, &item.original)),
            });
        }
        failed
    }
}

/// Whether the file at `path` still has identity `id` (an unknown identity counts as
/// there, so nothing is assumed gone).
fn still_there(path: &Path, id: Option<&FileIdentity>) -> bool {
    id.is_none_or(|id| identity::verify(path, id) == Verify::Same)
}
