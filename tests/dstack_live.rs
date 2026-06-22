//! Live dstack SDK smoke tests.
//!
//! Run with:
//! `PRIVATE_AI_GATEWAY_DSTACK_TEST_ENDPOINT=unix:/tmp/aci-dstack-sock-dev.dstack.sock cargo test dstack_live -- --ignored`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use private_ai_gateway::aci::canonical::sha256_hex;
use private_ai_gateway::aci::identity;
use private_ai_gateway::aci::keys::{
    verify_keyset_endorsement, verify_receipt_signature, KeyProvider, Quoter, ALGO_ECDSA_SECP256K1,
};
use private_ai_gateway::aci::receipt::{
    canonical_bytes_for_signing, EVENT_REQUEST_RECEIVED, EVENT_RESPONSE_RETURNED,
};
use private_ai_gateway::aci::types::{KeysetEpoch, ServiceCapabilities, SourceProvenance, TlsSpki};
use private_ai_gateway::aci::upstream::{
    UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aci::verifier::{
    AciDcapUpstreamVerifier, AciDcapVerifierPolicy, PreverifiedUpstreamVerifier,
};
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, ChatCompletionRequest, FixedClock, GatewayRequestContext,
    InMemoryReceiptStore, SystemClock, UpstreamVerificationRequest, UpstreamVerifier,
    CHAT_COMPLETIONS_PATH,
};
use private_ai_gateway::dstack::{DstackAciProvider, DstackAciProviderConfig};
use private_ai_gateway::http::build_router;
use serde::Deserialize;
use sha3::{Digest as Sha3Digest, Keccak256};

const CHAT_REQUEST: &[u8] =
    br#"{"model":"live-model","messages":[{"role":"user","content":"hello"}]}"#;
const CHAT_RESPONSE: &[u8] =
    br#"{"id":"chat-live-1","object":"chat.completion","model":"live-model","choices":[{"index":0,"message":{"role":"assistant","content":"world"},"finish_reason":"stop"}]}"#;

fn endpoint() -> String {
    std::env::var("PRIVATE_AI_GATEWAY_DSTACK_TEST_ENDPOINT")
        .expect("set PRIVATE_AI_GATEWAY_DSTACK_TEST_ENDPOINT=unix:/path/to/forwarded/dstack.sock")
}

struct LiveStubUpstream {
    calls: Arc<Mutex<Vec<UpstreamRequest>>>,
}

impl LiveStubUpstream {
    fn new() -> (Self, Arc<Mutex<Vec<UpstreamRequest>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                calls: calls.clone(),
            },
            calls,
        )
    }
}

#[async_trait]
impl UpstreamBackend for LiveStubUpstream {
    fn name(&self) -> &str {
        "live-stub-upstream"
    }

    fn url_origin(&self) -> Option<&str> {
        Some("https://live-stub-upstream.example")
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.calls.lock().unwrap().push(req);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamResponse {
            status_code: 200,
            body: CHAT_RESPONSE.to_vec(),
            headers,
            served_instance_id: None,
        })
    }
}

fn event<'a>(
    receipt: &'a private_ai_gateway::aci::types::Receipt,
    event_type: &str,
) -> &'a serde_json::Value {
    &receipt
        .event_log
        .iter()
        .find(|event| event.event_type == event_type)
        .unwrap()
        .fields
}

#[derive(Deserialize)]
struct DstackEvent {
    imr: u32,
    event: String,
    event_payload: String,
}

fn app_id_from_event_log(event_log: &str) -> Vec<u8> {
    let events = serde_json::from_str::<Vec<DstackEvent>>(event_log).unwrap();
    let app_id = events
        .iter()
        .take_while(|event| !(event.imr == 3 && event.event == "system-ready"))
        .find(|event| event.imr == 3 && event.event == "app-id")
        .unwrap();
    hex::decode(&app_id.event_payload).unwrap()
}

fn compressed_k256_public_key_hex(public_key_hex: &str) -> String {
    let public_key =
        hex::decode(public_key_hex.strip_prefix("0x").unwrap_or(public_key_hex)).unwrap();
    let point = EncodedPoint::from_bytes(public_key).unwrap();
    let key = K256VerifyingKey::from_encoded_point(&point).unwrap();
    hex::encode(key.to_sec1_bytes())
}

fn recover_k256_public_key(message: &[u8], signature_hex: &str) -> K256VerifyingKey {
    let signature = hex::decode(signature_hex).unwrap();
    assert_eq!(signature.len(), 65);
    let mut recovery_byte = signature[64];
    if (27..=30).contains(&recovery_byte) {
        recovery_byte -= 27;
    }
    let recid = RecoveryId::from_byte(recovery_byte).unwrap();
    let sig = K256Signature::from_slice(&signature[..64]).unwrap();
    let digest = Keccak256::new_with_prefix(message);
    K256VerifyingKey::recover_from_digest(digest, &sig, recid).unwrap()
}

fn accepted_kms_root_from_provider(provider: &DstackAciProvider, app_id: &[u8]) -> String {
    let evidence = provider.key_custody_evidence();
    let keys = evidence["keys"].as_array().unwrap();
    let identity = keys.iter().find(|key| key["role"] == "identity").unwrap();
    let purpose = identity["purpose"].as_str().unwrap();
    let public_key = identity["public_key"].as_str().unwrap();
    let signature_chain = identity["signature_chain"].as_array().unwrap();
    assert_eq!(signature_chain.len(), 2);

    let purpose_message = format!("{purpose}:{}", compressed_k256_public_key_hex(public_key));
    let app_public_key = recover_k256_public_key(
        purpose_message.as_bytes(),
        signature_chain[0].as_str().unwrap(),
    );
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id,
        &app_public_key.to_sec1_bytes(),
    ]
    .concat();
    let root_public_key =
        recover_k256_public_key(&root_message, signature_chain[1].as_str().unwrap());
    hex::encode(root_public_key.to_sec1_bytes())
}

async fn serve_aci_report(service: Arc<AciService>) -> String {
    let app: Router = build_router(service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
#[ignore = "requires PRIVATE_AI_GATEWAY_DSTACK_TEST_ENDPOINT=unix:/path/to/forwarded/dstack.sock"]
async fn dstack_live_provider_loads_kms_keys_and_quote() {
    let provider = DstackAciProvider::new(Some(endpoint()), DstackAciProviderConfig::default())
        .await
        .unwrap();

    let identity = provider.identity_public_key();
    assert_eq!(identity.algo, ALGO_ECDSA_SECP256K1);
    assert_eq!(identity.public_key_hex.len(), 130);

    let receipt_keys = provider.receipt_keys();
    assert_eq!(receipt_keys.len(), 1);
    assert_eq!(receipt_keys[0].algo, ALGO_ECDSA_SECP256K1);
    assert_eq!(receipt_keys[0].public_key_hex.len(), 130);

    let e2ee_keys = provider.e2ee_keys();
    assert_eq!(e2ee_keys.len(), 3);
    assert_eq!(
        e2ee_keys[0].algo,
        private_ai_gateway::aci::e2ee::E2EE_ALGO_SECP256K1_AESGCM
    );
    assert_eq!(e2ee_keys[0].public_key_hex.len(), 130);
    assert_eq!(e2ee_keys[1].algo, "ecdsa");
    assert_eq!(e2ee_keys[1].public_key_hex.len(), 128);
    assert_eq!(e2ee_keys[2].algo, "ed25519");
    assert_eq!(e2ee_keys[2].public_key_hex.len(), 64);

    let evidence = provider.key_custody_evidence();
    assert_eq!(evidence["provider"], "dstack-kms");
    assert_eq!(evidence["keys"].as_array().unwrap().len(), 4);
    assert!(evidence["keys"][0]["signature_chain"]
        .as_array()
        .is_some_and(|chain| !chain.is_empty()));

    let aci_report_data = [0xa5u8; 32];
    let quote = provider.get_quote(aci_report_data).await.unwrap();
    assert_eq!(
        quote.report_data,
        DstackAciProvider::dstack_report_data(aci_report_data)
    );
    assert!(quote.raw_quote.len() > 1024);
    assert!(quote.event_log.as_str().is_some_and(|s| !s.is_empty()));
    assert!(quote.vm_config.as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
#[ignore = "requires PRIVATE_AI_GATEWAY_DSTACK_TEST_ENDPOINT=unix:/path/to/forwarded/dstack.sock"]
async fn dstack_live_provider_keys_are_stable_for_same_paths() {
    let a = DstackAciProvider::new(Some(endpoint()), DstackAciProviderConfig::default())
        .await
        .unwrap();
    let b = DstackAciProvider::new(Some(endpoint()), DstackAciProviderConfig::default())
        .await
        .unwrap();

    assert_eq!(a.identity_public_key(), b.identity_public_key());
    assert_eq!(a.receipt_keys(), b.receipt_keys());
    assert_eq!(a.e2ee_keys(), b.e2ee_keys());
}

#[tokio::test]
#[ignore = "requires PRIVATE_AI_GATEWAY_DSTACK_TEST_ENDPOINT=unix:/path/to/forwarded/dstack.sock"]
async fn dstack_live_aci_report_and_receipt_chain_verify() {
    let provider = Arc::new(
        DstackAciProvider::new(Some(endpoint()), DstackAciProviderConfig::default())
            .await
            .unwrap(),
    );
    let keys: Arc<dyn KeyProvider> = provider.clone();
    let quoter: Arc<dyn Quoter> = provider;
    let (upstream, upstream_calls) = LiveStubUpstream::new();

    let mut cfg = AciServiceConfig::for_test("private-ai-gateway");
    cfg.vendor = "phala".to_string();
    cfg.tee_type = "tdx".to_string();
    cfg.source_provenance = SourceProvenance {
        repo_url: Some("https://github.com/Dstack-TEE/private-ai-gateway".to_string()),
        repo_commit: Some("live-test".to_string()),
        image_digest: None,
        image_provenance: None,
    };
    cfg.keyset_epoch = KeysetEpoch {
        version: 1,
        not_after: u64::MAX,
    };
    cfg.service_capabilities = ServiceCapabilities {
        supported_e2ee_versions: vec!["2".to_string()],
    };
    cfg.allow_test_keys = false;

    let service = AciService::new_with_upstream_verifier(
        keys,
        quoter,
        Arc::new(upstream),
        Arc::new(PreverifiedUpstreamVerifier::new("live-preverified/v1")),
        Arc::new(InMemoryReceiptStore::default()),
        cfg,
        Arc::new(FixedClock(1_700_000_000)),
    )
    .unwrap();

    let report = service
        .attestation_report(Some("live nonce".to_string()))
        .await
        .unwrap();

    assert_eq!(report.attestation.vendor, "phala");
    assert_eq!(report.attestation.tee_type, "tdx");
    assert_ne!(report.attestation.evidence["vm_config"]["stub"], true);

    let endorsement_payload =
        identity::keyset_endorsement_payload(&report.attestation.workload_keyset).unwrap();
    let endorsement_sig = hex::decode(&report.attestation.keyset_endorsement.value_hex).unwrap();
    assert!(verify_keyset_endorsement(
        &report
            .attestation
            .workload_keyset
            .workload_identity
            .public_key,
        &endorsement_payload,
        &endorsement_sig
    ));

    let statement = identity::attestation_statement(
        &report.attestation.workload_keyset,
        Some("live nonce".to_string()),
    )
    .unwrap();
    let aci_report_data = identity::report_data(&statement).unwrap();
    assert_eq!(
        report.attestation.report_data_hex,
        hex::encode(aci_report_data)
    );

    let quote_report_data = hex::decode(
        report.attestation.evidence["quote_report_data"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        quote_report_data,
        DstackAciProvider::dstack_report_data(aci_report_data)
    );
    assert!(report.attestation.evidence["quote"]
        .as_str()
        .is_some_and(|quote| quote.len() > 2048));

    let result = service
        .forward_chat_completion_request(ChatCompletionRequest {
            context: GatewayRequestContext::default(),
            endpoint_path: CHAT_COMPLETIONS_PATH,
            received_body: CHAT_REQUEST,
            forwarded_body: None,
            upstream_required: Some(true),
            upstream_verification_event: None,
            requester: None,
            e2ee: None,
        })
        .await
        .unwrap();

    assert_eq!(result.receipt.workload_id, report.workload_id);
    assert_eq!(
        result.receipt.workload_keyset_digest,
        report.workload_keyset_digest
    );
    assert_eq!(
        event(&result.receipt, EVENT_REQUEST_RECEIVED)["body_hash"],
        sha256_hex(CHAT_REQUEST)
    );
    assert_eq!(
        event(&result.receipt, EVENT_RESPONSE_RETURNED)["wire_hash"],
        sha256_hex(CHAT_RESPONSE)
    );
    assert_eq!(upstream_calls.lock().unwrap().len(), 1);

    let receipt_key = report
        .attestation
        .workload_keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == result.receipt.signature.key_id)
        .unwrap();
    let canonical_bytes = canonical_bytes_for_signing(&result.receipt).unwrap();
    let receipt_sig = hex::decode(&result.receipt.signature.value_hex).unwrap();
    assert!(verify_receipt_signature(
        receipt_key,
        &canonical_bytes,
        &receipt_sig
    ));
}

#[tokio::test]
#[ignore = "requires PRIVATE_AI_GATEWAY_DSTACK_TEST_ENDPOINT plus live PCCS/DCAP collateral access"]
async fn dstack_live_aci_dcap_upstream_verifier_accepts_real_aci_service() {
    let provider = Arc::new(
        DstackAciProvider::new(Some(endpoint()), DstackAciProviderConfig::default())
            .await
            .unwrap(),
    );
    let app_id_quote = provider.get_quote([0u8; 32]).await.unwrap();
    let event_log = app_id_quote.event_log.as_str().unwrap();
    let app_id = app_id_from_event_log(event_log);
    let accepted_kms_root = accepted_kms_root_from_provider(&provider, &app_id);

    let keys: Arc<dyn KeyProvider> = provider.clone();
    let quoter: Arc<dyn Quoter> = provider;
    let (upstream, _upstream_calls) = LiveStubUpstream::new();

    let mut cfg = AciServiceConfig::for_test("phala");
    cfg.allow_test_keys = false;
    cfg.source_provenance = SourceProvenance {
        repo_url: Some("https://github.com/Dstack-TEE/private-ai-gateway".to_string()),
        repo_commit: Some("live-test".to_string()),
        image_digest: None,
        image_provenance: None,
    };
    cfg.tls_public_keys = Some(vec![TlsSpki {
        domain: None,
        spki_sha256_hex: "aa".repeat(32),
    }]);

    let service = Arc::new(
        AciService::new(
            keys,
            quoter,
            Arc::new(upstream),
            Arc::new(InMemoryReceiptStore::default()),
            cfg,
            Arc::new(SystemClock),
        )
        .unwrap(),
    );
    let base_url = serve_aci_report(service.clone()).await;
    let policy = AciDcapVerifierPolicy::new(
        vec![service.workload_id().to_string()],
        Vec::new(),
        vec![accepted_kms_root],
    )
    .unwrap();
    let verifier = AciDcapUpstreamVerifier::with_default_pccs(&base_url, policy, 300).unwrap();

    let event = verifier
        .verify(UpstreamVerificationRequest {
            upstream_name: "live-aci-service".to_string(),
            url_origin: Some("http://127.0.0.1".to_string()),
            model_id: "live-model".to_string(),
            forwarded_body_hash: sha256_hex(CHAT_REQUEST),
            required: true,
        })
        .await;

    assert_eq!(
        event.result,
        private_ai_gateway::aci::receipt::VerificationResult::Verified,
        "{:?}",
        event.reason
    );
    assert_eq!(event.verifier_id, "aci-dcap/v1");
    assert!(event.evidence.is_some());
    assert_eq!(
        event.channel_bindings,
        vec![
            private_ai_gateway::aci::receipt::ChannelBinding::TlsSpkiSha256 {
                origin: base_url,
                spki_sha256: "aa".repeat(32),
            }
        ]
    );
}
