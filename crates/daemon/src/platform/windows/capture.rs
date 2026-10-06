//! Capturing the host's keyboard and mouse with low-level hooks.
//!
//! A dedicated thread installs `WH_KEYBOARD_LL` and `WH_MOUSE_LL` and pumps
//! messages; Windows calls the hook procedures on that thread for every input
//! event, system-wide, and lets them swallow it.
//!
//! While a client has the input, the real cursor is parked: every mouse move
//! is measured as an offset from the parking spot, forwarded, and the cursor
//! put back, so it can never run into a screen edge.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, KillTimer,
    PostThreadMessageW, SetCursorPos, SetTimer, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP,
    LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SM_CXSCREEN, SM_CYSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WM_XBUTTONDOWN, WM_XBUTTONUP,
};
use yzendris_core::keymap;
use yzendris_core::proto::button;
use yzendris_core::Point;

use crate::platform::Sink;

/// Input currently belongs to a client.
static CAPTURED: AtomicBool = AtomicBool::new(false);
/// Where the cursor is parked while captured.
static PARK_X: AtomicI32 = AtomicI32::new(0);
static PARK_Y: AtomicI32 = AtomicI32::new(0);
/// Last local cursor position seen, for computing motion direction.
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// Set on the hook thread only; the hook procedures run there.
    static SINK: RefCell<Option<Arc<dyn Sink>>> = const { RefCell::new(None) };
    /// Virtual keys we believe are down, to notice releases we never saw.
    static HELD: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// How often to check for keys whose release was missed (ms).
const STALE_CHECK_MS: u32 = 2000;

pub struct Capture {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    pub fn start(sink: Arc<dyn Sink>) -> Result<Self> {
        if HOOK_THREAD.load(Relaxed) != 0 {
            bail!("input capture is already running");
        }
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("input-hooks".into())
            .spawn(move || hook_thread(sink, ready_tx))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
            }),
            Ok(Err(e)) => bail!("could not install input hooks: {e}"),
            Err(_) => bail!("the input hook thread exited unexpectedly"),
        }
    }

    pub fn set_capture(&self, on: bool) {
        if on {
            // SAFETY: plain calls without pointer arguments.
            let (x, y) = unsafe {
                (
                    GetSystemMetrics(SM_CXSCREEN) / 2,
                    GetSystemMetrics(SM_CYSCREEN) / 2,
                )
            };
            PARK_X.store(x, Relaxed);
            PARK_Y.store(y, Relaxed);
            CAPTURED.store(true, Relaxed);
            // SAFETY: plain call.
            unsafe { SetCursorPos(x, y) };
        } else {
            CAPTURED.store(false, Relaxed);
        }
    }

    pub fn warp(&self, to: Point) {
        LAST_X.store(to.x, Relaxed);
        LAST_Y.store(to.y, Relaxed);
        // SAFETY: plain call.
        unsafe { SetCursorPos(to.x, to.y) };
    }

    /// Windows always reports the cursor position, so edges always work.
    pub fn sees_cursor(&self) -> bool {
        true
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURED.store(false, Relaxed);
        let id = HOOK_THREAD.load(Relaxed);
        if id != 0 {
            // SAFETY: posting a message to a thread id is always safe.
            unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn hook_thread(sink: Arc<dyn Sink>, ready: std::sync::mpsc::Sender<Result<(), String>>) {
    SINK.with(|s| *s.borrow_mut() = Some(sink));
    // SAFETY: standard hook installation and message loop. The hook
    // procedures are `extern "system"` functions with the required
    // signatures; every handle is released before the thread ends.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
        if keyboard.is_null() || mouse.is_null() {
            let error = std::io::Error::last_os_error().to_string();
            if !keyboard.is_null() {
                UnhookWindowsHookEx(keyboard);
            }
            if !mouse.is_null() {
                UnhookWindowsHookEx(mouse);
            }
            let _ = ready.send(Err(error));
            return;
        }
        let mut at = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut at) != 0 {
            LAST_X.store(at.x, Relaxed);
            LAST_Y.store(at.y, Relaxed);
        }
        HOOK_THREAD.store(GetCurrentThreadId(), Relaxed);
        let timer = SetTimer(std::ptr::null_mut(), 0, STALE_CHECK_MS, None);
        let _ = ready.send(Ok(()));

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_TIMER {
                drop_stale_keys();
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        KillTimer(std::ptr::null_mut(), timer);
        UnhookWindowsHookEx(keyboard);
        UnhookWindowsHookEx(mouse);
    }
    HOOK_THREAD.store(0, Relaxed);
    CAPTURED.store(false, Relaxed);
    SINK.with(|s| *s.borrow_mut() = None);
}

fn sink() -> Option<Arc<dyn Sink>> {
    // Cloned out so no borrow is held while the sink runs: it may move the
    // cursor, which can re-enter the mouse hook on this same thread.
    SINK.with(|s| s.borrow().clone())
}

/// Locking the session or pressing Ctrl+Alt+Del switches desktops and the
/// hook never sees those keys released. A key we think is held but Windows
/// reports as up would otherwise block switching forever.
fn drop_stale_keys() {
    if CAPTURED.load(Relaxed) {
        return; // swallowed keys never reach the state GetAsyncKeyState reports
    }
    let stale = HELD.with(|held| {
        let mut held = held.borrow_mut();
        let before = held.len();
        // SAFETY: plain call with a virtual-key code.
        held.retain(|vk| unsafe { GetAsyncKeyState(*vk as i32) } < 0);
        held.len() != before
    });
    if stale {
        HELD.with(|held| held.borrow_mut().clear());
        if let Some(sink) = sink() {
            sink.reset();
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for HC_ACTION, `lparam` points to a KBDLLHOOKSTRUCT valid for
    // the duration of this call.
    let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    let pass = || unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    // Injected events include the ones a client instance on this same machine
    // synthesizes; feeding them back would loop.
    if code != HC_ACTION as i32 || info.flags & LLKHF_INJECTED != 0 {
        return pass();
    }
    let Some(sink) = sink() else { return pass() };
    let down = info.flags & LLKHF_UP == 0;
    HELD.with(|held| {
        let mut held = held.borrow_mut();
        held.retain(|vk| *vk != info.vkCode);
        if down {
            held.push(info.vkCode);
        }
    });
    let swallow =
        match keymap::from_windows(info.scanCode, info.flags & LLKHF_EXTENDED != 0, info.vkCode) {
            Some(key) => sink.key(key, down),
            // A key we cannot translate still must not leak to the local desktop
            // while a client has the keyboard.
            None => CAPTURED.load(Relaxed),
        };
    if swallow {
        1
    } else {
        pass()
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for HC_ACTION, `lparam` points to an MSLLHOOKSTRUCT valid for
    // the duration of this call.
    let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    let pass = || unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    if code != HC_ACTION as i32 || info.flags & LLMHF_INJECTED != 0 {
        return pass();
    }
    let Some(sink) = sink() else { return pass() };
    // High word of mouseData: wheel delta or X-button index.
    let high = (info.mouseData >> 16) as u16;
    let x_button = if high == 2 {
        button::EXTRA
    } else {
        button::SIDE
    };

    let swallow = match wparam as u32 {
        WM_MOUSEMOVE => {
            let (x, y) = (info.pt.x, info.pt.y);
            if CAPTURED.load(Relaxed) {
                let (px, py) = (PARK_X.load(Relaxed), PARK_Y.load(Relaxed));
                let (dx, dy) = (x - px, y - py);
                // A zero offset is the echo of our own re-parking.
                if dx != 0 || dy != 0 {
                    sink.motion(f64::from(dx), f64::from(dy), None);
                    if CAPTURED.load(Relaxed) {
                        // SAFETY: plain call.
                        unsafe { SetCursorPos(px, py) };
                    }
                }
                true
            } else {
                let (dx, dy) = (x - LAST_X.swap(x, Relaxed), y - LAST_Y.swap(y, Relaxed));
                sink.motion(f64::from(dx), f64::from(dy), Some(Point::new(x, y)))
            }
        }
        WM_LBUTTONDOWN => sink.button(button::LEFT, true),
        WM_LBUTTONUP => sink.button(button::LEFT, false),
        WM_RBUTTONDOWN => sink.button(button::RIGHT, true),
        WM_RBUTTONUP => sink.button(button::RIGHT, false),
        WM_MBUTTONDOWN => sink.button(button::MIDDLE, true),
        WM_MBUTTONUP => sink.button(button::MIDDLE, false),
        WM_XBUTTONDOWN => sink.button(x_button, true),
        WM_XBUTTONUP => sink.button(x_button, false),
        // Windows and the wire protocol both count 120 per notch, up/right positive.
        WM_MOUSEWHEEL => sink.scroll(0, i32::from(high as i16)),
        WM_MOUSEHWHEEL => sink.scroll(i32::from(high as i16), 0),
        _ => CAPTURED.load(Relaxed),
    };
    if swallow {
        1
    } else {
        pass()
    }
}
