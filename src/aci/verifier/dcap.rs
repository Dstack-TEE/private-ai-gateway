//! Native ACI/DCAP upstream verifier: quote validation, policy enforcement,
//! and the per-request verification cache.

use std::collections::BTreeSet;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rand::RngCore;
use serde_json::Value;

use super::dstack::{
    compressed_k256_public_key_hex, verify_dstack_event_log_and_app_id,
    verify_dstack_kms_identity_custody,
};
use super::report::{validate_aci_report_binding, AciReportValidationError, ValidatedAciReport};
use super::{
    decode_hex, DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS, DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS,
};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::types::AttestationReport;
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

#[derive(Debug, thiserror::Error)]
pub enum AciDcapVerifierConfigError {
    #[error(
        "ACI DCAP upstream verifier requires at least one accepted workload id or image digest"
    )]
    EmptyPolicy,
    #[error(
        "ACI DCAP upstream verifier requires at least one accepted dstack KMS root public key"
    )]
    EmptyKmsRootPolicy,
    #[error("invalid dstack KMS root public key: {0}")]
    InvalidKmsRootPublicKey(String),
    #[error("upstream attestation report base URL is empty")]
    EmptyBaseUrl,
    #[error("failed to build verifier HTTP client: {0}")]
    Client(String),
}

#[derive(Debug, Clone)]
pub struct AciDcapVerifierPolicy {
    accepted_workload_ids: BTreeSet<String>,
    accepted_image_digests: BTreeSet<String>,
    pub(super) accepted_kms_root_public_keys: BTreeSet<String>,
}

impl AciDcapVerifierPolicy {
    pub fn new(
        accepted_workload_ids: impl IntoIterator<Item = String>,
        accepted_image_digests: impl IntoIterator<Item = String>,
        accepted_kms_root_public_keys: impl IntoIterator<Item = String>,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        let accepted_workload_ids = accepted_workload_ids
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        let accepted_image_digests = accepted_image_digests
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        let accepted_kms_root_public_keys = accepted_kms_root_public_keys
            .into_iter()
            .map(|key| {
                compressed_k256_public_key_hex(&key)
                    .map_err(AciDcapVerifierConfigError::InvalidKmsRootPublicKey)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if accepted_workload_ids.is_empty() && accepted_image_digests.is_empty() {
            return Err(AciDcapVerifierConfigError::EmptyPolicy);
        }
        if accepted_kms_root_public_keys.is_empty() {
            return Err(AciDcapVerifierConfigError::EmptyKmsRootPolicy);
        }
        Ok(Self {
            accepted_workload_ids,
            accepted_image_digests,
            accepted_kms_root_public_keys,
        })
    }

    fn accepts(&self, report: &AttestationReport) -> bool {
        self.accepted_workload_ids.contains(&report.workload_id)
            || report
                .attestation
                .source_provenance
                .image_digest
                .as_ref()
                .is_some_and(|digest| self.accepted_image_digests.contains(digest))
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AciDcapVerificationError {
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
    #[error("missing dstack event_log evidence")]
    MissingEventLog,
    #[error("invalid dstack event_log evidence: {0}")]
    InvalidEventLog(String),
    #[error("dstack event_log RTMR3 does not match verified quote")]
    EventLogRtmrMismatch,
    #[error("dstack app-id event missing from verified event log")]
    MissingAppId,
    #[error("missing dstack KMS key custody evidence")]
    MissingKeyCustody,
    #[error("unsupported key custody provider: {0}")]
    UnsupportedKeyCustodyProvider(String),
    #[error("invalid dstack KMS key custody evidence: {0}")]
    InvalidKeyCustody(String),
    #[error("missing dstack KMS identity key custody evidence")]
    MissingIdentityKeyCustody,
    #[error("dstack KMS identity key custody public key does not match workload identity")]
    IdentityKeyCustodyMismatch,
    #[error("dstack KMS identity signature chain verification failed: {0}")]
    KmsSignatureChain(String),
    #[error("dstack KMS root public key is not accepted by verifier policy")]
    KmsRootRejected,
    #[error("verified ACI/dstack upstream report did not publish a TLS SPKI binding")]
    MissingTlsSpkiBinding,
}

#[derive(Debug, Clone)]
pub(super) struct CachedAciDcapVerification {
    pub(super) expires_at: u64,
    pub(super) vendor: String,
    pub(super) evidence: Option<Value>,
    pub(super) channel_bindings: Vec<ChannelBinding>,
}

impl CachedAciDcapVerification {
    pub(super) fn event_for(
        &self,
        request: UpstreamVerificationRequest,
        verifier_id: &str,
    ) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: self.vendor.clone(),
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

/// Verifies an upstream ACI/dstack service by fetching its attestation
/// report, checking ACI identity/key binding against the configured
/// accepted identity, and verifying the embedded Intel DCAP quote with
/// `dcap-qvl`.
pub struct AciDcapUpstreamVerifier {
    client: reqwest::Client,
    report_base_url: String,
    pccs_url: String,
    policy: AciDcapVerifierPolicy,
    cache_ttl_seconds: u64,
    request_timeout_seconds: u64,
    cache: RwLock<Option<CachedAciDcapVerification>>,
    verifier_id: String,
}

impl AciDcapUpstreamVerifier {
    pub fn new(
        report_base_url: impl Into<String>,
        pccs_url: impl Into<String>,
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
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
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        let report_base_url = report_base_url.into();
        let report_base_url = report_base_url.trim().trim_end_matches('/').to_string();
        if report_base_url.is_empty() {
            return Err(AciDcapVerifierConfigError::EmptyBaseUrl);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .timeout(Duration::from_secs(request_timeout_seconds))
            .build()
            .map_err(|e| AciDcapVerifierConfigError::Client(e.to_string()))?;
        Ok(Self {
            client,
            report_base_url,
            pccs_url: pccs_url.into(),
            policy,
            cache_ttl_seconds,
            request_timeout_seconds,
            cache: RwLock::new(None),
            verifier_id: "aci-dcap/v1".to_string(),
        })
    }

    pub fn with_default_pccs(
        report_base_url: impl Into<String>,
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
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
        policy: AciDcapVerifierPolicy,
        cache_ttl_seconds: u64,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
    ) -> Result<Self, AciDcapVerifierConfigError> {
        Self::new_with_timeouts(
            report_base_url,
            dcap_qvl::PHALA_PCCS_URL.to_string(),
            policy,
            cache_ttl_seconds,
            connect_timeout_seconds,
            request_timeout_seconds,
        )
    }

    async fn verify_uncached(&self) -> Result<CachedAciDcapVerification, AciDcapVerificationError> {
        let nonce = random_nonce_hex();
        let report_url = format!("{}/v1/attestation/report", self.report_base_url);
        let url = format!("{report_url}?nonce={nonce}");
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| AciDcapVerificationError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| AciDcapVerificationError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(AciDcapVerificationError::HttpStatus {
                status,
                body: String::from_utf8_lossy(&body).to_string(),
            });
        }

        let report: AttestationReport = serde_json::from_slice(&body)
            .map_err(|e| AciDcapVerificationError::InvalidJson(e.to_string()))?;
        let verified_at = now_secs();
        let validated =
            validate_aci_report_binding(&report, Some(&nonce), verified_at, Some(&body))?;
        if !self.policy.accepts(&report) {
            return Err(AciDcapVerificationError::PolicyRejected);
        }
        self.verify_dcap_quote(&report, &validated, verified_at)
            .await?;
        let expires_at = verified_at
            .saturating_add(self.cache_ttl_seconds)
            .min(report.attestation.freshness.stale_after);
        let channel_bindings: Vec<ChannelBinding> = report
            .attestation
            .workload_keyset
            .tls_public_keys
            .iter()
            .map(|key| ChannelBinding::TlsSpkiSha256 {
                origin: self.report_base_url.clone(),
                spki_sha256: key.spki_sha256_hex.clone(),
            })
            .collect();
        if channel_bindings.is_empty() {
            return Err(AciDcapVerificationError::MissingTlsSpkiBinding);
        }

        Ok(CachedAciDcapVerification {
            expires_at,
            vendor: report.attestation.vendor,
            evidence: validated.evidence,
            channel_bindings,
        })
    }

    async fn verify_dcap_quote(
        &self,
        report: &AttestationReport,
        validated: &ValidatedAciReport,
        now_secs: u64,
    ) -> Result<(), AciDcapVerificationError> {
        let quote_hex = report
            .attestation
            .evidence
            .get("quote")
            .and_then(Value::as_str)
            .ok_or(AciDcapVerificationError::MissingQuote)?;
        let raw_quote = decode_hex(quote_hex).map_err(AciDcapVerificationError::InvalidQuoteHex)?;

        let collateral = dcap_qvl::collateral::get_collateral(&self.pccs_url, &raw_quote)
            .await
            .map_err(|e| AciDcapVerificationError::Collateral(e.to_string()))?;
        let verified = dcap_qvl::verify::rustcrypto::verify(&raw_quote, &collateral, now_secs)
            .map_err(|e| AciDcapVerificationError::QuoteVerification(e.to_string()))?;

        let verified_tee_type = if verified.report.is_sgx() {
            "sgx"
        } else {
            "tdx"
        };
        if report.attestation.tee_type != verified_tee_type {
            return Err(AciDcapVerificationError::TeeTypeMismatch {
                reported: report.attestation.tee_type.clone(),
                verified: verified_tee_type.to_string(),
            });
        }

        let quote_report_data = dcap_report_data(&verified.report);
        if let Some(evidence_report_data_hex) = report
            .attestation
            .evidence
            .get("quote_report_data")
            .and_then(Value::as_str)
        {
            let evidence_report_data = decode_hex(evidence_report_data_hex)
                .map_err(AciDcapVerificationError::InvalidQuoteReportDataHex)?;
            if evidence_report_data.as_slice() != quote_report_data {
                return Err(AciDcapVerificationError::QuoteReportDataEvidenceMismatch);
            }
        }

        if quote_report_data != expected_dcap_report_data(validated.report_data).as_slice() {
            return Err(AciDcapVerificationError::QuoteReportDataMismatch);
        }
        let app_id =
            verify_dstack_event_log_and_app_id(&report.attestation.evidence, &verified.report)?;
        verify_dstack_kms_identity_custody(report, &app_id, &self.policy)?;
        Ok(())
    }
}

#[async_trait]
impl UpstreamVerifier for AciDcapUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let now_secs = now_secs();
        if let Some(cached) = self
            .cache
            .read()
            .expect("ACI DCAP verifier cache poisoned")
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
        .map_err(|_| AciDcapVerificationError::Timeout)
        .and_then(|result| result)
        {
            Ok(verified) => {
                *self
                    .cache
                    .write()
                    .expect("ACI DCAP verifier cache poisoned") = Some(verified.clone());
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
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .expect("system time is before UNIX_EPOCH")
}

fn expected_dcap_report_data(report_data: [u8; 32]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&report_data);
    out
}

fn dcap_report_data(report: &dcap_qvl::quote::Report) -> &[u8; 64] {
    match report {
        dcap_qvl::quote::Report::SgxEnclave(report) => &report.report_data,
        dcap_qvl::quote::Report::TD10(report) => &report.report_data,
        dcap_qvl::quote::Report::TD15(report) => &report.base.report_data,
    }
}

pub(super) fn dcap_rtmr3(report: &dcap_qvl::quote::Report) -> Option<&[u8; 48]> {
    match report {
        dcap_qvl::quote::Report::TD10(report) => Some(&report.rt_mr3),
        dcap_qvl::quote::Report::TD15(report) => Some(&report.base.rt_mr3),
        dcap_qvl::quote::Report::SgxEnclave(_) => None,
    }
}
