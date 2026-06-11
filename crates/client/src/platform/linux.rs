//! Linux backend: uinput injection + Hyprland cursor/layout.
//! Thin adapter over the existing `uinput` and `hyprland` modules (unchanged).
use anyhow::Result;
use yzendris_protocol::Event;

pub use crate::hyprland::{cursor_pos, focused_monitor_rect, move_cursor};

pub const BACKEND: &str = "uinput/Hyprland";

/// Whether a game currently holds the pointer for relative-motion (camera) use.
/// Games on the Linux client grab the mouse via Wayland pointer constraints,
/// which aren't exposed by `hyprctl`, so we can't yet detect this from outside
/// the compositor — always report `false` (edge hand-off behaves as before).
/// Revisit if Linux gaming needs the same lock-to-client behaviour as Windows.
pub fn pointer_locked() -> bool {
    false
}

/// The client's screen resolution (logical), for reporting to the host.
pub fn screen_size() -> Option<(i32, i32)> {
    crate::hyprland::focused_monitor_rect().map(|(_, _, w, h)| (w, h))
}

pub struct Injector {
    dev: crate::uinput::VirtualKbdMouse,
}

impl Injector {
    pub fn new() -> Result<Self> {
        Ok(Self {
            dev: crate::uinput::VirtualKbdMouse::create()?,
        })
    }

    pub fn inject(&mut self, event: &Event) -> Result<bool> {
        self.dev.inject(event)
    }

    pub fn release_all(&mut self) -> Result<()> {
        self.dev.release_all()
    }
}

/// After the uinput device is created, wait for libinput/Hyprland to register
/// it and apply the keyboard layout — without this Hyprland doesn't recognise
/// modifiers in its bind system (see CLAUDE.md).
pub async fn setup(_inj: &mut Injector, kb_layout: &str) {
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let layout = if kb_layout.is_empty() {
        crate::hyprland::detect_layout()
    } else {
        kb_layout.to_owned()
    };
    if let Err(e) = crate::hyprland::apply_layout(crate::uinput::DEVICE_NAME, &layout) {
        tracing::warn!("apply_layout: {e}");
    }
    // Flat accel so libinput doesn't re-accelerate the host's already-
    // accelerated deltas (otherwise the cursor feels erratic/non-linear).
    crate::hyprland::apply_pointer_settings(crate::uinput::DEVICE_NAME);
}
