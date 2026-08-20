//! `aci verify`: run the spec 9.1 identity checks against a live service.
//!
//! Fetches the attestation report with a fresh nonce over a TLS channel
//! whose leaf SPKI is recorded, then checks online: DCAP collateral
//! fetched, observed channel bound.

use private_ai_gateway::aci::types::AttestationReport;

use crate::args::VerifyArgs;
use crate::checks::{
    now_secs, run_report_checks, ChannelEvidence, EstablishedIdentity, QuoteSource,
    ReportCheckContext,
};
use crate::client::{host_of, normalize_base_url, random_nonce_hex, AciClient};
use crate::transcript::Transcript;

/// Everything `aci send`, `aci curl`, and `aci serve` need after a full online
/// verify: the transcript, the report, and the client that observed the channel
/// (ready to have the attested SPKI pinned).
pub struct ServiceVerification {
    pub transcript: Transcript,
    pub report: AttestationReport,
    /// The identity id-2 established, when it did.
    pub identity: Option<EstablishedIdentity>,
    pub client: AciClient,
    pub base_url: String,
    pub host: String,
    pub observed_spki: Option<String>,
}

pub async fn verify_service(
    base_url: &str,
    nonce_arg: Option<&str>,
    accepted_composes: &[String],
    require_production_os: bool,
    explain: bool,
) -> Result<ServiceVerification, String> {
    let base_url = normalize_base_url(base_url);
    if base_url.is_empty() {
        return Err("base URL is empty".to_string());
    }
    let client = AciClient::new()?;
    let nonce = match nonce_arg {
        Some(nonce) => nonce.to_string(),
        None => random_nonce_hex(),
    };
    let resp = client.fetch_attestation(&base_url, &nonce).await?;
    resp.error_for_status("attestation report")?;
    let report: AttestationReport = serde_json::from_slice(&resp.body)
        .map_err(|e| format!("attestation report is not valid ACI JSON: {e}"))?;
    let host = host_of(&base_url)?;
    let observed_spki = client.observed_spki(&host);

    let mut transcript = Transcript::default();
    let channel = match &observed_spki {
        Some(spki) => ChannelEvidence::Observed {
            host: &host,
            spki_sha256: spki,
        },
        None => ChannelEvidence::NotObserved {
            reason: "no TLS handshake observed (plain-HTTP base URL)",
        },
    };
    let identity = run_report_checks(
        &mut transcript,
        &report,
        ReportCheckContext {
            nonce: Some(&nonce),
            now_secs: now_secs(),
            expiry_skipped: false,
            quote: QuoteSource::Online {
                pccs_url: dcap_qvl::PHALA_PCCS_URL,
            },
            channel,
            accepted_composes,
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
    })
}

pub async fn run(args: VerifyArgs, require_production_os: bool) -> Result<i32, String> {
    let verification = verify_service(
        &args.base_url,
        args.nonce.as_deref(),
        &args.accepted_composes,
        require_production_os,
        args.explain,
    )
    .await?;
    verification.transcript.print(args.json, args.explain)
}
