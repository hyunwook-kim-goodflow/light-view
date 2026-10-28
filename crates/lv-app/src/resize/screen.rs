//! The resize screen in the window (M6 Task 6): its geometry, the sidebar's keys and
//! clicks and the native number fields; its toasts are in `toasts`.
//!
//! Like the rest of `resize`, every function takes the `RefCell<App>` and holds no
//! borrow across Win32 calls that may re-enter the window procedure (SetFocus,
//! SetWindowText, showing a toast).

use std::cell::RefCell;

use lv_core::command::{Command, VK_ESCAPE, VK_RETURN};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{SetBkColor, SetTextColor, HDC};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, IsZoomed, KillTimer, SetTimer, SetWindowPos, EN_CHANGE, EN_KILLFOCUS,
    EN_SETFOCUS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, WM_CAPTURECHANGED, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_SETFOCUS, WM_SYSKEYDOWN, WM_TIMER,
};

use super::draw::{self, Geometry, ScreenView};
use super::fields::{self, Fields, Wanted, WM_APP_RESIZE_FIELD};
use super::layout::MIN_SIDEBAR_H;
use super::sidebar::{
    controls, stage_parts, stage_rect, tab_fields, Action, Ctl, Ctx, Field, Hit, Sidebar, FIELDS,
    VK_TAB,
};
use super::{go_to, leave, saving, set_memory, set_replace, step, ResizeState};
use crate::commands;
use crate::input;
use crate::overlay::Toast;
use crate::strings;
use crate::theme::palette;
use crate::toolbar::Rect;
use crate::uia::PxRect;
use crate::window::{invalidate, App};

/// Typing in a field is taken this long after the last key.
const FIELD_TIMER: usize = 8;
const FIELD_DELAY_MS: u32 = 700;

/// The screen's own state in `ResizeState`.
#[derive(Default)]
pub struct Ui {
    pub sidebar: Sidebar,
    fields: Option<Fields>,
    /// Whether the window's image area was last laid out for the screen.
    laid_out: bool,
}

/// The screen's geometry while it is open.
pub fn geometry_of(a: &App) -> Option<Geometry> {
    let s = a.resize.session.as_ref()?;
    let focus = a.resize.ui.sidebar.focus;
    Some(draw::geometry(
        a.client(),
        a.settings.sidebar_side,
        a.dip,
        s,
        focus,
    ))
}

/// The photo's area while the screen is open (the viewer's view).
pub fn photo_rect(a: &App) -> Option<Rect> {
    let s = a.resize.session.as_ref()?;
    let (w, h) = a.client();
    let view = Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
    };
    let stage = stage_rect(view, a.settings.sidebar_side, a.dip);
    Some(stage_parts(stage, s.items.len(), s.current).photo)
}

fn ctx(a: &App) -> Option<Ctx> {
    let st = &a.resize;
    let s = st.session.as_ref()?;
    Some(Ctx {
        memory: s.memory,
        replace: s.replace,
        blocked: st.replace_block.is_some(),
        saving: st.saving.is_some(),
        count: s.items.len(),
        current: s.current,
    })
}

/// What the screen draws besides the geometry.
pub fn view<'a>(st: &'a ResizeState, dark: bool, icon_family: &'a str) -> Option<ScreenView<'a>> {
    Some(ScreenView {
        session: st.session.as_ref()?,
        block: st.replace_block,
        progress: st.saving.as_ref().map(|p| (p.results.len(), p.total)),
        focus: st.ui.sidebar.focus,
        palette: palette(dark),
        icon_family,
        lang: strings::current(),
    })
}

/// The sidebar control with the keyboard focus for UI Automation: none while an EDIT
/// (a native element of its own) or another window has it.
pub fn uia_focus(a: &App, hwnd: HWND) -> Option<Ctl> {
    // SAFETY: plain call; it only reads this thread's focus.
    let focused = unsafe { GetFocus() };
    (focused == hwnd).then_some(a.resize.ui.sidebar.focus)
}

/// Before each paint: the image area follows the screen opening or closing, and the
/// number fields are created, placed, filled and shown (or destroyed with the screen).
pub fn sync(app: &RefCell<App>, hwnd: HWND) {
    let relayout = app.try_borrow_mut().is_ok_and(|mut a| {
        let open = a.resize.is_open();
        let changed = a.resize.ui.laid_out != open;
        a.resize.ui.laid_out = open;
        if changed {
            // A new session starts on the list.
            a.resize.ui.sidebar = Sidebar::default();
            a.on_size(hwnd);
        }
        changed
    });
    if relayout {
        input::sync_tooltips(app, hwnd);
        grow_to_fit(app, hwnd);
    }
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let Some(g) = geometry_of(&a) else {
        if let Some(f) = a.resize.ui.fields.take() {
            drop(a);
            release_focus(&f, hwnd);
            drop(f);
        }
        return;
    };
    let fields = match a.resize.ui.fields.take() {
        Some(f) => Some(f),
        None => Fields::create(hwnd),
    };
    let Some(mut fields) = fields else {
        return;
    };
    let s = a.resize.session.as_ref().expect("open");
    let m = s.memory;
    let enabled = a.resize.saving.is_none();
    let (dip, dark) = (a.dip, a.dark());
    let lang = strings::current();
    // SAFETY: plain call; it only reads this thread's focus.
    let focused = unsafe { GetFocus() };
    // Windows are drawn over the window's own drawing: under a dialog or the menu the
    // fields hide.
    let covered = a.overlay.dialog_open() || a.menu.is_open();
    let shown = if covered { &[][..] } else { tab_fields(m.tab) };
    let wanted: Vec<Wanted> = FIELDS
        .iter()
        .map(|f| Wanted {
            rect: shown
                .contains(f)
                .then(|| g.layout.field_rect(*f, &m))
                .flatten()
                .map(|r| Fields::inner(PxRect::from_dips(r, dip), dip)),
            enabled,
            text: f.text(&m),
            name: f.uia_name(&m, lang),
        })
        .collect();
    // A field being typed in keeps its text; the others show the settings' value.
    let replace: Vec<bool> = FIELDS.iter().map(|f| fields.hwnd(*f) != focused).collect();
    // A field losing its place (covered, or another tab) gives up the keyboard focus
    // in the model too, so the window's WM_SETFOCUS does not hand it back.
    let losing = FIELDS
        .iter()
        .zip(&wanted)
        .any(|(f, w)| w.rect.is_none() && fields.hwnd(*f) == focused);
    if losing {
        a.resize.ui.sidebar.focus = Ctl::List;
    }
    drop(a);
    // A field that goes away must not keep the focus.
    for (f, w) in FIELDS.iter().zip(&wanted) {
        if w.rect.is_none() && fields.hwnd(*f) == focused {
            // SAFETY: plain call on our own window; no App borrow is held.
            unsafe {
                let _ = SetFocus(hwnd);
            }
        }
    }
    fields.apply(&wanted, &replace, (dip, dark));
    if let Ok(mut a) = app.try_borrow_mut() {
        if a.resize.is_open() {
            a.resize.ui.fields = Some(fields);
        }
    }
}

/// A window opened too short for the sidebar grows to its minimum height (not while
/// maximized or full screen: those keep the screen's size).
fn grow_to_fit(app: &RefCell<App>, hwnd: HWND) {
    let Ok(short) = app
        .try_borrow()
        .map(|a| a.resize.is_open() && a.fullscreen.is_none() && a.client().1 < MIN_SIDEBAR_H)
    else {
        return;
    };
    // SAFETY: plain calls on our own window; no App borrow is held (WM_SIZE re-enters).
    unsafe {
        if !short || IsZoomed(hwnd).as_bool() {
            return;
        }
        let mut rc = RECT::default();
        if GetWindowRect(hwnd, &mut rc).is_err() {
            return;
        }
        if let Some(h) = crate::placement::window_height_for(hwnd, MIN_SIDEBAR_H) {
            let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(hwnd, None, 0, 0, rc.right - rc.left, h, flags);
        }
    }
}

/// The focus comes back to the window before `f` goes.
fn release_focus(f: &Fields, hwnd: HWND) {
    // SAFETY: plain calls on our own windows.
    unsafe {
        let focused = GetFocus();
        if f.field_of(focused).is_some() {
            let _ = SetFocus(hwnd);
        }
    }
}

/// The main window's messages for the screen, seen after the overlay and before the
/// viewer's commands. `None` passes the message on.
pub fn on_message(
    app: &RefCell<App>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    if !super::is_open(app) {
        return None;
    }
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let repeat = lparam.0 & (1 << 30) != 0;
            let action = {
                let mut a = app.try_borrow_mut().ok()?;
                let c = ctx(&a)?;
                let vk = wparam.0 as u16;
                a.resize
                    .ui
                    .sidebar
                    .on_key(vk, commands::mods(), repeat, &c)?
            };
            act(app, hwnd, action);
            Some(LRESULT(0))
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => press(app, hwnd, lparam),
        WM_LBUTTONUP => {
            let action = {
                let mut a = app.try_borrow_mut().ok()?;
                if !a.resize.ui.sidebar.pressing() {
                    return None;
                }
                let hit = hit_at(&a, lparam);
                let c = ctx(&a)?;
                a.resize.ui.sidebar.release(hit, &c)
            };
            // SAFETY: plain call; no App borrow is held (WM_CAPTURECHANGED re-enters).
            unsafe {
                let _ = ReleaseCapture();
            }
            act(app, hwnd, action);
            Some(LRESULT(0))
        }
        WM_CAPTURECHANGED => {
            if let Ok(mut a) = app.try_borrow_mut() {
                a.resize.ui.sidebar.cancel_press();
            }
            None
        }
        WM_COMMAND => on_command(app, hwnd, wparam, lparam),
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let mut a = app.try_borrow_mut().ok()?;
            let p = palette(a.dark());
            let fields = a.resize.ui.fields.as_mut()?;
            fields.field_of(HWND(lparam.0 as *mut _))?;
            let hdc = HDC(wparam.0 as *mut _);
            let ink = if msg == WM_CTLCOLOREDIT {
                p.ink
            } else {
                p.ink2
            };
            // SAFETY: the control's DC, handed to us for this message.
            unsafe {
                SetTextColor(hdc, fields::colorref(ink));
                SetBkColor(hdc, fields::colorref(p.paper));
            }
            Some(LRESULT(fields.brush(p.paper).0 as isize))
        }
        WM_SETFOCUS => {
            // Back to the window: a field that had the focus gets it again.
            let edit = {
                let a = app.try_borrow().ok()?;
                let Ctl::Field(f) = a.resize.ui.sidebar.focus else {
                    return None;
                };
                // Not into a field a dialog or the menu covers.
                if a.overlay.dialog_open() || a.menu.is_open() {
                    return None;
                }
                a.resize.ui.fields.as_ref().map(|x| x.hwnd(f))
            };
            if let Some(edit) = edit {
                // SAFETY: plain call on our own child window; no App borrow is held.
                unsafe {
                    let _ = SetFocus(edit);
                }
            }
            None
        }
        WM_TIMER if wparam.0 == FIELD_TIMER => {
            // SAFETY: plain call on our window's timer.
            unsafe {
                let _ = KillTimer(hwnd, FIELD_TIMER);
            }
            for f in FIELDS {
                commit(app, hwnd, f, false);
            }
            Some(LRESULT(0))
        }
        WM_APP_RESIZE_FIELD => {
            let (f, shift) = fields::posted(lparam)?;
            match wparam.0 as u16 {
                VK_TAB => {
                    commit(app, hwnd, f, true);
                    let action = {
                        let mut a = app.try_borrow_mut().ok()?;
                        let order = controls(&a.resize.session.as_ref()?.memory);
                        let sidebar = &mut a.resize.ui.sidebar;
                        sidebar.focus = Ctl::Field(f);
                        sidebar.tab(shift, &order)
                    };
                    if !matches!(action, Action::FocusField(_)) {
                        // SAFETY: plain call on our window; no App borrow is held.
                        unsafe {
                            let _ = SetFocus(hwnd);
                        }
                    }
                    act(app, hwnd, action);
                }
                VK_RETURN => commit(app, hwnd, f, true),
                VK_ESCAPE => {
                    // SAFETY: plain call on our window; no App borrow is held.
                    unsafe {
                        let _ = SetFocus(hwnd);
                    }
                    commands::run(app, hwnd, Command::Escape, None);
                }
                _ => {}
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// A field's notifications: typing waits for a pause; leaving it takes the value.
fn on_command(app: &RefCell<App>, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let f = Fields::field_of_id(wparam.0 & 0xFFFF)?;
    match ((wparam.0 >> 16) & 0xFFFF) as u32 {
        EN_CHANGE => {
            // Only typing: text set to the settings' value changes nothing.
            let same = app.try_borrow().ok().and_then(|a| {
                let m = a.resize.session.as_ref()?.memory;
                Some(fields::text_of(HWND(lparam.0 as *mut _)) == f.text(&m))
            });
            if same == Some(false) {
                // SAFETY: plain call on our window; restarts the one-shot timer.
                unsafe { SetTimer(hwnd, FIELD_TIMER, FIELD_DELAY_MS, None) };
            }
        }
        EN_KILLFOCUS => {
            commit(app, hwnd, f, true);
            invalidate(hwnd);
        }
        EN_SETFOCUS => {
            if let Ok(mut a) = app.try_borrow_mut() {
                a.resize.ui.sidebar.focus = Ctl::Field(f);
            }
            invalidate(hwnd);
        }
        _ => {}
    }
    Some(LRESULT(0))
}

/// Takes what field `f` holds, if it is a value it accepts and differs from the
/// settings. `done`: the typing is over (the text is shown normalized afterwards).
fn commit(app: &RefCell<App>, hwnd: HWND, f: Field, done: bool) {
    let next = {
        let Ok(a) = app.try_borrow() else {
            return;
        };
        if a.resize.saving.is_some() {
            return;
        }
        let (Some(s), Some(fields)) = (&a.resize.session, &a.resize.ui.fields) else {
            return;
        };
        let typed = fields.text(f);
        f.parse(&typed, &s.memory).filter(|m| *m != s.memory)
    };
    if let Some(m) = next {
        set_memory(app, hwnd, m);
    } else if done {
        invalidate(hwnd);
    }
}

/// The control or stage part under a mouse message's point.
fn hit_at(a: &App, lparam: LPARAM) -> Option<Hit> {
    let (x, y) = a.to_dips(input::point(lparam));
    let g = geometry_of(a)?;
    let c = ctx(a)?;
    if g.layout.panel.contains(x, y) {
        g.layout.hit(x, y, &c, &g.rows)
    } else {
        g.parts.hit(x, y)
    }
}

/// Mouse down: on the sidebar (or the arrows and the filmstrip) the screen takes it;
/// on the photo it goes on to the viewer (pan, double-click zoom). Either way a
/// field loses the focus.
fn press(app: &RefCell<App>, hwnd: HWND, lparam: LPARAM) -> Option<LRESULT> {
    let (taken, field) = {
        let mut a = app.try_borrow_mut().ok()?;
        let (x, y) = a.to_dips(input::point(lparam));
        let g = geometry_of(&a)?;
        let hit = hit_at(&a, lparam);
        let on_sidebar = g.layout.panel.contains(x, y);
        let sidebar = &mut a.resize.ui.sidebar;
        if on_sidebar || hit.is_some() {
            sidebar.press(hit);
        } else if matches!(sidebar.focus, Ctl::Field(_)) {
            // On the photo: the field gives up the focus for good.
            sidebar.focus = Ctl::List;
        }
        let field = match hit {
            Some(Hit::Control(Ctl::Field(f))) => Some(f),
            _ => None,
        };
        (on_sidebar || hit.is_some(), field)
    };
    let target = field.and_then(|f| {
        let a = app.try_borrow().ok()?;
        a.resize.ui.fields.as_ref().map(|x| x.hwnd(f))
    });
    // SAFETY: plain calls on our own windows; no App borrow is held (focus changes
    // send EN_KILLFOCUS and WM_SETFOCUS back to us).
    unsafe {
        let _ = SetFocus(target.unwrap_or(hwnd));
        if taken {
            SetCapture(hwnd);
        }
    }
    invalidate(hwnd);
    taken.then_some(LRESULT(0))
}

/// Carries out a sidebar action. No App borrow is held while it runs.
pub fn act(app: &RefCell<App>, hwnd: HWND, action: Action) {
    let saving = app.try_borrow().map_or(true, |a| a.resize.saving.is_some());
    match action {
        Action::Nothing => {}
        Action::Redraw => invalidate(hwnd),
        Action::Memory(m) if !saving => set_memory(app, hwnd, m),
        Action::Replace(on) if !saving => {
            set_replace(app, hwnd, on);
        }
        Action::Memory(_) | Action::Replace(_) => invalidate(hwnd),
        Action::FocusField(f) => {
            let edit = app.try_borrow().ok().and_then(|a| {
                let fields = a.resize.ui.fields.as_ref()?;
                Some(fields.hwnd(f))
            });
            if let Some(edit) = edit {
                // SAFETY: plain call on our own child window; no App borrow is held.
                unsafe {
                    let _ = SetFocus(edit);
                }
            }
            invalidate(hwnd);
        }
        Action::GoTo(i) => go_to(app, hwnd, i),
        Action::Step(d) => step(app, hwnd, d),
        Action::Leave => leave(app, hwnd),
        Action::Save => save(app, hwnd),
    }
}

/// UI Automation invoked a part of the screen: as a click on it.
pub fn uia_act(app: &RefCell<App>, hwnd: HWND, hit: Hit) {
    let action = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        let Some(c) = ctx(&a) else {
            return;
        };
        a.resize.ui.sidebar.invoke(hit, &c)
    };
    act(app, hwnd, action);
}

/// [저장 (N장)] and Ctrl+S: what is typed counts, then the save starts. A replace's
/// [실행 취소] from before goes (that batch is final now).
pub fn save(app: &RefCell<App>, hwnd: HWND) {
    // SAFETY: plain call on our window's timer.
    unsafe {
        let _ = KillTimer(hwnd, FIELD_TIMER);
    }
    for f in FIELDS {
        commit(app, hwnd, f, true);
    }
    if saving::start_save(app, hwnd) {
        if let Ok(mut a) = app.try_borrow_mut() {
            if a.overlay.toast.as_ref().is_some_and(Toast::undoes_replace) {
                a.overlay.toast = None;
            }
        }
        invalidate(hwnd);
    }
}
