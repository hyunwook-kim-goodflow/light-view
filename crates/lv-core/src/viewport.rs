//! Viewport math: fit, zoom about a point, and clamped panning.
//! Pure computation; all coordinates are in DIPs.

pub const MAX_SCALE: f32 = 32.0;
pub const MIN_SCALE_FLOOR: f32 = 0.05;
pub const ZOOM_STEP: f32 = 1.25;

/// Tolerance for deciding that the current scale equals the fit scale.
const FIT_EPS: f32 = 1e-4;

/// View state: window size, image size, scale and the on-screen position
/// (`offset`) of the image's top-left corner.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewport {
    view: (f32, f32),
    image: (f32, f32),
    scale: f32,
    offset: (f32, f32),
}

fn sane(size: (f32, f32)) -> (f32, f32) {
    let f = |v: f32| if v.is_finite() { v.max(1.0) } else { 1.0 };
    (f(size.0), f(size.1))
}

/// Position of an image axis: centered when it fits, otherwise clamped so no
/// background shows.
fn clamp_axis(offset: f32, view: f32, size: f32) -> f32 {
    if size <= view {
        (view - size) / 2.0
    } else {
        offset.clamp(view - size, 0.0)
    }
}

impl Viewport {
    /// Fit to the window when the image is larger, otherwise show at 1.0.
    /// Centered.
    pub fn initial(view: (f32, f32), image: (f32, f32)) -> Viewport {
        let mut v = Viewport {
            view: sane(view),
            image: sane(image),
            scale: 1.0,
            offset: (0.0, 0.0),
        };
        v.scale = v.fit_scale().min(1.0);
        v.clamp_offset();
        v
    }

    pub fn fit_scale(&self) -> f32 {
        (self.view.0 / self.image.0).min(self.view.1 / self.image.1)
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The smallest and largest scale `zoom_at` allows: down to the fit scale or
    /// `MIN_SCALE_FLOOR`, whichever is smaller, up to `MAX_SCALE` or the fit scale.
    pub fn zoom_bounds(&self) -> (f32, f32) {
        let fit = self.fit_scale();
        (fit.min(MIN_SCALE_FLOOR), MAX_SCALE.max(fit))
    }

    /// Zoom by `factor`; the image point under `anchor` stays put.
    pub fn zoom_at(&mut self, factor: f32, anchor: (f32, f32)) {
        let (min, max) = self.zoom_bounds();
        self.set_scale_at((self.scale * factor).clamp(min, max), anchor);
    }

    /// Fit <-> actual size (1.0, about `anchor`).
    pub fn toggle_fit_actual(&mut self, anchor: (f32, f32)) {
        if self.is_fitted() {
            self.set_scale_at(1.0, anchor);
        } else {
            self.scale = self.fit_scale();
            self.clamp_offset();
        }
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.offset.0 += dx;
        self.offset.1 += dy;
        self.clamp_offset();
    }

    /// Keeps fit when fitted; otherwise keeps the scale and the image point
    /// at the view centre.
    pub fn resize(&mut self, view: (f32, f32)) {
        let was_fitted = self.is_fitted();
        let old = self.view;
        self.view = sane(view);
        if was_fitted {
            self.scale = self.fit_scale();
        } else {
            let cx = (old.0 / 2.0 - self.offset.0) / self.scale;
            let cy = (old.1 / 2.0 - self.offset.1) / self.scale;
            self.offset = (
                self.view.0 / 2.0 - cx * self.scale,
                self.view.1 / 2.0 - cy * self.scale,
            );
        }
        self.clamp_offset();
    }

    /// x, y, w, h of the image on screen.
    pub fn image_rect(&self) -> (f32, f32, f32, f32) {
        (
            self.offset.0,
            self.offset.1,
            self.image.0 * self.scale,
            self.image.1 * self.scale,
        )
    }

    fn is_fitted(&self) -> bool {
        (self.scale - self.fit_scale()).abs() < FIT_EPS
    }

    fn set_scale_at(&mut self, new_scale: f32, anchor: (f32, f32)) {
        let ux = (anchor.0 - self.offset.0) / self.scale;
        let uy = (anchor.1 - self.offset.1) / self.scale;
        self.scale = new_scale;
        self.offset = (anchor.0 - ux * new_scale, anchor.1 - uy * new_scale);
        self.clamp_offset();
    }

    fn clamp_offset(&mut self) {
        self.offset = (
            clamp_axis(self.offset.0, self.view.0, self.image.0 * self.scale),
            clamp_axis(self.offset.1, self.view.1, self.image.1 * self.scale),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-3;

    #[test]
    fn large_image_fits_small_image_is_actual() {
        let v = Viewport::initial((800.0, 600.0), (4032.0, 3024.0));
        assert!((v.scale() - 800.0 / 4032.0).abs() < EPS);
        let v = Viewport::initial((800.0, 600.0), (200.0, 100.0));
        assert_eq!(v.scale(), 1.0);
        assert_eq!(v.image_rect(), (300.0, 250.0, 200.0, 100.0));
    }

    #[test]
    fn zoom_keeps_anchor_point_fixed() {
        let mut v = Viewport::initial((800.0, 600.0), (4000.0, 3000.0));
        let (x0, _, w0, _) = v.image_rect();
        let anchor = (200.0, 150.0);
        let u = (anchor.0 - x0) / w0; // relative position on the image
        v.zoom_at(ZOOM_STEP, anchor);
        let (x1, _, w1, _) = v.image_rect();
        assert!(((anchor.0 - x1) / w1 - u).abs() < EPS);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut v = Viewport::initial((800.0, 600.0), (100.0, 100.0));
        for _ in 0..100 {
            v.zoom_at(ZOOM_STEP, (400.0, 300.0));
        }
        assert_eq!(v.scale(), MAX_SCALE);
        for _ in 0..200 {
            v.zoom_at(1.0 / ZOOM_STEP, (400.0, 300.0));
        }
        assert!((v.scale() - MIN_SCALE_FLOOR).abs() < EPS);
    }

    #[test]
    fn pan_cannot_expose_background_when_larger() {
        let mut v = Viewport::initial((800.0, 600.0), (100.0, 100.0));
        v.zoom_at(16.0, (400.0, 300.0));
        v.pan(10_000.0, 10_000.0);
        let (x, y, _, _) = v.image_rect();
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn double_click_toggles_fit_and_actual() {
        let mut v = Viewport::initial((800.0, 600.0), (4000.0, 3000.0));
        v.toggle_fit_actual((400.0, 300.0));
        assert_eq!(v.scale(), 1.0);
        v.toggle_fit_actual((400.0, 300.0));
        assert!((v.scale() - v.fit_scale()).abs() < EPS);
    }

    #[test]
    fn resize_keeps_fit_when_fitted() {
        let mut v = Viewport::initial((800.0, 600.0), (4000.0, 3000.0));
        v.resize((1600.0, 1200.0));
        assert!((v.scale() - 0.4).abs() < EPS);
    }
}
