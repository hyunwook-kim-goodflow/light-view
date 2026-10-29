//! Starting the viewer: the window class, the main window and its `App`, the message
//! loop, and what happens at exit.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::Context;
use lv_core::settings::Settings;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::DragAcceptFiles;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, GetSystemMetrics, LoadCursorW,
    LoadImageW, RegisterClassExW, SetWindowPlacement, TranslateMessage, CS_DBLCLKS, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, HICON, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, MSG,
    SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, WINDOW_EX_STYLE, WNDCLASSEXW, WS_CLIPCHILDREN,
    WS_OVERLAPPEDWINDOW,
};

use crate::commands::{self, HwndCell};
use crate::delete::{Deleter, ShellTrash};
use crate::exif_card::ExifCard;
use crate::input::{self, Pointer};
use crate::menu::Menu;
use crate::overlay::Overlay;
use crate::placement;
use crate::render::Renderer;
use crate::resize::ResizeState;
use crate::start::StartScreen;
use crate::theme::{self, system_prefers_dark};
use crate::toolbar;
use crate::tooltip;
use crate::update::{self, UpdateState};
use crate::viewer::Redraw;
use crate::watch::Watching;

use super::{wndproc, App, CLASS_NAME};

/// How long the (already hidden) app waits at exit for the Recycle Bin.
const EXIT_COMMIT_WAIT: Duration = Duration::from_secs(10);
/// Shows the window with `initial` open, or with the resize screen holding `resize`
/// (`--resize`), and runs until it closes.
pub fn run(
    initial: Option<PathBuf>,
    resize: Vec<PathBuf>,
    settings: Settings,
    settings_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    let hwnd_cell = HwndCell::default();
    let (mut viewer, pool, decode_limits) = crate::timing::step("viewer-ms", || {
        commands::new_viewer(&settings, hwnd_cell.clone())
    });
    // The first photo starts decoding now, while the window and its renderer are made,
    // instead of after them. Its ready message may come before the window exists and is
    // then lost: the window checks for the photo itself once it is up.
    let first = initial.as_deref().and_then(commands::file_to_open);
    if let Some(file) = &first {
        crate::timing::set_target(file);
        crate::timing::step("open-ms", || viewer.open(file));
    }
    let (save_tx, save_rx) = channel();
    let (delete_tx, delete_rx) = channel();
    let post = hwnd_cell.clone();
    let trash = ShellTrash::new(Box::new(move |path, result| {
        let _ = delete_tx.send((path, result));
        post.post(commands::WM_APP_DELETE_DONE);
    }));
    let app = RefCell::new(App {
        renderer: None,
        viewer,
        uploaded: None,
        fullscreen: None,
        dark: system_prefers_dark(),
        settings,
        settings_path,
        restoring: false,
        client_dips: (0.0, 0.0),
        dip: 1.0,
        icon_family: "",
        pointer: Pointer::default(),
        overlay: Overlay::default(),
        exif_card: ExifCard::default(),
        menu: Menu::default(),
        start: StartScreen::default(),
        pool,
        decode_limits,
        saving: None,
        save_tx,
        save_rx,
        deleter: Deleter::new(Box::new(trash)),
        delete_rx,
        watching: Watching::default(),
        settings_win: None,
        update: UpdateState::default(),
        resize: ResizeState::default(),
    });

    // SAFETY: Win32 calls with valid arguments; `app` outlives the window because the
    // message loop below runs until the window is destroyed, and the window procedure
    // clears its pointer on WM_NCDESTROY.
    unsafe {
        let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).context("LoadCursorW")?,
            hIcon: app_icon(instance.into(), false),
            hIconSm: app_icon(instance.into(), true),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(windows::core::Error::from_win32()).context("RegisterClassExW");
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("light-view"),
            // Clip children: the resize screen's number fields are child windows.
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            instance,
            Some(std::ptr::from_ref(&app).cast()),
        )
        .context("CreateWindowExW")?;
        hwnd_cell.set(hwnd);
        DragAcceptFiles(hwnd, true);

        theme::apply_title_bar(hwnd, app.borrow().dark);
        let tooltip = tooltip::create(hwnd);
        if let Some(tt) = tooltip {
            let dark = app.borrow().dark;
            tooltip::set_dark(tt, dark);
        }
        // On failure, destroy the window before `app` goes away (no borrow is held).
        crate::timing::mark("window-at");
        let renderer = match crate::timing::step("d2d-ms", || Renderer::new(hwnd)) {
            Ok(r) => r,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                return Err(e).context("Direct2D renderer");
            }
        };
        {
            let mut a = app.borrow_mut();
            a.icon_family = toolbar::icon_family(&renderer);
            a.renderer = Some(renderer);
            a.pointer.set_tooltip(tooltip);
        }
        // The first photo (opened above) is shown if it is ready, and the title, toolbar
        // and folder watch follow it, as after any open.
        if first.is_some() {
            let mut a = app.borrow_mut();
            // Explorer may have told the folder's order before the window was there.
            a.viewer.take_sort_answers();
            a.viewer.check_current();
            drop(a);
            commands::after(&app, hwnd, Redraw::Full);
        }

        let saved = app.borrow().settings.window.clone();
        let wp = placement::initial_placement(saved.as_ref());
        // Restoring onto a monitor with another DPI sends WM_DPICHANGED; the saved
        // rect is already in that monitor's pixels, so it must not be rescaled.
        app.borrow_mut().restoring = true;
        let restored = crate::timing::step("show-ms", || SetWindowPlacement(hwnd, &wp));
        app.borrow_mut().restoring = false;
        if let Err(e) = restored {
            let _ = DestroyWindow(hwnd);
            return Err(e).context("SetWindowPlacement");
        }
        // Size the image area for the final (possibly maximized) size, not the
        // CW_USEDEFAULT size seen during creation.
        {
            let mut a = app.borrow_mut();
            a.on_size(hwnd);
            a.refresh_toolbar();
        }
        input::sync_tooltips(&app, hwnd);
        let _ = InvalidateRect(hwnd, None, false);
        crate::timing::mark("shown-at");
        // Explorer's 크기 조정: the screen opens with this launch's photos (R4).
        if !resize.is_empty() {
            crate::resize::enter(&app, hwnd, resize);
        }
        // The update question, if the first photo is already up; and the daily check
        // (on its own thread) if the user said yes before.
        update::maybe_ask(&app, hwnd);
        update::check_if_due(&app, hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // The window is gone: the pending delete goes to the Recycle Bin now, and so do the
    // originals a resize replaced (their undo toast is gone with the window).
    app.borrow_mut().deleter.commit_all(EXIT_COMMIT_WAIT);
    crate::resize::flush_at_exit(&app);
    Ok(())
}

/// The app icon (resource 1, `res/light-view.ico`) at the system's large or small icon
/// size, so the title bar and taskbar get a drawn size instead of a scaled one. A null
/// icon (no resource, as in some test builds) leaves the default.
pub(crate) fn app_icon(instance: HINSTANCE, small: bool) -> HICON {
    let (cx, cy) = if small {
        (SM_CXSMICON, SM_CYSMICON)
    } else {
        (SM_CXICON, SM_CYICON)
    };
    // SAFETY: plain calls; resource 1 is a MAKEINTRESOURCE id, not a pointer.
    unsafe {
        LoadImageW(
            instance,
            PCWSTR(APP_ICON_ID as usize as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(cx),
            GetSystemMetrics(cy),
            LR_DEFAULTCOLOR | LR_SHARED,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    }
}

/// The icon resource id `build.rs` gives `res/light-view.ico`.
const APP_ICON_ID: u16 = 1;
