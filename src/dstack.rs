//! dstack-backed ACI key custody and quote provider.
//!
//! This module is the only runtime implementation that talks to
//! dstack. It uses the Rust dstack SDK for KMS key release and TDX
//! quotes; protocol-only modules stay independent of dstack APIs.

use std::sync::Arc;

use async_trait::async_trait;
use dstack_sdk::dstack_client_v0::GetKeyConfig;
use dstack_sdk::DstackClientV1;
use ed25519_dalek::SigningKey as Ed25519SigningKey;
use k256::ecdsa::{RecoveryId, Signature as K256Signature, SigningKey as K256SigningKey};
use serde_json::json;
use sha3::{Digest, Keccak256};
use x25519_dalek::StaticSecret as X25519SecretKey;

use crate::aci::e2ee::{
    decrypt_legacy_ecdsa_with_secret_key, decrypt_legacy_ed25519_with_secret_key,
    decrypt_with_secret_key, decrypt_x25519_with_secret_key, ed25519_public_key_hex,
    legacy_ecdsa_public_key_from_secret, public_key_from_secret, secret_key_from_bytes,
    x25519_public_key_hex, x25519_secret_key_from_bytes, E2EE_ALGO_LEGACY_ECDSA,
    E2EE_ALGO_LEGACY_ED25519, E2EE_ALGO_SECP256K1_AESGCM, E2EE_ALGO_X25519_AESGCM,
};
use crate::aci::identity::report_data_slot;
use crate::aci::keys::{
    ethereum_address_from_uncompressed_public_key, KeyError, KeyProvider, LegacySignature, Quote,
    Quoter, ALGO_ED25519, LEGACY_ALGO_ECDSA, LEGACY_ALGO_ED25519,
};
use crate::aci::types::{KeyedPublicKey, TlsSpki};

mod attestation;

// The frozen guest API preserves the deployed KMS key derivation.
#[expect(deprecated, reason = "the gateway uses the frozen dstack guest API")]
type DstackClientV0 = dstack_sdk::dstack_client_v0::DstackClientV0;

const RECEIPT_PURPOSE: &str = "aci.receipt.ed25519.v1";
const E2EE_X25519_PURPOSE: &str = "aci.e2ee.x25519.v1";
/// The k256 E2EE key doubles as the E2EE v2 secp256k1 key, legacy
/// `signing_address`, and legacy `ecdsa` E2EE key. The ed25519 key is used only
/// by the legacy `ed25519` mode.
const LEGACY_E2EE_PURPOSE: &str = "aci.e2ee.v1";
const LEGACY_ED25519_PURPOSE: &str = "aci.legacy.ed25519.v1";
const PREFIX_HASH_PURPOSE: &str = "aci.prefix-hash.hmac-sha256.v1";
const CACHE_SALT_PATH: &str = "aci/cache-salt-hmac-sha256/v1";
const CACHE_SALT_PURPOSE: &str = "aci.cache-salt.hmac-sha256.v1";

#[derive(Debug, Clone)]
pub struct DstackAciProviderConfig {
    pub receipt_path: String,
    pub x25519_e2ee_path: String,
    pub legacy_e2ee_path: String,
    pub legacy_ed25519_path: String,
    pub prefix_hash_path: String,
    pub receipt_key_id: String,
    pub x25519_e2ee_key_id: String,
    pub legacy_e2ee_key_id: String,
    pub legacy_ed25519_key_id: String,
}

impl Default for DstackAciProviderConfig {
    fn default() -> Self {
        Self {
            receipt_path: "aci/receipt-ed25519/v1".to_string(),
            x25519_e2ee_path: "aci/e2ee-x25519/v1".to_string(),
            // Unchanged from the pre-ACI deployment: this path fixes the
            // legacy `signing_address` old clients already pin.
            legacy_e2ee_path: "aci/e2ee/v1".to_string(),
            legacy_ed25519_path: "aci/legacy-ed25519/v1".to_string(),
            prefix_hash_path: "aci/prefix-hash-hmac-sha256/v1".to_string(),
            receipt_key_id: "dstack-kms-receipt-ed25519-v1".to_string(),
            x25519_e2ee_key_id: "dstack-kms-e2ee-x25519-v1".to_string(),
            legacy_e2ee_key_id: "dstack-kms-e2ee-v1".to_string(),
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
    /// Compressed hex of the k256 counterpart of the released 32-byte scalar.
    /// The dstack KMS signature chain covers `"{purpose}:{kms_public_key}"`,
    /// so publishing it makes the chain verifiable for non-k256 roles.
    kms_public_key_hex: String,
    signature_chain: Vec<String>,
}

/// Provider backed by dstack KMS keys plus dstack TDX quotes.
pub struct DstackAciProvider {
    client: Arc<DstackClientV0>,
    attest_client: Option<DstackClientV1>,
    receipt: Ed25519SigningKey,
    x25519_e2ee: X25519SecretKey,
    legacy_e2ee: k256::SecretKey,
    legacy_ed25519: Ed25519SigningKey,
    receipt_evidence: KmsKeyEvidence,
    secp256k1_e2ee_evidence: KmsKeyEvidence,
    x25519_e2ee_evidence: KmsKeyEvidence,
    receipt_key_id: String,
    x25519_e2ee_key_id: String,
    legacy_e2ee_key_id: String,
    legacy_ed25519_key_id: String,
    prefix_hash_path: String,
}

impl DstackAciProvider {
    pub async fn new(
        endpoint: Option<String>,
        config: DstackAciProviderConfig,
    ) -> Result<Self, KeyError> {
        let endpoint = normalize_dstack_endpoint(endpoint)?;
        let client = Arc::new(DstackClientV0::new(endpoint.as_deref()));
        let attest_client = DstackClientV1::new(endpoint.as_deref());
        let attest_client = match attest_client.version().await {
            Ok(_) => Some(attest_client),
            // SDK 0.6 includes the HTTP status and the pRPC error text in
            // Display. Only an unmounted surface indicates a pre-v1 guest.
            Err(error)
                if error
                    .to_string()
                    .strip_prefix("HTTP 404: ")
                    .is_some_and(|body| !body.starts_with("Service not found:")) =>
            {
                None
            }
            Err(error) => return Err(KeyError::Quote(format!("dstack v1 Version: {error}"))),
        };
        Self::from_client(client, attest_client, config).await
    }

    async fn from_client(
        client: Arc<DstackClientV0>,
        attest_client: Option<DstackClientV1>,
        config: DstackAciProviderConfig,
    ) -> Result<Self, KeyError> {
        let (receipt_bytes, receipt_chain) =
            load_kms_raw32_key(&client, "receipt", &config.receipt_path, RECEIPT_PURPOSE).await?;
        let receipt = Ed25519SigningKey::from_bytes(&receipt_bytes);
        let receipt_evidence = KmsKeyEvidence {
            role: "receipt",
            path: config.receipt_path.clone(),
            purpose: RECEIPT_PURPOSE,
            algo: ALGO_ED25519,
            public_key_hex: ed25519_public_key_hex(&receipt),
            kms_public_key_hex: kms_counterpart_public_key_hex("receipt", &receipt_bytes)?,
            signature_chain: receipt_chain,
        };

        let (x25519_bytes, x25519_chain) = load_kms_raw32_key(
            &client,
            "e2ee-x25519",
            &config.x25519_e2ee_path,
            E2EE_X25519_PURPOSE,
        )
        .await?;
        let x25519_e2ee = x25519_secret_key_from_bytes(&x25519_bytes)?;
        let x25519_e2ee_evidence = KmsKeyEvidence {
            role: "e2ee-x25519",
            path: config.x25519_e2ee_path.clone(),
            purpose: E2EE_X25519_PURPOSE,
            algo: E2EE_ALGO_X25519_AESGCM,
            public_key_hex: x25519_public_key_hex(&x25519_e2ee),
            kms_public_key_hex: kms_counterpart_public_key_hex("e2ee-x25519", &x25519_bytes)?,
            signature_chain: x25519_chain,
        };

        let (legacy_e2ee_bytes, legacy_e2ee_chain) = load_kms_raw32_key(
            &client,
            "legacy-e2ee",
            &config.legacy_e2ee_path,
            LEGACY_E2EE_PURPOSE,
        )
        .await?;
        let legacy_e2ee = secret_key_from_bytes(&legacy_e2ee_bytes)?;
        let secp256k1_e2ee_evidence = KmsKeyEvidence {
            role: "e2ee-secp256k1",
            path: config.legacy_e2ee_path.clone(),
            purpose: LEGACY_E2EE_PURPOSE,
            algo: E2EE_ALGO_SECP256K1_AESGCM,
            public_key_hex: public_key_from_secret(&legacy_e2ee),
            kms_public_key_hex: kms_counterpart_public_key_hex(
                "e2ee-secp256k1",
                &legacy_e2ee_bytes,
            )?,
            signature_chain: legacy_e2ee_chain,
        };

        let (legacy_ed25519_bytes, _) = load_kms_raw32_key(
            &client,
            "legacy-ed25519",
            &config.legacy_ed25519_path,
            LEGACY_ED25519_PURPOSE,
        )
        .await?;
        let legacy_ed25519 = Ed25519SigningKey::from_bytes(&legacy_ed25519_bytes);

        Ok(Self {
            client,
            attest_client,
            receipt,
            x25519_e2ee,
            legacy_e2ee,
            legacy_ed25519,
            receipt_evidence,
            secp256k1_e2ee_evidence,
            x25519_e2ee_evidence,
            receipt_key_id: config.receipt_key_id,
            x25519_e2ee_key_id: config.x25519_e2ee_key_id,
            legacy_e2ee_key_id: config.legacy_e2ee_key_id,
            legacy_ed25519_key_id: config.legacy_ed25519_key_id,
            prefix_hash_path: config.prefix_hash_path,
        })
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

/// The compressed k256 public key the dstack KMS signature chain covers for a
/// released 32-byte scalar.
fn kms_counterpart_public_key_hex(role: &str, key_bytes: &[u8; 32]) -> Result<String, KeyError> {
    let key = K256SigningKey::from_slice(key_bytes)
        .map_err(|e| KeyError::Crypto(format!("invalid k256 scalar for {role}: {e}")))?;
    Ok(hex::encode(key.verifying_key().to_sec1_bytes()))
}

/// Release a KMS key and require it to be exactly 32 bytes — the raw scalar
/// shared by the k256/Ed25519/X25519 roles.
async fn load_kms_raw32_key(
    client: &DstackClientV0,
    role: &'static str,
    path: &str,
    purpose: &'static str,
) -> Result<([u8; 32], Vec<String>), KeyError> {
    let response = client
        .get_key(GetKeyConfig::builder().path(path).purpose(purpose).build())
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
    Ok((key_bytes, response.signature_chain))
}

fn decode_hex_field(field: &str, value: &str) -> Result<Vec<u8>, KeyError> {
    let hex_value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(hex_value).map_err(|e| KeyError::Quote(format!("invalid hex in {field}: {e}")))
}

impl DstackAciProvider {
    /// Release the middleware cache-isolation key, separate from prefixHash.
    /// Bumping both versioned constants rotates all user cache partitions.
    pub async fn cache_salt_key(&self) -> Result<[u8; 32], KeyError> {
        let (key, _signature_chain) = load_kms_raw32_key(
            &self.client,
            "cache-salt",
            CACHE_SALT_PATH,
            CACHE_SALT_PURPOSE,
        )
        .await?;
        Ok(key)
    }

    /// Derive the middleware's prefix-hash HMAC key. It stays inside the TEE,
    /// so the control plane cannot test guessed prefixes against `prefixHash`.
    pub async fn prefix_hash_key(&self) -> Result<[u8; 32], KeyError> {
        let (key, _signature_chain) = load_kms_raw32_key(
            &self.client,
            "prefix-hash",
            &self.prefix_hash_path,
            PREFIX_HASH_PURPOSE,
        )
        .await?;
        Ok(key)
    }

    /// Request a dstack TDX quote binding exactly the supplied 64-byte
    /// report-data, returning the quote plus its event log and VM config.
    async fn quote_with_report_data(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        if let Some(client) = &self.attest_client {
            let response = client
                .attest(report_data.to_vec(), false)
                .await
                .map_err(|e| KeyError::Quote(format!("dstack v1 Attest: {e}")))?;
            let mut quote = attestation::decode_tdx_quote(&response.attestation, report_data)?;
            let info = client
                .info()
                .await
                .map_err(|e| KeyError::Quote(format!("dstack v1 Info: {e}")))?;
            quote.app_compose = Some(info.app_compose);
            return Ok(quote);
        }

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
            // The values stay out: on the legacy report path the expected one
            // is the caller's nonce, and this text is logged.
            return Err(KeyError::Quote(
                "dstack quote report_data mismatch".to_string(),
            ));
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
            app_compose: Some(info.tcb_info.app_compose),
        })
    }
}

#[async_trait]
impl Quoter for DstackAciProvider {
    async fn get_quote(&self, report_data: [u8; 32]) -> Result<Quote, KeyError> {
        self.quote_with_report_data(report_data_slot(report_data))
            .await
    }

    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        self.quote_with_report_data(report_data).await
    }
}

impl KeyProvider for DstackAciProvider {
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
        // Raw 64-byte RFC 8032 signature over the exact payload bytes (§7.2).
        use ed25519_dalek::Signer;
        Ok(self.receipt.sign(payload).to_bytes().to_vec())
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        vec![
            KeyedPublicKey {
                key_id: self.legacy_e2ee_key_id.clone(),
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
        if key_id == self.legacy_e2ee_key_id {
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
                key_id: self.legacy_e2ee_key_id.clone(),
                algo: E2EE_ALGO_LEGACY_ECDSA.to_string(),
                public_key_hex: legacy_ecdsa_public_key_from_secret(&self.legacy_e2ee),
            },
            KeyedPublicKey {
                key_id: self.legacy_ed25519_key_id.clone(),
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
                // Legacy clients use one secp256k1 key (the E2EE key) for both
                // encryption and response signing, and verify against the
                // attestation `signing_address` (also the E2EE key) — sign with it.
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
                    signing_address: ed25519_public_key_hex(&self.legacy_ed25519),
                    signature: hex::encode(sig.to_bytes()),
                })
            }
            _ => Err(KeyError::UnsupportedAlgo(signing_algo.to_string())),
        }
    }

    fn key_custody_evidence(&self) -> serde_json::Value {
        // Keyset roles only (§3.3). The secp256k1 key also serves the legacy
        // compatibility surface, but it is listed here because E2EE v2
        // publishes and accepts that same key as a keyset role.
        json!({
            "provider": "dstack-kms",
            "keys": [
                key_evidence_json(&self.receipt_evidence),
                key_evidence_json(&self.secp256k1_e2ee_evidence),
                key_evidence_json(&self.x25519_e2ee_evidence),
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
        "kms_public_key": evidence.kms_public_key_hex,
        "signature_chain": evidence.signature_chain,
    })
}

#[cfg(test)]
mod tests {
    use super::normalize_dstack_endpoint;

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

    #[cfg(unix)]
    mod guest_rpc {
        use std::sync::Arc;
        use std::time::Duration;

        use aci_verify::dstack::verify_dstack_compose_measurement;
        use aci_verify::quote::quote_bytes;
        use aci_verify::report::validate_aci_report_binding;
        use axum::body::Body;
        use bytes::Bytes;
        use dcap_qvl::quote::Quote as DcapQuote;
        use dstack_sdk::dstack_client_v1::AttestResponse;
        use http_body_util::{BodyExt, Full};
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper::{Method, Request, Response, StatusCode};
        use hyper_util::rt::TokioIo;
        use serde_json::{json, Value};
        use tokio::net::UnixListener;
        use tower::ServiceExt;

        use super::super::{attestation, DstackAciProvider, DstackAciProviderConfig, Quoter};
        use crate::aci::keys::KeyError;
        use crate::aci::types::AttestationReport;
        use crate::aci::upstream::OpenAICompatibleBackend;
        use crate::aci::verifier::{dcap_report_data, verify_dstack_event_log};
        use crate::aggregator::service::{
            AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
        };
        use crate::http::build_router;

        // The V1 app-id vector generated by cc-eventlog at dad8326f in PR #531.
        const V1_LOG: &str = concat!(
            r#"[{"imr":0,"event_type":4,"digest":"010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101","event":"","event_payload":"0102"},"#,
            r#"{"imr":3,"event_type":134217729,"digest":"#,
            r#""3f1fbb6cfe25e7f19bd48cfa9dc590cabaa946da04f9215402e7083550177292c1478402b258a90c649ccfcb5f408175","#,
            r#""event":"app-id","event_payload":"1111111111111111111111111111111111111111"}]"#,
        );
        const APP_COMPOSE: &str = r#"{"name":"gateway"}"#;
        const RECORDED_ATTEST: &str =
            include_str!("../tests/fixtures/dstack/tdx-lite-v1-attest.json");

        type RpcResponse = Box<dyn Fn(&Value) -> String>;
        type Exchange = (&'static str, StatusCode, RpcResponse);

        fn rpc(path: &'static str, request: Value, response: Value) -> Exchange {
            (
                path,
                StatusCode::OK,
                Box::new(move |body| {
                    assert_eq!(body, &request);
                    response.to_string()
                }),
            )
        }

        #[tokio::test]
        async fn dstack_05_preserves_legacy_evidence() {
            let quote_log = V1_LOG.replace(r#""event_payload":"0102""#, r#""event_payload":"""#);
            let nonce = [0xa5; 32];
            let mut report_data = [0; 64];
            report_data[..32].copy_from_slice(&nonce);
            let report_data_hex = hex::encode(report_data);
            let tcb_info = json!({
                "mrtd": "00".repeat(48),
                "rtmr0": "01".repeat(48),
                "rtmr1": "02".repeat(48),
                "rtmr2": "03".repeat(48),
                "rtmr3": "04".repeat(48),
                "compose_hash": "05".repeat(32),
                "device_id": "06".repeat(32),
                "app_compose": APP_COMPOSE,
                "event_log": serde_json::from_str::<Value>(V1_LOG).unwrap(),
            });
            let quote = with_guest(
                false,
                [
                    rpc(
                        "/GetQuote",
                        json!({"report_data": report_data_hex}),
                        json!({
                            "quote": "1122",
                            "event_log": quote_log,
                            "report_data": report_data_hex,
                            "vm_config": "{}",
                        }),
                    ),
                    rpc(
                        "/Info",
                        json!({}),
                        json!({
                            "app_id": "07".repeat(20),
                            "instance_id": "08".repeat(20),
                            "app_cert": "",
                            "tcb_info": serde_json::to_string_pretty(&tcb_info).unwrap(),
                            "app_name": "gateway",
                            "device_id": tcb_info["device_id"],
                            "key_provider_info": "",
                            "compose_hash": tcb_info["compose_hash"],
                            "vm_config": "{}",
                        }),
                    ),
                ],
                async move |provider| provider.get_quote(nonce).await,
            )
            .await
            .unwrap();
            assert_eq!(quote.report_data, report_data);
            assert_eq!(quote.raw_quote, [0x11, 0x22]);
            assert_eq!(quote.event_log.as_str(), Some(V1_LOG));
            assert_eq!(quote.vm_config.as_str(), Some("{}"));
            assert_eq!(quote.app_compose.as_deref(), Some(APP_COMPOSE));
        }

        #[tokio::test]
        async fn dstack_06_uses_recorded_v1_attestation() {
            let report_data = [0x42; 64];
            let quote = with_guest(
                true,
                [
                    rpc(
                        "/v1/Attest",
                        json!({"report_data": hex::encode(report_data), "include_boottime_gpu_evidence": false}),
                        serde_json::from_str(RECORDED_ATTEST).unwrap(),
                    ),
                    rpc(
                        "/v1/Info",
                        json!({}),
                        json!({
                            "app_id": "07".repeat(20),
                            "instance_id": "08".repeat(20),
                            "device_id": "06".repeat(32),
                            "compose_hash": "05".repeat(32),
                            "app_compose": APP_COMPOSE,
                        }),
                    ),
                ],
                async move |provider| provider.get_quote_raw(report_data).await,
            )
            .await
            .unwrap();
            assert_eq!(quote.report_data, report_data);
            let event_log: Vec<Value> =
                serde_json::from_str(quote.event_log.as_str().unwrap()).unwrap();
            assert!(event_log
                .iter()
                .any(|event| event["event"] == "compose-hash"));
            let vm_config: Value = serde_json::from_str(quote.vm_config.as_str().unwrap()).unwrap();
            assert_eq!(vm_config["tdx_attestation_variant"], "lite");
            assert_eq!(quote.app_compose.as_deref(), Some(APP_COMPOSE));
        }

        #[tokio::test]
        async fn gateway_v2_report_verifies_rtmr3_and_compose() {
            const NOW: u64 = 1_700_000_000;
            let nonce = "17".repeat(32);
            let fixture: Value = serde_json::from_str(include_str!(
                "../clients/verifier-ts/test/fixtures/dstack_event_vectors.json"
            ))
            .unwrap();
            let vector = fixture["vectors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|vector| vector["name"] == "v2")
                .unwrap();
            let mut binary_events = vector["events"].clone();
            for event in binary_events.as_array_mut().unwrap() {
                for field in ["digest", "event_payload"] {
                    event[field] = json!(hex::decode(event[field].as_str().unwrap()).unwrap());
                }
            }
            let rtmr3 = hex::decode(vector["rtmr3"].as_str().unwrap()).unwrap();
            let recorded: AttestResponse = serde_json::from_str(RECORDED_ATTEST).unwrap();
            let recorded =
                attestation::decode_tdx_quote(&recorded.attestation, [0x42; 64]).unwrap();
            let quote = DcapQuote::parse(&recorded.raw_quote).unwrap();
            assert_eq!(quote.header.version, 4);
            assert!(matches!(quote.report, dcap_qvl::quote::Report::TD10(_)));
            let attest: RpcResponse = Box::new(move |request| {
                assert_eq!(request["include_boottime_gpu_evidence"], false);
                let report_data = request["report_data"].as_str().unwrap();
                let report_bytes: [u8; 64] = hex::decode(report_data).unwrap().try_into().unwrap();
                let mut raw_quote = recorded.raw_quote.clone();
                // Substitute the V2 RTMR3 and the caller's challenge in the TD10
                // body. This invalidates the signature, so the test starts
                // verification at the parsed quote body.
                raw_quote[520..568].copy_from_slice(&rtmr3);
                raw_quote[568..632].copy_from_slice(&report_bytes);
                let bytes = rmp_serde::to_vec_named(&json!({
                    "version": 1,
                    "platform": {"kind": "tdx", "data": {"quote": raw_quote, "event_log": binary_events}},
                    "stack": {"kind": "dstack", "data": {"report_data": report_bytes.to_vec(), "config": "{}"}},
                }))
                .unwrap();
                json!({"attestation": hex::encode(bytes)}).to_string()
            });
            let body = with_guest(
                true,
                [
                    ("/v1/Attest", StatusCode::OK, attest),
                    rpc(
                        "/v1/Info",
                        json!({}),
                        json!({
                            "app_id": "11".repeat(20),
                            "instance_id": "08".repeat(20),
                            "device_id": "06".repeat(32),
                            "compose_hash": vector["compose_hash"],
                            "app_compose": vector["app_compose"],
                        }),
                    ),
                ],
                async |provider| {
                    let provider = Arc::new(provider);
                    let service = Arc::new(
                        AciService::new(
                            provider.clone(),
                            provider,
                            Arc::new(OpenAICompatibleBackend::new("http://127.0.0.1:1").unwrap()),
                            Arc::new(InMemoryReceiptStore::default()),
                            AciServiceConfig::for_test(),
                            Arc::new(FixedClock(NOW)),
                        )
                        .unwrap(),
                    );
                    let response = build_router(service)
                        .oneshot(
                            Request::builder()
                                .uri(format!("/v1/aci/attestation?nonce={nonce}"))
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                    Ok(response.into_body().collect().await.unwrap().to_bytes())
                },
            )
            .await
            .unwrap();
            let report: AttestationReport = serde_json::from_slice(&body).unwrap();
            let binding = validate_aci_report_binding(&report, Some(&nonce), NOW, None).unwrap();
            let evidence = &report.attestation.evidence;
            let quote = DcapQuote::parse(&quote_bytes(evidence).unwrap()).unwrap();
            assert_eq!(
                dcap_report_data(&quote.report),
                &crate::aci::identity::report_data_slot(binding.report_data)
            );
            let log = verify_dstack_event_log(evidence, &quote.report).unwrap();
            assert_eq!(
                verify_dstack_compose_measurement(evidence, &log).unwrap(),
                vector["compose_hash"]
            );

            let mut altered_evidence = evidence.clone();
            let mut events: Value =
                serde_json::from_str(evidence["event_log"].as_str().unwrap()).unwrap();
            assert_eq!(events, vector["events"]);
            assert!(events[0]
                .as_object_mut()
                .unwrap()
                .shift_remove("preimage")
                .is_some());
            altered_evidence["event_log"] = json!(events.to_string());
            assert!(verify_dstack_event_log(&altered_evidence, &quote.report)
                .unwrap_err()
                .contains("preimage"));
        }

        async fn with_guest<T>(
            v1: bool,
            attestation_exchanges: impl IntoIterator<Item = Exchange>,
            call: impl AsyncFnOnce(DstackAciProvider) -> Result<T, KeyError>,
        ) -> Result<T, KeyError> {
            let config = DstackAciProviderConfig::default();
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("dstack.sock");
            let listener = UnixListener::bind(&socket_path).unwrap();
            let key = json!({"key": "01".repeat(32), "signature_chain": ["ab", "cd"]});
            let probe = if v1 {
                rpc(
                    "/v1/Version",
                    json!({}),
                    json!({"version": "0.6.5", "rev": "test"}),
                )
            } else {
                (
                    "/v1/Version",
                    StatusCode::NOT_FOUND,
                    Box::new(|body: &Value| {
                        assert_eq!(body, &json!({}));
                        "<h1>Not Found</h1>".into()
                    }) as RpcResponse,
                )
            };
            let mut exchanges = vec![probe];
            for (path, purpose) in [
                (&config.receipt_path, "aci.receipt.ed25519.v1"),
                (&config.x25519_e2ee_path, "aci.e2ee.x25519.v1"),
                (&config.legacy_e2ee_path, "aci.e2ee.v1"),
                (&config.legacy_ed25519_path, "aci.legacy.ed25519.v1"),
            ] {
                exchanges.push(rpc(
                    "/GetKey",
                    json!({"path": path, "purpose": purpose, "algorithm": "secp256k1"}),
                    key.clone(),
                ));
            }
            exchanges.extend(attestation_exchanges);

            let server = async {
                for (path, status, response) in exchanges {
                    let (stream, _) = listener.accept().await.unwrap();
                    http1::Builder::new()
                        .keep_alive(false)
                        .serve_connection(
                            TokioIo::new(stream),
                            service_fn(|request: hyper::Request<hyper::body::Incoming>| {
                                let response = &response;
                                async move {
                                    assert_eq!(request.method(), Method::POST);
                                    assert_eq!(
                                        request.uri().path_and_query().unwrap().as_str(),
                                        path
                                    );
                                    let headers = request.headers();
                                    assert_eq!(headers.get_all("content-type").iter().count(), 1);
                                    assert_eq!(headers["content-type"], "application/json");
                                    assert_eq!(headers["host"], "dstack");
                                    let body =
                                        request.into_body().collect().await.unwrap().to_bytes();
                                    let response =
                                        response(&serde_json::from_slice(&body).unwrap());
                                    Ok::<_, std::convert::Infallible>(
                                        Response::builder()
                                            .status(status)
                                            .body(Full::new(Bytes::from(response)))
                                            .unwrap(),
                                    )
                                }
                            }),
                        )
                        .await
                        .unwrap();
                }
                Ok(())
            };
            let client = async {
                let provider =
                    DstackAciProvider::new(Some(format!("unix:{}", socket_path.display())), config)
                        .await
                        .unwrap();
                call(provider).await
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::try_join!(server, client).map(|(_, quote)| quote)
            })
            .await
            .unwrap()
        }
    }
}
