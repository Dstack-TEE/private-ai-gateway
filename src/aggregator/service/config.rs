use super::ServiceError;
use crate::aci::types::{ServiceCapabilities, SourceProvenance, TlsSpki};

/// Default keyset lifetime: launch time + 30 days (§4.4 bounded lifetime).
/// The launcher computes `keyset_not_after` from this unless configured.
pub const DEFAULT_KEYSET_NOT_AFTER_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Validate source provenance before it is serialized in an attestation report.
/// The binary derives runtime provenance from git-launcher; unknown provenance
/// is valid and omitted from the wire report.
pub fn validate_source_provenance(sp: &SourceProvenance) -> Result<(), ServiceError> {
    if sp.is_unknown() {
        return Ok(());
    }
    let has_repo = sp.repo_url.as_deref().is_some_and(|s| !s.is_empty())
        && sp.repo_commit.as_deref().is_some_and(|s| !s.is_empty());
    let has_image = sp.image_digest.as_deref().is_some_and(|s| !s.is_empty());
    if has_repo || has_image {
        Ok(())
    } else {
        Err(ServiceError::InvalidSourceProvenance)
    }
}

pub(super) fn normalize_downstream_domain(raw: &str) -> Option<String> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.contains('/')
        || domain.contains(':')
        || domain.contains('=')
        || domain.contains(',')
        || domain.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(domain)
}

/// Configuration accepted by [`super::AciService::new`].
pub struct AciServiceConfig {
    pub tee_type: String,
    /// Runtime source provenance. The binary populates this from
    /// `/etc/git-launcher/gateway.conf`; missing launcher metadata is
    /// represented by `SourceProvenance::default()`.
    pub source_provenance: SourceProvenance,
    /// Absolute keyset expiry (§4.1 `not_after`): launch time plus a
    /// configurable duration ([`DEFAULT_KEYSET_NOT_AFTER_SECONDS`] default).
    pub keyset_not_after: u64,
    /// Optional profile-interpreted keyset `subject` (§4.1).
    pub subject: Option<String>,
    pub service_capabilities: ServiceCapabilities,
    /// How long receipts stay queryable in the in-memory store. Also the
    /// attested-session validity period and per-citation retention extension.
    pub receipt_ttl_seconds: u64,
    pub upstream_required_default: bool,
    pub allow_test_keys: bool,
    /// Overrides the TLS-SPKI digests reported by the key provider.
    /// Production deployments should derive this from the mounted
    /// client-facing TLS certificate path when plaintext HTTPS
    /// terminates for this workload.
    pub tls_public_keys: Option<Vec<TlsSpki>>,
}

impl AciServiceConfig {
    pub fn for_test() -> Self {
        Self {
            tee_type: "tdx".to_string(),
            source_provenance: SourceProvenance {
                repo_url: Some("https://github.com/Dstack-TEE/private-ai-gateway".to_string()),
                repo_commit: Some("deadbeef".to_string()),
                image_digest: None,
                image_provenance: None,
            },
            keyset_not_after: 2_000_000_000,
            subject: None,
            service_capabilities: ServiceCapabilities::default(),
            receipt_ttl_seconds: 3600,
            upstream_required_default: true,
            allow_test_keys: true,
            tls_public_keys: None,
        }
    }
}

/// Identifier the service records alongside a receipt so a relying party
/// can prove it was the original requester (§8.6).
///
/// The aggregator never stores raw bearer tokens; it stores the SHA-256
/// digest of whatever credential the requester presented at chat time.
/// Lookups must present the same credential, whose digest must match.
/// Receipts with `None` owner are anonymous and publicly retrievable;
/// in production a deployment should require auth on `POST
/// /v1/chat/completions`, after which every receipt is owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptOwner {
    pub auth_token_sha256: String,
}

impl ReceiptOwner {
    /// Build the receipt-owner record from a raw `Authorization: Bearer ...`
    /// token. The raw bytes are hashed immediately and never kept.
    pub fn from_bearer(token: &str) -> Self {
        Self {
            auth_token_sha256: crate::aci::digest::sha256_hex(token.as_bytes()),
        }
    }
}
