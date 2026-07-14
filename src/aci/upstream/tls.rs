//! TLS leaf pinning and response-header extraction: one `ServerCertVerifier`
//! with two trust modes, plus the reqwest client constructors over it.
//!
//! * [`pinned_spki_client`] / [`pinned_certificate_client_no_proxy`] — gateway
//!   upstream hops: accept only leaves in the attested pin set; the WebPKI
//!   chain is deliberately not consulted (the pin is the root of trust, and
//!   attested certs may be self-signed).
//! * [`observing_webpki_client`] — the `aci` CLI: normal WebPKI validation,
//!   recording the observed leaf SPKI per hostname and failing closed on any
//!   pin registered afterwards.

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
    accepted_certificates: Vec<String>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, UpstreamError> {
    pinned_client(
        accepted_spkis,
        accepted_certificates,
        connect_timeout_seconds,
        read_timeout_seconds,
        false,
    )
}

fn pinned_client(
    accepted_spkis: Vec<String>,
    accepted_certificates: Vec<String>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
    no_proxy: bool,
) -> Result<reqwest::Client, UpstreamError> {
    let tls = tls_config(VerifierMode::AcceptedLeaves {
        spkis: accepted_spkis.into_iter().collect(),
        certificates: accepted_certificates.into_iter().collect(),
    })?;
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_seconds))
        .read_timeout(Duration::from_secs(read_timeout_seconds))
        // A 3xx off the attested endpoint would move the forwarded bytes past
        // what `upstream.verified` attested; surface it, don't chase.
        .redirect(reqwest::redirect::Policy::none())
        .use_preconfigured_tls(tls);
    if no_proxy {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|e| UpstreamError::Transport(e.to_string()))
}

/// A reqwest client that validates chains with WebPKI exactly like a default
/// client while recording the leaf SPKI sha256 per hostname in `observations`
/// and failing closed on any pin registered there.
///
/// Redirects fail closed: recording and pins are per hostname, so a cross-host
/// 3xx would neither be pinned nor recorded for the original host. A redirect
/// surfaces as its 3xx.
pub fn observing_webpki_client(
    observations: Arc<SpkiObservations>,
    connect_timeout_seconds: u64,
    read_timeout_seconds: u64,
) -> Result<reqwest::Client, UpstreamError> {
    let tls = tls_config(VerifierMode::WebPkiObserving(observations))?;
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_seconds))
        .read_timeout(Duration::from_secs(read_timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .use_preconfigured_tls(tls)
        .build()
        .map_err(|e| UpstreamError::Transport(e.to_string()))
}

fn tls_config(mode: VerifierMode) -> Result<rustls::ClientConfig, UpstreamError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let webpki = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| UpstreamError::Transport(format!("failed to build TLS verifier: {e}")))?;
    Ok(rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SpkiVerifier { webpki, mode }))
        .with_no_client_auth())
}

/// Per-hostname record of observed leaf SPKIs and registered pins, shared
/// between an [`observing_webpki_client`] and the code that owns it.
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

    /// Record the SPKI observed for `host` and enforce any pin registered
    /// for it.
    fn observe(&self, host: String, spki: String) -> Result<(), RustlsError> {
        self.observed
            .lock()
            .expect("observed-SPKI map poisoned")
            .insert(host.clone(), spki.clone());
        if let Some(expected) = self.pins.lock().expect("SPKI pin map poisoned").get(&host) {
            if *expected != spki {
                return Err(RustlsError::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
enum VerifierMode {
    /// Accept only leaves whose SPKI or whole-certificate sha256 is in the
    /// attested set; an empty set rejects every handshake.
    AcceptedLeaves {
        spkis: HashSet<String>,
        certificates: HashSet<String>,
    },
    /// Normal WebPKI validation (roots, hostname, expiry), then record the
    /// observed leaf SPKI per hostname and enforce registered pins.
    WebPkiObserving(Arc<SpkiObservations>),
}

struct SpkiVerifier {
    /// Chain validation in [`VerifierMode::WebPkiObserving`]; handshake
    /// signature checks in both modes.
    webpki: Arc<dyn ServerCertVerifier>,
    mode: VerifierMode,
}

impl fmt::Debug for SpkiVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpkiVerifier")
            .field("mode", &self.mode)
            .finish()
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

impl ServerCertVerifier for SpkiVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        match &self.mode {
            VerifierMode::AcceptedLeaves {
                spkis,
                certificates,
            } => {
                if spkis.is_empty() && certificates.is_empty() {
                    return Err(RustlsError::InvalidCertificate(
                        CertificateError::ApplicationVerificationFailure,
                    ));
                }
                let certificate_matches =
                    certificates.contains(&hex::encode(Sha256::digest(end_entity.as_ref())));
                let spki_matches = spkis.contains(&leaf_spki_sha256_hex(end_entity)?);
                if certificate_matches || spki_matches {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(RustlsError::InvalidCertificate(
                        CertificateError::ApplicationVerificationFailure,
                    ))
                }
            }
            VerifierMode::WebPkiObserving(observations) => {
                self.webpki.verify_server_cert(
                    end_entity,
                    intermediates,
                    server_name,
                    ocsp_response,
                    now,
                )?;
                observations.observe(
                    server_name_string(server_name),
                    leaf_spki_sha256_hex(end_entity)?,
                )?;
                Ok(ServerCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_record_per_host_and_enforce_pins_fail_closed() {
        let obs = SpkiObservations::default();
        obs.observe("api.example.com".into(), "aa11".into())
            .unwrap();
        assert_eq!(
            obs.observed_spki("API.Example.com").as_deref(),
            Some("aa11")
        );
        assert_eq!(obs.observed_spki("other.example.com"), None);

        obs.pin("api.example.com", "AA11");
        obs.observe("api.example.com".into(), "aa11".into())
            .unwrap();
        obs.observe("api.example.com".into(), "bb22".into())
            .unwrap_err();
        // A pin binds only its own host.
        obs.observe("other.example.com".into(), "bb22".into())
            .unwrap();
    }
}
