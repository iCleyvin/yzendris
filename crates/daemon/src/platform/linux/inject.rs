//! Synthesizing input through `/dev/uinput`.
//!
//! Three virtual devices are created, each of a kind every compositor already
//! understands:
//!
//! * a keyboard,
//! * a relative mouse (buttons, wheel, and raw motion for pointer-grabbing
//!   applications such as games),
//! * an absolute pointer, like the tablet device virtual machines expose.
//!   The host sends absolute cursor positions through it, so the cursor is
//!   placed exactly, is immune to the local pointer-acceleration settings,
//!   and we never have to ask the compositor where the cursor is.

use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    RelativeAxisCode, UinputAbsSetup,
};
use yzendris_core::geom::Rect;
use yzendris_core::proto::SCROLL_NOTCH;

use super::{PRODUCT, VENDOR};
use crate::platform::Inject;

pub const KEYBOARD_NAME: &str = "Yzendris Keyboard";
const MOUSE_NAME: &str = "Yzendris Mouse";
const POINTER_NAME: &str = "Yzendris Pointer";

/// Range of the absolute axes. Compositors map it onto the whole desktop.
const ABS_MAX: i32 = 0xffff;
/// Highest key code declared on the keyboard (`KEY_MICMUTE`).
const LAST_KEY: u16 = 248;
const BTN_FIRST: u16 = 0x110;
const BTN_LAST: u16 = 0x117;

pub struct Uinput {
    keyboard: VirtualDevice,
    mouse: VirtualDevice,
    pointer: VirtualDevice,
    /// Wheel motion not yet amounting to a whole notch.
    wheel_rest: (i32, i32),
}

fn id(product_offset: u16) -> InputId {
    InputId::new(BusType::BUS_VIRTUAL, VENDOR, PRODUCT + product_offset, 2)
}

fn builder(
    name: &'static str,
    offset: u16,
) -> Result<evdev::uinput::VirtualDeviceBuilder<'static>> {
    Ok(VirtualDevice::builder()
        .context("opening /dev/uinput (run `yzendris install` to grant access)")?
        .name(name)
        .input_id(id(offset)))
}

impl Uinput {
    pub fn create() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        (1..=LAST_KEY).for_each(|c| keys.insert(KeyCode(c)));
        let keyboard = builder(KEYBOARD_NAME, 0)?.with_keys(&keys)?.build()?;

        let mut buttons = AttributeSet::<KeyCode>::new();
        (BTN_FIRST..=BTN_LAST).for_each(|c| buttons.insert(KeyCode(c)));
        let mut axes = AttributeSet::<RelativeAxisCode>::new();
        for axis in [
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
        ] {
            axes.insert(axis);
        }
        let mouse = builder(MOUSE_NAME, 1)?
            .with_keys(&buttons)?
            .with_relative_axes(&axes)?
            .build()?;

        // Absolute axes plus mouse buttons (and no touch/tool buttons) is what
        // makes the input stack classify this as an absolute *mouse* rather
        // than a touchscreen or tablet.
        let mut left = AttributeSet::<KeyCode>::new();
        left.insert(KeyCode(BTN_FIRST));
        let axis = |code| UinputAbsSetup::new(code, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0));
        let pointer = builder(POINTER_NAME, 2)?
            .with_keys(&left)?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y))?
            .build()?;

        tracing::info!("virtual keyboard, mouse and pointer ready");
        Ok(Self {
            keyboard,
            mouse,
            pointer,
            wheel_rest: (0, 0),
        })
    }
}

fn emit(device: &mut VirtualDevice, events: &[InputEvent]) {
    // `emit` appends the SYN_REPORT that commits the batch.
    if let Err(e) = device.emit(events) {
        tracing::debug!("uinput write failed: {e}");
    }
}

fn scale(v: i32, origin: i32, size: i32) -> i32 {
    if size <= 1 {
        return 0;
    }
    let pos = i64::from(v - origin).clamp(0, i64::from(size - 1));
    (pos * i64::from(ABS_MAX) / i64::from(size - 1)) as i32
}

impl Inject for Uinput {
    fn pointer_abs(&mut self, x: i32, y: i32, desktop: Rect) {
        let abs = EventType::ABSOLUTE.0;
        emit(
            &mut self.pointer,
            &[
                InputEvent::new(
                    abs,
                    AbsoluteAxisCode::ABS_X.0,
                    scale(x, desktop.x, desktop.w),
                ),
                InputEvent::new(
                    abs,
                    AbsoluteAxisCode::ABS_Y.0,
                    scale(y, desktop.y, desktop.h),
                ),
            ],
        );
    }

    fn pointer_rel(&mut self, dx: i32, dy: i32) {
        let rel = EventType::RELATIVE.0;
        let mut events = Vec::with_capacity(2);
        if dx != 0 {
            events.push(InputEvent::new(rel, RelativeAxisCode::REL_X.0, dx));
        }
        if dy != 0 {
            events.push(InputEvent::new(rel, RelativeAxisCode::REL_Y.0, dy));
        }
        if !events.is_empty() {
            emit(&mut self.mouse, &events);
        }
    }

    fn button(&mut self, code: u16, down: bool) {
        if (BTN_FIRST..=BTN_LAST).contains(&code) {
            emit(
                &mut self.mouse,
                &[InputEvent::new(EventType::KEY.0, code, i32::from(down))],
            );
        }
    }

    fn scroll(&mut self, dx: i32, dy: i32) {
        // Modern libinput reads the high-resolution axes; the classic ones are
        // still sent, a whole notch at a time, for everything else.
        let rel = EventType::RELATIVE.0;
        let mut events = Vec::with_capacity(4);
        let mut axis = |delta: i32, rest: &mut i32, hi: RelativeAxisCode, lo: RelativeAxisCode| {
            if delta == 0 {
                return;
            }
            events.push(InputEvent::new(rel, hi.0, delta));
            *rest += delta;
            let notches = *rest / SCROLL_NOTCH;
            if notches != 0 {
                *rest -= notches * SCROLL_NOTCH;
                events.push(InputEvent::new(rel, lo.0, notches));
            }
        };
        axis(
            dy,
            &mut self.wheel_rest.1,
            RelativeAxisCode::REL_WHEEL_HI_RES,
            RelativeAxisCode::REL_WHEEL,
        );
        axis(
            dx,
            &mut self.wheel_rest.0,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
            RelativeAxisCode::REL_HWHEEL,
        );
        if !events.is_empty() {
            emit(&mut self.mouse, &events);
        }
    }

    fn key(&mut self, code: u16, down: bool) {
        if (1..=LAST_KEY).contains(&code) {
            emit(
                &mut self.keyboard,
                &[InputEvent::new(EventType::KEY.0, code, i32::from(down))],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_scaling_covers_the_whole_range() {
        assert_eq!(scale(0, 0, 1920), 0);
        assert_eq!(scale(1919, 0, 1920), ABS_MAX);
        assert_eq!(scale(-50, 0, 1920), 0);
        assert_eq!(scale(5000, 0, 1920), ABS_MAX);
        // Desktops that start at a negative origin (a screen left of primary).
        assert_eq!(scale(-1920, -1920, 3840), 0);
        assert_eq!(scale(1919, -1920, 3840), ABS_MAX);
        assert_eq!(scale(10, 0, 1), 0);
    }
}
