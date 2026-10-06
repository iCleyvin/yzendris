//! Framed message I/O over an established TLS stream.
//!
//! Reading and writing each get their own task. Reading is not
//! cancellation-safe (a half-read frame would desynchronise the stream), so
//! nothing else ever races it; writing drains a queue in batches so a burst of
//! input costs one TLS record and one syscall.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use yzendris_core::proto::{self, Msg};

use crate::files::Outgoing;

/// Queued messages beyond which a peer is considered stalled and dropped.
const MAX_BACKLOG: usize = 4096;

pub struct FrameReader<R> {
    inner: R,
    body: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            body: Vec::new(),
        }
    }

    /// Next message, or `None` when the peer closed the stream cleanly.
    pub async fn next(&mut self) -> Result<Option<Msg>> {
        let mut prefix = [0u8; 4];
        match self.inner.read_exact(&mut prefix).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e).context("reading frame"),
        }
        let len = proto::frame_len(prefix)?;
        self.body.resize(len, 0);
        self.inner
            .read_exact(&mut self.body)
            .await
            .context("reading frame body")?;
        Ok(Some(proto::decode(&self.body)?))
    }

    /// Like [`next`](Self::next) but a closed stream is an error.
    pub async fn expect(&mut self) -> Result<Msg> {
        match self.next().await? {
            Some(msg) => Ok(msg),
            None => bail!("connection closed"),
        }
    }
}

pub struct FrameWriter<W> {
    inner: W,
    buf: Vec<u8>,
    /// Offset of the last queued frame when it is a pointer position.
    last_motion: Option<usize>,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(1024),
            last_motion: None,
        }
    }

    /// Queue a message. A pointer position directly following another one
    /// replaces it: only the newest matters.
    pub fn queue(&mut self, msg: &Msg) -> Result<()> {
        if msg.is_motion() {
            if let Some(at) = self.last_motion {
                self.buf.truncate(at);
            }
            self.last_motion = Some(self.buf.len());
        } else {
            self.last_motion = None;
        }
        proto::encode(msg, &mut self.buf)?;
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        if !self.buf.is_empty() {
            self.inner.write_all(&self.buf).await.context("writing")?;
            self.inner.flush().await.context("flushing")?;
            self.buf.clear();
            self.last_motion = None;
        }
        Ok(())
    }

    pub async fn send(&mut self, msg: &Msg) -> Result<()> {
        self.queue(msg)?;
        self.flush().await
    }

    pub async fn shutdown(&mut self) {
        let _ = self.inner.shutdown().await;
    }
}

/// Sending half of a live connection. Cheap to clone, callable from any
/// thread (including OS input hooks).
#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::UnboundedSender<Msg>,
    depth: Arc<AtomicUsize>,
}

impl Outbox {
    /// Queue a message. Returns `false` once the connection is gone or has
    /// stopped draining.
    pub fn send(&self, msg: Msg) -> bool {
        if self.depth.fetch_add(1, Ordering::Relaxed) >= MAX_BACKLOG {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        self.tx.send(msg).is_ok()
    }

    pub fn stalled(&self) -> bool {
        self.depth.load(Ordering::Relaxed) >= MAX_BACKLOG
    }
}

/// A running connection: inbound messages, an outbox, and a slot for
/// streaming a batch of files without starving input.
pub struct Conn {
    pub inbox: mpsc::Receiver<Msg>,
    pub outbox: Outbox,
    pub files: mpsc::Sender<Outgoing>,
    tasks: [JoinHandle<()>; 2],
}

impl Drop for Conn {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Conn {
    pub fn spawn<R, W>(mut reader: FrameReader<R>, writer: FrameWriter<W>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (in_tx, inbox) = mpsc::channel(256);
        let read_task = tokio::spawn(async move {
            loop {
                match reader.next().await {
                    Ok(Some(msg)) => {
                        if in_tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!("link closed: {e:#}");
                        break;
                    }
                }
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (files, files_rx) = mpsc::channel(1);
        let depth = Arc::new(AtomicUsize::new(0));
        let write_task = tokio::spawn(write_loop(writer, rx, files_rx, depth.clone()));
        Self {
            inbox,
            outbox: Outbox { tx, depth },
            files,
            tasks: [read_task, write_task],
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: FrameWriter<W>,
    mut rx: mpsc::UnboundedReceiver<Msg>,
    mut files_rx: mpsc::Receiver<Outgoing>,
    depth: Arc<AtomicUsize>,
) {
    let mut sending: Option<Outgoing> = None;
    let result: Result<()> = async {
        loop {
            tokio::select! {
                // Input always goes first; file chunks only fill idle time.
                biased;
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    let mut taken = 1;
                    writer.queue(&msg)?;
                    while taken < 512 {
                        let Ok(more) = rx.try_recv() else { break };
                        writer.queue(&more)?;
                        taken += 1;
                    }
                    depth.fetch_sub(taken, Ordering::Relaxed);
                    writer.flush().await?;
                }
                batch = files_rx.recv(), if sending.is_none() => {
                    let Some(batch) = batch else { break };
                    writer.send(&batch.begin()).await?;
                    sending = Some(batch);
                }
                next = async { sending.as_mut().expect("guarded").next().await },
                    if sending.is_some() =>
                {
                    match next {
                        Ok(Some(msg)) => writer.send(&msg).await?,
                        Ok(None) => {
                            writer.send(&Msg::FilesEnd).await?;
                            sending = None;
                        }
                        Err(e) => {
                            tracing::warn!("file transfer stopped: {e:#}");
                            writer.send(&Msg::FilesEnd).await?;
                            sending = None;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::debug!("writer stopped: {e:#}");
    }
    writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_cross_a_stream_and_motion_is_coalesced() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let mut writer = FrameWriter::new(a);
        let mut reader = FrameReader::new(b);

        writer.queue(&Msg::PointerAbs { x: 1, y: 1 }).unwrap();
        writer.queue(&Msg::PointerAbs { x: 2, y: 2 }).unwrap();
        writer
            .queue(&Msg::Button {
                code: 0x110,
                down: true,
            })
            .unwrap();
        writer.queue(&Msg::PointerAbs { x: 3, y: 3 }).unwrap();
        writer.flush().await.unwrap();
        writer.shutdown().await;

        // The stale position is gone, but order around the click is kept.
        assert_eq!(
            reader.next().await.unwrap(),
            Some(Msg::PointerAbs { x: 2, y: 2 })
        );
        assert_eq!(
            reader.next().await.unwrap(),
            Some(Msg::Button {
                code: 0x110,
                down: true
            })
        );
        assert_eq!(
            reader.next().await.unwrap(),
            Some(Msg::PointerAbs { x: 3, y: 3 })
        );
        assert_eq!(reader.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_frame_is_an_error_not_an_allocation() {
        let (mut a, b) = tokio::io::duplex(64);
        a.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
        let mut reader = FrameReader::new(b);
        assert!(reader.next().await.is_err());
    }

    #[tokio::test]
    async fn conn_delivers_both_ways() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let left = Conn::spawn(FrameReader::new(ar), FrameWriter::new(aw));
        let mut right = Conn::spawn(FrameReader::new(br), FrameWriter::new(bw));
        assert!(left.outbox.send(Msg::Ping(7)));
        assert_eq!(right.inbox.recv().await, Some(Msg::Ping(7)));
        drop(left);
        assert_eq!(right.inbox.recv().await, None);
    }
}
