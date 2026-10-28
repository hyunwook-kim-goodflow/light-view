//! The settings window's controls as a pure model: options, focus, keys, clicks and
//! the settings change each one makes.

use lv_core::command::{Mods, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT};
use lv_core::settings::{LangSetting, Settings, SidebarSide};

use crate::strings::{text, Lang, S};

use super::draw::Layout;

pub(super) const VK_TAB: u16 = 0x09;
pub(super) const VK_SPACE: u16 = 0x20;

/// The memory limit's options; `None` is automatic.
const MEMORY_MB: [Option<u32>; 5] = [None, Some(256), Some(512), Some(1024), Some(2048)];
const LANGUAGES: [LangSetting; 3] = [LangSetting::Auto, LangSetting::Ko, LangSetting::En];
const SIDES: [SidebarSide; 2] = [SidebarSide::Left, SidebarSide::Right];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Language,
    PinMenu,
    /// "사이드바 위치" (M6): the resize screen's sidebar on the left or the right.
    Sidebar,
    OpenIn,
    Rotation,
    Memory,
    /// "업데이트 확인" (spec §6.6): a checkbox, on only after a yes.
    UpdateCheck,
    DefaultApps,
}

/// Top to bottom; also the Tab order.
pub const CONTROLS: [Control; 8] = [
    Control::Language,
    Control::PinMenu,
    Control::Sidebar,
    Control::OpenIn,
    Control::Rotation,
    Control::Memory,
    Control::UpdateCheck,
    Control::DefaultApps,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
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
            Control::Sidebar => S::ResizeSidebar,
            Control::OpenIn => S::SettingsOpenOther,
            Control::Rotation => S::SettingsRotated,
            Control::Memory => S::SettingsMemory,
            Control::UpdateCheck => S::SettingsUpdateCheck,
            Control::DefaultApps => S::MenuDefaultApps,
        }
    }

    pub(super) fn kind(self) -> Kind {
        match self {
            Control::PinMenu | Control::UpdateCheck => Kind::Check,
            Control::DefaultApps => Kind::Button,
            _ => Kind::Choice,
        }
    }

    /// A choice's option labels in `lang`; empty for the checkbox and the button.
    pub fn option_labels(self, lang: Lang) -> Vec<String> {
        let keys = |keys: &[S]| keys.iter().map(|k| text(lang, *k).to_owned()).collect();
        match self {
            Control::Language => keys(&[S::SettingsAuto, S::SettingsKorean, S::SettingsEnglish]),
            Control::Sidebar => keys(&[S::SettingsLeft, S::SettingsRight]),
            Control::OpenIn => keys(&[S::SettingsSameWindow, S::SettingsNewWindow]),
            Control::Rotation => keys(&[S::SettingsAskToSave, S::SettingsSaveRightAway]),
            Control::Memory => MEMORY_MB
                .iter()
                .map(|mb| match mb {
                    None => text(lang, S::SettingsAuto).to_owned(),
                    Some(mb) => format!("{mb}{}", text(lang, S::SettingsMegabytes)),
                })
                .collect(),
            Control::PinMenu | Control::UpdateCheck | Control::DefaultApps => Vec::new(),
        }
    }

    pub(super) fn option_count(self) -> usize {
        match self {
            Control::Language => LANGUAGES.len(),
            Control::Sidebar | Control::OpenIn | Control::Rotation => 2,
            Control::Memory => MEMORY_MB.len(),
            Control::PinMenu | Control::UpdateCheck | Control::DefaultApps => 0,
        }
    }

    /// The chosen option of a choice. A memory limit that is not offered chooses none.
    pub fn selected(self, s: &Settings) -> Option<usize> {
        match self {
            Control::Language => LANGUAGES.iter().position(|l| *l == s.language),
            Control::Sidebar => SIDES.iter().position(|side| *side == s.sidebar_side),
            Control::OpenIn => Some(usize::from(s.open_in_new_window)),
            Control::Rotation => Some(usize::from(s.auto_save_rotation)),
            Control::Memory => {
                let mb = s.memory_budget_mb.filter(|&mb| mb > 0);
                MEMORY_MB.iter().position(|m| *m == mb)
            }
            Control::PinMenu | Control::UpdateCheck | Control::DefaultApps => None,
        }
    }

    /// A checkbox's state. Update checks show off until the user said yes.
    pub fn checked(self, s: &Settings) -> bool {
        match self {
            Control::PinMenu => s.toolbar_pinned,
            Control::UpdateCheck => s.update_check == Some(true),
            _ => false,
        }
    }

    /// The change a checkbox makes when turned to `on`.
    fn toggle(self, on: bool) -> Option<Change> {
        match self {
            Control::PinMenu => Some(Change::PinMenu(on)),
            Control::UpdateCheck => Some(Change::UpdateCheck(on)),
            _ => None,
        }
    }

    /// The change that picks option `i` of a choice.
    fn change_for(self, i: usize) -> Option<Change> {
        Some(match self {
            Control::Language => Change::Language(*LANGUAGES.get(i)?),
            Control::Sidebar => Change::Sidebar(*SIDES.get(i)?),
            Control::OpenIn => Change::NewWindow(i == 1),
            Control::Rotation => Change::AutoSave(i == 1),
            Control::Memory => Change::Memory(*MEMORY_MB.get(i)?),
            Control::PinMenu | Control::UpdateCheck | Control::DefaultApps => return None,
        })
    }
}

/// A settings change, applied to `Settings` by [`apply_change`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Language(LangSetting),
    PinMenu(bool),
    Sidebar(SidebarSide),
    NewWindow(bool),
    AutoSave(bool),
    Memory(Option<u32>),
    UpdateCheck(bool),
}

/// What the main window has to do after a change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MainUpdate {
    /// The language changed: title, cached texts and a redraw.
    pub retranslate: bool,
    /// The toolbar was pinned or unpinned, or the sidebar moved: the image area changes.
    pub relayout: bool,
    pub auto_save: Option<bool>,
    /// Update checks were turned on: check now if one is due.
    pub check_updates: bool,
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
        Change::Sidebar(side) => {
            (s.sidebar_side != side).then_some(())?;
            s.sidebar_side = side;
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
        Change::UpdateCheck(on) => {
            // Turning it off answers the first-run question too: it is not asked.
            (s.update_check != Some(on)).then_some(())?;
            s.update_check = Some(on);
            update.check_updates = on;
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
        Kind::Check => control
            .toggle(!control.checked(s))
            .map_or(Action::Nothing, Action::Change),
        Kind::Button => Action::OpenDefaultApps,
        Kind::Choice => Action::Nothing,
    }
}
