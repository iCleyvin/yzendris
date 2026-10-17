//! Reading (and setting) the local cursor position on a Linux host.
//!
//! Wayland deliberately has no protocol for this, so it only works where the
//! desktop offers its own way: Hyprland's IPC, or an X11 session. Elsewhere
//! (GNOME and KDE on Wayland) the host cannot tell when the cursor reaches a
//! screen edge, and machines are switched with keyboard shortcuts instead.

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use yzendris_core::Point;

use super::hyprland;

pub enum CursorProbe {
    Hyprland,
    X11 {
        conn: Box<RustConnection>,
        root: u32,
    },
}

impl CursorProbe {
    pub fn detect() -> Option<Self> {
        if hyprland::running() && hyprland::cursor().is_some() {
            tracing::info!("edge switching uses Hyprland's cursor position");
            return Some(Self::Hyprland);
        }
        // Under Wayland an X server (XWayland) only knows the cursor while it
        // is over an X11 window, so X11 is trusted only in a real X session.
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
        if !wayland {
            if let Ok((conn, screen)) = x11rb::connect(None) {
                let root = conn.setup().roots.get(screen)?.root;
                tracing::info!("edge switching uses the X11 cursor position");
                return Some(Self::X11 {
                    conn: Box::new(conn),
                    root,
                });
            }
        }
        None
    }

    pub fn position(&self) -> Option<Point> {
        match self {
            Self::Hyprland => hyprland::cursor(),
            Self::X11 { conn, root } => {
                let reply = conn.query_pointer(*root).ok()?.reply().ok()?;
                Some(Point::new(i32::from(reply.root_x), i32::from(reply.root_y)))
            }
        }
    }

    pub fn warp(&self, to: Point) {
        match self {
            Self::Hyprland => hyprland::warp(to),
            Self::X11 { conn, root } => {
                let (x, y) = (
                    to.x.clamp(0, i32::from(i16::MAX)) as i16,
                    to.y.clamp(0, i32::from(i16::MAX)) as i16,
                );
                let _ = conn.warp_pointer(x11rb::NONE, *root, 0, 0, 0, 0, x, y);
                let _ = conn.flush();
            }
        }
    }
}
