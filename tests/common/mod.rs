#![allow(dead_code)]

use async_trait::async_trait;
use ed25519_dalek::SigningKey as Ed25519SigningKey;
use k256::ecdsa::{
    signature::Signer as K256Signer, RecoveryId, Signature as K256Signature,
    SigningKey as K256SigningKey,
};
use sha2::{Digest, Sha256};
use sha3::Keccak256;

use private_ai_gateway::aci::e2ee::{
    decrypt_legacy_ecdsa_with_secret_key, decrypt_legacy_ed25519_with_secret_key,
    decrypt_with_secret_key, decrypt_x25519_with_secret_key, ed25519_public_key_hex,
    legacy_ecdsa_public_key_from_secret, public_key_from_secret, secret_key_from_bytes,
    x25519_public_key_hex, x25519_secret_key_from_bytes, E2EE_ALGO_LEGACY_ECDSA,
    E2EE_ALGO_LEGACY_ED25519, E2EE_ALGO_SECP256K1_AESGCM, E2EE_ALGO_X25519_AESGCM,
};
use private_ai_gateway::aci::keys::{
    ethereum_address_from_uncompressed_public_key, KeyError, KeyProvider, LegacySignature, Quote,
    Quoter, ALGO_ECDSA_SECP256K1, ALGO_ED25519, LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519,
};
use private_ai_gateway::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::types::{KeyedPublicKey, PublicKeyMaterial, TlsSpki};
use private_ai_gateway::aggregator::service::UpstreamVerificationRequest;
use x25519_dalek::StaticSecret as X25519SecretKey;

/// A `verified` upstream event with only identity fields and the required flag
/// set; everything else takes the struct default. Tests override individual
/// fields with struct-update syntax (`..verified_event("x", "y")`).
pub fn verified_event(upstream_name: &str, model_id: &str) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: upstream_name.to_string(),
        model_id: model_id.to_string(),
        result: VerificationResult::Verified,
        required: true,
        ..Default::default()
    }
}

/// Like [`verified_event`] but fail-closed (`result: Failed`).
pub fn failed_event(upstream_name: &str, model_id: &str) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: upstream_name.to_string(),
        model_id: model_id.to_string(),
        result: VerificationResult::Failed,
        required: true,
        ..Default::default()
    }
}

/// Builds an event the way a mock `UpstreamVerifier` does: copying
/// `upstream_name` / `model_id` / `url_origin` / `required` straight off the
/// request, with the given `result`. The caller fills `verifier_id` and any
/// `reason` / `evidence` via struct-update syntax.
pub fn event_from_request(
    request: &UpstreamVerificationRequest,
    result: VerificationResult,
) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: request.upstream_name.clone(),
        model_id: request.model_id.clone(),
        url_origin: request.url_origin.clone(),
        required: request.required,
        result,
        ..Default::default()
    }
}

pub struct StaticKeyProvider {
    identity: K256SigningKey,
    receipt_ed25519: Ed25519SigningKey,
    receipt_secp256k1: K256SigningKey,
    e2ee: k256::SecretKey,
    x25519_e2ee: X25519SecretKey,
    legacy_ed25519: Ed25519SigningKey,
    ed25519_receipt_key_id: String,
    secp256k1_receipt_key_id: String,
    e2ee_key_id: String,
    x25519_e2ee_key_id: String,
}

impl Default for StaticKeyProvider {
    fn default() -> Self {
        Self {
            identity: K256SigningKey::from_slice(&[0x11; 32]).unwrap(),
            receipt_ed25519: Ed25519SigningKey::from_bytes(&[0x66; 32]),
            receipt_secp256k1: K256SigningKey::from_slice(&[0x22; 32]).unwrap(),
            e2ee: secret_key_from_bytes(&[0x44; 32]).unwrap(),
            x25519_e2ee: x25519_secret_key_from_bytes(&[0x55; 32]).unwrap(),
            legacy_ed25519: Ed25519SigningKey::from_bytes(&[0x33; 32]),
            ed25519_receipt_key_id: "static-receipt-ed25519".to_string(),
            secp256k1_receipt_key_id: "static-receipt-secp256k1".to_string(),
            e2ee_key_id: "static-e2ee-key".to_string(),
            x25519_e2ee_key_id: "static-e2ee-x25519-key".to_string(),
        }
    }
}

impl StaticKeyProvider {
    /// The default receipt key id (Ed25519, §8.5 RECOMMENDED).
    pub fn receipt_key_id(&self) -> &str {
        &self.ed25519_receipt_key_id
    }

    /// The still-listed secp256k1 receipt key id, for exercising that path.
    pub fn secp256k1_receipt_key_id(&self) -> &str {
        &self.secp256k1_receipt_key_id
    }

    /// The X25519 E2EE key id.
    pub fn x25519_e2ee_key_id(&self) -> &str {
        &self.x25519_e2ee_key_id
    }
}

fn public_key_hex(key: &K256SigningKey) -> String {
    hex::encode(key.verifying_key().to_encoded_point(false).as_bytes())
}

impl KeyProvider for StaticKeyProvider {
    fn identity_public_key(&self) -> PublicKeyMaterial {
        PublicKeyMaterial {
            algo: ALGO_ECDSA_SECP256K1.to_string(),
            public_key_hex: public_key_hex(&self.identity),
        }
    }

    fn sign_keyset_endorsement(&self, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        let sig: K256Signature = K256Signer::sign(&self.identity, payload);
        Ok(sig.to_bytes().to_vec())
    }

    fn sign_keyset_revocation(&self, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        let sig: K256Signature = K256Signer::sign(&self.identity, payload);
        Ok(sig.to_bytes().to_vec())
    }

    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        // Ed25519 first = default signer (§8.5); secp256k1 stays listed.
        vec![
            KeyedPublicKey {
                key_id: self.ed25519_receipt_key_id.clone(),
                algo: ALGO_ED25519.to_string(),
                public_key_hex: ed25519_public_key_hex(&self.receipt_ed25519),
            },
            KeyedPublicKey {
                key_id: self.secp256k1_receipt_key_id.clone(),
                algo: ALGO_ECDSA_SECP256K1.to_string(),
                public_key_hex: public_key_hex(&self.receipt_secp256k1),
            },
        ]
    }

    fn sign_receipt(&self, key_id: &str, canonical_bytes: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id == self.ed25519_receipt_key_id {
            use ed25519_dalek::Signer;
            let sig = self.receipt_ed25519.sign(canonical_bytes);
            return Ok(sig.to_bytes().to_vec());
        }
        if key_id != self.secp256k1_receipt_key_id {
            return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
        }
        let prehash: [u8; 32] = Sha256::digest(canonical_bytes).into();
        let (sig, recid): (K256Signature, RecoveryId) = self
            .receipt_secp256k1
            .sign_prehash_recoverable(&prehash)
            .map_err(|e| KeyError::Crypto(format!("k256 sign_prehash: {e}")))?;
        let mut out = Vec::with_capacity(65);
        out.extend_from_slice(&sig.to_bytes());
        out.push(recid.to_byte());
        Ok(out)
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        vec![
            KeyedPublicKey {
                key_id: self.e2ee_key_id.clone(),
                algo: E2EE_ALGO_SECP256K1_AESGCM.to_string(),
                public_key_hex: public_key_from_secret(&self.e2ee),
            },
            KeyedPublicKey {
                key_id: self.x25519_e2ee_key_id.clone(),
                algo: E2EE_ALGO_X25519_AESGCM.to_string(),
                public_key_hex: x25519_public_key_hex(&self.x25519_e2ee),
            },
            KeyedPublicKey {
                key_id: format!("{}-legacy-ecdsa", self.e2ee_key_id),
                algo: E2EE_ALGO_LEGACY_ECDSA.to_string(),
                public_key_hex: legacy_ecdsa_public_key_from_secret(&self.e2ee),
            },
            KeyedPublicKey {
                key_id: format!("{}-legacy-ed25519", self.e2ee_key_id),
                algo: E2EE_ALGO_LEGACY_ED25519.to_string(),
                public_key_hex: ed25519_public_key_hex(&self.legacy_ed25519),
            },
        ]
    }

    fn decrypt_e2ee(
        &self,
        key_id: &str,
        ciphertext_hex: &str,
        aad: &[u8],
    ) -> Result<Vec<u8>, KeyError> {
        if key_id == self.e2ee_key_id {
            return decrypt_with_secret_key(&self.e2ee, ciphertext_hex, aad);
        }
        if key_id == self.x25519_e2ee_key_id {
            return decrypt_x25519_with_secret_key(&self.x25519_e2ee, ciphertext_hex, aad);
        }
        Err(KeyError::UnknownE2eeKeyId(key_id.to_string()))
    }

    fn decrypt_legacy_e2ee(
        &self,
        signing_algo: &str,
        ciphertext_hex: &str,
        aad: Option<&[u8]>,
    ) -> Result<Vec<u8>, KeyError> {
        match signing_algo {
            E2EE_ALGO_LEGACY_ECDSA => {
                decrypt_legacy_ecdsa_with_secret_key(&self.e2ee, ciphertext_hex, aad)
            }
            E2EE_ALGO_LEGACY_ED25519 => {
                decrypt_legacy_ed25519_with_secret_key(&self.legacy_ed25519, ciphertext_hex, aad)
            }
            _ => Err(KeyError::UnsupportedAlgo(signing_algo.to_string())),
        }
    }

    fn tls_spkis(&self) -> Vec<TlsSpki> {
        Vec::new()
    }

    fn sign_legacy_message(
        &self,
        signing_algo: &str,
        text: &str,
    ) -> Result<LegacySignature, KeyError> {
        match signing_algo {
            LEGACY_ALGO_ECDSA => {
                // Mirror production: legacy ECDSA signs with the E2EE key.
                let signing_key = K256SigningKey::from(&self.e2ee);
                let prehash = ethereum_personal_message_hash(text);
                let (sig, recid): (K256Signature, RecoveryId) = signing_key
                    .sign_prehash_recoverable(&prehash)
                    .map_err(|e| KeyError::Crypto(format!("k256 legacy sign_prehash: {e}")))?;
                let mut out = Vec::with_capacity(65);
                out.extend_from_slice(&sig.to_bytes());
                out.push(recid.to_byte() + 27);
                Ok(LegacySignature {
                    signing_algo: LEGACY_ALGO_ECDSA.to_string(),
                    signing_address: ethereum_address_from_uncompressed_public_key(
                        &public_key_from_secret(&self.e2ee),
                    )?,
                    signature: format!("0x{}", hex::encode(out)),
                })
            }
            LEGACY_ALGO_ED25519 => {
                use ed25519_dalek::Signer;
                let sig = self.legacy_ed25519.sign(text.as_bytes());
                Ok(LegacySignature {
                    signing_algo: LEGACY_ALGO_ED25519.to_string(),
                    signing_address: hex::encode(self.legacy_ed25519.verifying_key().as_bytes()),
                    signature: hex::encode(sig.to_bytes()),
                })
            }
            _ => Err(KeyError::UnsupportedAlgo(signing_algo.to_string())),
        }
    }

    fn is_test_only(&self) -> bool {
        true
    }
}

fn ethereum_personal_message_hash(text: &str) -> [u8; 32] {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", text.len());
    let mut hasher = Keccak256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

pub struct StubQuoter {
    vendor_label: Vec<u8>,
}

impl Default for StubQuoter {
    fn default() -> Self {
        Self {
            vendor_label: b"aci-stub-quote".to_vec(),
        }
    }
}

impl StubQuoter {
    fn quote_for(&self, report_data: Vec<u8>) -> Quote {
        let mut raw = Vec::with_capacity(self.vendor_label.len() + 1 + report_data.len());
        raw.extend_from_slice(&self.vendor_label);
        raw.push(b'|');
        raw.extend_from_slice(&report_data);
        Quote {
            raw_quote: raw,
            report_data,
            event_log: serde_json::Value::Null,
            vm_config: serde_json::json!({ "stub": true }),
        }
    }
}

#[async_trait]
impl Quoter for StubQuoter {
    async fn get_quote(&self, report_data: [u8; 32]) -> Result<Quote, KeyError> {
        Ok(self.quote_for(report_data.to_vec()))
    }

    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        Ok(self.quote_for(report_data.to_vec()))
    }
}
