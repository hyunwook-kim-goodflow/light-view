//! The resize screen's sidebar controls as a pure model (the settings window's way:
//! options, focus, keys, clicks and what each one changes); its layout is in
//! [`layout`](super::layout), re-exported here.
//!
//! The numbers are native EDIT controls (`fields`); everything else is drawn. The Tab
//! order runs through every control: tabs, the tab's fields, 기준, 형식, 품질 (not for
//! PNG, which has none), 저장, 위치 정보 제거, the list, [취소], [저장].

use lv_core::command::{Mods, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT};
use lv_core::settings::{ResizeAxis, ResizeFormat, ResizeMemory, ResizeTab, MIN_QUALITY};

pub use super::layout::*;
use crate::strings::{text, Lang, S};

pub const VK_TAB: u16 = 0x09;
pub const VK_SPACE: u16 = 0x20;
pub const VK_UP: u16 = 0x26;
pub const VK_DOWN: u16 = 0x28;

/// The quality choices: 원본 수준, then fixed qualities (never below 60).
pub const QUALITIES: [Option<u8>; 5] = [None, Some(90), Some(80), Some(70), Some(MIN_QUALITY)];

/// A fixed quality not among the choices (one from the settings file) becomes the
/// nearest offered one; a tie goes to the higher.
pub fn snap_quality(q: Option<u8>) -> Option<u8> {
    let q = q?;
    QUALITIES
        .iter()
        .flatten()
        .copied()
        .min_by_key(|o| (o.abs_diff(q), std::cmp::Reverse(*o)))
}
const TABS: [ResizeTab; 3] = [ResizeTab::Resolution, ResizeTab::FileSize, ResizeTab::Scale];
const AXES: [ResizeAxis; 3] = [ResizeAxis::LongEdge, ResizeAxis::Width, ResizeAxis::Height];
const FORMATS: [ResizeFormat; 3] = [ResizeFormat::Jpeg, ResizeFormat::Png, ResizeFormat::WebP];

/// A number typed in a native EDIT control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// 해상도: the limit in pixels on the chosen side (긴 변, 가로 or 세로).
    Pixels,
    /// 용량: 최대 용량 in MB, one decimal.
    MaxSize,
    /// 용량: 긴 변 상한 in pixels, may be empty.
    Limit,
    /// 비율: percent.
    Percent,
}

pub const FIELDS: [Field; 4] = [Field::Pixels, Field::MaxSize, Field::Limit, Field::Percent];

/// A sidebar control, in the Tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctl {
    Tabs,
    Field(Field),
    Axis,
    Format,
    Quality,
    SaveMode,
    StripGps,
    List,
    Cancel,
    Save,
}

/// The sidebar's controls for `memory`'s tab and format, in the Tab order.
pub fn controls(memory: &ResizeMemory) -> Vec<Ctl> {
    let mut c = vec![Ctl::Tabs];
    match memory.tab {
        ResizeTab::Resolution => c.extend([Ctl::Field(Field::Pixels), Ctl::Axis]),
        ResizeTab::FileSize => c.extend([Ctl::Field(Field::MaxSize), Ctl::Field(Field::Limit)]),
        ResizeTab::Scale => c.push(Ctl::Field(Field::Percent)),
    }
    c.push(Ctl::Format);
    if memory.format != ResizeFormat::Png {
        c.push(Ctl::Quality);
    }
    c.extend([
        Ctl::SaveMode,
        Ctl::StripGps,
        Ctl::List,
        Ctl::Cancel,
        Ctl::Save,
    ]);
    c
}

/// The fields `memory`'s tab shows.
pub fn tab_fields(tab: ResizeTab) -> &'static [Field] {
    match tab {
        ResizeTab::Resolution => &[Field::Pixels],
        ResizeTab::FileSize => &[Field::MaxSize, Field::Limit],
        ResizeTab::Scale => &[Field::Percent],
    }
}

impl Field {
    /// The row label: the 해상도 field is named after its side.
    pub fn label(self, memory: &ResizeMemory) -> S {
        match self {
            Field::Pixels => match memory.axis {
                ResizeAxis::LongEdge => S::ResizeLongEdge,
                ResizeAxis::Width => S::ResizeWidth,
                ResizeAxis::Height => S::ResizeHeight,
            },
            Field::MaxSize => S::ResizeMaxSize,
            Field::Limit => S::ResizeLimit,
            Field::Percent => S::ResizeScale,
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Field::Pixels | Field::Limit => "px",
            Field::MaxSize => "MB",
            Field::Percent => "%",
        }
    }

    /// The EDIT control's accessible name: "긴 변 (px)", "긴 변 상한 (px) (선택)".
    pub fn uia_name(self, memory: &ResizeMemory, lang: Lang) -> String {
        let name = format!("{} ({})", text(lang, self.label(memory)), self.unit());
        if self == Field::Limit {
            format!("{name} {}", text(lang, S::ResizeOptional))
        } else {
            name
        }
    }

    /// The text the field shows for `memory`.
    pub fn text(self, memory: &ResizeMemory) -> String {
        match self {
            Field::Pixels => memory.long_edge.to_string(),
            Field::MaxSize => {
                let t = memory.max_mb_tenths;
                if t.is_multiple_of(10) {
                    (t / 10).to_string()
                } else {
                    format!("{}.{}", t / 10, t % 10)
                }
            }
            Field::Limit => memory
                .limit_long_edge
                .map(|e| e.to_string())
                .unwrap_or_default(),
            Field::Percent => memory.percent.to_string(),
        }
    }

    /// `memory` with `typed` in this field; `None` while the text is not a value the
    /// field takes (it then keeps its last value). Pixels 1..=65535, MB 0.1..=10000
    /// with one decimal, percent 1..=100; an empty 긴 변 상한 means none.
    pub fn parse(self, typed: &str, memory: &ResizeMemory) -> Option<ResizeMemory> {
        let typed = typed.trim();
        let px = |s: &str| s.parse::<u32>().ok().filter(|v| (1..=65_535).contains(v));
        let mut m = *memory;
        match self {
            Field::Pixels => m.long_edge = px(typed)?,
            Field::Limit if typed.is_empty() => m.limit_long_edge = None,
            Field::Limit => m.limit_long_edge = Some(px(typed)?),
            Field::Percent => {
                m.percent = typed.parse::<u8>().ok().filter(|v| (1..=100).contains(v))?;
            }
            Field::MaxSize => {
                let (whole, frac) = typed.split_once('.').unwrap_or((typed, ""));
                let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
                if !digits(whole) || !digits(frac) || frac.len() > 1 || typed == "." {
                    return None;
                }
                let whole: u32 = if whole.is_empty() {
                    0
                } else {
                    whole.parse().ok()?
                };
                let tenths = whole.checked_mul(10)? + frac.parse::<u32>().unwrap_or(0);
                m.max_mb_tenths = Some(tenths).filter(|t| (1..=100_000).contains(t))?;
            }
        }
        Some(m)
    }
}

impl Ctl {
    /// A choice's option count; 0 for the others.
    pub fn option_count(self) -> usize {
        match self {
            Ctl::Tabs | Ctl::Axis | Ctl::Format => 3,
            Ctl::Quality => QUALITIES.len(),
            Ctl::SaveMode => 2,
            _ => 0,
        }
    }

    /// A choice's label (the tabs are named 크기 조정).
    pub fn label(self) -> Option<S> {
        Some(match self {
            Ctl::Tabs => S::Resize,
            Ctl::Axis => S::ResizeBasis,
            Ctl::Format => S::ResizeFormat,
            Ctl::Quality => S::ResizeQuality,
            Ctl::SaveMode => S::ResizeSaveMode,
            Ctl::StripGps => S::ResizeMetadata,
            _ => return None,
        })
    }

    /// A choice's option labels in `lang`.
    pub fn option_labels(self, lang: Lang) -> Vec<String> {
        let keys = |keys: &[S]| keys.iter().map(|k| text(lang, *k).to_owned()).collect();
        match self {
            Ctl::Tabs => keys(&[S::ResizeResolution, S::ResizeFileSize, S::ResizeScale]),
            Ctl::Axis => keys(&[S::ResizeLongEdge, S::ResizeWidth, S::ResizeHeight]),
            Ctl::Format => vec!["JPEG".into(), "PNG".into(), "WebP".into()],
            Ctl::Quality => QUALITIES
                .iter()
                .map(|q| {
                    q.map_or_else(
                        || text(lang, S::ResizeMatchOriginal).into(),
                        |q| q.to_string(),
                    )
                })
                .collect(),
            Ctl::SaveMode => keys(&[S::ResizeNewFile, S::ResizeReplace]),
            _ => Vec::new(),
        }
    }

    /// The chosen option of a choice (`replace`: 원본 대체 is on).
    pub fn selected(self, m: &ResizeMemory, replace: bool) -> Option<usize> {
        match self {
            Ctl::Tabs => TABS.iter().position(|t| *t == m.tab),
            Ctl::Axis => AXES.iter().position(|a| *a == m.axis),
            Ctl::Format => FORMATS.iter().position(|f| *f == m.format),
            Ctl::Quality => QUALITIES.iter().position(|q| *q == m.quality),
            Ctl::SaveMode => Some(usize::from(replace)),
            _ => None,
        }
    }

    /// Whether option `i` can be chosen: 원본 대체 not while it is blocked (R3).
    pub fn option_enabled(self, i: usize, ctx: &Ctx) -> bool {
        let blocked = self == Ctl::SaveMode && i == 1 && ctx.blocked;
        !(ctx.saving || blocked)
    }
}

/// What the sidebar's state depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ctx {
    pub memory: ResizeMemory,
    pub replace: bool,
    /// 원본 대체 is blocked (R3).
    pub blocked: bool,
    pub saving: bool,
    pub count: usize,
    pub current: usize,
}

/// What the screen does after a sidebar input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Nothing,
    Redraw,
    /// New sidebar values (`resize::set_memory`).
    Memory(ResizeMemory),
    /// 원본 대체 on or off.
    Replace(bool),
    /// The keyboard focus goes to a field's EDIT control.
    FocusField(Field),
    GoTo(usize),
    Step(isize),
    /// [취소].
    Leave,
    /// [저장 (N장)].
    Save,
}

/// Picking option `i` of choice `c`.
fn pick(c: Ctl, i: usize, ctx: &Ctx) -> Action {
    if ctx.saving || c.selected(&ctx.memory, ctx.replace) == Some(i) || !c.option_enabled(i, ctx) {
        return Action::Redraw;
    }
    let mut m = ctx.memory;
    match c {
        Ctl::Tabs => m.tab = TABS[i],
        Ctl::Axis => m.axis = AXES[i],
        Ctl::Format => m.format = FORMATS[i],
        Ctl::Quality => m.quality = QUALITIES[i],
        Ctl::SaveMode => return Action::Replace(i == 1),
        _ => return Action::Nothing,
    }
    Action::Memory(m)
}

/// Space, Enter or a click on the checkbox or a button.
fn press(c: Ctl, ctx: &Ctx) -> Action {
    match c {
        Ctl::StripGps if !ctx.saving => Action::Memory(ResizeMemory {
            strip_gps: !ctx.memory.strip_gps,
            ..ctx.memory
        }),
        Ctl::Cancel if !ctx.saving => Action::Leave,
        Ctl::Save if !ctx.saving => Action::Save,
        Ctl::Field(f) => Action::FocusField(f),
        _ => Action::Redraw,
    }
}

/// What a press landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Option(Ctl, usize),
    Control(Ctl),
    Row(usize),
    Strip(usize),
    Nav(isize),
}

/// The focused control and the pressed part.
#[derive(Debug)]
pub struct Sidebar {
    pub focus: Ctl,
    pressed: Option<Hit>,
}

impl Default for Sidebar {
    /// The list has the focus at first: ← → and ↑ ↓ move through the photos.
    fn default() -> Sidebar {
        Sidebar {
            focus: Ctl::List,
            pressed: None,
        }
    }
}

impl Sidebar {
    /// A key in the main window while the screen is open; `None` when it is not the
    /// sidebar's (it then goes on to the viewer's commands: Esc, Ctrl+S, zoom, F11).
    /// `repeat`: an auto-repeat, which presses nothing.
    pub fn on_key(&mut self, vk: u16, mods: Mods, repeat: bool, ctx: &Ctx) -> Option<Action> {
        if mods.alt || mods.ctrl || vk == VK_ESCAPE {
            return None;
        }
        let order = controls(&ctx.memory);
        if !order.contains(&self.focus) {
            self.focus = Ctl::List;
        }
        if repeat && matches!(vk, VK_SPACE | VK_RETURN) {
            return Some(Action::Nothing);
        }
        let choice = self.focus.option_count() > 0;
        Some(match vk {
            VK_TAB => self.tab(mods.shift, &order),
            VK_LEFT | VK_RIGHT if choice => {
                let i = self.focus.selected(&ctx.memory, ctx.replace).unwrap_or(0);
                let next = if vk == VK_LEFT {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|&n| n < self.focus.option_count())
                };
                next.map_or(Action::Nothing, |n| pick(self.focus, n, ctx))
            }
            VK_LEFT => Action::Step(-1),
            VK_RIGHT => Action::Step(1),
            VK_UP if self.focus == Ctl::List => Action::Step(-1),
            VK_DOWN if self.focus == Ctl::List => Action::Step(1),
            // Enter saves only on [저장 (N장)] (and cancels on [취소]).
            VK_RETURN if matches!(self.focus, Ctl::Save | Ctl::Cancel) => press(self.focus, ctx),
            VK_SPACE if matches!(self.focus, Ctl::StripGps | Ctl::Save | Ctl::Cancel) => {
                press(self.focus, ctx)
            }
            // Enter does nothing else here (the viewer's fullscreen key is not this
            // screen's); ↑ ↓ away from the list neither.
            VK_RETURN | VK_SPACE | VK_UP | VK_DOWN => Action::Nothing,
            _ => return None,
        })
    }

    /// Tab or Shift+Tab: the next control, wrapping.
    pub fn tab(&mut self, back: bool, order: &[Ctl]) -> Action {
        let n = order.len();
        let i = order.iter().position(|c| *c == self.focus).unwrap_or(0);
        self.focus = order[if back { (i + n - 1) % n } else { (i + 1) % n }];
        match self.focus {
            Ctl::Field(f) => Action::FocusField(f),
            _ => Action::Redraw,
        }
    }

    /// Mouse down on `hit`; a control takes the focus.
    /// A mouse pick on a choice never gives it the keyboard focus (← → stay the photo
    /// keys; a choice takes them only once Tab reached it). It does take the focus
    /// from a field.
    pub fn press(&mut self, hit: Option<Hit>) {
        self.pressed = hit;
        match hit {
            Some(Hit::Control(c)) => self.focus = c,
            Some(Hit::Row(_)) => self.focus = Ctl::List,
            Some(Hit::Option(..)) if matches!(self.focus, Ctl::Field(_)) => {
                self.focus = Ctl::List;
            }
            _ => {}
        }
    }

    pub fn cancel_press(&mut self) {
        self.pressed = None;
    }

    /// A press is waiting for its release.
    pub fn pressing(&self) -> bool {
        self.pressed.is_some()
    }

    /// Mouse up over `hit`: acts only on the part that was pressed.
    pub fn release(&mut self, hit: Option<Hit>, ctx: &Ctx) -> Action {
        let Some(pressed) = self.pressed.take() else {
            return Action::Nothing;
        };
        if hit != Some(pressed) {
            return Action::Redraw;
        }
        self.act(pressed, ctx)
    }

    /// UI Automation invoked `hit` (as a click would; it takes the focus).
    pub fn invoke(&mut self, hit: Hit, ctx: &Ctx) -> Action {
        self.press(Some(hit));
        self.pressed = None;
        self.act(hit, ctx)
    }

    fn act(&mut self, hit: Hit, ctx: &Ctx) -> Action {
        match hit {
            Hit::Option(c, i) => pick(c, i, ctx),
            Hit::Control(c) => press(c, ctx),
            Hit::Row(i) | Hit::Strip(i) if i < ctx.count => Action::GoTo(i),
            Hit::Nav(d) => Action::Step(d),
            _ => Action::Redraw,
        }
    }
}

#[cfg(test)]
#[path = "sidebar_tests.rs"]
mod tests;
