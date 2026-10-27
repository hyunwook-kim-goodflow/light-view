//! The settings window (ruling R2): a small window the main window owns, drawn with the
//! same `Renderer`. Its controls are a pure model (state, layout and hit testing in
//! DIPs); every change is applied to the main window's `App` and saved at once (no OK
//! button).
//!
//! Keys: Tab and Shift+Tab move the focus (wrapping), ← → pick an option of the
//! focused choice, Space and Enter toggle the checkbox or press the button (once while
//! held), Esc closes the window. Alt combinations go on to `DefWindowProc` (Alt+F4).
//! A click acts on release over the part it pressed (the mouse is captured meanwhile);
//! the second click of a double-click does nothing.
//!
//! The window lives in `App::settings_win`; its procedure reaches the main `App` the
//! same way the main window's does and holds no borrow across calls that dispatch
//! messages. Owned by the main window, it is destroyed with it, and there is at most
//! one.

use std::cell::RefCell;

use lv_core::command::{Mods, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT};
use lv_core::settings::{LangSetting, Settings};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromWindow, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, IsWindow, LoadCursorW, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW, CS_DBLCLKS, GWLP_USERDATA, IDC_ARROW,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CAPTURECHANGED,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETOBJECT, WM_KEYDOWN, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_SYSKEYDOWN,
    WNDCLASSEXW, WS_CAPTION, WS_OVERLAPPED, WS_SYSMENU,
};

use crate::about;
use crate::commands;
use crate::input;
use crate::overlay::Toast;
use crate::render::{Align, Renderer, TextRun};
use crate::shell;
use crate::strings::{self, t, text, Lang, S};
use crate::theme::{self, palette, Palette, Rgb};
use crate::toolbar::Rect;
use crate::uia::Act;
use crate::uia_provider;
use crate::viewer::Redraw;
use crate::window::{invalidate, App};

pub const CLASS_NAME: PCWSTR = w!("goodflow.LightView.Settings");
/// Client size in DIPs; the window cannot be resized.
pub const SIZE_DIP: (f32, f32) = (420.0, 480.0);
/// A caption and a close box: no sizing border, no minimize or maximize.
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0);

const VK_TAB: u16 = 0x09;
const VK_SPACE: u16 = 0x20;

/// The memory limit's options; `None` is automatic.
const MEMORY_MB: [Option<u32>; 5] = [None, Some(256), Some(512), Some(1024), Some(2048)];
const LANGUAGES: [LangSetting; 3] = [LangSetting::Auto, LangSetting::Ko, LangSetting::En];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Language,
    PinMenu,
    OpenIn,
    Rotation,
    Memory,
    DefaultApps,
}

/// Top to bottom; also the Tab order.
pub const CONTROLS: [Control; 6] = [
    Control::Language,
    Control::PinMenu,
    Control::OpenIn,
    Control::Rotation,
    Control::Memory,
    Control::DefaultApps,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A row of options, one of them chosen.
    Choice,
    Check,
    Button,
}

impl Control {
    pub fn label_key(self) -> S {
        match self {
            Control::Language => S::SettingsLanguage,
            Control::PinMenu => S::SettingsPinMenu,
            Control::OpenIn => S::SettingsOpenOther,
            Control::Rotation => S::SettingsRotated,
            Control::Memory => S::SettingsMemory,
            Control::DefaultApps => S::MenuDefaultApps,
        }
    }

    fn kind(self) -> Kind {
        match self {
            Control::PinMenu => Kind::Check,
            Control::DefaultApps => Kind::Button,
            _ => Kind::Choice,
        }
    }

    /// A choice's option labels in `lang`; empty for the checkbox and the button.
    pub fn option_labels(self, lang: Lang) -> Vec<String> {
        let keys = |keys: &[S]| keys.iter().map(|k| text(lang, *k).to_owned()).collect();
        match self {
            Control::Language => keys(&[S::SettingsAuto, S::SettingsKorean, S::SettingsEnglish]),
            Control::OpenIn => keys(&[S::SettingsSameWindow, S::SettingsNewWindow]),
            Control::Rotation => keys(&[S::SettingsAskToSave, S::SettingsSaveRightAway]),
            Control::Memory => MEMORY_MB
                .iter()
                .map(|mb| match mb {
                    None => text(lang, S::SettingsAuto).to_owned(),
                    Some(mb) => format!("{mb}{}", text(lang, S::SettingsMegabytes)),
                })
                .collect(),
            Control::PinMenu | Control::DefaultApps => Vec::new(),
        }
    }

    fn option_count(self) -> usize {
        match self {
            Control::Language => LANGUAGES.len(),
            Control::OpenIn | Control::Rotation => 2,
            Control::Memory => MEMORY_MB.len(),
            Control::PinMenu | Control::DefaultApps => 0,
        }
    }

    /// The chosen option of a choice. A memory limit that is not offered chooses none.
    pub fn selected(self, s: &Settings) -> Option<usize> {
        match self {
            Control::Language => LANGUAGES.iter().position(|l| *l == s.language),
            Control::OpenIn => Some(usize::from(s.open_in_new_window)),
            Control::Rotation => Some(usize::from(s.auto_save_rotation)),
            Control::Memory => {
                let mb = s.memory_budget_mb.filter(|&mb| mb > 0);
                MEMORY_MB.iter().position(|m| *m == mb)
            }
            Control::PinMenu | Control::DefaultApps => None,
        }
    }

    /// The checkbox's state.
    pub fn checked(self, s: &Settings) -> bool {
        self == Control::PinMenu && s.toolbar_pinned
    }

    /// The change that picks option `i` of a choice.
    fn change_for(self, i: usize) -> Option<Change> {
        Some(match self {
            Control::Language => Change::Language(*LANGUAGES.get(i)?),
            Control::OpenIn => Change::NewWindow(i == 1),
            Control::Rotation => Change::AutoSave(i == 1),
            Control::Memory => Change::Memory(*MEMORY_MB.get(i)?),
            Control::PinMenu | Control::DefaultApps => return None,
        })
    }
}

/// A settings change, applied to `Settings` by [`apply_change`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Language(LangSetting),
    PinMenu(bool),
    NewWindow(bool),
    AutoSave(bool),
    Memory(Option<u32>),
}

/// What the main window has to do after a change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MainUpdate {
    /// The language changed: title, cached texts and a redraw.
    pub retranslate: bool,
    /// The toolbar was pinned or unpinned: the image area changes size.
    pub relayout: bool,
    pub auto_save: Option<bool>,
}

/// Applies `c` to `s`; `None` when it changes nothing (nothing to save or redo). The
/// window and memory choices only change the file: a new launch reads the first, and
/// the memory limit applies from the next start.
pub fn apply_change(s: &mut Settings, c: Change) -> Option<MainUpdate> {
    let mut update = MainUpdate::default();
    match c {
        Change::Language(l) => {
            (s.language != l).then_some(())?;
            s.language = l;
            update.retranslate = true;
        }
        Change::PinMenu(on) => {
            (s.toolbar_pinned != on).then_some(())?;
            s.toolbar_pinned = on;
            update.relayout = true;
        }
        Change::NewWindow(on) => {
            (s.open_in_new_window != on).then_some(())?;
            s.open_in_new_window = on;
        }
        Change::AutoSave(on) => {
            (s.auto_save_rotation != on).then_some(())?;
            s.auto_save_rotation = on;
            update.auto_save = Some(on);
        }
        Change::Memory(mb) => {
            let mb = mb.filter(|&mb| mb > 0);
            (s.memory_budget_mb != mb).then_some(())?;
            s.memory_budget_mb = mb;
        }
    }
    Some(update)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Redraw,
    Change(Change),
    OpenDefaultApps,
    Close,
}

/// The focused control and the pressed part (control, option).
#[derive(Debug, Default)]
pub struct Model {
    /// Index into `CONTROLS`.
    pub focused: usize,
    pressed: Option<(usize, usize)>,
}

impl Model {
    /// A key; `None` for Alt combinations (they go on to `DefWindowProc`). `repeat`:
    /// an auto-repeat of a held key, which toggles or presses nothing.
    pub fn on_key(&mut self, vk: u16, mods: Mods, repeat: bool, s: &Settings) -> Option<Action> {
        if mods.alt {
            return None;
        }
        if repeat && matches!(vk, VK_SPACE | VK_RETURN) {
            return Some(Action::Nothing);
        }
        let n = CONTROLS.len();
        let control = CONTROLS[self.focused];
        Some(match vk {
            VK_TAB => {
                self.focused = if mods.shift {
                    (self.focused + n - 1) % n
                } else {
                    (self.focused + 1) % n
                };
                Action::Redraw
            }
            VK_ESCAPE => Action::Close,
            VK_LEFT | VK_RIGHT if control.kind() == Kind::Choice => {
                let last = control.option_count() - 1;
                let current = control.selected(s);
                let next = match current {
                    None => 0,
                    Some(i) if vk == VK_LEFT => i.saturating_sub(1),
                    Some(i) => (i + 1).min(last),
                };
                pick(control, current, next)
            }
            VK_SPACE | VK_RETURN => press_control(control, s),
            _ => Action::Nothing,
        })
    }

    fn hit(x: f32, y: f32, l: &Layout) -> Option<(usize, usize)> {
        l.controls.iter().enumerate().find_map(|(c, rects)| {
            let o = rects.options.iter().position(|r| r.contains(x, y))?;
            Some((c, o))
        })
    }

    /// Mouse down; true if it is on a control, which takes the focus.
    pub fn press(&mut self, x: f32, y: f32, l: &Layout) -> bool {
        self.pressed = Self::hit(x, y, l);
        if let Some((c, _)) = self.pressed {
            self.focused = c;
        }
        self.pressed.is_some()
    }

    /// UI Automation invoked option `option` of control `c` (a choice), or the
    /// checkbox or button `c` (`None`): it acts as a click would, and takes the focus.
    pub fn invoke(&mut self, c: usize, option: Option<usize>, s: &Settings) -> Action {
        let Some(&control) = CONTROLS.get(c) else {
            return Action::Nothing;
        };
        self.focused = c;
        let action = match (control.kind(), option) {
            (Kind::Choice, Some(o)) if o < control.option_count() => {
                pick(control, control.selected(s), o)
            }
            (Kind::Check | Kind::Button, None) => press_control(control, s),
            _ => Action::Nothing,
        };
        if action == Action::Nothing {
            Action::Redraw
        } else {
            action
        }
    }

    /// The mouse capture ended without a release (another window took it).
    pub fn cancel_press(&mut self) {
        self.pressed = None;
    }

    /// Mouse up: acts only when released on the part that was pressed.
    pub fn release(&mut self, x: f32, y: f32, l: &Layout, s: &Settings) -> Action {
        let Some(pressed) = self.pressed.take() else {
            return Action::Nothing;
        };
        if Self::hit(x, y, l) != Some(pressed) {
            return Action::Redraw;
        }
        let (c, o) = pressed;
        let control = CONTROLS[c];
        let action = match control.kind() {
            Kind::Choice => pick(control, control.selected(s), o),
            Kind::Check | Kind::Button => press_control(control, s),
        };
        if action == Action::Nothing {
            Action::Redraw
        } else {
            action
        }
    }
}

/// Picking option `i` of a choice whose chosen option is `current`.
fn pick(control: Control, current: Option<usize>, i: usize) -> Action {
    if current == Some(i) {
        return Action::Nothing;
    }
    control
        .change_for(i)
        .map_or(Action::Nothing, Action::Change)
}

/// Space or Enter (or a click) on the checkbox or the button.
fn press_control(control: Control, s: &Settings) -> Action {
    match control.kind() {
        Kind::Check => Action::Change(Change::PinMenu(!control.checked(s))),
        Kind::Button => Action::OpenDefaultApps,
        Kind::Choice => Action::Nothing,
    }
}

// Layout (DIPs).
const PAD_X: f32 = 24.0;
const TOP: f32 = 16.0;
const LABEL_H: f32 = 20.0;
const LABEL_GAP: f32 = 2.0;
const ROW_H: f32 = 44.0;
const OPTION_GAP: f32 = 6.0;
const SECTION_GAP: f32 = 8.0;
const NOTE_H: f32 = 16.0;
const VERSION_H: f32 = 18.0;
const BOTTOM: f32 = 12.0;

/// One control's rects: a choice's label above its options; the checkbox's row or the
/// button as its only "option". `focus` is what the focus ring goes around.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlRects {
    pub label: Option<Rect>,
    pub options: Vec<Rect>,
    pub focus: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Follows `CONTROLS`.
    pub controls: Vec<ControlRects>,
    /// "다음 실행부터 적용돼요", under the memory options.
    pub note: Rect,
    /// "다음에 사진을 열 때부터 적용돼요", under the open-in options.
    pub open_note: Rect,
    pub version: Rect,
}

/// The controls top to bottom inside 24 DIP side padding, each option row 44 DIP tall
/// with equal option widths; the version line at the bottom.
pub fn layout((w, h): (f32, f32)) -> Layout {
    let x = PAD_X;
    let inner = (w - 2.0 * PAD_X).max(0.0);
    let row = |y: f32| Rect {
        x,
        y,
        w: inner,
        h: ROW_H,
    };
    let mut y = TOP;
    let mut note = Rect {
        x,
        y,
        w: inner,
        h: NOTE_H,
    };
    let mut open_note = note;
    let mut controls = Vec::with_capacity(CONTROLS.len());
    for c in CONTROLS {
        let rects = match c.kind() {
            Kind::Choice => {
                let label = Rect {
                    x,
                    y,
                    w: inner,
                    h: LABEL_H,
                };
                y += LABEL_H + LABEL_GAP;
                let n = c.option_count();
                let ow = (inner - OPTION_GAP * (n as f32 - 1.0)) / n as f32;
                let options = (0..n)
                    .map(|i| Rect {
                        x: x + i as f32 * (ow + OPTION_GAP),
                        y,
                        w: ow,
                        h: ROW_H,
                    })
                    .collect();
                ControlRects {
                    label: Some(label),
                    options,
                    focus: row(y),
                }
            }
            Kind::Check | Kind::Button => ControlRects {
                label: None,
                options: vec![row(y)],
                focus: row(y),
            },
        };
        y += ROW_H;
        match c {
            Control::Memory => {
                note.y = y;
                y += NOTE_H;
            }
            Control::OpenIn => {
                open_note.y = y;
                y += NOTE_H;
            }
            _ => {}
        }
        y += SECTION_GAP;
        controls.push(rects);
    }
    Layout {
        controls,
        note,
        open_note,
        version: Rect {
            x,
            y: h - BOTTOM - VERSION_H,
            w: inner,
            h: VERSION_H,
        },
    }
}

/// Window position for `size` centered on `owner`, moved inside `work` (the work area
/// of the owner's monitor) when it would stick out.
fn center_on(owner: RECT, (w, h): (i32, i32), work: RECT) -> (i32, i32) {
    let x = owner.left + (owner.right - owner.left - w) / 2;
    let y = owner.top + (owner.bottom - owner.top - h) / 2;
    (
        x.min(work.right - w).max(work.left),
        y.min(work.bottom - h).max(work.top),
    )
}

// ---- Drawing -------------------------------------------------------------------

const UI_FAMILY: &str = "Segoe UI";
const LABEL_SIZE: f32 = 13.0;
const TEXT_SIZE: f32 = 14.0;
const SMALL_SIZE: f32 = 12.0;
const RADIUS: f32 = 10.0;
const CHECK_BOX: f32 = 20.0;
const CHECK_GLYPH: char = '\u{E73E}';
const FOCUS_OUTSET: f32 = 3.0;

fn tuple(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
}

fn run(text: &str, color: Rgb, size: f32, bold: bool) -> TextRun<'_> {
    TextRun {
        text,
        family: UI_FAMILY,
        size,
        bold,
        color,
    }
}

/// Paper background; choices as rows of surface2 options with the chosen one in
/// highlighter; the focus ring in ink around the focused control.
fn draw(r: &mut Renderer, m: &Model, s: &Settings, p: &Palette, icon_family: &str) {
    let lang = strings::current();
    let l = layout(SIZE_DIP);
    r.begin(p.paper);
    for (i, (c, rects)) in CONTROLS.iter().zip(&l.controls).enumerate() {
        if let Some(label) = rects.label {
            let label_run = [run(t(c.label_key()), p.ink2, LABEL_SIZE, false)];
            r.draw_runs_ellipsis(tuple(label), &label_run, Align::Leading);
        }
        match c.kind() {
            Kind::Choice => {
                let chosen = c.selected(s);
                let labels = c.option_labels(lang);
                for (j, (rect, text)) in rects.options.iter().zip(&labels).enumerate() {
                    let on = chosen == Some(j);
                    let (fill, color) = if on {
                        (p.highlighter, p.on_highlighter)
                    } else {
                        (p.surface2, p.ink)
                    };
                    r.fill_round_rect(tuple(*rect), RADIUS, fill);
                    let label = [run(text, color, LABEL_SIZE, on)];
                    r.draw_runs_ellipsis(tuple(*rect), &label, Align::Center);
                }
            }
            Kind::Check => {
                let label = t(c.label_key());
                draw_checkbox(r, rects.options[0], label, c.checked(s), p, icon_family);
            }
            Kind::Button => {
                let rect = rects.options[0];
                r.fill_round_rect(tuple(rect), RADIUS, p.surface2);
                let label = [run(t(c.label_key()), p.ink, TEXT_SIZE, false)];
                r.draw_runs_ellipsis(tuple(rect), &label, Align::Center);
            }
        }
        if i == m.focused {
            let f = rects.focus;
            let ring = (
                f.x - FOCUS_OUTSET,
                f.y - FOCUS_OUTSET,
                f.w + 2.0 * FOCUS_OUTSET,
                f.h + 2.0 * FOCUS_OUTSET,
            );
            r.stroke_round_rect(ring, RADIUS + FOCUS_OUTSET, p.ink, 1.5);
        }
    }
    for (rect, key) in [
        (l.open_note, S::SettingsOpenOtherNote),
        (l.note, S::SettingsMemoryNote),
    ] {
        let note = [run(t(key), p.ink2, SMALL_SIZE, false)];
        r.draw_runs_ellipsis(tuple(rect), &note, Align::Leading);
    }
    let version = about::version_line();
    let version = [run(&version, p.ink2, SMALL_SIZE, false)];
    r.draw_runs(tuple(l.version), &version, Align::Leading);
    if let Err(e) = r.end() {
        eprintln!("light-view: EndDraw failed in the settings window: {e}");
    }
}

fn draw_checkbox(
    r: &mut Renderer,
    row: Rect,
    label: &str,
    checked: bool,
    p: &Palette,
    icon_family: &str,
) {
    let mark = (
        row.x + 4.0,
        row.y + (row.h - CHECK_BOX) / 2.0,
        CHECK_BOX,
        CHECK_BOX,
    );
    if checked {
        r.fill_round_rect(mark, 5.0, p.highlighter);
        r.draw_glyph(
            mark,
            CHECK_GLYPH,
            icon_family,
            14.0,
            p.on_highlighter,
            false,
        );
    } else {
        r.stroke_round_rect(mark, 5.0, p.ink2, 1.5);
    }
    let label_x = mark.0 + CHECK_BOX + 10.0;
    let text = (label_x, row.y, row.x + row.w - label_x, row.h);
    r.draw_runs_ellipsis(text, &[run(label, p.ink, TEXT_SIZE, false)], Align::Leading);
}

// ---- Window --------------------------------------------------------------------

/// The open settings window, kept in `App::settings_win`.
pub struct SettingsWindow {
    pub hwnd: HWND,
    /// The main window.
    owner: HWND,
    model: Model,
    renderer: Renderer,
}

/// Ctrl+, and the menu's 설정: opens the window, or brings the open one forward.
pub fn open(app: &RefCell<App>, owner: HWND) {
    let Ok(existing) = app
        .try_borrow()
        .map(|a| a.settings_win.as_ref().map(|w| w.hwnd))
    else {
        return;
    };
    if let Some(hwnd) = existing {
        // SAFETY: plain calls; no App borrow is held (activation re-enters).
        unsafe {
            if IsWindow(hwnd).as_bool() {
                let _ = SetForegroundWindow(hwnd);
                return;
            }
        }
    }
    let hwnd = match create_window(owner, std::ptr::from_ref(app)) {
        Ok(hwnd) => hwnd,
        Err(e) => {
            eprintln!("light-view: could not open the settings window: {e}");
            return;
        }
    };
    let renderer = Renderer::new(hwnd);
    let dark = match renderer {
        Ok(renderer) => app.try_borrow_mut().ok().map(|mut a| {
            a.settings_win = Some(SettingsWindow {
                hwnd,
                owner,
                model: Model::default(),
                renderer,
            });
            a.dark()
        }),
        Err(e) => {
            eprintln!("light-view: could not draw the settings window: {e}");
            None
        }
    };
    let Some(dark) = dark else {
        // SAFETY: our own window; no App borrow is held.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    };
    theme::apply_title_bar(hwnd, dark);
    // SAFETY: plain call; no App borrow is held (showing activates and paints).
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

/// The system theme changed: the title bar and a repaint.
pub fn on_theme(app: &RefCell<App>, dark: bool) {
    let Some(hwnd) = app
        .try_borrow()
        .ok()
        .and_then(|a| a.settings_win.as_ref().map(|w| w.hwnd))
    else {
        return;
    };
    theme::apply_title_bar(hwnd, dark);
    invalidate(hwnd);
}

fn register_class() -> windows::core::Result<()> {
    // SAFETY: plain calls; the class strings are static.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(windows::core::Error::from_win32());
        }
    }
    Ok(())
}

/// Creates the (hidden) window: a `SIZE_DIP` client area at the owner's DPI, centered
/// on the owner. `app` (null in tests) is how the window procedure reaches the App.
fn create_window(owner: HWND, app: *const RefCell<App>) -> windows::core::Result<HWND> {
    register_class()?;
    // SAFETY: plain calls; every pointer passed is a live local, and `app` outlives the
    // window (see `wndproc`) or is null.
    unsafe {
        let dpi = match GetDpiForWindow(owner) {
            0 => 96,
            d => d,
        };
        let scale = dpi as f32 / 96.0;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: (SIZE_DIP.0 * scale).round() as i32,
            bottom: (SIZE_DIP.1 * scale).round() as i32,
        };
        AdjustWindowRectExForDpi(&mut rc, STYLE, false, WINDOW_EX_STYLE(0), dpi)?;
        let size = (rc.right - rc.left, rc.bottom - rc.top);
        let mut owner_rc = RECT::default();
        GetWindowRect(owner, &mut owner_rc)?;
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        let work = if GetMonitorInfoW(monitor, &mut mi).as_bool() {
            mi.rcWork
        } else {
            owner_rc
        };
        let (x, y) = center_on(owner_rc, size, work);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            &HSTRING::from(t(S::MenuSettings)),
            STYLE,
            x,
            y,
            size.0,
            size.1,
            owner,
            None,
            GetModuleHandleW(None)?,
            Some(app.cast()),
        )
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE, lparam points to the CREATESTRUCTW whose
        // lpCreateParams is the `*const RefCell<App>` given to CreateWindowExW (or null).
        unsafe {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
    }
    // SAFETY: GWLP_USERDATA is 0 or the main window's `RefCell<App>`, which outlives
    // this window: the main window owns it, so destroying the main window destroys
    // this one first, and WM_NCDESTROY clears the pointer.
    let app = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>).as_ref() };
    match app.and_then(|app| handle(app, hwnd, msg, wparam, lparam)) {
        Some(result) => result,
        // SAFETY: default handling with the arguments we were given.
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Returns `None` to fall through to `DefWindowProcW`, also for messages that arrive
/// before the window is in `App::settings_win`. No App borrow is held across calls that
/// may re-enter a window procedure.
fn handle(
    app: &RefCell<App>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    match msg {
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: paired BeginPaint/EndPaint on our window with a live PAINTSTRUCT.
            unsafe { BeginPaint(hwnd, &mut ps) };
            if let Ok(mut a) = app.try_borrow_mut() {
                let a = &mut *a;
                let (p, icons) = (palette(a.dark()), a.icon_family());
                if let Some(w) = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd) {
                    draw(&mut w.renderer, &w.model, &a.settings, p, icons);
                    let focused = w.model.focused;
                    uia_provider::publish(hwnd, || uia_tree(hwnd, focused, &a.settings));
                }
            }
            // SAFETY: as above.
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            uia_provider::flush(hwnd);
            Some(LRESULT(0))
        }
        WM_GETOBJECT => uia_provider::get_object(hwnd, wparam, lparam, || {
            let a = app.try_borrow().ok()?;
            let w = a.settings_win.as_ref().filter(|w| w.hwnd == hwnd)?;
            Some(uia_tree(hwnd, w.model.focused, &a.settings))
        }),
        uia_provider::WM_APP_UIA => {
            let action = {
                let act = uia_provider::action(hwnd, wparam)?;
                let mut a = app.try_borrow_mut().ok()?;
                let a = &mut *a;
                let w = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd)?;
                match act {
                    Act::Pick(c, o) => w.model.invoke(c, Some(o), &a.settings),
                    Act::Press(c) => w.model.invoke(c, None, &a.settings),
                    _ => Action::Nothing,
                }
            };
            act(app, hwnd, action);
            Some(LRESULT(0))
        }
        WM_SIZE => {
            let mut rc = RECT::default();
            // SAFETY: `rc` is a live local; plain call on our window.
            unsafe { GetClientRect(hwnd, &mut rc) }.ok()?;
            let mut a = app.try_borrow_mut().ok()?;
            let w = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd)?;
            let (pw, ph) = (rc.right - rc.left, rc.bottom - rc.top);
            w.renderer.resize(pw.max(0) as u32, ph.max(0) as u32);
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            {
                let mut a = app.try_borrow_mut().ok()?;
                let w = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd)?;
                w.renderer.set_dpi(f32::from((wparam.0 & 0xFFFF) as u16));
            }
            // SAFETY: for WM_DPICHANGED, lparam points to the suggested window RECT.
            let rc = unsafe { *(lparam.0 as *const RECT) };
            // SAFETY: plain call on our window; no App borrow is held (WM_SIZE re-enters).
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            invalidate(hwnd);
            Some(LRESULT(0))
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let action = {
                let mut a = app.try_borrow_mut().ok()?;
                let a = &mut *a;
                let w = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd)?;
                let vk = wparam.0 as u16;
                let repeat = lparam.0 & (1 << 30) != 0;
                w.model.on_key(vk, commands::mods(), repeat, &a.settings)?
            };
            act(app, hwnd, action);
            Some(LRESULT(0))
        }
        WM_LBUTTONDOWN => {
            let pressed = {
                let mut a = app.try_borrow_mut().ok()?;
                let w = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd)?;
                let (x, y) = to_dips(hwnd, lparam);
                w.model.press(x, y, &layout(SIZE_DIP))
            };
            if pressed {
                // SAFETY: plain call on our window; no App borrow is held.
                unsafe { SetCapture(hwnd) };
            }
            invalidate(hwnd);
            Some(LRESULT(0))
        }
        // The second click of a double-click: not another toggle.
        WM_LBUTTONDBLCLK => Some(LRESULT(0)),
        WM_LBUTTONUP => {
            let action = {
                let mut a = app.try_borrow_mut().ok()?;
                let a = &mut *a;
                let w = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd)?;
                let (x, y) = to_dips(hwnd, lparam);
                w.model.release(x, y, &layout(SIZE_DIP), &a.settings)
            };
            // SAFETY: plain call; no App borrow is held (WM_CAPTURECHANGED re-enters).
            unsafe {
                let _ = ReleaseCapture();
            }
            act(app, hwnd, action);
            Some(LRESULT(0))
        }
        WM_CAPTURECHANGED => {
            let mut a = app.try_borrow_mut().ok()?;
            let w = a.settings_win.as_mut().filter(|w| w.hwnd == hwnd)?;
            w.model.cancel_press();
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            uia_provider::detach(hwnd);
            if let Ok(mut a) = app.try_borrow_mut() {
                if a.settings_win.as_ref().is_some_and(|w| w.hwnd == hwnd) {
                    a.settings_win = None;
                }
            }
            Some(LRESULT(0))
        }
        WM_NCDESTROY => {
            // SAFETY: clears the App pointer so no later message can reach it.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            None
        }
        _ => None,
    }
}

/// The window's UI Automation tree in the current language.
fn uia_tree(hwnd: HWND, focused: usize, s: &Settings) -> crate::uia::Tree {
    crate::uia::settings_tree(focused, s, strings::current(), dip_per_px(hwnd))
}

fn dip_per_px(hwnd: HWND) -> f32 {
    // SAFETY: plain call on our window.
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => 1.0,
        dpi => 96.0 / dpi as f32,
    }
}

/// A mouse message's point in client DIPs.
fn to_dips(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let dip = dip_per_px(hwnd);
    let (x, y) = input::point(lparam);
    (x as f32 * dip, y as f32 * dip)
}

/// Carries out an action. No App borrow is held while it runs.
fn act(app: &RefCell<App>, hwnd: HWND, action: Action) {
    match action {
        Action::Nothing => {}
        Action::Redraw => invalidate(hwnd),
        Action::Close => {
            // SAFETY: our own window; no App borrow is held (WM_DESTROY re-enters).
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        Action::OpenDefaultApps => {
            invalidate(hwnd);
            if !shell::open(hwnd, "ms-settings:defaultapps".as_ref()) {
                if let Some(owner) = owner(app) {
                    input::show_toast(app, owner, |now| Toast::new(S::ToastOpenFailed, now));
                }
            }
        }
        Action::Change(change) => apply(app, hwnd, change),
    }
}

fn owner(app: &RefCell<App>) -> Option<HWND> {
    app.try_borrow()
        .ok()?
        .settings_win
        .as_ref()
        .map(|w| w.owner)
}

/// Applies a change to the main window's settings, saves them and brings the main
/// window up to date. After a language change the title bars, the cached text (the
/// toast; the image area's message and dialogs are looked up as they are drawn) and
/// the tooltips follow, and both windows repaint.
fn apply(app: &RefCell<App>, hwnd: HWND, change: Change) {
    let Some(owner) = owner(app) else {
        return;
    };
    let update = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        let Some(update) = apply_change(&mut a.settings, change) else {
            return;
        };
        // Only the changed field is written: another process may have saved others.
        a.persist(|s| {
            apply_change(s, change);
        });
        if update.retranslate {
            strings::set(a.settings.language);
            a.overlay.retranslate(strings::current());
        }
        if update.relayout {
            a.on_size(owner);
        }
        if let Some(on) = update.auto_save {
            a.viewer.set_auto_save(on);
        }
        update
    };
    if update.retranslate {
        // The main window's title and tooltips, and a repaint.
        commands::after(app, owner, Redraw::Full);
        // SAFETY: plain call on our window; no App borrow is held (WM_SETTEXT re-enters).
        unsafe {
            let _ = SetWindowTextW(hwnd, &HSTRING::from(t(S::MenuSettings)));
        }
    }
    if update.relayout {
        // As the toolbar's own pin button does: the tooltips follow, and after
        // unpinning the floating toolbar stays up a moment, then hides.
        input::pin_changed(app, owner);
    }
    invalidate(owner);
    invalidate(hwnd);
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
    const VK_TAB: u16 = 0x09;
    const VK_SPACE: u16 = 0x20;
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_LEFT: u16 = 0x25;
    const VK_RIGHT: u16 = 0x27;

    fn center(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn focus(m: &mut Model, c: Control) {
        m.focused = CONTROLS.iter().position(|x| *x == c).unwrap();
    }

    #[test]
    fn controls_in_spec_order_with_their_options() {
        let labels: Vec<_> = CONTROLS
            .iter()
            .map(|c| text(Lang::Ko, c.label_key()))
            .collect();
        assert_eq!(
            labels,
            [
                "언어",
                "메뉴 고정",
                "다른 사진을 열 때",
                "회전한 사진을",
                "메모리 한도",
                "기본 앱 설정 열기"
            ]
        );
        let options = |c: Control| c.option_labels(Lang::Ko);
        assert_eq!(options(Control::Language), ["자동", "한국어", "English"]);
        assert_eq!(options(Control::OpenIn), ["같은 창", "새 창"]);
        assert_eq!(options(Control::Rotation), ["저장할지 묻기", "바로 저장"]);
        assert_eq!(
            options(Control::Memory),
            ["자동", "256 MB", "512 MB", "1024 MB", "2048 MB"]
        );
        assert!(options(Control::PinMenu).is_empty());
        assert!(options(Control::DefaultApps).is_empty());
        assert_eq!(
            Control::Language.option_labels(Lang::En),
            ["Auto", "한국어", "English"]
        );
    }

    #[test]
    fn selected_options_follow_the_settings() {
        let mut s = Settings::default();
        assert_eq!(Control::Language.selected(&s), Some(0));
        assert_eq!(Control::OpenIn.selected(&s), Some(0));
        assert_eq!(Control::Rotation.selected(&s), Some(0));
        assert_eq!(Control::Memory.selected(&s), Some(0));
        s.language = LangSetting::En;
        s.open_in_new_window = true;
        s.auto_save_rotation = true;
        s.memory_budget_mb = Some(1024);
        assert_eq!(Control::Language.selected(&s), Some(2));
        assert_eq!(Control::OpenIn.selected(&s), Some(1));
        assert_eq!(Control::Rotation.selected(&s), Some(1));
        assert_eq!(Control::Memory.selected(&s), Some(3));
        // 0 MB means automatic; a value not offered selects nothing.
        s.memory_budget_mb = Some(0);
        assert_eq!(Control::Memory.selected(&s), Some(0));
        s.memory_budget_mb = Some(300);
        assert_eq!(Control::Memory.selected(&s), None);
        assert!(Control::PinMenu.checked(&Settings {
            toolbar_pinned: true,
            ..Settings::default()
        }));
    }

    #[test]
    fn tab_moves_the_focus_and_wraps() {
        let s = Settings::default();
        let mut m = Model::default();
        assert_eq!(m.focused, 0);
        assert_eq!(m.on_key(VK_TAB, NONE, false, &s), Some(Action::Redraw));
        assert_eq!(m.focused, 1);
        for _ in 0..5 {
            m.on_key(VK_TAB, NONE, false, &s);
        }
        assert_eq!(m.focused, 0);
        m.on_key(VK_TAB, SHIFT, false, &s);
        assert_eq!(m.focused, 5);
    }

    #[test]
    fn arrows_pick_options_space_and_enter_toggle_or_press() {
        let mut s = Settings::default();
        let mut m = Model::default();
        // Language: → picks the next option, ← the previous; no wrapping.
        assert_eq!(
            m.on_key(VK_RIGHT, NONE, false, &s),
            Some(Action::Change(Change::Language(LangSetting::Ko)))
        );
        s.language = LangSetting::En;
        assert_eq!(m.on_key(VK_RIGHT, NONE, false, &s), Some(Action::Nothing));
        assert_eq!(
            m.on_key(VK_LEFT, NONE, false, &s),
            Some(Action::Change(Change::Language(LangSetting::Ko)))
        );
        // Space and Enter do nothing on a choice.
        assert_eq!(m.on_key(VK_SPACE, NONE, false, &s), Some(Action::Nothing));
        // The checkbox: Space and Enter toggle; arrows do nothing.
        focus(&mut m, Control::PinMenu);
        assert_eq!(
            m.on_key(VK_SPACE, NONE, false, &s),
            Some(Action::Change(Change::PinMenu(true)))
        );
        s.toolbar_pinned = true;
        assert_eq!(
            m.on_key(VK_RETURN, NONE, false, &s),
            Some(Action::Change(Change::PinMenu(false)))
        );
        assert_eq!(m.on_key(VK_RIGHT, NONE, false, &s), Some(Action::Nothing));
        focus(&mut m, Control::OpenIn);
        assert_eq!(
            m.on_key(VK_RIGHT, NONE, false, &s),
            Some(Action::Change(Change::NewWindow(true)))
        );
        focus(&mut m, Control::Rotation);
        assert_eq!(
            m.on_key(VK_RIGHT, NONE, false, &s),
            Some(Action::Change(Change::AutoSave(true)))
        );
        focus(&mut m, Control::Memory);
        assert_eq!(
            m.on_key(VK_RIGHT, NONE, false, &s),
            Some(Action::Change(Change::Memory(Some(256))))
        );
        s.memory_budget_mb = Some(256);
        assert_eq!(
            m.on_key(VK_LEFT, NONE, false, &s),
            Some(Action::Change(Change::Memory(None)))
        );
        // A value not offered: → starts from the first option.
        s.memory_budget_mb = Some(300);
        assert_eq!(
            m.on_key(VK_RIGHT, NONE, false, &s),
            Some(Action::Change(Change::Memory(None)))
        );
        // The button: Space and Enter open the Windows page.
        focus(&mut m, Control::DefaultApps);
        assert_eq!(
            m.on_key(VK_SPACE, NONE, false, &s),
            Some(Action::OpenDefaultApps)
        );
        assert_eq!(
            m.on_key(VK_RETURN, NONE, false, &s),
            Some(Action::OpenDefaultApps)
        );
        assert_eq!(m.on_key(VK_LEFT, NONE, false, &s), Some(Action::Nothing));
    }

    #[test]
    fn held_space_and_enter_act_once() {
        let s = Settings::default();
        let mut m = Model::default();
        focus(&mut m, Control::PinMenu);
        assert_eq!(m.on_key(VK_SPACE, NONE, true, &s), Some(Action::Nothing));
        assert_eq!(m.on_key(VK_RETURN, NONE, true, &s), Some(Action::Nothing));
        // Held arrows and Tab still repeat.
        assert_eq!(m.on_key(VK_TAB, NONE, true, &s), Some(Action::Redraw));
        assert_eq!(
            m.on_key(VK_RIGHT, NONE, true, &s),
            Some(Action::Change(Change::NewWindow(true)))
        );
    }

    #[test]
    fn escape_closes_and_alt_combinations_pass() {
        let s = Settings::default();
        let mut m = Model::default();
        assert_eq!(m.on_key(VK_ESCAPE, NONE, false, &s), Some(Action::Close));
        assert_eq!(m.on_key(0x73, ALT, false, &s), None); // Alt+F4
        assert_eq!(m.on_key(0x41, NONE, false, &s), Some(Action::Nothing));
    }

    #[test]
    fn invocations_act_like_clicks_and_take_the_focus() {
        let s = Settings::default();
        let mut m = Model::default();
        assert_eq!(
            m.invoke(4, Some(2), &s),
            Action::Change(Change::Memory(Some(512)))
        );
        assert_eq!(m.focused, 4);
        // The chosen option again: only the focus moves.
        assert_eq!(m.invoke(0, Some(0), &s), Action::Redraw);
        assert_eq!(m.focused, 0);
        assert_eq!(m.invoke(1, None, &s), Action::Change(Change::PinMenu(true)));
        assert_eq!(m.invoke(5, None, &s), Action::OpenDefaultApps);
        // Mismatched or out-of-range parts do nothing.
        assert_eq!(m.invoke(0, None, &s), Action::Redraw);
        assert_eq!(m.invoke(1, Some(0), &s), Action::Redraw);
        assert_eq!(m.invoke(0, Some(7), &s), Action::Redraw);
        assert_eq!(m.invoke(9, None, &s), Action::Nothing);
    }

    #[test]
    fn clicks_pick_options_and_act_on_release_over_the_pressed_part() {
        let mut s = Settings::default();
        let l = layout(SIZE_DIP);
        let mut m = Model::default();
        let lang = &l.controls[0];
        // Pressed on 한국어, released on English: nothing; the focus moved there.
        let (kx, ky) = center(lang.options[1]);
        let (ex, ey) = center(lang.options[2]);
        m.focused = 3;
        assert!(m.press(kx, ky, &l));
        assert_eq!(m.focused, 0);
        assert_eq!(m.release(ex, ey, &l, &s), Action::Redraw);
        assert!(m.press(ex, ey, &l));
        assert_eq!(
            m.release(ex, ey, &l, &s),
            Action::Change(Change::Language(LangSetting::En))
        );
        // Clicking the option already chosen changes nothing.
        s.language = LangSetting::En;
        m.press(ex, ey, &l);
        assert_eq!(m.release(ex, ey, &l, &s), Action::Redraw);
        // The checkbox row toggles, the button opens the Windows page.
        let (px, py) = center(l.controls[1].options[0]);
        m.press(px, py, &l);
        assert_eq!(
            m.release(px, py, &l, &s),
            Action::Change(Change::PinMenu(true))
        );
        let (bx, by) = center(l.controls[5].options[0]);
        m.press(bx, by, &l);
        assert_eq!(m.release(bx, by, &l, &s), Action::OpenDefaultApps);
        assert_eq!(m.focused, 5);
        // Outside every control: not taken, nothing happens.
        assert!(!m.press(2.0, 2.0, &l));
        assert_eq!(m.release(2.0, 2.0, &l, &s), Action::Nothing);
    }

    #[test]
    fn layout_fits_the_window_with_44_dip_targets() {
        let l = layout(SIZE_DIP);
        assert_eq!(l.controls.len(), CONTROLS.len());
        let inside = |r: Rect| {
            r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= SIZE_DIP.0 && r.y + r.h <= SIZE_DIP.1
        };
        let mut bottom = 0.0f32;
        for (c, rects) in CONTROLS.iter().zip(&l.controls) {
            let want = c.option_labels(Lang::Ko).len().max(1);
            assert_eq!(rects.options.len(), want, "{c:?}");
            for r in &rects.options {
                assert!(r.h >= 44.0 && r.w >= 44.0, "{c:?} {r:?}");
                assert!(inside(*r), "{c:?} {r:?}");
                assert!(r.y >= bottom - 0.01, "{c:?} overlaps the control above");
            }
            if let Some(label) = rects.label {
                assert!(inside(label));
                assert!(label.y + label.h <= rects.options[0].y + 0.01);
            }
            bottom = rects.focus.y + rects.focus.h;
        }
        // Each note sits under its options, above the next control; the version at
        // the bottom.
        let under = |note: Rect, c: usize| {
            let o = l.controls[c].options[0];
            note.y >= o.y + o.h - 0.01
                && note.y + note.h
                    <= l.controls[c + 1]
                        .label
                        .unwrap_or(l.controls[c + 1].options[0])
                        .y
        };
        assert!(under(l.note, 4));
        assert!(under(l.open_note, 2));
        assert!(inside(l.open_note));
        assert!(l.version.y >= bottom);
        assert!(inside(l.note) && inside(l.version));
    }

    #[test]
    fn language_switch_redraws() {
        let mut s = Settings::default();
        let update = apply_change(&mut s, Change::Language(LangSetting::En)).unwrap();
        assert_eq!(s.language, LangSetting::En);
        assert!(update.retranslate);
        assert!(!update.relayout);
        // The same language again: nothing to do.
        assert_eq!(
            apply_change(&mut s, Change::Language(LangSetting::En)),
            None
        );
    }

    #[test]
    fn changes_apply_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut s = Settings::default();
        let pin = apply_change(&mut s, Change::PinMenu(true)).unwrap();
        assert!(pin.relayout && !pin.retranslate);
        let auto = apply_change(&mut s, Change::AutoSave(true)).unwrap();
        assert_eq!(auto.auto_save, Some(true));
        assert_eq!(
            apply_change(&mut s, Change::NewWindow(true)),
            Some(MainUpdate::default())
        );
        assert_eq!(
            apply_change(&mut s, Change::Memory(Some(512))),
            Some(MainUpdate::default())
        );
        apply_change(&mut s, Change::Language(LangSetting::Ko)).unwrap();
        assert!(s.toolbar_pinned && s.auto_save_rotation && s.open_in_new_window);
        assert_eq!(s.memory_budget_mb, Some(512));
        lv_core::settings::save_settings(&path, &s).unwrap();
        assert_eq!(lv_core::settings::load_settings(&path), s);
        // Back to automatic memory: no value in the file.
        apply_change(&mut s, Change::Memory(None)).unwrap();
        lv_core::settings::save_settings(&path, &s).unwrap();
        let loaded = lv_core::settings::load_settings(&path);
        assert_eq!(loaded.memory_budget_mb, None);
        assert_eq!(loaded, s);
    }

    #[test]
    fn the_window_is_centered_on_its_owner_and_kept_on_screen() {
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        let work = rect(0, 0, 1920, 1040);
        assert_eq!(
            center_on(rect(100, 100, 1100, 900), (420, 500), work),
            (390, 250)
        );
        // Near an edge: moved inside the work area.
        assert_eq!(
            center_on(rect(-900, -700, 100, 100), (420, 500), work),
            (0, 0)
        );
        assert_eq!(
            center_on(rect(1800, 900, 2400, 1300), (420, 500), work),
            (1500, 540)
        );
    }

    #[test]
    fn settings_window_closes_with_owner() {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindow, GetWindowLongW, GWL_STYLE, GW_OWNER, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
            WS_OVERLAPPEDWINDOW, WS_THICKFRAME,
        };
        // SAFETY: plain Win32 calls on windows this test creates and destroys; `rc` is
        // a live local.
        unsafe {
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("owner"),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                900,
                700,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let settings = create_window(owner, std::ptr::null()).unwrap();
            assert!(IsWindow(settings).as_bool());
            assert_eq!(GetWindow(settings, GW_OWNER).unwrap(), owner);
            // Fixed size: no sizing border, no maximize or minimize box.
            let style = GetWindowLongW(settings, GWL_STYLE) as u32;
            assert_eq!(style & WS_THICKFRAME.0, 0);
            assert_eq!(style & (WS_MAXIMIZEBOX.0 | WS_MINIMIZEBOX.0), 0);
            let dpi = GetDpiForWindow(settings).max(96) as f32;
            let mut rc = RECT::default();
            GetClientRect(settings, &mut rc).unwrap();
            assert_eq!(rc.right, (420.0 * dpi / 96.0).round() as i32);
            assert_eq!(rc.bottom, (480.0 * dpi / 96.0).round() as i32);
            // Destroying the main window takes the settings window with it.
            DestroyWindow(owner).unwrap();
            assert!(!IsWindow(settings).as_bool());
        }
    }
}
