/// Cursor-position tracker: knows (approximately) where the injected cursor
/// is on the client screen so we can hand control back to the Windows server
/// when the user pushes past an edge.
///
/// Position is estimated by accumulating MouseMove deltas and periodically
/// resyncing against the real cursor to cancel any drift introduced by
/// pointer acceleration.  The real cursor is read by a background thread
/// (`hyprctl cursorpos` forks a process, which must NOT run on the async
/// reactor) that publishes the latest sample into shared atomics; the hot
/// path only reads those atomics.
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;
use yzendris_protocol::{Event, EDGE_BOTTOM, EDGE_LEFT, EDGE_RIGHT, EDGE_TOP};

use crate::platform;

/// Pixels of accumulated push past an edge before we hand control back.
/// Small enough to feel instant, large enough to ignore incidental contact.
const OVERSHOOT_THRESHOLD: f64 = 25.0;

/// How often the hot path adopts the latest background cursor sample.
const RESYNC_INTERVAL: Duration = Duration::from_millis(150);

/// How often the background thread samples the real cursor while capturing.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(70);

/// After sending an `EdgeReached`, wait this long before sending another.
/// The server replies with `CaptureEnd` only when the edge actually leads
/// somewhere; if it doesn't (e.g. the top/bottom edge of a between-monitors
/// laptop), we stay active and the user can push a different edge instead of
/// getting stuck. The cooldown just avoids flooding the server meanwhile.
const EDGE_COOLDOWN: Duration = Duration::from_millis(250);

/// Shared snapshot written by the sampler thread, read by the hot path.
#[derive(Default)]
struct Sampler {
    x: AtomicI32,
    y: AtomicI32,
    valid: AtomicBool,
    /// Sample only while a capture session is active.
    active: AtomicBool,
}

pub struct CursorTracker {
    /// Focused monitor rect in global logical coords: (x, y, w, h).
    rect: (i32, i32, i32, i32),
    /// Estimated cursor position (global coords).
    x: f64,
    y: f64,
    /// Accumulated push past `overshoot_edge`.
    overshoot: f64,
    overshoot_edge: u8,
    /// Only track while a capture session is active.
    active: bool,
    last_sync: Instant,
    /// When we last sent an `EdgeReached` (for the resend cooldown).
    last_edge_sent: Option<Instant>,
    sampler: Arc<Sampler>,
}

impl CursorTracker {
    pub fn new() -> Self {
        let sampler = Arc::new(Sampler::default());

        // Background sampler: forks `hyprctl` off the async reactor.
        let s = sampler.clone();
        std::thread::Builder::new()
            .name("yzendris-cursor-sampler".into())
            .spawn(move || loop {
                if s.active.load(Ordering::Relaxed) {
                    if let Some((x, y)) = platform::cursor_pos() {
                        s.x.store(x, Ordering::Relaxed);
                        s.y.store(y, Ordering::Relaxed);
                        s.valid.store(true, Ordering::Relaxed);
                    }
                }
                std::thread::sleep(SAMPLE_INTERVAL);
            })
            .ok();

        Self {
            rect: (0, 0, 1920, 1080),
            x: 0.0,
            y: 0.0,
            overshoot: 0.0,
            overshoot_edge: EDGE_RIGHT,
            active: false,
            last_sync: Instant::now(),
            last_edge_sent: None,
            sampler,
        }
    }

    /// CaptureStart: refresh monitor geometry and the current cursor position.
    /// These one-shot `hyprctl` calls happen at the capture transition, not in
    /// the per-event hot path, so blocking here is acceptable.
    pub fn on_capture_start(&mut self) {
        if let Some(rect) = platform::focused_monitor_rect() {
            self.rect = rect;
        }
        if let Some((cx, cy)) = platform::cursor_pos() {
            self.x = cx as f64;
            self.y = cy as f64;
            self.sampler.x.store(cx, Ordering::Relaxed);
            self.sampler.y.store(cy, Ordering::Relaxed);
            self.sampler.valid.store(true, Ordering::Relaxed);
        } else {
            self.sampler.valid.store(false, Ordering::Relaxed);
        }
        self.overshoot = 0.0;
        self.active = true;
        self.last_sync = Instant::now();
        self.last_edge_sent = None;
        self.sampler.active.store(true, Ordering::Relaxed);
        tracing::debug!("tracker: capture start, rect={:?}", self.rect);
    }

    /// EnterAt: warp the cursor to the entry edge at `frac` along it.
    pub fn on_enter_at(&mut self, edge: u8, frac: f32) {
        // Re-read geometry in case focus moved between CaptureStart and EnterAt.
        if let Some(rect) = platform::focused_monitor_rect() {
            self.rect = rect;
        }
        let (rx, ry, rw, rh) = self.rect;
        let frac = frac.clamp(0.0, 1.0) as f64;
        let (tx, ty) = match edge {
            EDGE_LEFT => (rx + 1, ry + (frac * (rh - 1) as f64) as i32),
            EDGE_RIGHT => (rx + rw - 2, ry + (frac * (rh - 1) as f64) as i32),
            EDGE_TOP => (rx + (frac * (rw - 1) as f64) as i32, ry + 1),
            EDGE_BOTTOM => (rx + (frac * (rw - 1) as f64) as i32, ry + rh - 2),
            _ => return,
        };
        platform::move_cursor(tx, ty);
        self.x = tx as f64;
        self.y = ty as f64;
        self.sampler.x.store(tx, Ordering::Relaxed);
        self.sampler.y.store(ty, Ordering::Relaxed);
        self.sampler.valid.store(true, Ordering::Relaxed);
        self.overshoot = 0.0;
        tracing::debug!("tracker: enter at edge {edge} → ({tx},{ty})");
    }

    pub fn on_capture_end(&mut self) {
        self.active = false;
        self.sampler.active.store(false, Ordering::Relaxed);
    }

    /// MouseMove: update the estimate; when the user pushes past an edge hard
    /// enough, emit `EdgeReached` so the server takes the cursor back.
    pub fn on_mouse_move(&mut self, dx: i32, dy: i32, out_tx: &UnboundedSender<Event>) {
        if !self.active {
            return;
        }

        // Adopt the latest background sample to cancel accel drift — but never
        // while mid-overshoot, or the resync would reset the estimate back
        // inside the screen and the edge would never fire.
        if self.overshoot == 0.0
            && self.last_sync.elapsed() >= RESYNC_INTERVAL
            && self.sampler.valid.load(Ordering::Relaxed)
        {
            self.x = self.sampler.x.load(Ordering::Relaxed) as f64;
            self.y = self.sampler.y.load(Ordering::Relaxed) as f64;
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

        // Keep accumulating even when the dominant edge flips between axes
        // (a diagonal push toward a corner alternates RIGHT/BOTTOM); resetting
        // on every flip would stop the threshold from ever being reached.
        self.overshoot_edge = edge;
        self.overshoot += excess;

        if self.overshoot >= OVERSHOOT_THRESHOLD {
            // While a game holds the pointer for camera/character motion (cursor
            // hidden / relative-motion mode), never hand control back to the
            // host: the user is steering, not trying to leave. Edge hand-off
            // resumes the moment the game releases the pointer (pause/menu →
            // cursor visible again). Checked only here (when actually pushing an
            // edge), so it's not in the per-move hot path.
            if crate::platform::pointer_locked() {
                self.overshoot = 0.0;
                return;
            }

            // Respect the resend cooldown: we keep tracking after sending so
            // that if this edge leads nowhere (server doesn't reply with
            // CaptureEnd) the user can still escape via another edge. Only the
            // server's CaptureEnd actually deactivates us (on_capture_end).
            let now = Instant::now();
            let cooling = self
                .last_edge_sent
                .is_some_and(|t| now.duration_since(t) < EDGE_COOLDOWN);
            if !cooling {
                let frac = match edge {
                    EDGE_LEFT | EDGE_RIGHT => ((self.y - min_y) / (max_y - min_y).max(1.0)) as f32,
                    _ => ((self.x - min_x) / (max_x - min_x).max(1.0)) as f32,
                };
                tracing::info!("edge {edge} reached (frac {frac:.2}) — requesting return");
                let _ = out_tx.send(Event::EdgeReached { edge, frac });
                self.last_edge_sent = Some(now);
            }
            self.overshoot = 0.0;
        }
    }
}
