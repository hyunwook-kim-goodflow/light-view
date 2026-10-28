//! The sidebar's numbers as native EDIT child controls (M6 Task 6: only the numeric
//! fields are native; everything else is drawn). One EDIT per [`Field`]; the screen
//! shows those of the open tab inside frames it draws itself.
//!
//! An EDIT keeps the keys it handles (digits, the caret, selection, Ctrl+C/V); Tab,
//! Shift+Tab, Enter and Esc are posted to the main window as
//! `WM_APP_RESIZE_FIELD` (`wparam` the key, `lparam` the field and Shift) so the
//! sidebar's model handles them, without re-entering the window procedure.
//!
//! Each EDIT's accessible name ("긴 변 (px)") is set with Dynamic Annotation, since it
//! has no label control beside it.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, InvalidateRect, HBRUSH, HFONT, LOGFONTW,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{CLSID_AccPropServices, IAccPropServices, PROPID_ACC_NAME};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, VK_SHIFT};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetParent, GetWindowTextLengthW, GetWindowTextW, PostMessageW,
    SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow, CHILDID_SELF, ES_AUTOHSCROLL,
    ES_NUMBER, HMENU, OBJID_CLIENT, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_KEYDOWN, WM_SETFONT, WS_CHILD, WS_TABSTOP,
};

use super::sidebar::{Field, FIELDS, VK_TAB};
use crate::theme::Rgb;
use crate::uia::PxRect;
use lv_core::command::{VK_ESCAPE, VK_RETURN};

/// Posted by a field for Tab, Shift+Tab, Enter and Esc.
pub const WM_APP_RESIZE_FIELD: u32 = WM_APP + 11;
/// The child-window ids of the fields start here (`WM_COMMAND`'s low word).
const ID_FIRST: usize = 0x4C00;
/// `lparam` bit of a posted key: Shift was down.
pub const SHIFT_BIT: isize = 0x100;
/// The text's size (DIPs) and the inner padding of a field's frame (DIPs).
pub const TEXT_DIP: f32 = 14.0;
const INSET_DIP: f32 = 8.0;

/// What a field is told to show.
#[derive(Clone, Debug, PartialEq)]
pub struct Wanted {
    /// The EDIT's rect in client pixels; `None`: hidden.
    pub rect: Option<PxRect>,
    pub enabled: bool,
    pub text: String,
    pub name: String,
}

struct Edit {
    hwnd: HWND,
    shown: Option<Wanted>,
}

/// The four EDIT controls, their font and the brush behind their text.
pub struct Fields {
    edits: Vec<Edit>,
    font: Option<(f32, HFONT)>,
    brush: Option<(Rgb, HBRUSH)>,
    /// The theme the fields were last drawn in (dark): they repaint when it changes.
    dark: Option<bool>,
}

impl Fields {
    /// Creates the (hidden) fields in `parent`. `None` if one could not be made.
    pub fn create(parent: HWND) -> Option<Fields> {
        let mut edits = Vec::new();
        for (k, f) in FIELDS.iter().enumerate() {
            let number = if *f == Field::MaxSize { 0 } else { ES_NUMBER };
            let style =
                WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32 | number as u32);
            // SAFETY: plain window creation under our own window; the class is the
            // system's EDIT. The subclass procedure is a plain function.
            let hwnd = unsafe {
                let instance = GetModuleHandleW(None).ok()?;
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    PCWSTR::null(),
                    style,
                    0,
                    0,
                    0,
                    0,
                    parent,
                    HMENU((ID_FIRST + k) as *mut _),
                    instance,
                    None,
                )
                .ok()?;
                let _ = SetWindowSubclass(hwnd, Some(subclass), k, 0);
                hwnd
            };
            edits.push(Edit { hwnd, shown: None });
        }
        Some(Fields {
            edits,
            font: None,
            brush: None,
            dark: None,
        })
    }

    /// The field whose child id is `id` (`WM_COMMAND`'s low word).
    pub fn field_of_id(id: usize) -> Option<Field> {
        FIELDS.get(id.checked_sub(ID_FIRST)?).copied()
    }

    pub fn field_of(&self, hwnd: HWND) -> Option<Field> {
        let k = self.edits.iter().position(|e| e.hwnd == hwnd)?;
        Some(FIELDS[k])
    }

    pub fn hwnd(&self, f: Field) -> HWND {
        self.edits[index(f)].hwnd
    }

    /// The field's typed text.
    pub fn text(&self, f: Field) -> String {
        text_of(self.hwnd(f))
    }

    /// Brings the fields to `wanted` (per `FIELDS`): place, show or hide, enable, and
    /// the text (`replace_text[k]`: the field may get new text; never while typing).
    /// `dip` sizes the font; a change of `dark` repaints them in the new colours.
    /// Makes Win32 calls: hold no `App` borrow.
    pub fn apply(&mut self, wanted: &[Wanted], replace_text: &[bool], (dip, dark): (f32, bool)) {
        self.ensure_font(dip);
        if self.dark.replace(dark).is_some_and(|d| d != dark) {
            for e in &self.edits {
                // SAFETY: plain call on our own child window.
                unsafe {
                    let _ = InvalidateRect(e.hwnd, None, true);
                }
            }
        }
        let font = self.font.map(|f| f.1);
        for ((e, want), replace) in self.edits.iter_mut().zip(wanted).zip(replace_text) {
            let old = e.shown.clone();
            // SAFETY: plain calls on our own child window.
            unsafe {
                if let Some(font) = font {
                    if old.is_none() {
                        SendMessageW(e.hwnd, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
                    }
                }
                if old.as_ref().map(|o| &o.name) != Some(&want.name) {
                    set_name(e.hwnd, &want.name);
                }
                if needs_text(*replace, &text_of(e.hwnd), &want.text) {
                    let _ = SetWindowTextW(e.hwnd, &HSTRING::from(want.text.as_str()));
                }
                if old.as_ref().map(|o| o.enabled) != Some(want.enabled) {
                    let _ = EnableWindow(e.hwnd, want.enabled);
                }
                match want.rect {
                    Some(r) => {
                        if old.as_ref().and_then(|o| o.rect) != Some(r) {
                            let _ = SetWindowPos(
                                e.hwnd,
                                None,
                                r.x,
                                r.y,
                                r.w,
                                r.h,
                                SWP_NOZORDER | SWP_NOACTIVATE,
                            );
                            let _ = ShowWindow(e.hwnd, SW_SHOWNA);
                        }
                    }
                    None => {
                        if old.as_ref().is_some_and(|o| o.rect.is_some()) {
                            let _ = ShowWindow(e.hwnd, SW_HIDE);
                        }
                    }
                }
            }
            e.shown = Some(want.clone());
        }
    }

    /// A new font when the DPI changed (and a new `WM_SETFONT` for each field).
    fn ensure_font(&mut self, dip: f32) {
        if self.font.is_some_and(|(d, _)| d == dip) {
            return;
        }
        let mut lf = LOGFONTW {
            lfHeight: -((TEXT_DIP / dip.max(0.1)).round() as i32),
            lfWeight: 400,
            ..Default::default()
        };
        for (d, s) in lf.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *d = s;
        }
        // SAFETY: `lf` is a live LOGFONTW; the old font is deleted only after every
        // field was given the new one.
        unsafe {
            let font = CreateFontIndirectW(&lf);
            if font.is_invalid() {
                return;
            }
            for e in &mut self.edits {
                SendMessageW(e.hwnd, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            }
            if let Some((_, old)) = self.font.replace((dip, font)) {
                let _ = DeleteObject(old);
            }
        }
    }

    /// The brush for `WM_CTLCOLOREDIT`: the field's background, `color`.
    pub fn brush(&mut self, color: Rgb) -> HBRUSH {
        if let Some((c, b)) = self.brush {
            if c == color {
                return b;
            }
        }
        // SAFETY: plain GDI calls; the old brush is no longer handed out.
        unsafe {
            let b = CreateSolidBrush(colorref(color));
            if let Some((_, old)) = self.brush.replace((color, b)) {
                let _ = DeleteObject(old);
            }
            b
        }
    }

    /// The field's height in pixels for the font: one line plus a little room.
    pub fn line_px(dip: f32) -> i32 {
        ((TEXT_DIP * 1.45) / dip.max(0.1)).round() as i32
    }

    /// The EDIT's rect inside a frame drawn at `frame` (pixels): inset, one line tall,
    /// centred vertically.
    pub fn inner(frame: PxRect, dip: f32) -> PxRect {
        let inset = (INSET_DIP / dip.max(0.1)).round() as i32;
        let h = Self::line_px(dip).min(frame.h);
        PxRect {
            x: frame.x + inset,
            y: frame.y + (frame.h - h) / 2,
            w: (frame.w - 2 * inset).max(1),
            h,
        }
    }
}

impl Drop for Fields {
    fn drop(&mut self) {
        // SAFETY: our own child windows and GDI objects, destroyed once.
        unsafe {
            for (k, e) in self.edits.iter().enumerate() {
                let _ = RemoveWindowSubclass(e.hwnd, Some(subclass), k);
                let _ = DestroyWindow(e.hwnd);
            }
            if let Some((_, f)) = self.font.take() {
                let _ = DeleteObject(f);
            }
            if let Some((_, b)) = self.brush.take() {
                let _ = DeleteObject(b);
            }
        }
    }
}

fn index(f: Field) -> usize {
    FIELDS.iter().position(|x| *x == f).unwrap_or(0)
}

/// COLORREF is 0x00BBGGRR.
pub fn colorref(c: Rgb) -> COLORREF {
    COLORREF(u32::from(c.0) | u32::from(c.1) << 8 | u32::from(c.2) << 16)
}

/// Sets the EDIT's accessible name (Dynamic Annotation); a failure is only logged.
fn set_name(hwnd: HWND, name: &str) {
    // SAFETY: COM is initialized on the UI thread (main); plain calls with live
    // arguments.
    let r = unsafe {
        CoCreateInstance::<_, IAccPropServices>(&CLSID_AccPropServices, None, CLSCTX_INPROC_SERVER)
            .and_then(|s| {
                s.SetHwndPropStr(
                    hwnd,
                    OBJID_CLIENT.0 as u32,
                    CHILDID_SELF,
                    PROPID_ACC_NAME,
                    &HSTRING::from(name),
                )
            })
    };
    if let Err(e) = r {
        eprintln!("light-view: could not name a resize field: {e}");
    }
}

/// The fields' subclass: Tab, Enter and Esc go to the main window as
/// `WM_APP_RESIZE_FIELD`; their characters are swallowed (no beep).
unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    let vk = wparam.0 as u16;
    let ours = matches!(vk, VK_TAB | VK_RETURN | VK_ESCAPE);
    match msg {
        WM_KEYDOWN if ours => {
            // SAFETY: plain calls; the parent is our main window.
            unsafe {
                let shift = if GetKeyState(i32::from(VK_SHIFT.0)) < 0 {
                    SHIFT_BIT
                } else {
                    0
                };
                if let Ok(parent) = GetParent(hwnd) {
                    let _ = PostMessageW(
                        parent,
                        WM_APP_RESIZE_FIELD,
                        WPARAM(usize::from(vk)),
                        LPARAM(id as isize | shift),
                    );
                }
            }
            LRESULT(0)
        }
        WM_CHAR if matches!(wparam.0, 0x09 | 0x0D | 0x1B) => LRESULT(0),
        // SAFETY: the default handling with the arguments we were given.
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// A window's text (a field's, by its handle).
pub fn text_of(hwnd: HWND) -> String {
    // SAFETY: plain calls on a window of this thread; `buf` is a live buffer whose
    // length is passed with it.
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let got = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

/// Whether a field gets `want` as its text: only when it may (it is not being typed
/// in) and what it shows differs. Compared with what the field really shows, so an
/// invalid or cleared entry is put back to the value in effect once it is left.
pub fn needs_text(replace: bool, shown: &str, want: &str) -> bool {
    replace && shown != want
}

/// The field a posted key came from and whether Shift was down.
pub fn posted(lparam: LPARAM) -> Option<(Field, bool)> {
    let f = FIELDS.get((lparam.0 & 0xFF) as usize)?;
    Some((*f, lparam.0 & SHIFT_BIT != 0))
}

#[cfg(test)]
mod tests {
    use super::needs_text;

    /// I1: a cleared or invalid entry shows the value in effect again once the field
    /// is left, however often the same value was set before; never while typing.
    #[test]
    fn a_left_field_shows_the_value_in_effect() {
        assert!(needs_text(true, "", "1920"));
        assert!(needs_text(true, "abc", "2"));
        assert!(needs_text(true, "2.0", "2"));
        assert!(!needs_text(true, "1920", "1920"));
        // Being typed in: left alone.
        assert!(!needs_text(false, "", "1920"));
    }
}
