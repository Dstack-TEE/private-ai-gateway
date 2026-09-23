//! TLS clients that extract, observe, and enforce leaf-certificate SPKI pins.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use sha2::{Digest, Sha256};
use x509_parser::prelude::parse_x509_certificate;

pub fn pinned_spki_client(
    accepted_spkis: Vec<String>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, String> {
    let inner = webpki_verifier()?;
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
        .map_err(|error| error.to_string())
}

struct SpkiPinVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    accepted: HashSet<String>,
}

impl fmt::Debug for SpkiPinVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpkiPinVerifier")
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
            return Err(application_verification_failure());
        }
        let digest = leaf_spki_sha256_hex(end_entity)?;
        if self.accepted.contains(&digest) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(application_verification_failure())
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

/// Build a client that records the first observed leaf SPKI and enforces pins
/// registered for later handshakes.
pub fn observing_spki_client(
    observations: Arc<SpkiObservations>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, String> {
    let inner = webpki_verifier()?;
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
        .map_err(|error| error.to_string())
}

/// Per-hostname record of observed leaf SPKIs and registered pins.
#[derive(Debug, Default)]
pub struct SpkiObservations {
    observed: Mutex<HashMap<String, String>>,
    pins: Mutex<HashMap<String, String>>,
}

impl SpkiObservations {
    pub fn observed_spki(&self, host: &str) -> Option<String> {
        self.observed
            .lock()
            .expect("observed-SPKI map poisoned")
            .get(&host.to_ascii_lowercase())
            .cloned()
    }

    pub fn pin(&self, host: &str, spki_sha256: &str) {
        self.pins
            .lock()
            .expect("SPKI pin map poisoned")
            .insert(host.to_ascii_lowercase(), spki_sha256.to_ascii_lowercase());
    }

    fn observe(&self, host: String, spki: String) -> Result<(), RustlsError> {
        if let Some(expected) = self.pins.lock().expect("SPKI pin map poisoned").get(&host) {
            if *expected != spki {
                return Err(application_verification_failure());
            }
        }
        self.observed
            .lock()
            .expect("observed-SPKI map poisoned")
            .insert(host, spki);
        Ok(())
    }
}

struct ObservingSpkiVerifier {
    /// Used for handshake-signature checks and supported schemes only.
    inner: Arc<dyn ServerCertVerifier>,
    observations: Arc<SpkiObservations>,
}

impl fmt::Debug for ObservingSpkiVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ObservingSpkiVerifier").finish()
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

pub fn leaf_spki_sha256_hex(end_entity: &CertificateDer<'_>) -> Result<String, RustlsError> {
    let (_, cert) = parse_x509_certificate(end_entity.as_ref())
        .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
    Ok(hex::encode(Sha256::digest(cert.public_key().raw)))
}

fn webpki_verifier() -> Result<Arc<dyn ServerCertVerifier>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map(|verifier| verifier as Arc<dyn ServerCertVerifier>)
        .map_err(|error| format!("failed to build TLS verifier: {error}"))
}

fn server_name_string(name: &ServerName<'_>) -> String {
    match name {
        ServerName::DnsName(dns) => dns.as_ref().to_ascii_lowercase(),
        ServerName::IpAddress(ip) => std::net::IpAddr::from(*ip).to_string(),
        other => format!("{other:?}"),
    }
}

fn application_verification_failure() -> RustlsError {
    RustlsError::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
}
