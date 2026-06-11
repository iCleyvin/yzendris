use std::io;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// All events that travel from the Windows server to the Linux client.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Event {
    /// Key pressed. `keycode` is an evdev KEY_* code (AT scan code set 1).
    KeyPress { keycode: u16 },
    /// Key released.
    KeyRelease { keycode: u16 },
    /// Mouse moved (relative). dx/dy in pixels.
    MouseMove { dx: i32, dy: i32 },
    /// Mouse button pressed or released. `btn` is an evdev BTN_* code.
    MouseButton { btn: u16, pressed: bool },
    /// Wheel scroll. dy: vertical (positive = up), dx: horizontal.
    Scroll { dx: i32, dy: i32 },
    /// Server has captured input; client should start injecting.
    CaptureStart,
    /// Server released capture (user pressed release combo).
    CaptureEnd,
    /// Periodic heartbeat — client restarts if it stops arriving.
    Heartbeat,
    /// Sync currently-held keys so client can release stuck keys on peer drop.
    SyncKeys { keycodes_down: Vec<u16> },
    /// Clipboard text, sent in both directions:
    ///   Server→Client on CaptureStart  (Windows clipboard → Linux)
    ///   Client→Server on CaptureEnd    (Linux clipboard → Windows)
    ClipboardText { text: String },
    /// Server→Client right after CaptureStart: warp the client cursor to the
    /// screen edge the mouse came in through. `edge` is the client-screen edge
    /// (cursor appears AT this edge): 0=right 1=left 2=bottom 3=top.
    /// `frac` is the position along that edge (0.0–1.0).
    EnterAt { edge: u8, frac: f32 },
    /// Client→Server: the client cursor pushed past this edge of its screen —
    /// the server should release capture and place the Windows cursor on the
    /// matching side. Same edge encoding as `EnterAt`. `frac` is the position
    /// along the edge so the server can place the cursor at the same height.
    EdgeReached { edge: u8, frac: f32 },
    /// Client→Server: the client's screen resolution, sent once on connect so
    /// the host GUI can show it.
    ClientInfo { width: i32, height: i32 },
}

/// Edge encoding shared by `EnterAt` / `EdgeReached`.
pub const EDGE_RIGHT: u8 = 0;
pub const EDGE_LEFT: u8 = 1;
pub const EDGE_BOTTOM: u8 = 2;
pub const EDGE_TOP: u8 = 3;

// ─── Framing: 4-byte LE length prefix + bincode payload ──────────────────────

/// Maximum allowed frame size (64 KiB) — protects against corrupt streams.
const MAX_FRAME: u32 = 65536;

/// Write one `Event` as a length-prefixed frame to an `AsyncWrite`.
pub async fn send_event<W: AsyncWrite + Unpin>(writer: &mut W, event: &Event) -> anyhow::Result<()> {
    let payload = bincode::serialize(event).context("bincode serialize")?;
    let len = payload.len() as u32;
    // One buffer → one write: length prefix and payload go out as a single
    // TCP segment (and single TLS record), so a high-frequency mouse stream
    // doesn't pay two syscalls per event nor risk the prefix and body landing
    // in separate, separately-delayed packets.
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    writer.write_all(&frame).await.context("write frame")?;
    Ok(())
}

/// Read one `Event` from an `AsyncRead`. Returns `None` on clean EOF.
pub async fn recv_event<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<Option<Event>> {
    let len = match reader.read_u32_le().await {
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("read len"),
    };
    if len > MAX_FRAME {
        anyhow::bail!("frame too large: {len} bytes");
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await.context("read payload")?;
    let event: Event = bincode::deserialize(&buf).context("bincode deserialize")?;
    Ok(Some(event))
}
