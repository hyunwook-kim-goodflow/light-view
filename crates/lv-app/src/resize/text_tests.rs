use std::path::PathBuf;

use lv_core::identity::FileIdentity;

use super::*;

fn item(name: &str, size: (u32, u32), bytes: u64, estimate: Estimate) -> ResizeItem {
    let id = FileIdentity {
        volume_serial: 1,
        file_index: 2,
        size: bytes,
        modified: 4,
    };
    let mut i = ResizeItem::new(PathBuf::from(format!(r"C:\p\{name}")), id, 0);
    i.src_size = size;
    i.estimate = estimate;
    i
}

fn ready(size: (u32, u32), bytes: u64) -> Estimate {
    Estimate::Ready { size, bytes }
}

#[test]
fn caption_text_formats() {
    let i = item(
        "IMG_0951.JPG",
        (4032, 3024),
        5_100_000,
        ready((1920, 1440), 1_100_000),
    );
    let (src, result) = caption(Lang::Ko, &i);
    assert_eq!(
        format!("{src}{result}"),
        "IMG_0951.JPG · 4032×3024 · 5.1 MB → 1920×1440 · 약 1.1 MB"
    );
    // The highlighted part is the result alone.
    assert_eq!(result, "1920×1440 · 약 1.1 MB");
    let (src, result) = caption(Lang::En, &i);
    assert_eq!(
        format!("{src}{result}"),
        "IMG_0951.JPG · 4032×3024 · 5.1 MB → 1920×1440 · ≈ 1.1 MB"
    );
    // Before the photo was read, while it is worked out, and when nothing fits.
    let pending = item("a.png", (0, 0), 85_000, Estimate::Pending);
    assert_eq!(caption(Lang::Ko, &pending).0, "a.png · 85 KB → ");
    assert_eq!(caption(Lang::Ko, &pending).1, "…");
    let cant = item("a.png", (10, 10), 1, Estimate::CantFit);
    assert_eq!(caption(Lang::Ko, &cant).1, "맞추기 불가");
    assert_eq!(caption(Lang::En, &cant).1, "Can't fit");
    assert_eq!(size_text(1), "1 KB");
    assert_eq!(size_text(99_999), "100 KB");
    assert_eq!(size_text(100_000), "0.1 MB");
}

#[test]
fn total_line_sums_ready_items() {
    let items = [
        item(
            "a.heic",
            (4032, 3024),
            2_800_000,
            ready((1920, 1440), 900_000),
        ),
        item(
            "b.jpg",
            (4032, 3024),
            5_100_000,
            ready((1920, 1440), 1_100_000),
        ),
        item(
            "c.png",
            (2480, 3508),
            9_800_000,
            ready((1357, 1920), 1_600_000),
        ),
        // Left out: no result.
        item("d.png", (9000, 9000), 40_000_000, Estimate::CantFit),
        item("e.png", (0, 0), 1_000_000, Estimate::Pending),
    ];
    assert_eq!(
        total_line(Lang::Ko, &items).unwrap(),
        "합계 17.7 MB → 약 3.6 MB"
    );
    assert_eq!(
        total_line(Lang::En, &items).unwrap(),
        "Total 17.7 MB → ≈ 3.6 MB"
    );
    assert_eq!(total_line(Lang::Ko, &items[3..]), None);
    assert_eq!(save_button(Lang::Ko, 3), "저장 (3장)");
    assert_eq!(save_button(Lang::En, 3), "Save (3)");
    assert_eq!(progress(Lang::Ko, 2, 5), "저장하는 중… 2/5");
}

#[test]
fn replace_disabled_reasons() {
    assert_eq!(
        block_reason(Lang::Ko, ReplaceBlock::NoRecycleBin),
        "휴지통 없는 위치"
    );
    assert_eq!(
        block_reason(Lang::Ko, ReplaceBlock::Animated),
        "애니메이션 → 첫 장면"
    );
    assert_eq!(
        block_reason(Lang::Ko, ReplaceBlock::Scaled),
        text(Lang::Ko, S::ScaledOpen)
    );
    assert_eq!(
        block_reason(Lang::En, ReplaceBlock::NoRecycleBin),
        "No Recycle Bin here"
    );
    assert_eq!(
        block_reason(Lang::En, ReplaceBlock::Animated),
        "Animation → first frame"
    );
    assert_eq!(
        block_reason(Lang::En, ReplaceBlock::Scaled),
        "Opened at reduced size"
    );
}

#[test]
fn list_marks_animated_and_scaled() {
    let mut a = item("a.gif", (10, 10), 1, ready((10, 10), 1));
    assert_eq!(note(Lang::Ko, &a), None);
    a.animated = true;
    assert_eq!(note(Lang::Ko, &a), Some("애니메이션 → 첫 장면"));
    assert_eq!(note(Lang::En, &a), Some("Animation → first frame"));
    let mut s = item("s.jpg", (10, 10), 1, ready((10, 10), 1));
    s.scaled = true;
    assert_eq!(note(Lang::Ko, &s), Some(text(Lang::Ko, S::ScaledOpen)));
    assert_eq!(
        row_result(Lang::Ko, &ready((1920, 1440), 900_000)),
        ("1920×1440".to_owned(), "약 0.9 MB".to_owned())
    );
    assert_eq!(
        row_result(Lang::Ko, &Estimate::CantFit),
        ("맞추기 불가".to_owned(), String::new())
    );
}

#[test]
fn every_failure_has_a_text_in_both_languages() {
    let reasons = [
        FailReason::NameTaken,
        FailReason::ReadOnly,
        FailReason::Changed,
        FailReason::OverLimit,
        FailReason::ReplaceRefused(ReplaceRefusal::Animated),
        FailReason::ReplaceRefused(ReplaceRefusal::Scaled),
        FailReason::Io("Access is denied. (os error 5)".into()),
    ];
    for r in &reasons {
        for lang in [Lang::Ko, Lang::En] {
            assert!(!fail_reason(lang, r).is_empty());
        }
    }
    assert_eq!(fail_reason(Lang::Ko, &reasons[0]), "같은 이름의 파일 있음");
    assert_eq!(fail_reason(Lang::En, &reasons[0]), "Name already taken");
    // A system error's own (English) text is never shown.
    assert_eq!(fail_reason(Lang::Ko, &reasons[6]), "저장 실패");
    assert_eq!(fail_reason(Lang::En, &reasons[6]), "Save failed");
    for r in [
        UndoReason::ResultNotRecycled,
        UndoReason::ResultChanged,
        UndoReason::NameTaken,
        UndoReason::OriginalChanged,
    ] {
        assert!(!undo_reason(Lang::Ko, &r).is_empty());
        assert!(!undo_reason(Lang::En, &r).is_empty());
    }
}

#[test]
fn toasts_after_a_save_commit_or_undo() {
    let replaced = SaveSummary {
        replaced: 3,
        ..SaveSummary::default()
    };
    // With the toast's [실행 취소]: "3장 대체 · 실행 취소".
    assert_eq!(summary(Lang::Ko, &replaced), "3장 대체");
    assert_eq!(summary(Lang::En, &replaced), "3 replaced");
    let mixed = SaveSummary {
        saved: 2,
        replaced: 0,
        cant_fit: 1,
        failed: vec![(PathBuf::from(r"C:\p\a.jpg"), FailReason::NameTaken)],
    };
    assert_eq!(
        summary(Lang::Ko, &mixed),
        "2장 저장 · 1장 맞추기 불가 · 1장 실패 (a.jpg: 같은 이름의 파일 있음)"
    );
    assert_eq!(
        summary(Lang::En, &mixed),
        "2 saved · Can't fit: 1 · Failed: 1 (a.jpg: Name already taken)"
    );
    let kept = ToastText::Unrecycled(vec![Unrecycled {
        original: PathBuf::from(r"C:\p\IMG.jpg"),
        kept_at: PathBuf::from(r"C:\p\IMG (원본).jpg"),
    }]);
    assert_eq!(
        kept.render(Lang::Ko),
        "원본 휴지통 이동 실패 · 원본은 IMG (원본).jpg로 보관"
    );
    assert_eq!(
        kept.render(Lang::En),
        "Original not recycled · Original kept as IMG (원본).jpg"
    );
    let left = ToastText::Unrestored(vec![
        Unrestored {
            original: PathBuf::from(r"C:\p\IMG.jpg"),
            reason: UndoReason::NameTaken,
            kept_at: Some(PathBuf::from(r"C:\p\IMG (원본).jpg")),
        },
        Unrestored {
            original: PathBuf::from(r"C:\p\b.jpg"),
            reason: UndoReason::ResultChanged,
            kept_at: None,
        },
    ]);
    assert_eq!(
        left.render(Lang::Ko),
        "실행 취소 불가 · IMG.jpg · 같은 이름의 파일 있음 · 원본은 IMG (원본).jpg로 보관 외 1장"
    );
    assert_eq!(
        left.render(Lang::En),
        "Undo incomplete · IMG.jpg · Name already taken · Original kept as IMG (원본).jpg (+1)"
    );
}
