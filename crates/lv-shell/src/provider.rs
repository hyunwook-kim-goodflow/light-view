//! SPIKE: `IThumbnailProvider` + `IInitializeWithStream` answering a lime bitmap.

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use windows::core::{implement, IUnknown, Interface, Result, GUID};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_FAIL, E_UNEXPECTED};
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    IThumbnailProvider, IThumbnailProvider_Impl, WTSAT_RGB, WTS_ALPHATYPE,
};

use crate::{host, spike_log, worker, ObjectCount};

/// Most bytes the spike reads from a stream.
const READ_CAP: usize = 64 * 1024 * 1024;

pub fn create(outer: Option<&IUnknown>, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
    if outer.is_some() {
        return Err(CLASS_E_NOAGGREGATION.into());
    }
    let provider: IThumbnailProvider = Provider {
        stream: RefCell::new(None),
        _count: ObjectCount::new(),
    }
    .into();
    // SAFETY: COM contract from the caller: valid iid and out pointers.
    unsafe { provider.query(iid, out).ok() }
}

#[implement(IThumbnailProvider, IInitializeWithStream)]
struct Provider {
    stream: RefCell<Option<IStream>>,
    _count: ObjectCount,
}

#[allow(non_snake_case)]
impl IInitializeWithStream_Impl for Provider_Impl {
    fn Initialize(&self, stream: Option<&IStream>, mode: u32) -> Result<()> {
        spike_log(&format!("Initialize(mode={mode:#x})"));
        let mut slot = self.stream.borrow_mut();
        if slot.is_some() {
            return Err(E_UNEXPECTED.into());
        }
        *slot = stream.cloned();
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IThumbnailProvider_Impl for Provider_Impl {
    fn GetThumbnail(&self, cx: u32, bitmap: *mut HBITMAP, alpha: *mut WTS_ALPHATYPE) -> Result<()> {
        let result = catch_unwind(AssertUnwindSafe(|| self.thumbnail(cx, bitmap, alpha)));
        result.unwrap_or_else(|_| {
            spike_log("PANIC in GetThumbnail");
            Err(E_FAIL.into())
        })
    }
}

impl Provider_Impl {
    fn thumbnail(&self, cx: u32, bitmap: *mut HBITMAP, alpha: *mut WTS_ALPHATYPE) -> Result<()> {
        let t = Instant::now();
        spike_log(&format!(
            "GetThumbnail(cx={cx}) host: {}",
            host::describe_self()
        ));
        let stream = self.stream.borrow().clone().ok_or(E_UNEXPECTED)?;
        let data = read_all(&stream)?;
        spike_log(&format!(
            "read {} bytes from the stream in {:?}",
            data.len(),
            t.elapsed()
        ));
        let summary = worker::decode_once(data);
        let hbm = lime(cx)?;
        // SAFETY: COM contract: valid out pointers.
        unsafe {
            *bitmap = hbm;
            *alpha = WTSAT_RGB;
        }
        spike_log(&format!(
            "GetThumbnail done in {:?} (worker: {summary})",
            t.elapsed()
        ));
        Ok(())
    }
}

fn read_all(stream: &IStream) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut got = 0u32;
        // SAFETY: `chunk` holds `chunk.len()` writable bytes; `got` is a local.
        unsafe {
            stream.Read(
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                Some(&mut got),
            )
        }
        .ok()?;
        if got == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&chunk[..got as usize]);
        if data.len() > READ_CAP {
            return Err(E_FAIL.into());
        }
    }
}

/// A `cx` x `cx` top-down 32 bpp DIB filled with lime.
fn lime(cx: u32) -> Result<HBITMAP> {
    let side = cx.clamp(1, 4096) as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid BITMAPINFO; `bits` receives the pixel pointer.
    let hbm = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
    let count = (side as usize) * (side as usize);
    // SAFETY: the DIB section holds `count` 32-bit pixels at `bits`.
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u32>(), count) };
    pixels.fill(0xFF00_FF00); // BGRA little-endian: B=0, G=255, R=0, A=255
    Ok(hbm)
}
