//! Folder listing and navigation in Explorer order.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::format::is_supported_extension;

/// Natural ("Explorer") string comparison of two file names.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn natural_cmp(a: &OsStr, b: &OsStr) -> Ordering {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::StrCmpLogicalW;

    let wa: Vec<u16> = a.encode_wide().chain(std::iter::once(0)).collect();
    let wb: Vec<u16> = b.encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
    let r = unsafe { StrCmpLogicalW(PCWSTR(wa.as_ptr()), PCWSTR(wb.as_ptr())) };
    r.cmp(&0)
}

/// Fallback: digit runs compared numerically, everything else case-insensitive.
#[cfg(not(windows))]
pub fn natural_cmp(a: &OsStr, b: &OsStr) -> Ordering {
    let a: Vec<char> = a.to_string_lossy().chars().collect();
    let b: Vec<char> = b.to_string_lossy().chars().collect();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na: String = a[si..i]
                .iter()
                .collect::<String>()
                .trim_start_matches('0')
                .to_owned();
            let nb: String = b[sj..j]
                .iter()
                .collect::<String>()
                .trim_start_matches('0')
                .to_owned();
            let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb));
            if o != Ordering::Equal {
                return o;
            }
        } else {
            let o = a[i].to_lowercase().cmp(b[j].to_lowercase());
            if o != Ordering::Equal {
                return o;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    norm(a) == norm(b)
}

/// The first supported image file (sub-folders are skipped) of `dir` in Explorer order.
pub fn first_supported_in(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut first: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || !is_supported_extension(&path) {
            continue;
        }
        let name = path.file_name().unwrap_or_default();
        if first
            .as_ref()
            .is_none_or(|f| natural_cmp(name, f.file_name().unwrap_or_default()).is_lt())
        {
            first = Some(path);
        }
    }
    Ok(first)
}

/// Supported image files of one folder, sorted like Explorer, with a cursor.
#[derive(Debug, Clone, Default)]
pub struct FolderList {
    entries: Vec<PathBuf>,
    current: Option<usize>,
    /// Files kept out of `entries` although they are on disk (pending deletes), from
    /// this folder or another. A `refresh` keeps them out; `unhide` puts one of this
    /// folder back, `forget` drops it.
    hidden: Vec<PathBuf>,
    /// The listed folder (the parent of the file the list was made for).
    dir: Option<PathBuf>,
}

impl FolderList {
    fn build(paths: Vec<PathBuf>, keep: Option<&Path>) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| is_supported_extension(p) || keep.is_some_and(|c| same_path(p, c)))
            .collect();
        v.sort_by(|a, b| {
            natural_cmp(
                a.file_name().unwrap_or_default(),
                b.file_name().unwrap_or_default(),
            )
        });
        v
    }

    /// Keeps only supported files (plus `current`, always) sorted by `natural_cmp`.
    pub fn from_paths(mut paths: Vec<PathBuf>, current: &Path) -> FolderList {
        if !paths.iter().any(|p| same_path(p, current)) {
            paths.push(current.to_path_buf());
        }
        let entries = Self::build(paths, Some(current));
        let current_index = entries.iter().position(|p| same_path(p, current));
        FolderList {
            entries,
            current: current_index,
            hidden: Vec::new(),
            dir: current.parent().map(Path::to_path_buf),
        }
    }

    /// Lists the files (not sub-folders) in the parent folder of `file`.
    pub fn scan(file: &Path) -> std::io::Result<FolderList> {
        let file = std::path::absolute(file)?;
        let file = file.as_path();
        let dir = file.parent().unwrap_or(file);
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                paths.push(entry.path());
            }
        }
        Ok(Self::from_paths(paths, file))
    }

    pub fn current(&self) -> Option<&Path> {
        self.current.map(|i| self.entries[i].as_path())
    }

    /// (1-based index, total).
    pub fn position(&self) -> Option<(usize, usize)> {
        self.current.map(|i| (i + 1, self.entries.len()))
    }

    pub fn go_next(&mut self) -> bool {
        match self.current {
            Some(i) if i + 1 < self.entries.len() => {
                self.current = Some(i + 1);
                true
            }
            _ => false,
        }
    }

    pub fn go_prev(&mut self) -> bool {
        match self.current {
            Some(i) if i > 0 => {
                self.current = Some(i - 1);
                true
            }
            _ => false,
        }
    }

    pub fn neighbor(&self, offset: isize) -> Option<&Path> {
        let i = self.current?.checked_add_signed(offset)?;
        self.entries.get(i).map(PathBuf::as_path)
    }

    /// Replaces the listing, keeping the current file by path; if it is gone,
    /// moves to the same index (or the last entry). Hidden files stay hidden.
    pub fn refresh(&mut self, paths: Vec<PathBuf>) {
        let old_path = self.current().map(Path::to_path_buf);
        let old_index = self.current.unwrap_or(0);
        let hidden = &self.hidden;
        let paths = paths
            .into_iter()
            .filter(|p| !hidden.iter().any(|h| same_path(p, h)))
            .collect();
        self.entries = Self::build(paths, None);
        self.current = if self.entries.is_empty() {
            None
        } else {
            old_path
                .and_then(|c| self.entries.iter().position(|p| same_path(p, &c)))
                .or(Some(old_index.min(self.entries.len() - 1)))
        };
    }
}

impl FolderList {
    /// Takes `path` out of the listing until `unhide` or `forget`; `refresh` keeps it
    /// out. If it was the current file, the next one becomes current (the previous one
    /// at the end of the folder). False if it is not listed (it is still kept hidden).
    pub fn hide(&mut self, path: &Path) -> bool {
        if !self.hidden.iter().any(|p| same_path(p, path)) {
            self.hidden.push(path.to_path_buf());
        }
        let Some(i) = self.entries.iter().position(|p| same_path(p, path)) else {
            return false;
        };
        self.entries.remove(i);
        self.current = match self.current {
            _ if self.entries.is_empty() => None,
            Some(c) if c == i => Some(i.min(self.entries.len() - 1)),
            Some(c) if c > i => Some(c - 1),
            c => c,
        };
        true
    }

    /// Puts a hidden file of this folder back in Explorer order. The current file stays
    /// current (the restored one becomes current in an empty list). A hidden file of
    /// another folder is only dropped from the hidden set. False if nothing was listed.
    pub fn unhide(&mut self, path: &Path) -> bool {
        let Some(h) = self.hidden.iter().position(|p| same_path(p, path)) else {
            return false;
        };
        let p = self.hidden.remove(h);
        let here = match (p.parent(), &self.dir) {
            (Some(parent), Some(dir)) => same_path(parent, dir),
            _ => false,
        };
        if !here {
            return false;
        }
        let name = p.file_name().unwrap_or_default().to_owned();
        let i = self
            .entries
            .partition_point(|e| natural_cmp(e.file_name().unwrap_or_default(), &name).is_lt());
        self.entries.insert(i, p);
        self.current = match self.current {
            Some(c) if c >= i => Some(c + 1),
            Some(c) => Some(c),
            None => Some(i),
        };
        true
    }

    /// Makes the listed `path` current. False if it is not listed.
    pub fn go_to(&mut self, path: &Path) -> bool {
        let Some(i) = self.entries.iter().position(|p| same_path(p, path)) else {
            return false;
        };
        self.current = Some(i);
        true
    }

    /// Drops a hidden file for good (it was deleted). False if it is not hidden.
    pub fn forget(&mut self, path: &Path) -> bool {
        let before = self.hidden.len();
        self.hidden.retain(|p| !same_path(p, path));
        self.hidden.len() != before
    }

    pub fn hidden(&self) -> &[PathBuf] {
        &self.hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn p(s: &str) -> PathBuf {
        PathBuf::from(format!(r"C:\pics\{s}"))
    }

    #[test]
    fn natural_order_like_explorer() {
        let l = FolderList::from_paths(
            vec![p("img10.jpg"), p("img2.jpg"), p("IMG1.png"), p("notes.txt")],
            &p("img2.jpg"),
        );
        let names: Vec<_> = (-1..=1)
            .filter_map(|o| l.neighbor(o))
            .map(|x| x.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["IMG1.png", "img2.jpg", "img10.jpg"]);
        assert_eq!(l.position(), Some((2, 3)));
    }
    #[test]
    fn does_not_wrap_at_ends() {
        let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg")], &p("b.jpg"));
        assert!(!l.go_next());
        assert_eq!(l.current(), Some(p("b.jpg").as_path()));
        assert!(l.go_prev());
        assert!(!l.go_prev());
    }
    #[test]
    fn refresh_keeps_current_by_path() {
        let mut l = FolderList::from_paths(vec![p("b.jpg"), p("c.jpg")], &p("c.jpg"));
        l.refresh(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")]);
        assert_eq!(l.current(), Some(p("c.jpg").as_path()));
        assert_eq!(l.position(), Some((3, 3)));
    }
    #[test]
    fn refresh_when_current_removed_moves_to_neighbor() {
        let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")], &p("b.jpg"));
        l.refresh(vec![p("a.jpg"), p("c.jpg")]);
        assert_eq!(l.current(), Some(p("c.jpg").as_path()));
    }
    #[test]
    fn refresh_to_empty_is_empty() {
        let mut l = FolderList::from_paths(vec![p("a.jpg")], &p("a.jpg"));
        l.refresh(vec![]);
        assert_eq!(l.current(), None);
        assert_eq!(l.position(), None);
        assert!(!l.go_next());
    }
    #[test]
    fn scan_reads_real_folder() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["2.jpg", "10.jpg", "x.txt"] {
            std::fs::write(dir.path().join(n), b"").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub.jpg")).unwrap();
        let l = FolderList::scan(&dir.path().join("10.jpg")).unwrap();
        assert_eq!(l.position(), Some((2, 2)));
    }
    #[test]
    fn scan_with_forward_slashes_has_no_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.jpg", "b.jpg"] {
            std::fs::write(dir.path().join(n), b"").unwrap();
        }
        let f = PathBuf::from(
            dir.path()
                .join("a.jpg")
                .to_string_lossy()
                .replace("\\", "/"),
        );
        let l = FolderList::scan(&f).unwrap();
        assert_eq!(l.position(), Some((1, 2)));
    }
    #[test]
    fn first_supported_in_uses_explorer_order_and_skips_folders() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["10.jpg", "2.png", "1.txt"] {
            std::fs::write(dir.path().join(n), b"").unwrap();
        }
        std::fs::create_dir(dir.path().join("1.jpg")).unwrap();
        let first = first_supported_in(dir.path()).unwrap();
        assert_eq!(first.as_deref(), Some(dir.path().join("2.png").as_path()));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(first_supported_in(empty.path()).unwrap(), None);
    }
    #[test]
    fn scan_with_relative_path_has_no_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.jpg", "b.jpg"] {
            std::fs::write(dir.path().join(n), b"").unwrap();
        }
        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let r = FolderList::scan(Path::new("a.jpg"));
        std::env::set_current_dir(old).unwrap();
        assert_eq!(r.unwrap().position(), Some((1, 2)));
    }

    #[test]
    fn hide_moves_to_next_or_previous_at_the_end() {
        let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")], &p("b.jpg"));
        assert!(l.hide(&p("b.jpg")));
        assert_eq!(l.current(), Some(p("c.jpg").as_path()));
        assert_eq!(l.position(), Some((2, 2)));
        assert!(l.hide(&p("c.jpg")));
        assert_eq!(l.current(), Some(p("a.jpg").as_path()));
        assert!(l.hide(&p("a.jpg")));
        assert_eq!(l.current(), None);
        assert!(!l.hide(&p("x.jpg")));
        assert_eq!(l.hidden().len(), 4);
        l.refresh(vec![p("x.jpg"), p("y.jpg")]);
        assert_eq!(l.current(), Some(p("y.jpg").as_path()));
    }
    #[test]
    fn hiding_another_file_keeps_the_current_one() {
        let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")], &p("c.jpg"));
        l.hide(&p("a.jpg"));
        assert_eq!(l.current(), Some(p("c.jpg").as_path()));
        assert_eq!(l.position(), Some((2, 2)));
    }
    #[test]
    fn unhide_restores_order_and_keeps_the_current_one() {
        let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")], &p("b.jpg"));
        l.hide(&p("b.jpg"));
        assert!(l.unhide(&p("b.jpg")));
        assert_eq!(l.current(), Some(p("c.jpg").as_path()));
        assert_eq!(l.position(), Some((3, 3)));
        assert!(l.go_to(&p("b.jpg")));
        assert_eq!(l.position(), Some((2, 3)));
        assert!(!l.go_to(&p("x.jpg")));
        assert!(!l.unhide(&p("b.jpg")));
        assert!(l.hidden().is_empty());
        // A hidden file of another folder is not listed here.
        l.hide(&PathBuf::from(r"D:\other\b.jpg"));
        assert!(!l.unhide(&PathBuf::from(r"D:\other\b.jpg")));
        assert_eq!(l.position(), Some((2, 3)));
        assert!(l.hidden().is_empty());
        // Back into an empty list.
        let mut l = FolderList::from_paths(vec![p("a.jpg")], &p("a.jpg"));
        l.hide(&p("a.jpg"));
        assert!(l.unhide(&p("a.jpg")));
        assert_eq!(l.position(), Some((1, 1)));
    }
    #[test]
    fn refresh_keeps_hidden_files_out() {
        let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")], &p("a.jpg"));
        l.hide(&p("b.jpg"));
        l.refresh(vec![p("a.jpg"), p("B.JPG"), p("c.jpg"), p("d.jpg")]);
        assert_eq!(l.position(), Some((1, 3)));
        assert_eq!(l.neighbor(1), Some(p("c.jpg").as_path()));
        assert!(l.forget(&p("b.jpg")));
        assert!(!l.unhide(&p("b.jpg")));
        l.refresh(vec![p("a.jpg"), p("b.jpg")]);
        assert_eq!(l.position(), Some((1, 2)));
    }
}
