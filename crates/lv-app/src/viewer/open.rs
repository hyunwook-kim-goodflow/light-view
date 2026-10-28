//! Opening a photo within its folder, and the folder's order (ruling R5): a newly
//! opened folder starts in natural order and takes Explorer's order when its answer
//! comes; reopening a photo of the listed folder keeps the order it has.

use std::path::{Path, PathBuf};

use lv_core::explorer_sort::SortSpec;
use lv_core::listing::{same_path, FolderList};

use super::{Redraw, Viewer};

/// Where a folder's order comes from: `ask` starts a question (it must not block),
/// `answers` takes the answers that came since.
pub trait SortSource {
    fn ask(&self, dir: &Path);
    fn answers(&self) -> Vec<(PathBuf, SortSpec)>;
}

impl Viewer {
    pub fn set_sort_source(&mut self, source: Box<dyn SortSource>) {
        self.sort_source = Some(source);
    }

    /// Opens `path` within its folder. Never decodes on this thread.
    pub fn open(&mut self, path: &Path) {
        let dir = std::path::absolute(path)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let same_folder = match (&dir, self.list.dir()) {
            (Some(new), Some(old)) => same_path(new, old),
            _ => false,
        };
        let spec = if same_folder {
            self.list.spec()
        } else {
            SortSpec::NATURAL
        };
        let mut list = FolderList::scan_sorted(path, spec)
            .unwrap_or_else(|_| FolderList::from_paths(vec![path.to_path_buf()], path));
        // A pending delete stays hidden in the new listing.
        for hidden in self.list.hidden() {
            list.hide(hidden);
        }
        if !same_folder {
            if let (Some(source), Some(dir)) = (&self.sort_source, list.dir()) {
                source.ask(dir);
            }
        }
        self.open_list(list);
    }

    pub(super) fn open_list(&mut self, list: FolderList) {
        self.list = list;
        self.backward = false;
        self.quarters = 0;
        if self.showing_current() {
            // Reopening the shown photo: rebuild it unrotated and unzoomed.
            self.shown = None;
        }
        self.after_move();
    }

    /// Takes the order answers that came in: the listed folder is sorted again, the
    /// current photo staying current (and on screen); answers for another folder are
    /// dropped.
    pub fn take_sort_answers(&mut self) -> Redraw {
        let Some(source) = &self.sort_source else {
            return Redraw::None;
        };
        let mut redraw = Redraw::None;
        for (dir, spec) in source.answers() {
            let here = self.list.dir().is_some_and(|d| same_path(d, &dir));
            if here && self.list.resort(spec) {
                // Other neighbours now: prefetch those; the position in the title moved.
                self.request_wanted();
                redraw = Redraw::Full;
            }
        }
        redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lv_core::explorer_sort::SortKey;
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::viewer::tests::viewer_at;

    /// Records questions; answers with what the test puts in.
    #[derive(Clone, Default)]
    struct Fake {
        asked: Rc<RefCell<Vec<PathBuf>>>,
        answers: Rc<RefCell<Vec<(PathBuf, SortSpec)>>>,
    }

    impl SortSource for Fake {
        fn ask(&self, dir: &Path) {
            self.asked.borrow_mut().push(dir.to_path_buf());
        }
        fn answers(&self) -> Vec<(PathBuf, SortSpec)> {
            self.answers.borrow_mut().drain(..).collect()
        }
    }

    #[test]
    fn explorer_order_arrives_after_open_and_keeps_the_photo() {
        let dir = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
        for (n, len) in [("a.jpg", 3), ("b.jpg", 1), ("c.jpg", 2)] {
            std::fs::write(dir.path().join(n), vec![0u8; len]).unwrap();
        }
        let fake = Fake::default();
        let (mut v, _rx) = viewer_at("a.jpg");
        v.set_sort_source(Box::new(fake.clone()));
        v.open(&dir.path().join("b.jpg"));
        assert_eq!(fake.asked.borrow().len(), 1);
        assert_eq!(v.list.position(), Some((2, 3)));
        assert_eq!(v.take_sort_answers(), Redraw::None);
        let by_size = SortSpec {
            key: SortKey::Size,
            descending: true,
        };
        // An answer for another folder changes nothing.
        fake.answers
            .borrow_mut()
            .push((PathBuf::from(r"Z:\elsewhere"), by_size));
        assert_eq!(v.take_sort_answers(), Redraw::None);
        let listed = v.list.dir().unwrap().to_path_buf();
        fake.answers.borrow_mut().push((listed, by_size));
        assert_eq!(v.take_sort_answers(), Redraw::Full);
        assert_eq!(v.list.current(), Some(dir.path().join("b.jpg").as_path()));
        assert_eq!(v.list.position(), Some((3, 3)));
        // Another photo of the same folder: same order, no new question.
        v.open(&dir.path().join("a.jpg"));
        assert_eq!(fake.asked.borrow().len(), 1);
        assert_eq!(v.list.spec(), by_size);
        assert_eq!(v.list.position(), Some((1, 3)));
    }
}
