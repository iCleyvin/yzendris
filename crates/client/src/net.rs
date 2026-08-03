/// TCP listener (plain or TLS) for the Linux client.
/// Accepts one connection from the Windows server, forwards Events through a
/// channel, handles clipboard sync, heartbeat, and outbound client→server
/// events (EdgeReached).
use anyhow::{Context, Result};
use tokio::{net::TcpListener, sync::mpsc};
use tracing::{info, warn};
use yzendris_protocol::{recv_event, send_event, Event};

/// Bind the listening socket. Done once, outside the accept loop: rebinding
/// per connection left the port unbound between sessions, so a host that
/// reconnected quickly hit a closed port and had to back off.
pub async fn bind(addr: &str, tls: bool) -> Result<TcpListener> {
    let listener = TcpListener::bind(addr).await.context("bind TCP")?;
    info!("listening on {addr} (TLS: {tls})");
    Ok(listener)
}

/// Run one accept → receive → forward cycle on an already-bound listener.
/// When the connection drops the function returns; the caller loops.
/// `out_rx` carries client→server events (EdgeReached) from the injector.
pub async fn run_once(
    listener: &TcpListener,
    tx: mpsc::UnboundedSender<Event>,
    out_rx: mpsc::UnboundedReceiver<Event>,
    heartbeat_timeout_ms: u64,
    clipboard_enabled: bool,
    max_file_bytes: u64,
    tls_acceptor: Option<&tokio_rustls::TlsAcceptor>,
) -> Result<()> {
    let (tcp_stream, peer) = listener.accept().await.context("accept")?;
    info!("server connected from {peer}");

    // Disable Nagle so incoming mouse-move frames aren't delayed (and so our
    // outbound EdgeReached/heartbeat go out immediately too).
    if let Err(e) = tcp_stream.set_nodelay(true) {
        warn!("set_nodelay failed: {e}");
    }

    if let Some(acceptor) = tls_acceptor {
        let tls_stream = acceptor.accept(tcp_stream).await.context("TLS handshake")?;
        let (reader, writer) = tokio::io::split(tls_stream);
        drive(reader, writer, tx, out_rx, heartbeat_timeout_ms, clipboard_enabled, max_file_bytes).await
    } else {
        let (reader, writer) = tokio::io::split(tcp_stream);
        drive(reader, writer, tx, out_rx, heartbeat_timeout_ms, clipboard_enabled, max_file_bytes).await
    }
}

async fn drive<R, W>(
    mut reader: R,
    writer: W,
    tx: mpsc::UnboundedSender<Event>,
    mut out_rx: mpsc::UnboundedReceiver<Event>,
    heartbeat_timeout_ms: u64,
    clipboard_enabled: bool,
    max_file_bytes: u64,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let timeout = tokio::time::Duration::from_millis(heartbeat_timeout_ms);

    // All writes go through a dedicated task so the read loop is never
    // cancelled mid-frame (select! cancellation would desync the stream). The
    // same task also streams outgoing files: `file_tx` hands it an opened
    // batch, and its pump interleaves file events with input/heartbeat via a
    // fair select so a large transfer never stalls the cursor stream.
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Event>();
    let (file_tx, mut file_rx) = mpsc::unbounded_channel::<yzendris_filexfer::OutgoingBatch>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        let mut outgoing: Option<yzendris_filexfer::OutgoingBatch> = None;
        let mut files_alive = true;
        loop {
            tokio::select! {
                ev = wrx.recv() => match ev {
                    Some(e) => if let Err(e) = send_event(&mut writer, &e).await {
                        warn!("write to server failed: {e}");
                        break;
                    },
                    None => break,
                },
                ev = out_rx.recv() => match ev {
                    Some(e) => if let Err(e) = send_event(&mut writer, &e).await {
                        warn!("write to server failed: {e}");
                        break;
                    },
                    None => break,
                },
                // Accept a new batch only when idle; a second one waits in the
                // channel until the current transfer finishes.
                start = file_rx.recv(), if files_alive && outgoing.is_none() => match start {
                    Some(batch) => {
                        let count = batch.count();
                        if let Err(e) =
                            send_event(&mut writer, &Event::FileBatchStart { count }).await
                        {
                            warn!("send file batch start failed: {e}");
                            break;
                        }
                        outgoing = Some(batch);
                    }
                    None => files_alive = false,
                },
                ev = async { outgoing.as_mut().unwrap().next_event().await },
                    if outgoing.is_some() =>
                {
                    match ev {
                        Ok(Some(e)) => if let Err(e) = send_event(&mut writer, &e).await {
                            warn!("send file event failed: {e}");
                            break;
                        },
                        Ok(None) => {
                            if let Err(e) = send_event(&mut writer, &Event::FileBatchEnd).await {
                                warn!("send file batch end failed: {e}");
                                break;
                            }
                            info!("file batch send complete");
                            outgoing = None;
                        }
                        Err(e) => {
                            warn!("file send error: {e} — ending batch");
                            let _ = send_event(&mut writer, &Event::FileBatchEnd).await;
                            outgoing = None;
                        }
                    }
                }
            }
        }
    });

    // Reassembly state for an incoming batch, plus the last file set we
    // sent/received (shared so a just-received file isn't streamed straight
    // back).
    let mut incoming: Option<yzendris_filexfer::IncomingBatch> = None;
    let mut last_file_sig: Option<yzendris_filexfer::FileSig> = None;

    // Report our screen resolution to the host so its GUI can show it.
    if let Some((w, h)) = crate::platform::screen_size() {
        let _ = wtx.send(Event::ClientInfo { width: w, height: h });
    }

    loop {
        let maybe = tokio::time::timeout(timeout, recv_event(&mut reader)).await;

        match maybe {
            Err(_elapsed) => {
                warn!("heartbeat timeout — server dead");
                let _ = tx.send(Event::CaptureEnd);
                break;
            }
            Ok(Err(e)) => {
                tracing::error!("read error: {e}");
                let _ = tx.send(Event::CaptureEnd);
                break;
            }
            Ok(Ok(None)) => {
                info!("server closed connection");
                let _ = tx.send(Event::CaptureEnd);
                break;
            }
            Ok(Ok(Some(Event::Heartbeat))) => {
                if wtx.send(Event::Heartbeat).is_err() {
                    let _ = tx.send(Event::CaptureEnd);
                    break;
                }
            }
            Ok(Ok(Some(Event::ClipboardText { text }))) => {
                if clipboard_enabled {
                    info!("clipboard Windows→Linux ({} chars)", text.len());
                    crate::clipboard::write(&text);
                }
            }
            Ok(Ok(Some(Event::FileBatchStart { count }))) => {
                incoming = None;
                if clipboard_enabled {
                    match yzendris_filexfer::IncomingBatch::start().await {
                        Ok(b) => {
                            info!("receiving {count} file(s) from Windows");
                            incoming = Some(b);
                        }
                        Err(e) => tracing::error!("can't start incoming batch: {e}"),
                    }
                }
            }
            Ok(Ok(Some(Event::FileHeader { name, size }))) => {
                if let Some(b) = incoming.as_mut() {
                    if size > max_file_bytes {
                        warn!("incoming file '{name}' is {size} bytes \
                               (over the {max_file_bytes} byte limit) — skipping");
                    } else if let Err(e) = b.begin_file(&name, size).await {
                        tracing::error!("can't create incoming file: {e}");
                    }
                }
            }
            Ok(Ok(Some(Event::FileChunk { data }))) => {
                if let Some(b) = incoming.as_mut() {
                    if let Err(e) = b.write_chunk(&data).await {
                        tracing::error!("write incoming chunk: {e}");
                    }
                }
            }
            Ok(Ok(Some(Event::FileEnd { crc32 }))) => {
                if let Some(b) = incoming.as_mut() {
                    if let Err(e) = b.end_file(crc32).await {
                        tracing::error!("finish incoming file: {e}");
                    }
                }
            }
            Ok(Ok(Some(Event::FileBatchEnd))) => {
                if let Some(b) = incoming.take() {
                    let files = b.finish();
                    if !files.is_empty() {
                        info!("{} file(s) ready on clipboard", files.len());
                        crate::clipboard::write_file_paths(&files);
                        last_file_sig = Some(yzendris_filexfer::signature_of(&files));
                    }
                }
            }
            Ok(Ok(Some(Event::CaptureEnd))) => {
                // Before forwarding CaptureEnd, ship Linux clipboard to Windows.
                if clipboard_enabled {
                    if let Some(text) = crate::clipboard::read() {
                        info!("clipboard Linux→Windows ({} chars)", text.len());
                        let (clip, truncated) = Event::clipboard_text(text);
                        if truncated {
                            warn!("clipboard truncated to {} bytes",
                                  yzendris_protocol::MAX_CLIPBOARD_BYTES);
                        }
                        let _ = wtx.send(clip);
                    }
                    // If (new) files are on the clipboard, hand them to the
                    // writer task to stream to Windows.
                    let paths = crate::clipboard::read_file_paths();
                    if let Some(batch) = yzendris_filexfer::prepare_offer(
                        &paths, &mut last_file_sig, max_file_bytes,
                    ).await {
                        info!("sending {} file(s) → Windows", batch.count());
                        let _ = file_tx.send(batch);
                    }
                }
                let _ = tx.send(Event::CaptureEnd);
            }
            Ok(Ok(Some(event))) => {
                if tx.send(event).is_err() {
                    break;
                }
            }
        }
    }

    // If a batch was interrupted mid-file, delete the dangling partial.
    if let Some(mut b) = incoming {
        b.discard_current();
    }

    // Closing wtx makes the writer task exit (its other inputs, out_rx and
    // file_tx, are closed by the injector / this scope on teardown).
    drop(wtx);
    drop(file_tx);
    let _ = writer_task.await;
    Ok(())
}
