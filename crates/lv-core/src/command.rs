//! Key and wheel input mapping to viewer commands (spec §4.4).
//! Pure: no Win32 calls. Virtual-key codes are the Win32 `VK_*` values.

pub const VK_RETURN: u16 = 0x0D;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_LEFT: u16 = 0x25;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DELETE: u16 = 0x2E;
pub const VK_F11: u16 = 0x7A;
pub const VK_ADD: u16 = 0x6B;
pub const VK_SUBTRACT: u16 = 0x6D;
pub const VK_OEM_PLUS: u16 = 0xBB;
pub const VK_OEM_COMMA: u16 = 0xBC;
pub const VK_OEM_MINUS: u16 = 0xBD;
pub const VK_KEY_I: u16 = 0x49;
pub const VK_KEY_O: u16 = 0x4F;
pub const VK_KEY_S: u16 = 0x53;
pub const VK_KEY_Z: u16 = 0x5A;

/// Viewer commands triggered by keys or the mouse wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Next,
    Prev,
    ZoomIn,
    ZoomOut,
    ToggleFitActual,
    RotateLeft,
    RotateRight,
    Save,
    Delete,
    Undo,
    ToggleExif,
    ToggleFullscreen,
    Escape,
    Open,
    Settings,
    /// The menu's 크기 조정 (M6): the resize screen with the current photo. No key.
    Resize,
}

/// Modifier state at the time of the input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Maps a virtual-key code plus modifiers to a command. Any Alt combination
/// returns `None` so system shortcuts are left alone.
pub fn command_for_key(vk: u16, mods: Mods) -> Option<Command> {
    if mods.alt {
        return None;
    }
    if mods.ctrl {
        return match vk {
            VK_LEFT => Some(Command::RotateLeft),
            VK_RIGHT => Some(Command::RotateRight),
            VK_KEY_S => Some(Command::Save),
            VK_KEY_Z => Some(Command::Undo),
            VK_KEY_O => Some(Command::Open),
            VK_OEM_COMMA => Some(Command::Settings),
            _ => None,
        };
    }
    // Shift is deliberately ignored: `+` is Shift+= on many layouts.
    match vk {
        VK_LEFT => Some(Command::Prev),
        VK_RIGHT => Some(Command::Next),
        VK_OEM_PLUS | VK_ADD => Some(Command::ZoomIn),
        VK_OEM_MINUS | VK_SUBTRACT => Some(Command::ZoomOut),
        VK_DELETE => Some(Command::Delete),
        VK_KEY_I => Some(Command::ToggleExif),
        VK_F11 | VK_RETURN => Some(Command::ToggleFullscreen),
        VK_ESCAPE => Some(Command::Escape),
        _ => None,
    }
}

/// Maps a mouse wheel delta to a command. Positive delta is wheel-up;
/// zero is treated as wheel-down.
pub fn command_for_wheel(delta: i16, mods: Mods) -> Command {
    match (delta > 0, mods.ctrl) {
        (true, true) => Command::ZoomIn,
        (true, false) => Command::Prev,
        (false, true) => Command::ZoomOut,
        (false, false) => Command::Next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
    };
    const CTRL: Mods = Mods {
        ctrl: true,
        shift: false,
        alt: false,
    };

    #[test]
    fn key_table_maps_each_row() {
        let cases: &[(u16, Mods, Command)] = &[
            (VK_LEFT, NONE, Command::Prev),
            (VK_RIGHT, NONE, Command::Next),
            (VK_LEFT, CTRL, Command::RotateLeft),
            (VK_RIGHT, CTRL, Command::RotateRight),
            (VK_OEM_PLUS, NONE, Command::ZoomIn),
            (VK_ADD, NONE, Command::ZoomIn),
            (VK_OEM_MINUS, NONE, Command::ZoomOut),
            (VK_SUBTRACT, NONE, Command::ZoomOut),
            (VK_KEY_S, CTRL, Command::Save),
            (VK_DELETE, NONE, Command::Delete),
            (VK_KEY_Z, CTRL, Command::Undo),
            (VK_KEY_I, NONE, Command::ToggleExif),
            (VK_F11, NONE, Command::ToggleFullscreen),
            (VK_RETURN, NONE, Command::ToggleFullscreen),
            (VK_ESCAPE, NONE, Command::Escape),
            (VK_KEY_O, CTRL, Command::Open),
            (VK_OEM_COMMA, CTRL, Command::Settings),
        ];
        for &(vk, mods, cmd) in cases {
            assert_eq!(
                command_for_key(vk, mods),
                Some(cmd),
                "vk={vk:#x} mods={mods:?}"
            );
        }
    }

    #[test]
    fn alt_combinations_are_unmapped() {
        let alt = Mods {
            alt: true,
            ..Default::default()
        };
        assert_eq!(command_for_key(VK_LEFT, alt), None);
        assert_eq!(
            command_for_key(
                VK_KEY_S,
                Mods {
                    ctrl: true,
                    alt: true,
                    ..Default::default()
                }
            ),
            None
        );
    }

    #[test]
    fn ctrl_prefixed_keys_require_ctrl() {
        assert_eq!(command_for_key(VK_KEY_S, NONE), None);
        assert_eq!(command_for_key(VK_KEY_Z, NONE), None);
        assert_eq!(command_for_key(VK_KEY_O, NONE), None);
        assert_eq!(command_for_key(VK_OEM_COMMA, NONE), None);
        assert_eq!(command_for_key(VK_LEFT, CTRL), Some(Command::RotateLeft));
    }

    #[test]
    fn plain_keys_reject_ctrl() {
        // Plain keys map only when Ctrl is not held.
        assert_eq!(command_for_key(VK_KEY_I, CTRL), None);
        assert_eq!(command_for_key(VK_F11, CTRL), None);
        assert_eq!(command_for_key(VK_RETURN, CTRL), None);
        assert_eq!(command_for_key(VK_OEM_PLUS, CTRL), None);
        assert_eq!(command_for_key(VK_OEM_MINUS, CTRL), None);
    }

    #[test]
    fn shift_is_ignored_for_zoom_keys() {
        let shift = Mods {
            shift: true,
            ..Default::default()
        };
        assert_eq!(command_for_key(VK_OEM_PLUS, shift), Some(Command::ZoomIn));
        assert_eq!(command_for_key(VK_OEM_MINUS, shift), Some(Command::ZoomOut));
    }

    #[test]
    fn resize_has_no_shortcut() {
        // 크기 조정 comes only from the menus (spec §3): no key or wheel maps to it.
        for vk in 0..=u16::from(u8::MAX) {
            for mods in [NONE, CTRL] {
                assert_ne!(command_for_key(vk, mods), Some(Command::Resize), "{vk:#x}");
            }
        }
        for delta in [-120, 0, 120] {
            assert_ne!(command_for_wheel(delta, NONE), Command::Resize);
            assert_ne!(command_for_wheel(delta, CTRL), Command::Resize);
        }
    }

    #[test]
    fn unknown_key_is_unmapped() {
        assert_eq!(command_for_key(0x41, NONE), None); // 'A'
    }

    #[test]
    fn wheel_up_prev_ctrl_wheel_zoom() {
        assert_eq!(command_for_wheel(120, NONE), Command::Prev);
        assert_eq!(command_for_wheel(120, CTRL), Command::ZoomIn);
    }

    #[test]
    fn wheel_down_next_ctrl_wheel_zoom() {
        assert_eq!(command_for_wheel(-120, NONE), Command::Next);
        assert_eq!(command_for_wheel(-120, CTRL), Command::ZoomOut);
    }
}
