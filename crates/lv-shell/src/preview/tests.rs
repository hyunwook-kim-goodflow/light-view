//! The preview handler as prevhost uses it: a parent window of the test's own, a stream from
//! `SHCreateMemStream`, results posted to the child window and taken by pumping this
//! thread's messages. Tests that count objects or threads hold [`serial`].

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{implement, w, ComObject, IUnknown, Interface, Result, HRESULT, PCWSTR};
use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL, HWND, RECT, S_FALSE, S_OK};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    IClassFactory, ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATFLAG, STATSTG,
    STGC, STREAM_SEEK,
};
use windows::Win32::System::Ole::{IObjectWithSite, IOleWindow};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandlerVisuals};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetParent, GetWindowRect, IsWindow,
    PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
};

use super::draw::{live_bitmaps, Image};
use super::window::live_windows;
use super::{preview_side, queue, Handler, Render, MAX_SIDE, MIN_SIDE};
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
    // A tiny or empty area still asks for a useful image (it is never enlarged later).
    assert_eq!(MIN_SIDE, 256);
    assert_eq!(preview_side(RECT::default()), MIN_SIDE);
    let small = RECT {
        left: 0,
        top: 0,
        right: 40,
        bottom: 30,
    };
    assert_eq!(preview_side(small), MIN_SIDE);
    let inverted = RECT {
        left: 50,
        top: 50,
        right: 0,
        bottom: 0,
    };
    assert_eq!(preview_side(inverted), MIN_SIDE);
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

/// A render whose first call waits for the test (a slow first preview): it says when a call
/// starts (its index) and answers call `n` with a `30 + n` x 40 image, the first with 10 x 20.
struct Gated {
    render: Render,
    started: mpsc::Receiver<usize>,
    release: mpsc::Sender<()>,
}

fn gated_render() -> Gated {
    let (release, gate) = mpsc::channel::<()>();
    let (tell, started) = mpsc::channel::<usize>();
    let (gate, tell) = (Mutex::new(gate), Mutex::new(tell));
    let calls = AtomicUsize::new(0);
    let render: Render = Arc::new(move |_, _| {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        let _ = tell.lock().unwrap().send(call);
        if call == 0 {
            let _ = gate.lock().unwrap().recv();
            Some(image(10, 20))
        } else {
            Some(image(30 + call as u32, 40))
        }
    });
    Gated {
        render,
        started,
        release,
    }
}

/// Review Focus 4: when an earlier preview's result arrives after a later DoPreview, only
/// the later one is drawn.
#[test]
fn stale_preview_result_is_dropped() {
    let _serial = serial();
    let parent = Parent::new();
    let gated = gated_render();
    let handler = handler_with(gated.render, &parent, b"heic");
    do_preview(&handler).expect("first DoPreview");
    assert_eq!(gated.started.recv_timeout(Duration::from_secs(10)), Ok(0));
    do_preview(&handler).expect("second DoPreview");
    // Now the first preview answers, late, and then the second runs.
    gated.release.send(()).unwrap();
    let mut seen = Vec::new();
    assert!(
        pump_until(Duration::from_secs(10), || {
            let now = shown(&handler);
            if now.is_some() && seen.last() != Some(&now) {
                seen.push(now);
            }
            now.is_some()
        }),
        "the second result is drawn"
    );
    wait_for_no_threads();
    pump_until(Duration::from_millis(200), || false);
    assert_eq!(
        seen,
        [Some((31, 40))],
        "only the second result was ever shown"
    );
    assert_eq!(shown(&handler), Some((31, 40)));
    assert!(!handler.shared().has_ready(), "the stale result was kept");
}

/// Ruling I2: one render at a time in the process, the latest request wins. While the first
/// render runs, more previews come; only the first and the last start a worker.
#[test]
fn only_the_first_and_the_last_of_many_previews_start() {
    let _serial = serial();
    wait_for_no_threads();
    let before = queue::started();
    let parent = Parent::new();
    let gated = gated_render();
    let handler = handler_with(gated.render, &parent, b"heic");
    do_preview(&handler).expect("first DoPreview");
    assert_eq!(gated.started.recv_timeout(Duration::from_secs(10)), Ok(0));
    for _ in 0..5 {
        do_preview(&handler).expect("DoPreview while the first renders");
    }
    // A second handler's request (another file in the same prevhost) is the latest of all.
    let other_parent = Parent::new();
    let other = handler_with(Arc::new(|_, _| Some(image(7, 7))), &other_parent, b"other");
    do_preview(&other).expect("the other handler's DoPreview");
    gated.release.send(()).unwrap();
    assert!(pump_until(Duration::from_secs(10), || shown(&other).is_some()));
    wait_for_no_threads();
    assert_eq!(
        queue::started() - before,
        2,
        "only the first and the last started"
    );
    assert_eq!(
        gated.started.try_iter().collect::<Vec<_>>(),
        Vec::<usize>::new(),
        "none of the replaced previews started"
    );
    assert_eq!(shown(&other), Some((7, 7)));
    // The first handler's only started preview was superseded: it shows nothing.
    pump_until(Duration::from_millis(100), || false);
    assert_eq!(shown(&handler), None);

    // Unload empties the queue of the handler's waiting request.
    let late = gated_render();
    let third = handler_with(late.render, &parent, b"third");
    do_preview(&third).unwrap();
    assert_eq!(late.started.recv_timeout(Duration::from_secs(10)), Ok(0));
    do_preview(&third).unwrap();
    // SAFETY: plain call.
    unsafe { third.to_interface::<IPreviewHandler>().Unload() }.unwrap();
    let before = queue::started();
    late.release.send(()).unwrap();
    wait_for_no_threads();
    assert_eq!(
        queue::started(),
        before,
        "an unloaded preview's request started"
    );
}

/// Review I1: the parent destroys our window before Unload (the pane closes). Nothing is
/// posted to the dead window, every call afterwards leaves it alone, and the next SetWindow
/// and DoPreview make a new window that shows the image.
#[test]
fn a_window_destroyed_by_its_parent_is_forgotten() {
    let _serial = serial();
    let parent = Parent::new();
    let gated = gated_render();
    let handler = handler_with(gated.render, &parent, b"heic");
    let preview: IPreviewHandler = handler.to_interface();
    do_preview(&handler).unwrap();
    assert_eq!(gated.started.recv_timeout(Duration::from_secs(10)), Ok(0));
    let old = handler.child().expect("a window");
    assert_eq!(live_windows(), 1);
    drop(parent);
    // SAFETY: plain query of a handle value.
    assert!(!unsafe { IsWindow(old) }.as_bool());
    assert_eq!(handler.child(), None, "the dead window is still ours");
    assert_eq!(live_windows(), 0);

    // The render finishes for the dead window: nothing is posted or kept.
    gated.release.send(()).unwrap();
    wait_for_no_threads();
    assert!(!handler.shared().has_ready());
    assert_eq!(shown(&handler), None);
    // SAFETY: a local rect; plain calls. None may touch the dead window.
    unsafe {
        preview.SetRect(&AREA).unwrap();
        assert_eq!(
            (Interface::vtable(&preview).SetFocus)(preview.as_raw()),
            S_FALSE,
            "no window to focus"
        );
    }

    let parent = Parent::new();
    // SAFETY: our window and a local rect.
    unsafe { preview.SetWindow(parent.0, &AREA) }.unwrap();
    do_preview(&handler).expect("DoPreview in the new parent");
    assert!(pump_until(Duration::from_secs(10), || shown(&handler).is_some()));
    let new = handler.child().expect("a new window");
    assert_ne!(new, old);
    // SAFETY: plain query of a handle value.
    assert!(unsafe { IsWindow(new) }.as_bool());
    // SAFETY: plain call.
    unsafe { preview.Unload() }.unwrap();
    assert_eq!(live_windows(), 0);
    drop((preview, handler));
    assert_eq!(wait_until_unloadable(), S_OK);
}

#[test]
fn set_window_and_set_rect_move_the_child() {
    let _serial = serial();
    let parent = Parent::new();
    let handler = handler_with(Arc::new(|_, _| Some(image(5, 5))), &parent, b"heic");
    let preview: IPreviewHandler = handler.to_interface();
    do_preview(&handler).unwrap();
    let child = handler.child().unwrap();
    let placed = |want: RECT| {
        let mut got = RECT::default();
        // SAFETY: our window and a local rect.
        unsafe { GetWindowRect(child, &mut got) }.unwrap();
        let mut corner = [POINT {
            x: got.left,
            y: got.top,
        }];
        // SAFETY: plain call on a local array.
        assert!(unsafe { ScreenToClient(GetParent(child).unwrap(), &mut corner[0]) }.as_bool());
        assert_eq!(
            (
                corner[0].x,
                corner[0].y,
                got.right - got.left,
                got.bottom - got.top
            ),
            (
                want.left,
                want.top,
                want.right - want.left,
                want.bottom - want.top
            )
        );
    };
    placed(AREA);
    let moved = RECT {
        left: 10,
        top: 20,
        right: 110,
        bottom: 70,
    };
    // SAFETY: a local rect.
    unsafe { preview.SetRect(&moved) }.unwrap();
    placed(moved);
    let other = Parent::new();
    // SAFETY: our window and a local rect.
    unsafe { preview.SetWindow(other.0, &AREA) }.unwrap();
    // SAFETY: plain call.
    assert_eq!(unsafe { GetParent(child) }.unwrap(), other.0, "reparented");
    placed(AREA);
    // SAFETY: plain call.
    unsafe { preview.Unload() }.unwrap();
}

/// A memory stream whose first Read first runs `on_read`: another call of ours coming in
/// while prevhost's stream proxy is out of the apartment.
#[implement(IStream)]
struct Reentrant {
    inner: IStream,
    on_read: RefCell<Option<Box<dyn FnOnce()>>>,
}

#[allow(non_snake_case)]
impl ISequentialStream_Impl for Reentrant_Impl {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        let hook = self.on_read.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
        // SAFETY: the caller's buffer and out-pointer, passed on.
        unsafe {
            (Interface::vtable(&self.inner).base__.Read)(self.inner.as_raw(), pv, cb, pcbread)
        }
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        E_NOTIMPL
    }
}

#[allow(non_snake_case)]
impl IStream_Impl for Reentrant_Impl {
    fn Seek(&self, mv: i64, origin: STREAM_SEEK, newpos: *mut u64) -> Result<()> {
        // SAFETY: passed on to a valid stream.
        unsafe {
            self.inner
                .Seek(mv, origin, (!newpos.is_null()).then_some(newpos))
        }
    }
    fn SetSize(&self, _size: u64) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn CopyTo(&self, _: Option<&IStream>, _: u64, _: *mut u64, _: *mut u64) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Commit(&self, _flags: &STGC) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Revert(&self) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn LockRegion(&self, _: u64, _: u64, _: &LOCKTYPE) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn UnlockRegion(&self, _: u64, _: u64, _: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Stat(&self, stat: *mut STATSTG, flag: &STATFLAG) -> Result<()> {
        // SAFETY: passed on to a valid stream.
        unsafe { self.inner.Stat(stat, *flag) }
    }
    fn Clone(&self) -> Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}

/// Review I3: Unload comes in while DoPreview reads the stream. The DoPreview then stops:
/// no window is made (none leaks), no render starts, nothing is shown.
#[test]
fn unload_during_the_stream_read_stops_the_preview() {
    let _serial = serial();
    wait_for_no_threads();
    let before = queue::started();
    let parent = Parent::new();
    let handler = ComObject::new(Handler::with_render(Arc::new(|_, _| Some(image(5, 5)))));
    let preview: IPreviewHandler = handler.to_interface();
    let reentrant = preview.clone();
    let stream: IStream = Reentrant {
        inner: mem_stream(b"heic bytes"),
        on_read: RefCell::new(Some(Box::new(move || {
            // SAFETY: plain call, reentering the handler as a nested COM call would.
            unsafe { reentrant.Unload() }.unwrap();
        }))),
    }
    .into();
    // SAFETY: our window, a local rect and a valid stream.
    unsafe {
        preview.SetWindow(parent.0, &AREA).unwrap();
        handler
            .to_interface::<IInitializeWithStream>()
            .Initialize(&stream, 0)
            .unwrap();
    }
    assert!(do_preview(&handler).is_err(), "an unloaded preview went on");
    assert_eq!(handler.child(), None);
    assert_eq!(live_windows(), 0, "a window leaked");
    wait_for_no_threads();
    assert_eq!(queue::started(), before, "a render started");
    assert_eq!(shown(&handler), None);
    drop((stream, preview, handler));
    assert_eq!(wait_until_unloadable(), S_OK);
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
