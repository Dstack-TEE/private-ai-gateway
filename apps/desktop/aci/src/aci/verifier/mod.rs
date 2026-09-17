//! Reusable building blocks for upstream verifiers.
//!
//! ACI §1.2: every upstream that offers TEE attestation is verified
//! before it serves, the aggregator reaches it only over the channel
//! that verification bound, and each receipt records the outcome (§7.5). The
//! [`UpstreamVerifier`] trait is the seam; this module provides two small concrete
//! implementations that are useful right now:
//!
//! * [`StaticUpstreamVerifier`] — returns a fixed
//!   [`crate::aci::receipt::UpstreamVerifiedEvent`]. Useful in tests
//!   and during bring-up when the deployment trusts a single hard-coded
//!   upstream and the verifier_id field is the only thing a relying
//!   party needs.
//! * [`PreverifiedUpstreamVerifier`] — returns a `verified` event whose
//!   fields are populated from the per-request
//!   `UpstreamVerificationRequest`. Suitable as a default for the
//!   "trusted environment, single upstream" case while real per-provider
//!   verifiers (Chutes, Tinfoil, NEAR AI, Phala dstack) are being
//!   written.
//!
//! Neither of these is a substitute for a real provider adapter that
//! fetches the upstream's evidence, applies that provider's verification
//! rules, and returns binding material the forwarding path can enforce.
//! Chutes, Tinfoil, NEAR AI, Phala dstack, and future providers can
//! expose different evidence and transport formats; the aggregator only
//! needs the common [`UpstreamVerifiedEvent`] result.

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::aci::receipt::UpstreamVerifiedEvent;

/// One upstream channel verification request.
#[derive(Debug, Clone)]
pub struct UpstreamVerificationRequest {
    pub upstream_name: String,
    pub url_origin: Option<String>,
    pub model_id: String,
    pub forwarded_body_hash: String,
    pub required: bool,
}

/// Verifies that the selected upstream is acceptable for a request.
#[async_trait]
pub trait UpstreamVerifier: Send + Sync {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent;

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.invalidate(&request);
        self.verify(request).await
    }

    fn invalidate(&self, _request: &UpstreamVerificationRequest) {}
}

/// The channel boundary a provider attests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestationScope {
    PerRouter,
    PerModel,
    PerInstance,
}

impl AttestationScope {
    pub fn is_per_router(self) -> bool {
        matches!(self, Self::PerRouter)
    }

    pub fn from_declared(token: &str) -> Option<Self> {
        match token {
            "router" => Some(Self::PerRouter),
            "model" => Some(Self::PerModel),
            "instance" => Some(Self::PerInstance),
            _ => None,
        }
    }

    pub fn as_declared(self) -> &'static str {
        match self {
            Self::PerRouter => "router",
            Self::PerModel => "model",
            Self::PerInstance => "instance",
        }
    }
}

pub const DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS: u64 = 10;
pub const DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS: u64 = 60;
pub const DEFAULT_DCAP_PCCS_URL: &str = dcap_qvl::PHALA_PCCS_URL;

mod aci_service;
mod appraisal;
mod dstack;
mod external;
mod providers;
mod quote;
mod report;
mod simple;
#[cfg(test)]
mod tests;

pub use aci_service::{
    dcap_report_data, AciServiceUpstreamVerifier, AciServiceVerifierConfigError,
    AciServiceVerifierPolicy,
};
pub use appraisal::{
    appraise_report, Appraisal, AppraisalInputs, ChannelEvidence, CheckId, CheckResult,
    CustodyEvidence, FailureCause, Outcome, QuoteSource,
};
pub use dstack::{dstack_rtmr3_event, verify_dstack_event_log, DstackEventLog};
pub use external::ProviderVerifierConfigError;
pub use providers::{
    ChutesProviderVerifier, NearAiProviderVerifier, PhalaDirectProviderVerifier,
    RoutingUpstreamVerifier, SecretAiProviderVerifier, TinfoilProviderVerifier,
};
pub use quote::QuoteStepError;
pub use report::{
    validate_aci_report_binding, AciReportValidationError, ReportBinding, ValidatedAciReport,
};
pub use simple::{PreverifiedUpstreamVerifier, StaticUpstreamVerifier};

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|e| e.to_string())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = decode_hex(value)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 32 bytes, got {}", bytes.len()))
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
