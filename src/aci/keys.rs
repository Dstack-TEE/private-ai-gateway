//! Key-provider and quote-provider abstractions for ACI.
//!
//! An ACI service must hold private keys for the workload identity,
//! receipt signing, E2EE termination, and the TLS endpoint. The ACI
//! draft requires that every listed public key correspond to a
//! private key generated inside the attested workload, sealed
//! exclusively to it, or released by an attestation-gated mechanism
//! such as a dstack KMS path.
//!
//! Hard constraints:
//!
//! * The aggregator service never holds raw private bytes.
//!   [`KeyProvider`] is the only thing that signs.
//! * dstack-specific key custody lives outside this pure ACI module
//!   and uses the Rust dstack SDK.
//! * Test-only providers live in the integration test tree, not in
//!   the runtime library.

use async_trait::async_trait;
use ed25519_dalek::VerifyingKey;
use k256::ecdsa::Signature as K256Signature;
use k256::EncodedPoint;
use sha2::{Digest, Sha256};
use sha3::Keccak256;

use super::types::{KeyedPublicKey, PublicKeyMaterial, TlsSpki};

/// Wire algorithm names matching ACI §4.1.
pub const ALGO_ED25519: &str = "ed25519";
pub const ALGO_ECDSA_SECP256K1: &str = "ecdsa-secp256k1";
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
}

/// Produces a TEE quote binding caller-supplied report-data.
#[async_trait]
pub trait Quoter: Send + Sync {
    /// Return a fresh quote whose report-data slot binds the supplied 32
    /// bytes (the vendor profile decides any padding to the native slot
    /// width). Used by the canonical ACI report.
    async fn get_quote(&self, report_data: [u8; 32]) -> Result<Quote, KeyError>;

    /// Return a fresh quote whose report-data slot equals the supplied 64
    /// bytes verbatim. The legacy dstack-vllm-proxy compatibility report
    /// uses this to bind `signing_address ‖ zeros ‖ nonce` exactly, so the
    /// implementation MUST NOT mutate or pad the supplied bytes.
    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError>;
}

/// The set of ACI private-key operations the aggregator needs.
pub trait KeyProvider: Send + Sync {
    fn identity_public_key(&self) -> PublicKeyMaterial;

    /// Sign the JCS-canonicalised endorsement payload from
    /// [`super::identity::keyset_endorsement_payload`]. The algorithm
    /// MUST match [`KeyProvider::identity_public_key`]`.algo`.
    fn sign_keyset_endorsement(&self, payload: &[u8]) -> Result<Vec<u8>, KeyError>;

    fn receipt_keys(&self) -> Vec<KeyedPublicKey>;

    /// Sign the JCS canonical bytes of the receipt minus
    /// `signature.value` (ACI §9.4).
    ///
    /// * `ed25519`: raw 64-byte RFC 8032 signature over
    ///   `canonical_bytes`.
    /// * `ecdsa-secp256k1`: 65-byte recoverable signature over
    ///   `sha256(canonical_bytes)`, encoded as `r || s || v`.
    fn sign_receipt(&self, key_id: &str, canonical_bytes: &[u8]) -> Result<Vec<u8>, KeyError>;

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey>;

    /// Decrypt an ACI E2EE v2 field using a key listed in
    /// [`KeyProvider::e2ee_keys`].
    fn decrypt_e2ee(
        &self,
        key_id: &str,
        ciphertext_hex: &str,
        aad: &[u8],
    ) -> Result<Vec<u8>, KeyError> {
        let _ = (ciphertext_hex, aad);
        Err(KeyError::UnknownE2eeKeyId(key_id.to_string()))
    }

    /// Decrypt inherited dstack-vllm-proxy E2EE payloads selected by
    /// `X-Signing-Algo`. `aad == None` is legacy v1; `Some` is the
    /// legacy v2 AAD string.
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

    /// Sign the legacy dstack-vllm-proxy `/v1/signature/{chat_id}`
    /// payload. This is a compatibility profile, separate from ACI
    /// receipt signing.
    fn sign_legacy_message(
        &self,
        signing_algo: &str,
        text: &str,
    ) -> Result<LegacySignature, KeyError> {
        let _ = text;
        Err(KeyError::UnsupportedAlgo(signing_algo.to_string()))
    }

    /// Optional provider-specific proof of key custody or key release.
    /// dstack implementations use this to publish KMS signature chains
    /// for the released keys.
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

// ---------- Verifiers (used by tests; useful as reference) ----------

/// Verify a keyset endorsement signature under the identity key.
///
/// `ed25519`: 64-byte RFC 8032 signature over `payload`.
/// `ecdsa-secp256k1`: 64-byte `r || s` signature over `sha256(payload)`,
/// matching the in-process signer above. (ACI §4.2 leaves identity
/// endorsement encoding implementation-defined; receipts are the
/// path that mandates the 65-byte recoverable shape.)
pub fn verify_keyset_endorsement(
    identity: &PublicKeyMaterial,
    payload: &[u8],
    signature: &[u8],
) -> bool {
    match identity.algo.as_str() {
        ALGO_ED25519 => {
            let Ok(pub_bytes) = hex::decode(&identity.public_key_hex) else {
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
        ALGO_ECDSA_SECP256K1 => {
            if signature.len() != 64 {
                return false;
            }
            let Ok(pub_bytes) = hex::decode(&identity.public_key_hex) else {
                return false;
            };
            let Ok(pt) = EncodedPoint::from_bytes(&pub_bytes) else {
                return false;
            };
            let vk = match k256::ecdsa::VerifyingKey::from_encoded_point(&pt) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let sig = match K256Signature::from_slice(signature) {
                Ok(s) => s,
                Err(_) => return false,
            };
            use k256::ecdsa::signature::Verifier;
            vk.verify(payload, &sig).is_ok()
        }
        _ => false,
    }
}

/// Verify an ACI receipt signature per §9.4.
///
/// * `ed25519`: raw RFC 8032 signature over `canonical_bytes`.
/// * `ecdsa-secp256k1`: exactly 65 bytes encoded `r || s || v` (32 +
///   32 + 1) over `sha256(canonical_bytes)`. `v` must recover the
///   listed receipt public key. Bare 64-byte `r || s` shapes are
///   rejected: that is the JOSE ES256K form which ACI §9.4
///   explicitly excludes.
pub fn verify_receipt_signature(
    receipt_key: &KeyedPublicKey,
    canonical_bytes: &[u8],
    signature: &[u8],
) -> bool {
    match receipt_key.algo.as_str() {
        ALGO_ED25519 => {
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
            vk.verify_strict(canonical_bytes, &sig).is_ok()
        }
        ALGO_ECDSA_SECP256K1 => {
            if signature.len() != 65 {
                return false;
            }
            let mut v = signature[64];
            if (27..=30).contains(&v) {
                v -= 27;
            }
            let Some(recid) = k256::ecdsa::RecoveryId::from_byte(v) else {
                return false;
            };
            let r_s = &signature[..64];
            let Ok(pub_bytes) = hex::decode(&receipt_key.public_key_hex) else {
                return false;
            };
            let Ok(pt) = EncodedPoint::from_bytes(&pub_bytes) else {
                return false;
            };
            let expected_vk = match k256::ecdsa::VerifyingKey::from_encoded_point(&pt) {
                Ok(vk) => vk,
                Err(_) => return false,
            };
            let sig = match K256Signature::from_slice(r_s) {
                Ok(s) => s,
                Err(_) => return false,
            };
            let prehash: [u8; 32] = Sha256::digest(canonical_bytes).into();
            let Ok(recovered_vk) =
                k256::ecdsa::VerifyingKey::recover_from_prehash(&prehash, &sig, recid)
            else {
                return false;
            };
            recovered_vk == expected_vk
        }
        _ => false,
    }
}
