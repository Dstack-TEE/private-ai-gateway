//! TLS SPKI-pinning reqwest client and response-header extraction.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
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
            headers
                .entry(k.to_string())
                .and_modify(|existing: &mut String| {
                    existing.push(',');
                    existing.push_str(value);
                })
                .or_insert_with(|| value.to_string());
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
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let inner = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| UpstreamError::Transport(format!("failed to build TLS verifier: {e}")))?;
    let verifier = Arc::new(SpkiPinVerifier {
        inner,
        accepted: accepted_spkis.into_iter().collect(),
        accepted_certificates: accepted_certificates.into_iter().collect(),
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
    accepted_certificates: HashSet<String>,
}

impl fmt::Debug for SpkiPinVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpkiPinVerifier")
            .field("accepted_count", &self.accepted.len())
            .field(
                "accepted_certificate_count",
                &self.accepted_certificates.len(),
            )
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
        if self.accepted.is_empty() && self.accepted_certificates.is_empty() {
            return Err(RustlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }

        let certificate_digest = hex::encode(Sha256::digest(end_entity.as_ref()));
        let certificate_matches = self.accepted_certificates.contains(&certificate_digest);

        let (_, cert) = parse_x509_certificate(end_entity.as_ref())
            .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        let digest = Sha256::digest(cert.public_key().raw);
        let digest = hex::encode(digest);
        let spki_matches = self.accepted.contains(&digest);

        if certificate_matches || spki_matches {
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
