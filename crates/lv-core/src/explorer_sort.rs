//! The orders a folder can be walked in: Explorer's name, date modified, size and type
//! columns (ruling R5), or plain natural order when Explorer says nothing usable.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::listing::natural_cmp;

/// What a file's size and modification time are, as `sort_paths` reads them:
/// `(size in bytes, modified as 100 ns ticks since 1601)`.
pub type FileMeta = (u64, u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// No Explorer order known: names in natural order.
    #[default]
    Natural,
    /// Explorer's "Name" column (natural order too, but it can be descending).
    Name,
    /// "Date modified".
    Modified,
    /// "Size".
    Size,
    /// "Type", approximated by the file extension (case-insensitive).
    Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortSpec {
    pub key: SortKey,
    pub descending: bool,
}

impl SortSpec {
    pub const NATURAL: SortSpec = SortSpec {
        key: SortKey::Natural,
        descending: false,
    };

    /// The order compares sizes or modification times.
    pub fn needs_meta(self) -> bool {
        matches!(self.key, SortKey::Modified | SortKey::Size)
    }

    /// Orders two files: by the key (reversed when descending), files without the
    /// metadata the key needs last, then equal values by natural name order.
    pub fn compare(
        self,
        a: &Path,
        a_meta: Option<FileMeta>,
        b: &Path,
        b_meta: Option<FileMeta>,
    ) -> Ordering {
        let name = |p: &Path| p.file_name().unwrap_or_default().to_owned();
        let natural = || natural_cmp(&name(a), &name(b));
        let directed = |o: Ordering| if self.descending { o.reverse() } else { o };
        let by_meta = |pick: fn(FileMeta) -> u64| match (a_meta, b_meta) {
            (Some(x), Some(y)) => directed(pick(x).cmp(&pick(y))),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        match self.key {
            SortKey::Natural | SortKey::Name => directed(natural()),
            SortKey::Modified => by_meta(|m| m.1).then_with(natural),
            SortKey::Size => by_meta(|m| m.0).then_with(natural),
            SortKey::Kind => directed(extension(a).cmp(&extension(b))).then_with(natural),
        }
    }
}

fn extension(p: &Path) -> String {
    p.extension()
        .map(OsStr::to_string_lossy)
        .unwrap_or_default()
        .to_lowercase()
}

/// Sorts `paths` by `spec`; `meta` gives a file's size and modification time (`None`
/// when unknown: such files go last). Equal values keep natural name order.
pub fn sort_paths(paths: &mut [PathBuf], spec: SortSpec, meta: &dyn Fn(&Path) -> Option<FileMeta>) {
    let mut keyed: Vec<(Option<FileMeta>, PathBuf)> = paths
        .iter_mut()
        .map(|p| {
            let m = if spec.needs_meta() { meta(p) } else { None };
            (m, std::mem::take(p))
        })
        .collect();
    keyed.sort_by(|(am, a), (bm, b)| spec.compare(a, *am, b, *bm));
    for (slot, (_, p)) in paths.iter_mut().zip(keyed) {
        *slot = p;
    }
}

/// The size and modification time of `meta`, as `FileMeta`.
pub fn file_meta(meta: &std::fs::Metadata) -> FileMeta {
    #[cfg(windows)]
    let modified = std::os::windows::fs::MetadataExt::last_write_time(meta);
    #[cfg(not(windows))]
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| {
            d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100
        });
    (meta.len(), modified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(format!(r"C:\pics\{s}"))
    }

    fn names(v: &[PathBuf]) -> Vec<String> {
        v.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn sorted(files: &[(&str, FileMeta)], spec: SortSpec) -> Vec<String> {
        let meta: HashMap<PathBuf, FileMeta> = files.iter().map(|(n, m)| (p(n), *m)).collect();
        let mut v: Vec<PathBuf> = files.iter().map(|(n, _)| p(n)).collect();
        sort_paths(&mut v, spec, &|x| meta.get(x).copied());
        names(&v)
    }

    #[test]
    fn sort_by_modified_desc() {
        let files = [
            ("a.jpg", (1, 300)),
            ("b.jpg", (1, 100)),
            ("c.jpg", (1, 200)),
            ("d.jpg", (1, 300)),
        ];
        let spec = SortSpec {
            key: SortKey::Modified,
            descending: true,
        };
        // Newest first; the two equal times stay in natural order.
        assert_eq!(sorted(&files, spec), ["a.jpg", "d.jpg", "c.jpg", "b.jpg"]);
        let asc = SortSpec {
            descending: false,
            ..spec
        };
        assert_eq!(sorted(&files, asc), ["b.jpg", "c.jpg", "a.jpg", "d.jpg"]);
    }

    #[test]
    fn sort_by_size_then_natural() {
        let files = [
            ("img10.jpg", (5, 0)),
            ("img2.jpg", (5, 0)),
            ("big.png", (9, 0)),
            ("IMG1.jpg", (5, 0)),
        ];
        let spec = SortSpec {
            key: SortKey::Size,
            descending: false,
        };
        assert_eq!(
            sorted(&files, spec),
            ["IMG1.jpg", "img2.jpg", "img10.jpg", "big.png"]
        );
        let desc = SortSpec {
            descending: true,
            ..spec
        };
        assert_eq!(
            sorted(&files, desc),
            ["big.png", "IMG1.jpg", "img2.jpg", "img10.jpg"]
        );
    }

    #[test]
    fn sort_by_name_and_kind() {
        let files = [
            ("b.png", (0, 0)),
            ("a10.jpg", (0, 0)),
            ("a2.JPG", (0, 0)),
            ("c.gif", (0, 0)),
        ];
        let name_desc = SortSpec {
            key: SortKey::Name,
            descending: true,
        };
        assert_eq!(
            sorted(&files, name_desc),
            ["c.gif", "b.png", "a10.jpg", "a2.JPG"]
        );
        let kind = SortSpec {
            key: SortKey::Kind,
            descending: false,
        };
        assert_eq!(
            sorted(&files, kind),
            ["c.gif", "a2.JPG", "a10.jpg", "b.png"]
        );
    }

    #[test]
    fn files_without_metadata_go_last() {
        let meta = |x: &Path| (x != p("a.jpg")).then_some((1, 1));
        let mut v = vec![p("a.jpg"), p("b.jpg")];
        let spec = SortSpec {
            key: SortKey::Size,
            descending: true,
        };
        sort_paths(&mut v, spec, &meta);
        assert_eq!(names(&v), ["b.jpg", "a.jpg"]);
    }

    #[test]
    fn natural_needs_no_metadata() {
        let mut v = vec![p("img10.jpg"), p("img2.jpg")];
        sort_paths(&mut v, SortSpec::NATURAL, &|_| panic!("not asked"));
        assert_eq!(names(&v), ["img2.jpg", "img10.jpg"]);
    }
}
