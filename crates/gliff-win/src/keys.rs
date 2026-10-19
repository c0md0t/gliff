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

pub const KEY_ESC: u32 = 1;
pub const KEY_ENTER: u32 = 28;
pub const KEY_LEFTCTRL: u32 = 29;
pub const KEY_LEFTSHIFT: u32 = 42;
pub const KEY_RIGHTSHIFT: u32 = 54;
pub const KEY_LEFTALT: u32 = 56;
pub const KEY_RIGHTCTRL: u32 = 97;
pub const KEY_RIGHTALT: u32 = 100;

#[cfg_attr(not(windows), allow(dead_code))]
const VK_PAUSE: u32 = 0x13;
#[cfg_attr(not(windows), allow(dead_code))]
const VK_NUMLOCK: u32 = 0x90;

/// The evdev code for a Windows set-1 scancode, as a low-level keyboard
/// hook reports it: `scan`, whether the `0xE0` prefix was set, and the
/// virtual key. Pause and Num Lock share scancode 0x45 and are told apart
/// by the virtual key. `None` for codes with no key, such as the fake
/// shifts Windows wraps around some extended keys.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn evdev_from_set1(scan: u32, extended: bool, vk: u32) -> Option<u32> {
    match vk {
        VK_PAUSE => return Some(119),
        VK_NUMLOCK => return Some(69),
        _ => {}
    }
    if extended {
        return Some(match scan {
            0x10 => 165, // KEY_PREVIOUSSONG
            0x19 => 163, // KEY_NEXTSONG
            0x1C => 96,  // KEY_KPENTER
            0x1D => KEY_RIGHTCTRL,
            0x20 => 113, // KEY_MUTE
            0x21 => 140, // KEY_CALC
            0x22 => 164, // KEY_PLAYPAUSE
            0x24 => 166, // KEY_STOPCD
            0x2E => 114, // KEY_VOLUMEDOWN
            0x30 => 115, // KEY_VOLUMEUP
            0x32 => 172, // KEY_HOMEPAGE
            0x35 => 98,  // KEY_KPSLASH
            // Right Shift sometimes arrives flagged extended.
            0x36 => KEY_RIGHTSHIFT,
            0x37 => 99, // KEY_SYSRQ (Print Screen)
            0x38 => KEY_RIGHTALT,
            0x47 => 102, // KEY_HOME
            0x48 => 103, // KEY_UP
            0x49 => 104, // KEY_PAGEUP
            0x4B => 105, // KEY_LEFT
            0x4D => 106, // KEY_RIGHT
            0x4F => 107, // KEY_END
            0x50 => 108, // KEY_DOWN
            0x51 => 109, // KEY_PAGEDOWN
            0x52 => 110, // KEY_INSERT
            0x53 => 111, // KEY_DELETE
            0x5B => 125, // KEY_LEFTMETA
            0x5C => 126, // KEY_RIGHTMETA
            0x5D => 127, // KEY_COMPOSE (Menu)
            0x5E => 116, // KEY_POWER
            0x5F => 142, // KEY_SLEEP
            0x63 => 143, // KEY_WAKEUP
            0x65 => 217, // KEY_SEARCH
            0x66 => 156, // KEY_BOOKMARKS
            0x67 => 173, // KEY_REFRESH
            0x68 => 128, // KEY_STOP
            0x69 => 159, // KEY_FORWARD
            0x6A => 158, // KEY_BACK
            0x6B => 157, // KEY_COMPUTER
            0x6C => 155, // KEY_MAIL
            0x6D => 226, // KEY_MEDIA
            _ => return None,
        });
    }
    Some(match scan {
        // Escape through keypad '.' share their numbers with evdev.
        0x01..=0x53 => scan,
        0x54 => 99,                       // Alt+Print Screen
        0x56 => 86,                       // KEY_102ND
        0x57 => 87,                       // KEY_F11
        0x58 => 88,                       // KEY_F12
        0x59 => 117,                      // KEY_KPEQUAL
        0x64..=0x6E => scan - 0x64 + 183, // KEY_F13..KEY_F23
        0x70 => 93,                       // KEY_KATAKANAHIRAGANA
        0x73 => 89,                       // KEY_RO
        0x76 => 194,                      // KEY_F24
        0x79 => 92,                       // KEY_HENKAN
        0x7B => 94,                       // KEY_MUHENKAN
        0x7D => 124,                      // KEY_YEN
        0x7E => 121,                      // KEY_KPCOMMA
        0xF1 => 123,                      // KEY_HANJA
        0xF2 => 122,                      // KEY_HANGEUL
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set1_scancodes() {
        assert_eq!(evdev_from_set1(0x1E, false, 0x41), Some(30)); // A
        assert_eq!(evdev_from_set1(0x1C, false, 0x0D), Some(KEY_ENTER));
        assert_eq!(evdev_from_set1(0x1C, true, 0x0D), Some(96)); // keypad Enter
        assert_eq!(evdev_from_set1(0x5B, true, 0x5B), Some(125)); // left Win
        assert_eq!(evdev_from_set1(0x38, true, 0xA5), Some(KEY_RIGHTALT));
        assert_eq!(evdev_from_set1(0x4B, true, 0x25), Some(105)); // Left arrow
        assert_eq!(evdev_from_set1(0x4B, false, 0x64), Some(75)); // keypad 4
        assert_eq!(evdev_from_set1(0x45, false, VK_PAUSE), Some(119));
        assert_eq!(evdev_from_set1(0x45, true, VK_NUMLOCK), Some(69));
        assert_eq!(evdev_from_set1(0x57, false, 0x7A), Some(87)); // F11
        assert_eq!(evdev_from_set1(0x2A, true, 0xFF), None); // fake shift
    }

    /// The set-1 table and the KeyCode table agree, checked against
    /// winit's own reading of Windows scancodes (0xE0 prefix as 0xE000).
    #[cfg(windows)]
    #[test]
    fn set1_table_matches_winit_windows_conversion() {
        use winit::keyboard::PhysicalKey;
        use winit::platform::scancode::PhysicalKeyExtScancode;

        for extended in [false, true] {
            for scan in 1..0x100u32 {
                // Pause/Num Lock depend on the virtual key; checked above.
                if scan == 0x45 {
                    continue;
                }
                let raw = if extended { 0xE000 | scan } else { scan };
                let PhysicalKey::Code(code) = PhysicalKey::from_scancode(raw) else {
                    continue;
                };
                let Some(want) = evdev_key(code) else {
                    continue;
                };
                assert_eq!(
                    evdev_from_set1(scan, extended, 0),
                    Some(want),
                    "{code:?} (scancode {raw:#x})"
                );
            }
        }
    }

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
