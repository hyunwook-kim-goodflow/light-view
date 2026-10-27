//! Immersive toolbar (spec §4.2, §4.3): layout, hit testing, auto-hide (pure logic
//! in DIPs) and drawing through `Renderer`. No Win32 calls.

use lv_core::command::Command;

use crate::render::{Align, Renderer, TextRun};
use crate::theme::{Palette, Rgb};

pub const AUTO_HIDE_MS: u64 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolButton {
    Prev,
    Counter,
    Next,
    ZoomOut,
    ZoomIn,
    RotateLeft,
    RotateRight,
    Save,
    Exif,
    Delete,
    Pin,
}

/// Spec §4.2 order; `Save` appears only with an unsaved rotation.
const ORDER: [ToolButton; 11] = [
    ToolButton::Prev,
    ToolButton::Counter,
    ToolButton::Next,
    ToolButton::ZoomOut,
    ToolButton::ZoomIn,
    ToolButton::RotateLeft,
    ToolButton::RotateRight,
    ToolButton::Save,
    ToolButton::Exif,
    ToolButton::Delete,
    ToolButton::Pin,
];

const BUTTON: f32 = 44.0;
const COUNTER_W: f32 = 76.0;
const SAVE_W: f32 = 72.0;
const GAP: f32 = 4.0;
/// Pill padding around the buttons.
const PAD: f32 = 6.0;
/// Distance from the pill's bottom edge to the view bottom.
const FLOAT_MARGIN: f32 = 12.0;
const PINNED_BAR_H: f32 = 56.0;
const INFO_MIN_VIEW_W: f32 = 560.0;
/// Gap between a side info text and the window edge or the button group.
const INFO_INSET: f32 = 12.0;

impl ToolButton {
    fn width(self) -> f32 {
        match self {
            ToolButton::Counter => COUNTER_W,
            ToolButton::Save => SAVE_W,
            _ => BUTTON,
        }
    }

    /// The viewer command a click sends. `Pin` is handled by the toolbar itself.
    pub fn command(self) -> Option<Command> {
        match self {
            ToolButton::Prev => Some(Command::Prev),
            ToolButton::Next => Some(Command::Next),
            ToolButton::ZoomOut => Some(Command::ZoomOut),
            ToolButton::ZoomIn => Some(Command::ZoomIn),
            ToolButton::RotateLeft => Some(Command::RotateLeft),
            ToolButton::RotateRight => Some(Command::RotateRight),
            ToolButton::Save => Some(Command::Save),
            ToolButton::Exif => Some(Command::ToggleExif),
            ToolButton::Delete => Some(Command::Delete),
            ToolButton::Counter | ToolButton::Pin => None,
        }
    }

    /// Whether the button acts in this build. EXIF and Delete arrive in M3/M4.
    /// Disabled buttons are drawn in ink2.
    pub fn enabled(self) -> bool {
        matches!(
            self,
            ToolButton::Prev
                | ToolButton::Next
                | ToolButton::ZoomOut
                | ToolButton::ZoomIn
                | ToolButton::RotateLeft
                | ToolButton::RotateRight
                | ToolButton::Save
                | ToolButton::Pin
        )
    }

    /// Tooltip text: name and shortcut. The counter has none.
    pub fn tooltip(self) -> Option<&'static str> {
        Some(match self {
            ToolButton::Prev => "이전 사진 (←)",
            ToolButton::Next => "다음 사진 (→)",
            ToolButton::ZoomOut => "축소 (−)",
            ToolButton::ZoomIn => "확대 (+)",
            ToolButton::RotateLeft => "왼쪽으로 회전 (Ctrl+←)",
            ToolButton::RotateRight => "오른쪽으로 회전 (Ctrl+→)",
            ToolButton::Save => "저장 (Ctrl+S)",
            ToolButton::Exif => "정보 (I)",
            ToolButton::Delete => "삭제 (Delete)",
            ToolButton::Pin => "메뉴 고정",
            ToolButton::Counter => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolbarInput {
    /// Client area size in DIPs (the whole window, including a pinned bar).
    pub view_w: f32,
    pub view_h: f32,
    pub pinned: bool,
    pub unsaved_rotation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Button rects in DIPs, left to right with 4 DIP gaps, the group centered
/// horizontally. Floating: the pill's bottom edge is 12 DIP above the view bottom.
/// Pinned: buttons vertically centered in the 56 DIP bar at the bottom.
pub fn layout(input: &ToolbarInput) -> Vec<(ToolButton, Rect)> {
    let buttons: Vec<ToolButton> = ORDER
        .into_iter()
        .filter(|b| *b != ToolButton::Save || input.unsaved_rotation)
        .collect();
    let total: f32 =
        buttons.iter().map(|b| b.width()).sum::<f32>() + GAP * (buttons.len() - 1) as f32;
    let y = if input.pinned {
        input.view_h - (PINNED_BAR_H + BUTTON) / 2.0
    } else {
        input.view_h - FLOAT_MARGIN - PAD - BUTTON
    };
    let mut x = (input.view_w - total) / 2.0;
    buttons
        .into_iter()
        .map(|b| {
            let r = Rect {
                x,
                y,
                w: b.width(),
                h: BUTTON,
            };
            x += r.w + GAP;
            (b, r)
        })
        .collect()
}

/// Height taken from the viewport while the toolbar is pinned.
pub fn pinned_bar_height() -> f32 {
    PINNED_BAR_H
}

/// The toolbar's background: the floating pill, or the full-width pinned bar.
pub fn bar_rect(input: &ToolbarInput) -> Rect {
    if input.pinned {
        return Rect {
            x: 0.0,
            y: input.view_h - PINNED_BAR_H,
            w: input.view_w,
            h: PINNED_BAR_H,
        };
    }
    let l = layout(input);
    let (first, last) = (l[0].1, l[l.len() - 1].1);
    Rect {
        x: first.x - PAD,
        y: first.y - PAD,
        w: last.x + last.w - first.x + 2.0 * PAD,
        h: BUTTON + 2.0 * PAD,
    }
}

pub fn hit_test(layout: &[(ToolButton, Rect)], x: f32, y: f32) -> Option<ToolButton> {
    layout
        .iter()
        .find(|(_, r)| r.contains(x, y))
        .map(|(b, _)| *b)
}

/// The pinned bar's side texts (resolution and size, zoom) need this much width.
pub fn info_visible(view_w: f32) -> bool {
    view_w >= INFO_MIN_VIEW_W
}

/// The pinned bar's side text rects (left of and right of the button group, inset
/// from the window edge and the group), or `None` when they are not shown.
pub fn info_rects(input: &ToolbarInput) -> Option<(Rect, Rect)> {
    if !input.pinned || !info_visible(input.view_w) {
        return None;
    }
    let bar = bar_rect(input);
    let l = layout(input);
    let (first, last) = (l[0].1, l[l.len() - 1].1);
    let left = Rect {
        x: INFO_INSET,
        y: bar.y,
        w: (first.x - 2.0 * INFO_INSET).max(0.0),
        h: bar.h,
    };
    let right_x = last.x + last.w + INFO_INSET;
    let right = Rect {
        x: right_x,
        y: bar.y,
        w: (input.view_w - INFO_INSET - right_x).max(0.0),
        h: bar.h,
    };
    Some((left, right))
}

/// A side text is drawn only when its whole measured width fits its rect; it is
/// never shown clipped.
pub fn text_fits(text_w: f32, side: &Rect) -> bool {
    text_w <= side.w
}

/// Smallest client width (DIPs) at which the widest toolbar (with Save) fits, as a
/// floating pill with a `FLOAT_MARGIN` on each side; the pinned group is narrower.
pub fn min_client_width() -> f32 {
    let widest = ToolbarInput {
        view_w: 0.0,
        view_h: 0.0,
        pinned: false,
        unsaved_rotation: true,
    };
    bar_rect(&widest).w + 2.0 * FLOAT_MARGIN
}

/// Left info text, e.g. "4032×3024 · 2.4 MB".
pub fn info_text(width: u32, height: u32, bytes: u64) -> String {
    format!("{width}×{height} · {}", size_text(bytes))
}

fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// Right info text: the viewport scale as a percentage, e.g. "58%".
pub fn zoom_text(scale: f32) -> String {
    format!("{:.0}%", scale * 100.0)
}

/// Shows the floating toolbar on mouse movement and hides it `AUTO_HIDE_MS`
/// after the last movement, unless the pointer is over it or it is pinned.
#[derive(Clone, Copy, Debug, Default)]
pub struct AutoHide {
    last_move_ms: Option<u64>,
}

impl AutoHide {
    pub fn on_mouse_move(&mut self, now_ms: u64) {
        self.last_move_ms = Some(now_ms);
    }

    pub fn visible(&self, now_ms: u64, hovering_toolbar: bool, pinned: bool) -> bool {
        pinned
            || hovering_toolbar
            || self
                .last_move_ms
                .is_some_and(|t| now_ms.saturating_sub(t) < AUTO_HIDE_MS)
    }
}

const ICON_FAMILIES: [&str; 2] = ["Segoe Fluent Icons", "Segoe MDL2 Assets"];
const UI_FAMILY: &str = "Segoe UI";
const SERIF_FAMILY: &str = "Georgia";
const ICON_SIZE: f32 = 16.0;
const INFO_SIZE: f32 = 12.0;
/// Diameter of the Save fill and the pinned Pin ring inside a 44 DIP button.
const CHIP: f32 = 32.0;

/// Segoe Fluent Icons when installed (Windows 11), else Segoe MDL2 Assets.
pub fn icon_family(r: &Renderer) -> &'static str {
    ICON_FAMILIES
        .into_iter()
        .find(|f| r.font_family_exists(f))
        .unwrap_or(ICON_FAMILIES[1])
}

/// Icon glyph (same code points in both icon fonts) and whether it is mirrored.
/// There is no counter-clockwise rotate glyph, so RotateLeft mirrors Rotate (U+E7AD).
fn glyph(b: ToolButton, pinned: bool) -> Option<(char, bool)> {
    Some(match b {
        ToolButton::Prev => ('\u{E76B}', false),
        ToolButton::Next => ('\u{E76C}', false),
        ToolButton::ZoomOut => ('\u{E71F}', false),
        ToolButton::ZoomIn => ('\u{E8A3}', false),
        ToolButton::RotateLeft => ('\u{E7AD}', true),
        ToolButton::RotateRight => ('\u{E7AD}', false),
        ToolButton::Exif => ('\u{E946}', false),
        ToolButton::Delete => ('\u{E74D}', false),
        ToolButton::Pin if pinned => ('\u{E840}', false),
        ToolButton::Pin => ('\u{E718}', false),
        ToolButton::Counter | ToolButton::Save => return None,
    })
}

/// Everything the toolbar draws besides its layout input.
pub struct ToolbarView<'a> {
    pub input: ToolbarInput,
    pub palette: &'a Palette,
    pub dark: bool,
    pub icon_family: &'a str,
    /// 1-based position in the folder and the folder's photo count.
    pub position: (usize, usize),
    /// Pinned bar side texts: resolution · size (left) and zoom (right).
    pub info: (&'a str, &'a str),
}

fn tuple(r: Rect) -> (f32, f32, f32, f32) {
    (r.x, r.y, r.w, r.h)
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

pub fn draw(r: &mut Renderer, v: &ToolbarView) {
    let p = v.palette;
    let bar = bar_rect(&v.input);
    let buttons = layout(&v.input);
    if v.input.pinned {
        r.fill_round_rect(tuple(bar), 0.0, p.surface);
        r.fill_round_rect((bar.x, bar.y, bar.w, 1.0), 0.0, p.hairline);
        if let Some((left, right)) = info_rects(&v.input) {
            let sides = [
                (left, v.info.0, Align::Leading),
                (right, v.info.1, Align::Trailing),
            ];
            for (rect, text, align) in sides {
                let runs = [ui_run(text, p.ink2, INFO_SIZE, false)];
                if r.measure_runs(&runs).is_some_and(|w| text_fits(w, &rect)) {
                    r.draw_runs(tuple(rect), &runs, align);
                }
            }
        }
    } else {
        let radius = bar.h / 2.0;
        // Light: soft shadow (CSS 0 4px 14px ink @ 8%). Dark: no shadow; the hairline
        // outline (drawn in both modes, as in the mock-up) lifts the surface.
        if !v.dark {
            r.fill_soft_shadow(tuple(bar), radius, p.ink, (4.0, 14.0, 0.08));
        }
        r.fill_round_rect(tuple(bar), radius, p.surface);
        r.stroke_round_rect(tuple(bar), radius, p.hairline, 1.0);
    }
    for (b, rect) in buttons {
        draw_button(r, v, b, rect);
    }
}

fn draw_button(r: &mut Renderer, v: &ToolbarView, b: ToolButton, rect: Rect) {
    let p = v.palette;
    let chip = (
        rect.x + (rect.w - CHIP) / 2.0,
        rect.y + (rect.h - CHIP) / 2.0,
    );
    match b {
        ToolButton::Counter => {
            let pos = v.position.0.to_string();
            let total = format!(" / {}", v.position.1);
            let runs = [
                TextRun {
                    text: &pos,
                    family: SERIF_FAMILY,
                    size: 15.0,
                    bold: false,
                    color: p.ink,
                },
                ui_run(&total, p.ink2, 12.0, false),
            ];
            r.draw_runs(tuple(rect), &runs, Align::Center);
        }
        ToolButton::Save => {
            let fill = (rect.x + 4.0, chip.1, rect.w - 8.0, CHIP);
            r.fill_round_rect(fill, CHIP / 2.0, p.highlighter);
            let color = p.on_highlighter;
            r.draw_runs(
                tuple(rect),
                &[ui_run("저장", color, 13.0, true)],
                Align::Center,
            );
        }
        _ => {
            if b == ToolButton::Pin && v.input.pinned {
                r.stroke_round_rect((chip.0, chip.1, CHIP, CHIP), CHIP / 2.0, p.ink, 1.5);
            }
            if let Some((g, mirrored)) = glyph(b, v.input.pinned) {
                let color = if b.enabled() { p.ink } else { p.ink2 };
                r.draw_glyph(tuple(rect), g, v.icon_family, ICON_SIZE, color, mirrored);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_button_only_when_unsaved() {
        let mut i = ToolbarInput {
            view_w: 1200.0,
            view_h: 800.0,
            pinned: false,
            unsaved_rotation: false,
        };
        assert!(!layout(&i).iter().any(|(b, _)| *b == ToolButton::Save));
        i.unsaved_rotation = true;
        let order: Vec<_> = layout(&i).into_iter().map(|(b, _)| b).collect();
        assert_eq!(
            order,
            [
                ToolButton::Prev,
                ToolButton::Counter,
                ToolButton::Next,
                ToolButton::ZoomOut,
                ToolButton::ZoomIn,
                ToolButton::RotateLeft,
                ToolButton::RotateRight,
                ToolButton::Save,
                ToolButton::Exif,
                ToolButton::Delete,
                ToolButton::Pin
            ]
        );
    }

    #[test]
    fn hit_targets_are_at_least_44() {
        for pinned in [false, true] {
            let i = ToolbarInput {
                view_w: 1200.0,
                view_h: 800.0,
                pinned,
                unsaved_rotation: true,
            };
            for (_, r) in layout(&i) {
                assert!(r.w >= 44.0 && r.h >= 44.0);
            }
        }
    }

    #[test]
    fn floating_bar_is_centered_above_bottom() {
        let l = layout(&ToolbarInput {
            view_w: 1200.0,
            view_h: 800.0,
            pinned: false,
            unsaved_rotation: false,
        });
        let left = l.first().unwrap().1.x;
        let last = &l.last().unwrap().1;
        assert!(((left + last.x + last.w) / 2.0 - 600.0).abs() < 1.0);
        assert!(last.y + last.h <= 800.0 - 12.0);
    }

    #[test]
    fn autohide_after_2s_unless_hovered_or_pinned() {
        let mut a = AutoHide::default();
        a.on_mouse_move(1_000);
        assert!(a.visible(2_999, false, false));
        assert!(!a.visible(3_001, false, false));
        assert!(a.visible(9_999, true, false));
        assert!(a.visible(9_999, false, true));
    }

    #[test]
    fn hit_test_finds_button() {
        for pinned in [false, true] {
            let l = layout(&ToolbarInput {
                view_w: 1200.0,
                view_h: 800.0,
                pinned,
                unsaved_rotation: true,
            });
            for (b, r) in &l {
                assert_eq!(hit_test(&l, r.x + r.w / 2.0, r.y + r.h / 2.0), Some(*b));
            }
            assert_eq!(hit_test(&l, 600.0, 100.0), None);
            assert_eq!(hit_test(&l, 5.0, 795.0), None);
        }
    }

    #[test]
    fn hidden_before_any_mouse_move() {
        assert!(!AutoHide::default().visible(0, false, false));
    }

    #[test]
    fn pinned_bar_is_56_high_with_buttons_centered_in_it() {
        assert_eq!(pinned_bar_height(), 56.0);
        let l = layout(&ToolbarInput {
            view_w: 1000.0,
            view_h: 700.0,
            pinned: true,
            unsaved_rotation: false,
        });
        for (_, r) in &l {
            assert_eq!(r.y + r.h / 2.0, 700.0 - 28.0);
        }
        let left = l.first().unwrap().1.x;
        let last = l.last().unwrap().1;
        assert!(((left + last.x + last.w) / 2.0 - 500.0).abs() < 1.0);
    }

    #[test]
    fn floating_buttons_have_4_dip_gaps_and_pill_padding() {
        let i = ToolbarInput {
            view_w: 1200.0,
            view_h: 800.0,
            pinned: false,
            unsaved_rotation: true,
        };
        let l = layout(&i);
        for pair in l.windows(2) {
            assert_eq!(pair[1].1.x - (pair[0].1.x + pair[0].1.w), 4.0);
        }
        let widths: Vec<_> = l.iter().map(|(b, r)| (*b, r.w)).collect();
        assert!(widths.contains(&(ToolButton::Counter, 76.0)));
        assert!(widths.contains(&(ToolButton::Save, 72.0)));
        let pill = bar_rect(&i);
        assert_eq!(pill.y + pill.h, 800.0 - 12.0);
        assert_eq!(pill.x, l[0].1.x - 6.0);
        assert_eq!(pill.h, 44.0 + 12.0);
    }

    #[test]
    fn pinned_bar_spans_the_window_bottom() {
        let i = ToolbarInput {
            view_w: 900.0,
            view_h: 600.0,
            pinned: true,
            unsaved_rotation: false,
        };
        assert_eq!(
            bar_rect(&i),
            Rect {
                x: 0.0,
                y: 544.0,
                w: 900.0,
                h: 56.0
            }
        );
    }

    #[test]
    fn info_rects_sit_beside_the_group_and_only_when_pinned_and_wide() {
        let mut i = ToolbarInput {
            view_w: 1200.0,
            view_h: 800.0,
            pinned: true,
            unsaved_rotation: false,
        };
        let (left, right) = info_rects(&i).unwrap();
        let l = layout(&i);
        assert_eq!(left.x, 12.0);
        assert_eq!(left.x + left.w, l[0].1.x - 12.0);
        let last = l[l.len() - 1].1;
        assert_eq!(right.x, last.x + last.w + 12.0);
        assert_eq!(right.x + right.w, 1200.0 - 12.0);
        assert_eq!(left.y, 800.0 - 56.0);
        i.view_w = 559.0;
        assert_eq!(info_rects(&i), None);
        i.view_w = 1200.0;
        i.pinned = false;
        assert_eq!(info_rects(&i), None);
    }

    #[test]
    fn side_text_is_drawn_only_when_it_fits_entirely() {
        let i = ToolbarInput {
            view_w: 640.0,
            view_h: 600.0,
            pinned: true,
            unsaved_rotation: false,
        };
        let (left, _) = info_rects(&i).unwrap(); // 640 - 508 = 132; 66 - 24 = 42 wide
        assert!(text_fits(42.0, &left));
        assert!(!text_fits(42.5, &left));
        let wide = info_rects(&ToolbarInput {
            view_w: 1200.0,
            ..i
        })
        .unwrap()
        .0;
        assert!(text_fits(110.0, &wide)); // "4032×3024 · 2.4 MB" at 12 DIP
    }

    #[test]
    fn min_width_fits_the_widest_toolbar_with_margins() {
        let w = min_client_width();
        assert_eq!(w, 584.0 + 2.0 * 6.0 + 2.0 * 12.0);
        for pinned in [false, true] {
            let i = ToolbarInput {
                view_w: w,
                view_h: 600.0,
                pinned,
                unsaved_rotation: true,
            };
            let l = layout(&i);
            assert!(l[0].1.x >= 12.0 + 6.0 - 1e-3);
            let last = l[l.len() - 1].1;
            assert!(last.x + last.w <= w - 12.0 - 6.0 + 1e-3);
        }
        let pill = bar_rect(&ToolbarInput {
            view_w: w,
            view_h: 600.0,
            pinned: false,
            unsaved_rotation: true,
        });
        assert!((pill.x - 12.0).abs() < 1e-3);
    }

    #[test]
    fn info_hidden_below_560() {
        assert!(!info_visible(559.9));
        assert!(info_visible(560.0));
    }

    #[test]
    fn info_texts() {
        assert_eq!(info_text(4032, 3024, 2_516_582), "4032×3024 · 2.4 MB");
        assert_eq!(info_text(10, 20, 512), "10×20 · 512 B");
        assert_eq!(info_text(10, 20, 1536), "10×20 · 1.5 KB");
        assert_eq!(info_text(10, 20, 3 * 1024 * 1024 * 1024), "10×20 · 3.0 GB");
        assert_eq!(zoom_text(0.583), "58%");
        assert_eq!(zoom_text(1.0), "100%");
    }

    #[test]
    fn buttons_map_to_commands() {
        assert_eq!(ToolButton::Next.command(), Some(Command::Next));
        assert_eq!(ToolButton::ZoomOut.command(), Some(Command::ZoomOut));
        assert_eq!(ToolButton::Exif.command(), Some(Command::ToggleExif));
        assert_eq!(ToolButton::Pin.command(), None);
        assert_eq!(ToolButton::Counter.command(), None);
    }
}
