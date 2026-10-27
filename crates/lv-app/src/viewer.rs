//! Viewer state: folder position, prefetching, the displayed photo, zoom and view
//! rotation. No Win32 here; the window feeds it commands and sizes and draws what it
//! reports.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lv_core::command::Command;
use lv_core::decode::{DecodeError, DecodedImage};
use lv_core::exif_info::ExifData;
use lv_core::identity::FileIdentity;
use lv_core::listing::FolderList;
use lv_core::prefetch::{DecodeResult, Prefetcher};
use lv_core::viewport::{Viewport, ZOOM_STEP};

use crate::strings::{self, t, text, Lang, S};

mod leave;
mod rescan;

pub use leave::{LeaveDecision, LeaveReason};
pub use rescan::RescanFound;

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

/// What a folder rescan has to tell the user about the current photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanNotice {
    /// It left the folder while its rotation was unsaved; the rotation is gone.
    Vanished,
    /// It was changed by another program while its rotation was unsaved; it is decoded
    /// again, unrotated. Without a rotation the reload is silent (ruling R20).
    Reloaded,
}

/// What to draw in the image area (the loading pill is drawn on top, see `loading`).
pub enum Scene {
    Empty,
    /// `rect` is the on-screen box of the image turned clockwise by `quarters` × 90°.
    Image {
        rect: (f32, f32, f32, f32),
        quarters: u8,
    },
    Message(String),
}

enum Content {
    Image {
        result: DecodeResult,
        view: Viewport,
    },
    Message(Message),
}

/// The photo on screen. While the current photo is still decoding this is the
/// previous one, so the screen does not flash empty.
struct Shown {
    path: PathBuf,
    content: Content,
    /// The view rotation it is drawn with; kept while the next photo loads.
    quarters: u8,
    /// The file as it was right before its decode read it (or as the app last saved it);
    /// saves and deletes act only on that file. `None` if it could not be read.
    identity: Option<FileIdentity>,
    /// The file was saved since it was decoded: it stays drawn (with its old rotation,
    /// which the saved file now holds) until the new decode replaces it.
    stale: bool,
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
    /// A move came back to the photo still on screen; its animation timer, stopped
    /// while another photo was pending, must be restarted (the generation did not change).
    rearm_animation: bool,
    /// `Settings::auto_save_rotation`.
    auto_save: bool,
    /// A save of the shown photo failed: leaving it asks again even with `auto_save`, so
    /// a file that cannot be saved never traps the user.
    save_failed: bool,
    /// What waits for the save prompt or a running save.
    pending_leave: Option<LeaveReason>,
}

/// What the image area says instead of a photo. Kept as the reason, not its text, so a
/// language change shows it in the new language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    CannotOpen,
    /// Too large; with the pixel size when it is known.
    TooLarge(Option<(u32, u32)>),
}

impl Message {
    pub fn for_error(e: &DecodeError) -> Message {
        match e {
            DecodeError::TooLarge { width, height } => Message::TooLarge(Some((*width, *height))),
            DecodeError::FileTooLarge { .. } => Message::TooLarge(None),
            _ => Message::CannotOpen,
        }
    }

    pub fn text(self, lang: Lang) -> String {
        match self {
            Message::CannotOpen => text(lang, S::CannotOpen).to_owned(),
            Message::TooLarge(None) => text(lang, S::TooLarge).to_owned(),
            Message::TooLarge(Some((w, h))) => strings::too_large_text(lang, w, h),
        }
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
            rearm_animation: false,
            auto_save: false,
            save_failed: false,
            pending_leave: None,
        }
    }

    /// Opens `path` within its folder. Never decodes on this thread.
    pub fn open(&mut self, path: &Path) {
        let mut list = FolderList::scan(path)
            .unwrap_or_else(|_| FolderList::from_paths(vec![path.to_path_buf()], path));
        // A pending delete stays hidden in the new listing.
        for hidden in self.list.hidden() {
            list.hide(hidden);
        }
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
        let touched = self.touched;
        let Some(Shown {
            content: Content::Image { result, view },
            quarters,
            ..
        }) = &mut self.shown
        else {
            return;
        };
        let Ok(img) = result.as_ref() else { return };
        let image = image_dips(img, dip, *quarters);
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
                if let Some(s) = &mut self.shown {
                    s.quarters = self.quarters;
                }
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

    /// Asks the prefetcher for the current photo and one neighbour each side, the one in
    /// the direction of the last move first (spec §5.2). A second photo ahead was
    /// dropped in M5: it cost a decoded 12 MP photo (~46 MB) and kept the app over the
    /// 300 MB target. For a ~12 MP JPEG one decode (~0.25–0.3 s) is quicker than normal
    /// paging; larger photos (24 MP and up, HEIC) may show "불러오는 중…" briefly on a
    /// quick second press.
    fn request_wanted(&mut self) {
        let offsets: [isize; 3] = if self.backward {
            [0, -1, 1]
        } else {
            [0, 1, -1]
        };
        self.wanted = offsets
            .iter()
            .filter_map(|&o| self.list.neighbor(o))
            .map(Path::to_path_buf)
            .collect();
        self.prefetcher.request(&self.wanted);
    }

    fn after_move(&mut self) -> Redraw {
        self.request_wanted();
        match &self.shown {
            // Back on the photo still on screen (the move away never finished): take up
            // its rotation again, drop the loading note and restart its animation.
            Some(s) if self.showing_current() => {
                self.quarters = s.quarters;
                self.loading = false;
                self.rearm_animation = true;
            }
            _ => {
                self.show_current();
            }
        }
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
        let Some((result, identity)) = self.prefetcher.get_with_identity(&path) else {
            return Redraw::None;
        };
        let content = match result.as_ref() {
            Ok(img) if !img.frames.is_empty() => Content::Image {
                view: Viewport::initial(self.view_size, image_dips(img, self.dip, self.quarters)),
                result: result.clone(),
            },
            Ok(_) => Content::Message(Message::CannotOpen),
            Err(e) => Content::Message(Message::for_error(e)),
        };
        self.shown = Some(Shown {
            path,
            content,
            quarters: self.quarters,
            identity,
            stale: false,
        });
        self.generation += 1;
        self.save_failed = false;
        self.frame = 0;
        self.touched = false;
        self.loading = false;
        Redraw::Full
    }

    fn showing_current(&self) -> bool {
        let current = self.list.current();
        current.is_some()
            && self
                .shown
                .as_ref()
                .filter(|s| !s.stale)
                .map(|s| s.path.as_path())
                == current
    }

    /// The current photo is still decoding.
    pub fn pending(&self) -> bool {
        self.list.current().is_some() && !self.showing_current()
    }

    /// The current photo has been pending longer than the loading delay; the previous
    /// photo stays on screen under a small loading note.
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// The loading delay passed: show the loading note if still pending.
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
            s.content = Content::Message(Message::TooLarge(Some((width, height))));
        }
    }

    /// True once after a move returned to the photo still on screen: restart the
    /// animation timer with `frame_delay`.
    pub fn take_animation_rearm(&mut self) -> bool {
        std::mem::take(&mut self.rearm_animation)
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

    /// The current photo's path (a dialog's target).
    pub fn current_path(&self) -> Option<&Path> {
        self.list.current()
    }

    /// The folder listed (and watched).
    pub fn folder(&self) -> Option<&Path> {
        self.list.dir()
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
            title.push_str(t(S::TitleRotatedUnsaved));
        }
        title
    }

    pub fn scene(&self) -> Scene {
        match &self.shown {
            Some(Shown {
                content: Content::Image { view, .. },
                quarters,
                ..
            }) => Scene::Image {
                rect: view.image_rect(),
                quarters: *quarters,
            },
            Some(Shown {
                content: Content::Message(m),
                ..
            }) => Scene::Message(m.text(strings::current())),
            None => Scene::Empty,
        }
    }

    /// Width, height and file size of the shown image.
    pub fn info(&self) -> Option<(u32, u32, u64)> {
        self.image().map(|i| (i.width, i.height, i.byte_len))
    }

    /// The full size of a JPEG opened at reduced size (ruling R1); `None` otherwise.
    pub fn scaled_from(&self) -> Option<(u32, u32)> {
        self.image()?.scaled_from
    }

    /// The shown image's EXIF, read with its decode (ruling R5).
    pub fn exif(&self) -> Option<&Arc<ExifData>> {
        self.image()?.exif.as_ref()
    }

    /// The shown file's modification time (FILETIME ticks), as it was when decoded.
    pub fn modified(&self) -> Option<u64> {
        self.shown.as_ref()?.identity.as_ref().map(|id| id.modified)
    }

    pub fn zoom(&self) -> Option<f32> {
        self.view().map(Viewport::scale)
    }

    /// Changes whenever another photo is shown.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The shown image (always a decoded one) and the frame to draw.
    pub fn frame_to_upload(&self) -> Option<(&DecodeResult, usize)> {
        match &self.shown {
            Some(Shown {
                content: Content::Image { result, .. },
                ..
            }) if result.is_ok() => Some((result, self.frame)),
            _ => None,
        }
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
    use lv_core::identity;
    use lv_core::prefetch::Decode;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Mutex;
    use std::time::Duration;

    const VIEW: (f32, f32) = (800.0, 600.0);

    /// Every file decodes to a 400×300 still image. The gated path blocks until its
    /// sender is used or dropped.
    struct FakeDecoder {
        gate: Option<(PathBuf, Mutex<Receiver<()>>)>,
    }

    impl Decode for FakeDecoder {
        fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError> {
            if let Some((gated, rx)) = &self.gate {
                if gated == path {
                    let _ = rx.lock().unwrap().recv();
                }
            }
            Ok(DecodedImage {
                width: 400,
                height: 300,
                frames: vec![Frame {
                    bgra: vec![0; 4],
                    delay_ms: 0,
                }],
                format: ImageFormat::Png,
                byte_len: 0,
                exif: None,
                scaled_from: None,
            })
        }
    }

    fn p(name: &str) -> PathBuf {
        PathBuf::from(format!(r"C:\pics\{name}"))
    }

    /// A viewer on a.jpg … e.jpg, opened at `name`, and its ready notifications.
    fn viewer_at(name: &str) -> (Viewer, Receiver<PathBuf>) {
        viewer_with(name, None)
    }

    /// Like `viewer_at`, but decoding `gated` waits for the returned sender.
    fn gated_viewer_at(name: &str, gated: &str) -> (Viewer, Receiver<PathBuf>, Sender<()>) {
        let (release, gate) = channel();
        let (v, rx) = viewer_with(name, Some((p(gated), Mutex::new(gate))));
        (v, rx, release)
    }

    fn viewer_with(
        name: &str,
        gate: Option<(PathBuf, Mutex<Receiver<()>>)>,
    ) -> (Viewer, Receiver<PathBuf>) {
        let (tx, rx) = channel();
        let prefetcher = Prefetcher::new(FakeDecoder { gate }, 1, u64::MAX, move |path| {
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
    fn opened_before_the_window_exists_shows_once_checked_and_sized() {
        // At start-up the first photo is opened before the window exists, so its decode
        // runs while the window and renderer are made; the ready message posted then is
        // lost and the window checks for the photo itself.
        let (tx, rx) = channel();
        let prefetcher = Prefetcher::new(FakeDecoder { gate: None }, 1, u64::MAX, move |path| {
            let _ = tx.send(path);
        });
        let mut v = Viewer::new(prefetcher);
        let names = ["a.jpg", "b.jpg", "c.jpg"];
        v.open_list(FolderList::from_paths(names.map(p).to_vec(), &p("b.jpg")));
        let first = rx.recv_timeout(Duration::from_secs(5)).expect("a decode");
        assert_eq!(first, p("b.jpg"), "the opened photo decodes first");
        assert_eq!(v.check_current(), Redraw::Full);
        v.set_view(VIEW, 1.0);
        match v.scene() {
            Scene::Image { rect, .. } => assert_eq!((rect.2, rect.3), (400.0, 300.0)),
            _ => panic!("the photo should be shown"),
        }
    }

    #[test]
    fn prev_requests_prefetch_backwards() {
        let (mut v, _rx) = viewer_at("d.jpg");
        v.apply(Command::Prev);
        assert_eq!(v.wanted, [p("c.jpg"), p("b.jpg"), p("d.jpg")]);
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
    fn loading_keeps_the_previous_photo_on_screen() {
        let (mut v, rx, release) = gated_viewer_at("c.jpg", "d.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::Next);
        assert!(!v.loading());
        assert_eq!(v.loading_overdue(), Redraw::Full);
        assert!(v.loading());
        let previous = Viewport::initial(VIEW, (400.0, 300.0)).image_rect();
        assert!(matches!(v.scene(), Scene::Image { rect, quarters: 0 } if rect == previous));
        drop(release);
    }

    #[test]
    fn previous_rotated_photo_keeps_its_rotation_while_loading() {
        let (mut v, rx, release) = gated_viewer_at("c.jpg", "d.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateRight);
        v.apply(Command::Next);
        assert_eq!(v.view_rotation_quarters(), 0);
        assert_eq!(v.title(), "d.jpg · 4 / 5");
        let Scene::Image { rect, quarters } = v.scene() else {
            panic!("the previous photo should still be drawn");
        };
        // c turned once: a 300×400 box drawn with one quarter turn.
        assert_eq!(quarters, 1);
        assert!((rect.2 / rect.3 - 300.0 / 400.0).abs() < 1e-4);
        drop(release);
    }

    #[test]
    fn next_then_prev_on_a_still_pending_photo_restores_the_shown_one() {
        let (mut v, rx, release) = gated_viewer_at("c.jpg", "d.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateRight);
        let generation = v.generation();
        assert!(!v.take_animation_rearm());

        v.apply(Command::Next);
        assert_eq!(v.loading_overdue(), Redraw::Full);
        assert!(v.loading());
        // The animation timer stops while d is pending (advance_frame returns None).
        assert_eq!(v.advance_frame(), None);

        assert_eq!(v.apply(Command::Prev), Redraw::Full);
        assert!(!v.pending());
        assert!(!v.loading());
        assert_eq!(v.view_rotation_quarters(), 1);
        assert!(v.unsaved_rotation());
        assert_eq!(v.title(), "c.jpg · 3 / 5 · 회전됨 · 저장 안 됨");
        assert!(matches!(v.scene(), Scene::Image { quarters: 1, .. }));
        // Same photo, so no new generation: the window must re-arm the timer itself.
        assert_eq!(v.generation(), generation);
        assert!(v.take_animation_rearm());
        assert!(!v.take_animation_rearm());
        // Rotating again turns from the restored rotation.
        v.apply(Command::RotateRight);
        assert_eq!(v.view_rotation_quarters(), 2);
        drop(release);
    }

    #[test]
    fn leave_with_unsaved_rotation_asks() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        let next = LeaveReason::Move(Command::Next);
        assert_eq!(v.before_leave(next.clone()), LeaveDecision::Proceed);
        assert_eq!(v.take_pending_leave(), None);

        v.apply(Command::RotateRight);
        assert_eq!(v.before_leave(next.clone()), LeaveDecision::Ask);
        assert_eq!(v.take_pending_leave(), Some(next));
        for reason in [LeaveReason::Open(p("x.jpg")), LeaveReason::Close] {
            assert_eq!(v.before_leave(reason.clone()), LeaveDecision::Ask);
            assert_eq!(v.take_pending_leave(), Some(reason));
        }
        // Nothing to ask at the end of the folder: the move would not move.
        let (mut v, rx) = viewer_at("e.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateLeft);
        let next = LeaveReason::Move(Command::Next);
        assert_eq!(v.before_leave(next), LeaveDecision::Proceed);
        let prev = LeaveReason::Move(Command::Prev);
        assert_eq!(v.before_leave(prev), LeaveDecision::Ask);
    }

    #[test]
    fn auto_save_skips_prompt() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        v.set_auto_save(true);
        v.apply(Command::RotateRight);
        let next = LeaveReason::Move(Command::Next);
        assert_eq!(v.before_leave(next.clone()), LeaveDecision::AutoSave);
        assert_eq!(v.take_pending_leave(), Some(next.clone()));
        // A failed save stays on the photo; leaving it then asks.
        v.before_leave(next.clone());
        v.on_save_failed();
        assert_eq!(v.take_pending_leave(), None);
        assert_eq!(v.before_leave(next), LeaveDecision::Ask);
    }

    #[test]
    fn cancel_keeps_position() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateRight);
        assert_eq!(
            v.before_leave(LeaveReason::Move(Command::Prev)),
            LeaveDecision::Ask
        );
        v.cancel_leave();
        assert_eq!(v.take_pending_leave(), None);
        assert_eq!(v.position(), (3, 5));
        assert_eq!(v.view_rotation_quarters(), 1);
        assert!(v.unsaved_rotation());
    }

    #[test]
    fn a_waiting_close_is_not_replaced() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateRight);
        v.before_leave(LeaveReason::Close);
        assert!(v.closing_after_save());
        v.defer_leave(LeaveReason::Open(p("x.jpg")));
        v.before_leave(LeaveReason::Move(Command::Next));
        assert_eq!(v.take_pending_leave(), Some(LeaveReason::Close));
    }

    #[test]
    fn discard_drops_the_rotation() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        v.apply(Command::RotateRight);
        assert_eq!(v.discard_rotation(), Redraw::Full);
        assert!(!v.unsaved_rotation());
        assert!(matches!(v.scene(), Scene::Image { quarters: 0, .. }));
        assert_eq!(v.view(), Some(&Viewport::initial(VIEW, (400.0, 300.0))));
    }

    #[test]
    fn saved_photo_is_decoded_again_unrotated() {
        let (mut v, rx) = viewer_at("c.jpg");
        wait_shown(&mut v, &rx);
        assert_eq!(v.save_request(), None);
        v.apply(Command::RotateRight);
        let request = v.save_request().expect("a rotated photo on screen");
        assert_eq!((request.path.clone(), request.quarters), (p("c.jpg"), 1));
        let generation = v.generation();

        v.on_saved(&p("c.jpg"), None);
        assert!(!v.unsaved_rotation());
        assert_eq!(v.save_request(), None);
        // The old decode stays up, turned as the file now is, until the new one is in.
        assert!(v.pending());
        assert!(matches!(v.scene(), Scene::Image { quarters: 1, .. }));
        assert_eq!(v.apply(Command::RotateRight), Redraw::None);

        wait_shown(&mut v, &rx);
        assert!(v.generation() > generation);
        assert!(matches!(v.scene(), Scene::Image { quarters: 0, .. }));
        assert_eq!(v.title(), "c.jpg · 3 / 5");
    }

    #[test]
    fn a_file_changed_after_its_decode_is_not_saved() {
        use crate::save::{save_rotation, NoWorker, SaveOutcome};
        use lv_core::decode::DecodeLimits;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, b"as decoded").unwrap();
        let (tx, rx) = channel();
        let prefetcher = Prefetcher::new(FakeDecoder { gate: None }, 1, u64::MAX, move |p| {
            let _ = tx.send(p);
        });
        let mut v = Viewer::new(prefetcher);
        v.set_view(VIEW, 1.0);
        v.open_list(FolderList::from_paths(vec![path.clone()], &path));
        // Decoded (and cached) first; the file then changes before it is shown.
        let ready = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::fs::write(&path, b"changed after the decode").unwrap();
        v.on_image_ready(ready);
        assert!(v.showing_current());
        v.apply(Command::RotateRight);
        let request = v.save_request().unwrap();
        let id = request.identity.expect("read before the decode");
        let outcome = save_rotation(&path, &id, 1, &NoWorker, &DecodeLimits::default());
        assert_eq!(outcome, SaveOutcome::Changed);
        assert_eq!(std::fs::read(&path).unwrap(), b"changed after the decode");
    }

    #[test]
    fn deleting_hides_the_photo_and_undo_shows_it_again() {
        // d's decode is held back, so it cannot be prefetched (and shown from the
        // cache) before c is hidden.
        let (mut v, rx, release_d) = gated_viewer_at("c.jpg", "d.jpg");
        wait_shown(&mut v, &rx);
        let (path, _) = v.delete_target().expect("c is on screen");
        assert_eq!(path, p("c.jpg"));
        assert_eq!(v.hide(&path), Redraw::Full);
        assert_eq!(v.title(), "d.jpg · 3 / 4");
        // d is not on screen yet: nothing to delete until it is.
        assert_eq!(v.delete_target(), None);
        release_d.send(()).unwrap();
        wait_shown(&mut v, &rx);
        assert_eq!(v.delete_target().unwrap().0, p("d.jpg"));

        assert_eq!(v.restore(&p("c.jpg")), Redraw::Full);
        assert_eq!(v.title(), "c.jpg · 3 / 5");
        wait_shown(&mut v, &rx);
        assert_eq!(v.delete_target().unwrap().0, p("c.jpg"));
    }

    #[test]
    fn opening_again_keeps_a_pending_delete_hidden() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.jpg", "b.jpg", "c.jpg"] {
            std::fs::write(dir.path().join(n), b"").unwrap();
        }
        let (mut v, _rx) = viewer_at("a.jpg");
        v.open(&dir.path().join("b.jpg"));
        v.hide(&dir.path().join("b.jpg"));
        v.open(&p("a.jpg"));
        v.open(&dir.path().join("c.jpg"));
        assert_eq!(v.position(), (2, 2));
        v.restore(&dir.path().join("b.jpg"));
        assert_eq!(v.position(), (2, 3));
    }

    #[test]
    fn deleting_the_last_photos() {
        let (mut v, rx) = viewer_at("e.jpg");
        wait_shown(&mut v, &rx);
        v.hide(&p("e.jpg"));
        assert_eq!(v.title(), "d.jpg · 4 / 4");
        for name in ["a.jpg", "b.jpg", "c.jpg"] {
            v.remove(&p(name));
        }
        v.hide(&p("d.jpg"));
        assert!(!v.has_file());
        assert!(matches!(v.scene(), Scene::Empty));
        assert_eq!(v.title(), "light-view");
        // A failed recycle lists d again (and shows it: nothing else is left).
        assert_eq!(v.unhide(&p("d.jpg")), Redraw::Full);
        assert_eq!(v.title(), "d.jpg · 1 / 1");
        // Removed photos never come back.
        assert_eq!(v.unhide(&p("a.jpg")), Redraw::None);
        wait_shown(&mut v, &rx);
        v.forget_hidden(&p("d.jpg"));
        assert_eq!(v.position(), (1, 1));
    }

    /// A folder with `names` (each file holds its own name) and a viewer opened on
    /// `open` in it, shown.
    fn viewer_in_folder(
        names: &[&str],
        open: &str,
    ) -> (tempfile::TempDir, Viewer, Receiver<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        for n in names {
            std::fs::write(dir.path().join(n), n.as_bytes()).unwrap();
        }
        let (mut v, rx) = viewer_at("a.jpg");
        v.open(&dir.path().join(open));
        wait_shown(&mut v, &rx);
        (dir, v, rx)
    }

    #[test]
    fn rescan_keeps_current_and_hidden() {
        let (dir, mut v, _rx) = viewer_in_folder(&["a.jpg", "b.jpg", "c.jpg", "d.jpg"], "c.jpg");
        assert_eq!(v.folder(), Some(dir.path()));
        // b is a pending delete: still on disk, kept out of the list.
        v.hide(&dir.path().join("b.jpg"));
        let generation = v.generation();
        assert_eq!(v.rescan(), (Redraw::None, None));
        std::fs::write(dir.path().join("0.jpg"), b"new").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"").unwrap();
        assert_eq!(v.rescan(), (Redraw::Full, None));
        assert_eq!(v.title(), "c.jpg · 3 / 4");
        assert_eq!(v.current_path(), Some(dir.path().join("c.jpg").as_path()));
        assert_eq!(v.generation(), generation);
        assert!(!v.pending());
        assert_eq!(
            v.wanted[..2],
            [dir.path().join("c.jpg"), dir.path().join("d.jpg")]
        );
        // Undo still puts b back in place.
        v.restore(&dir.path().join("b.jpg"));
        assert_eq!(v.title(), "b.jpg · 3 / 5");
    }

    #[test]
    fn current_removed_moves_to_neighbor_and_drops_rotation() {
        let (dir, mut v, rx) = viewer_in_folder(&["a.jpg", "b.jpg", "c.jpg"], "b.jpg");
        v.apply(Command::RotateRight);
        std::fs::remove_file(dir.path().join("b.jpg")).unwrap();
        assert_eq!(v.rescan(), (Redraw::Full, Some(RescanNotice::Vanished)));
        assert_eq!(v.title(), "c.jpg · 2 / 2");
        assert!(!v.unsaved_rotation());
        assert_eq!(v.save_request(), None);
        wait_shown(&mut v, &rx);
        assert!(matches!(v.scene(), Scene::Image { quarters: 0, .. }));
        // Without a rotation the move is silent; at the end it goes to the previous one.
        std::fs::remove_file(dir.path().join("c.jpg")).unwrap();
        assert_eq!(v.rescan(), (Redraw::Full, None));
        assert_eq!(v.title(), "a.jpg · 1 / 1");
        std::fs::remove_file(dir.path().join("a.jpg")).unwrap();
        assert_eq!(v.rescan(), (Redraw::Full, None));
        assert!(!v.has_file());
        assert!(matches!(v.scene(), Scene::Empty));
        // The folder is still watched: a new file is shown.
        std::fs::write(dir.path().join("n.jpg"), b"n").unwrap();
        assert_eq!(v.rescan(), (Redraw::Full, None));
        assert_eq!(v.title(), "n.jpg · 1 / 1");
    }

    #[test]
    fn external_modification_invalidates() {
        let (dir, mut v, rx) = viewer_in_folder(&["a.jpg", "b.jpg"], "a.jpg");
        let path = dir.path().join("a.jpg");
        let generation = v.generation();
        v.apply(Command::RotateRight);
        std::fs::write(&path, b"edited elsewhere, longer").unwrap();
        assert_eq!(v.rescan(), (Redraw::Full, Some(RescanNotice::Reloaded)));
        assert!(!v.unsaved_rotation());
        assert_eq!(v.save_request(), None);
        wait_shown(&mut v, &rx);
        assert!(v.generation() > generation);
        let (_, id) = v.delete_target().unwrap();
        assert_eq!(id, identity::capture(&path).ok());
        // Unchanged since: nothing more to do. Without a rotation a reload is silent.
        assert_eq!(v.rescan(), (Redraw::None, None));
        std::fs::write(&path, b"edited again").unwrap();
        assert_eq!(v.rescan(), (Redraw::Full, None));
        wait_shown(&mut v, &rx);
    }

    #[test]
    fn a_changed_neighbor_is_decoded_again() {
        let (dir, mut v, rx) = viewer_in_folder(&["a.jpg", "b.jpg"], "a.jpg");
        let b = dir.path().join("b.jpg");
        while v.prefetcher.get(&b).is_none() {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        std::fs::write(&b, b"edited elsewhere, longer").unwrap();
        assert_eq!(v.rescan(), (Redraw::None, None));
        v.apply(Command::Next);
        wait_shown(&mut v, &rx);
        assert_eq!(v.delete_target().unwrap().1, identity::capture(&b).ok());
    }

    #[test]
    fn a_transient_missing_does_not_remove_the_current_photo() {
        use std::os::windows::fs::OpenOptionsExt;
        let (dir, mut v, _rx) = viewer_in_folder(&["a.jpg", "b.jpg"], "a.jpg");
        v.apply(Command::RotateRight);
        let path = dir.path().join("a.jpg");
        // Held open without sharing: reading its identity fails (a sharing violation),
        // but it is still listed and on disk.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(identity::capture(&path).is_err());
        assert_eq!(v.rescan(), (Redraw::None, None));
        drop(lock);
        assert_eq!(v.current_path(), Some(path.as_path()));
        assert!(v.unsaved_rotation());
        assert!(!v.pending());
    }

    #[test]
    fn a_rescan_for_an_old_photo_or_folder_is_dropped() {
        let (dir, mut v, rx) = viewer_in_folder(&["a.jpg", "b.jpg"], "a.jpg");
        std::fs::remove_file(dir.path().join("a.jpg")).unwrap();
        let job = v.rescan_job().unwrap();
        // The user moved on while the job ran (a new generation once b is shown).
        v.apply(Command::Next);
        wait_shown(&mut v, &rx);
        assert!(v.apply_rescan(job.clone().run()).is_none());
        assert_eq!(v.title(), "b.jpg · 2 / 2");
        // Another folder since.
        let (other, mut w, _rx2) = viewer_in_folder(&["x.jpg"], "x.jpg");
        let job = w.rescan_job().unwrap();
        w.open(&dir.path().join("b.jpg"));
        assert!(w.apply_rescan(job.run()).is_none());
        drop(other);
        // A failed run changes nothing.
        let job = v.rescan_job().unwrap();
        assert_eq!(v.apply_rescan(job.nothing()), Some((Redraw::None, None)));
        assert_eq!(v.title(), "b.jpg · 2 / 2");
        // A fresh one applies.
        assert_eq!(v.rescan(), (Redraw::Full, None));
        assert_eq!(v.title(), "b.jpg · 1 / 1");
    }

    #[test]
    fn too_large_error_names_the_size() {
        let e = DecodeError::TooLarge {
            width: 30000,
            height: 20000,
        };
        let ko = |e: &DecodeError| Message::for_error(e).text(Lang::Ko);
        assert_eq!(ko(&e), "너무 커서 열 수 없어요 (30000×20000)");
        assert_eq!(ko(&DecodeError::Unsupported), text(Lang::Ko, S::CannotOpen));
        assert_eq!(
            ko(&DecodeError::FileTooLarge { bytes: 1 << 40 }),
            "너무 커서 열 수 없어요"
        );
    }

    /// The shown message keeps what went wrong, not its text, so a language change
    /// redraws it in the new language.
    #[test]
    fn messages_follow_the_language() {
        let e = DecodeError::TooLarge {
            width: 3,
            height: 4,
        };
        let m = Message::for_error(&e);
        assert_eq!(m.text(Lang::En), "Too large to open (3×4)");
        assert_eq!(m.text(Lang::Ko), "너무 커서 열 수 없어요 (3×4)");
        let m = Message::for_error(&DecodeError::Unsupported);
        assert_eq!(m.text(Lang::En), "Couldn't open this photo");
        let m = Message::for_error(&DecodeError::FileTooLarge { bytes: 1 << 40 });
        assert_eq!(m.text(Lang::En), "Too large to open");
    }
}
