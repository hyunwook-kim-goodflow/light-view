//! The COM objects as Explorer uses them: created through the DLL's entry points or directly,
//! fed a stream from `SHCreateMemStream`, with a real sandboxed worker.
//!
//! The tests that create COM objects run one at a time ([`serial`]): `DllCanUnloadNow`
//! counts every object and worker thread of the process.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{Interface, Result, GUID, HRESULT};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_UNEXPECTED, S_FALSE, S_OK,
};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{IClassFactory, IStream, STREAM_SEEK_CUR};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
use windows::Win32::UI::Shell::{IThumbnailProvider, SHCreateMemStream, WTSAT_ARGB, WTS_ALPHATYPE};

use crate::bitmap::tests::take;
use crate::dll::{DllCanUnloadNow, DllGetClassObject};
use crate::thumbnail::{Provider, READ_CAP};
use crate::{guarded, worker_exe, CLSID_PREVIEW, CLSID_THUMBNAIL};

pub(crate) fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// `target/<profile>`, where `lv-worker.exe` and its codec DLLs are built.
pub(crate) fn profile_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("current exe");
    // target/<profile>/deps/lv_shell-<hash>.exe
    test_exe
        .parent()
        .and_then(Path::parent)
        .expect("test exe lives in target/<profile>/deps")
        .to_path_buf()
}

pub(crate) fn built_worker() -> PathBuf {
    let exe = profile_dir().join("lv-worker.exe");
    assert!(
        exe.is_file(),
        "{} is missing: run `cargo build -p lv-worker` first",
        exe.display()
    );
    exe
}

pub(crate) fn sample() -> Vec<u8> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/heif/rainbow-451x461.heic");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub(crate) fn mem_stream(bytes: &[u8]) -> IStream {
    // SAFETY: plain call; the stream copies `bytes`.
    unsafe { SHCreateMemStream(Some(bytes)) }.expect("SHCreateMemStream")
}

/// A thumbnail provider (made directly, with the built worker) initialized with `bytes`.
fn provider_with(bytes: &[u8]) -> IThumbnailProvider {
    let provider: IThumbnailProvider = Provider::new(built_worker()).into();
    let init: IInitializeWithStream = provider.cast().unwrap();
    // SAFETY: a valid stream; STGM_READ.
    unsafe { init.Initialize(&mem_stream(bytes), 0) }.expect("Initialize");
    provider
}

fn thumbnail(provider: &IThumbnailProvider, cx: u32) -> Result<(HBITMAP, WTS_ALPHATYPE)> {
    let mut bitmap = HBITMAP::default();
    let mut alpha = WTS_ALPHATYPE::default();
    // SAFETY: local out-pointers.
    unsafe { provider.GetThumbnail(cx, &mut bitmap, &mut alpha) }?;
    Ok((bitmap, alpha))
}

#[test]
fn thumbnail_of_heic_is_bitmap_of_requested_size() {
    let _serial = serial();
    let provider = provider_with(&sample());
    let (bitmap, alpha) = thumbnail(&provider, 256).expect("GetThumbnail");
    assert_eq!(alpha, WTSAT_ARGB);
    let (dib, pixels) = take(bitmap);
    // 451 x 461: the long side (height) becomes 256.
    assert_eq!(dib.dsBm.bmHeight, 256);
    assert_eq!(dib.dsBm.bmWidth, (451.0f64 * 256.0 / 461.0).round() as i32);
    assert_eq!(dib.dsBm.bmBitsPixel, 32);
    assert!(
        pixels.chunks_exact(4).all(|p| p[3] == 255),
        "an opaque HEIC is opaque"
    );
    assert!(
        pixels.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]),
        "not all black"
    );
}

#[test]
fn thumbnail_of_a_small_request_and_a_huge_one() {
    let _serial = serial();
    let (bitmap, _) = thumbnail(&provider_with(&sample()), 16).expect("cx 16");
    let (dib, _) = take(bitmap);
    assert_eq!(dib.dsBm.bmHeight, 16);
    // Far over what the worker may be asked for (clamped to 1024): never upscaled.
    let (bitmap, _) = thumbnail(&provider_with(&sample()), u32::MAX).expect("cx u32::MAX");
    let (dib, _) = take(bitmap);
    assert_eq!((dib.dsBm.bmWidth, dib.dsBm.bmHeight), (451, 461));
}

#[test]
fn corrupt_heic_fails_without_panic() {
    let _serial = serial();
    let mut corrupt = sample();
    // Keep the `ftyp` box, so the worker tries; scramble the rest.
    for (i, b) in corrupt.iter_mut().enumerate().skip(64) {
        *b = (i as u8).wrapping_mul(31);
    }
    for bytes in [corrupt, b"not an image at all".to_vec(), Vec::new()] {
        let provider = provider_with(&bytes);
        let mut bitmap = HBITMAP::default();
        let mut alpha = WTS_ALPHATYPE::default();
        // SAFETY: local out-pointers.
        let result = unsafe { provider.GetThumbnail(256, &mut bitmap, &mut alpha) };
        assert!(result.is_err(), "{} bytes", bytes.len());
        assert!(bitmap.is_invalid(), "no bitmap on failure");
    }
}

#[test]
fn truncated_stream_fails() {
    let _serial = serial();
    let whole = sample();
    let provider = provider_with(&whole[..whole.len() / 2]);
    assert!(thumbnail(&provider, 256).is_err());
}

/// Over the shell's 64 MiB cap by one byte: refused before any read (the reported size is
/// over the cap), so explorer.exe never holds such a file.
#[test]
fn stream_reporting_over_64_mib_is_refused_before_reading() {
    let _serial = serial();
    assert_eq!(READ_CAP, 64 * 1024 * 1024);
    assert!(READ_CAP <= lv_core::decode::max_file_bytes(&lv_core::decode::DecodeLimits::default()));
    let stream = mem_stream(&vec![0u8; READ_CAP as usize + 1]);
    let provider: IThumbnailProvider = Provider::new(built_worker()).into();
    let init: IInitializeWithStream = provider.cast().unwrap();
    // SAFETY: a valid stream.
    unsafe { init.Initialize(&stream, 0) }.expect("Initialize");
    assert!(thumbnail(&provider, 256).is_err());
    let mut position = u64::MAX;
    // SAFETY: a valid stream and a local out-pointer.
    unsafe { stream.Seek(0, STREAM_SEEK_CUR, Some(&mut position)) }.unwrap();
    assert_eq!(position, 0, "read from a stream over the cap");
}

#[test]
fn null_out_pointers_and_zero_size_are_refused() {
    let _serial = serial();
    let provider = provider_with(&sample());
    let mut bitmap = HBITMAP::default();
    let mut alpha = WTS_ALPHATYPE::default();
    // SAFETY: null pointers are what is tested; the provider checks them.
    unsafe {
        assert!(provider
            .GetThumbnail(256, std::ptr::null_mut(), &mut alpha)
            .is_err());
        assert!(provider
            .GetThumbnail(256, &mut bitmap, std::ptr::null_mut())
            .is_err());
        assert!(provider.GetThumbnail(0, &mut bitmap, &mut alpha).is_err());
    }
}

#[test]
fn get_thumbnail_before_initialize_fails() {
    let _serial = serial();
    let provider: IThumbnailProvider = Provider::new(built_worker()).into();
    assert_eq!(thumbnail(&provider, 256).unwrap_err().code(), E_UNEXPECTED);
}

#[test]
fn initialize_twice_is_refused() {
    let _serial = serial();
    let provider = provider_with(&sample());
    let init: IInitializeWithStream = provider.cast().unwrap();
    // SAFETY: a valid stream.
    let again = unsafe { init.Initialize(&mem_stream(b"other"), 0) };
    assert!(again.is_err(), "second Initialize accepted");
    // The first stream is kept.
    let (bitmap, _) = thumbnail(&provider, 64).expect("thumbnail of the first stream");
    take(bitmap);
}

#[test]
fn a_panic_is_an_error_not_an_unwind() {
    let result: Result<()> = guarded(|| panic!("expected in this test"));
    assert!(result.is_err());
}

#[test]
fn only_our_two_classes_are_served() {
    let _serial = serial();
    let mut out: *mut c_void = std::ptr::null_mut();
    for clsid in [CLSID_THUMBNAIL, CLSID_PREVIEW] {
        // SAFETY: valid GUID pointers and a local out-pointer.
        let hr = unsafe { DllGetClassObject(&clsid, &IClassFactory::IID, &mut out) };
        assert_eq!(hr, S_OK);
        // SAFETY: on S_OK `out` holds an IClassFactory reference that we now own.
        drop(unsafe { IClassFactory::from_raw(out) });
    }
    let thumbnail_class = GUID::from_u128(0x6A77F04F_33D3_40F0_885E_AA8CC5CC403C);
    let preview_class = GUID::from_u128(0x663472B2_A0DD_43AA_B3F1_97CEB0598154);
    assert_eq!(
        (CLSID_THUMBNAIL, CLSID_PREVIEW),
        (thumbnail_class, preview_class)
    );
    let details_class = GUID::from_u128(0x0EA2AA38_AE7B_4906_9BD9_605EA0348914);
    for clsid in [details_class, GUID::zeroed()] {
        // SAFETY: valid GUID pointers and a local out-pointer.
        let hr = unsafe { DllGetClassObject(&clsid, &IClassFactory::IID, &mut out) };
        assert_eq!(hr, CLASS_E_CLASSNOTAVAILABLE);
        assert!(out.is_null());
    }
    // A null GUID pointer fails and still nulls `*out`.
    out = std::ptr::NonNull::<c_void>::dangling().as_ptr();
    // SAFETY: null pointers are what is tested; `out` is only written, never read through.
    let hr = unsafe { DllGetClassObject(std::ptr::null(), &IClassFactory::IID, &mut out) };
    assert!(hr.is_err());
    assert!(out.is_null(), "*out not nulled on failure");
}

/// The class factory from `DllGetClassObject`, as COM gets it.
fn class_factory() -> IClassFactory {
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid GUID pointers and a local out-pointer.
    let hr = unsafe { DllGetClassObject(&CLSID_THUMBNAIL, &IClassFactory::IID, &mut out) };
    assert_eq!(hr, S_OK);
    // SAFETY: on S_OK `out` holds an IClassFactory reference that we now own.
    unsafe { IClassFactory::from_raw(out) }
}

pub(crate) fn wait_until_unloadable() -> HRESULT {
    let deadline = Instant::now() + Duration::from_secs(10);
    while DllCanUnloadNow() != S_OK && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    DllCanUnloadNow()
}

#[test]
fn dll_can_unload_after_release() {
    let _serial = serial();
    assert_eq!(wait_until_unloadable(), S_OK, "something left from before");

    let factory = class_factory();
    assert_eq!(DllCanUnloadNow(), S_FALSE, "the factory is alive");
    // SAFETY: plain calls on a valid factory.
    let made: Result<IThumbnailProvider> = unsafe { factory.CreateInstance(None) };
    let made = made.expect("CreateInstance");
    assert!(made.cast::<IInitializeWithStream>().is_ok());
    let unknown: windows::core::IUnknown = made.cast().unwrap();
    // SAFETY: aggregation is refused.
    let aggregated: Result<IThumbnailProvider> = unsafe { factory.CreateInstance(&unknown) };
    assert_eq!(aggregated.unwrap_err().code(), CLASS_E_NOAGGREGATION);
    drop(unknown);
    drop(factory);
    assert_eq!(DllCanUnloadNow(), S_FALSE, "the provider is alive");
    drop(made);
    assert_eq!(DllCanUnloadNow(), S_OK);

    // A server lock holds the DLL too.
    let factory = class_factory();
    // SAFETY: plain calls on a valid factory.
    unsafe { factory.LockServer(true) }.unwrap();
    drop(factory);
    assert_eq!(DllCanUnloadNow(), S_FALSE, "locked");
    let factory = class_factory();
    // SAFETY: plain calls on a valid factory.
    unsafe { factory.LockServer(false) }.unwrap();
    drop(factory);
    assert_eq!(DllCanUnloadNow(), S_OK);

    // After a thumbnail, the worker's helper threads hold it until they end.
    let provider = provider_with(&sample());
    let (bitmap, _) = thumbnail(&provider, 128).expect("GetThumbnail");
    take(bitmap);
    drop(provider);
    assert_eq!(wait_until_unloadable(), S_OK);
    assert_eq!(lv_sandbox::live_threads(), 0);
}

#[test]
fn worker_exe_is_next_to_this_module() {
    let module = std::env::current_exe().unwrap();
    assert_eq!(
        worker_exe().unwrap(),
        module.with_file_name("lv-worker.exe")
    );
}

/// The names of the DLLs `image` (a PE file) imports, directly or delay-loaded, lower case.
fn imported_dlls(image: &Path) -> Vec<String> {
    let b = std::fs::read(image).unwrap_or_else(|e| panic!("{}: {e}", image.display()));
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize;
    let pe = u32_at(0x3c);
    assert_eq!(&b[pe..pe + 4], b"PE\0\0");
    let coff = pe + 4;
    let sections = u16_at(coff + 2);
    let optional = coff + 20;
    assert_eq!(u16_at(optional), 0x20b, "PE32+");
    let directory = |i: usize| u32_at(optional + 112 + 8 * i);
    let section_table = optional + u16_at(coff + 16);
    let offset = |rva: usize| {
        (0..sections)
            .map(|s| section_table + 40 * s)
            .find_map(|s| {
                let (va, raw_len, raw) = (u32_at(s + 12), u32_at(s + 16), u32_at(s + 20));
                (va..va + raw_len).contains(&rva).then(|| rva - va + raw)
            })
            .expect("RVA in a section")
    };
    let name = |rva: usize| {
        let start = offset(rva);
        let end = start + b[start..].iter().position(|&c| c == 0).unwrap();
        String::from_utf8_lossy(&b[start..end]).to_ascii_lowercase()
    };
    let mut names = Vec::new();
    // Import descriptors (20 bytes, name at 12) and delay-load descriptors (32, name at 4).
    for (dir, size, name_at) in [(1, 20, 12), (13, 32, 4)] {
        let rva = directory(dir);
        if rva == 0 {
            continue;
        }
        let mut d = offset(rva);
        while b[d..d + size].iter().any(|&x| x != 0) {
            names.push(name(u32_at(d + name_at)));
            d += size;
        }
    }
    names
}

/// No C library may enter the thumbnail DLL (Global Constraints): it imports Windows DLLs
/// only. Checked on the built DLL when it is there (cargo builds it with the workspace) and
/// always on this test binary, which links the same crates.
#[test]
fn no_c_library_is_imported() {
    let windows = [
        "kernel32.dll",
        "ntdll.dll",
        "advapi32.dll",
        "ole32.dll",
        "oleaut32.dll",
        "user32.dll",
        "gdi32.dll",
        "shell32.dll",
        "shlwapi.dll",
        "userenv.dll",
        "ws2_32.dll",
        "bcrypt.dll",
        "bcryptprimitives.dll",
        "propsys.dll",
        "combase.dll",
        "secur32.dll",
        "vcruntime140.dll",
        "ucrtbase.dll",
    ];
    let mut images = vec![std::env::current_exe().unwrap()];
    let dll = profile_dir().join("lv_shell.dll");
    if dll.is_file() {
        images.push(dll);
    }
    for image in images {
        let names = imported_dlls(&image);
        assert!(names.iter().any(|n| n == "kernel32.dll"), "{names:?}");
        for name in &names {
            assert!(
                name.starts_with("api-ms-win-") || windows.contains(&name.as_str()),
                "{} imports {name}",
                image.display()
            );
        }
    }
}
