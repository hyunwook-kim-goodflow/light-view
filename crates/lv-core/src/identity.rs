//! File identity (volume serial + file ID + size + mtime) and `\\?\` extended paths.
//!
//! Identity is captured when a file is opened and re-checked right before a
//! destructive operation (delete, save), so we never act on a file that was
//! replaced or edited behind the viewer's back.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub volume_serial: u32,
    pub file_index: u64,
    pub size: u64,
    /// `ftLastWriteTime` as 100 ns ticks since 1601 (FILETIME), exact.
    pub modified: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verify {
    Same,
    Changed,
    Missing,
}

/// Reads the identity of `path` (normal or long path).
pub fn capture(path: &Path) -> io::Result<FileIdentity> {
    // std opens with FILE_SHARE_READ | WRITE | DELETE, so this never blocks
    // other readers, writers or deleters.
    let file = std::fs::File::open(to_extended_path(path))?;
    let info = file_info(&file)?;
    Ok(FileIdentity {
        volume_serial: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        modified: (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(info.ftLastWriteTime.dwLowDateTime),
    })
}

/// Compares the file on disk with `id`. When the recorded file index is 0
/// (some network file systems), only size and modification time are compared.
pub fn verify(path: &Path, id: &FileIdentity) -> Verify {
    let Ok(now) = capture(path) else {
        return Verify::Missing;
    };
    let same = if id.file_index == 0 {
        now.size == id.size && now.modified == id.modified
    } else {
        now == *id
    };
    if same {
        Verify::Same
    } else {
        Verify::Changed
    }
}

/// Converts a path to a `\\?\` (or `\\?\UNC\`) extended-length path.
/// `.`/`..` are normalised first because `\\?\` paths are not normalised by
/// Windows. Already-extended and device (`\\.\`) paths are returned unchanged.
pub fn to_extended_path(path: &Path) -> PathBuf {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if is_verbatim_or_device(&wide) {
        return path.to_path_buf();
    }
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let wide: Vec<u16> = abs.as_os_str().encode_wide().collect();
    if is_verbatim_or_device(&wide) {
        return abs;
    }
    let mut out: Vec<u16> = Vec::with_capacity(wide.len() + 8);
    if has_prefix(&wide, r"\\") {
        out.extend(r"\\?\UNC\".encode_utf16());
        out.extend_from_slice(&wide[2..]);
    } else {
        out.extend(r"\\?\".encode_utf16());
        out.extend_from_slice(&wide);
    }
    PathBuf::from(OsString::from_wide(&out))
}

fn is_verbatim_or_device(wide: &[u16]) -> bool {
    has_prefix(wide, r"\\?\") || has_prefix(wide, r"\\.\")
}

fn has_prefix(wide: &[u16], prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    wide.starts_with(&prefix)
}

#[allow(unsafe_code)]
fn file_info(file: &std::fs::File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle comes from a live `File` borrowed for the whole call,
    // and `info` is a valid, writable out-parameter of the expected type.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| io::Error::last_os_error())?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    #[test]
    fn same_file_is_same() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        fs::write(&p, b"hello").unwrap();
        let id = capture(&p).unwrap();
        assert_eq!(id.size, 5);
        assert_ne!(id.file_index, 0);
        assert_eq!(verify(&p, &id), Verify::Same);
    }

    #[test]
    fn changed_size_or_mtime_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        fs::write(&p, b"hello").unwrap();
        let id = capture(&p).unwrap();

        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        drop(f);
        assert_eq!(verify(&p, &id), Verify::Changed);

        let id2 = capture(&p).unwrap();
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(9).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
            .unwrap();
        drop(f);
        assert_eq!(verify(&p, &id2), Verify::Changed);
        // Size alone (mtime restored) is also detected.
        let id3 = capture(&p).unwrap();
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(11).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
            .unwrap();
        drop(f);
        assert_eq!(verify(&p, &id3), Verify::Changed);
    }

    #[test]
    fn zero_file_index_compares_size_and_mtime_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        fs::write(&p, b"hello").unwrap();
        let mut id = capture(&p).unwrap();
        id.file_index = 0;
        assert_eq!(verify(&p, &id), Verify::Same);
        id.size += 1;
        assert_eq!(verify(&p, &id), Verify::Changed);
    }

    #[test]
    fn deleted_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        fs::write(&p, b"hello").unwrap();
        let id = capture(&p).unwrap();
        fs::remove_file(&p).unwrap();
        assert_eq!(verify(&p, &id), Verify::Missing);
    }

    #[test]
    fn replaced_file_with_same_size_is_changed() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        fs::write(&a, b"hello").unwrap();
        fs::write(&b, b"world").unwrap();
        let id = capture(&a).unwrap();
        // Same mtime, so only the file index can tell them apart.
        let mtime = fs::metadata(&a).unwrap().modified().unwrap();
        fs::File::options()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        fs::rename(&b, &a).unwrap();
        let now = capture(&a).unwrap();
        assert_eq!((now.size, now.modified), (id.size, id.modified));
        assert_ne!(now.file_index, id.file_index);
        assert_eq!(verify(&a, &id), Verify::Changed);
    }

    #[test]
    fn extended_path_forms() {
        let e = |s: &str| to_extended_path(Path::new(s));
        assert_eq!(e(r"C:\x\y.jpg"), PathBuf::from(r"\\?\C:\x\y.jpg"));
        assert_eq!(
            e(r"\\server\share\x.jpg"),
            PathBuf::from(r"\\?\UNC\server\share\x.jpg")
        );
        assert_eq!(e(r"\\?\C:\x\y.jpg"), PathBuf::from(r"\\?\C:\x\y.jpg"));
        assert_eq!(
            e(r"\\?\UNC\server\share\x.jpg"),
            PathBuf::from(r"\\?\UNC\server\share\x.jpg")
        );
        assert_eq!(e(r"C:\x\.\a\..\y.jpg"), PathBuf::from(r"\\?\C:\x\y.jpg"));
        // Relative input becomes absolute against the cwd.
        let rel = e(r"foo\bar.jpg");
        assert!(rel.to_string_lossy().starts_with(r"\\?\"));
        assert!(rel.to_string_lossy().ends_with(r"foo\bar.jpg"));
    }

    #[test]
    fn long_path_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = dir.path().to_path_buf();
        while p.as_os_str().len() < 300 {
            p.push("d".repeat(40));
        }
        fs::create_dir_all(to_extended_path(&p)).unwrap();
        let f = p.join("long.bin");
        fs::write(to_extended_path(&f), b"hello").unwrap();
        assert!(f.as_os_str().len() > 300);
        let id = capture(&f).unwrap();
        assert_eq!(id.size, 5);
        assert_eq!(verify(&f, &id), Verify::Same);
        fs::remove_file(to_extended_path(&f)).unwrap();
        assert_eq!(verify(&f, &id), Verify::Missing);
    }
}
