/// TCP listener (plain or TLS) for the Linux client.
/// Accepts one connection from the Windows server, forwards Events through a
/// channel, handles clipboard sync, heartbeat, and outbound client→server
/// events (EdgeReached).
use anyhow::{Context, Result};
use tokio::{net::TcpListener, sync::mpsc};
use tracing::{info, warn};
use yzendris_protocol::{recv_event, send_event, Event};

/// Run one accept → receive → forward cycle.
/// When the connection drops the function returns; the caller reconnects.
/// `out_rx` carries client→server events (EdgeReached) from the injector.
pub async fn run_once(
    addr: &str,
    tx: mpsc::UnboundedSender<Event>,
    out_rx: mpsc::UnboundedReceiver<Event>,
    heartbeat_timeout_ms: u64,
    clipboard_enabled: bool,
    tls_acceptor: Option<&tokio_rustls::TlsAcceptor>,
) -> Result<()> {
    let listener = TcpListener::bind(addr).await.context("bind TCP")?;
    info!("listening on {addr} (TLS: {})", tls_acceptor.is_some());

    let (tcp_stream, peer) = listener.accept().await.context("accept")?;
    info!("server connected from {peer}");

    if let Some(acceptor) = tls_acceptor {
        let tls_stream = acceptor.accept(tcp_stream).await.context("TLS handshake")?;
        let (reader, writer) = tokio::io::split(tls_stream);
        drive(reader, writer, tx, out_rx, heartbeat_timeout_ms, clipboard_enabled).await
    } else {
        let (reader, writer) = tokio::io::split(tcp_stream);
        drive(reader, writer, tx, out_rx, heartbeat_timeout_ms, clipboard_enabled).await
    }
}

async fn drive<R, W>(
    mut reader: R,
    writer: W,
    tx: mpsc::UnboundedSender<Event>,
    mut out_rx: mpsc::UnboundedReceiver<Event>,
    heartbeat_timeout_ms: u64,
    clipboard_enabled: bool,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let timeout = tokio::time::Duration::from_millis(heartbeat_timeout_ms);

    // All writes go through a dedicated task so the read loop is never
    // cancelled mid-frame (select! cancellation would desync the stream).
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Event>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        loop {
            let event = tokio::select! {
                ev = wrx.recv() => match ev { Some(e) => e, None => break },
                ev = out_rx.recv() => match ev { Some(e) => e, None => break },
            };
            if let Err(e) = send_event(&mut writer, &event).await {
                warn!("write to server failed: {e}");
                break;
            }
        }
    });

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
            Ok(Ok(Some(Event::CaptureEnd))) => {
                // Before forwarding CaptureEnd, ship Linux clipboard to Windows.
                if clipboard_enabled {
                    if let Some(text) = crate::clipboard::read() {
                        info!("clipboard Linux→Windows ({} chars)", text.len());
                        let _ = wtx.send(Event::ClipboardText { text });
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

    // Closing wtx makes the writer task exit (its other input, out_rx, is
    // closed by the injector when the main loop tears down).
    drop(wtx);
    let _ = writer_task.await;
    Ok(())
}
