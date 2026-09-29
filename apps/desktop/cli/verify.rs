//! `private-ai-proxy verify`: run the spec 9.1 identity checks against a live service.
//!
//! Fetches the attestation report with a fresh nonce over a TLS channel
//! whose leaf SPKI is recorded, then checks online: DCAP collateral
//! fetched, observed channel bound.

use crate::aci::types::AttestationReport;
use crate::aci::verifier::DEFAULT_DCAP_PCCS_URL;

use crate::args::VerifyArgs;
use crate::checks::{
    run_report_checks, ChannelEvidence, EstablishedIdentity, QuoteSource, ReportCheckContext,
    ReportOutcome, VerifierPolicy,
};
use crate::client::{
    attestation_url, host_of, normalize_base_url, random_nonce_hex, AciClient, ServiceError,
};
use crate::transcript::Transcript;

/// Why a verification could not run: the service could not be reached or is
/// not an ACI service (authored for the user), or another failure whose
/// detail is for the log.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    Service(#[from] ServiceError),
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
    tls_pins: Vec<String>,
}

impl ServiceVerification {
    /// The pin set for every later connection to this host: the attested TLS
    /// keys the report declares its clients pin (§4.2), which id-6 required
    /// the observed key to be among. Empty unless id-6 passed.
    pub fn attested_spkis(&self) -> Vec<String> {
        self.tls_pins.clone()
    }
}

pub async fn verify_service(
    base_url: &str,
    nonce_arg: Option<&str>,
    policy: &VerifierPolicy,
    require_production_os: bool,
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
        .map_err(|error| ServiceError::from_request(&error, &host))?;
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
                pccs_url: DEFAULT_DCAP_PCCS_URL,
            },
            channel,
            policy,
            require_production_os,
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

pub async fn run(args: VerifyArgs, require_production_os: bool) -> Result<i32, String> {
    let verification = verify_service(
        &args.base_url,
        args.nonce.as_deref(),
        &args.policy.verifier_policy()?,
        require_production_os,
        args.explain,
    )
    .await?;
    verification.transcript.print(args.json, args.explain)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn failure(base_url: &str) -> ServiceError {
        match verify_service(base_url, None, &VerifierPolicy::default(), true, false).await {
            Err(VerifyError::Service(error)) => error,
            Err(VerifyError::Failed(detail)) => panic!("unclassified failure: {detail}"),
            Ok(_) => panic!("{base_url} verified"),
        }
    }

    #[tokio::test]
    async fn a_service_that_cannot_be_verified_is_explained_by_its_host() {
        let host = "127.0.0.1".to_string();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let refused = failure(&format!("https://127.0.0.1:{port}")).await;
        assert_eq!(refused, ServiceError::Refused { host: host.clone() });
        assert_eq!(
            refused.to_string(),
            "127.0.0.1 refused the connection. Check the service URL and port."
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = |code: u16| {
            axum::routing::get(
                move || async move { axum::http::StatusCode::from_u16(code).unwrap() },
            )
        };
        let plain = axum::Router::new()
            .route(
                "/v1/aci/attestation",
                axum::routing::get(|| async { "not an attestation" }),
            )
            .route("/busy/v1/aci/attestation", status(503))
            .route("/limited/v1/aci/attestation", status(429))
            .route("/private/v1/aci/attestation", status(403));
        tokio::spawn(async move { axum::serve(listener, plain).await });
        assert_eq!(
            failure(&format!("https://127.0.0.1:{port}")).await,
            ServiceError::Tls { host: host.clone() }
        );
        assert_eq!(
            failure(&format!("http://127.0.0.1:{port}/elsewhere")).await,
            ServiceError::NotAci { host: host.clone() }
        );
        for (path, status) in [("busy", 503), ("limited", 429)] {
            let unavailable = failure(&format!("http://127.0.0.1:{port}/{path}")).await;
            assert_eq!(
                unavailable,
                ServiceError::Unavailable {
                    host: host.clone(),
                    status
                }
            );
            assert_eq!(
                unavailable.to_string(),
                format!("127.0.0.1 is unavailable right now (HTTP {status}). Try again later.")
            );
        }
        assert_eq!(
            failure(&format!("http://127.0.0.1:{port}/private")).await,
            ServiceError::Status {
                host: host.clone(),
                status: 403
            }
        );
        assert_eq!(
            failure(&format!("http://127.0.0.1:{port}")).await,
            ServiceError::NotAci { host }
        );
    }
}
