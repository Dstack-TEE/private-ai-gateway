use crate::aci::canonical::CanonicalError;
use crate::aci::keys::KeyError;
use crate::aci::receipt::ReceiptError;
use crate::aci::upstream::UpstreamError;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(
        "refusing to start AciService with a test-only KeyProvider; set \
         allow_test_keys only in tests"
    )]
    TestKeysInProduction,
    #[error(
        "ACI §5.2 requires at least one source provenance arm: \
         (repo_url + repo_commit) or image_digest"
    )]
    InvalidSourceProvenance,
    #[error("upstream verification failed: {0}")]
    UpstreamVerification(#[from] UpstreamVerificationError),
    #[error("E2EE request failed: {0}")]
    E2ee(#[from] E2eeError),
    #[error("canonicalisation error: {0}")]
    Canonical(#[from] CanonicalError),
    #[error("key provider error: {0}")]
    Key(#[from] KeyError),
    #[error("receipt builder error: {0}")]
    Receipt(#[from] ReceiptError),
    #[error("upstream error: {0}")]
    Upstream(#[from] UpstreamError),
    #[error("metrics error: {0}")]
    Metrics(String),
    #[error("missing receipt signing key in keyset")]
    NoReceiptKey,
}

#[derive(Debug, thiserror::Error, Clone)]
pub enum UpstreamVerificationError {
    #[error("upstream verification required but no verifier result supplied")]
    NoVerifierResult,
    #[error("upstream verifier reported failed: {0}")]
    VerifierFailed(String),
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
    #[error("invalid E2EE nonce")]
    InvalidNonce,
    #[error("E2EE replay detected")]
    ReplayDetected,
    #[error("invalid E2EE timestamp")]
    InvalidTimestamp,
    #[error("invalid E2EE payload model")]
    InvalidPayloadModel,
    #[error("E2EE decryption failed")]
    DecryptionFailed,
    #[error("E2EE encryption failed")]
    EncryptionFailed,
}
