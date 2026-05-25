/// TLS support for the Windows server (network-client side).
///
/// Uses a custom `ServerCertVerifier` that ONLY checks the SHA-256 fingerprint
/// against a local `trusted_peers.txt` file (one fingerprint per line).
/// On first connection with an unknown fingerprint, prints it and prompts the
/// user to add it manually to `trusted_peers.txt`.
use anyhow::{Context, Result};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{ring as crypto_ring, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, Error as TlsError, SignatureScheme,
};
use std::{path::Path, sync::Arc};
use tokio_rustls::TlsConnector;
use tracing::{info, warn};

// ─── Fingerprint helpers ──────────────────────────────────────────────────────

/// SHA-256 fingerprint in `sha256:aabb:ccdd:…` format.
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(cert.as_ref());
    let pairs: Vec<String> = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{}", pairs.join(":"))
}

/// Load trusted fingerprints from `trusted_peers.txt` (one per line, # comments).
pub fn load_trusted(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_owned())
        .collect()
}

/// Persist a new fingerprint to `trusted_peers.txt`.
#[allow(dead_code)]
pub fn trust_fingerprint(path: &Path, fp: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .context("open trusted_peers.txt")?;
    writeln!(f, "{fp}").context("write fingerprint")?;
    Ok(())
}

// ─── Custom certificate verifier ─────────────────────────────────────────────

#[derive(Debug)]
pub struct FingerprintVerifier {
    trusted: Vec<String>,
    // Cache the provider so we don't reconstruct it per-call.
    provider: rustls::crypto::CryptoProvider,
}

impl FingerprintVerifier {
    pub fn new(trusted: Vec<String>) -> Self {
        Self {
            trusted,
            provider: crypto_ring::default_provider(),
        }
    }

    #[allow(dead_code)]
    pub fn is_trusted(&self, cert: &CertificateDer<'_>) -> bool {
        let fp = fingerprint(cert);
        self.trusted.contains(&fp)
    }
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let fp = fingerprint(end_entity);
        if self.trusted.contains(&fp) {
            info!("TLS: verified peer fingerprint {}", &fp[..40]);
            Ok(ServerCertVerified::assertion())
        } else {
            warn!("TLS: UNTRUSTED fingerprint {fp}");
            warn!("Add it to trusted_peers.txt and restart yzendris-server.");
            Err(TlsError::General(format!("untrusted server fingerprint: {fp}")))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
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
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ─── TlsConnector factory ─────────────────────────────────────────────────────

/// Build a `TlsConnector` that verifies the peer fingerprint.
pub fn make_connector(trusted: Vec<String>) -> Result<TlsConnector> {
    let verifier = Arc::new(FingerprintVerifier::new(trusted));
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}
