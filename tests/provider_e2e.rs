//! Provider adapter E2E tests.
//!
//! These tests cover the HTTP forwarding side of concrete provider
//! adapters. Provider-owned verifier scripts are covered by unit tests
//! in `aci::verifier`, where each supported provider has its own Rust
//! struct.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

mod common;

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit as AeadKeyInit},
    ChaCha20Poly1305, Nonce,
};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use ml_kem::{
    kem::{Decapsulate, Encapsulate, Kem, KeyExport, TryKeyInit},
    ml_kem_768::{
        Ciphertext as MlKemCiphertext768, DecapsulationKey as MlKemDecapsulationKey768,
        EncapsulationKey as MlKemEncapsulationKey768,
    },
    MlKem768,
};
use private_ai_gateway::aci::canonical::sha256_hex;
use private_ai_gateway::aci::receipt::{
    ChannelBinding, UpstreamVerifiedEvent, EVENT_REQUEST_FORWARDED, EVENT_UPSTREAM_VERIFIED,
};
use private_ai_gateway::aci::upstream::{
    ChutesProviderBackend, ChutesSessionStore, ChutesVerifiedDiscovery, ChutesVerifiedInstance,
    OpenAICompatibleBackend, UpstreamRequest,
};
use private_ai_gateway::aci::verifier::StaticUpstreamVerifier;
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::aggregator::upstream_config::{
    UpstreamConfig, UpstreamConfigManager, UpstreamProvider, UpstreamRuntimeOptions,
    UpstreamVerifierMode,
};
use private_ai_gateway::http::build_router;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::Digest;
use tower::ServiceExt;

use common::{verified_event, StaticKeyProvider, StubQuoter};

const CHAT_REQUEST: &[u8] =
    br#"{"model":"public-model","messages":[{"role":"user","content":"hello"}]}"#;
const PROVIDER_CHAT_REQUEST: &[u8] =
    br#"{"model":"provider-model","messages":[{"role":"user","content":"hello"}]}"#;
const CHAT_RESPONSE: &[u8] =
    br#"{"id":"chat-provider-1","object":"chat.completion","model":"provider-model","choices":[{"index":0,"message":{"role":"assistant","content":"world"},"finish_reason":"stop"}]}"#;
const EMBEDDINGS_REQUEST: &[u8] = br#"{"model":"public-embed","input":"the quick brown fox"}"#;
const EMBEDDINGS_RESPONSE: &[u8] =
    br#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}],"model":"provider-embed-model","usage":{"prompt_tokens":5,"total_tokens":5}}"#;
const STREAM_CHAT_REQUEST: &[u8] =
    br#"{"model":"provider-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
const STREAM_CHAT_RESPONSE_EVENT: &[u8] =
    br#"data: {"id":"chat-provider-1","object":"chat.completion.chunk","model":"provider-model","choices":[{"index":0,"delta":{"role":"assistant","content":"world"},"finish_reason":null}]}"#;
const CHUTES_CHUTE_ID: &str = "2ff25e81-4586-5ec8-b892-3a6f342693d7";
const CHUTES_INSTANCE_ID: &str = "instance-a";
const CHUTES_INSTANCE_ID_B: &str = "instance-b";
const CHUTES_NONCE: &str = "nonce-a";
const CHUTES_NONCE_B: &str = "nonce-b";
const CHUTES_NONCE_C: &str = "nonce-c";
const CHUTES_INSTANCE_B_NONCE: &str = "nonce-d";
const CHUTES_INSTANCE_B_NONCE_B: &str = "nonce-e";
const CHUTES_INSTANCE_B_NONCE_C: &str = "nonce-f";
const CHUTES_PREWARMED_NONCE: &str = "nonce-prewarmed";
const CHUTES_MLKEM_CT_SIZE: usize = 1088;
const CHUTES_INFO_REQ: &[u8] = b"e2e-req-v1";
const CHUTES_INFO_RESP: &[u8] = b"e2e-resp-v1";
const CHUTES_INFO_STREAM: &[u8] = b"e2e-stream-v1";

#[derive(Debug, Clone)]
struct ProviderCall {
    path: String,
    authorization: Option<String>,
    accept: Option<String>,
    content_type: Option<String>,
    x_chute_id: Option<String>,
    x_instance_id: Option<String>,
    x_e2e_nonce: Option<String>,
    x_e2e_stream: Option<String>,
    x_e2e_path: Option<String>,
    body: Vec<u8>,
    decrypted_body: Option<Value>,
}

#[derive(Clone)]
struct ProviderState {
    calls: Arc<Mutex<Vec<ProviderCall>>>,
}

async fn chat_handler(
    State(state): State<ProviderState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    state.calls.lock().unwrap().push(ProviderCall {
        path: "/v1/chat/completions".to_string(),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        accept: headers
            .get("accept")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        content_type: headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        x_chute_id: None,
        x_instance_id: None,
        x_e2e_nonce: None,
        x_e2e_stream: None,
        x_e2e_path: None,
        body: body.to_vec(),
        decrypted_body: None,
    });
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        CHAT_RESPONSE,
    )
}

async fn models_handler() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [{
            "id": "provider-model",
            "object": "model",
            "owned_by": "provider-fixture"
        }]
    }))
}

async fn embeddings_handler(
    State(state): State<ProviderState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    state.calls.lock().unwrap().push(ProviderCall {
        path: "/v1/embeddings".to_string(),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        accept: headers
            .get("accept")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        content_type: headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        x_chute_id: None,
        x_instance_id: None,
        x_e2e_nonce: None,
        x_e2e_stream: None,
        x_e2e_path: None,
        body: body.to_vec(),
        decrypted_body: None,
    });
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        EMBEDDINGS_RESPONSE,
    )
}

async fn serve_openai_provider_fixture() -> (String, Arc<Mutex<Vec<ProviderCall>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/embeddings", post(embeddings_handler))
        .route("/v1/models", get(models_handler))
        .with_state(ProviderState {
            calls: calls.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), calls)
}

#[derive(Clone)]
struct ChutesProviderState {
    calls: Arc<Mutex<Vec<ProviderCall>>>,
    instance_requests: Arc<Mutex<Vec<String>>>,
    instances: Arc<Vec<ChutesFixtureInstance>>,
    lookup_name: String,
}

struct ChutesFixtureInstance {
    instance_id: String,
    e2e_pubkey: String,
    e2e_secret_key: MlKemDecapsulationKey768,
    nonces: Vec<String>,
}

async fn chutes_models_handler() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [{
            "id": "provider-model",
            "object": "model",
            "chute_id": CHUTES_CHUTE_ID,
            "owned_by": "chutes-fixture"
        }]
    }))
}

async fn chutes_lookup_handler(
    Query(_query): Query<std::collections::HashMap<String, String>>,
    State(state): State<ChutesProviderState>,
) -> impl IntoResponse {
    Json(json!({
        "items": [{
            "name": state.lookup_name,
            "chute_id": CHUTES_CHUTE_ID
        }]
    }))
}

async fn chutes_instances_handler(
    Path(chute_id): Path<String>,
    State(state): State<ChutesProviderState>,
) -> impl IntoResponse {
    assert_eq!(chute_id, CHUTES_CHUTE_ID);
    state
        .instance_requests
        .lock()
        .unwrap()
        .push(chute_id.clone());
    let instances = state
        .instances
        .iter()
        .map(|instance| {
            json!({
                "instance_id": instance.instance_id,
                "e2e_pubkey": instance.e2e_pubkey,
                "nonces": instance.nonces
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "instances": instances,
        "nonce_expires_in": 55
    }))
}

async fn chutes_invoke_handler(
    State(state): State<ChutesProviderState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let instance_id = headers
        .get("x-instance-id")
        .and_then(|value| value.to_str().ok())
        .expect("Chutes E2EE request must include x-instance-id");
    let instance = state
        .instances
        .iter()
        .find(|candidate| candidate.instance_id == instance_id)
        .expect("fixture must receive a known Chutes instance id");
    let decrypted = decrypt_chutes_request_blob(&body, &instance.e2e_secret_key);
    state.calls.lock().unwrap().push(ProviderCall {
        path: "/e2e/invoke".to_string(),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        accept: headers
            .get("accept")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        content_type: headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        x_chute_id: headers
            .get("x-chute-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        x_instance_id: headers
            .get("x-instance-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        x_e2e_nonce: headers
            .get("x-e2e-nonce")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        x_e2e_stream: headers
            .get("x-e2e-stream")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        x_e2e_path: headers
            .get("x-e2e-path")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: body.to_vec(),
        decrypted_body: Some(decrypted.clone()),
    });
    let response_pk = decrypted
        .get("e2e_response_pk")
        .and_then(Value::as_str)
        .expect("Chutes E2EE request must include response key");
    let stream = headers
        .get("x-e2e-stream")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "true");
    if stream {
        (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            encrypt_chutes_stream_response(response_pk),
        )
    } else {
        (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            encrypt_chutes_response_blob(response_pk, CHAT_RESPONSE),
        )
    }
}

async fn serve_chutes_provider_fixture() -> (
    String,
    Arc<Mutex<Vec<ProviderCall>>>,
    String,
    Arc<Mutex<Vec<String>>>,
) {
    let (base_url, calls, e2e_pubkeys, instance_requests) =
        serve_chutes_provider_fixture_with_instances(vec![(
            CHUTES_INSTANCE_ID,
            vec![CHUTES_NONCE, CHUTES_NONCE_B, CHUTES_NONCE_C],
        )])
        .await;
    (
        base_url,
        calls,
        e2e_pubkeys.get(CHUTES_INSTANCE_ID).unwrap().clone(),
        instance_requests,
    )
}

async fn serve_chutes_provider_fixture_with_instances(
    instance_defs: Vec<(&'static str, Vec<&'static str>)>,
) -> (
    String,
    Arc<Mutex<Vec<ProviderCall>>>,
    BTreeMap<String, String>,
    Arc<Mutex<Vec<String>>>,
) {
    serve_chutes_provider_fixture_with_instances_and_lookup(instance_defs, "provider-model").await
}

async fn serve_chutes_provider_fixture_with_instances_and_lookup(
    instance_defs: Vec<(&'static str, Vec<&'static str>)>,
    lookup_name: &str,
) -> (
    String,
    Arc<Mutex<Vec<ProviderCall>>>,
    BTreeMap<String, String>,
    Arc<Mutex<Vec<String>>>,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let instance_requests = Arc::new(Mutex::new(Vec::new()));
    let mut e2e_pubkeys = BTreeMap::new();
    let instances = instance_defs
        .into_iter()
        .map(|(instance_id, nonces)| {
            let (e2e_secret_key, e2e_public_key) = MlKem768::generate_keypair();
            let e2e_pubkey = BASE64.encode(e2e_public_key.to_bytes().as_slice());
            e2e_pubkeys.insert(instance_id.to_string(), e2e_pubkey.clone());
            ChutesFixtureInstance {
                instance_id: instance_id.to_string(),
                e2e_pubkey,
                e2e_secret_key,
                nonces: nonces.into_iter().map(str::to_string).collect(),
            }
        })
        .collect::<Vec<_>>();
    let app = Router::new()
        .route("/v1/models", get(chutes_models_handler))
        .route("/chutes/", get(chutes_lookup_handler))
        .route("/e2e/instances/:chute_id", get(chutes_instances_handler))
        .route("/e2e/invoke", post(chutes_invoke_handler))
        .with_state(ChutesProviderState {
            calls: calls.clone(),
            instance_requests: instance_requests.clone(),
            instances: Arc::new(instances),
            lookup_name: lookup_name.to_string(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        format!("http://{addr}"),
        calls,
        e2e_pubkeys,
        instance_requests,
    )
}

fn temp_config_path() -> std::path::PathBuf {
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    std::env::temp_dir().join(format!(
        "private-ai-gateway-provider-e2e-{}-{}.json",
        std::process::id(),
        hex::encode(bytes)
    ))
}

fn runtime_options(mode: UpstreamVerifierMode) -> UpstreamRuntimeOptions {
    UpstreamRuntimeOptions {
        verifier_mode: mode,
        accepted_workload_ids: vec![],
        accepted_image_digests: vec![],
        accepted_dstack_kms_root_public_keys: vec![],
        pccs_url: None,
        verifier_cache_seconds: 300,
        connect_timeout_seconds: 10,
        read_timeout_seconds: 600,
        verifier_request_timeout_seconds: 60,
    }
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    body: impl Into<Vec<u8>>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.into()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

fn service_for_manager(manager: Arc<UpstreamConfigManager>) -> Arc<AciService> {
    Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            manager.backend(),
            manager.verifier(),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    )
}

fn receipt_event<'a>(
    receipt: &'a private_ai_gateway::aci::types::Receipt,
    event_type: &str,
) -> &'a Value {
    &receipt
        .event_log
        .iter()
        .find(|event| event.event_type == event_type)
        .unwrap()
        .fields
}

fn provider_evidence_fixture(name: &str) -> Value {
    json!({
        "digest": format!("sha256:{}", "11".repeat(32)),
        "data": format!(
            "data:application/json;base64,{}",
            BASE64.encode(format!(r#"{{"fixture":"{name}"}}"#).as_bytes())
        ),
    })
}

fn chutes_key_binding(e2e_pubkey: &str) -> private_ai_gateway::aci::receipt::ChannelBinding {
    chutes_key_binding_for(CHUTES_INSTANCE_ID, e2e_pubkey)
}

fn chutes_key_binding_for(
    instance_id: &str,
    e2e_pubkey: &str,
) -> private_ai_gateway::aci::receipt::ChannelBinding {
    let pubkey = BASE64.decode(e2e_pubkey).unwrap();
    private_ai_gateway::aci::receipt::ChannelBinding::E2eePublicKeySha256 {
        provider: "chutes".to_string(),
        key_id: Some(instance_id.to_string()),
        algorithm: "chutes-ml-kem-768".to_string(),
        public_key_sha256: hex::encode(sha2::Sha256::digest(&pubkey)),
    }
}

fn decrypt_chutes_request_blob(blob: &[u8], secret_key: &MlKemDecapsulationKey768) -> Value {
    let mlkem_ct = MlKemCiphertext768::try_from(&blob[..CHUTES_MLKEM_CT_SIZE]).unwrap();
    let nonce = &blob[CHUTES_MLKEM_CT_SIZE..CHUTES_MLKEM_CT_SIZE + 12];
    let ciphertext = &blob[CHUTES_MLKEM_CT_SIZE + 12..];
    let shared_secret = secret_key.decapsulate(&mlkem_ct);
    let key = chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_REQ,
    );
    let plaintext = chacha_decrypt(&key, nonce, ciphertext);
    serde_json::from_slice(&gzip_decompress(&plaintext)).unwrap()
}

fn encrypt_chutes_response_blob(response_pk_b64: &str, body: &[u8]) -> Vec<u8> {
    let response_pk = BASE64.decode(response_pk_b64).unwrap();
    let response_pk = MlKemEncapsulationKey768::new_from_slice(&response_pk).unwrap();
    let (mlkem_ct, shared_secret) = response_pk.encapsulate();
    let key = chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_RESP,
    );
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let encrypted = chacha_encrypt(&key, &nonce, &gzip_compress(body));
    let mut out = Vec::new();
    out.extend_from_slice(mlkem_ct.as_slice());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted);
    out
}

fn encrypt_chutes_stream_response(response_pk_b64: &str) -> Vec<u8> {
    let response_pk = BASE64.decode(response_pk_b64).unwrap();
    let response_pk = MlKemEncapsulationKey768::new_from_slice(&response_pk).unwrap();
    let (mlkem_ct, shared_secret) = response_pk.encapsulate();
    let key = chutes_derive_key(
        shared_secret.as_slice(),
        mlkem_ct.as_slice(),
        CHUTES_INFO_STREAM,
    );
    let encrypted = encrypt_chutes_stream_chunk(&key, STREAM_CHAT_RESPONSE_EVENT);
    format!(
        "data: {{\"e2e_init\":\"{}\"}}\n\ndata: {{\"e2e\":\"{}\"}}\n\ndata: [DONE]\n\n",
        BASE64.encode(mlkem_ct.as_slice()),
        BASE64.encode(encrypted)
    )
    .into_bytes()
}

fn encrypt_chutes_stream_chunk(key: &[u8], body: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let encrypted = chacha_encrypt(key, &nonce, body);
    let mut out = Vec::new();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted);
    out
}

fn chutes_derive_key(shared_secret: &[u8], mlkem_ct: &[u8], info: &[u8]) -> Vec<u8> {
    let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(Some(&mlkem_ct[..16]), shared_secret);
    let mut key = [0u8; 32];
    hkdf.expand(info, &mut key).unwrap();
    key.to_vec()
}

#[allow(deprecated)]
fn chacha_encrypt(key: &[u8], nonce: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).unwrap();
    cipher.encrypt(Nonce::from_slice(nonce), plaintext).unwrap()
}

#[allow(deprecated)]
fn chacha_decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).unwrap();
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .unwrap()
}

fn gzip_compress(plaintext: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(plaintext).unwrap();
    encoder.finish().unwrap()
}

fn gzip_decompress(compressed: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(compressed);
    let mut plaintext = Vec::new();
    decoder.read_to_end(&mut plaintext).unwrap();
    plaintext
}

#[tokio::test]
async fn openai_compatible_provider_supports_basic_auth_via_runtime_config() {
    let (base_url, provider_calls) = serve_openai_provider_fixture().await;
    let path = temp_config_path();
    let manager = Arc::new(
        UpstreamConfigManager::load(&path, runtime_options(UpstreamVerifierMode::Preverified))
            .unwrap(),
    );
    manager
        .replace(vec![UpstreamConfig {
            name: "private-chute".to_string(),
            provider: UpstreamProvider::OpenAiCompatible,
            base_url,
            path: None,
            models: BTreeMap::from([("public-model".to_string(), "provider-model".to_string())]),
            bearer_token: Some("scoped-credential".to_string()),
            authorization_scheme: Some("basic".to_string()),
            accepted_workload_ids: None,
            accepted_image_digests: None,
            accepted_dstack_kms_root_public_keys: None,
            pccs_url: None,
            verifier_cache_seconds: None,
            connect_timeout_seconds: None,
            read_timeout_seconds: None,
            verifier_request_timeout_seconds: None,
            verification_refresh_seconds: None,
            session_refresh_seconds: None,
            chutes_e2ee_api_base: None,
            chutes_chute_ids: None,
            chutes_e2ee_discovery_rounds: None,
            chutes_e2ee_discovery_interval_seconds: None,
        }])
        .unwrap();
    let app = build_router(service_for_manager(manager));

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some("Basic scoped-credential")
    );

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn openai_compatible_provider_e2e_via_runtime_config() {
    let (base_url, provider_calls) = serve_openai_provider_fixture().await;
    let path = temp_config_path();
    let manager = Arc::new(
        UpstreamConfigManager::load(&path, runtime_options(UpstreamVerifierMode::Preverified))
            .unwrap(),
    );
    manager
        .replace(vec![UpstreamConfig {
            name: "openai-compatible-provider".to_string(),
            provider: UpstreamProvider::OpenAiCompatible,
            base_url: base_url.clone(),
            path: None,
            models: BTreeMap::from([("public-model".to_string(), "provider-model".to_string())]),
            bearer_token: Some("provider-secret".to_string()),
            authorization_scheme: None,
            accepted_workload_ids: None,
            accepted_image_digests: None,
            accepted_dstack_kms_root_public_keys: None,
            pccs_url: None,
            verifier_cache_seconds: None,
            connect_timeout_seconds: None,
            read_timeout_seconds: None,
            verifier_request_timeout_seconds: None,
            verification_refresh_seconds: None,
            session_refresh_seconds: None,
            chutes_e2ee_api_base: None,
            chutes_chute_ids: None,
            chutes_e2ee_discovery_rounds: None,
            chutes_e2ee_discovery_interval_seconds: None,
        }])
        .unwrap();
    let service = service_for_manager(manager);
    let app = build_router(service.clone());

    let (models_status, _, models_body) = call(app.clone(), "GET", "/v1/models", Vec::new()).await;
    assert_eq!(models_status, StatusCode::OK);
    let models: Value = serde_json::from_slice(&models_body).unwrap();
    assert_eq!(models["data"][0]["id"], "public-model");

    let (status, headers, body) = call(app, "POST", "/v1/chat/completions", CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(body, CHAT_RESPONSE);
    let receipt_id = headers
        .get("x-receipt-id")
        .and_then(|value| value.to_str().ok())
        .expect("successful provider response must include x-receipt-id");

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/v1/chat/completions");
    assert_eq!(
        call.authorization.as_deref(),
        Some("Bearer provider-secret")
    );
    assert_eq!(call.accept.as_deref(), Some("application/json"));
    let forwarded: Value = serde_json::from_slice(&call.body).unwrap();
    assert_eq!(forwarded["model"], "provider-model");

    let receipt = service
        .get_receipt_by_receipt_id(receipt_id)
        .expect("provider E2E response must persist a receipt");
    assert_eq!(receipt.chat_id.as_deref(), Some("chat-provider-1"));
    assert_eq!(
        receipt_event(&receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
        sha256_hex(&call.body)
    );
    let upstream_verified = receipt_event(&receipt, EVENT_UPSTREAM_VERIFIED);
    assert_eq!(
        upstream_verified["upstream_name"],
        "openai-compatible-provider"
    );
    assert_eq!(upstream_verified["model_id"], "provider-model");
    assert_eq!(upstream_verified["url_origin"], base_url);
    assert_eq!(
        upstream_verified["verifier_id"],
        "preverified/out-of-band/v1"
    );
    assert_eq!(upstream_verified["result"], "verified");

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn openai_compatible_provider_routes_embeddings_via_runtime_config() {
    let (base_url, provider_calls) = serve_openai_provider_fixture().await;
    let path = temp_config_path();
    let manager = Arc::new(
        UpstreamConfigManager::load(&path, runtime_options(UpstreamVerifierMode::Preverified))
            .unwrap(),
    );
    manager
        .replace(vec![UpstreamConfig {
            name: "openai-compatible-provider".to_string(),
            provider: UpstreamProvider::OpenAiCompatible,
            base_url: base_url.clone(),
            path: None,
            models: BTreeMap::from([
                ("public-model".to_string(), "provider-model".to_string()),
                (
                    "public-embed".to_string(),
                    "provider-embed-model".to_string(),
                ),
            ]),
            bearer_token: Some("provider-secret".to_string()),
            authorization_scheme: None,
            accepted_workload_ids: None,
            accepted_image_digests: None,
            accepted_dstack_kms_root_public_keys: None,
            pccs_url: None,
            verifier_cache_seconds: None,
            connect_timeout_seconds: None,
            read_timeout_seconds: None,
            verifier_request_timeout_seconds: None,
            verification_refresh_seconds: None,
            session_refresh_seconds: None,
            chutes_e2ee_api_base: None,
            chutes_chute_ids: None,
            chutes_e2ee_discovery_rounds: None,
            chutes_e2ee_discovery_interval_seconds: None,
        }])
        .unwrap();
    let service = service_for_manager(manager);
    let app = build_router(service.clone());

    let (status, headers, body) = call(app, "POST", "/v1/embeddings", EMBEDDINGS_REQUEST).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(body, EMBEDDINGS_RESPONSE);
    let receipt_id = headers
        .get("x-receipt-id")
        .and_then(|value| value.to_str().ok())
        .expect("successful provider response must include x-receipt-id");

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/v1/embeddings");
    assert_eq!(
        call.authorization.as_deref(),
        Some("Bearer provider-secret")
    );
    assert_eq!(call.accept.as_deref(), Some("application/json"));
    let forwarded: Value = serde_json::from_slice(&call.body).unwrap();
    // Model alias rewritten to the upstream model id before
    // forwarding, identical to chat completions behavior.
    assert_eq!(forwarded["model"], "provider-embed-model");
    assert_eq!(forwarded["input"], "the quick brown fox");

    let receipt = service
        .get_receipt_by_receipt_id(receipt_id)
        .expect("provider embeddings response must persist a receipt");
    assert_eq!(receipt.endpoint, "/v1/embeddings");
    assert!(
        receipt.chat_id.is_none(),
        "embeddings responses have no id field; receipt chat_id should be empty"
    );
    assert_eq!(
        receipt_event(&receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
        sha256_hex(&call.body)
    );
    let upstream_verified = receipt_event(&receipt, EVENT_UPSTREAM_VERIFIED);
    assert_eq!(
        upstream_verified["upstream_name"],
        "openai-compatible-provider"
    );
    assert_eq!(upstream_verified["model_id"], "provider-embed-model");
    assert_eq!(upstream_verified["url_origin"], base_url);
    assert_eq!(
        upstream_verified["verifier_id"],
        "preverified/out-of-band/v1"
    );
    assert_eq!(upstream_verified["result"], "verified");

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn dynamic_runtime_config_delegates_verified_forwarding_to_selected_backend() {
    let (base_url, _provider_calls) = serve_openai_provider_fixture().await;
    let path = temp_config_path();
    let manager =
        UpstreamConfigManager::load(&path, runtime_options(UpstreamVerifierMode::None)).unwrap();
    manager
        .replace(vec![UpstreamConfig {
            name: "openai-compatible-provider".to_string(),
            provider: UpstreamProvider::OpenAiCompatible,
            base_url: base_url.clone(),
            path: None,
            models: BTreeMap::from([("public-model".to_string(), "provider-model".to_string())]),
            bearer_token: None,
            authorization_scheme: None,
            accepted_workload_ids: None,
            accepted_image_digests: None,
            accepted_dstack_kms_root_public_keys: None,
            pccs_url: None,
            verifier_cache_seconds: None,
            connect_timeout_seconds: None,
            read_timeout_seconds: None,
            verifier_request_timeout_seconds: None,
            verification_refresh_seconds: None,
            session_refresh_seconds: None,
            chutes_e2ee_api_base: None,
            chutes_chute_ids: None,
            chutes_e2ee_discovery_rounds: None,
            chutes_e2ee_discovery_interval_seconds: None,
        }])
        .unwrap();
    let backend = manager.backend();
    let prepared = backend
        .prepare(UpstreamRequest {
            body: CHAT_REQUEST.to_vec(),
            headers: Default::default(),
            path: None,
            target_route_id: None,
        })
        .unwrap();
    let event = UpstreamVerifiedEvent {
        url_origin: Some(base_url.clone()),
        verifier_id: "fixture-spki-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("attestation")),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: base_url,
            spki_sha256: "aa".repeat(32),
        }],
        ..verified_event("openai-compatible-provider", "provider-model")
    };

    let err = backend
        .forward_verified_prepared(prepared, &event)
        .await
        .expect_err("selected backend must enforce the verified binding");
    let err = err.to_string();
    assert!(
        err.contains("TLS channel binding requires an https upstream"),
        "{err}"
    );
    assert!(!err.contains("dynamic-upstream-config"), "{err}");

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn openai_compatible_provider_refuses_unenforceable_tls_binding() {
    let (base_url, provider_calls) = serve_openai_provider_fixture().await;
    let backend = OpenAICompatibleBackend::new(base_url.clone())
        .unwrap()
        .with_name("openai-compatible-provider");
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url.clone()),
        verifier_id: "fixture-spki-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("attestation")),
        channel_bindings: vec![
            private_ai_gateway::aci::receipt::ChannelBinding::TlsSpkiSha256 {
                origin: base_url,
                spki_sha256: "aa".repeat(32),
            },
        ],
        ..verified_event("openai-compatible-provider", "public-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["error"]["type"], "internal_error");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("TLS channel binding requires an https upstream"));
    assert!(provider_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn chutes_provider_uses_e2ee_transport_for_buffered_requests() {
    let (base_url, provider_calls, e2e_pubkey, _instance_requests) =
        serve_chutes_provider_fixture().await;
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone());
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![chutes_key_binding(&e2e_pubkey)],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", PROVIDER_CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(body, CHAT_RESPONSE);

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/e2e/invoke");
    assert_eq!(call.authorization.as_deref(), Some("Bearer chutes-secret"));
    assert_eq!(
        call.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(call.x_chute_id.as_deref(), Some(CHUTES_CHUTE_ID));
    assert_eq!(call.x_instance_id.as_deref(), Some(CHUTES_INSTANCE_ID));
    assert_eq!(call.x_e2e_nonce.as_deref(), Some(CHUTES_NONCE));
    assert_eq!(call.x_e2e_stream.as_deref(), Some("false"));
    assert_eq!(call.x_e2e_path.as_deref(), Some("/v1/chat/completions"));
    assert_ne!(call.body, PROVIDER_CHAT_REQUEST);
    assert_eq!(
        call.decrypted_body.as_ref().unwrap()["model"],
        json!("provider-model")
    );
    assert!(call.decrypted_body.as_ref().unwrap()["e2e_response_pk"].is_string());
}

#[tokio::test]
async fn chutes_provider_requires_exact_catalog_match() {
    let (base_url, provider_calls, e2e_pubkeys, instance_requests) =
        serve_chutes_provider_fixture_with_instances_and_lookup(
            vec![(CHUTES_INSTANCE_ID, vec![CHUTES_NONCE])],
            "different-provider-model",
        )
        .await;
    let e2e_pubkey = e2e_pubkeys.get(CHUTES_INSTANCE_ID).unwrap();
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone());
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![chutes_key_binding(e2e_pubkey)],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", PROVIDER_CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("exact chute_id match"));
    assert!(provider_calls.lock().unwrap().is_empty());
    assert!(instance_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn chutes_provider_uses_configured_chute_id_pin() {
    let (base_url, provider_calls, e2e_pubkeys, instance_requests) =
        serve_chutes_provider_fixture_with_instances_and_lookup(
            vec![(CHUTES_INSTANCE_ID, vec![CHUTES_NONCE])],
            "different-provider-model",
        )
        .await;
    let e2e_pubkey = e2e_pubkeys.get(CHUTES_INSTANCE_ID).unwrap();
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone())
        .with_chute_ids(BTreeMap::from([(
            "provider-model".to_string(),
            CHUTES_CHUTE_ID.to_string(),
        )]));
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![chutes_key_binding(e2e_pubkey)],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", PROVIDER_CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(body, CHAT_RESPONSE);

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].x_chute_id.as_deref(), Some(CHUTES_CHUTE_ID));
    assert_eq!(
        instance_requests.lock().unwrap().as_slice(),
        &[CHUTES_CHUTE_ID.to_string()]
    );
}

#[tokio::test]
async fn chutes_provider_pools_verified_single_use_nonces() {
    let (base_url, provider_calls, e2e_pubkey, instance_requests) =
        serve_chutes_provider_fixture().await;
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone());
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![chutes_key_binding(&e2e_pubkey)],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (first_status, _, first_body) = call(
        app.clone(),
        "POST",
        "/v1/chat/completions",
        PROVIDER_CHAT_REQUEST,
    )
    .await;
    assert_eq!(
        first_status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&first_body)
    );
    let (second_status, _, second_body) =
        call(app, "POST", "/v1/chat/completions", PROVIDER_CHAT_REQUEST).await;
    assert_eq!(
        second_status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&second_body)
    );

    let requests = instance_requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "nonce pool should reuse one verified /e2e/instances response"
    );
    drop(requests);

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].x_e2e_nonce.as_deref(), Some(CHUTES_NONCE));
    assert_eq!(calls[1].x_e2e_nonce.as_deref(), Some(CHUTES_NONCE_B));
}

#[tokio::test]
async fn chutes_provider_consumes_verifier_prewarmed_nonce_pool() {
    let (base_url, provider_calls, e2e_pubkey, instance_requests) =
        serve_chutes_provider_fixture().await;
    let store = Arc::new(ChutesSessionStore::new());
    let pubkey = BASE64.decode(&e2e_pubkey).unwrap();
    store
        .record_verified_discovery(ChutesVerifiedDiscovery {
            chute_id: CHUTES_CHUTE_ID.to_string(),
            nonce_expires_in: Some(55),
            instances: vec![ChutesVerifiedInstance {
                instance_id: CHUTES_INSTANCE_ID.to_string(),
                e2e_pubkey: e2e_pubkey.clone(),
                public_key_sha256: hex::encode(sha2::Sha256::digest(&pubkey)),
                nonces: vec![CHUTES_PREWARMED_NONCE.to_string()],
            }],
        })
        .unwrap();
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone())
        .with_session_store(store);
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![chutes_key_binding(&e2e_pubkey)],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", PROVIDER_CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let requests = instance_requests.lock().unwrap();
    assert!(
        requests.is_empty(),
        "prewarmed verified nonce pool should avoid /e2e/instances on the request path"
    );
    drop(requests);

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].x_e2e_nonce.as_deref(),
        Some(CHUTES_PREWARMED_NONCE)
    );
}

#[tokio::test]
async fn chutes_provider_refreshes_verified_nonce_pool_without_forwarding() {
    let (base_url, provider_calls, e2e_pubkey, instance_requests) =
        serve_chutes_provider_fixture().await;
    let store = Arc::new(ChutesSessionStore::new());
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone())
        .with_session_store(store);
    let event = UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![chutes_key_binding(&e2e_pubkey)],
        ..verified_event("chutes-provider", "provider-model")
    };

    let refreshed = backend
        .refresh_verified_sessions_for_model("provider-model", &event)
        .await
        .unwrap();
    assert_eq!(refreshed, 3);
    assert_eq!(instance_requests.lock().unwrap().len(), 1);
    assert!(provider_calls.lock().unwrap().is_empty());

    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(StaticUpstreamVerifier::new(event)),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);
    let (status, _, body) = call(app, "POST", "/v1/chat/completions", PROVIDER_CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        instance_requests.lock().unwrap().len(),
        1,
        "request should consume the refreshed pool without another /e2e/instances call"
    );
    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].x_e2e_nonce.as_deref(), Some(CHUTES_NONCE));
}

#[tokio::test]
async fn chutes_provider_interleaves_nonces_across_verified_instances() {
    let (base_url, provider_calls, e2e_pubkeys, instance_requests) =
        serve_chutes_provider_fixture_with_instances(vec![
            (
                CHUTES_INSTANCE_ID,
                vec![CHUTES_NONCE, CHUTES_NONCE_B, CHUTES_NONCE_C],
            ),
            (
                CHUTES_INSTANCE_ID_B,
                vec![
                    CHUTES_INSTANCE_B_NONCE,
                    CHUTES_INSTANCE_B_NONCE_B,
                    CHUTES_INSTANCE_B_NONCE_C,
                ],
            ),
        ])
        .await;
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone());
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![
            chutes_key_binding_for(
                CHUTES_INSTANCE_ID,
                e2e_pubkeys.get(CHUTES_INSTANCE_ID).unwrap(),
            ),
            chutes_key_binding_for(
                CHUTES_INSTANCE_ID_B,
                e2e_pubkeys.get(CHUTES_INSTANCE_ID_B).unwrap(),
            ),
        ],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    for _ in 0..3 {
        let (status, _, body) = call(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            PROVIDER_CHAT_REQUEST,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    }

    let requests = instance_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    drop(requests);

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].x_instance_id.as_deref(), Some(CHUTES_INSTANCE_ID));
    assert_eq!(calls[0].x_e2e_nonce.as_deref(), Some(CHUTES_NONCE));
    assert_eq!(
        calls[1].x_instance_id.as_deref(),
        Some(CHUTES_INSTANCE_ID_B)
    );
    assert_eq!(
        calls[1].x_e2e_nonce.as_deref(),
        Some(CHUTES_INSTANCE_B_NONCE)
    );
    assert_eq!(calls[2].x_instance_id.as_deref(), Some(CHUTES_INSTANCE_ID));
    assert_eq!(calls[2].x_e2e_nonce.as_deref(), Some(CHUTES_NONCE_B));
}

#[tokio::test]
async fn chutes_provider_decrypts_streaming_e2ee_response() {
    let (base_url, provider_calls, e2e_pubkey, _instance_requests) =
        serve_chutes_provider_fixture().await;
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone());
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![chutes_key_binding(&e2e_pubkey)],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", STREAM_CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains(r#""id":"chat-provider-1""#));
    assert!(body.contains("data: [DONE]"));

    let calls = provider_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].x_e2e_stream.as_deref(), Some("true"));
    assert_ne!(calls[0].body, STREAM_CHAT_REQUEST);
}

#[tokio::test]
async fn chutes_provider_refuses_unverified_e2ee_key() {
    let (base_url, provider_calls, _e2e_pubkey, _instance_requests) =
        serve_chutes_provider_fixture().await;
    let backend = ChutesProviderBackend::new_with_timeouts(base_url.clone(), 10, 600)
        .unwrap()
        .with_name("chutes-provider")
        .with_bearer_token("chutes-secret")
        .with_e2ee_api_base(base_url.clone());
    let verifier = StaticUpstreamVerifier::new(UpstreamVerifiedEvent {
        url_origin: Some(base_url),
        verifier_id: "fixture-chutes-verifier/v1".to_string(),
        evidence: Some(provider_evidence_fixture("chutes-attestation")),
        channel_bindings: vec![
            private_ai_gateway::aci::receipt::ChannelBinding::E2eePublicKeySha256 {
                provider: "chutes".to_string(),
                key_id: Some(CHUTES_INSTANCE_ID.to_string()),
                algorithm: "chutes-ml-kem-768".to_string(),
                public_key_sha256: "aa".repeat(32),
            },
        ],
        ..verified_event("chutes-provider", "provider-model")
    });
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(backend),
            Arc::new(verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test("provider-e2e"),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);

    let (status, _, body) = call(app, "POST", "/v1/chat/completions", PROVIDER_CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["error"]["type"], "internal_error");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("matching the verified binding"));
    assert!(provider_calls.lock().unwrap().is_empty());
}
