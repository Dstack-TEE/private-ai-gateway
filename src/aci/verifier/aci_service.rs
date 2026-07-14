//! Native verifier for an upstream ACI service: fetches its attestation report,
//! checks the identity/key policy and the evidence (a dstack DCAP/TDX quote today).

use std::collections::BTreeSet;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rand::RngCore;
use serde_json::Value;

use super::dstack::{
    compressed_k256_public_key_hex, dstack_rtmr3_event, verify_dstack_event_log,
    verify_dstack_kms_receipt_custody,
};
use super::report::{validate_aci_report_binding, AciReportValidationError, ValidatedAciReport};
use super::{
    decode_hex, DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS, DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS,
};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::types::{AttestationReport, SourceProvenance, WorkloadKeyset};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

#[derive(Debug, thiserror::Error)]
pub enum AciServiceVerifierConfigError {
    #[error(
        "ACI service upstream verifier requires at least one accepted subject or image digest"
    )]
    EmptyPolicy,
    #[error(
        "ACI service upstream verifier requires at least one accepted dstack KMS root public key"
    )]
    EmptyKmsRootPolicy,
    #[error("invalid dstack KMS root public key: {0}")]
    InvalidKmsRootPublicKey(String),
    #[error("upstream attestation report base URL is empty")]
    EmptyBaseUrl,
    #[error("invalid upstream attestation report base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("failed to build verifier HTTP client: {0}")]
    Client(String),
}

#[derive(Debug, Clone)]
pub struct AciServiceVerifierPolicy {
    accepted_subjects: BTreeSet<String>,
    accepted_image_digests: BTreeSet<String>,
    pub(super) accepted_kms_root_public_keys: BTreeSet<String>,
}

impl AciServiceVerifierPolicy {
    pub fn new(
        accepted_subjects: impl IntoIterator<Item = String>,
        accepted_image_digests: impl IntoIterator<Item = String>,
        accepted_kms_root_public_keys: impl IntoIterator<Item = String>,
    ) -> Result<Self, AciServiceVerifierConfigError> {
        let accepted_subjects = accepted_subjects
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
                    .map_err(AciServiceVerifierConfigError::InvalidKmsRootPublicKey)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if accepted_subjects.is_empty() && accepted_image_digests.is_empty() {
            return Err(AciServiceVerifierConfigError::EmptyPolicy);
        }
        if accepted_kms_root_public_keys.is_empty() {
            return Err(AciServiceVerifierConfigError::EmptyKmsRootPolicy);
        }
        Ok(Self {
            accepted_subjects,
            accepted_image_digests,
            accepted_kms_root_public_keys,
        })
    }

    /// §4 identity anchors: the attested keyset `subject` or the report's
    /// source-provenance image digest.
    fn accepts(&self, keyset: &WorkloadKeyset, provenance: &SourceProvenance) -> bool {
        keyset
            .subject
            .as_ref()
            .is_some_and(|subject| self.accepted_subjects.contains(subject))
            || provenance
                .image_digest
                .as_ref()
                .is_some_and(|digest| self.accepted_image_digests.contains(digest))
    }
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
    #[error("dstack app-id event missing from verified event log")]
    MissingAppId,
    #[error("missing dstack KMS key custody evidence")]
    MissingKeyCustody,
    #[error("unsupported key custody provider: {0}")]
    UnsupportedKeyCustodyProvider(String),
    #[error("invalid dstack KMS key custody evidence: {0}")]
    InvalidKeyCustody(String),
    #[error("missing dstack KMS receipt key custody evidence")]
    MissingReceiptKeyCustody,
    #[error("dstack KMS receipt key custody public key does not match the attested keyset")]
    ReceiptKeyCustodyMismatch,
    #[error("dstack KMS signature chain verification failed: {0}")]
    KmsSignatureChain(String),
    #[error("dstack KMS root public key is not accepted by verifier policy")]
    KmsRootRejected,
    #[error("verified ACI/dstack upstream report did not publish a TLS SPKI binding")]
    MissingTlsSpkiBinding,
    #[error("verified ACI/dstack upstream report did not select a downstream TLS binding")]
    MissingDownstreamTlsBinding,
    #[error("invalid downstream TLS binding: {0}")]
    InvalidDownstreamTlsBinding(String),
    #[error(
        "selected downstream TLS binding domain {reported:?} does not match upstream host {expected:?}"
    )]
    DownstreamTlsBindingHostMismatch { reported: String, expected: String },
    #[error("selected downstream TLS binding is not present in the attested keyset")]
    DownstreamTlsBindingNotInKeyset,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedDownstreamTlsBinding {
    domain: String,
    spki_sha256: String,
}

pub(super) fn aci_report_tls_channel_bindings(
    keyset: &WorkloadKeyset,
    evidence: &Value,
    origin: &str,
) -> Result<Vec<ChannelBinding>, AciServiceVerificationError> {
    let tls_public_keys = &keyset.tls_public_keys;
    if tls_public_keys.is_empty() {
        return Err(AciServiceVerificationError::MissingTlsSpkiBinding);
    }

    let has_domain_scoped_keys = tls_public_keys.iter().any(|key| key.domain.is_some());
    if !has_domain_scoped_keys {
        return tls_public_keys
            .iter()
            .map(|key| {
                Ok(ChannelBinding::TlsSpkiSha256 {
                    origin: origin.to_string(),
                    spki_sha256: normalize_sha256_hex(&key.spki_sha256_hex).map_err(|e| {
                        AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
                            "invalid keyset TLS SPKI digest: {e}"
                        ))
                    })?,
                })
            })
            .collect();
    }

    let selected = selected_downstream_tls_binding(evidence)?;
    let origin_domain = origin_host_domain(origin)?;
    if selected.domain != origin_domain {
        return Err(
            AciServiceVerificationError::DownstreamTlsBindingHostMismatch {
                reported: selected.domain,
                expected: origin_domain,
            },
        );
    }

    for key in tls_public_keys {
        let Some(domain) = key.domain.as_deref() else {
            continue;
        };
        let key_domain = normalize_tls_domain(domain).map_err(|e| {
            AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
                "invalid keyset TLS domain {domain:?}: {e}"
            ))
        })?;
        let key_spki = normalize_sha256_hex(&key.spki_sha256_hex).map_err(|e| {
            AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
                "invalid keyset TLS SPKI digest: {e}"
            ))
        })?;
        if key_domain == selected.domain && key_spki == selected.spki_sha256 {
            return Ok(vec![ChannelBinding::TlsSpkiSha256 {
                origin: origin.to_string(),
                spki_sha256: selected.spki_sha256,
            }]);
        }
    }

    Err(AciServiceVerificationError::DownstreamTlsBindingNotInKeyset)
}

fn selected_downstream_tls_binding(
    evidence: &Value,
) -> Result<SelectedDownstreamTlsBinding, AciServiceVerificationError> {
    let binding = evidence
        .get("downstream_tls_binding")
        .ok_or(AciServiceVerificationError::MissingDownstreamTlsBinding)?;
    let domain = binding
        .get("domain")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidDownstreamTlsBinding(
                "downstream_tls_binding.domain must be a string".to_string(),
            )
        })?;
    let spki_sha256 = binding
        .get("spki_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AciServiceVerificationError::InvalidDownstreamTlsBinding(
                "downstream_tls_binding.spki_sha256 must be a string".to_string(),
            )
        })?;
    Ok(SelectedDownstreamTlsBinding {
        domain: normalize_tls_domain(domain).map_err(|e| {
            AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
                "invalid downstream_tls_binding.domain: {e}"
            ))
        })?,
        spki_sha256: normalize_sha256_hex(spki_sha256).map_err(|e| {
            AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
                "invalid downstream_tls_binding.spki_sha256: {e}"
            ))
        })?,
    })
}

fn origin_host_domain(origin: &str) -> Result<String, AciServiceVerificationError> {
    let url = reqwest::Url::parse(origin).map_err(|e| {
        AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
            "invalid report base URL {origin:?}: {e}"
        ))
    })?;
    let host = url.host_str().ok_or_else(|| {
        AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
            "report base URL {origin:?} has no host"
        ))
    })?;
    normalize_tls_domain(host).map_err(|e| {
        AciServiceVerificationError::InvalidDownstreamTlsBinding(format!(
            "invalid report base URL host {host:?}: {e}"
        ))
    })
}

fn normalize_tls_domain(raw: &str) -> Result<String, String> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.contains('/')
        || domain.contains(':')
        || domain.contains('=')
        || domain.contains(',')
        || domain.chars().any(char::is_whitespace)
    {
        return Err(format!("invalid TLS domain {raw:?}"));
    }
    Ok(domain)
}

fn normalize_sha256_hex(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.len() != 64 || !value.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err("expected 64 hex characters".to_string());
    }
    Ok(value.to_ascii_lowercase())
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
            verifier_id: "aci-service/v1".to_string(),
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
            dcap_qvl::PHALA_PCCS_URL.to_string(),
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
        // `report_data = sha256(statement bytes)` (§4.2), which only the canonical
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
        let validated =
            validate_aci_report_binding(&report, Some(&nonce), verified_at, Some(&body))?;
        if !self
            .policy
            .accepts(&validated.keyset, &report.attestation.source_provenance)
        {
            return Err(AciServiceVerificationError::PolicyRejected);
        }
        self.verify_dcap_quote(&report, &validated, verified_at)
            .await?;
        // Recency of a cached verification is bounded by the cache TTL and the
        // keyset's own expiry — the report carries no other freshness metadata.
        let expires_at = verified_at
            .saturating_add(self.cache_ttl_seconds)
            .min(validated.keyset.not_after);
        let channel_bindings = aci_report_tls_channel_bindings(
            &validated.keyset,
            &report.attestation.evidence,
            &self.report_base_url,
        )?;

        Ok(CachedAciServiceVerification {
            expires_at,
            evidence: validated.evidence,
            channel_bindings,
        })
    }

    async fn verify_dcap_quote(
        &self,
        report: &AttestationReport,
        validated: &ValidatedAciReport,
        now_secs: u64,
    ) -> Result<(), AciServiceVerificationError> {
        let quote_hex = report
            .attestation
            .evidence
            .get("quote")
            .and_then(Value::as_str)
            .ok_or(AciServiceVerificationError::MissingQuote)?;
        let raw_quote =
            decode_hex(quote_hex).map_err(AciServiceVerificationError::InvalidQuoteHex)?;

        let collateral = dcap_qvl::collateral::get_collateral(&self.pccs_url, &raw_quote)
            .await
            .map_err(|e| AciServiceVerificationError::Collateral(e.to_string()))?;
        let verified = dcap_qvl::verify::rustcrypto::verify(&raw_quote, &collateral, now_secs)
            .map_err(|e| AciServiceVerificationError::QuoteVerification(e.to_string()))?;

        let verified_tee_type = if verified.report.is_sgx() {
            "sgx"
        } else {
            "tdx"
        };
        if report.attestation.tee_type != verified_tee_type {
            return Err(AciServiceVerificationError::TeeTypeMismatch {
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
                .map_err(AciServiceVerificationError::InvalidQuoteReportDataHex)?;
            if evidence_report_data.as_slice() != quote_report_data {
                return Err(AciServiceVerificationError::QuoteReportDataEvidenceMismatch);
            }
        }

        if quote_report_data
            != crate::aci::identity::report_data_slot(validated.report_data).as_slice()
        {
            return Err(AciServiceVerificationError::QuoteReportDataMismatch);
        }
        let events = verify_dstack_event_log(&report.attestation.evidence, &verified.report)
            .map_err(AciServiceVerificationError::InvalidEventLog)?;
        let app_id_event = dstack_rtmr3_event(&events, "app-id")
            .ok_or(AciServiceVerificationError::MissingAppId)?;
        let app_id = decode_hex(&app_id_event.event_payload)
            .map_err(AciServiceVerificationError::InvalidEventLog)?;
        verify_dstack_kms_receipt_custody(
            &report.attestation.evidence,
            &validated.keyset,
            &app_id,
            &self.policy,
        )?;
        Ok(())
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

/// The `report_data` field of a parsed DCAP quote, across report variants.
pub fn dcap_report_data(report: &dcap_qvl::quote::Report) -> &[u8; 64] {
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
