//! Pairing two machines with a short code.
//!
//! The host shows a six-digit code; the user types it on the client. Both run
//! SPAKE2 over the (not yet trusted) TLS connection using the code as the
//! password, then prove to each other that they derived the same key *and*
//! saw the same pair of TLS certificates. Someone in the middle would have to
//! guess the code on the first try — a wrong guess reveals nothing that helps
//! with the next one, and the host closes the pairing window after a few
//! failures.

use anyhow::{anyhow, bail, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::io::{AsyncRead, AsyncWrite};
use yzendris_core::proto::{Msg, RejectReason};
use yzendris_core::Fingerprint;

use crate::wire::{FrameReader, FrameWriter};

pub const CODE_DIGITS: usize = 6;
/// How long the host accepts a new device after the user asks for it.
pub const WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
/// Wrong codes tolerated before the window closes.
pub const MAX_ATTEMPTS: u8 = 3;

const CONTEXT: &[u8] = b"yzendris/pair/v1";

/// A uniformly random numeric code.
pub fn new_code() -> Result<String> {
    const LIMIT: u32 = 1_000_000;
    // Rejection sampling keeps every code equally likely.
    let ceiling = u32::MAX - u32::MAX % LIMIT;
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|e| anyhow!("no secure randomness: {e}"))?;
        let n = u32::from_le_bytes(bytes);
        if n < ceiling {
            return Ok(format!("{:0width$}", n % LIMIT, width = CODE_DIGITS));
        }
    }
}

/// Keep only the digits of what the user typed (they may add spaces).
pub fn normalize_code(input: &str) -> Option<String> {
    let code: String = input.chars().filter(char::is_ascii_digit).collect();
    (code.len() == CODE_DIGITS).then_some(code)
}

fn start(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_symmetric(&Password::new(code), &Identity::new(CONTEXT))
}

/// Proof of the shared key, bound to both certificates so it cannot be
/// replayed across a relayed connection.
fn proof(key: &[u8], who: &[u8], client: &Fingerprint, host: &Fingerprint) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(who);
    mac.update(&client.0);
    mac.update(&host.0);
    mac
}

fn tag(mac: Hmac<Sha256>) -> [u8; 32] {
    mac.finalize().into_bytes().into()
}

/// Client side. `host` is the fingerprint the TLS layer observed.
pub async fn join<R, W>(
    reader: &mut FrameReader<R>,
    writer: &mut FrameWriter<W>,
    code: &str,
    me: &Fingerprint,
    host: &Fingerprint,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (state, spake) = start(code);
    writer.send(&Msg::PairStart { spake }).await?;
    let reply = match reader.expect().await? {
        Msg::PairReply { spake } => spake,
        Msg::Reject(reason) => bail!(Refused(reason)),
        _ => bail!("unexpected reply while pairing"),
    };
    let key = state
        .finish(&reply)
        .map_err(|_| anyhow!("invalid pairing message"))?;
    writer
        .send(&Msg::PairConfirm {
            mac: tag(proof(&key, b"client", me, host)),
        })
        .await?;
    match reader.expect().await? {
        Msg::PairDone { mac } => proof(&key, b"host", me, host)
            .verify_slice(&mac)
            .map_err(|_| anyhow!(Refused(RejectReason::BadCode))),
        Msg::Reject(reason) => bail!(Refused(reason)),
        _ => bail!("unexpected reply while pairing"),
    }
}

/// Host side, entered after receiving the client's `PairStart`. Returns
/// `Ok(false)` when the client did not know the code.
pub async fn accept<R, W>(
    reader: &mut FrameReader<R>,
    writer: &mut FrameWriter<W>,
    code: &str,
    client_spake: &[u8],
    me: &Fingerprint,
    client: &Fingerprint,
) -> Result<bool>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (state, spake) = start(code);
    let Ok(key) = state.finish(client_spake) else {
        return Ok(false);
    };
    writer.send(&Msg::PairReply { spake }).await?;
    let Msg::PairConfirm { mac } = reader.expect().await? else {
        bail!("unexpected message while pairing");
    };
    if proof(&key, b"client", client, me)
        .verify_slice(&mac)
        .is_err()
    {
        return Ok(false);
    }
    writer
        .send(&Msg::PairDone {
            mac: tag(proof(&key, b"host", client, me)),
        })
        .await?;
    Ok(true)
}

/// The host turned the connection down.
#[derive(Debug)]
pub struct Refused(pub RejectReason);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            RejectReason::Version => "the other machine runs an incompatible Yzendris version",
            RejectReason::NotPaired => "the host is not accepting new devices right now",
            RejectReason::BadCode => "wrong pairing code",
            RejectReason::Busy => "the host is busy, try again in a moment",
            RejectReason::Disabled => "this device is disabled on the host",
        })
    }
}

impl std::error::Error for Refused {}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(host_code: &str, client_code: &str, relay: bool) -> (Result<bool>, Result<()>) {
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let (host_fp, client_fp) = (Fingerprint([1; 32]), Fingerprint([2; 32]));
        // With a relay in the middle the client sees the relay's certificate.
        let seen_host = if relay { Fingerprint([9; 32]) } else { host_fp };

        let host_code = host_code.to_owned();
        let host = tokio::spawn(async move {
            let (mut r, mut w) = (FrameReader::new(ar), FrameWriter::new(aw));
            let Msg::PairStart { spake } = r.expect().await? else {
                bail!("no start")
            };
            let ok = accept(&mut r, &mut w, &host_code, &spake, &host_fp, &client_fp).await?;
            if !ok {
                w.send(&Msg::Reject(RejectReason::BadCode)).await?;
            }
            Ok(ok)
        });
        let (mut r, mut w) = (FrameReader::new(br), FrameWriter::new(bw));
        let client = join(&mut r, &mut w, client_code, &client_fp, &seen_host).await;
        (host.await.unwrap(), client)
    }

    #[tokio::test]
    async fn matching_code_pairs() {
        let (host, client) = run("123456", "123456", false).await;
        assert!(host.unwrap());
        client.unwrap();
    }

    #[tokio::test]
    async fn wrong_code_is_refused_on_both_sides() {
        let (host, client) = run("123456", "654321", false).await;
        assert!(!host.unwrap());
        assert!(client.is_err());
    }

    #[tokio::test]
    async fn relayed_handshake_fails_even_with_the_right_code() {
        let (host, client) = run("123456", "123456", true).await;
        assert!(!host.unwrap());
        assert!(client.is_err());
    }

    #[test]
    fn codes_are_well_formed() {
        for _ in 0..200 {
            let code = new_code().unwrap();
            assert_eq!(code.len(), CODE_DIGITS);
            assert!(code.bytes().all(|b| b.is_ascii_digit()));
        }
        assert_eq!(normalize_code(" 123 456 ").as_deref(), Some("123456"));
        assert_eq!(normalize_code("12345"), None);
        assert_eq!(normalize_code("1234567"), None);
    }
}
