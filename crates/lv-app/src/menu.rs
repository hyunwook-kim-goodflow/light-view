//! The in-window right-click menu (spec §4.3a, ruling R1): state, layout and hit testing
//! (pure, in DIPs), drawing through `Renderer`, its input routing and what each item
//! does.
//!
//! It opens at the right-click point (moved inside the window when it would stick out),
//! or from the keyboard with the menu key or Shift+F10. While it is open it takes every
//! key but Alt combinations: ↑↓ (Home/End) move the selection, Enter or Space picks the
//! selected item, Esc closes it. A click outside closes it and is not passed on.

use std::cell::RefCell;

use lv_core::command::{Mods, VK_ESCAPE, VK_RETURN};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, WM_CONTEXTMENU, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::about;
use crate::clipboard;
use crate::commands;
use crate::input;
use crate::overlay::Toast;
use crate::shell;
use crate::start;
use crate::strings::{t, S};
use crate::toolbar::Rect;
use crate::window::{invalidate, App};

mod draw;

pub use draw::{draw, MenuView};

const VK_APPS: u16 = 0x5D;
const VK_F10: u16 = 0x79;
const VK_SPACE: u16 = 0x20;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_UP: u16 = 0x26;
const VK_DOWN: u16 = 0x28;

// Layout (DIPs).
const MENU_W: f32 = 264.0;
const ROW_H: f32 = 44.0;
const PAD_Y: f32 = 6.0;
const SEPARATOR_H: f32 = 9.0;
/// The menu keeps this far from the window edges when it is moved inside.
const EDGE: f32 = 8.0;
const RADIUS: f32 = 10.0;
const ROW_INSET: f32 = 4.0;
const ROW_RADIUS: f32 = 8.0;
const TEXT_PAD: f32 = 16.0;
/// Where a keyboard-opened menu goes when the pointer is not over the window.
const KEYBOARD_ANCHOR: (f32, f32) = (16.0, 16.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Open,
    ShowInFolder,
    CopyPath,
    DefaultApps,
    Settings,
    About,
}

/// Top to bottom; a separator follows the first `GROUP` items.
pub const ITEMS: [MenuItem; 6] = [
    MenuItem::Open,
    MenuItem::ShowInFolder,
    MenuItem::CopyPath,
    MenuItem::DefaultApps,
    MenuItem::Settings,
    MenuItem::About,
];
const GROUP: usize = 3;

impl MenuItem {
    /// The label's string key.
    pub fn key(self) -> S {
        match self {
            MenuItem::Open => S::MenuOpen,
            MenuItem::ShowInFolder => S::MenuShowInFolder,
            MenuItem::CopyPath => S::MenuCopyPath,
            MenuItem::DefaultApps => S::MenuDefaultApps,
            MenuItem::Settings => S::MenuSettings,
            MenuItem::About => S::MenuAbout,
        }
    }

    pub fn label(self) -> &'static str {
        t(self.key())
    }

    /// The shortcut's string key, for items that have one.
    pub fn shortcut_key(self) -> Option<S> {
        match self {
            MenuItem::Open => Some(S::ShortcutOpen),
            MenuItem::Settings => Some(S::ShortcutSettings),
            _ => None,
        }
    }

    pub fn shortcut(self) -> Option<&'static str> {
        self.shortcut_key().map(t)
    }

    /// Items about the shown photo, unavailable without one.
    pub fn needs_file(self) -> bool {
        matches!(self, MenuItem::ShowInFolder | MenuItem::CopyPath)
    }
}

/// Whether a key opens the menu: the menu key, or Shift+F10.
pub fn opens_menu(vk: u16, mods: Mods) -> bool {
    !mods.alt && !mods.ctrl && (vk == VK_APPS || (vk == VK_F10 && mods.shift))
}

/// Menu rects in DIPs; `items` follows `ITEMS`.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuLayout {
    pub card: Rect,
    pub items: Vec<Rect>,
    pub separator: Rect,
}

/// The menu's top-left corner at `anchor`, moved left or up just enough to stay
/// `EDGE` inside a `view`-sized client area (never past its top-left corner).
pub fn menu_layout(anchor: (f32, f32), view: (f32, f32)) -> MenuLayout {
    let h = 2.0 * PAD_Y + ITEMS.len() as f32 * ROW_H + SEPARATOR_H;
    let place = |at: f32, size: f32, room: f32| at.min(room - size - EDGE).max(0.0);
    let card = Rect {
        x: place(anchor.0, MENU_W, view.0),
        y: place(anchor.1, h, view.1),
        w: MENU_W,
        h,
    };
    let mut y = card.y + PAD_Y;
    let mut separator = card;
    let mut items = Vec::with_capacity(ITEMS.len());
    for i in 0..ITEMS.len() {
        if i == GROUP {
            separator = Rect {
                y,
                h: SEPARATOR_H,
                ..card
            };
            y += SEPARATOR_H;
        }
        items.push(Rect {
            y,
            h: ROW_H,
            ..card
        });
        y += ROW_H;
    }
    MenuLayout {
        card,
        items,
        separator,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Taken, nothing changed.
    Nothing,
    Redraw,
    /// The menu closed; do this.
    Activate(MenuItem),
}

#[derive(Debug, Default)]
pub struct Menu {
    /// Where it was opened (client DIPs); `None` while closed.
    anchor: Option<(f32, f32)>,
    has_file: bool,
    /// Index into `ITEMS`: the keyboard's (or the pointer's) item.
    pub selected: Option<usize>,
    pressed: Option<usize>,
    /// The menu key or Shift+F10 just closed the menu: the keyboard WM_CONTEXTMENU
    /// that DefWindowProc sends after the key must not open it again.
    closed_by_key: bool,
    /// A click outside just closed the menu: its double-click must not reach the photo.
    closed_by_click: bool,
}

impl Menu {
    /// Opens at `anchor`. From the keyboard the first item is selected.
    pub fn open(&mut self, anchor: (f32, f32), has_file: bool, keyboard: bool) {
        self.anchor = Some(anchor);
        self.has_file = has_file;
        self.pressed = None;
        self.closed_by_key = false;
        self.closed_by_click = false;
        self.selected = None;
        if keyboard {
            self.selected = self.step(None, 1);
        }
    }

    pub fn close(&mut self) {
        self.anchor = None;
        self.selected = None;
        self.pressed = None;
    }

    pub fn is_open(&self) -> bool {
        self.anchor.is_some()
    }

    pub fn layout(&self, view: (f32, f32)) -> Option<MenuLayout> {
        self.anchor.map(|a| menu_layout(a, view))
    }

    pub fn enabled(&self, i: usize) -> bool {
        ITEMS
            .get(i)
            .is_some_and(|item| self.has_file || !item.needs_file())
    }

    /// The next enabled item after `from` in direction `dir` (±1), wrapping; from
    /// `None`, the first (or last) enabled one.
    fn step(&self, from: Option<usize>, dir: isize) -> Option<usize> {
        let n = ITEMS.len() as isize;
        let start = from.map_or(if dir > 0 { -1 } else { n }, |i| i as isize);
        (1..=n)
            .map(|k| (start + dir * k).rem_euclid(n) as usize)
            .find(|&i| self.enabled(i))
    }

    /// A key while the menu is open; `None` if it is closed or the key is not its own
    /// (Alt combinations).
    pub fn on_key(&mut self, vk: u16, mods: Mods) -> Option<MenuAction> {
        if !self.is_open() || mods.alt {
            return None;
        }
        let moved = |s: &mut Menu, to: Option<usize>| {
            s.selected = to.or(s.selected);
            MenuAction::Redraw
        };
        Some(match vk {
            VK_ESCAPE => {
                self.close();
                MenuAction::Redraw
            }
            _ if opens_menu(vk, mods) => {
                self.close();
                self.closed_by_key = true;
                MenuAction::Redraw
            }
            VK_DOWN => moved(self, self.step(self.selected, 1)),
            VK_UP => moved(self, self.step(self.selected, -1)),
            VK_HOME => moved(self, self.step(None, 1)),
            VK_END => moved(self, self.step(None, -1)),
            VK_RETURN | VK_SPACE => match self.selected {
                Some(i) => self.activate(i),
                None => MenuAction::Nothing,
            },
            _ => MenuAction::Nothing,
        })
    }

    fn activate(&mut self, i: usize) -> MenuAction {
        if !self.enabled(i) {
            return MenuAction::Nothing;
        }
        self.close();
        MenuAction::Activate(ITEMS[i])
    }

    fn item_at(&self, x: f32, y: f32, l: &MenuLayout) -> Option<usize> {
        l.items.iter().position(|r| r.contains(x, y))
    }

    /// The pointer moved: the enabled item under it is selected. True if that changed.
    pub fn hover(&mut self, x: f32, y: f32, view: (f32, f32)) -> bool {
        let Some(l) = self.layout(view) else {
            return false;
        };
        let hit = self.item_at(x, y, &l).filter(|&i| self.enabled(i));
        if hit.is_none() || hit == self.selected {
            return false;
        }
        self.selected = hit;
        true
    }

    /// Mouse down; true if the menu takes it (always while open). Outside, it closes.
    pub fn press(&mut self, x: f32, y: f32, view: (f32, f32)) -> bool {
        let Some(l) = self.layout(view) else {
            return false;
        };
        if l.card.contains(x, y) {
            self.pressed = self.item_at(x, y, &l).filter(|&i| self.enabled(i));
        } else {
            self.close();
            self.closed_by_click = true;
        }
        true
    }

    /// A keyboard WM_CONTEXTMENU (the menu key or Shift+F10): true if it should open
    /// the menu. Not while it is open (the key handler opened it), and not right after
    /// the key closed it.
    pub fn keyboard_context(&mut self) -> bool {
        let reopen = !self.is_open() && !self.closed_by_key;
        self.closed_by_key = false;
        reopen
    }

    /// Any other key or click while the menu is closed ends the "just closed" states.
    /// `double_click`: true if this input is a double-click that the click which
    /// closed the menu started (it is to be swallowed).
    pub fn other_input(&mut self, double_click: bool) -> bool {
        let swallow = double_click && self.closed_by_click;
        self.closed_by_key = false;
        self.closed_by_click = false;
        swallow
    }

    /// Mouse up: `None` if the menu did not take the press. It acts only when released
    /// on the item that was pressed.
    pub fn release(&mut self, x: f32, y: f32, view: (f32, f32)) -> Option<MenuAction> {
        let l = self.layout(view)?;
        let pressed = self.pressed.take();
        match pressed {
            Some(i) if self.item_at(x, y, &l) == Some(i) => Some(self.activate(i)),
            _ => Some(MenuAction::Nothing),
        }
    }
}

// ---- Actions -------------------------------------------------------------------

/// Does what a picked item says. No App borrow is held while it runs (the Open dialog
/// is modal and dispatches messages).
pub fn run_item(app: &RefCell<App>, hwnd: HWND, item: MenuItem) {
    invalidate(hwnd);
    match item {
        MenuItem::Open => start::open_dialog(app, hwnd),
        MenuItem::ShowInFolder => {
            let Some(path) = current_path(app) else {
                return;
            };
            if let Err(e) = shell::show_in_folder(&path) {
                eprintln!(
                    "light-view: could not show {} in its folder: {e}",
                    path.display()
                );
                input::show_toast(app, hwnd, |now| Toast::new(S::ToastOpenFailed, now));
            }
        }
        MenuItem::CopyPath => {
            let Some(path) = current_path(app) else {
                return;
            };
            let text = shell::user_path(&path).to_string_lossy().into_owned();
            match clipboard::set_text(hwnd, &text) {
                Ok(()) => input::show_toast(app, hwnd, |now| Toast::new(S::ToastPathCopied, now)),
                Err(e) => eprintln!("light-view: could not copy to the clipboard: {e}"),
            }
        }
        MenuItem::DefaultApps => {
            if !shell::open(hwnd, "ms-settings:defaultapps".as_ref()) {
                input::show_toast(app, hwnd, |now| Toast::new(S::ToastOpenFailed, now));
            }
        }
        MenuItem::Settings => commands::open_settings(app, hwnd),
        MenuItem::About => about::show(app, hwnd),
    }
}

fn current_path(app: &RefCell<App>) -> Option<std::path::PathBuf> {
    let a = app.try_borrow().ok()?;
    a.viewer.current_path().map(std::path::Path::to_path_buf)
}

// ---- Input ---------------------------------------------------------------------

/// The menu's input, seen first while no dialog is open (`input::overlay`): the keys
/// that open it, everything while it is open, and the right-click (WM_CONTEXTMENU).
/// `None` passes the message on. No App borrow is held while an item runs. The
/// toolbar's tooltips are off while the menu is open.
pub fn route(
    app: &RefCell<App>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    let open = app.try_borrow().ok()?.menu.is_open();
    let result = handle(app, hwnd, open, msg, wparam, lparam);
    if app.try_borrow().is_ok_and(|a| a.menu.is_open() != open) {
        input::sync_tooltips(app, hwnd);
    }
    result
}

fn handle(
    app: &RefCell<App>,
    hwnd: HWND,
    open: bool,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let (vk, mods) = (wparam.0 as u16, commands::mods());
            let repeat = lparam.0 & (1 << 30) != 0;
            if !open {
                if repeat {
                    return None;
                }
                if !opens_menu(vk, mods) {
                    app.try_borrow_mut().ok()?.menu.other_input(false);
                    return None;
                }
                open_menu(app, hwnd, None);
                return Some(LRESULT(0));
            }
            // Only ↑↓ repeat while held.
            let action = if repeat && !matches!(vk, VK_UP | VK_DOWN) {
                MenuAction::Nothing
            } else {
                app.try_borrow_mut().ok()?.menu.on_key(vk, mods)?
            };
            menu_action(app, hwnd, action);
            Some(LRESULT(0))
        }
        // After Shift+F10 opened (or closed) the menu, F10's key-up would start
        // keyboard menu mode.
        WM_SYSKEYUP
            if wparam.0 as u16 == VK_F10 && (open || app.try_borrow().ok()?.menu.closed_by_key) =>
        {
            Some(LRESULT(0))
        }
        WM_CONTEXTMENU => on_context_menu(app, hwnd, lparam),
        WM_KILLFOCUS if open => {
            app.try_borrow_mut().ok()?.menu.close();
            invalidate(hwnd);
            None
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK if open => {
            {
                let mut a = app.try_borrow_mut().ok()?;
                let (x, y) = a.to_dips(input::point(lparam));
                let view = a.client();
                a.menu.press(x, y, view);
            }
            invalidate(hwnd);
            Some(LRESULT(0))
        }
        // The double-click of the click that closed the menu is swallowed.
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let double = msg == WM_LBUTTONDBLCLK;
            app.try_borrow_mut()
                .ok()?
                .menu
                .other_input(double)
                .then_some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            let action = {
                let mut a = app.try_borrow_mut().ok()?;
                let (x, y) = a.to_dips(input::point(lparam));
                let view = a.client();
                a.menu.release(x, y, view)?
            };
            menu_action(app, hwnd, action);
            Some(LRESULT(0))
        }
        // The item under the pointer is selected; the toolbar still sees the move.
        WM_MOUSEMOVE if open => {
            let changed = {
                let mut a = app.try_borrow_mut().ok()?;
                let (x, y) = a.to_dips(input::point(lparam));
                let view = a.client();
                a.menu.hover(x, y, view)
            };
            if changed {
                invalidate(hwnd);
            }
            None
        }
        WM_MOUSEWHEEL if open => Some(LRESULT(0)),
        _ => None,
    }
}

/// Opens the menu at `at` (client DIPs), or from the keyboard (`None`) at the pointer
/// when it is over the window, else near the top-left corner. A drag in progress ends.
fn open_menu(app: &RefCell<App>, hwnd: HWND, at: Option<(f32, f32)>) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let anchor = at.or(a.pointer.cursor()).unwrap_or(KEYBOARD_ANCHOR);
    let has_file = a.viewer.has_file();
    a.menu.open(anchor, has_file, at.is_none());
    let dragging = a.pointer.cancel_press();
    drop(a);
    if dragging {
        input::release_capture();
    }
    invalidate(hwnd);
}

/// A right-click in the client area opens the menu there (again, if it is open). The
/// menu key and Shift+F10 come with lparam -1 (from DefWindowProc, after the key
/// handler opened or closed it already): opened unless open or just closed. Right-clicks elsewhere (the title
/// bar) go on to DefWindowProc.
fn on_context_menu(app: &RefCell<App>, hwnd: HWND, lparam: LPARAM) -> Option<LRESULT> {
    if lparam.0 as u32 == u32::MAX {
        if app.try_borrow_mut().ok()?.menu.keyboard_context() {
            open_menu(app, hwnd, None);
        }
        return Some(LRESULT(0));
    }
    let (x, y) = input::point(lparam);
    let mut pt = POINT { x, y };
    let mut rc = RECT::default();
    // SAFETY: `pt` and `rc` are live locals; plain calls on our window.
    unsafe {
        if !ScreenToClient(hwnd, &mut pt).as_bool() || GetClientRect(hwnd, &mut rc).is_err() {
            return None;
        }
    }
    if pt.x < rc.left || pt.y < rc.top || pt.x >= rc.right || pt.y >= rc.bottom {
        return None;
    }
    let at = app.try_borrow().ok()?.to_dips((pt.x, pt.y));
    open_menu(app, hwnd, Some(at));
    Some(LRESULT(0))
}

/// After the menu took an input: repaint, or run the picked item.
fn menu_action(app: &RefCell<App>, hwnd: HWND, action: MenuAction) {
    match action {
        MenuAction::Nothing => {}
        MenuAction::Redraw => invalidate(hwnd),
        MenuAction::Activate(item) => run_item(app, hwnd, item),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::{text, Lang};

    const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
    };
    const SHIFT: Mods = Mods {
        ctrl: false,
        shift: true,
        alt: false,
    };
    const ALT: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: true,
    };
    const VIEW: (f32, f32) = (1000.0, 700.0);

    fn center(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn inside(inner: Rect, outer: Rect) -> bool {
        inner.x >= outer.x - 0.01
            && inner.y >= outer.y - 0.01
            && inner.x + inner.w <= outer.x + outer.w + 0.01
            && inner.y + inner.h <= outer.y + outer.h + 0.01
    }

    #[test]
    fn items_in_spec_order_with_shortcuts() {
        let labels: Vec<_> = ITEMS.iter().map(|i| text(Lang::Ko, i.key())).collect();
        assert_eq!(
            labels,
            [
                "열기",
                "폴더에서 보기",
                "경로 복사",
                "기본 앱 설정 열기",
                "설정",
                "정보"
            ]
        );
        let keys = |i: MenuItem| i.shortcut_key().map(|k| text(Lang::Ko, k));
        assert_eq!(keys(MenuItem::Open), Some("Ctrl+O"));
        assert_eq!(keys(MenuItem::Settings), Some("Ctrl+,"));
        assert_eq!(keys(MenuItem::About), None);
        assert_eq!(MenuItem::Open.label(), t(S::MenuOpen));
    }

    #[test]
    fn menu_opens_at_the_point_with_44_dip_rows() {
        let l = menu_layout((100.0, 120.0), VIEW);
        assert_eq!((l.card.x, l.card.y), (100.0, 120.0));
        assert_eq!(l.card.w, 264.0);
        assert_eq!(l.items.len(), ITEMS.len());
        assert_eq!(l.card.h, 6.0 * 44.0 + 9.0 + 12.0);
        for (i, r) in l.items.iter().enumerate() {
            assert!(r.h >= 44.0 && r.w >= 44.0, "{r:?}");
            assert!(inside(*r, l.card));
            if i > 0 {
                assert!(r.y >= l.items[i - 1].y + l.items[i - 1].h - 0.01);
            }
        }
        // The separator sits between the photo items and the app items.
        assert_eq!(l.separator.y, l.items[2].y + l.items[2].h);
        assert_eq!(l.items[3].y, l.separator.y + l.separator.h);
    }

    #[test]
    fn menu_moves_inside_the_window() {
        // Near the bottom-right corner: moved left and up, 8 DIP from the edges.
        let l = menu_layout((990.0, 690.0), VIEW);
        assert_eq!(l.card.x + l.card.w, 1000.0 - 8.0);
        assert_eq!(l.card.y + l.card.h, 700.0 - 8.0);
        // Only the axis that sticks out moves.
        let l = menu_layout((900.0, 50.0), VIEW);
        assert_eq!(l.card.x + l.card.w, 992.0);
        assert_eq!(l.card.y, 50.0);
        let l = menu_layout((50.0, 600.0), VIEW);
        assert_eq!(l.card.x, 50.0);
        assert_eq!(l.card.y + l.card.h, 692.0);
        // Smaller than the menu: pinned to the top-left corner.
        let l = menu_layout((100.0, 100.0), (200.0, 150.0));
        assert_eq!((l.card.x, l.card.y), (0.0, 0.0));
        // Off-window anchors (negative) come back in.
        let l = menu_layout((-20.0, -5.0), VIEW);
        assert_eq!((l.card.x, l.card.y), (0.0, 0.0));
    }

    #[test]
    fn menu_key_and_shift_f10_open_it() {
        assert!(opens_menu(VK_APPS, NONE));
        assert!(opens_menu(VK_APPS, SHIFT));
        assert!(opens_menu(VK_F10, SHIFT));
        assert!(!opens_menu(VK_F10, NONE));
        assert!(!opens_menu(VK_APPS, ALT));
        assert!(!opens_menu(
            VK_F10,
            Mods {
                ctrl: true,
                shift: true,
                alt: false
            }
        ));
        assert!(!opens_menu(0x41, SHIFT));
    }

    #[test]
    fn keyboard_open_selects_the_first_item_mouse_open_none() {
        let mut m = Menu::default();
        m.open((10.0, 10.0), true, true);
        assert_eq!(m.selected, Some(0));
        m.open((10.0, 10.0), true, false);
        assert_eq!(m.selected, None);
        assert!(m.is_open());
    }

    #[test]
    fn arrows_move_and_wrap_skipping_unavailable_items() {
        let mut m = Menu::default();
        m.open((0.0, 0.0), true, true);
        assert_eq!(m.on_key(VK_DOWN, NONE), Some(MenuAction::Redraw));
        assert_eq!(m.selected, Some(1));
        m.on_key(VK_UP, NONE);
        m.on_key(VK_UP, NONE);
        assert_eq!(m.selected, Some(5)); // wrapped to 정보
        m.on_key(VK_DOWN, NONE);
        assert_eq!(m.selected, Some(0));
        m.on_key(VK_END, NONE);
        assert_eq!(m.selected, Some(5));
        m.on_key(VK_HOME, NONE);
        assert_eq!(m.selected, Some(0));
        // Without a photo, 폴더에서 보기 and 경로 복사 are skipped.
        let mut m = Menu::default();
        m.open((0.0, 0.0), false, true);
        assert!(!m.enabled(1) && !m.enabled(2) && m.enabled(0) && m.enabled(3));
        m.on_key(VK_DOWN, NONE);
        assert_eq!(m.selected, Some(3));
        m.on_key(VK_UP, NONE);
        assert_eq!(m.selected, Some(0));
        // From no selection: ↓ the first item, ↑ the last.
        let mut m = Menu::default();
        m.open((0.0, 0.0), true, false);
        m.on_key(VK_DOWN, NONE);
        assert_eq!(m.selected, Some(0));
        m.open((0.0, 0.0), true, false);
        m.on_key(VK_UP, NONE);
        assert_eq!(m.selected, Some(5));
    }

    #[test]
    fn enter_picks_esc_closes_other_keys_are_swallowed() {
        let mut m = Menu::default();
        m.open((0.0, 0.0), true, true);
        m.on_key(VK_DOWN, NONE);
        m.on_key(VK_DOWN, NONE);
        assert_eq!(
            m.on_key(VK_RETURN, NONE),
            Some(MenuAction::Activate(MenuItem::CopyPath))
        );
        assert!(!m.is_open());
        assert_eq!(m.on_key(VK_RETURN, NONE), None); // closed: not taken
        m.open((0.0, 0.0), true, true);
        assert_eq!(
            m.on_key(VK_SPACE, NONE),
            Some(MenuAction::Activate(MenuItem::Open))
        );
        m.open((0.0, 0.0), true, true);
        assert_eq!(m.on_key(VK_ESCAPE, NONE), Some(MenuAction::Redraw));
        assert!(!m.is_open());
        // Enter with nothing selected does nothing; letters and ←/→ are swallowed.
        m.open((0.0, 0.0), true, false);
        assert_eq!(m.on_key(VK_RETURN, NONE), Some(MenuAction::Nothing));
        assert_eq!(m.on_key(0x25, NONE), Some(MenuAction::Nothing));
        assert_eq!(m.on_key(0x49, NONE), Some(MenuAction::Nothing));
        assert!(m.is_open());
        // Alt combinations pass (Alt+F4); the menu key toggles it closed.
        assert_eq!(m.on_key(0x73, ALT), None);
        assert_eq!(m.on_key(VK_APPS, NONE), Some(MenuAction::Redraw));
        assert!(!m.is_open());
    }

    #[test]
    fn the_menu_key_that_closed_it_does_not_reopen_it() {
        // WM_KEYDOWN VK_APPS (opens), then DefWindowProc's keyboard WM_CONTEXTMENU.
        let mut m = Menu::default();
        m.open(KEYBOARD_ANCHOR, true, true);
        assert!(!m.keyboard_context()); // already open: ignored
        assert!(m.is_open());
        // VK_APPS again closes it; the WM_CONTEXTMENU after its key-up is swallowed.
        assert_eq!(m.on_key(VK_APPS, NONE), Some(MenuAction::Redraw));
        assert!(!m.keyboard_context());
        assert!(!m.is_open());
        // A later keyboard WM_CONTEXTMENU (without the key handler) opens it.
        assert!(m.keyboard_context());
        // Shift+F10 closes it the same way; another key in between ends that state.
        m.open(KEYBOARD_ANCHOR, true, true);
        m.on_key(VK_F10, SHIFT);
        assert!(m.closed_by_key);
        m.other_input(false);
        assert!(m.keyboard_context());
        // Esc closing it is not the menu key: the next one opens it.
        m.open(KEYBOARD_ANCHOR, true, true);
        m.on_key(VK_ESCAPE, NONE);
        assert!(m.keyboard_context());
    }

    #[test]
    fn the_double_click_of_a_closing_click_is_swallowed() {
        let mut m = Menu::default();
        m.open((100.0, 100.0), true, false);
        assert!(m.press(900.0, 600.0, VIEW)); // outside: closes
        assert!(m.other_input(true)); // its WM_LBUTTONDBLCLK: swallowed
        assert!(!m.other_input(true)); // only once
                                       // A plain click in between ends it.
        m.open((100.0, 100.0), true, false);
        m.press(900.0, 600.0, VIEW);
        assert!(!m.other_input(false));
        assert!(!m.other_input(true));
        // Closing with a key: a later double-click is the photo's.
        m.open((100.0, 100.0), true, false);
        m.on_key(VK_ESCAPE, NONE);
        assert!(!m.other_input(true));
    }

    #[test]
    fn clicks_pick_items_and_outside_closes() {
        let mut m = Menu::default();
        assert!(!m.press(10.0, 10.0, VIEW)); // closed
        assert_eq!(m.release(10.0, 10.0, VIEW), None);
        m.open((100.0, 100.0), true, false);
        let l = m.layout(VIEW).unwrap();
        // Hover selects.
        let (x, y) = center(l.items[4]);
        assert!(m.hover(x, y, VIEW));
        assert_eq!(m.selected, Some(4));
        assert!(!m.hover(x, y, VIEW));
        // Pressed on one item, released on another: nothing.
        let (ax, ay) = center(l.items[0]);
        assert!(m.press(ax, ay, VIEW));
        assert_eq!(m.release(x, y, VIEW), Some(MenuAction::Nothing));
        assert!(m.is_open());
        assert!(m.press(x, y, VIEW));
        assert_eq!(
            m.release(x, y, VIEW),
            Some(MenuAction::Activate(MenuItem::Settings))
        );
        assert!(!m.is_open());
        // Outside: taken and closed.
        m.open((100.0, 100.0), true, false);
        assert!(m.press(900.0, 600.0, VIEW));
        assert!(!m.is_open());
        assert_eq!(m.release(900.0, 600.0, VIEW), None);
        // Unavailable items do nothing.
        m.open((100.0, 100.0), false, false);
        let (cx, cy) = center(l.items[2]);
        assert!(!m.hover(cx, cy, VIEW));
        assert!(m.press(cx, cy, VIEW));
        assert_eq!(m.release(cx, cy, VIEW), Some(MenuAction::Nothing));
        assert!(m.is_open());
    }
}
