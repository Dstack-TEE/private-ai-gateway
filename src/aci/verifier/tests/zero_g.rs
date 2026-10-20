//! 0G verifier tests against a mock router, chain RPC and provider brokers.
//! Quotes are the real cn-19 quote with `report_data` (and, for the boot-chain
//! case, RTMR2) patched; DCAP-to-root is stubbed, everything else is real.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::super::zero_g::{PreviewCandidate, ZeroGProviderVerifier};
use crate::aci::receipt::{ChannelBinding, VerificationResult};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

const MODEL: &str = "glm-5.3";
const QUOTE_HEX: &str =
    include_str!("../../../../tests/fixtures/0g-private-computer/quote-cn19.hex");
const REPORT_DATA: std::ops::Range<usize> = 568..632;
const RTMR2: std::ops::Range<usize> = 472..520;

/// One mock provider: its on-chain record and the quote its broker serves.
struct Provider {
    address: String,
    record: String,
    quote: Value,
}

#[derive(Default)]
struct Mock {
    preview: Mutex<Vec<Value>>,
    preview_requests: Mutex<Vec<(HeaderMap, Value)>>,
    providers: HashMap<String, Provider>,
    redirect_hits: AtomicUsize,
}

fn address(name: &str) -> String {
    format!("0x{}", hex::encode([name.as_bytes()[0]; 20]))
}

fn signer(name: &str) -> String {
    format!("0x{}", hex::encode([name.as_bytes()[0] ^ 0xff; 20]))
}

fn enc_pub(name: &str) -> [u8; 32] {
    [name.as_bytes()[0]; 32]
}

fn word(value: usize) -> Vec<u8> {
    let mut word = vec![0; 32];
    word[24..].copy_from_slice(&(value as u64).to_be_bytes());
    word
}

/// ABI-encode a `getService` return.
fn service_record(
    url: &str,
    model: &str,
    additional_info: &str,
    signer: &str,
    ack: bool,
) -> String {
    let mut head = Vec::new();
    let mut tail = Vec::new();
    let mut string = |head: &mut Vec<u8>, value: &str| {
        head.extend(word(11 * 32 + tail.len()));
        tail.extend(word(value.len()));
        tail.extend(value.as_bytes());
        tail.resize(tail.len().div_ceil(32) * 32, 0);
    };
    head.extend(word(0)); // provider
    string(&mut head, "chatbot");
    string(&mut head, url);
    head.extend(word(0)); // inputPrice
    head.extend(word(0)); // outputPrice
    head.extend(word(0)); // updatedAt
    string(&mut head, model);
    string(&mut head, "TeeML");
    string(&mut head, additional_info);
    let mut signer_word = vec![0; 12];
    signer_word.extend(hex::decode(&signer[2..]).unwrap());
    head.extend(signer_word);
    head.extend(word(ack as usize));
    let mut out = word(32);
    out.extend(head);
    out.extend(tail);
    format!("0x{}", hex::encode(out))
}

fn quote(enc_pub: [u8; 32], signer: &str, tamper_boot_chain: bool) -> Value {
    let mut quote = hex::decode(QUOTE_HEX.trim()).unwrap();
    let mut report_data = enc_pub.to_vec();
    report_data.extend(hex::decode(&signer[2..]).unwrap());
    report_data.extend(1u32.to_be_bytes());
    report_data.resize(64, 0);
    quote[REPORT_DATA].copy_from_slice(&report_data);
    if tamper_boot_chain {
        quote[RTMR2.start] ^= 1;
    }
    json!({"quote": hex::encode(quote), "event_log": "[]"})
}

/// Stubbed DCAP-to-root: provider `h`'s quote is refused.
fn quote_status(raw: &[u8]) -> Result<String, String> {
    if raw[REPORT_DATA.start] == b'h' {
        Err("collateral rejected the quote".to_string())
    } else {
        Ok("UpToDate".to_string())
    }
}

struct Fixture {
    mock: Arc<Mock>,
    verifier: ZeroGProviderVerifier,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let separated = r#"{"TargetSeparated":true,"TargetTeeAddress":"0x79a5"}"#;
        let private = r#"{"TargetSeparated":false,"TargetTeeAddress":""}"#;
        let mut providers = HashMap::new();
        for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            let info = if name == "b" { separated } else { private };
            let model = if name == "d" { "glm-5" } else { MODEL };
            let quote_signer = if name == "e" {
                signer("x")
            } else {
                signer(name)
            };
            providers.insert(
                name.to_string(),
                Provider {
                    address: address(name),
                    record: service_record(
                        &format!("{origin}/{name}"),
                        model,
                        info,
                        &signer(name),
                        name != "c",
                    ),
                    quote: quote(enc_pub(name), &quote_signer, name == "f"),
                },
            );
        }
        let mock = Arc::new(Mock {
            providers,
            ..Default::default()
        });
        let app = Router::new()
            .route("/v1/routing/preview", post(preview))
            .route("/rpc", post(rpc))
            .route("/target/v1/quote", get(redirect_target))
            .route("/:name/v1/quote", get(provider_quote))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let verifier = ZeroGProviderVerifier::new(
            origin.clone(),
            format!("{origin}/rpc"),
            "0x47340d900bdfec2bd393c626e12ea0656f938d84",
            None,
            5,
            5,
            300,
        )
        .unwrap()
        .with_api_key("zg-test-key")
        .with_quote_status(quote_status);
        Self {
            mock,
            verifier,
            server,
        }
    }

    fn offer(&self, names: &[&str]) {
        *self.mock.preview.lock().unwrap() = names
            .iter()
            .map(|name| json!({"address": address(name), "provider_identity": null, "canonical_id": MODEL, "model_id": MODEL}))
            .collect();
    }

    fn candidate(&self, name: &str) -> PreviewCandidate {
        PreviewCandidate {
            address: address(name),
            provider_identity: None,
            canonical_id: Some(MODEL.into()),
            model_id: Some(MODEL.into()),
        }
    }
}

fn request() -> UpstreamVerificationRequest {
    UpstreamVerificationRequest {
        upstream_name: "zero-g".into(),
        url_origin: None,
        model_id: MODEL.into(),
        forwarded_body_hash: String::new(),
        required: true,
    }
}

async fn preview(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    mock.preview_requests.lock().unwrap().push((headers, body));
    Json(json!({"object": "routing.preview", "providers": *mock.preview.lock().unwrap()}))
}

async fn rpc(State(mock): State<Arc<Mock>>, Json(call): Json<Value>) -> Json<Value> {
    let data = call["params"][0]["data"].as_str().unwrap();
    let address = format!("0x{}", &data[data.len() - 40..]);
    let record = mock
        .providers
        .values()
        .find(|provider| provider.address == address)
        .map(|provider| provider.record.clone())
        .unwrap();
    Json(json!({"jsonrpc": "2.0", "id": 1, "result": record}))
}

async fn provider_quote(State(mock): State<Arc<Mock>>, Path(name): Path<String>) -> Response {
    if name == "g" {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "/target/v1/quote")],
        )
            .into_response();
    }
    Json(mock.providers[&name].quote.clone()).into_response()
}

async fn redirect_target(State(mock): State<Arc<Mock>>) -> Json<Value> {
    mock.redirect_hits.fetch_add(1, Ordering::SeqCst);
    Json(mock.providers["a"].quote.clone())
}

#[tokio::test]
async fn rejects_each_unsafe_candidate_before_sealing() {
    let fixture = Fixture::new().await;
    for (name, expected) in [
        ("b", "TargetSeparated: plaintext leaves the provider TEE"),
        ("c", "not acknowledged"),
        ("d", "serves model \"glm-5\""),
        ("e", "is not the on-chain teeSignerAddress"),
        ("f", "boot chain is not an allowlisted"),
        ("g", "quote endpoint returned 307"),
        ("h", "collateral rejected"),
    ] {
        let err = fixture
            .verifier
            .verify_candidate(&fixture.candidate(name), MODEL)
            .await
            .err()
            .unwrap_or_else(|| panic!("candidate {name} must be rejected"));
        assert!(err.contains(expected), "{name}: {err}");
    }
    let mut relabelled = fixture.candidate("a");
    relabelled.canonical_id = Some("glm-5".into());
    let err = fixture
        .verifier
        .verify_candidate(&relabelled, MODEL)
        .await
        .err()
        .unwrap();
    assert!(err.contains("router offered model"), "{err}");
    assert_eq!(fixture.mock.redirect_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn binds_only_the_verified_private_candidate() {
    let fixture = Fixture::new().await;
    fixture.offer(&["b", "a", "a"]);
    let event = fixture.verifier.verify(request()).await;
    assert_eq!(
        event.result,
        VerificationResult::Verified,
        "{:?}",
        event.reason
    );
    assert_eq!(
        event.channel_bindings,
        vec![ChannelBinding::E2eePublicKeySha256 {
            provider: "0g-private-computer".into(),
            key_id: Some(address("a")),
            algorithm: "0g-pc-v1-hpke-x25519-hkdf-sha256-chacha20poly1305".into(),
            public_key_sha256: hex::encode(Sha256::digest(enc_pub("a"))),
        }]
    );
    let instance = &event.provider_claims.as_ref().unwrap()["instances"][address("a")];
    assert_eq!(instance["signing_address"], signer("a"));
    assert_eq!(instance["e2ee_public_key"], hex::encode(enc_pub("a")));
    assert_eq!(instance["tcb_status"], "UpToDate");
    assert_eq!(instance["os_image"], "dstack-nvidia-0.5.4.1");
    assert!(instance["compose_hash"].is_string());

    let requests = fixture.mock.preview_requests.lock().unwrap();
    let (headers, body) = &requests[0];
    assert_eq!(headers["x-0g-provider-trust-mode"], "private");
    assert_eq!(headers["authorization"], "Bearer zg-test-key");
    assert_eq!(body, &json!({"model": MODEL, "service_type": "chatbot"}));
}

#[tokio::test]
async fn fails_closed_when_no_candidate_verifies() {
    let fixture = Fixture::new().await;
    fixture.offer(&["b", "c"]);
    let event = fixture.verifier.verify(request()).await;
    assert_eq!(event.result, VerificationResult::Failed);
    assert!(event.channel_bindings.is_empty());
    assert!(event
        .reason
        .unwrap()
        .contains("no 0G private-tier candidate passed verification"));
    // A failed verification is never cached.
    assert!(fixture.verifier.cached(&request()).is_none());
}
