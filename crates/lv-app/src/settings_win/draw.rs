//! Layout and drawing of the settings window: the controls' rects in DIPs and how they
//! are painted.

use lv_core::settings::Settings;

use crate::about;
use crate::render::{Align, Renderer, TextRun};
use crate::strings::{self, t, S};
use crate::theme::{Palette, Rgb};
use crate::toolbar::Rect;

use super::model::{Control, Kind, Model, CONTROLS};
use super::SIZE_DIP;

// Layout (DIPs).
const PAD_X: f32 = 24.0;
const TOP: f32 = 16.0;
const LABEL_H: f32 = 20.0;
const LABEL_GAP: f32 = 2.0;
const ROW_H: f32 = 44.0;
const OPTION_GAP: f32 = 6.0;
const SECTION_GAP: f32 = 8.0;
const NOTE_H: f32 = 16.0;
const VERSION_H: f32 = 18.0;
const BOTTOM: f32 = 12.0;

/// One control's rects: a choice's label above its options; the checkbox's row or the
/// button as its only "option". `focus` is what the focus ring goes around.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlRects {
    pub label: Option<Rect>,
    pub options: Vec<Rect>,
    pub focus: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Follows `CONTROLS`.
    pub controls: Vec<ControlRects>,
    /// "다음 실행부터 적용돼요", under the memory options.
    pub note: Rect,
    /// "다음에 사진을 열 때부터 적용돼요", under the open-in options.
    pub open_note: Rect,
    pub version: Rect,
}

/// The controls top to bottom inside 24 DIP side padding, each option row 44 DIP tall
/// with equal option widths; the version line at the bottom.
pub fn layout((w, h): (f32, f32)) -> Layout {
    let x = PAD_X;
    let inner = (w - 2.0 * PAD_X).max(0.0);
    let row = |y: f32| Rect {
        x,
        y,
        w: inner,
        h: ROW_H,
    };
    let mut y = TOP;
    let mut note = Rect {
        x,
        y,
        w: inner,
        h: NOTE_H,
    };
    let mut open_note = note;
    let mut controls = Vec::with_capacity(CONTROLS.len());
    for c in CONTROLS {
        let rects = match c.kind() {
            Kind::Choice => {
                let label = Rect {
                    x,
                    y,
                    w: inner,
                    h: LABEL_H,
                };
                y += LABEL_H + LABEL_GAP;
                let n = c.option_count();
                let ow = (inner - OPTION_GAP * (n as f32 - 1.0)) / n as f32;
                let options = (0..n)
                    .map(|i| Rect {
                        x: x + i as f32 * (ow + OPTION_GAP),
                        y,
                        w: ow,
                        h: ROW_H,
                    })
                    .collect();
                ControlRects {
                    label: Some(label),
                    options,
                    focus: row(y),
                }
            }
            Kind::Check | Kind::Button => ControlRects {
                label: None,
                options: vec![row(y)],
                focus: row(y),
            },
        };
        y += ROW_H;
        match c {
            Control::Memory => {
                note.y = y;
                y += NOTE_H;
            }
            Control::OpenIn => {
                open_note.y = y;
                y += NOTE_H;
            }
            _ => {}
        }
        y += SECTION_GAP;
        controls.push(rects);
    }
    Layout {
        controls,
        note,
        open_note,
        version: Rect {
            x,
            y: h - BOTTOM - VERSION_H,
            w: inner,
            h: VERSION_H,
        },
    }
}
// ---- Drawing -------------------------------------------------------------------

const UI_FAMILY: &str = "Segoe UI";
const LABEL_SIZE: f32 = 13.0;
const TEXT_SIZE: f32 = 14.0;
const SMALL_SIZE: f32 = 12.0;
const RADIUS: f32 = 10.0;
const CHECK_BOX: f32 = 20.0;
const CHECK_GLYPH: char = '\u{E73E}';
const FOCUS_OUTSET: f32 = 3.0;

fn tuple(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
}

fn run(text: &str, color: Rgb, size: f32, bold: bool) -> TextRun<'_> {
    TextRun {
        text,
        family: UI_FAMILY,
        size,
        bold,
        color,
    }
}

/// Paper background; choices as rows of surface2 options with the chosen one in
/// highlighter; the focus ring in ink around the focused control.
pub(super) fn draw(r: &mut Renderer, m: &Model, s: &Settings, p: &Palette, icon_family: &str) {
    let lang = strings::current();
    let l = layout(SIZE_DIP);
    r.begin(p.paper);
    for (i, (c, rects)) in CONTROLS.iter().zip(&l.controls).enumerate() {
        if let Some(label) = rects.label {
            let label_run = [run(t(c.label_key()), p.ink2, LABEL_SIZE, false)];
            r.draw_runs_ellipsis(tuple(label), &label_run, Align::Leading);
        }
        match c.kind() {
            Kind::Choice => {
                let chosen = c.selected(s);
                let labels = c.option_labels(lang);
                for (j, (rect, text)) in rects.options.iter().zip(&labels).enumerate() {
                    let on = chosen == Some(j);
                    let (fill, color) = if on {
                        (p.highlighter, p.on_highlighter)
                    } else {
                        (p.surface2, p.ink)
                    };
                    r.fill_round_rect(tuple(*rect), RADIUS, fill);
                    let label = [run(text, color, LABEL_SIZE, on)];
                    r.draw_runs_ellipsis(tuple(*rect), &label, Align::Center);
                }
            }
            Kind::Check => {
                let label = t(c.label_key());
                draw_checkbox(r, rects.options[0], label, c.checked(s), p, icon_family);
            }
            Kind::Button => {
                let rect = rects.options[0];
                r.fill_round_rect(tuple(rect), RADIUS, p.surface2);
                let label = [run(t(c.label_key()), p.ink, TEXT_SIZE, false)];
                r.draw_runs_ellipsis(tuple(rect), &label, Align::Center);
            }
        }
        if i == m.focused {
            let f = rects.focus;
            let ring = (
                f.x - FOCUS_OUTSET,
                f.y - FOCUS_OUTSET,
                f.w + 2.0 * FOCUS_OUTSET,
                f.h + 2.0 * FOCUS_OUTSET,
            );
            r.stroke_round_rect(ring, RADIUS + FOCUS_OUTSET, p.ink, 1.5);
        }
    }
    for (rect, key) in [
        (l.open_note, S::SettingsOpenOtherNote),
        (l.note, S::SettingsMemoryNote),
    ] {
        let note = [run(t(key), p.ink2, SMALL_SIZE, false)];
        r.draw_runs_ellipsis(tuple(rect), &note, Align::Leading);
    }
    let version = about::version_line();
    let version = [run(&version, p.ink2, SMALL_SIZE, false)];
    r.draw_runs(tuple(l.version), &version, Align::Leading);
    if let Err(e) = r.end() {
        eprintln!("light-view: EndDraw failed in the settings window: {e}");
    }
}

fn draw_checkbox(
    r: &mut Renderer,
    row: Rect,
    label: &str,
    checked: bool,
    p: &Palette,
    icon_family: &str,
) {
    let mark = (
        row.x + 4.0,
        row.y + (row.h - CHECK_BOX) / 2.0,
        CHECK_BOX,
        CHECK_BOX,
    );
    if checked {
        r.fill_round_rect(mark, 5.0, p.highlighter);
        r.draw_glyph(
            mark,
            CHECK_GLYPH,
            icon_family,
            14.0,
            p.on_highlighter,
            false,
        );
    } else {
        r.stroke_round_rect(mark, 5.0, p.ink2, 1.5);
    }
    let label_x = mark.0 + CHECK_BOX + 10.0;
    let text = (label_x, row.y, row.x + row.w - label_x, row.h);
    r.draw_runs_ellipsis(text, &[run(label, p.ink, TEXT_SIZE, false)], Align::Leading);
}
