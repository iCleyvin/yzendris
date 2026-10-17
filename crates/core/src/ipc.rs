//! Messages exchanged between the GUI (or CLI) and the running daemon over
//! the local control endpoint: one JSON object per line, request → response.

use serde::{Deserialize, Serialize};

use crate::config::{Config, Role};
use crate::fingerprint::Fingerprint;
use crate::geom::Rect;
use crate::layout::Screen;
use crate::proto::Os;

/// Longest accepted request or response line.
pub const MAX_LINE: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    GetConfig,
    /// Replace the configuration; the daemon validates, saves and applies it.
    SetConfig {
        config: Box<Config>,
    },
    /// Host: accept one new device for the next couple of minutes.
    PairOpen,
    PairClose,
    /// Client: pair with the host at `addr` using the code it displays.
    PairJoin {
        addr: String,
        code: String,
    },
    /// Client: look for hosts on the local network that are open for pairing.
    Discover,
    /// Forget a paired peer.
    Forget {
        fingerprint: Fingerprint,
    },
    Logs,
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error { message: String },
    Status(Box<Status>),
    Config { config: Box<Config> },
    Hosts { hosts: Vec<FoundHost> },
    Logs { lines: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundHost {
    pub name: String,
    /// `address:port` to connect to.
    pub addr: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub role: Option<Role>,
    pub name: String,
    pub os: Os,
    pub fingerprint: Fingerprint,
    /// This machine's LAN address, if it has one.
    pub address: Option<String>,
    /// Port the host is listening on (host role, once bound).
    pub port: Option<u16>,
    pub screens: Vec<Screen>,
    /// Host: every paired client. Client: the paired host, if any.
    pub peers: Vec<Peer>,
    /// Host: name of the client currently receiving input, if any.
    /// Client: set to the host's name while it is controlling this machine.
    pub active: Option<String>,
    /// Pointer is locked to the current machine (edges disabled).
    pub locked: bool,
    pub pairing: Option<Pairing>,
    pub notices: Vec<Notice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub name: String,
    pub fingerprint: Fingerprint,
    pub link: Link,
    pub address: Option<String>,
    pub os: Option<Os>,
    pub screens: Vec<Rect>,
    pub latency_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    Offline,
    Connecting,
    Online,
    /// The peer refused us: it no longer has this machine paired.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    pub code: String,
    pub seconds_left: u32,
}

/// Something the user should know about; the GUI localizes it by `code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub code: NoticeCode,
    /// Technical detail (an OS error, a path) appended as-is.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeCode {
    /// Cannot create virtual input devices (no access to /dev/uinput).
    InjectDenied,
    /// Cannot read physical input devices (no access to /dev/input).
    CaptureDenied,
    /// No way to read the cursor position on this desktop: edge switching is
    /// off and only keyboard shortcuts switch machines.
    NoCursorProbe,
    /// Could not detect any screen.
    NoScreens,
    /// The listening port is taken or blocked.
    ListenFailed,
    /// The system clipboard is unreachable; clipboard sharing is off.
    ClipboardUnavailable,
}

// ── Blocking client, used by the GUI and the command line ───────────────────

use std::io::{BufRead, BufReader, Read, Write};

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::fs::File;

/// A connection to the running daemon.
pub struct Client {
    reader: BufReader<Stream>,
    writer: Stream,
}

impl Client {
    /// Connect to the daemon; fails quickly when it is not running.
    pub fn connect() -> std::io::Result<Self> {
        Self::connect_to(&crate::paths::ipc_endpoint())
    }

    /// Connect to a daemon at an explicit endpoint.
    pub fn connect_to(endpoint: &std::path::Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        let stream = {
            let s = Stream::connect(endpoint)?;
            // Pairing can legitimately take a while; everything else is instant.
            s.set_read_timeout(Some(std::time::Duration::from_secs(40)))?;
            s
        };
        #[cfg(windows)]
        let stream = {
            // All pipe instances may be momentarily busy; retry briefly.
            let mut attempt = 0;
            loop {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(endpoint)
                {
                    Ok(f) => break f,
                    Err(e) if e.raw_os_error() == Some(231) && attempt < 20 => {
                        attempt += 1;
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    pub fn call(&mut self, request: &Request) -> std::io::Result<Response> {
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        self.writer.flush()?;
        let mut reply = Vec::new();
        (&mut self.reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut reply)?;
        if reply.is_empty() {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(serde_json::from_slice(&reply)?)
    }
}

/// One-shot request to the daemon.
pub fn call(request: &Request) -> std::io::Result<Response> {
    Client::connect()?.call(request)
}
