//! Drawing of the overlay: the scrim, the dialog card and the toast through `Renderer`.

use crate::render::{Align, Renderer, TextRun};
use crate::strings::{t, S};
use crate::theme::{Palette, Rgb};
use crate::toolbar::Rect;

use super::dialog::{dialog_inner_width, dialog_layout, Dialog, Item, BUTTON_RADIUS, CARD_RADIUS};
use super::toast::{toast_layout, toast_parts, Toast, HIT_MIN, TOAST_RADIUS};
use super::Overlay;

// ---- Drawing -------------------------------------------------------------------

const UI_FAMILY: &str = "Segoe UI";
const TITLE_SIZE: f32 = 15.0;
const TEXT_SIZE: f32 = 14.0;
const DETAIL_SIZE: f32 = 13.0;
const CHECK_BOX: f32 = 20.0;
const CHECK_GLYPH: char = '\u{E73E}';
const FOCUS_OUTSET: f32 = 3.0;

/// Scrim over the photo: light `ink` at 30%, dark `paper` at 60%.
pub fn scrim(p: &Palette, dark: bool) -> (Rgb, f32) {
    if dark {
        (p.paper, 0.6)
    } else {
        (p.ink, 0.3)
    }
}

/// Toast background, text and optional border: inverted ink (ink + paper) in light
/// mode; surface2 + ink with a hairline in dark mode. The dark pair deliberately departs
/// from the brief's literal "inverted" wording (light ink background with dark text
/// would glare on a dark window); it is contrast-tested at >= 4.5:1 (ruling R15).
pub fn toast_colors(p: &Palette, dark: bool) -> (Rgb, Rgb, Option<Rgb>) {
    if dark {
        (p.surface2, p.ink, Some(p.hairline))
    } else {
        (p.ink, p.paper, None)
    }
}

/// Everything the overlay draws besides its own state.
pub struct OverlayView<'a> {
    /// Client area size in DIPs.
    pub client: (f32, f32),
    pub pinned: bool,
    pub palette: &'a Palette,
    pub dark: bool,
    pub icon_family: &'a str,
    pub now_ms: u64,
}

fn tuple(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
}

fn outset(r: Rect, dx: f32, dy: f32) -> Rect {
    Rect {
        x: r.x - dx,
        y: r.y - dy,
        w: r.w + 2.0 * dx,
        h: r.h + 2.0 * dy,
    }
}

fn ui_run(text: &str, color: Rgb, size: f32, bold: bool) -> TextRun<'_> {
    TextRun {
        text,
        family: UI_FAMILY,
        size,
        bold,
        color,
    }
}

/// The toast, then the dialog above it. Places the toast for hit testing.
pub fn draw(r: &mut Renderer, o: &mut Overlay, v: &OverlayView) {
    if let Some(t) = o.toast.as_mut().filter(|t| t.visible(v.now_ms)) {
        draw_toast(r, t, v);
    }
    if let Some(d) = &mut o.dialog {
        // Measured first: the layout (and later hit tests) use the line count.
        let width = dialog_inner_width(v.client.0);
        if let Some(n) = r.wrapped_line_count(d.prompt.title(), UI_FAMILY, TITLE_SIZE, width) {
            d.title_lines = Some(n);
        }
        draw_dialog(r, d, v);
    }
}

fn draw_toast(r: &mut Renderer, t: &mut Toast, v: &OverlayView) {
    let p = v.palette;
    let (bg, fg, border) = toast_colors(p, v.dark);
    let anchor = toast_layout(v.client.0, v.client.1, v.pinned);
    let text = [ui_run(&t.text, fg, TEXT_SIZE, false)];
    let label = t
        .action
        .as_ref()
        .map(|(l, _)| [ui_run(l, p.on_highlighter, TEXT_SIZE, true)]);
    let text_w = r.measure_runs(&text).unwrap_or(anchor.w);
    let action_w = label
        .as_ref()
        .map(|l| r.measure_runs(l).unwrap_or(2.0 * HIT_MIN));
    let parts = toast_parts(anchor, text_w, action_w);
    if !v.dark {
        r.fill_soft_shadow(tuple(parts.bg), TOAST_RADIUS, p.ink, (4.0, 14.0, 0.08));
    }
    r.fill_round_rect(tuple(parts.bg), TOAST_RADIUS, bg);
    if let Some(c) = border {
        r.stroke_round_rect(tuple(parts.bg), TOAST_RADIUS, c, 1.0);
    }
    r.draw_runs_ellipsis(tuple(parts.text), &text, Align::Leading);
    if let (Some(pill), Some(label)) = (parts.pill, label) {
        r.fill_round_rect(tuple(pill), pill.h / 2.0, p.highlighter);
        r.draw_runs(tuple(pill), &label, Align::Center);
    }
    t.place(parts);
}

fn draw_dialog(r: &mut Renderer, d: &Dialog, v: &OverlayView) {
    let p = v.palette;
    let (w, h) = v.client;
    let l = dialog_layout(w, h, d);
    let (scrim_color, alpha) = scrim(p, v.dark);
    r.fill_rect_alpha((0.0, 0.0, w, h), scrim_color, alpha);
    // Light: soft shadow. Dark: no shadow; a hairline border lifts the card.
    if !v.dark {
        r.fill_soft_shadow(tuple(l.card), CARD_RADIUS, p.ink, (8.0, 24.0, 0.12));
    }
    r.fill_round_rect(tuple(l.card), CARD_RADIUS, p.surface);
    if v.dark {
        r.stroke_round_rect(tuple(l.card), CARD_RADIUS, p.hairline, 1.0);
    }
    r.draw_wrapped(
        tuple(l.title),
        d.prompt.title(),
        UI_FAMILY,
        TITLE_SIZE,
        p.ink,
    );
    for (rect, detail) in l.details.iter().zip(d.prompt.details()) {
        let run = [ui_run(&detail, p.ink2, DETAIL_SIZE, false)];
        r.draw_runs_ellipsis(tuple(*rect), &run, Align::Leading);
    }
    for (i, (item, rect)) in d.prompt.items().iter().zip(&l.items).enumerate() {
        let focused = i == d.focused;
        match item {
            Item::Checkbox => draw_checkbox(r, *rect, d.dont_ask, focused, v),
            Item::Button(res) => {
                let default = *res == d.prompt.default_result();
                draw_button(r, *rect, res.label(), default, focused, p);
            }
        }
    }
}

fn draw_button(
    r: &mut Renderer,
    rect: Rect,
    label: &str,
    default: bool,
    focused: bool,
    p: &Palette,
) {
    let (fill, color) = if default {
        (p.highlighter, p.on_highlighter)
    } else {
        (p.surface2, p.ink)
    };
    r.fill_round_rect(tuple(rect), BUTTON_RADIUS, fill);
    r.draw_runs(
        tuple(rect),
        &[ui_run(label, color, TEXT_SIZE, default)],
        Align::Center,
    );
    if focused {
        let ring = outset(rect, FOCUS_OUTSET, FOCUS_OUTSET);
        r.stroke_round_rect(tuple(ring), BUTTON_RADIUS + FOCUS_OUTSET, p.ink, 1.5);
    }
}

fn draw_checkbox(r: &mut Renderer, row: Rect, checked: bool, focused: bool, v: &OverlayView) {
    let p = v.palette;
    let mark = (
        row.x,
        row.y + (row.h - CHECK_BOX) / 2.0,
        CHECK_BOX,
        CHECK_BOX,
    );
    if checked {
        r.fill_round_rect(mark, 5.0, p.highlighter);
        r.draw_glyph(
            mark,
            CHECK_GLYPH,
            v.icon_family,
            14.0,
            p.on_highlighter,
            false,
        );
    } else {
        r.stroke_round_rect(mark, 5.0, p.ink2, 1.5);
    }
    let label_x = row.x + CHECK_BOX + 10.0;
    let label = (label_x, row.y, row.x + row.w - label_x, row.h);
    r.draw_runs(
        label,
        &[ui_run(t(S::CheckboxAutoSave), p.ink, TEXT_SIZE, false)],
        Align::Leading,
    );
    if focused {
        let ring = outset(row, 8.0, 0.0);
        r.stroke_round_rect(tuple(ring), 10.0, p.ink, 1.5);
    }
}
