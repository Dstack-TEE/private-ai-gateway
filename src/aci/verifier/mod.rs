//! Reusable building blocks for upstream verifiers.
//!
//! ACI §1.2: every upstream that offers TEE attestation is verified
//! before it serves, the aggregator reaches it only over the channel
//! that verification bound, and each receipt records the outcome (§7.5). The trait [`crate::aggregator::service::UpstreamVerifier`]
//! is the seam; this module provides two small concrete
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

use crate::aci::receipt::UpstreamVerifiedEvent;
use crate::aggregator::service::UpstreamVerificationRequest;

pub const DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS: u64 = 10;
pub const DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS: u64 = 60;

mod aci_service;
mod appraisal;
mod dstack;
mod external;
mod privatemode;
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
pub use dstack::{dstack_rtmr3_event, verify_dstack_event_log, DstackEventLog, VerifiedEventLog};
pub use external::ProviderVerifierConfigError;
pub use privatemode::PrivatemodeProviderVerifier;
pub use providers::{
    ChutesProviderVerifier, NearAiProviderVerifier, PhalaDirectProviderVerifier,
    RoutingUpstreamVerifier, SecretAiProviderVerifier, TinfoilProviderVerifier,
};
pub use quote::QuoteStepError;
pub use report::{
    validate_aci_report_binding, AciReportValidationError, ReportBinding, ValidatedAciReport,
};
pub use simple::{PreverifiedUpstreamVerifier, StaticUpstreamVerifier};

/// A verified event cached by a provider verifier, re-labelled per request.
#[derive(Clone, Debug)]
struct CachedProviderEvent {
    expires_at: tokio::time::Instant,
    event: UpstreamVerifiedEvent,
}

impl CachedProviderEvent {
    fn event_for(&self, request: &UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let mut event = self.event.clone();
        event.upstream_name = request.upstream_name.clone();
        event.model_id = request.model_id.clone();
        event.url_origin = request.url_origin.clone();
        event.required = request.required;
        event
    }
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
