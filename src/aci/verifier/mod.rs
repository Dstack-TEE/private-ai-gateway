//! Reusable building blocks for upstream verifiers.
//!
//! ACI §1.2 requires an aggregator to "verify upstreams inside attested
//! code before forwarding sensitive traffic" and record the result in
//! the receipt. The trait [`crate::aggregator::service::UpstreamVerifier`]
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

pub const DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS: u64 = 10;
pub const DEFAULT_VERIFIER_REQUEST_TIMEOUT_SECONDS: u64 = 60;

mod aci_service;
mod dstack;
mod external;
mod providers;
mod report;
mod simple;
#[cfg(test)]
mod tests;

pub use aci_service::{
    AciServiceUpstreamVerifier, AciServiceVerifierConfigError, AciServiceVerifierPolicy,
};
pub use external::ProviderVerifierConfigError;
pub use providers::{
    ChutesProviderVerifier, NearAiProviderVerifier, PhalaDirectProviderVerifier,
    RoutingUpstreamVerifier, SecretAiProviderVerifier, TinfoilProviderVerifier,
};
pub use report::{validate_aci_report_binding, AciReportValidationError, ValidatedAciReport};
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
