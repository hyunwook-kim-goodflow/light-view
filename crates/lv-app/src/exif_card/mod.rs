//! The EXIF card and its full tag list (spec §4.6; rulings R3, R5, R6): state, text,
//! layout and hit testing (pure, in DIPs) and drawing through `Renderer`. The window
//! routes input here after an open dialog and before everything else (`input::overlay`).
//!
//! The card sits at the top right over the photo and does not block navigation: ←/→
//! still change photos and the card follows. Keys while it is open: Esc or I closes it;
//! Enter opens the full list. In the full list ↑↓, PgUp/PgDn, Home/End and the wheel
//! (over the card) move through the tags, Ctrl+C or Enter copies the selected value,
//! Esc goes back to the card and I closes it.

use std::sync::Arc;

use lv_core::command::{Mods, VK_ESCAPE, VK_KEY_I, VK_RETURN};
use lv_core::exif_info::{ExifData, ExifEntry};

use crate::overlay::toast_layout;
use crate::strings::{Lang, S};
use crate::toolbar::Rect;

mod draw;
mod text;

pub use draw::{draw, CardView};
pub use text::{card_text, local_time, CardInfo, CardText, Line};

const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_UP: u16 = 0x26;
const VK_DOWN: u16 = 0x28;
const VK_KEY_C: u16 = 0x43;

// Layout (DIPs).
const CARD_W: f32 = 320.0;
const MARGIN: f32 = 16.0;
const PAD: f32 = 16.0;
const RADIUS: f32 = 14.0;
/// The heading row; the close (or back) button is a 44 DIP square at its right.
const HEADER_H: f32 = 44.0;
const HEADER_INSET: f32 = 4.0;
const LINE_H: f32 = 22.0;
const EXPOSURE_H: f32 = 30.0;
const LINE_GAP: f32 = 4.0;
const GAP: f32 = 8.0;
const BUTTON_H: f32 = 44.0;
const GROUP_H: f32 = 28.0;
const ROW_H: f32 = 44.0;
/// Space between the card's bottom and the toast area (which sits above a pinned bar).
const BOTTOM_GAP: f32 = 8.0;
const SCROLLBAR_W: f32 = 3.0;
const WHEEL_ROWS: i64 = 3;
/// The name column's share of a row.
const NAME_SHARE: f32 = 0.4;

/// A row of the full list: a group heading or a tag (both index into the entries).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Group(usize),
    Entry(usize),
}

impl Row {
    fn height(self) -> f32 {
        match self {
            Row::Group(_) => GROUP_H,
            Row::Entry(_) => ROW_H,
        }
    }

    fn is_entry(self) -> bool {
        matches!(self, Row::Entry(_))
    }
}

/// The entries in order, with a group heading wherever the group changes.
pub fn rows(entries: &[ExifEntry]) -> Vec<Row> {
    let mut out = Vec::with_capacity(entries.len() + 8);
    for (i, e) in entries.iter().enumerate() {
        if i == 0 || entries[i - 1].group != e.group {
            out.push(Row::Group(i));
        }
        out.push(Row::Entry(i));
    }
    out
}

/// How many rows from `from` fit whole in `h`.
fn fit_count(rows: &[Row], from: usize, h: f32) -> usize {
    let mut used = 0.0;
    let mut n = 0;
    for r in rows.iter().skip(from) {
        used += r.height();
        if used > h + 0.01 {
            break;
        }
        n += 1;
    }
    n
}

/// The largest first row that still fills `h` (0 when everything fits).
fn max_scroll(rows: &[Row], h: f32) -> usize {
    let mut used = 0.0;
    for (i, r) in rows.iter().enumerate().rev() {
        used += r.height();
        if used > h + 0.01 {
            return i + 1;
        }
    }
    0
}

/// The rows drawn from `scroll`, each with its rect inside `list`.
pub fn visible_rows(rows: &[Row], scroll: usize, list: Rect) -> Vec<(usize, Rect)> {
    let n = fit_count(rows, scroll, list.h);
    let mut y = list.y;
    (scroll..scroll + n)
        .map(|i| {
            let r = Rect {
                x: list.x,
                y,
                w: list.w,
                h: rows[i].height(),
            };
            y += r.h;
            (i, r)
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardLayout {
    pub card: Rect,
    /// Close (on the card) or back (in the full list), 44 DIP square.
    pub close: Rect,
    pub heading: Rect,
    /// One rect per `CardText::lines` entry (card only).
    pub lines: Vec<Rect>,
    /// "전체 보기" (card only, with tags to list).
    pub expand: Option<Rect>,
    /// The list's rows area (full list only); shorter than its rows when they scroll.
    pub list: Option<Rect>,
    /// [복사] (full list only).
    pub copy: Option<Rect>,
}

/// Where the card may reach down to: above the toast area, which itself sits above a
/// pinned toolbar.
pub fn card_bottom(view: (f32, f32), pinned: bool) -> f32 {
    toast_layout(view.0, view.1, pinned).y - BOTTOM_GAP
}

/// The card is 320 DIP wide (the window width − 32 when narrower), 16 DIP from the top
/// and right edges. The full list grows down to `bottom` at most and scrolls past it.
pub fn card_layout(
    view_w: f32,
    bottom: f32,
    text: &CardText,
    rows: &[Row],
    expanded: bool,
) -> CardLayout {
    let w = (view_w - 2.0 * MARGIN).clamp(0.0, CARD_W);
    let x = view_w - MARGIN - w;
    let top = MARGIN;
    let close = Rect {
        x: x + w - HEADER_INSET - HEADER_H,
        y: top + HEADER_INSET,
        w: HEADER_H,
        h: HEADER_H,
    };
    let heading = Rect {
        x: x + PAD,
        y: top + HEADER_INSET,
        w: (close.x - x - PAD).max(0.0),
        h: HEADER_H,
    };
    let inner_x = x + PAD;
    let inner_w = (w - 2.0 * PAD).max(0.0);
    let mut y = top + HEADER_INSET + HEADER_H + HEADER_INSET;
    let mut lines = Vec::new();
    let (mut expand, mut list, mut copy) = (None, None, None);
    // Whatever does not fit above `bottom` is dropped (the heading row always stays):
    // in a very short window lines go first, then the button; keys still work.
    let fits = |y: f32, h: f32| y + h + PAD <= bottom + 0.01;
    if expanded {
        let natural: f32 = rows.iter().map(|r| r.height()).sum();
        let room = (bottom - y - GAP - BUTTON_H - PAD).max(0.0);
        let l = Rect {
            x: x + PAD / 2.0,
            y,
            w: (w - PAD).max(0.0),
            h: natural.min(room),
        };
        list = Some(l);
        let copy_y = l.y + l.h + GAP;
        if fits(copy_y, BUTTON_H) {
            copy = Some(Rect {
                x: inner_x,
                y: copy_y,
                w: inner_w,
                h: BUTTON_H,
            });
            y = copy_y + BUTTON_H;
        } else {
            y = l.y + l.h;
        }
    } else {
        let reserve = if text.can_expand { GAP + BUTTON_H } else { 0.0 };
        for line in &text.lines {
            let h = match line {
                Line::Plain(_) => LINE_H,
                Line::Exposure(_) => EXPOSURE_H,
            };
            if !fits(y, h + reserve) {
                break;
            }
            lines.push(Rect {
                x: inner_x,
                y,
                w: inner_w,
                h,
            });
            y += h + LINE_GAP;
        }
        let button_y = if lines.is_empty() {
            y
        } else {
            y - LINE_GAP + GAP
        };
        if text.can_expand && fits(button_y, BUTTON_H) {
            expand = Some(Rect {
                x: inner_x,
                y: button_y,
                w: inner_w,
                h: BUTTON_H,
            });
            y = button_y + BUTTON_H;
        } else if !lines.is_empty() {
            y -= LINE_GAP;
        }
    }
    CardLayout {
        card: Rect {
            x,
            y: top,
            w,
            h: y + PAD - top,
        },
        close,
        heading,
        lines,
        expand,
        list,
        copy,
    }
}

/// Everything the card needs for one event or one draw.
pub struct Model {
    pub exif: Option<Arc<ExifData>>,
    pub text: CardText,
    pub rows: Vec<Row>,
    /// The full list is shown (asked for, and there are tags).
    pub expanded: bool,
    pub layout: CardLayout,
}

impl Model {
    fn entry(&self, row: usize) -> Option<&ExifEntry> {
        match self.rows.get(row)? {
            Row::Entry(i) => self.exif.as_ref()?.entries.get(*i),
            Row::Group(_) => None,
        }
    }

    fn list_h(&self) -> f32 {
        self.layout.list.map_or(0.0, |l| l.h)
    }
}

/// A part of the card that takes clicks (and UI Automation invocations).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Close,
    Expand,
    Copy,
    Row(usize),
    Card,
}

/// What the window does after the card took an input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardAction {
    Redraw,
    /// Put this text on the clipboard and say "복사했어요".
    Copy(String),
}

#[derive(Debug, Default)]
pub struct ExifCard {
    open: bool,
    expanded: bool,
    /// Selected row; `None` (or a stale index) means the first tag.
    selected: Option<usize>,
    /// First list row drawn.
    scroll: usize,
    /// The viewer generation the selection belongs to.
    generation: Option<u64>,
    pressed: Option<Target>,
}

impl ExifCard {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// `I` and the toolbar ⓘ: opens the card (not the list), or closes it.
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open = true;
            self.expanded = false;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.expanded = false;
        self.pressed = None;
    }

    /// Another photo is shown: the list starts again from its top (the card stays open,
    /// and so does the list). `None` (no file open) closes the card.
    pub fn sync(&mut self, generation: Option<u64>) {
        let Some(generation) = generation else {
            self.close();
            return;
        };
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            self.selected = None;
            self.scroll = 0;
            self.pressed = None;
        }
    }

    pub fn model(&self, info: CardInfo, view: (f32, f32), bottom: f32, lang: Lang) -> Model {
        let text = card_text(&info, lang);
        let rows = info
            .exif
            .as_ref()
            .map(|e| rows(&e.entries))
            .unwrap_or_default();
        let expanded = self.expanded && text.can_expand;
        let layout = card_layout(view.0, bottom, &text, &rows, expanded);
        Model {
            exif: info.exif,
            text,
            rows,
            expanded,
            layout,
        }
    }

    /// The selected row, always a tag while there is one.
    pub fn selected(&self, m: &Model) -> Option<usize> {
        self.selected
            .filter(|&i| m.rows.get(i).is_some_and(|r| r.is_entry()))
            .or_else(|| m.rows.iter().position(|r| r.is_entry()))
    }

    /// The first list row drawn, kept in range.
    pub fn scroll(&self, m: &Model) -> usize {
        self.scroll.min(max_scroll(&m.rows, m.list_h()))
    }

    /// The selected tag's value: what Ctrl+C and [복사] copy.
    pub fn copy_text(&self, m: &Model) -> Option<String> {
        m.entry(self.selected(m)?).map(|e| e.value.clone())
    }

    pub fn contains(&self, x: f32, y: f32, m: &Model) -> bool {
        self.open && m.layout.card.contains(x, y)
    }

    /// A key while the card is open; `None` passes it on (←/→ still navigate).
    pub fn on_key(&mut self, vk: u16, mods: Mods, m: &Model) -> Option<CardAction> {
        if !self.open || mods.alt {
            return None;
        }
        if mods.ctrl {
            return (m.expanded && vk == VK_KEY_C).then(|| self.copy_action(m));
        }
        match vk {
            VK_KEY_I => self.close(),
            VK_ESCAPE if m.expanded => self.expanded = false,
            VK_ESCAPE => self.close(),
            VK_RETURN if m.expanded => return Some(self.copy_action(m)),
            VK_RETURN if m.text.can_expand => self.expanded = true,
            VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END if m.expanded => {
                self.move_selection(vk, m);
            }
            _ => return None,
        }
        Some(CardAction::Redraw)
    }

    fn copy_action(&self, m: &Model) -> CardAction {
        self.copy_text(m)
            .map_or(CardAction::Redraw, CardAction::Copy)
    }

    fn move_selection(&mut self, vk: u16, m: &Model) {
        let Some(sel) = self.selected(m) else {
            return;
        };
        let entries: Vec<usize> = (0..m.rows.len())
            .filter(|&i| m.rows[i].is_entry())
            .collect();
        let (first, last) = (entries[0], entries[entries.len() - 1]);
        let page = fit_count(&m.rows, self.scroll(m), m.list_h()).max(1);
        let next_at_or_after = |i: usize| entries.iter().copied().find(|&e| e >= i).unwrap_or(last);
        let prev_at_or_before = |i: usize| {
            entries
                .iter()
                .rev()
                .copied()
                .find(|&e| e <= i)
                .unwrap_or(first)
        };
        let target = match vk {
            VK_UP => prev_at_or_before(sel.saturating_sub(1)),
            VK_DOWN => next_at_or_after(sel + 1),
            VK_PRIOR => next_at_or_after(sel.saturating_sub(page)).min(sel),
            VK_NEXT => prev_at_or_before(sel + page).max(sel),
            VK_HOME => first,
            _ => last,
        };
        self.selected = Some(target);
        self.reveal(target, m);
    }

    /// Scrolls so that `row` is drawn whole (with its group heading when just above).
    fn reveal(&mut self, row: usize, m: &Model) {
        let h = m.list_h();
        let mut scroll = self.scroll(m);
        if row <= scroll {
            scroll = if row > 0 && !m.rows[row - 1].is_entry() {
                row - 1
            } else {
                row
            };
        } else {
            while scroll < row && row >= scroll + fit_count(&m.rows, scroll, h) {
                scroll += 1;
            }
        }
        self.scroll = scroll.min(max_scroll(&m.rows, h));
    }

    /// The wheel at (x, y): over the card it scrolls the list (3 rows a notch, positive
    /// = up) and is taken; elsewhere it is passed on.
    pub fn on_wheel(&mut self, x: f32, y: f32, notches: i32, m: &Model) -> bool {
        if !self.contains(x, y, m) {
            return false;
        }
        if m.expanded {
            let max = max_scroll(&m.rows, m.list_h()) as i64;
            let s = self.scroll(m) as i64 - i64::from(notches) * WHEEL_ROWS;
            self.scroll = s.clamp(0, max) as usize;
        }
        true
    }

    fn target_at(&self, x: f32, y: f32, m: &Model) -> Option<Target> {
        if !self.contains(x, y, m) {
            return None;
        }
        let l = &m.layout;
        let hit = |r: &Option<Rect>| r.is_some_and(|r| r.contains(x, y));
        if l.close.contains(x, y) {
            return Some(Target::Close);
        }
        if hit(&l.expand) {
            return Some(Target::Expand);
        }
        if hit(&l.copy) {
            return Some(Target::Copy);
        }
        if let Some(list) = l.list {
            let row = visible_rows(&m.rows, self.scroll(m), list)
                .into_iter()
                .find(|(i, r)| m.rows[*i].is_entry() && r.contains(x, y));
            if let Some((i, _)) = row {
                return Some(Target::Row(i));
            }
        }
        Some(Target::Card)
    }

    /// Mouse down; true if it is on the card (then nothing under it sees the press).
    pub fn press(&mut self, x: f32, y: f32, m: &Model) -> bool {
        self.pressed = self.target_at(x, y, m);
        self.pressed.is_some()
    }

    /// Mouse up: `None` if the press was not the card's; it acts only when released on
    /// what was pressed.
    pub fn release(&mut self, x: f32, y: f32, m: &Model) -> Option<Option<CardAction>> {
        let pressed = self.pressed.take()?;
        if Some(pressed) != self.target_at(x, y, m) {
            return Some(None);
        }
        Some(self.act(pressed, m))
    }

    /// Does what a click on `target` does; `None` for the card's background (and for a
    /// part the card does not show now).
    pub fn act(&mut self, target: Target, m: &Model) -> Option<CardAction> {
        let l = &m.layout;
        let shown = match target {
            Target::Expand => l.expand.is_some(),
            Target::Copy => l.copy.is_some(),
            Target::Row(i) => m.expanded && m.rows.get(i).is_some_and(|r| r.is_entry()),
            Target::Close | Target::Card => true,
        };
        if !self.open || !shown {
            return None;
        }
        let action = match target {
            Target::Close if m.expanded => {
                self.expanded = false;
                CardAction::Redraw
            }
            Target::Close => {
                self.close();
                CardAction::Redraw
            }
            Target::Expand => {
                self.expanded = true;
                CardAction::Redraw
            }
            Target::Copy => self.copy_action(m),
            Target::Row(i) => {
                self.selected = Some(i);
                CardAction::Redraw
            }
            Target::Card => return None,
        };
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::text::taken_lines;
    use super::*;
    use lv_core::exif_info::ExifSummary;

    const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
    };
    const CTRL: Mods = Mods {
        ctrl: true,
        shift: false,
        alt: false,
    };
    const VIEW: (f32, f32) = (1000.0, 700.0);

    fn entry(group: &str, name: &str, value: &str) -> ExifEntry {
        ExifEntry {
            group: group.into(),
            name: name.into(),
            value: value.into(),
        }
    }

    /// `n` tags: half in IFD0, half in Exif.
    fn data(n: usize) -> Arc<ExifData> {
        let entries = (0..n)
            .map(|i| {
                let group = if i < n / 2 { "IFD0" } else { "Exif" };
                entry(group, &format!("Tag{i}"), &format!("value {i}"))
            })
            .collect();
        Arc::new(ExifData {
            summary: ExifSummary {
                taken: Some("2026:10:03 17:42:10".into()),
                camera: Some("Canon EOS R5".into()),
                exposure: Some("1/250s \u{b7} f/1.8 \u{b7} ISO 64".into()),
                gps: Some((37.5, 127.01)),
            },
            entries,
        })
    }

    fn info(n: usize) -> CardInfo {
        CardInfo {
            exif: Some(data(n)),
            file: Some((4032, 3024, 2_500_000)),
            modified: None,
        }
    }

    fn open_card() -> ExifCard {
        let mut c = ExifCard::default();
        c.toggle();
        c.sync(Some(1));
        c
    }

    fn model(c: &ExifCard, info: CardInfo, view: (f32, f32)) -> Model {
        c.model(info, view, card_bottom(view, false), Lang::Ko)
    }

    fn center(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn click(c: &mut ExifCard, (x, y): (f32, f32), m: &Model) -> Option<Option<CardAction>> {
        assert!(c.press(x, y, m));
        c.release(x, y, m)
    }

    #[test]
    fn taken_is_formatted_per_language() {
        assert_eq!(
            taken_lines("2026:10:03 17:42:10", Lang::Ko),
            ("2026. 10. 3".into(), Some("오후 5:42".into()))
        );
        assert_eq!(
            taken_lines("2026:10:03 17:42:10", Lang::En),
            ("Oct 3, 2026".into(), Some("5:42 PM".into()))
        );
        assert_eq!(
            taken_lines("0000:00:00 00:00:00", Lang::Ko),
            ("0000:00:00 00:00:00".into(), None)
        );
        assert_eq!(
            taken_lines("yesterday", Lang::En),
            ("yesterday".into(), None)
        );
    }

    #[test]
    fn card_text_with_and_without_exif() {
        let text = card_text(&info(4), Lang::Ko);
        assert_eq!(text.heading, "2026. 10. 3");
        assert!(text.serif && text.can_expand);
        assert_eq!(
            text.lines,
            [
                Line::Plain("오후 5:42 \u{b7} Canon EOS R5".into()),
                Line::Exposure("1/250s \u{b7} f/1.8 \u{b7} ISO 64".into()),
                Line::Plain("4032×3024 \u{b7} 2.4 MB".into()),
                Line::Plain("위치 37.50000, 127.01000".into()),
            ]
        );
        let none = CardInfo {
            exif: None,
            file: Some((10, 20, 300)),
            modified: Some((2026, 10, 3, 9, 5)),
        };
        let text = card_text(&none, Lang::Ko);
        assert_eq!(text.heading, "EXIF 정보가 없어요");
        assert!(!text.serif && !text.can_expand);
        assert_eq!(
            text.lines,
            [
                Line::Plain("10×20 \u{b7} 300 B".into()),
                Line::Plain("수정 2026. 10. 3 오전 9:05".into()),
            ]
        );
        // No date: the next line becomes the heading; no GPS: no location line.
        let mut d = (*data(2)).clone();
        d.summary.taken = None;
        d.summary.gps = None;
        let text = card_text(
            &CardInfo {
                exif: Some(Arc::new(d)),
                ..CardInfo::default()
            },
            Lang::En,
        );
        assert_eq!(text.heading, "Canon EOS R5");
        assert_eq!(
            text.lines,
            [Line::Exposure("1/250s \u{b7} f/1.8 \u{b7} ISO 64".into())]
        );
    }

    #[test]
    fn layout_top_right_and_narrow_window() {
        let c = open_card();
        let m = model(&c, info(4), VIEW);
        let l = &m.layout;
        assert_eq!(
            (l.card.x, l.card.y, l.card.w),
            (1000.0 - 16.0 - 320.0, 16.0, 320.0)
        );
        // A narrow window: the card is the window width − 32.
        let m = model(&c, info(4), (300.0, 700.0));
        assert_eq!((m.layout.card.x, m.layout.card.w), (16.0, 268.0));
        // Long values stay inside the card (drawn with an ellipsis, never wider).
        for r in m.layout.lines.iter().chain(&m.layout.expand) {
            assert!(r.x >= m.layout.card.x && r.x + r.w <= m.layout.card.x + m.layout.card.w);
        }
        assert!(m.layout.heading.x + m.layout.heading.w <= m.layout.close.x);
    }

    #[test]
    fn hit_areas_are_44() {
        let mut c = open_card();
        let m = model(&c, info(4), VIEW);
        let l = &m.layout;
        assert!(l.close.w >= 44.0 && l.close.h >= 44.0);
        let e = l.expand.unwrap();
        assert!(e.h >= 44.0);
        assert_eq!(click(&mut c, center(e), &m), Some(Some(CardAction::Redraw)));
        let m = model(&c, info(4), VIEW);
        assert!(m.expanded);
        assert!(m.layout.copy.unwrap().h >= 44.0);
        let list = m.layout.list.unwrap();
        for (i, r) in visible_rows(&m.rows, 0, list) {
            if m.rows[i].is_entry() {
                assert!(r.h >= 44.0);
            }
        }
    }

    #[test]
    fn full_list_stays_above_the_toast_and_pinned_bar() {
        let mut c = open_card();
        c.expanded = true;
        for pinned in [false, true] {
            let view = (800.0, 400.0);
            let bottom = card_bottom(view, pinned);
            let m = c.model(info(200), view, bottom, Lang::Ko);
            assert!(m.layout.card.y + m.layout.card.h <= bottom + 0.01);
            assert!(bottom <= toast_layout(view.0, view.1, pinned).y);
            // Shrunk: it scrolls.
            assert!(max_scroll(&m.rows, m.layout.list.unwrap().h) > 0);
        }
        // A few tags in a tall window: no scrolling, no wasted space.
        let m = model(&c, info(2), VIEW);
        let list = m.layout.list.unwrap();
        assert_eq!(list.h, 2.0 * GROUP_H + 2.0 * ROW_H);
        assert_eq!(max_scroll(&m.rows, list.h), 0);
    }

    #[test]
    fn short_window_drops_lines_rather_than_overflow() {
        let mut c = open_card();
        for h in [700.0, 300.0, 240.0, 200.0, 160.0, 120.0] {
            for expanded in [false, true] {
                c.expanded = expanded;
                let view = (800.0, h);
                let bottom = card_bottom(view, true);
                let m = c.model(info(40), view, bottom, Lang::Ko);
                let l = &m.layout;
                let end = l.card.y + l.card.h;
                // Only the heading row is kept whatever the height.
                let header_end = l.close.y + l.close.h + HEADER_INSET + PAD;
                assert!(
                    end <= bottom.max(header_end) + 0.01,
                    "{h} {expanded}: {end} > {bottom}"
                );
                for r in l
                    .lines
                    .iter()
                    .chain(&l.expand)
                    .chain(&l.copy)
                    .chain(&l.list)
                {
                    assert!(r.y + r.h <= l.card.y + l.card.h, "{h} {expanded}");
                }
            }
        }
        // Tall enough: nothing is dropped; short: lines go before the button does.
        c.expanded = false;
        let m = c.model(info(4), VIEW, card_bottom(VIEW, false), Lang::Ko);
        assert_eq!(m.layout.lines.len(), m.text.lines.len());
        let view = (800.0, 250.0);
        let m = c.model(info(4), view, card_bottom(view, false), Lang::Ko);
        assert!(m.layout.lines.len() < m.text.lines.len());
        assert!(m.layout.expand.is_some());
    }

    #[test]
    fn sync_without_a_file_closes_the_card() {
        let mut c = open_card();
        c.expanded = true;
        c.sync(None);
        assert!(!c.is_open());
        c.toggle();
        assert!(c.is_open());
        assert!(!model(&c, info(4), VIEW).expanded);
    }

    #[test]
    fn keys_open_close_and_pass_navigation() {
        let mut c = ExifCard::default();
        let m = model(&c, info(4), VIEW);
        // Closed: nothing is taken.
        assert_eq!(c.on_key(VK_ESCAPE, NONE, &m), None);
        c.toggle();
        assert!(c.is_open());
        // ←/→ (and other keys) pass through to navigation.
        assert_eq!(c.on_key(0x25, NONE, &m), None);
        assert_eq!(c.on_key(0x27, NONE, &m), None);
        assert_eq!(c.on_key(VK_DOWN, NONE, &m), None);
        // Enter opens the list; Esc goes back to the card; Esc again closes it.
        assert_eq!(c.on_key(VK_RETURN, NONE, &m), Some(CardAction::Redraw));
        let m = model(&c, info(4), VIEW);
        assert!(m.expanded);
        assert_eq!(c.on_key(VK_ESCAPE, NONE, &m), Some(CardAction::Redraw));
        let m = model(&c, info(4), VIEW);
        assert!(!m.expanded && c.is_open());
        assert_eq!(c.on_key(VK_ESCAPE, NONE, &m), Some(CardAction::Redraw));
        assert!(!c.is_open());
        // I closes it from the list too, and opens on the card again.
        c.toggle();
        c.on_key(VK_RETURN, NONE, &m);
        let m = model(&c, info(4), VIEW);
        assert_eq!(c.on_key(VK_KEY_I, NONE, &m), Some(CardAction::Redraw));
        assert!(!c.is_open());
        c.toggle();
        assert!(!model(&c, info(4), VIEW).expanded);
        // Alt combinations are never taken (Alt+F4).
        assert_eq!(c.on_key(VK_ESCAPE, Mods { alt: true, ..NONE }, &m), None);
        // Without EXIF, Enter does not open an empty list.
        let none = model(&c, CardInfo::default(), VIEW);
        assert_eq!(c.on_key(VK_RETURN, NONE, &none), None);
    }

    #[test]
    fn list_keys_move_the_selection_and_scroll() {
        let mut c = open_card();
        c.expanded = true;
        let view = (800.0, 400.0);
        let m = model(&c, info(40), view);
        // Rows: Group(IFD0) 0, entries 1..=20, Group(Exif) 21, entries 22..=41.
        assert_eq!(m.rows.len(), 42);
        assert_eq!(c.selected(&m), Some(1));
        c.on_key(VK_UP, NONE, &m);
        assert_eq!((c.selected(&m), c.scroll(&m)), (Some(1), 0));
        for _ in 0..19 {
            c.on_key(VK_DOWN, NONE, &m);
        }
        assert_eq!(c.selected(&m), Some(20));
        // Down skips the group heading.
        c.on_key(VK_DOWN, NONE, &m);
        assert_eq!(c.selected(&m), Some(22));
        let list = m.layout.list.unwrap();
        let shown: Vec<usize> = visible_rows(&m.rows, c.scroll(&m), list)
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert!(shown.contains(&22), "{shown:?}");
        c.on_key(VK_END, NONE, &m);
        assert_eq!(c.selected(&m), Some(41));
        assert_eq!(c.scroll(&m), max_scroll(&m.rows, list.h));
        c.on_key(VK_NEXT, NONE, &m);
        assert_eq!(c.selected(&m), Some(41));
        c.on_key(VK_HOME, NONE, &m);
        assert_eq!((c.selected(&m), c.scroll(&m)), (Some(1), 0));
        c.on_key(VK_NEXT, NONE, &m);
        let page = fit_count(&m.rows, 0, list.h);
        assert!(
            c.selected(&m).unwrap() >= page.min(20),
            "{:?}",
            c.selected(&m)
        );
        c.on_key(VK_PRIOR, NONE, &m);
        assert_eq!(c.selected(&m), Some(1));
        // Upward reveal shows the group heading above the first tag.
        c.on_key(VK_END, NONE, &m);
        for _ in 0..19 {
            c.on_key(VK_UP, NONE, &m);
        }
        assert_eq!(c.selected(&m), Some(22));
        assert_eq!(c.scroll(&m), 21);
    }

    #[test]
    fn scroll_bounds_and_wheel() {
        let mut c = open_card();
        c.expanded = true;
        let view = (800.0, 400.0);
        let m = model(&c, info(40), view);
        let list = m.layout.list.unwrap();
        let max = max_scroll(&m.rows, list.h);
        assert!(max > 0);
        let over = center(m.layout.card);
        // Wheel up at the top stays at 0; down moves 3 rows a notch, to the end at most.
        assert!(c.on_wheel(over.0, over.1, 1, &m));
        assert_eq!(c.scroll(&m), 0);
        assert!(c.on_wheel(over.0, over.1, -1, &m));
        assert_eq!(c.scroll(&m), 3);
        assert!(c.on_wheel(over.0, over.1, -100, &m));
        assert_eq!(c.scroll(&m), max);
        // At the end the last row is drawn whole and nothing below is left blank.
        let last = visible_rows(&m.rows, max, list);
        assert_eq!(last.last().unwrap().0, m.rows.len() - 1);
        assert!(fit_count(&m.rows, max - 1, list.h) < m.rows.len() - (max - 1));
        // Away from the card the wheel is not taken (it navigates).
        assert!(!c.on_wheel(10.0, 10.0, -1, &m));
        // Over the closed-up card it is taken but does nothing.
        c.expanded = false;
        let m = model(&c, info(40), view);
        let over = center(m.layout.card);
        assert!(c.on_wheel(over.0, over.1, -1, &m));
        // A new photo starts the list from the top again.
        c.expanded = true;
        c.sync(Some(2));
        let m = model(&c, info(40), view);
        assert_eq!((c.scroll(&m), c.selected(&m)), (0, Some(1)));
    }

    #[test]
    fn copy_takes_the_selected_value() {
        let mut c = open_card();
        let m = model(&c, info(4), VIEW);
        // Not in the full list: Ctrl+C is not the card's.
        assert_eq!(c.on_key(VK_KEY_C, CTRL, &m), None);
        c.expanded = true;
        let m = model(&c, info(4), VIEW);
        assert_eq!(
            c.on_key(VK_KEY_C, CTRL, &m),
            Some(CardAction::Copy("value 0".into()))
        );
        c.on_key(VK_DOWN, NONE, &m);
        assert_eq!(c.copy_text(&m).as_deref(), Some("value 1"));
        assert_eq!(
            c.on_key(VK_RETURN, NONE, &m),
            Some(CardAction::Copy("value 1".into()))
        );
        // Clicking a row selects it; [복사] copies its value (not its name).
        let list = m.layout.list.unwrap();
        let (row, rect) = visible_rows(&m.rows, c.scroll(&m), list)
            .into_iter()
            .find(|(i, _)| m.rows[*i] == Row::Entry(3))
            .unwrap();
        assert_eq!(
            click(&mut c, center(rect), &m),
            Some(Some(CardAction::Redraw))
        );
        assert_eq!(c.selected(&m), Some(row));
        assert_eq!(
            click(&mut c, center(m.layout.copy.unwrap()), &m),
            Some(Some(CardAction::Copy("value 3".into())))
        );
    }

    #[test]
    fn parts_act_like_clicks() {
        let mut c = open_card();
        let m = model(&c, info(4), VIEW);
        // Only the parts shown act: no list rows or [복사] on the card.
        assert_eq!(c.act(Target::Copy, &m), None);
        assert_eq!(c.act(Target::Row(1), &m), None);
        assert_eq!(c.act(Target::Card, &m), None);
        assert_eq!(c.act(Target::Expand, &m), Some(CardAction::Redraw));
        let m = model(&c, info(4), VIEW);
        assert!(m.expanded);
        assert_eq!(c.act(Target::Expand, &m), None);
        // A group heading is not a row to select.
        assert_eq!(c.act(Target::Row(0), &m), None);
        assert_eq!(c.act(Target::Row(2), &m), Some(CardAction::Redraw));
        assert_eq!(c.selected(&m), Some(2));
        assert_eq!(
            c.act(Target::Copy, &m),
            Some(CardAction::Copy("value 1".into()))
        );
        // Back, then close.
        c.act(Target::Close, &m);
        let m = model(&c, info(4), VIEW);
        assert!(!m.expanded && c.is_open());
        c.act(Target::Close, &m);
        assert!(!c.is_open());
        assert_eq!(c.act(Target::Close, &m), None, "closed");
    }

    #[test]
    fn clicks_on_and_off_the_card() {
        let mut c = open_card();
        let m = model(&c, info(4), VIEW);
        // Off the card: not taken.
        assert!(!c.press(10.0, 10.0, &m));
        assert_eq!(c.release(10.0, 10.0, &m), None);
        // On the card but not on a button: taken, does nothing.
        let body = (
            m.layout.card.x + 4.0,
            m.layout.card.y + m.layout.card.h - 4.0,
        );
        assert_eq!(click(&mut c, body, &m), Some(None));
        // Released elsewhere: nothing happens.
        assert!(c.press(center(m.layout.close).0, center(m.layout.close).1, &m));
        assert_eq!(c.release(10.0, 10.0, &m), Some(None));
        assert!(c.is_open());
        // The back button leaves the list; the close button closes the card.
        c.expanded = true;
        let m = model(&c, info(4), VIEW);
        click(&mut c, center(m.layout.close), &m);
        let m = model(&c, info(4), VIEW);
        assert!(!m.expanded && c.is_open());
        click(&mut c, center(m.layout.close), &m);
        assert!(!c.is_open());
        assert!(!c.press(center(m.layout.card).0, center(m.layout.card).1, &m));
    }
}
