//! In-window dialogs and toasts (spec §4.7, §5.5, §5.6; rulings R1, R12): state,
//! layout and hit testing (pure, in DIPs) and drawing through `Renderer`. No Win32
//! calls; the window routes input here first (`input::overlay`).
//!
//! Dialog keys: Enter always picks the default button (even with the focus on another
//! button or on the checkbox), Esc cancels, Tab/→ and Shift+Tab/← move the focus
//! (wrapping), Space toggles the focused checkbox or presses the focused button.

use std::path::PathBuf;

use lv_core::command::{Mods, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT};

use crate::about;
use crate::render::{Align, Renderer, TextRun};
use crate::strings::{t, text, tf, Lang, S};
use crate::theme::{Palette, Rgb};
use crate::toolbar::{pinned_bar_height, Rect};

const VK_TAB: u16 = 0x09;
const VK_SPACE: u16 = 0x20;

pub const TOAST_MS: u64 = 6000;

// Dialog layout (DIPs).
const CARD_MAX_W: f32 = 420.0;
const CARD_MARGIN: f32 = 16.0;
const CARD_PAD: f32 = 24.0;
const CARD_RADIUS: f32 = 18.0;
const TITLE_LINE_H: f32 = 22.0;
const DETAIL_GAP: f32 = 4.0;
const DETAIL_H: f32 = 20.0;
const SECTION_GAP: f32 = 16.0;
const BUTTONS_GAP: f32 = 20.0;
const ROW_H: f32 = 44.0;
const BUTTON_GAP: f32 = 8.0;
const BUTTON_RADIUS: f32 = 12.0;

// Toast layout (DIPs).
const TOAST_H: f32 = 52.0;
const TOAST_RADIUS: f32 = 14.0;
const TOAST_MARGIN: f32 = 16.0;
const TOAST_PAD_L: f32 = 16.0;
const TOAST_PAD_R: f32 = 8.0;
const ACTION_GAP: f32 = 12.0;
const PILL_H: f32 = 36.0;
const PILL_PAD: f32 = 14.0;
const HIT_MIN: f32 = 44.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    SaveRotation,
    PermanentDelete {
        name: String,
    },
    /// The About dialog (ruling R4).
    About,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptResult {
    Save,
    Discard,
    Cancel,
    Confirm,
    /// About: [오픈소스 고지].
    Notices,
    /// About: [닫기].
    Close,
}

/// A focusable part of a dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Checkbox,
    Button(PromptResult),
}

impl Prompt {
    pub fn title(&self) -> &'static str {
        match self {
            Prompt::SaveRotation => t(S::PromptSaveRotation),
            Prompt::PermanentDelete { .. } => t(S::PromptPermanentDelete),
            Prompt::About => t(S::AboutTitle),
        }
    }

    /// Focus order: top to bottom, then left to right (as drawn).
    pub fn items(&self) -> &'static [Item] {
        match self {
            Prompt::SaveRotation => &[
                Item::Checkbox,
                Item::Button(PromptResult::Save),
                Item::Button(PromptResult::Discard),
                Item::Button(PromptResult::Cancel),
            ],
            Prompt::PermanentDelete { .. } => &[
                Item::Button(PromptResult::Confirm),
                Item::Button(PromptResult::Cancel),
            ],
            Prompt::About => &[
                Item::Button(PromptResult::Notices),
                Item::Button(PromptResult::Close),
            ],
        }
    }

    /// The Enter button, focused first and drawn in highlighter. Permanent delete has
    /// no undo, so its default is [취소] (ruling R13).
    pub fn default_result(&self) -> PromptResult {
        match self {
            Prompt::SaveRotation => PromptResult::Save,
            Prompt::PermanentDelete { .. } => PromptResult::Cancel,
            Prompt::About => PromptResult::Close,
        }
    }

    /// Title lines until the title is measured (`Dialog::title_lines`): the delete
    /// question wraps.
    fn estimated_title_lines(&self) -> u32 {
        match self {
            Prompt::SaveRotation | Prompt::About => 1,
            Prompt::PermanentDelete { .. } => 2,
        }
    }

    /// Lines under the title in ink2: the file name, or About's version and license.
    pub fn details(&self) -> Vec<String> {
        match self {
            Prompt::SaveRotation => Vec::new(),
            Prompt::PermanentDelete { name } => vec![name.clone()],
            Prompt::About => vec![about::version_line(), about::license_line().to_owned()],
        }
    }
}

impl PromptResult {
    pub fn label(self) -> &'static str {
        match self {
            PromptResult::Save => t(S::BtnSave),
            PromptResult::Discard => t(S::BtnDiscard),
            PromptResult::Cancel => t(S::BtnCancel),
            PromptResult::Confirm => t(S::BtnDelete),
            PromptResult::Notices => t(S::BtnNotices),
            PromptResult::Close => t(S::BtnClose),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub prompt: Prompt,
    /// Index into `prompt.items()`.
    pub focused: usize,
    pub dont_ask: bool,
    /// The title's line count as last drawn (it depends on the language and the card
    /// width); `None` until then.
    pub title_lines: Option<u32>,
}

impl Dialog {
    /// Focus starts on the default button.
    pub fn new(prompt: Prompt) -> Dialog {
        let default = Item::Button(prompt.default_result());
        let focused = prompt
            .items()
            .iter()
            .position(|i| *i == default)
            .unwrap_or(0);
        Dialog {
            prompt,
            focused,
            dont_ask: false,
            title_lines: None,
        }
    }

    /// `Some` closes the dialog with that result.
    pub fn on_key(&mut self, vk: u16, mods: Mods) -> Option<PromptResult> {
        let n = self.prompt.items().len();
        match vk {
            VK_RETURN => return Some(self.prompt.default_result()),
            VK_ESCAPE => return Some(PromptResult::Cancel),
            VK_SPACE => return self.activate(self.focused),
            VK_TAB if mods.shift => self.focused = (self.focused + n - 1) % n,
            VK_LEFT => self.focused = (self.focused + n - 1) % n,
            VK_TAB | VK_RIGHT => self.focused = (self.focused + 1) % n,
            _ => {}
        }
        None
    }

    /// A click on an item focuses and activates it.
    pub fn on_click(&mut self, x: f32, y: f32, layout: &DialogLayout) -> Option<PromptResult> {
        let i = self.hit(x, y, layout)?;
        self.focused = i;
        self.activate(i)
    }

    /// The item at (x, y), as an index into `prompt.items()`.
    pub fn hit(&self, x: f32, y: f32, layout: &DialogLayout) -> Option<usize> {
        layout.items.iter().position(|r| r.contains(x, y))
    }

    fn activate(&mut self, i: usize) -> Option<PromptResult> {
        match self.prompt.items().get(i)? {
            Item::Checkbox => {
                self.dont_ask = !self.dont_ask;
                None
            }
            Item::Button(r) => Some(*r),
        }
    }
}

/// Dialog rects in DIPs; `items` follows `prompt.items()`.
#[derive(Clone, Debug, PartialEq)]
pub struct DialogLayout {
    pub card: Rect,
    pub title: Rect,
    /// One rect per `Prompt::details` line.
    pub details: Vec<Rect>,
    pub items: Vec<Rect>,
}

fn dialog_card_width(view_w: f32) -> f32 {
    (view_w - 2.0 * CARD_MARGIN).clamp(0.0, CARD_MAX_W)
}

/// The width the title wraps in.
pub fn dialog_inner_width(view_w: f32) -> f32 {
    (dialog_card_width(view_w) - 2.0 * CARD_PAD).max(0.0)
}

/// The card is at most 420 DIP wide (16 DIP from the window edges when narrower) and
/// centered in the client area. Inside its 24 DIP padding: the title, the detail lines,
/// the checkbox row, then the buttons in one row of equal widths, all 44 DIP tall.
pub fn dialog_layout(view_w: f32, view_h: f32, dialog: &Dialog) -> DialogLayout {
    let p = &dialog.prompt;
    let card_w = dialog_card_width(view_w);
    let inner_w = dialog_inner_width(view_w);
    let lines = dialog
        .title_lines
        .unwrap_or_else(|| p.estimated_title_lines())
        .max(1);
    let title_h = lines as f32 * TITLE_LINE_H;
    let detail_count = p.details().len();
    let has_checkbox = p.items().contains(&Item::Checkbox);
    let card_h = CARD_PAD
        + title_h
        + if detail_count > 0 {
            DETAIL_GAP + detail_count as f32 * DETAIL_H
        } else {
            0.0
        }
        + if has_checkbox {
            SECTION_GAP + ROW_H
        } else {
            0.0
        }
        + BUTTONS_GAP
        + ROW_H
        + CARD_PAD;
    let card = Rect {
        x: (view_w - card_w) / 2.0,
        y: (view_h - card_h) / 2.0,
        w: card_w,
        h: card_h,
    };
    let x = card.x + CARD_PAD;
    let mut y = card.y + CARD_PAD;
    let title = Rect {
        x,
        y,
        w: inner_w,
        h: title_h,
    };
    y += title_h;
    if detail_count > 0 {
        y += DETAIL_GAP;
    }
    let details = (0..detail_count)
        .map(|_| {
            let r = Rect {
                x,
                y,
                w: inner_w,
                h: DETAIL_H,
            };
            y += DETAIL_H;
            r
        })
        .collect();
    let checkbox = has_checkbox.then(|| {
        let r = Rect {
            x,
            y: y + SECTION_GAP,
            w: inner_w,
            h: ROW_H,
        };
        y = r.y + r.h;
        r
    });
    let buttons_y = y + BUTTONS_GAP;
    let count = p.items().len() - usize::from(has_checkbox);
    let button_w = (inner_w - BUTTON_GAP * (count as f32 - 1.0)) / count as f32;
    let mut bx = x;
    let items = p
        .items()
        .iter()
        .map(|item| match item {
            Item::Checkbox => checkbox.expect("checkbox row"),
            Item::Button(_) => {
                let r = Rect {
                    x: bx,
                    y: buttons_y,
                    w: button_w,
                    h: ROW_H,
                };
                bx += button_w + BUTTON_GAP;
                r
            }
        })
        .collect();
    DialogLayout {
        card,
        title,
        details,
        items,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastAction {
    Undo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    /// What `text` is made of (a string key and a file name after it), so a language
    /// change can rebuild it.
    source: (S, String),
    /// The highlighter pill's label and what it does.
    pub action: Option<(String, ToastAction)>,
    pub shown_at_ms: u64,
    pub duration_ms: u64,
    /// Where the toast was last drawn (for hit testing).
    parts: Option<ToastParts>,
}

impl Toast {
    pub fn new(key: S, now_ms: u64) -> Toast {
        Toast::with_suffix(key, "", now_ms)
    }

    /// `key`'s text followed by `suffix` (a file name).
    pub fn with_suffix(key: S, suffix: &str, now_ms: u64) -> Toast {
        Toast {
            text: tf(key, &[&suffix]),
            source: (key, suffix.to_owned()),
            action: None,
            shown_at_ms: now_ms,
            duration_ms: TOAST_MS,
            parts: None,
        }
    }

    /// "휴지통으로 옮겼어요 [실행 취소]".
    pub fn recycled(now_ms: u64) -> Toast {
        Toast {
            action: Some((t(S::ToastUndo).to_owned(), ToastAction::Undo)),
            ..Toast::new(S::ToastRecycled, now_ms)
        }
    }

    /// Rebuilds the text and the action's label in `lang`.
    pub fn retranslate(&mut self, lang: Lang) {
        let (key, suffix) = &self.source;
        self.text = format!("{}{suffix}", text(lang, *key));
        if let Some((label, action)) = &mut self.action {
            *label = match action {
                ToastAction::Undo => text(lang, S::ToastUndo).to_owned(),
            };
        }
    }

    pub fn visible(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.shown_at_ms) < self.duration_ms
    }

    /// The action under (x, y), once the toast has been placed (drawn).
    pub fn hit_action(&self, x: f32, y: f32) -> Option<ToastAction> {
        let hit = self.parts.as_ref()?.action?;
        let (_, action) = self.action.as_ref()?;
        hit.contains(x, y).then_some(*action)
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        self.parts.as_ref().is_some_and(|p| p.bg.contains(x, y))
    }

    /// Where the toast was last drawn.
    pub fn parts(&self) -> Option<ToastParts> {
        self.parts
    }

    pub fn place(&mut self, parts: ToastParts) {
        self.parts = Some(parts);
    }
}

/// Where toasts go: bottom-left, 16 DIP from the window edges and above a pinned
/// toolbar; `w` is the widest a toast may be.
pub fn toast_layout(view_w: f32, view_h: f32, pinned: bool) -> Rect {
    let bar = if pinned { pinned_bar_height() } else { 0.0 };
    Rect {
        x: TOAST_MARGIN,
        y: view_h - bar - TOAST_MARGIN - TOAST_H,
        w: (view_w - 2.0 * TOAST_MARGIN).max(0.0),
        h: TOAST_H,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToastParts {
    pub bg: Rect,
    pub text: Rect,
    pub pill: Option<Rect>,
    /// The pill's hit area: its width, 44 DIP tall.
    pub action: Option<Rect>,
}

/// Fits a toast to its measured text (and action label) inside `anchor`: 16 DIP left
/// padding, the text, then the pill 8 DIP from the right edge (16 without a pill).
/// Too long a text is clipped, never the pill.
pub fn toast_parts(anchor: Rect, text_w: f32, action_w: Option<f32>) -> ToastParts {
    let pill_w = action_w.map(|w| (w + 2.0 * PILL_PAD).max(HIT_MIN));
    let pad_r = if pill_w.is_some() {
        TOAST_PAD_R
    } else {
        TOAST_PAD_L
    };
    let fixed = TOAST_PAD_L + pad_r + pill_w.map_or(0.0, |w| ACTION_GAP + w);
    let bg = Rect {
        w: (fixed + text_w).min(anchor.w),
        ..anchor
    };
    let text = Rect {
        x: bg.x + TOAST_PAD_L,
        y: bg.y,
        w: (bg.w - fixed).max(0.0),
        h: bg.h,
    };
    let pill = pill_w.map(|w| Rect {
        x: bg.x + bg.w - pad_r - w,
        y: bg.y + (bg.h - PILL_H) / 2.0,
        w,
        h: PILL_H,
    });
    let action = pill.map(|p| Rect {
        y: bg.y + (bg.h - HIT_MIN) / 2.0,
        h: HIT_MIN,
        ..p
    });
    ToastParts {
        bg,
        text,
        pill,
        action,
    }
}

/// The photo a dialog asks about, given by its caller and handed back with the answer,
/// so the answer applies to that photo even if the current one changed meanwhile
/// (ruling R14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogTarget {
    pub path: PathBuf,
    /// `Viewer::generation()` when the dialog was shown.
    pub generation: u64,
}

/// What the window acts on when the overlay closes a dialog or a toast action is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    Dialog {
        prompt: Prompt,
        result: PromptResult,
        dont_ask: bool,
        target: DialogTarget,
    },
    Toast(ToastAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    DialogItem(usize),
    /// Anywhere else while a dialog is open (it is modal).
    Dialog,
    ToastAction,
    Toast,
}

/// At most one dialog and one toast; a new toast replaces the old one.
#[derive(Debug, Default)]
pub struct Overlay {
    pub dialog: Option<Dialog>,
    /// The open dialog's target.
    target: Option<DialogTarget>,
    pub toast: Option<Toast>,
    pressed: Option<Target>,
    /// The latest path forwarded by another launch while a dialog was open.
    deferred_open: Option<PathBuf>,
}

impl Overlay {
    pub fn show_dialog(&mut self, prompt: Prompt, target: DialogTarget) {
        self.dialog = Some(Dialog::new(prompt));
        self.target = Some(target);
        self.pressed = None;
    }

    /// While a dialog is open, keeps `path` (replacing any kept before) to open once
    /// the dialog closes, and returns true. Otherwise returns false: open it now.
    pub fn defer_open(&mut self, path: PathBuf) -> bool {
        if !self.dialog_open() {
            return false;
        }
        self.deferred_open = Some(path);
        true
    }

    /// The deferred path, once no dialog is open.
    pub fn take_deferred_open(&mut self) -> Option<PathBuf> {
        if self.dialog_open() {
            return None;
        }
        self.deferred_open.take()
    }

    pub fn show_toast(&mut self, toast: Toast) {
        self.toast = Some(toast);
        self.pressed = None;
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// After a language change: the toast's text in `lang` (a dialog's text is looked
    /// up as it is drawn).
    pub fn retranslate(&mut self, lang: Lang) {
        if let Some(toast) = &mut self.toast {
            toast.retranslate(lang);
        }
    }

    /// Drops the toast once its time is up; true if one was dropped.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        let expired = self.toast.as_ref().is_some_and(|t| !t.visible(now_ms));
        if expired {
            self.toast = None;
        }
        expired
    }

    /// A key while a dialog is open; `Some` once it closes.
    pub fn on_key(&mut self, vk: u16, mods: Mods) -> Option<OverlayEvent> {
        let result = self.dialog.as_mut()?.on_key(vk, mods)?;
        self.finish(result)
    }

    /// Activates the open dialog's item `i` as a click on it would (UI Automation):
    /// the item takes the focus; `Some` once the dialog closes.
    pub fn activate(&mut self, i: usize) -> Option<OverlayEvent> {
        let d = self.dialog.as_mut()?;
        if i >= d.prompt.items().len() {
            return None;
        }
        d.focused = i;
        let result = d.activate(i)?;
        self.finish(result)
    }

    /// Uses the toast's action (UI Automation's invoke on [실행 취소]): the toast goes.
    pub fn take_toast_action(&mut self) -> Option<OverlayEvent> {
        let action = self.toast.as_ref()?.action.as_ref()?.1;
        self.toast = None;
        self.pressed = None;
        Some(OverlayEvent::Toast(action))
    }

    /// Mouse down at (x, y) in a `view`-sized client area; true if the overlay takes
    /// it (always while a dialog is open).
    pub fn press(&mut self, x: f32, y: f32, view: (f32, f32), now_ms: u64) -> bool {
        self.pressed = self.target_at(x, y, view, now_ms);
        self.pressed.is_some()
    }

    /// Mouse up: (taken, event). It acts only when released on what was pressed.
    pub fn release(
        &mut self,
        x: f32,
        y: f32,
        view: (f32, f32),
        now_ms: u64,
    ) -> (bool, Option<OverlayEvent>) {
        let pressed = self.pressed.take();
        let taken = pressed.is_some() || self.dialog_open();
        if pressed.is_none() || pressed != self.target_at(x, y, view, now_ms) {
            return (taken, None);
        }
        let event = match pressed {
            Some(Target::DialogItem(_)) => {
                let d = self.dialog.as_mut().expect("dialog target");
                let layout = dialog_layout(view.0, view.1, d);
                let result = d.on_click(x, y, &layout);
                result.and_then(|r| self.finish(r))
            }
            Some(Target::ToastAction) => {
                let t = self.toast.take().expect("toast target");
                t.action.map(|(_, a)| OverlayEvent::Toast(a))
            }
            _ => None,
        };
        (taken, event)
    }

    fn target_at(&self, x: f32, y: f32, (w, h): (f32, f32), now_ms: u64) -> Option<Target> {
        if let Some(d) = &self.dialog {
            let layout = dialog_layout(w, h, d);
            return Some(
                d.hit(x, y, &layout)
                    .map_or(Target::Dialog, Target::DialogItem),
            );
        }
        let t = self.toast.as_ref().filter(|t| t.visible(now_ms))?;
        if t.hit_action(x, y).is_some() {
            Some(Target::ToastAction)
        } else {
            t.contains(x, y).then_some(Target::Toast)
        }
    }

    fn finish(&mut self, result: PromptResult) -> Option<OverlayEvent> {
        let d = self.dialog.take()?;
        let target = self.target.take()?;
        self.pressed = None;
        Some(OverlayEvent::Dialog {
            prompt: d.prompt,
            result,
            dont_ask: d.dont_ask,
            target,
        })
    }
}

// ---- Drawing -------------------------------------------------------------------

const UI_FAMILY: &str = "Segoe UI";
const TITLE_SIZE: f32 = 15.0;
const TEXT_SIZE: f32 = 14.0;
const DETAIL_SIZE: f32 = 13.0;
const CHECK_BOX: f32 = 20.0;
const CHECK_GLYPH: char = '\u{E73E}';
const FOCUS_OUTSET: f32 = 3.0;

/// Scrim over the photo: light `ink` at 30%, dark `paper` at 60%.
pub fn scrim(p: &Palette, dark: bool) -> (Rgb, f32) {
    if dark {
        (p.paper, 0.6)
    } else {
        (p.ink, 0.3)
    }
}

/// Toast background, text and optional border: inverted ink (ink + paper) in light
/// mode; surface2 + ink with a hairline in dark mode. The dark pair deliberately departs
/// from the brief's literal "inverted" wording (light ink background with dark text
/// would glare on a dark window); it is contrast-tested at >= 4.5:1 (ruling R15).
pub fn toast_colors(p: &Palette, dark: bool) -> (Rgb, Rgb, Option<Rgb>) {
    if dark {
        (p.surface2, p.ink, Some(p.hairline))
    } else {
        (p.ink, p.paper, None)
    }
}

/// Everything the overlay draws besides its own state.
pub struct OverlayView<'a> {
    /// Client area size in DIPs.
    pub client: (f32, f32),
    pub pinned: bool,
    pub palette: &'a Palette,
    pub dark: bool,
    pub icon_family: &'a str,
    pub now_ms: u64,
}

fn tuple(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
}

fn outset(r: Rect, dx: f32, dy: f32) -> Rect {
    Rect {
        x: r.x - dx,
        y: r.y - dy,
        w: r.w + 2.0 * dx,
        h: r.h + 2.0 * dy,
    }
}

fn ui_run(text: &str, color: Rgb, size: f32, bold: bool) -> TextRun<'_> {
    TextRun {
        text,
        family: UI_FAMILY,
        size,
        bold,
        color,
    }
}

/// The toast, then the dialog above it. Places the toast for hit testing.
pub fn draw(r: &mut Renderer, o: &mut Overlay, v: &OverlayView) {
    if let Some(t) = o.toast.as_mut().filter(|t| t.visible(v.now_ms)) {
        draw_toast(r, t, v);
    }
    if let Some(d) = &mut o.dialog {
        // Measured first: the layout (and later hit tests) use the line count.
        let width = dialog_inner_width(v.client.0);
        if let Some(n) = r.wrapped_line_count(d.prompt.title(), UI_FAMILY, TITLE_SIZE, width) {
            d.title_lines = Some(n);
        }
        draw_dialog(r, d, v);
    }
}

fn draw_toast(r: &mut Renderer, t: &mut Toast, v: &OverlayView) {
    let p = v.palette;
    let (bg, fg, border) = toast_colors(p, v.dark);
    let anchor = toast_layout(v.client.0, v.client.1, v.pinned);
    let text = [ui_run(&t.text, fg, TEXT_SIZE, false)];
    let label = t
        .action
        .as_ref()
        .map(|(l, _)| [ui_run(l, p.on_highlighter, TEXT_SIZE, true)]);
    let text_w = r.measure_runs(&text).unwrap_or(anchor.w);
    let action_w = label
        .as_ref()
        .map(|l| r.measure_runs(l).unwrap_or(2.0 * HIT_MIN));
    let parts = toast_parts(anchor, text_w, action_w);
    if !v.dark {
        r.fill_soft_shadow(tuple(parts.bg), TOAST_RADIUS, p.ink, (4.0, 14.0, 0.08));
    }
    r.fill_round_rect(tuple(parts.bg), TOAST_RADIUS, bg);
    if let Some(c) = border {
        r.stroke_round_rect(tuple(parts.bg), TOAST_RADIUS, c, 1.0);
    }
    r.draw_runs_ellipsis(tuple(parts.text), &text, Align::Leading);
    if let (Some(pill), Some(label)) = (parts.pill, label) {
        r.fill_round_rect(tuple(pill), pill.h / 2.0, p.highlighter);
        r.draw_runs(tuple(pill), &label, Align::Center);
    }
    t.place(parts);
}

fn draw_dialog(r: &mut Renderer, d: &Dialog, v: &OverlayView) {
    let p = v.palette;
    let (w, h) = v.client;
    let l = dialog_layout(w, h, d);
    let (scrim_color, alpha) = scrim(p, v.dark);
    r.fill_rect_alpha((0.0, 0.0, w, h), scrim_color, alpha);
    // Light: soft shadow. Dark: no shadow; a hairline border lifts the card.
    if !v.dark {
        r.fill_soft_shadow(tuple(l.card), CARD_RADIUS, p.ink, (8.0, 24.0, 0.12));
    }
    r.fill_round_rect(tuple(l.card), CARD_RADIUS, p.surface);
    if v.dark {
        r.stroke_round_rect(tuple(l.card), CARD_RADIUS, p.hairline, 1.0);
    }
    r.draw_wrapped(
        tuple(l.title),
        d.prompt.title(),
        UI_FAMILY,
        TITLE_SIZE,
        p.ink,
    );
    for (rect, detail) in l.details.iter().zip(d.prompt.details()) {
        let run = [ui_run(&detail, p.ink2, DETAIL_SIZE, false)];
        r.draw_runs_ellipsis(tuple(*rect), &run, Align::Leading);
    }
    for (i, (item, rect)) in d.prompt.items().iter().zip(&l.items).enumerate() {
        let focused = i == d.focused;
        match item {
            Item::Checkbox => draw_checkbox(r, *rect, d.dont_ask, focused, v),
            Item::Button(res) => {
                let default = *res == d.prompt.default_result();
                draw_button(r, *rect, res.label(), default, focused, p);
            }
        }
    }
}

fn draw_button(
    r: &mut Renderer,
    rect: Rect,
    label: &str,
    default: bool,
    focused: bool,
    p: &Palette,
) {
    let (fill, color) = if default {
        (p.highlighter, p.on_highlighter)
    } else {
        (p.surface2, p.ink)
    };
    r.fill_round_rect(tuple(rect), BUTTON_RADIUS, fill);
    r.draw_runs(
        tuple(rect),
        &[ui_run(label, color, TEXT_SIZE, default)],
        Align::Center,
    );
    if focused {
        let ring = outset(rect, FOCUS_OUTSET, FOCUS_OUTSET);
        r.stroke_round_rect(tuple(ring), BUTTON_RADIUS + FOCUS_OUTSET, p.ink, 1.5);
    }
}

fn draw_checkbox(r: &mut Renderer, row: Rect, checked: bool, focused: bool, v: &OverlayView) {
    let p = v.palette;
    let mark = (
        row.x,
        row.y + (row.h - CHECK_BOX) / 2.0,
        CHECK_BOX,
        CHECK_BOX,
    );
    if checked {
        r.fill_round_rect(mark, 5.0, p.highlighter);
        r.draw_glyph(
            mark,
            CHECK_GLYPH,
            v.icon_family,
            14.0,
            p.on_highlighter,
            false,
        );
    } else {
        r.stroke_round_rect(mark, 5.0, p.ink2, 1.5);
    }
    let label_x = row.x + CHECK_BOX + 10.0;
    let label = (label_x, row.y, row.x + row.w - label_x, row.h);
    r.draw_runs(
        label,
        &[ui_run(t(S::CheckboxAutoSave), p.ink, TEXT_SIZE, false)],
        Align::Leading,
    );
    if focused {
        let ring = outset(row, 8.0, 0.0);
        r.stroke_round_rect(tuple(ring), 10.0, p.ink, 1.5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{contrast, DARK, LIGHT};
    use std::path::PathBuf;

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
    const VIEW: (f32, f32) = (1000.0, 700.0);

    fn save() -> Dialog {
        Dialog::new(Prompt::SaveRotation)
    }

    fn delete() -> Dialog {
        Dialog::new(Prompt::PermanentDelete {
            name: "a.jpg".into(),
        })
    }

    fn focused_item(d: &Dialog) -> Item {
        d.prompt.items()[d.focused]
    }

    fn center(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn items_and_default_focus() {
        let d = save();
        assert_eq!(
            d.prompt.items(),
            [
                Item::Checkbox,
                Item::Button(PromptResult::Save),
                Item::Button(PromptResult::Discard),
                Item::Button(PromptResult::Cancel)
            ]
        );
        assert_eq!(focused_item(&d), Item::Button(PromptResult::Save));
        assert!(!d.dont_ask);
        let d = delete();
        assert_eq!(
            d.prompt.items(),
            [
                Item::Button(PromptResult::Confirm),
                Item::Button(PromptResult::Cancel)
            ]
        );
        // Permanent delete: [취소] is the default (R13).
        assert_eq!(focused_item(&d), Item::Button(PromptResult::Cancel));
        assert_eq!(d.prompt.default_result(), PromptResult::Cancel);
    }

    #[test]
    fn texts_are_verbatim() {
        assert_eq!(Prompt::SaveRotation.title(), "회전한 내용을 저장할까요?");
        assert_eq!(
            delete().prompt.title(),
            "이 위치는 휴지통이 없어 영구 삭제돼요. 삭제할까요?"
        );
        assert_eq!(PromptResult::Save.label(), "저장");
        assert_eq!(PromptResult::Discard.label(), "저장 안 함");
        assert_eq!(PromptResult::Cancel.label(), "취소");
        assert_eq!(PromptResult::Confirm.label(), "삭제");
        assert_eq!(t(S::CheckboxAutoSave), "다음부터 묻지 않고 바로 저장");
        assert_eq!(t(S::ToastRecycled), "휴지통으로 옮겼어요");
        assert_eq!(t(S::ToastUndo), "실행 취소");
        assert_eq!(t(S::ToastSavedAsTag), "이 사진은 회전 정보로 저장했어요");
        assert_eq!(
            t(S::ToastFileChanged),
            "파일이 바뀌었어요, 다시 열어 주세요"
        );
        assert_eq!(t(S::ToastSaveFailed), "저장하지 못했어요");
        assert_eq!(
            t(S::ToastUnsupportedFormat),
            "이 형식은 아직 회전 저장을 지원하지 않아요"
        );
    }

    #[test]
    fn enter_is_the_default_button_wherever_the_focus_is() {
        let mut d = save();
        assert_eq!(d.on_key(VK_RETURN, NONE), Some(PromptResult::Save));
        d.focused = 3; // Cancel
        assert_eq!(d.on_key(VK_RETURN, NONE), Some(PromptResult::Save));
        d.focused = 0; // checkbox: Enter does not toggle it
        assert_eq!(d.on_key(VK_RETURN, NONE), Some(PromptResult::Save));
        assert!(!d.dont_ask);
        let mut d = delete();
        assert_eq!(d.on_key(VK_RETURN, NONE), Some(PromptResult::Cancel));
        d.focused = 0; // [삭제]: Enter still picks the default, [취소]
        assert_eq!(d.on_key(VK_RETURN, NONE), Some(PromptResult::Cancel));
    }

    #[test]
    fn escape_cancels() {
        assert_eq!(save().on_key(VK_ESCAPE, NONE), Some(PromptResult::Cancel));
        assert_eq!(delete().on_key(VK_ESCAPE, NONE), Some(PromptResult::Cancel));
    }

    #[test]
    fn tab_and_arrows_move_focus_with_wrapping() {
        let mut d = save(); // focus 1 (Save)
        assert_eq!(d.on_key(VK_TAB, NONE), None);
        assert_eq!(d.focused, 2);
        assert_eq!(d.on_key(VK_RIGHT, NONE), None);
        assert_eq!(d.focused, 3);
        d.on_key(VK_TAB, NONE);
        assert_eq!(d.focused, 0); // wraps to the checkbox
        d.on_key(VK_TAB, SHIFT);
        assert_eq!(d.focused, 3);
        d.on_key(VK_LEFT, NONE);
        assert_eq!(d.focused, 2);
        let mut d = delete(); // focus 1 (Cancel)
        d.on_key(VK_LEFT, NONE);
        assert_eq!(d.focused, 0);
        d.on_key(VK_RIGHT, NONE);
        assert_eq!(d.focused, 1);
        d.on_key(VK_TAB, NONE);
        assert_eq!(d.focused, 0);
    }

    #[test]
    fn space_toggles_the_focused_checkbox_or_presses_the_focused_button() {
        let mut d = save();
        d.focused = 0;
        assert_eq!(d.on_key(VK_SPACE, NONE), None);
        assert!(d.dont_ask);
        assert_eq!(d.on_key(VK_SPACE, NONE), None);
        assert!(!d.dont_ask);
        d.focused = 2;
        assert_eq!(d.on_key(VK_SPACE, NONE), Some(PromptResult::Discard));
        // Space on the delete dialog's initial focus cancels.
        assert_eq!(delete().on_key(VK_SPACE, NONE), Some(PromptResult::Cancel));
        let mut d = delete();
        d.focused = 0;
        assert_eq!(d.on_key(VK_SPACE, NONE), Some(PromptResult::Confirm));
    }

    #[test]
    fn other_keys_do_nothing() {
        let mut d = save();
        let before = d.clone();
        assert_eq!(d.on_key(0x41, NONE), None); // A
        assert_eq!(d.on_key(0x2E, NONE), None); // Delete
        assert_eq!(d, before);
    }

    #[test]
    fn clicks_hit_buttons_and_the_checkbox() {
        let mut d = save();
        let l = dialog_layout(VIEW.0, VIEW.1, &d);
        let (x, y) = center(l.items[2]);
        assert_eq!(d.on_click(x, y, &l), Some(PromptResult::Discard));
        let (x, y) = center(l.items[0]);
        assert_eq!(d.on_click(x, y, &l), None);
        assert!(d.dont_ask);
        assert_eq!(d.focused, 0);
        let (x, y) = center(l.items[1]);
        assert_eq!(d.on_click(x, y, &l), Some(PromptResult::Save));
        // Outside the items (title, card padding, scrim): nothing.
        let (x, y) = center(l.title);
        assert_eq!(d.on_click(x, y, &l), None);
        assert_eq!(d.on_click(5.0, 5.0, &l), None);
        assert!(d.dont_ask);
    }

    #[test]
    fn dialog_card_is_centered_420_wide_with_44_dip_targets() {
        for d in [save(), delete(), Dialog::new(Prompt::About)] {
            let l = dialog_layout(VIEW.0, VIEW.1, &d);
            assert_eq!(l.items.len(), d.prompt.items().len());
            assert_eq!(l.card.w, 420.0);
            assert!((l.card.x + l.card.w / 2.0 - 500.0).abs() < 0.01);
            assert!((l.card.y + l.card.h / 2.0 - 350.0).abs() < 0.01);
            for r in &l.items {
                assert!(r.h >= 44.0 && r.w >= 44.0, "{r:?}");
                // Inside the card's 24 DIP padding.
                assert!(r.x >= l.card.x + 24.0 - 0.01);
                assert!(r.x + r.w <= l.card.x + l.card.w - 24.0 + 0.01);
                assert!(r.y + r.h <= l.card.y + l.card.h - 24.0 + 0.01);
            }
            assert_eq!(l.title.y, l.card.y + 24.0);
            assert_eq!(l.title.x, l.card.x + 24.0);
            // No overlaps between items.
            for (i, a) in l.items.iter().enumerate() {
                for b in &l.items[i + 1..] {
                    let apart = a.x + a.w <= b.x
                        || b.x + b.w <= a.x
                        || a.y + a.h <= b.y
                        || b.y + b.h <= a.y;
                    assert!(apart, "{a:?} {b:?}");
                }
            }
        }
        let d = delete();
        assert_eq!(dialog_layout(VIEW.0, VIEW.1, &d).details.len(), 1);
        assert!(dialog_layout(VIEW.0, VIEW.1, &save()).details.is_empty());
    }

    #[test]
    fn measured_title_lines_grow_the_card() {
        // The English delete question can take three lines: the card grows and the
        // buttons stay inside it, under the title.
        let mut d = delete();
        assert_eq!(d.title_lines, None);
        let two = dialog_layout(VIEW.0, VIEW.1, &d);
        assert_eq!(two.title.h, 2.0 * 22.0);
        d.title_lines = Some(3);
        let three = dialog_layout(VIEW.0, VIEW.1, &d);
        assert_eq!(three.title.h, 3.0 * 22.0);
        assert_eq!(three.card.h, two.card.h + 22.0);
        for r in &three.items {
            assert!(r.y >= three.title.y + three.title.h);
            assert!(r.y + r.h <= three.card.y + three.card.h - 24.0 + 0.01);
        }
        // The title is measured in the card's inner width.
        assert_eq!(dialog_inner_width(VIEW.0), 420.0 - 48.0);
        assert_eq!(dialog_inner_width(300.0), 300.0 - 32.0 - 48.0);
    }

    #[test]
    fn dialog_card_shrinks_in_a_narrow_window() {
        let l = dialog_layout(400.0, 500.0, &save());
        assert_eq!(l.card.x, 16.0);
        assert_eq!(l.card.w, 400.0 - 32.0);
        for r in &l.items {
            assert!(r.h >= 44.0 && r.w >= 44.0);
        }
    }

    #[test]
    fn about_shows_version_and_license_with_notices_and_close() {
        use crate::strings::{text, Lang};
        let mut d = Dialog::new(Prompt::About);
        assert_eq!(d.prompt.title(), t(S::AboutTitle));
        assert_eq!(
            d.prompt.details(),
            [
                format!("{}{}", t(S::AboutVersion), env!("CARGO_PKG_VERSION")),
                t(S::AboutLicense).to_owned()
            ]
        );
        assert_eq!(text(Lang::Ko, S::AboutTitle), "light-view");
        assert_eq!(text(Lang::Ko, S::AboutVersion), "버전 ");
        assert_eq!(text(Lang::Ko, S::BtnNotices), "오픈소스 고지");
        assert_eq!(text(Lang::Ko, S::BtnClose), "닫기");
        assert_eq!(
            d.prompt.items(),
            [
                Item::Button(PromptResult::Notices),
                Item::Button(PromptResult::Close)
            ]
        );
        assert_eq!(PromptResult::Notices.label(), t(S::BtnNotices));
        assert_eq!(PromptResult::Close.label(), t(S::BtnClose));
        // [닫기] is the default: Enter and Esc both close, Space on 고지 opens them.
        assert_eq!(focused_item(&d), Item::Button(PromptResult::Close));
        assert_eq!(d.on_key(VK_RETURN, NONE), Some(PromptResult::Close));
        d.on_key(VK_LEFT, NONE);
        assert_eq!(d.on_key(VK_SPACE, NONE), Some(PromptResult::Notices));
        // Two detail lines, one under the other, above the buttons.
        let l = dialog_layout(VIEW.0, VIEW.1, &d);
        assert_eq!(l.details.len(), 2);
        assert_eq!(l.details[1].y, l.details[0].y + l.details[0].h);
        assert!(l.details[1].y + l.details[1].h <= l.items[0].y);
    }

    #[test]
    fn toast_text_follows_the_language() {
        use crate::strings::{text, Lang};
        let mut o = Overlay::default();
        o.show_toast(Toast::with_suffix(S::ToastRecycleFailed, "a.jpg", 0));
        o.retranslate(Lang::En);
        assert_eq!(
            o.toast.as_ref().unwrap().text,
            "Couldn't move to Recycle Bin: a.jpg"
        );
        o.show_toast(Toast::recycled(0));
        o.retranslate(Lang::En);
        let toast = o.toast.as_ref().unwrap();
        assert_eq!(toast.text, "Moved to Recycle Bin");
        assert_eq!(toast.action.as_ref().unwrap().0, "Undo");
        o.retranslate(Lang::Ko);
        let toast = o.toast.as_ref().unwrap();
        assert_eq!(toast.text, text(Lang::Ko, S::ToastRecycled));
        assert_eq!(toast.action.as_ref().unwrap().0, "실행 취소");
        assert_eq!(Toast::new(S::ToastCopied, 0).text, t(S::ToastCopied));
    }

    #[test]
    fn toast_lasts_six_seconds() {
        let toast = Toast::new(S::ToastSaveFailed, 1_000);
        assert_eq!(toast.duration_ms, 6000);
        assert!(toast.visible(1_000));
        assert!(toast.visible(6_999));
        assert!(!toast.visible(7_000));
        let mut o = Overlay::default();
        o.show_toast(toast);
        assert!(!o.expire(6_999));
        assert!(o.toast.is_some());
        assert!(o.expire(7_000));
        assert!(o.toast.is_none());
        assert!(!o.expire(8_000));
    }

    #[test]
    fn a_new_toast_replaces_the_old_one() {
        let mut o = Overlay::default();
        o.show_toast(Toast::recycled(0));
        o.show_toast(Toast::new(S::ToastFileChanged, 5_000));
        let toast = o.toast.as_ref().unwrap();
        assert_eq!(toast.text, t(S::ToastFileChanged));
        assert_eq!(toast.action, None);
        // The new toast's own six seconds count.
        assert!(!o.expire(10_999));
        assert!(o.expire(11_000));
    }

    #[test]
    fn toast_sits_bottom_left_and_above_the_pinned_bar() {
        let floating = toast_layout(1000.0, 700.0, false);
        assert_eq!(floating.x, 16.0);
        assert_eq!(floating.y + floating.h, 700.0 - 16.0);
        assert_eq!(floating.h, 52.0);
        assert_eq!(floating.w, 1000.0 - 32.0);
        let pinned = toast_layout(1000.0, 700.0, true);
        assert_eq!(pinned.x, 16.0);
        assert_eq!(pinned.y + pinned.h, 700.0 - 56.0 - 16.0);
    }

    #[test]
    fn toast_fits_its_text_and_pill() {
        let anchor = toast_layout(1000.0, 700.0, false);
        let p = toast_parts(anchor, 120.0, Some(56.0));
        let pill = p.pill.unwrap();
        assert_eq!(pill.w, 56.0 + 28.0);
        assert_eq!(p.bg.w, 16.0 + 120.0 + 12.0 + 84.0 + 8.0);
        assert_eq!(pill.x + pill.w, p.bg.x + p.bg.w - 8.0);
        assert_eq!(p.text.x, p.bg.x + 16.0);
        assert_eq!(p.text.w, 120.0);
        let hit = p.action.unwrap();
        assert!(hit.h >= 44.0 && hit.w >= 44.0);
        assert!(hit.y >= p.bg.y && hit.y + hit.h <= p.bg.y + p.bg.h);
        // Without an action: 16 DIP on both sides.
        let p = toast_parts(anchor, 120.0, None);
        assert_eq!(p.bg.w, 152.0);
        assert!(p.pill.is_none() && p.action.is_none());
        // Too long: the toast stops at the anchor and the text is clipped.
        let p = toast_parts(anchor, 5000.0, Some(10.0));
        assert_eq!(p.bg.w, anchor.w);
        assert_eq!(p.pill.unwrap().w, 44.0);
        assert!(p.text.x + p.text.w <= p.pill.unwrap().x);
    }

    #[test]
    fn toast_action_hit() {
        let mut undo = Toast::recycled(0);
        assert_eq!(undo.hit_action(10.0, 10.0), None); // not placed yet
        let parts = toast_parts(toast_layout(1000.0, 700.0, false), 120.0, Some(56.0));
        undo.place(parts);
        let (x, y) = center(parts.action.unwrap());
        assert_eq!(undo.hit_action(x, y), Some(ToastAction::Undo));
        let (x, y) = center(parts.text);
        assert_eq!(undo.hit_action(x, y), None);
        let mut plain = Toast::new(S::ToastSaveFailed, 0);
        plain.place(parts);
        let (x, y) = center(parts.action.unwrap());
        assert_eq!(plain.hit_action(x, y), None);
    }

    #[test]
    fn overlay_keys_close_the_dialog_with_its_result() {
        let mut o = Overlay::default();
        assert_eq!(o.on_key(VK_RETURN, NONE), None); // no dialog
        o.show_dialog(Prompt::SaveRotation, target(7));
        assert!(o.dialog_open());
        assert_eq!(o.on_key(VK_TAB, SHIFT), None); // to the checkbox
        assert_eq!(o.on_key(VK_SPACE, NONE), None);
        assert_eq!(
            o.on_key(VK_RETURN, NONE),
            Some(OverlayEvent::Dialog {
                prompt: Prompt::SaveRotation,
                result: PromptResult::Save,
                dont_ask: true,
                target: target(7),
            })
        );
        assert!(!o.dialog_open());
    }

    #[test]
    fn dialog_takes_every_click_and_acts_on_release_over_the_pressed_item() {
        let mut o = Overlay::default();
        o.show_dialog(Prompt::SaveRotation, target(1));
        let l = dialog_layout(VIEW.0, VIEW.1, o.dialog.as_ref().unwrap());
        // Scrim clicks are taken but do nothing.
        assert!(o.press(5.0, 5.0, VIEW, 0));
        assert_eq!(o.release(5.0, 5.0, VIEW, 0), (true, None));
        // Pressed on Cancel, released on Discard: nothing.
        let (cx, cy) = center(l.items[3]);
        let (dx, dy) = center(l.items[2]);
        assert!(o.press(cx, cy, VIEW, 0));
        assert_eq!(o.release(dx, dy, VIEW, 0), (true, None));
        assert!(o.dialog_open());
        assert!(o.press(cx, cy, VIEW, 0));
        assert_eq!(
            o.release(cx, cy, VIEW, 0),
            (
                true,
                Some(OverlayEvent::Dialog {
                    prompt: Prompt::SaveRotation,
                    result: PromptResult::Cancel,
                    dont_ask: false,
                    target: target(1),
                })
            )
        );
        assert!(!o.dialog_open());
    }

    fn target(generation: u64) -> DialogTarget {
        DialogTarget {
            path: PathBuf::from(format!(r"C:\photos\{generation}.jpg")),
            generation,
        }
    }

    #[test]
    fn the_dialog_target_comes_back_with_the_answer() {
        let mut o = Overlay::default();
        let name = "b.jpg".to_owned();
        o.show_dialog(Prompt::PermanentDelete { name: name.clone() }, target(42));
        assert_eq!(
            o.on_key(VK_ESCAPE, NONE),
            Some(OverlayEvent::Dialog {
                prompt: Prompt::PermanentDelete { name },
                result: PromptResult::Cancel,
                dont_ask: false,
                target: target(42),
            })
        );
        // A second dialog carries its own target, not the first one's.
        o.show_dialog(Prompt::SaveRotation, target(43));
        let Some(OverlayEvent::Dialog { target: t, .. }) = o.on_key(VK_RETURN, NONE) else {
            panic!("dialog answer expected");
        };
        assert_eq!(t, target(43));
    }

    #[test]
    fn forwarded_opens_wait_for_the_dialog() {
        let mut o = Overlay::default();
        // No dialog: open right away, nothing kept.
        assert!(!o.defer_open(PathBuf::from("a.jpg")));
        assert_eq!(o.take_deferred_open(), None);
        o.show_dialog(Prompt::SaveRotation, target(1));
        assert!(o.defer_open(PathBuf::from("b.jpg")));
        assert!(o.defer_open(PathBuf::from("c.jpg"))); // the latest wins
        assert_eq!(o.take_deferred_open(), None); // still open
        o.on_key(VK_ESCAPE, NONE);
        assert_eq!(o.take_deferred_open(), Some(PathBuf::from("c.jpg")));
        assert_eq!(o.take_deferred_open(), None);
    }

    #[test]
    fn activation_answers_the_dialog_and_uses_the_toast_action() {
        let target = DialogTarget {
            path: PathBuf::from("a.jpg"),
            generation: 1,
        };
        let mut o = Overlay::default();
        o.show_dialog(Prompt::SaveRotation, target.clone());
        // The checkbox toggles and takes the focus; the dialog stays.
        assert_eq!(o.activate(0), None);
        let d = o.dialog.as_ref().unwrap();
        assert!(d.dont_ask);
        assert_eq!(d.focused, 0);
        assert_eq!(o.activate(9), None, "out of range");
        // A button answers with the checkbox's state.
        assert_eq!(
            o.activate(2),
            Some(OverlayEvent::Dialog {
                prompt: Prompt::SaveRotation,
                result: PromptResult::Discard,
                dont_ask: true,
                target,
            })
        );
        assert!(!o.dialog_open());
        assert_eq!(o.activate(0), None, "no dialog");
        // A toast without an action stays; [실행 취소] is used once.
        o.show_toast(Toast::new(S::ToastCopied, 0));
        assert_eq!(o.take_toast_action(), None);
        assert!(o.toast.is_some());
        o.show_toast(Toast::recycled(0));
        assert_eq!(
            o.take_toast_action(),
            Some(OverlayEvent::Toast(ToastAction::Undo))
        );
        assert!(o.toast.is_none());
    }

    #[test]
    fn toast_clicks() {
        let mut o = Overlay::default();
        let mut t = Toast::recycled(0);
        let parts = toast_parts(toast_layout(VIEW.0, VIEW.1, false), 120.0, Some(56.0));
        t.place(parts);
        o.show_toast(t);
        // Away from the toast: not taken.
        assert!(!o.press(500.0, 300.0, VIEW, 0));
        assert_eq!(o.release(500.0, 300.0, VIEW, 0), (false, None));
        // On the text: taken, no action.
        let (x, y) = center(parts.text);
        assert!(o.press(x, y, VIEW, 0));
        assert_eq!(o.release(x, y, VIEW, 0), (true, None));
        // Expired toasts take nothing.
        let (ax, ay) = center(parts.action.unwrap());
        assert!(!o.press(ax, ay, VIEW, 6_000));
        // The action: taken, reported, toast gone.
        assert!(o.press(ax, ay, VIEW, 10));
        assert_eq!(
            o.release(ax, ay, VIEW, 20),
            (true, Some(OverlayEvent::Toast(ToastAction::Undo)))
        );
        assert!(o.toast.is_none());
    }

    #[test]
    fn toast_colors_keep_4_5_contrast_in_both_modes() {
        for (name, p, dark) in [("LIGHT", LIGHT, false), ("DARK", DARK, true)] {
            let (bg, fg, _) = toast_colors(&p, dark);
            let ratio = contrast(bg, fg);
            assert!(ratio >= 4.5, "{name}: toast contrast {ratio:.2} < 4.5");
            // Dialog button labels on surface2.
            let ratio = contrast(p.surface2, p.ink);
            assert!(
                ratio >= 4.5,
                "{name}: surface2/ink contrast {ratio:.2} < 4.5"
            );
        }
        assert_eq!(toast_colors(&LIGHT, false), (LIGHT.ink, LIGHT.paper, None));
        assert_eq!(
            toast_colors(&DARK, true),
            (DARK.surface2, DARK.ink, Some(DARK.hairline))
        );
        assert_eq!(scrim(&LIGHT, false), (LIGHT.ink, 0.3));
        assert_eq!(scrim(&DARK, true), (DARK.paper, 0.6));
    }
}
