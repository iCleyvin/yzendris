//! State shared by every part of the daemon.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use anyhow::Result;
use tokio::sync::{watch, Notify};
use yzendris_core::config::Config;
use yzendris_core::geom::Rect;
use yzendris_core::ipc::{Link, Notice, NoticeCode, Pairing, Peer, Status};
use yzendris_core::layout::Screen;
use yzendris_core::proto::Os;
use yzendris_core::{paths, Fingerprint};

use crate::clipboard::Clipboard;
use crate::identity::Identity;
use crate::logbuf::LogBuffer;
use crate::pairing;

/// A pairing window opened on the host.
pub struct PairWindow {
    pub code: String,
    pub deadline: Instant,
    pub attempts_left: u8,
}

/// What we currently know about a peer's connection.
#[derive(Clone)]
pub struct PeerLive {
    pub link: Link,
    pub address: Option<String>,
    pub os: Option<Os>,
    pub screens: Vec<Rect>,
    pub latency_ms: Option<u32>,
}

impl Default for PeerLive {
    fn default() -> Self {
        Self {
            link: Link::Offline,
            address: None,
            os: None,
            screens: Vec::new(),
            latency_ms: None,
        }
    }
}

#[derive(Default)]
pub struct Live {
    pub screens: Vec<Screen>,
    pub port: Option<u16>,
    pub peers: HashMap<Fingerprint, PeerLive>,
    pub active: Option<String>,
    pub locked: bool,
    pub pairing: Option<PairWindow>,
    pub notices: Vec<Notice>,
}

pub struct App {
    pub identity: Identity,
    pub logs: LogBuffer,
    pub clipboard: Clipboard,
    config: Mutex<Config>,
    live: Mutex<Live>,
    /// Bumped whenever the configuration changes.
    changes: watch::Sender<u64>,
    /// Signalled to shut the daemon down.
    pub quit: Notify,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not take the whole daemon down with it.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl App {
    pub fn new(logs: LogBuffer) -> Result<Arc<Self>> {
        let config = Config::load(&paths::config_file())?;
        Ok(Arc::new(Self {
            identity: Identity::load_or_create()?,
            logs,
            clipboard: Clipboard::start(),
            config: Mutex::new(config),
            live: Mutex::new(Live::default()),
            changes: watch::channel(0).0,
            quit: Notify::new(),
        }))
    }

    pub fn config(&self) -> Config {
        lock(&self.config).clone()
    }

    /// Change the configuration, persist it, and wake whoever depends on it.
    pub fn update_config(&self, change: impl FnOnce(&mut Config)) -> Result<()> {
        let mut guard = lock(&self.config);
        let mut next = guard.clone();
        change(&mut next);
        next.sanitize();
        if *guard != next {
            next.save(&paths::config_file())?;
            *guard = next;
            drop(guard);
            self.changes.send_modify(|n| *n += 1);
        }
        Ok(())
    }

    /// A receiver that resolves `changed()` after every configuration change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn live(&self) -> MutexGuard<'_, Live> {
        lock(&self.live)
    }

    pub fn set_peer(&self, fp: Fingerprint, change: impl FnOnce(&mut PeerLive)) {
        change(self.live().peers.entry(fp).or_default());
    }

    pub fn set_notice(&self, code: NoticeCode, detail: impl Into<String>) {
        let mut live = self.live();
        live.notices.retain(|n| n.code != code);
        live.notices.push(Notice {
            code,
            detail: detail.into(),
        });
    }

    pub fn clear_notice(&self, code: NoticeCode) {
        self.live().notices.retain(|n| n.code != code);
    }

    /// Reset everything a role task published, before another one starts.
    pub fn reset_live(&self) {
        *self.live() = Live::default();
    }

    // ── Pairing window (host) ───────────────────────────────────────────────

    pub fn open_pairing(&self) -> Result<String> {
        let code = pairing::new_code()?;
        self.live().pairing = Some(PairWindow {
            code: code.clone(),
            deadline: Instant::now() + pairing::WINDOW,
            attempts_left: pairing::MAX_ATTEMPTS,
        });
        Ok(code)
    }

    pub fn close_pairing(&self) {
        self.live().pairing = None;
    }

    pub fn pairing_open(&self) -> bool {
        self.pairing_code().is_some()
    }

    /// The current code, if the window is still open.
    pub fn pairing_code(&self) -> Option<String> {
        let mut live = self.live();
        match &live.pairing {
            Some(w) if w.deadline > Instant::now() && w.attempts_left > 0 => Some(w.code.clone()),
            Some(_) => {
                live.pairing = None;
                None
            }
            None => None,
        }
    }

    /// Record a failed attempt; the window closes once they run out.
    pub fn pairing_failed(&self) {
        let mut live = self.live();
        if let Some(w) = live.pairing.as_mut() {
            w.attempts_left = w.attempts_left.saturating_sub(1);
            if w.attempts_left == 0 {
                live.pairing = None;
                tracing::warn!("pairing closed after too many wrong codes");
            }
        }
    }

    // ── Status for the GUI ──────────────────────────────────────────────────

    pub fn status(&self) -> Status {
        let cfg = self.config();
        let _ = self.pairing_code(); // expire a stale window
        let live = self.live();
        let peer = |name: &str, fp: Fingerprint| {
            let l = live.peers.get(&fp).cloned().unwrap_or_default();
            Peer {
                name: name.to_owned(),
                fingerprint: fp,
                link: l.link,
                address: l.address,
                os: l.os,
                screens: l.screens,
                latency_ms: l.latency_ms,
            }
        };
        let peers = match cfg.role {
            Some(yzendris_core::config::Role::Host) => cfg
                .host
                .clients
                .iter()
                .map(|c| peer(&c.name, c.fingerprint))
                .collect(),
            Some(yzendris_core::config::Role::Client) => cfg
                .client
                .host_fingerprint
                .map(|fp| {
                    let mut p = peer(&cfg.client.host_name, fp);
                    p.address
                        .get_or_insert_with(|| cfg.client.host_addr.clone());
                    vec![p]
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut notices = live.notices.clone();
        if cfg.clipboard && self.clipboard.broken() {
            notices.push(Notice {
                code: NoticeCode::ClipboardUnavailable,
                detail: String::new(),
            });
        }
        Status {
            version: yzendris_core::VERSION.to_owned(),
            role: cfg.role,
            name: cfg.name,
            os: Os::current(),
            fingerprint: self.identity.fingerprint,
            address: crate::platform::lan_address().map(|ip| ip.to_string()),
            port: live.port,
            screens: live.screens.clone(),
            peers,
            active: live.active.clone(),
            locked: live.locked,
            pairing: live.pairing.as_ref().map(|w| Pairing {
                code: w.code.clone(),
                seconds_left: w
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs() as u32,
            }),
            notices,
        }
    }
}
