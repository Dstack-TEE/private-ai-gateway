use crate::aci::identity::InvalidNonce;
use crate::aci::keys::KeyError;
use crate::aci::receipt::{ReceiptError, UpstreamVerifiedEvent};
use crate::aci::upstream::UpstreamError;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(
        "refusing to start AciService with a test-only KeyProvider; set \
         allow_test_keys only in tests"
    )]
    TestKeysInProduction,
    #[error(
        "invalid source provenance: repo provenance must include both repo_url and repo_commit; \
         runtime source provenance is loaded from git-launcher or omitted when unknown"
    )]
    InvalidSourceProvenance,
    #[error("failed to seal workload keyset: {0}")]
    Keyset(String),
    #[error("invalid attestation nonce: {0}")]
    InvalidNonce(#[from] InvalidNonce),
    #[error("upstream verification failed: {0}")]
    UpstreamVerification(#[from] UpstreamVerificationError),
    #[error("E2EE request failed: {0}")]
    E2ee(#[from] E2eeError),
    #[error("key provider error: {0}")]
    Key(#[from] KeyError),
    #[error("receipt builder error: {0}")]
    Receipt(#[from] ReceiptError),
    #[error("upstream error: {0}")]
    Upstream(#[from] UpstreamError),
    #[error("attested session store error: {0}")]
    SessionStore(String),
    #[error("metrics error: {0}")]
    Metrics(String),
    #[error("missing receipt signing key in keyset")]
    NoReceiptKey,
    #[error("downstream TLS domain binding is required but request host is missing")]
    DownstreamTlsDomainMissing,
    #[error("no downstream TLS binding configured for request host {0:?}")]
    DownstreamTlsDomainUnknown(String),
}

/// The fail-closed refusal (§1.2): verification was required and did not
/// produce an enforceable verified binding, so the prompt was not forwarded.
/// Carries the §8.5 failed-form event so the response layer can finalize the
/// refusal receipt the `upstream_verification_failed` error cites (§8.5).
#[derive(Debug, thiserror::Error, Clone)]
#[error("upstream verification failed: {reason}")]
pub struct UpstreamVerificationError {
    pub reason: String,
    pub event: Box<UpstreamVerifiedEvent>,
}

#[derive(Debug, thiserror::Error, Clone)]
pub enum E2eeError {
    #[error("missing E2EE header")]
    HeaderMissing,
    #[error("invalid E2EE signing algorithm")]
    InvalidSigningAlgo,
    #[error("invalid E2EE version")]
    InvalidVersion,
    #[error("invalid E2EE public key")]
    InvalidPublicKey,
    #[error("X-Model-Pub-Key does not match this ACI service")]
    ModelKeyMismatch,
    #[error("E2EE decryption failed")]
    DecryptionFailed,
    #[error("E2EE encryption failed")]
    EncryptionFailed,
}
