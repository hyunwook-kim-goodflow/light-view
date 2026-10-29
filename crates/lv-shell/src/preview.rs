//! SPIKE (M7 Task 1b): `IPreviewHandler` + `IInitializeWithStream` + `IObjectWithSite` +
//! `IOleWindow` that shows a magenta child window. `DoPreview` logs the host process, reads
//! the stream and runs one sandboxed worker decode (the same code as the thumbnail spike).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::{implement, w, IUnknown, Interface, Result, GUID};
use windows::Win32::Foundation::{
    BOOL, CLASS_E_NOAGGREGATION, COLORREF, E_FAIL, E_NOINTERFACE, E_NOTIMPL, E_UNEXPECTED,
    HINSTANCE, HWND, LPARAM, LRESULT, RECT, S_FALSE, WPARAM,
};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::System::Com::IStream;
use windows::Win32::System::Ole::{
    IObjectWithSite, IObjectWithSite_Impl, IOleWindow, IOleWindow_Impl,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandler_Impl};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos, HMENU, MSG,
    SWP_NOACTIVATE, SWP_NOZORDER, WINDOW_EX_STYLE, WNDCLASSW, WS_CHILD, WS_VISIBLE,
};

use crate::{host, provider, spike_log, worker, ObjectCount};

/// Fill colour of the preview window: magenta (COLORREF is 0x00BBGGRR).
const MAGENTA: COLORREF = COLORREF(0x00FF_00FF);

pub fn create(outer: Option<&IUnknown>, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
    if outer.is_some() {
        return Err(CLASS_E_NOAGGREGATION.into());
    }
    spike_log(&format!(
        "preview CreateInstance host: {}",
        host::describe_self()
    ));
    let handler: IPreviewHandler = Handler {
        stream: RefCell::new(None),
        site: RefCell::new(None),
        parent: Cell::new(HWND::default()),
        rect: Cell::new(RECT::default()),
        window: Cell::new(HWND::default()),
        _count: ObjectCount::new(),
    }
    .into();
    // SAFETY: COM contract from the caller: valid iid and out pointers.
    unsafe { handler.query(iid, out).ok() }
}

#[implement(IPreviewHandler, IInitializeWithStream, IObjectWithSite, IOleWindow)]
struct Handler {
    stream: RefCell<Option<IStream>>,
    site: RefCell<Option<IUnknown>>,
    parent: Cell<HWND>,
    rect: Cell<RECT>,
    window: Cell<HWND>,
    _count: ObjectCount,
}

#[allow(non_snake_case)]
impl IInitializeWithStream_Impl for Handler_Impl {
    fn Initialize(&self, stream: Option<&IStream>, mode: u32) -> Result<()> {
        spike_log(&format!("preview Initialize(mode={mode:#x})"));
        let mut slot = self.stream.borrow_mut();
        if slot.is_some() {
            return Err(E_UNEXPECTED.into());
        }
        *slot = stream.cloned();
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IObjectWithSite_Impl for Handler_Impl {
    fn SetSite(&self, site: Option<&IUnknown>) -> Result<()> {
        spike_log(&format!("preview SetSite(present={})", site.is_some()));
        *self.site.borrow_mut() = site.cloned();
        Ok(())
    }

    fn GetSite(&self, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        match self.site.borrow().as_ref() {
            // SAFETY: COM contract from the caller: valid iid and out pointers.
            Some(site) => unsafe { site.query(iid, out).ok() },
            None => {
                // SAFETY: COM contract: writable out pointer.
                unsafe { *out = std::ptr::null_mut() };
                Err(E_NOINTERFACE.into())
            }
        }
    }
}

#[allow(non_snake_case)]
impl IOleWindow_Impl for Handler_Impl {
    fn GetWindow(&self) -> Result<HWND> {
        Ok(self.parent.get())
    }

    fn ContextSensitiveHelp(&self, _enter: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

#[allow(non_snake_case)]
impl IPreviewHandler_Impl for Handler_Impl {
    fn SetWindow(&self, hwnd: HWND, rect: *const RECT) -> Result<()> {
        spike_log(&format!("preview SetWindow(hwnd={:?})", hwnd.0));
        self.parent.set(hwnd);
        if !rect.is_null() {
            // SAFETY: COM contract: a valid RECT when non-null.
            self.rect.set(unsafe { *rect });
        }
        Ok(())
    }

    fn SetRect(&self, rect: *const RECT) -> Result<()> {
        if rect.is_null() {
            return Err(E_UNEXPECTED.into());
        }
        // SAFETY: checked non-null; COM contract: a valid RECT.
        let r = unsafe { *rect };
        self.rect.set(r);
        let window = self.window.get();
        if !window.0.is_null() {
            // SAFETY: our own child window.
            unsafe {
                SetWindowPos(
                    window,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )?
            };
        }
        Ok(())
    }

    fn DoPreview(&self) -> Result<()> {
        let result = catch_unwind(AssertUnwindSafe(|| self.preview()));
        result.unwrap_or_else(|_| {
            spike_log("PANIC in DoPreview");
            Err(E_FAIL.into())
        })
    }

    fn Unload(&self) -> Result<()> {
        spike_log("preview Unload");
        let window = self.window.replace(HWND::default());
        if !window.0.is_null() {
            // SAFETY: our own child window, created on this thread.
            let _ = unsafe { DestroyWindow(window) };
        }
        *self.stream.borrow_mut() = None;
        Ok(())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn QueryFocus(&self) -> Result<HWND> {
        Err(E_NOTIMPL.into())
    }

    fn TranslateAccelerator(&self, _msg: *const MSG) -> Result<()> {
        Err(windows::core::Error::from(S_FALSE))
    }
}

impl Handler_Impl {
    fn preview(&self) -> Result<()> {
        let t = Instant::now();
        spike_log(&format!("DoPreview host: {}", host::describe_self()));
        let parent = self.parent.get();
        let r = self.rect.get();
        register_class()?;
        // SAFETY: registered class, parent from SetWindow; the child is ours to destroy.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                CLASS_NAME,
                w!("light-view preview spike"),
                WS_CHILD | WS_VISIBLE,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                parent,
                HMENU::default(),
                module(),
                None,
            )
        };
        match window {
            Ok(w) => {
                self.window.set(w);
                spike_log("preview child window created");
            }
            Err(e) => spike_log(&format!("preview CreateWindowExW failed: {e}")),
        }
        let stream = self.stream.borrow().clone().ok_or(E_UNEXPECTED)?;
        let data = provider::read_all(&stream)?;
        spike_log(&format!(
            "preview read {} bytes in {:?}",
            data.len(),
            t.elapsed()
        ));
        let summary = worker::decode_once(data);
        spike_log(&format!(
            "DoPreview done in {:?} (worker: {summary})",
            t.elapsed()
        ));
        Ok(())
    }
}

const CLASS_NAME: windows::core::PCWSTR = w!("lvPreviewSpike");

fn module() -> HINSTANCE {
    HINSTANCE(crate::module_handle().0)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwarding the window procedure's own arguments.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Registers the magenta window class once per process.
fn register_class() -> Result<()> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| {
        // SAFETY: a solid brush owned by the class for the life of the process.
        let brush = unsafe { CreateSolidBrush(MAGENTA) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module(),
            hbrBackground: brush,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: a fully initialised WNDCLASSW whose strings are static.
        unsafe { RegisterClassW(&class) != 0 }
    });
    if ok {
        Ok(())
    } else {
        Err(E_FAIL.into())
    }
}
