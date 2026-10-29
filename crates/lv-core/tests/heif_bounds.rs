//! Hostile HEIF boxes cannot make the read-only HEIF functions allocate much: they run in
//! explorer.exe and the SYSTEM search indexer (the shell's details handler), where a failed
//! allocation aborts the host. A counting allocator measures the peak each call adds.
//!
//! Its own test binary, so the allocator counts only this file's one test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lv_core::exif_info::camera_facts;
use lv_core::heif_edit::{display_size, read_rotation};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, forwarded.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: a block this allocator handed out, with its layout.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most a call may add on top of what was live before it.
const BOUND: usize = 16 << 20;

/// Runs `f` and returns how many bytes it added at its peak, and how long it took.
fn measured<T>(f: impl FnOnce() -> T) -> (T, usize, Duration) {
    let before = LIVE.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    (out, PEAK.load(Ordering::SeqCst) - before, took)
}

fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    b.extend_from_slice(typ);
    b.extend_from_slice(payload);
    b
}

fn full(typ: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![version, 0, 0, 0];
    p.extend_from_slice(payload);
    bx(typ, &p)
}

/// A HEIF whose `meta` holds `iloc` and `ipma` as given (bodies after version and flags),
/// item 1 primary with one `ispe` property.
fn heif(iloc: (u8, Vec<u8>), ipma: Vec<u8>) -> Vec<u8> {
    let mut hdlr = vec![0; 4];
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0; 13]);
    let mut meta = full(b"hdlr", 0, &hdlr);
    meta.extend(full(b"pitm", 0, &1u16.to_be_bytes()));
    meta.extend(full(b"iloc", iloc.0, &iloc.1));
    let mut ispe = 40u32.to_be_bytes().to_vec();
    ispe.extend_from_slice(&30u32.to_be_bytes());
    let mut iprp = bx(b"ipco", &full(b"ispe", 0, &ispe));
    iprp.extend(full(b"ipma", 0, &ipma));
    meta.extend(bx(b"iprp", &iprp));
    let mut out = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    out.extend(full(b"meta", 0, &meta));
    out
}

/// iloc v0 with one item and no extent.
fn small_iloc() -> (u8, Vec<u8>) {
    (0, vec![0x44, 0, 0, 1, 0, 1, 0, 0, 0, 0])
}

/// ipma v0 with `entries` entries for `item`, each associating `count` times property 1.
fn ipma(entries: u32, item: u16, count: u8) -> Vec<u8> {
    let mut p = entries.to_be_bytes().to_vec();
    let mut entry = item.to_be_bytes().to_vec();
    entry.push(count);
    entry.extend(std::iter::repeat_n(0x81, usize::from(count)));
    for _ in 0..entries {
        p.extend_from_slice(&entry);
    }
    p
}

#[test]
fn hostile_boxes_fail_fast_with_bounded_memory() {
    let good = heif(small_iloc(), ipma(1, 1, 1));
    assert_eq!(
        display_size(&good),
        Ok((40, 30)),
        "the builder makes a valid file"
    );

    // iloc v1 with zero-size fields: every extent takes no input byte.
    let mut zero_extents = vec![0x00, 0x00];
    zero_extents.extend_from_slice(&u16::MAX.to_be_bytes());
    for id in 1..=u16::MAX {
        zero_extents.extend_from_slice(&id.to_be_bytes());
        zero_extents.extend_from_slice(&[0, 0, 0, 0]); // method, data reference
        zero_extents.extend_from_slice(&u16::MAX.to_be_bytes()); // extents
    }
    // iloc v2 with a million items of 10 bytes each.
    let mut many_items = vec![0x00, 0x00];
    many_items.extend_from_slice(&1_000_000u32.to_be_bytes());
    for id in 1..=1_000_000u32 {
        many_items.extend_from_slice(&id.to_be_bytes());
        many_items.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // method, dref, no extent
    }

    let hostile = [
        (
            "two million empty ipma entries",
            heif(small_iloc(), ipma(2_000_000, 7, 0)),
        ),
        (
            "20 000 entries of 255 associations",
            heif(small_iloc(), ipma(20_000, 1, 255)),
        ),
        (
            "4 000 entries of 255 associations",
            heif(small_iloc(), ipma(4_000, 1, 255)),
        ),
        ("a million iloc items", heif((2, many_items), ipma(1, 1, 1))),
        (
            "4 billion zero-byte extents",
            heif((1, zero_extents), ipma(1, 1, 1)),
        ),
    ];
    for (what, file) in &hostile {
        assert!(file.len() > 100_000, "{what}: {} bytes", file.len());
        let (size, peak, took) = measured(|| display_size(file));
        println!(
            "{what}: {} bytes in, peak {peak} bytes, {took:?}",
            file.len()
        );
        assert!(size.is_err(), "{what}: {size:?}");
        assert!(peak < BOUND, "{what}: display_size peaked at {peak} bytes");
        assert!(took < Duration::from_secs(5), "{what}: took {took:?}");
        let (rotation, peak, _) = measured(|| read_rotation(file));
        assert!(rotation.is_err(), "{what}");
        assert!(peak < BOUND, "{what}: read_rotation peaked at {peak} bytes");
        let (facts, peak, _) = measured(|| camera_facts(file));
        assert_eq!(facts, None, "{what}");
        assert!(peak < BOUND, "{what}: camera_facts peaked at {peak} bytes");
    }
}
