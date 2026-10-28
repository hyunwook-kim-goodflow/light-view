use std::path::PathBuf;

use lv_core::identity::FileIdentity;
use lv_core::settings::{ResizeMemory, ResizeTab, SidebarSide};

use super::*;
use crate::resize::draw::geometry;
use crate::resize::session::{Estimate, ResizeItem};
use crate::uia::{main_tree, MainInput, Tree};

fn session(memory: ResizeMemory) -> ResizeSession {
    let mut s = ResizeSession::new(None, 0, memory, 1);
    let items = ["IMG_0948.HEIC", "IMG_0951.JPG", "scan.png"]
        .iter()
        .enumerate()
        .map(|(k, n)| {
            let id = FileIdentity {
                volume_serial: 1,
                file_index: k as u64,
                size: 5_100_000,
                modified: 4,
            };
            let mut i = ResizeItem::new(PathBuf::from(format!(r"C:\p\{n}")), id, 0);
            i.src_size = (4032, 3024);
            i.estimate = Estimate::Ready {
                size: (1920, 1440),
                bytes: 1_100_000,
            };
            i
        })
        .collect();
    s.add(items);
    s.current = 1;
    s
}

fn tree(s: &ResizeSession, focus: Option<Ctl>, block: Option<ReplaceBlock>, lang: Lang) -> Tree {
    let client = (1200.0, 800.0);
    main_tree(&MainInput {
        dip: 1.0,
        client,
        photo: None,
        toolbar: None,
        start: None,
        card: None,
        menu: None,
        dialog: None,
        toast: None,
        resize: Some(ResizeInput {
            session: s,
            geometry: geometry(client, SidebarSide::Right, 1.0, s),
            focus,
            block,
            progress: None,
            lang,
        }),
    })
}

fn named<'a>(t: &'a Tree, name: &str) -> &'a crate::uia::Node {
    t.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no element named {name:?}"))
}

#[test]
fn every_resize_control_has_a_name() {
    let s = session(ResizeMemory::default());
    let t = tree(&s, Some(Ctl::List), None, Lang::Ko);
    for n in &t.nodes {
        assert!(!n.name.is_empty(), "{n:?}");
        if n.act.is_some() {
            assert!(n.rect.w > 0 && n.rect.h > 0, "{n:?}");
        }
    }
    for name in [
        "해상도",
        "용량",
        "비율",
        "긴 변",
        "가로",
        "세로",
        "JPEG",
        "PNG",
        "WebP",
        "원본 수준",
        "새 파일",
        "원본 대체",
        "위치 정보 제거",
        "예상 결과",
        "취소",
        "저장 (3장)",
        "합계 15.3 MB → 약 3.3 MB",
        "IMG_0951.JPG · 4032×3024 · 5.1 MB → 1920×1440 · 약 1.1 MB",
        "IMG_0951.JPG · 2 / 3",
        "필름 띠",
        "이전 사진 (←)",
    ] {
        named(&t, name);
    }
    // The tabs and choices are radio groups named by their labels.
    assert_eq!(named(&t, "형식").role, Role::Group);
    assert_eq!(named(&t, "용량").role, Role::RadioButton);
    assert_eq!(named(&t, "위치 정보 제거").toggle, Some(true));
    // The current photo's row has the focus while the list has it.
    let focused = &t.nodes[t.focused().unwrap()];
    assert_eq!(focused.role, Role::ListItem);
    assert!(focused.name.starts_with("IMG_0951.JPG"));
    assert_eq!(focused.act, Some(Act::Resize(Hit::Row(1))));
    // In English too.
    let t = tree(&s, None, None, Lang::En);
    for name in [
        "Resolution",
        "Replace original",
        "Remove location",
        "Save (3)",
    ] {
        named(&t, name);
    }
    assert!(t.focused().is_none(), "an EDIT (or no one) has the focus");
}

#[test]
fn blocked_replace_is_disabled_with_its_reason() {
    let s = session(ResizeMemory {
        tab: ResizeTab::FileSize,
        ..ResizeMemory::default()
    });
    let t = tree(
        &s,
        Some(Ctl::SaveMode),
        Some(ReplaceBlock::NoRecycleBin),
        Lang::Ko,
    );
    let replace = named(&t, "원본 대체");
    assert!(!replace.enabled && !replace.focusable);
    named(&t, "휴지통 없는 위치");
    // The 용량 tab's field labels are there; its focus is the chosen 새 파일.
    named(&t, "최대 용량");
    named(&t, "긴 변 상한");
    assert_eq!(t.nodes[t.focused().unwrap()].name, "새 파일");
}

#[test]
fn quality_is_not_offered_for_png() {
    let s = session(ResizeMemory {
        format: lv_core::settings::ResizeFormat::Png,
        ..ResizeMemory::default()
    });
    let t = tree(&s, None, None, Lang::Ko);
    let quality = named(&t, "품질");
    assert!(!quality.enabled);
    assert!(!named(&t, "원본 수준").enabled);
}
