//! The folders that hold live replace backups (ruling R2, final-review I2).
//!
//! Before a replace moves an original aside, its folder is listed in the data folder
//! (`live-backups`, portable-aware); once that folder's backups of this process are
//! committed, undone or put back, the entry goes. A process that dies or is ended by
//! Windows leaves its entries, and the next start runs
//! [`recover_backups`](super::recover_backups) for each of them, whichever folder it
//! opens. Each entry is one small file `<pid>-<hash>.folder` holding the folder's path,
//! so two running copies never rewrite each other's list.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::save::atomic::process_running;

const EXTENSION: &str = "folder";

/// This process's entries in one list folder.
pub struct LiveFolders {
    dir: PathBuf,
    /// Folder key → (folder, backups of this process alive there).
    held: Mutex<HashMap<String, (PathBuf, usize)>>,
}

impl LiveFolders {
    pub fn new(dir: PathBuf) -> LiveFolders {
        LiveFolders {
            dir,
            held: Mutex::new(HashMap::new()),
        }
    }

    /// A backup is about to be made in `folder`: lists it when it is the first.
    pub fn hold(&self, folder: &Path) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let entry = held
            .entry(key(folder))
            .or_insert_with(|| (folder.to_path_buf(), 0));
        entry.1 += 1;
        if entry.1 == 1 {
            if let Err(e) = self.write(folder) {
                eprintln!(
                    "light-view: cannot note {} for crash recovery: {e}",
                    folder.display()
                );
            }
        }
    }

    /// A backup in `folder` is no longer hidden aside (committed, undone, put back):
    /// the entry goes with the last one.
    pub fn release(&self, folder: &Path) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let k = key(folder);
        let Some(entry) = held.get_mut(&k) else {
            return;
        };
        entry.1 -= 1;
        if entry.1 == 0 {
            held.remove(&k);
            let _ = fs::remove_file(self.entry_path(std::process::id(), folder));
        }
    }

    fn entry_path(&self, pid: u32, folder: &Path) -> PathBuf {
        self.dir
            .join(format!("{pid}-{:016x}.{EXTENSION}", hash(&key(folder))))
    }

    fn write(&self, folder: &Path) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let bytes: Vec<u8> = folder
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        fs::write(self.entry_path(std::process::id(), folder), bytes)
    }
}

/// One listed folder: the process that listed it, the folder, the entry file.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub pid: u32,
    pub folder: PathBuf,
    pub file: PathBuf,
}

/// Every entry in list folder `dir` (none when it does not exist).
pub fn entries(dir: &Path) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for item in read.flatten() {
        let file = item.path();
        if file.extension().is_none_or(|e| e != EXTENSION) {
            continue;
        }
        let pid = file
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split_once('-'))
            .and_then(|(pid, _)| pid.parse().ok());
        let (Some(pid), Ok(bytes)) = (pid, fs::read(&file)) else {
            continue;
        };
        if bytes.is_empty() || bytes.len() % 2 != 0 {
            // Cut short: nothing to recover from it.
            let _ = fs::remove_file(&file);
            continue;
        }
        let wide: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        found.push(Entry {
            pid,
            folder: PathBuf::from(OsString::from_wide(&wide)),
            file,
        });
    }
    found
}

/// Start-up: every folder listed by a process that is no longer running gets `recover`
/// ([`recover_backups`](super::recover_backups)), then its entry goes. Entries of
/// running copies (this one included) are left to them. Returns the folders recovered.
pub fn recover_listed(dir: &Path, recover: impl Fn(&Path) -> usize) -> Vec<PathBuf> {
    let own = std::process::id();
    let mut done = Vec::new();
    for entry in entries(dir) {
        if entry.pid == own || process_running(entry.pid) {
            continue;
        }
        let handled = recover(&entry.folder);
        if handled > 0 {
            eprintln!(
                "light-view: recovered {handled} backup(s) in {}",
                entry.folder.display()
            );
        }
        let _ = fs::remove_file(&entry.file);
        done.push(entry.folder);
    }
    done
}

/// Case-insensitive key of a folder (NTFS names are).
fn key(folder: &Path) -> String {
    folder.to_string_lossy().to_lowercase()
}

/// FNV-1a, 64 bits: a short stable file name per folder.
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

static LIVE: OnceLock<LiveFolders> = OnceLock::new();

/// Turns the list on for this process, in list folder `dir` (once, at start). Without
/// it (`--bench`, tests) [`hold`] and [`release`] do nothing.
pub fn init(dir: PathBuf) {
    let _ = LIVE.set(LiveFolders::new(dir));
}

/// [`LiveFolders::hold`] on this process's list, if any.
pub fn hold(folder: &Path) {
    with_live(|live| live.hold(folder));
}

/// [`LiveFolders::release`] on this process's list, if any.
pub fn release(folder: &Path) {
    with_live(|live| live.release(folder));
}

#[cfg(test)]
thread_local! {
    /// A test's own list for the calls on its thread, so parallel tests never share one.
    static TEST_LIVE: std::cell::RefCell<Option<std::rc::Rc<LiveFolders>>> =
        const { std::cell::RefCell::new(None) };
}

fn with_live(f: impl FnOnce(&LiveFolders)) {
    #[cfg(test)]
    if let Some(live) = TEST_LIVE.with(|t| t.borrow().clone()) {
        f(&live);
        return;
    }
    if let Some(live) = LIVE.get() {
        f(live);
    }
}

/// The folder a backup or original at `path` is in (the key both sides use).
pub fn folder_of(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Command;

    fn list() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap()
    }

    #[test]
    fn a_folder_is_listed_while_it_holds_backups() {
        let dir = list();
        let live = LiveFolders::new(dir.path().join("live-backups"));
        let a = PathBuf::from(r"C:\Photos\여행 2026");
        let b = PathBuf::from(r"D:\other");
        assert!(entries(&live.dir).is_empty(), "no list folder yet");

        live.hold(&a);
        live.hold(&a);
        live.hold(&b);
        let mut got: Vec<_> = entries(&live.dir).into_iter().map(|e| e.folder).collect();
        got.sort();
        assert_eq!(got, [a.clone(), b.clone()], "the path comes back exactly");
        assert!(entries(&live.dir)
            .iter()
            .all(|e| e.pid == std::process::id()));

        // The same folder in another case is the same entry.
        live.release(&PathBuf::from(r"c:\photos\여행 2026"));
        assert_eq!(entries(&live.dir).len(), 2, "one backup is still there");
        live.release(&a);
        live.release(&b);
        assert!(entries(&live.dir).is_empty(), "pruned when empty");
        // Unknown folders are ignored.
        live.release(&a);
    }

    /// The pid of a process that has ended (a finished child of ours).
    fn ended_pid() -> u32 {
        let mut child = Command::new("cmd").args(["/c", "exit"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// An entry as process `pid` would have left it.
    fn leave_entry(dir: &Path, pid: u32, n: u32, folder: &Path) {
        let wide: Vec<u8> = folder
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        fs::write(dir.join(format!("{pid}-{n}.folder")), wide).unwrap();
    }

    #[test]
    fn start_up_recovers_the_folders_of_ended_processes_only() {
        let dir = list();
        let live = LiveFolders::new(dir.path().to_path_buf());
        live.hold(Path::new(r"C:\ours"));
        let dead = ended_pid();
        let dead_folder = PathBuf::from(r"C:\left by a crash");
        leave_entry(dir.path(), dead, 1, &dead_folder);
        // Garbage is dropped, other files are left alone.
        fs::write(dir.path().join(format!("{dead}-2.folder")), [1u8]).unwrap();
        fs::write(dir.path().join("readme.txt"), "x").unwrap();

        let asked = std::cell::RefCell::new(Vec::new());
        let done = recover_listed(dir.path(), |f| {
            asked.borrow_mut().push(f.to_path_buf());
            1
        });
        assert_eq!(asked.into_inner(), std::slice::from_ref(&dead_folder));
        assert_eq!(done, [dead_folder]);
        let left: Vec<_> = entries(dir.path()).into_iter().map(|e| e.folder).collect();
        assert_eq!(
            left,
            [PathBuf::from(r"C:\ours")],
            "a running copy's entry stays"
        );
        assert!(dir.path().join("readme.txt").exists());
        assert!(!dir.path().join(format!("{dead}-2.folder")).exists());
    }

    #[test]
    fn start_up_puts_back_an_original_hidden_in_a_listed_folder() {
        let (lists, photos) = (list(), list());
        let dead = ended_pid();
        // A replace cut off after step ②: only the hidden backup is left.
        let original = photos.path().join("a.jpg");
        let backup = photos.path().join(super::super::backup_name(
            std::ffi::OsStr::new("a.jpg"),
            dead,
            0,
        ));
        fs::write(&backup, b"the original").unwrap();
        leave_entry(lists.path(), dead, 7, photos.path());

        let done = recover_listed(lists.path(), super::super::recover_backups);
        assert_eq!(done, [photos.path().to_path_buf()]);
        assert_eq!(fs::read(&original).unwrap(), b"the original");
        assert!(!backup.exists());
        assert!(entries(lists.path()).is_empty(), "the entry goes");
    }

    #[test]
    fn a_replace_lists_its_folder_until_the_batch_is_committed() {
        use crate::delete::fake::FakeTrash;
        use lv_core::identity::capture;
        use lv_core::resize::OutputFormat;

        // This thread's list, in a folder of this test's own.
        let lists = list();
        let global = std::rc::Rc::new(LiveFolders::new(lists.path().to_path_buf()));
        TEST_LIVE.with(|t| *t.borrow_mut() = Some(global.clone()));
        let photos = list();
        let a = photos.path().join("a.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .save(&a)
            .unwrap();
        let super::super::ItemResult::Replaced { result, backup } =
            super::super::save_replace(&a, &capture(&a).unwrap(), b"new", OutputFormat::Png)
        else {
            panic!("replaced");
        };
        let listed: Vec<_> = entries(&global.dir).into_iter().map(|e| e.folder).collect();
        assert_eq!(listed, [photos.path().to_path_buf()]);
        let mut batch = super::super::batch::ReplaceBatch::new();
        batch.push(&result, &backup, &a);
        assert!(batch
            .commit(&FakeTrash::new(photos.path(), true))
            .is_empty());
        assert!(entries(&global.dir).is_empty(), "pruned after the commit");

        // A replace taken back right away (over the size limit) prunes it too.
        let super::super::ItemResult::Replaced { result, backup } =
            super::super::save_replace(&a, &capture(&a).unwrap(), b"newer", OutputFormat::Png)
        else {
            panic!("replaced");
        };
        assert_eq!(entries(&global.dir).len(), 1);
        assert!(super::super::revert_replace(&result, &backup, &a));
        assert!(entries(&global.dir).is_empty());
        TEST_LIVE.with(|t| *t.borrow_mut() = None);
    }
}
