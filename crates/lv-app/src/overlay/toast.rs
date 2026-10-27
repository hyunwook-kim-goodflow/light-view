//! Toasts (spec §5.6): the message, its optional action and where they go.

use crate::strings::{t, text, tf, Lang, S};
use crate::toolbar::{pinned_bar_height, Rect};

use super::TOAST_MS;

// Toast layout (DIPs).
const TOAST_H: f32 = 52.0;
pub(super) const TOAST_RADIUS: f32 = 14.0;
const TOAST_MARGIN: f32 = 16.0;
const TOAST_PAD_L: f32 = 16.0;
const TOAST_PAD_R: f32 = 8.0;
const ACTION_GAP: f32 = 12.0;
const PILL_H: f32 = 36.0;
const PILL_PAD: f32 = 14.0;
pub(super) const HIT_MIN: f32 = 44.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastAction {
    Undo,
    /// [보기]: opens the new release's page.
    ViewUpdate,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    /// What `text` is made of (a string key, a file name or version after it, and
    /// maybe a closing key), so a language change can rebuild it.
    source: (S, String, Option<S>),
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
            source: (key, suffix.to_owned(), None),
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

    /// "새 버전(0.2.0)이 나왔어요 [보기]" (ruling R14).
    pub fn update_available(version: &str, now_ms: u64) -> Toast {
        let (head, tail) = (S::ToastUpdateAvailable, S::ToastUpdateAvailableEnd);
        Toast {
            text: format!("{}{version}{}", t(head), t(tail)),
            source: (head, version.to_owned(), Some(tail)),
            action: Some((t(S::BtnView).to_owned(), ToastAction::ViewUpdate)),
            ..Toast::new(head, now_ms)
        }
    }

    /// Rebuilds the text and the action's label in `lang`.
    pub fn retranslate(&mut self, lang: Lang) {
        let (key, suffix, tail) = &self.source;
        let tail = tail.map_or("", |k| text(lang, k));
        self.text = format!("{}{suffix}{tail}", text(lang, *key));
        if let Some((label, action)) = &mut self.action {
            *label = match action {
                ToastAction::Undo => text(lang, S::ToastUndo).to_owned(),
                ToastAction::ViewUpdate => text(lang, S::BtnView).to_owned(),
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

    pub(super) fn contains(&self, x: f32, y: f32) -> bool {
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
