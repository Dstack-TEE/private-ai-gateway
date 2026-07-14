//! Key-provider and quote-provider abstractions for ACI.
//!
//! An ACI service holds private keys for receipt signing, E2EE termination,
//! and (via the deployment) the TLS endpoint. Every listed public key must
//! satisfy the §4.3 custody rules: generated inside the attested workload,
//! sealed exclusively to it, or released by an attestation-gated mechanism
//! such as a dstack KMS path.
//!
//! Hard constraints:
//!
//! * The aggregator service never holds raw private bytes.
//!   [`KeyProvider`] is the only thing that signs or decrypts.
//! * dstack-specific key custody lives outside this pure ACI module.
//! * Test-only providers live in the test tree, not the runtime library.

use async_trait::async_trait;
use ed25519_dalek::VerifyingKey;
use sha3::{Digest, Keccak256};

use super::types::{KeyedPublicKey, TlsSpki};

/// The one ACI v1 signature algorithm (§8.2, Appendix A).
pub const ALGO_ED25519: &str = "ed25519";
/// §13 legacy `X-Signing-Algo` labels (not ACI algorithm names).
pub const LEGACY_ALGO_ED25519: &str = "ed25519";
pub const LEGACY_ALGO_ECDSA: &str = "ecdsa";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("unknown receipt key id: {0}")]
    UnknownReceiptKeyId(String),
    #[error("unknown E2EE key id: {0}")]
    UnknownE2eeKeyId(String),
    #[error("unsupported algorithm for this provider: {0}")]
    UnsupportedAlgo(String),
    #[error("crypto failure: {0}")]
    Crypto(String),
    #[error("quote provider failure: {0}")]
    Quote(String),
}

#[derive(Debug, Clone)]
pub struct LegacySignature {
    pub signing_algo: String,
    pub signing_address: String,
    pub signature: String,
}

/// Output of a TEE quote operation.
#[derive(Debug, Clone)]
pub struct Quote {
    /// Vendor-encoded quote body (e.g. an Intel TDX quote).
    pub raw_quote: Vec<u8>,
    /// Report-data bytes actually supplied to the TEE quote operation.
    pub report_data: Vec<u8>,
    /// Boot event log, when the vendor format separates it. The dstack
    /// guest agent returns this as a JSON string, so we preserve it as
    /// JSON evidence instead of imposing one wire encoding here.
    pub event_log: serde_json::Value,
    /// VM / TCB configuration metadata. Serialised verbatim into the
    /// attestation envelope `evidence.vm_config`.
    pub vm_config: serde_json::Value,
    /// Raw app-compose.json the CVM booted, for the §10.1(4) compose check:
    /// `sha256(app_compose)` must equal the RTMR3 `compose-hash` event.
    pub app_compose: serde_json::Value,
}

/// Produces a TEE quote binding caller-supplied report-data.
#[async_trait]
pub trait Quoter: Send + Sync {
    /// Return a fresh quote binding the 32-byte ACI `report_data`, placed in
    /// the vendor report-data slot per §4.2 (zero-padded to the slot width).
    async fn get_quote(&self, report_data: [u8; 32]) -> Result<Quote, KeyError>;

    /// Return a fresh quote whose report-data slot equals the supplied 64
    /// bytes verbatim. The legacy dstack-vllm-proxy compatibility report
    /// uses this to bind `signing_address ‖ zeros ‖ nonce` exactly, so the
    /// implementation MUST NOT mutate or pad the supplied bytes.
    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError>;
}

/// The set of ACI private-key operations the aggregator needs.
pub trait KeyProvider: Send + Sync {
    /// Attested Ed25519 receipt signing keys (§4.1).
    fn receipt_keys(&self) -> Vec<KeyedPublicKey>;

    /// Sign the exact receipt payload bytes (§8.2): a raw 64-byte RFC 8032
    /// Ed25519 signature over `payload`.
    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError>;

    /// Attested E2EE keys (§4.1) — spec-shaped suite entries only.
    fn e2ee_keys(&self) -> Vec<KeyedPublicKey>;

    /// Unseal one ACI E2EE v3 request unit (§7.1) with the keyset key named by
    /// `key_id`. `context` and `model`, plus `client_public_key_hex` (the
    /// normalized `X-Client-Pub-Key`), reproduce the request AAD (§7.2).
    fn decrypt_e2ee(
        &self,
        key_id: &str,
        sealed: &[u8],
        context: &str,
        model: &str,
        client_public_key_hex: &str,
    ) -> Result<Vec<u8>, KeyError> {
        let _ = (sealed, context, model, client_public_key_hex);
        Err(KeyError::UnknownE2eeKeyId(key_id.to_string()))
    }

    /// §13 legacy `X-Signing-Algo` compatibility keys (`ecdsa` / `ed25519`
    /// labels). These never appear in the ACI keyset; the legacy report and
    /// E2EE surfaces resolve them here.
    fn legacy_e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        Vec::new()
    }

    /// Decrypt inherited dstack-vllm-proxy E2EE payloads selected by
    /// `X-Signing-Algo` (§13). `aad` is always `None` for the surviving v1
    /// mode; the parameter stays for signature stability with old payloads.
    fn decrypt_legacy_e2ee(
        &self,
        signing_algo: &str,
        ciphertext_hex: &str,
        aad: Option<&[u8]>,
    ) -> Result<Vec<u8>, KeyError> {
        let _ = (ciphertext_hex, aad);
        Err(KeyError::UnsupportedAlgo(signing_algo.to_string()))
    }

    fn tls_spkis(&self) -> Vec<TlsSpki>;

    /// Sign the legacy dstack-vllm-proxy `/v1/signature/{chat_id}` payload
    /// (§13). A compatibility profile, separate from ACI receipt signing.
    fn sign_legacy_message(
        &self,
        signing_algo: &str,
        text: &str,
    ) -> Result<LegacySignature, KeyError> {
        let _ = text;
        Err(KeyError::UnsupportedAlgo(signing_algo.to_string()))
    }

    /// Optional provider-specific proof of key custody or key release (§4.3).
    /// dstack implementations publish KMS signature chains for the released
    /// keys.
    fn key_custody_evidence(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// True for test-only providers. A production launcher checks
    /// this and refuses to start.
    fn is_test_only(&self) -> bool;
}

pub fn ethereum_address_from_uncompressed_public_key(
    public_key_hex: &str,
) -> Result<String, KeyError> {
    let public_key = hex::decode(public_key_hex)
        .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 public key hex: {e}")))?;
    let public_key = match public_key.as_slice() {
        [0x04, rest @ ..] if rest.len() == 64 => rest,
        rest if rest.len() == 64 => rest,
        _ => {
            return Err(KeyError::Crypto(format!(
                "secp256k1 public key must be 64 or 65 bytes, got {}",
                public_key.len()
            )));
        }
    };
    let digest = Keccak256::digest(public_key);
    Ok(format!("0x{}", hex::encode(&digest[12..])))
}

/// Verify an ACI receipt signature (§10.2): a raw RFC 8032 Ed25519 signature
/// over the exact decoded payload bytes. The attested keyset entry decides
/// the algorithm; anything but `ed25519` fails.
pub fn verify_receipt_signature(
    receipt_key: &KeyedPublicKey,
    payload: &[u8],
    signature: &[u8],
) -> bool {
    if receipt_key.algo != ALGO_ED25519 {
        return false;
    }
    let Ok(pub_bytes) = hex::decode(&receipt_key.public_key_hex) else {
        return false;
    };
    let Ok(arr) = <[u8; 32]>::try_from(pub_bytes.as_slice()) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&arr) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
    vk.verify_strict(payload, &sig).is_ok()
}
