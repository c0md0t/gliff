//! `--probe-keys`: check keyboard capture without a server. The window
//! opens and takes focus, then a thread types A and B with SendInput, and
//! C with the window minimised. It fails unless the hook saw every key.
//! CI runs it on Windows, so capture is tested on a real desktop.

use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_MINIMIZE};
use winit::event_loop::EventLoopProxy;

use crate::capture;
use crate::net::Event;

/// Virtual key and set-1 scancode of each key the probe types.
const KEYS: &[(u16, u16)] = &[(0x41, 0x1E), (0x42, 0x30)];

/// Type the probe keys after the window has had time to come up, then end
/// the event loop.
pub fn start(proxy: EventLoopProxy<Event>) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        for &(vk, scan) in KEYS {
            for up in [false, true] {
                eprintln!(
                    "probe: sending vk {vk:#x} {}",
                    if up { "up" } else { "down" }
                );
                send(vk, scan, up);
                std::thread::sleep(Duration::from_millis(150));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        let active = capture::calls();
        // Control: with our window minimised, someone else is in front, and
        // the hook must see keys whatever happens to ours.
        // SAFETY: ShowWindow on our own window handle; the event loop
        // thread handles the resulting messages.
        unsafe { ShowWindow(capture::our_hwnd(), SW_MINIMIZE) };
        std::thread::sleep(Duration::from_millis(500));
        send(0x43, 0x2E, false);
        send(0x43, 0x2E, true);
        std::thread::sleep(Duration::from_millis(500));
        let minimised = capture::calls() - active;
        eprintln!("probe: hook calls with our window active {active}, minimised {minimised}");
        // Each key is a press and a release.
        let want = KEYS.len() as u32 * 2;
        if active < want || minimised < 2 {
            eprintln!("probe: FAILED: the hook must see every key, ours in front or not");
            std::process::exit(1);
        }
        eprintln!("probe: ok");
        let _ = proxy.send_event(Event::Closed);
    });
}

fn send(vk: u16, scan: u16, up: bool) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one valid INPUT, with its size.
    let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        eprintln!("probe: SendInput failed");
    }
}
