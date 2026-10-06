//! Shared building blocks for Yzendris: wire protocol, configuration, peer
//! identity, screen geometry and the focus-switching state machine.
//!
//! Everything here is platform-neutral and free of I/O runtimes so it can be
//! unit-tested in isolation and reused by both the daemon and the GUI.

pub mod config;
pub mod fingerprint;
pub mod geom;
pub mod ipc;
pub mod keymap;
pub mod layout;
pub mod paths;
pub mod proto;
pub mod switch;

pub use fingerprint::Fingerprint;
pub use geom::{Point, Rect, Side};

/// TCP port the host listens on, and UDP port used for LAN discovery.
pub const DEFAULT_PORT: u16 = 7547;

/// Crate version, surfaced in `--version` and the GUI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
