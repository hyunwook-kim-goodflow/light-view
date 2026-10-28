//! Dialogs (spec §4.7, §5.5): prompts, their buttons and checkbox, keys, clicks and the
//! card layout.

use lv_core::command::{Mods, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT};

use std::path::PathBuf;

use crate::about;
use crate::strings::{photo_count, t, S};
use crate::toolbar::Rect;

pub(super) const VK_TAB: u16 = 0x09;
pub(super) const VK_SPACE: u16 = 0x20;
// Dialog layout (DIPs).
const CARD_MAX_W: f32 = 420.0;
const CARD_MARGIN: f32 = 16.0;
const CARD_PAD: f32 = 24.0;
pub(super) const CARD_RADIUS: f32 = 18.0;
const TITLE_LINE_H: f32 = 22.0;
const DETAIL_GAP: f32 = 4.0;
const DETAIL_H: f32 = 20.0;
const SECTION_GAP: f32 = 16.0;
const BUTTONS_GAP: f32 = 20.0;
const ROW_H: f32 = 44.0;
const BUTTON_GAP: f32 = 8.0;
pub(super) const BUTTON_RADIUS: f32 = 12.0;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    SaveRotation,
    PermanentDelete {
        name: String,
    },
    /// The About dialog (ruling R4).
    About,
    /// "새 버전이 나오면 알려 드릴까요?", once, after the first photo (spec §6.6).
    UpdateCheck,
    /// Several photos dropped on the viewer: [보기] or [크기 조정] (M6, spec §3).
    DropMany {
        paths: Vec<PathBuf>,
    },
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
    /// Update question: [알려 주세요].
    NotifyMe,
    /// Update question: [괜찮아요].
    NoThanks,
    /// Several photos dropped: [보기] opens the first.
    View,
    /// Several photos dropped: [크기 조정].
    Resize,
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
            Prompt::UpdateCheck => t(S::PromptUpdateCheck),
            Prompt::DropMany { .. } => t(S::PromptDropMany),
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
            Prompt::UpdateCheck => &[
                Item::Button(PromptResult::NotifyMe),
                Item::Button(PromptResult::NoThanks),
            ],
            Prompt::DropMany { .. } => &[
                Item::Button(PromptResult::View),
                Item::Button(PromptResult::Resize),
                Item::Button(PromptResult::Cancel),
            ],
        }
    }

    /// The Enter button, focused first and drawn in highlighter. Permanent delete has
    /// no undo, so its default is [취소] (ruling R13). The update question turns
    /// network use on only when [알려 주세요] itself is chosen (spec §6.6: no default
    /// opt-in), so Enter says [괜찮아요].
    pub fn default_result(&self) -> PromptResult {
        match self {
            Prompt::SaveRotation => PromptResult::Save,
            Prompt::PermanentDelete { .. } => PromptResult::Cancel,
            Prompt::About => PromptResult::Close,
            Prompt::UpdateCheck => PromptResult::NoThanks,
            // A drop has always opened the photo; Enter keeps doing that.
            Prompt::DropMany { .. } => PromptResult::View,
        }
    }

    /// Title lines until the title is measured (`Dialog::title_lines`): the delete
    /// question wraps.
    fn estimated_title_lines(&self) -> u32 {
        match self {
            Prompt::SaveRotation
            | Prompt::About
            | Prompt::UpdateCheck
            | Prompt::DropMany { .. } => 1,
            Prompt::PermanentDelete { .. } => 2,
        }
    }

    /// Lines under the title in ink2: the file name, About's version and license, or how
    /// many photos were dropped.
    pub fn details(&self) -> Vec<String> {
        match self {
            Prompt::SaveRotation | Prompt::UpdateCheck => Vec::new(),
            Prompt::DropMany { paths } => vec![photo_count(paths.len())],
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
            PromptResult::NotifyMe => t(S::BtnNotifyMe),
            PromptResult::NoThanks => t(S::BtnNoThanks),
            PromptResult::View => t(S::BtnView),
            PromptResult::Resize => t(S::Resize),
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

    pub(super) fn activate(&mut self, i: usize) -> Option<PromptResult> {
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
