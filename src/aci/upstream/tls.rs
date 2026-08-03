//! TLS SPKI-pinning reqwest client and response-header extraction.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use sha2::{Digest, Sha256};
use x509_parser::prelude::parse_x509_certificate;

use super::UpstreamError;

pub(super) fn response_headers(resp: &reqwest::Response) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for (k, v) in resp.headers().iter() {
        if let Ok(value) = v.to_str() {
            headers.insert(k.to_string(), value.to_string());
        }
    }
    headers
}

pub(super) fn pinned_spki_client(
    accepted_spkis: Vec<String>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, UpstreamError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let inner = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| UpstreamError::Transport(format!("failed to build TLS verifier: {e}")))?;
    let verifier = Arc::new(SpkiPinVerifier {
        inner,
        accepted: accepted_spkis.into_iter().collect(),
    });
    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_seconds))
        .read_timeout(Duration::from_secs(read_timeout_seconds))
        .use_preconfigured_tls(tls)
        .build()
        .map_err(|e| UpstreamError::Transport(e.to_string()))
}

struct SpkiPinVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    accepted: HashSet<String>,
}

impl fmt::Debug for SpkiPinVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpkiPinVerifier")
            .field("accepted_count", &self.accepted.len())
            .finish()
    }
}

impl ServerCertVerifier for SpkiPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        if self.accepted.is_empty() {
            return Err(RustlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }

        let (_, cert) = parse_x509_certificate(end_entity.as_ref())
            .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        let digest = Sha256::digest(cert.public_key().raw);
        let digest = hex::encode(digest);
        let spki_matches = self.accepted.contains(&digest);

        if spki_matches {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(RustlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Client for the `aci` CLI: it has no pin before the first handshake — that
/// is how it learns the SPKI to check at §9.1(6) — so it records what it sees
/// and enforces any pin registered afterwards.
pub fn observing_spki_client(
    observations: Arc<SpkiObservations>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, UpstreamError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let inner = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| UpstreamError::Transport(format!("failed to build TLS verifier: {e}")))?;
    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(ObservingSpkiVerifier {
            inner,
            observations,
        }))
        .with_no_client_auth();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_seconds))
        .read_timeout(Duration::from_secs(read_timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .use_preconfigured_tls(tls)
        .build()
        .map_err(|e| UpstreamError::Transport(e.to_string()))
}

/// Per-hostname record of observed leaf SPKIs and registered pins, shared
/// between an [`observing_spki_client`] and the code that owns it.
#[derive(Debug, Default)]
pub struct SpkiObservations {
    observed: Mutex<HashMap<String, String>>,
    pins: Mutex<HashMap<String, String>>,
}

impl SpkiObservations {
    /// The leaf SPKI sha256 (hex) observed on the most recent TLS handshake
    /// to `host`; `None` for hosts never contacted over TLS.
    pub fn observed_spki(&self, host: &str) -> Option<String> {
        self.observed
            .lock()
            .expect("observed-SPKI map poisoned")
            .get(&host.to_ascii_lowercase())
            .cloned()
    }

    /// Enforce `spki_sha256` (hex) on every future TLS handshake to `host`;
    /// a handshake presenting any other key fails closed.
    pub fn pin(&self, host: &str, spki_sha256: &str) {
        self.pins
            .lock()
            .expect("SPKI pin map poisoned")
            .insert(host.to_ascii_lowercase(), spki_sha256.to_ascii_lowercase());
    }

    /// Enforce any pin registered for `host`, then record the SPKI observed.
    /// A rejected handshake records nothing: the observation map feeds
    /// transcripts, which must never report a key that was refused.
    fn observe(&self, host: String, spki: String) -> Result<(), RustlsError> {
        if let Some(expected) = self.pins.lock().expect("SPKI pin map poisoned").get(&host) {
            if *expected != spki {
                return Err(RustlsError::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ));
            }
        }
        self.observed
            .lock()
            .expect("observed-SPKI map poisoned")
            .insert(host, spki);
        Ok(())
    }
}

/// Records the leaf SPKI per hostname and enforces registered pins. The
/// certificate chain is deliberately not consulted: ACI's root of trust is the
/// attested keyset (§1.1), and an attested certificate may be self-signed.
/// Verification is what the §9.1 transcript reports; the handshake only
/// observes and enforces the pin.
struct ObservingSpkiVerifier {
    /// Handshake signature checks and supported schemes only.
    inner: Arc<dyn ServerCertVerifier>,
    observations: Arc<SpkiObservations>,
}

impl fmt::Debug for ObservingSpkiVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObservingSpkiVerifier").finish()
    }
}

impl ServerCertVerifier for ObservingSpkiVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        self.observations.observe(
            server_name_string(server_name),
            leaf_spki_sha256_hex(end_entity)?,
        )?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn leaf_spki_sha256_hex(end_entity: &CertificateDer<'_>) -> Result<String, RustlsError> {
    let (_, cert) = parse_x509_certificate(end_entity.as_ref())
        .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
    Ok(hex::encode(Sha256::digest(cert.public_key().raw)))
}

fn server_name_string(name: &ServerName<'_>) -> String {
    match name {
        ServerName::DnsName(dns) => dns.as_ref().to_ascii_lowercase(),
        ServerName::IpAddress(ip) => std::net::IpAddr::from(*ip).to_string(),
        other => format!("{other:?}"),
    }
}
