/// TCP client: connects to the Linux client and streams Events.
/// Retries with exponential back-off until shutdown is requested.
/// Clipboard is synced on capture transitions:
///   CaptureStart → send Windows clipboard to Linux
///   Receive ClipboardText → write to Windows clipboard (Linux → Windows)
use anyhow::{Context, Result};
use tokio::{net::TcpStream, sync::mpsc, time};
use tracing::{error, info, warn};
use yzendris_protocol::{recv_event, send_event, Event};

pub async fn run(
    addr: &str,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
    heartbeat_ms: u64,
    clipboard_enabled: bool,
    tls_connector: Option<tokio_rustls::TlsConnector>,
) -> Result<()> {
    let mut backoff_ms: u64 = 1000;

    loop {
        info!("connecting to {addr}…");
        match TcpStream::connect(addr).await {
            Err(e) => {
                warn!("connect failed: {e} — retrying in {backoff_ms}ms");
                time::sleep(time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(30_000);
                continue;
            }
            Ok(stream) => {
                backoff_ms = 1000;
                info!("connected to {addr}");

                let result = if let Some(ref connector) = tls_connector {
                    // Wrap in TLS.
                    let domain = rustls::pki_types::ServerName::try_from("yzendris")
                        .expect("static domain name");
                    match connector.connect(domain, stream).await {
                        Err(e) => {
                            error!("TLS handshake failed: {e}");
                            Err(anyhow::anyhow!("TLS handshake: {e}"))
                        }
                        Ok(tls_stream) => {
                            let (reader, writer) = tokio::io::split(tls_stream);
                            handle_generic(reader, writer, &mut event_rx, heartbeat_ms, clipboard_enabled).await
                        }
                    }
                } else {
                    let (reader, writer) = tokio::io::split(stream);
                    handle_generic(reader, writer, &mut event_rx, heartbeat_ms, clipboard_enabled).await
                };

                if let Err(e) = result {
                    error!("connection error: {e}");
                }

                // If still capturing, reset state and drain queue.
                if crate::hook::CAPTURING.load(std::sync::atomic::Ordering::Relaxed) {
                    crate::hook::CAPTURING.store(false, std::sync::atomic::Ordering::Relaxed);
                    while event_rx.try_recv().is_ok() {}
                }
                warn!("disconnected — reconnecting in {backoff_ms}ms");
                time::sleep(time::Duration::from_millis(backoff_ms)).await;
            }
        }
    }
}

async fn handle_generic<R, W>(
    mut reader: R,
    mut writer: W,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    heartbeat_ms: u64,
    clipboard_enabled: bool,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let heartbeat_interval = time::Duration::from_millis(heartbeat_ms);
    let mut heartbeat_tick = time::interval(heartbeat_interval);

    loop {
        tokio::select! {
            // Events from the hook thread.
            maybe = event_rx.recv() => {
                let event = match maybe {
                    Some(e) => e,
                    None => return Ok(()), // channel closed → shutdown
                };

                // On CaptureStart, piggyback the Windows clipboard.
                if matches!(event, Event::CaptureStart) && clipboard_enabled {
                    if let Some(text) = crate::clipboard::read() {
                        send_event(&mut writer, &Event::ClipboardText { text })
                            .await
                            .context("send clipboard")?;
                    }
                }

                send_event(&mut writer, &event).await.context("send_event")?;
            }

            // Heartbeat tick.
            _ = heartbeat_tick.tick() => {
                send_event(&mut writer, &Event::Heartbeat)
                    .await
                    .context("heartbeat send")?;
            }

            // Inbound from client (heartbeat echo, clipboard reply).
            result = recv_event(&mut reader) => {
                match result.context("recv from client")? {
                    Some(Event::Heartbeat) => { /* echo — all good */ }
                    Some(Event::ClipboardText { text }) => {
                        if clipboard_enabled {
                            info!("received clipboard from Linux ({} chars)", text.len());
                            crate::clipboard::write(&text);
                        }
                    }
                    Some(_) => { /* unexpected — ignore */ }
                    None => {
                        info!("client closed connection");
                        return Ok(());
                    }
                }
            }
        }
    }
}
