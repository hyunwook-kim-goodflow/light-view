//! The preview's child window and the state it shares with the render thread.
//!
//! A result reaches the window only through [`Shared::deliver`], which posts to the window
//! while holding the mailbox lock, and only if the window is still open and the result
//! belongs to the current preview (its generation). The mailbox forgets the window under
//! that lock before the window is destroyed ([`Shared::close`], for Unload) or as anyone else
//! destroys it (its parent: WM_NCDESTROY), so a late result is dropped (its pixels freed) and
//! never posted to a destroyed window or to one that reused its handle. The mailbox's window
//! is also the only window the handler uses ([`Shared::window`]): a dead or foreign handle is
//! never touched.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, LoadCursorW,
    PostMessageW, RegisterClassExW, SetWindowLongPtrW, SetWindowPos, UnregisterClassW, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, SWP_NOACTIVATE, SWP_NOZORDER, WM_APP, WM_ERASEBKGND,
    WM_NCDESTROY, WM_PAINT, WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS, WS_EX_NOPARENTNOTIFY,
    WS_VISIBLE,
};

use super::draw::{Image, View};
use crate::{this_module, ObjectCount};

/// Posted to the window when the result of a preview (its generation in `wParam`) is ready.
const WM_RESULT: u32 = WM_APP + 1;
/// The window class, registered by this module. A class registered by a DLL outlives the
/// DLL, so it is unregistered again once no window of it is left ([`unregister_unused`]).
const CLASS: PCWSTR = w!("light-view HEIC preview");
/// Windows of [`CLASS`] not yet destroyed: a window counts from just before it is made until
/// its WM_NCDESTROY. The class is registered when this leaves 0 and only unregistered while
/// it is 0. Locked only around the count and the class calls, never across CreateWindowExW
/// or DestroyWindow (which may wait on other threads, or reenter on this one).
static WINDOWS: Mutex<usize> = Mutex::new(0);

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct Mailbox {
    /// The window results go to (a raw `HWND`); `None` once closed or destroyed.
    window: Option<isize>,
    /// The current preview; results of any other are stale.
    generation: u64,
    /// The current preview's result, posted but not yet taken by the window.
    ready: Option<Image>,
}

/// What a preview handler, its window and the render thread share. Counted as an object of
/// the DLL while any of them holds it.
pub(crate) struct Shared {
    mailbox: Mutex<Mailbox>,
    /// The window's content: used by the window procedure and by the handler, both on the
    /// host's thread that made the handler.
    view: Mutex<View>,
    _count: ObjectCount,
}

impl Shared {
    pub(crate) fn new() -> Arc<Shared> {
        Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            view: Mutex::new(View::new()),
            _count: ObjectCount::new(),
        })
    }

    pub(crate) fn view(&self) -> MutexGuard<'_, View> {
        locked(&self.view)
    }

    /// Results go to `window` from now on.
    pub(crate) fn open(&self, window: HWND) {
        locked(&self.mailbox).window = Some(window.0 as isize);
    }

    /// Our window, while it is open and not destroyed.
    pub(crate) fn window(&self) -> Option<HWND> {
        locked(&self.mailbox).window.map(|w| HWND(w as *mut _))
    }

    /// Starts a new preview and returns its generation: results of earlier ones are stale.
    pub(crate) fn begin(&self) -> u64 {
        let mut mailbox = locked(&self.mailbox);
        mailbox.generation += 1;
        mailbox.ready = None;
        mailbox.generation
    }

    /// Whether preview `generation` is the current one and has a window to go to.
    pub(crate) fn is_current(&self, generation: u64) -> bool {
        let mailbox = locked(&self.mailbox);
        mailbox.window.is_some() && mailbox.generation == generation
    }

    /// Hands the result of preview `generation` to the window, if that is still the current
    /// preview and the window is open; else drops it. Whether it was posted.
    pub(crate) fn deliver(&self, generation: u64, image: Image) -> bool {
        let mut mailbox = locked(&self.mailbox);
        let Some(window) = mailbox.window.filter(|_| generation == mailbox.generation) else {
            return false;
        };
        mailbox.ready = Some(image);
        // SAFETY: the window exists: the mailbox forgets it under this lock before or as it
        // is destroyed (`close`, WM_NCDESTROY). Posting needs no particular thread.
        let posted = unsafe {
            PostMessageW(
                HWND(window as *mut _),
                WM_RESULT,
                WPARAM(generation as usize),
                LPARAM(0),
            )
        }
        .is_ok();
        if !posted {
            mailbox.ready = None;
        }
        posted
    }

    /// The result of preview `generation`, if it is the current one and has one.
    fn take(&self, generation: u64) -> Option<Image> {
        let mut mailbox = locked(&self.mailbox);
        if generation != mailbox.generation {
            return None;
        }
        mailbox.ready.take()
    }

    /// No result goes anywhere any more (until [`Shared::open`]); the current preview is
    /// over. Returns the window, for the caller to destroy.
    pub(crate) fn close(&self) -> Option<HWND> {
        let mut mailbox = locked(&self.mailbox);
        mailbox.generation += 1;
        mailbox.ready = None;
        mailbox.window.take().map(|w| HWND(w as *mut _))
    }

    /// `window` is being destroyed, by anyone: if it is ours, forget it and end the current
    /// preview, as [`Shared::close`] does.
    fn gone(&self, window: HWND) {
        let mut mailbox = locked(&self.mailbox);
        if mailbox.window == Some(window.0 as isize) {
            mailbox.window = None;
            mailbox.generation += 1;
            mailbox.ready = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn has_ready(&self) -> bool {
        locked(&self.mailbox).ready.is_some()
    }
}

/// A visible child window of `parent` at `rect` that shows `shared`'s view. The window holds
/// a reference to `shared` until it is destroyed. The caller opens the mailbox to it.
pub(crate) fn create(parent: HWND, rect: RECT, shared: &Arc<Shared>) -> Result<HWND> {
    let instance = HINSTANCE(this_module()?.0);
    {
        let mut windows = locked(&WINDOWS);
        if *windows == 0 {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                // SAFETY: a system cursor; nothing to free.
                hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            // SAFETY: a valid class description whose strings are static.
            if unsafe { RegisterClassExW(&class) } == 0 {
                // SAFETY: plain call.
                let error = unsafe { GetLastError() };
                if error != ERROR_CLASS_ALREADY_EXISTS {
                    return Err(error.into());
                }
            }
        }
        // Counted before it is made, so no other thread unregisters the class meanwhile.
        *windows += 1;
    }
    // SAFETY: our registered class; no creation data.
    let made = unsafe {
        CreateWindowExW(
            WS_EX_NOPARENTNOTIFY,
            CLASS,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            rect.left,
            rect.top,
            rect.right.saturating_sub(rect.left).max(0),
            rect.bottom.saturating_sub(rect.top).max(0),
            parent,
            None,
            instance,
            None,
        )
    };
    let window = match made {
        Ok(window) => window,
        Err(e) => {
            // No WM_NCDESTROY of ours counts it out: its user data was never set.
            *locked(&WINDOWS) -= 1;
            unregister_unused();
            return Err(e);
        }
    };
    let raw = Arc::into_raw(Arc::clone(shared));
    // SAFETY: our window; `window_proc` takes the reference back at WM_NCDESTROY. Until
    // this call its user data is 0 and it does not touch `shared` (nothing is painted or
    // posted to it before it is returned).
    unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, raw as isize) };
    Ok(window)
}

/// Moves `window` to `rect` in its parent (it repaints: CS_HREDRAW | CS_VREDRAW).
pub(crate) fn place(window: HWND, rect: RECT) {
    // SAFETY: our live window (`Shared::window`).
    let _ = unsafe {
        SetWindowPos(
            window,
            None,
            rect.left,
            rect.top,
            rect.right.saturating_sub(rect.left).max(0),
            rect.bottom.saturating_sub(rect.top).max(0),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// Repaints `window` (later, as WM_PAINT).
pub(crate) fn invalidate(window: HWND) {
    // SAFETY: our live window (`Shared::window`).
    let _ = unsafe { InvalidateRect(window, None, false) };
}

/// Destroys `window` (made by [`create`], on this thread, and already forgotten by the
/// mailbox: [`Shared::close`]) and unregisters the class if it was the last window of it.
pub(crate) fn destroy(window: HWND) {
    // SAFETY: our window, alive (the mailbox forgets a window as it is destroyed). On
    // another thread than its own this fails, and the window (with its reference to the
    // shared state, which keeps the DLL loaded) stays.
    let _ = unsafe { DestroyWindow(window) };
    unregister_unused();
}

/// Unregisters the class if no window of it is left. `DllCanUnloadNow` calls it too, for
/// windows their parent destroyed: a window's own WM_NCDESTROY cannot (it still exists).
pub(crate) fn unregister_unused() {
    let windows = locked(&WINDOWS);
    if *windows == 0 {
        if let Ok(module) = this_module() {
            // SAFETY: plain call; it fails, harmlessly, if the class is not registered.
            let _ = unsafe { UnregisterClassW(CLASS, HINSTANCE(module.0)) };
        }
    }
}

/// Windows of the class not yet destroyed (tests).
#[cfg(test)]
pub(crate) fn live_windows() -> usize {
    *locked(&WINDOWS)
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // No panic may unwind into the system's message dispatch.
    catch_unwind(AssertUnwindSafe(|| handle(window, message, wparam, lparam))).unwrap_or(LRESULT(0))
}

fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: our window; its user data is 0 or a reference made by `create`.
    let data = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) };
    if data == 0 {
        // SAFETY: the default handling of a message for our window.
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }
    // SAFETY: the reference `create` made; released only at WM_NCDESTROY, below.
    let shared = unsafe { &*(data as *const Shared) };
    match message {
        WM_RESULT => {
            if let Some(image) = shared.take(wparam.0 as u64) {
                // A bitmap that cannot be made shows nothing, as a failed preview does.
                let _ = shared.view().show(image);
                invalidate(window);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let mut client = RECT::default();
            // SAFETY: our window, painted in its WM_PAINT; EndPaint pairs with BeginPaint.
            unsafe {
                let dc = BeginPaint(window, &mut paint);
                let _ = GetClientRect(window, &mut client);
                shared.view().paint(dc, client);
                let _ = EndPaint(window, &paint);
            }
            LRESULT(0)
        }
        // WM_PAINT paints every pixel.
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => {
            // Whoever destroys it (Unload, or its parent): no result goes to it any more.
            shared.gone(window);
            *locked(&WINDOWS) -= 1;
            // SAFETY: our window; the user data is cleared before the reference goes, so
            // nothing can use it afterwards.
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                drop(Arc::from_raw(data as *const Shared));
                DefWindowProcW(window, message, wparam, lparam)
            }
        }
        // SAFETY: the default handling of a message for our window.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
