//! The details (property) handler of all-users installs: `IInitializeWithStream` +
//! `IPropertyStore` (read-only) + `IPropertyStoreCapabilities`.
//!
//! Windows calls it inside `explorer.exe` and inside the search indexer's
//! `SearchProtocolHost.exe`, which runs as SYSTEM. So it only parses bytes in Rust: it reads
//! the stream it is given (up to the shell's 64 MiB cap) and the HEIF boxes and EXIF in them.
//! It never starts a process or a thread, never opens a path, and has no C code; the test
//! `the_details_code_has_no_way_to_a_worker` holds this module (and `stream`) to that.
//!
//! Exported: `System.Image.HorizontalSize`, `VerticalSize` and `Dimensions` (the displayed
//! size, after `clap` and `irot`), `System.Photo.DateTaken` (EXIF `DateTimeOriginal`, in
//! UTC through `OffsetTimeOriginal` when present, else as the machine's local time, as
//! Windows' own photo handlers do), `CameraManufacturer` and `CameraModel`. Never GPS.
//! A file that does not parse has no properties; nothing here panics into the caller.

mod read;

use std::sync::{Mutex, MutexGuard};

use windows::core::{implement, Result, PROPVARIANT};
use windows::Win32::Foundation::{
    ERROR_ALREADY_INITIALIZED, E_INVALIDARG, E_POINTER, FILETIME, STG_E_ACCESSDENIED, SYSTEMTIME,
    S_FALSE,
};
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_Image_Dimensions, PKEY_Image_HorizontalSize, PKEY_Image_VerticalSize,
    PKEY_Photo_CameraManufacturer, PKEY_Photo_CameraModel, PKEY_Photo_DateTaken,
};
use windows::Win32::System::Com::StructuredStorage::{
    InitPropVariantFromFileTime, PropVariantChangeType, PROPVAR_CHANGE_FLAGS,
};
use windows::Win32::System::Com::{IStream, STGM_READWRITE, STGM_WRITE, STREAM_SEEK_SET};
use windows::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl, IPropertyStore, IPropertyStoreCapabilities,
    IPropertyStoreCapabilities_Impl, IPropertyStore_Impl, PROPERTYKEY,
};

use crate::stream::{read_capped, READ_CAP};
use crate::{guarded, ObjectCount};
use read::{heif_properties, DateTaken, HeifProps};

/// One exported value, kept as plain data until a caller asks for it.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    /// `VT_UI4`.
    Count(u32),
    /// `VT_LPWSTR`.
    Text(String),
    /// `VT_FILETIME`, UTC ticks.
    Time(u64),
}

#[implement(IPropertyStore, IPropertyStoreCapabilities, IInitializeWithStream)]
pub(crate) struct Store {
    /// `None` until `Initialize` has read the file; then its properties (maybe none).
    values: Mutex<Option<Vec<(PROPERTYKEY, Value)>>>,
    _count: ObjectCount,
}

impl Store {
    pub(crate) fn new() -> Store {
        Store {
            values: Mutex::new(None),
            _count: ObjectCount::new(),
        }
    }

    fn values(&self) -> MutexGuard<'_, Option<Vec<(PROPERTYKEY, Value)>>> {
        self.values.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A new details handler, as the class factory hands it out.
pub(crate) fn create() -> IPropertyStore {
    Store::new().into()
}

/// The properties of `props`, in a fixed order; those without a value are left out.
fn values_of(props: &HeifProps) -> Vec<(PROPERTYKEY, Value)> {
    let (width, height) = (props.width, props.height);
    let mut out = vec![
        (PKEY_Image_HorizontalSize, Value::Count(width)),
        (PKEY_Image_VerticalSize, Value::Count(height)),
        (
            PKEY_Image_Dimensions,
            Value::Text(format!("{width} x {height}")),
        ),
    ];
    if let Some(ticks) = props.date_taken.and_then(utc_ticks) {
        out.push((PKEY_Photo_DateTaken, Value::Time(ticks)));
    }
    if let Some(make) = &props.make {
        out.push((PKEY_Photo_CameraManufacturer, Value::Text(make.clone())));
    }
    if let Some(model) = &props.model {
        out.push((PKEY_Photo_CameraModel, Value::Text(model.clone())));
    }
    out
}

/// The UTC ticks of `date`; a local time goes through this machine's time zone rules (for
/// that date). `None` when Windows cannot convert it.
fn utc_ticks(date: DateTaken) -> Option<u64> {
    let local = match date {
        DateTaken::Utc(ticks) => return Some(ticks),
        DateTaken::Local(ticks) => filetime(ticks),
    };
    let mut wall = SYSTEMTIME::default();
    let mut utc = SYSTEMTIME::default();
    let mut out = FILETIME::default();
    // SAFETY: local in- and out-structs; a null time zone means the current one.
    unsafe {
        FileTimeToSystemTime(&local, &mut wall).ok()?;
        TzSpecificLocalTimeToSystemTime(None, &wall, &mut utc).ok()?;
        SystemTimeToFileTime(&utc, &mut out).ok()?;
    }
    Some(u64::from(out.dwHighDateTime) << 32 | u64::from(out.dwLowDateTime))
}

fn filetime(ticks: u64) -> FILETIME {
    FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    }
}

/// `value` as the property system's canonical type for it.
fn to_propvariant(value: &Value) -> Result<PROPVARIANT> {
    match value {
        Value::Count(n) => Ok(PROPVARIANT::from(*n)),
        Value::Text(text) => {
            // From &str makes a VT_BSTR; the string properties are VT_LPWSTR.
            let bstr = PROPVARIANT::from(text.as_str());
            let mut out = PROPVARIANT::new();
            // SAFETY: a valid source and an empty destination, both ours.
            unsafe { PropVariantChangeType(&mut out, &bstr, PROPVAR_CHANGE_FLAGS(0), VT_LPWSTR)? };
            Ok(out)
        }
        // SAFETY: a local FILETIME.
        Value::Time(ticks) => unsafe { InitPropVariantFromFileTime(&filetime(*ticks)) },
    }
}

#[allow(non_snake_case)]
impl IInitializeWithStream_Impl for Store_Impl {
    fn Initialize(&self, stream: Option<&IStream>, mode: u32) -> Result<()> {
        guarded(|| {
            let stream = stream.ok_or(E_INVALIDARG)?;
            // Read-only: a caller that wants to write is told so up front.
            if mode & (STGM_WRITE.0 | STGM_READWRITE.0) != 0 {
                return Err(STG_E_ACCESSDENIED.into());
            }
            let already = || Err(ERROR_ALREADY_INITIALIZED.to_hresult().into());
            if self.values().is_some() {
                return already();
            }
            // No lock is held while the stream is read: a stream may call back into us.
            // From the start, whatever the caller did with the stream. A stream that cannot
            // seek is read from where it is.
            // SAFETY: a valid stream; no out-pointer.
            let _ = unsafe { stream.Seek(0, STREAM_SEEK_SET, None) };
            let data = read_capped(stream, READ_CAP)?;
            let found = heif_properties(&data).map_or_else(Vec::new, |p| values_of(&p));
            drop(data);
            let mut values = self.values();
            if values.is_some() {
                return already();
            }
            *values = Some(found);
            Ok(())
        })
    }
}

#[allow(non_snake_case)]
impl IPropertyStore_Impl for Store_Impl {
    fn GetCount(&self) -> Result<u32> {
        guarded(|| Ok(self.values().as_ref().map_or(0, |v| v.len() as u32)))
    }

    fn GetAt(&self, index: u32, key: *mut PROPERTYKEY) -> Result<()> {
        guarded(|| {
            if key.is_null() {
                return Err(E_POINTER.into());
            }
            let values = self.values();
            let found = values
                .as_ref()
                .and_then(|v| v.get(index as usize))
                .map(|(k, _)| *k);
            // SAFETY: checked non-null; COM contract: writable.
            unsafe { *key = found.unwrap_or_default() };
            found.map(|_| ()).ok_or_else(|| E_INVALIDARG.into())
        })
    }

    fn GetValue(&self, key: *const PROPERTYKEY) -> Result<PROPVARIANT> {
        guarded(|| {
            if key.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: checked non-null; COM contract: readable.
            let key = unsafe { *key };
            let value = self
                .values()
                .as_ref()
                .and_then(|v| v.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()));
            // A property this file does not have is VT_EMPTY, and still S_OK.
            value.map_or_else(|| Ok(PROPVARIANT::new()), |v| to_propvariant(&v))
        })
    }

    fn SetValue(&self, _key: *const PROPERTYKEY, _value: *const PROPVARIANT) -> Result<()> {
        guarded(|| Err(STG_E_ACCESSDENIED.into()))
    }

    fn Commit(&self) -> Result<()> {
        guarded(|| Err(STG_E_ACCESSDENIED.into()))
    }
}

#[allow(non_snake_case)]
impl IPropertyStoreCapabilities_Impl for Store_Impl {
    fn IsPropertyWritable(&self, _key: *const PROPERTYKEY) -> Result<()> {
        // S_FALSE: no property is writable.
        guarded(|| Err(S_FALSE.into()))
    }
}

#[cfg(test)]
mod tests;
