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
    write_private_key(key_path, &key_pem).context("write key.pem")?;

    tracing::info!("TLS: generated new self-signed cert → {}", cert_path.display());

    load_cert(cert_path, key_path)
}

/// Write the private key with owner-only permissions. `std::fs::write` would
/// leave it world-readable under the usual 022 umask, and this key is what
/// proves our identity to the host.
fn write_private_key(path: &Path, pem: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(pem.as_bytes())
    }
    #[cfg(not(unix))]
    {
        // On Windows the per-user config dir is already ACL'd to the account.
        std::fs::write(path, pem)
    }
}

/// Compute the SHA-256 fingerprint of a DER certificate.
/// Format: `sha256:aabb:ccdd:…` (SSH-style lowercase hex pairs).
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(cert.as_ref());
    let hex_pairs: Vec<String> = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{}", hex_pairs.join(":"))
}

/// Load trusted host fingerprints from `trusted_peers.txt` (one per line,
/// `#` comments allowed). Same file format the host uses for our fingerprint.
pub fn load_trusted(config_dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(config_dir.join("trusted_peers.txt")) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_owned())
        .collect()
}

/// Verifies the *host's* certificate by pinned SHA-256 fingerprint.
///
/// Without this the client accepted any TLS peer: the pinning was one-way (the
/// host checked us, we checked nobody), so anyone on the LAN could connect to
/// the listening port and inject keystrokes and mouse input through uinput.
#[derive(Debug)]
struct FingerprintClientVerifier {
    trusted: Vec<String>,
    provider: rustls::crypto::CryptoProvider,
    /// We accept any issuer — the pin is on the leaf, not a chain.
    no_hints: Vec<rustls::DistinguishedName>,
}

impl rustls::server::danger::ClientCertVerifier for FingerprintClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &self.no_hints
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        if self.trusted.contains(&fp) {
            tracing::info!("TLS: verified host fingerprint {}", &fp[..40]);
            Ok(rustls::server::danger::ClientCertVerified::assertion())
        } else {
            tracing::warn!("TLS: UNTRUSTED host fingerprint {fp}");
            tracing::warn!("Add it to trusted_peers.txt in the config dir and restart the client.");
            Err(rustls::Error::General(format!("untrusted host fingerprint: {fp}")))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build a `TlsAcceptor` that presents our cert AND requires the connecting
/// host to present one whose fingerprint is in `trusted`.
pub fn make_acceptor(
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    trusted: Vec<String>,
) -> Result<TlsAcceptor> {
    let verifier = Arc::new(FingerprintClientVerifier {
        trusted,
        provider: rustls::crypto::ring::default_provider(),
        no_hints: Vec::new(),
    });
    let config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, key)
        .context("build ServerConfig")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}
