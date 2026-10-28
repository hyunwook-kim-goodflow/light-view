//! The resize screen's layout (spec §4, the approved mockup). The window is split into
//! the stage (the photo, its arrows, the filmstrip and the caption) and the sidebar:
//! 330 DIP wide, 260 in a window narrower than 720 DIP, on the side the settings
//! choose. Top to bottom the sidebar has the title, the tabs 해상도 / 용량 / 비율, two
//! rows for the tab's numbers, the output rows (형식, 품질, 저장, why 원본 대체 is off,
//! 메타데이터), the estimate list, the total and the buttons [취소] [저장 (N장)].

use lv_core::settings::{ResizeMemory, ResizeTab, SidebarSide};

use super::sidebar::{controls, tab_fields, Ctl, Ctx, Field, Hit};
use crate::toolbar::Rect;

/// The sidebar's width (DIPs), and in a window narrower than `NARROW_BELOW`.
pub const SIDEBAR_W: f32 = 330.0;
pub const SIDEBAR_NARROW_W: f32 = 260.0;
pub const NARROW_BELOW: f32 = 720.0;

// Sidebar layout (DIPs).
const PAD: f32 = 16.0;
const TOP: f32 = 12.0;
const TITLE_H: f32 = 28.0;
const TABS_H: f32 = 36.0;
const ROW_H: f32 = 40.0;
const CONTROL_H: f32 = 34.0;
const LABEL_W: f32 = 72.0;
/// The number rows' labels are longer ("Long edge limit"); their controls are narrow.
const FIELD_LABEL_W: f32 = 104.0;
const FIELD_W: f32 = 88.0;
const SECTION_H: f32 = 22.0;
const NOTE_H: f32 = 18.0;
const TOTAL_H: f32 = 26.0;
const BUTTON_H: f32 = 40.0;
const BUTTON_GAP: f32 = 8.0;
const OPTION_GAP: f32 = 4.0;
/// Estimate list rows: two lines, three with a note.
pub const LIST_ROW_H: f32 = 44.0;
pub const LIST_NOTE_H: f32 = 18.0;
/// The client height the sidebar needs: every row, one list row with its note, the
/// total and the buttons (the window is kept at least this tall while it is open).
pub const MIN_SIDEBAR_H: f32 = TOP
    + TITLE_H
    + TABS_H
    + 4.0
    + 5.0 * ROW_H
    + 2.0 * SECTION_H
    + NOTE_H
    + ROW_H
    + LIST_ROW_H
    + LIST_NOTE_H
    + BUTTON_GAP
    + TOTAL_H
    + BUTTON_H
    + PAD;

// Stage layout (DIPs): the band under the photo holds the filmstrip and the caption.
const BAND_H: f32 = 84.0;
const CAPTION_H: f32 = 26.0;
const STRIP_H: f32 = 28.0;
const STRIP_W: f32 = 38.0;
const STRIP_GAP: f32 = 6.0;
const NAV_W: f32 = 44.0;
const NAV_H: f32 = 72.0;

/// The sidebar's width in a `view_w` wide window.
pub fn sidebar_width(view_w: f32) -> f32 {
    if view_w < NARROW_BELOW {
        SIDEBAR_NARROW_W
    } else {
        SIDEBAR_W
    }
}

/// The sidebar's panel: `view`'s left or right edge, its width snapped to whole pixels
/// (`dip` DIPs per pixel) so the EDIT controls line up with the drawing.
fn panel(view: Rect, side: SidebarSide, dip: f32) -> Rect {
    let snap = |v: f32| {
        if dip > 0.0 {
            (v / dip).round() * dip
        } else {
            v
        }
    };
    let w = snap(sidebar_width(view.w).min(view.w));
    let x = match side {
        SidebarSide::Left => view.x,
        SidebarSide::Right => view.x + view.w - w,
    };
    Rect { x, w, ..view }
}

/// The stage: `view` without the sidebar.
pub fn stage_rect(view: Rect, side: SidebarSide, dip: f32) -> Rect {
    let p = panel(view, side, dip);
    let x = match side {
        SidebarSide::Left => p.x + p.w,
        SidebarSide::Right => view.x,
    };
    Rect {
        x,
        w: (view.w - p.w).max(0.0),
        ..view
    }
}

/// One labelled row: the label on the left, the control after it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    pub label: Rect,
    pub control: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarLayout {
    pub panel: Rect,
    pub title: Rect,
    pub tabs: Vec<Rect>,
    /// The tab's first and second number rows (the 해상도 tab's second row is 기준).
    pub rows: [Row; 2],
    pub output: Rect,
    pub format: Row,
    pub quality: Row,
    pub save_mode: Row,
    /// Why 원본 대체 is off (R3), under the 저장 row.
    pub reason: Rect,
    pub metadata: Row,
    pub estimate: Rect,
    pub list: Rect,
    pub total: Rect,
    pub cancel: Rect,
    pub save: Rect,
}

/// The sidebar in `view` (DIPs) on `side`; `dip` is DIPs per pixel.
pub fn sidebar_layout(view: Rect, side: SidebarSide, dip: f32) -> SidebarLayout {
    let p = panel(view, side, dip);
    let (x, inner) = (p.x + PAD, (p.w - 2.0 * PAD).max(0.0));
    let line = |y: f32, h: f32| Rect { x, y, w: inner, h };
    let row = |y: f32| Row {
        label: Rect {
            x,
            y,
            w: LABEL_W,
            h: ROW_H,
        },
        control: Rect {
            x: x + LABEL_W,
            y: y + (ROW_H - CONTROL_H) / 2.0,
            w: (inner - LABEL_W).max(0.0),
            h: CONTROL_H,
        },
    };
    let mut y = p.y + TOP;
    let title = line(y, TITLE_H);
    y += TITLE_H;
    let tabs = split(line(y, TABS_H), 3, &[1.0; 3], 0.0);
    y += TABS_H + 4.0;
    let rows = [row(y), row(y + ROW_H)];
    y += 2.0 * ROW_H;
    let output = line(y, SECTION_H);
    y += SECTION_H;
    let (format, quality, save_mode) = (row(y), row(y + ROW_H), row(y + 2.0 * ROW_H));
    y += 3.0 * ROW_H;
    let reason = line(y, NOTE_H);
    y += NOTE_H;
    let metadata = row(y);
    y += ROW_H;
    let estimate = line(y, SECTION_H);
    y += SECTION_H;
    let bottom = p.y + p.h - PAD;
    let buttons_y = bottom - BUTTON_H;
    let total = line(buttons_y - BUTTON_GAP - TOTAL_H, TOTAL_H);
    let list = line(y, (total.y - y).max(0.0));
    let half = (inner - BUTTON_GAP) / 2.0;
    let cancel = Rect {
        x,
        y: buttons_y,
        w: half,
        h: BUTTON_H,
    };
    let save = Rect {
        x: x + half + BUTTON_GAP,
        ..cancel
    };
    SidebarLayout {
        panel: p,
        title,
        tabs,
        rows,
        output,
        format,
        quality,
        save_mode,
        reason,
        metadata,
        estimate,
        list,
        total,
        cancel,
        save,
    }
}

/// `r` split into `n` options side by side, sized by `weights`, `gap` apart.
fn split(r: Rect, n: usize, weights: &[f32], gap: f32) -> Vec<Rect> {
    let total: f32 = weights.iter().take(n).sum();
    let free = (r.w - gap * (n as f32 - 1.0)).max(0.0);
    let mut x = r.x;
    (0..n)
        .map(|i| {
            let w = free * weights[i] / total;
            let o = Rect { x, w, ..r };
            x += w + gap;
            o
        })
        .collect()
}

impl SidebarLayout {
    /// The row a control sits in for `memory`'s tab.
    pub fn row_of(&self, c: Ctl, memory: &ResizeMemory) -> Option<Row> {
        let fields = tab_fields(memory.tab);
        Some(match c {
            Ctl::Field(f) => {
                let row = self.rows[fields.iter().position(|x| *x == f)?];
                let grow = FIELD_LABEL_W - row.label.w;
                Row {
                    label: Rect {
                        w: FIELD_LABEL_W,
                        ..row.label
                    },
                    control: Rect {
                        x: row.control.x + grow,
                        w: (row.control.w - grow).max(0.0),
                        ..row.control
                    },
                }
            }
            Ctl::Axis if memory.tab == ResizeTab::Resolution => self.rows[1],
            Ctl::Format => self.format,
            Ctl::Quality => self.quality,
            Ctl::SaveMode => self.save_mode,
            Ctl::StripGps => self.metadata,
            _ => return None,
        })
    }

    /// A field's EDIT control box (the unit follows it).
    pub fn field_rect(&self, f: Field, memory: &ResizeMemory) -> Option<Rect> {
        let c = self.row_of(Ctl::Field(f), memory)?.control;
        Some(Rect {
            w: FIELD_W.min(c.w),
            ..c
        })
    }

    /// A choice's option rects.
    pub fn options(&self, c: Ctl, memory: &ResizeMemory) -> Vec<Rect> {
        if c == Ctl::Tabs {
            return self.tabs.clone();
        }
        let Some(row) = self.row_of(c, memory) else {
            return Vec::new();
        };
        let n = c.option_count();
        // 원본 수준 and 원본 대체 get more room than the numbers beside them.
        let weights: &[f32] = match c {
            Ctl::Quality => &[3.0, 1.0, 1.0, 1.0, 1.0],
            Ctl::SaveMode => &[1.0, 1.3],
            _ => &[1.0, 1.0, 1.0],
        };
        split(row.control, n, weights, OPTION_GAP)
    }

    /// What the focus ring goes around.
    pub fn focus_rect(&self, c: Ctl, memory: &ResizeMemory) -> Option<Rect> {
        Some(match c {
            Ctl::Tabs => line_of(&self.tabs),
            Ctl::List => self.list,
            Ctl::Cancel => self.cancel,
            Ctl::Save => self.save,
            Ctl::Field(f) => self.field_rect(f, memory)?,
            _ => self.row_of(c, memory)?.control,
        })
    }

    /// The control under (x, y) for `ctx`; list rows come from `rows`.
    pub fn hit(&self, x: f32, y: f32, ctx: &Ctx, rows: &[(usize, Rect)]) -> Option<Hit> {
        for c in controls(&ctx.memory) {
            if c.option_count() > 0 {
                let options = self.options(c, &ctx.memory);
                if let Some(i) = options.iter().position(|r| r.contains(x, y)) {
                    return Some(Hit::Option(c, i));
                }
            } else if matches!(c, Ctl::Field(_) | Ctl::StripGps | Ctl::Cancel | Ctl::Save)
                && self.focus_rect(c, &ctx.memory)?.contains(x, y)
            {
                return Some(Hit::Control(c));
            }
        }
        rows.iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(i, _)| Hit::Row(*i))
    }
}

fn line_of(rects: &[Rect]) -> Rect {
    let (first, last) = (rects[0], rects[rects.len() - 1]);
    Rect {
        w: last.x + last.w - first.x,
        ..first
    }
}

/// The list rows that fit in `list`, scrolled so the current photo shows: (index,
/// rect). `notes[i]`: row `i` has a third line.
pub fn list_rows(list: Rect, notes: &[bool], current: usize) -> Vec<(usize, Rect)> {
    let h = |i: usize| LIST_ROW_H + if notes[i] { LIST_NOTE_H } else { 0.0 };
    // The first row shown: the earliest from which the current row still fits.
    let mut first = current.min(notes.len().saturating_sub(1));
    let mut used = notes.get(first).map_or(0.0, |_| h(first));
    while first > 0 && used + h(first - 1) <= list.h {
        first -= 1;
        used += h(first);
    }
    let mut y = list.y;
    let mut rows = Vec::new();
    for i in first..notes.len() {
        if y + h(i) > list.y + list.h + 0.01 {
            break;
        }
        rows.push((i, Rect { y, h: h(i), ..list }));
        y += h(i);
    }
    rows
}

/// The stage's parts: the photo area, the ‹ › arrows, the filmstrip and the caption.
#[derive(Clone, Debug, PartialEq)]
pub struct StageParts {
    pub photo: Rect,
    pub prev: Option<Rect>,
    pub next: Option<Rect>,
    /// (photo index, cell); a window of cells around the current one when they do
    /// not all fit.
    pub strip: Vec<(usize, Rect)>,
    pub caption: Rect,
}

/// The photo above an 84 DIP band holding the filmstrip and the caption; the arrows at
/// the photo area's sides (only where there is a photo to go to).
pub fn stage_parts(stage: Rect, count: usize, current: usize) -> StageParts {
    let band = BAND_H.min(stage.h);
    let photo = Rect {
        h: stage.h - band,
        ..stage
    };
    let nav_y = photo.y + (photo.h - NAV_H).max(0.0) / 2.0;
    let nav = |x: f32| Rect {
        x,
        y: nav_y,
        w: NAV_W,
        h: NAV_H.min(photo.h),
    };
    let prev = (current > 0).then(|| nav(photo.x + 8.0));
    let next = (current + 1 < count).then(|| nav(photo.x + photo.w - NAV_W - 8.0));
    let caption = Rect {
        x: stage.x + 8.0,
        y: stage.y + stage.h - 10.0 - CAPTION_H,
        w: (stage.w - 16.0).max(0.0),
        h: CAPTION_H,
    };
    let strip_y = caption.y - 8.0 - STRIP_H;
    let fit = (((stage.w - 32.0 + STRIP_GAP) / (STRIP_W + STRIP_GAP)).floor() as usize).max(1);
    let shown = count.min(fit);
    let first = current
        .saturating_sub(shown / 2)
        .min(count.saturating_sub(shown));
    let width = shown as f32 * (STRIP_W + STRIP_GAP) - STRIP_GAP;
    let x0 = stage.x + (stage.w - width) / 2.0;
    let strip = (0..shown)
        .map(|k| {
            let r = Rect {
                x: x0 + k as f32 * (STRIP_W + STRIP_GAP),
                y: strip_y,
                w: STRIP_W,
                h: STRIP_H,
            };
            (first + k, r)
        })
        .collect();
    StageParts {
        photo,
        prev,
        next,
        strip,
        caption,
    }
}

impl StageParts {
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if self.prev.is_some_and(|r| r.contains(x, y)) {
            return Some(Hit::Nav(-1));
        }
        if self.next.is_some_and(|r| r.contains(x, y)) {
            return Some(Hit::Nav(1));
        }
        self.strip
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(i, _)| Hit::Strip(*i))
    }
}
