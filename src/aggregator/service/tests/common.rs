//! Fixed keys, quote and receipt clock for the service characterization tests.
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use ed25519_dalek::{Signer, SigningKey};

use crate::aci::e2ee::{
    ed25519_public_key_hex, public_key_from_secret, secret_key_from_bytes, x25519_public_key_hex,
    x25519_secret_key_from_bytes, E2EE_ALGO_SECP256K1_AESGCM, E2EE_ALGO_X25519_AESGCM,
};
use crate::aci::keys::{KeyError, KeyProvider, Quote, Quoter, ALGO_ED25519};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::types::{KeyedPublicKey, TlsSpki};
use crate::aggregator::service::{Clock, UpstreamVerificationRequest};

pub struct TestClock(pub AtomicU64);
impl TestClock {
    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
}
impl Clock for TestClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub fn event_from_request(
    request: &UpstreamVerificationRequest,
    result: VerificationResult,
) -> UpstreamVerifiedEvent {
    let evidence = crate::aggregator::session::EvidenceRef::from_bytes(
        "application/json",
        br#"{"fixture":"verifier-input"}"#,
    );
    UpstreamVerifiedEvent {
        upstream_name: request.upstream_name.clone(),
        model_id: request.model_id.clone(),
        url_origin: request.url_origin.clone(),
        required: request.required,
        result,
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://upstream.test".into(),
            spki_sha256: "11".repeat(32),
        }],
        evidence: Some(serde_json::json!({"digest": evidence.digest, "data": evidence.data_uri})),
        ..Default::default()
    }
}

pub struct StaticKeyProvider {
    receipt: SigningKey,
    x25519: x25519_dalek::StaticSecret,
    secp256k1: k256::SecretKey,
}
impl Default for StaticKeyProvider {
    fn default() -> Self {
        Self {
            receipt: SigningKey::from_bytes(&[0x66; 32]),
            x25519: x25519_secret_key_from_bytes(&[0x55; 32]).unwrap(),
            secp256k1: secret_key_from_bytes(&[0x44; 32]).unwrap(),
        }
    }
}
impl KeyProvider for StaticKeyProvider {
    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: "static-receipt-ed25519".into(),
            algo: ALGO_ED25519.into(),
            public_key_hex: ed25519_public_key_hex(&self.receipt),
        }]
    }
    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id != "static-receipt-ed25519" {
            return Err(KeyError::UnknownReceiptKeyId(key_id.into()));
        }
        Ok(self.receipt.sign(payload).to_bytes().to_vec())
    }
    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        vec![
            KeyedPublicKey {
                key_id: "static-e2ee-key-secp256k1".into(),
                algo: E2EE_ALGO_SECP256K1_AESGCM.into(),
                public_key_hex: public_key_from_secret(&self.secp256k1),
            },
            KeyedPublicKey {
                key_id: "static-e2ee-x25519-key".into(),
                algo: E2EE_ALGO_X25519_AESGCM.into(),
                public_key_hex: x25519_public_key_hex(&self.x25519),
            },
        ]
    }
    fn tls_spkis(&self) -> Vec<TlsSpki> {
        Vec::new()
    }
    fn is_test_only(&self) -> bool {
        true
    }
}

#[derive(Default)]
pub struct StubQuoter;
impl StubQuoter {
    fn quote(&self, report_data: &[u8]) -> Quote {
        let mut raw_quote = b"aci-stub-quote|".to_vec();
        raw_quote.extend_from_slice(report_data);
        Quote {
            raw_quote,
            report_data: report_data.to_vec(),
            event_log: serde_json::Value::Null,
            vm_config: serde_json::json!({"stub": true}),
            app_compose: None,
        }
    }
}
#[async_trait]
impl Quoter for StubQuoter {
    async fn get_quote(&self, report_data: [u8; 32]) -> Result<Quote, KeyError> {
        Ok(self.quote(&report_data))
    }
    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        Ok(self.quote(&report_data))
    }
}
