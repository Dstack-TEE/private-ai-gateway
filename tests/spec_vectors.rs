//! Byte-exact cross-check against `spec/test-vectors.md`.
//!
//! Every constant here is a published vector; the tests rebuild each artifact
//! through the library's own constructions (fixed seeds, no randomness) and
//! assert the exact bytes, digests, and signatures. If any construction in the
//! implementation drifts from the spec, one of these tests goes red.
//! `spec/tools/gen-vectors.py` reproduces the same values independently.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey as Ed25519SigningKey};
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    x25519_public_key_hex, x25519_secret_key_from_bytes, E2EE_ALGO_X25519_AESGCM,
};
use private_ai_gateway::aci::identity::{
    attestation_statement, report_data, report_data_slot, SealedWorkloadKeyset,
};
use private_ai_gateway::aci::keys::{verify_receipt_signature, KeyError, KeyProvider};
use private_ai_gateway::aci::receipt::{
    receipt_signing_input, ChannelBinding, ReceiptBuilder, UpstreamVerifiedEvent,
    VerificationResult,
};
use private_ai_gateway::aci::types::{KeyedPublicKey, TlsSpki, WorkloadKeyset};
use private_ai_gateway::aggregator::session::{
    AttestedSession, Claim, ClaimSource, EvidenceRef, SessionClaims, SessionDocument,
};

// ---- Fixed keys (test-vectors "Fixed keys") --------------------------------

const RECEIPT_SEED: [u8; 32] = [0x02; 32];
const E2EE_SEED: [u8; 32] = [0x03; 32];

const RECEIPT_PUB: &str = "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394";
const E2EE_PUB: &str = "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22";

// ---- §1 workload keyset ------------------------------------------------------

const KEYSET_BYTES: &str = r#"{"e2ee_public_keys":[{"algo":"x25519-aes-256-gcm-hkdf-sha256","key_id":"e2ee-1","public_key":"5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22"}],"not_after":1800000000,"receipt_signing_keys":[{"algo":"ed25519","key_id":"receipt-1","public_key":"8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394"}],"subject":"dstack-app://example-app","tls_public_keys":[{"domain":"api.example.com","spki_sha256":"c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0"}]}"#;
const KEYSET_DIGEST: &str =
    "sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371";

// ---- §2 attestation statement / report_data ---------------------------------

const STATEMENT_TEST_NONCE: &str = r#"{"keyset_digest":"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.report_data.v1"}"#;
const STATEMENT_NULL: &str = r#"{"keyset_digest":"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371","nonce":null,"purpose":"aci.report_data.v1"}"#;
const REPORT_DATA_TEST_NONCE: &str =
    "df2174d28130852b413646a3786927b93e94c11d770268b65def8bdba45cb49e";
const REPORT_DATA_NULL: &str = "0633919ca3f00e97bafaa3304278eb22420cc3ff0d19f87dfca2d3f7508150bc";
const REPORT_DATA_SLOT: &str = "df2174d28130852b413646a3786927b93e94c11d770268b65def8bdba45cb49e0000000000000000000000000000000000000000000000000000000000000000";

// ---- §3 attested session -----------------------------------------------------

const SESSION_BYTES: &str = r#"{"api_version":"aci/1","channel_binding":[{"origin":"https://upstream.example.com","spki_sha256":"d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1","type":"tls_spki_sha256"}],"claims":{"extra":{"gpu_arch":"HOPPER","tcb_status":"UpToDate"},"gpu_attested":{"status":"unknown"},"model_weights_provenance":{"status":"unknown"},"os_known_good":{"status":"unknown"},"serving_software_known_good":{"status":"unknown"},"tcb_up_to_date":{"status":"unknown"},"tee_attested":{"reason":"example quote verified","source":"hardware_proven","status":"asserted"}},"endpoint":"https://upstream.example.com","established_at":1750000000,"evidence":{"data":"data:text/plain;base64,ZXhhbXBsZS1ldmlkZW5jZQ==","digest":"sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d"},"expires_at":1750003600,"upstream_name":"demo-upstream","verifier_id":"example/1"}"#;
const SESSION_ID: &str = "95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f";

// ---- §4 receipt ----------------------------------------------------------------

const REQUEST_BODY: &[u8] =
    br#"{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}"#;
const RESPONSE_BODY: &[u8] = br#"{"choices":[],"id":"chatcmpl-123"}"#;

const SIGNING_INPUT: &str = r#"{"api_version":"aci/1","chat_id":"chatcmpl-123","endpoint":"/v1/chat/completions","event_log":[{"body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b","type":"request.received"},{"body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b","type":"request.forwarded"},{"model_id":"demo-model","required":true,"result":"verified","session_id":"95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f","type":"upstream.verified"},{"body_hash":"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61","type":"response.returned"}],"key_id":"receipt-1","method":"POST","model":"demo-model","receipt_id":"rcpt-0001","served_at":1750000000,"workload_keyset_digest":"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371"}"#;
const SIGNING_INPUT_SHA256: &str =
    "1bd328e6880a5a12b3915af95ea32111310e04ab9e21ac3d71ce268e33b965c9";
const SIGNATURE_HEX: &str = "d5b005e093bde3b577faf270b7184b09e169cacb0ecb206b103bd2581f997db03da616175454b063323a23ac1dc68f1ce506c2a6eba8aa0561d5e724f0b80c03";
const DOCUMENT_BYTES: &str = r#"{"api_version":"aci/1","chat_id":"chatcmpl-123","endpoint":"/v1/chat/completions","event_log":[{"body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b","type":"request.received"},{"body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b","type":"request.forwarded"},{"model_id":"demo-model","required":true,"result":"verified","session_id":"95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f","type":"upstream.verified"},{"body_hash":"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61","type":"response.returned"}],"key_id":"receipt-1","method":"POST","model":"demo-model","receipt_id":"rcpt-0001","served_at":1750000000,"signature":"d5b005e093bde3b577faf270b7184b09e169cacb0ecb206b103bd2581f997db03da616175454b063323a23ac1dc68f1ce506c2a6eba8aa0561d5e724f0b80c03","workload_keyset_digest":"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371"}"#;

fn vector_keyset() -> WorkloadKeyset {
    let e2ee_secret = x25519_secret_key_from_bytes(&E2EE_SEED).unwrap();
    WorkloadKeyset {
        subject: Some("dstack-app://example-app".to_string()),
        not_after: 1_800_000_000,
        receipt_signing_keys: vec![KeyedPublicKey {
            key_id: "receipt-1".to_string(),
            algo: "ed25519".to_string(),
            public_key_hex: hex::encode(
                Ed25519SigningKey::from_bytes(&RECEIPT_SEED)
                    .verifying_key()
                    .as_bytes(),
            ),
        }],
        e2ee_public_keys: vec![KeyedPublicKey {
            key_id: "e2ee-1".to_string(),
            algo: E2EE_ALGO_X25519_AESGCM.to_string(),
            public_key_hex: x25519_public_key_hex(&e2ee_secret),
        }],
        tls_public_keys: vec![TlsSpki {
            spki_sha256_hex: "c0".repeat(32),
            domain: Some("api.example.com".to_string()),
        }],
    }
}

#[test]
fn keyset_seals_to_the_published_bytes_and_digest() {
    let sealed = SealedWorkloadKeyset::seal(vector_keyset()).unwrap();
    assert_eq!(
        std::str::from_utf8(sealed.bytes()).unwrap(),
        KEYSET_BYTES,
        "keyset wire bytes"
    );
    assert_eq!(sealed.digest(), KEYSET_DIGEST);
    // The derived public keys match the published fixed keys.
    assert_eq!(
        sealed.keyset().receipt_signing_keys[0].public_key_hex,
        RECEIPT_PUB
    );
    assert_eq!(sealed.keyset().e2ee_public_keys[0].public_key_hex, E2EE_PUB);
}

#[test]
fn statement_bytes_and_report_data_match_for_both_nonce_forms() {
    let with_nonce = attestation_statement(
        KEYSET_DIGEST,
        Some("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&with_nonce).unwrap(),
        STATEMENT_TEST_NONCE
    );
    assert_eq!(
        hex::encode(report_data(&with_nonce)),
        REPORT_DATA_TEST_NONCE
    );
    assert_eq!(
        hex::encode(report_data_slot(report_data(&with_nonce))),
        REPORT_DATA_SLOT
    );

    let without_nonce = attestation_statement(KEYSET_DIGEST, None).unwrap();
    assert_eq!(std::str::from_utf8(&without_nonce).unwrap(), STATEMENT_NULL);
    assert_eq!(hex::encode(report_data(&without_nonce)), REPORT_DATA_NULL);
}

fn vector_session_document() -> SessionDocument {
    let evidence = b"example-evidence";
    SessionDocument {
        api_version: "aci/1".to_string(),
        upstream_name: "demo-upstream".to_string(),
        endpoint: Some("https://upstream.example.com".to_string()),
        verifier_id: "example/1".to_string(),
        established_at: 1_750_000_000,
        expires_at: 1_750_003_600,
        identity: None,
        channel_binding: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://upstream.example.com".to_string(),
            spki_sha256: "d1".repeat(32),
        }],
        claims: SessionClaims {
            tee_attested: Claim::asserted(ClaimSource::HardwareProven, "example quote verified"),
            extra: [
                ("gpu_arch".to_string(), "HOPPER".into()),
                ("tcb_status".to_string(), "UpToDate".into()),
            ]
            .into_iter()
            .collect(),
            ..SessionClaims::default()
        },
        evidence: EvidenceRef {
            digest: Some(sha256_hex(evidence)),
            data_uri: Some(format!(
                "data:text/plain;base64,{}",
                BASE64.encode(evidence)
            )),
        },
    }
}

#[test]
fn session_document_seals_to_the_published_bytes_and_id() {
    let session = AttestedSession::seal(vector_session_document()).unwrap();
    assert_eq!(
        std::str::from_utf8(session.bytes()).unwrap(),
        SESSION_BYTES,
        "session wire bytes"
    );
    assert_eq!(session.session_id(), SESSION_ID);
    // Adopting the published bytes reproduces the same id.
    let adopted = AttestedSession::from_bytes(SESSION_BYTES.as_bytes().to_vec()).unwrap();
    assert_eq!(adopted.session_id(), SESSION_ID);
}

/// spec/proxy-image-binding.md §6: the §3 session with that binding.
#[test]
fn proxy_image_binding_session_matches_the_published_id() {
    let mut document = vector_session_document();
    document.channel_binding = vec![ChannelBinding::ProxyImageSha256 {
        provider: "privatemode".to_string(),
        proxy_image_digest: format!("sha256:{}", "e1".repeat(32)),
        credential_sha256: "f2".repeat(32),
    }];
    let session = AttestedSession::seal(document).unwrap();
    assert_eq!(
        session.session_id(),
        "c9e76e6247b2bf28ce034b7c3b4866b9fe9ce9177e9858ad999503e63ed4f0df"
    );
}

/// Minimal provider over the pinned receipt key; production custody rules
/// (§3.3) do not apply to published test vectors.
struct VectorKeys;

impl KeyProvider for VectorKeys {
    fn receipt_keys(&self) -> Vec<KeyedPublicKey> {
        vec![KeyedPublicKey {
            key_id: "receipt-1".to_string(),
            algo: "ed25519".to_string(),
            public_key_hex: RECEIPT_PUB.to_string(),
        }]
    }

    fn sign_receipt(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, KeyError> {
        assert_eq!(key_id, "receipt-1");
        Ok(Ed25519SigningKey::from_bytes(&RECEIPT_SEED)
            .sign(payload)
            .to_bytes()
            .to_vec())
    }

    fn e2ee_keys(&self) -> Vec<KeyedPublicKey> {
        Vec::new()
    }

    fn tls_spkis(&self) -> Vec<TlsSpki> {
        Vec::new()
    }

    fn is_test_only(&self) -> bool {
        true
    }
}

#[test]
fn receipt_payload_signature_and_envelope_match_the_published_vector() {
    let mut builder = ReceiptBuilder::new(
        "rcpt-0001".to_string(),
        Some("chatcmpl-123".to_string()),
        Some("demo-model".to_string()),
        KEYSET_DIGEST.to_string(),
        "/v1/chat/completions".to_string(),
        "POST".to_string(),
        1_750_000_000,
    );
    builder.add_request_received(REQUEST_BODY).unwrap();
    builder.add_request_forwarded(REQUEST_BODY).unwrap();
    builder
        .add_upstream_verified_with_session(
            &UpstreamVerifiedEvent {
                upstream_name: "demo-upstream".to_string(),
                model_id: "demo-model".to_string(),
                verifier_id: "example/1".to_string(),
                result: VerificationResult::Verified,
                required: true,
                ..Default::default()
            },
            SESSION_ID,
        )
        .unwrap();
    builder.add_response_returned(RESPONSE_BODY).unwrap();
    let receipt = builder.finalize(&VectorKeys, "receipt-1").unwrap();

    assert_eq!(
        std::str::from_utf8(&receipt.document).unwrap(),
        DOCUMENT_BYTES,
        "receipt document bytes (JCS)"
    );
    assert_eq!(receipt.signature_hex, SIGNATURE_HEX);

    // §9.2: the signature verifies over JCS(document minus `signature`)
    // under the keyset entry — from any encoding of the document.
    let document = receipt.document_json().unwrap();
    let signing_input = receipt_signing_input(&document).unwrap();
    assert_eq!(std::str::from_utf8(&signing_input).unwrap(), SIGNING_INPUT);
    assert_eq!(
        sha256_hex(&signing_input),
        format!("sha256:{SIGNING_INPUT_SHA256}")
    );
    let signature = hex::decode(SIGNATURE_HEX).unwrap();
    assert!(verify_receipt_signature(
        &vector_keyset().receipt_signing_keys[0],
        &signing_input,
        &signature
    ));
}

// ---- E2EE v2.1 (e2ee-v2-test-vectors.md, "Version 2.1") ---------------------

const V21_CLIENT_SEED: [u8; 32] = [0x04; 32];
const V21_CLIENT_PUB: &str = "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b";
const V21_REQUEST_SECRET: [u8; 32] = [0x0a; 32];
const V21_NONCE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const V21_FIELD_AAD: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256-v2.1","field":"messages.0.content","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.request.v2.1","ts":1750000000}"#;
const V21_FIELD_CIPHERTEXT: &str = "50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d0606060606060606060606067c3f905a1e58cb237be461eb622de6935599cbae71e1e1";
const V21_BODY: &str = r#"{"model":"demo-model","temperature":0.7,"messages":[{"role":"user","content":"50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d0606060606060606060606067c3f905a1e58cb237be461eb622de6935599cbae71e1e1"}]}"#;
const V21_BODY_JCS: &str = r#"{"messages":[{"content":"50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d0606060606060606060606067c3f905a1e58cb237be461eb622de6935599cbae71e1e1","role":"user"}],"model":"demo-model","temperature":0.7}"#;
const V21_BODY_DIGEST: &str = "dc75693896a5b40b45592667704f41ff057a5063c92e3117d930b98049032da5";
const V21_REQUEST_KEY_AAD: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256-v2.1","client_pub_key":"ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.request-key.v2.1","ts":1750000000}"#;
const V21_REQUEST_KEY: &str = "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d080808080808080808080808d4c43624763b8e0079b38271adfa9fae2e78243bb0fa918cd95421605c3b82b5bd7ac017cda0e8ac1c9836d409013e2369dd90e254dbbe44006ff09d098c5f212ff666088c3724c5c51b43e51576c633";
const V21_RESPONSE_AAD: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256-v2.1","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.response.v2.1","ts":1750000000}"#;
const V21_RESPONSE_CIPHERTEXT: &str = "57db4b359f23ae5e146e4e2512056704722506348c150c14753d0c933d04d4210b0b0b0b0b0b0b0b0b0b0b0b2db692c9ddf499146c55c93bff5841a9a068773aca36860864a0c2f3f37f";

const V21_RESPONSE_OBJECT: &str = r#"{"choices":[{"index":0,"message":{"content":"57db4b359f23ae5e146e4e2512056704722506348c150c14753d0c933d04d4210b0b0b0b0b0b0b0b0b0b0b0b2db692c9ddf499146c55c93bff5841a9a068773aca36860864a0c2f3f37f","role":"assistant"}}],"id":"chatcmpl-123"}"#;
const V21_RESPONSE_MAC: &str = "85874235a2eb56bdcf4bf6dc7f52f0fab9b3edfd410274bb9aaf56b4dd620c28";

#[test]
fn e2ee_v2_1_vectors_open_with_the_library_constructions() {
    use private_ai_gateway::aci::e2ee::{decrypt_x25519, E2eeKdf, E2EE_ALGO_X25519_AESGCM_V2_1};

    let aad = |value: serde_json::Value| {
        String::from_utf8(private_ai_gateway::aci::digest::jcs_bytes(&value).unwrap()).unwrap()
    };
    assert_eq!(
        aad(serde_json::json!({
            "purpose": "aci.e2ee.request.v2.1", "algo": E2EE_ALGO_X25519_AESGCM_V2_1,
            "model": "demo-model", "field": "messages.0.content",
            "nonce": V21_NONCE, "ts": 1_750_000_000u64,
        })),
        V21_FIELD_AAD
    );
    assert_eq!(
        aad(serde_json::json!({
            "purpose": "aci.e2ee.request-key.v2.1", "algo": E2EE_ALGO_X25519_AESGCM_V2_1,
            "model": "demo-model", "client_pub_key": V21_CLIENT_PUB,
            "nonce": V21_NONCE, "ts": 1_750_000_000u64,
        })),
        V21_REQUEST_KEY_AAD
    );
    let client = x25519_secret_key_from_bytes(&V21_CLIENT_SEED).unwrap();
    assert_eq!(x25519_public_key_hex(&client), V21_CLIENT_PUB);

    // The body digest covers the JCS form, whatever order the client sent.
    let body: serde_json::Value = serde_json::from_str(V21_BODY).unwrap();
    let body_jcs = serde_json_canonicalizer::to_vec(&body).unwrap();
    assert_eq!(std::str::from_utf8(&body_jcs).unwrap(), V21_BODY_JCS);
    assert_eq!(sha256_hex(&body_jcs), format!("sha256:{V21_BODY_DIGEST}"));

    // The service opens the request key, then the field under its secret.
    let service = x25519_secret_key_from_bytes(&E2EE_SEED).unwrap();
    let opened = decrypt_x25519(
        &service,
        V21_REQUEST_KEY,
        V21_REQUEST_KEY_AAD.as_bytes(),
        E2eeKdf::V2_1RequestKey,
    )
    .unwrap();
    assert_eq!(&opened[..32], V21_REQUEST_SECRET.as_slice());
    assert_eq!(hex::encode(&opened[32..]), V21_BODY_DIGEST);
    let kdf = E2eeKdf::V2_1Field {
        request_secret: &V21_REQUEST_SECRET,
    };
    assert_eq!(
        decrypt_x25519(
            &service,
            V21_FIELD_CIPHERTEXT,
            V21_FIELD_AAD.as_bytes(),
            kdf
        )
        .unwrap(),
        br#""hello""#
    );

    // The client opens the reply with its private key and the same secret.
    assert_eq!(
        decrypt_x25519(
            &client,
            V21_RESPONSE_CIPHERTEXT,
            V21_RESPONSE_AAD.as_bytes(),
            kdf
        )
        .unwrap(),
        br#""plain-answer""#
    );

    // The response MAC is HMAC-SHA256 over the object's JCS form.
    let object: serde_json::Value = serde_json::from_str(V21_RESPONSE_OBJECT).unwrap();
    assert_eq!(
        hex::encode(private_ai_gateway::aci::e2ee::v2_1_response_mac(
            &V21_REQUEST_SECRET,
            &serde_json_canonicalizer::to_vec(&object).unwrap()
        )),
        V21_RESPONSE_MAC
    );
}

#[test]
fn e2ee_v2_1_stream_vector_matches_the_gateway_mac_and_opens_for_the_client() {
    use private_ai_gateway::aci::e2ee::{decrypt_x25519, v2_1_response_mac, E2eeKdf};
    let mac = |object: serde_json::Value| {
        hex::encode(v2_1_response_mac(
            &V21_REQUEST_SECRET,
            &serde_json_canonicalizer::to_vec(&object).unwrap(),
        ))
    };
    let delta = "97c3b10b4d6c133a78ea5dcc1cf6421d3f81ae37b1f628ce14ca6fce7730f3330d0d0d0d0d0d0d0d0d0d0d0d9ec4f191936e4e179f68a3bbe739537e639286fe5c6a6b";
    assert_eq!(
        mac(serde_json::json!({"id": "chatcmpl-123", "e2ee_seq": 0,
            "choices": [{"index": 0, "delta": {"content": delta}}]})),
        "5ebf698533dd276e7017daf135bde41e3bf351b5e49d08223260b5e03e57ec21"
    );
    assert_eq!(
        mac(serde_json::json!({"id": "chatcmpl-123", "e2ee_seq": 1,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})),
        "7d11e2d261802a4a90f15a70727e541c1972168c6e053dd7ef5d4e6fe3b5cd7c"
    );
    let aad = private_ai_gateway::aci::digest::jcs_bytes(&serde_json::json!({
        "purpose": "aci.e2ee.response.v2.1", "algo": "x25519-aes-256-gcm-hkdf-sha256-v2.1",
        "model": "demo-model", "id": "chatcmpl-123", "field": "choices.0.delta.content",
        "nonce": V21_NONCE, "ts": 1_750_000_000u64,
    }))
    .unwrap();
    let client = x25519_secret_key_from_bytes(&V21_CLIENT_SEED).unwrap();
    let kdf = E2eeKdf::V2_1Field {
        request_secret: &V21_REQUEST_SECRET,
    };
    assert_eq!(
        decrypt_x25519(&client, delta, &aad, kdf).unwrap(),
        br#""Hello""#
    );
}
