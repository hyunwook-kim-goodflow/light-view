//! Drawing the EXIF card and its full list through `Renderer`.

use super::*;
use crate::render::{Align, Renderer, TextRun};
use crate::strings::t;
use crate::theme::{Palette, Rgb};

const UI_FAMILY: &str = "Segoe UI";
const SERIF_FAMILY: &str = "Georgia";
const CLOSE_GLYPH: char = '\u{E711}';
const BACK_GLYPH: char = '\u{E72B}';

pub struct CardView<'a> {
    pub palette: &'a Palette,
    pub dark: bool,
    pub icon_family: &'a str,
}

fn tuple(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
}

fn run<'a>(text: &'a str, family: &'a str, size: f32, bold: bool, color: Rgb) -> TextRun<'a> {
    TextRun {
        text,
        family,
        size,
        bold,
        color,
    }
}

fn inset(r: Rect, dx: f32) -> Rect {
    Rect {
        x: r.x + dx,
        w: (r.w - 2.0 * dx).max(0.0),
        ..r
    }
}

pub fn draw(r: &mut Renderer, card: &ExifCard, m: &Model, v: &CardView) {
    if !card.open {
        return;
    }
    let p = v.palette;
    let l = &m.layout;
    // Light: soft shadow. Dark: no shadow; a hairline lifts the card.
    if !v.dark {
        r.fill_soft_shadow(tuple(l.card), RADIUS, p.ink, (4.0, 14.0, 0.08));
    }
    r.fill_round_rect(tuple(l.card), RADIUS, p.surface);
    if v.dark {
        r.stroke_round_rect(tuple(l.card), RADIUS, p.hairline, 1.0);
    }
    let glyph = if m.expanded { BACK_GLYPH } else { CLOSE_GLYPH };
    r.draw_glyph(tuple(l.close), glyph, v.icon_family, 12.0, p.ink2, false);
    let heading = if m.expanded {
        run(t(S::ExifShowAll), UI_FAMILY, 15.0, true, p.ink)
    } else if m.text.serif {
        run(&m.text.heading, SERIF_FAMILY, 22.0, false, p.ink)
    } else {
        run(&m.text.heading, UI_FAMILY, 15.0, true, p.ink)
    };
    r.draw_runs_ellipsis(tuple(l.heading), &[heading], Align::Leading);
    for (line, rect) in m.text.lines.iter().zip(&l.lines) {
        match line {
            Line::Plain(s) => {
                let runs = [run(s, UI_FAMILY, 13.0, false, p.ink2)];
                r.draw_runs_ellipsis(tuple(*rect), &runs, Align::Leading);
            }
            Line::Exposure(s) => {
                r.fill_round_rect(tuple(*rect), 8.0, p.wash);
                let runs = [run(s, UI_FAMILY, 13.0, false, p.ink)];
                r.draw_runs_ellipsis(tuple(inset(*rect, 10.0)), &runs, Align::Leading);
            }
        }
    }
    if let Some(b) = l.expand {
        r.fill_round_rect(tuple(b), 12.0, p.surface2);
        let label = [run(t(S::ExifShowAll), UI_FAMILY, 14.0, false, p.ink)];
        r.draw_runs(tuple(b), &label, Align::Center);
    }
    if let Some(list) = l.list {
        draw_list(r, card, m, list, p);
    }
    if let Some(b) = l.copy {
        r.fill_round_rect(tuple(b), 12.0, p.highlighter);
        let label = [run(t(S::ExifCopy), UI_FAMILY, 14.0, true, p.on_highlighter)];
        r.draw_runs(tuple(b), &label, Align::Center);
    }
}

fn draw_list(r: &mut Renderer, card: &ExifCard, m: &Model, list: Rect, p: &Palette) {
    let Some(exif) = &m.exif else { return };
    let scroll = card.scroll(m);
    let selected = card.selected(m);
    let scrolls = max_scroll(&m.rows, list.h) > 0;
    let content_w = list.w - if scrolls { SCROLLBAR_W + 4.0 } else { 0.0 };
    for (i, rect) in visible_rows(&m.rows, scroll, list) {
        let rect = Rect {
            w: content_w,
            ..rect
        };
        match m.rows[i] {
            Row::Group(e) => {
                let runs = [run(&exif.entries[e].group, UI_FAMILY, 11.0, true, p.ink2)];
                r.draw_runs_ellipsis(tuple(inset(rect, 8.0)), &runs, Align::Leading);
            }
            Row::Entry(e) => {
                if Some(i) == selected {
                    r.fill_round_rect(tuple(rect), 8.0, p.wash);
                }
                let entry = &exif.entries[e];
                let inner = inset(rect, 8.0);
                let name_w = inner.w * NAME_SHARE;
                let name = Rect {
                    w: (name_w - 8.0).max(0.0),
                    ..inner
                };
                let value = Rect {
                    x: inner.x + name_w,
                    w: inner.w - name_w,
                    ..inner
                };
                let n = [run(&entry.name, UI_FAMILY, 12.0, false, p.ink2)];
                r.draw_runs_ellipsis(tuple(name), &n, Align::Leading);
                let v = [run(&entry.value, UI_FAMILY, 13.0, false, p.ink)];
                r.draw_runs_ellipsis(tuple(value), &v, Align::Leading);
            }
        }
    }
    if scrolls {
        // A thumb sized and placed by rows (the list scrolls a row at a time).
        let total = m.rows.len() as f32;
        let shown = fit_count(&m.rows, scroll, list.h) as f32;
        let track = (list.x + list.w - SCROLLBAR_W, list.y, SCROLLBAR_W, list.h);
        let thumb_h = (list.h * shown / total).max(16.0).min(list.h);
        let thumb_y =
            list.y + (list.h - thumb_h) * scroll as f32 / max_scroll(&m.rows, list.h).max(1) as f32;
        r.fill_round_rect(track, SCROLLBAR_W / 2.0, p.surface2);
        r.fill_round_rect(
            (track.0, thumb_y, SCROLLBAR_W, thumb_h),
            SCROLLBAR_W / 2.0,
            p.ink2,
        );
    }
}
