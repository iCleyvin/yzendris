/// Virtual uinput device: creates one device for both keyboard AND mouse,
/// then exposes a single `inject` method that maps protocol Events to kernel
/// input_event writes.
///
/// Uses evdev 0.13 API (uinput module is always available, no feature flag).
/// IMPORTANT: evdev 0.13 does NOT add SYN_REPORT automatically — every batch
/// of events must end with an explicit SYN_REPORT.
use anyhow::{Context, Result};
use evdev::{
    uinput::VirtualDevice,
    AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode,
};
use yzendris_protocol::Event;

/// The kernel device name — must match what `hyprland::apply_layout` targets.
pub const DEVICE_NAME: &str = "yzendris-virtual-kb";

/// EV_SYN / SYN_REPORT — must terminate every event batch.
fn syn_report() -> InputEvent {
    InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)
}

pub struct VirtualKbdMouse {
    inner: VirtualDevice,
}

impl VirtualKbdMouse {
    /// Create the uinput device and register it with the kernel.
    /// Caller should wait ~1-2 s then call `hyprland::apply_layout`.
    pub fn create() -> Result<Self> {
        // ── Keys: evdev codes 1–248 (standard keyboard) ──────────────────────
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in 1u16..=248 {
            keys.insert(KeyCode(code));
        }
        // Mouse buttons: BTN_LEFT=0x110 … BTN_TASK=0x117
        for code in 0x110u16..=0x117 {
            keys.insert(KeyCode(code));
        }

        // ── Relative axes: mouse X/Y + wheel ─────────────────────────────────
        let mut axes = AttributeSet::<RelativeAxisCode>::new();
        axes.insert(RelativeAxisCode::REL_X);
        axes.insert(RelativeAxisCode::REL_Y);
        axes.insert(RelativeAxisCode::REL_WHEEL);
        axes.insert(RelativeAxisCode::REL_HWHEEL);
        axes.insert(RelativeAxisCode::REL_WHEEL_HI_RES);
        axes.insert(RelativeAxisCode::REL_HWHEEL_HI_RES);

        let device = VirtualDevice::builder()
            .context("VirtualDevice::builder (is /dev/uinput writable? check `input` group)")?
            .name(DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_USB, 0x1234, 0x5678, 1))
            .with_keys(&keys)
            .context("VirtualDeviceBuilder::with_keys")?
            .with_relative_axes(&axes)
            .context("VirtualDeviceBuilder::with_relative_axes")?
            .build()
            .context("VirtualDeviceBuilder::build")?;

        Ok(Self { inner: device })
    }

    /// Translate a protocol `Event` into kernel input events and emit them.
    /// Returns `true` if the event was injected, `false` if it was a control event.
    pub fn inject(&mut self, event: &Event) -> Result<bool> {
        let mut evs: Vec<InputEvent> = match event {
            Event::KeyPress { keycode } => {
                vec![InputEvent::new(EventType::KEY.0, *keycode, 1)]
            }
            Event::KeyRelease { keycode } => {
                vec![InputEvent::new(EventType::KEY.0, *keycode, 0)]
            }
            Event::MouseMove { dx, dy } => {
                let mut evs = Vec::new();
                if *dx != 0 {
                    evs.push(InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, *dx));
                }
                if *dy != 0 {
                    evs.push(InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, *dy));
                }
                evs
            }
            Event::MouseButton { btn, pressed } => {
                vec![InputEvent::new(EventType::KEY.0, *btn, if *pressed { 1 } else { 0 })]
            }
            Event::Scroll { dx, dy } => {
                // libinput >= 1.19 requires *_HI_RES events when the device
                // declares them in its capabilities. Sending only REL_WHEEL is
                // silently dropped on modern wlroots compositors. The conventional
                // ratio is 120 hi-res units per wheel notch (matches Windows
                // WHEEL_DELTA), so we just multiply by 120.
                let mut evs = Vec::new();
                if *dy != 0 {
                    evs.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_WHEEL_HI_RES.0,
                        *dy * 120,
                    ));
                    evs.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_WHEEL.0,
                        *dy,
                    ));
                }
                if *dx != 0 {
                    evs.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_HWHEEL_HI_RES.0,
                        *dx * 120,
                    ));
                    evs.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_HWHEEL.0,
                        *dx,
                    ));
                }
                evs
            }
            Event::SyncKeys { keycodes_down } => keycodes_down
                .iter()
                .map(|&code| InputEvent::new(EventType::KEY.0, code, 0))
                .collect(),
            // Control events — not injected into the kernel.
            // ClipboardText is handled separately by main.rs (xdg clipboard set).
            Event::CaptureStart
            | Event::CaptureEnd
            | Event::Heartbeat
            | Event::ClipboardText { .. }
            | Event::EnterAt { .. }
            | Event::EdgeReached { .. } => {
                return Ok(false);
            }
        };

        if evs.is_empty() {
            return Ok(true);
        }

        // Every batch MUST end with SYN_REPORT so the kernel flushes it.
        evs.push(syn_report());
        self.inner.emit(&evs).context("uinput emit")?;
        Ok(true)
    }

    /// Release every key and button (on CaptureEnd / peer disconnect).
    pub fn release_all(&mut self) -> Result<()> {
        let mut evs: Vec<InputEvent> = (1u16..=248)
            .map(|code| InputEvent::new(EventType::KEY.0, code, 0))
            .chain((0x110u16..=0x117).map(|code| InputEvent::new(EventType::KEY.0, code, 0)))
            .collect();
        evs.push(syn_report());
        self.inner.emit(&evs).context("release_all emit")?;
        Ok(())
    }
}
