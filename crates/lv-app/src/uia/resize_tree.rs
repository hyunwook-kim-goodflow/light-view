//! The resize screen's elements (M6 Task 6, M4's accessibility kept): the photo, its
//! arrows, the filmstrip and the caption; the sidebar's tabs and choices as radio
//! groups, the number fields' labels (each field is a native EDIT with its own name),
//! the checkbox, the estimate list, the total and the buttons. The keyboard focus is
//! the focused control's element, as in the settings window.

use lv_core::settings::ResizeFormat;

use crate::resize::draw::{save_count, Geometry};
use crate::resize::session::{ReplaceBlock, ResizeSession};
use crate::resize::sidebar::{controls, tab_fields, Ctl, Hit};
use crate::resize::text;
use crate::strings::{text as tr, Lang, S};

use super::{Act, Builder, Role};

// Element ids (the main window's other parts use lower ranges, the card's list rows
// 10 000 and up).
const STAGE_PHOTO: u32 = 3000;
const PREV: u32 = 3001;
const NEXT: u32 = 3002;
const STRIP: u32 = 3003;
const CAPTION: u32 = 3004;
const SIDEBAR: u32 = 3005;
/// Each choice's group, its options after it.
const CHOICE_FIRST: u32 = 3010;
const CHOICE_STRIDE: u32 = 10;
const FIELD_LABEL_FIRST: u32 = 3070;
const GPS: u32 = 3080;
const REASON: u32 = 3081;
const LIST: u32 = 3082;
const TOTAL: u32 = 3083;
const CANCEL: u32 = 3084;
const SAVE: u32 = 3085;
const PROGRESS: u32 = 3086;
const STRIP_CELL_FIRST: u32 = 20_000;
const LIST_ITEM_FIRST: u32 = 40_000;

/// The resize screen's state, as the tree needs it.
pub struct ResizeInput<'a> {
    pub session: &'a ResizeSession,
    pub geometry: Geometry,
    /// The focused sidebar control while the window has the keyboard focus.
    pub focus: Option<Ctl>,
    pub block: Option<ReplaceBlock>,
    /// (done, total) while a save runs.
    pub progress: Option<(usize, usize)>,
    pub lang: Lang,
}

const CHOICES: [Ctl; 5] = [
    Ctl::Tabs,
    Ctl::Axis,
    Ctl::Format,
    Ctl::Quality,
    Ctl::SaveMode,
];

pub(super) fn add_resize(b: &mut Builder, i: &ResizeInput) {
    let s = i.session;
    let g = &i.geometry;
    let lang = i.lang;
    let m = &s.memory;
    let saving = i.progress.is_some();
    let current = s.items.get(s.current);
    let name = current.map_or(String::new(), |c| {
        format!(
            "{} · {} / {}",
            text::file_name(&c.path),
            s.current + 1,
            s.items.len()
        )
    });
    b.add(None, STAGE_PHOTO, Role::Image, &name, g.parts.photo);
    for (id, rect, key, d) in [
        (PREV, g.parts.prev, S::TipPrev, -1),
        (NEXT, g.parts.next, S::TipNext, 1),
    ] {
        if let Some(r) = rect {
            b.add(None, id, Role::Button, tr(lang, key), r);
            b.last().act = Some(Act::Resize(Hit::Nav(d)));
        }
    }
    if let (Some(first), Some(last)) = (g.parts.strip.first(), g.parts.strip.last()) {
        let area = crate::toolbar::Rect {
            w: last.1.x + last.1.w - first.1.x,
            ..first.1
        };
        let strip = b.add(None, STRIP, Role::List, tr(lang, S::ResizePhotoList), area);
        for (k, r) in &g.parts.strip {
            let Some(item) = s.items.get(*k) else {
                continue;
            };
            let id = STRIP_CELL_FIRST + *k as u32;
            b.add(
                Some(strip),
                id,
                Role::ListItem,
                &text::file_name(&item.path),
                *r,
            );
            let node = b.last();
            node.selected = Some(*k == s.current);
            node.act = Some(Act::Resize(Hit::Strip(*k)));
        }
    }
    if let Some(c) = current {
        let (src, result) = text::caption(lang, c);
        b.add(
            None,
            CAPTION,
            Role::Text,
            &format!("{src}{result}"),
            g.parts.caption,
        );
    }
    let l = &g.layout;
    let panel = b.add(None, SIDEBAR, Role::Group, tr(lang, S::Resize), l.panel);
    let order = controls(m);
    for (k, c) in CHOICES.iter().enumerate() {
        let png_quality = *c == Ctl::Quality && m.format == ResizeFormat::Png;
        if !(order.contains(c) || png_quality) {
            continue;
        }
        let id = CHOICE_FIRST + k as u32 * CHOICE_STRIDE;
        let rects = l.options(*c, m);
        let Some(rect) = l.focus_rect(*c, m) else {
            continue;
        };
        let label = c.label().map_or("", |key| tr(lang, key));
        let group = b.add(Some(panel), id, Role::Group, label, rect);
        // 품질 has nothing to choose for PNG.
        let png = *c == Ctl::Quality && m.format == ResizeFormat::Png;
        b.last().enabled = !png;
        let chosen = if png { None } else { c.selected(m, s.replace) };
        let has_focus = i.focus == Some(*c);
        b.last().focused = has_focus && chosen.is_none() && !png;
        let options = c.option_labels(lang);
        for (o, (name, r)) in options.iter().zip(&rects).enumerate() {
            b.add(Some(group), id + 1 + o as u32, Role::RadioButton, name, *r);
            let node = b.last();
            let blocked = *c == Ctl::SaveMode && o == 1 && i.block.is_some();
            node.enabled = !png && !blocked && !saving;
            node.focusable = node.enabled;
            node.focused = has_focus && chosen == Some(o);
            node.selected = Some(chosen == Some(o));
            node.act = Some(Act::Resize(Hit::Option(*c, o)));
        }
    }
    for (k, f) in tab_fields(m.tab).iter().enumerate() {
        if let Some(row) = l.row_of(Ctl::Field(*f), m) {
            let id = FIELD_LABEL_FIRST + k as u32;
            b.add(Some(panel), id, Role::Text, tr(lang, f.label(m)), row.label);
        }
    }
    if let Some(block) = i.block {
        b.add(
            Some(panel),
            REASON,
            Role::Text,
            text::block_reason(lang, block),
            l.reason,
        );
    }
    b.add(
        Some(panel),
        GPS,
        Role::CheckBox,
        tr(lang, S::ResizeStripGps),
        l.metadata.control,
    );
    let node = b.last();
    node.toggle = Some(m.strip_gps);
    node.enabled = !saving;
    node.focusable = !saving;
    node.focused = i.focus == Some(Ctl::StripGps);
    node.act = Some(Act::Resize(Hit::Control(Ctl::StripGps)));
    let list = b.add(
        Some(panel),
        LIST,
        Role::List,
        tr(lang, S::ResizeEstimate),
        l.list,
    );
    for (k, r) in &g.rows {
        let Some(item) = s.items.get(*k) else {
            continue;
        };
        let (src, result) = text::caption(lang, item);
        let mut name = format!("{src}{result}");
        if let Some(note) = text::note(lang, item) {
            name = format!("{name} · {note}");
        }
        b.add(
            Some(list),
            LIST_ITEM_FIRST + *k as u32,
            Role::ListItem,
            &name,
            *r,
        );
        let node = b.last();
        let on = *k == s.current;
        node.focusable = true;
        node.focused = on && i.focus == Some(Ctl::List);
        node.selected = Some(on);
        node.act = Some(Act::Resize(Hit::Row(*k)));
    }
    if let Some(total) = text::total_line(lang, &s.items) {
        b.add(Some(panel), TOTAL, Role::Text, &total, l.total);
    }
    match i.progress {
        Some((done, total)) => {
            let line = text::progress(lang, done, total);
            // The row drawn over both buttons.
            let row = crate::toolbar::Rect {
                w: l.save.x + l.save.w - l.cancel.x,
                ..l.cancel
            };
            b.add(Some(panel), PROGRESS, Role::Text, &line, row);
        }
        None => {
            let save = text::save_button(lang, save_count(s));
            for (id, ctl, name, r) in [
                (
                    CANCEL,
                    Ctl::Cancel,
                    tr(lang, S::BtnCancel).to_owned(),
                    l.cancel,
                ),
                (SAVE, Ctl::Save, save, l.save),
            ] {
                b.add(Some(panel), id, Role::Button, &name, r);
                let node = b.last();
                node.focusable = true;
                node.focused = i.focus == Some(ctl);
                node.act = Some(Act::Resize(Hit::Control(ctl)));
            }
        }
    }
    // In a short window the middle scrolls: what is out of its view is not drawn.
    let vp = super::PxRect::from_dips(l.viewport, b.dip);
    let choices = CHOICE_FIRST + 2 * CHOICE_STRIDE..CHOICE_FIRST + 5 * CHOICE_STRIDE;
    for n in &mut b.nodes {
        let middle = choices.contains(&n.id)
            || matches!(n.id, REASON | GPS | LIST)
            || n.id >= LIST_ITEM_FIRST;
        if middle && (n.rect.y + n.rect.h <= vp.y || n.rect.y >= vp.y + vp.h) {
            n.offscreen = true;
        }
    }
}

#[cfg(test)]
#[path = "resize_tree_tests.rs"]
mod tests;
