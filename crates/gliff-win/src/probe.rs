//! `--probe-keys`: check keyboard capture without a server. The window
//! opens and takes focus, then a thread types A and B with SendInput.
//! The console shows whether each key reached the hook or only winit.
//! CI runs it on Windows, so capture can be tested on a real desktop.

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
        eprintln!(
            "probe: hook calls with our window active: {}",
            capture::calls()
        );
        // Control: with our window minimised, someone else is in front.
        // SAFETY: ShowWindow on our own window handle; the event loop
        // thread handles the resulting messages.
        unsafe { ShowWindow(capture::our_hwnd(), SW_MINIMIZE) };
        std::thread::sleep(Duration::from_millis(500));
        let before = capture::calls();
        eprintln!("probe: sending vk 0x43 with our window minimised");
        send(0x43, 0x2E, false);
        send(0x43, 0x2E, true);
        std::thread::sleep(Duration::from_millis(500));
        eprintln!(
            "probe: hook calls for C with our window minimised: {}",
            capture::calls() - before
        );
        eprintln!("probe: done");
        let _ = proxy.send_event(Event::Closed);
    });
}

/// The same check with a bare Win32 window and no winit, to tell a winit
/// effect from one of this process (`GLIFF_HOOK=plain`).
pub fn plain() {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GetForegroundWindow, PeekMessageW,
        SetForegroundWindow, TranslateMessage, MSG, PM_REMOVE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    let _hook = match capture::spawn_hook_thread() {
        Ok(hook) => hook,
        Err(code) => {
            eprintln!("probe: cannot install the hook (error {code})");
            return;
        }
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (class, title) = (wide("STATIC"), wide("gliff probe"));
    // SAFETY: a built-in window class and NUL-terminated strings that
    // outlive the call; no parent, menu or creation data.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            400,
            300,
            0,
            0,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if hwnd == 0 {
        eprintln!("probe: cannot create a plain window");
        return;
    }
    capture::set_window(hwnd);
    // SAFETY: our own window handle.
    unsafe { SetForegroundWindow(hwnd) };
    let typist = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(800));
        eprintln!("probe: sending vk 0x41 to the plain window");
        send(0x41, 0x1E, false);
        send(0x41, 0x1E, true);
    });
    let until = std::time::Instant::now() + Duration::from_millis(2000);
    // SAFETY: an all-zero MSG is a valid out value.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while std::time::Instant::now() < until {
        // SAFETY: `msg` is a valid MSG; messages are dispatched as read.
        unsafe {
            while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = typist.join();
    // SAFETY: no preconditions.
    let front = unsafe { GetForegroundWindow() } == hwnd;
    eprintln!(
        "probe: plain window in front: {front}; hook calls: {}",
        capture::calls()
    );
    // SAFETY: our own window, destroyed once.
    unsafe { DestroyWindow(hwnd) };
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
