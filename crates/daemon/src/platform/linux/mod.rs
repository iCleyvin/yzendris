//! Linux back-end.
//!
//! Input goes through the kernel (`/dev/uinput` to synthesize, evdev to
//! capture) rather than through any one display server, so it behaves the
//! same on GNOME, KDE Plasma, Hyprland, Sway, COSMIC and X11.

mod capture;
mod hyprland;
mod inject;
mod probe;
mod screens;

use anyhow::Result;

pub use capture::Capture;
pub use screens::screens;

use super::Inject;

pub const LOG_TO_FILE: bool = false;

/// USB-style identity of our virtual devices; also how capture recognises
/// (and skips) them. 0x1209 is the pid.codes open-source vendor ID.
pub(crate) const VENDOR: u16 = 0x1209;
pub(crate) const PRODUCT: u16 = 0x7947;

/// A systemd user service may start without the session's display variables.
/// Recover them so clipboard and screen detection can reach the compositor.
pub fn prepare() {
    let Some(runtime) = runtime_dir() else { return };
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        if let Some(name) = screens::wayland_socket_name(&runtime) {
            // SAFETY: called first thing in `main`, before any thread exists.
            unsafe { std::env::set_var("WAYLAND_DISPLAY", name) };
        }
    }
    if std::env::var_os("DISPLAY").is_none() && std::path::Path::new("/tmp/.X11-unix/X0").exists() {
        // SAFETY: as above.
        unsafe { std::env::set_var("DISPLAY", ":0") };
    }
}

pub(crate) fn runtime_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        // SAFETY: getuid has no preconditions.
        .or_else(|| Some(format!("/run/user/{}", unsafe { libc::getuid() }).into()))
        .filter(|p| p.is_dir())
}

pub fn local_clock() -> (u32, u32, u32) {
    // SAFETY: `time` accepts null; `localtime_r` writes only into `tm`.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        (tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }
}

pub fn injector(keyboard_layout: &str) -> Result<Box<dyn Inject>> {
    let devices = inject::Uinput::create()?;
    hyprland::fix_keyboard_layout(keyboard_layout.to_owned());
    Ok(Box::new(devices))
}

/// Is a graphical session reachable right now?
pub fn display_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var_os("DISPLAY").is_some()
        || runtime_dir().is_some_and(|r| screens::wayland_socket_name(&r).is_some())
        || std::path::Path::new("/tmp/.X11-unix/X0").exists()
}

/// Whether edge switching can work on this desktop when it acts as host.
pub fn cursor_visible_to_host() -> bool {
    probe::CursorProbe::detect().is_some()
}
