//! Painting the main window: the image upload, the frame (photo, start screen, card,
//! toolbar, overlay) and the loading pill.

use windows::Win32::Foundation::HWND;

use crate::commands;
use crate::exif_card::{self, CardView};
use crate::menu::{self, MenuView};
use crate::overlay::{self, OverlayView};
use crate::render::{Align, Renderer, TextRun};
use crate::start;
use crate::strings::{t, S};
use crate::theme::{palette, Palette};
use crate::toolbar::{self, ToolbarView};
use crate::uia;
use crate::uia_provider;
use crate::viewer::Scene;

use super::App;

const LOADING_SIZE_DIP: f32 = 13.0;
const PILL_HEIGHT_DIP: f32 = 28.0;
const PILL_PAD_DIP: f32 = 14.0;

impl App {
    /// Uploads the shown image (or animation frame) if it changed since the last paint,
    /// or again after the renderer lost its device (it keeps no pixel copy), and
    /// (re)starts the animation timer for a newly shown photo. Done at paint time
    /// so that a burst of key repeats uploads only the photo that ends up on screen.
    fn upload_image(&mut self, hwnd: HWND) {
        let key = (
            self.viewer.generation(),
            self.viewer.frame_to_upload().map_or(0, |f| f.1),
        );
        let lost = self.renderer.as_ref().is_some_and(Renderer::needs_upload);
        if self.uploaded == Some(key) && !lost {
            return;
        }
        let new_photo = self.uploaded.map(|k| k.0) != Some(key.0);
        self.uploaded = Some(key);
        let Some(r) = &mut self.renderer else { return };
        match self.viewer.frame_to_upload() {
            Some((img, frame)) => {
                crate::timing::step("upload-ms", || r.set_image(Some(img), frame));
                if !r.has_image() {
                    // Larger than the GPU's maximum bitmap size; release the pixel copy.
                    r.set_image(None, 0);
                    self.viewer.too_large_to_draw();
                }
            }
            None => r.set_image(None, 0),
        }
        if new_photo {
            commands::set_animation_timer(hwnd, self.viewer.frame_delay());
        }
    }

    pub(super) fn paint(&mut self, hwnd: HWND) {
        self.upload_image(hwnd);
        let toolbar = self.toolbar_input().filter(|_| self.pointer.shown);
        let info = self
            .viewer
            .info()
            .map(|(w, h, bytes)| toolbar::info_text(w, h, bytes))
            .unwrap_or_default();
        let zoom = self
            .viewer
            .zoom()
            .map(toolbar::zoom_text)
            .unwrap_or_default();
        let (view_w, view_h) = self.view_dips();
        let now_ms = self.pointer.now_ms();
        let pinned_bar = self.toolbar_input().is_some_and(|i| i.pinned);
        let card = self.card_model();
        let start = self.start_layout();
        let menu = self.menu.layout(self.client_dips);
        let Some(r) = &mut self.renderer else { return };
        let p = palette(self.dark);
        r.begin(p.paper);
        let saving = self.saving.as_ref().is_some_and(|s| s.overdue);
        let loading = self.viewer.loading() && !saving;
        let mut drew_image = false;
        match self.viewer.scene() {
            // The previous photo stays up while the next one loads.
            Scene::Image { rect, quarters } => {
                r.draw_image(rect, quarters);
                drew_image = r.has_image();
            }
            Scene::Message(text) if !loading => r.draw_text_center(&text, p.ink2),
            Scene::Message(_) | Scene::Empty => {}
        }
        if let Some(l) = &start {
            start::draw(r, l, p);
        }
        if saving {
            draw_pill(r, p, self.dark, (view_w, view_h), t(S::Saving));
        } else if loading {
            draw_pill(r, p, self.dark, (view_w, view_h), t(S::Loading));
        }
        if let Some(input) = toolbar {
            let view = ToolbarView {
                input,
                palette: p,
                dark: self.dark,
                icon_family: self.icon_family,
                position: self.viewer.position(),
                info: (&info, &zoom),
            };
            toolbar::draw(r, &view);
        }
        if let Some(m) = &card {
            let view = CardView {
                palette: p,
                dark: self.dark,
                icon_family: self.icon_family,
            };
            exif_card::draw(r, &self.exif_card, m, &view);
        }
        if let Some(l) = &menu {
            let view = MenuView {
                palette: p,
                dark: self.dark,
            };
            menu::draw(r, &self.menu, l, &view);
        }
        let view = OverlayView {
            client: self.client_dips,
            pinned: pinned_bar,
            palette: p,
            dark: self.dark,
            icon_family: self.icon_family,
            now_ms,
        };
        overlay::draw(r, &mut self.overlay, &view);
        let ended = if drew_image {
            crate::timing::step("present-ms", || r.end())
        } else {
            r.end()
        };
        match ended {
            Ok(()) if drew_image => crate::timing::first_image_presented(),
            Ok(()) => {}
            Err(e) => eprintln!("light-view: EndDraw failed: {e}"),
        }
        // What was just drawn is what a screen reader sees.
        uia_provider::publish(hwnd, || uia::main_tree_of(self));
    }
}
/// A small "불러오는 중…" or "저장하는 중…" pill centred in the image area: surface
/// fill, plus a hairline border in dark mode (no shadows there).
fn draw_pill(r: &mut Renderer, p: &Palette, dark: bool, (view_w, view_h): (f32, f32), text: &str) {
    let run = TextRun {
        text,
        family: "Segoe UI",
        size: LOADING_SIZE_DIP,
        bold: false,
        color: p.ink2,
    };
    let text_w = r.measure_runs(&[run]).unwrap_or(80.0);
    let (w, h) = (text_w + 2.0 * PILL_PAD_DIP, PILL_HEIGHT_DIP);
    let rect = ((view_w - w) / 2.0, (view_h - h) / 2.0, w, h);
    r.fill_round_rect(rect, h / 2.0, p.surface);
    if dark {
        r.stroke_round_rect(rect, h / 2.0, p.hairline, 1.0);
    }
    r.draw_runs(rect, &[run], Align::Center);
}
