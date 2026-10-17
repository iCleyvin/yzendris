//! Clipboard access.
//!
//! The system clipboard is owned by one long-lived thread: on Linux the
//! process that sets the clipboard must stay around to serve its contents,
//! and the underlying APIs are blocking.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use yzendris_core::proto::{clamp_clipboard, Msg};

use crate::files::{signature, FileSig, Outgoing};

enum Cmd {
    GetText(oneshot::Sender<Option<String>>),
    SetText(String),
    GetFiles(oneshot::Sender<Vec<PathBuf>>),
    SetFiles(Vec<PathBuf>),
}

#[derive(Clone)]
pub struct Clipboard {
    tx: mpsc::Sender<Cmd>,
    broken: Arc<AtomicBool>,
}

impl Clipboard {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let broken = Arc::new(AtomicBool::new(false));
        let flag = broken.clone();
        let spawned = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let mut board: Option<arboard::Clipboard> = None;
                let mut last_failure: Option<Instant> = None;
                for cmd in rx {
                    if board.is_none()
                        && last_failure.is_none_or(|t| t.elapsed() > Duration::from_secs(5))
                    {
                        match arboard::Clipboard::new() {
                            Ok(b) => board = Some(b),
                            Err(e) => {
                                tracing::debug!("clipboard unavailable: {e}");
                                last_failure = Some(Instant::now());
                            }
                        }
                    }
                    flag.store(board.is_none(), Ordering::Relaxed);
                    match cmd {
                        Cmd::GetText(reply) => {
                            let text = board.as_mut().and_then(|b| b.get_text().ok());
                            let _ = reply.send(text.filter(|t| !t.is_empty()));
                        }
                        Cmd::SetText(text) => {
                            if let Some(Err(e)) = board.as_mut().map(|b| b.set_text(text)) {
                                tracing::debug!("could not set the clipboard: {e}");
                            }
                        }
                        Cmd::GetFiles(reply) => {
                            let files = board.as_mut().and_then(|b| b.get().file_list().ok());
                            let _ = reply.send(files.unwrap_or_default());
                        }
                        Cmd::SetFiles(files) => {
                            if let Some(Err(e)) = board.as_mut().map(|b| b.set().file_list(&files))
                            {
                                tracing::debug!("could not put files on the clipboard: {e}");
                            }
                        }
                    }
                }
            });
        if spawned.is_err() {
            broken.store(true, Ordering::Relaxed);
        }
        Self { tx, broken }
    }

    /// True when the last attempt to reach the system clipboard failed.
    pub fn broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }

    pub async fn text(&self) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::GetText(tx)).ok()?;
        rx.await.ok().flatten()
    }

    pub fn set_text(&self, text: String) {
        let _ = self.tx.send(Cmd::SetText(text));
    }

    pub async fn files(&self) -> Vec<PathBuf> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::GetFiles(tx)).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub fn set_files(&self, files: Vec<PathBuf>) {
        let _ = self.tx.send(Cmd::SetFiles(files));
    }
}

fn digest(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// What one end of a link knows to be on *both* clipboards already, so the
/// same content is not bounced back and forth.
#[derive(Default)]
pub struct ClipSync {
    text: Option<u64>,
    files: Option<FileSig>,
}

/// What to send to the peer after a focus change.
pub struct Offer {
    pub text: Option<Msg>,
    pub files: Option<Outgoing>,
}

impl ClipSync {
    /// Compare the local clipboard with what the peer already has.
    pub async fn offer(&mut self, clip: &Clipboard, files: bool, max_bytes: u64) -> Offer {
        let mut offer = Offer {
            text: None,
            files: None,
        };
        if files {
            let paths = clip.files().await;
            if !paths.is_empty() {
                let sig = signature(&paths);
                if self.files.as_ref() != Some(&sig) {
                    offer.files = Outgoing::prepare(&paths, max_bytes);
                    self.files = Some(sig);
                }
                // A file selection also exposes its paths as text; that text
                // is meaningless on another machine.
                return offer;
            }
        }
        if let Some(text) = clip.text().await {
            let text = clamp_clipboard(text);
            let d = digest(&text);
            if self.text != Some(d) {
                self.text = Some(d);
                offer.text = Some(Msg::ClipboardText(text));
            }
        }
        offer
    }

    pub fn accept_text(&mut self, clip: &Clipboard, text: String) {
        if text.is_empty() || text.len() > yzendris_core::proto::MAX_CLIPBOARD_BYTES {
            return;
        }
        self.text = Some(digest(&text));
        clip.set_text(text);
    }

    pub fn accept_files(&mut self, clip: &Clipboard, files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        self.files = Some(signature(&files));
        clip.set_files(files);
    }
}
