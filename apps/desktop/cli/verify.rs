//! `private-ai-proxy verify`: run the spec 9.1 identity checks against a live service.
//!
//! Fetches the attestation report with a fresh nonce over a TLS channel
//! whose leaf SPKI is recorded, then checks online: DCAP collateral
//! fetched, observed channel bound.

use std::time::Duration;

use crate::aci::types::AttestationReport;
use crate::aci::verifier::DEFAULT_PCCS_URL;
use crate::args::VerifyArgs;
use crate::checks::{
    run_report_checks, ChannelEvidence, EstablishedIdentity, QuoteSource, ReportCheckContext,
    ReportOutcome, VerifierPolicy,
};
use crate::client::{
    attestation_url, host_of, normalize_base_url, random_nonce_hex, AciClient, ServiceError,
};
use crate::transcript::Transcript;

pub(crate) const VERIFICATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a verification could not run or did not pass: the service could not
/// be reached or is not an ACI service (authored for the user), its verdict
/// was not VERIFIED (the transcript says why), or another failure whose
/// detail is for the log.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error("service verification failed; refusing to start the proxy (fail closed)")]
    Rejected(Transcript),
    #[error("{0}")]
    Failed(String),
}

impl From<String> for VerifyError {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

impl From<VerifyError> for String {
    fn from(error: VerifyError) -> Self {
        error.to_string()
    }
}

/// Everything `private-ai-proxy send` and `private-ai-proxy serve` need after a full online
/// verify: the transcript, the report, and the client that observed the
/// channel (ready to have the attested SPKI pinned).
pub struct ServiceVerification {
    pub transcript: Transcript,
    pub report: AttestationReport,
    /// The identity id-2 established, when it did.
    pub identity: Option<EstablishedIdentity>,
    pub client: AciClient,
    pub base_url: String,
    pub host: String,
    pub observed_spki: Option<String>,
    /// The pin set for every later connection to this host: the attested TLS
    /// keys the report declares its clients pin (§4.2), which id-6 required
    /// the observed key to be among. Empty unless id-6 passed.
    pub tls_pins: Vec<String>,
}

pub async fn verify_service(
    base_url: &str,
    nonce_arg: Option<&str>,
    policy: &VerifierPolicy,
    explain: bool,
) -> Result<ServiceVerification, VerifyError> {
    verify_service_with_timeout(base_url, nonce_arg, policy, explain, VERIFICATION_TIMEOUT).await
}

pub(crate) async fn verify_service_with_timeout(
    base_url: &str,
    nonce_arg: Option<&str>,
    policy: &VerifierPolicy,
    explain: bool,
    timeout: Duration,
) -> Result<ServiceVerification, VerifyError> {
    tokio::time::timeout(
        timeout,
        verify_service_inner(base_url, nonce_arg, policy, explain),
    )
    .await
    .map_err(|_| ServiceError::TimedOut {
        host: host_of(&normalize_base_url(base_url)).unwrap_or_else(|_| "service".to_string()),
    })?
}

async fn verify_service_inner(
    base_url: &str,
    nonce_arg: Option<&str>,
    policy: &VerifierPolicy,
    explain: bool,
) -> Result<ServiceVerification, VerifyError> {
    let base_url = normalize_base_url(base_url);
    if base_url.is_empty() {
        return Err(VerifyError::Failed("base URL is empty".to_string()));
    }
    let host = host_of(&base_url)?;
    let client = AciClient::new()?;
    let nonce = match nonce_arg {
        Some(nonce) => nonce.to_string(),
        None => random_nonce_hex(),
    };
    let url = attestation_url(&base_url, &nonce)?;
    let resp = client
        .fetch_attestation(&url)
        .await
        .map_err(|error| ServiceError::from_get(&error, &host))?;
    if !(200..300).contains(&resp.status) {
        return Err(ServiceError::from_status(&host, resp.status).into());
    }
    let report: AttestationReport = serde_json::from_slice(&resp.body)
        .map_err(|_| ServiceError::NotAci { host: host.clone() })?;
    let observed_spki = client.observed_spki(&host);

    let mut transcript = Transcript::default();
    let channel = match &observed_spki {
        Some(spki) => ChannelEvidence::Observed {
            origin: &base_url,
            spki_sha256: spki,
        },
        None => ChannelEvidence::NotObserved {
            reason: "no TLS handshake observed (plain-HTTP base URL)",
        },
    };
    let ReportOutcome { identity, tls_pins } = run_report_checks(
        &mut transcript,
        &report,
        ReportCheckContext {
            nonce: Some(&nonce),
            now_secs: desktop_core::now_secs(),
            expiry_skipped: false,
            quote: QuoteSource::Online {
                pccs_url: DEFAULT_PCCS_URL,
            },
            channel,
            policy,
            explain,
        },
    )
    .await?;

    Ok(ServiceVerification {
        transcript,
        report,
        identity,
        client,
        base_url,
        host,
        observed_spki,
        tls_pins,
    })
}

pub async fn run(args: VerifyArgs) -> Result<i32, String> {
    let verification = verify_service(
        &args.base_url,
        args.nonce.as_deref(),
        &args.policy.verifier_policy()?,
        args.explain,
    )
    .await?;
    verification.transcript.print(args.json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, response::Response, routing::get, Router};
    use std::{
        convert::Infallible,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    #[tokio::test]
    async fn a_trickling_attestation_hits_the_injected_total_deadline() {
        let chunks = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/v1/aci/attestation",
            get({
                let chunks = chunks.clone();
                move || {
                    let chunks = chunks.clone();
                    async move {
                        let body = async_stream::stream! {
                            loop {
                                chunks.fetch_add(1, Ordering::SeqCst);
                                yield Ok::<_, Infallible>(axum::body::Bytes::from_static(b" "));
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        };
                        Response::new(Body::from_stream(body))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            verify_service_with_timeout(
                &base_url,
                Some("test-nonce"),
                &VerifierPolicy::default(),
                false,
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("verification did not enforce its total deadline");
        assert!(matches!(
            result,
            Err(VerifyError::Service(ServiceError::TimedOut { .. }))
        ));
        assert!(
            chunks.load(Ordering::SeqCst) > 1,
            "attestation did not trickle"
        );
        server.abort();
    }
}
