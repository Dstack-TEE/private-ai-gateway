//! TLS client that observes the first leaf SPKI and enforces the verified pin.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use sha2::{Digest, Sha256};
use x509_parser::prelude::parse_x509_certificate;

/// Client for Private AI Proxy's ACI commands: it has no pin before the first handshake — that
/// is how it learns the SPKI to check at §9.1(6) — so it records what it sees
/// and enforces any pin registered afterwards.
pub fn observing_spki_client(
    observations: Arc<SpkiObservations>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, String> {
    crate::install_crypto_provider();
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let inner = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| format!("failed to build TLS verifier: {e}"))?;
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
        .map_err(|e| e.to_string())
}

/// Per-hostname record of observed leaf SPKIs and registered pins, shared
/// between an [`observing_spki_client`] and the code that owns it.
#[derive(Debug, Default)]
pub struct SpkiObservations {
    observed: Mutex<HashMap<String, String>>,
    pins: Mutex<HashMap<String, Vec<String>>>,
    /// Handshakes each host's pin refused, so a caller can tell a stale pin
    /// from any other connect failure (DNS, refused port, timeout).
    rejections: Mutex<HashMap<String, u64>>,
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

    /// Enforce the pin set `spkis` (sha256 hex) on every future TLS handshake
    /// to `host`: a handshake presenting any other key fails closed. Like an
    /// HPKP pin set (RFC 7469 §2.6), any one member satisfies the pin.
    pub fn pin(&self, host: &str, spkis: &[String]) {
        let mut spkis: Vec<String> = spkis.iter().map(|s| s.to_ascii_lowercase()).collect();
        spkis.sort();
        spkis.dedup();
        self.pins
            .lock()
            .expect("SPKI pin map poisoned")
            .insert(host.to_ascii_lowercase(), spkis);
    }

    /// The pin set currently enforced for `host`, sorted; empty when unpinned.
    pub fn pinned_spkis(&self, host: &str) -> Vec<String> {
        self.pins
            .lock()
            .expect("SPKI pin map poisoned")
            .get(&host.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// How many handshakes to `host` the pin has refused so far. A caller
    /// compares the count around a send to learn whether its own connect
    /// failure was a pin refusal.
    pub fn pin_rejections(&self, host: &str) -> u64 {
        self.rejections
            .lock()
            .expect("pin rejection map poisoned")
            .get(&host.to_ascii_lowercase())
            .copied()
            .unwrap_or(0)
    }

    /// Enforce any pin registered for `host`, then record the SPKI observed.
    /// A rejected handshake records nothing: the observation map feeds
    /// transcripts, which must never report a key that was refused.
    fn observe(&self, host: String, spki: String) -> Result<(), RustlsError> {
        if let Some(expected) = self.pins.lock().expect("SPKI pin map poisoned").get(&host) {
            if !expected.contains(&spki) {
                *self
                    .rejections
                    .lock()
                    .expect("pin rejection map poisoned")
                    .entry(host)
                    .or_default() += 1;
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

pub(crate) fn leaf_spki_sha256_hex(end_entity: &CertificateDer<'_>) -> Result<String, RustlsError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_set_accepts_any_member_and_counts_refusals() {
        let observations = SpkiObservations::default();
        let (a, b) = ("aa".repeat(32), "bb".repeat(32));
        observations.pin("Host.Example", &[b.to_ascii_uppercase(), a.clone()]);
        assert_eq!(
            observations.pinned_spkis("host.example"),
            vec![a.clone(), b.clone()]
        );

        assert!(observations
            .observe("host.example".into(), b.clone())
            .is_ok());
        assert_eq!(observations.pin_rejections("host.example"), 0);

        let refused = observations.observe("host.example".into(), "cc".repeat(32));
        assert!(refused.is_err());
        assert_eq!(observations.pin_rejections("host.example"), 1);
        // A refused key is never reported as observed.
        assert_eq!(observations.observed_spki("host.example"), Some(b));
    }

    /// Pins are registered under `host_of(url)` and enforced under the
    /// handshake's server name; the two must agree or a pin never applies.
    #[test]
    fn url_hosts_match_tls_server_names() {
        for (url, name) in [
            ("https://[::1]:8443/v1", "::1"),
            ("https://127.0.0.1:8443", "127.0.0.1"),
            ("https://API.Example.com", "api.example.com"),
        ] {
            let server_name = ServerName::try_from(name).unwrap();
            assert_eq!(
                crate::client::host_of(url).unwrap(),
                server_name_string(&server_name),
                "{url}"
            );
        }
    }
}
