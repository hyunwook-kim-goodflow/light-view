//! UI Automation tree (spec §4.8, Task 6): what a screen reader sees of the window's
//! in-window controls, as plain data. The tree is rebuilt on the UI thread from the
//! window's state (`main_tree`, `settings_tree`); `uia_provider` hands snapshots of it
//! to UI Automation, whose calls arrive on other threads, and posts invocations back.
//!
//! Rects are client pixels (physical); the provider adds the client origin on screen.

use lv_core::settings::Settings;

use crate::exif_card::{self, Line, Row};
use crate::menu::{MenuLayout, ITEMS};
use crate::overlay::{dialog_layout, Dialog, Item, Toast, ToastParts};
use crate::settings_win::{self, Control, CONTROLS};
use crate::start::StartLayout;
use crate::strings::{t, text, Lang, S};
use crate::toolbar::{self, pinned_bar_height, Rect, ToolButton, ToolbarInput};

mod uia_glue;

pub use uia_glue::{main_tree_of, run_main};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Button,
    CheckBox,
    Group,
    Image,
    List,
    ListItem,
    Menu,
    MenuItem,
    RadioButton,
    Text,
    ToolBar,
}

/// What invoking (or toggling, or selecting) an element does, on the UI thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Tool(ToolButton),
    /// The start screen's [열기]: the same as Ctrl+O.
    Open,
    Card(exif_card::Target),
    /// Index into `menu::ITEMS`.
    Menu(usize),
    /// Index into the open dialog's `prompt.items()`.
    Dialog(usize),
    Undo,
    /// Settings window: (control index into `CONTROLS`, option index) of a choice.
    Pick(usize, usize),
    /// Settings window: the checkbox or the button (control index).
    Press(usize),
}

/// A rectangle in client pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl PxRect {
    /// `r` (DIPs) at `dip` DIPs per pixel, edges rounded.
    pub fn from_dips(r: Rect, dip: f32) -> PxRect {
        let px = |v: f32| (v / dip).round() as i32;
        let (x, y) = (px(r.x), px(r.y));
        PxRect {
            x,
            y,
            w: px(r.x + r.w) - x,
            h: px(r.y + r.h) - y,
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// The same rect on screen, for a client area whose origin is at `origin`.
    pub fn on_screen(&self, origin: (i32, i32)) -> PxRect {
        PxRect {
            x: self.x + origin.0,
            y: self.y + origin.1,
            ..*self
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Stable for the same control across rebuilds (the runtime id's last part).
    pub id: u32,
    /// Index of the parent node; `None` for the window's direct children.
    pub parent: Option<usize>,
    pub role: Role,
    pub name: String,
    pub rect: PxRect,
    pub enabled: bool,
    pub focusable: bool,
    /// Has the keyboard focus (at most one node).
    pub focused: bool,
    /// Toggle pattern state (pin, checkboxes).
    pub toggle: Option<bool>,
    /// SelectionItem pattern state (list items, settings options).
    pub selected: Option<bool>,
    /// Not drawn now (the auto-hidden toolbar).
    pub offscreen: bool,
    pub act: Option<Act>,
}

/// One window's elements, parents before their children, in reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// The toast to announce: (identity, text). A new identity is a new toast.
    pub announce: Option<(u64, String)>,
}

impl Tree {
    pub fn index(&self, id: u32) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    fn children(&self, parent: Option<usize>) -> impl DoubleEndedIterator<Item = usize> + '_ {
        (0..self.nodes.len()).filter(move |&i| self.nodes[i].parent == parent)
    }

    /// The first child of `parent` (`None`: of the window).
    pub fn first_child(&self, parent: Option<usize>) -> Option<usize> {
        self.children(parent).next()
    }

    pub fn last_child(&self, parent: Option<usize>) -> Option<usize> {
        self.children(parent).next_back()
    }

    pub fn next_sibling(&self, i: usize) -> Option<usize> {
        let parent = self.nodes.get(i)?.parent;
        self.children(parent).find(|&c| c > i)
    }

    pub fn prev_sibling(&self, i: usize) -> Option<usize> {
        let parent = self.nodes.get(i)?.parent;
        self.children(parent).rev().find(|&c| c < i)
    }

    pub fn focused(&self) -> Option<usize> {
        self.nodes.iter().position(|n| n.focused)
    }

    /// The topmost, deepest element at client pixel (x, y): later nodes are drawn over
    /// earlier ones and children over their parents.
    pub fn node_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.nodes.len())
            .rev()
            .find(|&i| !self.nodes[i].offscreen && self.nodes[i].rect.contains(x, y))
    }
}

/// What to tell UI Automation after a rebuild: the element that newly has the focus,
/// and a toast that newly appeared.
#[derive(Debug, Default)]
pub struct Announcer {
    focus: Option<u32>,
    toast: Option<u64>,
}

impl Announcer {
    /// Starts from `tree` without announcing what it already shows.
    pub fn primed(tree: &Tree) -> Announcer {
        Announcer {
            focus: tree.focused().map(|i| tree.nodes[i].id),
            toast: tree.announce.as_ref().map(|a| a.0),
        }
    }

    /// (focus moved to this element, toast text to announce).
    pub fn update(&mut self, tree: &Tree) -> (Option<u32>, Option<String>) {
        let focus = tree.focused().map(|i| tree.nodes[i].id);
        let moved = (focus != self.focus).then_some(focus).flatten();
        self.focus = focus;
        let toast = tree
            .announce
            .as_ref()
            .filter(|a| Some(a.0) != self.toast)
            .map(|a| a.1.clone());
        self.toast = tree.announce.as_ref().map(|a| a.0);
        (moved, toast)
    }
}

// Element ids. Each part of the window has its own range.
const PHOTO: u32 = 1;
const START_HINT: u32 = 2;
const START_OPEN: u32 = 3;
const TOOLBAR: u32 = 10;
const TOOL_FIRST: u32 = 11;
const CARD: u32 = 40;
const CARD_CLOSE: u32 = 41;
const CARD_HEADING: u32 = 42;
const CARD_EXPAND: u32 = 43;
const CARD_LIST: u32 = 44;
const CARD_COPY: u32 = 45;
const CARD_LINE_FIRST: u32 = 50;
const MENU: u32 = 100;
const MENU_ITEM_FIRST: u32 = 101;
const DIALOG: u32 = 200;
const DIALOG_TITLE: u32 = 201;
const DIALOG_DETAIL_FIRST: u32 = 210;
const DIALOG_ITEM_FIRST: u32 = 220;
const TOAST: u32 = 300;
const TOAST_UNDO: u32 = 301;
const SETTINGS_FIRST: u32 = 1000;
const SETTINGS_STRIDE: u32 = 20;
const SETTINGS_NOTE: u32 = 2000;
const SETTINGS_OPEN_NOTE: u32 = 2001;
const SETTINGS_VERSION: u32 = 2002;
const LIST_ROW_FIRST: u32 = 10_000;

/// The shown photo: its file name and (1-based position, count) in the folder.
pub struct PhotoInput<'a> {
    pub name: &'a str,
    pub position: (usize, usize),
}

pub struct CardInput<'a> {
    pub model: &'a exif_card::Model,
    /// The selected list row and the first row drawn (`ExifCard::selected`, `scroll`).
    pub selected: Option<usize>,
    pub scroll: usize,
}

pub struct MenuInput {
    pub layout: MenuLayout,
    pub selected: Option<usize>,
    /// Per `ITEMS` entry.
    pub enabled: [bool; ITEMS.len()],
}

pub struct ToastInput<'a> {
    pub toast: &'a Toast,
    /// Where it was drawn (`None` before its first paint).
    pub parts: Option<ToastParts>,
}

/// The main window's state, as the tree needs it.
pub struct MainInput<'a> {
    /// DIPs per pixel.
    pub dip: f32,
    /// Client size in DIPs.
    pub client: (f32, f32),
    pub photo: Option<PhotoInput<'a>>,
    /// The toolbar's input and whether it is drawn now.
    pub toolbar: Option<(ToolbarInput, bool)>,
    pub start: Option<StartLayout>,
    pub card: Option<CardInput<'a>>,
    pub menu: Option<MenuInput>,
    pub dialog: Option<&'a Dialog>,
    pub toast: Option<ToastInput<'a>>,
}

/// Collects nodes with their rects converted to pixels.
struct Builder {
    nodes: Vec<Node>,
    dip: f32,
}

impl Builder {
    fn add(&mut self, parent: Option<usize>, id: u32, role: Role, name: &str, r: Rect) -> usize {
        self.nodes.push(Node {
            id,
            parent,
            role,
            name: name.to_owned(),
            rect: PxRect::from_dips(r, self.dip),
            enabled: true,
            focusable: false,
            focused: false,
            toggle: None,
            selected: None,
            offscreen: false,
            act: None,
        });
        self.nodes.len() - 1
    }

    fn last(&mut self) -> &mut Node {
        self.nodes.last_mut().expect("a node was added")
    }
}

/// The main window's tree. An open dialog is modal and so is the open menu: then only
/// it (and a toast) is exposed. Otherwise: the photo (or the start screen), the EXIF
/// card, the toolbar and the toast. The keyboard focus is the dialog's focused item,
/// the menu's selected item, or the selected row of the card's full list.
pub fn main_tree(i: &MainInput) -> Tree {
    let mut b = Builder {
        nodes: Vec::new(),
        dip: i.dip,
    };
    let full = Rect {
        x: 0.0,
        y: 0.0,
        w: i.client.0,
        h: i.client.1,
    };
    if let Some(d) = i.dialog {
        add_dialog(&mut b, d, i.client);
    } else if let Some(m) = &i.menu {
        add_menu(&mut b, m);
    } else {
        if let Some(p) = &i.photo {
            let bar = match i.toolbar {
                Some((t, _)) if t.pinned => pinned_bar_height(),
                _ => 0.0,
            };
            let view = Rect {
                h: (full.h - bar).max(0.0),
                ..full
            };
            b.add(None, PHOTO, Role::Image, &photo_name(p), view);
        }
        if let Some(l) = &i.start {
            b.add(None, START_HINT, Role::Text, t(S::StartHint), l.text);
            b.add(None, START_OPEN, Role::Button, t(S::BtnOpen), l.button);
            b.last().act = Some(Act::Open);
        }
        if let Some(c) = &i.card {
            add_card(&mut b, c);
        }
        if let Some((input, shown)) = i.toolbar {
            add_toolbar(&mut b, input, shown, i.photo.as_ref().map(|p| p.position));
        }
    }
    let announce = i.toast.as_ref().map(|ti| {
        add_toast(&mut b, ti, full);
        (ti.toast.shown_at_ms, ti.toast.text.clone())
    });
    Tree {
        nodes: b.nodes,
        announce,
    }
}

/// "a.jpg · 3 / 12".
fn photo_name(p: &PhotoInput) -> String {
    let (i, n) = p.position;
    if n == 0 {
        p.name.to_owned()
    } else {
        format!("{} · {i} / {n}", p.name)
    }
}

fn add_toolbar(
    b: &mut Builder,
    input: ToolbarInput,
    shown: bool,
    position: Option<(usize, usize)>,
) {
    let bar = b.add(
        None,
        TOOLBAR,
        Role::ToolBar,
        t(S::UiaToolbar),
        toolbar::bar_rect(&input),
    );
    b.last().offscreen = !shown;
    for (button, r) in toolbar::layout(&input) {
        let id = TOOL_FIRST + button as u32;
        if button == ToolButton::Counter {
            let (pos, n) = position.unwrap_or((0, 0));
            b.add(Some(bar), id, Role::Text, &format!("{pos} / {n}"), r);
        } else {
            let name = button.tooltip().unwrap_or_default();
            b.add(Some(bar), id, Role::Button, name, r);
            let node = b.last();
            node.enabled = button.enabled();
            node.act = Some(Act::Tool(button));
            if button == ToolButton::Pin {
                node.toggle = Some(input.pinned);
            }
        }
        b.last().offscreen = !shown;
    }
}

fn add_menu(b: &mut Builder, m: &MenuInput) {
    let menu = b.add(None, MENU, Role::Menu, t(S::UiaMenu), m.layout.card);
    for (k, (item, r)) in ITEMS.iter().zip(&m.layout.items).enumerate() {
        b.add(
            Some(menu),
            MENU_ITEM_FIRST + k as u32,
            Role::MenuItem,
            item.label(),
            *r,
        );
        let node = b.last();
        node.enabled = m.enabled[k];
        node.focusable = m.enabled[k];
        node.focused = m.selected == Some(k);
        node.act = Some(Act::Menu(k));
    }
}

fn add_dialog(b: &mut Builder, d: &Dialog, client: (f32, f32)) {
    let l = dialog_layout(client.0, client.1, d);
    let title = d.prompt.title();
    let dialog = b.add(None, DIALOG, Role::Group, title, l.card);
    b.add(Some(dialog), DIALOG_TITLE, Role::Text, title, l.title);
    for (k, (line, r)) in d.prompt.details().iter().zip(&l.details).enumerate() {
        b.add(
            Some(dialog),
            DIALOG_DETAIL_FIRST + k as u32,
            Role::Text,
            line,
            *r,
        );
    }
    for (k, (item, r)) in d.prompt.items().iter().zip(&l.items).enumerate() {
        let id = DIALOG_ITEM_FIRST + k as u32;
        match item {
            Item::Checkbox => {
                b.add(Some(dialog), id, Role::CheckBox, t(S::CheckboxAutoSave), *r);
                b.last().toggle = Some(d.dont_ask);
            }
            Item::Button(result) => {
                b.add(Some(dialog), id, Role::Button, result.label(), *r);
            }
        }
        let node = b.last();
        node.focusable = true;
        node.focused = d.focused == k;
        node.act = Some(Act::Dialog(k));
    }
}

fn add_toast(b: &mut Builder, ti: &ToastInput, full: Rect) {
    let anchor = crate::overlay::toast_layout(full.w, full.h, false);
    let bg = ti.parts.map_or(anchor, |p| p.bg);
    let toast = b.add(None, TOAST, Role::Text, &ti.toast.text, bg);
    if let Some((label, _)) = &ti.toast.action {
        let r = ti.parts.and_then(|p| p.action).unwrap_or(bg);
        b.add(Some(toast), TOAST_UNDO, Role::Button, label, r);
        b.last().act = Some(Act::Undo);
    }
}

fn add_card(b: &mut Builder, c: &CardInput) {
    use exif_card::Target;
    let m = c.model;
    let l = &m.layout;
    let card = b.add(None, CARD, Role::Group, t(S::UiaPhotoInfo), l.card);
    let close = if m.expanded { S::UiaBack } else { S::BtnClose };
    b.add(Some(card), CARD_CLOSE, Role::Button, t(close), l.close);
    b.last().act = Some(Act::Card(Target::Close));
    b.add(
        Some(card),
        CARD_HEADING,
        Role::Text,
        &m.text.heading,
        l.heading,
    );
    for (k, (line, r)) in m.text.lines.iter().zip(&l.lines).enumerate() {
        let (Line::Plain(s) | Line::Exposure(s)) = line;
        b.add(Some(card), CARD_LINE_FIRST + k as u32, Role::Text, s, *r);
    }
    if let Some(r) = l.expand {
        b.add(Some(card), CARD_EXPAND, Role::Button, t(S::ExifShowAll), r);
        b.last().act = Some(Act::Card(Target::Expand));
    }
    if let (Some(list_rect), Some(exif)) = (l.list, &m.exif) {
        let list = b.add(
            Some(card),
            CARD_LIST,
            Role::List,
            t(S::UiaExifList),
            list_rect,
        );
        for (row, r) in exif_card::visible_rows(&m.rows, c.scroll, list_rect) {
            let id = LIST_ROW_FIRST + row as u32;
            match m.rows[row] {
                Row::Group(e) => {
                    let group = exif.entries.get(e).map_or("", |e| e.group.as_str());
                    b.add(Some(list), id, Role::Text, group, r);
                }
                Row::Entry(e) => {
                    let Some(entry) = exif.entries.get(e) else {
                        continue;
                    };
                    let name = format!("{}: {}", entry.name, entry.value);
                    b.add(Some(list), id, Role::ListItem, &name, r);
                    let node = b.last();
                    let selected = c.selected == Some(row);
                    node.focusable = true;
                    node.focused = selected;
                    node.selected = Some(selected);
                    node.act = Some(Act::Card(Target::Row(row)));
                }
            }
        }
    }
    if let Some(r) = l.copy {
        b.add(Some(card), CARD_COPY, Role::Button, t(S::ExifCopy), r);
        b.last().act = Some(Act::Card(Target::Copy));
    }
}

/// The settings window's tree (its own root): each choice is a group of radio buttons
/// named by its label, then the checkbox, the button, the notes and the version. The
/// focused choice's chosen option has the keyboard focus (the group itself when no
/// option is chosen).
pub fn settings_tree(focused: usize, s: &Settings, lang: Lang, dip: f32) -> Tree {
    let mut b = Builder {
        nodes: Vec::new(),
        dip,
    };
    let l = settings_win::layout(settings_win::SIZE_DIP);
    for (c, (control, rects)) in CONTROLS.iter().zip(&l.controls).enumerate() {
        let id = SETTINGS_FIRST + c as u32 * SETTINGS_STRIDE;
        let label = text(lang, control.label_key());
        let has_focus = focused == c;
        let options = control.option_labels(lang);
        if options.is_empty() {
            let role = if matches!(control, Control::PinMenu | Control::UpdateCheck) {
                Role::CheckBox
            } else {
                Role::Button
            };
            b.add(None, id, role, label, rects.focus);
            let node = b.last();
            node.focusable = true;
            node.focused = has_focus;
            node.act = Some(Act::Press(c));
            if role == Role::CheckBox {
                node.toggle = Some(control.checked(s));
            }
            continue;
        }
        let chosen = control.selected(s);
        let group = b.add(None, id, Role::Group, label, rects.focus);
        b.last().focused = has_focus && chosen.is_none();
        for (o, (name, r)) in options.iter().zip(&rects.options).enumerate() {
            b.add(Some(group), id + 1 + o as u32, Role::RadioButton, name, *r);
            let node = b.last();
            node.focusable = true;
            node.focused = has_focus && chosen == Some(o);
            node.selected = Some(chosen == Some(o));
            node.act = Some(Act::Pick(c, o));
        }
    }
    b.add(
        None,
        SETTINGS_OPEN_NOTE,
        Role::Text,
        text(lang, S::SettingsOpenOtherNote),
        l.open_note,
    );
    b.add(
        None,
        SETTINGS_NOTE,
        Role::Text,
        text(lang, S::SettingsMemoryNote),
        l.note,
    );
    let version = format!(
        "{}{}",
        text(lang, S::AboutVersion),
        env!("CARGO_PKG_VERSION")
    );
    b.add(None, SETTINGS_VERSION, Role::Text, &version, l.version);
    Tree {
        nodes: b.nodes,
        announce: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exif_card::{CardInfo, ExifCard};
    use crate::menu::{menu_layout, MenuItem};
    use crate::overlay::{Prompt, PromptResult};
    use crate::start::start_layout;
    use lv_core::exif_info::{ExifData, ExifEntry, ExifSummary};
    use std::sync::Arc;

    const CLIENT: (f32, f32) = (1000.0, 700.0);

    fn input<'a>() -> MainInput<'a> {
        MainInput {
            dip: 0.5, // 200 % scaling
            client: CLIENT,
            photo: None,
            toolbar: None,
            start: None,
            card: None,
            menu: None,
            dialog: None,
            toast: None,
        }
    }

    fn by_role(tree: &Tree, role: Role) -> Vec<&Node> {
        tree.nodes.iter().filter(|n| n.role == role).collect()
    }

    fn named<'a>(tree: &'a Tree, name: &str) -> &'a Node {
        tree.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name:?} in {tree:#?}"))
    }

    fn toolbar_input(pinned: bool) -> ToolbarInput {
        ToolbarInput {
            view_w: CLIENT.0,
            view_h: CLIENT.1,
            pinned,
            unsaved_rotation: true,
        }
    }

    #[test]
    fn uia_tree_names_controls() {
        // Toolbar: buttons named by their tooltips, the pin as a toggle, the counter
        // as text; the photo as an image named "file · position".
        let mut i = input();
        i.photo = Some(PhotoInput {
            name: "a.jpg",
            position: (3, 12),
        });
        i.toolbar = Some((toolbar_input(true), true));
        let tree = main_tree(&i);
        let photo = named(&tree, "a.jpg · 3 / 12");
        assert_eq!(photo.role, Role::Image);
        // The view above the pinned bar, in pixels at 200 %.
        let view_h = ((CLIENT.1 - pinned_bar_height()) * 2.0) as i32;
        assert_eq!(
            photo.rect,
            PxRect {
                x: 0,
                y: 0,
                w: 2000,
                h: view_h
            }
        );
        let bar = by_role(&tree, Role::ToolBar);
        assert_eq!(bar.len(), 1);
        assert_eq!(bar[0].name, t(S::UiaToolbar));
        let buttons = by_role(&tree, Role::Button);
        let names: Vec<&str> = buttons.iter().map(|n| n.name.as_str()).collect();
        let want: Vec<&str> = [
            ToolButton::Prev,
            ToolButton::Next,
            ToolButton::ZoomOut,
            ToolButton::ZoomIn,
            ToolButton::RotateLeft,
            ToolButton::RotateRight,
            ToolButton::Save,
            ToolButton::Exif,
            ToolButton::Delete,
            ToolButton::Pin,
        ]
        .iter()
        .map(|b| b.tooltip().unwrap())
        .collect();
        assert_eq!(names, want);
        let bar_index = tree.index(TOOLBAR).unwrap();
        for n in &buttons {
            assert_eq!(n.parent, Some(bar_index));
            assert!(n.enabled && !n.offscreen && !n.focused);
            assert!(matches!(n.act, Some(Act::Tool(_))));
        }
        let pin = named(&tree, ToolButton::Pin.tooltip().unwrap());
        assert_eq!(pin.toggle, Some(true));
        assert_eq!(pin.act, Some(Act::Tool(ToolButton::Pin)));
        assert_eq!(named(&tree, "3 / 12").role, Role::Text);
        // Rects: the button's DIP rect doubled.
        let (_, r) = toolbar::layout(&toolbar_input(true))
            .into_iter()
            .find(|(b, _)| *b == ToolButton::Prev)
            .unwrap();
        let prev = named(&tree, ToolButton::Prev.tooltip().unwrap());
        assert_eq!(
            prev.rect,
            PxRect {
                x: (r.x * 2.0).round() as i32,
                y: (r.y * 2.0).round() as i32,
                w: 88,
                h: 88
            }
        );
        assert_eq!(tree.focused(), None);

        // Auto-hidden: still there for a screen reader, but offscreen.
        i.toolbar = Some((toolbar_input(false), false));
        let tree = main_tree(&i);
        assert!(tree
            .nodes
            .iter()
            .filter(|n| n.parent == Some(tree.index(TOOLBAR).unwrap()))
            .all(|n| n.offscreen));
        assert_eq!(
            named(&tree, ToolButton::Pin.tooltip().unwrap()).toggle,
            Some(false)
        );
    }

    #[test]
    fn start_screen_open_is_an_invokable_button() {
        let mut i = input();
        i.start = Some(start_layout(CLIENT));
        let tree = main_tree(&i);
        let open = named(&tree, t(S::BtnOpen));
        assert_eq!(open.role, Role::Button);
        assert_eq!(open.act, Some(Act::Open));
        assert_eq!(named(&tree, t(S::StartHint)).role, Role::Text);
        assert!(by_role(&tree, Role::Image).is_empty());
    }

    #[test]
    fn open_menu_items_with_state_and_focus() {
        let mut i = input();
        i.toolbar = Some((toolbar_input(false), true));
        i.menu = Some(MenuInput {
            layout: menu_layout((100.0, 100.0), CLIENT),
            selected: Some(3),
            enabled: [true, false, false, true, true, true],
        });
        let tree = main_tree(&i);
        // The menu is modal: the toolbar is not exposed while it is open.
        assert!(by_role(&tree, Role::ToolBar).is_empty());
        let items = by_role(&tree, Role::MenuItem);
        let names: Vec<&str> = items.iter().map(|n| n.name.as_str()).collect();
        let want: Vec<&str> = ITEMS.iter().map(|m| m.label()).collect();
        assert_eq!(names, want);
        assert!(!named(&tree, MenuItem::ShowInFolder.label()).enabled);
        assert!(!named(&tree, MenuItem::CopyPath.label()).enabled);
        let focused = &tree.nodes[tree.focused().unwrap()];
        assert_eq!(focused.name, MenuItem::DefaultApps.label());
        assert_eq!(focused.act, Some(Act::Menu(3)));
        assert_eq!(by_role(&tree, Role::Menu)[0].name, t(S::UiaMenu));
    }

    #[test]
    fn dialog_buttons_and_checkbox() {
        let mut i = input();
        i.photo = Some(PhotoInput {
            name: "a.jpg",
            position: (1, 1),
        });
        let mut d = Dialog::new(Prompt::SaveRotation);
        d.dont_ask = true;
        i.dialog = Some(&d);
        let tree = main_tree(&i);
        assert!(by_role(&tree, Role::Image).is_empty(), "modal");
        let check = named(&tree, t(S::CheckboxAutoSave));
        assert_eq!(check.role, Role::CheckBox);
        assert_eq!(check.toggle, Some(true));
        let names: Vec<&str> = by_role(&tree, Role::Button)
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                PromptResult::Save.label(),
                PromptResult::Discard.label(),
                PromptResult::Cancel.label()
            ]
        );
        // Focus starts on the default button.
        let focused = &tree.nodes[tree.focused().unwrap()];
        assert_eq!(focused.name, PromptResult::Save.label());
        assert_eq!(focused.act, Some(Act::Dialog(1)));
        let group = named(&tree, t(S::PromptSaveRotation));
        assert_eq!(group.role, Role::Group);
    }

    #[test]
    fn toast_is_text_with_its_undo_button_and_is_announced() {
        let mut i = input();
        let toast = Toast::recycled(1234);
        i.toast = Some(ToastInput {
            toast: &toast,
            parts: None,
        });
        let tree = main_tree(&i);
        let text_node = named(&tree, t(S::ToastRecycled));
        assert_eq!(text_node.role, Role::Text);
        let undo = named(&tree, t(S::ToastUndo));
        assert_eq!(undo.role, Role::Button);
        assert_eq!(undo.act, Some(Act::Undo));
        assert_eq!(undo.parent, tree.index(TOAST));
        assert_eq!(tree.announce, Some((1234, t(S::ToastRecycled).to_owned())));
    }

    #[test]
    fn exif_list_items_with_selection() {
        let entries = vec![
            ExifEntry {
                group: "IFD0".into(),
                name: "Make".into(),
                value: "Canon".into(),
            },
            ExifEntry {
                group: "IFD0".into(),
                name: "Model".into(),
                value: "EOS R5".into(),
            },
        ];
        let exif = Arc::new(ExifData {
            summary: ExifSummary::default(),
            entries,
        });
        let mut card = ExifCard::default();
        card.toggle();
        let info = CardInfo {
            exif: Some(exif),
            file: Some((10, 10, 100)),
            modified: None,
            scaled_from: None,
        };
        // Open the full list (Enter).
        let m = card.model(info.clone(), CLIENT, 600.0, Lang::Ko);
        card.on_key(
            lv_core::command::VK_RETURN,
            lv_core::command::Mods::default(),
            &m,
        );
        let m = card.model(info, CLIENT, 600.0, Lang::Ko);
        assert!(m.expanded);
        let mut i = input();
        i.card = Some(CardInput {
            model: &m,
            selected: card.selected(&m),
            scroll: card.scroll(&m),
        });
        let tree = main_tree(&i);
        let list = by_role(&tree, Role::List);
        assert_eq!(list.len(), 1);
        let items = by_role(&tree, Role::ListItem);
        let names: Vec<&str> = items.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Make: Canon", "Model: EOS R5"]);
        assert_eq!(items[0].selected, Some(true));
        assert_eq!(items[1].selected, Some(false));
        assert!(items[0].focused);
        assert_eq!(items[1].act, Some(Act::Card(exif_card::Target::Row(2))));
        assert_eq!(named(&tree, "IFD0").role, Role::Text);
        assert_eq!(named(&tree, t(S::UiaBack)).role, Role::Button);
        assert_eq!(named(&tree, t(S::ExifCopy)).role, Role::Button);
        assert_eq!(by_role(&tree, Role::Group)[0].name, t(S::UiaPhotoInfo));
    }

    #[test]
    fn settings_controls_in_their_own_tree() {
        let s = Settings {
            toolbar_pinned: true,
            ..Settings::default()
        };
        // Focus on the checkbox (control 1).
        let tree = settings_tree(1, &s, Lang::Ko, 1.0);
        let lang = named(&tree, text(Lang::Ko, S::SettingsLanguage));
        assert_eq!(lang.role, Role::Group);
        let radios: Vec<&Node> = tree
            .nodes
            .iter()
            .filter(|n| n.parent == tree.index(lang.id))
            .collect();
        let names: Vec<&str> = radios.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["자동", "한국어", "English"]);
        assert!(radios.iter().all(|n| n.role == Role::RadioButton));
        assert_eq!(radios[0].selected, Some(true));
        assert_eq!(radios[2].act, Some(Act::Pick(0, 2)));
        let pin = named(&tree, text(Lang::Ko, S::SettingsPinMenu));
        assert_eq!(pin.role, Role::CheckBox);
        assert_eq!(pin.toggle, Some(true));
        assert!(pin.focused);
        assert_eq!(tree.nodes.iter().filter(|n| n.focused).count(), 1);
        let button = named(&tree, text(Lang::Ko, S::MenuDefaultApps));
        assert_eq!(button.role, Role::Button);
        assert_eq!(button.act, Some(Act::Press(6)));
        // Update checks: a checkbox too, off until the user said yes.
        let update = named(&tree, text(Lang::Ko, S::SettingsUpdateCheck));
        assert_eq!(update.role, Role::CheckBox);
        assert_eq!(update.toggle, Some(false));
        assert_eq!(update.act, Some(Act::Press(5)));
        // Focus on the language choice: its chosen option has it.
        let tree = settings_tree(0, &s, Lang::Ko, 1.0);
        assert_eq!(tree.nodes[tree.focused().unwrap()].name, "자동");
        // A memory limit that is not offered: the group has it.
        let odd = Settings {
            memory_budget_mb: Some(300),
            ..Settings::default()
        };
        let tree = settings_tree(4, &odd, Lang::En, 1.0);
        let focused = &tree.nodes[tree.focused().unwrap()];
        assert_eq!(focused.role, Role::Group);
        assert_eq!(focused.name, "Memory limit");
    }

    #[test]
    fn navigation_follows_the_tree_order() {
        let mut i = input();
        let d = Dialog::new(Prompt::About);
        i.dialog = Some(&d);
        let tree = main_tree(&i);
        let dialog = tree.index(DIALOG).unwrap();
        assert_eq!(tree.first_child(None), Some(dialog));
        assert_eq!(tree.last_child(None), Some(dialog));
        let first = tree.first_child(Some(dialog)).unwrap();
        assert_eq!(tree.nodes[first].id, DIALOG_TITLE);
        let second = tree.next_sibling(first).unwrap();
        assert_eq!(tree.prev_sibling(second), Some(first));
        assert_eq!(tree.prev_sibling(first), None);
        let last = tree.last_child(Some(dialog)).unwrap();
        assert_eq!(tree.nodes[last].name, PromptResult::Close.label());
        assert_eq!(tree.next_sibling(last), None);
        assert_eq!(tree.nodes[last].parent, Some(dialog));
    }

    #[test]
    fn focus_and_toast_events_fire_on_change() {
        let mut i = input();
        let mut d = Dialog::new(Prompt::About);
        i.dialog = Some(&d);
        let tree = main_tree(&i);
        let mut a = Announcer::primed(&tree);
        assert_eq!(a.update(&tree), (None, None), "nothing new");
        d.focused = 0;
        let mut i = input();
        i.dialog = Some(&d);
        let tree = main_tree(&i);
        assert_eq!(a.update(&tree), (Some(DIALOG_ITEM_FIRST), None));
        assert_eq!(a.update(&tree), (None, None));
        let toast = Toast::new(S::ToastCopied, 50);
        i.toast = Some(ToastInput {
            toast: &toast,
            parts: None,
        });
        let tree = main_tree(&i);
        assert_eq!(a.update(&tree), (None, Some(t(S::ToastCopied).to_owned())));
        assert_eq!(a.update(&tree), (None, None), "announced once");
        // The same text again is a new toast.
        let again = Toast::new(S::ToastCopied, 900);
        i.toast = Some(ToastInput {
            toast: &again,
            parts: None,
        });
        let tree = main_tree(&i);
        assert_eq!(a.update(&tree).1, Some(t(S::ToastCopied).to_owned()));
        // Focus leaving every element is not an event.
        let tree = main_tree(&input());
        assert_eq!(a.update(&tree), (None, None));
    }

    #[test]
    fn hit_test_and_screen_rects() {
        let mut i = input();
        i.dip = 1.0;
        let l = start_layout(CLIENT);
        i.start = Some(l);
        let tree = main_tree(&i);
        let (cx, cy) = ((l.button.x + 10.0) as i32, (l.button.y + 10.0) as i32);
        let hit = tree.node_at(cx, cy).unwrap();
        assert_eq!(tree.nodes[hit].id, START_OPEN);
        assert_eq!(tree.node_at(-5, -5), None);
        let r = PxRect {
            x: 10,
            y: 20,
            w: 30,
            h: 40,
        };
        assert_eq!(
            r.on_screen((100, 200)),
            PxRect {
                x: 110,
                y: 220,
                w: 30,
                h: 40
            }
        );
        // Edges are rounded, not the size: 150 % keeps adjacent rects touching.
        let a = PxRect::from_dips(
            Rect {
                x: 1.0,
                y: 1.0,
                w: 1.0,
                h: 1.0,
            },
            1.0 / 1.5,
        );
        assert_eq!(
            a,
            PxRect {
                x: 2,
                y: 2,
                w: 1,
                h: 1
            }
        );
    }
}
