/// Cursor-position tracker: knows (approximately) where the injected cursor
/// is on the client screen so we can hand control back to the Windows server
/// when the user pushes past an edge.
///
/// Position is estimated by accumulating MouseMove deltas and periodically
/// resyncing against the real cursor (`hyprctl cursorpos`) to cancel any
/// drift introduced by pointer acceleration.
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;
use yzendris_protocol::{Event, EDGE_BOTTOM, EDGE_LEFT, EDGE_RIGHT, EDGE_TOP};

use crate::hyprland;

/// Pixels of accumulated push past an edge before we hand control back.
/// Small enough to feel instant, large enough to ignore incidental contact.
const OVERSHOOT_THRESHOLD: f64 = 25.0;

/// How often we resync the estimated position with the real cursor.
const RESYNC_INTERVAL: Duration = Duration::from_millis(150);

pub struct CursorTracker {
    /// Focused monitor rect in global logical coords: (x, y, w, h).
    rect: (i32, i32, i32, i32),
    /// Estimated cursor position (global coords).
    x: f64,
    y: f64,
    /// Accumulated push past `overshoot_edge`.
    overshoot: f64,
    overshoot_edge: u8,
    /// Only track while a capture session is active and EdgeReached unsent.
    active: bool,
    last_sync: Instant,
}

impl CursorTracker {
    pub fn new() -> Self {
        Self {
            rect: (0, 0, 1920, 1080),
            x: 0.0,
            y: 0.0,
            overshoot: 0.0,
            overshoot_edge: EDGE_RIGHT,
            active: false,
            last_sync: Instant::now(),
        }
    }

    /// CaptureStart: refresh monitor geometry and the current cursor position.
    pub fn on_capture_start(&mut self) {
        if let Some(rect) = hyprland::focused_monitor_rect() {
            self.rect = rect;
        }
        if let Some((cx, cy)) = hyprland::cursor_pos() {
            self.x = cx as f64;
            self.y = cy as f64;
        }
        self.overshoot = 0.0;
        self.active = true;
        self.last_sync = Instant::now();
        tracing::debug!("tracker: capture start, rect={:?}", self.rect);
    }

    /// EnterAt: warp the cursor to the entry edge at `frac` along it.
    pub fn on_enter_at(&mut self, edge: u8, frac: f32) {
        let (rx, ry, rw, rh) = self.rect;
        let frac = frac.clamp(0.0, 1.0) as f64;
        let (tx, ty) = match edge {
            EDGE_LEFT => (rx + 1, ry + (frac * (rh - 1) as f64) as i32),
            EDGE_RIGHT => (rx + rw - 2, ry + (frac * (rh - 1) as f64) as i32),
            EDGE_TOP => (rx + (frac * (rw - 1) as f64) as i32, ry + 1),
            EDGE_BOTTOM => (rx + (frac * (rw - 1) as f64) as i32, ry + rh - 2),
            _ => return,
        };
        hyprland::move_cursor(tx, ty);
        self.x = tx as f64;
        self.y = ty as f64;
        self.overshoot = 0.0;
        tracing::debug!("tracker: enter at edge {edge} → ({tx},{ty})");
    }

    pub fn on_capture_end(&mut self) {
        self.active = false;
    }

    /// MouseMove: update the estimate; when the user pushes past an edge hard
    /// enough, emit `EdgeReached` so the server takes the cursor back.
    pub fn on_mouse_move(&mut self, dx: i32, dy: i32, out_tx: &UnboundedSender<Event>) {
        if !self.active {
            return;
        }

        // Periodic resync against the real cursor (cancels accel drift).
        if self.last_sync.elapsed() >= RESYNC_INTERVAL {
            if let Some((cx, cy)) = hyprland::cursor_pos() {
                self.x = cx as f64;
                self.y = cy as f64;
            }
            self.last_sync = Instant::now();
        }

        self.x += dx as f64;
        self.y += dy as f64;

        let (rx, ry, rw, rh) = self.rect;
        let (min_x, max_x) = (rx as f64, (rx + rw - 1) as f64);
        let (min_y, max_y) = (ry as f64, (ry + rh - 1) as f64);

        // Excess past each edge this event; clamp the estimate back inside.
        let mut excess = 0.0;
        let mut edge = u8::MAX;
        if self.x < min_x {
            excess = min_x - self.x;
            edge = EDGE_LEFT;
            self.x = min_x;
        } else if self.x > max_x {
            excess = self.x - max_x;
            edge = EDGE_RIGHT;
            self.x = max_x;
        }
        if self.y < min_y {
            if min_y - self.y > excess {
                excess = min_y - self.y;
                edge = EDGE_TOP;
            }
            self.y = min_y;
        } else if self.y > max_y {
            if self.y - max_y > excess {
                excess = self.y - max_y;
                edge = EDGE_BOTTOM;
            }
            self.y = max_y;
        }

        if edge == u8::MAX {
            // Back inside the screen — forget any partial push.
            self.overshoot = 0.0;
            return;
        }

        if edge != self.overshoot_edge {
            self.overshoot = 0.0;
            self.overshoot_edge = edge;
        }
        self.overshoot += excess;

        if self.overshoot >= OVERSHOOT_THRESHOLD {
            let frac = match edge {
                EDGE_LEFT | EDGE_RIGHT => ((self.y - min_y) / (max_y - min_y).max(1.0)) as f32,
                _ => ((self.x - min_x) / (max_x - min_x).max(1.0)) as f32,
            };
            tracing::info!("edge {edge} reached (frac {frac:.2}) — handing control back");
            let _ = out_tx.send(Event::EdgeReached { edge, frac });
            self.active = false;
            self.overshoot = 0.0;
        }
    }
}
