use async_trait::async_trait;
use ed25519_dalek::{Signer, SigningKey};
use private_ai_proxy_aci::aci::e2ee::{
    public_key_from_secret, secret_key_from_bytes, x25519_public_key_hex,
    x25519_secret_key_from_bytes, E2EE_ALGO_SECP256K1_AESGCM, E2EE_ALGO_X25519_AESGCM,
};
use private_ai_proxy_aci::aci::keys::{KeyError, KeyProvider, Quote, Quoter, ALGO_ED25519};
use private_ai_proxy_aci::aci::receipt::{
    ChannelBinding, UpstreamVerifiedEvent, VerificationResult,
};
use private_ai_proxy_aci::aci::types::{KeyedPublicKey, TlsSpki};

pub fn verified_event(upstream_name: &str, model_id: &str) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: upstream_name.to_string(),
        model_id: model_id.to_string(),
        result: VerificationResult::Verified,
        required: true,
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://upstream.test".to_string(),
            spki_sha256: "11".repeat(32),
        }],
        ..Default::default()
    }
}

pub struct StaticKeyProvider {
    receipt: SigningKey,
}

impl Default for StaticKeyProvider {
    fn default() -> Self {
        Self {
            receipt: SigningKey::from_bytes(&[0x66; 32]),
        }
    }
}

impl KeyProvider for StaticKeyProvider {
    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: "static-receipt-ed25519".to_string(),
            algo: ALGO_ED25519.to_string(),
            public_key_hex: hex::encode(self.receipt.verifying_key().as_bytes()),
        }]
    }

    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        if key_id != "static-receipt-ed25519" {
            return Err(KeyError::UnknownReceiptKeyId(key_id.to_string()));
        }
        Ok(self.receipt.sign(payload).to_bytes().to_vec())
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        let secp256k1 = secret_key_from_bytes(&[0x44; 32]).expect("fixed test key");
        let x25519 = x25519_secret_key_from_bytes(&[0x55; 32]).expect("fixed test key");
        vec![
            KeyedPublicKey {
                key_id: "static-e2ee-key-secp256k1".to_string(),
                algo: E2EE_ALGO_SECP256K1_AESGCM.to_string(),
                public_key_hex: public_key_from_secret(&secp256k1),
            },
            KeyedPublicKey {
                key_id: "static-e2ee-x25519-key".to_string(),
                algo: E2EE_ALGO_X25519_AESGCM.to_string(),
                public_key_hex: x25519_public_key_hex(&x25519),
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

pub struct StubQuoter;

impl StubQuoter {
    fn quote(report_data: Vec<u8>) -> Quote {
        let mut raw_quote = b"aci-stub-quote|".to_vec();
        raw_quote.extend_from_slice(&report_data);
        Quote {
            raw_quote,
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
        Ok(Self::quote(report_data.to_vec()))
    }

    async fn get_quote_raw(&self, report_data: [u8; 64]) -> Result<Quote, KeyError> {
        Ok(Self::quote(report_data.to_vec()))
    }
}
