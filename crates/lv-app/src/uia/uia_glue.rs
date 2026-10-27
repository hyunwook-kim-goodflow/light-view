//! Window glue (UI thread): the main window's tree from its `App`, and invocations
//! posted back by `uia_provider` run against it.

use std::cell::RefCell;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;

use crate::input;
use crate::menu::{self, ITEMS};
use crate::start;
use crate::window::{invalidate, App};

use super::{main_tree, Act, CardInput, MainInput, MenuInput, PhotoInput, ToastInput, Tree};

/// The main window's tree from its state.
pub fn main_tree_of(app: &App) -> Tree {
    let name = app
        .viewer
        .current_path()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let photo = name.as_deref().map(|name| PhotoInput {
        name,
        position: app.viewer.position(),
    });
    let model = app.card_layout();
    let card = model.as_ref().map(|m| CardInput {
        model: m,
        selected: app.exif_card.selected(m),
        scroll: app.exif_card.scroll(m),
    });
    let client = app.client();
    let menu = app.menu.layout(client).map(|layout| MenuInput {
        layout,
        selected: app.menu.selected,
        enabled: std::array::from_fn(|k| app.menu.enabled(k)),
    });
    let now = app.pointer.now_ms();
    let toast = app
        .overlay
        .toast
        .as_ref()
        .filter(|t| t.visible(now))
        .map(|toast| ToastInput {
            toast,
            parts: toast.parts(),
        });
    main_tree(&MainInput {
        dip: app.dip,
        client,
        photo,
        toolbar: app.toolbar_input().map(|i| (i, app.pointer.shown)),
        start: app.start_layout(),
        card,
        menu,
        dialog: app.overlay.dialog.as_ref(),
        toast,
    })
}

/// Carries out an element's invocation (`WM_APP_UIA`) in the main window, as the same
/// click would. Elements outside an open dialog or menu are not exposed, but a stale
/// invocation could still arrive: then nothing happens. Nothing happens either while
/// the window is disabled by the modal Open dialog, as for a click.
pub fn run_main(app: &RefCell<App>, hwnd: HWND, act: Act) {
    // SAFETY: plain query on our own window handle.
    if !unsafe { IsWindowEnabled(hwnd) }.as_bool() {
        return;
    }
    let Ok(a) = app.try_borrow() else {
        return;
    };
    let (dialog, menu) = (a.overlay.dialog_open(), a.menu.is_open());
    let has_file = a.toolbar_input().is_some();
    drop(a);
    match act {
        Act::Tool(b) if !dialog && !menu && has_file => input::on_button(app, hwnd, b),
        Act::Open if !dialog && !menu => start::open_dialog(app, hwnd),
        Act::Card(target) if !dialog && !menu => {
            let action = {
                let Ok(mut a) = app.try_borrow_mut() else {
                    return;
                };
                let Some(m) = a.card_model() else {
                    return;
                };
                a.exif_card.act(target, &m)
            };
            if let Some(action) = action {
                input::card_action(app, hwnd, action);
            }
        }
        Act::Menu(k) if menu => {
            let enabled = {
                let Ok(mut a) = app.try_borrow_mut() else {
                    return;
                };
                let enabled = a.menu.enabled(k);
                a.menu.close();
                enabled
            };
            invalidate(hwnd);
            input::sync_tooltips(app, hwnd);
            if let (true, Some(item)) = (enabled, ITEMS.get(k)) {
                menu::run_item(app, hwnd, *item);
            }
        }
        Act::Dialog(k) if dialog => {
            let event = app
                .try_borrow_mut()
                .ok()
                .and_then(|mut a| a.overlay.activate(k));
            input::overlay_changed(app, hwnd, event);
        }
        Act::Undo if !dialog => {
            let event = app
                .try_borrow_mut()
                .ok()
                .and_then(|mut a| a.overlay.take_toast_action());
            if event.is_some() {
                input::overlay_changed(app, hwnd, event);
            }
        }
        _ => {}
    }
}
