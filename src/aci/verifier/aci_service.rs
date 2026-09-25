//! Native verifier for an upstream ACI service: fetches its attestation report,
//! checks the identity/key policy and the evidence (a dstack DCAP/TDX quote today).

use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aci_verifier::{
    appraise_report, raw_evidence, AciReportValidationError, AciServiceVerifierPolicy,
    AppraisalInputs, ChannelEvidence, CheckResult, CustodyEvidence, FailureCause, Outcome,
    QuoteSource, QuoteStepError,
};
use async_trait::async_trait;
use rand::RngCore;
use serde_json::Value;

use super::{DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS, DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::types::AttestationReport;
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

#[derive(Debug, thiserror::Error)]
pub enum AciServiceVerifierConfigError {
    #[error("upstream attestation report base URL is empty")]
    EmptyBaseUrl,
    #[error("invalid upstream attestation report base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("failed to build verifier HTTP client: {0}")]
    Client(String),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AciServiceVerificationError {
    #[error("upstream attestation request failed: {0}")]
    Transport(String),
    #[error("upstream attestation returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("invalid upstream attestation JSON: {0}")]
    InvalidJson(String),
    #[error("ACI report binding failed: {0}")]
    AciBinding(#[from] AciReportValidationError),
    #[error("upstream attestation did not match verifier policy")]
    PolicyRejected,
    #[error(
        "report publishes no source provenance (needs repo_url+repo_commit or image_digest, §4.1)"
    )]
    MissingProvenance,
    #[error("missing DCAP quote evidence")]
    MissingQuote,
    #[error("invalid DCAP quote hex: {0}")]
    InvalidQuoteHex(String),
    #[error("invalid quote_report_data hex: {0}")]
    InvalidQuoteReportDataHex(String),
    #[error("quote_report_data evidence does not match verified quote")]
    QuoteReportDataEvidenceMismatch,
    #[error("DCAP collateral fetch failed: {0}")]
    Collateral(String),
    #[error("DCAP quote verification failed: {0}")]
    QuoteVerification(String),
    #[error("upstream attestation verification timed out")]
    Timeout,
    #[error("attestation tee_type {reported:?} does not match verified quote type {verified:?}")]
    TeeTypeMismatch { reported: String, verified: String },
    #[error("verified quote report_data does not bind the ACI report_data")]
    QuoteReportDataMismatch,
    #[error("invalid dstack event_log evidence: {0}")]
    InvalidEventLog(String),
    #[error("invalid dstack KMS key custody evidence: {0}")]
    InvalidKeyCustody(String),
    #[error("invalid downstream TLS binding: {0}")]
    InvalidDownstreamTlsBinding(String),
}

/// Fold a §9.1 check result onto this deployment's error surface, so the
/// messages and metrics that read these variants keep working.
impl From<&CheckResult> for AciServiceVerificationError {
    fn from(result: &CheckResult) -> Self {
        let cause = match &result.outcome {
            Outcome::Failed(cause) => cause,
            // Unevaluable is a rejection here: this verifier is deciding
            // whether to forward a prompt, and it has no evidence.
            Outcome::Unevaluable(_) | Outcome::Pass => {
                return Self::PolicyRejected;
            }
        };
        match cause {
            FailureCause::Binding(_) | FailureCause::Quote(_) => {
                Self::QuoteVerification(cause.to_string())
            }
            FailureCause::Expired { .. } => {
                Self::AciBinding(AciReportValidationError::KeysetExpired)
            }
            FailureCause::Provenance(_) => Self::MissingProvenance,
            FailureCause::Evidence(e) => Self::InvalidEventLog(e.clone()),
            FailureCause::Custody(e) => Self::InvalidKeyCustody(e.clone()),
            FailureCause::Channel(e) => Self::InvalidDownstreamTlsBinding(e.clone()),
            FailureCause::Policy(_) | FailureCause::NotReached(_) => Self::PolicyRejected,
        }
    }
}

/// Keep the deployment's existing error surface while the §9.1(1) steps live
/// in `aci-verifier`.
impl From<QuoteStepError> for AciServiceVerificationError {
    fn from(e: QuoteStepError) -> Self {
        match e {
            QuoteStepError::MissingQuote => Self::MissingQuote,
            QuoteStepError::InvalidQuoteHex(m) => Self::InvalidQuoteHex(m),
            QuoteStepError::UnparsableQuote(m) => Self::QuoteVerification(m),
            QuoteStepError::InvalidEvidenceReportDataHex(m) => Self::InvalidQuoteReportDataHex(m),
            QuoteStepError::EvidenceReportDataMismatch => Self::QuoteReportDataEvidenceMismatch,
            QuoteStepError::ReportDataSlotMismatch { .. } => Self::QuoteReportDataMismatch,
            QuoteStepError::Collateral { reason, .. } => Self::Collateral(reason),
            QuoteStepError::Verification(m) => Self::QuoteVerification(m),
            QuoteStepError::TeeTypeMismatch { reported, verified } => Self::TeeTypeMismatch {
                reported,
                verified: verified.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct CachedAciServiceVerification {
    pub(super) expires_at: u64,
    pub(super) evidence: Option<Value>,
    pub(super) channel_bindings: Vec<ChannelBinding>,
}

impl CachedAciServiceVerification {
    pub(super) fn event_for(
        &self,
        request: UpstreamVerificationRequest,
        verifier_id: &str,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            model_id: request.model_id,
            url_origin: request.url_origin,
            verifier_id: verifier_id.to_string(),
            result: VerificationResult::Verified,
            required: request.required,
            evidence: self.evidence.clone(),
            channel_bindings: self.channel_bindings.clone(),
            ..Default::default()
        }
    }
}

/// Verifies an upstream ACI service: fetches its canonical report
/// (`/v1/aci/attestation`), checks identity/key binding, and verifies the DCAP quote.
pub struct AciServiceUpstreamVerifier {
    client: reqwest::Client,
    report_base_url: String,
    pccs_url: String,
    policy: AciServiceVerifierPolicy,
    cache_ttl_seconds: u64,
    request_timeout_seconds: u64,
    cache: RwLock<Option<CachedAciServiceVerification>>,
    verifier_id: String,
}

impl AciServiceUpstreamVerifier {
    pub fn new(
        report_base_url: impl Into<String>,
        pccs_url: impl Into<String>,
        policy: AciServiceVerifierPolicy,
        cache_ttl_seconds: u64,
    ) -> Result<Self, AciServiceVerifierConfigError> {
        Self::new_with_timeouts(
            report_base_url,
            pccs_url,
            policy,
            cache_ttl_seconds,
            DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS,
            DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS,
        )
    }

    pub fn new_with_timeouts(
        report_base_url: impl Into<String>,
        pccs_url: impl Into<String>,
        policy: AciServiceVerifierPolicy,
        cache_ttl_seconds: u64,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
    ) -> Result<Self, AciServiceVerifierConfigError> {
        let report_base_url = report_base_url.into();
        let report_base_url = report_base_url.trim().trim_end_matches('/').to_string();
        if report_base_url.is_empty() {
            return Err(AciServiceVerifierConfigError::EmptyBaseUrl);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .timeout(Duration::from_secs(request_timeout_seconds))
            .build()
            .map_err(|e| AciServiceVerifierConfigError::Client(e.to_string()))?;
        Ok(Self {
            client,
            report_base_url,
            pccs_url: pccs_url.into(),
            policy,
            cache_ttl_seconds,
            request_timeout_seconds,
            cache: RwLock::new(None),
            verifier_id: "aci-service/v2".to_string(),
        })
    }

    pub fn with_default_pccs(
        report_base_url: impl Into<String>,
        policy: AciServiceVerifierPolicy,
        cache_ttl_seconds: u64,
    ) -> Result<Self, AciServiceVerifierConfigError> {
        Self::with_default_pccs_and_timeouts(
            report_base_url,
            policy,
            cache_ttl_seconds,
            DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS,
            DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS,
        )
    }

    pub fn with_default_pccs_and_timeouts(
        report_base_url: impl Into<String>,
        policy: AciServiceVerifierPolicy,
        cache_ttl_seconds: u64,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
    ) -> Result<Self, AciServiceVerifierConfigError> {
        Self::new_with_timeouts(
            report_base_url,
            aci_verifier::dcap_qvl::PHALA_PCCS_URL.to_string(),
            policy,
            cache_ttl_seconds,
            connect_timeout_seconds,
            request_timeout_seconds,
        )
    }

    async fn verify_uncached(
        &self,
    ) -> Result<CachedAciServiceVerification, AciServiceVerificationError> {
        let nonce = random_nonce_hex();
        // Canonical report, not the legacy alias: the binding check below expects
        // `report_data = sha256(statement bytes)` (§3.2), which only the canonical
        // report carries (the legacy alias binds `identity ‖ nonce`).
        let report_url = format!("{}/v1/aci/attestation", self.report_base_url);
        let url = format!("{report_url}?nonce={nonce}");
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| AciServiceVerificationError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| AciServiceVerificationError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(AciServiceVerificationError::HttpStatus {
                status,
                body: String::from_utf8_lossy(&body).to_string(),
            });
        }

        let report: AttestationReport = serde_json::from_slice(&body)
            .map_err(|e| AciServiceVerificationError::InvalidJson(e.to_string()))?;
        let verified_at = now_secs();
        // One appraisal, shared with the desktop verifier (`aci-verifier`): this
        // deployment folds the §9.1 outcomes into a single accept/reject.
        let appraisal = appraise_report(AppraisalInputs {
            report: &report,
            nonce: Some(&nonce),
            now_secs: verified_at,
            expiry_waived: false,
            quote: QuoteSource::Online {
                pccs_url: &self.pccs_url,
            },
            accepted_composes: &[],
            custody: CustodyEvidence::DstackKms {
                policy: &self.policy,
            },
            channel: ChannelEvidence::DeclaredFor {
                origin: &self.report_base_url,
            },
            explain: false,
        })
        .await
        .map_err(AciServiceVerificationError::InvalidJson)?;

        if let Some(problem) = appraisal.first_problem() {
            return Err(problem.into());
        }
        let keyset = appraisal
            .identity
            .as_ref()
            .expect("an accepted appraisal establishes the keyset")
            .keyset
            .clone();
        // Recency of a cached verification is bounded by the cache TTL and the
        // keyset's own expiry — the report carries no other freshness metadata.
        let expires_at = verified_at
            .saturating_add(self.cache_ttl_seconds)
            .min(keyset.not_after);
        let evidence = Some(raw_evidence(&body, "application/json", None));
        let channel_bindings = appraisal.channel_bindings;

        Ok(CachedAciServiceVerification {
            expires_at,
            evidence,
            channel_bindings,
        })
    }
}

#[async_trait]
impl UpstreamVerifier for AciServiceUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let now_secs = now_secs();
        if let Some(cached) = self
            .cache
            .read()
            .expect("ACI service verifier cache poisoned")
            .clone()
        {
            if now_secs < cached.expires_at {
                return cached.event_for(request, &self.verifier_id);
            }
        }

        match tokio::time::timeout(
            Duration::from_secs(self.request_timeout_seconds),
            self.verify_uncached(),
        )
        .await
        .map_err(|_| AciServiceVerificationError::Timeout)
        .and_then(|result| result)
        {
            Ok(verified) => {
                *self
                    .cache
                    .write()
                    .expect("ACI service verifier cache poisoned") = Some(verified.clone());
                verified.event_for(request, &self.verifier_id)
            }
            Err(err) => UpstreamVerifiedEvent {
                upstream_name: request.upstream_name,
                model_id: request.model_id,
                url_origin: request.url_origin,
                verifier_id: self.verifier_id.clone(),
                result: VerificationResult::Failed,
                required: request.required,
                reason: Some(err.to_string()),
                ..Default::default()
            },
        }
    }
}

fn random_nonce_hex() -> String {
    // §3.2: a nonce is a 32-byte value as exactly 64 lowercase hex
    // characters; a conformant upstream rejects anything else.
    let mut nonce = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .expect("system time is before UNIX_EPOCH")
}
