//! Platform abstraction for the client's injection + cursor backend.
//!
//!   Linux:   uinput virtual device + Hyprland cursor/layout.
//!   Windows: SendInput + Win32 cursor/monitor.
//!
//! The rest of the client (net, tracker, main loop) is platform-agnostic and
//! talks only to the uniform API re-exported here:
//!   - `Injector::new()`, `inject()`, `release_all()`
//!   - `setup(&mut Injector, kb_layout)` (async, one-shot post-create)
//!   - `cursor_pos()`, `focused_monitor_rect()`, `move_cursor()`
//!   - `BACKEND` (label for logging)

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
