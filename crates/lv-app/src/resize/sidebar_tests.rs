use lv_core::settings::SidebarSide;

use super::*;
use crate::toolbar::Rect;

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

fn view(w: f32, h: f32) -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
    }
}

fn ctx(memory: ResizeMemory) -> Ctx {
    Ctx {
        memory,
        replace: false,
        blocked: false,
        saving: false,
        count: 3,
        current: 1,
    }
}

#[test]
fn sidebar_on_left_moves_stage_right() {
    let v = view(1200.0, 800.0);
    let right = stage_rect(v, SidebarSide::Right, 1.0);
    assert_eq!(
        right,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 870.0,
            h: 800.0
        }
    );
    let l = sidebar_layout(v, SidebarSide::Right, 1.0);
    assert_eq!(l.panel.x, 870.0);
    let left = stage_rect(v, SidebarSide::Left, 1.0);
    assert_eq!(
        left,
        Rect {
            x: 330.0,
            y: 0.0,
            w: 870.0,
            h: 800.0
        }
    );
    let l = sidebar_layout(v, SidebarSide::Left, 1.0);
    assert_eq!((l.panel.x, l.panel.w), (0.0, 330.0));
    // Every sidebar part stays inside the panel.
    for r in [
        l.title,
        l.list,
        l.total,
        l.cancel,
        l.save,
        l.rows[0].control,
    ] {
        assert!(
            r.x >= l.panel.x && r.x + r.w <= l.panel.x + l.panel.w,
            "{r:?}"
        );
    }
    // At 150 % the width is whole pixels.
    let s = stage_rect(view(1000.0, 700.0), SidebarSide::Left, 1.0 / 1.5);
    assert!(((s.x * 1.5) - (s.x * 1.5).round()).abs() < 1e-3, "{s:?}");
}

#[test]
fn narrow_window_shrinks_sidebar() {
    assert_eq!(sidebar_width(720.0), SIDEBAR_W);
    assert_eq!(sidebar_width(719.0), SIDEBAR_NARROW_W);
    let l = sidebar_layout(view(700.0, 600.0), SidebarSide::Right, 1.0);
    assert_eq!(l.panel.w, 260.0);
    assert_eq!(
        stage_rect(view(700.0, 600.0), SidebarSide::Right, 1.0).w,
        440.0
    );
    // The list keeps room between the rows above and the total line.
    assert!(l.list.h > 0.0 && l.list.y + l.list.h <= l.total.y);
    assert!(l.total.y + l.total.h <= l.save.y);
}

#[test]
fn tab_order_reaches_every_control() {
    for tab in TABS {
        for format in FORMATS {
            let memory = ResizeMemory {
                tab,
                format,
                ..ResizeMemory::default()
            };
            let order = controls(&memory);
            let mut s = Sidebar::default();
            let mut seen = vec![s.focus];
            for _ in 0..order.len() {
                s.on_key(VK_TAB, NONE, false, &ctx(memory));
                seen.push(s.focus);
            }
            // Around once, back where it started, every control seen.
            assert_eq!(seen.first(), seen.last());
            for c in &order {
                assert!(seen.contains(c), "{tab:?} {format:?} {c:?}");
            }
            // Every field of the tab is in the order; 품질 only when it applies.
            for f in tab_fields(tab) {
                assert!(order.contains(&Ctl::Field(*f)));
            }
            assert_eq!(order.contains(&Ctl::Quality), format != ResizeFormat::Png);
            assert_eq!(order.contains(&Ctl::Axis), tab == ResizeTab::Resolution);
            for c in [
                Ctl::Tabs,
                Ctl::Format,
                Ctl::SaveMode,
                Ctl::StripGps,
                Ctl::List,
            ] {
                assert!(order.contains(&c));
            }
            assert_eq!(&order[order.len() - 2..], [Ctl::Cancel, Ctl::Save]);
            // Every drawn control has somewhere to draw its focus ring.
            let l = sidebar_layout(view(1200.0, 800.0), SidebarSide::Right, 1.0);
            for c in &order {
                assert!(l.focus_rect(*c, &memory).is_some(), "{c:?}");
            }
        }
    }
    // Landing on a field hands the focus to its EDIT control; Shift+Tab goes back.
    let mut s = Sidebar {
        focus: Ctl::Tabs,
        ..Sidebar::default()
    };
    let c = ctx(ResizeMemory::default());
    assert_eq!(
        s.on_key(VK_TAB, NONE, false, &c),
        Some(Action::FocusField(Field::Pixels))
    );
    assert_eq!(s.on_key(VK_TAB, SHIFT, false, &c), Some(Action::Redraw));
    assert_eq!(s.focus, Ctl::Tabs);
}

#[test]
fn keys_move_through_photos_and_options() {
    let c = ctx(ResizeMemory::default());
    let mut s = Sidebar::default();
    // On the list: ← → and ↑ ↓ move through the photos.
    assert_eq!(s.on_key(VK_RIGHT, NONE, false, &c), Some(Action::Step(1)));
    assert_eq!(s.on_key(VK_LEFT, NONE, false, &c), Some(Action::Step(-1)));
    assert_eq!(s.on_key(VK_DOWN, NONE, false, &c), Some(Action::Step(1)));
    assert_eq!(s.on_key(VK_UP, NONE, false, &c), Some(Action::Step(-1)));
    // Enter saves only on [저장]; elsewhere it does nothing (no fullscreen).
    assert_eq!(s.on_key(VK_RETURN, NONE, false, &c), Some(Action::Nothing));
    s.focus = Ctl::Save;
    assert_eq!(s.on_key(VK_RETURN, NONE, false, &c), Some(Action::Save));
    assert_eq!(s.on_key(VK_RETURN, NONE, true, &c), Some(Action::Nothing));
    s.focus = Ctl::Cancel;
    assert_eq!(s.on_key(VK_SPACE, NONE, false, &c), Some(Action::Leave));
    // Esc, Ctrl and Alt combinations go on to the viewer's commands.
    assert_eq!(s.on_key(VK_ESCAPE, NONE, false, &c), None);
    let ctrl = Mods { ctrl: true, ..NONE };
    assert_eq!(s.on_key(0x53, ctrl, false, &c), None);
    // On a choice ← → pick its options.
    s.focus = Ctl::Format;
    let Some(Action::Memory(m)) = s.on_key(VK_RIGHT, NONE, false, &c) else {
        panic!("format");
    };
    assert_eq!(m.format, ResizeFormat::Png);
    assert_eq!(s.on_key(VK_LEFT, NONE, false, &c), Some(Action::Nothing));
    s.focus = Ctl::Tabs;
    let Some(Action::Memory(m)) = s.on_key(VK_RIGHT, NONE, false, &c) else {
        panic!("tabs");
    };
    assert_eq!(m.tab, ResizeTab::FileSize);
    // Space turns 위치 정보 제거 off.
    s.focus = Ctl::StripGps;
    let Some(Action::Memory(m)) = s.on_key(VK_SPACE, NONE, false, &c) else {
        panic!("gps");
    };
    assert!(!m.strip_gps);
    // 원본 대체: on, unless it is blocked.
    s.focus = Ctl::SaveMode;
    assert_eq!(
        s.on_key(VK_RIGHT, NONE, false, &c),
        Some(Action::Replace(true))
    );
    let blocked = Ctx { blocked: true, ..c };
    assert_eq!(
        s.on_key(VK_RIGHT, NONE, false, &blocked),
        Some(Action::Redraw)
    );
    // While a save runs nothing changes.
    let saving = Ctx { saving: true, ..c };
    s.focus = Ctl::Save;
    assert_eq!(
        s.on_key(VK_RETURN, NONE, false, &saving),
        Some(Action::Redraw)
    );
    s.focus = Ctl::Format;
    assert_eq!(
        s.on_key(VK_RIGHT, NONE, false, &saving),
        Some(Action::Redraw)
    );
}

#[test]
fn fields_parse_and_show_their_values() {
    let m = ResizeMemory::default();
    assert_eq!(Field::Pixels.text(&m), "1920");
    assert_eq!(Field::MaxSize.text(&m), "2");
    assert_eq!(Field::Limit.text(&m), "");
    assert_eq!(Field::Percent.text(&m), "50");
    let tenths = |s: &str| Field::MaxSize.parse(s, &m).map(|m| m.max_mb_tenths);
    assert_eq!(tenths("1.5"), Some(15));
    assert_eq!(tenths("2"), Some(20));
    assert_eq!(tenths(" 0.3 "), Some(3));
    assert_eq!(tenths(".5"), Some(5));
    for bad in ["", ".", "1.25", "0", "a", "1,5", "-1"] {
        assert_eq!(tenths(bad), None, "{bad}");
    }
    let shown = |t: u32| {
        Field::MaxSize.text(&ResizeMemory {
            max_mb_tenths: t,
            ..m
        })
    };
    assert_eq!(shown(15), "1.5");
    assert_eq!(shown(3), "0.3");
    assert_eq!(Field::Pixels.parse("800", &m).unwrap().long_edge, 800);
    assert!(Field::Pixels.parse("0", &m).is_none());
    assert!(Field::Pixels.parse("70000", &m).is_none());
    assert_eq!(Field::Limit.parse("", &m).unwrap().limit_long_edge, None);
    assert_eq!(
        Field::Limit.parse("1920", &m).unwrap().limit_long_edge,
        Some(1920)
    );
    assert_eq!(Field::Percent.parse("100", &m).unwrap().percent, 100);
    assert!(Field::Percent.parse("101", &m).is_none());
}

#[test]
fn clicks_act_on_release_over_the_pressed_part() {
    let memory = ResizeMemory::default();
    let c = ctx(memory);
    let l = sidebar_layout(view(1200.0, 800.0), SidebarSide::Right, 1.0);
    let centre = |r: Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
    let rows = list_rows(l.list, &[false, true, false], 1);
    let mut s = Sidebar::default();
    let (x, y) = centre(l.tabs[2]);
    let hit = l.hit(x, y, &c, &rows);
    assert_eq!(hit, Some(Hit::Option(Ctl::Tabs, 2)));
    s.press(hit);
    assert_eq!(s.focus, Ctl::Tabs);
    let Action::Memory(m) = s.release(hit, &c) else {
        panic!("tab");
    };
    assert_eq!(m.tab, ResizeTab::Scale);
    let (x, y) = centre(l.save);
    s.press(l.hit(x, y, &c, &rows));
    assert_eq!(s.release(None, &c), Action::Redraw, "released elsewhere");
    s.press(l.hit(x, y, &c, &rows));
    assert_eq!(s.release(l.hit(x, y, &c, &rows), &c), Action::Save);
    let (x, y) = centre(rows[2].1);
    assert_eq!(l.hit(x, y, &c, &rows), Some(Hit::Row(2)));
    assert_eq!(s.invoke(Hit::Row(2), &c), Action::GoTo(2));
    assert_eq!(s.focus, Ctl::List);
    let field = l.field_rect(Field::Pixels, &memory).unwrap();
    let (x, y) = centre(field);
    assert_eq!(
        l.hit(x, y, &c, &rows),
        Some(Hit::Control(Ctl::Field(Field::Pixels)))
    );
}

#[test]
fn list_scrolls_to_the_current_photo() {
    let list = Rect {
        x: 0.0,
        y: 100.0,
        w: 300.0,
        h: 140.0,
    };
    let notes = [false; 10];
    let rows = list_rows(list, &notes, 0);
    assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [0, 1, 2]);
    let rows = list_rows(list, &notes, 7);
    assert_eq!(rows.last().unwrap().0, 7);
    assert!(rows
        .iter()
        .all(|(_, r)| r.y + r.h <= list.y + list.h + 0.01));
    // A row with a note is taller.
    let rows = list_rows(list, &[true, false], 0);
    assert_eq!(rows[0].1.h, LIST_ROW_H + LIST_NOTE_H);
    assert!(list_rows(list, &[], 0).is_empty());
}

#[test]
fn stage_has_arrows_strip_and_caption() {
    let stage = Rect {
        x: 330.0,
        y: 0.0,
        w: 870.0,
        h: 800.0,
    };
    let p = stage_parts(stage, 3, 0);
    assert!(p.prev.is_none() && p.next.is_some());
    assert_eq!(p.strip.len(), 3);
    assert!(p.photo.h < stage.h && p.caption.y > p.photo.y + p.photo.h);
    assert!(p.strip.iter().all(|(_, r)| r.y + r.h <= p.caption.y));
    let (i, r) = p.strip[2];
    assert_eq!(p.hit(r.x + 1.0, r.y + 1.0), Some(Hit::Strip(i)));
    // Many photos: a window of cells around the current one.
    let p = stage_parts(Rect { w: 300.0, ..stage }, 50, 40);
    assert!(p.strip.len() < 50);
    assert!(p.strip.iter().any(|(i, _)| *i == 40));
    assert!(p.strip.iter().all(|(_, r)| r.x >= stage.x));
}

#[test]
fn uia_names_name_every_field() {
    let m = ResizeMemory::default();
    assert_eq!(Field::Pixels.uia_name(&m, Lang::Ko), "긴 변 (px)");
    let width = ResizeMemory {
        axis: ResizeAxis::Width,
        ..m
    };
    assert_eq!(Field::Pixels.uia_name(&width, Lang::En), "Width (px)");
    assert_eq!(Field::MaxSize.uia_name(&m, Lang::Ko), "최대 용량 (MB)");
    assert_eq!(
        Field::Limit.uia_name(&m, Lang::Ko),
        "긴 변 상한 (px) (선택)"
    );
    assert_eq!(Field::Percent.uia_name(&m, Lang::En), "Scale (%)");
    assert_eq!(
        Ctl::Quality.option_labels(Lang::Ko),
        ["원본 수준", "90", "80", "70", "60"]
    );
    assert_eq!(
        Ctl::Tabs.option_labels(Lang::Ko),
        ["해상도", "용량", "비율"]
    );
    assert_eq!(
        Ctl::SaveMode.option_labels(Lang::En),
        ["New file", "Replace original"]
    );
}
