#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use ed25519_dalek::SigningKey as Ed25519SigningKey;
use k256::ecdsa::{RecoveryId, Signature as K256Signature, SigningKey as K256SigningKey};
use sha3::{Digest, Keccak256};

use private_ai_gateway::aci::e2ee::{
    decrypt_legacy_ecdsa_with_secret_key, decrypt_legacy_ed25519_with_secret_key,
    decrypt_with_secret_key, decrypt_x25519_with_secret_key, ed25519_public_key_hex,
    legacy_ecdsa_public_key_from_secret, public_key_from_secret, secret_key_from_bytes,
    x25519_public_key_hex, x25519_secret_key_from_bytes, E2EE_ALGO_LEGACY_ECDSA,
    E2EE_ALGO_LEGACY_ED25519, E2EE_ALGO_SECP256K1_AESGCM, E2EE_ALGO_X25519_AESGCM,
};
use private_ai_gateway::aci::keys::{
    ethereum_address_from_uncompressed_public_key, KeyError, KeyProvider, LegacySignature, Quote,
    Quoter, ALGO_ED25519, LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519,
};
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::types::{KeyedPublicKey, TlsSpki};
use private_ai_gateway::aggregator::service::UpstreamVerificationRequest;
use x25519_dalek::StaticSecret as X25519SecretKey;

/// A `verified` upstream event with only identity fields, an enforceable
/// channel binding, and the required flag set; everything else takes the
/// struct default. Tests override individual fields with struct-update syntax
/// (`..verified_event("x", "y")`).
pub fn verified_event(upstream_name: &str, model_id: &str) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: upstream_name.to_string(),
        model_id: model_id.to_string(),
        result: VerificationResult::Verified,
        required: true,
        channel_bindings: vec![test_channel_binding()],
        evidence: Some(test_evidence()),
        ..Default::default()
    }
}

/// The evidence bundle [`verified_event`] carries: every stored session needs
/// a complete bundle, its digest plus the data that hashes to it (§8.2).
pub fn test_evidence() -> serde_json::Value {
    let evidence = private_ai_gateway::aggregator::session::EvidenceRef::from_bytes(
        "application/json",
        br#"{"fixture":"verifier-input"}"#,
    );
    serde_json::json!({ "digest": evidence.digest, "data": evidence.data_uri })
}

/// The enforceable channel binding [`verified_event`] pins: the fail-closed
/// gate refuses a verified result with no binding, so mock verified events
/// carry one.
pub fn test_channel_binding() -> ChannelBinding {
    ChannelBinding::TlsSpkiSha256 {
        origin: "https://upstream.test".to_string(),
        spki_sha256: "11".repeat(32),
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
/// request, with the given `result` (verified events get an enforceable
/// binding and evidence). The caller fills `verifier_id` and any `reason` via
/// struct-update syntax.
pub fn event_from_request(
    request: &UpstreamVerificationRequest,
    result: VerificationResult,
) -> UpstreamVerifiedEvent {
    let (channel_bindings, evidence) = match result {
        VerificationResult::Verified => (vec![test_channel_binding()], Some(test_evidence())),
        VerificationResult::Failed => (Vec::new(), None),
    };
    UpstreamVerifiedEvent {
        upstream_name: request.upstream_name.clone(),
        model_id: request.model_id.clone(),
        url_origin: request.url_origin.clone(),
        required: request.required,
        result,
        channel_bindings,
        evidence,
        ..Default::default()
    }
}

pub struct StaticKeyProvider {
    receipt: Ed25519SigningKey,
    x25519_e2ee: X25519SecretKey,
    legacy_e2ee: k256::SecretKey,
    legacy_ed25519: Ed25519SigningKey,
    receipt_key_id: String,
    x25519_e2ee_key_id: String,
    legacy_e2ee_key_id: String,
}

impl Default for StaticKeyProvider {
    fn default() -> Self {
        Self {
            receipt: Ed25519SigningKey::from_bytes(&[0x66; 32]),
            x25519_e2ee: x25519_secret_key_from_bytes(&[0x55; 32]).unwrap(),
            legacy_e2ee: secret_key_from_bytes(&[0x44; 32]).unwrap(),
            legacy_ed25519: Ed25519SigningKey::from_bytes(&[0x33; 32]),
            receipt_key_id: "static-receipt-ed25519".to_string(),
            x25519_e2ee_key_id: "static-e2ee-x25519-key".to_string(),
            legacy_e2ee_key_id: "static-e2ee-key".to_string(),
        }
    }
}

impl StaticKeyProvider {
    /// The one attested receipt key id (Ed25519, §8.2).
    pub fn receipt_key_id(&self) -> &str {
        &self.receipt_key_id
    }

    /// The X25519 E2EE key id (§7.1 suite).
    pub fn x25519_e2ee_key_id(&self) -> &str {
        &self.x25519_e2ee_key_id
    }
}

impl KeyProvider for StaticKeyProvider {
    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: self.receipt_key_id.clone(),
            algo: ALGO_ED25519.to_string(),
            public_key_hex: ed25519_public_key_hex(&self.receipt),
        }]
    }

    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id != self.receipt_key_id {
            return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
        }
        use ed25519_dalek::Signer;
        Ok(self.receipt.sign(payload).to_bytes().to_vec())
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        vec![
            KeyedPublicKey {
                key_id: format!("{}-secp256k1", self.legacy_e2ee_key_id),
                algo: E2EE_ALGO_SECP256K1_AESGCM.to_string(),
                public_key_hex: public_key_from_secret(&self.legacy_e2ee),
            },
            KeyedPublicKey {
                key_id: self.x25519_e2ee_key_id.clone(),
                algo: E2EE_ALGO_X25519_AESGCM.to_string(),
                public_key_hex: x25519_public_key_hex(&self.x25519_e2ee),
            },
        ]
    }

    fn decrypt_e2ee(
        &self,
        key_id: &str,
        ciphertext_hex: &str,
        aad: &[u8],
    ) -> Result<Vec<u8>, KeyError> {
        if key_id == format!("{}-secp256k1", self.legacy_e2ee_key_id) {
            return decrypt_with_secret_key(&self.legacy_e2ee, ciphertext_hex, aad);
        }
        if key_id == self.x25519_e2ee_key_id {
            return decrypt_x25519_with_secret_key(&self.x25519_e2ee, ciphertext_hex, aad);
        }
        Err(KeyError::UnknownE2eeKeyId(key_id.to_string()))
    }

    fn legacy_e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        vec![
            KeyedPublicKey {
                key_id: format!("{}-legacy-ecdsa", self.legacy_e2ee_key_id),
                algo: E2EE_ALGO_LEGACY_ECDSA.to_string(),
                public_key_hex: legacy_ecdsa_public_key_from_secret(&self.legacy_e2ee),
            },
            KeyedPublicKey {
                key_id: format!("{}-legacy-ed25519", self.legacy_e2ee_key_id),
                algo: E2EE_ALGO_LEGACY_ED25519.to_string(),
                public_key_hex: ed25519_public_key_hex(&self.legacy_ed25519),
            },
        ]
    }

    fn decrypt_legacy_e2ee(
        &self,
        signing_algo: &str,
        ciphertext_hex: &str,
        aad: Option<&[u8]>,
    ) -> Result<Vec<u8>, KeyError> {
        match signing_algo {
            E2EE_ALGO_LEGACY_ECDSA => {
                decrypt_legacy_ecdsa_with_secret_key(&self.legacy_e2ee, ciphertext_hex, aad)
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
                // Mirror production: legacy ECDSA signs with the legacy E2EE key.
                let signing_key = K256SigningKey::from(&self.legacy_e2ee);
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
                        &public_key_from_secret(&self.legacy_e2ee),
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
            app_compose: None,
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

/// Load a task-local upstream configuration for session HTTP tests, then remove
/// its file: these tests only read the manager's in-memory snapshot.
pub fn session_config(
    config: serde_json::Value,
) -> Arc<private_ai_gateway::aggregator::upstream_config::UpstreamConfigManager> {
    use private_ai_gateway::aggregator::upstream_config::{
        UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "pag-session-config-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("upstreams.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let manager = UpstreamConfigManager::load(
        &path,
        UpstreamRuntimeOptions {
            verifier_mode: UpstreamVerifierMode::None,
            accepted_subjects: Vec::new(),
            accepted_image_digests: Vec::new(),
            accepted_dstack_kms_root_public_keys: Vec::new(),
            pccs_url: None,
            verifier_cache_seconds: 300,
            connect_timeout_seconds: 10,
            read_timeout_seconds: 600,
            verifier_request_timeout_seconds: 60,
            privatemode_proxy: None,
        },
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    Arc::new(manager.unwrap())
}
