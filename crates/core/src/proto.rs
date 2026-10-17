//! Wire protocol.
//!
//! Every message is one frame: a little-endian `u32` length followed by a
//! [`postcard`]-encoded [`Msg`]. Frames travel inside a mutually
//! authenticated TLS 1.3 stream; the first frame in each direction is always
//! [`Msg::Hello`].

use serde::{Deserialize, Serialize};

use crate::geom::Rect;

/// Bumped on any incompatible change to [`Msg`].
pub const PROTOCOL_VERSION: u16 = 2;

/// Upper bound for one encoded frame. Anything larger is a protocol error and
/// drops the connection, so a hostile or buggy peer cannot make us allocate
/// unbounded memory.
pub const MAX_FRAME: usize = 1024 * 1024 + 4096;

/// Largest clipboard text we transmit (bytes of UTF-8).
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// Payload size of one [`Msg::FileData`] frame. Small enough that input events
/// queued behind it are delayed by well under a millisecond on a LAN.
pub const FILE_CHUNK_BYTES: usize = 32 * 1024;

/// Most files accepted in a single clipboard transfer.
pub const MAX_FILES_PER_BATCH: u32 = 512;

/// Scroll distance of one wheel notch, matching both `WHEEL_DELTA` on Windows
/// and `REL_WHEEL_HI_RES` on Linux.
pub const SCROLL_NOTCH: i32 = 120;

/// Linux `BTN_*` codes used for mouse buttons on the wire.
pub mod button {
    pub const LEFT: u16 = 0x110;
    pub const RIGHT: u16 = 0x111;
    pub const MIDDLE: u16 = 0x112;
    pub const SIDE: u16 = 0x113;
    pub const EXTRA: u16 = 0x114;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Os {
    Linux,
    Windows,
    Other,
}

impl Os {
    pub const fn current() -> Self {
        if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Other
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u16,
    /// Friendly machine name shown to the user on the other side.
    pub name: String,
    pub os: Os,
    /// The sender's screens in its own logical pixel space. Only meaningful
    /// from a client; a host sends an empty list.
    pub screens: Vec<Rect>,
    /// Client only: this connection is a pairing attempt, not a session.
    pub pairing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    /// The peer speaks a different protocol version.
    Version,
    /// The certificate is not paired with this host and pairing is closed.
    NotPaired,
    /// Pairing failed: wrong code or tampered handshake.
    BadCode,
    /// The host is not accepting more connections right now.
    Busy,
    /// The client is paired but disabled in the host's settings.
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    // ── Handshake ───────────────────────────────────────────────────────────
    Hello(Hello),
    /// Host → client: the session is authenticated and live.
    Welcome,
    /// Host → client: the connection is refused; it closes right after.
    Reject(RejectReason),
    /// Client → host: first SPAKE2 message of a pairing attempt.
    PairStart {
        spake: Vec<u8>,
    },
    /// Host → client: the host's SPAKE2 message.
    PairReply {
        spake: Vec<u8>,
    },
    /// Client → host: proof the client derived the same key.
    PairConfirm {
        mac: [u8; 32],
    },
    /// Host → client: proof the host derived the same key. Pairing is done.
    PairDone {
        mac: [u8; 32],
    },

    // ── Liveness ────────────────────────────────────────────────────────────
    Ping(u32),
    Pong(u32),

    // ── Session control ─────────────────────────────────────────────────────
    /// Client → host: the client's screen layout changed.
    Screens(Vec<Rect>),
    /// Host → client: input now targets you; cursor starts at this position
    /// (client logical pixels). `relative` selects raw relative motion.
    Enter {
        x: i32,
        y: i32,
        relative: bool,
    },
    /// Host → client: input left; release every key and button still held.
    Leave,
    /// Host → client: switch between absolute and raw relative pointer motion.
    Relative(bool),

    // ── Input ───────────────────────────────────────────────────────────────
    /// Absolute cursor position in client logical pixels.
    PointerAbs {
        x: i32,
        y: i32,
    },
    /// Raw relative motion (used while the pointer is locked, e.g. in games).
    PointerRel {
        dx: i32,
        dy: i32,
    },
    /// Mouse button, as a Linux `BTN_*` code.
    Button {
        code: u16,
        down: bool,
    },
    /// Wheel motion in 1/120ths of a notch. Positive `dy` scrolls up,
    /// positive `dx` scrolls right.
    Scroll {
        dx: i32,
        dy: i32,
    },
    /// Keyboard key, as a Linux `KEY_*` code.
    Key {
        code: u16,
        down: bool,
    },

    // ── Clipboard ───────────────────────────────────────────────────────────
    ClipboardText(String),
    /// Start of a batch of copied files; `total` is the summed size in bytes.
    FilesBegin {
        count: u32,
        total: u64,
    },
    FileBegin {
        name: String,
        size: u64,
    },
    FileData(Vec<u8>),
    FileEnd {
        crc32: u32,
    },
    FilesEnd,
}

impl Msg {
    /// Motion messages may be dropped in favour of a newer one when the link
    /// is congested; everything else must be delivered in order.
    pub const fn is_motion(&self) -> bool {
        matches!(self, Msg::PointerAbs { .. })
    }
}

#[derive(Debug)]
pub enum FrameError {
    TooLarge(usize),
    Malformed,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes exceeds the protocol limit"),
            FrameError::Malformed => f.write_str("malformed frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Append one length-prefixed frame for `msg` to `out`.
pub fn encode(msg: &Msg, out: &mut Vec<u8>) -> Result<(), FrameError> {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    let buf = std::mem::take(out);
    *out = postcard::to_extend(msg, buf).map_err(|_| FrameError::Malformed)?;
    let len = out.len() - start - 4;
    if len > MAX_FRAME {
        out.truncate(start);
        return Err(FrameError::TooLarge(len));
    }
    out[start..start + 4].copy_from_slice(&(len as u32).to_le_bytes());
    Ok(())
}

/// Validate a frame's length prefix.
pub fn frame_len(prefix: [u8; 4]) -> Result<usize, FrameError> {
    let len = u32::from_le_bytes(prefix) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    Ok(len)
}

/// Decode the body of one frame.
pub fn decode(body: &[u8]) -> Result<Msg, FrameError> {
    postcard::from_bytes(body).map_err(|_| FrameError::Malformed)
}

/// Truncate clipboard text to the wire limit on a character boundary.
pub fn clamp_clipboard(mut text: String) -> String {
    if text.len() > MAX_CLIPBOARD_BYTES {
        let mut end = MAX_CLIPBOARD_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(msg: Msg) {
        let mut buf = Vec::new();
        encode(&msg, &mut buf).unwrap();
        let len = frame_len(buf[..4].try_into().unwrap()).unwrap();
        assert_eq!(len, buf.len() - 4);
        assert_eq!(decode(&buf[4..]).unwrap(), msg);
    }

    #[test]
    fn frames_round_trip() {
        round_trip(Msg::Hello(Hello {
            version: PROTOCOL_VERSION,
            name: "escritorio".into(),
            os: Os::Linux,
            screens: vec![Rect::new(0, 0, 1920, 1080)],
            pairing: false,
        }));
        round_trip(Msg::PointerAbs { x: -5, y: 70000 });
        round_trip(Msg::Key {
            code: 125,
            down: true,
        });
        round_trip(Msg::FileData(vec![7; FILE_CHUNK_BYTES]));
        round_trip(Msg::ClipboardText("ñ".repeat(MAX_CLIPBOARD_BYTES / 2)));
    }

    #[test]
    fn input_frames_are_tiny() {
        let mut buf = Vec::new();
        encode(&Msg::PointerAbs { x: 1919, y: 1079 }, &mut buf).unwrap();
        assert!(buf.len() <= 10, "pointer frame is {} bytes", buf.len());
    }

    #[test]
    fn rejects_oversized_and_garbage() {
        assert!(frame_len((MAX_FRAME as u32 + 1).to_le_bytes()).is_err());
        assert!(frame_len([0; 4]).is_err());
        assert!(decode(&[0xff, 0xff, 0xff]).is_err());
        let mut buf = Vec::new();
        assert!(encode(&Msg::FileData(vec![0; MAX_FRAME + 1]), &mut buf).is_err());
        assert!(buf.is_empty());
    }

    #[test]
    fn clipboard_clamp_respects_char_boundaries() {
        let text = "é".repeat(MAX_CLIPBOARD_BYTES);
        let clamped = clamp_clipboard(text);
        assert!(clamped.len() <= MAX_CLIPBOARD_BYTES);
        assert!(clamped.chars().all(|c| c == 'é'));
    }
}
