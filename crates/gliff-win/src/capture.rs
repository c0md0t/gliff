//! Keyboard capture on Windows. A low-level keyboard hook sees every key
//! before the shell does, so while our window is in front and capture is
//! on, Win, Alt-Tab, Ctrl-Esc and the rest go to the remote instead of
//! Windows. Only Ctrl-Alt-Del and Win-L stay with Windows: no hook can
//! take them.
//!
//! The hook runs on the thread that installed it, which is the event loop
//! thread, while that thread pumps messages. It must return quickly or
//! Windows removes it, so it only posts the key to the event loop.

use std::cell::RefCell;
use std::collections::HashSet;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, WH_KEYBOARD_LL,
};
use winit::event_loop::EventLoopProxy;

use crate::keys;
use crate::net::Event;

struct State {
    hwnd: HWND,
    proxy: EventLoopProxy<Event>,
    /// Capture is on: not handed back with the release hotkey.
    active: bool,
    /// Keys whose press the hook took, so it takes their release too, even
    /// after capture ends; a release Windows saw without its press is
    /// harmless, a press without its release would leave the key stuck.
    taken: HashSet<u32>,
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
/// on the event loop thread.
pub fn install(hwnd: HWND, proxy: EventLoopProxy<Event>) -> Option<Hook> {
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            proxy,
            active: true,
            taken: HashSet::new(),
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
        STATE.with(|s| *s.borrow_mut() = None);
        return None;
    }
    Some(Hook(hook))
}

/// Turn capture on or off. Keys already taken still have their releases
/// taken.
pub fn set_active(active: bool) {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.active = active;
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
            // SAFETY: GetForegroundWindow has no preconditions.
            let front = unsafe { GetForegroundWindow() } == state.hwnd;
            if !(state.active && front) {
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
