//! Text drawing and measuring with DirectWrite.

use windows::core::{w, Result, HSTRING};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::D2D_POINT_2F;
use windows::Win32::Graphics::Direct2D::{
    ID2D1HwndRenderTarget, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFontCollection, IDWriteTextLayout, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
};

use super::{to_rect_f, Align, Renderer, TextRun};
use crate::theme::Rgb;

impl Renderer {
    /// Draws `text` centered in the whole client area.
    pub fn draw_text_center(&mut self, text: &str, color: Rgb) {
        let Some(d) = &self.device else { return };
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain calls on live COM objects; `wide` and `layout` outlive the call.
        unsafe {
            let size = d.target.GetSize();
            let layout = to_rect_f((0.0, 0.0, size.width, size.height));
            d.brush.SetColor(&color.to_d2d());
            d.target.DrawText(
                &wide,
                &self.text_format,
                &layout,
                &d.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// Semibold `text` wrapped at word boundaries inside `rect`, from its top-left
    /// corner, clipped to the rect.
    pub fn draw_wrapped(
        &mut self,
        rect: (f32, f32, f32, f32),
        text: &str,
        family: &str,
        size: f32,
        color: Rgb,
    ) {
        let Some(d) = &self.device else { return };
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain calls on live COM objects; `wide`, the family string, the
        // rect and the color are live locals that outlive the calls using them.
        let result = unsafe {
            self.dwrite
                .CreateTextFormat(
                    &HSTRING::from(family),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("ko-KR"),
                )
                .and_then(|f| {
                    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                    f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                    d.brush.SetColor(&color.to_d2d());
                    d.target.DrawText(
                        &wide,
                        &f,
                        &to_rect_f(rect),
                        &d.brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                    Ok(())
                })
        };
        if let Err(e) = result {
            eprintln!("light-view: text drawing failed: {e}");
        }
    }

    /// How many lines `draw_wrapped` breaks `text` into at `width` DIPs, or `None` on
    /// failure.
    pub fn wrapped_line_count(
        &self,
        text: &str,
        family: &str,
        size: f32,
        width: f32,
    ) -> Option<u32> {
        if width <= 0.0 {
            return None;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut metrics = DWRITE_TEXT_METRICS::default();
        // SAFETY: plain calls on live COM objects; `wide`, the family string and
        // `metrics` are live locals that outlive the calls using them.
        unsafe {
            let format = self
                .dwrite
                .CreateTextFormat(
                    &HSTRING::from(family),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("ko-KR"),
                )
                .ok()?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).ok()?;
            let layout = self
                .dwrite
                .CreateTextLayout(&wide, &format, width, f32::MAX)
                .ok()?;
            layout.GetMetrics(&mut metrics).ok()?;
        }
        Some(metrics.lineCount)
    }

    /// Whether `family` is installed (system font collection).
    pub fn font_family_exists(&self, family: &str) -> bool {
        let name = HSTRING::from(family);
        let mut collection: Option<IDWriteFontCollection> = None;
        let (mut index, mut exists) = (0u32, BOOL(0));
        // SAFETY: out-pointers are live locals; `name` outlives the call.
        unsafe {
            self.dwrite
                .GetSystemFontCollection(&mut collection, false)
                .is_ok()
                && collection.is_some_and(|c| {
                    c.FindFamilyName(&name, &mut index, &mut exists).is_ok() && exists.as_bool()
                })
        }
    }

    /// Draws `runs` as one unwrapped line in `rect` (x, y, w, h in DIPs), vertically
    /// centered and clipped to the rect.
    pub fn draw_runs(&mut self, rect: (f32, f32, f32, f32), runs: &[TextRun], align: Align) {
        if let Err(e) = self.try_draw_runs(rect, runs, align, false, false) {
            eprintln!("light-view: text drawing failed: {e}");
        }
    }

    /// Like `draw_runs`, but text wider than the rect ends in an ellipsis (…).
    pub fn draw_runs_ellipsis(
        &mut self,
        rect: (f32, f32, f32, f32),
        runs: &[TextRun],
        align: Align,
    ) {
        if let Err(e) = self.try_draw_runs(rect, runs, align, false, true) {
            eprintln!("light-view: text drawing failed: {e}");
        }
    }

    /// Draws one icon-font glyph centered in `rect`, optionally mirrored horizontally.
    pub fn draw_glyph(
        &mut self,
        rect: (f32, f32, f32, f32),
        glyph: char,
        family: &str,
        size: f32,
        color: Rgb,
        mirrored: bool,
    ) {
        let mut buf = [0u8; 4];
        let run = TextRun {
            text: glyph.encode_utf8(&mut buf),
            family,
            size,
            bold: false,
            color,
        };
        if let Err(e) = self.try_draw_runs(rect, &[run], Align::Center, mirrored, false) {
            eprintln!("light-view: glyph drawing failed: {e}");
        }
    }

    fn try_draw_runs(
        &mut self,
        rect: (f32, f32, f32, f32),
        runs: &[TextRun],
        align: Align,
        mirrored: bool,
        ellipsis: bool,
    ) -> Result<()> {
        let (Some(d), Some(first)) = (&self.device, runs.first()) else {
            return Ok(());
        };
        let (x, y, w, h) = rect;
        if w <= 0.0 || h <= 0.0 {
            return Ok(());
        }
        let layout = self.text_layout(runs, align, (w, h), Some(&d.target), ellipsis)?;
        // SAFETY: plain calls on live COM objects; the transform, color and layout
        // are live locals.
        unsafe {
            if mirrored {
                // x' = 2x + w - x: reflects about the rect's vertical center line.
                let m = Matrix3x2 {
                    M11: -1.0,
                    M22: 1.0,
                    M31: 2.0 * x + w,
                    ..Default::default()
                };
                d.target.SetTransform(&m);
            }
            d.brush.SetColor(&first.color.to_d2d());
            d.target.DrawTextLayout(
                D2D_POINT_2F { x, y },
                &layout,
                &d.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
            );
            if mirrored {
                d.target.SetTransform(&Matrix3x2::identity());
            }
        }
        Ok(())
    }

    /// Width in DIPs of `runs` laid out as one unwrapped line, or `None` on failure.
    pub fn measure_runs(&self, runs: &[TextRun]) -> Option<f32> {
        if runs.is_empty() {
            return Some(0.0);
        }
        let layout = self
            .text_layout(runs, Align::Leading, (f32::MAX, f32::MAX), None, false)
            .ok()?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        // SAFETY: `metrics` is a live local the layout writes into.
        unsafe { layout.GetMetrics(&mut metrics) }.ok()?;
        Some(metrics.widthIncludingTrailingWhitespace)
    }

    /// One unwrapped, vertically centered line of `runs` in a `size` box. With a
    /// `target`, each run gets a brush of its color as its drawing effect. With
    /// `ellipsis`, text past the box's width is trimmed to an ellipsis.
    fn text_layout(
        &self,
        runs: &[TextRun],
        align: Align,
        (w, h): (f32, f32),
        target: Option<&ID2D1HwndRenderTarget>,
        ellipsis: bool,
    ) -> Result<IDWriteTextLayout> {
        let first = runs[0];
        let text: String = runs.iter().map(|r| r.text).collect();
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain calls on live COM objects; every string, range and brush passed
        // is a live local that outlives the call using it.
        unsafe {
            let format = self.dwrite.CreateTextFormat(
                &HSTRING::from(first.family),
                None,
                weight(first.bold),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                first.size,
                w!("ko-KR"),
            )?;
            format.SetTextAlignment(text_alignment(align))?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            if ellipsis {
                let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                format.SetTrimming(&trimming, &sign)?;
            }
            let layout = self.dwrite.CreateTextLayout(&wide, &format, w, h)?;
            let mut start = 0u32;
            for run in runs {
                let length = run.text.encode_utf16().count() as u32;
                let range = DWRITE_TEXT_RANGE {
                    startPosition: start,
                    length,
                };
                layout.SetFontFamilyName(&HSTRING::from(run.family), range)?;
                layout.SetFontSize(run.size, range)?;
                layout.SetFontWeight(weight(run.bold), range)?;
                if let Some(t) = target {
                    let brush = t.CreateSolidColorBrush(&run.color.to_d2d(), None)?;
                    layout.SetDrawingEffect(&brush, range)?;
                }
                start += length;
            }
            Ok(layout)
        }
    }
}

fn weight(bold: bool) -> DWRITE_FONT_WEIGHT {
    if bold {
        DWRITE_FONT_WEIGHT_BOLD
    } else {
        DWRITE_FONT_WEIGHT_NORMAL
    }
}

fn text_alignment(align: Align) -> DWRITE_TEXT_ALIGNMENT {
    match align {
        Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
        Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        Align::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
    }
}
