//! Paths longer than 260 characters through the whole app: open (listing, decode,
//! identity), rotation save, delete up to the Recycle Bin hand-off, and resize save.

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};
use lv_core::decode::{decode_file, DecodeLimits};
use lv_core::explorer_sort::{SortKey, SortSpec};
use lv_core::identity::{capture, to_extended_path};
use lv_core::prefetch::Prefetcher;
use lv_core::resize::OutputFormat;

use crate::delete::{commit, DeleteStep, Deleter, PendingDelete, ShellTrash, Trash};
use crate::resize::save::{save_new, ItemResult};
use crate::routing::RoutingDecoder;
use crate::save::{save_rotation, NoWorker, SaveOutcome};
use crate::shell::user_path;
use crate::viewer::Viewer;

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(w, h, |x, _| Rgb([if x < w / 2 { 200 } else { 20 }, 90, 40]));
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&img)
        .unwrap();
    out
}

fn units(p: &Path) -> usize {
    p.as_os_str().encode_wide().count()
}

/// Says there is a Recycle Bin everywhere; nothing may reach it here.
struct Bin;

impl Trash for Bin {
    fn has_recycle_bin(&self, _path: &Path) -> bool {
        true
    }
    fn recycle(&self, _p: PendingDelete) {
        panic!("nothing is recycled in this test");
    }
    fn flush(&self, _timeout: Duration) {}
}

#[test]
fn long_path_open_rotate_delete_resize() {
    let root = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
    let mut dir = root.path().to_path_buf();
    while units(&dir) < 300 {
        dir.push(format!("{}여름", "d".repeat(36)));
    }
    fs::create_dir_all(to_extended_path(&dir)).unwrap();
    let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
    fs::write(to_extended_path(&a), jpeg(61, 31)).unwrap();
    fs::write(to_extended_path(&b), jpeg(48, 48)).unwrap();
    // a.jpg an hour older, so the order below does not hang on the clock's tick.
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(to_extended_path(&a))
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    assert!(units(&a) > 300);
    assert_eq!(user_path(&to_extended_path(&a)), a);
    let limits = DecodeLimits::default();

    // Open: the folder in an Explorer order, the decode and the identity it was read with.
    let (tx, rx) = channel();
    let prefetcher = Prefetcher::new(RoutingDecoder::start(limits, 1), 1, u64::MAX, move |p| {
        let _ = tx.send(p);
    });
    let mut v = Viewer::new(prefetcher);
    v.open(&a);
    assert_eq!(v.position(), (1, 2));
    let ready = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(ready, a);
    v.check_current();
    let (shown, id) = v.delete_target().expect("the photo is on screen");
    assert_eq!(shown, a);
    let id = id.expect("identity read through the long path");
    let newest = SortSpec {
        key: SortKey::Modified,
        descending: true,
    };
    let list = lv_core::listing::FolderList::scan_sorted(&b, newest).unwrap();
    assert_eq!(list.position(), Some((1, 2)), "b.jpg was written last");

    // Rotate: saved through the long path (an odd size: the EXIF tag, no worker needed).
    let outcome = save_rotation(&a, &id, 1, &NoWorker, &limits);
    assert!(
        matches!(
            outcome,
            SaveOutcome::Saved | SaveOutcome::SavedAsOrientationTag
        ),
        "{outcome:?}"
    );
    let turned = decode_file(&a, &limits).unwrap();
    assert_eq!((turned.width, turned.height), (31, 61));

    // Delete: the real Recycle Bin is not offered for a long path (the shell would
    // delete it for good); where one is, the file becomes pending and its commit
    // checks hand the long path over unchanged.
    let id = capture(&a).unwrap();
    let real = ShellTrash::new(Box::new(|_, _| {}));
    assert!(!real.has_recycle_bin(&a));
    let mut deleter = Deleter::new(Box::new(Bin));
    assert_eq!(
        deleter.delete(&a, &id),
        DeleteStep::Pending { committed: None }
    );
    assert_eq!(deleter.pending_path(), Some(a.as_path()));
    let mut handed = None;
    let pending = PendingDelete {
        path: a.clone(),
        id,
    };
    commit(&pending, |p| {
        handed = Some(p.to_path_buf());
        Ok(())
    })
    .unwrap();
    assert_eq!(handed.as_deref(), Some(a.as_path()));
    assert!(
        fs::metadata(to_extended_path(&a)).is_ok(),
        "nothing deleted"
    );

    // Resize: a new file beside the long-path original.
    let small = jpeg(24, 24);
    let saved = save_new(&b, &small, OutputFormat::Jpeg);
    let target: PathBuf = dir.join("b_small.jpg");
    assert_eq!(saved, ItemResult::Saved(target.clone()));
    assert_eq!(decode_file(&target, &limits).unwrap().width, 24);

    drop(v);
    fs::remove_dir_all(to_extended_path(root.path())).unwrap();
}

/// The short (8.3) form of `path` as Windows gives it, in user form; `None` where the
/// volume makes no short names.
fn short_form(path: &Path) -> Option<PathBuf> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;
    let ext = HSTRING::from(to_extended_path(path).as_os_str());
    let mut buf = vec![0u16; 2048];
    // SAFETY: `ext` is NUL-terminated and `buf` is a live buffer passed with its length.
    let len = unsafe { GetShortPathNameW(&ext, Some(&mut buf)) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    let short = user_path(&PathBuf::from(String::from_utf16_lossy(&buf[..len])));
    (short != path).then_some(short)
}

/// A long path handed over in its 8.3 form (as the shell may pass it) opens as the
/// file's real name: listed once, and a resized copy is named after the real name.
#[test]
fn short_form_of_a_long_path_opens_by_its_long_name() {
    let root = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
    let mut dir = root.path().to_path_buf();
    while units(&dir) < 300 {
        dir.push(format!("{} folder", "long".repeat(9)));
    }
    fs::create_dir_all(to_extended_path(&dir)).unwrap();
    let (a, b) = (dir.join("a photo.jpg"), dir.join("b summer photo.jpg"));
    fs::write(to_extended_path(&a), jpeg(16, 16)).unwrap();
    fs::write(to_extended_path(&b), jpeg(16, 16)).unwrap();
    let Some(short) = short_form(&b) else {
        println!("no 8.3 names on this volume: skipped");
        return;
    };
    let args = crate::parse_args(
        ["lv".into(), short.clone().into_os_string()]
            .into_iter()
            .skip(1),
    );
    assert_eq!(args.initial.as_deref(), Some(b.as_path()));
    let args = crate::parse_args(["--resize".into(), short.into_os_string()].into_iter());
    assert_eq!(args.resize, std::slice::from_ref(&b));
    let mut v = Viewer::new(Prefetcher::new(
        RoutingDecoder::start(DecodeLimits::default(), 1),
        1,
        u64::MAX,
        |_| {},
    ));
    v.open(args.resize[0].as_path());
    assert_eq!(v.position(), (2, 2));
    drop(v);
    fs::remove_dir_all(to_extended_path(root.path())).unwrap();
}
