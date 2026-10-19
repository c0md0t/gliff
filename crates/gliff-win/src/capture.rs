//! Keyboard capture on Windows. A low-level keyboard hook sees every key
//! before the shell does, so while our window is in front and capture is
//! on, Win, Alt-Tab, Ctrl-Esc and the rest go to the remote instead of
//! Windows. Only Ctrl-Alt-Del and Win-L stay with Windows: no hook can
//! take them.
//!
//! The hook gets its own thread and message loop. Windows waits only a
//! short time for a low-level hook before passing the key on (and drops a
//! hook that keeps missing it), so it must not share a thread with
//! drawing. "In front" is asked of Windows on every key: the foreground
//! window belongs to this process, whose only window is ours (the console
//! belongs to the terminal).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, MSG, PM_NOREMOVE,
    WH_KEYBOARD_LL, WM_QUIT, WM_USER,
};
use winit::event_loop::EventLoopProxy;

use crate::keys;
use crate::net::Event;

/// Capture is on: not handed back with the release hotkey.
static ACTIVE: AtomicBool = AtomicBool::new(true);
static PROXY: OnceLock<EventLoopProxy<Event>> = OnceLock::new();

/// Key presses reported on the console, for diagnosing capture.
const REPORT_PRESSES: u32 = 3;

thread_local! {
    /// Keys whose press the hook took, so it takes their release too, even
    /// after capture ends; a release Windows saw without its press is
    /// harmless, a press without its release would leave the key stuck.
    /// Only the hook thread touches it.
    static TAKEN: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static REPORTED: Cell<u32> = const { Cell::new(0) };
}

/// The hook thread; dropping it removes the hook.
pub struct Hook {
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to a thread id has no memory effects; the
        // thread's loop ends and it removes its hook.
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Capture keys on a thread of their own, posting them to `proxy` as
/// `Event::Key`. The error is the Windows error code.
pub fn install(proxy: EventLoopProxy<Event>) -> Result<Hook, u32> {
    let _ = PROXY.set(proxy);
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("gliff-keys".into())
        .spawn(move || hook_thread(tx))
        .map_err(|_| 0u32)?;
    match rx.recv() {
        Ok(Ok(thread_id)) => Ok(Hook {
            thread_id,
            thread: Some(thread),
        }),
        Ok(Err(code)) => Err(code),
        Err(_) => Err(0),
    }
}

fn hook_thread(ready: std::sync::mpsc::Sender<Result<u32, u32>>) {
    // SAFETY: an all-zero MSG is a valid value for these out-parameters.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: PeekMessage with a valid MSG creates this thread's message
    // queue, so the WM_QUIT posted on drop cannot be lost.
    unsafe { PeekMessageW(&mut msg, 0, WM_USER, WM_USER, PM_NOREMOVE) };
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
        let _ = ready.send(Err(unsafe { GetLastError() }));
        return;
    }
    // SAFETY: GetCurrentThreadId has no preconditions.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    // The hook procedure runs inside this loop's GetMessage.
    // SAFETY: `msg` is a valid MSG; a null window takes this thread's
    // messages, and each is translated and dispatched as received.
    unsafe {
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(hook);
    }
}

/// Turn capture on or off. Keys already taken still have their releases
/// taken.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

/// The foreground window belongs to this process.
fn ours_in_front() -> bool {
    // SAFETY: these calls have no preconditions; `pid` is a valid out
    // pointer for the duration of the call.
    unsafe {
        let front = GetForegroundWindow();
        if front == 0 {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(front, &mut pid);
        pid == GetCurrentProcessId()
    }
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
    if up {
        if !TAKEN.with(|t| t.borrow_mut().remove(&id)) {
            return false;
        }
    } else {
        let front = ours_in_front();
        let active = ACTIVE.load(Ordering::Relaxed);
        let n = REPORTED.get();
        if n < REPORT_PRESSES {
            REPORTED.set(n + 1);
            eprintln!(
                "gliff: key {:#x} pressed (gliff in front: {front}, capturing: {active})",
                kb.vkCode
            );
        }
        if !(front && active) {
            return false;
        }
        TAKEN.with(|t| t.borrow_mut().insert(id));
    }
    // While captured, keys with no evdev code are kept from Windows too,
    // and just not forwarded.
    if let (Some(keycode), Some(proxy)) = (
        keys::evdev_from_set1(kb.scanCode, extended, kb.vkCode),
        PROXY.get(),
    ) {
        let _ = proxy.send_event(Event::Key {
            keycode,
            pressed: !up,
        });
    }
    true
}
