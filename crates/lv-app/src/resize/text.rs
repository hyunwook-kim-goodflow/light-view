//! The resize screen's texts (spec §4, global constraints: noun forms, short): sizes,
//! the caption, the list rows, the total, why 원본 대체 is off, and the toasts after a
//! save, a commit or an undo. Pure: every function takes the language.

use std::path::Path;

use super::save::batch::{UndoReason, Unrecycled, Unrestored};
use super::save::{FailReason, ReplaceRefusal};
use super::saving::SaveSummary;
use super::session::{Estimate, ReplaceBlock, ResizeItem};
use crate::strings::{text, Lang, S};

const MB: f64 = 1_000_000.0;

/// A file size the way the 용량 tab counts it (1 MB = 1,000,000 bytes): "5.1 MB",
/// under 0.1 MB in KB ("85 KB").
pub fn size_text(bytes: u64) -> String {
    if bytes >= 100_000 {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1000).max(1))
    }
}

/// An estimated size: "약 1.1 MB" / "≈ 1.1 MB".
pub fn about_text(lang: Lang, bytes: u64) -> String {
    format!("{}{}", text(lang, S::ResizeAbout), size_text(bytes))
}

/// The file name shown for `path`.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// "4032×3024 · 5.1 MB" (only the size until the photo was read).
pub fn source_text(item: &ResizeItem) -> String {
    match item.src_size {
        (0, 0) => size_text(item.src_bytes),
        (w, h) => format!("{w}×{h} · {}", size_text(item.src_bytes)),
    }
}

/// The result part: "1920×1440 · 약 1.1 MB", "맞추기 불가", "예상 불가", or "…" while
/// it is worked out.
pub fn result_text(lang: Lang, estimate: &Estimate) -> String {
    match estimate {
        Estimate::Pending => "…".to_owned(),
        Estimate::Ready { size, bytes } => {
            format!("{}×{} · {}", size.0, size.1, about_text(lang, *bytes))
        }
        Estimate::CantFit => text(lang, S::ResizeCantFit).to_owned(),
        Estimate::Failed(_) => text(lang, S::ResizeNoEstimate).to_owned(),
    }
}

/// The caption under the photo, as (source part, result part); the result part is
/// drawn in the highlighter: "IMG_0951.JPG · 4032×3024 · 5.1 MB → " and
/// "1920×1440 · 약 1.1 MB".
pub fn caption(lang: Lang, item: &ResizeItem) -> (String, String) {
    (
        format!("{} · {} → ", file_name(&item.path), source_text(item)),
        result_text(lang, &item.estimate),
    )
}

/// A list row's note: an animation keeps its first frame; a photo opened at reduced
/// size says so.
pub fn note(lang: Lang, item: &ResizeItem) -> Option<&'static str> {
    if item.animated {
        Some(text(lang, S::ResizeAnimated))
    } else if item.scaled {
        Some(text(lang, S::ScaledOpen))
    } else {
        None
    }
}

/// A list row's right side, two lines: ("1920×1440", "약 0.9 MB"), or the state.
pub fn row_result(lang: Lang, estimate: &Estimate) -> (String, String) {
    match estimate {
        Estimate::Ready { size, bytes } => {
            (format!("{}×{}", size.0, size.1), about_text(lang, *bytes))
        }
        other => (result_text(lang, other), String::new()),
    }
}

/// "합계 17.7 MB → 약 3.6 MB": the photos with a result (맞추기 불가 and the rest
/// left out); `None` before any has one.
pub fn total_line(lang: Lang, items: &[ResizeItem]) -> Option<String> {
    let (from, to) = items
        .iter()
        .filter_map(|i| match i.estimate {
            Estimate::Ready { bytes, .. } => Some((i.src_bytes, bytes)),
            _ => None,
        })
        .fold(None, |acc: Option<(u64, u64)>, (a, b)| {
            let (x, y) = acc.unwrap_or_default();
            Some((x + a, y + b))
        })?;
    Some(format!(
        "{} {} → {}",
        text(lang, S::ResizeTotal),
        size_text(from),
        about_text(lang, to)
    ))
}

/// [저장 (N장)]: "저장 (3장)" / "Save (3)".
pub fn save_button(lang: Lang, n: usize) -> String {
    match lang {
        Lang::Ko => format!("{} ({n}장)", text(lang, S::BtnSave)),
        Lang::En => format!("{} ({n})", text(lang, S::BtnSave)),
    }
}

/// While a save runs: "저장하는 중… 2/5".
pub fn progress(lang: Lang, done: usize, total: usize) -> String {
    format!("{} {done}/{total}", text(lang, S::Saving))
}

/// Why 원본 대체 is off (R3).
pub fn block_reason(lang: Lang, block: ReplaceBlock) -> &'static str {
    text(
        lang,
        match block {
            ReplaceBlock::NoRecycleBin => S::ResizeNoRecycleBin,
            ReplaceBlock::Animated => S::ResizeAnimated,
            ReplaceBlock::Scaled => S::ScaledOpen,
        },
    )
}

/// Why a photo was not saved.
pub fn fail_reason(lang: Lang, reason: &FailReason) -> &'static str {
    text(
        lang,
        match reason {
            FailReason::NameTaken => S::ResizeNameTaken,
            FailReason::ReadOnly => S::ResizeReadOnly,
            FailReason::Changed => S::ResizeChanged,
            FailReason::OverLimit => S::ResizeOverLimit,
            FailReason::ReplaceRefused(ReplaceRefusal::Animated) => S::ResizeAnimated,
            FailReason::ReplaceRefused(ReplaceRefusal::Scaled) => S::ScaledOpen,
            // Free-form system errors are only logged.
            FailReason::Io(_) => S::ResizeSaveFailed,
        },
    )
}

/// Why an undo did not fully restore a photo.
pub fn undo_reason(lang: Lang, reason: &UndoReason) -> &'static str {
    text(
        lang,
        match reason {
            UndoReason::ResultNotRecycled => S::ResizeResultNotRecycled,
            UndoReason::ResultChanged => S::ResizeResultChanged,
            UndoReason::NameTaken => S::ResizeNameTaken,
            UndoReason::OriginalChanged => S::ResizeChanged,
        },
    )
}

/// "원본은 IMG (원본).jpg로 보관" / "Original kept as IMG (원본).jpg".
pub fn kept_as(lang: Lang, kept_at: &Path) -> String {
    text(lang, S::ResizeKeptAs).replace("{}", &file_name(kept_at))
}

/// What a toast after a resize save, commit or undo says. Kept as data so a language
/// change can say it again.
#[derive(Clone, Debug, PartialEq)]
pub enum ToastText {
    Summary(SaveSummary),
    Unrecycled(Vec<Unrecycled>),
    Unrestored(Vec<Unrestored>),
}

impl ToastText {
    pub fn render(&self, lang: Lang) -> String {
        match self {
            ToastText::Summary(s) => summary(lang, s),
            ToastText::Unrecycled(v) => {
                let Some(first) = v.first() else {
                    return String::new();
                };
                let line = format!(
                    "{} · {}",
                    text(lang, S::ResizeNotRecycled),
                    kept_as(lang, &first.kept_at)
                );
                with_more(lang, line, v.len())
            }
            ToastText::Unrestored(v) => {
                let Some(first) = v.first() else {
                    return String::new();
                };
                let mut line = format!(
                    "{} · {} · {}",
                    text(lang, S::ResizeUndoIncomplete),
                    file_name(&first.original),
                    undo_reason(lang, &first.reason)
                );
                if let Some(kept) = &first.kept_at {
                    line = format!("{line} · {}", kept_as(lang, kept));
                }
                with_more(lang, line, v.len())
            }
        }
    }
}

/// `line` about the first of `n` photos, with how many more: "… 외 2장" / "… (+2)".
fn with_more(lang: Lang, line: String, n: usize) -> String {
    match (n, lang) {
        (0 | 1, _) => line,
        (n, Lang::Ko) => format!("{line} 외 {}장", n - 1),
        (n, Lang::En) => format!("{line} (+{})", n - 1),
    }
}

/// After a save: "3장 저장 · 1장 맞추기 불가 · 1장 실패 (a.jpg: 같은 이름의 파일 있음)";
/// in English "3 saved · Can't fit: 1 · Failed: 1 (…)". A replace reads "3장 대체"
/// ("3 replaced"), followed by the toast's [실행 취소].
pub fn summary(lang: Lang, s: &SaveSummary) -> String {
    let count = |n: usize, key: S| match lang {
        Lang::Ko => format!("{n}장 {}", text(lang, key)),
        Lang::En if key == S::ResizeSaved || key == S::ResizeReplaced => {
            format!("{n} {}", text(lang, key))
        }
        Lang::En => format!("{}: {n}", text(lang, key)),
    };
    let mut parts = Vec::new();
    if s.saved > 0 {
        parts.push(count(s.saved, S::ResizeSaved));
    }
    if s.replaced > 0 {
        parts.push(count(s.replaced, S::ResizeReplaced));
    }
    if s.cant_fit > 0 {
        parts.push(count(s.cant_fit, S::ResizeCantFit));
    }
    if let Some((path, reason)) = s.failed.first() {
        let why = format!("{}: {}", file_name(path), fail_reason(lang, reason));
        parts.push(format!(
            "{} ({why})",
            count(s.failed.len(), S::ResizeFailed)
        ));
    }
    parts.join(" · ")
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
