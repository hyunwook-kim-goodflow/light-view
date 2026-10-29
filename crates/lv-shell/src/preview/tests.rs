//! The preview handler as prevhost uses it: a parent window of the test's own, a stream from
//! `SHCreateMemStream`, results posted to the child window and taken by pumping this
//! thread's messages. Tests that count objects or threads hold [`serial`].

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, ComObject, IUnknown, Interface, Result, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, HWND, RECT, S_FALSE, S_OK};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::Ole::{IObjectWithSite, IOleWindow};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandlerVisuals};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, IsWindow, PeekMessageW, TranslateMessage,
    MSG, PM_REMOVE, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
};

use super::draw::{live_bitmaps, Image};
use super::{preview_side, Handler, Render, MAX_SIDE};
use crate::dll::{DllCanUnloadNow, DllGetClassObject};
use crate::tests::{built_worker, mem_stream, sample, serial, wait_until_unloadable};
use crate::CLSID_PREVIEW;

/// Set in the low integrity child test process: the worker exe it uses.
const CHILD_ENV: &str = "LV_SHELL_LOW_HOST_WORKER";

/// A hidden top-level window of this thread, standing in for the preview pane's.
struct Parent(HWND);

impl Parent {
    fn new() -> Parent {
        // SAFETY: a system class; no creation data.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR::null(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                640,
                480,
                None,
                None,
                None,
                None,
            )
        }
        .expect("parent window");
        Parent(hwnd)
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        // SAFETY: our window, on this thread.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

const AREA: RECT = RECT {
    left: 0,
    top: 0,
    right: 300,
    bottom: 200,
};

/// Dispatches this thread's messages until `done` or `timeout`; whether `done`.
fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let mut msg = MSG::default();
        // SAFETY: plain message loop calls on this thread's queue.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn image(width: u32, height: u32) -> Image {
    Image {
        width,
        height,
        bgra: [0, 0, 255, 255].repeat((width * height) as usize),
    }
}

/// A handler with `render`, its window set to `parent` and initialized with `bytes`.
fn handler_with(render: Render, parent: &Parent, bytes: &[u8]) -> ComObject<Handler> {
    let handler = ComObject::new(Handler::with_render(render));
    let preview: IPreviewHandler = handler.to_interface();
    let init: IInitializeWithStream = handler.to_interface();
    // SAFETY: our window, a local rect and a valid stream.
    unsafe {
        preview.SetWindow(parent.0, &AREA).expect("SetWindow");
        init.Initialize(&mem_stream(bytes), 0).expect("Initialize");
    }
    handler
}

fn shown(handler: &ComObject<Handler>) -> Option<(u32, u32)> {
    handler.shared().view().shown_size()
}

fn do_preview(handler: &ComObject<Handler>) -> Result<()> {
    // SAFETY: plain call.
    unsafe { handler.to_interface::<IPreviewHandler>().DoPreview() }
}

fn wait_for_no_threads() {
    let deadline = Instant::now() + Duration::from_secs(10);
    while lv_sandbox::live_threads() != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(lv_sandbox::live_threads(), 0, "a background thread is left");
}

#[test]
fn preview_side_is_the_long_side_of_the_area_clamped() {
    assert_eq!(preview_side(AREA), 300);
    let tall = RECT {
        left: 10,
        top: 10,
        right: 110,
        bottom: 610,
    };
    assert_eq!(preview_side(tall), 600);
    let huge = RECT {
        left: i32::MIN,
        top: 0,
        right: i32::MAX,
        bottom: 1,
    };
    assert_eq!(preview_side(huge), MAX_SIDE);
    assert_eq!(MAX_SIDE, 2048);
    assert_eq!(preview_side(RECT::default()), 1);
    let inverted = RECT {
        left: 50,
        top: 50,
        right: 0,
        bottom: 0,
    };
    assert_eq!(preview_side(inverted), 1);
}

#[test]
fn do_preview_without_window_fails() {
    let _serial = serial();
    let handler = ComObject::new(Handler::with_render(std::sync::Arc::new(|_, _| None)));
    let preview: IPreviewHandler = handler.to_interface();
    let init: IInitializeWithStream = handler.to_interface();
    // Neither initialized nor given a window.
    // SAFETY: plain call.
    assert!(unsafe { preview.DoPreview() }.is_err());
    // SAFETY: a valid stream.
    unsafe { init.Initialize(&mem_stream(b"bytes"), 0) }.unwrap();
    // SAFETY: plain call.
    assert!(unsafe { preview.DoPreview() }.is_err(), "no window");
    assert_eq!(handler.child(), None, "no window made");
    // Null rects and windows are refused.
    let parent = Parent::new();
    // SAFETY: null pointers and handles are what is tested.
    unsafe {
        assert!(preview.SetWindow(parent.0, std::ptr::null()).is_err());
        assert!(preview.SetWindow(HWND::default(), &AREA).is_err());
        assert!(preview.SetRect(std::ptr::null()).is_err());
    }
    // An empty file fails without a window or a worker either.
    let empty = handler_with(std::sync::Arc::new(|_, _| None), &parent, b"");
    assert!(do_preview(&empty).is_err());
    assert_eq!(empty.child(), None);
}

/// Review Focus 4: when an earlier preview's result arrives after a later DoPreview, only
/// the later one is drawn.
#[test]
fn stale_preview_result_is_dropped() {
    let _serial = serial();
    let parent = Parent::new();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let calls = AtomicUsize::new(0);
    let render: Render = std::sync::Arc::new(move |_, _| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            // The first preview is slow: it answers only when the test says so.
            let _ = gate.lock().unwrap().recv();
            Some(image(10, 20))
        } else {
            Some(image(30, 40))
        }
    });
    let handler = handler_with(render, &parent, b"heic");
    do_preview(&handler).expect("first DoPreview");
    do_preview(&handler).expect("second DoPreview");
    assert!(
        pump_until(Duration::from_secs(10), || shown(&handler).is_some()),
        "the second result is drawn"
    );
    assert_eq!(shown(&handler), Some((30, 40)));

    // Now the first preview answers, late.
    release.send(()).unwrap();
    wait_for_no_threads();
    pump_until(Duration::from_millis(200), || false);
    assert_eq!(
        shown(&handler),
        Some((30, 40)),
        "the stale result was drawn"
    );
    assert!(!handler.shared().has_ready(), "the stale result was kept");
}

#[test]
fn result_after_unload_is_dropped() {
    let _serial = serial();
    let parent = Parent::new();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let render: Render = std::sync::Arc::new(move |_, _| {
        let _ = gate.lock().unwrap().recv();
        Some(image(10, 20))
    });
    let handler = handler_with(render, &parent, b"heic");
    do_preview(&handler).expect("DoPreview");
    let child = handler.child().expect("a child window");
    // SAFETY: plain call.
    unsafe { handler.to_interface::<IPreviewHandler>().Unload() }.unwrap();
    // SAFETY: plain query of a handle value.
    assert!(
        !unsafe { IsWindow(child) }.as_bool(),
        "the window is destroyed"
    );

    release.send(()).unwrap();
    wait_for_no_threads();
    pump_until(Duration::from_millis(200), || false);
    assert_eq!(shown(&handler), None);
    assert!(!handler.shared().has_ready(), "the late result was kept");
    drop(handler);
    assert_eq!(wait_until_unloadable(), S_OK);
}

/// The preview handler from the DLL's entry points, as prevhost makes it.
fn preview_from_dll() -> IPreviewHandler {
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid GUID pointers and a local out-pointer.
    let hr = unsafe { DllGetClassObject(&CLSID_PREVIEW, &IClassFactory::IID, &mut out) };
    assert_eq!(hr, S_OK);
    // SAFETY: on S_OK `out` holds an IClassFactory reference that we now own.
    let factory = unsafe { IClassFactory::from_raw(out) };
    // SAFETY: plain call.
    unsafe { factory.CreateInstance(None) }.expect("CreateInstance")
}

#[test]
fn preview_of_heic_shows_the_image_and_unload_releases_bitmap_and_window() {
    let _serial = serial();
    assert_eq!(wait_until_unloadable(), S_OK, "something left from before");
    let parent = Parent::new();
    let handler = ComObject::new(Handler::new(built_worker()));
    let preview: IPreviewHandler = handler.to_interface();
    let init: IInitializeWithStream = handler.to_interface();
    // SAFETY: our window, a local rect and a valid stream.
    unsafe {
        preview.SetWindow(parent.0, &AREA).unwrap();
        init.Initialize(&mem_stream(&sample()), 0).unwrap();
        preview.DoPreview().expect("DoPreview");
    }
    assert!(
        pump_until(Duration::from_secs(20), || shown(&handler).is_some()),
        "no image within 20 s"
    );
    // 451 x 461, asked for the area's long side (300): the long side becomes 300.
    let (width, height) = shown(&handler).unwrap();
    assert_eq!(height, 300);
    assert_eq!(width, (451.0f64 * 300.0 / 461.0).round() as u32);
    let child = handler.child().expect("a child window");
    assert!(handler.shared().view().bitmap().is_some());
    assert_eq!(live_bitmaps(), 1);
    // SAFETY: plain query of a handle value.
    assert!(unsafe { IsWindow(child) }.as_bool());
    assert_eq!(DllCanUnloadNow(), S_FALSE, "the handler is alive");

    // SAFETY: plain call.
    unsafe { preview.Unload() }.unwrap();
    // SAFETY: plain query of a handle value.
    assert!(!unsafe { IsWindow(child) }.as_bool(), "the window is left");
    assert_eq!(live_bitmaps(), 0, "the bitmap is left");
    assert_eq!(shown(&handler), None);
    assert_eq!(handler.child(), None);

    // Unloaded, the handler takes another file (prevhost reuses handlers).
    // SAFETY: a valid stream.
    unsafe { init.Initialize(&mem_stream(&sample()), 0) }.expect("Initialize after Unload");
    // SAFETY: plain call.
    unsafe { preview.DoPreview() }.expect("DoPreview again");
    assert!(pump_until(Duration::from_secs(20), || shown(&handler).is_some()));
    // Released without Unload: the window goes with it.
    let child = handler.child().unwrap();
    drop((preview, init, handler));
    // SAFETY: plain query of a handle value.
    assert!(!unsafe { IsWindow(child) }.as_bool());
    assert_eq!(wait_until_unloadable(), S_OK);
}

#[test]
fn the_dll_serves_every_preview_interface() {
    let _serial = serial();
    let preview = preview_from_dll();
    assert!(preview.cast::<IInitializeWithStream>().is_ok());
    assert!(preview.cast::<IPreviewHandlerVisuals>().is_ok());
    let site: IObjectWithSite = preview.cast().unwrap();
    let window: IOleWindow = preview.cast().unwrap();
    let mut out: *mut c_void = std::ptr::NonNull::<c_void>::dangling().as_ptr();
    // SAFETY: a local out-pointer, through the vtable to see it nulled.
    let none =
        unsafe { (Interface::vtable(&site).GetSite)(site.as_raw(), &IUnknown::IID, &mut out) };
    assert_eq!(none, E_FAIL, "no site yet");
    assert!(out.is_null(), "*out not nulled");
    // SAFETY: plain call.
    assert!(unsafe { window.GetWindow() }.is_err(), "no window yet");
    // Through the vtable: the wrappers take S_FALSE for success.
    let vtable = Interface::vtable(&preview);
    // SAFETY: a null message is never read: there is no frame to pass it to.
    let key = unsafe { (vtable.TranslateAccelerator)(preview.as_raw(), std::ptr::null()) };
    assert_eq!(key, S_FALSE, "no frame: not handled");
    // SAFETY: plain call.
    assert_eq!(
        unsafe { (vtable.SetFocus)(preview.as_raw()) },
        S_FALSE,
        "no window"
    );

    // Any object will do as a site; it has no IPreviewHandlerFrame.
    let some_site: IUnknown = preview.cast().unwrap();
    // SAFETY: plain calls.
    unsafe {
        site.SetSite(&some_site).unwrap();
        let got: IUnknown = site.GetSite().unwrap();
        assert_eq!(got, some_site);
        site.SetSite(None).unwrap();
    }
    drop(some_site);
    let parent = Parent::new();
    let visuals: IPreviewHandlerVisuals = preview.cast().unwrap();
    // SAFETY: our window and a local rect; plain calls.
    unsafe {
        preview.SetWindow(parent.0, &AREA).unwrap();
        assert_eq!(window.GetWindow().unwrap(), parent.0);
        visuals
            .SetBackgroundColor(windows::Win32::Foundation::COLORREF(0x123456))
            .unwrap();
        visuals
            .SetTextColor(windows::Win32::Foundation::COLORREF(0))
            .unwrap();
        visuals.SetFont(std::ptr::null()).unwrap();
    }
    drop((preview, site, window, visuals));
    assert_eq!(wait_until_unloadable(), S_OK);
}

/// The child half of [`preview_works_from_a_low_integrity_host`]: does nothing unless that
/// test started this process.
#[test]
fn low_integrity_preview_child() {
    let Some(exe) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    assert!(lv_sandbox::low_host::is_low_integrity_in_job());
    let parent = Parent::new();
    let handler = ComObject::new(Handler::new(exe.into()));
    let preview: IPreviewHandler = handler.to_interface();
    let init: IInitializeWithStream = handler.to_interface();
    // SAFETY: our window, a local rect and a valid stream.
    unsafe {
        preview.SetWindow(parent.0, &AREA).unwrap();
        init.Initialize(&mem_stream(&sample()), 0).unwrap();
        preview.DoPreview().expect("DoPreview");
    }
    assert!(
        pump_until(Duration::from_secs(30), || shown(&handler).is_some()),
        "no image within 30 s at low integrity"
    );
    assert_eq!(shown(&handler).map(|(w, h)| w.max(h)), Some(300));
    // SAFETY: plain call.
    unsafe { preview.Unload() }.unwrap();
}

/// Prevhost may run at low integrity (Task 1b left it unverified), itself in a Job: the
/// whole preview path still works there, the worker falling back from its AppContainer if
/// it must (ruling R2).
#[test]
fn preview_works_from_a_low_integrity_host() {
    let (_folder, exe) = lv_sandbox::low_host::staged_worker(&built_worker());
    lv_sandbox::low_host::run_test_at_low_integrity(
        "preview::tests::low_integrity_preview_child",
        CHILD_ENV,
        exe.as_os_str(),
    );
}
