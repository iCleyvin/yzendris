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
    client: usize,
    addr: &str,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
    heartbeat_ms: u64,
    clipboard_enabled: bool,
    tls_connector: Option<tokio_rustls::TlsConnector>,
) -> Result<()> {
    let mut backoff_ms: u64 = 1000;

    loop {
        info!("[client {client}] connecting to {addr}…");
        match TcpStream::connect(addr).await {
            Err(e) => {
                warn!("[client {client}] connect failed: {e} — retrying in {backoff_ms}ms");
                time::sleep(time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(30_000);
                continue;
            }
            Ok(stream) => {
                backoff_ms = 1000;
                info!("[client {client}] TCP connected to {addr}");

                let result = if let Some(ref connector) = tls_connector {
                    // Wrap in TLS.
                    let domain = rustls::pki_types::ServerName::try_from("yzendris")
                        .expect("static domain name");
                    match connector.connect(domain, stream).await {
                        Err(e) => {
                            error!("[client {client}] TLS handshake failed: {e}");
                            Err(anyhow::anyhow!("TLS handshake: {e}"))
                        }
                        Ok(tls_stream) => {
                            crate::hook::set_client_connected(client, true);
                            info!("[client {client}] connected to {addr} (TLS OK)");
                            let (reader, writer) = tokio::io::split(tls_stream);
                            handle_generic(client, reader, writer, &mut event_rx, heartbeat_ms, clipboard_enabled).await
                        }
                    }
                } else {
                    crate::hook::set_client_connected(client, true);
                    info!("[client {client}] connected to {addr}");
                    let (reader, writer) = tokio::io::split(stream);
                    handle_generic(client, reader, writer, &mut event_rx, heartbeat_ms, clipboard_enabled).await
                };

                if let Err(e) = result {
                    error!("[client {client}] connection error: {e}");
                }

                crate::hook::set_client_connected(client, false);
                crate::hook::release_capture_on_disconnect(client);
                while event_rx.try_recv().is_ok() {}
                warn!("[client {client}] disconnected — reconnecting in {backoff_ms}ms");
                time::sleep(time::Duration::from_millis(backoff_ms)).await;
            }
        }
    }
}

async fn handle_generic<R, W>(
    client: usize,
    reader: R,
    mut writer: W,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    heartbeat_ms: u64,
    clipboard_enabled: bool,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    let heartbeat_interval = time::Duration::from_millis(heartbeat_ms);
    let mut heartbeat_tick = time::interval(heartbeat_interval);

    // Reads run in their own task: recv_event is NOT cancellation-safe, and
    // a select! arm that loses the race drops it mid-frame, desyncing the
    // stream (observed as EdgeReached never arriving while MouseMove events
    // flooded the other branches).
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Event>();
    let reader_task = tokio::spawn(async move {
        let mut reader = reader;
        loop {
            match recv_event(&mut reader).await {
                Ok(Some(event)) => {
                    if in_tx.send(event).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    info!("client closed connection");
                    break;
                }
                Err(e) => {
                    error!("recv from client: {e}");
                    break;
                }
            }
        }
        // in_tx drops here → main loop sees the channel close.
    });

    let result: Result<()> = loop {
        tokio::select! {
            // Events from the hook thread.
            maybe = event_rx.recv() => {
                let event = match maybe {
                    Some(e) => e,
                    None => break Ok(()), // channel closed → shutdown
                };

                // On CaptureStart, piggyback the Windows clipboard.
                if matches!(event, Event::CaptureStart) && clipboard_enabled {
                    if let Some(text) = crate::clipboard::read() {
                        if let Err(e) = send_event(&mut writer, &Event::ClipboardText { text }).await {
                            break Err(e).context("send clipboard");
                        }
                    }
                }

                if let Err(e) = send_event(&mut writer, &event).await {
                    break Err(e).context("send_event");
                }
            }

            // Heartbeat tick.
            _ = heartbeat_tick.tick() => {
                if let Err(e) = send_event(&mut writer, &Event::Heartbeat).await {
                    break Err(e).context("heartbeat send");
                }
            }

            // Inbound from client (heartbeat echo, clipboard, EdgeReached).
            inbound = in_rx.recv() => {
                match inbound {
                    Some(Event::Heartbeat) => { /* echo — all good */ }
                    Some(Event::ClipboardText { text }) => {
                        if clipboard_enabled {
                            info!("received clipboard from Linux ({} chars)", text.len());
                            crate::clipboard::write(&text);
                        }
                    }
                    Some(Event::EdgeReached { edge, frac }) => {
                        // Client cursor pushed past its screen edge — hand
                        // control back on the matching Windows side.
                        crate::hook::release_capture_toward(client, edge, frac);
                    }
                    Some(Event::ClientInfo { width, height }) => {
                        crate::hook::set_client_resolution(client, width, height);
                    }
                    Some(_) => { /* unexpected — ignore */ }
                    None => break Ok(()), // reader task ended (EOF or error)
                }
            }
        }
    };

    reader_task.abort();
    result
}
