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
}

// ─── Framing: 4-byte LE length prefix + bincode payload ──────────────────────

/// Maximum allowed frame size (64 KiB) — protects against corrupt streams.
const MAX_FRAME: u32 = 65536;

/// Write one `Event` as a length-prefixed frame to an `AsyncWrite`.
pub async fn send_event<W: AsyncWrite + Unpin>(writer: &mut W, event: &Event) -> anyhow::Result<()> {
    let payload = bincode::serialize(event).context("bincode serialize")?;
    let len = payload.len() as u32;
    writer.write_u32_le(len).await.context("write len")?;
    writer.write_all(&payload).await.context("write payload")?;
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
