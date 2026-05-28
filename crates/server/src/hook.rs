/// Windows low-level keyboard and mouse hooks.
///
/// Architecture:
///   • A dedicated Win32 thread installs WH_KEYBOARD_LL and WH_MOUSE_LL and
///     runs a GetMessage loop so the hooks are kept alive.
///   • Hook callbacks write `protocol::Event` values into a
///     `tokio::sync::mpsc::UnboundedSender` that lives in a `OnceLock`.
///   • The Tokio runtime drains that channel in `net.rs` and forwards events.
///
/// SAFETY: all unsafe blocks are required for Win32 interop.  The global
/// atomics and OnceLock are the only shared state; no raw pointers escape.

use std::sync::{
    atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
    OnceLock,
};

use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};
use yzendris_protocol::Event;

#[cfg(windows)]
use windows::Win32::{
    Foundation::{BOOL, LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT,
        MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetCursorPos,
        SetWindowsHookExW, ShowCursor, TranslateMessage, UnhookWindowsHookEx,
        WH_KEYBOARD_LL, WH_MOUSE_LL,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
        WM_XBUTTONDOWN, WM_XBUTTONUP,
    },
};

// ─── Globals ─────────────────────────────────────────────────────────────────

/// Channel to the async runtime.
static EVENT_TX: OnceLock<UnboundedSender<Event>> = OnceLock::new();

/// Whether we are currently routing input to the Linux client.
pub static CAPTURING: AtomicBool = AtomicBool::new(false);

/// Whether a client TCP connection is currently live.
/// Edge detection only triggers capture when this is true.
pub static CLIENT_CONNECTED: AtomicBool = AtomicBool::new(false);

pub fn set_client_connected(val: bool) {
    CLIENT_CONNECTED.store(val, Ordering::Relaxed);
}

/// Win32 thread ID of the hook thread (used to post WM_QUIT for shutdown).
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Modifier key state (for release-combo detection).
static CTRL_DOWN:  AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static ALT_DOWN:   AtomicBool = AtomicBool::new(false);

/// Cursor position we reset to while capturing ("infinite mouse" trick).
static CAPTURE_CENTER_X: AtomicI32 = AtomicI32::new(960);
static CAPTURE_CENTER_Y: AtomicI32 = AtomicI32::new(540);

/// Guards against the hook seeing the SetCursorPos call as a real delta.
static RESETTING_CURSOR: AtomicBool = AtomicBool::new(false);

// ─── Per-thread key state (hook thread only) ─────────────────────────────────
//
// 512-bit bitmask of currently-held evdev keycodes.  Code N lives in bit
// (N % 64) of word (N / 64).  Covers codes 0–511 which includes all standard
// keyboard keys AND mouse buttons (BTN_LEFT=0x110=272, fits in word 4).
//
// This is thread-local because the hook proc is always called on the hook
// thread (the one that installed the hook and runs GetMessage).

thread_local! {
    static HELD_KEYS: std::cell::RefCell<[u64; 8]> =
        const { std::cell::RefCell::new([0u64; 8]) };
}

fn held_key_set(code: u16, down: bool) {
    let idx = code as usize / 64;
    let bit = code as usize % 64;
    if idx < 8 {
        HELD_KEYS.with(|hk| {
            let mut h = hk.borrow_mut();
            if down { h[idx] |=  1u64 << bit; }
            else    { h[idx] &= !(1u64 << bit); }
        });
    }
}

fn held_keys_collect() -> Vec<u16> {
    HELD_KEYS.with(|hk| {
        let h = hk.borrow();
        let mut out = Vec::new();
        for (slot, &word) in h.iter().enumerate() {
            let mut w = word;
            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                out.push((slot * 64 + bit) as u16);
                w &= w - 1; // clear lowest set bit
            }
        }
        out
    })
}

fn held_keys_clear() {
    HELD_KEYS.with(|hk| *hk.borrow_mut() = [0u64; 8]);
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Initialise the event sender.  Must be called before `start`.
pub fn init(sender: UnboundedSender<Event>) {
    EVENT_TX.set(sender).ok();
}

/// Set the "park position" for the captured mouse cursor.
pub fn set_capture_center(x: i32, y: i32) {
    CAPTURE_CENTER_X.store(x, Ordering::Relaxed);
    CAPTURE_CENTER_Y.store(y, Ordering::Relaxed);
}

/// Configure which edge triggers capture.
/// `trigger_coord`: the X (right/left) or Y (top/bottom) threshold.
/// `side`: 0=right, 1=left, 2=bottom, 3=top.
pub fn configure_edge(trigger_coord: i32, side: u8) {
    EDGE_TRIGGER_X.store(trigger_coord, Ordering::Relaxed);
    EDGE_TRIGGER_SIDE.store(side as u32, Ordering::Relaxed);
}

/// Spawn the Win32 hook thread.  Returns immediately.
pub fn start() {
    std::thread::spawn(hook_thread);
}

/// Called from the network layer when the TCP connection drops.
/// Safely exits capture from any thread (ShowCursor / SetCursorPos are
/// thread-safe Win32 calls).
pub fn release_capture_on_disconnect() {
    if !CAPTURING.load(Ordering::Relaxed) {
        return;
    }
    held_keys_clear();
    CAPTURING.store(false, Ordering::Relaxed);
    CTRL_DOWN.store(false,  Ordering::Relaxed);
    SHIFT_DOWN.store(false, Ordering::Relaxed);
    ALT_DOWN.store(false,   Ordering::Relaxed);
    #[cfg(windows)]
    unsafe {
        ShowCursor(windows::Win32::Foundation::BOOL(1));
        let cx = CAPTURE_CENTER_X.load(Ordering::Relaxed);
        let cy = CAPTURE_CENTER_Y.load(Ordering::Relaxed);
        let _ = SetCursorPos(cx, cy);
    }
    warn!("capture released (client disconnected)");
}

/// Send WM_QUIT to the hook thread so it tears down and exits.
pub fn stop() {
    let tid = HOOK_THREAD_ID.load(Ordering::Relaxed);
    if tid != 0 {
        #[cfg(windows)]
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

// ─── Edge globals ─────────────────────────────────────────────────────────────

static EDGE_TRIGGER_X:    AtomicI32 = AtomicI32::new(i32::MAX);
static EDGE_TRIGGER_SIDE: AtomicU32 = AtomicU32::new(0); // 0=right 1=left 2=bottom 3=top

// ─── Hook thread ─────────────────────────────────────────────────────────────

fn hook_thread() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::GetCurrentThreadId;
        HOOK_THREAD_ID.store(GetCurrentThreadId(), Ordering::Relaxed);

        let kb_hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0) {
            Ok(h) => h,
            Err(e) => { warn!("SetWindowsHookExW(KB) failed: {e}"); return; }
        };
        let ms_hook = match SetWindowsHookExW(WH_MOUSE_LL, Some(ms_proc), None, 0) {
            Ok(h) => h,
            Err(e) => {
                warn!("SetWindowsHookExW(MS) failed: {e}");
                let _ = UnhookWindowsHookEx(kb_hook);
                return;
            }
        };
        info!("hooks installed — message loop starting");

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 { break; } // 0=WM_QUIT, -1=error
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        info!("hook thread exiting");
        let _ = UnhookWindowsHookEx(kb_hook);
        let _ = UnhookWindowsHookEx(ms_hook);
    }

    #[cfg(not(windows))]
    { warn!("hook_thread: not on Windows — no-op"); }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn send(event: Event) {
    if let Some(tx) = EVENT_TX.get() { let _ = tx.send(event); }
}

/// AT scan-code set 1 → evdev keycode.
/// Non-extended codes 1-88 map 1-to-1 with evdev.
/// Extended (E0-prefixed) keys need explicit translation.
fn scancode_to_evdev(scancode: u32, extended: bool) -> Option<u16> {
    if extended {
        Some(match scancode {
            0x1C => 96,  // KEY_KPENTER
            0x1D => 97,  // KEY_RIGHTCTRL
            0x35 => 98,  // KEY_KPSLASH
            0x38 => 100, // KEY_RIGHTALT
            0x46 => 119, // KEY_PAUSE (Break+Ctrl)
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
            0x5D => 127, // KEY_COMPOSE
            0x5E => 116, // KEY_POWER
            0x5F => 142, // KEY_SLEEP
            0x63 => 213, // KEY_WAKEUP
            _ => return None,
        })
    } else {
        if (1..=88).contains(&scancode) { Some(scancode as u16) } else { None }
    }
}

fn update_modifiers(vkcode: u32, down: bool) {
    match vkcode {
        0xA2 | 0xA3 => CTRL_DOWN.store(down,  Ordering::Relaxed),
        0xA0 | 0xA1 => SHIFT_DOWN.store(down, Ordering::Relaxed),
        0xA4 | 0xA5 => ALT_DOWN.store(down,   Ordering::Relaxed),
        _ => {}
    }
}

fn all_modifiers_down() -> bool {
    CTRL_DOWN.load(Ordering::Relaxed)
        && SHIFT_DOWN.load(Ordering::Relaxed)
        && ALT_DOWN.load(Ordering::Relaxed)
}

// ─── Keyboard hook proc ───────────────────────────────────────────────────────

#[cfg(windows)]
unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != 0 /* HC_ACTION */ {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let flags_raw = kb.flags.0;

    // Bit 7 = LLKHF_UP (key released).
    let is_down    = flags_raw & 0x80 == 0;
    // Bit 0 = LLKHF_EXTENDED (E0-prefixed key).
    let is_extended = flags_raw & 0x01 != 0;

    update_modifiers(kb.vkCode, is_down);

    // Skip injected events (bit 4 = LLKHF_INJECTED) to avoid hook loops.
    if flags_raw & 0x10 != 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    if CAPTURING.load(Ordering::Relaxed) {
        // Release combo: Ctrl+Shift+Alt all down → exit capture.
        if is_down && all_modifiers_down() {
            exit_capture();
            return CallNextHookEx(None, code, wparam, lparam);
        }

        if let Some(evdev_code) = scancode_to_evdev(kb.scanCode, is_extended) {
            // Track held keys for stuck-key recovery on disconnect.
            held_key_set(evdev_code, is_down);

            let event = if is_down {
                Event::KeyPress  { keycode: evdev_code }
            } else {
                Event::KeyRelease { keycode: evdev_code }
            };
            send(event);
        }
        return LRESULT(1); // suppress
    }

    CallNextHookEx(None, code, wparam, lparam)
}

// ─── Mouse hook proc ──────────────────────────────────────────────────────────

#[cfg(windows)]
unsafe extern "system" fn ms_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != 0 /* HC_ACTION */ {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let msg = wparam.0 as u32;

    // Skip injected mouse events (LLMHF_INJECTED = bit 0).
    if ms.flags & 0x01 != 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    if CAPTURING.load(Ordering::Relaxed) {
        match msg {
            WM_MOUSEMOVE => {
                if RESETTING_CURSOR.load(Ordering::Relaxed) {
                    return LRESULT(1);
                }
                let cx = CAPTURE_CENTER_X.load(Ordering::Relaxed);
                let cy = CAPTURE_CENTER_Y.load(Ordering::Relaxed);
                let dx = ms.pt.x - cx;
                let dy = ms.pt.y - cy;
                if dx != 0 || dy != 0 {
                    send(Event::MouseMove { dx, dy });
                    RESETTING_CURSOR.store(true, Ordering::Relaxed);
                    let _ = SetCursorPos(cx, cy);
                    RESETTING_CURSOR.store(false, Ordering::Relaxed);
                }
                return LRESULT(1);
            }
            WM_LBUTTONDOWN => { send(Event::MouseButton { btn: 0x110, pressed: true  }); return LRESULT(1); }
            WM_LBUTTONUP   => { send(Event::MouseButton { btn: 0x110, pressed: false }); return LRESULT(1); }
            WM_RBUTTONDOWN => { send(Event::MouseButton { btn: 0x111, pressed: true  }); return LRESULT(1); }
            WM_RBUTTONUP   => { send(Event::MouseButton { btn: 0x111, pressed: false }); return LRESULT(1); }
            WM_MBUTTONDOWN => { send(Event::MouseButton { btn: 0x112, pressed: true  }); return LRESULT(1); }
            WM_MBUTTONUP   => { send(Event::MouseButton { btn: 0x112, pressed: false }); return LRESULT(1); }
            WM_XBUTTONDOWN => {
                let which = (ms.mouseData >> 16) as u16;
                let btn = if which == 2 { 0x114u16 } else { 0x113u16 };
                send(Event::MouseButton { btn, pressed: true });
                return LRESULT(1);
            }
            WM_XBUTTONUP => {
                let which = (ms.mouseData >> 16) as u16;
                let btn = if which == 2 { 0x114u16 } else { 0x113u16 };
                send(Event::MouseButton { btn, pressed: false });
                return LRESULT(1);
            }
            WM_MOUSEWHEEL => {
                let raw_delta = (ms.mouseData >> 16) as i16;
                let clicks = raw_delta as i32 / 120;
                if clicks != 0 { send(Event::Scroll { dx: 0, dy: clicks }); }
                return LRESULT(1);
            }
            _ => return LRESULT(1),
        }
    } else {
        if msg == WM_MOUSEMOVE {
            check_edge_and_enter(ms.pt.x, ms.pt.y);
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}

// ─── Capture enter / exit ─────────────────────────────────────────────────────

#[cfg(windows)]
unsafe fn check_edge_and_enter(x: i32, y: i32) {
    let connected = CLIENT_CONNECTED.load(Ordering::Relaxed);
    let side      = EDGE_TRIGGER_SIDE.load(Ordering::Relaxed);
    let threshold = EDGE_TRIGGER_X.load(Ordering::Relaxed);
    let at_edge   = match side {
        0 => x >= threshold,
        1 => x <= threshold,
        2 => y >= threshold,
        3 => y <= threshold,
        _ => false,
    };
    if at_edge {
        if !connected {
            return;
        }
        tracing::debug!(
            "edge hit: x={x} y={y} threshold={threshold} side={side} — entering capture"
        );
        enter_capture();
    }
}

#[cfg(windows)]
unsafe fn enter_capture() {
    held_keys_clear();
    CTRL_DOWN.store(false,  Ordering::Relaxed);
    SHIFT_DOWN.store(false, Ordering::Relaxed);
    ALT_DOWN.store(false,   Ordering::Relaxed);
    CAPTURING.store(true, Ordering::Relaxed);

    let cx = CAPTURE_CENTER_X.load(Ordering::Relaxed);
    let cy = CAPTURE_CENTER_Y.load(Ordering::Relaxed);
    ShowCursor(BOOL(0));
    RESETTING_CURSOR.store(true, Ordering::Relaxed);
    let _ = SetCursorPos(cx, cy);
    RESETTING_CURSOR.store(false, Ordering::Relaxed);

    send(Event::CaptureStart);
    info!("capture started");
}

#[cfg(windows)]
unsafe fn exit_capture() {
    // Collect all keys held on the Linux side so the client can release them.
    let held = held_keys_collect();
    held_keys_clear();

    CAPTURING.store(false, Ordering::Relaxed);
    CTRL_DOWN.store(false,  Ordering::Relaxed);
    SHIFT_DOWN.store(false, Ordering::Relaxed);
    ALT_DOWN.store(false,   Ordering::Relaxed);

    ShowCursor(BOOL(1));
    let cx = CAPTURE_CENTER_X.load(Ordering::Relaxed);
    let cy = CAPTURE_CENTER_Y.load(Ordering::Relaxed);
    let _ = SetCursorPos(cx, cy);

    // Tell the client which keys to release, then signal CaptureEnd.
    if !held.is_empty() {
        send(Event::SyncKeys { keycodes_down: held });
    }
    send(Event::CaptureEnd);
    info!("capture ended");
}
