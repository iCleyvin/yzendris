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
    atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering},
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

/// One channel to the async runtime per client (indexed by client id).
static EVENT_TXS: OnceLock<Vec<UnboundedSender<Event>>> = OnceLock::new();

/// Whether we are currently routing input to a client.
pub static CAPTURING: AtomicBool = AtomicBool::new(false);

/// Which client currently holds capture (valid only while CAPTURING).
static ACTIVE_CLIENT: AtomicUsize = AtomicUsize::new(0);

/// Bitmask of clients with a live TCP connection (bit k = client k).
/// Edge detection only triggers capture for a connected client.
static CLIENT_CONNECTED_MASK: AtomicU64 = AtomicU64::new(0);

pub fn set_client_connected(client: usize, val: bool) {
    if client >= 64 {
        return;
    }
    let bit = 1u64 << client;
    if val {
        CLIENT_CONNECTED_MASK.fetch_or(bit, Ordering::Relaxed);
    } else {
        CLIENT_CONNECTED_MASK.fetch_and(!bit, Ordering::Relaxed);
    }
    write_status_file();
}

/// Write the connected-clients bitmask to %APPDATA%\yzendris\status so the GUI
/// can show live per-client connection state reliably (the log scrolls).
fn write_status_file() {
    let mask = CLIENT_CONNECTED_MASK.load(Ordering::Relaxed);
    if let Some(dir) = std::env::var_os("APPDATA") {
        let path = std::path::PathBuf::from(dir).join("yzendris").join("status");
        let _ = std::fs::write(path, mask.to_string());
    }
}

fn client_connected(client: usize) -> bool {
    client < 64 && CLIENT_CONNECTED_MASK.load(Ordering::Relaxed) & (1u64 << client) != 0
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

fn held_key_is_set(code: u16) -> bool {
    let idx = code as usize / 64;
    let bit = code as usize % 64;
    if idx >= 8 {
        return false;
    }
    HELD_KEYS.with(|hk| hk.borrow()[idx] & (1u64 << bit) != 0)
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Initialise the per-client event senders.  Must be called before `start`.
pub fn init(senders: Vec<UnboundedSender<Event>>) {
    EVENT_TXS.set(senders).ok();
}

/// Set the "park position" for the captured mouse cursor.
pub fn set_capture_center(x: i32, y: i32) {
    CAPTURE_CENTER_X.store(x, Ordering::Relaxed);
    CAPTURE_CENTER_Y.store(y, Ordering::Relaxed);
}

/// A client and the layout/boundary it occupies in the host's screen space.
#[derive(Debug, Clone, Copy)]
pub struct Portal {
    pub client: usize,
    pub layout: Layout,
}

/// Install all client portals. Must be called once before `start`.
pub fn configure_portals(portals: Vec<Portal>) {
    PORTALS.set(portals).ok();
}

/// Spawn the Win32 hook thread.  Returns immediately.
pub fn start() {
    std::thread::spawn(hook_thread);
}

/// Called from the network layer when a client's TCP connection drops.
/// Only releases capture if that client was the one holding it.
pub fn release_capture_on_disconnect(client: usize) {
    if !CAPTURING.load(Ordering::Relaxed) || ACTIVE_CLIENT.load(Ordering::Relaxed) != client {
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
        set_prev(cx, cy);
        RESETTING_CURSOR.store(true, Ordering::Relaxed);
        let _ = SetCursorPos(cx, cy);
        RESETTING_CURSOR.store(false, Ordering::Relaxed);
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

// ─── Layout ──────────────────────────────────────────────────────────────────

/// Simple integer rectangle (Win32 convention: right/bottom exclusive).
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[allow(dead_code)]
impl Rect {
    pub fn width(&self) -> i32 { self.right - self.left }
    pub fn height(&self) -> i32 { self.bottom - self.top }
}

/// Where the client (laptop) screen sits relative to this PC's monitors.
#[derive(Debug, Clone, Copy)]
pub enum Layout {
    /// Classic mode: the laptop is past an outer edge of the whole desktop.
    /// `side`: 0=right 1=left 2=bottom 3=top.  `screen` is the virtual screen.
    Edge { side: u8, screen: Rect },
    /// The laptop sits BETWEEN two horizontally adjacent monitors (laptop to
    /// the side). Crossing the vertical boundary line routes through the
    /// laptop's left/right edge.
    SideBySide { left: Rect, right: Rect, boundary_x: i32 },
    /// The laptop sits BETWEEN two vertically stacked monitors (one above the
    /// other, laptop in the middle). Crossing the horizontal boundary line
    /// routes through the laptop's top/bottom edge.
    Stacked { top: Rect, bottom: Rect, boundary_y: i32 },
}

static PORTALS: OnceLock<Vec<Portal>> = OnceLock::new();

/// Layout of the client that currently holds capture (its portal's layout).
fn active_layout() -> Option<Layout> {
    let id = ACTIVE_CLIENT.load(Ordering::Relaxed);
    PORTALS.get()?.iter().find(|p| p.client == id).map(|p| p.layout)
}

/// Pixels we inset the cursor from the boundary/edge when handing control
/// back, so the very next mouse move doesn't instantly re-trigger capture.
/// Large enough that a normal post-return mouse move doesn't immediately
/// re-cross the threshold.
const RETURN_MARGIN: i32 = 16;

/// Previous (non-captured) cursor position — used to detect boundary
/// crossings in `Between` mode. PREV_VALID guards the first event.
static PREV_X: AtomicI32 = AtomicI32::new(0);
static PREV_Y: AtomicI32 = AtomicI32::new(0);
static PREV_VALID: AtomicBool = AtomicBool::new(false);

/// Which side capture was entered from in `Between` mode (0 = from the left
/// monitor, 1 = from the right monitor). Used by the release combo to put the
/// cursor back where the user came from.
static CAME_FROM: AtomicU32 = AtomicU32::new(0);

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

/// Send to a specific client's channel.
fn send_to(client: usize, event: Event) {
    if let Some(txs) = EVENT_TXS.get() {
        if let Some(tx) = txs.get(client) {
            let _ = tx.send(event);
        }
    }
}

/// Send to whichever client currently holds capture.
fn send(event: Event) {
    send_to(ACTIVE_CLIENT.load(Ordering::Relaxed), event);
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
            // Drop auto-repeat: Windows posts repeated WM_KEYDOWN while a key
            // is held, but the client's kernel already repeats a held uinput
            // key on its own. Forwarding the repeats would double them.
            // A WM_KEYDOWN for a key already marked down is a repeat.
            // (LL hooks don't expose LLKHF_AUTOREPEAT, so we dedup by state.)
            if is_down && held_key_is_set(evdev_code) {
                return LRESULT(1); // suppress repeat
            }

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
            if RESETTING_CURSOR.load(Ordering::Relaxed) {
                // Move generated by our own SetCursorPos — just resync PREV.
                set_prev(ms.pt.x, ms.pt.y);
            } else if check_edge_and_enter(ms.pt.x, ms.pt.y) {
                // This move triggered capture — swallow it so the cursor
                // doesn't visibly land on the far monitor before parking.
                return LRESULT(1);
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}

// ─── Capture enter / exit ─────────────────────────────────────────────────────

fn set_prev(x: i32, y: i32) {
    PREV_X.store(x, Ordering::Relaxed);
    PREV_Y.store(y, Ordering::Relaxed);
    PREV_VALID.store(true, Ordering::Relaxed);
}

/// Fraction (0.0–1.0) of `v` along the span [lo, hi).
fn frac_of(v: i32, lo: i32, hi: i32) -> f32 {
    if hi <= lo {
        return 0.5;
    }
    ((v - lo) as f32 / (hi - lo) as f32).clamp(0.0, 1.0)
}

/// Pure geometry: does moving from (px,py) to (x,y) cross into this layout's
/// client? Returns (client entry edge, frac along it, came_from side).
fn crossing_for(
    layout: &Layout,
    prev_valid: bool,
    px: i32,
    py: i32,
    x: i32,
    y: i32,
) -> Option<(u8, f32, u32)> {
    use yzendris_protocol::{EDGE_BOTTOM, EDGE_LEFT, EDGE_RIGHT, EDGE_TOP};
    match *layout {
        Layout::Edge { side, screen } => {
            let at_edge = match side {
                0 => x >= screen.right - 1,
                1 => x <= screen.left,
                2 => y >= screen.bottom - 1,
                3 => y <= screen.top,
                _ => false,
            };
            if !at_edge {
                return None;
            }
            Some(match side {
                0 => (EDGE_LEFT, frac_of(y, screen.top, screen.bottom), 0),
                1 => (EDGE_RIGHT, frac_of(y, screen.top, screen.bottom), 0),
                2 => (EDGE_TOP, frac_of(x, screen.left, screen.right), 0),
                _ => (EDGE_BOTTOM, frac_of(x, screen.left, screen.right), 0),
            })
        }
        Layout::SideBySide { left, right, boundary_x } => {
            if !prev_valid {
                return None;
            }
            let band_top = left.top.max(right.top);
            let band_bottom = left.bottom.min(right.bottom);
            if !(y >= band_top && y < band_bottom) {
                return None;
            }
            if px < boundary_x && x >= boundary_x {
                Some((EDGE_LEFT, frac_of(y, band_top, band_bottom), 0))
            } else if px >= boundary_x && x < boundary_x {
                Some((EDGE_RIGHT, frac_of(y, band_top, band_bottom), 1))
            } else {
                None
            }
        }
        Layout::Stacked { top, bottom, boundary_y } => {
            if !prev_valid {
                return None;
            }
            let band_left = top.left.max(bottom.left);
            let band_right = top.right.min(bottom.right);
            if !(x >= band_left && x < band_right) {
                return None;
            }
            if py < boundary_y && y >= boundary_y {
                Some((EDGE_TOP, frac_of(x, band_left, band_right), 0))
            } else if py >= boundary_y && y < boundary_y {
                Some((EDGE_BOTTOM, frac_of(x, band_left, band_right), 1))
            } else {
                None
            }
        }
    }
}

/// Returns `true` when this move entered capture (caller should swallow it).
/// Checks every connected client's portal and enters the first one crossed.
#[cfg(windows)]
unsafe fn check_edge_and_enter(x: i32, y: i32) -> bool {
    let Some(portals) = PORTALS.get() else {
        set_prev(x, y);
        return false;
    };
    let prev_valid = PREV_VALID.load(Ordering::Relaxed);
    let px = PREV_X.load(Ordering::Relaxed);
    let py = PREV_Y.load(Ordering::Relaxed);

    let mut entered = false;
    for portal in portals {
        if !client_connected(portal.client) {
            continue;
        }
        if let Some((edge, frac, came_from)) = crossing_for(&portal.layout, prev_valid, px, py, x, y) {
            tracing::debug!("client {} portal crossed — entering capture", portal.client);
            enter_capture(portal.client, edge, frac, came_from);
            entered = true;
            break;
        }
    }
    set_prev(x, y);
    entered
}

#[cfg(windows)]
unsafe fn enter_capture(client: usize, client_edge: u8, frac: f32, came_from: u32) {
    held_keys_clear();
    CTRL_DOWN.store(false,  Ordering::Relaxed);
    SHIFT_DOWN.store(false, Ordering::Relaxed);
    ALT_DOWN.store(false,   Ordering::Relaxed);
    CAME_FROM.store(came_from, Ordering::Relaxed);
    ACTIVE_CLIENT.store(client, Ordering::Relaxed);
    CAPTURING.store(true, Ordering::Relaxed);

    let cx = CAPTURE_CENTER_X.load(Ordering::Relaxed);
    let cy = CAPTURE_CENTER_Y.load(Ordering::Relaxed);
    ShowCursor(BOOL(0));
    RESETTING_CURSOR.store(true, Ordering::Relaxed);
    let _ = SetCursorPos(cx, cy);
    RESETTING_CURSOR.store(false, Ordering::Relaxed);

    send_to(client, Event::CaptureStart);
    send_to(client, Event::EnterAt { edge: client_edge, frac });
    info!("capture started (client {client} enters edge {client_edge} at {frac:.2})");
}

/// Called from the network task when the client reports its cursor pushed
/// past an edge of its screen (`Event::EdgeReached`): release capture and
/// place the Windows cursor on the matching side.
pub fn release_capture_toward(client: usize, edge: u8, frac: f32) {
    use yzendris_protocol::{EDGE_BOTTOM, EDGE_LEFT, EDGE_RIGHT, EDGE_TOP};

    // Only the client currently holding capture may hand it back.
    if !CAPTURING.load(Ordering::Relaxed) || ACTIVE_CLIENT.load(Ordering::Relaxed) != client {
        return;
    }
    let Some(layout) = active_layout() else { return };

    // Work out where the cursor should reappear on the Windows desktop.
    let target: Option<(i32, i32)> = match layout {
        Layout::SideBySide { left, right, boundary_x } => {
            // `frac` is relative to the shared vertical band (same basis as
            // capture entry), so map it back through that band.
            let band_top = left.top.max(right.top);
            let band_bottom = left.bottom.min(right.bottom);
            match edge {
                EDGE_RIGHT => Some((
                    boundary_x + RETURN_MARGIN,
                    lerp_clamped(band_top, band_bottom, frac),
                )),
                EDGE_LEFT => Some((
                    boundary_x - 1 - RETURN_MARGIN,
                    lerp_clamped(band_top, band_bottom, frac),
                )),
                _ => None,
            }
        }
        Layout::Stacked { top, bottom, boundary_y } => {
            // `frac` is relative to the shared horizontal band.
            let band_left = top.left.max(bottom.left);
            let band_right = top.right.min(bottom.right);
            match edge {
                // Client left through its BOTTOM edge → continue on the bottom
                // monitor, just past the boundary.
                EDGE_BOTTOM => Some((
                    lerp_clamped(band_left, band_right, frac),
                    boundary_y + RETURN_MARGIN,
                )),
                // Client left through its TOP edge → back to the top monitor.
                EDGE_TOP => Some((
                    lerp_clamped(band_left, band_right, frac),
                    boundary_y - 1 - RETURN_MARGIN,
                )),
                _ => None,
            }
        }
        Layout::Edge { side, screen } => match (side, edge) {
            // Only the edge facing the PC hands control back.
            (0, EDGE_LEFT)   => Some((screen.right - 1 - RETURN_MARGIN,
                                      lerp_clamped(screen.top, screen.bottom, frac))),
            (1, EDGE_RIGHT)  => Some((screen.left + RETURN_MARGIN,
                                      lerp_clamped(screen.top, screen.bottom, frac))),
            (2, EDGE_TOP)    => Some((lerp_clamped(screen.left, screen.right, frac),
                                      screen.bottom - 1 - RETURN_MARGIN)),
            (3, EDGE_BOTTOM) => Some((lerp_clamped(screen.left, screen.right, frac),
                                      screen.top + RETURN_MARGIN)),
            _ => None,
        },
    };

    let Some((tx_pos, ty_pos)) = target else {
        // Edge leads nowhere (e.g. the top/bottom edge of a between-monitors
        // laptop). Stay captured; the client keeps tracking so the user can
        // escape via an edge that does lead back to the PC.
        tracing::debug!("EdgeReached on edge {edge} leads nowhere — staying captured");
        return;
    };

    // Tell the client to release everything that's still held.
    let held = held_keys_collect();
    held_keys_clear();
    CAPTURING.store(false, Ordering::Relaxed);
    CTRL_DOWN.store(false,  Ordering::Relaxed);
    SHIFT_DOWN.store(false, Ordering::Relaxed);
    ALT_DOWN.store(false,   Ordering::Relaxed);

    if !held.is_empty() {
        send(Event::SyncKeys { keycodes_down: held });
    }
    send(Event::CaptureEnd);

    #[cfg(windows)]
    unsafe {
        ShowCursor(BOOL(1));
        set_prev(tx_pos, ty_pos);
        RESETTING_CURSOR.store(true, Ordering::Relaxed);
        let _ = SetCursorPos(tx_pos, ty_pos);
        RESETTING_CURSOR.store(false, Ordering::Relaxed);
    }
    info!("capture released toward edge {edge} → cursor at ({tx_pos},{ty_pos})");
}

/// Integer lerp along [lo, hi) clamped inside the span.
fn lerp_clamped(lo: i32, hi: i32, frac: f32) -> i32 {
    let span = (hi - lo - 1).max(0) as f32;
    lo + (frac.clamp(0.0, 1.0) * span) as i32
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
    // Release combo = "bring the cursor back here". In Between mode return to
    // the centre of the monitor the cursor came from; otherwise the park centre.
    let came_from = CAME_FROM.load(Ordering::Relaxed);
    let (cx, cy) = match active_layout() {
        Some(Layout::SideBySide { left, right, .. }) => {
            let m = if came_from == 0 { left } else { right };
            ((m.left + m.right) / 2, (m.top + m.bottom) / 2)
        }
        Some(Layout::Stacked { top, bottom, .. }) => {
            let m = if came_from == 0 { top } else { bottom };
            ((m.left + m.right) / 2, (m.top + m.bottom) / 2)
        }
        _ => (
            CAPTURE_CENTER_X.load(Ordering::Relaxed),
            CAPTURE_CENTER_Y.load(Ordering::Relaxed),
        ),
    };
    set_prev(cx, cy);
    RESETTING_CURSOR.store(true, Ordering::Relaxed);
    let _ = SetCursorPos(cx, cy);
    RESETTING_CURSOR.store(false, Ordering::Relaxed);

    // Tell the client which keys to release, then signal CaptureEnd.
    if !held.is_empty() {
        send(Event::SyncKeys { keycodes_down: held });
    }
    send(Event::CaptureEnd);
    info!("capture ended");
}
