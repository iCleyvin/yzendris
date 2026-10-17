//! Client role: connects out to the paired host and reproduces the input it
//! sends.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use yzendris_core::config::{clean_name, Role};
use yzendris_core::geom::Rect;
use yzendris_core::ipc::{Link, NoticeCode};
use yzendris_core::proto::{Hello, Msg, Os, RejectReason, PROTOCOL_VERSION};
use yzendris_core::{Fingerprint, DEFAULT_PORT};

use crate::app::App;
use crate::clipboard::ClipSync;
use crate::discovery;
use crate::host::{offer_clipboard, receive_shared, DEAD_AFTER, HANDSHAKE, PING_EVERY};
use crate::identity::{peer_fingerprint, server_name};
use crate::pairing::{self, Refused};
use crate::platform::{self, Inject};
use crate::wire::{Conn, FrameReader, FrameWriter};

const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// Add the default port to an address typed without one.
pub fn with_default_port(addr: &str) -> String {
    let addr = addr.trim();
    if addr.parse::<SocketAddr>().is_ok() {
        return addr.to_owned();
    }
    if let Ok(ip) = addr.parse::<IpAddr>() {
        return SocketAddr::new(ip, DEFAULT_PORT).to_string();
    }
    match addr.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() => addr.to_owned(),
        _ => format!("{addr}:{DEFAULT_PORT}"),
    }
}

fn hello(app: &App, screens: Vec<Rect>, pairing: bool) -> Msg {
    Msg::Hello(Hello {
        version: PROTOCOL_VERSION,
        name: app.config().name,
        os: Os::current(),
        screens,
        pairing,
    })
}

type Reader = FrameReader<tokio::io::ReadHalf<tokio_rustls::client::TlsStream<TcpStream>>>;
type Writer = FrameWriter<tokio::io::WriteHalf<tokio_rustls::client::TlsStream<TcpStream>>>;

/// Connect, complete TLS and exchange hellos. Returns the framed stream, the
/// host's fingerprint and the name it announced.
async fn connect(
    app: &App,
    connector: &TlsConnector,
    addr: &str,
    screens: Vec<Rect>,
    pairing: bool,
) -> Result<(Reader, Writer, Fingerprint, String)> {
    let stream = timeout(HANDSHAKE, TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow!("no answer from {addr}"))?
        .with_context(|| format!("connecting to {addr}"))?;
    stream.set_nodelay(true)?;
    let tls = timeout(HANDSHAKE, connector.connect(server_name(), stream))
        .await
        .map_err(|_| anyhow!("{addr} did not complete the secure handshake"))??;
    let host = peer_fingerprint(tls.get_ref().1)?;
    let (r, w) = tokio::io::split(tls);
    let (mut reader, mut writer) = (FrameReader::new(r), FrameWriter::new(w));
    writer.send(&hello(app, screens, pairing)).await?;
    match timeout(HANDSHAKE, reader.expect())
        .await
        .map_err(|_| anyhow!("the host did not answer"))??
    {
        Msg::Hello(h) if h.version == PROTOCOL_VERSION => {
            Ok((reader, writer, host, clean_name(&h.name)))
        }
        Msg::Hello(_) | Msg::Reject(RejectReason::Version) => bail!(Refused(RejectReason::Version)),
        Msg::Reject(reason) => bail!(Refused(reason)),
        _ => bail!("unexpected answer from the host"),
    }
}

/// Pair this machine with the host at `addr` using the code it displays, and
/// make it our host.
pub async fn pair(app: &Arc<App>, addr: &str, code: &str) -> Result<()> {
    let code = pairing::normalize_code(code)
        .ok_or_else(|| anyhow!("the code has {} digits", pairing::CODE_DIGITS))?;
    let addr = with_default_port(addr);
    // No pin yet: the code exchange below is what authenticates the host.
    let connector = app.identity.connector(None)?;
    let (mut reader, mut writer, host, name) =
        connect(app, &connector, &addr, Vec::new(), true).await?;
    if host == app.identity.fingerprint {
        bail!("that is this very machine");
    }
    timeout(
        HANDSHAKE * 2,
        pairing::join(
            &mut reader,
            &mut writer,
            &code,
            &app.identity.fingerprint,
            &host,
        ),
    )
    .await
    .map_err(|_| anyhow!("the host stopped answering"))??;
    app.update_config(|cfg| {
        cfg.role = Some(Role::Client);
        cfg.client.host_addr = addr.clone();
        cfg.client.host_fingerprint = Some(host);
        cfg.client.host_name = name.clone();
    })?;
    tracing::info!("paired with a host");
    Ok(())
}

/// Run the client until the configuration changes.
pub async fn run(app: Arc<App>) -> Result<()> {
    let mut changes = app.subscribe();
    let cfg = app.config();

    let mut injector = loop {
        match platform::injector(&cfg.client.keyboard_layout) {
            Ok(i) => break i,
            Err(e) => {
                tracing::error!("cannot create virtual input devices: {e:#}");
                app.set_notice(NoticeCode::InjectDenied, format!("{e:#}"));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = changes.changed() => return Ok(()),
                }
            }
        }
    };
    app.clear_notice(NoticeCode::InjectDenied);

    let Some(host) = cfg.client.host_fingerprint else {
        tracing::info!("not paired with a host yet");
        let _ = changes.changed().await;
        return Ok(());
    };
    let connector = app.identity.connector(Some(host))?;
    let configured = with_default_port(&cfg.client.host_addr);
    let mut attempt = configured.clone();
    let mut backoff = Duration::from_secs(1);
    let mut failures = 0u32;

    loop {
        app.set_peer(host, |p| {
            if p.link != Link::Refused {
                p.link = Link::Connecting;
            }
        });
        let started = Instant::now();
        let result = tokio::select! {
            r = session(&app, &connector, &attempt, host, injector.as_mut()) => r,
            _ = changes.changed() => return Ok(()),
        };
        app.live().active = None;
        let refused = result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<Refused>())
            .map(|r| r.0);
        app.set_peer(host, |p| {
            p.link = if refused.is_some() {
                Link::Refused
            } else {
                Link::Offline
            };
            p.latency_ms = None;
        });
        match &result {
            Ok(()) => tracing::info!("disconnected from the host"),
            Err(e) => tracing::debug!("link to the host failed: {e:#}"),
        }
        if started.elapsed() > Duration::from_secs(20) {
            (backoff, failures) = (Duration::from_secs(1), 0);
        }
        failures += 1;
        // The host may have moved to another address: ask the network. The
        // answer is an unauthenticated hint, so it is only *tried*; it becomes
        // the saved address once the host there has proved its identity.
        attempt = configured.clone();
        if failures % 3 == 0 && refused.is_none() {
            if let Some(found) = discovery::locate(&host).await {
                if found != configured {
                    tracing::info!("trying the host at {found}");
                    attempt = found;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = changes.changed() => return Ok(()),
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Keys and buttons we pressed on behalf of the host, so they can be released
/// if it leaves or disappears.
#[derive(Default)]
struct Pressed {
    keys: Vec<u16>,
    buttons: Vec<u16>,
}

impl Pressed {
    fn track(list: &mut Vec<u16>, code: u16, down: bool) {
        list.retain(|c| *c != code);
        if down && list.len() < 64 {
            list.push(code);
        }
    }

    fn release_all(&mut self, injector: &mut dyn Inject) {
        for code in self.buttons.drain(..) {
            injector.button(code, false);
        }
        for code in self.keys.drain(..) {
            injector.key(code, false);
        }
    }
}

async fn local_screens() -> Vec<Rect> {
    tokio::task::spawn_blocking(|| platform::screens().into_iter().map(|s| s.rect).collect())
        .await
        .unwrap_or_default()
}

async fn session(
    app: &Arc<App>,
    connector: &TlsConnector,
    addr: &str,
    host: Fingerprint,
    injector: &mut dyn Inject,
) -> Result<()> {
    let mut screens = local_screens().await;
    if screens.is_empty() {
        app.set_notice(NoticeCode::NoScreens, "");
        // Still usable: assume a common size so the host can aim somewhere.
        screens.push(Rect::new(0, 0, 1920, 1080));
    } else {
        app.clear_notice(NoticeCode::NoScreens);
    }
    app.live().screens = platform::screens();

    let (mut reader, writer, _, host_name) =
        connect(app, connector, addr, screens.clone(), false).await?;
    match timeout(HANDSHAKE, reader.expect())
        .await
        .map_err(|_| anyhow!("the host did not answer"))??
    {
        Msg::Welcome => {}
        Msg::Reject(reason) => bail!(Refused(reason)),
        _ => bail!("unexpected answer from the host"),
    }
    let mut conn = Conn::spawn(reader, writer);
    app.set_peer(host, |p| {
        p.link = Link::Online;
        p.address = Some(addr.to_owned());
    });
    if !host_name.is_empty() && host_name != app.config().client.host_name {
        // Cosmetic only; do not restart the session for it.
        tracing::debug!("the host now calls itself differently");
    }
    tracing::info!("connected to the host");
    // Reaching this point means the machine at `addr` holds the paired host's
    // private key, so the address is worth remembering.
    if with_default_port(&app.config().client.host_addr) != addr {
        let addr = addr.to_owned();
        app.update_config(|cfg| cfg.client.host_addr = addr)?;
    }

    let mut desktop = Rect::bounding(&screens).unwrap_or_default();
    let mut pressed = Pressed::default();
    let mut active = false;
    let mut sync = ClipSync::default();
    let mut incoming = None;
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut last_heard = Instant::now();
    let mut ping_sent: Option<(u32, Instant)> = None;
    let mut seq = 0u32;
    let (screens_tx, mut screens_rx) = tokio::sync::mpsc::channel::<Vec<Rect>>(1);

    let outcome = loop {
        tokio::select! {
            msg = conn.inbox.recv() => {
                let Some(msg) = msg else { break Ok(()) };
                last_heard = Instant::now();
                match msg {
                    Msg::Ping(n) => { conn.outbox.send(Msg::Pong(n)); }
                    Msg::Pong(n) => {
                        if let Some((_, sent)) = ping_sent.take_if(|(s, _)| *s == n) {
                            let ms = sent.elapsed().as_millis() as u32;
                            app.set_peer(host, |p| p.latency_ms = Some(ms));
                        }
                    }
                    Msg::Enter { x, y, .. } => {
                        active = true;
                        app.live().active = Some(host_name.clone());
                        injector.pointer_abs(x, y, desktop);
                        // Screens may have changed since we connected; check
                        // without holding up input.
                        let tx = screens_tx.clone();
                        tokio::spawn(async move { let _ = tx.send(local_screens().await).await; });
                    }
                    Msg::Leave => {
                        active = false;
                        pressed.release_all(injector);
                        app.live().active = None;
                        offer_clipboard(app, &mut sync, &conn).await;
                    }
                    // Input is honoured only between Enter and Leave.
                    Msg::PointerAbs { x, y } if active => injector.pointer_abs(x, y, desktop),
                    Msg::PointerRel { dx, dy } if active => injector.pointer_rel(dx, dy),
                    Msg::Scroll { dx, dy } if active => injector.scroll(dx, dy),
                    Msg::Button { code, down } if active => {
                        Pressed::track(&mut pressed.buttons, code, down);
                        injector.button(code, down);
                    }
                    Msg::Key { code, down } if active => {
                        Pressed::track(&mut pressed.keys, code, down);
                        injector.key(code, down);
                    }
                    other => {
                        if let Err(e) = receive_shared(app, &mut sync, &mut incoming, other).await {
                            break Err(e);
                        }
                    }
                }
            }
            Some(fresh) = screens_rx.recv() => {
                if !fresh.is_empty() && fresh != screens {
                    screens = fresh;
                    desktop = Rect::bounding(&screens).unwrap_or_default();
                    conn.outbox.send(Msg::Screens(screens.clone()));
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > DEAD_AFTER {
                    break Err(anyhow!("the host stopped responding"));
                }
                seq = seq.wrapping_add(1);
                ping_sent = Some((seq, Instant::now()));
                conn.outbox.send(Msg::Ping(seq));
            }
        }
    };
    // Never leave a key stuck down because the link dropped.
    pressed.release_all(injector);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_added_only_when_missing() {
        assert_eq!(with_default_port("192.168.1.5"), "192.168.1.5:7547");
        assert_eq!(with_default_port(" 192.168.1.5:9000 "), "192.168.1.5:9000");
        assert_eq!(with_default_port("desk.local"), "desk.local:7547");
        assert_eq!(with_default_port("desk.local:81"), "desk.local:81");
        assert_eq!(with_default_port("fe80::1"), "[fe80::1]:7547");
        assert_eq!(with_default_port("[fe80::1]:99"), "[fe80::1]:99");
    }
}
