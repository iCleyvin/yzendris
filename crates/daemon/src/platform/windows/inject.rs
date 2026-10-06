//! Synthesizing input with `SendInput`.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
};
use yzendris_core::geom::Rect;
use yzendris_core::keymap::{to_windows, WinKey};
use yzendris_core::proto::button;

use crate::platform::Inject;

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

pub struct SendInputInjector;

fn mouse(dx: i32, dy: i32, data: u32, flags: u32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one fully initialised INPUT with its exact size.
    unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
}

fn keyboard(vk: u16, scan: u16, flags: u32) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one fully initialised INPUT with its exact size.
    unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
}

/// Map a pixel coordinate onto the 0–65535 range `SendInput` expects.
fn normalize(v: i32, origin: i32, size: i32) -> i32 {
    if size <= 1 {
        return 0;
    }
    let pos = i64::from(v - origin).clamp(0, i64::from(size - 1));
    ((pos * 65535 + i64::from(size - 1) / 2) / i64::from(size - 1)) as i32
}

impl Inject for SendInputInjector {
    fn pointer_abs(&mut self, x: i32, y: i32, desktop: Rect) {
        mouse(
            normalize(x, desktop.x, desktop.w),
            normalize(y, desktop.y, desktop.h),
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        );
    }

    fn pointer_rel(&mut self, dx: i32, dy: i32) {
        // Relative moves are what games read through Raw Input.
        mouse(dx, dy, 0, MOUSEEVENTF_MOVE);
    }

    fn button(&mut self, code: u16, down: bool) {
        let (press, release, data) = match code {
            button::LEFT => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
            button::RIGHT => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
            button::MIDDLE => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
            button::SIDE => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1),
            button::EXTRA => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2),
            _ => return,
        };
        mouse(0, 0, data, if down { press } else { release });
    }

    fn scroll(&mut self, dx: i32, dy: i32) {
        if dy != 0 {
            mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL);
        }
        if dx != 0 {
            mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL);
        }
    }

    fn key(&mut self, code: u16, down: bool) {
        let up = if down { 0 } else { KEYEVENTF_KEYUP };
        match to_windows(code) {
            // Scan codes are layout-neutral: the physical key is reproduced
            // and this machine's own layout decides what it types.
            Some(WinKey::Scan { code, extended }) => {
                let ext = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
                keyboard(0, code, KEYEVENTF_SCANCODE | ext | up);
            }
            Some(WinKey::Vk(vk)) => keyboard(vk, 0, up),
            None => {}
        }
    }
}
