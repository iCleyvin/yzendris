//! Canned data for `--demo <scene>`: lets you see every screen without a
//! running service or a second computer. Used for screenshots and UI work.

use yzendris_core::config::{ClientEntry, Config, Role};
use yzendris_core::geom::{Rect, Side};
use yzendris_core::ipc::{FoundHost, Link, Notice, NoticeCode, Pairing, Peer, Status};
use yzendris_core::layout::Screen;
use yzendris_core::proto::Os;
use yzendris_core::Fingerprint;

use crate::backend::Shared;

fn peer(name: &str, seed: u8, link: Link, os: Os, w: i32, h: i32, ms: u32) -> Peer {
    Peer {
        name: name.into(),
        fingerprint: Fingerprint([seed; 32]),
        link,
        address: Some(format!("192.168.1.{}", 40 + seed)),
        os: Some(os),
        screens: vec![Rect::new(0, 0, w, h)],
        latency_ms: (link == Link::Online).then_some(ms),
    }
}

fn entry(name: &str, seed: u8, monitor: &str, side: Side, enabled: bool) -> ClientEntry {
    ClientEntry {
        name: name.into(),
        fingerprint: Fingerprint([seed; 32]),
        monitor: monitor.into(),
        side,
        enabled,
    }
}

pub fn scene(name: &str) -> Shared {
    let mut config = Config {
        name: "escritorio".into(),
        ..Config::default()
    };
    let mut status = Status {
        version: yzendris_core::VERSION.into(),
        role: None,
        name: config.name.clone(),
        os: Os::Linux,
        fingerprint: Fingerprint([0x3f; 32]),
        address: Some("192.168.1.20".into()),
        port: None,
        screens: vec![],
        peers: vec![],
        active: None,
        locked: false,
        pairing: None,
        notices: vec![],
    };
    let mut shared = Shared {
        online: true,
        ..Shared::default()
    };

    match name {
        "welcome" => {}
        "client" | "join" => {
            config.role = Some(Role::Client);
            config.name = "portátil".into();
            status.screens = vec![Screen {
                name: "eDP-1".into(),
                rect: Rect::new(0, 0, 1920, 1200),
                primary: true,
            }];
            if name == "client" {
                config.client.host_name = "escritorio".into();
                config.client.host_fingerprint = Some(Fingerprint([0x3f; 32]));
                let mut host = peer("escritorio", 0x3f, Link::Online, Os::Windows, 0, 0, 1);
                host.address = Some("192.168.1.20:7547".into());
                status.peers = vec![host];
            } else {
                shared.hosts = vec![FoundHost {
                    name: "escritorio".into(),
                    addr: "192.168.1.20:7547".into(),
                }];
            }
        }
        _ => {
            config.role = Some(Role::Host);
            config.host.clients = vec![
                entry("portátil", 1, "DP-1", Side::Right, true),
                entry("bazzite", 2, "DP-1", Side::Left, true),
                entry("mini-pc", 3, "DP-2", Side::Top, true),
            ];
            status.port = Some(7547);
            status.screens = vec![
                Screen {
                    name: "DP-1".into(),
                    rect: Rect::new(0, 0, 2560, 1440),
                    primary: true,
                },
                Screen {
                    name: "DP-2".into(),
                    rect: Rect::new(2560, 180, 1920, 1080),
                    primary: false,
                },
            ];
            status.peers = vec![
                peer("portátil", 1, Link::Online, Os::Linux, 1920, 1200, 1),
                peer("bazzite", 2, Link::Online, Os::Linux, 2560, 1600, 2),
                peer("mini-pc", 3, Link::Offline, Os::Windows, 1920, 1080, 0),
            ];
            match name {
                "pairing" => {
                    status.pairing = Some(Pairing {
                        code: "482913".into(),
                        seconds_left: 96,
                    })
                }
                "active" => status.active = Some("portátil".into()),
                "notice" => {
                    status.notices = vec![Notice {
                        code: NoticeCode::NoCursorProbe,
                        detail: String::new(),
                    }];
                }
                "empty" => {
                    config.host.clients.clear();
                    status.peers.clear();
                    status.screens.truncate(1);
                }
                _ => {}
            }
        }
    }
    status.role = config.role;
    status.name = config.name.clone();
    shared.logs = [
        "09:14:02 INFO  Yzendris 0.2.0 started (3f3f-3f3f-3f3f)",
        "09:14:02 INFO  host ready on port 7547",
        "09:14:05 INFO  a client connected",
        "09:20:41 WARN  a client stopped responding",
        "09:20:44 INFO  a client connected",
        "09:31:10 INFO  received 2 file(s) through the clipboard",
    ]
    .map(String::from)
    .to_vec();
    shared.status = Some(status);
    shared.config = Some(config);
    shared
}
