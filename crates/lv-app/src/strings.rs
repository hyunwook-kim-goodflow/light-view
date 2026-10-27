//! Every user-visible string, in Korean and English (spec §7). Call sites use
//! `t(S::Key)` (or a helper below); the language is a process-wide setting that
//! `set` changes at runtime, so the next draw or title update picks it up.
//!
//! Debug and log text (`eprintln!`) is not routed through here.

use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

use lv_core::settings::LangSetting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ko,
    En,
}

/// `LANG_KOREAN`: the primary language id (the low ten bits of a LANGID).
const LANG_KOREAN: u16 = 0x12;

/// Decides the language from the setting and the user's UI language id. Pure, so the
/// rule is testable without touching the OS.
pub fn lang_for(setting: LangSetting, ui_lang_id: u16) -> Lang {
    match setting {
        LangSetting::Ko => Lang::Ko,
        LangSetting::En => Lang::En,
        LangSetting::Auto => {
            if ui_lang_id & 0x3ff == LANG_KOREAN {
                Lang::Ko
            } else {
                Lang::En
            }
        }
    }
}

fn ui_language_id() -> u16 {
    // SAFETY: plain call with no arguments; it only reads the user's locale.
    unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() }
}

// 0 = Korean, 1 = English. Korean until `set` runs (the strings the app shipped with).
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Ko,
    }
}

/// Applies the language setting. The caller re-applies the window title and redraws.
pub fn set(setting: LangSetting) {
    let lang = match setting {
        LangSetting::Auto => lang_for(setting, ui_language_id()),
        _ => lang_for(setting, 0),
    };
    CURRENT.store(u8::from(lang == Lang::En), Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum S {
    // Dialog.
    CheckboxAutoSave,
    PromptSaveRotation,
    PromptPermanentDelete,
    BtnSave,
    BtnDiscard,
    BtnCancel,
    BtnDelete,
    // Toasts. `ToastKeptCopy` and `ToastRecycleFailed` are followed by a file name.
    ToastRecycled,
    ToastUndo,
    ToastSavedAsTag,
    ToastFileChanged,
    ToastSaveFailed,
    ToastUnsupportedFormat,
    ToastReadOnly,
    ToastKeptCopy,
    ToastNoPermission,
    ToastInUse,
    ToastNotAFile,
    ToastDeleteFailed,
    ToastRecycleFailed,
    ToastPhotoVanished,
    ToastPhotoReloaded,
    // Viewer.
    CannotOpen,
    TooLarge,
    TitleRotatedUnsaved,
    Loading,
    Saving,
    // Toolbar.
    ToolbarSave,
    TipPrev,
    TipNext,
    TipZoomOut,
    TipZoomIn,
    TipRotateLeft,
    TipRotateRight,
    TipSave,
    TipInfo,
    TipDelete,
    TipPin,
    // EXIF card. `ExifLocation` and `ExifModified` label a line ("위치 37.5, 127.0").
    ExifNone,
    ExifShowAll,
    ExifCopy,
    ExifLocation,
    ExifModified,
    ToastCopied,
    TimeAm,
    TimePm,
    // Right-click menu. The shortcut keys are shown at the right of their items.
    MenuOpen,
    MenuShowInFolder,
    MenuCopyPath,
    MenuDefaultApps,
    MenuSettings,
    MenuAbout,
    ShortcutOpen,
    ShortcutSettings,
    // Empty start screen, the Open dialog and drops.
    StartHint,
    BtnOpen,
    FilterPhotos,
    FilterAll,
    ToastNoPhotos,
    ToastPathCopied,
    ToastOpenFailed,
    // About dialog. `AboutVersion` is followed by the version number.
    AboutTitle,
    AboutVersion,
    AboutLicense,
    BtnNotices,
    BtnClose,
}

/// Every key, for tests.
#[cfg(test)]
const ALL: [S; 66] = [
    S::CheckboxAutoSave,
    S::PromptSaveRotation,
    S::PromptPermanentDelete,
    S::BtnSave,
    S::BtnDiscard,
    S::BtnCancel,
    S::BtnDelete,
    S::ToastRecycled,
    S::ToastUndo,
    S::ToastSavedAsTag,
    S::ToastFileChanged,
    S::ToastSaveFailed,
    S::ToastUnsupportedFormat,
    S::ToastReadOnly,
    S::ToastKeptCopy,
    S::ToastNoPermission,
    S::ToastInUse,
    S::ToastNotAFile,
    S::ToastDeleteFailed,
    S::ToastRecycleFailed,
    S::ToastPhotoVanished,
    S::ToastPhotoReloaded,
    S::CannotOpen,
    S::TooLarge,
    S::TitleRotatedUnsaved,
    S::Loading,
    S::Saving,
    S::ToolbarSave,
    S::TipPrev,
    S::TipNext,
    S::TipZoomOut,
    S::TipZoomIn,
    S::TipRotateLeft,
    S::TipRotateRight,
    S::TipSave,
    S::TipInfo,
    S::TipDelete,
    S::TipPin,
    S::ExifNone,
    S::ExifShowAll,
    S::ExifCopy,
    S::ExifLocation,
    S::ExifModified,
    S::ToastCopied,
    S::TimeAm,
    S::TimePm,
    S::MenuOpen,
    S::MenuShowInFolder,
    S::MenuCopyPath,
    S::MenuDefaultApps,
    S::MenuSettings,
    S::MenuAbout,
    S::ShortcutOpen,
    S::ShortcutSettings,
    S::StartHint,
    S::BtnOpen,
    S::FilterPhotos,
    S::FilterAll,
    S::ToastNoPhotos,
    S::ToastPathCopied,
    S::ToastOpenFailed,
    S::AboutTitle,
    S::AboutVersion,
    S::AboutLicense,
    S::BtnNotices,
    S::BtnClose,
];

/// The text for `key` in `lang`.
pub fn text(lang: Lang, key: S) -> &'static str {
    let (ko, en) = match key {
        S::CheckboxAutoSave => (
            "다음부터 묻지 않고 바로 저장",
            "Save without asking from now on",
        ),
        S::PromptSaveRotation => ("회전한 내용을 저장할까요?", "Save the rotation?"),
        S::PromptPermanentDelete => (
            "이 위치는 휴지통이 없어 영구 삭제돼요. 삭제할까요?",
            "This location has no Recycle Bin, so the file will be deleted for good. Delete it?",
        ),
        S::BtnSave => ("저장", "Save"),
        S::BtnDiscard => ("저장 안 함", "Don't save"),
        S::BtnCancel => ("취소", "Cancel"),
        S::BtnDelete => ("삭제", "Delete"),
        S::ToastRecycled => ("휴지통으로 옮겼어요", "Moved to Recycle Bin"),
        S::ToastUndo => ("실행 취소", "Undo"),
        S::ToastSavedAsTag => (
            "이 사진은 회전 정보로 저장했어요",
            "Saved this photo's rotation as a tag",
        ),
        S::ToastFileChanged => (
            "파일이 바뀌었어요, 다시 열어 주세요",
            "The file changed. Please reopen it",
        ),
        S::ToastSaveFailed => ("저장하지 못했어요", "Couldn't save"),
        S::ToastUnsupportedFormat => (
            "이 형식은 아직 회전 저장을 지원하지 않아요",
            "Saving rotation isn't supported for this format yet",
        ),
        S::ToastReadOnly => ("읽기 전용 파일이에요", "This file is read-only"),
        S::ToastKeptCopy => (
            "저장 중 문제가 생겨 사본을 남겼어요: ",
            "Something went wrong while saving, so a copy was kept: ",
        ),
        S::ToastNoPermission => (
            "삭제할 권한이 없어요",
            "You don't have permission to delete this",
        ),
        S::ToastInUse => (
            "다른 프로그램이 쓰고 있어 삭제할 수 없어요",
            "Another program is using this file, so it can't be deleted",
        ),
        S::ToastNotAFile => ("이 항목은 삭제할 수 없어요", "This item can't be deleted"),
        S::ToastDeleteFailed => ("삭제하지 못했어요", "Couldn't delete"),
        S::ToastRecycleFailed => (
            "휴지통으로 옮기지 못했어요: ",
            "Couldn't move to Recycle Bin: ",
        ),
        S::ToastPhotoVanished => (
            "보던 사진이 폴더에서 사라졌어요",
            "The photo you were viewing is no longer in the folder",
        ),
        S::ToastPhotoReloaded => (
            "사진이 바깥에서 바뀌어 다시 불러왔어요",
            "The photo was changed elsewhere, so it was reloaded",
        ),
        S::CannotOpen => ("이 사진은 열 수 없어요", "Couldn't open this photo"),
        S::TooLarge => ("너무 커서 열 수 없어요", "Too large to open"),
        S::TitleRotatedUnsaved => (" · 회전됨 · 저장 안 됨", " · Rotated · Not saved"),
        S::Loading => ("불러오는 중…", "Loading…"),
        S::Saving => ("저장하는 중…", "Saving…"),
        S::ToolbarSave => ("저장", "Save"),
        S::TipPrev => ("이전 사진 (←)", "Previous photo (←)"),
        S::TipNext => ("다음 사진 (→)", "Next photo (→)"),
        S::TipZoomOut => ("축소 (−)", "Zoom out (−)"),
        S::TipZoomIn => ("확대 (+)", "Zoom in (+)"),
        S::TipRotateLeft => ("왼쪽으로 회전 (Ctrl+←)", "Rotate left (Ctrl+←)"),
        S::TipRotateRight => ("오른쪽으로 회전 (Ctrl+→)", "Rotate right (Ctrl+→)"),
        S::TipSave => ("저장 (Ctrl+S)", "Save (Ctrl+S)"),
        S::TipInfo => ("정보 (I)", "Info (I)"),
        S::TipDelete => ("삭제 (Delete)", "Delete (Del)"),
        S::TipPin => ("메뉴 고정", "Pin menu"),
        S::ExifNone => ("EXIF 정보가 없어요", "No EXIF information"),
        S::ExifShowAll => ("전체 보기", "Show all"),
        S::ExifCopy => ("복사", "Copy"),
        S::ExifLocation => ("위치", "Location"),
        S::ExifModified => ("수정", "Modified"),
        S::ToastCopied => ("복사했어요", "Copied"),
        S::TimeAm => ("오전", "AM"),
        S::TimePm => ("오후", "PM"),
        S::MenuOpen => ("열기", "Open"),
        S::MenuShowInFolder => ("폴더에서 보기", "Show in folder"),
        S::MenuCopyPath => ("경로 복사", "Copy path"),
        S::MenuDefaultApps => ("기본 앱 설정 열기", "Default apps settings"),
        S::MenuSettings => ("설정", "Settings"),
        S::MenuAbout => ("정보", "About"),
        S::ShortcutOpen => ("Ctrl+O", "Ctrl+O"),
        S::ShortcutSettings => ("Ctrl+,", "Ctrl+,"),
        S::StartHint => (
            "사진을 끌어다 놓거나 열어 주세요",
            "Drop a photo here or open one",
        ),
        S::BtnOpen => ("열기", "Open"),
        S::FilterPhotos => ("사진", "Photos"),
        S::FilterAll => ("모든 파일", "All files"),
        S::ToastNoPhotos => ("열 수 있는 사진이 없어요", "No photos to open"),
        S::ToastPathCopied => ("경로를 복사했어요", "Path copied"),
        S::ToastOpenFailed => ("열지 못했어요", "Couldn't open it"),
        S::AboutTitle => ("light-view", "light-view"),
        S::AboutVersion => ("버전 ", "Version "),
        S::AboutLicense => (
            "MIT 또는 Apache-2.0 라이선스로 배포해요",
            "Licensed under MIT or Apache-2.0",
        ),
        S::BtnNotices => ("오픈소스 고지", "Open source notices"),
        S::BtnClose => ("닫기", "Close"),
    };
    match lang {
        Lang::Ko => ko,
        Lang::En => en,
    }
}

/// The text for `key` in the current language.
pub fn t(key: S) -> &'static str {
    text(current(), key)
}

/// `key`'s text followed by `args`, joined with nothing in between: the prefix keys
/// (`ToastKeptCopy`, `ToastRecycleFailed`) end in their own separator.
pub fn tf(key: S, args: &[&dyn Display]) -> String {
    let mut s = t(key).to_owned();
    for a in args {
        s.push_str(&a.to_string());
    }
    s
}

/// "너무 커서 열 수 없어요 (30000×20000)": the text with the pixel size appended.
pub fn too_large_with_size(width: u32, height: u32) -> String {
    format!("{} ({width}×{height})", t(S::TooLarge))
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A calendar date: "2026. 10. 3" in Korean, "Oct 3, 2026" in English. `month` is 1..=12.
pub fn date_text(lang: Lang, year: u16, month: u8, day: u8) -> String {
    match lang {
        Lang::Ko => format!("{year}. {month}. {day}"),
        Lang::En => {
            let name = MONTHS[usize::from(month.clamp(1, 12)) - 1];
            format!("{name} {day}, {year}")
        }
    }
}

/// A 12-hour clock time: "오후 5:42" in Korean, "5:42 PM" in English. `hour` is 0..=23.
pub fn time_text(lang: Lang, hour: u8, minute: u8) -> String {
    let half = if hour < 12 { S::TimeAm } else { S::TimePm };
    let h12 = match hour % 12 {
        0 => 12,
        h => h,
    };
    let half = text(lang, half);
    match lang {
        Lang::Ko => format!("{half} {h12}:{minute:02}"),
        Lang::En => format!("{h12}:{minute:02} {half}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_has_both_languages() {
        for key in ALL {
            for lang in [Lang::Ko, Lang::En] {
                assert!(!text(lang, key).is_empty(), "{key:?} {lang:?}");
            }
        }
    }

    #[test]
    fn all_lists_every_key_once() {
        // The last variant is the sentinel: a key added to `S` after it must be
        // added to `ALL` too, or the tests above would skip it.
        assert_eq!(ALL.last(), Some(&S::BtnClose));
        for (i, a) in ALL.iter().enumerate() {
            assert!(!ALL[i + 1..].contains(a), "{a:?} listed twice");
        }
    }

    #[test]
    fn auto_follows_ui_language() {
        // 0x0412 ko-KR, 0x0409 en-US, 0x0411 ja-JP, 0x0812 (Korean, other sublanguage).
        assert_eq!(lang_for(LangSetting::Auto, 0x0412), Lang::Ko);
        assert_eq!(lang_for(LangSetting::Auto, 0x0812), Lang::Ko);
        assert_eq!(lang_for(LangSetting::Auto, 0x0409), Lang::En);
        assert_eq!(lang_for(LangSetting::Auto, 0x0411), Lang::En);
        assert_eq!(lang_for(LangSetting::Auto, 0), Lang::En);
        // An explicit choice ignores the OS.
        assert_eq!(lang_for(LangSetting::Ko, 0x0409), Lang::Ko);
        assert_eq!(lang_for(LangSetting::En, 0x0412), Lang::En);
    }

    #[test]
    fn set_ko_selects_korean() {
        // Only the Korean choice is applied here: other tests read the global and
        // expect the shipped Korean text.
        set(LangSetting::Ko);
        assert_eq!(current(), Lang::Ko);
        assert_eq!(t(S::BtnCancel), "취소");
    }

    #[test]
    fn helpers_format() {
        assert_eq!(
            tf(S::ToastRecycleFailed, &[&"a.jpg"]),
            "휴지통으로 옮기지 못했어요: a.jpg"
        );
        assert_eq!(
            too_large_with_size(30000, 20000),
            "너무 커서 열 수 없어요 (30000×20000)"
        );
    }

    #[test]
    fn dates_and_times_per_language() {
        assert_eq!(date_text(Lang::Ko, 2026, 10, 3), "2026. 10. 3");
        assert_eq!(date_text(Lang::En, 2026, 10, 3), "Oct 3, 2026");
        assert_eq!(date_text(Lang::En, 2026, 1, 31), "Jan 31, 2026");
        assert_eq!(time_text(Lang::Ko, 17, 42), "오후 5:42");
        assert_eq!(time_text(Lang::En, 17, 42), "5:42 PM");
        assert_eq!(time_text(Lang::Ko, 0, 5), "오전 12:05");
        assert_eq!(time_text(Lang::En, 12, 0), "12:00 PM");
        assert_eq!(time_text(Lang::En, 9, 7), "9:07 AM");
        assert_eq!(text(Lang::Ko, S::ExifNone), "EXIF 정보가 없어요");
        assert_eq!(text(Lang::Ko, S::ExifShowAll), "전체 보기");
        assert_eq!(text(Lang::Ko, S::ExifCopy), "복사");
        assert_eq!(text(Lang::Ko, S::ToastCopied), "복사했어요");
    }

    #[test]
    fn menu_start_and_about_texts() {
        assert_eq!(
            text(Lang::Ko, S::StartHint),
            "사진을 끌어다 놓거나 열어 주세요"
        );
        assert_eq!(text(Lang::Ko, S::ToastNoPhotos), "열 수 있는 사진이 없어요");
        assert_eq!(text(Lang::Ko, S::MenuShowInFolder), "폴더에서 보기");
        assert_eq!(text(Lang::Ko, S::MenuDefaultApps), "기본 앱 설정 열기");
        assert_eq!(text(Lang::En, S::MenuAbout), "About");
    }

    /// The Korean text the app shipped with through M3, byte for byte (the constants
    /// that were in overlay.rs, viewer.rs, toolbar.rs and window.rs).
    #[test]
    fn korean_strings_unchanged() {
        let expected: [(S, &str); 38] = [
            (S::CheckboxAutoSave, "다음부터 묻지 않고 바로 저장"),
            (S::PromptSaveRotation, "회전한 내용을 저장할까요?"),
            (
                S::PromptPermanentDelete,
                "이 위치는 휴지통이 없어 영구 삭제돼요. 삭제할까요?",
            ),
            (S::BtnSave, "저장"),
            (S::BtnDiscard, "저장 안 함"),
            (S::BtnCancel, "취소"),
            (S::BtnDelete, "삭제"),
            (S::ToastRecycled, "휴지통으로 옮겼어요"),
            (S::ToastUndo, "실행 취소"),
            (S::ToastSavedAsTag, "이 사진은 회전 정보로 저장했어요"),
            (S::ToastFileChanged, "파일이 바뀌었어요, 다시 열어 주세요"),
            (S::ToastSaveFailed, "저장하지 못했어요"),
            (
                S::ToastUnsupportedFormat,
                "이 형식은 아직 회전 저장을 지원하지 않아요",
            ),
            (S::ToastReadOnly, "읽기 전용 파일이에요"),
            (S::ToastKeptCopy, "저장 중 문제가 생겨 사본을 남겼어요: "),
            (S::ToastNoPermission, "삭제할 권한이 없어요"),
            (S::ToastInUse, "다른 프로그램이 쓰고 있어 삭제할 수 없어요"),
            (S::ToastNotAFile, "이 항목은 삭제할 수 없어요"),
            (S::ToastDeleteFailed, "삭제하지 못했어요"),
            (S::ToastRecycleFailed, "휴지통으로 옮기지 못했어요: "),
            (S::ToastPhotoVanished, "보던 사진이 폴더에서 사라졌어요"),
            (
                S::ToastPhotoReloaded,
                "사진이 바깥에서 바뀌어 다시 불러왔어요",
            ),
            (S::CannotOpen, "이 사진은 열 수 없어요"),
            (S::TooLarge, "너무 커서 열 수 없어요"),
            (S::TitleRotatedUnsaved, " · 회전됨 · 저장 안 됨"),
            (S::Loading, "불러오는 중…"),
            (S::Saving, "저장하는 중…"),
            (S::ToolbarSave, "저장"),
            (S::TipPrev, "이전 사진 (←)"),
            (S::TipNext, "다음 사진 (→)"),
            (S::TipZoomOut, "축소 (−)"),
            (S::TipZoomIn, "확대 (+)"),
            (S::TipRotateLeft, "왼쪽으로 회전 (Ctrl+←)"),
            (S::TipRotateRight, "오른쪽으로 회전 (Ctrl+→)"),
            (S::TipSave, "저장 (Ctrl+S)"),
            (S::TipInfo, "정보 (I)"),
            (S::TipDelete, "삭제 (Delete)"),
            (S::TipPin, "메뉴 고정"),
        ];
        for (key, ko) in expected {
            assert_eq!(text(Lang::Ko, key).as_bytes(), ko.as_bytes(), "{key:?}");
        }
    }
}
