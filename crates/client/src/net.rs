/// TCP listener (plain or TLS) for the Linux client.
/// Accepts one connection from the Windows server, forwards Events through a
/// channel, handles clipboard sync and heartbeat.
use anyhow::{Context, Result};
use tokio::{net::TcpListener, sync::mpsc};
use tracing::{info, warn};
use yzendris_protocol::{recv_event, send_event, Event};

/// Run one accept → receive → forward cycle.
/// When the connection drops the function returns; the caller reconnects.
pub async fn run_once(
    addr: &str,
    tx: mpsc::UnboundedSender<Event>,
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
        drive(reader, writer, tx, heartbeat_timeout_ms, clipboard_enabled).await
    } else {
        let (reader, writer) = tokio::io::split(tcp_stream);
        drive(reader, writer, tx, heartbeat_timeout_ms, clipboard_enabled).await
    }
}

async fn drive<R, W>(
    mut reader: R,
    mut writer: W,
    tx: mpsc::UnboundedSender<Event>,
    heartbeat_timeout_ms: u64,
    clipboard_enabled: bool,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let timeout = tokio::time::Duration::from_millis(heartbeat_timeout_ms);

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
                if let Err(e) = send_event(&mut writer, &Event::Heartbeat).await {
                    warn!("heartbeat reply: {e}");
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
                        let _ = send_event(&mut writer, &Event::ClipboardText { text }).await;
                    }
                }
                let _ = tx.send(Event::CaptureEnd);
            }
            Ok(Ok(Some(event))) => {
                if tx.send(event).is_err() { break; }
            }
        }
    }
    Ok(())
}
