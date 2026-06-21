//! dstack-backed ACI key custody and quote provider.
//!
//! This module is the only runtime implementation that talks to
//! dstack. It uses the Rust dstack SDK for KMS key release and TDX
//! quotes; protocol-only modules stay independent of dstack APIs.

use std::sync::Arc;

use async_trait::async_trait;
use dstack_sdk::dstack_client::{DstackClient, GetKeyResponse};
use ed25519_dalek::SigningKey as Ed25519SigningKey;
use k256::ecdsa::{
    signature::Signer as K256Signer, RecoveryId, Signature as K256Signature,
    SigningKey as K256SigningKey,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sha3::Keccak256;

use crate::aci::e2ee::{
    decrypt_legacy_ecdsa_with_secret_key, decrypt_legacy_ed25519_with_secret_key,
    decrypt_with_secret_key, ed25519_public_key_hex, legacy_ecdsa_public_key_from_secret,
    public_key_from_secret, secret_key_from_bytes, E2EE_ALGO_LEGACY_ECDSA,
    E2EE_ALGO_LEGACY_ED25519, E2EE_ALGO_SECP256K1_AESGCM,
};
use crate::aci::keys::{
    ethereum_address_from_uncompressed_public_key, KeyError, KeyProvider, LegacySignature, Quote,
    Quoter, ALGO_ECDSA_SECP256K1, LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519,
};
use crate::aci::types::{KeyedPublicKey, PublicKeyMaterial, TlsSpki};

const IDENTITY_PURPOSE: &str = "aci.identity.v1";
const RECEIPT_PURPOSE: &str = "aci.receipt.v1";
const E2EE_PURPOSE: &str = "aci.e2ee.v1";
const LEGACY_ED25519_PURPOSE: &str = "aci.legacy.ed25519.v1";

#[derive(Debug, Clone)]
pub struct DstackAciProviderConfig {
    pub identity_path: String,
    pub receipt_path: String,
    pub e2ee_path: String,
    pub legacy_ed25519_path: String,
    pub receipt_key_id: String,
    pub e2ee_key_id: String,
    pub legacy_ed25519_key_id: String,
}

impl Default for DstackAciProviderConfig {
    fn default() -> Self {
        Self {
            identity_path: "aci/identity/v1".to_string(),
            receipt_path: "aci/receipt/v1".to_string(),
            e2ee_path: "aci/e2ee/v1".to_string(),
            legacy_ed25519_path: "aci/legacy-ed25519/v1".to_string(),
            receipt_key_id: "dstack-kms-receipt-v1".to_string(),
            e2ee_key_id: "dstack-kms-e2ee-v1".to_string(),
            legacy_ed25519_key_id: "dstack-kms-legacy-ed25519-v1".to_string(),
        }
    }
}

#[derive(Clone)]
struct KmsKeyEvidence {
    role: &'static str,
    path: String,
    purpose: &'static str,
    algo: &'static str,
    public_key_hex: String,
    signature_chain: Vec<String>,
}

/// Provider backed by dstack KMS keys plus dstack TDX quotes.
pub struct DstackAciProvider {
    client: Arc<DstackClient>,
    identity: K256SigningKey,
    receipt: K256SigningKey,
    e2ee: k256::SecretKey,
    legacy_ed25519: Ed25519SigningKey,
    identity_evidence: KmsKeyEvidence,
    receipt_evidence: KmsKeyEvidence,
    e2ee_evidence: KmsKeyEvidence,
    legacy_ed25519_evidence: KmsKeyEvidence,
    receipt_key_id: String,
    e2ee_key_id: String,
    legacy_ed25519_key_id: String,
}

impl DstackAciProvider {
    pub async fn new(
        endpoint: Option<String>,
        config: DstackAciProviderConfig,
    ) -> Result<Self, KeyError> {
        let endpoint = normalize_dstack_endpoint(endpoint)?;
        let client = Arc::new(DstackClient::new(endpoint.as_deref()));
        Self::from_client(client, config).await
    }

    async fn from_client(
        client: Arc<DstackClient>,
        config: DstackAciProviderConfig,
    ) -> Result<Self, KeyError> {
        let (identity, identity_evidence) =
            load_kms_signing_key(&client, "identity", &config.identity_path, IDENTITY_PURPOSE)
                .await?;
        let (receipt, receipt_evidence) =
            load_kms_signing_key(&client, "receipt", &config.receipt_path, RECEIPT_PURPOSE).await?;
        let (e2ee_signing_key, e2ee_evidence) =
            load_kms_signing_key(&client, "e2ee", &config.e2ee_path, E2EE_PURPOSE).await?;
        let e2ee = secret_key_from_bytes(&e2ee_signing_key.to_bytes())?;
        let (legacy_ed25519, legacy_ed25519_evidence) = load_kms_ed25519_key(
            &client,
            "legacy-ed25519",
            &config.legacy_ed25519_path,
            LEGACY_ED25519_PURPOSE,
        )
        .await?;

        Ok(Self {
            client,
            identity,
            receipt,
            e2ee,
            legacy_ed25519,
            identity_evidence,
            receipt_evidence,
            e2ee_evidence,
            legacy_ed25519_evidence,
            receipt_key_id: config.receipt_key_id,
            e2ee_key_id: config.e2ee_key_id,
            legacy_ed25519_key_id: config.legacy_ed25519_key_id,
        })
    }

    pub fn dstack_report_data(aci_report_data: [u8; 32]) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&aci_report_data);
        out
    }
}

fn normalize_dstack_endpoint(endpoint: Option<String>) -> Result<Option<String>, KeyError> {
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(KeyError::Quote("dstack endpoint is empty".to_string()));
    }
    let normalized = endpoint
        .strip_prefix("unix://")
        .or_else(|| endpoint.strip_prefix("unix:"))
        .unwrap_or(endpoint);
    if normalized.is_empty() {
        return Err(KeyError::Quote("dstack endpoint is empty".to_string()));
    }
    Ok(Some(normalized.to_string()))
}

async fn load_kms_signing_key(
    client: &DstackClient,
    role: &'static str,
    path: &str,
    purpose: &'static str,
) -> Result<(K256SigningKey, KmsKeyEvidence), KeyError> {
    let response = client
        .get_key(Some(path.to_string()), Some(purpose.to_string()))
        .await
        .map_err(|e| KeyError::Crypto(format!("dstack KMS get_key({role}): {e}")))?;
    let key = signing_key_from_kms_response(role, &response)?;
    let public_key_hex = public_key_hex(&key);
    let evidence = KmsKeyEvidence {
        role,
        path: path.to_string(),
        purpose,
        algo: if role == "e2ee" {
            E2EE_ALGO_SECP256K1_AESGCM
        } else {
            ALGO_ECDSA_SECP256K1
        },
        public_key_hex,
        signature_chain: response.signature_chain,
    };
    Ok((key, evidence))
}

async fn load_kms_ed25519_key(
    client: &DstackClient,
    role: &'static str,
    path: &str,
    purpose: &'static str,
) -> Result<(Ed25519SigningKey, KmsKeyEvidence), KeyError> {
    let response = client
        .get_key(Some(path.to_string()), Some(purpose.to_string()))
        .await
        .map_err(|e| KeyError::Crypto(format!("dstack KMS get_key({role}): {e}")))?;
    let key_bytes = response
        .decode_key()
        .map_err(|e| KeyError::Crypto(format!("invalid dstack KMS key hex for {role}: {e}")))?;
    let key_bytes: [u8; 32] = key_bytes.as_slice().try_into().map_err(|_| {
        KeyError::Crypto(format!(
            "dstack KMS key for {role} must be 32 bytes, got {}",
            key_bytes.len()
        ))
    })?;
    let key = Ed25519SigningKey::from_bytes(&key_bytes);
    let public_key_hex = ed25519_public_key_hex(&key);
    let evidence = KmsKeyEvidence {
        role,
        path: path.to_string(),
        purpose,
        algo: E2EE_ALGO_LEGACY_ED25519,
        public_key_hex,
        signature_chain: response.signature_chain,
    };
    Ok((key, evidence))
}

fn signing_key_from_kms_response(
    role: &str,
    response: &GetKeyResponse,
) -> Result<K256SigningKey, KeyError> {
    let key_bytes = response
        .decode_key()
        .map_err(|e| KeyError::Crypto(format!("invalid dstack KMS key hex for {role}: {e}")))?;
    if key_bytes.len() != 32 {
        return Err(KeyError::Crypto(format!(
            "dstack KMS key for {role} must be 32 bytes, got {}",
            key_bytes.len()
        )));
    }
    K256SigningKey::from_slice(&key_bytes)
        .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 KMS key for {role}: {e}")))
}

fn public_key_hex(key: &K256SigningKey) -> String {
    hex::encode(key.verifying_key().to_encoded_point(false).as_bytes())
}

fn decode_hex_field(field: &str, value: &str) -> Result<Vec<u8>, KeyError> {
    let hex_value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(hex_value).map_err(|e| KeyError::Quote(format!("invalid hex in {field}: {e}")))
}

impl DstackAciProvider {
    /// Request a dstack TDX quote binding exactly the supplied 64-byte
    /// report-data, returning the verified quote plus its event log and VM
    /// config evidence.
    async fn quote_with_report_data(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        let response = self
            .client
            .get_quote(report_data.to_vec())
            .await
            .map_err(|e| KeyError::Quote(format!("dstack get_quote: {e}")))?;
        let raw_quote = response
            .decode_quote()
            .map_err(|e| KeyError::Quote(format!("invalid dstack quote hex: {e}")))?;
        let returned_report_data = decode_hex_field("dstack report_data", &response.report_data)?;
        if returned_report_data != report_data {
            return Err(KeyError::Quote(format!(
                "dstack quote report_data mismatch: expected {}, got {}",
                hex::encode(report_data),
                hex::encode(returned_report_data)
            )));
        }
        let info = self
            .client
            .info()
            .await
            .map_err(|e| KeyError::Quote(format!("dstack info: {e}")))?;
        let event_log = serde_json::to_string(&info.tcb_info.event_log)
            .map_err(|e| KeyError::Quote(format!("serialize dstack event log: {e}")))?;

        Ok(Quote {
            raw_quote,
            report_data: returned_report_data,
            event_log: serde_json::Value::String(event_log),
            vm_config: serde_json::Value::String(response.vm_config),
        })
    }
}

#[async_trait]
impl Quoter for DstackAciProvider {
    async fn get_quote(&self, report_data: [u8; 32]) -> Result<Quote, KeyError> {
        self.quote_with_report_data(Self::dstack_report_data(report_data))
            .await
    }

    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        self.quote_with_report_data(report_data).await
    }
}

impl KeyProvider for DstackAciProvider {
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

    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: self.receipt_key_id.clone(),
            algo: ALGO_ECDSA_SECP256K1.to_string(),
            public_key_hex: public_key_hex(&self.receipt),
        }]
    }

    fn sign_receipt(&self, key_id: &str, canonical_bytes: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id != self.receipt_key_id {
            return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
        }

        let prehash: [u8; 32] = Sha256::digest(canonical_bytes).into();
        let (sig, recid): (K256Signature, RecoveryId) = self
            .receipt
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
                key_id: format!("{}-legacy-ecdsa", self.e2ee_key_id),
                algo: E2EE_ALGO_LEGACY_ECDSA.to_string(),
                public_key_hex: legacy_ecdsa_public_key_from_secret(&self.e2ee),
            },
            KeyedPublicKey {
                key_id: self.legacy_ed25519_key_id.clone(),
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
        if key_id != self.e2ee_key_id {
            return Err(KeyError::UnknownE2eeKeyId(key_id.to_string()));
        }
        decrypt_with_secret_key(&self.e2ee, ciphertext_hex, aad)
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
                // Legacy clients use one secp256k1 key (the E2EE key) for both
                // encryption and response signing, and verify against the
                // attestation `signing_address` (also the E2EE key) — sign with it.
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
                    signing_address: ed25519_public_key_hex(&self.legacy_ed25519),
                    signature: hex::encode(sig.to_bytes()),
                })
            }
            _ => Err(KeyError::UnsupportedAlgo(signing_algo.to_string())),
        }
    }

    fn key_custody_evidence(&self) -> serde_json::Value {
        json!({
            "provider": "dstack-kms",
            "keys": [
                key_evidence_json(&self.identity_evidence),
                key_evidence_json(&self.receipt_evidence),
                key_evidence_json(&self.e2ee_evidence),
                key_evidence_json(&self.legacy_ed25519_evidence),
            ],
        })
    }

    fn is_test_only(&self) -> bool {
        false
    }
}

fn ethereum_personal_message_hash(text: &str) -> [u8; 32] {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", text.len());
    let mut hasher = Keccak256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

fn key_evidence_json(evidence: &KmsKeyEvidence) -> serde_json::Value {
    json!({
        "role": evidence.role,
        "path": evidence.path,
        "purpose": evidence.purpose,
        "algo": evidence.algo,
        "public_key": evidence.public_key_hex,
        "signature_chain": evidence.signature_chain,
    })
}

#[cfg(test)]
mod tests {
    use super::{normalize_dstack_endpoint, DstackAciProvider};

    #[test]
    fn normalizes_unix_scheme_for_dstack_sdk() {
        assert_eq!(
            normalize_dstack_endpoint(Some("unix:/tmp/dstack.sock".to_string()))
                .unwrap()
                .as_deref(),
            Some("/tmp/dstack.sock")
        );
        assert_eq!(
            normalize_dstack_endpoint(Some("unix:///tmp/dstack.sock".to_string()))
                .unwrap()
                .as_deref(),
            Some("/tmp/dstack.sock")
        );
    }

    #[test]
    fn dstack_report_data_is_aci_digest_padded_with_zeroes() {
        let aci_report_data = [0x42u8; 32];
        let dstack_report_data = DstackAciProvider::dstack_report_data(aci_report_data);
        assert_eq!(&dstack_report_data[..32], &aci_report_data);
        assert_eq!(&dstack_report_data[32..], &[0u8; 32]);
    }
}
