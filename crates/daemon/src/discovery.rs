//! Finding the host on the local network.
//!
//! A client broadcasts a tiny UDP question on the Yzendris port; hosts answer
//! with their name and TCP port. Two questions exist:
//!
//! * `pair` — answered only while the host's pairing window is open, so a
//!   host is invisible on the network the rest of the time.
//! * `find <fingerprint>` — answered only by the host with that identity, so
//!   a paired client can follow its host across an address change.
//!
//! Answers are unauthenticated hints. Trust comes from the TLS handshake that
//! follows, never from discovery.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use yzendris_core::config::clean_name;
use yzendris_core::ipc::FoundHost;
use yzendris_core::{Fingerprint, DEFAULT_PORT};

use crate::app::App;

const MAGIC: &str = "yzendris1";
const MAX_DATAGRAM: usize = 512;
/// Minimum spacing between answers, to be useless as a traffic amplifier.
const ANSWER_SPACING: Duration = Duration::from_millis(100);

#[derive(Serialize, Deserialize)]
struct Answer {
    name: String,
    port: u16,
}

/// Host: answer discovery questions until dropped. Questions always arrive on
/// the well-known port; the answer says which TCP `port` to connect to.
pub async fn serve(app: Arc<App>, port: u16) {
    let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("discovery disabled: {e}");
            return;
        }
    };
    let mut buf = [0u8; MAX_DATAGRAM];
    let mut last_answer = Instant::now() - ANSWER_SPACING;
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buf).await else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let Ok(text) = std::str::from_utf8(&buf[..len]) else {
            continue;
        };
        let mut words = text.split_ascii_whitespace();
        if words.next() != Some(MAGIC) {
            continue;
        }
        let wanted = match (words.next(), words.next()) {
            (Some("pair"), None) => app.pairing_open(),
            (Some("find"), Some(fp)) => {
                fp.parse::<Fingerprint>().ok() == Some(app.identity.fingerprint)
            }
            _ => false,
        };
        if !wanted || last_answer.elapsed() < ANSWER_SPACING {
            continue;
        }
        last_answer = Instant::now();
        let answer = Answer {
            name: app.config().name,
            port,
        };
        if let Ok(json) = serde_json::to_vec(&answer) {
            let _ = socket.send_to(&json, from).await;
        }
    }
}

async fn ask(question: &str, wait: Duration) -> Vec<(IpAddr, Answer)> {
    let mut found: Vec<(IpAddr, Answer)> = Vec::new();
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
        return found;
    };
    let _ = socket.set_broadcast(true);
    let datagram = format!("{MAGIC} {question}");
    let mut targets = vec![IpAddr::V4(Ipv4Addr::BROADCAST)];
    // The subnet's directed broadcast reaches hosts that ignore 255.255.255.255.
    if let Some(IpAddr::V4(ip)) = crate::platform::lan_address() {
        let [a, b, c, _] = ip.octets();
        targets.push(IpAddr::V4(Ipv4Addr::new(a, b, c, 255)));
        // Same machine, for trying both roles side by side.
        targets.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    for target in targets {
        let _ = socket
            .send_to(datagram.as_bytes(), SocketAddr::new(target, DEFAULT_PORT))
            .await;
    }

    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = [0u8; MAX_DATAGRAM];
    while let Ok(Ok((len, from))) =
        tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await
    {
        let Ok(mut answer) = serde_json::from_slice::<Answer>(&buf[..len]) else {
            continue;
        };
        answer.name = clean_name(&answer.name);
        if answer.port != 0 && !found.iter().any(|(ip, _)| *ip == from.ip()) {
            found.push((from.ip(), answer));
        }
        if found.len() >= 16 {
            break;
        }
    }
    found
}

/// Client: hosts currently open for pairing.
pub async fn hosts_open_for_pairing() -> Vec<FoundHost> {
    ask("pair", Duration::from_millis(900))
        .await
        .into_iter()
        .map(|(ip, a)| FoundHost {
            name: a.name,
            addr: SocketAddr::new(ip, a.port).to_string(),
        })
        .collect()
}

/// Client: the current address of the host with this identity, if it answers.
pub async fn locate(host: &Fingerprint) -> Option<String> {
    ask(&format!("find {host}"), Duration::from_millis(900))
        .await
        .into_iter()
        .next()
        .map(|(ip, a)| SocketAddr::new(ip, a.port).to_string())
}
