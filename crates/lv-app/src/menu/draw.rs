//! Drawing of the right-click menu through `Renderer`.

use crate::render::{Align, Renderer, TextRun};
use crate::theme::{Palette, Rgb};
use crate::toolbar::Rect;

use super::{Menu, MenuLayout, ITEMS, RADIUS, ROW_INSET, ROW_RADIUS, TEXT_PAD};

const UI_FAMILY: &str = "Segoe UI";
const TEXT_SIZE: f32 = 14.0;

pub struct MenuView<'a> {
    pub palette: &'a Palette,
    pub dark: bool,
}

fn tuple(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
}

fn run(text: &str, color: Rgb) -> TextRun<'_> {
    TextRun {
        text,
        family: UI_FAMILY,
        size: TEXT_SIZE,
        bold: false,
        color,
    }
}

/// Surface card (soft shadow in light mode, hairline in dark mode), the selected row on
/// wash, labels in ink (ink2 when unavailable), shortcuts in ink2 at the right.
pub fn draw(r: &mut Renderer, menu: &Menu, l: &MenuLayout, v: &MenuView) {
    let p = v.palette;
    if !v.dark {
        r.fill_soft_shadow(tuple(l.card), RADIUS, p.ink, (4.0, 16.0, 0.12));
    }
    r.fill_round_rect(tuple(l.card), RADIUS, p.surface);
    if v.dark {
        r.stroke_round_rect(tuple(l.card), RADIUS, p.hairline, 1.0);
    }
    let sep = l.separator;
    r.fill_rect_alpha(
        (
            sep.x + TEXT_PAD,
            sep.y + sep.h / 2.0,
            sep.w - 2.0 * TEXT_PAD,
            1.0,
        ),
        p.hairline,
        1.0,
    );
    for (i, (item, row)) in ITEMS.iter().zip(&l.items).enumerate() {
        if menu.selected == Some(i) {
            let bg = Rect {
                x: row.x + ROW_INSET,
                w: row.w - 2.0 * ROW_INSET,
                ..*row
            };
            r.fill_round_rect(tuple(bg), ROW_RADIUS, p.wash);
        }
        let color = if menu.enabled(i) { p.ink } else { p.ink2 };
        let text = (row.x + TEXT_PAD, row.y, row.w - 2.0 * TEXT_PAD, row.h);
        r.draw_runs_ellipsis(text, &[run(item.label(), color)], Align::Leading);
        if let Some(keys) = item.shortcut() {
            r.draw_runs(text, &[run(keys, p.ink2)], Align::Trailing);
        }
    }
}
