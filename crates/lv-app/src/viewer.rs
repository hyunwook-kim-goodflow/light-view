//! Viewer state: folder position, prefetching, the displayed photo, zoom and view
//! rotation. No Win32 here; the window feeds it commands and sizes and draws what it
//! reports.

use std::path::{Path, PathBuf};

use lv_core::command::Command;
use lv_core::decode::{DecodeError, DecodedImage};
use lv_core::listing::FolderList;
use lv_core::prefetch::{DecodeResult, Prefetcher};
use lv_core::viewport::{Viewport, ZOOM_STEP};

pub const CANNOT_OPEN: &str = "이 사진은 열 수 없어요";
/// Frame delays below this are played at `DEFAULT_DELAY_MS`, as browsers do.
const MIN_DELAY_MS: u32 = 20;
const DEFAULT_DELAY_MS: u32 = 100;
/// Tolerance for "zoomed in further than fit".
const FIT_EPS: f32 = 1e-4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redraw {
    None,
    Full,
}

/// What to draw in the image area.
pub enum Scene<'a> {
    Empty,
    /// The current photo is still decoding after the loading delay.
    Loading,
    /// `rect` is the on-screen box of the image turned clockwise by `quarters` × 90°.
    Image {
        rect: (f32, f32, f32, f32),
        quarters: u8,
    },
    Message(&'a str),
}

enum Content {
    Image {
        result: DecodeResult,
        view: Viewport,
    },
    Message(String),
}

/// The photo on screen. While the current photo is still decoding this is the
/// previous one, so the screen does not flash empty.
struct Shown {
    path: PathBuf,
    content: Content,
}

pub struct Viewer {
    list: FolderList,
    prefetcher: Prefetcher,
    shown: Option<Shown>,
    /// Bumped whenever `shown` changes to another photo.
    generation: u64,
    /// Animation frame of the shown image.
    frame: usize,
    quarters: u8,
    /// The last move was towards the start of the folder.
    backward: bool,
    /// The last prefetch request, current photo first.
    wanted: Vec<PathBuf>,
    /// Image area in DIPs.
    view_size: (f32, f32),
    /// DIPs per device pixel.
    dip: f32,
    /// The user zoomed or panned since the viewport was last built.
    touched: bool,
    /// The current photo has been pending longer than the loading delay.
    loading: bool,
}

pub fn error_message(e: &DecodeError) -> String {
    match e {
        DecodeError::TooLarge { width, height } => {
            format!("너무 커서 열 수 없어요 ({width}×{height})")
        }
        _ => CANNOT_OPEN.to_owned(),
    }
}

/// The image size in DIPs (1 image pixel = 1 device pixel at scale 1.0), turned by
/// `quarters`.
fn image_dips(img: &DecodedImage, dip: f32, quarters: u8) -> (f32, f32) {
    let (w, h) = (img.width as f32 * dip, img.height as f32 * dip);
    if quarters % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    }
}

/// A viewport for a new view size / DPI that keeps the zoom relative to fit and the
/// image point at the view centre.
fn rescaled(old: &Viewport, old_size: (f32, f32), size: (f32, f32), image: (f32, f32)) -> Viewport {
    let (x, y, w, h) = old.image_rect();
    let rel = ((old_size.0 / 2.0 - x) / w, (old_size.1 / 2.0 - y) / h);
    let ratio = old.scale() / old.fit_scale();
    let centre = (size.0 / 2.0, size.1 / 2.0);
    let mut v = Viewport::initial(size, image);
    v.zoom_at(ratio * v.fit_scale() / v.scale(), centre);
    let (x, y, w, h) = v.image_rect();
    v.pan(centre.0 - (x + rel.0 * w), centre.1 - (y + rel.1 * h));
    v
}

fn frame_delay(img: &DecodedImage, frame: usize) -> Option<u32> {
    if img.frames.len() < 2 {
        return None;
    }
    let d = img.frames.get(frame)?.delay_ms;
    Some(if d < MIN_DELAY_MS {
        DEFAULT_DELAY_MS
    } else {
        d
    })
}

impl Viewer {
    pub fn new(prefetcher: Prefetcher) -> Viewer {
        Viewer {
            list: FolderList::default(),
            prefetcher,
            shown: None,
            generation: 0,
            frame: 0,
            quarters: 0,
            backward: false,
            wanted: Vec::new(),
            view_size: (0.0, 0.0),
            dip: 1.0,
            touched: false,
            loading: false,
        }
    }

    /// Opens `path` within its folder. Never decodes on this thread.
    pub fn open(&mut self, path: &Path) {
        let list = FolderList::scan(path)
            .unwrap_or_else(|_| FolderList::from_paths(vec![path.to_path_buf()], path));
        self.open_list(list);
    }

    fn open_list(&mut self, list: FolderList) {
        self.list = list;
        self.backward = false;
        self.quarters = 0;
        if self.showing_current() {
            // Reopening the shown photo: rebuild it unrotated and unzoomed.
            self.shown = None;
        }
        self.after_move();
    }

    /// The image area size in DIPs and the DIPs per device pixel. An untouched
    /// viewport is rebuilt; a zoomed one keeps its zoom (relative to fit on a DPI
    /// change).
    pub fn set_view(&mut self, size: (f32, f32), dip: f32) {
        if size.0 < 1.0 || size.1 < 1.0 {
            return; // minimized
        }
        let old_size = std::mem::replace(&mut self.view_size, size);
        let dpi_changed = std::mem::replace(&mut self.dip, dip) != dip;
        let (touched, quarters) = (self.touched, self.quarters);
        let Some(Shown {
            content: Content::Image { result, view },
            ..
        }) = &mut self.shown
        else {
            return;
        };
        let Ok(img) = result.as_ref() else { return };
        let image = image_dips(img, dip, quarters);
        if !touched {
            *view = Viewport::initial(size, image);
        } else if dpi_changed {
            *view = rescaled(view, old_size, size, image);
        } else {
            view.resize(size);
        }
    }

    pub fn apply(&mut self, cmd: Command) -> Redraw {
        let centre = (self.view_size.0 / 2.0, self.view_size.1 / 2.0);
        self.apply_at(cmd, centre)
    }

    /// Like `apply`; zoom commands keep the image point under `anchor` (view DIPs) fixed.
    pub fn apply_at(&mut self, cmd: Command, anchor: (f32, f32)) -> Redraw {
        match cmd {
            Command::Next | Command::Prev => {
                let backward = cmd == Command::Prev;
                let moved = if backward {
                    self.list.go_prev()
                } else {
                    self.list.go_next()
                };
                if !moved {
                    return Redraw::None;
                }
                self.backward = backward;
                self.quarters = 0;
                self.after_move()
            }
            Command::ZoomIn => self.with_view(|v| v.zoom_at(ZOOM_STEP, anchor)),
            Command::ZoomOut => self.with_view(|v| v.zoom_at(1.0 / ZOOM_STEP, anchor)),
            Command::ToggleFitActual => self.with_view(|v| v.toggle_fit_actual(anchor)),
            Command::RotateLeft | Command::RotateRight => {
                if !self.showing_current() || self.view().is_none() {
                    return Redraw::None;
                }
                let turn = if cmd == Command::RotateRight { 1 } else { 3 };
                self.quarters = (self.quarters + turn) % 4;
                self.touched = false;
                let (size, dip) = (self.view_size, self.dip);
                self.set_view(size, dip);
                Redraw::Full
            }
            // Fullscreen and Escape belong to the window; the rest arrive in M3/M4.
            _ => Redraw::None,
        }
    }

    pub fn pan(&mut self, dx: f32, dy: f32) -> Redraw {
        self.with_view(|v| v.pan(dx, dy))
    }

    /// Zoomed in further than fit, so dragging moves the image.
    pub fn pannable(&self) -> bool {
        self.view()
            .is_some_and(|v| v.scale() > v.fit_scale() + FIT_EPS)
    }

    fn with_view(&mut self, f: impl FnOnce(&mut Viewport)) -> Redraw {
        match &mut self.shown {
            Some(Shown {
                content: Content::Image { view, .. },
                ..
            }) => {
                f(view);
                self.touched = true;
                Redraw::Full
            }
            _ => Redraw::None,
        }
    }

    fn after_move(&mut self) -> Redraw {
        let offsets: [isize; 4] = if self.backward {
            [0, -1, 1, -2]
        } else {
            [0, 1, -1, 2]
        };
        self.wanted = offsets
            .iter()
            .filter_map(|&o| self.list.neighbor(o))
            .map(Path::to_path_buf)
            .collect();
        self.prefetcher.request(&self.wanted);
        self.show_current();
        Redraw::Full
    }

    /// A decode finished; shows it if it is the current photo.
    pub fn on_image_ready(&mut self, path: PathBuf) -> Redraw {
        if self.list.current() != Some(path.as_path()) {
            return Redraw::None;
        }
        self.show_current()
    }

    /// Re-checks whether the current photo is ready (after a worker wake-up).
    pub fn check_current(&mut self) -> Redraw {
        match self.list.current() {
            Some(path) => self.on_image_ready(path.to_path_buf()),
            None => Redraw::None,
        }
    }

    fn show_current(&mut self) -> Redraw {
        if self.showing_current() {
            return Redraw::None;
        }
        let Some(path) = self.list.current().map(Path::to_path_buf) else {
            return Redraw::None;
        };
        let Some(result) = self.prefetcher.get(&path) else {
            return Redraw::None;
        };
        let content = match result.as_ref() {
            Ok(img) if !img.frames.is_empty() => Content::Image {
                view: Viewport::initial(self.view_size, image_dips(img, self.dip, self.quarters)),
                result: result.clone(),
            },
            Ok(_) => Content::Message(CANNOT_OPEN.to_owned()),
            Err(e) => Content::Message(error_message(e)),
        };
        self.shown = Some(Shown { path, content });
        self.generation += 1;
        self.frame = 0;
        self.touched = false;
        self.loading = false;
        Redraw::Full
    }

    fn showing_current(&self) -> bool {
        let current = self.list.current();
        current.is_some() && self.shown.as_ref().map(|s| s.path.as_path()) == current
    }

    /// The current photo is still decoding.
    pub fn pending(&self) -> bool {
        self.list.current().is_some() && !self.showing_current()
    }

    /// The loading delay passed: show the loading text if still pending.
    pub fn loading_overdue(&mut self) -> Redraw {
        if !self.pending() || self.loading {
            return Redraw::None;
        }
        self.loading = true;
        Redraw::Full
    }

    /// The GPU cannot hold the shown image; show the size message instead.
    pub fn too_large_to_draw(&mut self) {
        let Some((width, height, _)) = self.info() else {
            return;
        };
        if let Some(s) = &mut self.shown {
            s.content = Content::Message(error_message(&DecodeError::TooLarge { width, height }));
        }
    }

    pub fn view_rotation_quarters(&self) -> u8 {
        self.quarters
    }

    /// The view is turned but not saved (M3 asks before leaving such a photo).
    pub fn unsaved_rotation(&self) -> bool {
        self.view_rotation_quarters() != 0
    }

    pub fn has_file(&self) -> bool {
        self.list.current().is_some()
    }

    /// (1-based index, count), or (0, 0) with no file.
    pub fn position(&self) -> (usize, usize) {
        self.list.position().unwrap_or((0, 0))
    }

    pub fn title(&self) -> String {
        let Some(path) = self.list.current() else {
            return "light-view".to_owned();
        };
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let (i, n) = self.position();
        let mut title = format!("{name} · {i} / {n}");
        if self.unsaved_rotation() {
            title.push_str(" · 회전됨 · 저장 안 됨");
        }
        title
    }

    pub fn scene(&self) -> Scene<'_> {
        if self.loading {
            return Scene::Loading;
        }
        match &self.shown {
            Some(Shown {
                content: Content::Image { view, .. },
                ..
            }) => Scene::Image {
                rect: view.image_rect(),
                quarters: self.quarters,
            },
            Some(Shown {
                content: Content::Message(text),
                ..
            }) => Scene::Message(text),
            None => Scene::Empty,
        }
    }

    /// Width, height and file size of the shown image.
    pub fn info(&self) -> Option<(u32, u32, u64)> {
        self.image().map(|i| (i.width, i.height, i.byte_len))
    }

    pub fn zoom(&self) -> Option<f32> {
        self.view().map(Viewport::scale)
    }

    /// Changes whenever another photo is shown.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The shown image and the frame to draw.
    pub fn frame_to_upload(&self) -> Option<(&DecodedImage, usize)> {
        self.image().map(|i| (i, self.frame))
    }

    /// The current frame's delay in ms, if the shown photo is animated.
    pub fn frame_delay(&self) -> Option<u32> {
        frame_delay(self.image()?, self.frame)
    }

    /// Steps the animation; returns the new frame's delay, or `None` to stop (not
    /// animated, or the photo is being changed).
    pub fn advance_frame(&mut self) -> Option<u32> {
        if !self.showing_current() {
            return None;
        }
        let n = self.image()?.frames.len();
        if n < 2 {
            return None;
        }
        self.frame = (self.frame + 1) % n;
        self.frame_delay()
    }

    fn view(&self) -> Option<&Viewport> {
        match &self.shown {
            Some(Shown {
                content: Content::Image { view, .. },
                ..
            }) => Some(view),
            _ => None,
        }
    }

    fn image(&self) -> Option<&DecodedImage> {
        match &self.shown {
            Some(Shown {
                content: Content::Image { result, .. },
                ..
            }) => result.as_ref().as_ref().ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lv_core::decode::Frame;
    use lv_core::format::ImageFormat;
    use lv_core::prefetch::Decode;
    use std::sync::mpsc::{channel, Receiver};
    use std::time::Duration;

    const VIEW: (f32, f32) = (800.0, 600.0);

    /// Every file decodes to a 400×300 still image.
    struct FakeDecoder;

    impl Decode for FakeDecoder {
        fn decode(&self, _path: &Path) -> Result<DecodedImage, DecodeError> {
            Ok(DecodedImage {
                width: 400,
                height: 300,
                frames: vec![Frame {
                    rgba: vec![0; 4],
                    delay_ms: 0,
                }],
                icc: None,
                format: ImageFormat::Png,
                byte_len: 0,
            })
        }
    }

    fn p(name: &str) -> PathBuf {
        PathBuf::from(format!(r"C:\pics\{name}"))
    }

    /// A viewer on a.jpg … e.jpg, opened at `name`, and its ready notifications.
    fn viewer_at(name: &str) -> (Viewer, Receiver<PathBuf>) {
        let (tx, rx) = channel();
        let prefetcher = Prefetcher::new(FakeDecoder, 1, u64::MAX, move |path| {
            let _ = tx.send(path);
        });
        let mut v = Viewer::new(prefetcher);
        v.set_view(VIEW, 1.0);
        let names = ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg"];
        v.open_list(FolderList::from_paths(names.map(p).to_vec(), &p(name)));
        (v, rx)
    }

    /// Feeds ready notifications to the viewer until the current photo is shown.
    fn wait_shown(v: &mut Viewer, rx: &Receiver<PathBuf>) {
        while !v.showing_current() {
            let path = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the current photo never became ready");
            v.on_image_ready(path);
        }
    }

    #[test]
    fn next_requests_prefetch_in_direction() {
        let (mut v, _rx) = viewer_at("c.jpg");
        assert_eq!(v.apply(Command::Next), Redraw::Full);
        assert_eq!(v.wanted, [p("d.jpg"), p("e.jpg"), p("c.jpg")]);
    }

    #[test]
    fn prev_requests_prefetch_backwards() {
        let (mut v, _rx) = viewer_at("d.jpg");
        v.apply(Command::Prev);
        assert_eq!(v.wanted, [p("c.jpg"), p("b.jpg"), p("d.jpg"), p("a.jpg")]);
    }

    #[test]
    fn rotation_four_times_clears_unsaved() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateRight);
        assert_eq!(v.view_rotation_quarters(), 1);
        assert!(v.unsaved_rotation());
        for _ in 0..3 {
            v.apply(Command::RotateRight);
        }
        assert_eq!(v.view_rotation_quarters(), 0);
        assert!(!v.unsaved_rotation());
        v.apply(Command::RotateLeft);
        assert_eq!(v.view_rotation_quarters(), 3);
    }

    #[test]
    fn title_shows_position_and_unsaved() {
        let (mut v, rx) = viewer_at("c.jpg");
        assert_eq!(v.title(), "c.jpg · 3 / 5");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateLeft);
        assert_eq!(v.title(), "c.jpg · 3 / 5 · 회전됨 · 저장 안 됨");
    }

    #[test]
    fn changing_photo_resets_rotation_and_viewport() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateRight);
        // Rotated: the viewport is rebuilt for the turned (300×400) image.
        assert_eq!(v.view(), Some(&Viewport::initial(VIEW, (300.0, 400.0))));
        v.apply(Command::ZoomIn);
        assert_ne!(v.view(), Some(&Viewport::initial(VIEW, (300.0, 400.0))));

        v.apply(Command::Next);
        assert_eq!(v.view_rotation_quarters(), 0);
        wait_shown(&mut v, &rx);
        assert_eq!(v.view(), Some(&Viewport::initial(VIEW, (400.0, 300.0))));
        assert_eq!(v.title(), "d.jpg · 4 / 5");
    }

    #[test]
    fn dpi_change_keeps_zoom_relative_to_fit() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        for _ in 0..4 {
            v.apply(Command::ZoomIn);
        }
        let before = v.view().unwrap().clone();
        let ratio = before.scale() / before.fit_scale();
        v.set_view(VIEW, 0.5);
        let after = v.view().unwrap();
        assert!((after.scale() / after.fit_scale() - ratio).abs() < 1e-3);
    }

    #[test]
    fn too_large_error_names_the_size() {
        let e = DecodeError::TooLarge {
            width: 30000,
            height: 20000,
        };
        assert_eq!(error_message(&e), "너무 커서 열 수 없어요 (30000×20000)");
        assert_eq!(error_message(&DecodeError::Unsupported), CANNOT_OPEN);
    }
}
