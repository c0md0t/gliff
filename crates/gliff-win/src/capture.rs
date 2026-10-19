//! Keyboard capture on Windows. A low-level keyboard hook sees every key
//! before the shell does, so while our window has the keyboard focus and
//! capture is on, Win, Alt-Tab, Ctrl-Esc and the rest go to the remote
//! instead of Windows. Only Ctrl-Alt-Del and Win-L stay with Windows: no
//! hook can take them.
//!
//! The hook runs on the thread that installed it, which is the event loop
//! thread, while that thread pumps messages. It must return quickly or
//! Windows removes it, so it only posts the key to the event loop.

use std::cell::RefCell;
use std::collections::HashSet;

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, WH_KEYBOARD_LL,
};
use winit::event_loop::EventLoopProxy;

use crate::keys;
use crate::net::Event;

struct State {
    hwnd: HWND,
    proxy: EventLoopProxy<Event>,
    /// Capture is on: not handed back with the release hotkey.
    active: bool,
    /// Our window has the keyboard focus, as winit reports it.
    focused: bool,
    /// Keys whose press the hook took, so it takes their release too, even
    /// after capture ends; a release Windows saw without its press is
    /// harmless, a press without its release would leave the key stuck.
    taken: HashSet<u32>,
    /// The first key press has been reported on the console.
    reported: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The installed hook; dropping it removes the hook.
pub struct Hook(HHOOK);

impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the hook `install` set, removed only here.
        unsafe { UnhookWindowsHookEx(self.0) };
        STATE.with(|s| *s.borrow_mut() = None);
    }
}

/// Capture keys for `hwnd`, posting them to `proxy` as `Event::Key`. Call
/// on the event loop thread. The error is the Windows error code.
pub fn install(hwnd: HWND, proxy: EventLoopProxy<Event>) -> Result<Hook, u32> {
    // SAFETY: GetForegroundWindow has no preconditions.
    let focused = unsafe { GetForegroundWindow() } == hwnd;
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            proxy,
            active: true,
            focused,
            taken: HashSet::new(),
            reported: false,
        })
    });
    // SAFETY: a null name asks for this executable's module handle; the
    // hook procedure lives in it and matches the HOOKPROC signature.
    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(hook_proc),
            GetModuleHandleW(std::ptr::null()),
            0,
        )
    };
    if hook == 0 {
        // SAFETY: GetLastError has no preconditions.
        let error = unsafe { GetLastError() };
        STATE.with(|s| *s.borrow_mut() = None);
        return Err(error);
    }
    Ok(Hook(hook))
}

/// Turn capture on or off. Keys already taken still have their releases
/// taken.
pub fn set_active(active: bool) {
    with_state(|state| state.active = active);
}

/// Follow winit's focus events for the window.
pub fn set_focused(focused: bool) {
    with_state(|state| state.focused = focused);
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            f(state);
        }
    });
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, `lparam` points at the KBDLLHOOKSTRUCT for
        // this event, valid for the duration of the call.
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if take(kb) {
            return 1;
        }
    }
    // SAFETY: passing the event on unchanged, as every hook must.
    unsafe { CallNextHookEx(0, code, wparam, lparam) }
}

/// Decide whether the hook keeps this key from Windows, posting it to the
/// event loop if so.
fn take(kb: &KBDLLHOOKSTRUCT) -> bool {
    // Synthesised keys (winit's focus request, AutoHotkey and the like)
    // belong to whoever sent them.
    if kb.flags & LLKHF_INJECTED != 0 {
        return false;
    }
    let extended = kb.flags & LLKHF_EXTENDED != 0;
    let up = kb.flags & LLKHF_UP != 0;
    // Identifies the physical key across its press and release.
    let id = kb.vkCode << 16 | u32::from(extended) << 8 | (kb.scanCode & 0xFF);
    STATE.with(|s| {
        let Ok(mut guard) = s.try_borrow_mut() else {
            return false;
        };
        let Some(state) = guard.as_mut() else {
            return false;
        };
        if up {
            if !state.taken.remove(&id) {
                return false;
            }
        } else {
            if !state.reported {
                state.reported = true;
                // SAFETY: GetForegroundWindow has no preconditions.
                let front = unsafe { GetForegroundWindow() } == state.hwnd;
                eprintln!(
                    "gliff: keyboard hook saw its first key (focused: {}, in front: {front}, capturing: {})",
                    state.focused, state.active
                );
            }
            if !(state.active && state.focused) {
                return false;
            }
            state.taken.insert(id);
        }
        // While captured, keys with no evdev code are kept from Windows
        // too, and just not forwarded.
        if let Some(keycode) = keys::evdev_from_set1(kb.scanCode, extended, kb.vkCode) {
            let _ = state.proxy.send_event(Event::Key {
                keycode,
                pressed: !up,
            });
        }
        true
    })
}
