/// TLS support for the Linux client (network-server side).
///
/// On first run generates a self-signed certificate and saves it to
/// `~/.config/yzendris/cert.pem` + `key.pem`.  Prints the SHA-256 fingerprint
/// so the user can configure the Windows server's `trusted_peers.txt`.
use anyhow::{Context, Result};
use std::{path::Path, sync::Arc};
use tokio_rustls::TlsAcceptor;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Load or generate the TLS certificate.  Returns `(cert_chain, private_key)`.
pub fn load_or_generate_cert(config_dir: &Path) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_path = config_dir.join("cert.pem");
    let key_path  = config_dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        load_cert(&cert_path, &key_path)
    } else {
        generate_cert(config_dir, &cert_path, &key_path)
    }
}

fn load_cert(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_pem = std::fs::read(cert_path).context("reading cert.pem")?;
    let key_pem  = std::fs::read(key_path).context("reading key.pem")?;

    let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .context("parse cert.pem")?;

    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .context("parse key.pem")?
        .context("no private key in key.pem")?;

    Ok((certs, key))
}

fn generate_cert(
    config_dir: &Path,
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    use rcgen::{generate_simple_self_signed, CertifiedKey};

    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["yzendris".to_string()])
            .context("generate self-signed cert")?;

    let cert_pem = cert.pem();
    let key_pem  = key_pair.serialize_pem();

    std::fs::create_dir_all(config_dir).context("create config dir")?;
    std::fs::write(cert_path, &cert_pem).context("write cert.pem")?;
    std::fs::write(key_path,  &key_pem).context("write key.pem")?;

    tracing::info!("TLS: generated new self-signed cert → {}", cert_path.display());

    load_cert(cert_path, key_path)
}

/// Compute the SHA-256 fingerprint of a DER certificate.
/// Format: `sha256:aabb:ccdd:…` (SSH-style lowercase hex pairs).
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(cert.as_ref());
    let hex_pairs: Vec<String> = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{}", hex_pairs.join(":"))
}

/// Build a `TlsAcceptor` from the cert and key.
pub fn make_acceptor(
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<TlsAcceptor> {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .context("build ServerConfig")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}
