//! winit physical keys and mouse buttons to the evdev codes the protocol
//! carries. winit derives `KeyCode` from the scancode on every platform, so
//! one table serves Windows scancodes and Linux evdev alike; the server's
//! keymap then turns the codes into symbols.

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

/// The evdev `KEY_*` code for a physical key, or `None` for keys with no
/// evdev counterpart.
pub fn evdev_key(code: KeyCode) -> Option<u32> {
    use KeyCode::*;
    Some(match code {
        Escape => 1,
        Digit1 => 2,
        Digit2 => 3,
        Digit3 => 4,
        Digit4 => 5,
        Digit5 => 6,
        Digit6 => 7,
        Digit7 => 8,
        Digit8 => 9,
        Digit9 => 10,
        Digit0 => 11,
        Minus => 12,
        Equal => 13,
        Backspace => 14,
        Tab => 15,
        KeyQ => 16,
        KeyW => 17,
        KeyE => 18,
        KeyR => 19,
        KeyT => 20,
        KeyY => 21,
        KeyU => 22,
        KeyI => 23,
        KeyO => 24,
        KeyP => 25,
        BracketLeft => 26,
        BracketRight => 27,
        Enter => 28,
        ControlLeft => 29,
        KeyA => 30,
        KeyS => 31,
        KeyD => 32,
        KeyF => 33,
        KeyG => 34,
        KeyH => 35,
        KeyJ => 36,
        KeyK => 37,
        KeyL => 38,
        Semicolon => 39,
        Quote => 40,
        Backquote => 41,
        ShiftLeft => 42,
        Backslash => 43,
        KeyZ => 44,
        KeyX => 45,
        KeyC => 46,
        KeyV => 47,
        KeyB => 48,
        KeyN => 49,
        KeyM => 50,
        Comma => 51,
        Period => 52,
        Slash => 53,
        ShiftRight => 54,
        NumpadMultiply => 55,
        AltLeft => 56,
        Space => 57,
        CapsLock => 58,
        F1 => 59,
        F2 => 60,
        F3 => 61,
        F4 => 62,
        F5 => 63,
        F6 => 64,
        F7 => 65,
        F8 => 66,
        F9 => 67,
        F10 => 68,
        NumLock => 69,
        ScrollLock => 70,
        Numpad7 => 71,
        Numpad8 => 72,
        Numpad9 => 73,
        NumpadSubtract => 74,
        Numpad4 => 75,
        Numpad5 => 76,
        Numpad6 => 77,
        NumpadAdd => 78,
        Numpad1 => 79,
        Numpad2 => 80,
        Numpad3 => 81,
        Numpad0 => 82,
        NumpadDecimal => 83,
        Lang5 => 85, // KEY_ZENKAKUHANKAKU
        IntlBackslash => 86,
        F11 => 87,
        F12 => 88,
        IntlRo => 89,
        Lang3 => 90, // KEY_KATAKANA
        Lang4 => 91, // KEY_HIRAGANA
        Convert => 92,
        KanaMode => 93, // KEY_KATAKANAHIRAGANA
        NonConvert => 94,
        NumpadEnter => 96,
        ControlRight => 97,
        NumpadDivide => 98,
        PrintScreen => 99,
        AltRight => 100,
        Home => 102,
        ArrowUp => 103,
        PageUp => 104,
        ArrowLeft => 105,
        ArrowRight => 106,
        End => 107,
        ArrowDown => 108,
        PageDown => 109,
        Insert => 110,
        Delete => 111,
        AudioVolumeMute => 113,
        AudioVolumeDown => 114,
        AudioVolumeUp => 115,
        Power => 116,
        NumpadEqual => 117,
        Pause => 119,
        NumpadComma => 121,
        Lang1 => 122, // KEY_HANGEUL
        Lang2 => 123, // KEY_HANJA
        IntlYen => 124,
        SuperLeft => 125,
        SuperRight => 126,
        ContextMenu => 127, // KEY_COMPOSE, the Menu key
        BrowserStop => 128,
        Again => 129,
        Props => 130,
        Undo => 131,
        Copy => 133,
        Open => 134,
        Paste => 135,
        Find => 136,
        Cut => 137,
        Help => 138,
        LaunchApp2 => 140, // KEY_CALC
        Sleep => 142,
        WakeUp => 143,
        LaunchMail => 155,
        BrowserFavorites => 156,
        LaunchApp1 => 157, // KEY_COMPUTER
        BrowserBack => 158,
        BrowserForward => 159,
        Eject => 161,
        MediaTrackNext => 163,
        MediaPlayPause => 164,
        MediaTrackPrevious => 165,
        MediaStop => 166,
        BrowserHome => 172,
        BrowserRefresh => 173,
        F13 => 183,
        F14 => 184,
        F15 => 185,
        F16 => 186,
        F17 => 187,
        F18 => 188,
        F19 => 189,
        F20 => 190,
        F21 => 191,
        F22 => 192,
        F23 => 193,
        F24 => 194,
        BrowserSearch => 217,
        MediaSelect => 226,
        _ => return None,
    })
}

/// The evdev `BTN_*` code for a mouse button, as the Linux client sends it.
pub fn evdev_button(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0x110,    // BTN_LEFT
        MouseButton::Right => 0x111,   // BTN_RIGHT
        MouseButton::Middle => 0x112,  // BTN_MIDDLE
        MouseButton::Back => 0x116,    // BTN_SIDE
        MouseButton::Forward => 0x115, // BTN_EXTRA
        MouseButton::Other(_) => 0x110,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_editing_keys() {
        assert_eq!(evdev_key(KeyCode::KeyA), Some(30));
        assert_eq!(evdev_key(KeyCode::Enter), Some(28));
        assert_eq!(evdev_key(KeyCode::ArrowLeft), Some(105));
        assert_eq!(evdev_key(KeyCode::SuperLeft), Some(125));
        assert_eq!(evdev_key(KeyCode::Fn), None);
    }

    /// On Linux winit reads physical keys from evdev codes, so its own
    /// conversion checks the table both ways: every key the table maps
    /// comes back as itself, and every evdev code winit knows is mapped.
    #[cfg(target_os = "linux")]
    #[test]
    fn table_matches_winit_evdev_conversion() {
        use winit::keyboard::PhysicalKey;
        use winit::platform::scancode::PhysicalKeyExtScancode;

        for evdev in 0..256u32 {
            let PhysicalKey::Code(code) = PhysicalKey::from_scancode(evdev) else {
                continue;
            };
            let ours =
                evdev_key(code).unwrap_or_else(|| panic!("{code:?} (evdev {evdev}) is not mapped"));
            assert_eq!(
                PhysicalKey::from_scancode(ours),
                PhysicalKey::Code(code),
                "{code:?}: table gives {ours}, winit reads it from {evdev}"
            );
        }
    }
}
