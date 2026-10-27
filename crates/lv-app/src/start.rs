//! The empty start screen (spec §4.3a) and the Open dialog: without a photo the window
//! shows one line, "사진을 끌어다 놓거나 열어 주세요", over a highlighter [열기] button,
//! and no toolbar. Also what a drop opens.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use lv_core::format::{is_supported_extension, EXTENSIONS};
use lv_core::listing::first_supported_in;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FILEMUSTEXIST,
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
};

use crate::commands;
use crate::render::{Align, Renderer, TextRun};
use crate::shell::{user_path, with_com};
use crate::strings::{t, S};
use crate::theme::Palette;
use crate::toolbar::Rect;
use crate::window::App;

// Layout (DIPs).
const TEXT_H: f32 = 24.0;
const GAP: f32 = 16.0;
const BUTTON_W: f32 = 120.0;
const BUTTON_H: f32 = 44.0;
const MARGIN: f32 = 16.0;
const BUTTON_RADIUS: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StartLayout {
    pub text: Rect,
    pub button: Rect,
}

/// The line and the button below it, centred together in the client area.
pub fn start_layout(view: (f32, f32)) -> StartLayout {
    let (w, h) = view;
    let top = ((h - (TEXT_H + GAP + BUTTON_H)) / 2.0).max(0.0);
    let text = Rect {
        x: MARGIN,
        y: top,
        w: (w - 2.0 * MARGIN).max(0.0),
        h: TEXT_H,
    };
    let button = Rect {
        x: ((w - BUTTON_W) / 2.0).max(0.0),
        y: top + TEXT_H + GAP,
        w: BUTTON_W,
        h: BUTTON_H,
    };
    StartLayout { text, button }
}

/// The [열기] button's press state.
#[derive(Debug, Default)]
pub struct StartScreen {
    pressed: bool,
}

impl StartScreen {
    /// Mouse down; true if it is on the button (the screen takes it).
    pub fn press(&mut self, x: f32, y: f32, l: &StartLayout) -> bool {
        self.pressed = l.button.contains(x, y);
        self.pressed
    }

    /// Mouse up: `None` if the press was not the button's; `Some(true)` when released
    /// on it (open the dialog).
    pub fn release(&mut self, x: f32, y: f32, l: &StartLayout) -> Option<bool> {
        if !std::mem::take(&mut self.pressed) {
            return None;
        }
        Some(l.button.contains(x, y))
    }
}

pub fn draw(r: &mut Renderer, l: &StartLayout, p: &Palette) {
    let hint = TextRun {
        text: t(S::StartHint),
        family: "Segoe UI",
        size: 15.0,
        bold: false,
        color: p.ink2,
    };
    let b = l.button;
    r.draw_runs_ellipsis(
        (l.text.x, l.text.y, l.text.w, l.text.h),
        &[hint],
        Align::Center,
    );
    r.fill_round_rect((b.x, b.y, b.w, b.h), BUTTON_RADIUS, p.highlighter);
    let label = TextRun {
        text: t(S::BtnOpen),
        family: "Segoe UI",
        size: 14.0,
        bold: true,
        color: p.on_highlighter,
    };
    r.draw_runs((b.x, b.y, b.w, b.h), &[label], Align::Center);
}

/// "*.jpg;*.jpeg;…": the Open dialog's filter for the supported formats.
pub fn filter_spec() -> String {
    EXTENSIONS
        .iter()
        .map(|e| format!("*.{e}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// What a drop opens: the first supported file, or the first supported file of the
/// first dropped folder that has one, in drop order. `None`: nothing to open.
pub fn pick_drop(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find_map(|p| {
        if p.is_dir() {
            first_supported_in(p).ok().flatten()
        } else {
            is_supported_extension(p).then(|| p.clone())
        }
    })
}

/// Ctrl+O, the menu's 열기 and the start screen's button: the Windows Open dialog,
/// starting in the shown photo's folder. The picked file goes through the usual open
/// (and so through the save prompt). No App borrow is held while the dialog runs.
pub fn open_dialog(app: &RefCell<App>, hwnd: HWND) {
    let folder = {
        let Ok(a) = app.try_borrow() else {
            return;
        };
        a.viewer.folder().map(Path::to_path_buf)
    };
    let picked = pick_file(hwnd, folder.as_deref());
    // A path forwarded by another launch while the dialog was up waited for it. The
    // user's pick, made after it, wins; without a pick the forwarded path opens.
    let forwarded = app
        .try_borrow_mut()
        .ok()
        .and_then(|mut a| a.overlay.take_deferred_open());
    if let Some(path) = picked.or(forwarded) {
        commands::open(app, hwnd, &path);
    }
}

/// Shows the modal Open dialog; `None` when cancelled or on failure.
fn pick_file(hwnd: HWND, folder: Option<&Path>) -> Option<PathBuf> {
    let photos = HSTRING::from(t(S::FilterPhotos));
    let all = HSTRING::from(t(S::FilterAll));
    let spec = HSTRING::from(filter_spec());
    let filters = [
        COMDLG_FILTERSPEC {
            pszName: PCWSTR(photos.as_ptr()),
            pszSpec: PCWSTR(spec.as_ptr()),
        },
        COMDLG_FILTERSPEC {
            pszName: PCWSTR(all.as_ptr()),
            pszSpec: w!("*.*"),
        },
    ];
    let folder = folder.map(|f| HSTRING::from(user_path(f).as_os_str()));
    with_com(|| {
        // SAFETY: COM is initialized for this thread by `with_com`; the filter strings
        // and `folder` outlive the dialog; the returned path is freed once with
        // CoTaskMemFree after copying it.
        let result = unsafe {
            (|| -> windows::core::Result<PathBuf> {
                let dialog: IFileOpenDialog =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                dialog.SetFileTypes(&filters)?;
                let options = dialog.GetOptions()?;
                dialog.SetOptions(
                    options | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST,
                )?;
                if let Some(dir) = &folder {
                    if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(dir, None) {
                        let _ = dialog.SetFolder(&item);
                    }
                }
                dialog.Show(hwnd)?;
                let item = dialog.GetResult()?;
                let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
                // Copied as UTF-16 units, so names that are not valid Unicode open too.
                let path = PathBuf::from(OsString::from_wide(name.as_wide()));
                CoTaskMemFree(Some(name.0 as *const _));
                Ok(path)
            })()
        };
        match result {
            Ok(path) => Some(path),
            Err(e) => {
                // ERROR_CANCELLED as an HRESULT: the user closed the dialog.
                if e.code().0 as u32 != 0x8007_04C7 {
                    eprintln!("light-view: the Open dialog failed: {e}");
                }
                None
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f32, f32) = (1000.0, 700.0);

    fn center(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn line_and_button_are_centred() {
        let l = start_layout(VIEW);
        // The pair is centred vertically: line, 16 DIP, then the 44 DIP button.
        let top = l.text.y;
        let bottom = l.button.y + l.button.h;
        assert!(((top + bottom) / 2.0 - 350.0).abs() < 0.01);
        assert_eq!(l.button.y, l.text.y + l.text.h + 16.0);
        assert!((l.button.x + l.button.w / 2.0 - 500.0).abs() < 0.01);
        assert!((l.text.x + l.text.w / 2.0 - 500.0).abs() < 0.01);
        assert!(l.button.h >= 44.0 && l.button.w >= 44.0);
        // The line may use the whole width but the 16 DIP margins.
        assert_eq!(l.text.x, 16.0);
        assert_eq!(l.text.w, 1000.0 - 32.0);
        // A tiny window: still inside, never negative.
        let l = start_layout((100.0, 50.0));
        assert!(l.button.x >= 0.0 && l.text.w >= 0.0 && l.text.y >= 0.0);
    }

    #[test]
    fn the_button_acts_on_release_over_it() {
        let l = start_layout(VIEW);
        let mut s = StartScreen::default();
        let (x, y) = center(l.button);
        assert!(!s.press(10.0, 10.0, &l));
        assert_eq!(s.release(x, y, &l), None);
        assert!(s.press(x, y, &l));
        assert_eq!(s.release(10.0, 10.0, &l), Some(false));
        assert!(s.press(x, y, &l));
        assert_eq!(s.release(x, y, &l), Some(true));
        assert_eq!(s.release(x, y, &l), None);
    }

    #[test]
    fn the_filter_lists_every_supported_extension() {
        let spec = filter_spec();
        assert!(spec.starts_with("*.jpg;*.jpeg;"));
        assert_eq!(spec.split(';').count(), EXTENSIONS.len());
        assert!(spec.split(';').any(|s| s == "*.heic"));
        assert!(!spec.ends_with(';'));
    }

    #[test]
    fn drops_open_the_first_photo_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        std::fs::write(p("notes.txt"), b"x").unwrap();
        std::fs::write(p("b.png"), b"x").unwrap();
        // Only unsupported files: nothing (the toast "열 수 있는 사진이 없어요").
        assert_eq!(pick_drop(&[p("notes.txt")]), None);
        assert_eq!(pick_drop(&[]), None);
        // The first supported one, skipping others.
        assert_eq!(pick_drop(&[p("notes.txt"), p("b.png")]), Some(p("b.png")));
        // A folder opens its first photo; one without photos is skipped.
        let empty = p("empty");
        std::fs::create_dir(&empty).unwrap();
        let sub = p("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("z.jpg"), b"x").unwrap();
        std::fs::write(sub.join("a.JPG"), b"x").unwrap();
        assert_eq!(pick_drop(std::slice::from_ref(&empty)), None);
        assert_eq!(pick_drop(&[empty, sub.clone()]), Some(sub.join("a.JPG")));
    }
}
