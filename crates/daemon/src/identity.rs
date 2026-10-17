//! This machine's cryptographic identity and the TLS configuration built on
//! it.
//!
//! There is no certificate authority. Each machine has one self-signed
//! certificate, created on first run, and peers recognise each other by its
//! SHA-256 fingerprint. TLS proves the peer holds the private key for the
//! certificate it presents; pinning proves it is the certificate we paired
//! with.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use yzendris_core::config::write_private;
use yzendris_core::{paths, Fingerprint};

pub struct Identity {
    pub fingerprint: Fingerprint,
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    provider: Arc<CryptoProvider>,
}

impl Identity {
    /// Load the certificate and key, creating them on first run.
    pub fn load_or_create() -> Result<Self> {
        let (cert_path, key_path) = (paths::cert_file(), paths::key_file());
        if !cert_path.exists() || !key_path.exists() {
            let made = rcgen::generate_simple_self_signed(vec!["yzendris".to_owned()])
                .context("generating certificate")?;
            // Key first: a certificate without its key is useless, the
            // reverse would leave a half-written identity that looks valid.
            write_private(&key_path, made.signing_key.serialize_pem().as_bytes())?;
            write_private(&cert_path, made.cert.pem().as_bytes())?;
            tracing::info!("created a new identity for this machine");
        }
        let cert = CertificateDer::from_pem_file(&cert_path)
            .map_err(|e| anyhow!("reading {}: {e}", cert_path.display()))?;
        let key = PrivateKeyDer::from_pem_file(&key_path)
            .map_err(|e| anyhow!("reading {}: {e}", key_path.display()))?;
        Ok(Self {
            fingerprint: Fingerprint::of_der(&cert),
            cert,
            key,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        })
    }

    /// TLS for the host's listening socket. Every client must present a
    /// certificate; whether it is a *paired* one is checked right after the
    /// handshake, before any message is processed.
    pub fn acceptor(&self) -> Result<TlsAcceptor> {
        let verifier = Arc::new(PinVerifier {
            expect: None,
            provider: self.provider.clone(),
        });
        let config = rustls::ServerConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![self.cert.clone()], self.key.clone_key())?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }

    /// TLS for connecting to a host. With `expect` set, the handshake fails
    /// unless the host presents exactly that certificate — and it fails before
    /// our own certificate is sent. `None` is used only while pairing.
    pub fn connector(&self, expect: Option<Fingerprint>) -> Result<TlsConnector> {
        let verifier = Arc::new(PinVerifier {
            expect,
            provider: self.provider.clone(),
        });
        let mut config = rustls::ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![self.cert.clone()], self.key.clone_key())?;
        config.enable_sni = false;
        Ok(TlsConnector::from(Arc::new(config)))
    }
}

/// Name sent in the (unused) TLS server-name field.
pub fn server_name() -> ServerName<'static> {
    ServerName::try_from("yzendris").expect("static name is valid")
}

/// Fingerprint of the certificate the peer authenticated with.
pub fn peer_fingerprint(conn: &rustls::CommonState) -> Result<Fingerprint> {
    conn.peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| Fingerprint::of_der(cert))
        .ok_or_else(|| anyhow!("peer presented no certificate"))
}

/// Accepts a self-signed peer certificate, optionally pinned to one
/// fingerprint. The handshake signature is still verified, so the peer must
/// own the certificate's private key.
#[derive(Debug)]
struct PinVerifier {
    expect: Option<Fingerprint>,
    provider: Arc<CryptoProvider>,
}

impl PinVerifier {
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        match self.expect {
            Some(want) if Fingerprint::of_der(cert) != want => Err(rustls::Error::General(
                "the host presented a different identity than the one paired".into(),
            )),
            _ => Ok(()),
        }
    }

    fn tls12_refused() -> rustls::Error {
        rustls::Error::General("TLS 1.2 is not supported".into())
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|()| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(Self::tls12_refused())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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

impl ClientCertVerifier for PinVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|()| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(Self::tls12_refused())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
