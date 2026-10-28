//! The resize session's state (spec §3–§6, rulings R1, R3, R4): the photos, the one
//! shown, the settings and each photo's estimate. Pure: no Win32, no threads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lv_core::identity::FileIdentity;
use lv_core::listing::same_path;
use lv_core::resize::{OutputFormat, Quality, ResizeSpec, Target};
use lv_core::settings::{ResizeAxis, ResizeFormat, ResizeMemory, ResizeTab, MIN_QUALITY};

use super::save::{FailReason, ItemResult};
use super::thumb::Thumb;

/// One megabyte in the 용량 tab: decimal, so a site's "5 MB" is met whichever way it
/// counts.
const MB: u64 = 1_000_000;

/// What the result of one photo will be, as far as is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Estimate {
    /// Not computed yet for the current settings.
    Pending,
    /// The result's size and its exact length in bytes (the bytes a save writes, R1).
    Ready { size: (u32, u32), bytes: u64 },
    /// No result meets the 용량 limit ("맞추기 불가"); left out of the save.
    CantFit,
    /// The photo could not be read or encoded.
    Failed(String),
}

/// One photo of the session.
#[derive(Clone, Debug)]
pub struct ResizeItem {
    pub path: PathBuf,
    /// The file as it was when it joined the session; saves act only on that file.
    pub id: FileIdentity,
    /// The size the result is made from (oriented, turned by `quarters`); (0, 0) until
    /// the photo was first decoded.
    pub src_size: (u32, u32),
    pub src_bytes: u64,
    /// An animation: only its first frame is used ("애니메이션 → 첫 장면").
    pub animated: bool,
    /// Opened at reduced size (over the memory limit): that size is the original.
    pub scaled: bool,
    pub estimate: Estimate,
    /// Clockwise quarter turns of an unsaved viewer rotation, applied to the result's
    /// pixels (spec §10); the original is not touched.
    pub quarters: u8,
    /// The filmstrip's tile, once the photo was first decoded.
    pub thumb: Option<Arc<Thumb>>,
    /// Why the last save did not save this photo (spec §6: the reason stays in the list).
    pub save_error: Option<FailReason>,
}

impl ResizeItem {
    pub fn new(path: PathBuf, id: FileIdentity, quarters: u8) -> ResizeItem {
        let src_bytes = id.size;
        ResizeItem {
            path,
            id,
            src_size: (0, 0),
            src_bytes,
            animated: false,
            scaled: false,
            estimate: Estimate::Pending,
            quarters: quarters % 4,
            thumb: None,
            save_error: None,
        }
    }
}

/// What decoding a photo told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub size: (u32, u32),
    pub bytes: u64,
    pub animated: bool,
    pub scaled: bool,
    /// Its filmstrip tile.
    pub thumb: Option<Arc<Thumb>>,
}

/// The resize screen's state while it is open.
#[derive(Debug)]
pub struct ResizeSession {
    pub items: Vec<ResizeItem>,
    /// Index into `items` of the photo shown.
    pub current: usize,
    /// What `memory` asks for, as the encoder takes it.
    pub spec: ResizeSpec,
    /// 원본 대체; always false when a session starts (spec §8).
    pub replace: bool,
    /// The photo the viewer showed before the session (none: the empty screen).
    pub entered_from: Option<PathBuf>,
    /// The view rotation entered_from had pending (clockwise quarters): given back
    /// to the viewer when the screen closes without replacing it (M6 ruling).
    pub entered_quarters: u8,
    /// A save (or an undo of one) rewrote the photo at `entered_from` while the screen
    /// stayed open: leaving opens it fresh, never restoring an old decode or rotation.
    pub entered_rewritten: bool,
    /// The sidebar's values, every tab's; saved to the settings as they change.
    pub memory: ResizeMemory,
    /// Bumped whenever `spec` changes: estimates of another generation are stale. Never
    /// reused across sessions (`ResizeState` hands out the first one).
    pub generation: u64,
}

impl ResizeSession {
    /// A session whose first generation is `generation`.
    pub fn new(
        entered_from: Option<PathBuf>,
        entered_quarters: u8,
        memory: ResizeMemory,
        generation: u64,
    ) -> ResizeSession {
        ResizeSession {
            items: Vec::new(),
            current: 0,
            spec: spec_for(&memory),
            replace: false,
            entered_from,
            entered_quarters: entered_quarters % 4,
            entered_rewritten: false,
            memory,
            generation,
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.items.iter().any(|i| same_path(&i.path, path))
    }

    /// Adds the photos not in the session yet, in order; returns the indices added.
    pub fn add(&mut self, items: Vec<ResizeItem>) -> Vec<usize> {
        let mut added = Vec::new();
        for item in items {
            if !self.contains(&item.path) {
                added.push(self.items.len());
                self.items.push(item);
            }
        }
        added
    }

    /// New sidebar values. True when the result changes (the spec did): every estimate is
    /// pending again under a new generation.
    pub fn set_memory(&mut self, memory: ResizeMemory) -> bool {
        self.memory = memory;
        let spec = spec_for(&memory);
        if spec == self.spec {
            return false;
        }
        self.spec = spec;
        self.generation += 1;
        for item in &mut self.items {
            item.estimate = Estimate::Pending;
            item.save_error = None;
        }
        true
    }

    /// After a save (spec §6): the photos it saved leave the session; those it did
    /// not save stay, each with why ("맞추기 불가", or the failure in `save_error`). The
    /// first left is shown. True when any stayed.
    pub fn keep_unsaved(&mut self, results: &[(PathBuf, ItemResult)]) -> bool {
        self.items.retain_mut(|item| {
            let result = results
                .iter()
                .find(|(p, _)| same_path(p, &item.path))
                .map(|(_, r)| r);
            match result {
                Some(ItemResult::Saved(_) | ItemResult::Replaced { .. }) => false,
                Some(ItemResult::CantFit) => {
                    item.estimate = Estimate::CantFit;
                    item.save_error = None;
                    true
                }
                Some(ItemResult::Failed(reason)) => {
                    item.save_error = Some(reason.clone());
                    true
                }
                None => true,
            }
        });
        self.current = 0;
        // Back to the photo opened from: under its new name if the save replaced it.
        let replaced = self.entered_from.as_deref().and_then(|from| {
            results.iter().find_map(|(original, r)| match r {
                ItemResult::Replaced { result, .. } if same_path(original, from) => {
                    Some(result.clone())
                }
                _ => None,
            })
        });
        if let Some(result) = replaced {
            self.entered_from = Some(result);
            self.entered_quarters = 0;
            self.entered_rewritten = true;
        }
        !self.items.is_empty()
    }

    /// An undo of a replace ended while the screen is open: a photo it brought back is
    /// where Esc goes, opened fresh (`paths` pairs each original with its result).
    pub fn undo_entered(&mut self, paths: &[(PathBuf, PathBuf)]) {
        let Some(from) = &self.entered_from else {
            return;
        };
        if let Some((original, _)) = paths.iter().find(|(_, r)| same_path(r, from)) {
            self.entered_from = Some(original.clone());
            self.entered_quarters = 0;
            self.entered_rewritten = true;
        }
    }

    /// How the viewer comes back when the screen closes after `results` (empty for
    /// Esc): the photo opened from, with its pending rotation, unless a save rewrote
    /// it (then it is opened fresh, under its new name if it has one).
    pub fn return_plan(&self, results: &[(PathBuf, ItemResult)]) -> Return {
        let Some(target) = return_target(self.entered_from.as_deref(), results) else {
            return Return::Empty;
        };
        let rewritten = self.entered_rewritten
            || results.iter().any(|(original, r)| {
                matches!(r, ItemResult::Replaced { .. }) && same_path(original, &target)
            });
        if self.entered_from.as_deref() == Some(target.as_path()) && !rewritten {
            Return::Restore(target, self.entered_quarters)
        } else {
            Return::Fresh(target)
        }
    }

    /// An estimate for `index` made under `generation`: taken unless it is stale (another
    /// generation, or not that photo). True if taken.
    pub fn apply_estimate(
        &mut self,
        generation: u64,
        index: usize,
        path: &Path,
        source: Option<Source>,
        estimate: Estimate,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(item) = self.items.get_mut(index).filter(|i| i.path == path) else {
            return false;
        };
        if let Some(s) = source {
            item.src_size = s.size;
            item.src_bytes = s.bytes;
            item.animated = s.animated;
            item.scaled = s.scaled;
            // The tile only changes with `quarters`, fixed per photo: the first one stays
            // (its bitmap is not made again on every setting change).
            if item.thumb.is_none() {
                item.thumb = s.thumb;
            }
        }
        item.estimate = estimate;
        true
    }

    /// Why 원본 대체 is not offered (R3), or `None` when it is. `has_bin` answers for a
    /// photo's path whether its Recycle Bin can take the files a replace makes.
    pub fn replace_block(&self, has_bin: &dyn Fn(&Path) -> bool) -> Option<ReplaceBlock> {
        if self.items.iter().any(|i| i.animated) {
            return Some(ReplaceBlock::Animated);
        }
        if self.items.iter().any(|i| i.scaled) {
            return Some(ReplaceBlock::Scaled);
        }
        if self.items.iter().any(|i| !has_bin(&i.path)) {
            return Some(ReplaceBlock::NoRecycleBin);
        }
        None
    }
}

/// Why 원본 대체 is not offered (ruling R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceBlock {
    /// A photo is where no Recycle Bin can hold its original (a NAS, say).
    NoRecycleBin,
    /// A photo is an animation (only its first frame is kept).
    Animated,
    /// A photo was opened at reduced size.
    Scaled,
}

/// The encoder's settings for the sidebar's values.
pub fn spec_for(memory: &ResizeMemory) -> ResizeSpec {
    let px = memory.long_edge.max(1);
    let target = match memory.tab {
        ResizeTab::Resolution => match memory.axis {
            ResizeAxis::LongEdge => Target::LongEdge(px),
            ResizeAxis::Width => Target::Width(px),
            ResizeAxis::Height => Target::Height(px),
        },
        ResizeTab::FileSize => Target::MaxBytes {
            bytes: u64::from(memory.max_mb_tenths.max(1)) * (MB / 10),
            long_edge: memory.limit_long_edge.map(|e| e.max(1)),
        },
        ResizeTab::Scale => Target::Percent(memory.percent.clamp(1, 100)),
    };
    ResizeSpec {
        target,
        format: match memory.format {
            ResizeFormat::Jpeg => OutputFormat::Jpeg,
            ResizeFormat::Png => OutputFormat::Png,
            ResizeFormat::WebP => OutputFormat::WebP,
        },
        quality: memory.quality.map_or(Quality::MatchOriginal, |q| {
            Quality::Fixed(q.clamp(MIN_QUALITY, 100))
        }),
        strip_gps: memory.strip_gps,
    }
}

/// The order estimates are made in: the shown photo first, then the ones after it, then
/// the ones before (spec §5).
pub fn estimate_order(current: usize, count: usize) -> Vec<usize> {
    let start = if current < count { current } else { 0 };
    (start..count).chain(0..start).collect()
}

/// What a drop of photos does (spec §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAction {
    /// One photo (or a folder) in the viewer: opened as before.
    Open,
    /// Several photos in the viewer: "보기 / 크기 조정".
    Ask,
    /// The resize screen is open: added to it.
    Add,
}

/// `count` photo files dropped, while the resize screen is open (`in_resize`) or not.
pub fn drop_action(count: usize, in_resize: bool) -> DropAction {
    match (in_resize, count) {
        (true, _) => DropAction::Add,
        (false, 0 | 1) => DropAction::Open,
        (false, _) => DropAction::Ask,
    }
}

/// How the viewer comes back when the screen closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Return {
    /// No photo was shown: the empty screen.
    Empty,
    /// The photo opened from, with its pending rotation (no prompt).
    Restore(PathBuf, u8),
    /// A photo a save rewrote: opened again from the file.
    Fresh(PathBuf),
}

/// Where the viewer goes once a save ends (spec §6): the photo it showed before, or the
/// result that replaced it under a new name; `None` (the empty screen) if it showed
/// none. `results` pairs each saved photo's path with how it ended.
pub fn return_target(
    entered_from: Option<&Path>,
    results: &[(PathBuf, ItemResult)],
) -> Option<PathBuf> {
    let from = entered_from?;
    let replaced = results.iter().find_map(|(original, result)| match result {
        ItemResult::Replaced { result, .. } if same_path(original, from) => Some(result.clone()),
        _ => None,
    });
    Some(replaced.unwrap_or_else(|| from.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> FileIdentity {
        FileIdentity {
            volume_serial: 1,
            file_index: n,
            size: 1000 + n,
            modified: 7,
        }
    }

    fn item(name: &str) -> ResizeItem {
        ResizeItem::new(PathBuf::from(name), id(name.len() as u64), 0)
    }

    fn session(names: &[&str]) -> ResizeSession {
        let mut s = ResizeSession::new(None, 0, ResizeMemory::default(), 1);
        s.add(names.iter().map(|n| item(n)).collect());
        s
    }

    fn ready() -> Estimate {
        Estimate::Ready {
            size: (10, 10),
            bytes: 100,
        }
    }

    #[test]
    fn drop_of_two_asks_view_or_resize() {
        assert_eq!(drop_action(1, false), DropAction::Open);
        assert_eq!(drop_action(0, false), DropAction::Open);
        assert_eq!(drop_action(2, false), DropAction::Ask);
        assert_eq!(drop_action(17, false), DropAction::Ask);
        for n in [0, 1, 2, 30] {
            assert_eq!(drop_action(n, true), DropAction::Add, "{n}");
        }
    }

    #[test]
    fn estimates_current_first() {
        assert_eq!(estimate_order(2, 5), vec![2, 3, 4, 0, 1]);
        assert_eq!(estimate_order(0, 3), vec![0, 1, 2]);
        assert_eq!(estimate_order(4, 5), vec![4, 0, 1, 2, 3]);
        assert_eq!(estimate_order(0, 0), Vec::<usize>::new());
        // A stale index (past the end) starts at the beginning.
        assert_eq!(estimate_order(9, 2), vec![0, 1]);
    }

    #[test]
    fn spec_change_cancels_stale_estimates() {
        let mut s = session(&["a.jpg", "bb.jpg"]);
        let old = s.generation;
        let p = PathBuf::from("a.jpg");
        assert!(s.apply_estimate(old, 0, &p, None, ready()));
        assert_eq!(s.items[0].estimate, ready());
        // The same values again: nothing changes, the estimate stays.
        assert!(!s.set_memory(s.memory));
        assert_eq!(s.generation, old);
        // Another long edge: a new generation, every estimate pending again.
        let memory = ResizeMemory {
            long_edge: 800,
            ..s.memory
        };
        assert!(s.set_memory(memory));
        assert_ne!(s.generation, old);
        assert!(s.items.iter().all(|i| i.estimate == Estimate::Pending));
        // A result computed for the old settings is dropped.
        assert!(!s.apply_estimate(old, 0, &p, None, ready()));
        assert_eq!(s.items[0].estimate, Estimate::Pending);
        // So is one for another photo at that index, or past the end.
        let gen = s.generation;
        assert!(!s.apply_estimate(gen, 0, Path::new("bb.jpg"), None, ready()));
        assert!(!s.apply_estimate(gen, 5, &p, None, ready()));
        // The current one is taken, with what decoding told about the photo.
        let source = Source {
            size: (4032, 3024),
            bytes: 5_000_000,
            animated: true,
            scaled: false,
            thumb: None,
        };
        assert!(s.apply_estimate(gen, 0, &p, Some(source), Estimate::CantFit));
        assert_eq!(s.items[0].estimate, Estimate::CantFit);
        assert_eq!(s.items[0].src_size, (4032, 3024));
        assert!(s.items[0].animated);
        // Only the strip-GPS choice changed: the bytes differ, so it is a new spec too.
        let memory = ResizeMemory {
            strip_gps: false,
            ..s.memory
        };
        assert!(s.set_memory(memory));
        // Another tab's number does not change the result.
        let memory = ResizeMemory {
            percent: 10,
            ..s.memory
        };
        let gen = s.generation;
        assert!(!s.set_memory(memory));
        assert_eq!(s.generation, gen);
        assert_eq!(s.memory.percent, 10);
    }

    #[test]
    fn add_skips_photos_already_there() {
        let mut s = session(&["a.jpg"]);
        let added = s.add(vec![item("A.JPG"), item("b.jpg"), item("b.jpg")]);
        assert_eq!(added, vec![1]);
        assert_eq!(s.items.len(), 2);
        assert!(s.contains(Path::new("B.jpg")));
        assert!(!s.replace, "a session starts with 새 파일");
    }

    #[test]
    fn save_done_returns_to_entry() {
        let a = PathBuf::from(r"C:\p\a.heic");
        let b = PathBuf::from(r"C:\p\b.jpg");
        let saved = vec![
            (
                b.clone(),
                ItemResult::Saved(PathBuf::from(r"C:\p\b_small.jpg")),
            ),
            (a.clone(), ItemResult::CantFit),
        ];
        // Back to the photo shown before, or the empty screen.
        assert_eq!(return_target(Some(&b), &saved), Some(b.clone()));
        assert_eq!(return_target(None, &saved), None);
        // Replaced under a new name (IMG.HEIC → IMG.jpg): the result is shown.
        let replaced = vec![(
            a.clone(),
            ItemResult::Replaced {
                result: PathBuf::from(r"C:\p\a.jpg"),
                backup: PathBuf::from(r"C:\p\.a.heic.lvbak-1-0"),
            },
        )];
        assert_eq!(
            return_target(Some(Path::new(r"c:\P\A.HEIC")), &replaced),
            Some(PathBuf::from(r"C:\p\a.jpg"))
        );
        assert_eq!(return_target(Some(&b), &replaced), Some(b));
    }

    /// I5 (ruling): after a save, the saved photos leave; failed and "맞추기 불가" ones
    /// stay with their reasons, the first shown; all saved leaves nothing.
    #[test]
    fn a_save_keeps_only_the_photos_it_did_not_save() {
        let mut s = session(&["a.jpg", "bb.jpg", "ccc.jpg", "dddd.jpg"]);
        s.entered_from = Some(PathBuf::from("a.jpg"));
        s.entered_quarters = 1;
        s.current = 3;
        s.items[3].estimate = ready();
        let results = vec![
            (
                PathBuf::from("a.jpg"),
                ItemResult::Replaced {
                    result: PathBuf::from("a.png"),
                    backup: PathBuf::from(".a.jpg.lvbak-1-0"),
                },
            ),
            (PathBuf::from("bb.jpg"), ItemResult::CantFit),
            (
                PathBuf::from("ccc.jpg"),
                ItemResult::Saved(PathBuf::from("ccc_small.jpg")),
            ),
            (
                PathBuf::from("dddd.jpg"),
                ItemResult::Failed(FailReason::NameTaken),
            ),
        ];
        assert!(s.keep_unsaved(&results));
        let left: Vec<_> = s.items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(left, [PathBuf::from("bb.jpg"), PathBuf::from("dddd.jpg")]);
        assert_eq!(s.current, 0);
        assert_eq!(s.items[0].estimate, Estimate::CantFit);
        assert_eq!(s.items[0].save_error, None);
        assert_eq!(s.items[1].save_error, Some(FailReason::NameTaken));
        // The photo opened from was replaced: Esc goes back to its new name, plain.
        assert_eq!(s.entered_from, Some(PathBuf::from("a.png")));
        assert_eq!(s.entered_quarters, 0);
        // A new setting clears the old reasons.
        let m = ResizeMemory {
            long_edge: 800,
            ..s.memory
        };
        assert!(s.set_memory(m));
        assert_eq!(s.items[1].save_error, None);
        // Everything saved: nothing stays.
        let mut s = session(&["a.jpg"]);
        let all = vec![(
            PathBuf::from("a.jpg"),
            ItemResult::Saved(PathBuf::from("a_small.jpg")),
        )];
        assert!(!s.keep_unsaved(&all));
        assert!(s.items.is_empty());
    }

    fn replaced(result: &str) -> ItemResult {
        ItemResult::Replaced {
            result: PathBuf::from(result),
            backup: PathBuf::from(".x.lvbak-1-0"),
        }
    }

    /// Re-review r1: a save that keeps the screen open and rewrote the photo opened
    /// from must make Esc open it fresh, never restore an old decode or rotation.
    #[test]
    fn esc_after_a_kept_open_save_opens_the_rewritten_photo_fresh() {
        let fail = ItemResult::Failed(FailReason::NameTaken);
        // Nothing saved yet: Esc restores the pending rotation.
        let mut s = session(&["a.jpg", "bb.jpg"]);
        s.entered_from = Some(PathBuf::from("a.jpg"));
        s.entered_quarters = 3;
        assert_eq!(
            s.return_plan(&[]),
            Return::Restore(PathBuf::from("a.jpg"), 3)
        );
        // Case (b): replaced under the same name, bb.jpg failed, the screen stays.
        let results = vec![
            (PathBuf::from("a.jpg"), replaced("a.jpg")),
            (PathBuf::from("bb.jpg"), fail.clone()),
        ];
        assert!(s.keep_unsaved(&results));
        assert_eq!(s.return_plan(&[]), Return::Fresh(PathBuf::from("a.jpg")));
        // Under a new name: Esc opens the result.
        let mut s = session(&["a.heic", "bb.jpg"]);
        s.entered_from = Some(PathBuf::from("a.heic"));
        let results = vec![
            (PathBuf::from("a.heic"), replaced("a.jpg")),
            (PathBuf::from("bb.jpg"), fail),
        ];
        assert!(s.keep_unsaved(&results));
        assert_eq!(s.return_plan(&[]), Return::Fresh(PathBuf::from("a.jpg")));
        // Case (a): [실행 취소] while the screen is open removes the result and brings
        // the original back: Esc goes there, fresh.
        s.undo_entered(&[(PathBuf::from("a.heic"), PathBuf::from("a.jpg"))]);
        assert_eq!(s.entered_from, Some(PathBuf::from("a.heic")));
        assert_eq!(s.return_plan(&[]), Return::Fresh(PathBuf::from("a.heic")));
        // An undo of other photos leaves it alone.
        s.undo_entered(&[(PathBuf::from("c.png"), PathBuf::from("c.jpg"))]);
        assert_eq!(s.entered_from, Some(PathBuf::from("a.heic")));
        // No photo was shown: the empty screen.
        assert_eq!(session(&["a.jpg"]).return_plan(&[]), Return::Empty);
    }

    /// Re-review r1 minor: a photo's tile is made once; later estimates keep it.
    #[test]
    fn a_photo_keeps_its_first_tile() {
        let mut s = session(&["a.jpg"]);
        let p = PathBuf::from("a.jpg");
        let tile =
            |n: u8| super::super::thumb::thumbnail(&[n, n, n, 255], (1, 1), 64).map(Arc::new);
        let source = |thumb| Source {
            size: (1, 1),
            bytes: 1,
            animated: false,
            scaled: false,
            thumb,
        };
        let first = tile(1);
        let gen = s.generation;
        assert!(s.apply_estimate(gen, 0, &p, Some(source(first.clone())), ready()));
        s.set_memory(ResizeMemory {
            long_edge: 640,
            ..s.memory
        });
        let gen = s.generation;
        assert!(s.apply_estimate(gen, 0, &p, Some(source(tile(2))), ready()));
        assert_eq!(s.items[0].thumb, first);
    }

    #[test]
    fn spec_follows_the_tab() {
        let m = ResizeMemory::default();
        let spec = spec_for(&m);
        assert_eq!(spec.target, Target::LongEdge(1920));
        assert_eq!(spec.format, OutputFormat::Jpeg);
        assert_eq!(spec.quality, Quality::MatchOriginal);
        assert!(spec.strip_gps);
        let m = ResizeMemory {
            axis: ResizeAxis::Height,
            long_edge: 1000,
            format: ResizeFormat::WebP,
            quality: Some(30),
            strip_gps: false,
            ..m
        };
        let spec = spec_for(&m);
        assert_eq!(spec.target, Target::Height(1000));
        assert_eq!(spec.format, OutputFormat::WebP);
        // Never under the quality floor (§10).
        assert_eq!(spec.quality, Quality::Fixed(MIN_QUALITY));
        assert!(!spec.strip_gps);
        let file = ResizeMemory {
            tab: ResizeTab::FileSize,
            max_mb_tenths: 25,
            limit_long_edge: Some(3000),
            ..m
        };
        assert_eq!(
            spec_for(&file).target,
            Target::MaxBytes {
                bytes: 2_500_000,
                long_edge: Some(3000)
            }
        );
        let scale = ResizeMemory {
            tab: ResizeTab::Scale,
            percent: 25,
            ..m
        };
        assert_eq!(spec_for(&scale).target, Target::Percent(25));
    }

    #[test]
    fn replace_is_blocked_for_r3_cases() {
        let mut s = session(&["a.jpg", "bb.jpg"]);
        let bin = |_: &Path| true;
        assert_eq!(s.replace_block(&bin), None);
        assert_eq!(
            s.replace_block(&|p: &Path| p != Path::new("bb.jpg")),
            Some(ReplaceBlock::NoRecycleBin)
        );
        s.items[1].scaled = true;
        assert_eq!(s.replace_block(&bin), Some(ReplaceBlock::Scaled));
        s.items[0].animated = true;
        assert_eq!(s.replace_block(&bin), Some(ReplaceBlock::Animated));
    }
}
