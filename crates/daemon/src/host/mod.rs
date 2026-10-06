//! Host role: owns the keyboard and mouse and shares them with paired
//! clients that connect in.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;
use yzendris_core::config::{ClientEntry, Config, Role};
use yzendris_core::geom::{Rect, Side};
use yzendris_core::ipc::{Link, NoticeCode};
use yzendris_core::proto::{Hello, Msg, Os, RejectReason, PROTOCOL_VERSION};
use yzendris_core::switch::{ClientId, ClientSpec, Effect, Switch};
use yzendris_core::{paths, Fingerprint, Point};

use crate::app::App;
use crate::clipboard::ClipSync;
use crate::files::Incoming;
use crate::identity::peer_fingerprint;
use crate::platform::{self, Capture, Sink};
use crate::wire::{Conn, FrameReader, FrameWriter, Outbox};
use crate::{discovery, pairing};

/// Time allowed for each step before a session exists.
pub const HANDSHAKE: Duration = Duration::from_secs(6);
pub const PING_EVERY: Duration = Duration::from_secs(2);
/// Silence after which a peer is considered gone.
pub const DEAD_AFTER: Duration = Duration::from_secs(7);
/// Connections allowed in the pre-session phase at once.
const MAX_PENDING: usize = 8;
const SCREEN_REFRESH: Duration = Duration::from_secs(10);
/// How long after input leaves a client its clipboard is accepted.
const CLIPBOARD_WINDOW: Duration = Duration::from_secs(10);

enum Nudge {
    /// Input just moved to this client: offer it our clipboard.
    OfferClipboard,
    /// Drop the connection.
    Kick,
}

struct LinkHandle {
    serial: u64,
    outbox: Outbox,
    nudges: mpsc::UnboundedSender<Nudge>,
}

struct Known {
    id: ClientId,
    fingerprint: Fingerprint,
    name: String,
}

/// Shared by the network tasks and the OS input thread.
struct Core {
    app: Arc<App>,
    switch: Mutex<Switch>,
    links: Mutex<HashMap<ClientId, LinkHandle>>,
    known: Mutex<Vec<Known>>,
    /// The capture back-end holds a reference back to this struct, so it is
    /// taken out and dropped explicitly when the host stops.
    capture: Mutex<Option<Capture>>,
    /// When input last left each client. A client may hand its clipboard to
    /// the host only right after that, never at a moment of its choosing.
    handoff: Mutex<HashMap<ClientId, Instant>>,
    locked: AtomicBool,
    serial: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Core {
    /// Run `f` on the switch, then carry out whatever it decided. The switch
    /// lock is released first, so effects can never deadlock against input.
    fn with<R>(&self, f: impl FnOnce(&mut Switch) -> R) -> R {
        let (result, effects, locked) = {
            let mut sw = lock(&self.switch);
            let result = f(&mut sw);
            let effects: Vec<Effect> = sw.drain().collect();
            (result, effects, sw.locked())
        };
        if self.locked.swap(locked, Ordering::Relaxed) != locked {
            self.app.live().locked = locked;
        }
        for effect in effects {
            self.apply(effect);
        }
        result
    }

    fn apply(&self, effect: Effect) {
        match effect {
            Effect::Send { client, msg } => {
                if let Some(link) = lock(&self.links).get(&client) {
                    if !link.outbox.send(msg) {
                        let _ = link.nudges.send(Nudge::Kick);
                    }
                }
            }
            Effect::Capture(on) => {
                if let Some(capture) = lock(&self.capture).as_ref() {
                    capture.set_capture(on);
                }
            }
            Effect::Warp(to) => {
                if let Some(capture) = lock(&self.capture).as_ref() {
                    capture.warp(to);
                }
            }
            Effect::Focus { from, to } => {
                if let Some(id) = from {
                    lock(&self.handoff).insert(id, Instant::now());
                }
                let name = to.and_then(|id| {
                    lock(&self.known)
                        .iter()
                        .find(|k| k.id == id)
                        .map(|k| k.name.clone())
                });
                self.app.live().active = name;
                if let Some(link) =
                    to.and_then(|id| lock(&self.links).get(&id).map(|l| l.nudges.clone()))
                {
                    let _ = link.send(Nudge::OfferClipboard);
                }
            }
        }
    }

    fn clipboard_expected_from(&self, id: ClientId) -> bool {
        lock(&self.handoff)
            .get(&id)
            .is_some_and(|left| left.elapsed() < CLIPBOARD_WINDOW)
    }

    fn id_of(&self, fp: &Fingerprint) -> Option<ClientId> {
        lock(&self.known)
            .iter()
            .find(|k| k.fingerprint == *fp)
            .map(|k| k.id)
    }

    /// Apply the client list and options from `cfg`.
    fn configure(&self, cfg: &Config) {
        let specs: Vec<ClientSpec> = {
            let mut known = lock(&self.known);
            let mut next = known.iter().map(|k| k.id).max().map_or(1, |m| m + 1);
            let mut fresh = Vec::new();
            let mut specs = Vec::new();
            for entry in &cfg.host.clients {
                let id = known
                    .iter()
                    .find(|k| k.fingerprint == entry.fingerprint)
                    .map(|k| k.id)
                    .unwrap_or_else(|| {
                        next += 1;
                        next - 1
                    });
                fresh.push(Known {
                    id,
                    fingerprint: entry.fingerprint,
                    name: entry.name.clone(),
                });
                specs.push(ClientSpec {
                    id,
                    monitor: entry.monitor.clone(),
                    side: entry.side,
                    enabled: entry.enabled,
                });
            }
            *known = fresh;
            specs
        };
        // Clients that were removed or disabled lose their connection.
        let allowed: Vec<ClientId> = specs.iter().filter(|s| s.enabled).map(|s| s.id).collect();
        for (id, link) in lock(&self.links).iter() {
            if !allowed.contains(id) {
                let _ = link.nudges.send(Nudge::Kick);
            }
        }
        self.with(|sw| {
            sw.set_options(
                cfg.host.edges,
                cfg.host.hotkeys,
                f64::from(cfg.host.pointer_speed),
            );
            sw.set_clients(specs);
        });
    }

    fn refresh_screens(&self) {
        let screens = platform::screens();
        if screens.is_empty() {
            self.app.set_notice(NoticeCode::NoScreens, "");
        } else {
            self.app.clear_notice(NoticeCode::NoScreens);
        }
        if self.app.live().screens != screens {
            self.app.live().screens = screens.clone();
            self.with(|sw| sw.set_monitors(screens));
        }
    }
}

impl Sink for Core {
    fn motion(&self, dx: f64, dy: f64, at: Option<Point>) -> bool {
        self.with(|sw| sw.on_motion(dx, dy, at))
    }

    fn button(&self, code: u16, down: bool) -> bool {
        self.with(|sw| sw.on_button(code, down))
    }

    fn scroll(&self, dx: i32, dy: i32) -> bool {
        self.with(|sw| sw.on_scroll(dx, dy))
    }

    fn key(&self, code: u16, down: bool) -> bool {
        self.with(|sw| sw.on_key(code, down))
    }

    fn reset(&self) {
        self.with(Switch::reset_held);
    }
}

/// Run the host until the configuration asks for something this task cannot
/// change in place (another role, another port).
pub async fn run(app: Arc<App>) -> Result<()> {
    let mut changes = app.subscribe();
    let cfg = app.config();
    let core = Arc::new(Core {
        app: app.clone(),
        switch: Mutex::new(Switch::new()),
        links: Mutex::new(HashMap::new()),
        known: Mutex::new(Vec::new()),
        capture: Mutex::new(None),
        handoff: Mutex::new(HashMap::new()),
        locked: AtomicBool::new(false),
        serial: AtomicU64::new(0),
    });
    core.refresh_screens();
    core.configure(&cfg);

    // Input capture. Without it the host still listens, so the GUI can show
    // what is wrong and pairing keeps working.
    match Capture::start(core.clone()) {
        Ok(capture) => {
            app.clear_notice(NoticeCode::CaptureDenied);
            if capture.sees_cursor() {
                app.clear_notice(NoticeCode::NoCursorProbe);
            } else {
                app.set_notice(NoticeCode::NoCursorProbe, "");
            }
            *lock(&core.capture) = Some(capture);
        }
        Err(e) => {
            tracing::error!("input capture unavailable: {e:#}");
            app.set_notice(NoticeCode::CaptureDenied, format!("{e:#}"));
        }
    }

    let listen = (cfg.host.bind.clone(), cfg.host.port);
    let listener = loop {
        match TcpListener::bind((listen.0.as_str(), listen.1)).await {
            Ok(l) => break l,
            Err(e) => {
                tracing::error!("cannot listen on {}:{}: {e}", listen.0, listen.1);
                app.set_notice(
                    NoticeCode::ListenFailed,
                    format!("{}:{} — {e}", listen.0, listen.1),
                );
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = changes.changed() => return Ok(()),
                }
            }
        }
    };
    app.clear_notice(NoticeCode::ListenFailed);
    app.live().port = Some(listen.1);
    tracing::info!("host ready on port {}", listen.1);

    let acceptor = app.identity.acceptor()?;
    let pending = Arc::new(Semaphore::new(MAX_PENDING));
    let discovery = tokio::spawn(discovery::serve(app.clone(), listen.1));
    let mut screens_tick = tokio::time::interval(SCREEN_REFRESH);
    let mut sessions = tokio::task::JoinSet::new();

    let outcome = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, addr)) = accepted else { continue };
                let (core, acceptor, pending) = (core.clone(), acceptor.clone(), pending.clone());
                sessions.spawn(async move {
                    if let Err(e) = connection(core, acceptor, pending, stream, addr).await {
                        tracing::debug!("connection from {addr} ended: {e:#}");
                    }
                });
            }
            Some(_) = sessions.join_next() => {}
            _ = screens_tick.tick() => core.refresh_screens(),
            changed = changes.changed() => {
                if changed.is_err() {
                    break Ok(());
                }
                let next = app.config();
                if next.role != Some(Role::Host) || (next.host.bind.clone(), next.host.port) != listen {
                    break Ok(());
                }
                core.configure(&next);
            }
        }
    };
    discovery.abort();
    sessions.shutdown().await;
    // Bring input home, then stop capturing. The capture is moved out of the
    // lock before it is dropped: stopping it waits for the input thread, which
    // may itself be asking for that lock.
    core.with(|sw| sw.set_clients(Vec::new()));
    let capture = lock(&core.capture).take();
    drop(capture);
    outcome
}

async fn connection(
    core: Arc<Core>,
    acceptor: tokio_rustls::TlsAcceptor,
    pending: Arc<Semaphore>,
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<()> {
    // Everything before `Welcome` is reachable by anyone on the network, so
    // it is bounded in number, time and message size.
    let permit = pending
        .try_acquire_owned()
        .context("too many pending connections")?;
    stream.set_nodelay(true)?;
    let tls = timeout(HANDSHAKE, acceptor.accept(stream))
        .await
        .context("TLS timeout")??;
    let peer = peer_fingerprint(tls.get_ref().1)?;
    let (r, w) = tokio::io::split(tls);
    let (mut reader, mut writer) = (FrameReader::new(r), FrameWriter::new(w));

    let Msg::Hello(hello) = timeout(HANDSHAKE, reader.expect())
        .await
        .context("hello timeout")??
    else {
        bail!("expected hello");
    };
    let app = &core.app;
    let cfg = app.config();
    if hello.version != PROTOCOL_VERSION {
        writer.send(&Msg::Reject(RejectReason::Version)).await?;
        bail!("protocol version {} not supported", hello.version);
    }
    writer
        .send(&Msg::Hello(Hello {
            version: PROTOCOL_VERSION,
            name: cfg.name.clone(),
            os: Os::current(),
            screens: Vec::new(),
            pairing: false,
        }))
        .await?;

    if hello.pairing {
        return pair(app, &mut reader, &mut writer, &hello, peer).await;
    }
    let reject = match cfg.host.clients.iter().find(|c| c.fingerprint == peer) {
        None => Some(RejectReason::NotPaired),
        Some(c) if !c.enabled => Some(RejectReason::Disabled),
        Some(_) => None,
    };
    if let Some(reason) = reject {
        writer.send(&Msg::Reject(reason)).await?;
        bail!("refused: {reason:?}");
    }
    let Some(id) = core.id_of(&peer) else {
        bail!("client vanished from the configuration")
    };
    writer.send(&Msg::Welcome).await?;
    drop(permit);

    session(&core, id, peer, addr, hello, Conn::spawn(reader, writer)).await;
    Ok(())
}

/// Handle a pairing attempt from a not-yet-trusted machine.
async fn pair<R, W>(
    app: &Arc<App>,
    reader: &mut FrameReader<R>,
    writer: &mut FrameWriter<W>,
    hello: &Hello,
    peer: Fingerprint,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let Some(code) = app.take_pairing_attempt() else {
        writer.send(&Msg::Reject(RejectReason::NotPaired)).await?;
        bail!("pairing attempt while the window is closed");
    };
    let me = app.identity.fingerprint;
    let exchange = async {
        let Msg::PairStart { spake } = reader.expect().await? else {
            bail!("expected pairing start");
        };
        pairing::accept(reader, writer, &code, &spake, &me, &peer).await
    };
    // A timeout or a broken exchange counts as a failed attempt too.
    let ok = matches!(timeout(HANDSHAKE * 2, exchange).await, Ok(Ok(true)));
    if !ok {
        app.pairing_failed();
        writer.send(&Msg::Reject(RejectReason::BadCode)).await?;
        bail!("wrong pairing code");
    }
    app.close_pairing();
    let name = yzendris_core::config::clean_name(&hello.name);
    app.update_config(|cfg| {
        let clients = &mut cfg.host.clients;
        if let Some(existing) = clients.iter_mut().find(|c| c.fingerprint == peer) {
            existing.name = name.clone();
            existing.enabled = true;
            return;
        }
        // Put the newcomer on the first side of the primary monitor that no
        // other client occupies.
        let side = [Side::Right, Side::Left, Side::Top, Side::Bottom]
            .into_iter()
            .find(|s| !clients.iter().any(|c| c.monitor.is_empty() && c.side == *s))
            .unwrap_or(Side::Right);
        clients.push(ClientEntry {
            name: name.clone(),
            fingerprint: peer,
            monitor: String::new(),
            side,
            enabled: true,
        });
    })?;
    tracing::info!("paired with a new device");
    Ok(())
}

/// Keep only plausible screens from what a client reports.
fn checked_screens(screens: Vec<Rect>) -> Vec<Rect> {
    const LIMIT: i32 = 1 << 20;
    screens
        .into_iter()
        .filter(|r| (1..=65_536).contains(&r.w) && (1..=65_536).contains(&r.h))
        .filter(|r| r.x.abs() < LIMIT && r.y.abs() < LIMIT)
        .take(16)
        .collect()
}

async fn session(
    core: &Arc<Core>,
    id: ClientId,
    peer: Fingerprint,
    addr: SocketAddr,
    hello: Hello,
    mut conn: Conn,
) {
    let app = &core.app;
    let serial = core.serial.fetch_add(1, Ordering::Relaxed);
    let (nudges, mut nudge_rx) = mpsc::unbounded_channel();
    let screens = checked_screens(hello.screens);

    let handle = LinkHandle {
        serial,
        outbox: conn.outbox.clone(),
        nudges,
    };
    if let Some(old) = lock(&core.links).insert(id, handle) {
        let _ = old.nudges.send(Nudge::Kick); // a reconnect replaces the old link
    }
    // The client was authorized against a snapshot of the configuration taken
    // before several awaits. Check again now that the link is registered: a
    // removal that happened in between has either already been applied (seen
    // here) or will find the link in the table and kick it.
    let still_paired = core.id_of(&peer) == Some(id)
        && app
            .config()
            .host
            .clients
            .iter()
            .any(|c| c.fingerprint == peer && c.enabled);
    if !still_paired {
        let mut links = lock(&core.links);
        if links.get(&id).is_some_and(|l| l.serial == serial) {
            links.remove(&id);
        }
        tracing::warn!("a device was removed while it was connecting; dropped");
        return;
    }
    app.set_peer(peer, |p| {
        p.link = Link::Online;
        p.address = Some(addr.ip().to_string());
        p.os = Some(hello.os);
        p.screens = screens.clone();
        p.latency_ms = None;
    });
    core.with(|sw| sw.client_online(id, screens));
    tracing::info!("a client connected");

    let mut sync = ClipSync::default();
    let mut incoming: Option<Incoming> = None;
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut last_heard = Instant::now();
    let mut ping_sent: Option<(u32, Instant)> = None;
    let mut seq = 0u32;

    loop {
        tokio::select! {
            msg = conn.inbox.recv() => {
                let Some(msg) = msg else { break };
                last_heard = Instant::now();
                match msg {
                    Msg::Ping(n) => { conn.outbox.send(Msg::Pong(n)); }
                    Msg::Pong(n) => {
                        if let Some((_, sent)) = ping_sent.take_if(|(s, _)| *s == n) {
                            let ms = sent.elapsed().as_millis() as u32;
                            app.set_peer(peer, |p| p.latency_ms = Some(ms));
                        }
                    }
                    Msg::Screens(screens) => {
                        let screens = checked_screens(screens);
                        app.set_peer(peer, |p| p.screens = screens.clone());
                        core.with(|sw| sw.client_screens(id, screens));
                    }
                    other => {
                        let starts_transfer = matches!(other, Msg::ClipboardText(_) | Msg::FilesBegin { .. });
                        if starts_transfer && !core.clipboard_expected_from(id) {
                            // Unsolicited: the file frames that follow, if
                            // any, find no open batch and are ignored too.
                            incoming = None;
                            tracing::debug!("ignored clipboard data a client sent unprompted");
                            continue;
                        }
                        if let Err(e) = receive_shared(app, &mut sync, &mut incoming, other).await {
                            tracing::warn!("dropping a client: {e:#}");
                            break;
                        }
                    }
                }
            }
            nudge = nudge_rx.recv() => match nudge {
                Some(Nudge::OfferClipboard) => offer_clipboard(app, &mut sync, &conn).await,
                Some(Nudge::Kick) | None => break,
            },
            _ = ping.tick() => {
                if last_heard.elapsed() > DEAD_AFTER || conn.outbox.stalled() {
                    tracing::warn!("a client stopped responding");
                    break;
                }
                seq = seq.wrapping_add(1);
                ping_sent = Some((seq, Instant::now()));
                conn.outbox.send(Msg::Ping(seq));
            }
        }
    }

    // Only clean up if a newer session has not already taken our place.
    let ours = {
        let mut links = lock(&core.links);
        let ours = links.get(&id).is_some_and(|l| l.serial == serial);
        if ours {
            links.remove(&id);
        }
        ours
    };
    if ours {
        core.with(|sw| sw.client_offline(id));
        app.set_peer(peer, |p| {
            p.link = Link::Offline;
            p.latency_ms = None;
        });
        tracing::info!("a client disconnected");
    }
}

/// Send the peer our clipboard if it changed since the last exchange.
pub async fn offer_clipboard(app: &Arc<App>, sync: &mut ClipSync, conn: &Conn) {
    let cfg = app.config();
    if !cfg.clipboard {
        return;
    }
    let offer = sync
        .offer(&app.clipboard, cfg.files, cfg.max_file_bytes())
        .await;
    if let Some(text) = offer.text {
        conn.outbox.send(text);
    }
    if let Some(files) = offer.files {
        // A transfer already in flight keeps going; this one is skipped.
        let _ = conn.files.try_send(files);
    }
}

/// Handle the messages both roles treat the same way: clipboard text and the
/// file stream. An error means the peer broke the protocol.
pub async fn receive_shared(
    app: &Arc<App>,
    sync: &mut ClipSync,
    incoming: &mut Option<Incoming>,
    msg: Msg,
) -> Result<()> {
    match msg {
        Msg::ClipboardText(text) => {
            if app.config().clipboard {
                sync.accept_text(&app.clipboard, text);
            }
        }
        Msg::FilesBegin { count, .. } => {
            let cfg = app.config();
            *incoming = None;
            if cfg.clipboard && cfg.files {
                *incoming = Some(
                    Incoming::start(&paths::incoming_dir(), count, cfg.max_file_bytes()).await?,
                );
            }
        }
        Msg::FileBegin { name, size } => {
            if let Some(batch) = incoming {
                batch.begin_file(&name, size).await?;
            }
        }
        Msg::FileData(chunk) => {
            if let Some(batch) = incoming {
                batch.data(&chunk).await?;
            }
        }
        Msg::FileEnd { crc32 } => {
            if let Some(batch) = incoming {
                batch.end_file(crc32).await?;
            }
        }
        Msg::FilesEnd => {
            if let Some(batch) = incoming.take() {
                let files = batch.finish().await;
                if !files.is_empty() {
                    tracing::info!("received {} file(s) through the clipboard", files.len());
                    sync.accept_files(&app.clipboard, files);
                }
            }
        }
        // Anything else is not something this side accepts; ignore it.
        _ => {}
    }
    Ok(())
}
