//! Everything that talks to the operating system: reading physical input,
//! synthesizing input, enumerating screens.
//!
//! Each supported OS provides the same items; the rest of the daemon is
//! written against them:
//!
//! * `prepare()` — one-time process setup.
//! * `screens()` — the local monitors.
//! * `injector(layout)` — a [`Inject`] that synthesizes input (client role).
//! * `Capture` — observes and intercepts physical input (host role).
//! * `local_clock()` — wall-clock time for log lines.

use std::net::{IpAddr, UdpSocket};

use yzendris_core::geom::Rect;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(any(target_os = "linux", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "linux", windows)))]
pub use unsupported::*;

/// Synthesizes keyboard and mouse input on this machine.
pub trait Inject: Send {
    /// Move the cursor to `(x, y)`, given in the pixel space of `desktop`
    /// (the bounding box of this machine's screens).
    fn pointer_abs(&mut self, x: i32, y: i32, desktop: Rect);
    /// Raw relative motion, for applications that grab the pointer.
    fn pointer_rel(&mut self, dx: i32, dy: i32);
    /// Mouse button, as a Linux `BTN_*` code.
    fn button(&mut self, code: u16, down: bool);
    /// Wheel motion in 1/120 notches.
    fn scroll(&mut self, dx: i32, dy: i32);
    /// Key, as a Linux `KEY_*` code.
    fn key(&mut self, code: u16, down: bool);
}

/// Receives physical input on the host. Every method returns whether the
/// event must be hidden from the local desktop. Called from OS input threads:
/// implementations must be quick and never block.
pub trait Sink: Send + Sync + 'static {
    /// Pointer motion; `at` is the resulting cursor position when known.
    fn motion(&self, dx: f64, dy: f64, at: Option<yzendris_core::Point>) -> bool;
    fn button(&self, code: u16, down: bool) -> bool;
    fn scroll(&self, dx: i32, dy: i32) -> bool;
    fn key(&self, code: u16, down: bool) -> bool;
    /// The platform lost track of which keys are held.
    fn reset(&self);
}

/// This machine's address on the local network, found by asking the OS which
/// interface it would route through (no packet is sent).
pub fn lan_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}
