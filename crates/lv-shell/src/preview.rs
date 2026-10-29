//! The preview handler (all-users installs; it runs in `prevhost.exe`): `IPreviewHandler`,
//! `IInitializeWithStream`, `IObjectWithSite`, `IOleWindow` and `IPreviewHandlerVisuals`.
//!
//! `DoPreview` reads the stream (up to [`READ_CAP`]) on the calling thread, makes a child
//! window of the preview pane's window, and has one sandboxed worker make the image on a
//! background thread (its long side at most the pane's long side, at most [`MAX_SIDE`]). The
//! result is posted to the window, which draws it fitted (aspect kept, never enlarged,
//! centered) over the pane's background. Each `DoPreview` starts a new generation, so a
//! result of an earlier one is dropped; `Unload` destroys the window and the bitmap, and a
//! result arriving after it is dropped too (`window::Shared`). A failed preview shows the
//! background only.
//!
//! Every method runs on the host's thread that made the handler (an STA with a message loop).

mod draw;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use windows::core::{implement, IUnknown, Interface, Result, GUID};
use windows::Win32::Foundation::{
    BOOL, COLORREF, ERROR_ALREADY_INITIALIZED, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER,
    E_UNEXPECTED, HWND, RECT, S_FALSE, S_OK,
};
use windows::Win32::Graphics::Gdi::LOGFONTW;
use windows::Win32::System::Com::{IStream, STREAM_SEEK_SET};
use windows::Win32::System::Ole::{
    IObjectWithSite, IObjectWithSite_Impl, IOleWindow, IOleWindow_Impl,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    IPreviewHandler, IPreviewHandlerFrame, IPreviewHandlerVisuals, IPreviewHandlerVisuals_Impl,
    IPreviewHandler_Impl,
};
use windows::Win32::UI::WindowsAndMessaging::{SetParent, MSG};

use crate::thumbnail::{ask_worker, READ_CAP};
use crate::{guarded, stream, ObjectCount};
use draw::Image;
use window::Shared;

/// The largest long side asked of the worker for a preview (the plan's 2048; well under
/// lv-ipc's 4096).
pub(crate) const MAX_SIDE: u32 = 2048;

/// Makes the image of a HEIC file (its bytes) with the given largest long side, or `None`.
/// The worker in production; tests put their own.
pub(crate) type Render = Arc<dyn Fn(Vec<u8>, u32) -> Option<Image> + Send + Sync>;

/// The long side asked of the worker for a preview area of `rect`: the area's long side,
/// at least 1 and at most [`MAX_SIDE`].
pub(crate) fn preview_side(rect: RECT) -> u32 {
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    width.max(height).clamp(1, i64::from(MAX_SIDE)) as u32
}

#[derive(Default)]
struct State {
    stream: Option<IStream>,
    /// The preview pane's window (SetWindow) and our area in it.
    parent: Option<HWND>,
    rect: RECT,
    /// Our child window, from the first DoPreview until Unload.
    child: Option<HWND>,
    site: Option<IUnknown>,
    frame: Option<IPreviewHandlerFrame>,
}

#[implement(
    IPreviewHandler,
    IPreviewHandlerVisuals,
    IObjectWithSite,
    IOleWindow,
    IInitializeWithStream
)]
pub(crate) struct Handler {
    render: Render,
    shared: Arc<Shared>,
    /// Used on the host's thread only. Never borrowed across a call that may send our
    /// window a message.
    state: RefCell<State>,
    _count: ObjectCount,
}

impl Handler {
    /// A handler whose previews `exe` (`lv-worker.exe`) makes.
    pub(crate) fn new(exe: PathBuf) -> Handler {
        Handler::with_render(Arc::new(move |data, max_side| {
            let (width, height, bgra) = ask_worker(&exe, max_side, data)?;
            Some(Image {
                width,
                height,
                bgra,
            })
        }))
    }

    pub(crate) fn with_render(render: Render) -> Handler {
        Handler {
            render,
            shared: Shared::new(),
            state: RefCell::new(State::default()),
            _count: ObjectCount::new(),
        }
    }

    /// Ends the current preview: no result goes to the window any more, the window and the
    /// bitmap are destroyed and the stream released. The handler can be initialized again.
    fn unload(&self) {
        self.shared.close();
        let child = {
            let mut state = self.state.borrow_mut();
            state.stream = None;
            state.child.take()
        };
        if let Some(child) = child {
            window::destroy(child);
        }
        self.shared.view().clear();
    }

    fn do_preview(&self) -> Result<()> {
        let (stream, parent, rect, child) = {
            let state = self.state.borrow();
            let stream = state.stream.clone().ok_or(E_UNEXPECTED)?;
            let parent = state.parent.ok_or(E_UNEXPECTED)?;
            (stream, parent, state.rect, state.child)
        };
        // From the start again (a second DoPreview reads it again); a stream that cannot
        // seek is read from where it is.
        // SAFETY: a valid stream.
        let _ = unsafe { stream.Seek(0, STREAM_SEEK_SET, None) };
        // Read here: the stream belongs to this thread's apartment.
        let data = stream::read_capped(&stream, READ_CAP)?;
        if data.is_empty() {
            return Err(E_FAIL.into());
        }
        let child = match child {
            Some(child) => child,
            None => {
                let child = window::create(parent, rect, &self.shared)?;
                self.shared.open(child);
                self.state.borrow_mut().child = Some(child);
                child
            }
        };
        let generation = self.shared.begin();
        self.shared.view().clear();
        window::invalidate(child);
        let (render, shared, side) = (
            Arc::clone(&self.render),
            Arc::clone(&self.shared),
            preview_side(rect),
        );
        // Counted by lv-sandbox, so the DLL stays loaded while it runs.
        lv_sandbox::spawn_counted("light-view preview".into(), move || {
            if let Some(image) = render(data, side) {
                shared.deliver(generation, image);
            }
        })
        .map_err(|_| E_FAIL)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn child(&self) -> Option<HWND> {
        self.state.borrow().child
    }

    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        // A host that releases the handler without Unload: the window must not outlive it.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.unload()));
    }
}

#[allow(non_snake_case)]
impl IPreviewHandler_Impl for Handler_Impl {
    fn SetWindow(&self, hwnd: HWND, prc: *const RECT) -> Result<()> {
        guarded(|| {
            if prc.is_null() || hwnd.is_invalid() {
                return Err(E_INVALIDARG.into());
            }
            // SAFETY: checked non-null; COM contract: points to a RECT.
            let rect = unsafe { *prc };
            let child = {
                let mut state = self.state.borrow_mut();
                state.parent = Some(hwnd);
                state.rect = rect;
                state.child
            };
            if let Some(child) = child {
                // SAFETY: our window and the host's; a failure leaves it where it was.
                let _ = unsafe { SetParent(child, hwnd) };
                window::place(child, rect);
            }
            Ok(())
        })
    }

    fn SetRect(&self, prc: *const RECT) -> Result<()> {
        guarded(|| {
            if prc.is_null() {
                return Err(E_INVALIDARG.into());
            }
            // SAFETY: checked non-null; COM contract: points to a RECT.
            let rect = unsafe { *prc };
            let child = {
                let mut state = self.state.borrow_mut();
                state.rect = rect;
                state.child
            };
            if let Some(child) = child {
                window::place(child, rect);
            }
            Ok(())
        })
    }

    fn DoPreview(&self) -> Result<()> {
        guarded(|| self.do_preview())
    }

    fn Unload(&self) -> Result<()> {
        guarded(|| {
            self.unload();
            Ok(())
        })
    }

    fn SetFocus(&self) -> Result<()> {
        guarded(|| {
            let child = self.state.borrow().child;
            let child = child.ok_or(S_FALSE)?;
            // SAFETY: our window, on its thread.
            let _ = unsafe { SetFocus(child) };
            Ok(())
        })
    }

    fn QueryFocus(&self) -> Result<HWND> {
        guarded(|| {
            // SAFETY: plain call.
            let focus = unsafe { GetFocus() };
            if focus.is_invalid() {
                return Err(E_FAIL.into());
            }
            Ok(focus)
        })
    }

    fn TranslateAccelerator(&self, pmsg: *const MSG) -> Result<()> {
        guarded(|| {
            // We use no keys: the host's frame may (S_FALSE: nobody did).
            let frame = self.state.borrow().frame.clone();
            let Some(frame) = frame else {
                return Err(S_FALSE.into());
            };
            // Called through the vtable to pass the frame's S_FALSE on as it is.
            // SAFETY: a valid frame; `pmsg` is the host's, passed on unchanged.
            let hr =
                unsafe { (Interface::vtable(&frame).TranslateAccelerator)(frame.as_raw(), pmsg) };
            if hr == S_OK {
                Ok(())
            } else {
                Err(hr.into())
            }
        })
    }
}

#[allow(non_snake_case)]
impl IPreviewHandlerVisuals_Impl for Handler_Impl {
    fn SetBackgroundColor(&self, color: COLORREF) -> Result<()> {
        guarded(|| {
            self.shared.view().set_background(color);
            let child = self.state.borrow().child;
            if let Some(child) = child {
                window::invalidate(child);
            }
            Ok(())
        })
    }

    /// No text is drawn.
    fn SetFont(&self, _font: *const LOGFONTW) -> Result<()> {
        Ok(())
    }

    /// No text is drawn.
    fn SetTextColor(&self, _color: COLORREF) -> Result<()> {
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IObjectWithSite_Impl for Handler_Impl {
    fn SetSite(&self, site: Option<&IUnknown>) -> Result<()> {
        guarded(|| {
            let frame = site.and_then(|s| s.cast::<IPreviewHandlerFrame>().ok());
            let mut state = self.state.borrow_mut();
            state.site = site.cloned();
            state.frame = frame;
            Ok(())
        })
    }

    fn GetSite(&self, riid: *const GUID, site: *mut *mut core::ffi::c_void) -> Result<()> {
        guarded(|| {
            if riid.is_null() || site.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: checked non-null; COM contract: writable.
            unsafe { *site = std::ptr::null_mut() };
            let ours = self.state.borrow().site.clone().ok_or(E_FAIL)?;
            // SAFETY: COM contract: `riid` points to a GUID and `site` is writable.
            unsafe { ours.query(riid, site).ok() }
        })
    }
}

#[allow(non_snake_case)]
impl IOleWindow_Impl for Handler_Impl {
    /// The preview pane's window, as the preview handler samples answer.
    fn GetWindow(&self) -> Result<HWND> {
        guarded(|| Ok(self.state.borrow().parent.ok_or(E_FAIL)?))
    }

    fn ContextSensitiveHelp(&self, _enter: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

#[allow(non_snake_case)]
impl IInitializeWithStream_Impl for Handler_Impl {
    fn Initialize(&self, stream: Option<&IStream>, _mode: u32) -> Result<()> {
        guarded(|| {
            let stream = stream.ok_or(E_INVALIDARG)?;
            let mut state = self.state.borrow_mut();
            if state.stream.is_some() {
                return Err(ERROR_ALREADY_INITIALIZED.to_hresult().into());
            }
            state.stream = Some(stream.clone());
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
