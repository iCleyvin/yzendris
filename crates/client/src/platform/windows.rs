//! Windows backend: inject keyboard/mouse via SendInput, read/move the cursor
//! and query the monitor under it via Win32.
//!
//! The protocol carries evdev keycodes (the Windows *server* translates its
//! scancodes to evdev before sending). Here we translate them back to AT
//! scan-code set 1 and inject with KEYEVENTF_SCANCODE, which is layout-neutral
//! — the physical key matches regardless of the laptop's keymap.
use anyhow::Result;
use yzendris_protocol::Event;

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEINPUT, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

pub const BACKEND: &str = "SendInput/Win32";

const WHEEL_DELTA: i32 = 120;
const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

/// evdev keycode → (AT scan-code set 1, extended?). Inverse of the server's
/// `scancode_to_evdev`. Returns None for codes we don't map.
fn evdev_to_scancode(code: u16) -> Option<(u16, bool)> {
    // E0-prefixed (extended) keys.
    let ext = match code {
        96 => 0x1C,  // KP_ENTER
        97 => 0x1D,  // RIGHTCTRL
        98 => 0x35,  // KP_SLASH
        100 => 0x38, // RIGHTALT
        102 => 0x47, // HOME
        103 => 0x48, // UP
        104 => 0x49, // PAGEUP
        105 => 0x4B, // LEFT
        106 => 0x4D, // RIGHT
        107 => 0x4F, // END
        108 => 0x50, // DOWN
        109 => 0x51, // PAGEDOWN
        110 => 0x52, // INSERT
        111 => 0x53, // DELETE
        125 => 0x5B, // LEFTMETA (Win)
        126 => 0x5C, // RIGHTMETA
        127 => 0x5D, // COMPOSE / menu
        _ => 0,
    };
    if ext != 0 {
        return Some((ext, true));
    }
    // Non-extended evdev codes 1..=88 equal AT set-1 scancodes 1:1.
    if (1..=88).contains(&code) {
        return Some((code, false));
    }
    None
}

/// evdev BTN_* → mouse down/up flag pair. Returns (down_flag, up_flag, xdata).
fn evdev_button(btn: u16) -> Option<(windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, u32)> {
    match btn {
        0x110 => Some((MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0)),     // BTN_LEFT
        0x111 => Some((MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0)),   // BTN_RIGHT
        0x112 => Some((MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0)), // BTN_MIDDLE
        0x113 => Some((MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1)),    // BTN_SIDE
        0x114 => Some((MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2)),    // BTN_EXTRA
        _ => None,
    }
}

fn key_input(scancode: u16, extended: bool, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scancode,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    if inputs.is_empty() {
        return;
    }
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

pub struct Injector {
    /// Track scancodes we've pressed (for release_all on disconnect).
    held: std::collections::HashSet<(u16, bool)>,
}

impl Injector {
    pub fn new() -> Result<Self> {
        Ok(Self {
            held: std::collections::HashSet::new(),
        })
    }

    /// Translate one protocol Event into SendInput calls.
    /// Returns Ok(true) if injected, Ok(false) for control events.
    pub fn inject(&mut self, event: &Event) -> Result<bool> {
        match event {
            Event::KeyPress { keycode } => {
                if let Some((sc, ext)) = evdev_to_scancode(*keycode) {
                    self.held.insert((sc, ext));
                    send(&[key_input(sc, ext, false)]);
                }
            }
            Event::KeyRelease { keycode } => {
                if let Some((sc, ext)) = evdev_to_scancode(*keycode) {
                    self.held.remove(&(sc, ext));
                    send(&[key_input(sc, ext, true)]);
                }
            }
            Event::MouseMove { dx, dy } => {
                send(&[mouse_input(*dx, *dy, 0, MOUSEEVENTF_MOVE)]);
            }
            Event::MouseButton { btn, pressed } => {
                if let Some((down, up, xdata)) = evdev_button(*btn) {
                    let flag = if *pressed { down } else { up };
                    send(&[mouse_input(0, 0, xdata, flag)]);
                }
            }
            Event::Scroll { dx, dy } => {
                let mut evs = Vec::new();
                if *dy != 0 {
                    evs.push(mouse_input(0, 0, (*dy * WHEEL_DELTA) as u32, MOUSEEVENTF_WHEEL));
                }
                if *dx != 0 {
                    evs.push(mouse_input(0, 0, (*dx * WHEEL_DELTA) as u32, MOUSEEVENTF_HWHEEL));
                }
                send(&evs);
            }
            Event::SyncKeys { keycodes_down } => {
                let ups: Vec<INPUT> = keycodes_down
                    .iter()
                    .filter_map(|&c| evdev_to_scancode(c))
                    .map(|(sc, ext)| {
                        self.held.remove(&(sc, ext));
                        key_input(sc, ext, true)
                    })
                    .collect();
                send(&ups);
            }
            Event::CaptureStart
            | Event::CaptureEnd
            | Event::Heartbeat
            | Event::ClipboardText { .. }
            | Event::EnterAt { .. }
            | Event::EdgeReached { .. } => {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Release everything still marked as held (on CaptureEnd / disconnect).
    pub fn release_all(&mut self) -> Result<()> {
        let ups: Vec<INPUT> = self
            .held
            .drain()
            .map(|(sc, ext)| key_input(sc, ext, true))
            .collect();
        send(&ups);
        // Also release every mouse button, in case one was held.
        send(&[
            mouse_input(0, 0, 0, MOUSEEVENTF_LEFTUP),
            mouse_input(0, 0, 0, MOUSEEVENTF_RIGHTUP),
            mouse_input(0, 0, 0, MOUSEEVENTF_MIDDLEUP),
            mouse_input(0, 0, XBUTTON1, MOUSEEVENTF_XUP),
            mouse_input(0, 0, XBUTTON2, MOUSEEVENTF_XUP),
        ]);
        Ok(())
    }
}

/// No post-create setup needed on Windows (no virtual device, no layout dance).
pub async fn setup(_inj: &mut Injector, _kb_layout: &str) {}

// ─── Cursor / monitor ────────────────────────────────────────────────────────

pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe {
        if GetCursorPos(&mut p).is_ok() {
            Some((p.x, p.y))
        } else {
            None
        }
    }
}

pub fn move_cursor(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// Rect of the monitor under the cursor, as (x, y, width, height) in virtual
/// desktop coordinates — matches what the tracker expects.
pub fn focused_monitor_rect() -> Option<(i32, i32, i32, i32)> {
    let (cx, cy) = cursor_pos()?;
    unsafe {
        let hmon: HMONITOR = MonitorFromPoint(POINT { x: cx, y: cy }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut info).as_bool() {
            let r = info.rcMonitor;
            Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
        } else {
            None
        }
    }
}
