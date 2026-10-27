//! UI Automation providers for a window's `uia::Tree` (Task 6): a fragment root that
//! answers `WM_GETOBJECT` (UiaRootObjectId) and one fragment per tree node.
//!
//! UI Automation calls providers on its own threads. They never touch `App`: they read
//! the snapshot the UI thread last published (behind a mutex), and an invocation is
//! posted back to the window as `WM_APP_UIA` with the element's id, so it runs on the
//! UI thread like any other input. Focus and toast events are raised by the UI thread
//! after a paint published a new snapshot, with no `App` borrow held.

// The UIA constants matched below are named as in the Windows SDK.
#![allow(non_upper_case_globals)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::{implement, Error, IUnknown, Interface, Result, BSTR, HRESULT, VARIANT};
use windows::Win32::Foundation::{BOOL, E_OUTOFMEMORY, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement};
use windows::Win32::System::Variant::VT_I4;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::uia::{Act, Announcer, Node, PxRect, Role, Tree};

/// Posted to the window by an element's Invoke, Toggle or Select: wparam is its id.
pub const WM_APP_UIA: u32 = WM_APP + 6;

/// What the providers of one window share with its UI thread.
struct Shared {
    /// The window (as an integer: providers live on other threads).
    hwnd: isize,
    tree: Mutex<Tree>,
    /// False once the window is going away: every call then fails as "not available".
    alive: AtomicBool,
}

fn not_available() -> Error {
    Error::from(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}

/// A null interface result (S_OK with nothing).
fn none<T>() -> Result<T> {
    Err(Error::empty())
}

impl Shared {
    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    fn tree(&self) -> Result<MutexGuard<'_, Tree>> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(not_available());
        }
        Ok(self.tree.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// `f` on the node with `id` and its index; "not available" once it is gone.
    fn node<R>(&self, id: u32, f: impl FnOnce(&Tree, usize, &Node) -> R) -> Result<R> {
        let tree = self.tree()?;
        let i = tree.index(id).ok_or_else(not_available)?;
        Ok(f(&tree, i, &tree.nodes[i]))
    }

    /// The client area's origin on screen.
    fn origin(&self) -> (i32, i32) {
        let mut pt = POINT::default();
        // SAFETY: `pt` is a live local; ClientToScreen only reads the window's position
        // (any thread may call it; a destroyed window just fails).
        unsafe {
            let _ = ClientToScreen(self.hwnd(), &mut pt);
        }
        (pt.x, pt.y)
    }

    /// Asks the UI thread to act on the element `id`, if it is enabled.
    fn post(&self, id: u32) -> Result<()> {
        let enabled = self.node(id, |_, _, n| n.enabled && n.act.is_some())?;
        if !enabled {
            return Err(Error::from(HRESULT(UIA_E_ELEMENTNOTENABLED as i32)));
        }
        // SAFETY: plain call; posting to a window of another thread is allowed.
        unsafe { PostMessageW(self.hwnd(), WM_APP_UIA, WPARAM(id as usize), LPARAM(0)) }
    }

    fn element(self: &Arc<Self>, id: u32) -> IRawElementProviderFragment {
        Element {
            shared: Arc::clone(self),
            id,
        }
        .into()
    }

    fn root(self: &Arc<Self>) -> Root {
        Root {
            shared: Arc::clone(self),
        }
    }

    /// The element at `i`, or nothing.
    fn fragment(
        self: &Arc<Self>,
        tree: &Tree,
        i: Option<usize>,
    ) -> Result<IRawElementProviderFragment> {
        match i {
            Some(i) => Ok(self.element(tree.nodes[i].id)),
            None => none(),
        }
    }
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct Root {
    shared: Arc<Shared>,
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider,
    IToggleProvider,
    ISelectionItemProvider
)]
struct Element {
    shared: Arc<Shared>,
    id: u32,
}

#[allow(non_snake_case)]
impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> Result<IUnknown> {
        none()
    }
    /// The window's own properties (name, control type) come from the host provider.
    fn GetPropertyValue(&self, _: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(VARIANT::default())
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // SAFETY: plain call with our window handle.
        unsafe { UiaHostProviderFromHwnd(self.shared.hwnd()) }
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let tree = self.shared.tree()?;
        let i = match direction {
            NavigateDirection_FirstChild => tree.first_child(None),
            NavigateDirection_LastChild => tree.last_child(None),
            _ => None,
        };
        self.shared.fragment(&tree, i)
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // The host provider gives the window's.
        Ok(std::ptr::null_mut())
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(UiaRect::default())
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.shared.root().into())
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let mut pt = POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        // SAFETY: `pt` is a live local; plain call.
        if !unsafe { ScreenToClient(self.shared.hwnd(), &mut pt) }.as_bool() {
            return none();
        }
        let tree = self.shared.tree()?;
        self.shared.fragment(&tree, tree.node_at(pt.x, pt.y))
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let tree = self.shared.tree()?;
        self.shared.fragment(&tree, tree.focused())
    }
}

fn control_type(role: Role) -> UIA_CONTROLTYPE_ID {
    match role {
        Role::Button => UIA_ButtonControlTypeId,
        Role::CheckBox => UIA_CheckBoxControlTypeId,
        Role::Group => UIA_GroupControlTypeId,
        Role::Image => UIA_ImageControlTypeId,
        Role::List => UIA_ListControlTypeId,
        Role::ListItem => UIA_ListItemControlTypeId,
        Role::Menu => UIA_MenuControlTypeId,
        Role::MenuItem => UIA_MenuItemControlTypeId,
        Role::RadioButton => UIA_RadioButtonControlTypeId,
        Role::Text => UIA_TextControlTypeId,
        Role::ToolBar => UIA_ToolBarControlTypeId,
    }
}

/// Which control patterns a node supports: toggles, selectable items, or else Invoke
/// for anything that acts.
fn supports(n: &Node, pattern: UIA_PATTERN_ID) -> bool {
    match pattern {
        UIA_TogglePatternId => n.toggle.is_some(),
        UIA_SelectionItemPatternId => n.selected.is_some(),
        UIA_InvokePatternId => n.act.is_some() && n.toggle.is_none() && n.selected.is_none(),
        _ => false,
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderSimple_Impl for Element_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        if !self.shared.node(self.id, |_, _, n| supports(n, pattern))? {
            return none();
        }
        Ok(Element {
            shared: Arc::clone(&self.shared),
            id: self.id,
        }
        .into())
    }
    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        self.shared.node(self.id, |_, _, n| match property {
            UIA_ControlTypePropertyId => VARIANT::from(control_type(n.role).0),
            UIA_NamePropertyId => VARIANT::from(BSTR::from(n.name.as_str())),
            UIA_AutomationIdPropertyId => VARIANT::from(BSTR::from(format!("lv{}", n.id))),
            UIA_IsEnabledPropertyId => VARIANT::from(n.enabled),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(n.focusable),
            UIA_HasKeyboardFocusPropertyId => VARIANT::from(n.focused),
            UIA_IsOffscreenPropertyId => VARIANT::from(n.offscreen),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        none()
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragment_Impl for Element_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let tree = self.shared.tree()?;
        let i = tree.index(self.id).ok_or_else(not_available)?;
        let to = match direction {
            NavigateDirection_Parent => match tree.nodes[i].parent {
                Some(p) => Some(p),
                None => return Ok(self.shared.root().into()),
            },
            NavigateDirection_NextSibling => tree.next_sibling(i),
            NavigateDirection_PreviousSibling => tree.prev_sibling(i),
            NavigateDirection_FirstChild => tree.first_child(Some(i)),
            NavigateDirection_LastChild => tree.last_child(Some(i)),
            _ => None,
        };
        self.shared.fragment(&tree, to)
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let ids = [UiaAppendRuntimeId as i32, self.id as i32];
        // SAFETY: a new two-element VT_I4 vector, filled in bounds; UI Automation
        // takes ownership of it.
        unsafe {
            let sa = SafeArrayCreateVector(VT_I4, 0, ids.len() as u32);
            if sa.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            for (k, v) in ids.iter().enumerate() {
                if let Err(e) = SafeArrayPutElement(sa, &(k as i32), std::ptr::from_ref(v).cast()) {
                    let _ = SafeArrayDestroy(sa);
                    return Err(e);
                }
            }
            Ok(sa)
        }
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let (r, offscreen) = self.shared.node(self.id, |_, _, n| (n.rect, n.offscreen))?;
        if offscreen {
            return Ok(UiaRect::default());
        }
        let PxRect { x, y, w, h } = r.on_screen(self.shared.origin());
        Ok(UiaRect {
            left: f64::from(x),
            top: f64::from(y),
            width: f64::from(w),
            height: f64::from(h),
        })
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    /// Keyboard focus moves only with the window's own keys.
    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.shared.root().into())
    }
}

#[allow(non_snake_case)]
impl IInvokeProvider_Impl for Element_Impl {
    fn Invoke(&self) -> Result<()> {
        self.shared.post(self.id)
    }
}

#[allow(non_snake_case)]
impl IToggleProvider_Impl for Element_Impl {
    fn Toggle(&self) -> Result<()> {
        self.shared.post(self.id)
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        let on = self
            .shared
            .node(self.id, |_, _, n| n.toggle == Some(true))?;
        Ok(if on { ToggleState_On } else { ToggleState_Off })
    }
}

#[allow(non_snake_case)]
impl ISelectionItemProvider_Impl for Element_Impl {
    fn Select(&self) -> Result<()> {
        self.shared.post(self.id)
    }
    fn AddToSelection(&self) -> Result<()> {
        self.shared.post(self.id)
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Err(Error::from(HRESULT(UIA_E_INVALIDOPERATION as i32)))
    }
    fn IsSelected(&self) -> Result<BOOL> {
        Ok(self
            .shared
            .node(self.id, |_, _, n| n.selected == Some(true))?
            .into())
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        let parent = self
            .shared
            .node(self.id, |t, _, n| n.parent.map(|p| t.nodes[p].id))?;
        match parent {
            Some(id) => self.shared.element(id).cast(),
            None => Ok(self.shared.root().into()),
        }
    }
}

// ---- UI thread -----------------------------------------------------------------

/// A window's providers, kept on its UI thread once UI Automation asked for them.
struct Host {
    hwnd: HWND,
    shared: Arc<Shared>,
    root: IRawElementProviderSimple,
    announcer: Announcer,
    /// Events found by the last publish, raised by `flush`.
    focus: Option<u32>,
    toast: Option<String>,
}

thread_local! {
    static HOSTS: RefCell<Vec<Host>> = const { RefCell::new(Vec::new()) };
}

fn with_host<R>(hwnd: HWND, f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    HOSTS.with(|h| h.borrow_mut().iter_mut().find(|h| h.hwnd == hwnd).map(f))
}

/// `WM_GETOBJECT`: for UiaRootObjectId, the window's root provider (made on the first
/// request, from `build`'s tree). `None` passes other requests to `DefWindowProc`.
pub fn get_object(
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
    build: impl FnOnce() -> Option<Tree>,
) -> Option<LRESULT> {
    if lparam.0 as i32 != UiaRootObjectId {
        return None;
    }
    let root = match with_host(hwnd, |h| h.root.clone()) {
        Some(root) => root,
        None => {
            let tree = build()?;
            let shared = Arc::new(Shared {
                hwnd: hwnd.0 as isize,
                tree: Mutex::new(Tree::default()),
                alive: AtomicBool::new(true),
            });
            let announcer = Announcer::primed(&tree);
            *shared.tree.lock().unwrap_or_else(|e| e.into_inner()) = tree;
            let root: IRawElementProviderSimple = shared.root().into();
            HOSTS.with(|h| {
                h.borrow_mut().push(Host {
                    hwnd,
                    shared,
                    root: root.clone(),
                    announcer,
                    focus: None,
                    toast: None,
                });
            });
            root
        }
    };
    // SAFETY: plain call with the message's own arguments and our provider.
    Some(unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &root) })
}

/// Replaces the window's snapshot with `build`'s tree and notes the focus and toast
/// events it brings. Does nothing (and builds nothing) until UI Automation asked for
/// the window's providers.
pub fn publish(hwnd: HWND, build: impl FnOnce() -> Tree) {
    let Some(shared) = with_host(hwnd, |h| Arc::clone(&h.shared)) else {
        return;
    };
    let tree = build();
    with_host(hwnd, |h| {
        let (focus, toast) = h.announcer.update(&tree);
        h.focus = focus.or(h.focus.take());
        h.toast = toast.or(h.toast.take());
    });
    *shared.tree.lock().unwrap_or_else(|e| e.into_inner()) = tree;
}

/// Raises the events the last publish found: focus moved (while the window has the
/// keyboard focus) and a new toast. Call with no `App` borrow held: UI Automation may
/// call back into the providers.
pub fn flush(hwnd: HWND) {
    let Some((shared, root, focus, toast)) = with_host(hwnd, |h| {
        (
            Arc::clone(&h.shared),
            h.root.clone(),
            h.focus.take(),
            h.toast.take(),
        )
    }) else {
        return;
    };
    // SAFETY: plain calls; the providers are ours and stay alive for the call.
    unsafe {
        if !UiaClientsAreListening().as_bool() {
            return;
        }
        if let Some(id) = focus.filter(|_| GetFocus() == hwnd) {
            if let Ok(el) = shared.element(id).cast::<IRawElementProviderSimple>() {
                let _ = UiaRaiseAutomationEvent(&el, UIA_AutomationFocusChangedEventId);
            }
        }
        if let Some(text) = toast {
            let _ = UiaRaiseNotificationEvent(
                &root,
                NotificationKind_ActionCompleted,
                NotificationProcessing_ImportantMostRecent,
                &BSTR::from(text),
                &BSTR::from("light-view.toast"),
            );
        }
    }
}

/// `WM_APP_UIA`: what the element `id` (wparam) does, if it is still there and enabled.
pub fn action(hwnd: HWND, wparam: WPARAM) -> Option<Act> {
    let shared = with_host(hwnd, |h| Arc::clone(&h.shared))?;
    let id = u32::try_from(wparam.0).ok()?;
    shared
        .node(id, |_, _, n| n.act.filter(|_| n.enabled))
        .ok()
        .flatten()
}

/// `WM_DESTROY`: the providers stop answering and UI Automation lets go of them.
pub fn detach(hwnd: HWND) {
    let host = HOSTS.with(|h| {
        let mut hosts = h.borrow_mut();
        let i = hosts.iter().position(|h| h.hwnd == hwnd)?;
        Some(hosts.swap_remove(i))
    });
    let Some(host) = host else {
        return;
    };
    host.shared.alive.store(false, Ordering::Release);
    // SAFETY: plain calls; a null provider tells UI Automation the window is gone.
    unsafe {
        let _ = UiaReturnRawElementProvider(
            hwnd,
            WPARAM(0),
            LPARAM(0),
            None::<&IRawElementProviderSimple>,
        );
        let _ = UiaDisconnectProvider(&host.root);
    }
}
