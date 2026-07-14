//! Byte-exact cross-check against `spec/test-vectors.md`.
//!
//! Every constant here is a published vector; the tests rebuild each artifact
//! through the library's own constructions (fixed seeds, no randomness) and
//! assert the exact bytes, digests, and signatures. If any construction in the
//! implementation drifts from the spec, one of these tests goes red.
//! `spec/tools/gen-vectors.py` reproduces the same values independently.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey as Ed25519SigningKey};
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::e2ee::{
    e2ee_aad, unseal_v3, x25519_public_key_hex, x25519_secret_key_from_bytes,
    E2EE_ALGO_X25519_AESGCM, E2EE_CONTEXT_REQUEST, E2EE_CONTEXT_RESPONSE,
};
use private_ai_gateway::aci::identity::{
    attestation_statement, report_data, report_data_slot, SealedWorkloadKeyset,
};
use private_ai_gateway::aci::keys::{verify_receipt_signature, KeyError, KeyProvider};
use private_ai_gateway::aci::receipt::{
    ChannelBinding, ReceiptBuilder, UpstreamVerifiedEvent, VerificationResult,
};
use private_ai_gateway::aci::types::{KeyedPublicKey, TlsSpki, WorkloadKeyset};
use private_ai_gateway::aggregator::session::{
    AttestedSession, Claim, ClaimSource, EvidenceRef, SessionClaims, SessionDocument,
};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519SecretKey};

// ---- Fixed keys (test-vectors "Fixed keys") --------------------------------

const RECEIPT_SEED: [u8; 32] = [0x02; 32];
const E2EE_SEED: [u8; 32] = [0x03; 32];
const CLIENT_SEED: [u8; 32] = [0x04; 32];
const EPH_REQUEST_SEED: [u8; 32] = [0x05; 32];
const EPH_RESPONSE_SEED: [u8; 32] = [0x06; 32];
const EPH_SSE_SEED: [u8; 32] = [0x07; 32];

const RECEIPT_PUB: &str = "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394";
const E2EE_PUB: &str = "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22";
const CLIENT_PUB: &str = "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b";

// ---- §1 workload keyset ------------------------------------------------------

const KEYSET_BYTES: &str = r#"{"subject":"dstack-app://example-app","not_after":1800000000,"receipt_signing_keys":[{"key_id":"receipt-1","algo":"ed25519","public_key":"8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394"}],"e2ee_public_keys":[{"key_id":"e2ee-1","algo":"x25519-aes-256-gcm-hkdf-sha256","public_key":"5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22"}],"tls_public_keys":[{"spki_sha256":"c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0","domain":"api.example.com"}]}"#;
const KEYSET_DIGEST: &str =
    "sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4";

// ---- §2 attestation statement / report_data ---------------------------------

const STATEMENT_TEST_NONCE: &str = r#"{"keyset_digest":"sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4","nonce":"test-nonce","purpose":"aci.report_data.v1"}"#;
const STATEMENT_NULL: &str = r#"{"keyset_digest":"sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4","nonce":null,"purpose":"aci.report_data.v1"}"#;
const REPORT_DATA_TEST_NONCE: &str =
    "8b899aae55437dec4d1d0d435920e112aca2a74d17595eeb601a7764d901ea07";
const REPORT_DATA_NULL: &str = "a98b0e34ef2ce05cf7d3fd64d86889deaf6836b8aa4e5d8baa9dd437fea07987";
const REPORT_DATA_SLOT: &str = "8b899aae55437dec4d1d0d435920e112aca2a74d17595eeb601a7764d901ea070000000000000000000000000000000000000000000000000000000000000000";

// ---- §3 attested session -----------------------------------------------------

const SESSION_BYTES: &str = r#"{"api_version":"aci/1","upstream_name":"demo-upstream","endpoint":"https://upstream.example.com","verifier_id":"example/1","established_at":1750000000,"expires_at":1750003600,"channel_binding":[{"type":"tls_spki_sha256","origin":"https://upstream.example.com","spki_sha256":"d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1"}],"claims":{"tee_attested":{"status":"asserted","source":"hardware_proven","reason":"example quote verified"},"gpu_attested":{"status":"unknown"},"tcb_up_to_date":{"status":"unknown"},"os_known_good":{"status":"unknown"},"serving_software_known_good":{"status":"unknown"},"model_weights_provenance":{"status":"unknown"},"extra":{"gpu_arch":"HOPPER","tcb_status":"UpToDate"}},"evidence":{"digest":"sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d","data":"data:text/plain;base64,ZXhhbXBsZS1ldmlkZW5jZQ=="}}"#;
const SESSION_ID: &str = "sha256:a595d269728e15fe8236af46586fe84f220696c0d7d4e647eed36922b7b20cb6";

// ---- §4 receipt ----------------------------------------------------------------

const REQUEST_BODY: &[u8] =
    br#"{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}"#;
const RESPONSE_BODY: &[u8] = br#"{"choices":[],"id":"chatcmpl-123"}"#;

const PAYLOAD_BYTES: &str = r#"{"api_version":"aci/1","receipt_id":"rcpt-0001","chat_id":"chatcmpl-123","model":"demo-model","workload_keyset_digest":"sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4","endpoint":"/v1/chat/completions","method":"POST","served_at":1750000000,"event_log":[{"type":"request.received","body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b"},{"type":"request.forwarded","body_hash":"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b"},{"type":"upstream.verified","result":"verified","required":true,"model_id":"demo-model","session_id":"sha256:a595d269728e15fe8236af46586fe84f220696c0d7d4e647eed36922b7b20cb6"},{"type":"response.returned","body_hash":"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61"}]}"#;
const PAYLOAD_SHA256: &str = "5a04d7ce350a09a9faa4f32e5a21790cd1080a46239039538bac98c798dc2dab";
const SIGNATURE_HEX: &str = "b0b2c830be73d6b6ad9a90b75b9c347a930e6a918e6e4f70ad1c3ce0d3dbfe6789504be5f7d317d24ba9eb84cd8bf634d58e898de89baa7fc939abd12e1b7400";

// ---- §5 E2EE v3 -----------------------------------------------------------------

const REQUEST_AAD_HEX: &str = "6163692e653265652e76332e726571756573740064656d6f2d6d6f64656c0061633031623232303965383633353466623835333233376235646530663466616231336337666362663433336136316330313933363936313766656366313062";
const RESPONSE_AAD_HEX: &str = "6163692e653265652e76332e726573706f6e73650064656d6f2d6d6f64656c";

const REQUEST_GCM_NONCE: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const RESPONSE_GCM_NONCE: [u8; 12] = [16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27];
const SSE_GCM_NONCE: [u8; 12] = [32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43];

const REQUEST_SEALED_HEX: &str = "50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d000102030405060708090a0b3216caf5913e5ac1cc3abacc3d9e522872087b64b4588d1846266624899249a17d9ca9e5e730dfe417be0983116a9a855bac522183fbddab9080ed2e086c7aa2ee366c9a2891839ef26bc4f4fa783616408c";
const RESPONSE_SEALED_HEX: &str = "f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d68174557101112131415161718191a1b497499f8bc6bb890ecfd49d9e4e886161be04d5014796252171e8e2e67a06dd1c6e181a3c1e105c6762431c9971ed32c58c6";
const SSE_SEALED_HEX: &str = "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d202122232425262728292a2b97d48f43a339f977ac5b2808607cee21003fcabdeed440c426cc57465afc764e9da772ad74762ae683d37b6207561d7cb9ae62e12ecf12423ef76d2e375a9637578819a9b7e4f3a4967aed7ca5dfcade9dea53844d30f2288440d3ba21c2f1dade14576ede947ec0a48dffeaf7ead8380513129a79bf4b";

const SSE_EVENT_BODY: &[u8] = br#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"hi"}}]}"#;

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
    let with_nonce = attestation_statement(KEYSET_DIGEST, Some("test-nonce")).unwrap();
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

/// Minimal provider over the pinned receipt key; production custody rules
/// (§4.3) do not apply to published test vectors.
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
        std::str::from_utf8(&receipt.payload).unwrap(),
        PAYLOAD_BYTES,
        "receipt payload bytes"
    );
    assert_eq!(
        sha256_hex(&receipt.payload),
        format!("sha256:{PAYLOAD_SHA256}")
    );
    assert_eq!(receipt.signature_hex, SIGNATURE_HEX);

    let envelope = receipt.envelope();
    assert_eq!(envelope["payload_b64"], BASE64.encode(PAYLOAD_BYTES));
    assert_eq!(envelope["key_id"], "receipt-1");
    assert_eq!(envelope["algo"], "ed25519");
    assert_eq!(envelope["signature"], SIGNATURE_HEX);

    // §10.2: the signature verifies over the decoded payload bytes under the
    // keyset entry.
    let signature = hex::decode(SIGNATURE_HEX).unwrap();
    assert!(verify_receipt_signature(
        &vector_keyset().receipt_signing_keys[0],
        PAYLOAD_BYTES.as_bytes(),
        &signature
    ));
}

/// The §7.1 sealing with a pinned ephemeral key and GCM nonce — the same
/// construction as `seal_v3` with the randomness fixed, reproducing the
/// published sealed bytes exactly.
fn seal_v3_deterministic(
    recipient_public: &[u8; 32],
    context: &str,
    model: &str,
    client_public_key_hex: Option<&str>,
    plaintext: &[u8],
    ephemeral_seed: [u8; 32],
    gcm_nonce: [u8; 12],
) -> Vec<u8> {
    let ephemeral = X25519SecretKey::from(ephemeral_seed);
    let ephemeral_public = X25519PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&X25519PublicKey::from(*recipient_public));
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, shared.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(context.as_bytes(), &mut key).unwrap();
    let ciphertext = Aes256Gcm::new_from_slice(&key)
        .unwrap()
        .encrypt(
            &gcm_nonce.into(),
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad: &e2ee_aad(context, model, client_public_key_hex),
            },
        )
        .unwrap();
    let mut out = ephemeral_public.as_bytes().to_vec();
    out.extend_from_slice(&gcm_nonce);
    out.extend_from_slice(&ciphertext);
    out
}

fn public_of(seed: [u8; 32]) -> [u8; 32] {
    *X25519PublicKey::from(&X25519SecretKey::from(seed)).as_bytes()
}

#[test]
fn e2ee_aad_bytes_match_the_published_vectors() {
    // The request AAD binds the client key; the response AAD does not (§7.2).
    assert_eq!(
        hex::encode(e2ee_aad(
            E2EE_CONTEXT_REQUEST,
            "demo-model",
            Some(CLIENT_PUB)
        )),
        REQUEST_AAD_HEX
    );
    assert_eq!(
        hex::encode(e2ee_aad(E2EE_CONTEXT_RESPONSE, "demo-model", None)),
        RESPONSE_AAD_HEX
    );
}

#[test]
fn e2ee_request_seal_matches_and_the_service_key_unseals_it() {
    assert_eq!(hex::encode(public_of(CLIENT_SEED)), CLIENT_PUB);

    let sealed = seal_v3_deterministic(
        &public_of(E2EE_SEED),
        E2EE_CONTEXT_REQUEST,
        "demo-model",
        Some(CLIENT_PUB),
        REQUEST_BODY,
        EPH_REQUEST_SEED,
        REQUEST_GCM_NONCE,
    );
    assert_eq!(hex::encode(&sealed), REQUEST_SEALED_HEX);

    // The service-side unseal recovers the exact original client bytes and
    // enforces the AAD model and client-key bindings.
    let sk = x25519_secret_key_from_bytes(&E2EE_SEED).unwrap();
    let req = E2EE_CONTEXT_REQUEST;
    assert_eq!(
        unseal_v3(&sk, req, "demo-model", Some(CLIENT_PUB), &sealed).unwrap(),
        REQUEST_BODY
    );
    // A different envelope model fails AEAD authentication.
    assert!(unseal_v3(&sk, req, "other-model", Some(CLIENT_PUB), &sealed).is_err());
    // A replay that swaps X-Client-Pub-Key recomputes a different AAD, so the
    // sealed request no longer unseals (the confidentiality fix, §7.2).
    assert!(unseal_v3(&sk, req, "demo-model", Some(E2EE_PUB), &sealed).is_err());
}

#[test]
fn e2ee_response_and_sse_seals_match_and_the_client_key_unseals_them() {
    let client_public = public_of(CLIENT_SEED);
    let client_secret = x25519_secret_key_from_bytes(&CLIENT_SEED).unwrap();

    let resp = E2EE_CONTEXT_RESPONSE;
    let response_sealed = seal_v3_deterministic(
        &client_public,
        resp,
        "demo-model",
        None,
        RESPONSE_BODY,
        EPH_RESPONSE_SEED,
        RESPONSE_GCM_NONCE,
    );
    assert_eq!(hex::encode(&response_sealed), RESPONSE_SEALED_HEX);
    let opened = unseal_v3(&client_secret, resp, "demo-model", None, &response_sealed).unwrap();
    assert_eq!(opened, RESPONSE_BODY);

    let sse_sealed = seal_v3_deterministic(
        &client_public,
        resp,
        "demo-model",
        None,
        SSE_EVENT_BODY,
        EPH_SSE_SEED,
        SSE_GCM_NONCE,
    );
    assert_eq!(hex::encode(&sse_sealed), SSE_SEALED_HEX);
    let opened = unseal_v3(&client_secret, resp, "demo-model", None, &sse_sealed).unwrap();
    assert_eq!(opened, SSE_EVENT_BODY);

    // §7.3 streaming: plaintext SSE framing around the sealed payload, and a
    // plaintext [DONE] sentinel.
    let wire = format!(
        "data: {{\"sealed_b64\":\"{}\"}}\n\ndata: [DONE]\n\n",
        BASE64.encode(&sse_sealed)
    );
    assert!(wire.starts_with("data: {\"sealed_b64\":\"E75P6ury"));
}
