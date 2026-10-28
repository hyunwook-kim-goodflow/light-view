//! Drawing the resize screen (spec §4, the approved mockup): around the photo the ‹ ›
//! arrows, the filmstrip with the current photo in the highlighter and the caption
//! whose result part is highlighted; the sidebar with its tabs, rows, the estimate
//! list (the current photo on the wash), the total and the buttons. The photo itself
//! is the viewer's, drawn by the window first.

use lv_core::settings::{ResizeFormat, ResizeTab, SidebarSide};

use super::session::{Estimate, ReplaceBlock, ResizeSession};
use super::sidebar::{
    controls, list_rows, sidebar_layout, stage_parts, stage_rect, tab_fields, Ctl, Field, Row,
    SidebarLayout, StageParts, LIST_NOTE_H, LIST_ROW_H,
};
use super::text;
use crate::render::{Align, Renderer, TextRun};
use crate::strings::{text as tr, Lang, S};
use crate::theme::{Palette, Rgb};
use crate::toolbar::Rect;

const UI: &str = "Segoe UI";
const RADIUS: f32 = 8.0;
const FOCUS_OUTSET: f32 = 3.0;
const CHECK_BOX: f32 = 18.0;
const CHECK_GLYPH: char = '\u{E73E}';
const PREV_GLYPH: char = '\u{E76B}';
const NEXT_GLYPH: char = '\u{E76C}';

/// Where everything is, for drawing, hit tests and UI Automation.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub stage: Rect,
    pub parts: StageParts,
    pub layout: SidebarLayout,
    /// The list rows shown: (photo index, rect).
    pub rows: Vec<(usize, Rect)>,
}

/// The screen's geometry in a `client` sized window (DIPs, `dip` DIPs per pixel), the
/// sidebar's middle scrolled to show the focused control `focus` (short windows).
pub fn geometry(
    client: (f32, f32),
    side: SidebarSide,
    dip: f32,
    s: &ResizeSession,
    focus: Ctl,
) -> Geometry {
    let view = Rect {
        x: 0.0,
        y: 0.0,
        w: client.0,
        h: client.1,
    };
    let stage = stage_rect(view, side, dip);
    let layout = sidebar_layout(view, side, dip).scrolled_for(focus, &s.memory);
    let notes: Vec<bool> = s
        .items
        .iter()
        .map(|i| i.animated || i.scaled || i.save_error.is_some())
        .collect();
    let rows = list_rows(layout.list, &notes, s.current);
    Geometry {
        stage,
        parts: stage_parts(stage, s.items.len(), s.current),
        layout,
        rows,
    }
}

/// What the screen shows besides the geometry.
pub struct ScreenView<'a> {
    pub session: &'a ResizeSession,
    pub block: Option<ReplaceBlock>,
    /// (done, total) while a save runs.
    pub progress: Option<(usize, usize)>,
    pub focus: Ctl,
    pub palette: &'a Palette,
    pub icon_family: &'a str,
    pub lang: Lang,
}

fn t(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
}

fn run(text: &str, color: Rgb, size: f32, bold: bool) -> TextRun<'_> {
    TextRun {
        text,
        family: UI,
        size,
        bold,
        color,
    }
}

pub fn draw(r: &mut Renderer, g: &Geometry, v: &ScreenView) {
    draw_stage(r, g, v);
    draw_sidebar(r, g, v);
}

fn draw_stage(r: &mut Renderer, g: &Geometry, v: &ScreenView) {
    let p = v.palette;
    let s = v.session;
    // The band under the photo is the paper again (a zoomed photo may reach it).
    let band = Rect {
        y: g.parts.photo.y + g.parts.photo.h,
        h: g.stage.h - g.parts.photo.h,
        ..g.stage
    };
    r.fill_round_rect(t(band), 0.0, p.paper);
    for (rect, glyph) in [(g.parts.prev, PREV_GLYPH), (g.parts.next, NEXT_GLYPH)] {
        if let Some(rect) = rect {
            r.fill_rect_alpha(t(rect), p.surface, 0.72);
            r.draw_glyph(t(rect), glyph, v.icon_family, 18.0, p.ink, false);
        }
    }
    // The filmstrip: each photo's tile (its number until the tile is ready), the
    // current one outlined in the highlighter.
    for (i, cell) in &g.parts.strip {
        let on = *i == s.current;
        match s.items.get(*i).and_then(|item| item.thumb.as_ref()) {
            Some(th) => {
                r.fill_round_rect(t(*cell), 3.0, p.surface2);
                r.draw_thumb(th.id, th.size, &th.bgra, t(*cell));
                // The tile's square corners are masked to the cell's rounding.
                let mask = (cell.x - 2.0, cell.y - 2.0, cell.w + 4.0, cell.h + 4.0);
                r.stroke_round_rect(mask, 5.0, p.paper, 2.0);
            }
            None => {
                r.fill_round_rect(t(*cell), 3.0, p.surface2);
                let n = (i + 1).to_string();
                r.draw_runs(t(*cell), &[run(&n, p.ink2, 11.0, on)], Align::Center);
            }
        }
        if on {
            let ring = (cell.x - 2.0, cell.y - 2.0, cell.w + 4.0, cell.h + 4.0);
            r.stroke_round_rect(ring, 4.0, p.highlighter, 2.0);
        }
    }
    let Some(item) = s.items.get(s.current) else {
        return;
    };
    let (src, result) = text::caption(v.lang, item);
    let left = [run(&src, p.ink, 12.5, false)];
    let right = [run(&result, p.on_highlighter, 12.5, true)];
    let (lw, rw) = (
        r.measure_runs(&left).unwrap_or(200.0),
        r.measure_runs(&right).unwrap_or(100.0),
    );
    let c = g.parts.caption;
    let pad = 6.0;
    let total = lw + rw + 2.0 * pad;
    let x = c.x + ((c.w - total) / 2.0).max(0.0);
    let lw = lw.min((c.w - rw - 2.0 * pad).max(0.0));
    r.draw_runs_ellipsis((x, c.y, lw, c.h), &left, Align::Leading);
    let mark = (x + lw, c.y + 2.0, (rw + 2.0 * pad).min(c.w), c.h - 4.0);
    r.fill_round_rect(mark, 4.0, p.highlighter);
    r.draw_runs(mark, &right, Align::Center);
}

fn draw_sidebar(r: &mut Renderer, g: &Geometry, v: &ScreenView) {
    let p = v.palette;
    let l = &g.layout;
    let s = v.session;
    let m = &s.memory;
    let saving = v.progress.is_some();
    r.fill_round_rect(t(l.panel), 0.0, p.surface);
    // The hairline between the stage and the sidebar.
    let edge_x = if l.panel.x > g.stage.x {
        l.panel.x
    } else {
        l.panel.x + l.panel.w - 1.0
    };
    r.fill_round_rect((edge_x, l.panel.y, 1.0, l.panel.h), 0.0, p.hairline);
    let count = format!("{} / {}", s.current + 1, s.items.len());
    r.draw_runs(
        t(l.title),
        &[run(tr(v.lang, S::Resize), p.ink, 15.0, true)],
        Align::Leading,
    );
    r.draw_runs(
        t(l.title),
        &[run(&count, p.ink2, 12.0, false)],
        Align::Trailing,
    );
    // Tabs: the chosen one bold with a highlighter underline, a hairline under all.
    let tabs = l.tabs.iter().zip(Ctl::Tabs.option_labels(v.lang));
    let chosen_tab = Ctl::Tabs.selected(m, s.replace);
    for (k, (rect, label)) in tabs.enumerate() {
        let on = chosen_tab == Some(k);
        let color = if on { p.ink } else { p.ink2 };
        r.draw_runs(t(*rect), &[run(&label, color, 13.0, on)], Align::Center);
        if on {
            let bar = (rect.x + 8.0, rect.y + rect.h - 3.0, rect.w - 16.0, 3.0);
            r.fill_round_rect(bar, 1.5, p.highlighter);
        }
    }
    let under = l.tabs[0];
    r.fill_round_rect(
        (l.title.x, under.y + under.h, l.title.w, 1.0),
        0.0,
        p.hairline,
    );
    // The tab's numbers.
    for f in tab_fields(m.tab) {
        if let (Some(row), Some(frame)) = (l.row_of(Ctl::Field(*f), m), l.field_rect(*f, m)) {
            draw_field(r, row, frame, *f, v);
        }
    }
    if m.tab == ResizeTab::Resolution {
        draw_choice(r, l, Ctl::Axis, v);
    }
    // The middle (outputs and list) scrolls in its viewport in a short window.
    r.push_clip(t(l.viewport));
    section(r, l.output, tr(v.lang, S::ResizeOutput), p);
    draw_choice(r, l, Ctl::Format, v);
    draw_choice(r, l, Ctl::Quality, v);
    draw_choice(r, l, Ctl::SaveMode, v);
    if let Some(block) = v.block {
        let reason = [run(text::block_reason(v.lang, block), p.ink2, 12.0, false)];
        let x = l.save_mode.control.x;
        let rect = (x, l.reason.y, l.reason.x + l.reason.w - x, l.reason.h);
        r.draw_runs_ellipsis(rect, &reason, Align::Leading);
    }
    label(r, l.metadata, tr(v.lang, S::ResizeMetadata), p);
    let gps = tr(v.lang, S::ResizeStripGps);
    draw_checkbox(r, l.metadata.control, gps, m.strip_gps, !saving, v);
    section(r, l.estimate, tr(v.lang, S::ResizeEstimate), p);
    draw_list(r, g, v);
    r.pop_clip();
    if let Some(total) = text::total_line(v.lang, &s.items) {
        r.draw_runs_ellipsis(
            t(l.total),
            &[run(&total, p.ink2, 12.0, false)],
            Align::Leading,
        );
    }
    match v.progress {
        Some((done, total)) => {
            let line = text::progress(v.lang, done, total);
            let row = Rect {
                w: l.save.x + l.save.w - l.cancel.x,
                ..l.cancel
            };
            r.fill_round_rect(t(row), RADIUS, p.surface2);
            r.draw_runs(t(row), &[run(&line, p.ink, 13.0, false)], Align::Center);
        }
        None => {
            r.fill_round_rect(t(l.cancel), RADIUS, p.surface2);
            let cancel = [run(tr(v.lang, S::BtnCancel), p.ink, 13.0, false)];
            r.draw_runs(t(l.cancel), &cancel, Align::Center);
            r.fill_round_rect(t(l.save), RADIUS, p.ink);
            let label = text::save_button(v.lang, save_count(s));
            r.draw_runs(
                t(l.save),
                &[run(&label, p.paper, 13.0, true)],
                Align::Center,
            );
        }
    }
    let focus = v.focus;
    let shown =
        controls(m).contains(&focus) && !(saving && matches!(focus, Ctl::Cancel | Ctl::Save));
    if let (true, Some(f)) = (shown, l.focus_rect(focus, m)) {
        let ring = (
            f.x - FOCUS_OUTSET,
            f.y - FOCUS_OUTSET,
            f.w + 2.0 * FOCUS_OUTSET,
            f.h + 2.0 * FOCUS_OUTSET,
        );
        let middle = l.in_middle(focus);
        if middle {
            r.push_clip(t(l.viewport));
        }
        r.stroke_round_rect(ring, RADIUS + FOCUS_OUTSET, p.ink, 1.5);
        if middle {
            r.pop_clip();
        }
    }
}

/// How many photos [저장 (N장)] saves: all but those that cannot fit.
pub fn save_count(s: &ResizeSession) -> usize {
    s.items
        .iter()
        .filter(|i| i.estimate != Estimate::CantFit)
        .count()
}

fn section(r: &mut Renderer, rect: Rect, text: &str, p: &Palette) {
    let runs = [run(text, p.ink2, 11.5, true)];
    r.draw_runs(t(rect), &runs, Align::Leading);
}

fn label(r: &mut Renderer, row: Row, text: &str, p: &Palette) {
    r.draw_runs_ellipsis(
        t(row.label),
        &[run(text, p.ink2, 13.0, false)],
        Align::Leading,
    );
}

/// A number's row: its label, the frame its EDIT control sits in, the unit after it.
fn draw_field(r: &mut Renderer, row: Row, frame: Rect, f: Field, v: &ScreenView) {
    let p = v.palette;
    let m = &v.session.memory;
    label(r, row, tr(v.lang, f.label(m)), p);
    r.fill_round_rect(t(frame), 6.0, p.paper);
    r.stroke_round_rect(t(frame), 6.0, p.hairline, 1.0);
    let mut unit = f.unit().to_owned();
    if f == Field::Limit {
        unit = format!("{unit}  {}", tr(v.lang, S::ResizeOptional));
    }
    let after = Rect {
        x: frame.x + frame.w + 8.0,
        w: (row.control.x + row.control.w - frame.x - frame.w - 8.0).max(0.0),
        ..row.control
    };
    r.draw_runs_ellipsis(t(after), &[run(&unit, p.ink2, 13.0, false)], Align::Leading);
}

/// A choice's row: its label, then its options; the chosen one in the highlighter.
/// Options that cannot be chosen (원본 대체 while blocked; all while saving; 품질 for
/// PNG) are drawn without a fill.
fn draw_choice(r: &mut Renderer, l: &SidebarLayout, c: Ctl, v: &ScreenView) {
    let p = v.palette;
    let s = v.session;
    let m = &s.memory;
    let Some(row) = l.row_of(c, m) else {
        return;
    };
    if let Some(key) = c.label() {
        label(r, row, tr(v.lang, key), p);
    }
    let none = c == Ctl::Quality && m.format == ResizeFormat::Png;
    let chosen = if none { None } else { c.selected(m, s.replace) };
    let ctx_saving = v.progress.is_some();
    for (k, (rect, text)) in l
        .options(c, m)
        .iter()
        .zip(c.option_labels(v.lang))
        .enumerate()
    {
        let on = chosen == Some(k);
        let blocked = c == Ctl::SaveMode && k == 1 && v.block.is_some();
        let dim = none || blocked || ctx_saving;
        let (fill, color) = match (on, dim) {
            (true, _) => (p.highlighter, p.on_highlighter),
            (false, true) => (p.surface, p.ink2),
            (false, false) => (p.surface2, p.ink),
        };
        r.fill_round_rect(t(*rect), RADIUS, fill);
        if dim && !on {
            r.stroke_round_rect(t(*rect), RADIUS, p.hairline, 1.0);
        }
        let label = [run(&text, color, 12.5, on)];
        r.draw_runs_ellipsis(t(*rect), &label, Align::Center);
    }
}

fn draw_checkbox(
    r: &mut Renderer,
    row: Rect,
    label: &str,
    on: bool,
    enabled: bool,
    v: &ScreenView,
) {
    let p = v.palette;
    let mark = (
        row.x + 2.0,
        row.y + (row.h - CHECK_BOX) / 2.0,
        CHECK_BOX,
        CHECK_BOX,
    );
    if on {
        r.fill_round_rect(mark, 4.0, p.highlighter);
        r.draw_glyph(
            mark,
            CHECK_GLYPH,
            v.icon_family,
            13.0,
            p.on_highlighter,
            false,
        );
    } else {
        r.stroke_round_rect(mark, 4.0, p.ink2, 1.5);
    }
    let x = mark.0 + CHECK_BOX + 8.0;
    let color = if enabled { p.ink } else { p.ink2 };
    let rect = (x, row.y, row.x + row.w - x, row.h);
    r.draw_runs_ellipsis(rect, &[run(label, color, 13.0, false)], Align::Leading);
}

/// The estimate list: name and source on the left, the result on the right, a note
/// line for animations and photos opened at reduced size; the current row on the wash.
fn draw_list(r: &mut Renderer, g: &Geometry, v: &ScreenView) {
    let p = v.palette;
    let s = v.session;
    for (i, rect) in &g.rows {
        let Some(item) = s.items.get(*i) else {
            continue;
        };
        if *i == s.current {
            r.fill_round_rect(t(*rect), 6.0, p.wash);
        }
        let x = rect.x + 6.0;
        let w = rect.w - 12.0;
        let right_w = (w * 0.42).min(130.0);
        let left_w = w - right_w - 6.0;
        let line = |k: f32, h: f32| (x, rect.y + 4.0 + k, left_w, h);
        let name = text::file_name(&item.path);
        r.draw_runs_ellipsis(
            line(0.0, 19.0),
            &[run(&name, p.ink, 13.0, true)],
            Align::Leading,
        );
        let src = text::source_text(item);
        r.draw_runs_ellipsis(
            line(19.0, 17.0),
            &[run(&src, p.ink2, 12.0, false)],
            Align::Leading,
        );
        let (top, bottom) = text::row_result(v.lang, &item.estimate);
        let bold = matches!(item.estimate, Estimate::CantFit);
        let rx = x + w - right_w;
        let right = |k: f32, h: f32| (rx, rect.y + 4.0 + k, right_w, h);
        r.draw_runs_ellipsis(
            right(0.0, 19.0),
            &[run(&top, p.ink, 12.5, bold)],
            Align::Trailing,
        );
        r.draw_runs_ellipsis(
            right(19.0, 17.0),
            &[run(&bottom, p.ink, 12.5, false)],
            Align::Trailing,
        );
        if let Some(note) = text::note(v.lang, item) {
            let rect = (x, rect.y + LIST_ROW_H - 4.0, w, LIST_NOTE_H);
            r.draw_runs_ellipsis(rect, &[run(note, p.ink2, 12.0, false)], Align::Leading);
        }
    }
}
