use super::ServiceError;
use crate::aci::types::{KeysetEpoch, ServiceCapabilities, SourceProvenance, TlsSpki};

/// Validate ACI §5.2 source-provenance arms.
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

/// Configuration accepted by [`AciService::new`].
pub struct AciServiceConfig {
    pub vendor: String,
    pub tee_type: String,
    pub source_provenance: SourceProvenance,
    pub keyset_epoch: KeysetEpoch,
    pub identity_subject: Option<String>,
    pub service_capabilities: ServiceCapabilities,
    pub freshness_seconds: u64,
    /// How long receipts stay queryable in the in-memory store.
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
    pub fn for_test(vendor: &str) -> Self {
        Self {
            vendor: vendor.to_string(),
            tee_type: "tdx".to_string(),
            source_provenance: SourceProvenance {
                repo_url: Some("https://github.com/Dstack-TEE/private-ai-gateway".to_string()),
                repo_commit: Some("deadbeef".to_string()),
                image_digest: None,
                image_provenance: None,
            },
            keyset_epoch: KeysetEpoch {
                version: 1,
                not_after: 2_000_000_000,
            },
            identity_subject: None,
            service_capabilities: ServiceCapabilities::default(),
            freshness_seconds: 3600,
            receipt_ttl_seconds: 3600,
            upstream_required_default: true,
            allow_test_keys: true,
            tls_public_keys: None,
        }
    }
}

/// Identifier the service records alongside a receipt so a relying party
/// can prove it was the original requester (ACI §9.1, §9.5).
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
            auth_token_sha256: crate::aci::canonical::sha256_hex(token.as_bytes()),
        }
    }
}
