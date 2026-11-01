//! The details handler as Explorer and the search indexer use it: made through the DLL's
//! entry points or directly, fed a stream from `SHCreateMemStream`.
//!
//! The tests that make a store hold [`serial`]: `DllCanUnloadNow` counts every object of the
//! process, and `property_store_never_starts_worker` asserts on it.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::core::{Interface, Result, PROPVARIANT};
use windows::Win32::Foundation::{
    E_INVALIDARG, E_POINTER, FILETIME, STG_E_ACCESSDENIED, SYSTEMTIME, S_FALSE, S_OK,
};
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_GPS_Latitude, PKEY_GPS_LatitudeRef, PKEY_GPS_Longitude, PKEY_Image_Dimensions,
    PKEY_Image_HorizontalSize, PKEY_Image_VerticalSize, PKEY_Photo_CameraManufacturer,
    PKEY_Photo_CameraModel, PKEY_Photo_DateTaken,
};
use windows::Win32::System::Com::{IClassFactory, STGM_READ, STGM_READWRITE, STGM_WRITE};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IPropertyStore, IPropertyStoreCapabilities, PROPERTYKEY,
};

use super::boundary::{used_paths, violations, ALLOWED};
use super::read::{heif_properties, DateTaken, HeifProps};
use super::Store;
use crate::dll::{DllCanUnloadNow, DllGetClassObject};
use crate::heif_build::{self, irot, Builder};
use crate::tests::{mem_stream, serial, wait_until_unloadable, WORKER_EXE_CALLS};
use crate::CLSID_DETAILS;

// ---- Fixtures ---------------------------------------------------------------------------

/// One TIFF entry: tag, type, count, value bytes (little-endian).
type Entry = (u16, u16, u32, Vec<u8>);

fn ascii(tag: u16, s: &str) -> Entry {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    (tag, 2, b.len() as u32, b)
}

fn long(tag: u16, v: u32) -> Entry {
    (tag, 4, 1, v.to_le_bytes().to_vec())
}

fn rationals(tag: u16, v: &[(u32, u32)]) -> Entry {
    let b = v
        .iter()
        .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
        .collect();
    (tag, 5, v.len() as u32, b)
}

/// Writes one IFD (entries in tag order, values over 4 bytes after it) and returns where.
fn write_ifd(out: &mut Vec<u8>, entries: &[Entry]) -> u32 {
    let at = out.len();
    let mut entries = entries.to_vec();
    entries.sort_by_key(|e| e.0);
    let data_at = at + 2 + 12 * entries.len() + 4;
    let mut data = Vec::new();
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in &entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if value.len() <= 4 {
            let mut inline = value.clone();
            inline.resize(4, 0);
            out.extend_from_slice(&inline);
        } else {
            out.extend_from_slice(&((data_at + data.len()) as u32).to_le_bytes());
            data.extend_from_slice(value);
            if data.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&data);
    at as u32
}

/// A little-endian TIFF block: `ifd0` plus pointers to an Exif IFD and a GPS IFD.
fn tiff(ifd0: &[Entry], exif: &[Entry], gps: &[Entry]) -> Vec<u8> {
    let mut out = b"II*\0\0\0\0\0".to_vec();
    let exif_at = write_ifd(&mut out, exif);
    let gps_at = write_ifd(&mut out, gps);
    let mut ifd0 = ifd0.to_vec();
    ifd0.push(long(0x8769, exif_at));
    ifd0.push(long(0x8825, gps_at));
    let ifd0_at = write_ifd(&mut out, &ifd0);
    out[4..8].copy_from_slice(&ifd0_at.to_le_bytes());
    out
}

/// The EXIF of the fixtures: an iPhone, a date with its offset, and a position (which must
/// never be exported).
fn camera_tiff(date: &str, offset: Option<&str>) -> Vec<u8> {
    let mut exif = vec![ascii(0x9003, date)];
    exif.extend(offset.map(|o| ascii(0x9011, o)));
    tiff(
        &[ascii(0x010F, "Apple"), ascii(0x0110, "iPhone 15 Pro")],
        &exif,
        &[
            ascii(0x0001, "N"),
            rationals(0x0002, &[(37, 1), (33, 1), (0, 1)]),
            ascii(0x0003, "E"),
            rationals(0x0004, &[(126, 1), (58, 1), (0, 1)]),
        ],
    )
}

/// The rainbow image (451x461 after its `clap`) with `transforms`, and an EXIF item holding
/// `tiff` after the image data in `mdat`.
fn with_exif(transforms: &[Vec<u8>], tiff: &[u8]) -> Vec<u8> {
    let mut b = Builder::new();
    let image = b.image(&heif_build::rainbow(), true, transforms);
    b.exif(image, tiff);
    b.primary(image);
    b.finish()
}

const FIXTURE: &str = "rainbow-451x461-exif.heic";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/heif")
        .join(name)
}

fn fixture() -> Vec<u8> {
    with_exif(&[], &camera_tiff("2024:05:06 07:08:09", Some("+09:00")))
}

/// 2024-05-05T22:08:09Z (2024-05-06 07:08:09 at +09:00) in FILETIME ticks.
const TAKEN_UTC: u64 = 133_594_204_890_000_000;

#[test]
#[ignore = "writes testdata/heif/rainbow-451x461-exif.heic"]
fn write_exif_fixture() {
    std::fs::write(fixture_path(FIXTURE), fixture()).unwrap();
}

#[test]
fn committed_exif_fixture_is_what_the_builder_makes() {
    let committed = std::fs::read(fixture_path(FIXTURE)).expect("committed fixture");
    assert!(committed == fixture(), "run the ignored write_exif_fixture");
}

// ---- The COM object ---------------------------------------------------------------------

fn store_with(bytes: &[u8]) -> IPropertyStore {
    let store: IPropertyStore = Store::new().into();
    let init: IInitializeWithStream = store.cast().unwrap();
    // SAFETY: a valid stream; STGM_READ.
    unsafe { init.Initialize(&mem_stream(bytes), STGM_READ.0) }.expect("Initialize");
    store
}

fn keys(store: &IPropertyStore) -> Vec<PROPERTYKEY> {
    // SAFETY: plain calls with a local out-pointer.
    let count = unsafe { store.GetCount() }.unwrap();
    (0..count)
        .map(|i| {
            let mut key = PROPERTYKEY::default();
            unsafe { store.GetAt(i, &mut key) }.unwrap();
            key
        })
        .collect()
}

fn value(store: &IPropertyStore, key: PROPERTYKEY) -> PROPVARIANT {
    // SAFETY: a local key.
    unsafe { store.GetValue(&key) }.expect("GetValue")
}

fn vt(v: &PROPVARIANT) -> u16 {
    // SAFETY: `vt` is valid in every PROPVARIANT.
    unsafe { v.as_raw().Anonymous.Anonymous.vt }
}

const VT_EMPTY: u16 = 0;
const VT_UI4: u16 = 19;
const VT_LPWSTR: u16 = 31;
const VT_FILETIME: u16 = 64;

fn u32_of(v: &PROPVARIANT) -> u32 {
    assert_eq!(vt(v), VT_UI4);
    // SAFETY: checked VT_UI4.
    unsafe { v.as_raw().Anonymous.Anonymous.Anonymous.ulVal }
}

fn text_of(v: &PROPVARIANT) -> String {
    assert_eq!(vt(v), VT_LPWSTR);
    // SAFETY: checked VT_LPWSTR: a NUL-terminated wide string the PROPVARIANT owns.
    unsafe {
        let p = v.as_raw().Anonymous.Anonymous.Anonymous.pwszVal;
        windows::core::PCWSTR(p.cast()).to_string().unwrap()
    }
}

fn ticks_of(v: &PROPVARIANT) -> u64 {
    assert_eq!(vt(v), VT_FILETIME);
    // SAFETY: checked VT_FILETIME.
    let t = unsafe { v.as_raw().Anonymous.Anonymous.Anonymous.filetime };
    u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)
}

const EXPORTED: [PROPERTYKEY; 6] = [
    PKEY_Image_HorizontalSize,
    PKEY_Image_VerticalSize,
    PKEY_Image_Dimensions,
    PKEY_Photo_DateTaken,
    PKEY_Photo_CameraManufacturer,
    PKEY_Photo_CameraModel,
];

// ---- Tests ------------------------------------------------------------------------------

#[test]
fn heif_properties_of_sample() {
    let _serial = serial();
    let props = heif_properties(&std::fs::read(fixture_path(FIXTURE)).unwrap()).unwrap();
    assert_eq!(
        props,
        HeifProps {
            width: 451,
            height: 461,
            date_taken: Some(DateTaken::Utc(TAKEN_UTC)),
            make: Some("Apple".into()),
            model: Some("iPhone 15 Pro".into()),
        }
    );
    // The samples without EXIF: the size only.
    for (name, size) in [
        ("rainbow-451x461.heic", (451, 461)),
        ("grid-4032x3024-thumbs.heic", (4032, 3024)),
    ] {
        let props = heif_properties(&std::fs::read(fixture_path(name)).unwrap()).unwrap();
        assert_eq!((props.width, props.height), size, "{name}");
        assert_eq!(
            (props.date_taken, props.make, props.model),
            (None, None, None)
        );
    }

    let store = store_with(&std::fs::read(fixture_path(FIXTURE)).unwrap());
    assert_eq!(keys(&store), EXPORTED);
    assert_eq!(u32_of(&value(&store, PKEY_Image_HorizontalSize)), 451);
    assert_eq!(u32_of(&value(&store, PKEY_Image_VerticalSize)), 461);
    assert_eq!(text_of(&value(&store, PKEY_Image_Dimensions)), "451 x 461");
    assert_eq!(ticks_of(&value(&store, PKEY_Photo_DateTaken)), TAKEN_UTC);
    assert_eq!(
        text_of(&value(&store, PKEY_Photo_CameraManufacturer)),
        "Apple"
    );
    assert_eq!(
        text_of(&value(&store, PKEY_Photo_CameraModel)),
        "iPhone 15 Pro"
    );
}

#[test]
fn rotated_heic_reports_rotated_size() {
    let _serial = serial();
    let irot90 = std::fs::read(fixture_path("rainbow-451x461-irot90.heic")).unwrap();
    let props = heif_properties(&irot90).unwrap();
    assert_eq!((props.width, props.height), (461, 451));

    let turned = with_exif(&[irot(1)], &camera_tiff("2024:05:06 07:08:09", None));
    let props = heif_properties(&turned).unwrap();
    assert_eq!((props.width, props.height), (461, 451));
    assert_eq!(props.model.as_deref(), Some("iPhone 15 Pro"));
    let store = store_with(&turned);
    assert_eq!(u32_of(&value(&store, PKEY_Image_HorizontalSize)), 461);
    assert_eq!(u32_of(&value(&store, PKEY_Image_VerticalSize)), 451);
    assert_eq!(text_of(&value(&store, PKEY_Image_Dimensions)), "461 x 451");
    // A half turn keeps the size.
    let props = heif_properties(&with_exif(&[irot(2)], &[])).unwrap();
    assert_eq!((props.width, props.height), (451, 461));
}

/// Without `OffsetTimeOriginal` the date is this machine's local time, as Windows' photo
/// handlers read it: shown back in local time, it is the time the camera wrote.
#[test]
fn a_date_without_offset_is_local_time() {
    let _serial = serial();
    let file = with_exif(&[], &camera_tiff("2024:05:06 07:08:09", None));
    let props = heif_properties(&file).unwrap();
    let Some(DateTaken::Local(_)) = props.date_taken else {
        panic!("{:?}", props.date_taken);
    };
    let ticks = ticks_of(&value(&store_with(&file), PKEY_Photo_DateTaken));
    let utc = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let (mut utc_time, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: local in- and out-structs; the current time zone.
    unsafe {
        FileTimeToSystemTime(&utc, &mut utc_time).unwrap();
        SystemTimeToTzSpecificLocalTime(None, &utc_time, &mut local).unwrap();
    }
    let wall = (
        local.wYear,
        local.wMonth,
        local.wDay,
        local.wHour,
        local.wMinute,
        local.wSecond,
    );
    assert_eq!(wall, (2024, 5, 6, 7, 8, 9));
}

#[test]
fn unusable_dates_are_left_out() {
    let _serial = serial();
    for (date, offset) in [
        ("0000:00:00 00:00:00", None),
        ("2024:02:30 07:08:09", Some("+09:00")),
        ("garbage", None),
        // Before 1601 once in UTC.
        ("1601:01:01 00:30:00", Some("+01:00")),
    ] {
        let props = heif_properties(&with_exif(&[], &camera_tiff(date, offset))).unwrap();
        assert_eq!(props.date_taken, None, "{date}");
        assert_eq!(props.make.as_deref(), Some("Apple"));
        let store = store_with(&with_exif(&[], &camera_tiff(date, offset)));
        assert!(!keys(&store).contains(&PKEY_Photo_DateTaken), "{date}");
        assert_eq!(vt(&value(&store, PKEY_Photo_DateTaken)), VT_EMPTY);
    }
    // A bad offset is ignored: the date is then local time.
    let props = heif_properties(&with_exif(
        &[],
        &camera_tiff("2024:05:06 07:08:09", Some("+9")),
    ))
    .unwrap();
    assert!(matches!(props.date_taken, Some(DateTaken::Local(_))));
}

#[test]
fn property_store_survives_corrupt_heic() {
    let _serial = serial();
    let whole = fixture();
    let mut scrambled = whole.clone();
    for (i, b) in scrambled.iter_mut().enumerate().skip(32) {
        *b = (i as u8).wrapping_mul(31);
    }
    let mut not_heif = vec![0xFF, 0xD8, 0xFF, 0xE0];
    not_heif.extend_from_slice(&whole[4..]);
    for bytes in [
        Vec::new(),
        b"not an image at all".to_vec(),
        scrambled,
        not_heif,
        // Cut inside `meta`.
        whole[..200].to_vec(),
    ] {
        assert_eq!(heif_properties(&bytes), None, "{} bytes", bytes.len());
        let store = store_with(&bytes);
        // SAFETY: plain call.
        assert_eq!(unsafe { store.GetCount() }.unwrap(), 0);
        for key in EXPORTED {
            assert_eq!(vt(&value(&store, key)), VT_EMPTY);
        }
        let mut key = PROPERTYKEY::default();
        // SAFETY: a local out-pointer.
        let at = unsafe { store.GetAt(0, &mut key) };
        assert_eq!(at.unwrap_err().code(), E_INVALIDARG);
    }
    // Every cut of the file: never a panic. Once `meta` is whole the size is known; the
    // EXIF item, last in `mdat`, is read only when it is all there.
    for len in 0..=whole.len() {
        if let Some(props) = heif_properties(&whole[..len]) {
            assert_eq!((props.width, props.height), (451, 461));
            assert_eq!(props.make.is_some(), len == whole.len(), "{len}");
        }
    }
    // A container that parses with EXIF that does not: the size only.
    let props = heif_properties(&with_exif(&[], b"II*\0garbage garbage")).unwrap();
    assert_eq!((props.width, props.height, props.make), (451, 461, None));
    // Byte flips anywhere never panic.
    let mut state = 0x2545_F491u32;
    for _ in 0..2000 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let mut b = whole.clone();
        let at = state as usize % b.len();
        b[at] = (state >> 24) as u8;
        let _ = heif_properties(&b);
    }
}

/// The details code is never a way to a worker: it is called by the SYSTEM search indexer.
/// Every path its sources (and the stream reader it uses) name is on the allowlist of
/// `boundary.rs`, which has nothing of the sandbox, the worker, the other handlers,
/// processes, threads, files, the network or C; and making and using one through the DLL
/// asks for no worker path and leaves no thread.
#[test]
fn property_store_never_starts_worker() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut used = Vec::new();
    for file in ["properties.rs", "properties/read.rs", "stream.rs"] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        assert_eq!(violations(&text), Vec::<String>::new(), "{file}");
        used.extend(used_paths(&text));
    }
    // The scan sees the imports, and the list holds nothing they do not use.
    for allowed in ALLOWED {
        assert!(
            used.iter().any(|u| u == allowed),
            "{allowed} is listed but unused"
        );
    }

    let _serial = serial();
    assert_eq!(wait_until_unloadable(), S_OK, "something left from before");
    WORKER_EXE_CALLS.with(|c| c.set(0));
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid GUID pointers and a local out-pointer.
    let hr = unsafe { DllGetClassObject(&CLSID_DETAILS, &IClassFactory::IID, &mut out) };
    assert_eq!(hr, S_OK);
    // SAFETY: on S_OK `out` holds an IClassFactory reference that we now own.
    let factory = unsafe { IClassFactory::from_raw(out) };
    // SAFETY: a plain call on a valid factory.
    let store: IPropertyStore = unsafe { factory.CreateInstance(None) }.expect("CreateInstance");
    drop(factory);
    assert!(store.cast::<IPropertyStoreCapabilities>().is_ok());
    let init: IInitializeWithStream = store.cast().unwrap();
    // SAFETY: a valid stream.
    unsafe { init.Initialize(&mem_stream(&fixture()), STGM_READ.0) }.unwrap();
    assert_eq!(
        text_of(&value(&store, PKEY_Photo_CameraModel)),
        "iPhone 15 Pro"
    );
    assert_eq!(
        WORKER_EXE_CALLS.with(|c| c.get()),
        0,
        "asked for the worker"
    );
    assert_eq!(lv_sandbox::live_threads(), 0, "a thread was started");
    assert_eq!(DllCanUnloadNow(), S_FALSE, "the store holds the DLL");
    drop(init);
    drop(store);
    assert_eq!(DllCanUnloadNow(), S_OK);
}

#[test]
fn no_gps_properties_exported() {
    let _serial = serial();
    let store = store_with(&fixture());
    let keys = keys(&store);
    assert_eq!(keys, EXPORTED, "exactly the six properties");
    for gps in [PKEY_GPS_Latitude, PKEY_GPS_LatitudeRef, PKEY_GPS_Longitude] {
        assert!(!keys.contains(&gps));
        assert_eq!(vt(&value(&store, gps)), VT_EMPTY);
    }
    // The fixture does carry a position.
    let summary = lv_core::exif_info::read_exif(&fixture()).unwrap().0;
    assert!(summary.gps.is_some());
}

#[test]
fn set_value_is_denied() {
    let _serial = serial();
    let store = store_with(&fixture());
    let v = PROPVARIANT::from(5u32);
    // SAFETY: a local key and value.
    let set = unsafe { store.SetValue(&PKEY_Image_HorizontalSize, &v) };
    assert_eq!(set.unwrap_err().code(), STG_E_ACCESSDENIED);
    // SAFETY: plain call.
    assert_eq!(
        unsafe { store.Commit() }.unwrap_err().code(),
        STG_E_ACCESSDENIED
    );
    assert_eq!(
        u32_of(&value(&store, PKEY_Image_HorizontalSize)),
        451,
        "unchanged"
    );

    // Every property is read-only: S_FALSE, read through the vtable (the wrapper calls it
    // Ok).
    let caps: IPropertyStoreCapabilities = store.cast().unwrap();
    for key in EXPORTED.iter().chain([&PKEY_GPS_Latitude]) {
        // SAFETY: a valid interface and a local key.
        let hr = unsafe { (Interface::vtable(&caps).IsPropertyWritable)(caps.as_raw(), key) };
        assert_eq!(hr, S_FALSE);
    }

    // Opening for writing is refused up front.
    for mode in [STGM_WRITE.0, STGM_READWRITE.0] {
        let fresh: IInitializeWithStream = IPropertyStore::from(Store::new()).cast().unwrap();
        // SAFETY: a valid stream.
        let r = unsafe { fresh.Initialize(&mem_stream(&fixture()), mode) };
        assert_eq!(r.unwrap_err().code(), STG_E_ACCESSDENIED);
    }
}

#[test]
fn initialize_rules_and_null_pointers() -> Result<()> {
    let _serial = serial();
    let store: IPropertyStore = Store::new().into();
    // Before Initialize: nothing, and no error.
    // SAFETY: plain calls with local pointers.
    unsafe {
        assert_eq!(store.GetCount()?, 0);
        assert_eq!(vt(&store.GetValue(&PKEY_Image_HorizontalSize)?), VT_EMPTY);
    }
    let init: IInitializeWithStream = store.cast()?;
    // SAFETY: null stream and pointers are what is tested; the store checks them.
    unsafe {
        assert_eq!(init.Initialize(None, 0).unwrap_err().code(), E_INVALIDARG);
        init.Initialize(&mem_stream(&fixture()), STGM_READ.0)?;
        let again = init.Initialize(&mem_stream(b"other"), STGM_READ.0);
        assert!(again.is_err(), "second Initialize accepted");
        assert_eq!(store.GetCount()?, 6, "the first stream is kept");
        let get_at = store.GetAt(0, std::ptr::null_mut());
        assert_eq!(get_at.unwrap_err().code(), E_POINTER);
        let get_value = store.GetValue(std::ptr::null());
        assert_eq!(get_value.unwrap_err().code(), E_POINTER);
    }
    // A stream read from the middle is still read whole.
    let stream = mem_stream(&fixture());
    // SAFETY: a valid stream.
    unsafe { stream.Seek(100, windows::Win32::System::Com::STREAM_SEEK_SET, None)? };
    let moved: IPropertyStore = Store::new().into();
    // SAFETY: a valid stream.
    unsafe {
        moved
            .cast::<IInitializeWithStream>()?
            .Initialize(&stream, STGM_READ.0)?
    };
    assert_eq!(keys(&moved), EXPORTED);
    Ok(())
}

/// Over the shell's 64 MiB cap: Initialize fails without reading, like the other handlers.
#[test]
fn a_stream_over_the_cap_is_refused() {
    let _serial = serial();
    let stream = mem_stream(&vec![0u8; crate::stream::READ_CAP as usize + 1]);
    let store: IPropertyStore = Store::new().into();
    let init: IInitializeWithStream = store.cast().unwrap();
    // SAFETY: a valid stream.
    assert!(unsafe { init.Initialize(&stream, STGM_READ.0) }.is_err());
    // SAFETY: plain call.
    assert_eq!(unsafe { store.GetCount() }.unwrap(), 0);
}

/// The boundary scan refuses what the old deny-list let through: a module imported under a
/// short name, a thread started through Win32, a process or file reached after a test
/// module, an alias, a new handler's code.
#[test]
fn the_boundary_scan_refuses_new_ways_out() {
    let refused = |source: &str| {
        let found = violations(source);
        assert!(!found.is_empty(), "accepted: {source}");
        found
    };
    let found = refused(
        "use std::{fs, io};
fn f() { let _ = fs::read(\"x\"); }
",
    );
    assert!(found.contains(&"use std::fs".to_string()), "{found:?}");
    assert!(found.contains(&"use std::io".to_string()), "{found:?}");
    assert!(found.contains(&"path fs::read".to_string()), "{found:?}");
    let found = refused(
        "fn f() { unsafe { windows::Win32::System::Threading::CreateThread(None, 0, None, None, 0, None) }; }",
    );
    assert_eq!(
        found,
        ["path windows::Win32::System::Threading::CreateThread"]
    );
    refused("use windows::Win32::System::Threading::{CreateThread as Go};");
    refused("use windows::Win32::UI::Shell::SHCreateStreamOnFileEx;");
    refused("use crate::{guarded, preview::Handler};");
    refused("use crate::stream;");
    refused("use windows::Win32::Foundation::*;");
    refused("extern \"system\" { fn WinExec(); }");
    // Test modules are skipped, but product code after one is not.
    let found = refused(
        "#[cfg(test)]
mod tests {
    use std::fs;
}
fn later() { std::process::exit(0) }
",
    );
    assert_eq!(found, ["path std::process::exit"]);
    // Comments, strings, primitives and type paths are not paths out.
    let fine = "// std::fs::read
use crate::guarded;
fn f() -> u32 { let _ = \"std::fs\";                 let _ = '\''; u32::from(1u8) + Value::Count as u32 }
";
    assert_eq!(violations(fine), Vec::<String>::new());
}
