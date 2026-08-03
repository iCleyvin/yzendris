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
    max_file_bytes: u64,
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
                // Disable Nagle: mouse moves are tiny one-per-event frames, and
                // Nagle would hold them up to ~40ms waiting to coalesce, which
                // shows up as choppy/laggy cursor motion on the client.
                if let Err(e) = stream.set_nodelay(true) {
                    warn!("[client {client}] set_nodelay failed: {e}");
                }
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
                            handle_generic(client, reader, writer, &mut event_rx, heartbeat_ms, clipboard_enabled, max_file_bytes).await
                        }
                    }
                } else {
                    crate::hook::set_client_connected(client, true);
                    info!("[client {client}] connected to {addr}");
                    let (reader, writer) = tokio::io::split(stream);
                    handle_generic(client, reader, writer, &mut event_rx, heartbeat_ms, clipboard_enabled, max_file_bytes).await
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
    max_file_bytes: u64,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    let heartbeat_interval = time::Duration::from_millis(heartbeat_ms);
    let mut heartbeat_tick = time::interval(heartbeat_interval);

    // File transfer state (Windows→Linux out, Linux→Windows in). Both the
    // sender's dedup signature and the receiver share this scope so a file we
    // just received (and put on the clipboard) isn't streamed straight back.
    let mut outgoing: Option<yzendris_filexfer::OutgoingBatch> = None;
    let mut incoming: Option<yzendris_filexfer::IncomingBatch> = None;
    let mut last_file_sig: Option<yzendris_filexfer::FileSig> = None;

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
                        let (clip, truncated) = Event::clipboard_text(text);
                        if truncated {
                            warn!("[client {client}] clipboard truncated to {} bytes",
                                  yzendris_protocol::MAX_CLIPBOARD_BYTES);
                        }
                        if let Err(e) = send_event(&mut writer, &clip).await {
                            break Err(e).context("send clipboard");
                        }
                    }
                    // If the clipboard holds (new) files, start streaming them.
                    if outgoing.is_none() {
                        let paths = crate::clipboard::read_file_paths();
                        if let Some(batch) = yzendris_filexfer::prepare_offer(
                            &paths, &mut last_file_sig, max_file_bytes,
                        ).await {
                            let count = batch.count();
                            info!("[client {client}] sending {count} file(s) → Linux");
                            if let Err(e) = send_event(
                                &mut writer, &Event::FileBatchStart { count },
                            ).await {
                                break Err(e).context("send file batch start");
                            }
                            outgoing = Some(batch);
                        }
                    }
                }

                if let Err(e) = send_event(&mut writer, &event).await {
                    break Err(e).context("send_event");
                }
            }

            // File pump — active only while a transfer is in flight. The
            // random-fair select interleaves file events with input and
            // heartbeats so a large batch never starves the cursor stream.
            ev = async { outgoing.as_mut().unwrap().next_event().await },
                if outgoing.is_some() =>
            {
                match ev {
                    Ok(Some(e)) => {
                        if let Err(err) = send_event(&mut writer, &e).await {
                            break Err(err).context("send file event");
                        }
                    }
                    Ok(None) => {
                        if let Err(e) = send_event(&mut writer, &Event::FileBatchEnd).await {
                            break Err(e).context("send file batch end");
                        }
                        info!("[client {client}] file batch send complete");
                        outgoing = None;
                    }
                    Err(e) => {
                        warn!("[client {client}] file send error: {e} — ending batch");
                        let _ = send_event(&mut writer, &Event::FileBatchEnd).await;
                        outgoing = None;
                    }
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
                    Some(Event::FileBatchStart { count }) => {
                        incoming = None;
                        if clipboard_enabled {
                            match yzendris_filexfer::IncomingBatch::start().await {
                                Ok(b) => {
                                    info!("[client {client}] receiving {count} file(s) from Linux");
                                    incoming = Some(b);
                                }
                                Err(e) => error!("[client {client}] can't start incoming batch: {e}"),
                            }
                        }
                    }
                    Some(Event::FileHeader { name, size }) => {
                        if let Some(b) = incoming.as_mut() {
                            if size > max_file_bytes {
                                warn!("[client {client}] incoming file '{name}' is {size} bytes \
                                       (over the {max_file_bytes} byte limit) — skipping");
                            } else if let Err(e) = b.begin_file(&name, size).await {
                                error!("[client {client}] can't create incoming file: {e}");
                            }
                        }
                    }
                    Some(Event::FileChunk { data }) => {
                        if let Some(b) = incoming.as_mut() {
                            if let Err(e) = b.write_chunk(&data).await {
                                error!("[client {client}] write incoming chunk: {e}");
                            }
                        }
                    }
                    Some(Event::FileEnd { crc32 }) => {
                        if let Some(b) = incoming.as_mut() {
                            if let Err(e) = b.end_file(crc32).await {
                                error!("[client {client}] finish incoming file: {e}");
                            }
                        }
                    }
                    Some(Event::FileBatchEnd) => {
                        if let Some(b) = incoming.take() {
                            let files = b.finish();
                            if !files.is_empty() {
                                info!("[client {client}] {} file(s) ready on clipboard", files.len());
                                crate::clipboard::write_file_paths(&files);
                                // Record them so crossing back doesn't stream them out again.
                                last_file_sig = Some(yzendris_filexfer::signature_of(&files));
                            }
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

    // If a batch was interrupted mid-file, delete the dangling partial.
    if let Some(mut b) = incoming {
        b.discard_current();
    }

    reader_task.abort();
    result
}
